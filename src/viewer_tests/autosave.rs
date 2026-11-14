//! Autosave and crash recovery as the running app drives them: the layers it
//! writes, the offer it makes after a crash, and what Restore, Discard and
//! Escape each leave on disk. (The store itself is tested in
//! `osm_gpui::autosave`.) Every test keeps its autosaves in a temporary
//! directory of its own; none touches the real app-data directory.

use crate::{parse_cli_args_from, MapViewer};
use gpui::{px, size, TestAppContext};
use osm_gpui::autosave::{Session, Snapshot};
use osm_gpui::coordinates::LatLon;
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::layers::LayerId;
use osm_gpui::layers::MapLayer;
use osm_gpui::osm::{OsmData, OsmNode, OsmWay};
use osm_gpui::selection::FeatureKind;
use osm_gpui::viewport::Viewport;
use osm_gpui::{auth, autosave, custom_imagery_store, persist, settings_store};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn tmp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "osm-gpui-driver-{tag}-{}-{:08x}",
        std::process::id(),
        rand::random::<u32>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture() -> OsmData {
    let node = |id: i64, lon: f64| OsmNode {
        id,
        lat: 40.0,
        lon,
        version: 2,
        tags: HashMap::new(),
    };
    OsmData {
        nodes: HashMap::from([(1, node(1, -74.001)), (2, node(2, -73.999))]),
        ways: HashMap::from([(
            10,
            OsmWay {
                id: 10,
                nodes: vec![1, 2],
                version: 3,
                tags: HashMap::new(),
            },
        )]),
        relations: Vec::new(),
        bounds: Vec::new(),
    }
}

fn edited_layer(id: u64, name: &str, key: &str) -> OsmLayer {
    let mut layer = OsmLayer::new_with_data(LayerId(id), name, Arc::new(fixture()));
    layer
        .as_editable_mut()
        .unwrap()
        .set_tag(FeatureKind::Way, 10, key, "yes")
        .unwrap();
    layer
}

fn window(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, _window, _cx| {
            view.viewport =
                Viewport::new(LatLon::new(40.0, -74.0), 18.0, size(px(800.0), px(600.0)));
        })
        .unwrap();
    window
}

/// A session that crashed: it wrote `layer` and then its lock went away.
fn crashed_session(root: &std::path::Path, layer: &OsmLayer, api: Option<&str>) -> PathBuf {
    let session = Session::start(root).unwrap();
    let (current, original) = layer.autosave_snapshot().unwrap();
    session
        .write(&Snapshot {
            layer_id: layer.id().0,
            layer_name: layer.name().to_string(),
            source_path: None,
            api_server: api.map(str::to_string),
            current,
            original,
        })
        .unwrap();
    let dir = session.dir().to_path_buf();
    drop(session);
    dir
}

fn session_dirs(root: &std::path::Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn layer_files(root: &std::path::Path) -> usize {
    session_dirs(root)
        .iter()
        .flat_map(|d| std::fs::read_dir(d).unwrap())
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "osm"))
        .count()
}

fn pending(view: &MapViewer, id: LayerId) -> usize {
    MapViewer::unsaved_edit_count(view.layer_manager.find_layer(id).unwrap())
}

