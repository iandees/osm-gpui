//! Creating, deleting and restoring nodes and ways.

use super::{data_with, empty_tags, viewport_centered_on};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::LayerId;
use crate::layers::{EditError, EditableLayer};
use crate::osm::{OsmNode, OsmWay};
use crate::selection::{FeatureKind, FeatureRef};
use gpui::{point, px};

#[test]
fn create_node_allocates_noncolliding_negative_id_and_is_hit_testable() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let new_lat = 40.5;
    let new_lon = -74.5;
    let id = layer
        .create_node(new_lat, new_lon, None)
        .expect("should allocate an id");
    assert!(id < 0, "new node id must be negative, got {}", id);
    assert!(layer.is_modified());

    let updated = layer.get_osm_data().unwrap();
    assert_eq!(
        updated.nodes.get(&id).map(|n| (n.lat, n.lon)),
        Some((new_lat, new_lon))
    );
    assert!(updated.nodes.get(&id).unwrap().tags.is_empty());

    let viewport = viewport_centered_on(new_lat, new_lon);
    let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert!(
        hits.iter()
            .any(|h| h.kind == FeatureKind::Node && h.feature.id == id),
        "got {:?}",
        hits
    );

    // A second auto-allocation must not collide with the first.
    let id2 = layer.create_node(new_lat, new_lon, None).unwrap();
    assert_ne!(id, id2);
}

#[test]
fn create_node_starts_below_existing_negative_ids() {
    // Simulates a reloaded, previously-saved-locally file that already
    // contains negative (not-yet-uploaded) ids.
    let n1 = OsmNode {
        id: -5,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 0,
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let id = layer.create_node(41.0, -75.0, None).unwrap();
    assert!(
        id < -5,
        "new id ({}) must not collide with existing negative id -5",
        id
    );
}

#[test]
fn create_node_with_explicit_id_fails_on_collision() {
    let n1 = OsmNode {
        id: -1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 0,
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    assert_eq!(
        layer.create_node(41.0, -75.0, Some(-1)),
        Err(EditError::IdInUse(FeatureKind::Node, -1))
    );
    assert_eq!(layer.create_node(41.0, -75.0, Some(-99)), Ok(-99));
}

#[test]
fn delete_feature_refuses_node_referenced_by_way_but_succeeds_once_way_deleted() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.001,
        lon: -74.001,
        tags: empty_tags(),
        version: 1,
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        tags: empty_tags(),
        version: 1,
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    assert_eq!(
        layer.delete_feature(FeatureKind::Node, 1).unwrap_err(),
        EditError::NodeInWay(1),
        "node still referenced by a way must be refused"
    );
    assert!(!layer.is_modified());

    assert!(layer.delete_feature(FeatureKind::Way, 10).is_ok());
    // Now node 1 is no longer referenced by any way.
    let snapshot = layer
        .delete_feature(FeatureKind::Node, 1)
        .expect("standalone node should delete");
    assert_eq!(snapshot.kind, FeatureKind::Node);
    assert_eq!(snapshot.id, 1);
    assert_eq!(snapshot.node_lat_lon, Some((40.0, -74.0)));

    let updated = layer.get_osm_data().unwrap();
    assert!(!updated.nodes.contains_key(&1));
}

#[test]
fn delete_feature_standalone_node_succeeds_directly() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let snapshot = layer
        .delete_feature(FeatureKind::Node, 1)
        .expect("standalone node should delete");
    assert!(layer.is_modified());
    assert_eq!(snapshot.node_lat_lon, Some((40.0, -74.0)));
    assert!(!layer.get_osm_data().unwrap().nodes.contains_key(&1));
    assert!(!layer.derived.node_cache.index_by_id.contains_key(&1));

    let viewport = viewport_centered_on(40.0, -74.0);
    let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert!(
        !hits.iter().any(|h| h.feature.id == 1),
        "deleted node must not be hit-testable: {:?}",
        hits
    );
}

#[test]
fn delete_feature_way_removes_way_but_keeps_nodes_and_fixes_other_ways() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.001,
        lon: -74.001,
        tags: empty_tags(),
        version: 1,
    };
    let n3 = OsmNode {
        id: 3,
        lat: 40.002,
        lon: -74.002,
        tags: empty_tags(),
        version: 1,
    };
    let mut tags10 = empty_tags();
    tags10.insert("highway".to_string(), "residential".to_string());
    let way_a = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        tags: tags10,
        version: 1,
    };
    // way_b also references node 1, to verify node_to_ways stays correct
    // for the surviving way after way_a is removed.
    let way_b = OsmWay {
        id: 20,
        nodes: vec![1, 3],
        tags: empty_tags(),
        version: 1,
    };
    let data = data_with(vec![n1, n2, n3], vec![way_a, way_b]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let snapshot = layer
        .delete_feature(FeatureKind::Way, 10)
        .expect("way should delete");
    assert!(layer.is_modified());
    assert_eq!(snapshot.kind, FeatureKind::Way);
    assert_eq!(snapshot.way_nodes, vec![1, 2]);
    assert_eq!(
        snapshot.tags,
        vec![("highway".to_string(), "residential".to_string())]
    );

    let updated = layer.get_osm_data().unwrap();
    assert!(!updated.ways.contains_key(&10), "way 10 must be gone");
    assert!(updated.ways.contains_key(&20), "way 20 must remain");
    // Member nodes of the deleted way are left in place.
    assert!(updated.nodes.contains_key(&1));
    assert!(updated.nodes.contains_key(&2));

    // way_b (still referencing node 1) must still be findable/correct.
    assert_eq!(layer.way_node_ids(20), Some(vec![1, 3]));
    layer.set_highlight(&[FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Way,
        id: 20,
    }]);
    let viewport = viewport_centered_on(40.0, -74.0);
    let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert!(hits
        .iter()
        .any(|h| h.feature.id == 1 && h.kind == FeatureKind::Node));
}

