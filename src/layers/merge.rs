//! Merging freshly downloaded server data into a layer that may already
//! hold data — and local edits.
//!
//! A layer keeps two snapshots: `original` (what the server had when each
//! object was last fetched) and `current` (the same plus local edits); the
//! difference between them is what gets uploaded (see `diff`). Merging a
//! download has to keep that difference meaning "my edits" and nothing
//! else, so each incoming object is handled by what the layer already knows
//! about its id:
//!
//! - **Unknown** — added to both snapshots.
//! - **Known and locally untouched** — replaced in both snapshots with the
//!   server's copy, picking up anyone else's changes.
//! - **Locally modified or deleted** — left alone in both snapshots. The
//!   local edit wins for now; if the server's copy has moved on, the upload
//!   will be rejected with a version conflict rather than silently
//!   overwriting either side.
//!
//! Nothing is ever removed: an object absent from the download is just
//! outside the downloaded area.

use crate::osm::{OsmData, OsmRelation};
use std::collections::HashMap;
use std::hash::Hash;

/// What a merge did, for reporting to the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeStats {
    /// Objects the layer didn't have before.
    pub added: usize,
    /// Locally untouched objects replaced by a different server copy.
    pub updated: usize,
    /// Incoming objects skipped because the layer has local edits to them
    /// (a modification or a deletion).
    pub kept_local: usize,
}

impl MergeStats {
    /// A one-line summary, e.g. `"412 new, 3 updated, 1 local edit kept"`.
    pub fn summary(&self) -> String {
        if self.added == 0 && self.updated == 0 && self.kept_local == 0 {
            return "nothing new".to_string();
        }
        let mut parts = Vec::new();
        if self.added > 0 {
            parts.push(format!("{} new", self.added));
        }
        if self.updated > 0 {
            parts.push(format!("{} updated", self.updated));
        }
        match self.kept_local {
            0 => {}
            1 => parts.push("1 local edit kept".to_string()),
            n => parts.push(format!("{n} local edits kept")),
        }
        parts.join(", ")
    }
}

/// Merge one id-keyed element map (nodes or ways) per the module rules.
fn merge_map<K, V>(
    original: &mut HashMap<K, V>,
    current: &mut HashMap<K, V>,
    incoming: HashMap<K, V>,
    stats: &mut MergeStats,
) where
    K: Eq + Hash + Copy,
    V: PartialEq + Clone,
{
    enum Known {
        No,
        UntouchedAndCurrent,
        UntouchedButStale,
        LocallyEdited,
    }

    for (id, element) in incoming {
        let known = match (original.get(&id), current.get(&id)) {
            (None, None) => Known::No,
            (Some(orig), Some(cur)) if orig == cur => {
                if *orig == element {
                    Known::UntouchedAndCurrent
                } else {
                    Known::UntouchedButStale
                }
            }
            // Locally modified, locally deleted, or (not expected for a
            // server id) present only locally: the local state stands.
            _ => Known::LocallyEdited,
        };
        match known {
            Known::No => stats.added += 1,
            Known::UntouchedButStale => stats.updated += 1,
            Known::UntouchedAndCurrent => continue,
            Known::LocallyEdited => {
                stats.kept_local += 1;
                continue;
            }
        }
        original.insert(id, element.clone());
        current.insert(id, element);
    }
}

/// Merge relations, which are read-only in this app (so there are never
/// local edits to protect): add unknown ones and replace known ones.
fn merge_relations(target: &mut Vec<OsmRelation>, incoming: &[OsmRelation]) {
    let index: HashMap<i64, usize> = target.iter().enumerate().map(|(i, r)| (r.id, i)).collect();
    for relation in incoming {
        match index.get(&relation.id) {
            Some(&i) => target[i] = relation.clone(),
            None => target.push(relation.clone()),
        }
    }
}

