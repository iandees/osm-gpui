//! Reversible primitive edits on one layer, and a recorder that captures them.
//!
//! An undo step is a list of `EditOp`s. Undo applies the inverted ops newest
//! first and redo applies the ops oldest first, so no edit needs inverse code
//! of its own: every op carries what both directions need (ids, versions and
//! list positions included) and `invert` is a mechanical swap.

use crate::layers::{EditError, EditableLayer, LayerId};
use crate::osm::OsmMember;
use crate::selection::{DeletedFeatureSnapshot, DeletedRelationSnapshot, FeatureKind, FeatureRef};

/// One node's move: id, (lat, lon) before and (lat, lon) after.
pub type NodeMove = (i64, (f64, f64), (f64, f64));

/// One reversible primitive on a layer's data.
#[derive(Clone, Debug, PartialEq)]
pub enum EditOp {
    /// A node or way comes into being with exactly the snapshot's id, tags,
    /// geometry and version.
    PutFeature {
        snapshot: DeletedFeatureSnapshot,
    },
    /// The node or way the snapshot describes goes away; the snapshot is what
    /// the layer held, so the inverse restores it exactly.
    RemoveFeature {
        snapshot: DeletedFeatureSnapshot,
    },
    /// A relation comes into being at the snapshot's list position.
    PutRelation {
        snapshot: DeletedRelationSnapshot,
    },
    RemoveRelation {
        snapshot: DeletedRelationSnapshot,
    },
    /// Nodes move together, as one layer commit.
    MoveNodes {
        moves: Vec<NodeMove>,
    },
    /// One tag of a node, way or relation changes; `None` is "absent".
    SetTag {
        kind: FeatureKind,
        id: i64,
        key: String,
        before: Option<String>,
        after: Option<String>,
    },
    /// A way's whole node list is replaced.
    SetWayNodes {
        id: i64,
        before: Vec<i64>,
        after: Vec<i64>,
    },
    /// A relation's whole member list is replaced.
    SetRelationMembers {
        id: i64,
        before: Vec<OsmMember>,
        after: Vec<OsmMember>,
    },
    /// A way's node order is reversed.
    ReverseWay {
        id: i64,
    },
}

impl EditOp {
    /// The op that undoes this one.
    pub fn invert(&self) -> EditOp {
        match self {
            EditOp::PutFeature { snapshot } => EditOp::RemoveFeature {
                snapshot: snapshot.clone(),
            },
            EditOp::RemoveFeature { snapshot } => EditOp::PutFeature {
                snapshot: snapshot.clone(),
            },
            EditOp::PutRelation { snapshot } => EditOp::RemoveRelation {
                snapshot: snapshot.clone(),
            },
            EditOp::RemoveRelation { snapshot } => EditOp::PutRelation {
                snapshot: snapshot.clone(),
            },
            EditOp::MoveNodes { moves } => EditOp::MoveNodes {
                moves: moves
                    .iter()
                    .map(|&(id, before, after)| (id, after, before))
                    .collect(),
            },
            EditOp::SetTag {
                kind,
                id,
                key,
                before,
                after,
            } => EditOp::SetTag {
                kind: *kind,
                id: *id,
                key: key.clone(),
                before: after.clone(),
                after: before.clone(),
            },
            EditOp::SetWayNodes { id, before, after } => EditOp::SetWayNodes {
                id: *id,
                before: after.clone(),
                after: before.clone(),
            },
            EditOp::SetRelationMembers { id, before, after } => EditOp::SetRelationMembers {
                id: *id,
                before: after.clone(),
                after: before.clone(),
            },
            EditOp::ReverseWay { id } => EditOp::ReverseWay { id: *id },
        }
    }