#[test]
fn delete_feature_missing_id_returns_none() {
    let data = data_with(vec![], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    assert_eq!(
        layer.delete_feature(FeatureKind::Node, 999).unwrap_err(),
        EditError::UnknownNode(999)
    );
    assert_eq!(
        layer.delete_feature(FeatureKind::Way, 999).unwrap_err(),
        EditError::UnknownWay(999)
    );
    assert!(!layer.is_modified());
}

#[test]
fn restore_feature_round_trips_deleted_node() {
    let mut tags = empty_tags();
    tags.insert("amenity".to_string(), "cafe".to_string());
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags,
        version: 1,
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let snapshot = layer.delete_feature(FeatureKind::Node, 1).unwrap();
    assert!(!layer.get_osm_data().unwrap().nodes.contains_key(&1));

    layer.restore_feature(snapshot).unwrap();
    let restored = layer.get_osm_data().unwrap();
    let node = restored.nodes.get(&1).expect("node should be restored");
    assert_eq!((node.lat, node.lon), (40.0, -74.0));
    assert_eq!(node.tags.get("amenity"), Some(&"cafe".to_string()));

    // Hit-testable again after restore.
    let viewport = viewport_centered_on(40.0, -74.0);
    let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
    assert!(hits
        .iter()
        .any(|h| h.feature.id == 1 && h.kind == FeatureKind::Node));
}

#[test]
fn restore_feature_round_trips_deleted_way() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version: 1,
    };
    let n2 = OsmNode {
        id: 2,
        lat: 40.001,
        lon: -74.001,
        tags: empty_tags(),
        version: 1,
    };
    let mut tags = empty_tags();
    tags.insert("highway".to_string(), "residential".to_string());
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        tags,
        version: 1,
    };
    let data = data_with(vec![n1, n2], vec![way]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let snapshot = layer.delete_feature(FeatureKind::Way, 10).unwrap();
    assert!(layer.way_node_ids(10).is_none());

    layer.restore_feature(snapshot).unwrap();
    assert_eq!(layer.way_node_ids(10), Some(vec![1, 2]));
    let restored = layer.get_osm_data().unwrap();
    let way = restored.ways.get(&10).unwrap();
    assert_eq!(way.tags.get("highway"), Some(&"residential".to_string()));

    // node_to_ways was patched so the restored way is picked up by a
    // node-move on one of its members.
    layer.commit_node_moves(&[(1, 41.0, -75.0)]).unwrap();
    let (mx, my) = crate::coordinates::lat_lon_to_mercator(41.0, -75.0);
    let verts =
        layer.derived.way_vertices[*layer.derived.way_id_to_index.get(&10).unwrap()].clone();
    let &(_, vmx, vmy) = verts.iter().find(|&&(id, _, _)| id == 1).unwrap();
    assert!((vmx - mx).abs() < 1e-6);
    assert!((vmy - my).abs() < 1e-6);
}

#[test]
fn undoing_a_delete_keeps_the_server_version_in_the_uploaded_modify() {
    let node = |id, version| OsmNode {
        id,
        lat: 40.0,
        lon: -74.0,
        tags: empty_tags(),
        version,
    };
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        tags: empty_tags(),
        version: 7,
    };
    let data = data_with(vec![node(1, 2), node(2, 2), node(3, 4)], vec![way]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    let way_snapshot = layer.delete_feature(FeatureKind::Way, 10).unwrap();
    let node_snapshot = layer.delete_feature(FeatureKind::Node, 3).unwrap();
    assert_eq!((way_snapshot.version, node_snapshot.version), (7, 4));
    // Redo (delete again after undo) reads the version from the layer.
    layer.restore_feature(node_snapshot.clone()).unwrap();
    layer.restore_feature(way_snapshot.clone()).unwrap();
    assert_eq!(layer.get_osm_data().unwrap().nodes[&3].version, 4);
    assert_eq!(layer.get_osm_data().unwrap().ways[&10].version, 7);
    // Undone: nothing to upload yet.
    assert!(layer.diff_for_upload().is_empty());

    layer
        .set_tag(FeatureKind::Way, 10, "highway", "path")
        .unwrap();
    layer
        .set_tag(FeatureKind::Node, 3, "amenity", "cafe")
        .unwrap();
    let xml = crate::osm_upload::build_osm_change_xml(1, &[("L", layer.diff_for_upload())]);
    assert!(xml.contains(r#"<way id="10" version="7""#), "{xml}");
    assert!(
        xml.contains(r#"<node id="3" lat="40" lon="-74" version="4""#),
        "{xml}"
    );

    // Deleting the restored objects again reports the original versions.
    layer.delete_feature(FeatureKind::Way, 10).unwrap();
    layer.delete_feature(FeatureKind::Node, 3).unwrap();
    let diff = layer.diff_for_upload();
    assert_eq!(diff.deleted_way_ids, vec![(10, 7)]);
    assert_eq!(diff.deleted_node_ids, vec![(3, 4)]);
}
