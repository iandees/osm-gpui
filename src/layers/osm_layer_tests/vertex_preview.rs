//! Vertex preview of the hovered way and its effect on hit tests and box select.

use super::{data_with, empty_tags, node_point, view_at, way_with_poi_layer};
use crate::layers::node_markers::NodeForm;
use crate::layers::osm_layer::OsmLayer;
use crate::layers::EditableLayer;
use crate::layers::LayerId;
use crate::osm::{OsmNode, OsmWay};
use crate::selection::{FeatureKind, FeatureRef};
use crate::viewport::Viewport;
use gpui::{point, px, size, Bounds};

fn way_ref(id: i64) -> FeatureRef {
    FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Way,
        id,
    }
}

fn node_ref(id: i64) -> FeatureRef {
    FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Node,
        id,
    }
}

/// Preview way 100 as a hover on its line would.
fn preview_100(layer: &mut OsmLayer, vp: &Viewport) -> bool {
    let on_line = node_point(layer, vp, 30);
    layer.update_vertex_preview(vp, on_line, Some(&way_ref(100)), true)
}

#[test]
fn preview_vertices_are_drawn_and_targets_from_the_preview_zoom_only() {
    for z in [16.0, 17.0, 17.99] {
        let mut layer = way_with_poi_layer(60);
        let vp = view_at(z);
        preview_100(&mut layer, &vp);
        assert_eq!(layer.vertex_preview_way(), None, "z{z}: no preview");
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Way), "z{z}");
    }
    for z in [18.0, 19.0, 21.0] {
        let mut layer = way_with_poi_layer(60);
        let vp = view_at(z);
        assert!(preview_100(&mut layer, &vp), "z{z}");
        assert_eq!(layer.vertex_preview_way(), Some(100));
        let pins = layer.pins();
        let idx = layer.derived.node_cache.index_by_id[&30];
        assert_eq!(layer.node_form_at(&pins, idx, 30, z), NodeForm::Preview);
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
        assert_eq!(hits.len(), 1, "z{z}");
        assert_eq!(hits[0].feature, node_ref(30), "z{z}");
    }
    // A preview left over from a higher zoom does not show or target
    // anything once the view is below the threshold.
    let mut layer = way_with_poi_layer(60);
    preview_100(&mut layer, &view_at(19.0));
    let low = view_at(17.0);
    let hits = layer.hit_test(&low, node_point(&layer, &low, 30));
    assert!(hits.iter().all(|h| h.kind == FeatureKind::Way));
}

#[test]
fn previewed_vertex_wins_over_the_line_and_is_hit_slightly_off_it() {
    let mut layer = way_with_poi_layer(60);
    let vp = view_at(19.0);
    let at = node_point(&layer, &vp, 30);
    // Without a preview the line is hit.
    assert_eq!(layer.hit_test(&vp, at)[0].kind, FeatureKind::Way);
    preview_100(&mut layer, &vp);
    let hits = layer.hit_test(&vp, at);
    assert!(hits.iter().all(|h| h.kind == FeatureKind::Node));
    // Off the vertex but within its radius (and within the line's own
    // tolerance): still the node, not the way.
    let near = point(at.x + px(5.0), at.y + px(3.0));
    let hits = layer.hit_test(&vp, near);
    assert_eq!(hits[0].feature, node_ref(30));
    // Beyond the vertex radius but on the line: the way.
    let on_line = point(at.x + px(20.0), at.y);
    assert_eq!(layer.hit_test(&vp, on_line)[0].kind, FeatureKind::Way);
}

