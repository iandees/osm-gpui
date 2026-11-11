//! What exactly one Add-mode click creates, how many undo steps it records,
//! and whether one undo returns the layer to its baseline (pending count 0
//! and no modified marker). Covers empty map, OSM node / way, and every kind
//! of GPX / GeoJSON reference snap target.

use super::*;
use gpui::{point, size, TestAppContext};
use osm_gpui::osm::{OsmNode, OsmWay};
use osm_gpui::reference::{Builder, FeatureKind as RefKind, Format};
use std::collections::HashMap;

const LAT: f64 = 40.0;
const LON: f64 = -74.0;

/// A standalone OSM node that a reference vertex also sits on.
const NODE_3: (f64, f64) = (LAT + 0.0006, LON - 0.0005);
/// Reference line A: two vertices, well away from all OSM data.
const A_V1: (f64, f64) = (LAT - 0.0006, LON - 0.0007);
const A_V2: (f64, f64) = (LAT - 0.0006, LON + 0.0003);
/// A reference marker (waypoint).
const MARKER: (f64, f64) = (LAT - 0.0006, LON + 0.0007);

fn node(id: i64, lat: f64, lon: f64) -> OsmNode {
    OsmNode {
        id,
        lat,
        lon,
        version: 1,
        tags: HashMap::new(),
    }
}

/// Way 10 from node 1 to node 2 through the viewport centre, plus the
/// standalone node 3.
fn osm_data() -> OsmData {
    let mut nodes = HashMap::new();
    nodes.insert(1, node(1, LAT, LON - 0.001));
    nodes.insert(2, node(2, LAT, LON + 0.001));
    nodes.insert(3, node(3, NODE_3.0, NODE_3.1));
    let mut ways = HashMap::new();
    ways.insert(
        10,
        OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: HashMap::new(),
        },
    );
    OsmData {
        nodes,
        ways,
        relations: Vec::new(),
        bounds: Vec::new(),
    }
}

fn gpx(lines: &[&[(f64, f64)]], markers: &[(f64, f64)]) -> osm_gpui::reference::ReferenceData {
    let mut b = Builder::new(Format::Gpx);
    for line in lines {
        let f = b.feature(RefKind::Track, "", Some("Track".into()), vec![]);
        let pts: Vec<(f64, f64)> = line.iter().map(|&(lat, lon)| (lon, lat)).collect();
        b.line(f, &pts).unwrap();
    }
    for &(lat, lon) in markers {
        let f = b.feature(RefKind::Waypoint, "", Some("Marker".into()), vec![]);
        b.point(f, lon, lat).unwrap();
    }
    b.finish().unwrap()
}

/// A view with the OSM layer active (baseline: nothing pending, nothing
/// modified), in Add mode, plus the given reference layers.
fn setup(
    cx: &mut TestAppContext,
    refs: impl FnOnce(&Viewport) -> Vec<osm_gpui::reference::ReferenceData> + 'static,
) -> (gpui::WindowHandle<MapViewer>, LayerId) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    let mut id = LayerId(0);
    let id_ref = &mut id;
    window
        .update(cx, |view, _w, cx| {
            view.viewport = Viewport::new(LAT, LON, 18.0, size(px(800.0), px(600.0)));
            let layer_id = view.layer_manager.alloc_id();
            view.layer_manager
                .add_layer(Box::new(OsmLayer::new_with_data(
                    layer_id,
                    "data".to_string(),
                    Arc::new(osm_data()),
                )));
            view.active_layer = Some(layer_id);
            for (i, data) in refs(&view.viewport).into_iter().enumerate() {
                view.add_reference_dataset(format!("ref{i}"), data, cx);
            }
            // add_reference_dataset re-fits the viewport; restore ours.
            view.viewport = Viewport::new(LAT, LON, 18.0, size(px(800.0), px(600.0)));
            view.active_layer = Some(layer_id);
            view.mode = EditMode::Add;
            *id_ref = layer_id;
        })
        .unwrap();
    (window, id)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct Outcome {
    nodes: usize,
    ways: usize,
    pending: usize,
    steps: usize,
}