    /// Perform this op on `layer`.
    pub fn apply(&self, layer: &mut dyn EditableLayer) -> Result<(), EditError> {
        match self {
            EditOp::PutFeature { snapshot } => layer.restore_feature(snapshot.clone()),
            EditOp::RemoveFeature { snapshot } => {
                layer.delete_feature(snapshot.kind, snapshot.id).map(drop)
            }
            EditOp::PutRelation { snapshot } => layer.restore_relation(snapshot.clone()),
            EditOp::RemoveRelation { snapshot } => {
                layer.delete_relation(snapshot.relation.id).map(drop)
            }
            EditOp::MoveNodes { moves } => {
                let moves: Vec<(i64, f64, f64)> = moves
                    .iter()
                    .map(|&(id, _, (lat, lon))| (id, lat, lon))
                    .collect();
                layer.commit_node_moves(&moves)
            }
            EditOp::SetTag {
                kind,
                id,
                key,
                after,
                ..
            } => match after {
                Some(value) => layer.set_tag(*kind, *id, key, value),
                None => layer.remove_tag(*kind, *id, key),
            },
            EditOp::SetWayNodes { id, after, .. } => layer.set_way_nodes(*id, after.clone()),
            EditOp::SetRelationMembers { id, after, .. } => {
                layer.set_relation_members(*id, after.clone()).map(drop)
            }
            EditOp::ReverseWay { id } => layer.reverse_way(*id),
        }
    }

    /// The features this op names, on layer `layer_id`.
    pub fn touches(&self, layer_id: LayerId) -> Vec<FeatureRef> {
        let at = |kind, id| FeatureRef { layer_id, kind, id };
        match self {
            EditOp::PutFeature { snapshot } | EditOp::RemoveFeature { snapshot } => {
                vec![at(snapshot.kind, snapshot.id)]
            }
            EditOp::PutRelation { snapshot } | EditOp::RemoveRelation { snapshot } => {
                vec![at(FeatureKind::Relation, snapshot.relation.id)]
            }
            EditOp::MoveNodes { moves } => moves
                .iter()
                .map(|&(id, ..)| at(FeatureKind::Node, id))
                .collect(),
            EditOp::SetTag { kind, id, .. } => vec![at(*kind, *id)],
            EditOp::SetWayNodes { id, .. } | EditOp::ReverseWay { id } => {
                vec![at(FeatureKind::Way, *id)]
            }
            EditOp::SetRelationMembers { id, .. } => vec![at(FeatureKind::Relation, *id)],
        }
    }
}

/// Replay `ops` on `layer`: forward in order (redo), or inverted newest first
/// (undo). A refused op is added to `failures` and the rest still run.
pub fn replay(
    ops: &[EditOp],
    forward: bool,
    layer: &mut dyn EditableLayer,
    failures: &mut Vec<EditError>,
) {
    let mut run = |op: &EditOp| {
        if let Err(e) = op.apply(layer) {
            failures.push(e);
        }
    };
    if forward {
        ops.iter().for_each(&mut run);
    } else {
        ops.iter().rev().map(EditOp::invert).for_each(|op| run(&op));
    }
}

/// An `EditableLayer` seen through a recorder: each mutating method performs
/// the primitive and, only if the layer accepted it, appends the op that
/// describes it. Code that edits through a `Recording` cannot forget to
/// record.
pub struct Recording<'a> {
    layer_id: LayerId,
    layer: &'a mut dyn EditableLayer,
    ops: Vec<EditOp>,
}

impl<'a> Recording<'a> {
    pub fn new(layer_id: LayerId, layer: &'a mut dyn EditableLayer) -> Self {
        Self::resume(layer_id, layer, Vec::new())
    }

    /// Continue recording after `ops`, captured earlier by another
    /// `Recording` of the same layer.
    pub fn resume(layer_id: LayerId, layer: &'a mut dyn EditableLayer, ops: Vec<EditOp>) -> Self {
        Recording {
            layer_id,
            layer,
            ops,
        }
    }

    pub fn layer_id(&self) -> LayerId {
        self.layer_id
    }

    /// The layer, for reads.
    pub fn layer(&self) -> &dyn EditableLayer {
        &*self.layer
    }

