//! Direct tests of the layer primitives the upload, conflict-resolution and
//! purge paths drive: `has_pending_edit`, `purge_features` and
//! `restore_purged`, `set_base_version`, `adopt_server_element`,
//! `forget_local_delete` and `apply_chunk_result`.

use crate::layers::osm_layer::OsmLayer;
use crate::layers::{EditableLayer, LayerId};
use crate::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
use crate::osm_api::ServerElement;
use crate::osm_upload::UploadResult;
use crate::selection::FeatureKind::{Node, Relation, Way};
use std::collections::HashMap;
use std::sync::Arc;

const VERSION: i32 = 5;

fn node(id: i64, lat: f64, lon: f64, version: i32) -> OsmNode {
    OsmNode {
        id,
        lat,
        lon,
        version,
        tags: HashMap::new(),
    }
}

fn tags(list: &[(&str, &str)]) -> HashMap<String, String> {
    list.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn way_member(reference: i64) -> OsmMember {
    OsmMember {
        member_type: MemberType::Way,
        reference,
        role: String::new(),
    }
}

fn relation(id: i64, t: &[(&str, &str)], members: Vec<OsmMember>) -> OsmRelation {
    OsmRelation {
        id,
        version: VERSION,
        tags: tags(t),
        members,
    }
}

/// Nodes 1-3 (way 10), a loose node 4, and relations 50 and 52 over way 10
/// and 51 over node 4, all at version 5.
fn server_data() -> OsmData {
    let nodes = [
        node(1, 40.000, -74.000, VERSION),
        node(2, 40.001, -74.000, VERSION),
        node(3, 40.002, -74.000, VERSION),
        node(4, 40.010, -74.010, VERSION),
    ];
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2, 3],
        version: VERSION,
        tags: tags(&[("highway", "residential")]),
    };
    OsmData {
        nodes: nodes.into_iter().map(|n| (n.id, n)).collect(),
        ways: HashMap::from([(10, way)]),
        relations: vec![
            relation(50, &[("type", "route")], vec![way_member(10)]),
            relation(
                51,
                &[("type", "site")],
                vec![OsmMember {
                    member_type: MemberType::Node,
                    reference: 4,
                    role: String::new(),
                }],
            ),
            relation(52, &[("type", "route")], vec![way_member(10)]),
        ],
        bounds: Vec::new(),
    }
}

fn layer() -> OsmLayer {
    OsmLayer::new_with_data(LayerId(1), "L", Arc::new(server_data()))
}

/// `server_data` without the named relations, for tests that delete a node
/// or way the layer would otherwise refuse to delete while they list it.
fn layer_without_relations(ids: &[i64]) -> OsmLayer {
    let mut data = server_data();
    data.relations.retain(|r| !ids.contains(&r.id));
    OsmLayer::new_with_data(LayerId(1), "L", Arc::new(data))
}

fn counts(l: &OsmLayer) -> (usize, usize, usize) {
    l.diff_for_upload().counts()
}

#[test]
fn has_pending_edit_is_false_for_everything_just_loaded_and_for_unknown_ids() {
    let l = layer();
    for (kind, id) in [(Node, 1), (Node, 4), (Way, 10), (Relation, 50)] {
        assert!(!l.has_pending_edit(kind, id), "{kind:?} {id}");
    }
    for kind in [Node, Way, Relation] {
        assert!(!l.has_pending_edit(kind, 12345), "{kind:?} unknown");
    }
}

#[test]
fn has_pending_edit_follows_each_kind_of_edit_and_clears_when_the_edit_is_undone() {
    let mut l = layer();

    l.commit_node_moves(&[(1, 40.5, -74.5)]).unwrap();
    assert!(l.has_pending_edit(Node, 1));
    assert!(!l.has_pending_edit(Node, 2), "a neighbour is not edited");
    l.commit_node_moves(&[(1, 40.000, -74.000)]).unwrap();
    assert!(!l.has_pending_edit(Node, 1), "moved back to the baseline");

    l.set_tag(Way, 10, "name", "Main").unwrap();
    assert!(l.has_pending_edit(Way, 10));
    l.remove_tag(Way, 10, "name").unwrap();
    assert!(!l.has_pending_edit(Way, 10));

    l.set_relation_members(50, vec![way_member(10), way_member(11)])
        .unwrap();
    assert!(l.has_pending_edit(Relation, 50));
    assert!(!l.has_pending_edit(Relation, 52));
    l.set_relation_members(50, vec![way_member(10)]).unwrap();
    assert!(!l.has_pending_edit(Relation, 50));

    let created = l.add_node(40.2, -74.2);
    assert!(l.has_pending_edit(Node, created), "a created node");
    let way = l.add_way(vec![created, 1], vec![]);
    assert!(l.has_pending_edit(Way, way), "a created way");
    let rel = l.create_relation(vec![], vec![way_member(way)]).unwrap();
    assert!(l.has_pending_edit(Relation, rel), "a created relation");
}