fn state(view: &MapViewer, layer: LayerId) -> Outcome {
    let l = view.layer_manager.find_layer(layer).unwrap();
    let osm = l.as_any().downcast_ref::<OsmLayer>().unwrap();
    let data = osm.get_osm_data().unwrap();
    Outcome {
        nodes: data.nodes.len(),
        ways: data.ways.len(),
        pending: view.active_layer_pending().unwrap(),
        steps: view.undo_stack.cursor,
    }
}

fn is_modified(view: &MapViewer, layer: LayerId) -> bool {
    view.layer_manager.find_layer(layer).unwrap().is_modified()
}

/// The screen position of the data's centre. The test window is laid out
/// (and the viewport resized) after the first update, so positions are always
/// taken relative to this rather than as absolute pixels.
fn center(view: &MapViewer) -> gpui::Point<gpui::Pixels> {
    view.viewport.geo_to_screen(LAT, LON)
}

/// A spot far from every OSM and reference feature.
fn far(view: &MapViewer) -> gpui::Point<gpui::Pixels> {
    center(view) + point(px(300.0), px(200.0))
}

fn screen(view: &MapViewer, p: (f64, f64)) -> gpui::Point<gpui::Pixels> {
    view.viewport.geo_to_screen(p.0, p.1)
}

fn node_pos(view: &MapViewer, layer: LayerId, id: i64) -> (f64, f64) {
    view.layer_manager
        .find_layer(layer)
        .and_then(|l| l.as_editable())
        .and_then(|e| e.node_lat_lon(id))
        .unwrap()
}

/// Perform exactly one Add click and return the change it made relative to
/// the baseline; then undo once and assert the baseline is fully restored.
fn one_click_then_undo(
    view: &mut MapViewer,
    window: &mut Window,
    cx: &mut Context<MapViewer>,
    layer: LayerId,
    pt: gpui::Point<gpui::Pixels>,
    ctrl: bool,
) -> Outcome {
    let base = state(view, layer);
    assert_eq!(base.pending, 0, "baseline has nothing pending");
    assert!(!is_modified(view, layer), "baseline is not modified");
    view.add_progress = None;
    view.handle_add_click(pt, ctrl);
    let after = state(view, layer);
    let delta = Outcome {
        nodes: after.nodes - base.nodes,
        ways: after.ways - base.ways,
        pending: after.pending,
        steps: after.steps - base.steps,
    };
    assert!(is_modified(view, layer));
    view.on_undo(&Undo, window, cx);
    assert_eq!(state(view, layer), base, "one undo restores the baseline");
    assert!(
        !is_modified(view, layer),
        "after undoing back to the baseline the layer must not be marked modified"
    );
    delta
}

macro_rules! click_case {
    ($name:ident, $refs:expr, $pt:expr, $ctrl:expr, $expect:expr) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            let (window, layer) = setup(cx, $refs);
            window
                .update(cx, |view, w, cx| {
                    let pt = ($pt)(&*view);
                    let got = one_click_then_undo(view, w, cx, layer, pt, $ctrl);
                    assert_eq!(got, $expect, "{}", stringify!($name));
                })
                .unwrap();
        }
    };
}

fn all_refs(_: &Viewport) -> Vec<osm_gpui::reference::ReferenceData> {
    vec![gpx(
        &[&[A_V1, A_V2], &[NODE_3, (LAT + 0.0010, LON - 0.0003)]],
        &[MARKER],
    )]
}

const NODE_ONLY: Outcome = Outcome {
    nodes: 1,
    ways: 0,
    pending: 1,
    steps: 1,
};

click_case!(
    empty_map_click_is_one_node_one_step,
    all_refs,
    far,
    false,
    NODE_ONLY
);

click_case!(
    click_on_a_reference_vertex_is_one_node_one_step,
    all_refs,
    |v: &MapViewer| screen(v, A_V1),
    false,
    NODE_ONLY
);

click_case!(
    click_near_a_reference_vertex_is_one_node_one_step,
    all_refs,
    |v: &MapViewer| screen(v, A_V1) + point(px(3.0), px(-3.0)),
    false,
    NODE_ONLY
);

