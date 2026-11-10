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
            return crate::tr!("status.merge.nothing_new").to_string();
        }
        let mut parts = Vec::new();
        if self.added > 0 {
            parts.push(crate::tr_count!("status.merge.added", self.added));
        }
        if self.updated > 0 {
            parts.push(crate::tr_count!("status.merge.updated", self.updated));
        }
        if self.kept_local > 0 {
            parts.push(crate::tr_count!("status.merge.kept_local", self.kept_local));
        }
        parts.join(&crate::tr!("status.merge.separator"))
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

/// Merge relations by id, like `merge_map`: unknown ones are added to both
/// snapshots, untouched ones are replaced in both, and one whose members or
/// tags were edited locally is left alone (so its edit stays pending and the
/// upload is checked against the base version). Relations don't count
/// towards `added`/`updated`, only towards `kept_local`.
fn merge_relations(
    original: &mut Vec<OsmRelation>,
    current: &mut Vec<OsmRelation>,
    incoming: &[OsmRelation],
    stats: &mut MergeStats,
) {
    let index = |relations: &[OsmRelation]| -> HashMap<i64, usize> {
        relations
            .iter()
            .enumerate()
            .map(|(i, r)| (r.id, i))
            .collect()
    };
    let mut original_index = index(original);
    let mut current_index = index(current);
    for relation in incoming {
        match (
            original_index.get(&relation.id).copied(),
            current_index.get(&relation.id).copied(),
        ) {
            (None, None) => {
                original_index.insert(relation.id, original.len());
                original.push(relation.clone());
                current_index.insert(relation.id, current.len());
                current.push(relation.clone());
            }
            (Some(o), Some(c)) if original[o] == current[c] => {
                original[o] = relation.clone();
                current[c] = relation.clone();
            }
            // Edited locally (or present on only one side): the local state
            // stands.
            _ => stats.kept_local += 1,
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
    merge_relations(
        &mut original.relations,
        &mut current.relations,
        &incoming.relations,
        &mut stats,
    );
    // The downloaded area grows by the new rectangle(s), exact repeats
    // excepted. Both snapshots carry them so the baseline stays in step.
    for rect in incoming.bounds {
        for snapshot in [&mut *original, &mut *current] {
            if !snapshot.bounds.contains(&rect) {
                snapshot.bounds.push(rect.clone());
            }
        }
    }
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
            bounds: Vec::new(),
        }
    }

    fn rect(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> crate::osm::OsmBounds {
        crate::osm::OsmBounds {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        }
    }

    #[test]
    fn merging_appends_the_downloaded_rectangle() {
        let mut original = data(vec![node(1, 1.0, 1)], vec![]);
        original.bounds = vec![rect(0.0, 1.0, 0.0, 1.0)];
        let mut current = original.clone();
        let mut incoming = data(vec![node(2, 2.0, 1)], vec![]);
        incoming.bounds = vec![rect(1.0, 2.0, 0.0, 1.0)];

        merge_download(&mut original, &mut current, incoming);

        let expected = vec![rect(0.0, 1.0, 0.0, 1.0), rect(1.0, 2.0, 0.0, 1.0)];
        assert_eq!(current.bounds, expected);
        assert_eq!(original.bounds, expected);
    }

    #[test]
    fn merging_the_same_rectangle_twice_does_not_duplicate_it() {
        let mut original = data(vec![], vec![]);
        let mut current = original.clone();
        for _ in 0..2 {
            let mut incoming = data(vec![], vec![]);
            incoming.bounds = vec![rect(0.0, 1.0, 0.0, 1.0)];
            merge_download(&mut original, &mut current, incoming);
        }
        assert_eq!(current.bounds, vec![rect(0.0, 1.0, 0.0, 1.0)]);
    }

    #[test]
    fn merging_data_without_bounds_keeps_the_known_area() {
        let mut original = data(vec![], vec![]);
        original.bounds = vec![rect(0.0, 1.0, 0.0, 1.0)];
        let mut current = original.clone();
        merge_download(
            &mut original,
            &mut current,
            data(vec![node(2, 2.0, 1)], vec![]),
        );
        assert_eq!(current.bounds, vec![rect(0.0, 1.0, 0.0, 1.0)]);
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
    fn a_locally_edited_relation_is_kept_and_stays_pending() {
        use crate::osm::{MemberType, OsmMember};
        let member = |reference| OsmMember {
            member_type: MemberType::Way,
            reference,
            role: String::new(),
        };
        let relation = |id, version, members: Vec<OsmMember>| OsmRelation {
            id,
            members,
            version,
            tags: HashMap::new(),
        };
        let mut original = data(vec![], vec![]);
        original.relations = vec![
            relation(1, 1, vec![member(10)]),
            relation(2, 1, vec![member(20)]),
        ];
        let mut current = original.clone();
        current.relations[0].members.push(member(11)); // local membership edit
        let mut incoming = data(vec![], vec![]);
        incoming.relations = vec![
            relation(1, 2, vec![member(10), member(12)]),
            relation(2, 2, vec![member(20), member(21)]),
        ];

        let stats = merge_download(&mut original, &mut current, incoming);

        assert_eq!(stats.kept_local, 1);
        // Relation 1 keeps the local edit on the old base version.
        assert_eq!(current.relations[0].members, vec![member(10), member(11)]);
        assert_eq!(original.relations[0].version, 1);
        // Relation 2 was untouched, so it takes the server's copy.
        assert_eq!(current.relations[1].version, 2);
        let diff = diff_osm_data(&original, &current);
        assert_eq!(
            diff.modified_relations
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![1]
        );
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