#[test]
fn has_pending_edit_is_false_for_an_object_deleted_here() {
    let mut l = layer_without_relations(&[51]);
    l.delete_feature(Node, 4).unwrap();
    assert!(!l.has_pending_edit(Node, 4));
    assert!(l.is_locally_deleted(Node, 4));
}

#[test]
fn purge_removes_objects_from_data_and_baseline_without_marking_them_deleted() {
    let mut l = layer();
    let revision = l.edit_revision();
    let purged = l.purge_features(&[4], &[], &[51]);

    assert_eq!(purged.nodes.len(), 1);
    assert_eq!(purged.nodes[0].id, 4);
    assert_eq!(purged.relations.len(), 1);
    assert_eq!(purged.relations[0].0.id, 51);
    assert!(purged.ways.is_empty());

    assert!(!l.has_feature(Node, 4));
    assert!(!l.has_feature(Relation, 51));
    assert!(
        !l.is_locally_deleted(Node, 4),
        "a purged object is not a deleted one"
    );
    assert!(l.diff_for_upload().is_empty(), "nothing to upload for it");
    assert!(!l.is_modified());
    assert!(l.edit_revision() > revision);
    assert_eq!(l.relation_ids(), vec![50, 52]);
    assert_eq!(l.node_lat_lon(4), None);
    assert!(
        l.derived.node_index.iter().all(|g| g.data != 4),
        "a purged node is not in the spatial index"
    );
}

#[test]
fn purging_a_way_keeps_the_relation_that_names_it() {
    let mut l = layer();
    let purged = l.purge_features(&[1, 2, 3], &[10], &[]);
    assert_eq!(purged.ways.len(), 1);
    assert_eq!(purged.nodes.len(), 3);
    assert!(l.diff_for_upload().is_empty());
    assert_eq!(
        l.relation(50).unwrap().members,
        vec![way_member(10)],
        "the relation keeps a member that is no longer downloaded"
    );
    assert_eq!(l.relations_containing_way(10), vec![50, 52]);
    assert_eq!(l.way_node_ids(10), None);
}

#[test]
fn purging_unknown_ids_changes_nothing_and_reports_nothing() {
    let mut l = layer();
    let revision = l.edit_revision();
    let purged = l.purge_features(&[999], &[998], &[997]);
    assert!(purged.is_empty());
    assert_eq!(l.edit_revision(), revision, "no change, no new revision");
    assert_eq!(l.relation_ids(), vec![50, 51, 52]);
}

#[test]
fn restore_purged_puts_everything_back_in_place_and_in_the_baseline() {
    let mut l = layer();
    let purged = l.purge_features(&[4], &[], &[51]);
    assert_eq!(l.relation_ids(), vec![50, 52]);

    l.restore_purged(purged);
    assert!(l.has_feature(Node, 4));
    assert_eq!(l.node_lat_lon(4), Some((40.010, -74.010)));
    assert_eq!(
        l.relation_ids(),
        vec![50, 51, 52],
        "relations return to their original positions"
    );
    assert!(l.diff_for_upload().is_empty(), "back in the baseline too");
    assert!(!l.has_pending_edit(Node, 4));
    assert!(
        l.derived.node_index.iter().any(|g| g.data == 4),
        "and in the spatial index"
    );
}

#[test]
fn restore_purged_puts_back_several_relations_in_their_original_order() {
    let mut l = layer();
    let purged = l.purge_features(&[], &[], &[50, 52]);
    assert_eq!(l.relation_ids(), vec![51]);
    l.restore_purged(purged);
    assert_eq!(l.relation_ids(), vec![50, 51, 52]);
}

