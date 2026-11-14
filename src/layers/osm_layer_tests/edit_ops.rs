//! Every `EditOp` is undone exactly by its inverse, and a `Recording` captures
//! the ops an edit performed.

use super::fresh_rebuild::{assert_fresh, layer};
use crate::edit_ops::{replay, EditOp, Recording};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::{EditableLayer, LayerId};
use crate::osm::{MemberType, OsmData, OsmMember};
use crate::selection::{DeletedFeatureSnapshot, FeatureKind};

fn data(l: &OsmLayer) -> OsmData {
    l.store.osm_data.as_deref().cloned().unwrap()
}

/// Applying `op` changes the layer, applying its inverse puts everything back
/// (data and derived tables), and inverting twice is the identity.
fn round_trips(op: EditOp) {
    let mut l = layer();
    let before = data(&l);
    op.apply(&mut l).unwrap();
    assert_ne!(data(&l), before, "{op:?} changed nothing");
    assert_fresh(&l, "after the op");
    let after = data(&l);
    op.invert().apply(&mut l).unwrap();
    assert_eq!(data(&l), before, "the inverse of {op:?} did not restore");
    assert_fresh(&l, "after the inverse");
    op.apply(&mut l).unwrap();
    assert_eq!(data(&l), after, "re-applying {op:?} differs");
    assert_eq!(op.invert().invert(), op);
}

fn way_snapshot() -> DeletedFeatureSnapshot {
    DeletedFeatureSnapshot {
        kind: FeatureKind::Way,
        id: -50,
        tags: vec![("highway".into(), "path".into())],
        way_nodes: vec![8, 9],
        node_lat_lon: None,
        version: 0,
    }
}

fn member(reference: i64) -> OsmMember {
    OsmMember {
        member_type: MemberType::Way,
        reference,
        role: "x".into(),
    }
}

#[test]
fn put_and_remove_a_node() {
    round_trips(EditOp::PutFeature {
        snapshot: DeletedFeatureSnapshot {
            kind: FeatureKind::Node,
            id: -7,
            tags: vec![("a".into(), "b".into())],
            way_nodes: Vec::new(),
            node_lat_lon: Some((40.5, -74.5)),
            version: 0,
        },
    });
    // An unused node, tagged, with a server version that must round-trip.
    round_trips(EditOp::RemoveFeature {
        snapshot: DeletedFeatureSnapshot {
            kind: FeatureKind::Node,
            id: 8,
            tags: Vec::new(),
            way_nodes: Vec::new(),
            node_lat_lon: Some((40.001, -73.999)),
            version: 1,
        },
    });
    round_trips(EditOp::RemoveFeature {
        snapshot: DeletedFeatureSnapshot {
            kind: FeatureKind::Node,
            id: 7,
            tags: vec![("amenity".into(), "bench".into())],
            way_nodes: Vec::new(),
            node_lat_lon: Some((40.001, -74.0)),
            version: 1,
        },
    });
}

#[test]
fn put_and_remove_a_way() {
    round_trips(EditOp::PutFeature {
        snapshot: way_snapshot(),
    });
    round_trips(EditOp::RemoveFeature {
        snapshot: DeletedFeatureSnapshot {
            kind: FeatureKind::Way,
            id: 105,
            tags: vec![("highway".into(), "footway".into())],
            way_nodes: vec![4, 15],
            node_lat_lon: None,
            version: 1,
        },
    });
}

#[test]
fn put_and_remove_a_relation_keeps_its_list_position() {
    let mut scratch = layer();
    let snapshot = scratch.delete_relation(201).unwrap();
    assert_eq!(snapshot.index, 1);
    round_trips(EditOp::RemoveRelation {
        snapshot: snapshot.clone(),
    });
    let mut new = snapshot;
    new.relation.id = -300;
    round_trips(EditOp::PutRelation { snapshot: new });
}

