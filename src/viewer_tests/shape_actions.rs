//! Split, merge, join, rotate, extrude, align, straighten and other geometry edits.

use super::{
    long_way_nodes, node_position, pairs, pending, redo_step, select, setup, setup_network,
    tags_of, undo_step, way_nodes, ways_at_node,
};
use crate::{ExtrudeDrag, ExtrudeStyle, MapViewer};
use gpui::{point, px, TestAppContext};
use osm_gpui::coordinates::LatLon;
use osm_gpui::layers::LayerId;
use osm_gpui::transfer::{TransferHandle, TransferKind, TransferPhase};

#[gpui::test]
fn split_way_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            select(view, Node, &[4]);
            view.run_selection_action("split", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            let split_users = ways_at_node(view, 4);
            assert_eq!(split_users.len(), 4);
            let new: Vec<i64> = split_users.iter().copied().filter(|&id| id < 0).collect();
            assert_eq!(way_nodes(view, layer_id, 101), vec![3, 4]);
            assert_eq!(pending(view), (2, 2, 0));
            let new_tags: Vec<_> = new.iter().map(|&id| tags_of(view, Way, id)).collect();
            // The node stays selected.
            assert_eq!(view.selected.len(), 1);
            assert_eq!(view.selected[0].id, 4);

            undo_step(view);
            assert_eq!(way_nodes(view, layer_id, 101), vec![3, 4, 5]);
            assert_eq!(way_nodes(view, layer_id, 103), vec![8, 4, 9]);
            assert_eq!(ways_at_node(view, 4), vec![101, 103]);
            for &id in &new {
                assert!(way_nodes(view, layer_id, id).is_empty());
            }
            assert_eq!(pending(view), (0, 0, 0));

            // Redo recreates the same ways under the same ids.
            redo_step(view);
            assert_eq!(ways_at_node(view, 4), split_users);
            assert_eq!(way_nodes(view, layer_id, 101), vec![3, 4]);
            let redone_tags: Vec<_> = new.iter().map(|&id| tags_of(view, Way, id)).collect();
            assert_eq!(redone_tags, new_tags);
            assert_eq!(pending(view), (2, 2, 0));
            undo_step(view);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn merge_nodes_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            // Node 4 is a crossing shared by ways 101 and 103; merge it
            // into the untagged node 8 at the end of way 103.
            select(view, Node, &[8, 4]);
            view.run_selection_action("merge", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(way_nodes(view, layer_id, 101), vec![3, 8, 5]);
            assert_eq!(way_nodes(view, layer_id, 103), vec![8, 9]);
            assert_eq!(pending(view), (0, 3, 1));
            assert_eq!(tags_of(view, Node, 8), pairs(&[("highway", "crossing")]));
            assert_eq!(view.selected.len(), 1);
            assert_eq!(view.selected[0].id, 8);

            undo_step(view);
            assert_eq!(way_nodes(view, layer_id, 101), vec![3, 4, 5]);
            assert_eq!(way_nodes(view, layer_id, 103), vec![8, 4, 9]);
            assert_eq!(pending(view), (0, 0, 0));
            assert_eq!(tags_of(view, Node, 4), pairs(&[("highway", "crossing")]));
            assert!(tags_of(view, Node, 8).is_empty());

            redo_step(view);
            assert_eq!(way_nodes(view, layer_id, 101), vec![3, 8, 5]);
            assert_eq!(way_nodes(view, layer_id, 103), vec![8, 9]);
            assert_eq!(pending(view), (0, 3, 1));
            undo_step(view);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn align_and_distribute_nodes_are_one_undo_step_each_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let original: Vec<(f64, f64)> = [1, 2, 4, 5]
                .iter()
                .map(|&id| node_position(view, id))
                .collect();
            select(view, Node, &[1, 2, 4, 5]);
            view.run_selection_action("align", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            let aligned: Vec<(f64, f64)> = [1, 2, 4, 5]
                .iter()
                .map(|&id| node_position(view, id))
                .collect();
            assert_ne!(aligned, original);
            view.run_selection_action("distribute", cx);
            assert_eq!(view.undo_stack.actions.len(), 2);
            let spaced: Vec<(f64, f64)> = [1, 2, 4, 5]
                .iter()
                .map(|&id| node_position(view, id))
                .collect();
            assert_ne!(spaced, aligned);

            undo_step(view);
            let now: Vec<(f64, f64)> = [1, 2, 4, 5]
                .iter()
                .map(|&id| node_position(view, id))
                .collect();
            assert_eq!(now, aligned);
            undo_step(view);
            let now: Vec<(f64, f64)> = [1, 2, 4, 5]
                .iter()
                .map(|&id| node_position(view, id))
                .collect();
            assert_eq!(now, original);
            redo_step(view);
            redo_step(view);
            let now: Vec<(f64, f64)> = [1, 2, 4, 5]
                .iter()
                .map(|&id| node_position(view, id))
                .collect();
            assert_eq!(now, spaced);
        })
        .unwrap();
}