fn layer_with_relations(ids: &[i64]) -> OsmLayer {
    let mut data = server_data();
    data.relations = ids
        .iter()
        .map(|&id| relation(id, &[("type", "route")], vec![way_member(10)]))
        .collect();
    OsmLayer::new_with_data(LayerId(1), "L", Arc::new(data))
}

/// Purging `purge` in one call and restoring leaves the working data and the
/// baseline, relation order included, exactly as they were.
fn assert_purge_round_trips(all: &[i64], purge: &[i64]) {
    let mut l = layer_with_relations(all);
    let data = l.store.osm_data.as_deref().cloned().unwrap();
    let base = l.store.original_data.as_deref().cloned().unwrap();
    let purged = l.purge_features(&[], &[], purge);
    assert_eq!(purged.relations.len(), purge.len());
    let left: Vec<i64> = all.iter().copied().filter(|i| !purge.contains(i)).collect();
    assert_eq!(l.relation_ids(), left, "purge {purge:?}");
    l.restore_purged(purged);
    assert_eq!(l.relation_ids(), all, "working order after {purge:?}");
    let base_ids: Vec<i64> = l
        .store
        .original_data
        .as_ref()
        .unwrap()
        .relations
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(base_ids, all, "baseline order after {purge:?}");
    assert_eq!(
        l.store.osm_data.as_deref(),
        Some(&data),
        "data after {purge:?}"
    );
    assert_eq!(
        l.store.original_data.as_deref(),
        Some(&base),
        "baseline after {purge:?}"
    );
    super::fresh_rebuild::assert_fresh(&l, &format!("restoring {purge:?}"));
    assert!(l.diff_for_upload().is_empty());
}

#[test]
fn restore_purged_round_trips_relations_purged_together_in_any_shape() {
    let three = [50, 51, 52];
    assert_purge_round_trips(&three, &[52, 50]);
    assert_purge_round_trips(&three, &[50, 51]);
    assert_purge_round_trips(&three, &[51, 52]);
    assert_purge_round_trips(&three, &[52, 51, 50]);
    assert_purge_round_trips(&three, &[51]);
    assert_purge_round_trips(&three, &[50, 51, 52]);
    let five = [50, 51, 52, 53, 54];
    assert_purge_round_trips(&five, &[51, 53]);
    assert_purge_round_trips(&five, &[54, 50, 52]);
}

#[test]
fn restore_purged_after_a_way_was_purged_rebuilds_its_geometry() {
    let mut l = layer();
    let purged = l.purge_features(&[1, 2, 3], &[10], &[]);
    l.restore_purged(purged);
    assert_eq!(l.way_node_ids(10), Some(vec![1, 2, 3]));
    assert_eq!(l.ways_using_node(2), vec![10]);
    assert!(l.diff_for_upload().is_empty());
}

#[test]
fn set_base_version_moves_the_base_of_a_pending_edit_and_keeps_the_edit() {
    let mut l = layer();
    l.commit_node_moves(&[(1, 40.5, -74.5)]).unwrap();
    assert!(l.set_base_version(Node, 1, 9));
    let diff = l.diff_for_upload();
    assert_eq!(diff.modified_nodes.len(), 1);
    assert_eq!(diff.modified_nodes[0].version, 9);
    assert_eq!(diff.modified_nodes[0].lat, 40.5, "the edit stays");
    assert_eq!(l.feature_version(Node, 1), Some(9));
    assert!(l.is_modified());
}

#[test]
fn set_base_version_moves_the_base_of_a_local_delete() {
    let mut l = layer_without_relations(&[51]);
    l.delete_feature(Node, 4).unwrap();
    assert!(l.set_base_version(Node, 4, 8));
    assert_eq!(l.diff_for_upload().deleted_node_ids, vec![(4, 8)]);
}

#[test]
fn set_base_version_covers_ways_and_relations() {
    let mut l = layer();
    l.set_tag(Way, 10, "name", "Main").unwrap();
    l.set_relation_members(50, vec![]).unwrap();
    assert!(l.set_base_version(Way, 10, 7));
    assert!(l.set_base_version(Relation, 50, 6));
    let diff = l.diff_for_upload();
    assert_eq!(diff.modified_ways[0].version, 7);
    assert_eq!(diff.modified_relations[0].version, 6);
    assert_eq!(diff.modified_relations[0].id, 50);
}

