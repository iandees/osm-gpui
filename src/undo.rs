//! Undo/redo model for committed data mutations.

use osm_gpui::edit_ops::{replay, EditOp, Recording};
use osm_gpui::layers::{EditError, LayerId, LayerManager};
use osm_gpui::osm::OsmMember;
use osm_gpui::selection::FeatureRef;
#[cfg(test)]
use osm_gpui::selection::{DeletedFeatureSnapshot, DeletedRelationSnapshot, FeatureKind};

/// Undo what `rec` has recorded so far, for an action that stopped part way.
pub(crate) fn roll_back_recording(rec: Recording<'_>) {
    for failure in rec.roll_back() {
        eprintln!("rolling back a half-done edit was refused: {failure}");
    }
}

/// One `SetTags` entry: feature, key, value before, value after.
pub(crate) type TagEntry = (FeatureRef, String, Option<String>, Option<String>);

/// Write each entry's "after" value to its layer and return the entries that
/// took effect, for the undo step. An entry whose layer or feature is gone is
/// logged and left out, so the step never claims an edit that did not happen.
pub(crate) fn write_tag_entries(
    layers: &mut LayerManager,
    entries: Vec<TagEntry>,
) -> Vec<TagEntry> {
    entries
        .into_iter()
        .filter(|(feature, key, _, after)| {
            let Some(editable) = layers
                .find_layer_mut(feature.layer_id)
                .and_then(|layer| layer.as_editable_mut())
            else {
                eprintln!(
                    "tag edit skipped: layer {:?} is not editable",
                    feature.layer_id
                );
                return false;
            };
            let written = match after {
                Some(value) => editable.set_tag(feature.kind, feature.id, key, value),
                None => editable.remove_tag(feature.kind, feature.id, key),
            };
            if let Err(e) = &written {
                eprintln!("tag edit on {:?} {} refused: {e}", feature.kind, feature.id);
            }
            written.is_ok()
        })
        .collect()
}

/// A single node move: its id, (lat, lon) before, and (lat, lon) after.
pub(crate) type NodeMoveUndoEntry = osm_gpui::edit_ops::NodeMove;

/// Per layer: node id -> (before (lat, lon), after (lat, lon)).
pub(crate) type NodeMoveUndoEntries = Vec<(LayerId, Vec<NodeMoveUndoEntry>)>;

/// What the History panel calls an `Edit` step, with whatever its text
/// counts.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EditLabel {
    CreatedNode,
    #[cfg(test)]
    CreatedWay,
    /// A node or way deleted; the kind picks the text.
    Deleted(osm_gpui::selection::FeatureKind),
    ExtendedWay,
    CreatedBuilding,
    CreatedArc,
    ExtrudedWay,
    InsertedNode,
    SnappedNode,
    /// Ways reversed, counted.
    Reversed(usize),
    #[cfg(test)]
    ChangedWayNodes,
    ChangedRelationMembers,
    CreatedRelation,
    /// Nodes moved, counted.
    MovedNodes(usize),
    /// Tag entries changed, counted.
    ChangedTags(usize),
    /// A label the caller words itself (a multi-part action's name).
    Text(String),
}

impl EditLabel {
    fn text(&self) -> String {
        use osm_gpui::selection::FeatureKind;
        match self {
            EditLabel::CreatedNode => osm_gpui::tr!("history.created_node").to_string(),
            #[cfg(test)]
            EditLabel::CreatedWay => osm_gpui::tr!("history.created_way").to_string(),
            EditLabel::Deleted(FeatureKind::Node) => {
                osm_gpui::tr!("history.deleted_node").to_string()
            }
            EditLabel::Deleted(FeatureKind::Way) => {
                osm_gpui::tr!("history.deleted_way").to_string()
            }
            EditLabel::Deleted(FeatureKind::Relation) => {
                osm_gpui::tr!("history.deleted_relation").to_string()
            }
            EditLabel::ExtendedWay => osm_gpui::tr!("history.extended_way").to_string(),
            EditLabel::CreatedBuilding => osm_gpui::tr!("history.created_building").to_string(),
            EditLabel::CreatedArc => osm_gpui::tr!("history.created_arc").to_string(),
            EditLabel::ExtrudedWay => osm_gpui::tr!("history.extruded_way").to_string(),
            EditLabel::InsertedNode => osm_gpui::tr!("history.inserted_node").to_string(),
            EditLabel::SnappedNode => osm_gpui::tr!("history.snapped_node").to_string(),
            EditLabel::Reversed(count) => {
                osm_gpui::tr_count!("action.reverse.done", *count).to_string()
            }
            #[cfg(test)]
            EditLabel::ChangedWayNodes => osm_gpui::tr!("history.changed_way_nodes").to_string(),
            EditLabel::ChangedRelationMembers => {
                osm_gpui::tr!("history.changed_relation_members").to_string()
            }
            EditLabel::CreatedRelation => osm_gpui::tr!("history.created_relation").to_string(),
            EditLabel::MovedNodes(count) => {
                osm_gpui::tr_count!("history.moved_nodes", *count).to_string()
            }
            EditLabel::ChangedTags(count) => {
                osm_gpui::tr_count!("history.changed_tags", *count).to_string()
            }
            EditLabel::Text(text) => text.clone(),
        }
    }
}