#[gpui::test]
fn rotate_follows_the_pointer_and_commits_one_undo_step(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let ids = [1, 2, 3];
            let original: Vec<(f64, f64)> = ids.iter().map(|&id| node_position(view, id)).collect();
            select(view, Way, &[100]);
            view.run_selection_action("rotate", cx);
            let center = view
                .pointer
                .rotate_session
                .as_ref()
                .expect("session started")
                .center;
            let (cx_px, cy_px) = view.rotate_center_px(center);
            let at = |dx: f64, dy: f64| {
                gpui::point(gpui::px((cx_px + dx) as f32), gpui::px((cy_px + dy) as f32))
            };
            // The pointer starts east of the centre ...
            view.pointer.last_mouse_pos = Some(at(100.0, 0.0));
            view.pointer.rotate_session.as_mut().unwrap().start_angle = Some(
                osm_gpui::rotate::pointer_angle((cx_px, cy_px), (cx_px + 100.0, cy_px)),
            );
            // ... and circles a quarter turn clockwise (to the south).
            view.update_rotate(at(0.0, 100.0), false);
            assert!((view.rotate_degrees().unwrap() - 90.0).abs() < 1e-6);
            let during: Vec<(f64, f64)> = ids.iter().map(|&id| node_position(view, id)).collect();
            assert_ne!(during, original);
            // Nothing is on the undo stack until it is applied.
            assert_eq!(view.undo_stack.actions.len(), 0);
            view.finish_rotate();
            assert!(view.pointer.rotate_session.is_none());
            assert_eq!(view.undo_stack.actions.len(), 1);
            let done: Vec<(f64, f64)> = ids.iter().map(|&id| node_position(view, id)).collect();
            assert_eq!(done, during);

            undo_step(view);
            let now: Vec<(f64, f64)> = ids.iter().map(|&id| node_position(view, id)).collect();
            assert_eq!(now, original);
            redo_step(view);
            let now: Vec<(f64, f64)> = ids.iter().map(|&id| node_position(view, id)).collect();
            assert_eq!(now, done);
        })
        .unwrap();
}

#[gpui::test]
fn rotate_snaps_with_shift_and_cancel_restores_everything(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let ids = [1, 2, 4];
            let original: Vec<(f64, f64)> = ids.iter().map(|&id| node_position(view, id)).collect();
            select(view, Node, &ids);
            view.run_selection_action("rotate", cx);
            let center = view.pointer.rotate_session.as_ref().unwrap().center;
            let (cx_px, cy_px) = view.rotate_center_px(center);
            view.pointer.rotate_session.as_mut().unwrap().start_angle = Some(
                osm_gpui::rotate::pointer_angle((cx_px, cy_px), (cx_px + 100.0, cy_px)),
            );
            // About 20 degrees clockwise snaps to 15.
            let a = 20f64.to_radians();
            let pointer = gpui::point(
                gpui::px((cx_px + 100.0 * a.cos()) as f32),
                gpui::px((cy_px + 100.0 * a.sin()) as f32),
            );
            view.update_rotate(pointer, true);
            assert!((view.rotate_degrees().unwrap() - 15.0).abs() < 1e-6);
            view.update_rotate(pointer, false);
            assert!((view.rotate_degrees().unwrap() - 20.0).abs() < 1e-2);

            assert!(view.cancel_rotate());
            assert!(view.pointer.rotate_session.is_none());
            let now: Vec<(f64, f64)> = ids.iter().map(|&id| node_position(view, id)).collect();
            assert_eq!(now, original);
            assert_eq!(view.undo_stack.actions.len(), 0);
        })
        .unwrap();
}