#[test]
fn set_base_version_of_an_unknown_object_is_false_and_changes_nothing() {
    let mut l = layer();
    l.set_tag(Way, 10, "name", "Main").unwrap();
    let before = counts(&l);
    let revision = l.edit_revision();
    for kind in [Node, Way, Relation] {
        assert!(!l.set_base_version(kind, 777, 3), "{kind:?}");
    }
    assert_eq!(counts(&l), before);
    assert_eq!(l.diff_for_upload().modified_ways[0].version, VERSION);
    assert_eq!(l.edit_revision(), revision, "no change, no new revision");
}

fn server_with_node(id: i64, lat: f64, version: i32) -> ServerElement {
    let mut data = OsmData {
        nodes: HashMap::new(),
        ways: HashMap::new(),
        relations: Vec::new(),
        bounds: Vec::new(),
    };
    data.nodes.insert(id, node(id, lat, -74.0, version));
    ServerElement::Present(data)
}

#[test]
fn adopting_the_servers_node_drops_the_local_change() {
    let mut l = layer();
    l.commit_node_moves(&[(1, 40.5, -74.5)]).unwrap();
    assert!(l.is_modified());

    assert!(l.adopt_server_element(Node, 1, server_with_node(1, 40.123, 6)));
    assert_eq!(l.node_lat_lon(1), Some((40.123, -74.0)));
    assert_eq!(l.feature_version(Node, 1), Some(6));
    assert!(l.diff_for_upload().is_empty(), "no edit of it is left");
    assert!(!l.is_modified(), "nothing left to upload");
    assert!(!l.has_pending_edit(Node, 1));
}

#[test]
fn adopting_a_server_deletion_removes_the_object_from_data_and_baseline() {
    let mut l = layer_without_relations(&[51]);
    l.set_tag(Way, 10, "name", "Main").unwrap();
    assert!(l.adopt_server_element(Way, 10, ServerElement::Gone));
    assert!(!l.has_feature(Way, 10));
    assert!(
        !l.is_locally_deleted(Way, 10),
        "not a pending delete either"
    );
    let diff = l.diff_for_upload();
    assert!(diff.modified_ways.is_empty() && diff.deleted_way_ids.is_empty());
    assert_eq!(l.way_node_ids(10), None);

    l.delete_feature(Node, 4).unwrap();
    assert!(l.adopt_server_element(Node, 4, ServerElement::Gone));
    assert!(!l.is_locally_deleted(Node, 4));
    assert!(l.diff_for_upload().deleted_node_ids.is_empty());
}

#[test]
fn adopting_a_server_way_replaces_it_and_its_nodes_but_keeps_other_node_edits() {
    let mut l = layer();
    l.set_way_nodes(10, vec![1, 2]).unwrap();
    l.commit_node_moves(&[(3, 40.9, -74.9)]).unwrap();

    let mut theirs = OsmData {
        nodes: HashMap::new(),
        ways: HashMap::new(),
        relations: Vec::new(),
        bounds: Vec::new(),
    };
    theirs.nodes.insert(1, node(1, 40.000, -74.000, 6));
    theirs.nodes.insert(2, node(2, 40.0011, -74.000, 6));
    theirs.nodes.insert(3, node(3, 40.0022, -74.000, 6));
    theirs.ways.insert(
        10,
        OsmWay {
            id: 10,
            nodes: vec![1, 2, 3],
            version: 6,
            tags: tags(&[("highway", "tertiary")]),
        },
    );
    assert!(l.adopt_server_element(Way, 10, ServerElement::Present(theirs)));

    assert_eq!(l.way_node_ids(10), Some(vec![1, 2, 3]));
    assert_eq!(l.feature_version(Way, 10), Some(6));
    assert_eq!(l.node_lat_lon(2), Some((40.0011, -74.0)), "theirs");
    assert_eq!(
        l.node_lat_lon(3),
        Some((40.9, -74.9)),
        "the user's own move of node 3 stays"
    );
    let diff = l.diff_for_upload();
    assert!(diff.modified_ways.is_empty(), "the way is theirs now");
    assert_eq!(diff.modified_nodes.len(), 1);
    assert_eq!(diff.modified_nodes[0].id, 3);
}

