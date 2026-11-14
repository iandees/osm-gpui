//! Add mode: snapping the first and second click, and extending, limiting and splitting ways.

use super::{
    long_way_nodes, pending, select, setup, setup_network, undo_step, way_nodes, CENTER_LAT,
    CENTER_LON,
};
use crate::{EditMode, ExtrudeDrag, ExtrudeStyle, MapViewer};
use gpui::{point, px, TestAppContext};
use osm_gpui::coordinates::LatLon;

#[gpui::test]
fn first_click_in_add_mode_snaps_onto_existing_way(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);

            // On the line, exactly at the viewport's projected center.
            view.handle_add_click(point(px(400.0), px(300.0)), false);

            let nodes = way_nodes(view, layer_id, 10);
            assert_eq!(nodes.len(), 3, "expected a node spliced in: {:?}", nodes);
            assert_eq!((nodes[0], nodes[2]), (1, 2));
            let new_id = nodes[1];

            // Add mode's own progress continues from the snapped node,
            // not from an implicit continuation of way 10.
            assert!(view.add_progress().is_some());
            assert_eq!(view.add_progress().unwrap().way_id, None);
            assert_eq!(view.add_progress().unwrap().last_node_id, new_id);

            // Undo removes the splice and deletes the node.
            let action = view.undo_stack.undo().expect("expected an undo entry");
            view.apply_undo_action(&action, false);
            assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
            let layer = view.layer_manager.find_layer(layer_id).unwrap();
            assert_eq!(layer.as_editable().unwrap().node_lat_lon(new_id), None);
        })
        .unwrap();
}

#[gpui::test]
fn second_click_snaps_and_folds_with_compound_undo(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();

            // First click: a free node A, far from way 10.
            view.handle_add_click(point(px(50.0), px(50.0)), false);
            let node_a = view.add_progress().unwrap().last_node_id;
            assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);

            // Second click: on way 10's line — snaps a new node B into
            // way 10, and folds B into a brand-new way (A -> B).
            view.handle_add_click(point(px(400.0), px(300.0)), false);

            let nodes10 = way_nodes(view, layer_id, 10);
            assert_eq!(
                nodes10.len(),
                3,
                "expected a node spliced in: {:?}",
                nodes10
            );
            assert_eq!((nodes10[0], nodes10[2]), (1, 2));
            let node_b = nodes10[1];

            let drawn_way_id = view.add_progress().unwrap().way_id.unwrap();
            assert_eq!(
                way_nodes(view, layer_id, drawn_way_id),
                vec![node_a, node_b]
            );

            // Undo reverses both mutations from that single click: the
            // drawn way goes away entirely (it was created by this
            // click), way 10 reverts, and node B is deleted — but node A
            // (created by the *first* click, a separate undo entry) is
            // untouched.
            let action = view.undo_stack.undo().expect("expected an undo entry");
            view.apply_undo_action(&action, false);
            assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
            let layer = view.layer_manager.find_layer(layer_id).unwrap();
            let editable = layer.as_editable().unwrap();
            assert_eq!(editable.node_lat_lon(node_b), None);
            assert!(editable.node_lat_lon(node_a).is_some());
            assert_eq!(editable.way_node_ids(drawn_way_id), None);
        })
        .unwrap();
}

/// Plain vertices are never drawn unless selected, but a click on one in
/// Add mode still connects to it instead of splicing in a new node.
#[gpui::test]
fn second_click_on_an_undrawn_vertex_connects_to_it(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            // Node 1 is untagged and way 10 is not selected: not drawn,
            // not a hit target.
            let on_node_1 = view
                .viewport
                .geo_to_screen(LatLon::new(CENTER_LAT, CENTER_LON - 0.001));
            let hits = view.layer_manager.hit_test_all(&view.viewport, on_node_1);
            assert!(hits.iter().flatten().all(|h| h.feature.id != 1));

            view.handle_add_click(point(px(50.0), px(50.0)), false);
            let node_a = view.add_progress().unwrap().last_node_id;
            view.handle_add_click(on_node_1, false);

            // Way 10 gained no node; the new way runs from A to node 1.
            assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
            let selected = view.selected[0].id;
            assert_eq!(way_nodes(view, layer_id, selected), vec![node_a, 1]);
        })
        .unwrap();
}

