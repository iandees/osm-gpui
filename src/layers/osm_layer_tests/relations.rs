//! Relation membership, tags, creation and deletion through the layer.

use super::{data_with, empty_tags};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::LayerId;
use crate::layers::{EditableLayer, MapLayer};
use crate::osm::{OsmNode, OsmWay};
use crate::selection::{FeatureKind, FeatureRef};
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn set_relation_members_returns_the_previous_list_and_marks_the_layer_modified() {
    use crate::osm::{MemberType, OsmMember, OsmRelation};
    let member = |reference| OsmMember {
        member_type: MemberType::Way,
        reference,
        role: "r".to_string(),
    };
    let mut base = (*data_with(vec![], vec![])).clone();
    base.relations.push(OsmRelation {
        id: 500,
        version: 3,
        tags: empty_tags(),
        members: vec![member(10)],
    });
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(base));
    assert!(!layer.is_modified());

    assert_eq!(layer.set_relation_members(404, vec![]), None);
    assert!(!layer.is_modified(), "unknown relation changes nothing");

    let previous = layer.set_relation_members(500, vec![member(10), member(11)]);
    assert_eq!(previous, Some(vec![member(10)]));
    assert!(layer.is_modified());
    assert_eq!(layer.relation(500).unwrap().members.len(), 2);
    assert_eq!(layer.relations_containing_way(11), vec![500]);
    let diff = layer.diff_for_upload();
    assert_eq!(diff.modified_relations.len(), 1);
    assert_eq!(diff.modified_relations[0].version, 3);

    // Putting the old list back leaves nothing to upload.
    layer.set_relation_members(500, vec![member(10)]);
    assert!(layer.diff_for_upload().is_empty());
}

#[test]
fn relation_member_highlight_differs_from_selection_and_hover() {
    use crate::layers::HighlightKind::{Hovered, Member, Selected};
    let colors = |kind| super::super::highlight_colors(kind);
    assert_ne!(colors(Member), colors(Selected));
    assert_ne!(colors(Member), colors(Hovered));
}

#[test]
fn relation_tags_read_edit_and_remove_through_the_layer() {
    use crate::osm::OsmRelation;
    let mut base = (*data_with(vec![], vec![])).clone();
    base.relations.push(OsmRelation {
        id: 500,
        version: 3,
        tags: HashMap::from([("type".to_string(), "route".to_string())]),
        members: vec![],
    });
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(base));
    let rel = FeatureRef {
        layer_id: LayerId(1),
        kind: FeatureKind::Relation,
        id: 500,
    };
    let missing = FeatureRef { id: 404, ..rel };
    assert_eq!(
        EditableLayer::feature_tags(&layer, &rel),
        Some(vec![("type".to_string(), "route".to_string())])
    );
    assert_eq!(EditableLayer::feature_tags(&layer, &missing), None);
    assert_eq!(
        EditableLayer::feature_geometry(&layer, &rel, crate::presets::area_keys()),
        Some(crate::presets::Geometry::Relation)
    );
    assert!(!layer.is_modified());

    layer.set_tag(FeatureKind::Relation, 500, "name", "Blue Line");
    assert!(layer.is_modified());
    assert_eq!(EditableLayer::feature_tags(&layer, &rel).unwrap().len(), 2);
    let diff = layer.diff_for_upload();
    assert_eq!(diff.modified_relations.len(), 1);
    assert_eq!(diff.modified_relations[0].tags["name"], "Blue Line");

    layer.remove_tag(FeatureKind::Relation, 500, "name");
    assert!(layer.diff_for_upload().is_empty(), "back to the original");

    // A relation can't be deleted yet, and a missing one is ignored.
    assert!(layer.delete_feature(FeatureKind::Relation, 500).is_none());
    assert!(layer.relation(500).is_some());
    layer.set_tag(FeatureKind::Relation, 404, "a", "b");
}

