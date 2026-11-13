//! Which features undo and redo select.

use super::{editable, select, setup_network, tag_node_one};
use crate::undo::UndoableAction;
use crate::{key_bindings_from_settings, AddProgress, MapViewer, Redo, Undo};
use gpui::{point, px, size, TestAppContext};

fn selected_ids(view: &MapViewer, kind: osm_gpui::selection::FeatureKind) -> Vec<i64> {
    let mut ids: Vec<i64> = view
        .selected
        .iter()
        .filter(|f| f.kind == kind)
        .map(|f| f.id)
        .collect();
    ids.sort();
    ids
}

/// Undo and redo select the features the step affected that still exist,
/// for nodes, ways and relations alike: undoing a delete selects the
/// restored feature, redoing the delete leaves nothing selected (it is
/// gone), and a created relation is selected again by redo but dropped
/// from the selection by undo.
#[gpui::test]
fn undo_and_redo_select_the_affected_features_for_every_feature_kind(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Relation, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, window, cx| {
            let layer_id = view.active_layer.unwrap();

            select(view, Way, &[102]);
            view.delete_selected_features(cx);
            assert!(view.selected.is_empty());
            view.on_undo(&Undo, window, cx);
            assert!(editable(view).way_node_ids(102).is_some());
            assert_eq!(selected_ids(view, Way), vec![102]);
            view.on_redo(&Redo, window, cx);
            // The way is gone, so it can't stay selected (the relation
            // that lost it as a member is also part of the step).
            assert!(editable(view).way_node_ids(102).is_none());
            assert!(selected_ids(view, Way).is_empty());
            view.on_undo(&Undo, window, cx);

            select(view, Node, &[7]);
            view.delete_selected_features(cx);
            assert!(view.selected.is_empty());
            view.on_undo(&Undo, window, cx);
            assert!(editable(view).node_lat_lon(7).is_some());
            assert_eq!(selected_ids(view, Node), vec![7]);
            view.on_redo(&Redo, window, cx);
            assert!(editable(view).node_lat_lon(7).is_none());
            assert!(selected_ids(view, Node).is_empty());
            view.on_undo(&Undo, window, cx);

            view.select_relation(layer_id, 900);
            view.delete_selected_features(cx);
            view.on_undo(&Undo, window, cx);
            assert!(editable(view).relation(900).is_some());
            assert_eq!(selected_ids(view, Relation), vec![900]);
            view.on_redo(&Redo, window, cx);
            assert!(view.selected.is_empty());
            view.on_undo(&Undo, window, cx);

            // Creating a relation selects it. Undoing removes it, so the
            // selection must not keep pointing at it (stale selection);
            // redoing selects it again.
            select(view, Way, &[101]);
            view.run_selection_action("new_relation", cx);
            assert_eq!(view.selected.len(), 1);
            assert_eq!(view.selected[0].kind, Relation);
            let created = view_relation_id(view);
            view.on_undo(&Undo, window, cx);
            assert!(editable(view).relation(created).is_none());
            assert!(view.selected.is_empty());
            view.on_redo(&Redo, window, cx);
            assert!(editable(view).relation(created).is_some());
            assert_eq!(selected_ids(view, Relation), vec![created]);
        })
        .unwrap();
}

/// A batch selects the union of what its steps affected (a multi-feature
/// delete is restored as a multi-feature selection), and a reshape
/// selects the reshaped way even when something else was selected.
#[gpui::test]
fn undo_selects_batch_and_reshape_targets(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, window, cx| {
            select(view, Way, &[101, 102]);
            view.delete_selected_features(cx);
            view.on_undo(&Undo, window, cx);
            assert_eq!(selected_ids(view, Way), vec![101, 102]);

            select(view, Way, &[101]);
            view.run_selection_action("reverse", cx);
            select(view, Node, &[7]);
            view.on_undo(&Undo, window, cx);
            assert_eq!(selected_ids(view, Way), vec![101]);
            assert!(selected_ids(view, Node).is_empty());
            select(view, Node, &[7]);
            view.on_redo(&Redo, window, cx);
            assert_eq!(selected_ids(view, Way), vec![101]);
        })
        .unwrap();
}

/// Tag edits and moves select what they changed, and ids that no longer
/// exist are always pruned from the selection.
#[gpui::test]
fn undo_selects_tagged_and_moved_features_and_prunes_stale_ids(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, window, cx| {
            tag_node_one(view);
            select(view, Way, &[101]);
            view.on_undo(&Undo, window, cx);
            assert_eq!(selected_ids(view, Node), vec![1]);
            assert!(selected_ids(view, Way).is_empty());

            let layer_id = view.active_layer.unwrap();
            let (lat, lon) = editable(view).node_lat_lon(1).unwrap();
            editable(view).commit_node_moves(&[(1, lat + 0.001, lon)]);
            view.undo_stack.push(UndoableAction::MoveNodes {
                select: None,
                per_layer: vec![(layer_id, vec![(1, (lat, lon), (lat + 0.001, lon))])],
            });
            // A selection holding a feature that doesn't exist is pruned
            // even though the step didn't name it.
            view.set_selection(vec![osm_gpui::selection::FeatureRef {
                layer_id,
                kind: Way,
                id: 424242,
            }]);
            view.on_undo(&Undo, window, cx);
            assert_eq!(selected_ids(view, Node), vec![1]);
            assert!(selected_ids(view, Way).is_empty());
        })
        .unwrap();
}