#[gpui::test]
fn ctrl_held_bypasses_both_node_and_way_snap(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            let n1_screen = view
                .viewport
                .geo_to_screen(LatLon::new(CENTER_LAT, CENTER_LON - 0.001));

            // First click: a free node A, far from way 10 (Ctrl
            // irrelevant here — nothing nearby to snap to).
            view.handle_add_click(point(px(50.0), px(50.0)), false);
            let node_a = view.add_progress().unwrap().last_node_id;

            // Second click, Ctrl held, exactly on top of existing node 1:
            // without Ctrl this would connect to node 1 (see the "click
            // hits an existing node" path); with Ctrl it must create a
            // brand-new independent node instead.
            view.handle_add_click(n1_screen, true);
            let node_b = view.add_progress().unwrap().last_node_id;
            assert_ne!(node_b, 1, "Ctrl should not have connected to node 1");
            assert_eq!(
                way_nodes(view, layer_id, 10),
                vec![1, 2],
                "way 10 untouched"
            );

            // Third click, Ctrl held, on way 10's line: without Ctrl this
            // would snap into way 10 (as in the sibling tests above);
            // with Ctrl it must stay a free node.
            view.handle_add_click(point(px(400.0), px(300.0)), true);
            let node_c = view.add_progress().unwrap().last_node_id;
            assert_eq!(
                way_nodes(view, layer_id, 10),
                vec![1, 2],
                "Ctrl should not have snapped onto way 10"
            );

            let drawn_way_id = view.add_progress().unwrap().way_id.unwrap();
            assert_eq!(
                way_nodes(view, layer_id, drawn_way_id),
                vec![node_a, node_b, node_c]
            );
        })
        .unwrap();
}

#[gpui::test]
fn click_with_no_way_nearby_falls_back_to_free_node(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();

            // Far from way 10's line — outside the 6px snap tolerance.
            view.handle_add_click(point(px(50.0), px(50.0)), false);

            assert_eq!(
                way_nodes(view, layer_id, 10),
                vec![1, 2],
                "way 10 untouched"
            );
            let new_id = view.add_progress().unwrap().last_node_id;
            let layer = view.layer_manager.find_layer(layer_id).unwrap();
            assert!(layer.as_editable().unwrap().node_lat_lon(new_id).is_some());
        })
        .unwrap();
}

/// Window-independent screen point for a spot well clear of the network.
fn empty_spot(view: &MapViewer, east: f64) -> gpui::Point<gpui::Pixels> {
    view.viewport
        .geo_to_screen(LatLon::new(39.98, -73.98 + east))
}

#[gpui::test]
fn extend_from_a_way_end_appends_with_one_undo_step_per_click(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            select(view, Way, &[100]);
            view.run_selection_action("extend", cx);
            assert_eq!(view.mode(), EditMode::Add);
            let progress = view.add_progress().unwrap();
            assert_eq!((progress.way_id, progress.last_node_id), (Some(100), 3));
            assert!(view.undo_stack.actions.is_empty());
            assert_eq!(pending(view), (0, 0, 0));

            view.handle_add_click(empty_spot(view, 0.0), false);
            let after_one = way_nodes(view, layer_id, 100);
            assert_eq!(&after_one[..3], &[1, 2, 3]);
            assert_eq!(after_one.len(), 4);
            view.handle_add_click(empty_spot(view, 0.001), false);
            let after_two = way_nodes(view, layer_id, 100);
            assert_eq!(&after_two[..4], &after_one[..]);
            assert_eq!(after_two.len(), 5);
            assert_eq!(view.undo_stack.actions.len(), 2);
            assert_eq!(pending(view), (2, 1, 0));
            assert_eq!(view.selected[0].id, 100);

            undo_step(view);
            assert_eq!(way_nodes(view, layer_id, 100), after_one);
            undo_step(view);
            assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3]);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn extend_from_a_start_node_prepends_and_undoes_from_the_front(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            select(view, Node, &[1]);
            view.run_selection_action("extend", cx);
            assert_eq!(view.mode(), EditMode::Add);
            assert!(view.add_progress().unwrap().at_start);

            view.handle_add_click(empty_spot(view, 0.0), false);
            let first = way_nodes(view, layer_id, 100);
            assert_eq!(&first[1..], &[1, 2, 3]);
            view.handle_add_click(empty_spot(view, 0.001), false);
            let second = way_nodes(view, layer_id, 100);
            assert_eq!(&second[2..], &[1, 2, 3]);
            // The newest node leads, and the one before it follows.
            assert_eq!(second[1], first[0]);
            assert_eq!(pending(view), (2, 1, 0));

            // Enter (or Esc) ends the continuation; the way is kept.
            view.set_add_progress(None);

            undo_step(view);
            assert_eq!(way_nodes(view, layer_id, 100), first);
            undo_step(view);
            assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3]);
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn refused_extend_stays_in_select_mode(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Node;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            select(view, Node, &[2]);
            view.run_selection_action("extend", cx);
            assert_eq!(view.mode(), EditMode::Select);
            assert!(view.add_progress().is_none());
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("Select an open way, or the end node of one, to continue drawing it")
            );
        })
        .unwrap();
}

