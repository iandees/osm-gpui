//! Undo and redo interacting with things that are not edits: operations the
//! app refuses, autosave and restore, and several layers open at once.

use super::*;
use osm_gpui::autosave;
use osm_gpui::coordinates::LatLon;

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

/// Run `f` as a session on another layer of the same view.
fn on<R>(s: &mut Session, layer: LayerId, f: impl FnOnce(&mut Session) -> R) -> R {
    s.view.active_layer = Some(layer);
    let mut t = Session {
        view: &mut *s.view,
        cx: &mut *s.cx,
        layer,
    };
    f(&mut t)
}

fn add_layer(s: &mut Session, name: &str, data: OsmData) -> LayerId {
    let id = s.view.layer_manager.alloc_id();
    s.view
        .layer_manager
        .add_layer(Box::new(OsmLayer::new_with_data(
            id,
            name.to_string(),
            Arc::new(data),
        )));
    id
}

/// Undo or redo through the same path the Undo / Redo commands use.
fn history_step(s: &mut Session, forward: bool) {
    let action = if forward {
        s.view.undo_stack.redo()
    } else {
        s.view.undo_stack.undo()
    }
    .expect("a step");
    s.view.run_history_step(&action, forward);
}

// ---------------------------------------------------------------------------
// 1. Refused operations leave the history alone
// ---------------------------------------------------------------------------

/// Ways 20 and 21 are free-standing roads that can be reversed (the two real
/// edits). The rest set up one refusal each:
/// - join of 10 + 11 (11 starts in the middle of 10, a T junction),
/// - merge of nodes 5 + 6 (the only two nodes of way 12, would collapse it),
/// - delete of way 13 (the only member of relation 900).
fn refusal_data() -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13])
        .way(10, &[1, 2, 3], &road())
        .way(11, &[2, 4], &road())
        .way(12, &[5, 6], &road())
        .way(13, &[7, 8], &road())
        .way(20, &[11, 12], &road())
        .way(21, &[12, 13], &road())
        .relation(900, &[("type", "route"), ("route", "bus")], &[wm(13, "")])
        .done()
}

fn refusal_survives_between_two_edits(
    cx: &mut TestAppContext,
    sel: Vec<(FeatureKind, i64)>,
    action: &'static str,
) {
    with_session(cx, refusal_data(), move |s| {
        let original = s.snap();
        s.select(&[(W, 20)]);
        assert!(s.act("reverse"));
        let after_first = s.snap();
        s.select(&[(W, 21)]);
        assert!(s.act("reverse"));
        let after_second = s.snap();
        assert_eq!(s.steps(), 2);

        // Undo the second edit so a redo branch is live, then try the
        // operation that must be refused.
        s.undo();
        assert_eq!(s.snap(), after_first);
        let revision = s.view.undo_stack.revision;

        s.select(&sel);
        assert!(!s.act(action), "{action} must be refused");
        assert_eq!(s.snap(), after_first, "refusal changed data");
        assert_eq!(s.steps(), 2, "refusal changed the number of steps");
        assert_eq!(s.view.undo_stack.cursor, 1, "refusal moved the cursor");
        assert_eq!(s.view.undo_stack.revision, revision);

        // The redo branch is still there and still replays the second edit.
        s.redo();
        assert_eq!(s.snap(), after_second);

        // A refusal at the tip does not disturb the stack either.
        s.select(&sel);
        assert!(!s.act(action));
        assert_eq!(s.steps(), 2);
        assert_eq!(s.view.undo_stack.cursor, 2);

        s.undo();
        s.undo();
        assert_eq!(s.snap(), original);
        assert_eq!(s.counts(), (0, 0, 0));
        assert!(s.view.undo_stack.undo().is_none());
    });
}

#[gpui::test]
fn a_refused_join_between_two_edits_keeps_the_stack_and_the_redo_branch(cx: &mut TestAppContext) {
    refusal_survives_between_two_edits(cx, vec![(W, 10), (W, 11)], "join");
}