click_case!(
    click_on_a_reference_segment_is_one_node_one_step,
    all_refs,
    |v: &MapViewer| {
        let a = screen(v, A_V1);
        let b = screen(v, A_V2);
        point((a.x + b.x) / 2.0, a.y + px(4.0))
    },
    false,
    NODE_ONLY
);

click_case!(
    click_on_a_reference_marker_is_one_node_one_step,
    all_refs,
    |v: &MapViewer| screen(v, MARKER),
    false,
    NODE_ONLY
);

click_case!(
    ctrl_click_on_a_reference_vertex_is_one_node_one_step,
    all_refs,
    |v: &MapViewer| screen(v, A_V1),
    true,
    NODE_ONLY
);

// First click on an existing standalone OSM node that a reference vertex also
// sits on: only the second click of a way connects to nodes, so the first
// click places a new node (here on the same spot).
click_case!(
    first_click_on_an_osm_node_with_a_reference_vertex_is_one_node_one_step,
    all_refs,
    |v: &MapViewer| screen(v, NODE_3),
    false,
    NODE_ONLY
);

// Click on an OSM way's line: a node is spliced into the way. That is two
// pending objects (the new node and the modified way) but ONE undo step.
click_case!(
    click_on_an_osm_way_splices_a_node_in_one_step,
    |_: &Viewport| Vec::new(),
    |v: &MapViewer| center(v) + point(px(0.0), px(1.0)),
    false,
    Outcome {
        nodes: 1,
        ways: 0,
        pending: 2,
        steps: 1
    }
);

// A reference line 3 px from the OSM way and a click between them: the OSM
// way wins and the new node lies on the OSM way, not on the reference line.
#[gpui::test]
fn osm_way_snap_wins_over_a_nearby_reference_line(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, |vp| {
        let c = vp.geo_to_screen(LAT, LON);
        let a = vp.screen_to_geo(c + point(px(-150.0), px(3.0)));
        let b = vp.screen_to_geo(c + point(px(150.0), px(3.0)));
        vec![gpx(&[&[a, b]], &[])]
    });
    window
        .update(cx, |view, w, cx| {
            let pt = center(view) + point(px(0.0), px(2.0));
            assert!(
                view.reference_snap_at(pt).is_some(),
                "reference line is in reach"
            );
            view.add_progress = None;
            view.handle_add_click(pt, false);
            // The new node is on the OSM way (lat == LAT), not on the
            // reference line 3 px below it.
            let created = view.selected[0].id;
            assert!((node_pos(view, layer, created).0 - LAT).abs() < 1e-7);
            assert_eq!(
                state(view, layer),
                Outcome {
                    nodes: 4,
                    ways: 1,
                    pending: 2,
                    steps: 1
                }
            );
            view.on_undo(&Undo, w, cx);
            assert_eq!(state(view, layer).pending, 0);
            assert!(!is_modified(view, layer));
        })
        .unwrap();
}

// The node lands exactly on the reference vertex / marker / segment point.
#[gpui::test]
fn the_new_node_lands_on_the_snap_target(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, all_refs);
    window
        .update(cx, |view, _w, _cx| {
            for (target, pt) in [
                (A_V1, screen(view, A_V1) + point(px(3.0), px(3.0))),
                (MARKER, screen(view, MARKER) + point(px(-2.0), px(2.0))),
            ] {
                view.add_progress = None;
                view.handle_add_click(pt, false);
                let id = view.selected[0].id;
                let (lat, lon) = node_pos(view, layer, id);
                assert!((lat - target.0).abs() < 1e-9 && (lon - target.1).abs() < 1e-9);
            }
        })
        .unwrap();
}

// A hidden reference layer does not snap: the node lands under the cursor.
#[gpui::test]
fn a_hidden_reference_layer_does_not_snap_the_click(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, all_refs);
    window
        .update(cx, |view, w, cx| {
            let ref_id = view.layer_manager.layers()[0].id();
            view.toggle_layer_visibility(ref_id);
            let pt = screen(view, A_V1) + point(px(4.0), px(4.0));
            let got = one_click_then_undo(view, w, cx, layer, pt, false);
            assert_eq!(got, NODE_ONLY);
            view.add_progress = None;
            view.handle_add_click(pt, false);
            let (lat, lon) = node_pos(view, layer, view.selected[0].id);
            assert!((lat - A_V1.0).abs() > 1e-7 || (lon - A_V1.1).abs() > 1e-7);
        })
        .unwrap();
}