#[gpui::test]
fn add_mode_refuses_to_grow_a_way_past_the_node_limit(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            select(view, Way, &[100]);
            view.run_selection_action("extend", cx);
            let set = |view: &mut MapViewer, len| {
                view.layer_manager
                    .find_layer_mut(layer_id)
                    .and_then(|l| l.as_editable_mut())
                    .unwrap()
                    .set_way_nodes(100, long_way_nodes(len))
                    .unwrap();
            };

            // 1,999 nodes: one more click reaches the limit exactly.
            set(view, 1999);
            view.handle_add_click(empty_spot(view, 0.0), false);
            assert_eq!(way_nodes(view, layer_id, 100).len(), 2000);
            let undo_steps = view.undo_stack.actions.len();

            // At the limit the next click is refused, with a message,
            // and changes nothing (no node, no undo step).
            let before = pending(view);
            view.handle_add_click(empty_spot(view, 0.001), false);
            assert_eq!(way_nodes(view, layer_id, 100).len(), 2000);
            assert_eq!(pending(view), before);
            assert_eq!(view.undo_stack.actions.len(), undo_steps);
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("A way can have at most 2,000 nodes on this server")
            );
        })
        .unwrap();
}

#[gpui::test]
fn inserting_a_node_on_a_full_way_is_refused(cx: &mut TestAppContext) {
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            view.layer_manager
                .find_layer_mut(layer_id)
                .and_then(|l| l.as_editable_mut())
                .unwrap()
                .set_way_nodes(100, long_way_nodes(2000))
                .unwrap();
            let before = pending(view);
            let drag = ExtrudeDrag {
                layer: layer_id,
                way_id: 100,
                node_a: 1,
                node_b: 2,
                segment: 0,
                style: ExtrudeStyle::Reshape,
                down: point(px(0.0), px(0.0)),
            };
            view.insert_node_on_segment(&drag, point(px(400.0), px(300.0)));
            assert_eq!(way_nodes(view, layer_id, 100).len(), 2000);
            assert_eq!(pending(view), before);
            assert!(view.status_message.is_some());
        })
        .unwrap();
}

#[gpui::test]
fn double_click_inserts_the_node_on_the_segment_not_under_the_cursor(cx: &mut TestAppContext) {
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, _cx| {
            let layer_id = view.active_layer.unwrap();
            let (a_geo, b_geo) = {
                let e = view
                    .layer_manager
                    .find_layer(layer_id)
                    .and_then(|l| l.as_editable())
                    .unwrap();
                (e.node_lat_lon(1).unwrap(), e.node_lat_lon(2).unwrap())
            };
            let a = view.viewport.geo_to_screen(LatLon::new(a_geo.0, a_geo.1));
            let b = view.viewport.geo_to_screen(LatLon::new(b_geo.0, b_geo.1));
            // A click 30px off the middle of the segment.
            let (dx, dy) = ((b.x - a.x).to_f64(), (b.y - a.y).to_f64());
            let len = (dx * dx + dy * dy).sqrt();
            let click = point(
                px(((a.x + b.x).to_f64() / 2.0 - dy / len * 30.0) as f32),
                px(((a.y + b.y).to_f64() / 2.0 + dx / len * 30.0) as f32),
            );
            let drag = ExtrudeDrag {
                layer: layer_id,
                way_id: 100,
                node_a: 1,
                node_b: 2,
                segment: 0,
                style: ExtrudeStyle::Reshape,
                down: click,
            };
            view.insert_node_on_segment(&drag, click);
            let nodes = way_nodes(view, layer_id, 100);
            assert_eq!(nodes.len(), 4);
            let new_geo = view
                .layer_manager
                .find_layer(layer_id)
                .and_then(|l| l.as_editable())
                .unwrap()
                .node_lat_lon(nodes[1])
                .unwrap();
            let n = view
                .viewport
                .geo_to_screen(LatLon::new(new_geo.0, new_geo.1));
            let dist = osm_gpui::selection::point_to_segment_distance(n, a, b);
            assert!(dist < 0.5, "new node is {dist}px off the segment");
            assert!((n.x - click.x).to_f64().hypot((n.y - click.y).to_f64()) > 20.0);
        })
        .unwrap();
}
