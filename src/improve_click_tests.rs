//! Improve Way mode: with a way selected, a click moves the nearest node to
//! the cursor, a modifier-click inserts a node along the way, and a
//! Shift-click deletes the nearest node. Each is one undo step.

use super::*;
use gpui::{point, size, TestAppContext};
use osm_gpui::osm::{OsmData, OsmNode, OsmWay};

const LAT: f64 = 40.0;
const LON: f64 = -74.0;

fn setup(cx: &mut TestAppContext) -> (gpui::WindowHandle<MapViewer>, LayerId) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    let mut id = LayerId(0);
    let id_ref = &mut id;
    let node = |id: i64, lon: f64| {
        (
            id,
            OsmNode {
                id,
                lat: LAT,
                lon,
                version: 1,
                tags: Default::default(),
            },
        )
    };
    let data = OsmData {
        nodes: [
            node(1, LON - 0.0015),
            node(2, LON - 0.0005),
            node(3, LON + 0.0005),
            node(4, LON + 0.0015),
        ]
        .into_iter()
        .collect(),
        ways: [(
            10,
            OsmWay {
                id: 10,
                nodes: vec![1, 2, 3, 4],
                version: 1,
                tags: Default::default(),
            },
        )]
        .into_iter()
        .collect(),
        relations: Vec::new(),
        bounds: Vec::new(),
    };
    window
        .update(cx, |view, _w, _cx| {
            view.viewport = Viewport::new(LAT, LON, 18.0, size(px(800.0), px(600.0)));
            let layer_id = view.layer_manager.alloc_id();
            view.layer_manager
                .add_layer(Box::new(OsmLayer::new_with_data(
                    layer_id,
                    "data".to_string(),
                    Arc::new(data),
                )));
            view.active_layer = Some(layer_id);
            view.mode = EditMode::Improve;
            *id_ref = layer_id;
        })
        .unwrap();
    (window, id)
}

fn select_way(view: &mut MapViewer, layer: LayerId) {
    view.selected = vec![osm_gpui::selection::FeatureRef {
        layer_id: layer,
        kind: osm_gpui::selection::FeatureKind::Way,
        id: 10,
    }];
}

fn way_nodes(view: &MapViewer, layer: LayerId) -> Vec<i64> {
    view.layer_manager
        .find_layer(layer)
        .unwrap()
        .as_editable()
        .unwrap()
        .way_node_ids(10)
        .unwrap()
}

fn lat_lon(view: &MapViewer, layer: LayerId, id: i64) -> Option<(f64, f64)> {
    view.layer_manager
        .find_layer(layer)
        .unwrap()
        .as_editable()
        .unwrap()
        .node_lat_lon(id)
}

#[gpui::test]
fn a_plain_click_moves_the_nearest_node_to_the_cursor(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx);
    window
        .update(cx, |view, w, cx| {
            select_way(view, layer);
            let c = view.viewport.geo_to_screen(LAT, LON);
            // Nearest to node 2 (about 47 px left of centre).
            let click = c + point(px(-40.0), px(30.0));
            view.handle_improve_click(click, cx);

            assert_eq!(way_nodes(view, layer), vec![1, 2, 3, 4]);
            let (lat, lon) = lat_lon(view, layer, 2).unwrap();
            let p = view.viewport.geo_to_screen(lat, lon);
            assert!((p.x - click.x).to_f64().abs() < 0.5 && (p.y - click.y).to_f64().abs() < 0.5);
            assert_eq!(view.undo_stack.cursor, 1);
            assert_eq!(view.selected.len(), 1, "the way stays selected");

            view.on_undo(&Undo, w, cx);
            assert_eq!(lat_lon(view, layer, 2).unwrap().0, LAT);
        })
        .unwrap();
}

#[gpui::test]
fn the_add_modifier_inserts_a_node_on_the_way(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx);
    window
        .update(cx, |view, w, cx| {
            select_way(view, layer);
            view.last_modifiers = gpui::Modifiers {
                control: true,
                ..Default::default()
            };
            let c = view.viewport.geo_to_screen(LAT, LON);
            view.handle_improve_click(c + point(px(0.0), px(40.0)), cx);

            let nodes = way_nodes(view, layer);
            assert_eq!(nodes.len(), 5);
            // Between nodes 2 and 3, on the line (not under the cursor).
            assert_eq!((nodes[1], nodes[3]), (2, 3));
            let (lat, _) = lat_lon(view, layer, nodes[2]).unwrap();
            assert!((lat - LAT).abs() < 1e-9);

            view.on_undo(&Undo, w, cx);
            assert_eq!(way_nodes(view, layer), vec![1, 2, 3, 4]);
        })
        .unwrap();
}

#[gpui::test]
fn shift_deletes_the_nearest_node(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx);
    window
        .update(cx, |view, w, cx| {
            select_way(view, layer);
            view.last_modifiers = gpui::Modifiers {
                shift: true,
                ..Default::default()
            };
            let c = view.viewport.geo_to_screen(LAT, LON);
            view.handle_improve_click(c + point(px(50.0), px(10.0)), cx);

            assert_eq!(way_nodes(view, layer), vec![1, 2, 4]);
            assert!(lat_lon(view, layer, 3).is_none());
            assert_eq!(view.undo_stack.cursor, 1, "one undo step");
            assert_eq!(view.selected.len(), 1, "the way stays selected");

            view.on_undo(&Undo, w, cx);
            assert_eq!(way_nodes(view, layer), vec![1, 2, 3, 4]);
            assert!(lat_lon(view, layer, 3).is_some());
        })
        .unwrap();
}

#[gpui::test]
fn without_a_selected_way_a_click_picks_the_way_under_it(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx);
    window
        .update(cx, |view, _w, cx| {
            let c = view.viewport.geo_to_screen(LAT, LON);
            view.handle_improve_click(c + point(px(0.0), px(2.0)), cx);
            assert_eq!(view.improve_target(), Some((layer, 10)));
            assert_eq!(view.undo_stack.cursor, 0, "picking edits nothing");
        })
        .unwrap();
}
