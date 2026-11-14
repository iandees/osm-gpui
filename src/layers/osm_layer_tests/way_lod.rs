//! The level-of-detail rules and the data they depend on (`WayGeom`,
//! the way scene), including that no edit leaves them stale.

// Explicit imports: a glob of `super::*` brings gpui's `test` macro in
// and shadows `#[test]`.
use super::super::{
    vertex_significance, WayDetail, WayLod, CASING_MIN_ZOOM, COARSE_BELOW_ZOOM, COLLAPSE_BELOW_PX,
};
use crate::coordinates::LatLon;
use crate::layers::osm_layer::OsmLayer;
use crate::layers::{EditableLayer, LayerId};
use crate::osm::{OsmData, OsmNode, OsmWay};
use crate::viewport::Viewport;
use gpui::{point, px, size, Bounds, Pixels};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn tags() -> HashMap<String, String> {
    HashMap::new()
}

fn node(id: i64, lat: f64, lon: f64) -> OsmNode {
    OsmNode {
        id,
        lat,
        lon,
        tags: tags(),
        version: 1,
    }
}

fn way(id: i64, nodes: Vec<i64>) -> OsmWay {
    OsmWay {
        id,
        nodes,
        tags: tags(),
        version: 1,
    }
}

fn vert(x: f64, y: f64) -> (i64, f64, f64) {
    (0, x, y)
}

#[test]
fn significance_of_a_straight_line_is_zero_and_ends_are_infinite() {
    let sig = vertex_significance(&[vert(0.0, 0.0), vert(1.0, 0.0), vert(2.0, 0.0)]);
    assert_eq!(sig[0], f32::INFINITY);
    assert_eq!(sig[2], f32::INFINITY);
    assert_eq!(sig[1], 0.0);
}

#[test]
fn significance_is_the_distance_from_the_chord() {
    let sig = vertex_significance(&[vert(0.0, 0.0), vert(5.0, 3.0), vert(10.0, 0.0)]);
    assert!((sig[1] - 3.0).abs() < 1e-6, "{sig:?}");
}

#[test]
fn thresholding_the_significance_is_douglas_peucker() {
    // A spike (10) followed by a bump (0.5) next to it, then a flat run.
    let pts = [
        vert(0.0, 0.0),
        vert(2.0, 0.5),
        vert(4.0, 10.0),
        vert(6.0, 0.5),
        vert(8.0, 0.0),
        vert(10.0, 0.0),
    ];
    let sig = vertex_significance(&pts);
    // Never larger than the split vertex's ancestors, which is what
    // makes a threshold equal to running Douglas-Peucker.
    assert!(sig[2] >= sig[1] && sig[2] >= sig[3], "{sig:?}");
    let kept = |tol: f32| -> Vec<usize> { (0..pts.len()).filter(|&i| sig[i] >= tol).collect() };
    assert_eq!(kept(0.0).len(), pts.len());
    assert_eq!(kept(5.0), vec![0, 2, 5], "only the spike survives");
    assert_eq!(kept(100.0), vec![0, 5], "ends are always kept");
}

#[test]
fn significance_of_a_closed_ring_keeps_its_far_corner() {
    let ring = [
        vert(0.0, 0.0),
        vert(10.0, 0.0),
        vert(10.0, 10.0),
        vert(0.0, 10.0),
        vert(0.0, 0.0),
    ];
    let sig = vertex_significance(&ring);
    assert_eq!(sig[0], f32::INFINITY);
    assert_eq!(sig[4], f32::INFINITY);
    assert!(sig[1..4].iter().all(|&s| s > 5.0), "{sig:?}");
}

#[test]
fn significance_of_degenerate_input_does_not_panic() {
    assert!(vertex_significance(&[]).is_empty());
    assert_eq!(vertex_significance(&[vert(1.0, 1.0)]), vec![f32::INFINITY]);
    let same = vertex_significance(&[vert(1.0, 1.0), vert(1.0, 1.0), vert(1.0, 1.0)]);
    assert_eq!(same[1], 0.0);
}

#[test]
fn the_coarse_rules_start_below_zoom_16() {
    assert!(WayLod::at(15.99).coarse);
    assert!(!WayLod::at(16.0).coarse);
    assert!(!WayLod::at(19.5).coarse);
    assert!(WayLod::at(14.0).tolerance_px > WayLod::at(16.0).tolerance_px);
    assert!(WayLod::at(14.0).min_segment_px > WayLod::at(16.0).min_segment_px);
}

