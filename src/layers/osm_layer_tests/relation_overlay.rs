//! Relation overlays: member highlights and restriction markers.

use super::super::{lat_lon_to_mercator, OsmLayer, RESTRICTION_OFFSET_PX};
use crate::layers::{EditError, EditableLayer, LayerId};
use crate::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
use crate::selection::FeatureKind;
use crate::viewport::Viewport;
use gpui::{point, px, size};
use std::collections::HashMap;
use std::sync::Arc;

fn node(id: i64, lat: f64, lon: f64) -> OsmNode {
    OsmNode {
        id,
        lat,
        lon,
        version: 1,
        tags: HashMap::new(),
    }
}

fn way(id: i64, nodes: &[i64]) -> OsmWay {
    OsmWay {
        id,
        nodes: nodes.to_vec(),
        version: 1,
        tags: HashMap::from([("highway".to_string(), "residential".to_string())]),
    }
}

fn rel(id: i64, tags: &[(&str, &str)], members: &[(MemberType, i64, &str)]) -> OsmRelation {
    OsmRelation {
        id,
        version: 1,
        tags: tags
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        members: members
            .iter()
            .map(|&(member_type, reference, role)| OsmMember {
                member_type,
                reference,
                role: role.to_string(),
            })
            .collect(),
    }
}

/// Ways 1 (nodes 1-2) and 2 (nodes 2-3) meet at node 2. Route 10 (bus)
/// has both, route 11 (bicycle, colour tag) has way 2 and a way that
/// isn't loaded. Restriction 20 is a `no_left_turn` via node 2, 21 an
/// `only_straight_on` via a node that isn't loaded.
fn layer() -> OsmLayer {
    use MemberType::*;
    let mut nodes = HashMap::new();
    for n in [
        node(4, 40.002, -73.99),
        node(1, 40.0, -74.0),
        node(2, 40.0, -73.999),
        node(3, 40.001, -73.999),
    ] {
        nodes.insert(n.id, n);
    }
    let mut ways = HashMap::new();
    for w in [way(1, &[1, 2]), way(2, &[2, 3])] {
        ways.insert(w.id, w);
    }
    let data = OsmData {
        nodes,
        ways,
        relations: vec![
            rel(
                11,
                &[
                    ("type", "route"),
                    ("route", "bicycle"),
                    ("colour", "#ff0000"),
                ],
                &[(Way, 2, ""), (Way, 99, "")],
            ),
            rel(
                10,
                &[("type", "route"), ("route", "bus")],
                &[(Way, 1, ""), (Way, 2, "")],
            ),
            rel(
                12,
                &[("type", "route_master"), ("route", "bus")],
                &[(Way, 1, "")],
            ),
            rel(
                20,
                &[("type", "restriction"), ("restriction", "no_left_turn")],
                &[(Way, 1, "from"), (Node, 2, "via"), (Way, 2, "to")],
            ),
            rel(
                21,
                &[("type", "restriction"), ("restriction", "only_straight_on")],
                &[(Way, 1, "from"), (Node, 777, "via"), (Way, 2, "to")],
            ),
            rel(
                22,
                &[("type", "restriction"), ("restriction", "no_u_turn")],
                &[(Way, 1, "from"), (Node, 4, "via"), (Way, 2, "to")],
            ),
        ],
        bounds: Vec::new(),
    };
    OsmLayer::new_with_data(LayerId(1), "L", Arc::new(data))
}

#[test]
fn a_way_in_several_routes_gets_the_lowest_relation_id() {
    let layer = layer();
    assert_eq!(layer.route_overlay_colour(1), Some(0x9B30D9));
    // Routes 10 (bus) and 11 (red): 10 wins.
    assert_eq!(layer.route_overlay_colour(2), Some(0x9B30D9));
    // Not a member of anything, or not loaded.
    assert_eq!(layer.route_overlay_colour(98), None);
}

#[test]
fn route_edits_follow_the_relation() {
    let mut layer = layer();
    layer
        .set_tag(FeatureKind::Relation, 10, "type", "route_master")
        .unwrap();
    assert_eq!(layer.route_overlay_colour(1), None);
    assert_eq!(layer.route_overlay_colour(2), Some(0xFF0000));
    layer
        .set_tag(FeatureKind::Relation, 10, "type", "route")
        .unwrap();
    assert_eq!(layer.route_overlay_colour(1), Some(0x9B30D9));
}

#[test]
fn restriction_markers_sit_at_the_via_and_skip_missing_vias() {
    let layer = layer();
    let markers = layer.restriction_markers();
    // 21's via node isn't loaded: no marker.
    assert_eq!(markers.iter().map(|m| m.0).collect::<Vec<_>>(), [20, 22]);
    let (_, x, y, only) = markers[0];
    assert!(!only);
    let (ex, ey) = lat_lon_to_mercator(40.0, -73.999);
    assert!((x - ex).abs() < 1e-6 && (y - ey).abs() < 1e-6);
}

#[test]
fn moving_or_deleting_the_via_node_updates_the_marker() {
    let mut layer = layer();
    layer.commit_node_moves(&[(2, 40.0005, -73.998)]).unwrap();
    let (_, x, y, _) = layer.restriction_markers()[0];
    let (ex, ey) = lat_lon_to_mercator(40.0005, -73.998);
    assert!((x - ex).abs() < 1e-6 && (y - ey).abs() < 1e-6);

    // Node 4 is a standalone via: the restriction keeps it from being
    // deleted, but it can be purged (the restriction then has no marker).
    assert!(matches!(
        layer.delete_feature(FeatureKind::Node, 4),
        Err(EditError::InRelation {
            id: 4,
            relation: 22,
            ..
        })
    ));
    let purged = layer.purge_features(&[4], &[], &[]);
    assert_eq!(layer.restriction_markers().len(), 1);
    layer.restore_purged(purged);
    assert_eq!(layer.restriction_markers().len(), 2);
}

#[test]
fn clicking_the_marker_selects_the_restriction_only_at_vertex_zoom() {
    let mut layer = layer();
    let v = Viewport::new(40.0005, -73.9985, 18.0, size(px(800.0), px(600.0)));
    let via = v.geo_to_screen(40.0, -73.999);
    let on_marker = point(
        via.x + px(RESTRICTION_OFFSET_PX.0),
        via.y + px(RESTRICTION_OFFSET_PX.1),
    );
    let hit = layer.hit_test(&v, on_marker);
    assert_eq!(hit.len(), 1);
    assert_eq!(
        (hit[0].kind, hit[0].feature.id),
        (FeatureKind::Relation, 20)
    );
    // The via node itself is still the node (once it is selected: a
    // plain vertex is not otherwise a target).
    layer.set_highlight(&[crate::selection::FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Node,
        id: 2,
    }]);
    let hit = layer.hit_test(&v, point(via.x, via.y));
    assert_eq!(hit[0].kind, FeatureKind::Node);
    // Zoomed out, no marker to click.
    let far = Viewport::new(40.0005, -73.9985, 12.0, size(px(800.0), px(600.0)));
    let via = far.geo_to_screen(40.0, -73.999);
    let hit = layer.hit_test(
        &far,
        point(
            via.x + px(RESTRICTION_OFFSET_PX.0),
            via.y + px(RESTRICTION_OFFSET_PX.1),
        ),
    );
    assert!(hit.iter().all(|h| h.kind != FeatureKind::Relation));
}
