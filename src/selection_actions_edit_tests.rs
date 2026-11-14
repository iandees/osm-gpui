//! Refused edits inside selection actions: the up-front check of Merge Nodes,
//! and the roll-back of a step that stopped part way.

use super::*;
use crate::test_support::Build;
use osm_gpui::layers::osm_layer::OsmLayer;
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

    let editable = layers
        .find_layer_mut(id)
        .unwrap()
        .as_editable_mut()
        .unwrap();
    let result = record_edit(id, editable, |rec| {
        // Two steps of an action are applied...
        rec.set_way_nodes(10, vec![1, 3]).map_err(refused)?;
        rec.delete_feature(FeatureKind::Node, 2).map_err(refused)?;
        // ...and the third is refused: node 3 is still in way 10.
        rec.delete_feature(FeatureKind::Node, 3).map_err(refused)
    });
    let message = result.unwrap_err();

    assert_eq!(snapshot(&layers, id).0, before.0);
    assert!(message.contains("node 3"), "{message}");
}

#[test]
fn a_successful_edit_hands_back_what_it_recorded() {
    let (mut layers, id) = layers_with(Build::new().nodes(&[1, 2, 3]).done());
    let editable = layers
        .find_layer_mut(id)
        .unwrap()
        .as_editable_mut()
        .unwrap();
    let (new_id, ops) = record_edit(id, editable, |rec| {
        rec.commit_node_moves(&[(1, 5.0, 6.0)]).map_err(refused)?;
        Ok(rec.add_node(1.0, 2.0))
    })
    .unwrap();
    assert!(new_id < 0);
    assert!(matches!(ops[..], [EditOp::MoveNodes { .. }, EditOp::PutFeature { .. }]));
}

#[test]
fn a_failed_edit_leaves_tags_and_positions_as_they_were() {
    let (mut layers, id) = layers_with(Build::new().nodes(&[1, 2]).way(10, &[1, 2], &[]).done());
    let before = snapshot(&layers, id);
    let editable = layers
        .find_layer_mut(id)
        .unwrap()
        .as_editable_mut()
        .unwrap();
    let result = record_edit(id, editable, |rec| {
        rec.set_tag(FeatureKind::Way, 10, "highway", "path")
            .map_err(refused)?;
        rec.commit_node_moves(&[(1, 5.0, 6.0), (2, 7.0, 8.0)])
            .map_err(refused)?;
        rec.reverse_way(10).map_err(refused)?;
        // No such way.
        rec.set_way_nodes(99, vec![1, 2]).map_err(refused)
    });
    assert!(result.is_err());
    assert_eq!(snapshot(&layers, id).0, before.0);
}