#[test]
fn tiny_ways_are_skipped_or_collapsed_only_when_coarse() {
    let coarse = WayLod::at(15.0);
    assert_eq!(coarse.detail(0.1), WayDetail::Skip);
    assert_eq!(coarse.detail(1.5), WayDetail::Collapse);
    assert_eq!(coarse.detail(COLLAPSE_BELOW_PX), WayDetail::Full);
    assert_eq!(coarse.detail(400.0), WayDetail::Full);
    let editing = WayLod::at(16.0);
    assert_eq!(editing.detail(0.1), WayDetail::Full);
    assert_eq!(editing.detail(1.5), WayDetail::Full);
}

#[test]
fn tiny_fills_are_dropped_only_when_coarse() {
    assert!(!WayLod::at(15.0).fills(1.0));
    assert!(WayLod::at(15.0).fills(10.0));
    assert!(WayLod::at(17.0).fills(0.1));
}

#[test]
fn minor_ways_are_dropped_only_below_zoom_15() {
    assert!(!WayLod::at(14.0).strokes(1.5));
    assert!(!WayLod::at(14.9).strokes(1.0));
    assert!(WayLod::at(14.0).strokes(2.0));
    assert!(WayLod::at(15.0).strokes(1.0));
    assert!(WayLod::at(16.0).strokes(1.0));
    assert!(WayLod::at(19.0).strokes(1.0));
}

#[test]
fn casing_starts_at_the_editing_zoom() {
    assert_eq!(CASING_MIN_ZOOM, COARSE_BELOW_ZOOM);
}

// ---- the scene follows edits ---------------------------------------

const LAT: f64 = 40.0;
const LON: f64 = -74.0;

/// A straight 170 m way east, with a kink 0.2 m off the line (node 2) and
/// two vertices on it (nodes 3 and 4). At zoom 16 it simplifies to a
/// single segment; at zoom 19 the kink is a pixel and is kept.
fn layer() -> OsmLayer {
    let nodes = vec![
        node(1, LAT, LON),
        node(2, LAT + 0.000002, LON + 0.0005),
        node(3, LAT, LON + 0.001),
        node(4, LAT, LON + 0.0015),
        node(5, LAT, LON + 0.002),
    ];
    let data = OsmData {
        nodes: nodes.into_iter().map(|n| (n.id, n)).collect(),
        ways: HashMap::from([(10, way(10, vec![1, 2, 3, 4, 5]))]),
        relations: Vec::new(),
        bounds: Vec::new(),
    };
    OsmLayer::new_with_data(LayerId(1), "t", Arc::new(data))
}

fn view(zoom: f64) -> (Viewport, Bounds<Pixels>) {
    let v = Viewport::new(
        LatLon::new(LAT, LON + 0.001),
        zoom,
        size(px(800.0), px(600.0)),
    );
    let b = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(800.0), px(600.0)),
    };
    (v, b)
}

fn scene_vertices(layer: &OsmLayer, zoom: f64) -> usize {
    let (v, b) = view(zoom);
    layer.build_way_scene(&v, b).vertex_count()
}

fn scene_paths(layer: &OsmLayer, zoom: f64) -> usize {
    let (v, b) = view(zoom);
    layer.build_way_scene(&v, b).path_count()
}

fn layer_styled(css: &str) -> OsmLayer {
    let mut l = layer();
    l.set_stylesheet(Arc::new(crate::style::Stylesheet::parse(css).unwrap()));
    l
}

#[test]
fn a_layer_styles_itself_from_the_sheet_it_was_given_not_the_global_one() {
    const PLAIN: &str = "way { width: 3; }";
    const CASED: &str = "way { width: 3; casing-width: 2; casing-color: red; }";
    let sheet = |css: &str| Arc::new(crate::style::Stylesheet::parse(css).unwrap());
    let data = layer().store.osm_data.clone().unwrap();

    crate::style::custom::set_active(Some(sheet(CASED)));
    let plain = OsmLayer::new_with_data_styled(LayerId(1), "t", data.clone(), sheet(PLAIN));
    let empty_plain = OsmLayer::new_styled(LayerId(2), sheet(PLAIN));
    crate::style::custom::set_active(Some(sheet(PLAIN)));
    let cased = OsmLayer::new_with_data_styled(LayerId(3), "t", data.clone(), sheet(CASED));
    crate::style::custom::set_active(None);

    assert_eq!(scene_paths(&plain, 15.0), 1);
    assert_eq!(scene_paths(&cased, 15.0), 2);
    assert!(!empty_plain.has_data());
}

