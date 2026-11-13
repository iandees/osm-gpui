//! Point, segment, rectangle and interior hit tests, and snapping to a way.

use super::{data_with, empty_tags, viewport_centered_on};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::EditableLayer;
use crate::layers::LayerId;
use crate::osm::{OsmNode, OsmWay};
use crate::selection::{FeatureKind, FeatureRef};
use gpui::{point, px, size, Bounds};

#[test]
fn hit_test_node_wins_over_coincident_way() {
    let center_lat = 40.0;
    let center_lon = -74.0;
    let n1 = OsmNode {
        id: 1,
        lat: center_lat,
        lon: center_lon,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: center_lat,
        lon: center_lon + 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let viewport = viewport_centered_on(center_lat, center_lon);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    // Plain vertices are drawn (and hit) only on a selected way.
    layer.set_highlight(&[FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Way,
        id: 10,
    }]);

    let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, FeatureKind::Node);
    assert_eq!(hits[0].feature.id, 1);
}

#[test]
fn hit_test_falls_through_to_way() {
    let center_lat = 40.0;
    let center_lon = -74.0;
    let n1 = OsmNode {
        id: 1,
        lat: center_lat,
        lon: center_lon - 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: center_lat,
        lon: center_lon + 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let viewport = viewport_centered_on(center_lat, center_lon);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert!(hits.iter().all(|h| h.kind == FeatureKind::Way));
    assert!(hits.iter().any(|h| h.feature.id == 10));
}

#[test]
fn hit_test_interior_hits_closed_way_center_but_plain_hit_test_misses() {
    let center_lat = 40.0;
    let center_lon = -74.0;
    let d = 0.01;
    let n1 = OsmNode {
        id: 1,
        lat: center_lat - d,
        lon: center_lon - d,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: center_lat - d,
        lon: center_lon + d,
        version: 1,
        tags: empty_tags(),
    };
    let n3 = OsmNode {
        id: 3,
        lat: center_lat + d,
        lon: center_lon + d,
        version: 1,
        tags: empty_tags(),
    };
    let n4 = OsmNode {
        id: 4,
        lat: center_lat + d,
        lon: center_lon - d,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2, 3, 4, 1],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2, n3, n4], vec![way]);
    let viewport = viewport_centered_on(center_lat, center_lon);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    let center = point(px(400.0), px(300.0));

    // The center of the square is far from every edge — a plain
    // hit_test (edge/outline distance only) should not select it.
    assert!(layer.hit_test(&viewport, center).is_empty());

    // Interior hit-testing should find it.
    let hits = layer.hit_test_interior(&viewport, center);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, FeatureKind::Way);
    assert_eq!(hits[0].feature.id, 10);
}

#[test]
fn resolve_hits_picks_smallest_of_nested_closed_ways() {
    let center_lat = 40.0;
    let center_lon = -74.0;

    // Outer, large square.
    let d_outer = 0.01;
    let n1 = OsmNode {
        id: 1,
        lat: center_lat - d_outer,
        lon: center_lon - d_outer,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: center_lat - d_outer,
        lon: center_lon + d_outer,
        version: 1,
        tags: empty_tags(),
    };
    let n3 = OsmNode {
        id: 3,
        lat: center_lat + d_outer,
        lon: center_lon + d_outer,
        version: 1,
        tags: empty_tags(),
    };
    let n4 = OsmNode {
        id: 4,
        lat: center_lat + d_outer,
        lon: center_lon - d_outer,
        version: 1,
        tags: empty_tags(),
    };
    let outer_way = OsmWay {
        id: 10,
        nodes: vec![1, 2, 3, 4, 1],
        version: 1,
        tags: empty_tags(),
    };

    // Inner, small square nested entirely inside the outer one, drawn
    // first so a plain "last one wins" tie-break would pick the outer
    // way instead.
    let d_inner = 0.001;
    let n5 = OsmNode {
        id: 5,
        lat: center_lat - d_inner,
        lon: center_lon - d_inner,
        version: 1,
        tags: empty_tags(),
    };
    let n6 = OsmNode {
        id: 6,
        lat: center_lat - d_inner,
        lon: center_lon + d_inner,
        version: 1,
        tags: empty_tags(),
    };
    let n7 = OsmNode {
        id: 7,
        lat: center_lat + d_inner,
        lon: center_lon + d_inner,
        version: 1,
        tags: empty_tags(),
    };
    let n8 = OsmNode {
        id: 8,
        lat: center_lat + d_inner,
        lon: center_lon - d_inner,
        version: 1,
        tags: empty_tags(),
    };
    let inner_way = OsmWay {
        id: 20,
        nodes: vec![5, 6, 7, 8, 5],
        version: 1,
        tags: empty_tags(),
    };

    let data = data_with(
        vec![n1, n2, n3, n4, n5, n6, n7, n8],
        vec![inner_way, outer_way],
    );
    let viewport = viewport_centered_on(center_lat, center_lon);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    let center = point(px(400.0), px(300.0));

    let hits = layer.hit_test_interior(&viewport, center);
    assert_eq!(hits.len(), 2);

    let winner = crate::selection::resolve_hits(vec![hits]).unwrap();
    assert_eq!(winner.id, 20, "smallest enclosing way should win");
}

