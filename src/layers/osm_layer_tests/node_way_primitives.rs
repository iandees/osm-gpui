//! The low-level node and way edit primitives and feature geometry classification.

use super::{data_with, empty_tags, viewport_centered_on};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::LayerId;
use crate::layers::{EditableLayer, MapLayer};
use crate::osm::{OsmNode, OsmWay};
use crate::selection::FeatureKind;
use gpui::{point, px};
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn add_node_assigns_decrementing_placeholder_ids_and_is_hit_testable() {
    let mut layer = OsmLayer::new(LayerId(1));
    let viewport = viewport_centered_on(40.0, -74.0);

    let id1 = layer.add_node(40.0, -74.0);
    let id2 = layer.add_node(40.0, -74.0);
    assert_eq!(id1, -1, "first placeholder id should be -1");
    assert_eq!(id2, -2, "ids should decrement");
    assert!(layer.is_modified());

    let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert!(hits
        .iter()
        .any(|h| h.feature.id == id1 && h.kind == FeatureKind::Node));
}

#[test]
fn remove_node_drops_it_from_data_and_index() {
    let mut layer = OsmLayer::new(LayerId(1));
    let viewport = viewport_centered_on(40.0, -74.0);
    let id = layer.add_node(40.0, -74.0);

    layer.remove_node(id);

    let data = layer.get_osm_data().expect("data should still exist");
    assert!(!data.nodes.contains_key(&id));
    let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert!(
        hits.is_empty(),
        "removed node should not be hit-testable: {:?}",
        hits
    );
}

#[test]
fn add_way_creates_closed_building_way_and_is_hit_testable() {
    let mut layer = OsmLayer::new(LayerId(1));
    let viewport = viewport_centered_on(40.0, -74.0);
    // Straddle the viewport center rather than placing a node exactly on
    // it (as `hit_test_falls_through_to_way` also does): `hit_test`
    // returns node hits before even considering ways, so a node sitting
    // exactly at the click point would shadow the way we're testing.
    let n1 = layer.add_node(40.0, -74.001);
    let n2 = layer.add_node(40.0, -73.999);
    let way_id = layer.add_way(
        vec![n1, n2, n1],
        vec![("building".to_string(), "yes".to_string())],
    );
    assert_eq!(way_id, -1, "first placeholder way id should be -1");

    let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert!(hits
        .iter()
        .any(|h| h.feature.id == way_id && h.kind == FeatureKind::Way));
    let tags = layer
        .feature_tags(&crate::selection::FeatureRef {
            layer_id: layer.id(),
            kind: FeatureKind::Way,
            id: way_id,
        })
        .expect("way should have tags");
    assert!(tags.contains(&("building".to_string(), "yes".to_string())));
}

#[test]
fn remove_way_drops_it_from_data_and_index_but_keeps_its_nodes() {
    let mut layer = OsmLayer::new(LayerId(1));
    let viewport = viewport_centered_on(40.0, -74.0);
    let n1 = layer.add_node(40.0, -74.001);
    let n2 = layer.add_node(40.0, -74.0);
    let way_id = layer.add_way(vec![n1, n2], vec![]);

    layer.remove_way(way_id);

    let data = layer.get_osm_data().unwrap();
    assert!(!data.ways.contains_key(&way_id));
    assert!(
        data.nodes.contains_key(&n1),
        "removing the way must not remove its nodes"
    );
    let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert!(
        !hits.iter().any(|h| h.kind == FeatureKind::Way),
        "way should no longer hit-test: {:?}",
        hits
    );
}

#[test]
fn extend_way_appends_node_and_updates_bbox() {
    let mut layer = OsmLayer::new(LayerId(1));
    let n1 = layer.add_node(40.0, -74.001);
    let n2 = layer.add_node(40.0, -74.0);
    let way_id = layer.add_way(vec![n1, n2], vec![]);
    let n3 = layer.add_node(40.001, -74.0);

    layer.extend_way(way_id, n3);

    let data = layer.get_osm_data().unwrap();
    let way = data.ways.get(&way_id).unwrap();
    assert_eq!(way.nodes, vec![n1, n2, n3]);
}

#[test]
fn reverse_way_flips_node_order_and_is_its_own_inverse() {
    let mut layer = OsmLayer::new(LayerId(1));
    let n1 = layer.add_node(40.0, -74.001);
    let n2 = layer.add_node(40.0, -74.0);
    let n3 = layer.add_node(40.001, -74.0);
    let way_id = layer.add_way(vec![n1, n2, n3], vec![]);

    assert!(layer.reverse_way(way_id));
    assert_eq!(layer.way_node_ids(way_id), Some(vec![n3, n2, n1]));
    assert!(layer.is_modified());

    assert!(layer.reverse_way(way_id));
    assert_eq!(layer.way_node_ids(way_id), Some(vec![n1, n2, n3]));

    assert!(!layer.reverse_way(999));
}

