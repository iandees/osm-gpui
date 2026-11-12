//! Arc mode: three clicks (start, end, bulge) create one open way whose
//! nodes lie on a circle, as a single undo step.

use super::*;
use gpui::{point, size, TestAppContext};
use osm_gpui::osm::OsmData;

const LAT: f64 = 40.0;
const LON: f64 = -74.0;

fn setup(cx: &mut TestAppContext) -> (gpui::WindowHandle<MapViewer>, LayerId) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    let mut id = LayerId(0);
    let id_ref = &mut id;
    window
        .update(cx, |view, _w, _cx| {
            view.viewport = Viewport::new(LAT, LON, 18.0, size(px(800.0), px(600.0)));
            let layer_id = view.layer_manager.alloc_id();
            let empty = OsmData {
                nodes: Default::default(),
                ways: Default::default(),
                relations: Vec::new(),
                bounds: Vec::new(),
            };
            view.layer_manager
                .add_layer(Box::new(OsmLayer::new_with_data(
                    layer_id,
                    "data".to_string(),
                    Arc::new(empty),
                )));
            view.active_layer = Some(layer_id);
            view.mode = EditMode::Arc;
            *id_ref = layer_id;
        })
        .unwrap();
    (window, id)
}

fn counts(view: &MapViewer, layer: LayerId) -> (usize, usize) {
    let l = view.layer_manager.find_layer(layer).unwrap();
    let osm = l.as_any().downcast_ref::<OsmLayer>().unwrap();
    let data = osm.get_osm_data().unwrap();
    (data.nodes.len(), data.ways.len())
}

#[gpui::test]
fn three_clicks_make_one_arc_and_one_undo_removes_it(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx);
    window
        .update(cx, |view, w, cx| {
            let c = view.viewport.geo_to_screen(LAT, LON);
            // Chord along the horizontal, bulging upward: a shallow arc.
            let start = c + point(px(-100.0), px(0.0));
            let end = c + point(px(100.0), px(0.0));
            let bulge = c + point(px(0.0), px(-60.0));

            view.handle_arc_click(start);
            assert!(view.arc_progress.is_some_and(|p| p.end.is_none()));
            view.handle_arc_click(end);
            assert!(view.arc_progress.is_some_and(|p| p.end.is_some()));
            assert_eq!(counts(view, layer), (0, 0), "nothing is created early");

            view.handle_arc_click(bulge);
            assert!(view.arc_progress.is_none());
            let (nodes, ways) = counts(view, layer);
            assert_eq!(ways, 1);
            assert!(nodes >= 3, "an arc has interior nodes, got {nodes}");
            assert_eq!(view.undo_stack.cursor, 1, "one undo step");

            // The new way is open, untagged and selected.
            let [sel] = view.selected.as_slice() else {
                panic!("the new way should be selected");
            };
            let editable = view
                .layer_manager
                .find_layer(layer)
                .unwrap()
                .as_editable()
                .unwrap();
            let ids = editable.way_node_ids(sel.id).unwrap();
            assert_eq!(ids.len(), nodes);
            assert_ne!(ids.first(), ids.last());

            // Every node sits on one circle (checked on screen, where the
            // arc was constructed) through the three clicks.
            let screen: Vec<(f64, f64)> = ids
                .iter()
                .map(|&id| {
                    let (lat, lon) = editable.node_lat_lon(id).unwrap();
                    let p = view.viewport.geo_to_screen(lat, lon);
                    (p.x.to_f64(), p.y.to_f64())
                })
                .collect();
            let (a, b) = (screen[0], screen[screen.len() - 1]);
            assert!((a.0 - start.x.to_f64()).abs() < 0.5 && (a.1 - start.y.to_f64()).abs() < 0.5);
            assert!((b.0 - end.x.to_f64()).abs() < 0.5 && (b.1 - end.y.to_f64()).abs() < 0.5);
            // Chord 200 px, sagitta 60 px -> radius (100^2 + 60^2) / (2 * 60).
            let radius = (100.0_f64.powi(2) + 60.0_f64.powi(2)) / 120.0;
            let center = (c.x.to_f64(), c.y.to_f64() - 60.0 + radius);
            for p in &screen {
                let d = ((p.0 - center.0).powi(2) + (p.1 - center.1).powi(2)).sqrt();
                assert!((d - radius).abs() < 0.5, "off the circle by {}", d - radius);
            }

            view.on_undo(&Undo, w, cx);
            assert_eq!(counts(view, layer), (0, 0), "undo removes the whole arc");
        })
        .unwrap();
}

#[gpui::test]
fn a_collinear_third_click_is_ignored(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx);
    window
        .update(cx, |view, _w, _cx| {
            let c = view.viewport.geo_to_screen(LAT, LON);
            view.handle_arc_click(c + point(px(-100.0), px(0.0)));
            view.handle_arc_click(c + point(px(100.0), px(0.0)));
            // On the chord itself: no circle, so the arc stays pending.
            view.handle_arc_click(c);
            assert!(view.arc_progress.is_some_and(|p| p.end.is_some()));
            assert_eq!(counts(view, layer), (0, 0));
        })
        .unwrap();
}
