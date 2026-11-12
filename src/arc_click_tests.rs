//! Arc mode: three clicks (start, end, bulge) create one open way whose
//! nodes lie on a circle, as a single undo step.

use super::*;
use gpui::{point, size, TestAppContext};
use osm_gpui::osm::OsmData;

const LAT: f64 = 40.0;
const LON: f64 = -74.0;

fn setup(cx: &mut TestAppContext) -> (gpui::WindowHandle<MapViewer>, LayerId) {
    setup_with(
        cx,
        OsmData {
            nodes: Default::default(),
            ways: Default::default(),
            relations: Vec::new(),
            bounds: Vec::new(),
        },
    )
}

fn setup_with(cx: &mut TestAppContext, data: OsmData) -> (gpui::WindowHandle<MapViewer>, LayerId) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    let mut id = LayerId(0);
    let id_ref = &mut id;
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

            view.handle_arc_click(start, false);
            assert!(view.arc_progress.is_some_and(|p| p.end.is_none()));
            view.handle_arc_click(end, false);
            assert!(view.arc_progress.is_some_and(|p| p.end.is_some()));
            assert_eq!(counts(view, layer), (0, 0), "nothing is created early");

            view.handle_arc_click(bulge, false);
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
            view.handle_arc_click(c + point(px(-100.0), px(0.0)), false);
            view.handle_arc_click(c + point(px(100.0), px(0.0)), false);
            // On the chord itself: no circle, so the arc stays pending.
            view.handle_arc_click(c, false);
            assert!(view.arc_progress.is_some_and(|p| p.end.is_some()));
            assert_eq!(counts(view, layer), (0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn endpoints_snap_onto_a_way_and_are_spliced_into_it(cx: &mut TestAppContext) {
    use osm_gpui::osm::{OsmNode, OsmWay};
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
        nodes: [node(1, LON - 0.002), node(2, LON + 0.002)]
            .into_iter()
            .collect(),
        ways: [(
            10,
            OsmWay {
                id: 10,
                nodes: vec![1, 2],
                version: 1,
                tags: Default::default(),
            },
        )]
        .into_iter()
        .collect(),
        relations: Vec::new(),
        bounds: Vec::new(),
    };
    let (window, layer) = setup_with(cx, data);
    window
        .update(cx, |view, w, cx| {
            let c = view.viewport.geo_to_screen(LAT, LON);
            // Start and end a few pixels off the horizontal way; the bulge
            // is far from it.
            view.handle_arc_click(c + point(px(-100.0), px(4.0)), false);
            view.handle_arc_click(c + point(px(100.0), px(-4.0)), false);
            view.handle_arc_click(c + point(px(0.0), px(-60.0)), false);

            let editable = view
                .layer_manager
                .find_layer(layer)
                .unwrap()
                .as_editable()
                .unwrap();
            let host = editable.way_node_ids(10).unwrap();
            assert_eq!(host.len(), 4, "both endpoints are spliced into the way");
            let [sel] = view.selected.as_slice() else {
                panic!("the new way should be selected");
            };
            let arc = editable.way_node_ids(sel.id).unwrap();
            assert!(host.contains(arc.first().unwrap()));
            assert!(host.contains(arc.last().unwrap()));
            // The spliced nodes lie exactly on the way.
            for id in [arc[0], arc[arc.len() - 1]] {
                assert!((editable.node_lat_lon(id).unwrap().0 - LAT).abs() < 1e-9);
            }

            // Ctrl/Alt turns snapping off: nothing is spliced.
            view.on_undo(&Undo, w, cx);
            view.on_undo(&Undo, w, cx);
            view.on_undo(&Undo, w, cx);
            let host_len = |view: &MapViewer| {
                view.layer_manager
                    .find_layer(layer)
                    .unwrap()
                    .as_editable()
                    .unwrap()
                    .way_node_ids(10)
                    .unwrap()
                    .len()
            };
            assert_eq!(host_len(view), 2, "undo removes the splices too");
            view.handle_arc_click(c + point(px(-100.0), px(4.0)), true);
            view.handle_arc_click(c + point(px(100.0), px(-4.0)), true);
            view.handle_arc_click(c + point(px(0.0), px(-60.0)), true);
            assert_eq!(host_len(view), 2);
        })
        .unwrap();
}