#[test]
fn a_styled_casing_adds_a_path_below_the_default_casing_zoom() {
    // Zoom 15: no default casing, so one stroke path.
    assert_eq!(scene_paths(&layer_styled("way { width: 3; }"), 15.0), 1);
    let cased = layer_styled("way { width: 3; casing-width: 2; casing-color: red; }");
    assert_eq!(scene_paths(&cased, 15.0), 2);
    // At zoom 17 the styled casing replaces the default one.
    assert_eq!(scene_paths(&cased, 17.0), 2);
}

/// Two overlapping ways over the same nodes, tagged `highway=lo` (way
/// 10) and `highway=hi` (way 11).
fn two_way_layer(css: &str) -> OsmLayer {
    let l = layer();
    let mut data = (**l.store.osm_data.as_ref().unwrap()).clone();
    let mut lo = way(10, vec![1, 2, 3, 4, 5]);
    lo.tags.insert("highway".into(), "lo".into());
    let mut hi = way(11, vec![1, 2, 3, 4, 5]);
    hi.tags.insert("highway".into(), "hi".into());
    data.ways = HashMap::from([(10, lo), (11, hi)]);
    let mut l = OsmLayer::new_with_data(LayerId(1), "t", Arc::new(data));
    l.set_stylesheet(Arc::new(crate::style::Stylesheet::parse(css).unwrap()));
    l
}

fn line_colors(l: &OsmLayer, zoom: f64) -> Vec<u32> {
    let (v, b) = view(zoom);
    l.build_way_scene(&v, b)
        .lines
        .iter()
        .map(|(rgba, _)| rgba >> 8)
        .collect()
}

#[test]
fn z_index_orders_strokes_whatever_the_way_order() {
    // `lo` is way 10 and would draw first anyway; swap which one is on top.
    let css = "way[highway=lo] { color: #0000ff; width: 3; z-index: 1; } \
                   way[highway=hi] { color: #ff0000; width: 3; z-index: 5; }";
    let l = two_way_layer(css);
    assert_eq!(line_colors(&l, 17.0), vec![0x0000ff, 0xff0000]);
    let css = "way[highway=lo] { color: #0000ff; width: 3; z-index: 9; } \
                   way[highway=hi] { color: #ff0000; width: 3; z-index: 5; }";
    let l = two_way_layer(css);
    assert_eq!(line_colors(&l, 17.0), vec![0xff0000, 0x0000ff]);
}

#[test]
fn a_higher_layers_casing_covers_a_lower_layers_stroke() {
    let css = "way[highway=lo] { color: #0000ff; width: 3; z-index: 1; casing-width: 1; casing-color: #111111; } \
                   way[highway=hi] { color: #ff0000; width: 3; z-index: 5; casing-width: 1; casing-color: #222222; }";
    let l = two_way_layer(css);
    assert_eq!(
        line_colors(&l, 17.0),
        vec![0x111111, 0x0000ff, 0x222222, 0xff0000]
    );
}

#[test]
fn stroke_opacity_becomes_the_batch_alpha() {
    let l = layer_styled("way { color: #123456; width: 3; opacity: 0.5; }");
    let (v, b) = view(17.0);
    let scene = l.build_way_scene(&v, b);
    assert_eq!(scene.lines.len(), 1);
    assert_eq!(scene.lines[0].0, 0x1234_5600 | 128);
}

#[test]
fn a_dashed_stroke_draws_fewer_pixels_than_a_solid_one() {
    let area = |l: &OsmLayer| {
        let (v, b) = view(18.0);
        l.build_way_scene(&v, b)
            .lines
            .iter()
            .map(|(_, p)| p.vertices.len())
            .sum::<usize>()
    };
    let solid = layer_styled("way { width: 3; }");
    let dashed = layer_styled("way { width: 3; dashes: 8,8; }");
    // Many short rectangles instead of one long one.
    assert!(area(&dashed) > area(&solid));
    // And never below the editing zoom, where the stroke is simplified.
    assert_eq!(scene_vertices(&dashed, 15.0), scene_vertices(&solid, 15.0));
}

#[test]
fn equal_z_index_keeps_a_stable_order_across_frames() {
    let css = "way[highway] { width: 3; } way[highway=lo] { color: #0000ff; } way[highway=hi] { color: #ff0000; }";
    let l = two_way_layer(css);
    let first = line_colors(&l, 17.0);
    assert_eq!(first.len(), 2);
    for _ in 0..50 {
        assert_eq!(line_colors(&l, 17.0), first);
    }
}

#[test]
fn zoom_gated_casing_follows_the_zoom_bucket() {
    let l = layer_styled("way { width: 3; } way|z15- { casing-width: 2; }");
    assert_eq!(scene_paths(&l, 14.5), 1);
    assert_eq!(scene_paths(&l, 15.2), 2);
    // And back: the per-bucket table is not stale.
    assert_eq!(scene_paths(&l, 14.5), 1);
}