#[test]
fn hit_test_interior_ignores_open_way() {
    let center_lat = 40.0;
    let center_lon = -74.0;
    let d = 0.01;
    let n1 = OsmNode {
        id: 1,
        lat: center_lat - d,
        lon: center_lon - d,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: center_lat - d,
        lon: center_lon + d,
        version: 1,
        tags: empty_tags(),
    };
    let n3 = OsmNode {
        id: 3,
        lat: center_lat + d,
        lon: center_lon + d,
        version: 1,
        tags: empty_tags(),
    };
    let n4 = OsmNode {
        id: 4,
        lat: center_lat + d,
        lon: center_lon - d,
        version: 1,
        tags: empty_tags(),
    };
    // Not closed: first node ref (1) != last (4).
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2, 3, 4],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2, n3, n4], vec![way]);
    let viewport = viewport_centered_on(center_lat, center_lon);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    let center = point(px(400.0), px(300.0));

    assert!(layer.hit_test_interior(&viewport, center).is_empty());
}

#[test]
fn hit_test_no_match_returns_empty() {
    let n = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n], vec![]);
    let viewport = viewport_centered_on(40.0, -74.0);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let hits = layer.hit_test(&viewport, point(px(50.0), px(50.0)));
    assert!(hits.is_empty(), "unexpected hits: {:?}", hits);
}

#[test]
fn hit_test_segment_finds_nearest_segment_and_endpoint_indices() {
    let center_lat = 40.0;
    let center_lon = -74.0;
    let n1 = OsmNode {
        id: 1,
        lat: center_lat,
        lon: center_lon - 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: center_lat,
        lon: center_lon + 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let viewport = viewport_centered_on(center_lat, center_lon);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let hit = layer.hit_test_segment(&viewport, point(px(400.0), px(300.0)), 6.0);
    assert_eq!(hit, Some((10, 1, 2, 0)));
}

#[test]
fn hit_test_segment_none_when_out_of_tolerance() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.001,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let viewport = viewport_centered_on(40.0, -74.0);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let hit = layer.hit_test_segment(&viewport, point(px(0.0), px(0.0)), 6.0);
    assert!(hit.is_none());
}

