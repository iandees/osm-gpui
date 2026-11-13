//! The diff a layer reports for upload and how an upload result is applied.

use super::{data_with, empty_tags};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::LayerId;
use crate::layers::MapLayer;
use crate::osm::{OsmNode, OsmWay};
use crate::selection::FeatureKind;
use std::sync::Arc;

#[test]
fn diff_for_upload_no_changes_is_empty() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    assert!(layer.diff_for_upload().is_empty());
}

#[test]
fn diff_for_upload_detects_tag_edit_as_modified() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    layer.set_tag(FeatureKind::Node, 1, "amenity", "cafe");
    let diff = layer.diff_for_upload();
    assert_eq!(diff.modified_nodes.len(), 1);
    assert_eq!(diff.modified_nodes[0].id, 1);
    assert!(diff.created_nodes.is_empty());
    assert!(diff.deleted_node_ids.is_empty());
}

#[test]
fn diff_for_upload_detects_move_as_modified() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    layer.commit_node_moves(&[(1, 41.0, -75.0)]);
    let diff = layer.diff_for_upload();
    assert_eq!(diff.modified_nodes.len(), 1);
    assert_eq!(diff.modified_nodes[0].lat, 41.0);
}

#[test]
fn apply_upload_result_updates_version_for_modified_node() {
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    layer.commit_node_moves(&[(1, 41.0, -75.0)]);
    assert!(layer.is_modified());

    let mut result = crate::osm_upload::UploadResult::default();
    result.node_id_remap.insert(1, (1, 2));
    layer.apply_upload_result(&result);

    let updated = layer.get_osm_data().unwrap();
    assert_eq!(updated.nodes.get(&1).unwrap().version, 2);
    assert!(
        !layer.is_modified(),
        "layer should be clean after a reconciled upload"
    );
    assert!(
        layer.diff_for_upload().is_empty(),
        "new baseline should match current data"
    );
}

#[test]
fn apply_upload_result_remaps_created_node_id_in_referencing_way() {
    // A way that references a newly-created (negative local id) node.
    // After upload, that node gets a real server id — the way's `nodes`
    // list must be updated to point at the new id, or its geometry
    // would silently corrupt from then on.
    let n1 = OsmNode {
        id: 1,
        lat: 40.0,
        lon: -74.0,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    // Simulate a locally-created node (-1) and a way referencing both
    // the pre-existing node 1 and the new node -1, by mutating the
    // layer's data directly (create_node/append_way don't exist yet).
    let current = layer.get_osm_data().unwrap();
    let mut new_data = (*current).clone();
    new_data.nodes.insert(
        -1,
        OsmNode {
            id: -1,
            lat: 40.001,
            lon: -74.001,
            version: 0,
            tags: empty_tags(),
        },
    );
    new_data.ways.insert(
        -2,
        OsmWay {
            id: -2,
            nodes: vec![1, -1],
            version: 0,
            tags: empty_tags(),
        },
    );
    layer.set_osm_data_for_test(Arc::new(new_data));

    let diff = layer.diff_for_upload();
    assert_eq!(diff.created_nodes.len(), 1);
    assert_eq!(diff.created_ways.len(), 1);

    let mut result = crate::osm_upload::UploadResult::default();
    result.node_id_remap.insert(-1, (999, 1));
    result.way_id_remap.insert(-2, (888, 1));
    layer.apply_upload_result(&result);

    let updated = layer.get_osm_data().unwrap();
    assert!(
        updated.nodes.contains_key(&999),
        "new node id should be present"
    );
    assert!(
        !updated.nodes.contains_key(&-1),
        "old local id should be gone"
    );
    let way = updated.ways.get(&888).expect("way should have its new id");
    assert_eq!(
        way.nodes,
        vec![1, 999],
        "way must reference the node's NEW id, not the stale local one"
    );
    assert!(!layer.is_modified());
    assert!(layer.diff_for_upload().is_empty());
}
