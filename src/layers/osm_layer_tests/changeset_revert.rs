//! Applying a planned changeset revert to a layer: what the upload diff then
//! reports, and that one undo takes it all back.

use super::{data_with, empty_tags};
use crate::changeset_revert::{apply, Conflict, Object, Plan, Prepared, SkipReason, Step};
use crate::edit_ops::replay;
use crate::ids::{NodeId, WayId};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::{EditableLayer, LayerId};
use crate::osm::{OsmData, OsmNode, OsmWay};
use crate::selection::FeatureKind;
use crate::tags::Tags;

fn node(id: i64, lat: f64, version: i32, tags: &[(&str, &str)]) -> OsmNode {
    OsmNode {
        id: NodeId(id),
        lat,
        lon: -74.0,
        version,
        tags: Tags::from(tags),
    }
}

fn way(id: i64, nodes: &[i64], version: i32) -> OsmWay {
    OsmWay {
        id: WayId(id),
        nodes: NodeId::from_raw_vec(nodes),
        version,
        tags: empty_tags(),
    }
}

fn prepared(steps: Vec<Step>, server: OsmData) -> Prepared {
    Prepared {
        changeset: 7,
        plan: Plan {
            steps,
            ..Plan::default()
        },
        server,
    }
}

fn server(nodes: Vec<OsmNode>, ways: Vec<OsmWay>) -> OsmData {
    (*data_with(nodes, ways)).clone()
}

#[test]
fn a_modified_node_goes_back_as_a_modify_of_the_current_version() {
    // The layer has the node as it was before the changeset; the server has
    // the changeset's version 3.
    let mut layer = OsmLayer::new_with_data(
        LayerId(1),
        "L",
        data_with(vec![node(1, 40.0, 2, &[("a", "old")])], vec![]),
    );
    let now = node(1, 41.0, 3, &[("a", "new"), ("b", "added")]);
    let before = node(1, 40.0, 3, &[("a", "old")]);
    let p = prepared(
        vec![Step::Restore {
            before: Object::Node(before.clone()),
            current: Object::Node(now.clone()),
        }],
        server(vec![now], vec![]),
    );

    let (ops, report) = apply(LayerId(1), &mut layer, &p);

    assert_eq!(report.reverted, 1);
    assert!(report.skipped.is_empty());
    let diff = layer.diff_for_upload();
    assert_eq!(diff.modified_nodes, vec![before]);
    assert!(diff.created_nodes.is_empty());
    assert!(!ops.is_empty());
}

#[test]
fn a_created_way_and_its_nodes_are_deleted() {
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data_with(vec![], vec![]));
    let nodes = vec![node(1, 40.0, 1, &[]), node(2, 40.1, 1, &[])];
    let w = way(5, &[1, 2], 1);
    let p = prepared(
        vec![
            Step::Delete {
                current: Object::Node(nodes[0].clone()),
            },
            Step::Delete {
                current: Object::Node(nodes[1].clone()),
            },
            Step::Delete {
                current: Object::Way(w.clone()),
            },
        ],
        server(nodes, vec![w]),
    );

    let (_, report) = apply(LayerId(1), &mut layer, &p);

    assert_eq!(report.reverted, 3, "{report:?}");
    let diff = layer.diff_for_upload();
    assert_eq!(diff.deleted_way_ids, vec![(WayId(5), 1)]);
    assert_eq!(diff.deleted_node_ids.len(), 2);
}

#[test]
fn a_node_still_used_by_another_way_is_skipped_not_deleted() {
    let mut layer = OsmLayer::new_with_data(
        LayerId(1),
        "L",
        data_with(vec![node(1, 40.0, 1, &[])], vec![way(9, &[1, 1], 4)]),
    );
    let created = node(1, 40.0, 1, &[]);
    let p = prepared(
        vec![Step::Delete {
            current: Object::Node(created.clone()),
        }],
        server(vec![created], vec![]),
    );

    let (_, report) = apply(LayerId(1), &mut layer, &p);

    assert_eq!(report.reverted, 0);
    assert_eq!(report.skipped.len(), 1);
    assert!(matches!(report.skipped[0].2, SkipReason::Refused(_)));
    assert!(layer.has_feature(FeatureKind::Node, 1));
}