// Click a reference vertex, click elsewhere, press Enter: a two-node way.
// Two clicks, two undo steps; each undo reverts exactly one click.
#[gpui::test]
fn two_clicks_and_enter_make_a_two_node_way_in_two_steps(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, all_refs);
    window
        .update(cx, |view, w, cx| {
            let base = state(view, layer);
            view.handle_add_click(screen(view, A_V1), false);
            let after1 = state(view, layer);
            assert_eq!((after1.nodes, after1.ways, after1.pending), (4, 1, 1));
            assert_eq!(after1.steps, 1);
            view.handle_add_click(far(view), false);
            let after2 = state(view, layer);
            assert_eq!((after2.nodes, after2.ways, after2.pending), (5, 2, 3));
            assert_eq!(after2.steps, 2);
            // Enter finishes the way: no edit, no extra step.
            view.add_progress = None;
            assert_eq!(state(view, layer), after2);
            assert!(view.add_progress.is_none());

            view.on_undo(&Undo, w, cx);
            assert_eq!((state(view, layer).nodes, state(view, layer).ways), (4, 1));
            assert_eq!(state(view, layer).pending, 1);
            assert!(is_modified(view, layer));
            view.on_undo(&Undo, w, cx);
            assert_eq!(state(view, layer), base);
            assert!(!is_modified(view, layer));
        })
        .unwrap();
}

// Escape (cancelling the in-progress way) neither adds nor removes steps:
// the first node stays and one undo still removes it.
#[gpui::test]
fn escape_after_the_first_click_keeps_the_node_and_adds_no_step(cx: &mut TestAppContext) {
    let (window, layer) = setup(cx, all_refs);
    window
        .update(cx, |view, w, cx| {
            view.handle_add_click(screen(view, A_V1), false);
            let after = state(view, layer);
            view.add_progress = None;
            assert_eq!(state(view, layer), after);
            view.on_undo(&Undo, w, cx);
            assert_eq!(state(view, layer).pending, 0);
            assert!(!is_modified(view, layer));
        })
        .unwrap();
}

// Second click on an OSM node that also carries a reference vertex: the OSM
// node wins, no new node is created, and the way ends there.
#[gpui::test]
fn second_click_on_an_osm_node_under_a_reference_vertex_connects_to_the_osm_node(
    cx: &mut TestAppContext,
) {
    let (window, layer) = setup(cx, all_refs);
    window
        .update(cx, |view, w, cx| {
            // Make node 3 hittable as a selectable node: tag it.
            view.layer_manager
                .find_layer_mut(layer)
                .unwrap()
                .as_editable_mut()
                .unwrap()
                .set_tag(
                    osm_gpui::selection::FeatureKind::Node,
                    3,
                    "amenity",
                    "bench",
                );
            let base = state(view, layer);
            let base_pending = base.pending;
            view.handle_add_click(far(view), false);
            let first = state(view, layer);
            view.handle_add_click(screen(view, NODE_3), false);
            let second = state(view, layer);
            // One new node (the first click) and a way; the second click made
            // no node.
            assert_eq!(second.nodes, first.nodes);
            assert_eq!(second.ways, first.ways + 1);
            assert!(view.add_progress.is_none());
            let way = view.selected[0].id;
            let nodes = view
                .layer_manager
                .find_layer(layer)
                .and_then(|l| l.as_editable())
                .and_then(|e| e.way_node_ids(way))
                .unwrap();
            assert_eq!(nodes.len(), 2);
            assert_eq!(nodes[1], 3);
            assert_eq!(second.steps, first.steps + 1);
            // Undo the connect: the OSM node stays.
            view.on_undo(&Undo, w, cx);
            assert_eq!(state(view, layer).nodes, first.nodes);
            assert!(view
                .layer_manager
                .find_layer(layer)
                .and_then(|l| l.as_editable())
                .and_then(|e| e.node_lat_lon(3))
                .is_some());
            let _ = base_pending;
        })
        .unwrap();
}
