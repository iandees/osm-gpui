//! Which nodes are drawn and clickable at each zoom.

use super::{node_point, view_at, way_with_poi_layer};
use crate::layers::node_markers::NodeForm;
use crate::layers::EditableLayer;
use crate::layers::{node_markers, LayerId};
use crate::selection::{FeatureKind, FeatureRef};
use crate::style::Stylesheet;
use gpui::{point, px, size, Bounds};
use std::collections::HashSet;
use std::sync::Arc;

#[test]
fn hidden_vertex_is_not_a_target_and_the_way_is_hit() {
    let layer = way_with_poi_layer(60);
    let vp = view_at(16.0);
    let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h.kind == FeatureKind::Way), "{hits:?}");
}

#[test]
fn unselected_vertex_is_never_a_target_even_at_high_zoom() {
    let layer = way_with_poi_layer(60);
    for z in [17.0, node_markers::FULL_MIN_ZOOM, 20.0] {
        let vp = view_at(z);
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Way), "z{z}");
    }
}

#[test]
fn vertex_of_a_selected_way_is_clickable_at_high_zoom() {
    let mut layer = way_with_poi_layer(60);
    layer.set_highlight(&[FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Way,
        id: 100,
    }]);
    for z in [18.0, 20.0] {
        let vp = view_at(z);
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
        assert_eq!(hits[0].kind, FeatureKind::Node, "z{z}");
        assert_eq!(hits[0].feature.id, 30, "z{z}");
    }
}

#[test]
fn standalone_untagged_node_is_a_target_like_a_tagged_one() {
    let layer = way_with_poi_layer(60);
    for z in [12.0, 16.0] {
        let vp = view_at(z);
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 2000));
        assert!(hits.iter().all(|h| h.feature.id != 2000), "z{z}");
    }
    for z in [17.0, 18.0, 20.0] {
        let vp = view_at(z);
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 2000));
        assert_eq!(hits[0].feature.id, 2000, "z{z}");
    }
}

#[test]
fn add_mode_snaps_to_an_undrawn_vertex() {
    let layer = way_with_poi_layer(60);
    for z in [14.0, 17.0, 19.0] {
        let vp = view_at(z);
        let near = node_point(&layer, &vp, 30);
        // Not a hit target, but a snap target.
        let hits = layer.hit_test(&vp, near);
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Way), "z{z}");
        assert_eq!(layer.snap_to_node(&vp, near, 8.0), Some(30), "z{z}");
    }
    // Far from any node: nothing to snap to.
    let vp = view_at(19.0);
    let mut far = node_point(&layer, &vp, 30);
    far.y += px(100.0);
    assert_eq!(layer.snap_to_node(&vp, far, 8.0), None);
}

#[test]
fn simple_tagged_node_is_a_target_at_low_zoom() {
    let layer = way_with_poi_layer(60);
    for z in [12.0, 16.0] {
        // Not drawn, so not a target.
        let vp = view_at(z);
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 1000));
        assert!(hits.iter().all(|h| h.feature.id != 1000), "z{z}");
    }
    for z in [node_markers::TAGGED_MIN_ZOOM, 17.5] {
        let vp = view_at(z);
        let hits = layer.hit_test(&vp, node_point(&layer, &vp, 1000));
        assert_eq!(hits[0].kind, FeatureKind::Node, "z{z}");
        assert_eq!(hits[0].feature.id, 1000);
        // Just outside the simple radius it is not hit.
        let mut away = node_point(&layer, &vp, 1000);
        away.x += px(node_markers::SIMPLE_HIT_RADIUS + 1.0);
        let hits = layer.hit_test(&vp, away);
        assert!(hits.iter().all(|h| h.feature.id != 1000), "z{z}");
    }
}

#[test]
fn nodes_of_a_selected_way_and_selected_nodes_are_targets_at_low_zoom() {
    let mut layer = way_with_poi_layer(60);
    let vp = view_at(16.0);
    layer.set_highlight(&[FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Way,
        id: 100,
    }]);
    // Vertices are about a pixel apart here: 30 is among the node hits.
    let hits = layer.hit_test(&vp, node_point(&layer, &vp, 30));
    assert!(hits.iter().all(|h| h.kind == FeatureKind::Node));
    assert!(hits.iter().any(|h| h.feature.id == 30));

    layer.set_highlight(&[FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Node,
        id: 20,
    }]);
    let hits = layer.hit_test(&vp, node_point(&layer, &vp, 20));
    assert_eq!(hits[0].feature.id, 20);
    assert_eq!(hits[0].kind, FeatureKind::Node);
}