#[gpui::test]
fn rotate_is_refused_for_a_single_node(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            select(view, Node, &[1]);
            view.run_selection_action("rotate", cx);
            assert!(view.pointer.rotate_session.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn align_is_refused_during_an_upload_and_for_too_few_nodes(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            select(view, Node, &[1, 2]);
            view.run_selection_action("align", cx);
            assert!(view.undo_stack.actions.is_empty());
            select(view, Node, &[1, 2, 4, 5]);
            view.transfer = Some(TransferHandle::new(
                TransferKind::Upload,
                TransferPhase::OpeningChangeset,
            ));
            assert!(view.edits_blocked(cx));
            assert!(view.undo_stack.actions.is_empty());
        })
        .unwrap();
}

#[gpui::test]
fn straighten_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            let on_line = node_position(view, 2);
            let off_line = (on_line.0 + 0.00005, on_line.1);
            view.layer_manager
                .find_layer_mut(layer_id)
                .unwrap()
                .as_editable_mut()
                .unwrap()
                .commit_node_moves(&[(2, off_line.0, off_line.1)])
                .unwrap();
            select(view, Way, &[100]);
            view.run_selection_action("straighten", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            let straightened = node_position(view, 2);
            // Moved back to the line through nodes 1 and 3 (which also
            // slides it a little along it, since the line is diagonal).
            assert_ne!(straightened, off_line);
            assert!((straightened.0 - on_line.0).abs() < 1e-4);

            undo_step(view);
            assert_eq!(node_position(view, 2), off_line);
            redo_step(view);
            assert_eq!(node_position(view, 2), straightened);
            // A curved way is refused with the reason and no undo step.
            select(view, Way, &[101]);
            view.run_selection_action("straighten", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("Can't straighten: the way bends more than 10% of its length")
            );
        })
        .unwrap();
}

#[gpui::test]
fn parallel_way_asks_first_then_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            select(view, Way, &[100]);
            // Running the action only asks for the offset.
            view.run_selection_action("parallel", cx);
            assert!(view.dialogs.pending_parallel_dialog);
            assert!(view.undo_stack.actions.is_empty());
            assert_eq!(pending(view), (0, 0, 0));
            view.dialogs.pending_parallel_dialog = false;

            view.create_parallel_ways(osm_gpui::parallel::ParallelParams::default());
            assert_eq!(view.undo_stack.actions.len(), 1);
            // Three new nodes and the new way; the copy is selected.
            assert_eq!(pending(view), (4, 0, 0));
            assert_eq!(view.selected.len(), 1);
            let copy = view.selected[0].id;
            assert!(copy < 0);
            assert_eq!(way_nodes(view, layer_id, copy).len(), 3);
            assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3]);

            undo_step(view);
            assert_eq!(pending(view), (0, 0, 0));
            assert!(way_nodes(view, layer_id, copy).is_empty());
            redo_step(view);
            assert_eq!(pending(view), (4, 0, 0));
            assert_eq!(way_nodes(view, layer_id, copy).len(), 3);
        })
        .unwrap();
}