/// A single reversible data mutation, recorded on the global undo stack.
#[derive(Clone)]
pub(crate) enum UndoableAction {
    /// Primitive edits to one layer, captured as they were made (see
    /// `osm_gpui::edit_ops`): redo replays `ops` in order, undo replays their
    /// inverses newest first, so the step needs no inverse code of its own.
    Edit {
        layer: LayerId,
        label: EditLabel,
        ops: Vec<EditOp>,
        /// What to select after undoing or redoing, when that should differ
        /// from "everything the ops touch, in order" (a drawn building
        /// selects its way, not its corner nodes).
        select: Option<Vec<osm_gpui::selection::FeatureRef>>,
    },
    /// Purge: objects dropped from `layer`'s downloaded data without being
    /// deleted. Undo puts them back as downloaded; redo purges them again.
    Purge {
        layer: LayerId,
        purged: osm_gpui::selection::PurgedFeatures,
    },
    /// Paste or Duplicate: a set of new nodes, ways and relations created in
    /// one layer at once. Undo removes all of them (and brings back the
    /// selection from before); redo recreates them under the same ids.
    PasteFeatures {
        pasted: osm_gpui::feature_clipboard::PastedFeatures,
    },
    /// Several actions recorded as one undo step (e.g. deleting a whole
    /// selection). Undo reverts them last-to-first; redo reapplies them in
    /// order. `label` is the history-list text.
    Batch {
        label: String,
        actions: Vec<UndoableAction>,
    },
}

impl UndoableAction {
    /// A step made of `ops`, recorded on `layer`.
    pub(crate) fn edit(layer: LayerId, label: EditLabel, ops: Vec<EditOp>) -> Self {
        UndoableAction::Edit {
            layer,
            label,
            ops,
            select: None,
        }
    }

    /// Nodes moved, one entry list per layer. A step spanning several layers
    /// is a `Batch` of one `Edit` per layer, each selecting `select`.
    pub(crate) fn move_nodes(
        per_layer: NodeMoveUndoEntries,
        select: Option<Vec<FeatureRef>>,
    ) -> Self {
        let count: usize = per_layer.iter().map(|(_, entries)| entries.len()).sum();
        let mut edits: Vec<UndoableAction> = per_layer
            .into_iter()
            .map(|(layer, moves)| {
                let label = EditLabel::MovedNodes(moves.len());
                let mut edit = Self::edit(layer, label, vec![EditOp::MoveNodes { moves }]);
                if let (UndoableAction::Edit { select: slot, .. }, Some(select)) =
                    (&mut edit, &select)
                {
                    *slot = Some(select.clone());
                }
                edit
            })
            .collect();
        if edits.len() == 1 {
            return edits.remove(0);
        }
        UndoableAction::Batch {
            label: EditLabel::MovedNodes(count).text(),
            actions: edits,
        }
    }

    /// Tags changed, one entry per feature and key. A step spanning several
    /// layers is a `Batch` of one `Edit` per layer.
    pub(crate) fn set_tags(entries: Vec<TagEntry>) -> Self {
        let count = entries.len();
        let mut per_layer: Vec<(LayerId, Vec<EditOp>)> = Vec::new();
        for (feature, key, before, after) in entries {
            let op = EditOp::SetTag {
                kind: feature.kind,
                id: feature.id,
                key,
                before,
                after,
            };
            match per_layer.iter_mut().find(|(layer, _)| *layer == feature.layer_id) {
                Some((_, ops)) => ops.push(op),
                None => per_layer.push((feature.layer_id, vec![op])),
            }
        }
        let mut edits: Vec<UndoableAction> = per_layer
            .into_iter()
            .map(|(layer, ops)| Self::edit(layer, EditLabel::ChangedTags(ops.len()), ops))
            .collect();
        if edits.len() == 1 {
            return edits.remove(0);
        }
        UndoableAction::Batch {
            label: EditLabel::ChangedTags(count).text(),
            actions: edits,
        }
    }

    #[cfg(test)]
    /// A node created on `layer` at `(lat, lon)` under `id`.
    pub(crate) fn create_node(layer: LayerId, id: i64, lat: f64, lon: f64) -> Self {
        let snapshot = DeletedFeatureSnapshot {
            kind: FeatureKind::Node,
            id,
            tags: Vec::new(),
            way_nodes: Vec::new(),
            node_lat_lon: Some((lat, lon)),
            version: 0,
        };
        Self::edit(
            layer,
            EditLabel::CreatedNode,
            vec![EditOp::PutFeature { snapshot }],
        )
    }

    #[cfg(test)]
    /// A way created on `layer`, as `snapshot` describes it.
    pub(crate) fn create_way(layer: LayerId, snapshot: DeletedFeatureSnapshot) -> Self {
        Self::edit(
            layer,
            EditLabel::CreatedWay,
            vec![EditOp::PutFeature { snapshot }],
        )
    }