#[test]
fn only_the_preview_ways_vertices_are_previewed() {
    // Ways 100 (nodes 1-5) and 200 (nodes 5-9) share node 5; way 300 is
    // apart.
    let mut nodes = Vec::new();
    let mut add = |id: i64, lat: f64, lon: f64| {
        nodes.push(OsmNode {
            id,
            lat,
            lon,
            version: 1,
            tags: empty_tags(),
        });
    };
    for i in 1..=5 {
        add(i, 40.0, -74.0 + i as f64 * 0.0001);
    }
    for i in 6..=9 {
        add(i, 40.0 + (i - 5) as f64 * 0.0001, -74.0 + 5.0 * 0.0001);
    }
    for i in 11..=15 {
        add(i, 40.002, -74.0 + (i - 10) as f64 * 0.0001);
    }
    let way = |id: i64, nodes: Vec<i64>| OsmWay {
        id,
        nodes,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(
        nodes,
        vec![
            way(100, vec![1, 2, 3, 4, 5]),
            way(200, vec![5, 6, 7, 8, 9]),
            way(300, vec![11, 12, 13, 14, 15]),
        ],
    );
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    let vp = Viewport::new(40.0003, -74.0003, 19.0, size(px(800.0), px(600.0)));
    let forms = |layer: &OsmLayer| -> Vec<i64> {
        let pins = layer.pins();
        (1..=15)
            .filter(|id| {
                layer
                    .derived
                    .node_cache
                    .index_by_id
                    .get(id)
                    .is_some_and(|&idx| {
                        layer.node_form_at(&pins, idx, *id, 19.0) == NodeForm::Preview
                    })
            })
            .collect()
    };
    assert!(forms(&layer).is_empty());
    let mid = node_point(&layer, &vp, 3);
    layer.update_vertex_preview(&vp, mid, Some(&way_ref(100)), true);
    assert_eq!(forms(&layer), vec![1, 2, 3, 4, 5]);
    // The shared node appears when either way is hovered.
    let mid = node_point(&layer, &vp, 7);
    layer.update_vertex_preview(&vp, mid, Some(&way_ref(200)), true);
    assert_eq!(forms(&layer), vec![5, 6, 7, 8, 9]);
    layer.update_vertex_preview(&vp, mid, None, false);
    assert!(forms(&layer).is_empty());
}

#[test]
fn preview_hysteresis_and_switching() {
    let mut layer = way_with_poi_layer(60);
    let vp = view_at(19.0);
    preview_100(&mut layer, &vp);
    let at = node_point(&layer, &vp, 30);
    let off = |dy: f32| point(at.x + px(24.0), at.y + px(dy));
    // Hovering nothing, 10px off the line (past its 6px tolerance but
    // inside tolerance + vertex radius): still previewed.
    assert!(!layer.update_vertex_preview(&vp, off(10.0), None, true));
    assert_eq!(layer.vertex_preview_way(), Some(100));
    // Well away: dropped.
    assert!(layer.update_vertex_preview(&vp, off(16.0), None, true));
    assert_eq!(layer.vertex_preview_way(), None);
    // Hovering one of its vertices keeps the preview; a node that is
    // not one of its vertices drops it; another way replaces it.
    preview_100(&mut layer, &vp);
    layer.update_vertex_preview(&vp, at, Some(&node_ref(30)), true);
    assert_eq!(layer.vertex_preview_way(), Some(100));
    layer.update_vertex_preview(&vp, at, Some(&node_ref(1000)), true);
    assert_eq!(layer.vertex_preview_way(), None);
    preview_100(&mut layer, &vp);
    layer.update_vertex_preview(&vp, at, Some(&way_ref(999)), true);
    assert_eq!(layer.vertex_preview_way(), None);
    // Hovering a feature of another layer drops it too.
    preview_100(&mut layer, &vp);
    let foreign = FeatureRef {
        layer_id: LayerId(2),
        kind: FeatureKind::Way,
        id: 100,
    };
    layer.update_vertex_preview(&vp, at, Some(&foreign), true);
    assert_eq!(layer.vertex_preview_way(), None);
    // Disabled (other mode, dialog, imagery offset) clears it.
    preview_100(&mut layer, &vp);
    layer.update_vertex_preview(&vp, at, Some(&way_ref(100)), false);
    assert_eq!(layer.vertex_preview_way(), None);
}

#[test]
fn box_select_counts_previewed_vertices_as_drawn_only_while_previewed() {
    let mut layer = way_with_poi_layer(60);
    let vp = view_at(19.0);
    let rect = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(800.0), px(600.0)),
    };
    let has_vertex = |layer: &OsmLayer| {
        layer
            .hit_test_rect(&vp, rect)
            .iter()
            .any(|f| f.kind == FeatureKind::Node && f.id == 30)
    };
    assert!(!has_vertex(&layer));
    preview_100(&mut layer, &vp);
    assert!(has_vertex(&layer));
    // The viewer clears the preview as soon as a box-select drag moves.
    let at = node_point(&layer, &vp, 30);
    layer.update_vertex_preview(&vp, at, None, false);
    assert!(!has_vertex(&layer));
}