#[test]
fn modified_relation_uploads_with_placeholders_and_is_remapped_afterwards() {
    use crate::osm::{MemberType, OsmMember, OsmRelation};
    let member = |member_type, reference, role: &str| OsmMember {
        member_type,
        reference,
        role: role.to_string(),
    };
    let mut base = (*data_with(
        vec![OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        }],
        vec![OsmWay {
            id: 10,
            nodes: vec![1],
            version: 2,
            tags: empty_tags(),
        }],
    ))
    .clone();
    base.relations.push(OsmRelation {
        id: 500,
        version: 7,
        tags: empty_tags(),
        members: vec![member(MemberType::Way, 10, "route")],
    });
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(base.clone()));

    // A new way -2 (with a new node -1) joins relation 500 as a member.
    let mut edited = base.clone();
    edited.nodes.insert(
        -1,
        OsmNode {
            id: -1,
            lat: 40.001,
            lon: -74.001,
            version: 0,
            tags: empty_tags(),
        },
    );
    edited.ways.insert(
        -2,
        OsmWay {
            id: -2,
            nodes: vec![1, -1],
            version: 0,
            tags: empty_tags(),
        },
    );
    edited.relations[0]
        .members
        .push(member(MemberType::Way, -2, "route"));
    edited.relations[0]
        .members
        .push(member(MemberType::Node, -1, "stop"));
    layer.set_osm_data_for_test(Arc::new(edited));

    let diff = layer.diff_for_upload();
    assert_eq!(diff.counts(), (2, 1, 0));
    let xml = crate::osm_upload::build_osm_change_xml(3, &[("L", diff)]);
    let way_create = xml.find(r#"<way id="-2""#).unwrap();
    let relation_modify = xml.find(r#"<relation id="500" version="7""#).unwrap();
    assert!(way_create < relation_modify, "{xml}");
    assert!(
        xml.contains(r#"<member type="way" ref="-2" role="route"/>"#),
        "{xml}"
    );

    let mut result = crate::osm_upload::UploadResult::default();
    result.node_id_remap.insert(-1, (999, 1));
    result.way_id_remap.insert(-2, (888, 1));
    result.relation_id_remap.insert(500, (500, 8));
    layer.apply_upload_result(&result);

    let updated = layer.get_osm_data().unwrap();
    let relation = &updated.relations[0];
    assert_eq!(relation.version, 8);
    assert_eq!(
        relation
            .members
            .iter()
            .map(|m| (m.member_type, m.reference))
            .collect::<Vec<_>>(),
        vec![
            (MemberType::Way, 10),
            (MemberType::Way, 888),
            (MemberType::Node, 999)
        ]
    );
    assert!(!layer.is_modified());
    assert!(layer.diff_for_upload().is_empty());
}

fn relation_member(member_type: crate::osm::MemberType, reference: i64) -> crate::osm::OsmMember {
    crate::osm::OsmMember {
        member_type,
        reference,
        role: String::new(),
    }
}

fn layer_with_server_relation() -> OsmLayer {
    use crate::osm::{MemberType, OsmRelation};
    let mut base = (*data_with(vec![], vec![])).clone();
    base.relations.push(OsmRelation {
        id: 500,
        version: 7,
        tags: empty_tags(),
        members: vec![relation_member(MemberType::Way, 10)],
    });
    OsmLayer::new_with_data(LayerId(1), "L", Arc::new(base))
}

#[test]
fn create_relation_allocates_unique_negative_ids_shared_with_nodes_and_ways() {
    use crate::osm::MemberType::Way;
    let mut layer = layer_with_server_relation();
    let n = layer.add_node(40.0, -74.0);
    let w = layer.add_way(vec![n], vec![]);
    let r1 = layer.create_relation(vec![], vec![]).unwrap();
    let r2 = layer
        .create_relation(
            vec![("type".to_string(), "multipolygon".to_string())],
            vec![relation_member(Way, w)],
        )
        .unwrap();
    assert!(r1 < 0 && r2 < r1);
    assert!(r1 < n.min(w), "below every node and way id handed out");
    // Later nodes and ways stay clear of the relation ids.
    let n2 = layer.add_node(40.1, -74.1);
    assert!(n2 < r2);
    assert_eq!(layer.relation(r2).unwrap().version, 0);
    assert_eq!(layer.relations().len(), 3);
    assert!(layer.is_modified());
    let diff = layer.diff_for_upload();
    assert_eq!(diff.created_relations.len(), 2);
}

#[test]
fn delete_relation_refuses_a_member_relation_and_restores_in_place() {
    use crate::osm::MemberType::Relation;
    let mut layer = layer_with_server_relation();
    let child = layer.create_relation(vec![], vec![]).unwrap();
    let parent = layer
        .create_relation(vec![], vec![relation_member(Relation, child)])
        .unwrap();
    assert_eq!(layer.relations_containing_relation(child), vec![parent]);
    assert!(layer.delete_relation(child).is_none());
    assert!(layer.relation(child).is_some());
    assert!(layer.delete_relation(404).is_none());

    // A server relation: delete, restore at the same position, version
    // intact, and nothing left to upload.
    let before = layer.relations();
    let snapshot = layer.delete_relation(500).unwrap();
    assert_eq!(snapshot.relation.version, 7);
    assert_eq!(snapshot.index, 0);
    assert_eq!(layer.diff_for_upload().deleted_relation_ids, vec![(500, 7)]);
    assert!(layer.restore_relation(snapshot.clone()));
    assert!(!layer.restore_relation(snapshot), "id already present");
    assert_eq!(layer.relations(), before);
    assert!(layer.diff_for_upload().deleted_relation_ids.is_empty());

    // Once the container is gone the member can go too, and redo of the
    // create (restore) brings back the same ids.
    let parent_snapshot = layer.delete_relation(parent).unwrap();
    let child_snapshot = layer.delete_relation(child).unwrap();
    assert!(layer.diff_for_upload().created_relations.is_empty());
    assert!(layer.restore_relation(child_snapshot));
    assert!(layer.restore_relation(parent_snapshot));
    assert_eq!(layer.relations(), before);
}

#[test]
fn created_relations_are_remapped_inside_other_relations_after_upload() {
    use crate::osm::MemberType::{Node, Relation, Way};
    let mut layer = layer_with_server_relation();
    let n = layer.add_node(40.0, -74.0);
    let w = layer.add_way(vec![n], vec![]);
    let child = layer
        .create_relation(vec![], vec![relation_member(Node, n)])
        .unwrap();
    let parent = layer
        .create_relation(
            vec![("type".to_string(), "route".to_string())],
            vec![relation_member(Relation, child), relation_member(Way, w)],
        )
        .unwrap();
    // The server relation also gains the new parent as a member.
    let mut members = layer.relation(500).unwrap().members;
    members.push(relation_member(Relation, parent));
    layer.set_relation_members(500, members);

    let diff = layer.diff_for_upload();
    assert_eq!(diff.counts(), (4, 1, 0));
    let xml = crate::osm_upload::build_osm_change_xml(5, &[("L", diff)]);
    assert!(xml.find("<relation id=\"-2\"") < xml.find("<relation id=\"-3\""));
    assert!(xml.find("<relation id=\"-3\"") < xml.find("<modify>"));

    let mut result = crate::osm_upload::UploadResult::default();
    result.node_id_remap.insert(n, (900, 1));
    result.way_id_remap.insert(w, (800, 1));
    result.relation_id_remap.insert(child, (701, 1));
    result.relation_id_remap.insert(parent, (702, 1));
    result.relation_id_remap.insert(500, (500, 8));
    layer.apply_upload_result(&result);

    let summary: Vec<_> = layer
        .relations()
        .iter()
        .map(|r| {
            (
                r.id,
                r.version,
                r.members
                    .iter()
                    .map(|m| (m.member_type, m.reference))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (500, 8, vec![(Way, 10), (Relation, 702)]),
            (701, 1, vec![(Node, 900)]),
            (702, 1, vec![(Relation, 701), (Way, 800)]),
        ]
    );
    assert!(!layer.is_modified());
    assert!(layer.diff_for_upload().is_empty());
}

#[test]
fn deleted_relation_drops_out_of_the_baseline_after_upload() {
    let mut layer = layer_with_server_relation();
    layer.delete_relation(500).unwrap();
    assert_eq!(layer.diff_for_upload().counts(), (0, 0, 1));
    // The server confirms a delete with no new_id, so no remap entry.
    layer.apply_upload_result(&crate::osm_upload::UploadResult::default());
    assert!(layer.diff_for_upload().is_empty());
    assert!(layer.relation(500).is_none());
}

#[test]
fn merge_keeps_a_locally_created_relation_and_a_locally_deleted_one() {
    use crate::osm::{MemberType::Way, OsmData, OsmRelation};
    let mut layer = layer_with_server_relation();
    let created = layer
        .create_relation(vec![], vec![relation_member(Way, 10)])
        .unwrap();
    layer.delete_relation(500).unwrap();

    // The server still has 500 (at a newer version) plus an unrelated 501.
    let server = |id, version| OsmRelation {
        id,
        version,
        tags: empty_tags(),
        members: vec![relation_member(Way, 10)],
    };
    let incoming = OsmData {
        nodes: HashMap::new(),
        ways: HashMap::new(),
        relations: vec![server(500, 9), server(501, 1)],
        bounds: Vec::new(),
    };
    let stats = layer.merge_download(incoming);
    assert_eq!(
        stats.kept_local, 1,
        "the deleted relation 500 is kept deleted"
    );
    assert!(layer.relation(500).is_none());
    assert!(layer.relation(501).is_some());
    assert!(layer.relation(created).is_some());
    let diff = layer.diff_for_upload();
    assert_eq!(diff.created_relations.len(), 1);
    // Still deleted, against the version the layer last knew.
    assert_eq!(diff.deleted_relation_ids, vec![(500, 7)]);
}

#[test]
fn apply_upload_result_confirms_delete_with_no_remap_entry() {
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
        lon: -74.001,
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![n1, n2], vec![]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);

    // Simulate a delete by directly removing node 2 from osm_data (the
    // real delete_feature API doesn't exist yet).
    let current = layer.get_osm_data().unwrap();
    let mut new_data = (*current).clone();
    new_data.nodes.remove(&2);
    layer.set_osm_data_for_test(Arc::new(new_data));

    let diff = layer.diff_for_upload();
    assert_eq!(diff.deleted_node_ids, vec![(2, 1)]);

    // Deletes get no remap entry in the diffResult; applying an empty
    // result should still clear `modified` and reset the baseline so
    // the deletion isn't reported again.
    let result = crate::osm_upload::UploadResult::default();
    layer.apply_upload_result(&result);
    assert!(!layer.is_modified());
    assert!(layer.diff_for_upload().is_empty());
}