    #[cfg(test)]
    /// A node or way deleted from `layer`; `snapshot` is what it held.
    pub(crate) fn delete_feature(layer: LayerId, snapshot: DeletedFeatureSnapshot) -> Self {
        let label = EditLabel::Deleted(snapshot.kind);
        Self::edit(layer, label, vec![EditOp::RemoveFeature { snapshot }])
    }

    #[cfg(test)]
    /// `way_ids` reversed on `layer`.
    pub(crate) fn reverse_ways(layer: LayerId, way_ids: Vec<i64>) -> Self {
        let label = EditLabel::Reversed(way_ids.len());
        let ops = way_ids
            .into_iter()
            .map(|id| EditOp::ReverseWay { id })
            .collect();
        Self::edit(layer, label, ops)
    }

    #[cfg(test)]
    /// A way's node list replaced.
    pub(crate) fn set_way_nodes(
        layer: LayerId,
        id: i64,
        before: Vec<i64>,
        after: Vec<i64>,
    ) -> Self {
        Self::edit(
            layer,
            EditLabel::ChangedWayNodes,
            vec![EditOp::SetWayNodes { id, before, after }],
        )
    }

    /// A relation's member list replaced.
    pub(crate) fn set_relation_members(
        layer: LayerId,
        id: i64,
        before: Vec<OsmMember>,
        after: Vec<OsmMember>,
    ) -> Self {
        Self::edit(
            layer,
            EditLabel::ChangedRelationMembers,
            vec![EditOp::SetRelationMembers { id, before, after }],
        )
    }

    #[cfg(test)]
    /// A relation created on `layer`.
    pub(crate) fn create_relation(layer: LayerId, snapshot: DeletedRelationSnapshot) -> Self {
        Self::edit(
            layer,
            EditLabel::CreatedRelation,
            vec![EditOp::PutRelation { snapshot }],
        )
    }

    #[cfg(test)]
    /// A relation deleted from `layer`.
    pub(crate) fn delete_relation(layer: LayerId, snapshot: DeletedRelationSnapshot) -> Self {
        Self::edit(
            layer,
            EditLabel::Deleted(FeatureKind::Relation),
            vec![EditOp::RemoveRelation { snapshot }],
        )
    }

    /// This step, selecting `features` after an undo or redo.
    pub(crate) fn selecting(mut self, features: Vec<osm_gpui::selection::FeatureRef>) -> Self {
        if let UndoableAction::Edit { select, .. } = &mut self {
            *select = Some(features);
        }
        self
    }

    /// The features this step touched, whichever direction it is applied in.
    /// After an undo or redo the caller selects the ones that still exist
    /// (an undone create or a redone delete names features that are gone, so
    /// those drop out when the caller prunes). Pure: it only reads the
    /// recorded ids, never the layer. The result has no duplicates.
    pub(crate) fn affected_features(&self) -> Vec<osm_gpui::selection::FeatureRef> {
        use osm_gpui::selection::FeatureRef;
        let mut out: Vec<FeatureRef> = Vec::new();
        match self {
            UndoableAction::Edit {
                layer, ops, select, ..
            } => match select {
                Some(select) => out.extend(select.iter().copied()),
                None => out.extend(ops.iter().flat_map(|op| op.touches(*layer))),
            },
            UndoableAction::Purge { layer, purged } => out.extend(purged.features(*layer)),
            UndoableAction::PasteFeatures { pasted } => out.extend(pasted.created()),
            UndoableAction::Batch { actions, .. } => {
                for action in actions {
                    out.extend(action.affected_features());
                }
            }
        }
        let mut seen: Vec<FeatureRef> = Vec::with_capacity(out.len());
        for feature in out {
            if !seen.contains(&feature) {
                seen.push(feature);
            }
        }
        seen
    }

    /// Every layer this step touches (a `MoveNodes` or `Batch` can span
    /// several). Pure, like `affected_features`, but it also reports layers
    /// whose part of the step names no features (an empty `per_layer` entry).
    /// The result has no duplicates.
    pub(crate) fn layers(&self) -> Vec<LayerId> {
        let mut out: Vec<LayerId> = Vec::new();
        self.collect_layers(&mut out);
        out
    }

    fn collect_layers(&self, out: &mut Vec<LayerId>) {
        let mut add = |layer: LayerId| {
            if !out.contains(&layer) {
                out.push(layer);
            }
        };
        match self {
            UndoableAction::Edit { layer, .. } => add(*layer),
            UndoableAction::Purge { layer, .. } => add(*layer),
            UndoableAction::PasteFeatures { pasted } => add(pasted.layer),
            UndoableAction::Batch { actions, .. } => {
                for action in actions {
                    action.collect_layers(out);
                }
            }
        }
    }