#[gpui::test]
fn a_refused_merge_between_two_edits_keeps_the_stack_and_the_redo_branch(cx: &mut TestAppContext) {
    refusal_survives_between_two_edits(cx, vec![(N, 5), (N, 6)], "merge");
}

#[gpui::test]
fn a_delete_refused_by_a_relation_check_between_two_edits_keeps_the_stack_and_the_redo_branch(
    cx: &mut TestAppContext,
) {
    refusal_survives_between_two_edits(cx, vec![(W, 13)], "delete");
}

// ---------------------------------------------------------------------------
// 2. Undo after autosave / restore
// ---------------------------------------------------------------------------

fn tmp_root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "osm-gpui-undo-interactions-{tag}-{}-{:08x}",
        std::process::id(),
        rand::random::<u32>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Autosave `s`'s layer into a fresh session in `root` and let the session
/// "crash" (its lock is dropped, as when the process dies).
fn autosave_and_crash(s: &Session, root: &std::path::Path) {
    let (current, original) = s
        .view
        .layer_manager
        .find_layer(s.layer)
        .unwrap()
        .as_osm()
        .unwrap()
        .autosave_snapshot()
        .unwrap();
    let session = autosave::Session::start(root).unwrap();
    session
        .write(&autosave::Snapshot {
            layer_id: s.layer.0,
            layer_name: "Net".into(),
            source_path: None,
            api_server: None,
            current,
            original,
        })
        .unwrap();
    drop(session);
}

/// What the next start does with a crashed session: read each layer back with
/// its pending edits and put it on the map as a new layer. Returns the new
/// layers' ids. (`MapViewer::finish_restore` does this plus bookkeeping for
/// the autosave files; it is private to the autosave driver.)
fn restore_all(s: &mut Session, root: &std::path::Path) -> Vec<LayerId> {
    let mut ids = Vec::new();
    for crashed in autosave::find_crashed(root, None) {
        for rec in &crashed.layers {
            let (data, edits) = rec.load().unwrap();
            let id = s.view.layer_manager.alloc_id();
            let layer = OsmLayer::new_with_edits(
                id,
                rec.manifest.layer_name.clone(),
                Arc::new(data),
                &edits,
            );
            s.view.layer_manager.add_layer(Box::new(layer));
            ids.push(id);
        }
        crashed.discard();
    }
    ids
}

/// A view as it is right after a restart: no history, only restored layers.
fn with_restored(
    cx: &mut TestAppContext,
    root: std::path::PathBuf,
    f: impl FnOnce(&mut Session<'_, '_>) + 'static,
) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, _window, cx| {
            view.viewport =
                Viewport::new(LatLon::new(40.0, -74.0), 18.0, size(px(800.0), px(600.0)));
            view.enter_mode(EditMode::Select);
            let mut s = Session {
                view,
                cx,
                layer: LayerId(0),
            };
            let ids = restore_all(&mut s, &root);
            assert_eq!(ids.len(), 1, "one layer was autosaved");
            s.layer = ids[0];
            s.view.active_layer = Some(ids[0]);
            f(&mut s);
        })
        .unwrap();
}

fn two_roads() -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2], &road())
        .way(11, &[3, 4], &road())
        .done()
}