#[gpui::test]
fn parallel_way_is_refused_during_an_upload(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            select(view, Way, &[100]);
            view.transfer = Some(TransferHandle::new(
                TransferKind::Upload,
                TransferPhase::OpeningChangeset,
            ));
            view.run_selection_action("parallel", cx);
            assert!(!view.dialogs.pending_parallel_dialog);
            view.create_parallel_ways(osm_gpui::parallel::ParallelParams::default());
            assert!(view.undo_stack.actions.is_empty());
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn inapplicable_action_explains_itself_and_changes_nothing(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            view.set_selection(vec![osm_gpui::selection::FeatureRef {
                layer_id,
                kind: osm_gpui::selection::FeatureKind::Way,
                id: 10,
            }]);
            view.run_selection_action("circularize", cx);
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("Select a closed way to circularize it")
            );
            assert!(view.undo_stack.actions.is_empty());
            assert!(!view
                .layer_manager
                .find_layer(layer_id)
                .unwrap()
                .as_osm()
                .unwrap()
                .is_modified());
        })
        .unwrap();
}

#[gpui::test]
fn deleting_several_features_is_one_undo_step(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            // Way first (nodes are still referenced until it goes).
            view.set_selection(vec![
                osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Way,
                    id: 10,
                },
                osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Node,
                    id: 1,
                },
            ]);
            view.run_selection_action("delete", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert!(view.selected.is_empty());
            assert!(way_nodes(view, layer_id, 10).is_empty());

            let action = view.undo_stack.undo().unwrap();
            view.apply_undo_action(&action, false);
            assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
            let node = view
                .layer_manager
                .find_layer(layer_id)
                .unwrap()
                .as_editable()
                .unwrap()
                .node_lat_lon(1);
            assert!(node.is_some());
        })
        .unwrap();
}

/// A drag of one segment of building 108 (a triangle 14-15-16) along
/// that segment's screen normal, `dist` pixels, plus the drag record.
fn extrude_triangle_drag(
    view: &MapViewer,
    layer_id: LayerId,
    style: ExtrudeStyle,
    dist: f32,
) -> (ExtrudeDrag, gpui::Point<gpui::Pixels>) {
    let screen = |id: i64| {
        let (lat, lon) = view
            .layer_manager
            .find_layer(layer_id)
            .and_then(|l| l.as_editable())
            .and_then(|e| e.node_lat_lon(id))
            .unwrap();
        view.viewport.geo_to_screen(LatLon::new(lat, lon))
    };
    let (a, b) = (screen(14), screen(15));
    let mid = point((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    let (dx, dy) = (f32::from(b.x - a.x), f32::from(b.y - a.y));
    let len = dx.hypot(dy);
    let up = point(mid.x + px(-dy / len * dist), mid.y + px(dx / len * dist));
    let drag = ExtrudeDrag {
        layer: layer_id,
        way_id: 108,
        node_a: 14,
        node_b: 15,
        segment: 0,
        style,
        down: mid,
    };
    (drag, up)
}

#[gpui::test]
fn extrude_reshapes_the_way_in_one_undoable_step(cx: &mut TestAppContext) {
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            let before_pending = pending(view);
            let (drag, up) = extrude_triangle_drag(view, layer_id, ExtrudeStyle::Reshape, 60.0);
            view.commit_extrude_reshape(&drag, up);
            // A triangle's neighbours are not along the drag: 2 new nodes
            // are spliced in between 14 and 15; the way keeps its id.
            let after = way_nodes(view, layer_id, 108);
            assert_eq!(after.len(), 6);
            assert_eq!((after[0], after[3], after[4], after[5]), (14, 15, 16, 14));
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_ne!(pending(view), before_pending);

            undo_step(view);
            assert_eq!(way_nodes(view, layer_id, 108), vec![14, 15, 16, 14]);
            assert_eq!(pending(view), before_pending);

            redo_step(view);
            assert_eq!(way_nodes(view, layer_id, 108), after);
        })
        .unwrap();
}