#[test]
fn an_undeleted_object_is_a_modify_of_the_existing_one_not_a_create() {
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data_with(vec![], vec![]));
    // Deleted at version 4, so the upload names version 4.
    let back = node(9, 40.0, 4, &[("amenity", "cafe")]);
    let p = prepared(
        vec![Step::Undelete {
            before: Object::Node(back.clone()),
        }],
        OsmData::default(),
    );

    let (ops, report) = apply(LayerId(1), &mut layer, &p);

    assert_eq!(report.reverted, 1);
    assert!(layer.has_feature(FeatureKind::Node, 9));
    let diff = layer.diff_for_upload();
    assert!(diff.created_nodes.is_empty(), "undelete must not create");
    assert_eq!(diff.modified_nodes, vec![back]);
    assert!(diff.deleted_node_ids.is_empty());

    // The one undo step takes it out of the data and the baseline both.
    let mut failures = Vec::new();
    replay(&ops, false, &mut layer, &mut failures);
    assert!(failures.is_empty(), "{failures:?}");
    assert!(!layer.has_feature(FeatureKind::Node, 9));
    assert!(layer.diff_for_upload().is_empty());

    // And redo brings it back the same way.
    replay(&ops, true, &mut layer, &mut failures);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(layer.diff_for_upload().modified_nodes.len(), 1);
    assert!(layer.diff_for_upload().created_nodes.is_empty());
}

#[test]
fn an_undeleted_way_comes_back_with_its_undeleted_nodes() {
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data_with(vec![], vec![]));
    let p = prepared(
        vec![
            Step::Undelete {
                before: Object::Node(node(1, 40.0, 2, &[])),
            },
            Step::Undelete {
                before: Object::Node(node(2, 40.1, 2, &[])),
            },
            Step::Undelete {
                before: Object::Way(way(5, &[1, 2], 3)),
            },
        ],
        OsmData::default(),
    );

    let (_, report) = apply(LayerId(1), &mut layer, &p);

    assert_eq!(report.reverted, 3);
    let diff = layer.diff_for_upload();
    assert!(diff.created_nodes.is_empty() && diff.created_ways.is_empty());
    assert_eq!(diff.modified_nodes.len(), 2);
    assert_eq!(diff.modified_ways.len(), 1);
    assert_eq!(diff.modified_ways[0].version, 3);
}

#[test]
fn an_object_with_unuploaded_edits_is_left_alone() {
    let mut layer = OsmLayer::new_with_data(
        LayerId(1),
        "L",
        data_with(vec![node(1, 40.0, 3, &[])], vec![]),
    );
    layer.set_tag(FeatureKind::Node, 1, "mine", "yes").unwrap();
    let now = node(1, 41.0, 3, &[]);
    let p = prepared(
        vec![Step::Restore {
            before: Object::Node(node(1, 40.0, 3, &[])),
            current: Object::Node(now.clone()),
        }],
        server(vec![now], vec![]),
    );

    let (_, report) = apply(LayerId(1), &mut layer, &p);

    assert_eq!(report.reverted, 0);
    assert_eq!(
        report.skipped,
        vec![(FeatureKind::Node, 1, SkipReason::EditedLocally)]
    );
    let diff = layer.diff_for_upload();
    assert_eq!(diff.modified_nodes.len(), 1);
    assert_eq!(diff.modified_nodes[0].tags.get("mine"), Some("yes"));
}

#[test]
fn conflicts_from_the_plan_reach_the_report() {
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data_with(vec![], vec![]));
    let p = Prepared {
        changeset: 7,
        plan: Plan {
            skipped: vec![(
                FeatureKind::Way,
                5,
                Conflict::ChangedSince { current_version: 9 },
            )],
            neutral: 2,
            ..Plan::default()
        },
        server: OsmData::default(),
    };

    let (ops, report) = apply(LayerId(1), &mut layer, &p);

    assert!(ops.is_empty());
    assert_eq!(report.neutral, 2);
    assert_eq!(
        report.skipped,
        vec![(
            FeatureKind::Way,
            5,
            SkipReason::Conflict(Conflict::ChangedSince { current_version: 9 })
        )]
    );
}
