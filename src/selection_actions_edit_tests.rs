//! Refused edits inside selection actions: the up-front check of Merge Nodes,
//! and the roll-back of a step that stopped part way.

use super::*;
use crate::test_support::Build;
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::layers::EditError;
use std::sync::Arc;

fn layers_with(data: osm_gpui::osm::OsmData) -> (LayerManager, LayerId) {
    let mut layers = LayerManager::new();
    let id = layers.alloc_id();
    layers.add_layer(Box::new(OsmLayer::new_with_data(id, "L", Arc::new(data))));
    (layers, id)
}

fn snapshot(layers: &LayerManager, id: LayerId) -> (osm_gpui::osm::OsmData, bool) {
    let osm = layers.find_layer(id).unwrap().as_osm().unwrap();
    ((*osm.get_osm_data().unwrap()).clone(), osm.is_modified())
}

fn node(layer: LayerId, id: i64) -> FeatureRef {
    FeatureRef {
        layer_id: layer,
        kind: FeatureKind::Node,
        id,
    }
}

#[test]
fn merge_nodes_leaves_no_way_pointing_at_a_removed_node() {
    let (mut layers, id) = layers_with(
        Build::new()
            .nodes(&[1, 2, 3, 4])
            .way(10, &[1, 2], &[])
            .way(11, &[3, 4], &[])
            .done(),
    );
    let outcome = apply_merge(&mut layers, &[node(id, 2), node(id, 3)], Some(id)).unwrap();
    assert!(outcome.undo.is_some());
    let (data, _) = snapshot(&layers, id);
    assert_eq!(data.nodes.len(), 3);
    for way in data.ways.values() {
        assert!(way.nodes.iter().all(|n| data.nodes.contains_key(n)));
    }
}

#[test]
fn the_merge_check_names_a_node_a_way_would_still_use() {
    let lists: [(i64, &[i64]); 2] = [(10, &[1, 2]), (11, &[3, 9])];
    assert_eq!(node_left_in_a_way(&[2, 9], &lists), Some(2));
    assert_eq!(node_left_in_a_way(&[5, 6], &lists), None);
    assert_eq!(node_left_in_a_way(&[], &lists), None);
}

#[test]
fn a_refused_step_undoes_what_was_applied_and_reports_it() {
    let (mut layers, id) = layers_with(
        Build::new()
            .nodes(&[1, 2, 3])
            .way(10, &[1, 2, 3], &[])
            .done(),
    );
    let before = snapshot(&layers, id);

    // Two steps of an action were applied...
    let editable = layers
        .find_layer_mut(id)
        .unwrap()
        .as_editable_mut()
        .unwrap();
    editable.set_way_nodes(10, vec![1, 3]).unwrap();
    let removed = editable.delete_feature(FeatureKind::Node, 2).unwrap();
    let done = vec![
        UndoableAction::SetWayNodes {
            layer: id,
            way_id: 10,
            before: vec![1, 2, 3],
            after: vec![1, 3],
        },
        UndoableAction::DeleteFeature {
            layer: id,
            snapshot: removed,
        },
    ];
    // ...and the third was refused.
    let message = abort(&mut layers, &done, EditError::NodeInWay(7));

    assert_eq!(snapshot(&layers, id).0, before.0);
    assert!(message.contains("node 7"), "{message}");
}
