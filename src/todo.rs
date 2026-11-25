//! The to-do list: features set aside to work through one after another,
//! like JOSM's TODO list plugin.
//!
//! Features are added from the selection (whether picked on the map or found
//! with Edit > Find), visited from the list, and marked done, which takes
//! them off it and moves on to the next. The list is only a list of
//! references: it never edits anything, has no undo, and lasts for the
//! session.

use crate::layers::LayerId;
use crate::osm_upload::UploadResult;
use crate::selection::{FeatureKind, FeatureRef};

/// An empty list, for code that needs a `&TodoList` and has none.
pub static EMPTY: TodoList = TodoList { items: Vec::new() };

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TodoList {
    items: Vec<FeatureRef>,
}

impl TodoList {
    pub fn items(&self) -> &[FeatureRef] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn contains(&self, feature: &FeatureRef) -> bool {
        self.items.contains(feature)
    }

    pub fn position(&self, feature: &FeatureRef) -> Option<usize> {
        self.items.iter().position(|f| f == feature)
    }

    /// Append the features not on the list yet, in order. Returns how many
    /// were added.
    pub fn add(&mut self, features: &[FeatureRef]) -> usize {
        let mut added = 0;
        for f in features {
            if !self.contains(f) {
                self.items.push(*f);
                added += 1;
            }
        }
        added
    }

    /// Mark `features` done: take the ones on the list off it. Returns how
    /// many were removed and the entry to visit next: the one that followed
    /// the first removed entry, else the one before it (the end of the list
    /// was done), else none (the list is empty).
    pub fn mark_done(&mut self, features: &[FeatureRef]) -> (usize, Option<FeatureRef>) {
        let Some(first) = self.items.iter().position(|f| features.contains(f)) else {
            return (0, None);
        };
        let before = self.items.len();
        self.items.retain(|f| !features.contains(f));
        let removed = before - self.items.len();
        let next = self.items.get(first).or_else(|| self.items.last()).copied();
        (removed, next)
    }

    /// Remove the entry at `index`, as `mark_done` would.
    pub fn remove_at(&mut self, index: usize) -> Option<FeatureRef> {
        let f = *self.items.get(index)?;
        self.mark_done(&[f]).1
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Drop the entries of layers that are gone.
    pub fn retain_layers(&mut self, exists: impl Fn(LayerId) -> bool) {
        self.items.retain(|f| exists(f.layer_id));
    }

    /// After an upload of `layers`, give entries for objects the server
    /// created the ids it assigned.
    pub fn remap_uploaded(&mut self, layers: &[LayerId], result: &UploadResult) {
        for f in &mut self.items {
            if !layers.contains(&f.layer_id) {
                continue;
            }
            let new_id = match f.kind {
                FeatureKind::Node => result
                    .node_id_remap
                    .get(&crate::ids::NodeId(f.id))
                    .map(|(id, _)| id.get()),
                FeatureKind::Way => result
                    .way_id_remap
                    .get(&crate::ids::WayId(f.id))
                    .map(|(id, _)| id.get()),
                FeatureKind::Relation => result
                    .relation_id_remap
                    .get(&crate::ids::RelationId(f.id))
                    .map(|(id, _)| id.get()),
            };
            if let Some(id) = new_id {
                f.id = id;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{NodeId, WayId};

    fn way(id: i64) -> FeatureRef {
        FeatureRef {
            layer_id: LayerId(1),
            kind: FeatureKind::Way,
            id,
        }
    }

    fn node(layer: u64, id: i64) -> FeatureRef {
        FeatureRef {
            layer_id: LayerId(layer),
            kind: FeatureKind::Node,
            id,
        }
    }

    #[test]
    fn adding_skips_what_is_already_listed_and_keeps_order() {
        let mut l = TodoList::default();
        assert_eq!(l.add(&[way(1), way(2)]), 2);
        assert_eq!(l.add(&[way(2), way(3), way(3)]), 1);
        assert_eq!(l.items(), &[way(1), way(2), way(3)]);
        // Same id, other kind or layer: a different feature.
        assert_eq!(l.add(&[node(1, 1), node(2, 1)]), 2);
        assert_eq!(l.len(), 5);
    }

    #[test]
    fn done_moves_on_to_the_next_entry_or_back_from_the_end() {
        let mut l = TodoList::default();
        l.add(&[way(1), way(2), way(3)]);
        assert_eq!(l.mark_done(&[way(2)]), (1, Some(way(3))));
        assert_eq!(l.mark_done(&[way(3)]), (1, Some(way(1))));
        assert_eq!(l.mark_done(&[way(1)]), (1, None));
        assert!(l.is_empty());
    }

    #[test]
    fn done_with_several_selected_continues_after_the_first() {
        let mut l = TodoList::default();
        l.add(&[way(1), way(2), way(3), way(4)]);
        assert_eq!(l.mark_done(&[way(3), way(2), way(9)]), (2, Some(way(4))));
        assert_eq!(l.items(), &[way(1), way(4)]);
    }

    #[test]
    fn done_with_nothing_listed_changes_nothing() {
        let mut l = TodoList::default();
        l.add(&[way(1)]);
        assert_eq!(l.mark_done(&[way(7)]), (0, None));
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn remove_at_returns_what_to_visit_next() {
        let mut l = TodoList::default();
        l.add(&[way(1), way(2)]);
        assert_eq!(l.remove_at(0), Some(way(2)));
        assert_eq!(l.remove_at(5), None);
    }

    #[test]
    fn entries_of_removed_layers_go() {
        let mut l = TodoList::default();
        l.add(&[node(1, 1), node(2, 1)]);
        l.retain_layers(|id| id == LayerId(2));
        assert_eq!(l.items(), &[node(2, 1)]);
    }

    #[test]
    fn created_objects_get_their_new_ids_after_an_upload() {
        let mut l = TodoList::default();
        l.add(&[node(1, -1), way(-5), way(40), node(2, -1)]);
        let mut result = UploadResult::default();
        result.node_id_remap.insert(NodeId(-1), (NodeId(9001), 1));
        result.way_id_remap.insert(WayId(-5), (WayId(777), 1));
        result.way_id_remap.insert(WayId(40), (WayId(40), 3));
        l.remap_uploaded(&[LayerId(1)], &result);
        assert_eq!(l.items(), &[node(1, 9001), way(777), way(40), node(2, -1)]);
    }
}