    /// Apply this step to `layers`: `forward` redoes it, else undoes it.
    /// Whatever a layer refuses is added to `failures`; the rest still runs.
    pub(crate) fn apply(
        &self,
        layers: &mut LayerManager,
        forward: bool,
        failures: &mut Vec<EditError>,
    ) {
        let action = self;
        match action {
            UndoableAction::Edit { layer, ops, .. } => {
                if let Some(editable) = layers
                    .find_layer_mut(*layer)
                    .and_then(|layer| layer.as_editable_mut())
                {
                    replay(ops, forward, editable, failures);
                }
            }
            UndoableAction::Purge { layer, purged } => {
                let Some(editable) = layers
                    .find_layer_mut(*layer)
                    .and_then(|layer| layer.as_editable_mut())
                else {
                    return;
                };
                if forward {
                    let ids: Vec<i64> = purged.nodes.iter().map(|n| n.id).collect();
                    let way_ids: Vec<i64> = purged.ways.iter().map(|w| w.id).collect();
                    let relation_ids: Vec<i64> =
                        purged.relations.iter().map(|(r, ..)| r.id).collect();
                    editable.purge_features(&ids, &way_ids, &relation_ids);
                } else {
                    editable.restore_purged(purged.clone());
                }
            }
            UndoableAction::PasteFeatures { pasted } => {
                let Some(editable) = layers
                    .find_layer_mut(pasted.layer)
                    .and_then(|layer| layer.as_editable_mut())
                else {
                    return;
                };
                failures.extend(if forward {
                    pasted.redo(editable)
                } else {
                    pasted.undo(editable)
                });
            }
            UndoableAction::Batch { actions, .. } => {
                if forward {
                    for action in actions {
                        action.apply(layers, true, failures);
                    }
                } else {
                    for action in actions.iter().rev() {
                        action.apply(layers, false, failures);
                    }
                }
            }
        }
    }

    /// Human-readable label for the history list, e.g. "Moved 3 nodes".
    pub(crate) fn description(&self) -> String {
        match self {
            UndoableAction::Edit { label, .. } => label.text(),
            UndoableAction::Purge { .. } => osm_gpui::tr!("history.purged").to_string(),
            UndoableAction::PasteFeatures { pasted } => {
                let key = if pasted.duplicate {
                    "history.duplicated_features"
                } else {
                    "history.pasted_features"
                };
                osm_gpui::i18n::tr_dynamic(key).to_string()
            }
            UndoableAction::Batch { label, .. } => label.clone(),
        }
    }
}

/// A global undo/redo stack of committed data mutations, shared across all
/// layers in the order actions happened.
#[derive(Default)]
pub(crate) struct UndoStack {
    pub(crate) actions: Vec<UndoableAction>,
    /// Index of the next action that would be redone. Equals
    /// `actions.len()` when at the tip (nothing to redo).
    pub(crate) cursor: usize,
    /// Bumped on every push/undo/redo. Lets callers cache values derived
    /// from the edited data (e.g. the status bar's pending-edit count)
    /// without `(cursor, len)` aliasing after an undo-then-push.
    pub(crate) revision: u64,
}

impl UndoStack {
    pub(crate) fn push(&mut self, action: UndoableAction) {
        self.revision += 1;
        self.actions.truncate(self.cursor);
        self.actions.push(action);
        self.cursor = self.actions.len();
    }

    /// Remove every step (undoable or redoable) that touches one of
    /// `tainted`, keeping the rest in order with the cursor still pointing
    /// at the same place in it. Returns how many steps were removed.
    ///
    /// Used when the data behind some features was replaced from outside the
    /// history. Steps that name none of them operate on other features only
    /// and don't depend on the removed steps, so they stay valid; any step
    /// that does name one (a `Batch` counts if any part of it does) could
    /// resurrect a stale version or content, so it goes.
    pub(crate) fn remove_touching(&mut self, tainted: &[osm_gpui::selection::FeatureRef]) -> usize {
        let touches = |a: &UndoableAction| {
            a.affected_features()
                .iter()
                .any(|f| tainted.iter().any(|t| t == f))
        };
        let before = self.actions.len();
        let below_cursor_removed = self.actions[..self.cursor]
            .iter()
            .filter(|a| touches(a))
            .count();
        self.actions.retain(|a| !touches(a));
        self.cursor -= below_cursor_removed;
        self.revision += 1;
        before - self.actions.len()
    }

    /// Remove every step (undoable or redoable) that belongs to `layer`,
    /// keeping the rest in order with the cursor still pointing at the same
    /// place among them. Returns how many steps were removed. Bumps the
    /// revision.
    ///
    /// Used when the layer is deleted: its steps can never be applied again,
    /// and applying one to a later layer would be worse than a no-op.
    ///
    /// Rule for steps spanning layers (a `MoveNodes` with several `per_layer`
    /// lists, a `Batch` with parts in several layers): the whole step goes if
    /// any part belongs to `layer`. Trimming only that part would leave a
    /// step whose label, ordering assumptions (a `Batch` orders its parts so
    /// each is valid after the previous one) and selection afterwards no
    /// longer match what was recorded. The surviving layers' edits stay in
    /// place; they just can't be undone from the history any more.
    pub(crate) fn remove_layer(&mut self, layer: LayerId) -> usize {
        let belongs = |a: &UndoableAction| a.layers().contains(&layer);
        let before = self.actions.len();
        let below_cursor_removed = self.actions[..self.cursor]
            .iter()
            .filter(|a| belongs(a))
            .count();
        self.actions.retain(|a| !belongs(a));
        self.cursor -= below_cursor_removed;
        self.revision += 1;
        before - self.actions.len()
    }