#[gpui::test]
fn a_restored_layer_keeps_its_pending_edits_but_has_no_undo_history(cx: &mut TestAppContext) {
    let root = tmp_root("restart");
    let crash_root = root.clone();
    let cell = std::rc::Rc::new(std::cell::RefCell::new(None));
    let cell2 = cell.clone();
    // The run that crashes: one reverse, autosaved.
    with_session(cx, two_roads(), move |s| {
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert_eq!(s.steps(), 1);
        autosave_and_crash(s, &crash_root);
        *cell2.borrow_mut() = Some((s.snap(), s.counts()));
    });
    let (snap, counts) = cell.borrow_mut().take().unwrap();
    assert_eq!(counts, (0, 1, 0));

    // The next run: the edit is back and uploadable, but nothing can be
    // undone (undo history is not persisted).
    with_restored(cx, root, move |s| {
        assert_eq!(s.way_nodes(10), Some(vec![2, 1]), "the reverse is back");
        assert_eq!(s.counts(), counts);
        assert_eq!(s.snap().ways, snap.ways);
        assert_eq!(s.steps(), 0);
        assert!(s.view.undo_stack.undo().is_none());
        assert!(s.view.undo_stack.redo().is_none());

        // New work on the restored layer can be undone, but only back to the
        // restored state, never past it.
        let restored = s.snap();
        s.select(&[(W, 11)]);
        assert!(s.act("reverse"));
        assert_eq!(s.steps(), 1);
        s.undo();
        assert_eq!(s.snap(), restored);
        assert_eq!(s.counts(), counts);
        assert!(s.view.undo_stack.undo().is_none());
    });
}

#[gpui::test]
fn restoring_a_layer_into_a_running_view_leaves_the_existing_history_alone(
    cx: &mut TestAppContext,
) {
    let root = tmp_root("running");
    with_session(cx, two_roads(), move |s| {
        // The layer that was lost in the crash.
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        autosave_and_crash(s, &root);
        let lost_counts = s.counts();
        // Forget the live copy so the history is only the other layer's.
        let lost = s.layer;
        let removed = s.view.layer_manager.remove_at(0).map(|l| l.id());
        assert_eq!(removed, Some(lost));
        s.view.undo_stack = Default::default();

        // This run has its own layer, with an edit and a redo branch. The
        // restore adds a layer and must not touch either.
        let other = add_layer(s, "Other", two_roads());
        on(s, other, |t| {
            t.select(&[(W, 10)]);
            assert!(t.act("reverse"));
            t.select(&[(W, 11)]);
            assert!(t.act("reverse"));
            t.undo();
        });
        let (steps, cursor) = (s.steps(), s.view.undo_stack.cursor);
        assert_eq!((steps, cursor), (2, 1));
        let other_before = on(s, other, |t| t.snap());

        let restored = restore_all(s, &root);
        assert_eq!(restored.len(), 1);
        assert_ne!(restored[0], other);
        assert_eq!(s.steps(), steps);
        assert_eq!(s.view.undo_stack.cursor, cursor);
        assert!(
            s.view
                .undo_stack
                .actions
                .iter()
                .all(|a| !a.layers().contains(&restored[0])),
            "no history step names the restored layer"
        );
        assert_eq!(on(s, restored[0], |t| t.counts()), lost_counts);
        assert_eq!(on(s, other, |t| t.snap()), other_before);

        // Undo and redo still work on the layer they were recorded for and
        // leave the restored layer's pending edit alone.
        history_step(s, false);
        assert_eq!(on(s, restored[0], |t| t.counts()), lost_counts);
        history_step(s, true);
        history_step(s, true);
        assert_eq!(on(s, restored[0], |t| t.counts()), lost_counts);
    });
}

// ---------------------------------------------------------------------------
// 3. Several layers
// ---------------------------------------------------------------------------