#[test]
fn adopting_a_server_relation_replaces_it_in_data_and_baseline() {
    let mut l = layer();
    l.set_relation_members(50, vec![]).unwrap();
    let mut theirs = server_data();
    theirs.relations[0].version = 9;
    theirs.relations[0].members = vec![way_member(10), way_member(11)];
    assert!(l.adopt_server_element(Relation, 50, ServerElement::Present(theirs)));
    let r = l.relation(50).unwrap();
    assert_eq!(r.version, 9);
    assert_eq!(r.members.len(), 2);
    assert!(l.diff_for_upload().is_empty());
}

#[test]
fn adopting_an_element_the_server_data_does_not_hold_is_false_and_changes_nothing() {
    let mut l = layer();
    l.commit_node_moves(&[(1, 40.5, -74.5)]).unwrap();
    let before = counts(&l);
    assert!(!l.adopt_server_element(Node, 1, server_with_node(2, 40.0, 6)));
    assert!(!l.adopt_server_element(Way, 10, server_with_node(1, 40.0, 6)));
    assert!(!l.adopt_server_element(Relation, 50, server_with_node(1, 40.0, 6)));
    assert!(!l.adopt_server_element(Node, 999, ServerElement::Gone));
    assert_eq!(counts(&l), before);
    assert_eq!(l.node_lat_lon(1), Some((40.5, -74.5)));
}

#[test]
fn forgetting_a_local_delete_drops_it_from_the_diff() {
    let mut l = layer_without_relations(&[51]);
    l.delete_feature(Node, 4).unwrap();
    assert_eq!(l.diff_for_upload().deleted_node_ids.len(), 1);
    assert!(l.is_locally_deleted(Node, 4));

    assert!(l.forget_local_delete(Node, 4));
    assert!(l.diff_for_upload().is_empty());
    assert!(!l.is_locally_deleted(Node, 4));
    assert!(!l.is_modified(), "nothing left to upload");
    assert!(!l.has_feature(Node, 4), "the object stays gone");
}

#[test]
fn forgetting_a_local_delete_covers_ways_and_relations() {
    let mut l = layer_without_relations(&[50, 52]);
    l.delete_relation(51).unwrap();
    l.delete_feature(Way, 10).unwrap();
    assert!(l.forget_local_delete(Way, 10));
    assert!(l.forget_local_delete(Relation, 51));
    let diff = l.diff_for_upload();
    assert!(diff.deleted_way_ids.is_empty() && diff.deleted_relation_ids.is_empty());
}

#[test]
fn forgetting_a_delete_of_something_not_deleted_here_does_nothing() {
    let mut l = layer();
    l.commit_node_moves(&[(1, 40.5, -74.5)]).unwrap();
    let before = counts(&l);
    assert!(!l.forget_local_delete(Node, 1), "present, only edited");
    assert!(!l.forget_local_delete(Node, 999), "unknown");
    assert!(!l.forget_local_delete(Relation, 50), "present");
    assert_eq!(counts(&l), before);
    assert!(l.has_feature(Node, 1));
}

/// What the server would answer for a chunk holding all of `chunk`: every
/// created object gets a real id (placeholder id `-n` becomes `1000 * k`),
/// every modified one the next version.
fn answer_for(chunk: &crate::layers::diff::LayerDiff) -> UploadResult {
    let mut result = UploadResult::default();
    for (k, n) in chunk.created_nodes.iter().enumerate() {
        result.node_id_remap.insert(n.id, (1000 + k as i64, 1));
    }
    for n in &chunk.modified_nodes {
        result.node_id_remap.insert(n.id, (n.id, n.version + 1));
    }
    for (k, w) in chunk.created_ways.iter().enumerate() {
        result.way_id_remap.insert(w.id, (2000 + k as i64, 1));
    }
    for w in &chunk.modified_ways {
        result.way_id_remap.insert(w.id, (w.id, w.version + 1));
    }
    for (k, r) in chunk.created_relations.iter().enumerate() {
        result.relation_id_remap.insert(r.id, (3000 + k as i64, 1));
    }
    for r in &chunk.modified_relations {
        result.relation_id_remap.insert(r.id, (r.id, r.version + 1));
    }
    result
}

