//! The edit primitives of `EditableLayer`: the merged pairs behave as the old
//! separate functions did, a refused edit changes nothing, and new ids never
//! collide with ids in the data.

use super::*;
use osm_gpui::layers::{EditError, EditableLayer};

fn layer_with(data: OsmData) -> OsmLayer {
    OsmLayer::new_with_data(LayerId(1), "L", Arc::new(data))
}

type Observed = (
    OsmData,
    Vec<(i64, Vec<i64>)>,
    Vec<(i64, Option<(f64, f64)>)>,
);

/// Everything about the layer a caller can observe. After an edit this must
/// equal what a layer freshly loaded from the same data shows.
fn observable(layer: &OsmLayer) -> Observed {
    let data = (*layer.get_osm_data().unwrap()).clone();
    let mut users: Vec<(i64, Vec<i64>)> = data
        .nodes
        .keys()
        .map(|&id| (id, layer.ways_using_node(id)))
        .collect();
    users.sort();
    let mut positions: Vec<(i64, Option<(f64, f64)>)> = data
        .nodes
        .keys()
        .map(|&id| (id, layer.node_lat_lon(id)))
        .collect();
    positions.sort_by_key(|p| p.0);
    (data, users, positions)
}

fn assert_same_as_fresh(layer: &OsmLayer) {
    let fresh = layer_with((*layer.get_osm_data().unwrap()).clone());
    assert_eq!(observable(layer), observable(&fresh));
    assert_eq!(layer.all_features(), fresh.all_features());
}

fn network() -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3], &[("highway", "path")])
        .way(11, &[3, 4], &[])
        .done()
}

#[test]
fn add_node_and_create_node_without_an_id_are_the_same_operation() {
    let mut a = layer_with(network());
    let mut b = layer_with(network());
    let from_add = a.add_node(40.5, -74.5);
    let from_create = b.create_node(40.5, -74.5, None).unwrap();
    assert_eq!(from_add, from_create);
    assert!(from_add < 0);
    assert_eq!(observable(&a), observable(&b));
    assert!(a.is_modified() && b.is_modified());
    assert_same_as_fresh(&a);
}

#[test]
fn create_node_on_a_layer_without_data_creates_it() {
    let mut layer = OsmLayer::new(LayerId(1));
    let id = layer.create_node(40.0, -74.0, None).unwrap();
    assert_eq!(layer.node_lat_lon(id), Some((40.0, -74.0)));
    let mut redo = OsmLayer::new(LayerId(2));
    assert_eq!(redo.create_node(40.0, -74.0, Some(-50)), Ok(-50));
    assert_eq!(
        redo.create_node(41.0, -74.0, Some(-50)),
        Err(EditError::IdInUse(FeatureKind::Node, -50))
    );
}

#[test]
fn remove_node_and_delete_feature_leave_the_same_layer() {
    let data = Build::new().nodes(&[1, 2, 3]).way(10, &[1, 2], &[]).done();
    let mut removed = layer_with(data.clone());
    let mut deleted = layer_with(data);
    removed.remove_node(3).unwrap();
    deleted.delete_feature(FeatureKind::Node, 3).unwrap();
    assert_eq!(observable(&removed), observable(&deleted));
    assert_same_as_fresh(&removed);
}

#[test]
fn a_node_still_in_a_way_is_refused_by_both_removals_and_nothing_changes() {
    let mut layer = layer_with(network());
    let before = observable(&layer);
    let revision = layer.edit_revision();
    assert_eq!(layer.remove_node(2), Err(EditError::NodeInWay(2)));
    assert_eq!(
        layer.delete_feature(FeatureKind::Node, 2).unwrap_err(),
        EditError::NodeInWay(2)
    );
    assert_eq!(layer.remove_node(99), Err(EditError::UnknownNode(99)));
    assert_eq!(observable(&layer), before);
    assert_eq!(layer.edit_revision(), revision);
    assert!(!layer.is_modified());
}

#[test]
fn undoing_a_creation_removes_the_way_before_its_nodes() {
    let mut layer = layer_with(network());
    let a = layer.add_node(40.1, -74.1);
    let b = layer.add_node(40.2, -74.2);
    let way = layer.add_way(vec![a, b], vec![]);
    assert_eq!(layer.remove_node(a), Err(EditError::NodeInWay(a)));
    layer.remove_way(way).unwrap();
    layer.remove_node(a).unwrap();
    layer.remove_node(b).unwrap();
    assert_same_as_fresh(&layer);
}