/// Undoing and redoing a dragged way's move keeps the way selected, not
/// the nodes that were translated with it.
#[gpui::test]
fn undo_of_a_dragged_way_keeps_the_way_selected(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, window, cx| {
            let layer_id = view.active_layer.unwrap();
            select(view, Way, &[101]);
            let nodes = editable(view).way_node_ids(101).unwrap();
            let (lat, lon) = editable(view).node_lat_lon(nodes[0]).unwrap();
            editable(view).commit_node_moves(&[(nodes[0], lat + 0.001, lon)]);
            view.undo_stack.push(UndoableAction::MoveNodes {
                select: Some(view.selected.clone()),
                per_layer: vec![(layer_id, vec![(nodes[0], (lat, lon), (lat + 0.001, lon))])],
            });
            view.on_undo(&Undo, window, cx);
            assert_eq!(selected_ids(view, Way), vec![101]);
            assert!(selected_ids(view, Node).is_empty());
            view.on_redo(&Redo, window, cx);
            assert_eq!(selected_ids(view, Way), vec![101]);
            assert!(selected_ids(view, Node).is_empty());
        })
        .unwrap();
}

/// The same through the real key binding: Delete a way with the map
/// focused, then cmd-z restores and selects it.
#[gpui::test]
fn cmd_z_after_delete_selects_the_restored_way(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    cx.update(|cx| {
        cx.bind_keys(key_bindings_from_settings(
            &osm_gpui::settings_store::snapshot(),
            cfg!(target_os = "macos"),
        ))
    });
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let root = window.root(vcx).unwrap();
    vcx.draw(
        point(px(0.0), px(0.0)),
        size(
            gpui::AvailableSpace::Definite(px(1200.0)),
            gpui::AvailableSpace::Definite(px(800.0)),
        ),
        |_, _| gpui::AnyView::from(root),
    );
    window
        .update(vcx, |view, _, cx| {
            select(view, Way, &[102]);
            view.delete_selected_features(cx);
            assert!(selected_ids(view, Way).is_empty());
        })
        .unwrap();
    // Undo is bound to `secondary-z`: Cmd on macOS, Ctrl elsewhere.
    vcx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    window
        .update(vcx, |view, _, _| {
            assert!(editable(view).way_node_ids(102).is_some());
            // The way (and the orphan node restored with it): way 102 is a
            // member of relation 900, which the step also touched, but
            // relations are never mixed in.
            assert_eq!(selected_ids(view, Way), vec![102]);
            assert!(selected_ids(view, osm_gpui::selection::FeatureKind::Relation).is_empty());
        })
        .unwrap();

    // The menu path: the Undo action dispatched on the window (what the
    // harness's `action Undo` and the Edit menu do), then redo, then
    // undo again.
    vcx.dispatch_action(Redo);
    window
        .update(vcx, |view, _, _| {
            assert!(editable(view).way_node_ids(102).is_none());
            // Only the relation that lost the way is left to select.
            assert!(selected_ids(view, Way).is_empty());
        })
        .unwrap();
    vcx.dispatch_action(Undo);
    window
        .update(vcx, |view, _, _| {
            // The way, plus the orphan node that was deleted with it.
            assert_eq!(view.selected.len(), 2);
            assert_eq!(selected_ids(view, Way), vec![102]);
        })
        .unwrap();
}

/// Mid-way through drawing in Add mode the selection is left alone apart
/// from pruning, so undo can't disturb the drawing session.
#[gpui::test]
fn undo_while_drawing_leaves_the_selection_alone(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, window, cx| {
            tag_node_one(view);
            select(view, Way, &[101]);
            view.add_progress = Some(AddProgress {
                way_id: None,
                last_node_id: 1,
                at_start: false,
            });
            view.on_undo(&Undo, window, cx);
            assert_eq!(selected_ids(view, Way), vec![101]);
            assert!(selected_ids(view, Node).is_empty());
        })
        .unwrap();
}

/// The id of the relation `new_relation` created (the only negative one).
fn view_relation_id(view: &mut MapViewer) -> i64 {
    editable(view)
        .relation_ids()
        .into_iter()
        .find(|id| *id < 0)
        .expect("a created relation")
}
