//! Improve Way mode: with a way selected, a click moves the nearest node to
//! the cursor, a modifier-click inserts a node at the cursor into the way, and a
//! Shift-click deletes the nearest node. Each is one undo step.

use super::*;
use crate::test_support::*;
use gpui::{point, TestAppContext};
use osm_gpui::coordinates::LatLon;

/// Way 10 along the viewport's latitude, nodes 1-4 either side of the centre.
fn data() -> OsmData {
    Build::new()
        .node_at(1, LAT, LON - 0.0015)
        .node_at(2, LAT, LON - 0.0005)
        .node_at(3, LAT, LON + 0.0005)
        .node_at(4, LAT, LON + 0.0015)
        .way(10, &[1, 2, 3, 4], &[])
        .done()
}

fn select_way(view: &mut MapViewer, layer: LayerId) {
    view.selected = vec![osm_gpui::selection::FeatureRef {
        layer_id: layer,
        kind: osm_gpui::selection::FeatureKind::Way,
        id: 10,
    }];
}

#[gpui::test]
fn a_plain_click_moves_the_nearest_node_to_the_cursor(cx: &mut TestAppContext) {
    with_osm(cx, data(), EditMode::Improve, |view, w, cx, layer| {
        select_way(view, layer);
        let c = view.viewport.geo_to_screen(LatLon::new(LAT, LON));
        // Nearest to node 2 (about 47 px left of centre).
        let click = c + point(px(-40.0), px(30.0));
        crate::modes::improve::click(view, click, cx);

        assert_eq!(way_node_ids(view, layer, 10).unwrap(), vec![1, 2, 3, 4]);
        let (lat, lon) = node_lat_lon(view, layer, 2).unwrap();
        let p = view.viewport.geo_to_screen(LatLon::new(lat, lon));
        assert!((p.x - click.x).to_f64().abs() < 0.5 && (p.y - click.y).to_f64().abs() < 0.5);
        assert_eq!(view.undo_stack.cursor, 1);
        assert_eq!(view.selected.len(), 1, "the way stays selected");

        view.on_undo(&Undo, w, cx);
        assert_eq!(node_lat_lon(view, layer, 2).unwrap().0, LAT);
    });
}

#[gpui::test]
fn the_add_modifier_inserts_a_node_on_the_way(cx: &mut TestAppContext) {
    with_osm(cx, data(), EditMode::Improve, |view, w, cx, layer| {
        select_way(view, layer);
        view.pointer.last_modifiers = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        let c = view.viewport.geo_to_screen(LatLon::new(LAT, LON));
        let click = c + point(px(0.0), px(40.0));
        crate::modes::improve::click(view, click, cx);

        let nodes = way_node_ids(view, layer, 10).unwrap();
        assert_eq!(nodes.len(), 5);
        // Between nodes 2 and 3 in the way, at the cursor.
        assert_eq!((nodes[1], nodes[3]), (2, 3));
        let (lat, lon) = node_lat_lon(view, layer, nodes[2]).unwrap();
        let p = view.viewport.geo_to_screen(LatLon::new(lat, lon));
        assert!((p.x - click.x).to_f64().abs() < 0.5 && (p.y - click.y).to_f64().abs() < 0.5);

        view.on_undo(&Undo, w, cx);
        assert_eq!(way_node_ids(view, layer, 10).unwrap(), vec![1, 2, 3, 4]);
    });
}

#[gpui::test]
fn shift_deletes_the_nearest_node(cx: &mut TestAppContext) {
    with_osm(cx, data(), EditMode::Improve, |view, w, cx, layer| {
        select_way(view, layer);
        view.pointer.last_modifiers = gpui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let c = view.viewport.geo_to_screen(LatLon::new(LAT, LON));
        crate::modes::improve::click(view, c + point(px(50.0), px(10.0)), cx);

        assert_eq!(way_node_ids(view, layer, 10).unwrap(), vec![1, 2, 4]);
        assert!(node_lat_lon(view, layer, 3).is_none());
        assert_eq!(view.undo_stack.cursor, 1, "one undo step");
        assert_eq!(view.selected.len(), 1, "the way stays selected");

        view.on_undo(&Undo, w, cx);
        assert_eq!(way_node_ids(view, layer, 10).unwrap(), vec![1, 2, 3, 4]);
        assert!(node_lat_lon(view, layer, 3).is_some());
    });
}

#[gpui::test]
fn without_a_selected_way_a_click_picks_the_way_under_it(cx: &mut TestAppContext) {
    with_osm(cx, data(), EditMode::Improve, |view, _w, cx, layer| {
        let c = view.viewport.geo_to_screen(LatLon::new(LAT, LON));
        crate::modes::improve::click(view, c + point(px(0.0), px(2.0)), cx);
        assert_eq!(view.improve_target(), Some((layer, 10)));
        assert_eq!(view.undo_stack.cursor, 0, "picking edits nothing");
    });
}
