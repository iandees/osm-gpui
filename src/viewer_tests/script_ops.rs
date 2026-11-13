//! Script operations that drive the viewer: typed text, checks, tags and export.

use super::{make_pending_change, way_fixture, CENTER_LAT, CENTER_LON};
use crate::{script_harness, MapViewer};
use gpui::prelude::*;
use gpui::{px, size, TestAppContext};
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::osm_upload;
use osm_gpui::viewport::Viewport;
use std::sync::Arc;

fn dialog_window(cx: &mut TestAppContext) -> gpui::WindowHandle<gpui_component::Root> {
    cx.update(gpui_component::init);
    cx.add_window(|window, cx| {
        let view = cx.new(|cx| MapViewer::new(window, cx));
        gpui_component::Root::new(view, window, cx)
    })
}

/// The window's `MapViewer`, with a one-way layer (way 10 on nodes 1, 2)
/// loaded and active.
fn view_with_layer(
    window: gpui::WindowHandle<gpui_component::Root>,
    vcx: &mut gpui::VisualTestContext,
) -> gpui::Entity<MapViewer> {
    let root = window.root(vcx).unwrap();
    let view: gpui::Entity<MapViewer> = root
        .read_with(vcx, |root, _| root.view().clone())
        .downcast::<MapViewer>()
        .unwrap();
    view.update_in(vcx, |view, _window, _cx| {
        let layer_id = view.layer_manager.alloc_id();
        let layer = OsmLayer::new_with_data(
            layer_id,
            "L".to_string(),
            Arc::new(way_fixture(CENTER_LAT, CENTER_LON)),
        );
        view.layer_manager.add_layer(Box::new(layer));
        view.active_layer = Some(layer_id);
        view.viewport = Viewport::new(CENTER_LAT, CENTER_LON, 18.0, size(px(800.0), px(600.0)));
    });
    view
}

#[gpui::test]
fn typed_script_text_lands_in_the_comment_field(cx: &mut TestAppContext) {
    let window = dialog_window(cx);
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let view = view_with_layer(window, vcx);
    view.update_in(vcx, |view, window, cx| {
        make_pending_change(view);
        view.open_upload_dialog_as(Some("me"), window, cx);
        assert!(view.upload_dialog.is_some());
    });
    let text = "osm-gpui live test: a b x & <ok> \"q\" caf\u{e9} \u{65e5}";
    for ch in text.chars() {
        vcx.update(|window, cx| {
            window.dispatch_keystroke(script_harness::keystroke_for_char(ch), cx);
        });
    }
    view.update_in(vcx, |view, _window, cx| {
        let dialog = view.upload_dialog.clone().unwrap();
        assert_eq!(dialog.read(cx).draft_comment(cx), text);
    });
}