#[gpui::test]
fn extrude_move_translates_without_new_nodes_and_undoes(cx: &mut TestAppContext) {
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            let before_pending = pending(view);
            let old = |view: &MapViewer, id| {
                view.layer_manager
                    .find_layer(layer_id)
                    .and_then(|l| l.as_editable())
                    .and_then(|e| e.node_lat_lon(id))
                    .unwrap()
            };
            let (a0, b0) = (old(view, 14), old(view, 15));
            let (drag, up) = extrude_triangle_drag(view, layer_id, ExtrudeStyle::Move, 60.0);
            view.commit_extrude_reshape(&drag, up);
            assert_eq!(way_nodes(view, layer_id, 108), vec![14, 15, 16, 14]);
            assert_ne!(old(view, 14), a0);
            assert_ne!(old(view, 15), b0);
            undo_step(view);
            assert_eq!((old(view, 14), old(view, 15)), (a0, b0));
            assert_eq!(pending(view), before_pending);
        })
        .unwrap();
}

#[gpui::test]
fn extrude_refuses_to_grow_a_full_way(cx: &mut TestAppContext) {
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            let mut nodes = long_way_nodes(2000);
            nodes[0] = 14;
            nodes[1] = 15;
            view.layer_manager
                .find_layer_mut(layer_id)
                .and_then(|l| l.as_editable_mut())
                .unwrap()
                .set_way_nodes(108, nodes)
                .unwrap();
            let (drag, up) = extrude_triangle_drag(view, layer_id, ExtrudeStyle::Reshape, 60.0);
            view.commit_extrude_reshape(&drag, up);
            assert_eq!(way_nodes(view, layer_id, 108).len(), 2000);
            assert!(view.undo_stack.actions.is_empty());
            assert!(view.status_message.is_some());
        })
        .unwrap();
}

#[gpui::test]
fn join_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            select(view, Way, &[100, 101]);
            view.run_selection_action("join", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3, 4, 5]);
            assert!(way_nodes(view, layer_id, 101).is_empty());
            assert_eq!(pending(view), (0, 1, 1));
            // The joined way is what stays selected.
            assert_eq!(view.selected.len(), 1);
            assert_eq!(view.selected[0].id, 100);

            undo_step(view);
            assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3]);
            assert_eq!(way_nodes(view, layer_id, 101), vec![3, 4, 5]);
            assert_eq!(pending(view), (0, 0, 0));
            let tags = view
                .layer_manager
                .find_layer(layer_id)
                .unwrap()
                .as_editable()
                .unwrap()
                .feature_tags(&osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: Way,
                    id: 100,
                })
                .unwrap();
            assert!(!tags.iter().any(|(k, _)| k == "name"));

            redo_step(view);
            assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3, 4, 5]);
            assert!(way_nodes(view, layer_id, 101).is_empty());
            assert_eq!(pending(view), (0, 1, 1));
            // Undo once more to prove redo left a sound state.
            undo_step(view);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn refused_join_leaves_no_undo_step_and_shows_why(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            select(view, Way, &[101, 105]);
            view.run_selection_action("join", cx);
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("Can't join: the ways have different values for 'name'")
            );
            assert!(view.undo_stack.actions.is_empty());
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn separate_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            select(view, Node, &[4]);
            view.run_selection_action("separate", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            let separated = way_nodes(view, layer_id, 103);
            let new_id = separated[1];
            assert!(new_id < 0);
            assert_eq!(pending(view), (1, 1, 0));
            // The original node is still the selection.
            assert_eq!(view.selected.len(), 1);
            assert_eq!(view.selected[0].id, 4);

            undo_step(view);
            assert_eq!(way_nodes(view, layer_id, 103), vec![8, 4, 9]);
            let editable = view
                .layer_manager
                .find_layer(layer_id)
                .unwrap()
                .as_editable()
                .unwrap();
            assert_eq!(editable.node_lat_lon(new_id), None);
            assert_eq!(pending(view), (0, 0, 0));

            redo_step(view);
            assert_eq!(way_nodes(view, layer_id, 103), separated);
            assert_eq!(pending(view), (1, 1, 0));
            undo_step(view);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}