#[test]
fn remove_way_and_delete_feature_leave_the_same_layer() {
    let mut removed = layer_with(network());
    let mut deleted = layer_with(network());
    removed.remove_way(10).unwrap();
    let snapshot = deleted.delete_feature(FeatureKind::Way, 10).unwrap();
    assert_eq!(snapshot.way_nodes, vec![1, 2, 3]);
    assert_eq!(observable(&removed), observable(&deleted));
    assert_same_as_fresh(&removed);
    assert_eq!(removed.remove_way(10), Err(EditError::UnknownWay(10)));
}

#[test]
fn add_way_and_restore_feature_build_the_same_way() {
    let mut added = layer_with(network());
    let id = added.add_way(vec![1, 4], vec![("name".into(), "x".into())]);
    let mut restored = layer_with(network());
    restored
        .restore_feature(osm_gpui::selection::DeletedFeatureSnapshot {
            kind: FeatureKind::Way,
            id,
            tags: vec![("name".into(), "x".into())],
            way_nodes: vec![1, 4],
            node_lat_lon: None,
            version: 0,
        })
        .unwrap();
    // `add_way` starts a way at version 0 and so does this snapshot.
    assert_eq!(observable(&added), observable(&restored));
    assert_same_as_fresh(&added);
    assert_same_as_fresh(&restored);
}

#[test]
fn delete_feature_refuses_a_relation_and_restore_feature_a_taken_id() {
    let mut layer = layer_with(
        Build::new()
            .nodes(&[1, 2])
            .way(10, &[1, 2], &[])
            .relation(500, &[("type", "route")], &[wm(10, "")])
            .done(),
    );
    assert_eq!(
        layer
            .delete_feature(FeatureKind::Relation, 500)
            .unwrap_err(),
        EditError::NotNodeOrWay(FeatureKind::Relation)
    );
    let snapshot = layer.delete_feature(FeatureKind::Way, 10).unwrap();
    layer.restore_feature(snapshot.clone()).unwrap();
    assert_eq!(
        layer.restore_feature(snapshot),
        Err(EditError::IdInUse(FeatureKind::Way, 10))
    );
}

#[test]
fn insert_node_into_way_with_a_bad_way_or_index_creates_nothing() {
    let mut layer = layer_with(network());
    let before = observable(&layer);
    let revision = layer.edit_revision();
    assert_eq!(
        layer.insert_node_into_way(99, 0, 40.0, -74.0),
        Err(EditError::UnknownWay(99))
    );
    assert_eq!(
        layer.insert_node_into_way(10, 4, 40.0, -74.0),
        Err(EditError::IndexOutOfRange {
            way_id: 10,
            index: 4,
            len: 3
        })
    );
    assert_eq!(observable(&layer), before);
    assert_eq!(layer.edit_revision(), revision);
    assert!(!layer.is_modified());

    // The end of the list is a valid position.
    let id = layer.insert_node_into_way(10, 3, 40.0, -74.0).unwrap();
    assert_eq!(layer.way_node_ids(10), Some(vec![1, 2, 3, id]));
    assert_same_as_fresh(&layer);
}

#[test]
fn the_way_edits_refuse_an_unknown_way_or_index_without_changing_anything() {
    let mut layer = layer_with(network());
    let before = observable(&layer);
    assert_eq!(layer.extend_way(99, 1), Err(EditError::UnknownWay(99)));
    assert_eq!(layer.reverse_way(99), Err(EditError::UnknownWay(99)));
    assert_eq!(
        layer.set_way_nodes(99, vec![]),
        Err(EditError::UnknownWay(99))
    );
    assert_eq!(
        layer.remove_node_from_way(10, 3),
        Err(EditError::IndexOutOfRange {
            way_id: 10,
            index: 3,
            len: 3
        })
    );
    assert_eq!(observable(&layer), before);
    assert!(!layer.is_modified());
}