/// Every kind of edit at once: a moved node, a retagged way, a deleted loose
/// node and relation, and a created node, way and relation.
fn edited_layer() -> OsmLayer {
    let mut l = layer();
    l.commit_node_moves(&[(1, 40.5, -74.5)]).unwrap();
    l.set_tag(Way, 10, "name", "Main").unwrap();
    l.delete_relation(51).unwrap();
    l.delete_feature(Node, 4).unwrap();
    let n = l.add_node(40.2, -74.2);
    let w = l.add_way(
        vec![n, 3],
        vec![("highway".to_string(), "path".to_string())],
    );
    l.create_relation(
        vec![("type".to_string(), "route".to_string())],
        vec![way_member(w)],
    )
    .unwrap();
    l
}

#[test]
fn a_chunk_holding_every_pending_edit_leaves_nothing_pending() {
    let mut l = edited_layer();
    let chunk = l.diff_for_upload();
    assert_eq!(chunk.counts(), (3, 2, 2));
    let result = answer_for(&chunk);

    l.apply_chunk_result(&result, &chunk);

    assert!(l.diff_for_upload().is_empty(), "{:?}", l.diff_for_upload());
    assert!(!l.is_modified());
    assert_eq!(l.feature_version(Node, 1), Some(VERSION + 1));
    assert_eq!(l.feature_version(Way, 10), Some(VERSION + 1));
    assert!(
        l.has_feature(Node, 1000),
        "the created node has its real id"
    );
    assert!(l.has_feature(Way, 2000));
    assert!(l.has_feature(Relation, 3000));
    assert_eq!(
        l.way_node_ids(2000),
        Some(vec![1000, 3]),
        "the created way's node list names the real id"
    );
    assert_eq!(l.relation(3000).unwrap().members, vec![way_member(2000)]);
    assert!(!l.is_locally_deleted(Node, 4), "the delete is confirmed");
    assert!(!l.has_feature(Relation, 51));
}

#[test]
fn a_partial_chunk_confirms_only_its_own_edits() {
    let mut l = edited_layer();
    let full = l.diff_for_upload();
    // Send the node edits first: the move, the creation and the delete.
    let chunk = crate::layers::diff::LayerDiff {
        created_nodes: full.created_nodes.clone(),
        modified_nodes: full.modified_nodes.clone(),
        deleted_node_ids: full.deleted_node_ids.clone(),
        ..Default::default()
    };
    let result = answer_for(&chunk);

    l.apply_chunk_result(&result, &chunk);

    let rest = l.diff_for_upload();
    assert!(rest.created_nodes.is_empty());
    assert!(rest.modified_nodes.is_empty());
    assert!(rest.deleted_node_ids.is_empty());
    assert_eq!(rest.modified_ways.len(), 1, "the retag is still pending");
    assert_eq!(rest.created_ways.len(), 1);
    assert_eq!(rest.created_relations.len(), 1);
    assert_eq!(rest.deleted_relation_ids.len(), 1);
    assert_eq!(
        rest.created_ways[0].nodes,
        vec![1000, 3],
        "the pending way already names the created node's real id"
    );
    assert!(l.is_modified(), "chunks are still to go");
    assert_eq!(l.feature_version(Node, 1), Some(VERSION + 1));
    assert!(!l.is_locally_deleted(Node, 4));
    assert_eq!(l.node_lat_lon(1), Some((40.5, -74.5)), "the move is kept");
}

#[test]
fn successive_chunks_add_up_to_the_whole_upload() {
    let mut l = edited_layer();
    let full = l.diff_for_upload();
    let nodes = crate::layers::diff::LayerDiff {
        created_nodes: full.created_nodes.clone(),
        modified_nodes: full.modified_nodes.clone(),
        deleted_node_ids: full.deleted_node_ids.clone(),
        ..Default::default()
    };
    l.apply_chunk_result(&answer_for(&nodes), &nodes);

    let rest = l.diff_for_upload();
    let ways = crate::layers::diff::LayerDiff {
        created_ways: rest.created_ways.clone(),
        modified_ways: rest.modified_ways.clone(),
        ..Default::default()
    };
    l.apply_chunk_result(&answer_for(&ways), &ways);
    assert!(l.is_modified());

    let rest = l.diff_for_upload();
    assert_eq!(rest.counts(), (1, 0, 1), "only the relations remain");
    l.apply_chunk_result(&answer_for(&rest), &rest);

    assert!(l.diff_for_upload().is_empty());
    assert!(!l.is_modified());
    assert!(l.has_feature(Relation, 3000));
    assert_eq!(l.relation(3000).unwrap().members, vec![way_member(2000)]);
}