#[gpui::test]
fn an_edited_layer_is_written_and_its_file_goes_with_the_layer(cx: &mut TestAppContext) {
    let root = tmp_root("write");
    let window = window(cx);
    let id = window
        .update(cx, |view, _window, cx| {
            view.init_autosave(Some(root.clone()), cx);
            let id = view.layer_manager.alloc_id();
            view.layer_manager
                .add_layer(Box::new(edited_layer(id.0, "Mine", "building")));
            // A layer with nothing pending is never written.
            let clean = view.layer_manager.alloc_id();
            view.layer_manager
                .add_layer(Box::new(OsmLayer::new_with_data(
                    clean,
                    "Clean",
                    Arc::new(fixture()),
                )));
            view.autosave_now(cx);
            id
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(layer_files(&root), 1);
    window
        .update(cx, |view, _window, _cx| {
            assert!(view.script_autosave_idle().is_ok());
            assert!(view.script_autosave_files(Some(1)).is_ok());
            assert!(view.script_autosave_files(Some(2)).is_err());
        })
        .unwrap();

    // Deleting the layer removes its autosave.
    window
        .update(cx, |view, _window, cx| view.delete_layer_now(id, cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(layer_files(&root), 0);
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn an_upload_that_leaves_nothing_pending_removes_the_autosave(cx: &mut TestAppContext) {
    let root = tmp_root("uploaded");
    let window = window(cx);
    let id = window
        .update(cx, |view, _window, cx| {
            view.init_autosave(Some(root.clone()), cx);
            let id = view.layer_manager.alloc_id();
            view.layer_manager
                .add_layer(Box::new(edited_layer(id.0, "Mine", "building")));
            view.autosave_now(cx);
            id
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(layer_files(&root), 1);

    // The server accepts everything: the layer's data becomes the new
    // baseline (what `apply_upload_result` does), and the app asks for
    // an immediate write, as it does after each applied changeset.
    window
        .update(cx, |view, _window, cx| {
            let layer = view
                .layer_manager
                .find_layer_mut(id)
                .unwrap()
                .as_osm_mut()
                .unwrap();
            layer.apply_upload_result(&osm_gpui::osm_upload::UploadResult {
                node_id_remap: HashMap::new(),
                way_id_remap: HashMap::from([(10, (10, 4))]),
                relation_id_remap: HashMap::new(),
            });
            view.autosave_now(cx);
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(layer_files(&root), 0, "nothing left to recover");
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn a_crashed_run_is_offered_and_restore_brings_the_edits_back(cx: &mut TestAppContext) {
    let root = tmp_root("restore");
    let layer = edited_layer(7, "Downtown", "building");
    let before = layer.diff_for_upload().counts();
    let crashed = crashed_session(&root, &layer, None);
    assert!(crashed.exists());

    let window = window(cx);
    window
        .update(cx, |view, _window, cx| {
            view.init_autosave(Some(root.clone()), cx)
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |view, window, cx| {
            // (The window's own render may have shown it already.)
            view.check_for_pending_restore(window, cx);
            let dialog = view
                .dialogs
                .restore_dialog
                .clone()
                .expect("the offer is shown");
            let items = dialog.read(cx).items().to_vec();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].layer_name, "Downtown");
            assert_eq!(items[0].edits, before.0 + before.1 + before.2);
            assert_eq!(items[0].other_server, None);
            dialog.update(cx, |dialog, cx| dialog.press_restore(cx));
        })
        .unwrap();
    cx.run_until_parked();

    window
        .update(cx, |view, _window, _cx| {
            assert!(view.dialogs.restore_dialog.is_none());
            let restored = view
                .layer_manager
                .layers()
                .iter()
                .find(|l| l.name() == "Downtown")
                .expect("the layer is back");
            let restored_osm = restored.as_osm().unwrap();
            // Edits pending, and the layer counts as unsaved.
            assert!(restored_osm.is_modified());
            assert_eq!(restored_osm.diff_for_upload().counts(), before);
            assert_eq!(pending(view, restored.id()), before.0 + before.1 + before.2);
            assert_eq!(view.active_layer, Some(restored.id()));
            assert!(view.script_autosave_idle().is_ok());
            assert!(view.status_message.is_some());
        })
        .unwrap();
    // The old session is gone and its layer lives on in this run's
    // session, so a second crash would lose nothing.
    assert!(!crashed.exists());
    assert_eq!(layer_files(&root), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn escape_asks_again_next_time_and_discard_deletes(cx: &mut TestAppContext) {
    let root = tmp_root("later");
    let layer = edited_layer(7, "Downtown", "building");
    let crashed = crashed_session(&root, &layer, None);

    let window = window(cx);
    window
        .update(cx, |view, _window, cx| {
            view.init_autosave(Some(root.clone()), cx)
        })
        .unwrap();
    cx.run_until_parked();
    // Escape: the dialog goes, the files stay.
    window
        .update(cx, |view, window, cx| {
            view.check_for_pending_restore(window, cx);
            let dialog = view.dialogs.restore_dialog.clone().unwrap();
            dialog.update(cx, |dialog, cx| dialog.press_later(cx));
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |view, _window, _cx| {
            assert!(view.dialogs.restore_dialog.is_none());
            assert!(view
                .layer_manager
                .layers()
                .iter()
                .all(|l| l.name() != "Downtown"));
        })
        .unwrap();
    assert!(crashed.exists(), "Escape keeps the autosave");
    assert_eq!(layer_files(&root), 1);

    // The next start finds it again; this time the user discards it.
    let window = self::window(cx);
    window
        .update(cx, |view, _window, cx| {
            view.init_autosave(Some(root.clone()), cx)
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |view, window, cx| {
            view.check_for_pending_restore(window, cx);
            let dialog = view.dialogs.restore_dialog.clone().expect("offered again");
            dialog.update(cx, |dialog, cx| dialog.press_discard(cx));
        })
        .unwrap();
    cx.run_until_parked();
    assert!(!crashed.exists());
    assert_eq!(layer_files(&root), 0);
    window
        .update(cx, |view, _window, _cx| {
            assert!(view.dialogs.restore_dialog.is_none());
            assert!(view
                .layer_manager
                .layers()
                .iter()
                .all(|l| l.name() != "Downtown"));
        })
        .unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn a_layer_from_another_server_is_flagged_in_the_offer(cx: &mut TestAppContext) {
    let root = tmp_root("server");
    let layer = edited_layer(7, "Dev data", "building");
    crashed_session(
        &root,
        &layer,
        Some("https://master.apis.dev.openstreetmap.org/api/0.6"),
    );
    let window = window(cx);
    window
        .update(cx, |view, _window, cx| {
            view.init_autosave(Some(root.clone()), cx)
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |view, window, cx| {
            view.check_for_pending_restore(window, cx);
            let dialog = view.dialogs.restore_dialog.clone().unwrap();
            let items = dialog.read(cx).items().to_vec();
            let (from, current) = items[0].other_server.clone().expect("flagged");
            assert!(from.contains("master.apis.dev"), "{from}");
            assert_eq!(current, settings_store::api_base_url());
            // Restoring keeps the layer's origin for the next autosave.
            dialog.update(cx, |dialog, cx| dialog.press_restore(cx));
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |view, _window, _cx| {
            let id = view
                .layer_manager
                .layers()
                .iter()
                .find(|l| l.name() == "Dev data")
                .unwrap()
                .id();
            let origin = &view.autosave.as_ref().unwrap().origin_of(id);
            assert_eq!(
                origin.api_server.as_deref(),
                Some("https://master.apis.dev.openstreetmap.org/api/0.6")
            );
        })
        .unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn autosave_is_off_without_a_directory(cx: &mut TestAppContext) {
    let window = window(cx);
    window
        .update(cx, |view, _window, cx| {
            view.init_autosave(None, cx);
            assert!(view.autosave.is_none());
            // The calls the rest of the app makes are harmless no-ops.
            view.autosave_now(cx);
            assert!(view.script_autosave_idle().is_err());
            // So is the upload-in-progress marker.
            assert!(view
                .upload_marker_prepare(autosave::UploadMarker::default())
                .is_none());
            view.upload_marker_after(osm_gpui::upload_chunks::MarkerAfter::Release, cx);
            assert!(view.restored_upload_warning().is_none());
        })
        .unwrap();
}

#[test]
fn autosave_dir_is_a_command_line_option() {
    let parse = |list: &[&str]| parse_cli_args_from(list.iter().map(|s| s.to_string()));
    assert_eq!(parse(&[]).autosave_dir, None);
    assert_eq!(
        parse(&["--autosave-dir", "/tmp/x", "--script", "s.osmscript"]).autosave_dir,
        Some(PathBuf::from("/tmp/x"))
    );
    // A scripted run with no directory resolves to no autosave at all.
    let args = parse(&["--script", "s.osmscript"]);
    assert_eq!(
        autosave::resolve_root(
            args.script.is_some(),
            args.autosave_dir,
            Some(PathBuf::from("/real/app/data/autosave"))
        ),
        None
    );
}

/// The binary's tests link the library without its `cfg(test)`, so this is
/// the guard that matters for them: the persistence policy is never raised
/// outside `main()`, and no save entry point reaches the real files.
#[test]
fn bin_tests_never_modify_the_real_saved_state() {
    let state_dir = dirs::config_dir().map(|d| d.join("osm-gpui"));
    let probe_paths: Vec<PathBuf> = state_dir
        .iter()
        .flat_map(|d| {
            [
                "settings.json",
                "custom-imagery.json",
                "imagery-offsets.json",
                "oauth.json",
            ]
            .map(|f| d.join(f))
        })
        .collect();
    // Preconditions first: if the guard were broken, fail before saving.
    assert_eq!(persist::policy(), persist::Policy::Disabled);
    assert_eq!(settings_store::default_path(), None);
    assert_eq!(auth::default_path(), None);
    assert_eq!(osm_gpui::autosave::default_root(), None);
    for p in &probe_paths {
        assert!(!persist::write_permitted(p), "{p:?} must be protected");
    }
    let mtimes = |paths: &[PathBuf]| -> Vec<Option<std::time::SystemTime>> {
        paths
            .iter()
            .map(|p| std::fs::metadata(p).ok().and_then(|m| m.modified().ok()))
            .collect()
    };
    let before = mtimes(&probe_paths);

    settings_store::update(true, |s| s.welcome_dismissed = true);
    settings_store::set_welcome_dismissed(true, true);
    settings_store::set_invert_scroll_zoom(true, true);
    custom_imagery_store::update_store(Vec::new());

    // The lowest write point reports the skip for the real settings file.
    if let Some(real) = probe_paths.first() {
        let outcome = persist::try_write_atomic(real, b"{}", persist::WriteOpts::default());
        assert_eq!(outcome.unwrap(), persist::WriteOutcome::Skipped);
    }
    assert_eq!(before, mtimes(&probe_paths));
}