/// The layer's cached geometry and its scene are what a layer freshly
/// built from the same data would have, at every zoom tried.
fn assert_same_as_fresh(layer: &OsmLayer) {
    let data = layer.store.osm_data.clone().expect("data");
    let fresh = OsmLayer::new_with_data(LayerId(9), "fresh", data);
    for (i, id) in layer.derived.way_ids.iter().enumerate() {
        let j = fresh
            .derived
            .way_ids
            .iter()
            .position(|x| x == id)
            .expect("way");
        assert_eq!(
            layer.derived.way_vertices[i], fresh.derived.way_vertices[j],
            "way {id}"
        );
    }
    for zoom in [12.0, 15.5, 16.0, 17.0, 19.0] {
        assert_eq!(
            scene_vertices(layer, zoom),
            scene_vertices(&fresh, zoom),
            "zoom {zoom}"
        );
    }
}

#[test]
fn detail_grows_with_zoom() {
    let l = layer();
    // z16 drops the kink 0.2 m off the line, z19 keeps it.
    assert!(scene_vertices(&l, 19.0) > scene_vertices(&l, 16.0));
    assert!(scene_vertices(&l, 16.0) > 0);
}

#[test]
fn a_minor_way_has_no_outline_at_zoom_14_but_does_at_15() {
    // The test way is untagged: the default 1.5 px stroke.
    let l = layer();
    assert_eq!(scene_vertices(&l, 14.0), 0);
    assert!(scene_vertices(&l, 15.0) > 0);
}

#[test]
fn moving_a_node_updates_the_simplification() {
    let mut l = layer();
    assert_same_as_fresh(&l);
    let before = scene_vertices(&l, 17.0);
    // Node 3 starts on the line and is simplified away. Pull it 55 m off
    // the line: it becomes significant, so the way draws more segments.
    l.commit_node_moves(&[(3, LAT + 0.0005, LON + 0.001)])
        .unwrap();
    assert_same_as_fresh(&l);
    let bent = scene_vertices(&l, 17.0);
    assert!(bent > before, "{bent} > {before}");
    // And back (what undo does): it is simplified away again.
    l.commit_node_moves(&[(3, LAT, LON + 0.001)]).unwrap();
    assert_same_as_fresh(&l);
    assert_eq!(scene_vertices(&l, 17.0), before);
}

#[test]
fn adding_and_removing_ways_updates_the_scene() {
    let mut l = layer();
    let before = scene_vertices(&l, 17.0);
    let a = l.add_node(LAT + 0.0002, LON);
    let b = l.add_node(LAT + 0.0002, LON + 0.001);
    let c = l.add_node(LAT + 0.0004, LON + 0.002);
    let id = l.add_way(vec![a, b, c], Vec::new());
    assert_same_as_fresh(&l);
    assert!(scene_vertices(&l, 17.0) > before);
    l.remove_way(id).unwrap();
    assert_same_as_fresh(&l);
    assert_eq!(scene_vertices(&l, 17.0), before);
}

#[test]
fn changing_a_ways_nodes_updates_the_scene() {
    let mut l = layer();
    // Zoom 19 keeps the 0.2 m kink (node 2), so dropping it shows.
    let before = scene_vertices(&l, 19.0);
    assert!(l.set_way_nodes(10, vec![1, 5]).is_ok());
    assert_same_as_fresh(&l);
    assert!(scene_vertices(&l, 19.0) < before);
    l.extend_way(10, 3).unwrap();
    l.remove_node_from_way(10, 0).unwrap();
    assert_same_as_fresh(&l);
}

#[test]
fn a_drag_in_progress_draws_every_vertex() {
    let mut l = layer();
    // z16 simplifies the 0.2 m kink away ...
    let simplified = scene_vertices(&l, 16.0);
    // ... but while nodes are dragged nothing is dropped, because the
    // cached significance is of the geometry before the drag.
    l.set_drag_preview(&HashSet::from([4]), point(px(3.0), px(0.0)));
    assert!(scene_vertices(&l, 16.0) > simplified);
    l.clear_drag_preview();
    assert_eq!(scene_vertices(&l, 16.0), simplified);
}

#[test]
fn repeated_frames_build_identical_scenes() {
    // The vertex-capacity hints carry over from frame to frame and must
    // not change what is drawn.
    let l = layer();
    let first = scene_vertices(&l, 17.0);
    assert_eq!(scene_vertices(&l, 17.0), first);
    assert_eq!(scene_vertices(&l, 17.0), first);
}