#[test]
fn snap_to_way_on_line_returns_exact_click_point() {
    let center_lat = 40.0;
    let center_lon = -74.0;
    let n1 = OsmNode {
        id: 1,
        lat: center_lat,
        lon: center_lon - 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: center_lat,
        lon: center_lon + 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let viewport = viewport_centered_on(center_lat, center_lon);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let (way_id, a, b, idx, lat, lon) = layer
        .snap_to_way(&viewport, point(px(400.0), px(300.0)), 6.0)
        .expect("expected a snap hit");
    assert_eq!((way_id, a, b, idx), (10, 1, 2, 0));
    assert!((lat - center_lat).abs() < 1e-6, "got lat {}", lat);
    assert!((lon - center_lon).abs() < 1e-6, "got lon {}", lon);
}

#[test]
fn snap_to_way_off_line_projects_onto_segment() {
    let center_lat = 40.0;
    let center_lon = -74.0;
    let n1 = OsmNode {
        id: 1,
        lat: center_lat,
        lon: center_lon - 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: center_lat,
        lon: center_lon + 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let viewport = viewport_centered_on(center_lat, center_lon);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    // 3px above the (horizontal) line, within the 6px tolerance.
    let (way_id, a, b, idx, lat, lon) = layer
        .snap_to_way(&viewport, point(px(403.0), px(297.0)), 6.0)
        .expect("expected a snap hit");
    assert_eq!((way_id, a, b, idx), (10, 1, 2, 0));
    // The line is flat (both endpoints share center_lat), so the
    // projected point's lat matches the line exactly regardless of the
    // click's vertical offset.
    assert!((lat - center_lat).abs() < 1e-6, "got lat {}", lat);
    assert!(
        lon > center_lon - 0.001 && lon < center_lon + 0.001,
        "got lon {}",
        lon
    );
}

#[test]
fn snap_to_way_none_when_out_of_tolerance() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.001,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let viewport = viewport_centered_on(40.0, -74.0);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let hit = layer.snap_to_way(&viewport, point(px(0.0), px(0.0)), 6.0);
    assert!(hit.is_none());
}

#[test]
fn hit_test_rect_selects_contained_nodes_and_fully_enclosed_ways() {
    let center_lat = 40.0;
    let center_lon = -74.0;
    // n1 and n2 sit exactly at the viewport center (mercator-identical);
    // n3 is a full degree away, so its mercator position is far outside
    // any modest screen-space rect around the center.
    let n1 = OsmNode {
        id: 1,
        lat: center_lat,
        lon: center_lon,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: center_lat,
        lon: center_lon,
        version: 1,
        tags: empty_tags(),
    };
    let n3 = OsmNode {
        id: 3,
        lat: center_lat + 1.0,
        lon: center_lon + 1.0,
        version: 1,
        tags: empty_tags(),
    };
    // way_in's bbox is the (degenerate) point at the center: fully enclosed.
    let way_in = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    // way_partial's bbox spans from the center to the far node: NOT fully
    // enclosed by a modest rect around the center.
    let way_partial = OsmWay {
        id: 20,
        nodes: vec![1, 3],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2, n3], vec![way_in, way_partial]);
    let viewport = viewport_centered_on(center_lat, center_lon);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    // Plain vertices are boxed only when their way is selected.
    layer.set_highlight(&[FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Way,
        id: 10,
    }]);

    // A screen-space box symmetric around the viewport's center screen
    // point (400, 300) — always brackets the center's mercator position
    // regardless of zoom level.
    let rect = Bounds {
        origin: point(px(300.0), px(200.0)),
        size: size(px(200.0), px(200.0)),
    };

    let hits = layer.hit_test_rect(&viewport, rect);
    let node_ids: Vec<i64> = hits
        .iter()
        .filter(|f| f.kind == FeatureKind::Node)
        .map(|f| f.id)
        .collect();
    let way_ids: Vec<i64> = hits
        .iter()
        .filter(|f| f.kind == FeatureKind::Way)
        .map(|f| f.id)
        .collect();

    assert!(
        node_ids.contains(&1) && node_ids.contains(&2),
        "got {:?}",
        node_ids
    );
    assert!(
        !node_ids.contains(&3),
        "far node should not be selected: {:?}",
        node_ids
    );
    assert!(
        way_ids.contains(&10),
        "fully-enclosed way should be selected: {:?}",
        way_ids
    );
    assert!(
        !way_ids.contains(&20),
        "partially-overlapping way should not be selected: {:?}",
        way_ids
    );
}

#[test]
fn hit_test_rect_empty_when_no_data() {
    let layer = OsmLayer::new(LayerId(1));
    let viewport = viewport_centered_on(40.0, -74.0);
    let rect = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(800.0), px(600.0)),
    };
    assert!(layer.hit_test_rect(&viewport, rect).is_empty());
}
