//! Resolving upload conflicts: keep mine, use theirs, drop my change.

use super::{
    editable, pending, select, setup, status_text, tag_node_one, way_fixture, CENTER_LAT,
    CENTER_LON,
};
use crate::undo::UndoableAction;
use gpui::{px, size, TestAppContext};
use osm_gpui::coordinates::LatLon;
use osm_gpui::osm_upload;
use osm_gpui::viewport::Viewport;

fn server_node(version: i32, lat: f64) -> osm_gpui::osm_api::ServerElement {
    let mut data = way_fixture(CENTER_LAT, CENTER_LON);
    data.ways.clear();
    data.nodes.retain(|id, _| *id == 1);
    let node = data.nodes.get_mut(&1).unwrap();
    node.version = version;
    node.lat = lat;
    node.tags.insert("shop".to_string(), "bakery".to_string());
    osm_gpui::osm_api::ServerElement::Present(data)
}

#[gpui::test]
fn keep_mine_moves_the_version_and_keeps_edit_and_undo(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    use osm_gpui::ui::upload_dialog::ConflictAction;
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            tag_node_one(view);
            let note = view
                .apply_resolution(ConflictAction::KeepMine, Node, 1, server_node(5, 40.0))
                .unwrap();
            assert!(note.contains("Node 1"), "{note}");
            let layer_id = view.active_layer.unwrap();
            let diff = view
                .layer_manager
                .find_layer(layer_id)
                .unwrap()
                .as_osm()
                .unwrap()
                .diff_for_upload();
            assert_eq!(diff.modified_nodes.len(), 1);
            assert_eq!(diff.modified_nodes[0].version, 5);
            assert_eq!(
                diff.modified_nodes[0]
                    .tags
                    .get("amenity")
                    .map(String::as_str),
                Some("cafe")
            );
            assert_eq!(view.undo_stack.actions.len(), 1, "history untouched");
        })
        .unwrap();
}

#[gpui::test]
fn keep_mine_refuses_an_object_the_server_deleted(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    use osm_gpui::ui::upload_dialog::ConflictAction;
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            tag_node_one(view);
            let err = view
                .apply_resolution(
                    ConflictAction::KeepMine,
                    Node,
                    1,
                    osm_gpui::osm_api::ServerElement::Gone,
                )
                .unwrap_err();
            assert!(err.contains("deleted on the server"), "{err}");
            assert_eq!(pending(view), (0, 1, 0), "nothing changed");
            assert_eq!(view.undo_stack.actions.len(), 1);
        })
        .unwrap();
}

#[gpui::test]
fn use_theirs_replaces_the_object_and_keeps_unrelated_undo_history(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    use osm_gpui::ui::upload_dialog::ConflictAction;
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            tag_node_one(view);
            // An unrelated earlier edit is on the stack too: it stays.
            view.undo_stack.push(UndoableAction::reverse_ways(
                view.active_layer.unwrap(),
                vec![99],
            ));
            let note = view
                .apply_resolution(ConflictAction::UseTheirs, Node, 1, server_node(5, 40.0))
                .unwrap();
            assert!(note.contains("Replaced Node 1"), "{note}");
            assert!(note.contains("Undo steps that involved it"), "{note}");
            assert_eq!(view.undo_stack.actions.len(), 1, "only the unrelated step");
            assert_eq!(view.undo_stack.cursor, 1);
            assert!(matches!(
                view.undo_stack.actions[0],
                UndoableAction::Edit {
                    label: crate::undo::EditLabel::Reversed(_),
                    ..
                }
            ));
            assert_eq!(pending(view), (0, 0, 0), "the change to it is gone");
            let layer_id = view.active_layer.unwrap();
            let tags = editable(view)
                .feature_tags(&osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: Node,
                    id: 1,
                })
                .unwrap();
            assert_eq!(tags, vec![("shop".to_string(), "bakery".to_string())]);
        })
        .unwrap();
}

#[gpui::test]
fn use_theirs_for_a_deleted_object_removes_it_and_its_selection(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    use osm_gpui::ui::upload_dialog::ConflictAction;
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            editable(view).set_tag(Way, 10, "highway", "path").unwrap();
            select(view, Way, &[10]);
            assert_eq!(view.selected.len(), 1);
            let note = view
                .apply_resolution(
                    ConflictAction::UseTheirs,
                    Way,
                    10,
                    osm_gpui::osm_api::ServerElement::Gone,
                )
                .unwrap();
            assert!(note.contains("Way 10"), "{note}");
            assert!(editable(view).way_node_ids(10).is_none());
            assert!(view.selected.is_empty(), "no dangling selection");
            let _ = layer_id;
            assert_eq!(pending(view), (0, 0, 0));
            // Asking again: the object is gone, so there is nothing to do.
            let err = view
                .apply_resolution(
                    ConflictAction::UseTheirs,
                    Way,
                    10,
                    osm_gpui::osm_api::ServerElement::Gone,
                )
                .unwrap_err();
            assert!(err.contains("no longer in the loaded data"), "{err}");
        })
        .unwrap();
}

#[gpui::test]
fn drop_my_change_forgets_the_local_delete(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    use osm_gpui::ui::upload_dialog::ConflictAction;
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            let snapshot = editable(view).delete_feature(Way, 10).unwrap();
            view.undo_stack
                .push(UndoableAction::delete_feature(layer_id, snapshot));
            assert_eq!(pending(view), (0, 0, 1));
            let note = view
                .apply_resolution(
                    ConflictAction::DropMyChange,
                    Way,
                    10,
                    osm_gpui::osm_api::ServerElement::Gone,
                )
                .unwrap();
            assert!(note.contains("Forgot your delete of Way 10"), "{note}");
            assert_eq!(pending(view), (0, 0, 0));
            assert!(view.undo_stack.actions.is_empty());
        })
        .unwrap();
}

#[gpui::test]
fn show_selects_the_object_and_closes_the_dialog(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    use osm_gpui::ui::upload_dialog::ConflictAction;
    let window = setup(cx);
    window
        .update(cx, |view, _window, cx| {
            let conflict = osm_upload::UploadConflict::parse(
                409,
                "Version mismatch: Provided 1, server had: 2 of Node 2",
            );
            view.viewport = Viewport::new(LatLon::new(10.0, 10.0), 5.0, size(px(800.0), px(600.0)));
            view.handle_conflict_action(ConflictAction::Show, conflict, cx);
            assert_eq!(view.selected.len(), 1);
            assert_eq!((view.selected[0].kind, view.selected[0].id), (Node, 2));
            // Zoomed in on node 2.
            let p = view
                .viewport
                .geo_to_screen(LatLon::new(CENTER_LAT, CENTER_LON + 0.001));
            assert!(p.x > px(0.0) && p.x < px(800.0) && p.y > px(0.0) && p.y < px(600.0));
            assert!(view.upload_dialog.is_none());
            assert!(status_text(view).unwrap().contains("Node 2"));
        })
        .unwrap();
}