#[test]
fn commit_node_moves_that_matches_nothing_changes_nothing() {
    let mut layer = layer_with(network());
    let before = observable(&layer);
    let revision = layer.edit_revision();
    assert_eq!(
        layer.commit_node_moves(&[(99, 41.0, -75.0)]),
        Err(EditError::UnknownNode(99))
    );
    // A move with one bad id moves none of them.
    assert_eq!(
        layer.commit_node_moves(&[(1, 41.0, -75.0), (99, 41.0, -75.0)]),
        Err(EditError::UnknownNode(99))
    );
    assert_eq!(observable(&layer), before);
    assert_eq!(layer.edit_revision(), revision);
    assert!(!layer.is_modified());

    assert_eq!(layer.commit_node_moves(&[]), Ok(()));
    assert!(!layer.is_modified());
    layer.commit_node_moves(&[(1, 41.0, -75.0)]).unwrap();
    assert!(layer.is_modified());
    assert_eq!(layer.node_lat_lon(1), Some((41.0, -75.0)));
    assert_same_as_fresh(&layer);
}

#[test]
fn tag_edits_on_an_unknown_object_are_refused() {
    let mut layer = layer_with(network());
    assert_eq!(
        layer.set_tag(FeatureKind::Way, 99, "a", "b"),
        Err(EditError::UnknownWay(99))
    );
    assert_eq!(
        layer.remove_tag(FeatureKind::Relation, 99, "a"),
        Err(EditError::UnknownRelation(99))
    );
    assert!(!layer.is_modified());
}

#[test]
fn relation_edits_report_why_they_were_refused() {
    let mut layer = layer_with(
        Build::new()
            .nodes(&[1, 2])
            .way(10, &[1, 2], &[])
            .relation(500, &[("type", "route")], &[wm(10, "")])
            .relation(501, &[("type", "route")], &[rm(500, "")])
            .done(),
    );
    assert_eq!(
        layer.delete_relation(500).unwrap_err(),
        EditError::RelationInUse(500)
    );
    assert_eq!(
        layer.delete_relation(404).unwrap_err(),
        EditError::UnknownRelation(404)
    );
    assert_eq!(
        layer.set_relation_members(404, vec![]),
        Err(EditError::UnknownRelation(404))
    );
    assert!(!layer.is_modified());
}

/// A layer loaded from data whose ids are all local (negative) must not hand
/// out an id that data already uses, whichever way the data arrived.
fn assert_fresh_ids_are_unused(layer: &mut OsmLayer) {
    let ways_before = layer.get_osm_data().unwrap().ways.len();
    let used_node = layer.add_node(40.0, -74.0);
    let used_node_2 = layer.add_node(40.0, -74.1);
    let way = layer.add_way(vec![used_node, used_node_2], vec![]);
    let relation = layer.create_relation(vec![], vec![]).unwrap();
    let data = layer.get_osm_data().unwrap();
    assert_eq!(data.nodes.values().filter(|n| n.id == used_node).count(), 1);
    assert_ne!(used_node, used_node_2);
    assert_eq!(data.ways.len(), ways_before + 1, "the new way replaced one");
    assert!(data.ways.contains_key(&way));
    assert_eq!(
        data.relations.iter().filter(|r| r.id == relation).count(),
        1
    );
}

fn local_ids() -> OsmData {
    Build::new()
        .node_at(-5, 40.0, -74.0)
        .node_at(-6, 40.1, -74.0)
        .way(-7, &[-5, -6], &[])
        .way(-8, &[-6, -5], &[])
        .relation(-9, &[], &[wm(-7, "")])
        .done()
}

#[test]
fn ids_for_new_objects_start_below_the_loaded_ones() {
    let mut layer = layer_with(local_ids());
    assert_fresh_ids_are_unused(&mut layer);
    let data = layer.get_osm_data().unwrap();
    assert!(data.nodes.keys().filter(|&&id| id > -5).count() == 0);
    assert!(data.ways.keys().all(|&id| id <= -7));
}

#[test]
fn ids_are_derived_again_when_data_is_loaded_into_an_existing_layer() {
    // The layer has handed out ids already; loading replaces its data, and the
    // counters follow the new data, not the old.
    let mut layer = layer_with(network());
    layer.add_node(40.0, -74.0);
    layer.add_way(vec![1, 2], vec![]);
    layer.set_osm_data(Arc::new(local_ids()));
    assert_fresh_ids_are_unused(&mut layer);
    let data = layer.get_osm_data().unwrap();
    assert!(data.ways.contains_key(&-7) && data.ways.contains_key(&-8));
}

#[test]
fn ids_are_derived_again_after_a_download_is_merged() {
    let mut layer = layer_with(Build::new().nodes(&[1, 2]).way(10, &[1, 2], &[]).done());
    layer.merge_download(local_ids());
    assert_fresh_ids_are_unused(&mut layer);
}
