//! Undo/redo model for committed data mutations.

use osm_gpui::layers::LayerId;

/// A single node move: its id, (lat, lon) before, and (lat, lon) after.
pub(crate) type NodeMoveUndoEntry = (i64, (f64, f64), (f64, f64));

/// Per layer: node id -> (before (lat, lon), after (lat, lon)).
pub(crate) type NodeMoveUndoEntries = Vec<(LayerId, Vec<NodeMoveUndoEntry>)>;

/// A single reversible data mutation, recorded on the global undo stack.
/// Only one kind exists today (produced by committing a drag-to-move), but
/// the enum leaves room for future mutation kinds (tag edits, deletes, ...)
/// without restructuring the stack.
#[derive(Clone)]
pub(crate) enum UndoableAction {
    MoveNodes {
        per_layer: NodeMoveUndoEntries,
        /// What to select after undoing or redoing this step, when it should
        /// differ from the moved nodes (a dragged way stays the selection).
        select: Option<Vec<osm_gpui::selection::FeatureRef>>,
    },
    /// One entry per affected feature: which key, and its value before/
    /// after (`None` = key was/becomes absent). A key rename is modeled as
    /// two entries for the same feature — remove-old plus add-new — so
    /// this stays a single uniform apply loop.
    SetTags {
        entries: Vec<(
            osm_gpui::selection::FeatureRef,
            String,
            Option<String>,
            Option<String>,
        )>,
    },
    /// A node created at `(lat, lon)` on `layer`. Undo deletes it (via
    /// `delete_feature`); redo recreates it at the same id (via
    /// `create_node`'s explicit-id form), so redo reproduces the exact same
    /// node rather than allocating a fresh one.
    CreateNode {
        layer: LayerId,
        id: i64,
        lat: f64,
        lon: f64,
    },
    /// A node or way deleted from `layer`. Undo restores it (via
    /// `restore_feature`); redo deletes it again (via `delete_feature`).
    DeleteFeature {
        layer: LayerId,
        snapshot: osm_gpui::selection::DeletedFeatureSnapshot,
    },
    /// A way created with a known id (Split Way's new half). The mirror of
    /// `DeleteFeature`: undo deletes it, redo restores it from the snapshot
    /// under the same id, so later steps that refer to it stay valid.
    CreateWay {
        layer: LayerId,
        snapshot: osm_gpui::selection::DeletedFeatureSnapshot,
    },
    /// Add mode's 2nd+ click: a node (new or pre-existing) appended to a
    /// way (creating the way first if `way_created`). The two booleans are
    /// independent: `way_created` says whether this click created `way_id`
    /// (vs. extending an already-existing way), and `node_created` says
    /// whether this click created `node_id` (vs. connecting to a node the
    /// user already had, via the "connect to existing node" gesture). Undo
    /// always detaches `node_id` from the way — either by deleting the
    /// whole way (if `way_created`) or by removing just that node from the
    /// way's node list (if not) — and only deletes `node_id` itself when
    /// `node_created` is true, since a pre-existing node may be shared with
    /// other ways or carry its own tags.
    ExtendWay {
        layer: LayerId,
        way_id: i64,
        node_id: i64,
        way_created: bool,
        node_created: bool,
        /// True if the node was prepended (continuing a way from its first
        /// node) rather than appended.
        at_start: bool,
    },
    /// Building mode's 3rd click: 4 new nodes + one closed `building=yes`
    /// way, committed atomically. Undo deletes the way then all 4 nodes.
    CreateBuilding {
        layer: LayerId,
        way_id: i64,
        node_ids: [i64; 4],
    },
    /// Extrude mode's drag commit: 2 new nodes + one closed, untagged way
    /// off an existing segment. The segment's own 2 nodes are untouched.
    /// Undo deletes the way then the 2 new nodes.
    ExtrudeWay {
        layer: LayerId,
        way_id: i64,
        new_node_ids: [i64; 2],
    },
    /// Extrude mode's double-click: one new node spliced into an existing
    /// way at `index`. Undo removes it from the way, then deletes it.
    InsertNodeIntoWay {
        layer: LayerId,
        way_id: i64,
        index: usize,
        node_id: i64,
    },
    /// Add mode's 2nd+ click when it snaps onto an existing way: a node
    /// spliced into `snap_way_id` at `snap_index` (like `InsertNodeIntoWay`),
    /// then that same node folded into the way being drawn — `way_id`,
    /// created fresh if `way_created`, otherwise extended (like `ExtendWay`
    /// with `node_created: true`). Undo reverses both, in the order that
    /// matches how they were applied: detach from the drawn way first
    /// (deleting it if `way_created`, without deleting the node — it is
    /// still referenced by `snap_way_id`), then remove the node from
    /// `snap_way_id` and delete it.
    SnapExtendWay {
        layer: LayerId,
        way_id: i64,
        way_created: bool,
        snap_way_id: i64,
        snap_index: usize,
        node_id: i64,
        /// True if the node was prepended to the drawn way, not appended.
        at_start: bool,
    },
    /// Selection action "Reverse": the listed ways had their node order
    /// reversed. Reversing is its own inverse, so undo and redo both just
    /// reverse them again.
    ReverseWays { layer: LayerId, way_ids: Vec<i64> },
    /// A way's whole node list replaced (join, separate): undo restores
    /// `before`, redo reapplies `after`. The nodes in either list must exist
    /// whenever that direction is applied, so callers order the surrounding
    /// `CreateNode`/`DeleteFeature` entries of a `Batch` accordingly.
    SetWayNodes {
        layer: LayerId,
        way_id: i64,
        before: Vec<i64>,
        after: Vec<i64>,
    },
    /// A relation's whole member list replaced (split, merge, join and
    /// delete keeping their relations intact): undo restores `before`, redo
    /// reapplies `after`. Members may name ways or nodes that only exist
    /// while a neighbouring `Batch` entry is applied, which is fine: the
    /// layer stores member refs without resolving them.
    SetRelationMembers {
        layer: LayerId,
        relation_id: i64,
        before: Vec<osm_gpui::osm::OsmMember>,
        after: Vec<osm_gpui::osm::OsmMember>,
    },
    /// A relation created with a known id. Undo deletes it (the layer
    /// refuses while another relation still has it as a member, so callers
    /// order the surrounding `Batch` entries accordingly); redo restores it
    /// from the snapshot under the same id.
    CreateRelation {
        layer: LayerId,
        snapshot: osm_gpui::selection::DeletedRelationSnapshot,
    },
    /// A relation deleted from `layer`. Undo restores it (same id, version
    /// and position); redo deletes it again.
    DeleteRelation {
        layer: LayerId,
        snapshot: osm_gpui::selection::DeletedRelationSnapshot,
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
    /// The features this step touched, whichever direction it is applied in.
    /// After an undo or redo the caller selects the ones that still exist
    /// (an undone create or a redone delete names features that are gone, so
    /// those drop out when the caller prunes). Pure: it only reads the
    /// recorded ids, never the layer. The result has no duplicates.
    pub(crate) fn affected_features(&self) -> Vec<osm_gpui::selection::FeatureRef> {
        use osm_gpui::selection::{FeatureKind, FeatureRef};
        let node = |layer: LayerId, id: i64| FeatureRef {
            layer_id: layer,
            kind: FeatureKind::Node,
            id,
        };
        let way = |layer: LayerId, id: i64| FeatureRef {
            layer_id: layer,
            kind: FeatureKind::Way,
            id,
        };
        let mut out: Vec<FeatureRef> = Vec::new();
        match self {
            UndoableAction::MoveNodes { per_layer, select } => {
                if let Some(select) = select {
                    out.extend(select.iter().copied());
                } else {
                    for (layer, entries) in per_layer {
                        out.extend(entries.iter().map(|&(id, _, _)| node(*layer, id)));
                    }
                }
            }
            UndoableAction::SetTags { entries } => {
                out.extend(entries.iter().map(|(feature, ..)| *feature));
            }
            UndoableAction::CreateNode { layer, id, .. } => out.push(node(*layer, *id)),
            UndoableAction::DeleteFeature { layer, snapshot }
            | UndoableAction::CreateWay { layer, snapshot } => out.push(FeatureRef {
                layer_id: *layer,
                kind: snapshot.kind,
                id: snapshot.id,
            }),
            UndoableAction::ExtendWay {
                layer,
                way_id,
                node_id,
                ..
            } => {
                out.push(way(*layer, *way_id));
                out.push(node(*layer, *node_id));
            }
            // The helper nodes of a building / extrusion are an
            // implementation detail; the way is what the user made.
            UndoableAction::CreateBuilding { layer, way_id, .. }
            | UndoableAction::ExtrudeWay { layer, way_id, .. } => out.push(way(*layer, *way_id)),
            UndoableAction::InsertNodeIntoWay {
                layer,
                way_id,
                node_id,
                ..
            } => {
                out.push(way(*layer, *way_id));
                out.push(node(*layer, *node_id));
            }
            UndoableAction::SnapExtendWay {
                layer,
                way_id,
                snap_way_id,
                node_id,
                ..
            } => {
                out.push(way(*layer, *way_id));
                out.push(way(*layer, *snap_way_id));
                out.push(node(*layer, *node_id));
            }
            UndoableAction::ReverseWays { layer, way_ids } => {
                out.extend(way_ids.iter().map(|&id| way(*layer, id)));
            }
            UndoableAction::SetWayNodes { layer, way_id, .. } => out.push(way(*layer, *way_id)),
            UndoableAction::SetRelationMembers {
                layer, relation_id, ..
            } => out.push(FeatureRef {
                layer_id: *layer,
                kind: FeatureKind::Relation,
                id: *relation_id,
            }),
            UndoableAction::CreateRelation { layer, snapshot }
            | UndoableAction::DeleteRelation { layer, snapshot } => out.push(FeatureRef {
                layer_id: *layer,
                kind: FeatureKind::Relation,
                id: snapshot.relation.id,
            }),
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
            UndoableAction::MoveNodes { per_layer, .. } => {
                for (layer, _) in per_layer {
                    add(*layer);
                }
            }
            UndoableAction::SetTags { entries } => {
                for (feature, ..) in entries {
                    add(feature.layer_id);
                }
            }
            UndoableAction::CreateNode { layer, .. }
            | UndoableAction::DeleteFeature { layer, .. }
            | UndoableAction::CreateWay { layer, .. }
            | UndoableAction::ExtendWay { layer, .. }
            | UndoableAction::CreateBuilding { layer, .. }
            | UndoableAction::ExtrudeWay { layer, .. }
            | UndoableAction::InsertNodeIntoWay { layer, .. }
            | UndoableAction::SnapExtendWay { layer, .. }
            | UndoableAction::ReverseWays { layer, .. }
            | UndoableAction::SetWayNodes { layer, .. }
            | UndoableAction::SetRelationMembers { layer, .. }
            | UndoableAction::CreateRelation { layer, .. }
            | UndoableAction::DeleteRelation { layer, .. }
            | UndoableAction::Purge { layer, .. } => add(*layer),
            UndoableAction::PasteFeatures { pasted } => add(pasted.layer),
            UndoableAction::Batch { actions, .. } => {
                for action in actions {
                    action.collect_layers(out);
                }
            }
        }
    }

    /// Human-readable label for the history list, e.g. "Moved 3 nodes".
    pub(crate) fn description(&self) -> String {
        match self {
            UndoableAction::MoveNodes { per_layer, .. } => {
                let count: usize = per_layer.iter().map(|(_, entries)| entries.len()).sum();
                osm_gpui::tr_count!("history.moved_nodes", count).to_string()
            }
            UndoableAction::SetTags { entries } => {
                osm_gpui::tr_count!("history.changed_tags", entries.len()).to_string()
            }
            UndoableAction::CreateNode { .. } => osm_gpui::tr!("history.created_node").to_string(),
            UndoableAction::DeleteFeature { snapshot, .. } => match snapshot.kind {
                osm_gpui::selection::FeatureKind::Node => {
                    osm_gpui::tr!("history.deleted_node").to_string()
                }
                osm_gpui::selection::FeatureKind::Way => {
                    osm_gpui::tr!("history.deleted_way").to_string()
                }
                osm_gpui::selection::FeatureKind::Relation => {
                    osm_gpui::tr!("history.deleted_relation").to_string()
                }
            },
            UndoableAction::CreateWay { .. } => osm_gpui::tr!("history.created_way").to_string(),
            UndoableAction::ExtendWay { .. } => osm_gpui::tr!("history.extended_way").to_string(),
            UndoableAction::CreateBuilding { .. } => {
                osm_gpui::tr!("history.created_building").to_string()
            }
            UndoableAction::ExtrudeWay { .. } => osm_gpui::tr!("history.extruded_way").to_string(),
            UndoableAction::InsertNodeIntoWay { .. } => {
                osm_gpui::tr!("history.inserted_node").to_string()
            }
            UndoableAction::SnapExtendWay { .. } => {
                osm_gpui::tr!("history.snapped_node").to_string()
            }
            UndoableAction::ReverseWays { way_ids, .. } => {
                osm_gpui::tr_count!("action.reverse.done", way_ids.len()).to_string()
            }
            UndoableAction::SetWayNodes { .. } => {
                osm_gpui::tr!("history.changed_way_nodes").to_string()
            }
            UndoableAction::SetRelationMembers { .. } => {
                osm_gpui::tr!("history.changed_relation_members").to_string()
            }
            UndoableAction::CreateRelation { .. } => {
                osm_gpui::tr!("history.created_relation").to_string()
            }
            UndoableAction::DeleteRelation { .. } => {
                osm_gpui::tr!("history.deleted_relation").to_string()
            }
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
    use super::{LayerId, UndoStack, UndoableAction};
    use osm_gpui::selection::{FeatureKind, FeatureRef};

    fn move_one(id: i64, before: (f64, f64), after: (f64, f64)) -> UndoableAction {
        UndoableAction::MoveNodes {
            select: None,
            per_layer: vec![(LayerId(1), vec![(id, before, after)])],
        }
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
        UndoableAction::MoveNodes {
            select: None,
            per_layer: vec![(LayerId(layer), vec![(id, (0.0, 0.0), (1.0, 1.0))])],
        }
    }

    fn create_in(layer: u64, id: i64) -> UndoableAction {
        UndoableAction::CreateNode {
            layer: LayerId(layer),
            id,
            lat: 0.0,
            lon: 0.0,
        }
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
        stack.push(UndoableAction::MoveNodes {
            select: None,
            per_layer: vec![
                (LayerId(1), vec![(1, (0.0, 0.0), (1.0, 1.0))]),
                (LayerId(2), vec![(2, (0.0, 0.0), (1.0, 1.0))]),
            ],
        });
        stack.push(UndoableAction::Batch {
            label: "mixed".into(),
            actions: vec![create_in(1, -1), create_in(3, -2)],
        });
        stack.push(UndoableAction::Batch {
            label: "only one".into(),
            actions: vec![create_in(1, -3), move_in(1, 5)],
        });
        // A layer with an empty entry still counts as touched.
        stack.push(UndoableAction::MoveNodes {
            select: None,
            per_layer: vec![
                (LayerId(1), vec![(9, (0.0, 0.0), (1.0, 1.0))]),
                (LayerId(2), vec![]),
            ],
        });
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

        let two = UndoableAction::MoveNodes {
            select: None,
            per_layer: vec![(
                LayerId(1),
                vec![(1, (0.0, 0.0), (1.0, 1.0)), (2, (0.0, 0.0), (1.0, 1.0))],
            )],
        };
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

        let one = UndoableAction::SetTags {
            entries: vec![tag_change(f, "highway", None, Some("residential"))],
        };
        assert_eq!(one.description(), "Changed 1 tag");

        let two = UndoableAction::SetTags {
            entries: vec![
                tag_change(f, "highway", None, Some("residential")),
                tag_change(f, "surface", None, Some("paved")),
            ],
        };
        assert_eq!(two.description(), "Changed 2 tags");
    }

    #[test]
    fn create_node_description() {
        let action = UndoableAction::CreateNode {
            layer: LayerId(1),
            id: -1,
            lat: 40.0,
            lon: -74.0,
        };
        assert_eq!(action.description(), "Created 1 node");
    }

    #[test]
    fn create_node_undo_redo_round_trips() {
        let mut stack = UndoStack::default();
        stack.push(UndoableAction::CreateNode {
            layer: LayerId(1),
            id: -1,
            lat: 40.0,
            lon: -74.0,
        });

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
        let node_action = UndoableAction::DeleteFeature {
            layer: LayerId(1),
            snapshot: node_snapshot(1),
        };
        assert_eq!(node_action.description(), "Deleted 1 node");

        let way_action = UndoableAction::DeleteFeature {
            layer: LayerId(1),
            snapshot: way_snapshot(10),
        };
        assert_eq!(way_action.description(), "Deleted 1 way");
    }

    #[test]
    fn create_way_description() {
        let action = UndoableAction::CreateWay {
            layer: LayerId(1),
            snapshot: way_snapshot(-1),
        };
        assert_eq!(action.description(), "Created 1 way");
    }

    #[test]
    fn delete_feature_undo_redo_round_trips() {
        let mut stack = UndoStack::default();
        stack.push(UndoableAction::DeleteFeature {
            layer: LayerId(1),
            snapshot: way_snapshot(10),
        });

        let undone = stack.undo().expect("should have one action to undo");
        assert_eq!(undone.description(), "Deleted 1 way");
        assert!(stack.undo().is_none());

        let redone = stack.redo().expect("should be able to redo after undo");
        assert_eq!(redone.description(), "Deleted 1 way");
        assert!(stack.redo().is_none());
    }

    #[test]
    fn create_building_description() {
        let action = UndoableAction::CreateBuilding {
            layer: LayerId(1),
            way_id: -1,
            node_ids: [-1, -2, -3, -4],
        };
        assert_eq!(action.description(), "Created a building");
    }

    #[test]
    fn extrude_way_description() {
        let action = UndoableAction::ExtrudeWay {
            layer: LayerId(1),
            way_id: -1,
            new_node_ids: [-1, -2],
        };
        assert_eq!(action.description(), "Extruded a way");
    }

    fn extend_way(
        way_id: i64,
        node_id: i64,
        way_created: bool,
        node_created: bool,
    ) -> UndoableAction {
        UndoableAction::ExtendWay {
            layer: LayerId(1),
            way_id,
            node_id,
            way_created,
            node_created,
            at_start: false,
        }
    }

    #[test]
    fn extend_way_description() {
        // `description()` doesn't distinguish the four (way_created,
        // node_created) combinations — it's the same human-readable label
        // regardless.
        for way_created in [false, true] {
            for node_created in [false, true] {
                let action = extend_way(-1, -2, way_created, node_created);
                assert_eq!(action.description(), "Extended a way");
            }
        }
    }

    #[test]
    fn extend_way_undo_redo_round_trips_new_way_new_node() {
        // The "continue clicking" path's first extension: a brand-new way
        // and a brand-new node (way_created: true, node_created: true).
        let mut stack = UndoStack::default();
        stack.push(extend_way(-10, -11, true, true));

        let undone = stack.undo().expect("should have one action to undo");
        assert_eq!(undone.description(), "Extended a way");
        assert!(stack.undo().is_none());

        let redone = stack.redo().expect("should be able to redo after undo");
        assert_eq!(redone.description(), "Extended a way");
        assert!(stack.redo().is_none());
    }

    #[test]
    fn extend_way_undo_redo_round_trips_existing_way_existing_node() {
        // Extending an already-existing way by connecting to a pre-existing
        // node (way_created: false, node_created: false) — the case the
        // data-integrity fix targets: undo must detach the node from the
        // way but never delete it.
        let mut stack = UndoStack::default();
        stack.push(extend_way(10, 5, false, false));

        let undone = stack.undo().expect("should have one action to undo");
        assert_eq!(undone.description(), "Extended a way");
        if let UndoableAction::ExtendWay {
            way_created,
            node_created,
            ..
        } = undone
        {
            assert!(!way_created);
            assert!(!node_created);
        } else {
            panic!("expected ExtendWay");
        }
        assert!(stack.undo().is_none());

        let redone = stack.redo().expect("should be able to redo after undo");
        assert_eq!(redone.description(), "Extended a way");
        assert!(stack.redo().is_none());
    }

    #[test]
    fn extend_way_undo_redo_round_trips_new_way_existing_node() {
        // A brand-new 2-node way whose *second* node was an existing node
        // the user clicked to connect (way_created: true, node_created:
        // false) — undo must delete the new way but must not delete the
        // pre-existing node.
        let mut stack = UndoStack::default();
        stack.push(extend_way(-20, 7, true, false));

        let undone = stack.undo().expect("should have one action to undo");
        if let UndoableAction::ExtendWay {
            way_created,
            node_created,
            node_id,
            ..
        } = undone
        {
            assert!(way_created);
            assert!(!node_created);
            assert_eq!(node_id, 7);
        } else {
            panic!("expected ExtendWay");
        }
        assert!(stack.undo().is_none());
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

        let tags = UndoableAction::SetTags {
            entries: vec![
                (fref(Way, 5), "a".into(), None, Some("b".into())),
                (fref(Way, 5), "c".into(), None, Some("d".into())),
                (fref(Node, 6), "a".into(), None, Some("b".into())),
            ],
        };
        assert_eq!(tags.affected_features(), vec![fref(Way, 5), fref(Node, 6)]);

        let create_node = UndoableAction::CreateNode {
            layer,
            id: -1,
            lat: 0.0,
            lon: 0.0,
        };
        assert_eq!(create_node.affected_features(), vec![fref(Node, -1)]);

        let delete = UndoableAction::DeleteFeature {
            layer,
            snapshot: way_snapshot(9),
        };
        assert_eq!(delete.affected_features(), vec![fref(Way, 9)]);

        let reverse = UndoableAction::ReverseWays {
            layer,
            way_ids: vec![1, 2],
        };
        assert_eq!(
            reverse.affected_features(),
            vec![fref(Way, 1), fref(Way, 2)]
        );

        let set_nodes = UndoableAction::SetWayNodes {
            layer,
            way_id: 4,
            before: vec![],
            after: vec![],
        };
        assert_eq!(set_nodes.affected_features(), vec![fref(Way, 4)]);

        let members = UndoableAction::SetRelationMembers {
            layer,
            relation_id: 900,
            before: vec![],
            after: vec![],
        };
        assert_eq!(members.affected_features(), vec![fref(Relation, 900)]);

        let extend = extend_way(-20, 7, true, true);
        assert_eq!(
            extend.affected_features(),
            vec![fref(Way, -20), fref(Node, 7)]
        );

        let building = UndoableAction::CreateBuilding {
            layer,
            way_id: -30,
            node_ids: [-1, -2, -3, -4],
        };
        assert_eq!(building.affected_features(), vec![fref(Way, -30)]);

        let snap = UndoableAction::SnapExtendWay {
            layer,
            way_id: -40,
            way_created: true,
            snap_way_id: 8,
            snap_index: 1,
            node_id: -5,
            at_start: false,
        };
        assert_eq!(
            snap.affected_features(),
            vec![fref(Way, -40), fref(Way, 8), fref(Node, -5)]
        );
    }

    #[test]
    fn extrude_segment_batch_affects_the_way_and_its_nodes() {
        use FeatureKind::{Node, Way};
        let layer = LayerId(1);
        let batch = UndoableAction::Batch {
            label: "Extruded a segment".into(),
            actions: vec![
                UndoableAction::CreateNode {
                    layer,
                    id: -1,
                    lat: 0.0,
                    lon: 0.0,
                },
                UndoableAction::SetWayNodes {
                    layer,
                    way_id: 4,
                    before: vec![1, 2],
                    after: vec![1, -1, 2],
                },
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
                UndoableAction::ReverseWays {
                    layer: LayerId(1),
                    way_ids: vec![1],
                },
                move_one(3, (0.0, 0.0), (1.0, 1.0)),
                UndoableAction::ReverseWays {
                    layer: LayerId(1),
                    way_ids: vec![1, 2],
                },
            ],
        };
        assert_eq!(
            batch.affected_features(),
            vec![fref(Way, 1), fref(Node, 3), fref(Way, 2)]
        );
    }
}
