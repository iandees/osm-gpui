//! Undo/redo model for committed data mutations.

use osm_gpui::layers::LayerId;

/// Per layer: node id -> (before (lat, lon), after (lat, lon)).
pub(crate) type NodeMoveUndoEntries = Vec<(LayerId, Vec<(i64, (f64, f64), (f64, f64))>)>;

/// A single reversible data mutation, recorded on the global undo stack.
/// Only one kind exists today (produced by committing a drag-to-move), but
/// the enum leaves room for future mutation kinds (tag edits, deletes, ...)
/// without restructuring the stack.
#[derive(Clone)]
pub(crate) enum UndoableAction {
    MoveNodes {
        per_layer: NodeMoveUndoEntries,
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
}

impl UndoableAction {
    /// Human-readable label for the history list, e.g. "Moved 3 nodes".
    pub(crate) fn description(&self) -> String {
        match self {
            UndoableAction::MoveNodes { per_layer } => {
                let count: usize = per_layer.iter().map(|(_, entries)| entries.len()).sum();
                if count == 1 {
                    "Moved 1 node".to_string()
                } else {
                    format!("Moved {} nodes", count)
                }
            }
            UndoableAction::SetTags { entries } => {
                if entries.len() == 1 {
                    "Changed 1 tag".to_string()
                } else {
                    format!("Changed {} tags", entries.len())
                }
            }
            UndoableAction::CreateNode { .. } => "Created 1 node".to_string(),
            UndoableAction::DeleteFeature { snapshot, .. } => match snapshot.kind {
                osm_gpui::selection::FeatureKind::Node => "Deleted 1 node".to_string(),
                osm_gpui::selection::FeatureKind::Way => "Deleted 1 way".to_string(),
            },
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
}

impl UndoStack {
    pub(crate) fn push(&mut self, action: UndoableAction) {
        self.actions.truncate(self.cursor);
        self.actions.push(action);
        self.cursor = self.actions.len();
    }

    /// Returns the action to invert, and moves the cursor back. `None` if
    /// there's nothing to undo.
    pub(crate) fn undo(&mut self) -> Option<UndoableAction> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
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
        Some(action)
    }
}

#[cfg(test)]
mod undo_stack_tests {
    use super::{LayerId, UndoStack, UndoableAction};

    fn move_one(id: i64, before: (f64, f64), after: (f64, f64)) -> UndoableAction {
        UndoableAction::MoveNodes {
            per_layer: vec![(LayerId(1), vec![(id, before, after)])],
        }
    }

    #[test]
    fn description_singular_and_plural() {
        let one = move_one(1, (0.0, 0.0), (1.0, 1.0));
        assert_eq!(one.description(), "Moved 1 node");

        let two = UndoableAction::MoveNodes {
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
            entries: vec![tag_change(f.clone(), "highway", None, Some("residential"))],
        };
        assert_eq!(one.description(), "Changed 1 tag");

        let two = UndoableAction::SetTags {
            entries: vec![
                tag_change(f.clone(), "highway", None, Some("residential")),
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
        }
    }

    fn way_snapshot(id: i64) -> osm_gpui::selection::DeletedFeatureSnapshot {
        osm_gpui::selection::DeletedFeatureSnapshot {
            kind: osm_gpui::selection::FeatureKind::Way,
            id,
            tags: vec![("highway".to_string(), "residential".to_string())],
            way_nodes: vec![1, 2],
            node_lat_lon: None,
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
}