/// Merge downloaded server data `incoming` into a layer's `original` and
/// `current` snapshots. See the module docs for the rules.
pub fn merge_download(
    original: &mut OsmData,
    current: &mut OsmData,
    incoming: OsmData,
) -> MergeStats {
    let mut stats = MergeStats::default();
    merge_map(
        &mut original.nodes,
        &mut current.nodes,
        incoming.nodes,
        &mut stats,
    );
    merge_map(
        &mut original.ways,
        &mut current.ways,
        incoming.ways,
        &mut stats,
    );
    merge_relations(&mut original.relations, &incoming.relations);
    merge_relations(&mut current.relations, &incoming.relations);
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::diff::diff_osm_data;
    use crate::osm::{OsmNode, OsmWay};

    fn node(id: i64, lat: f64, version: i32) -> OsmNode {
        OsmNode {
            id,
            lat,
            lon: 0.0,
            version,
            tags: HashMap::new(),
        }
    }

    fn way(id: i64, nodes: Vec<i64>, version: i32) -> OsmWay {
        OsmWay {
            id,
            nodes,
            version,
            tags: HashMap::new(),
        }
    }

    fn data(nodes: Vec<OsmNode>, ways: Vec<OsmWay>) -> OsmData {
        OsmData {
            nodes: nodes.into_iter().map(|n| (n.id, n)).collect(),
            ways: ways.into_iter().map(|w| (w.id, w)).collect(),
            relations: Vec::new(),
            bounds: None,
        }
    }

    #[test]
    fn unknown_objects_are_added_to_both_snapshots() {
        let mut original = data(vec![node(1, 1.0, 1)], vec![]);
        let mut current = original.clone();
        let incoming = data(
            vec![node(2, 2.0, 1), node(3, 3.0, 1)],
            vec![way(10, vec![2, 3], 1)],
        );

        let stats = merge_download(&mut original, &mut current, incoming);

        assert_eq!(
            stats,
            MergeStats {
                added: 3,
                updated: 0,
                kept_local: 0
            }
        );
        assert_eq!(current.nodes.len(), 3);
        assert!(current.ways.contains_key(&10));
        // Nothing to upload: the new objects are server state, not edits.
        assert!(diff_osm_data(&original, &current).is_empty());
    }

    #[test]
    fn re_downloading_the_same_area_changes_nothing() {
        let mut original = data(vec![node(1, 1.0, 1)], vec![way(10, vec![1], 1)]);
        let mut current = original.clone();
        let incoming = original.clone();

        let stats = merge_download(&mut original, &mut current, incoming);

        assert_eq!(stats, MergeStats::default());
        assert_eq!(stats.summary(), "nothing new");
        assert_eq!(current.nodes.len(), 1);
    }

    #[test]
    fn untouched_objects_pick_up_the_servers_newer_copy() {
        let mut original = data(vec![node(1, 1.0, 1)], vec![]);
        let mut current = original.clone();
        let incoming = data(vec![node(1, 1.5, 2)], vec![]);

        let stats = merge_download(&mut original, &mut current, incoming);

        assert_eq!(stats.updated, 1);
        assert_eq!(current.nodes[&1].lat, 1.5);
        assert_eq!(current.nodes[&1].version, 2);
        assert!(diff_osm_data(&original, &current).is_empty());
    }

    #[test]
    fn a_local_modification_survives_and_stays_the_only_pending_edit() {
        let mut original = data(vec![node(1, 1.0, 1), node(2, 2.0, 1)], vec![]);
        let mut current = original.clone();
        current.nodes.get_mut(&1).unwrap().lat = 9.0; // local move
        let incoming = data(vec![node(1, 1.5, 2), node(2, 2.0, 1)], vec![]);

        let stats = merge_download(&mut original, &mut current, incoming);

        assert_eq!(stats.kept_local, 1);
        assert_eq!(stats.updated, 0);
        assert_eq!(current.nodes[&1].lat, 9.0);
        // The base version is unchanged, so an upload against the server's
        // newer copy is rejected as a conflict instead of clobbering it.
        assert_eq!(original.nodes[&1].version, 1);
        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.counts(), (0, 1, 0));
    }

    #[test]
    fn a_local_deletion_is_not_resurrected() {
        let mut original = data(vec![node(1, 1.0, 1)], vec![]);
        let mut current = original.clone();
        current.nodes.remove(&1);
        let incoming = data(vec![node(1, 1.0, 1)], vec![]);

        let stats = merge_download(&mut original, &mut current, incoming);

        assert_eq!(stats.kept_local, 1);
        assert!(!current.nodes.contains_key(&1));
        assert_eq!(diff_osm_data(&original, &current).counts(), (0, 0, 1));
    }

    #[test]
    fn locally_created_objects_are_untouched() {
        let mut original = data(vec![], vec![]);
        let mut current = data(vec![node(-1, 5.0, 1)], vec![]);
        let incoming = data(vec![node(7, 7.0, 1)], vec![]);

        let stats = merge_download(&mut original, &mut current, incoming);

        assert_eq!(stats.added, 1);
        assert!(current.nodes.contains_key(&-1));
        assert_eq!(diff_osm_data(&original, &current).counts(), (1, 0, 0));
    }

    #[test]
    fn relations_are_added_and_replaced_by_id() {
        let relation = |id, version| OsmRelation {
            id,
            members: Vec::new(),
            version,
            tags: HashMap::new(),
        };
        let mut original = data(vec![], vec![]);
        original.relations.push(relation(1, 1));
        let mut current = original.clone();
        let mut incoming = data(vec![], vec![]);
        incoming.relations = vec![relation(1, 2), relation(2, 1)];

        merge_download(&mut original, &mut current, incoming);

        let versions: Vec<(i64, i32)> = current
            .relations
            .iter()
            .map(|r| (r.id, r.version))
            .collect();
        assert_eq!(versions, vec![(1, 2), (2, 1)]);
        assert_eq!(original.relations, current.relations);
    }

    #[test]
    fn summary_reads_naturally() {
        let stats = MergeStats {
            added: 412,
            updated: 3,
            kept_local: 1,
        };
        assert_eq!(stats.summary(), "412 new, 3 updated, 1 local edit kept");
        let stats = MergeStats {
            added: 0,
            updated: 0,
            kept_local: 2,
        };
        assert_eq!(stats.summary(), "2 local edits kept");
    }
}