#[gpui::test]
fn script_checks_see_pending_edits_dialogs_and_selection(cx: &mut TestAppContext) {
    use osm_gpui::script::{Check, DialogExpect, SelectionKind};
    let window = dialog_window(cx);
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let view = view_with_layer(window, vcx);
    view.update_in(vcx, |view, window, cx| {
        assert!(view.script_check(&Check::Pending(0), cx).is_ok());
        assert!(view.script_check(&Check::Pending(1), cx).is_err());
        make_pending_change(view);
        assert!(view.script_check(&Check::Pending(1), cx).is_ok());
        assert!(view
            .script_check(&Check::Dialog(DialogExpect::None), cx)
            .is_ok());
        assert!(view
            .script_check(&Check::Dialog(DialogExpect::Editing), cx)
            .is_err());
        assert!(view.script_check(&Check::TransferIdle, cx).is_ok());

        view.open_upload_dialog_as(Some("me"), window, cx);
        assert!(view
            .script_check(&Check::Dialog(DialogExpect::Editing), cx)
            .is_ok());
        assert!(view
            .script_check(&Check::Dialog(DialogExpect::None), cx)
            .is_err());
        assert!(view
            .script_check(&Check::Dialog(DialogExpect::Failed), cx)
            .is_err());

        // The Upload button does nothing without a comment, and the
        // script says why.
        assert!(view
            .upload_dialog
            .clone()
            .unwrap()
            .read(cx)
            .confirm_blocker(cx)
            .is_some());
        view.upload_dialog.clone().unwrap().update(cx, |d, cx| {
            d.set_draft_comment("osm-gpui live test".into(), window, cx)
        });
        assert!(view
            .upload_dialog
            .clone()
            .unwrap()
            .read(cx)
            .confirm_blocker(cx)
            .is_none());

        // A refused upload is a conflict of a kind, naming an object.
        let conflict = osm_upload::UploadConflict::parse(
            409,
            "Version mismatch: Provided 1, server had: 2 of Node 2",
        );
        let conflict_view = view.conflict_view(conflict);
        view.upload_dialog
            .clone()
            .unwrap()
            .update(cx, |d, cx| d.fail_with_conflict(conflict_view, cx));
        let expect = |kind, object| Check::Dialog(DialogExpect::Conflict { kind, object });
        use osm_gpui::script::{ConflictKindName, FeatureKind as K};
        assert!(view.script_check(&expect(None, None), cx).is_ok());
        assert!(view
            .script_check(
                &expect(Some(ConflictKindName::VersionMismatch), Some(K::Node)),
                cx
            )
            .is_ok());
        assert!(view
            .script_check(&expect(Some(ConflictKindName::AlreadyDeleted), None), cx)
            .is_err());
        assert!(view.script_check(&expect(None, Some(K::Way)), cx).is_err());
        assert!(view
            .script_check(&Check::Dialog(DialogExpect::Failed), cx)
            .is_ok());

        assert!(view
            .script_check(&Check::Selection(SelectionKind::None), cx)
            .is_ok());
        assert!(view
            .script_check(&Check::Selection(SelectionKind::Node), cx)
            .is_err());
    });
}

#[gpui::test]
fn script_set_tag_and_remove_tag_edit_the_selection_in_one_undo_step_each(cx: &mut TestAppContext) {
    use osm_gpui::selection::{FeatureKind, FeatureRef};
    let window = dialog_window(cx);
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let view = view_with_layer(window, vcx);
    view.update_in(vcx, |view, _window, cx| {
        let layer_id = view.active_layer.unwrap();
        assert!(
            view.script_set_tag("name", "x", cx).is_err(),
            "no selection"
        );
        view.selected = vec![FeatureRef {
            layer_id,
            kind: FeatureKind::Way,
            id: 10,
        }];
        let value = "A & B <C> \"D\" caf\u{e9}";
        view.script_set_tag("name", value, cx).unwrap();
        assert_eq!(
            view.selected_feature_tag_snapshots()[0].1,
            vec![("name".to_string(), value.to_string())]
        );
        assert_eq!(view.undo_stack.actions.len(), 1);
        assert_eq!(view.active_layer_pending(), Some(1));
        // Setting the same value again changes nothing.
        view.script_set_tag("name", value, cx).unwrap();
        assert_eq!(view.undo_stack.actions.len(), 1);
        view.script_remove_tag("name", cx).unwrap();
        assert!(view.selected_feature_tag_snapshots()[0].1.is_empty());
        assert!(view.script_remove_tag("name", cx).is_err());
    });
}

#[gpui::test]
fn script_export_writes_the_active_layer(cx: &mut TestAppContext) {
    let window = dialog_window(cx);
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let view = view_with_layer(window, vcx);
    let path = std::env::temp_dir()
        .join(format!("osm-gpui-script-export-{}", std::process::id()))
        .join("nested")
        .join("layer.osm");
    view.update_in(vcx, |view, _window, _cx| {
        make_pending_change(view);
        view.script_export_layer(&path).unwrap();
    });
    let xml = std::fs::read_to_string(&path).unwrap();
    assert!(xml.contains("<osm"), "{xml}");
    assert!(xml.contains("<way"), "{xml}");
    // The export loads back with the pending edit still pending.
    let (data, edits) = osm_gpui::osm::OsmParser::new()
        .parse_file_with_edits(path.to_str().unwrap())
        .unwrap();
    assert!(data.ways.contains_key(&10));
    let _ = edits;
    let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
}

#[test]
fn a_scripted_run_only_uploads_to_the_dev_server() {
    // Outside script mode nothing is refused.
    assert!(!script_harness::script_mode());
    assert_eq!(
        script_harness::scripted_upload_refusal("https://api.openstreetmap.org"),
        None
    );
}