    pub fn ops(&self) -> &[EditOp] {
        &self.ops
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn into_ops(self) -> Vec<EditOp> {
        self.ops
    }

    /// Undo everything recorded so far, newest first, and return what the
    /// layer refused.
    pub fn roll_back(self) -> Vec<EditError> {
        let mut failures = Vec::new();
        replay(&self.ops, false, self.layer, &mut failures);
        failures
    }

    fn tag_of(&self, kind: FeatureKind, id: i64, key: &str) -> Option<String> {
        let feature = FeatureRef {
            layer_id: self.layer_id,
            kind,
            id,
        };
        self.layer
            .feature_tags(&feature)?
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    fn way_nodes(&self, way_id: i64) -> Result<Vec<i64>, EditError> {
        self.layer
            .way_node_ids(way_id)
            .ok_or(EditError::UnknownWay(way_id))
    }

    fn new_node_snapshot(id: i64, lat: f64, lon: f64) -> DeletedFeatureSnapshot {
        DeletedFeatureSnapshot {
            kind: FeatureKind::Node,
            id,
            tags: Vec::new(),
            way_nodes: Vec::new(),
            node_lat_lon: Some((lat, lon)),
            version: 0,
        }
    }

    pub fn commit_node_moves(&mut self, moves: &[(i64, f64, f64)]) -> Result<(), EditError> {
        let recorded: Vec<NodeMove> = moves
            .iter()
            .filter_map(|&(id, lat, lon)| Some((id, self.layer.node_lat_lon(id)?, (lat, lon))))
            .collect();
        self.layer.commit_node_moves(moves)?;
        if !recorded.is_empty() {
            self.ops.push(EditOp::MoveNodes { moves: recorded });
        }
        Ok(())
    }

    pub fn set_tag(
        &mut self,
        kind: FeatureKind,
        id: i64,
        key: &str,
        value: &str,
    ) -> Result<(), EditError> {
        let before = self.tag_of(kind, id, key);
        self.layer.set_tag(kind, id, key, value)?;
        self.ops.push(EditOp::SetTag {
            kind,
            id,
            key: key.to_string(),
            before,
            after: Some(value.to_string()),
        });
        Ok(())
    }

    pub fn remove_tag(&mut self, kind: FeatureKind, id: i64, key: &str) -> Result<(), EditError> {
        let before = self.tag_of(kind, id, key);
        self.layer.remove_tag(kind, id, key)?;
        self.ops.push(EditOp::SetTag {
            kind,
            id,
            key: key.to_string(),
            before,
            after: None,
        });
        Ok(())
    }

    pub fn create_node(&mut self, lat: f64, lon: f64, id: Option<i64>) -> Result<i64, EditError> {
        let id = self.layer.create_node(lat, lon, id)?;
        self.ops.push(EditOp::PutFeature {
            snapshot: Self::new_node_snapshot(id, lat, lon),
        });
        Ok(id)
    }

    pub fn add_node(&mut self, lat: f64, lon: f64) -> i64 {
        let id = self.layer.add_node(lat, lon);
        self.ops.push(EditOp::PutFeature {
            snapshot: Self::new_node_snapshot(id, lat, lon),
        });
        id
    }

    pub fn delete_feature(
        &mut self,
        kind: FeatureKind,
        id: i64,
    ) -> Result<DeletedFeatureSnapshot, EditError> {
        let snapshot = self.layer.delete_feature(kind, id)?;
        self.ops.push(EditOp::RemoveFeature {
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    pub fn restore_feature(&mut self, snapshot: DeletedFeatureSnapshot) -> Result<(), EditError> {
        self.layer.restore_feature(snapshot.clone())?;
        self.ops.push(EditOp::PutFeature { snapshot });
        Ok(())
    }

    pub fn add_way(&mut self, node_ids: Vec<i64>, tags: Vec<(String, String)>) -> i64 {
        let id = self.layer.add_way(node_ids.clone(), tags.clone());
        self.ops.push(EditOp::PutFeature {
            snapshot: DeletedFeatureSnapshot {
                kind: FeatureKind::Way,
                id,
                tags,
                way_nodes: node_ids,
                node_lat_lon: None,
                version: 0,
            },
        });
        id
    }

    pub fn extend_way(&mut self, way_id: i64, node_id: i64) -> Result<(), EditError> {
        let before = self.way_nodes(way_id)?;
        self.layer.extend_way(way_id, node_id)?;
        let mut after = before.clone();
        after.push(node_id);
        self.ops.push(EditOp::SetWayNodes {
            id: way_id,
            before,
            after,
        });
        Ok(())
    }

    pub fn insert_node_into_way(
        &mut self,
        way_id: i64,
        index: usize,
        lat: f64,
        lon: f64,
    ) -> Result<i64, EditError> {
        let before = self.way_nodes(way_id)?;
        let id = self.layer.insert_node_into_way(way_id, index, lat, lon)?;
        let mut after = before.clone();
        after.insert(index, id);
        self.ops.push(EditOp::PutFeature {
            snapshot: Self::new_node_snapshot(id, lat, lon),
        });
        self.ops.push(EditOp::SetWayNodes {
            id: way_id,
            before,
            after,
        });
        Ok(id)
    }

    pub fn remove_node(&mut self, node_id: i64) -> Result<(), EditError> {
        self.delete_feature(FeatureKind::Node, node_id).map(drop)
    }

    pub fn remove_way(&mut self, way_id: i64) -> Result<(), EditError> {
        self.delete_feature(FeatureKind::Way, way_id).map(drop)
    }

    pub fn remove_node_from_way(&mut self, way_id: i64, index: usize) -> Result<(), EditError> {
        let before = self.way_nodes(way_id)?;
        self.layer.remove_node_from_way(way_id, index)?;
        let mut after = before.clone();
        after.remove(index);
        self.ops.push(EditOp::SetWayNodes {
            id: way_id,
            before,
            after,
        });
        Ok(())
    }

    pub fn reverse_way(&mut self, way_id: i64) -> Result<(), EditError> {
        self.layer.reverse_way(way_id)?;
        self.ops.push(EditOp::ReverseWay { id: way_id });
        Ok(())
    }

    pub fn set_way_nodes(&mut self, way_id: i64, node_ids: Vec<i64>) -> Result<(), EditError> {
        let before = self.way_nodes(way_id)?;
        self.layer.set_way_nodes(way_id, node_ids.clone())?;
        self.ops.push(EditOp::SetWayNodes {
            id: way_id,
            before,
            after: node_ids,
        });
        Ok(())
    }

    pub fn set_relation_members(
        &mut self,
        relation_id: i64,
        members: Vec<OsmMember>,
    ) -> Result<Vec<OsmMember>, EditError> {
        let previous = self
            .layer
            .set_relation_members(relation_id, members.clone())?;
        self.ops.push(EditOp::SetRelationMembers {
            id: relation_id,
            before: previous.clone(),
            after: members,
        });
        Ok(previous)
    }

    pub fn create_relation(
        &mut self,
        tags: Vec<(String, String)>,
        members: Vec<OsmMember>,
    ) -> Result<i64, EditError> {
        let id = self.layer.create_relation(tags, members)?;
        if let (Some(relation), Some(index)) = (
            self.layer.relation(id),
            self.layer.relation_ids().iter().position(|&r| r == id),
        ) {
            self.ops.push(EditOp::PutRelation {
                snapshot: DeletedRelationSnapshot { relation, index },
            });
        }
        Ok(id)
    }

    pub fn delete_relation(
        &mut self,
        relation_id: i64,
    ) -> Result<DeletedRelationSnapshot, EditError> {
        let snapshot = self.layer.delete_relation(relation_id)?;
        self.ops.push(EditOp::RemoveRelation {
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    pub fn restore_relation(&mut self, snapshot: DeletedRelationSnapshot) -> Result<(), EditError> {
        self.layer.restore_relation(snapshot.clone())?;
        self.ops.push(EditOp::PutRelation { snapshot });
        Ok(())
    }
}