#[test]
fn box_select_and_hit_test_agree_on_which_nodes_are_shown() {
    let layer = way_with_poi_layer(60);
    let rect = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(800.0), px(600.0)),
    };
    for z in [14.0, 16.0, 17.9, 18.0, 19.0] {
        let vp = view_at(z);
        let boxed: HashSet<i64> = layer
            .hit_test_rect(&vp, rect)
            .into_iter()
            .filter(|f| f.kind == FeatureKind::Node)
            .map(|f| f.id)
            .collect();
        for id in (1..=60).chain([1000, 2000]) {
            let sp = node_point(&layer, &vp, id);
            let in_view = sp.x >= px(0.0) && sp.x <= px(800.0) && sp.y >= px(0.0);
            let in_view = in_view && sp.y <= px(600.0);
            let clicked = layer
                .hit_test(&vp, sp)
                .iter()
                .any(|h| h.kind == FeatureKind::Node && h.feature.id == id);
            if in_view && boxed.contains(&id) {
                assert!(clicked, "z{z}: node {id} boxed but not clickable");
            }
            if id >= 1000 {
                assert_eq!(
                    boxed.contains(&id),
                    z >= node_markers::TAGGED_MIN_ZOOM,
                    "z{z}: tagged/standalone node {id} boxed only from its minimum zoom"
                );
            } else {
                assert!(!boxed.contains(&id), "z{z}: undrawn vertex {id} boxed");
            }
        }
    }
}

#[test]
fn marker_form_follows_the_view_in_the_layer() {
    let layer = way_with_poi_layer(5);
    let pins = layer.pins();
    let vertex = layer.node_cache.index_by_id[&3];
    let poi = layer.node_cache.index_by_id[&1000];
    assert_eq!(layer.node_form_at(&pins, vertex, 3, 17.0), NodeForm::Hidden);
    assert_eq!(layer.node_form_at(&pins, vertex, 3, 18.5), NodeForm::Hidden);
    assert_eq!(
        layer.node_form_at(&pins, poi, 1000, 17.0),
        NodeForm::Simple(node_markers::SIMPLE_SIZE)
    );
    let stray = layer.node_cache.index_by_id[&2000];
    assert_eq!(
        layer.node_form_at(&pins, stray, 2000, 17.0),
        NodeForm::Simple(node_markers::SIMPLE_SIZE)
    );
    assert_eq!(layer.node_form_at(&pins, stray, 2000, 18.5), NodeForm::Full);
}

#[test]
fn a_stylesheet_with_square_and_cross_markers_shows_every_node_from_z16() {
    let mut layer = way_with_poi_layer(5);
    let wire = crate::style::custom::BUILT_IN_STYLES
        .iter()
        .find(|s| s.id == "wireframe")
        .unwrap();
    layer.set_stylesheet(Arc::new(Stylesheet::parse(wire.source).unwrap()));
    let pins = layer.pins();
    let vertex = layer.node_cache.index_by_id[&3];
    let poi = layer.node_cache.index_by_id[&1000];
    let stray = layer.node_cache.index_by_id[&2000];
    let form = |idx, id, z| layer.node_form_at(&pins, idx, id, z);
    // Vertices are hidden when zoomed out, then a small square.
    assert_eq!(form(vertex, 3, 15.0), NodeForm::Hidden);
    let NodeForm::Styled(v) = form(vertex, 3, 16.0) else {
        panic!("vertex should be styled");
    };
    assert_eq!(v.shape, crate::style::mapcss::NodeShape::Square);
    // Tagged and free-standing nodes are crosses.
    let NodeForm::Styled(p) = form(poi, 1000, 16.0) else {
        panic!("poi should be styled");
    };
    assert_eq!(p.shape, crate::style::mapcss::NodeShape::Cross);
    let NodeForm::Styled(f) = form(stray, 2000, 19.0) else {
        panic!("free node should be styled");
    };
    assert_eq!(f.shape, crate::style::mapcss::NodeShape::Cross);
    assert!(NodeForm::Styled(v).hit_radius() > 0.0);
}

#[test]
fn relation_membership_restyles_ways_when_a_relation_changes() {
    let mut layer = way_with_poi_layer(5);
    let wire = crate::style::custom::BUILT_IN_STYLES
        .iter()
        .find(|s| s.id == "wireframe")
        .unwrap();
    layer.set_stylesheet(Arc::new(Stylesheet::parse(wire.source).unwrap()));
    let idx = layer.way_id_to_index[&100];
    assert_eq!(layer.way_styles[idx].casing_width, 0.0);
    let members = vec![crate::osm::OsmMember {
        member_type: crate::osm::MemberType::Way,
        reference: 100,
        role: String::new(),
    }];
    let rel = layer.create_relation(vec![], members.clone()).unwrap();
    assert!(layer.way_styles[idx].casing_width > 0.0);
    let snapshot = layer.delete_relation(rel).unwrap();
    assert_eq!(layer.way_styles[idx].casing_width, 0.0);
    assert!(layer.restore_relation(snapshot).is_ok());
    assert!(layer.way_styles[idx].casing_width > 0.0);
    let rel2 = layer.relations()[0].id;
    layer.set_relation_members(rel2, vec![]).unwrap();
    assert_eq!(layer.way_styles[idx].casing_width, 0.0);
}

#[test]
fn box_select_includes_vertices_of_a_selected_way() {
    let mut layer = way_with_poi_layer(60);
    layer.set_highlight(&[FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Way,
        id: 100,
    }]);
    let rect = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(800.0), px(600.0)),
    };
    let vp = view_at(19.0);
    let boxed = layer.hit_test_rect(&vp, rect);
    assert!(boxed
        .iter()
        .any(|f| f.kind == FeatureKind::Node && f.id == 30));
}