#[gpui::test]
fn undo_reverts_the_layer_the_step_was_recorded_on_not_the_active_layer(cx: &mut TestAppContext) {
    // Both layers hold identical data, so a step applied to the wrong layer
    // would find the same ids and silently succeed.
    with_session(cx, two_roads(), |s| {
        let a = s.layer;
        let b = add_layer(s, "Second", two_roads());
        let a_original = s.snap();
        let b_original = on(s, b, |t| t.snap());

        s.view.active_layer = Some(a);
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        let a_edited = s.snap();
        on(s, b, |t| {
            t.select(&[(W, 10)]);
            assert!(t.act("reverse"));
        });
        let b_edited = on(s, b, |t| t.snap());

        // Layer A is active, but the newest step belongs to B.
        s.view.active_layer = Some(a);
        history_step(s, false);
        assert_eq!(s.snap(), a_edited, "the active layer is not touched");
        assert_eq!(on(s, b, |t| t.snap()), b_original);
        // Design: selecting the undone feature makes its layer the active one.
        assert_eq!(
            s.view.active_layer,
            Some(b),
            "active layer follows the step"
        );
        assert_eq!(s.counts(), (0, 1, 0));
        assert_eq!(on(s, b, |t| t.counts()), (0, 0, 0));
        // The undone feature is selected, in its own layer.
        assert!(!s.view.selected.is_empty());
        assert!(s.view.selected.iter().all(|f| f.layer_id == b));

        // Switch to B and undo again: now A's step goes, B stays as it is.
        s.view.active_layer = Some(b);
        history_step(s, false);
        assert_eq!(s.snap(), a_original);
        assert_eq!(on(s, b, |t| t.snap()), b_original);

        // Redo replays in the same order regardless of the active layer.
        s.view.active_layer = Some(b);
        history_step(s, true);
        assert_eq!(s.snap(), a_edited);
        assert_eq!(on(s, b, |t| t.snap()), b_original);
        history_step(s, true);
        assert_eq!(on(s, b, |t| t.snap()), b_edited);
        assert_eq!(s.snap(), a_edited);
    });
}

#[gpui::test]
fn edits_interleaved_across_layers_undo_in_one_shared_order_and_redo_back(cx: &mut TestAppContext) {
    with_session(cx, two_roads(), |s| {
        let a = s.layer;
        let b = add_layer(s, "Second", two_roads());
        let mut states: Vec<(Snap, Snap)> = vec![(s.snap(), on(s, b, |t| t.snap()))];

        // A, B, A, B, each a different feature.
        for (layer, way) in [(a, 10), (b, 10), (a, 11), (b, 11)] {
            s.view.active_layer = Some(layer);
            on(s, layer, |t| {
                t.select(&[(W, way)]);
                assert!(t.act("reverse"));
            });
            states.push((s.snap(), on(s, b, |t| t.snap())));
        }
        assert_eq!(s.steps(), 4);

        for i in (0..4).rev() {
            history_step(s, false);
            assert_eq!(
                s.snap(),
                states[i].0,
                "layer A after undoing step {}",
                i + 1
            );
            assert_eq!(
                on(s, b, |t| t.snap()),
                states[i].1,
                "layer B after undoing step {}",
                i + 1
            );
        }
        assert_eq!(s.counts(), (0, 0, 0));
        assert_eq!(on(s, b, |t| t.counts()), (0, 0, 0));
        for state in states.iter().skip(1) {
            history_step(s, true);
            assert_eq!(s.snap(), state.0);
            assert_eq!(on(s, b, |t| t.snap()), state.1);
        }
    });
}

#[gpui::test]
fn a_new_edit_in_one_layer_drops_the_redo_branch_of_the_other_layer(cx: &mut TestAppContext) {
    // Design: the history is one global list, so editing layer A after
    // undoing a layer-B step abandons B's redo.
    with_session(cx, two_roads(), |s| {
        let a = s.layer;
        let b = add_layer(s, "Second", two_roads());
        on(s, b, |t| {
            t.select(&[(W, 10)]);
            assert!(t.act("reverse"));
        });
        let b_edited = on(s, b, |t| t.snap());
        history_step(s, false);
        assert!(s.view.undo_stack.actions.len() > s.view.undo_stack.cursor);

        s.view.active_layer = Some(a);
        s.select(&[(W, 11)]);
        assert!(s.act("reverse"));
        assert_eq!(s.steps(), 1);
        assert!(s.view.undo_stack.redo().is_none());
        assert_ne!(on(s, b, |t| t.snap()), b_edited);
    });
}