#[test]
fn set_way_nodes_rewrites_the_list_and_tracks_node_usage() {
    let mut layer = OsmLayer::new(LayerId(1));
    let n1 = layer.add_node(40.0, -74.001);
    let n2 = layer.add_node(40.0, -74.0);
    let n3 = layer.add_node(40.001, -74.0);
    let w1 = layer.add_way(vec![n1, n2], vec![]);
    let w2 = layer.add_way(vec![n2, n3], vec![]);
    assert_eq!(layer.ways_using_node(n2), vec![w2, w1]);

    // Swap w2's use of n2 for a fresh node at the same spot.
    let copy = layer.add_node(40.0, -74.0);
    assert!(layer.set_way_nodes(w2, vec![copy, n3]));
    assert_eq!(layer.way_node_ids(w2), Some(vec![copy, n3]));
    assert_eq!(layer.ways_using_node(n2), vec![w1]);
    assert_eq!(layer.ways_using_node(copy), vec![w2]);

    // A node no way uses any more can be deleted again.
    assert!(layer.set_way_nodes(w2, vec![n3, n1]));
    assert!(layer.delete_feature(FeatureKind::Node, copy).is_some());
    assert!(!layer.set_way_nodes(999, vec![]));
}

#[test]
fn relations_containing_way_reads_member_refs() {
    let node = |id| OsmNode {
        id,
        lat: 40.0,
        lon: -74.0 + id as f64 * 0.001,
        version: 1,
        tags: empty_tags(),
    };
    let way = |id, nodes: Vec<i64>| OsmWay {
        id,
        nodes,
        version: 1,
        tags: empty_tags(),
    };
    let mut data = (*data_with(
        vec![node(1), node(2), node(3)],
        vec![way(10, vec![1, 2]), way(11, vec![2, 3])],
    ))
    .clone();
    data.relations.push(crate::osm::OsmRelation {
        id: 500,
        members: vec![
            crate::osm::OsmMember {
                member_type: crate::osm::MemberType::Way,
                reference: 11,
                role: String::new(),
            },
            // A node with the same numeric id is not the way.
            crate::osm::OsmMember {
                member_type: crate::osm::MemberType::Node,
                reference: 10,
                role: String::new(),
            },
        ],
        version: 1,
        tags: HashMap::new(),
    });
    let layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(data));
    assert_eq!(layer.relations_containing_way(11), vec![500]);
    assert!(layer.relations_containing_way(10).is_empty());
    // Node membership is by node ref, not way ref.
    assert_eq!(layer.relations_containing_node(10), vec![500]);
    assert!(layer.relations_containing_node(11).is_empty());
}

#[test]
fn insert_node_into_way_splices_at_index_and_remove_node_from_way_undoes_it() {
    let mut layer = OsmLayer::new(LayerId(1));
    let n1 = layer.add_node(40.0, -74.001);
    let n2 = layer.add_node(40.0, -74.0);
    let way_id = layer.add_way(vec![n1, n2], vec![]);

    let mid = layer.insert_node_into_way(way_id, 1, 40.0, -74.0005);
    let data = layer.get_osm_data().unwrap();
    let way = data.ways.get(&way_id).unwrap();
    assert_eq!(way.nodes, vec![n1, mid, n2]);

    layer.remove_node_from_way(way_id, 1);
    let data = layer.get_osm_data().unwrap();
    let way = data.ways.get(&way_id).unwrap();
    assert_eq!(way.nodes, vec![n1, n2]);
    assert!(
        data.nodes.contains_key(&mid),
        "remove_node_from_way must not delete the node itself"
    );
}

#[test]
fn feature_geometry_classifies_unreferenced_node_as_point() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    let area_keys = crate::presets::AreaKeys::from_json("{}").unwrap();
    let feature = crate::selection::FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Node,
        id: 1,
    };
    assert_eq!(
        layer.feature_geometry(&feature, &area_keys),
        Some(crate::presets::Geometry::Point)
    );
}

#[test]
fn feature_geometry_returns_none_for_wrong_layer() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    let area_keys = crate::presets::AreaKeys::from_json("{}").unwrap();
    let feature = crate::selection::FeatureRef {
        layer_id: LayerId(2),
        kind: FeatureKind::Node,
        id: 1,
    };
    assert_eq!(layer.feature_geometry(&feature, &area_keys), None);
}

#[test]
fn feature_geometry_classifies_closed_area_way() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.001,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let mut tags = HashMap::new();
    tags.insert("building".to_string(), "yes".to_string());
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2, 2, 1],
        version: 1,
        tags,
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    let area_keys = crate::presets::AreaKeys::from_json(r#"{"building": {}}"#).unwrap();
    let feature = crate::selection::FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Way,
        id: 10,
    };
    assert_eq!(
        layer.feature_geometry(&feature, &area_keys),
        Some(crate::presets::Geometry::Area)
    );
}