    /// Returns the action to invert, and moves the cursor back. `None` if
    /// there's nothing to undo.
    pub(crate) fn undo(&mut self) -> Option<UndoableAction> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        self.revision += 1;
        Some(self.actions[self.cursor].clone())
    }

    /// Returns the action to reapply, and moves the cursor forward. `None`
    /// if there's nothing to redo.
    pub(crate) fn redo(&mut self) -> Option<UndoableAction> {
        if self.cursor == self.actions.len() {
            return None;
        }
        let action = self.actions[self.cursor].clone();
        self.cursor += 1;
        self.revision += 1;
        Some(action)
    }
}

#[cfg(test)]
mod undo_stack_tests {
    use super::{EditLabel, LayerId, UndoStack, UndoableAction};
    use osm_gpui::edit_ops::EditOp;
    use osm_gpui::selection::{FeatureKind, FeatureRef};

    fn move_one(id: i64, before: (f64, f64), after: (f64, f64)) -> UndoableAction {
        UndoableAction::move_nodes(vec![(LayerId(1), vec![(id, before, after)])], None)
    }

    #[test]
    fn remove_touching_keeps_unrelated_steps_and_the_cursor_position() {
        let tainted = [FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Node,
            id: 2,
        }];
        let mut stack = UndoStack::default();
        stack.push(move_one(1, (0.0, 0.0), (1.0, 1.0)));
        stack.push(move_one(2, (0.0, 0.0), (1.0, 1.0)));
        stack.push(UndoableAction::Batch {
            label: "batch".into(),
            actions: vec![
                move_one(3, (0.0, 0.0), (1.0, 1.0)),
                move_one(2, (1.0, 1.0), (2.0, 2.0)),
            ],
        });
        stack.push(move_one(4, (0.0, 0.0), (1.0, 1.0)));
        // Undo the last step so it is redoable, then prune.
        stack.undo();
        assert_eq!((stack.actions.len(), stack.cursor), (4, 3));
        let removed = stack.remove_touching(&tainted);
        assert_eq!(removed, 2, "the direct step and the batch holding one");
        assert_eq!((stack.actions.len(), stack.cursor), (2, 1));
        // The surviving step below the cursor is node 1's, the redoable one node 4's.
        assert!(stack.undo().is_some());
        assert!(stack.undo().is_none());
        assert!(stack.redo().is_some());
        assert!(stack.redo().is_some());
        assert!(stack.redo().is_none());
    }

    #[test]
    fn remove_touching_matches_the_layer_too() {
        let other_layer = [FeatureRef {
            layer_id: LayerId(9),
            kind: FeatureKind::Node,
            id: 1,
        }];
        let mut stack = UndoStack::default();
        stack.push(move_one(1, (0.0, 0.0), (1.0, 1.0)));
        assert_eq!(stack.remove_touching(&other_layer), 0);
        assert_eq!(stack.actions.len(), 1);
    }

    fn move_in(layer: u64, id: i64) -> UndoableAction {
        UndoableAction::move_nodes(
            vec![(LayerId(layer), vec![(id, (0.0, 0.0), (1.0, 1.0))])],
            None,
        )
    }

    fn create_in(layer: u64, id: i64) -> UndoableAction {
        UndoableAction::create_node(LayerId(layer), id, 0.0, 0.0)
    }

    #[test]
    fn remove_layer_keeps_other_layers_in_order_around_a_middle_cursor() {
        let mut stack = UndoStack::default();
        stack.push(move_in(1, 1)); // 0 keep
        stack.push(move_in(2, 2)); // 1 drop
        stack.push(create_in(1, -1)); // 2 keep
        stack.push(create_in(2, -2)); // 3 drop (redoable)
        stack.push(move_in(1, 3)); // 4 keep (redoable)
        stack.push(move_in(2, 4)); // 5 drop (redoable)
        stack.undo();
        stack.undo();
        stack.undo();
        assert_eq!(stack.cursor, 3);
        let revision = stack.revision;
        assert_eq!(stack.remove_layer(LayerId(2)), 3);
        assert!(stack.revision > revision);
        assert_eq!((stack.actions.len(), stack.cursor), (3, 2));
        assert!(stack.actions.iter().all(|a| a.layers() == vec![LayerId(1)]));
        // Undoing walks back through the two surviving steps below the
        // cursor, redo returns the surviving one above it.
        assert_eq!(stack.undo().unwrap().description(), "Created 1 node");
        assert_eq!(stack.undo().unwrap().description(), "Moved 1 node");
        assert!(stack.undo().is_none());
        stack.redo();
        stack.redo();
        assert_eq!(stack.redo().unwrap().description(), "Moved 1 node");
        assert!(stack.redo().is_none());
    }

    #[test]
    fn remove_layer_drops_whole_steps_spanning_layers() {
        let mut stack = UndoStack::default();
        stack.push(UndoableAction::move_nodes(
            vec![
                (LayerId(1), vec![(1, (0.0, 0.0), (1.0, 1.0))]),
                (LayerId(2), vec![(2, (0.0, 0.0), (1.0, 1.0))]),
            ],
            None,
        ));
        stack.push(UndoableAction::Batch {
            label: "mixed".into(),
            actions: vec![create_in(1, -1), create_in(3, -2)],
        });
        stack.push(UndoableAction::Batch {
            label: "only one".into(),
            actions: vec![create_in(1, -3), move_in(1, 5)],
        });
        // A layer with an empty entry still counts as touched.
        stack.push(UndoableAction::move_nodes(
            vec![
                (LayerId(1), vec![(9, (0.0, 0.0), (1.0, 1.0))]),
                (LayerId(2), vec![]),
            ],
            None,
        ));
        assert_eq!(stack.remove_layer(LayerId(2)), 2);
        assert_eq!(stack.remove_layer(LayerId(3)), 1);
        assert_eq!(stack.actions.len(), 1);
        assert_eq!(stack.actions[0].description(), "only one");
        assert_eq!(stack.cursor, 1);
    }

    #[test]
    fn remove_layer_with_nothing_to_remove_changes_nothing() {
        let mut stack = UndoStack::default();
        stack.push(move_in(1, 1));
        stack.push(move_in(1, 2));
        stack.undo();
        assert_eq!(stack.remove_layer(LayerId(7)), 0);
        assert_eq!((stack.actions.len(), stack.cursor), (2, 1));
        let mut empty = UndoStack::default();
        assert_eq!(empty.remove_layer(LayerId(1)), 0);
        assert_eq!(empty.cursor, 0);
    }

    #[test]
    fn description_singular_and_plural() {
        let one = move_one(1, (0.0, 0.0), (1.0, 1.0));
        assert_eq!(one.description(), "Moved 1 node");

        let two = UndoableAction::move_nodes(
            vec![(
                LayerId(1),
                vec![(1, (0.0, 0.0), (1.0, 1.0)), (2, (0.0, 0.0), (1.0, 1.0))],
            )],
            None,
        );
        assert_eq!(two.description(), "Moved 2 nodes");
    }

    #[test]
    fn undo_redo_at_empty_stack_is_none() {
        let mut stack = UndoStack::default();
        assert!(stack.undo().is_none());
        assert!(stack.redo().is_none());
    }

    #[test]
    fn push_then_undo_then_redo_round_trips() {
        let mut stack = UndoStack::default();
        stack.push(move_one(1, (0.0, 0.0), (1.0, 1.0)));

        assert!(stack.redo().is_none(), "nothing to redo right after a push");

        let undone = stack.undo().expect("should have one action to undo");
        assert_eq!(undone.description(), "Moved 1 node");
        assert!(stack.undo().is_none(), "only one action was pushed");

        let redone = stack.redo().expect("should be able to redo after undo");
        assert_eq!(redone.description(), "Moved 1 node");
        assert!(
            stack.redo().is_none(),
            "back at the tip, nothing left to redo"
        );
    }

    #[test]
    fn push_after_undo_discards_redo_branch() {
        let mut stack = UndoStack::default();
        stack.push(move_one(1, (0.0, 0.0), (1.0, 1.0)));
        stack.push(move_one(2, (0.0, 0.0), (2.0, 2.0)));

        stack.undo(); // cursor now points at action 2 as redo-able
        stack.push(move_one(3, (0.0, 0.0), (3.0, 3.0))); // discards action 2

        // Only actions 1 and 3 remain; redo should have nothing left.
        assert!(stack.redo().is_none());
        let undone_3 = stack.undo().unwrap();
        assert_eq!(undone_3.description(), "Moved 1 node");
        let undone_1 = stack.undo().unwrap();
        assert_eq!(undone_1.description(), "Moved 1 node");
        assert!(stack.undo().is_none());
    }

    fn tag_change(
        feature: osm_gpui::selection::FeatureRef,
        key: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> (
        osm_gpui::selection::FeatureRef,
        String,
        Option<String>,
        Option<String>,
    ) {
        (
            feature,
            key.to_string(),
            before.map(|s| s.to_string()),
            after.map(|s| s.to_string()),
        )
    }

    #[test]
    fn set_tags_description_singular_and_plural() {
        use osm_gpui::selection::{FeatureKind, FeatureRef};
        let f = FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Node,
            id: 1,
        };

        let one = UndoableAction::set_tags(vec![tag_change(f, "highway", None, Some("residential"))]);
        assert_eq!(one.description(), "Changed 1 tag");

        let two = UndoableAction::set_tags(vec![
            tag_change(f, "highway", None, Some("residential")),
            tag_change(f, "surface", None, Some("paved")),
        ]);
        assert_eq!(two.description(), "Changed 2 tags");
    }

    #[test]
    fn create_node_description() {
        let action = UndoableAction::create_node(LayerId(1), -1, 40.0, -74.0);
        assert_eq!(action.description(), "Created 1 node");
    }

    #[test]
    fn create_node_undo_redo_round_trips() {
        let mut stack = UndoStack::default();
        stack.push(UndoableAction::create_node(LayerId(1), -1, 40.0, -74.0));

        let undone = stack.undo().expect("should have one action to undo");
        assert_eq!(undone.description(), "Created 1 node");
        assert!(stack.undo().is_none());

        let redone = stack.redo().expect("should be able to redo after undo");
        assert_eq!(redone.description(), "Created 1 node");
        assert!(stack.redo().is_none());
    }

    fn node_snapshot(id: i64) -> osm_gpui::selection::DeletedFeatureSnapshot {
        osm_gpui::selection::DeletedFeatureSnapshot {
            kind: osm_gpui::selection::FeatureKind::Node,
            id,
            tags: vec![("amenity".to_string(), "cafe".to_string())],
            way_nodes: Vec::new(),
            node_lat_lon: Some((40.0, -74.0)),
            version: 1,
        }
    }

    fn way_snapshot(id: i64) -> osm_gpui::selection::DeletedFeatureSnapshot {
        osm_gpui::selection::DeletedFeatureSnapshot {
            kind: osm_gpui::selection::FeatureKind::Way,
            id,
            tags: vec![("highway".to_string(), "residential".to_string())],
            way_nodes: vec![1, 2],
            node_lat_lon: None,
            version: 1,
        }
    }

    #[test]
    fn delete_feature_description_node_vs_way() {
        let node_action = UndoableAction::delete_feature(LayerId(1), node_snapshot(1));
        assert_eq!(node_action.description(), "Deleted 1 node");

        let way_action = UndoableAction::delete_feature(LayerId(1), way_snapshot(10));
        assert_eq!(way_action.description(), "Deleted 1 way");
    }

    #[test]
    fn create_way_description() {
        let action = UndoableAction::create_way(LayerId(1), way_snapshot(-1));
        assert_eq!(action.description(), "Created 1 way");
    }

    #[test]
    fn delete_feature_undo_redo_round_trips() {
        let mut stack = UndoStack::default();
        stack.push(UndoableAction::delete_feature(LayerId(1), way_snapshot(10)));

        let undone = stack.undo().expect("should have one action to undo");
        assert_eq!(undone.description(), "Deleted 1 way");
        assert!(stack.undo().is_none());

        let redone = stack.redo().expect("should be able to redo after undo");
        assert_eq!(redone.description(), "Deleted 1 way");
        assert!(stack.redo().is_none());
    }

    fn put_node(id: i64) -> EditOp {
        EditOp::PutFeature {
            snapshot: osm_gpui::selection::DeletedFeatureSnapshot {
                kind: FeatureKind::Node,
                id,
                tags: Vec::new(),
                way_nodes: Vec::new(),
                node_lat_lon: Some((0.0, 0.0)),
                version: 0,
            },
        }
    }

    #[test]
    fn drawing_labels_describe_the_step() {
        for (label, text) in [
            (EditLabel::CreatedBuilding, "Created a building"),
            (EditLabel::ExtrudedWay, "Extruded a way"),
            (EditLabel::ExtendedWay, "Extended a way"),
            (EditLabel::CreatedArc, "Created an arc"),
        ] {
            let action = UndoableAction::edit(LayerId(1), label, vec![put_node(-1)]);
            assert_eq!(action.description(), text);
        }
    }

    #[test]
    fn edit_undo_redo_round_trips_through_the_stack() {
        let mut stack = UndoStack::default();
        stack.push(UndoableAction::edit(
            LayerId(1),
            EditLabel::ExtendedWay,
            vec![put_node(-11)],
        ));

        let undone = stack.undo().expect("should have one action to undo");
        assert_eq!(undone.description(), "Extended a way");
        assert!(stack.undo().is_none());

        let redone = stack.redo().expect("should be able to redo after undo");
        assert_eq!(redone.description(), "Extended a way");
        assert!(stack.redo().is_none());
    }

    fn fref(kind: FeatureKind, id: i64) -> FeatureRef {
        FeatureRef {
            layer_id: LayerId(1),
            kind,
            id,
        }
    }

    #[test]
    fn a_paste_names_everything_it_created_and_its_layer() {
        use FeatureKind::{Node, Relation, Way};
        let layer = LayerId(4);
        let node = |id: i64| osm_gpui::selection::DeletedFeatureSnapshot {
            kind: Node,
            id,
            tags: Vec::new(),
            way_nodes: Vec::new(),
            node_lat_lon: Some((0.0, 0.0)),
            version: 0,
        };
        let paste = UndoableAction::PasteFeatures {
            pasted: osm_gpui::feature_clipboard::PastedFeatures {
                layer,
                nodes: vec![node(-1), node(-2)],
                ways: vec![way_snapshot(-3)],
                relations: vec![osm_gpui::selection::DeletedRelationSnapshot {
                    relation: osm_gpui::osm::OsmRelation {
                        id: -4,
                        members: Vec::new(),
                        version: 0,
                        tags: Default::default(),
                    },
                    index: 0,
                }],
                selection: vec![FeatureRef {
                    layer_id: layer,
                    kind: Way,
                    id: -3,
                }],
                previous_selection: Vec::new(),
                dropped_members: 0,
                duplicate: false,
            },
        };
        let on = |kind, id| FeatureRef {
            layer_id: layer,
            kind,
            id,
        };
        assert_eq!(
            paste.affected_features(),
            vec![on(Node, -1), on(Node, -2), on(Way, -3), on(Relation, -4)]
        );
        assert_eq!(paste.layers(), vec![layer]);
        assert_eq!(paste.description(), "Pasted features");

        // Pruning by feature or by layer finds it, alone or inside a batch.
        let mut stack = UndoStack::default();
        stack.push(paste.clone());
        stack.push(UndoableAction::Batch {
            label: "b".into(),
            actions: vec![paste],
        });
        assert_eq!(stack.remove_touching(&[on(Node, -2)]), 2);

        let mut stack = UndoStack::default();
        stack.push(move_one(1, (0.0, 0.0), (1.0, 1.0)));
        stack.push(UndoableAction::PasteFeatures {
            pasted: osm_gpui::feature_clipboard::PastedFeatures {
                layer,
                nodes: Vec::new(),
                ways: Vec::new(),
                relations: Vec::new(),
                selection: Vec::new(),
                previous_selection: Vec::new(),
                dropped_members: 0,
                duplicate: true,
            },
        });
        assert_eq!(stack.actions[1].description(), "Duplicated features");
        assert_eq!(stack.remove_layer(layer), 1);
        assert_eq!(stack.actions.len(), 1);
    }

    #[test]
    fn affected_features_per_variant_family() {
        use FeatureKind::{Node, Relation, Way};
        let layer = LayerId(1);

        assert_eq!(
            move_one(3, (0.0, 0.0), (1.0, 1.0)).affected_features(),
            vec![fref(Node, 3)]
        );

        let tags = UndoableAction::set_tags(vec![
            (fref(Way, 5), "a".into(), None, Some("b".into())),
            (fref(Way, 5), "c".into(), None, Some("d".into())),
            (fref(Node, 6), "a".into(), None, Some("b".into())),
        ]);
        assert_eq!(tags.affected_features(), vec![fref(Way, 5), fref(Node, 6)]);

        let create_node = UndoableAction::create_node(layer, -1, 0.0, 0.0);
        assert_eq!(create_node.affected_features(), vec![fref(Node, -1)]);

        let delete = UndoableAction::delete_feature(layer, way_snapshot(9));
        assert_eq!(delete.affected_features(), vec![fref(Way, 9)]);

        let reverse = UndoableAction::reverse_ways(layer, vec![1, 2]);
        assert_eq!(
            reverse.affected_features(),
            vec![fref(Way, 1), fref(Way, 2)]
        );

        let set_nodes = UndoableAction::set_way_nodes(layer, 4, vec![], vec![]);
        assert_eq!(set_nodes.affected_features(), vec![fref(Way, 4)]);

        let members = UndoableAction::set_relation_members(layer, 900, vec![], vec![]);
        assert_eq!(members.affected_features(), vec![fref(Relation, 900)]);

        // An `Edit` names what its ops touch, in order, unless it says what
        // to select.
        let ops = vec![
            put_node(-5),
            EditOp::SetWayNodes {
                id: 8,
                before: vec![],
                after: vec![-5],
            },
            EditOp::PutFeature {
                snapshot: osm_gpui::selection::DeletedFeatureSnapshot {
                    kind: Way,
                    id: -40,
                    tags: Vec::new(),
                    way_nodes: vec![-6, -5],
                    node_lat_lon: None,
                    version: 0,
                },
            },
        ];
        let derived = UndoableAction::edit(layer, EditLabel::SnappedNode, ops.clone());
        assert_eq!(
            derived.affected_features(),
            vec![fref(Node, -5), fref(Way, 8), fref(Way, -40)]
        );
        let chosen = UndoableAction::edit(layer, EditLabel::SnappedNode, ops)
            .selecting(vec![fref(Way, -40)]);
        assert_eq!(chosen.affected_features(), vec![fref(Way, -40)]);
        assert_eq!(chosen.layers(), vec![layer]);
    }

    #[test]
    fn extrude_segment_batch_affects_the_way_and_its_nodes() {
        use FeatureKind::{Node, Way};
        let layer = LayerId(1);
        let batch = UndoableAction::Batch {
            label: "Extruded a segment".into(),
            actions: vec![
                UndoableAction::create_node(layer, -1, 0.0, 0.0),
                UndoableAction::set_way_nodes(layer, 4, vec![1, 2], vec![1, -1, 2]),
                move_one(2, (0.0, 0.0), (1.0, 1.0)),
            ],
        };
        assert_eq!(
            batch.affected_features(),
            vec![fref(Node, -1), fref(Way, 4), fref(Node, 2)]
        );
    }

    #[test]
    fn batch_affected_features_is_the_deduplicated_union() {
        use FeatureKind::{Node, Way};
        let batch = UndoableAction::Batch {
            label: "x".into(),
            actions: vec![
                UndoableAction::reverse_ways(LayerId(1), vec![1]),
                move_one(3, (0.0, 0.0), (1.0, 1.0)),
                UndoableAction::reverse_ways(LayerId(1), vec![1, 2]),
            ],
        };
        assert_eq!(
            batch.affected_features(),
            vec![fref(Way, 1), fref(Node, 3), fref(Way, 2)]
        );
    }
}