#[test]
fn move_nodes_set_tag_way_nodes_members_and_reverse() {
    round_trips(EditOp::MoveNodes {
        moves: vec![
            (3, (40.0, -73.998), (40.01, -73.99)),
            (22, (40.012, -73.988), (40.02, -73.98)),
        ],
    });
    for (kind, id, before, after) in [
        (FeatureKind::Way, 100, Some("residential"), Some("primary")),
        (FeatureKind::Way, 100, Some("residential"), None),
        (FeatureKind::Node, 8, None, Some("v")),
        (FeatureKind::Relation, 201, Some("bus"), Some("tram")),
    ] {
        let key = match kind {
            FeatureKind::Way if id == 100 => "highway",
            FeatureKind::Relation => "route",
            _ => "k",
        };
        round_trips(EditOp::SetTag {
            kind,
            id,
            key: key.into(),
            before: before.map(String::from),
            after: after.map(String::from),
        });
    }
    round_trips(EditOp::SetWayNodes {
        id: 100,
        before: vec![1, 2, 3, 4],
        after: vec![1, 8, 2, 3],
    });
    round_trips(EditOp::SetRelationMembers {
        id: 201,
        before: vec![
            OsmMember {
                member_type: MemberType::Way,
                reference: 100,
                role: String::new(),
            },
            OsmMember {
                member_type: MemberType::Way,
                reference: 101,
                role: String::new(),
            },
        ],
        after: vec![member(105)],
    });
    round_trips(EditOp::ReverseWay { id: 100 });
}

#[test]
fn touches_names_what_an_op_changes() {
    let at = |kind, id| crate::selection::FeatureRef {
        layer_id: LayerId(4),
        kind,
        id,
    };
    let reverse = EditOp::ReverseWay { id: 3 };
    assert_eq!(reverse.touches(LayerId(4)), vec![at(FeatureKind::Way, 3)]);
    let moves = EditOp::MoveNodes {
        moves: vec![(1, (0.0, 0.0), (1.0, 1.0)), (2, (0.0, 0.0), (1.0, 1.0))],
    };
    assert_eq!(
        moves.touches(LayerId(4)),
        vec![at(FeatureKind::Node, 1), at(FeatureKind::Node, 2)]
    );
}

#[test]
fn a_recording_replays_both_ways_and_rolls_back() {
    let mut l = layer();
    let before = data(&l);
    let ops = {
        let mut rec = Recording::new(LayerId(1), &mut l);
        let a = rec.add_node(40.5, -74.5);
        let b = rec.add_node(40.6, -74.6);
        let way = rec.add_way(vec![a, b], vec![("highway".into(), "path".into())]);
        rec.insert_node_into_way(100, 1, 40.1, -74.1).unwrap();
        rec.extend_way(way, 8).unwrap();
        rec.remove_node_from_way(100, 3).unwrap();
        rec.reverse_way(101).unwrap();
        rec.set_tag(FeatureKind::Way, 100, "highway", "primary")
            .unwrap();
        rec.set_tag(FeatureKind::Way, 100, "name", "Main").unwrap();
        rec.remove_tag(FeatureKind::Way, 102, "building").unwrap();
        rec.commit_node_moves(&[(7, 41.0, -75.0)]).unwrap();
        rec.set_relation_members(201, vec![member(100)]).unwrap();
        let relation = rec.create_relation(vec![], vec![member(101)]).unwrap();
        rec.delete_relation(relation).unwrap();
        let removed = rec.delete_relation(202).unwrap();
        assert_eq!(removed.relation.id, 202);
        rec.set_way_nodes(105, vec![15, 4]).unwrap();
        // A refused primitive records nothing.
        let recorded = rec.ops().len();
        assert!(rec.extend_way(-999, 1).is_err());
        assert!(rec.remove_node(3).is_err());
        assert_eq!(rec.ops().len(), recorded);
        rec.into_ops()
    };
    let after = data(&l);
    assert_ne!(after, before);
    assert_fresh(&l, "after the recorded edit");

    let mut failures = Vec::new();
    replay(&ops, false, &mut l, &mut failures);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(data(&l), before);
    assert_fresh(&l, "after undo");
    replay(&ops, true, &mut l, &mut failures);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(data(&l), after);
    assert_fresh(&l, "after redo");

    // Rolling a recording back leaves the layer as it was.
    let mut l = layer();
    let mut rec = Recording::new(LayerId(1), &mut l);
    let n = rec.add_node(1.0, 2.0);
    rec.insert_node_into_way(100, 0, 3.0, 4.0).unwrap();
    rec.set_tag(FeatureKind::Node, n, "a", "b").unwrap();
    assert!(rec.roll_back().is_empty());
    assert_eq!(data(&l), before);
}
