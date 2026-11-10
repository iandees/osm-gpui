//! Snapshot-and-diff model used to build changeset upload payloads.
//!
//! `diff_osm_data` compares an "original" (last-known-server) `OsmData`
//! snapshot against the current, possibly-edited one and reports exactly
//! what changed, by id presence/absence and by field comparison — no
//! dependency on the undo stack or on how the edit was made. This means it
//! automatically covers node/way creation and deletion the moment
//! `OsmLayer` gains `create_node`/`delete_feature` methods, with zero
//! changes required here.

use crate::osm::{FileEdits, OsmData, OsmNode, OsmRelation, OsmWay};
use std::collections::HashMap;

/// Tag placed on the *baseline* copy of an element that a loaded file marks
/// as modified. Such a file records the element's edited state and base
/// version but not what it looked like on the server, so its real baseline
/// is unknown; all that matters is that the baseline copy differs from the
/// current one, which makes the element count as locally modified for both
/// upload diffing and download merging. The baseline is never rendered,
/// exported, or uploaded, so this tag never leaves this process.
pub const UNKNOWN_BASELINE_TAG: &str = "osm-gpui:unknown-baseline";

/// Reconstruct the server baseline for data loaded from a JOSM-style file,
/// so that diffing it against `current` reproduces the file's pending
/// edits:
///
/// - elements with negative ids were created locally: absent from the
///   baseline;
/// - elements marked modified get a baseline copy that is guaranteed to
///   differ (see [`UNKNOWN_BASELINE_TAG`]);
/// - elements marked deleted are present in the baseline only;
/// - everything else is identical in both.
pub fn baseline_for_loaded_file(current: &OsmData, edits: &FileEdits) -> OsmData {
    let mut baseline = current.clone();
    baseline.nodes.retain(|id, _| *id >= 0);
    baseline.ways.retain(|id, _| *id >= 0);

    let marker = || (UNKNOWN_BASELINE_TAG.to_string(), "yes".to_string());
    for id in &edits.modified_nodes {
        if let Some(node) = baseline.nodes.get_mut(id) {
            node.tags.extend([marker()]);
        }
    }
    for id in &edits.modified_ways {
        if let Some(way) = baseline.ways.get_mut(id) {
            way.tags.extend([marker()]);
        }
    }
    baseline.relations.retain(|r| r.id >= 0);
    for relation in &mut baseline.relations {
        if edits.modified_relations.contains(&relation.id) {
            relation.tags.extend([marker()]);
        }
    }
    for (id, node) in &edits.deleted_nodes {
        // A deleted element that was never uploaded has nothing to delete.
        if *id >= 0 {
            baseline.nodes.insert(*id, node.clone());
        }
    }
    for (id, way) in &edits.deleted_ways {
        if *id >= 0 {
            baseline.ways.insert(*id, way.clone());
        }
    }
    for (id, relation) in &edits.deleted_relations {
        if *id >= 0 {
            baseline.relations.push(relation.clone());
        }
    }
    baseline
}

/// The set of changes needed to bring the server's view of a layer's data up
/// to date with the current in-memory state. Pure data — no gpui/network
/// dependencies — so it can be built and tested independent of `OsmLayer`'s
/// rendering/indexing machinery.
#[derive(Debug, Clone, Default)]
pub struct LayerDiff {
    pub created_nodes: Vec<OsmNode>,
    pub modified_nodes: Vec<OsmNode>,
    /// (id, version-as-of-original) for nodes present in the original
    /// snapshot but no longer present in the current data.
    pub deleted_node_ids: Vec<(i64, i32)>,
    pub created_ways: Vec<OsmWay>,
    pub modified_ways: Vec<OsmWay>,
    /// (id, version-as-of-original) for ways present in the original
    /// snapshot but no longer present in the current data.
    pub deleted_way_ids: Vec<(i64, i32)>,
    /// Relations whose members or tags differ from the baseline.
    pub modified_relations: Vec<OsmRelation>,
    /// Relations absent from the baseline (negative local ids).
    pub created_relations: Vec<OsmRelation>,
    /// (id, version-as-of-original) for relations present in the original
    /// snapshot but no longer present in the current data.
    pub deleted_relation_ids: Vec<(i64, i32)>,
}

impl LayerDiff {
    /// True when there is nothing to upload for this layer.
    pub fn is_empty(&self) -> bool {
        self.created_nodes.is_empty()
            && self.modified_nodes.is_empty()
            && self.deleted_node_ids.is_empty()
            && self.created_ways.is_empty()
            && self.modified_ways.is_empty()
            && self.deleted_way_ids.is_empty()
            && self.modified_relations.is_empty()
            && self.created_relations.is_empty()
            && self.deleted_relation_ids.is_empty()
    }

    /// Total number of created/modified/deleted elements (nodes, ways and
    /// relations), for building a human-readable per-layer summary.
    pub fn counts(&self) -> (usize, usize, usize) {
        let created =
            self.created_nodes.len() + self.created_ways.len() + self.created_relations.len();
        let modified =
            self.modified_nodes.len() + self.modified_ways.len() + self.modified_relations.len();
        let deleted = self.deleted_node_ids.len()
            + self.deleted_way_ids.len()
            + self.deleted_relation_ids.len();
        (created, modified, deleted)
    }
}

/// Classify relations (matched by id) against the baseline. Member order is
/// part of a relation, so reordering counts as a modification.
fn diff_relations(original: &OsmData, current: &OsmData, diff: &mut LayerDiff) {
    let originals: HashMap<i64, &OsmRelation> =
        original.relations.iter().map(|r| (r.id, r)).collect();
    for relation in &current.relations {
        match originals.get(&relation.id) {
            None => diff.created_relations.push(relation.clone()),
            Some(orig) => {
                if orig.members != relation.members || orig.tags != relation.tags {
                    diff.modified_relations.push(relation.clone());
                }
            }
        }
    }
    let currents: std::collections::HashSet<i64> = current.relations.iter().map(|r| r.id).collect();
    for orig in &original.relations {
        if !currents.contains(&orig.id) {
            diff.deleted_relation_ids.push((orig.id, orig.version));
        }
    }
}

/// Compare `original` against `current` by id and classify every node/way as
/// created, modified, deleted, or unchanged. A node/way counts as modified
/// if its tags or its position/member list differ; unchanged elements
/// (including elements untouched since `original` was captured) are omitted
/// entirely.
pub fn diff_osm_data(original: &OsmData, current: &OsmData) -> LayerDiff {
    let mut diff = LayerDiff::default();

    for node in current.nodes.values() {
        match original.nodes.get(&node.id) {
            None => diff.created_nodes.push(node.clone()),
            Some(orig) => {
                if orig.lat != node.lat || orig.lon != node.lon || orig.tags != node.tags {
                    diff.modified_nodes.push(node.clone());
                }
            }
        }
    }
    for orig in original.nodes.values() {
        if !current.nodes.contains_key(&orig.id) {
            diff.deleted_node_ids.push((orig.id, orig.version));
        }
    }

    for way in current.ways.values() {
        match original.ways.get(&way.id) {
            None => diff.created_ways.push(way.clone()),
            Some(orig) => {
                if orig.nodes != way.nodes || orig.tags != way.tags {
                    diff.modified_ways.push(way.clone());
                }
            }
        }
    }
    for orig in original.ways.values() {
        if !current.ways.contains_key(&orig.id) {
            diff.deleted_way_ids.push((orig.id, orig.version));
        }
    }

    diff_relations(original, current, &mut diff);
    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_tags() -> HashMap<String, String> {
        HashMap::new()
    }

    fn data(nodes: Vec<OsmNode>, ways: Vec<OsmWay>) -> OsmData {
        let mut map = HashMap::new();
        for n in nodes {
            map.insert(n.id, n);
        }
        let mut way_map = HashMap::new();
        for w in ways {
            way_map.insert(w.id, w);
        }
        OsmData {
            nodes: map,
            ways: way_map,
            relations: Vec::new(),
            bounds: None,
        }
    }

    fn node(id: i64, lat: f64, lon: f64, version: i32) -> OsmNode {
        OsmNode {
            id,
            lat,
            lon,
            version,
            tags: empty_tags(),
        }
    }

    fn way(id: i64, nodes: Vec<i64>, version: i32) -> OsmWay {
        OsmWay {
            id,
            nodes,
            version,
            tags: empty_tags(),
        }
    }

    #[test]
    fn baseline_for_loaded_file_reproduces_the_files_pending_edits() {
        // Current data as loaded: node 1 untouched, node 2 marked modified,
        // node -1 created locally; node 3 (v4) was marked deleted, as was a
        // never-uploaded node -2.
        let current = data(
            vec![
                node(1, 1.0, 1.0, 1),
                node(2, 2.0, 2.0, 7),
                node(-1, 9.0, 9.0, 1),
            ],
            vec![way(10, vec![1, 2], 3)],
        );
        let mut edits = FileEdits::default();
        edits.modified_nodes.insert(2);
        edits.modified_ways.insert(10);
        edits.deleted_nodes.insert(3, node(3, 3.0, 3.0, 4));
        edits.deleted_nodes.insert(-2, node(-2, 8.0, 8.0, 1));

        let baseline = baseline_for_loaded_file(&current, &edits);
        let diff = diff_osm_data(&baseline, &current);

        let ids = |nodes: &[OsmNode]| {
            let mut ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
            ids.sort_unstable();
            ids
        };
        assert_eq!(ids(&diff.created_nodes), vec![-1]);
        assert_eq!(ids(&diff.modified_nodes), vec![2]);
        assert_eq!(diff.deleted_node_ids, vec![(3, 4)]);
        assert_eq!(diff.modified_ways.len(), 1);
        // The base version of a modified element is the one the file gave.
        assert_eq!(baseline.nodes[&2].version, 7);
        // The marker lives only in the baseline.
        assert!(!current.nodes[&2].tags.contains_key(UNKNOWN_BASELINE_TAG));
    }

    #[test]
    fn baseline_for_a_file_without_edits_is_the_data_itself() {
        let current = data(vec![node(1, 1.0, 1.0, 1)], vec![way(10, vec![1], 1)]);
        let baseline = baseline_for_loaded_file(&current, &FileEdits::default());
        assert_eq!(baseline, current);
    }

    #[test]
    fn identical_data_produces_empty_diff() {
        let d = data(vec![node(1, 1.0, 1.0, 1)], vec![way(10, vec![1], 1)]);
        let diff = diff_osm_data(&d, &d);
        assert!(diff.is_empty(), "{:?}", diff);
    }

    #[test]
    fn detects_created_node_and_way() {
        let original = data(vec![node(1, 1.0, 1.0, 1)], vec![]);
        let mut current = original.clone();
        current.nodes.insert(2, node(2, 2.0, 2.0, 1));
        current.ways.insert(10, way(10, vec![1, 2], 1));

        let diff = diff_osm_data(&original, &current);
        assert_eq!(
            diff.created_nodes.iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(
            diff.created_ways.iter().map(|w| w.id).collect::<Vec<_>>(),
            vec![10]
        );
        assert!(diff.modified_nodes.is_empty());
        assert!(diff.deleted_node_ids.is_empty());
    }

    #[test]
    fn detects_created_node_with_negative_local_id() {
        let original = data(vec![node(1, 1.0, 1.0, 1)], vec![]);
        let mut current = original.clone();
        current.nodes.insert(-1, node(-1, 3.0, 3.0, 1));

        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.created_nodes.len(), 1);
        assert_eq!(diff.created_nodes[0].id, -1);
    }

    #[test]
    fn detects_deleted_node_and_way() {
        let original = data(
            vec![node(1, 1.0, 1.0, 3), node(2, 2.0, 2.0, 5)],
            vec![way(10, vec![1, 2], 2)],
        );
        let mut current = original.clone();
        current.nodes.remove(&2);
        current.ways.clear();

        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.deleted_node_ids, vec![(2, 5)]);
        assert_eq!(diff.deleted_way_ids, vec![(10, 2)]);
        assert!(diff.created_nodes.is_empty());
        assert!(diff.modified_nodes.is_empty());
    }

    #[test]
    fn detects_modified_node_position_and_tags() {
        let original = data(vec![node(1, 1.0, 1.0, 1)], vec![]);
        let mut current = original.clone();
        current.nodes.get_mut(&1).unwrap().lat = 1.5;

        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.modified_nodes.len(), 1);
        assert_eq!(diff.modified_nodes[0].lat, 1.5);

        let mut current2 = original.clone();
        current2
            .nodes
            .get_mut(&1)
            .unwrap()
            .tags
            .insert("amenity".to_string(), "cafe".to_string());
        let diff2 = diff_osm_data(&original, &current2);
        assert_eq!(diff2.modified_nodes.len(), 1);
    }

    #[test]
    fn detects_modified_way_nodes_and_tags() {
        let original = data(
            vec![node(1, 1.0, 1.0, 1), node(2, 2.0, 2.0, 1)],
            vec![way(10, vec![1, 2], 1)],
        );
        let mut current = original.clone();
        current.ways.get_mut(&10).unwrap().nodes.push(1); // append a node ref -> geometry changed

        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.modified_ways.len(), 1);
        assert_eq!(diff.modified_ways[0].nodes, vec![1, 2, 1]);
    }

    #[test]
    fn unchanged_elements_are_not_reported() {
        let original = data(
            vec![node(1, 1.0, 1.0, 1), node(2, 2.0, 2.0, 1)],
            vec![way(10, vec![1, 2], 1)],
        );
        let mut current = original.clone();
        // Only node 2 changes; node 1 and the way are untouched.
        current.nodes.get_mut(&2).unwrap().lon = 9.0;

        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.modified_nodes.len(), 1);
        assert_eq!(diff.modified_nodes[0].id, 2);
        assert!(diff.modified_ways.is_empty());
    }

    fn member(kind: crate::osm::MemberType, reference: i64, role: &str) -> crate::osm::OsmMember {
        crate::osm::OsmMember {
            member_type: kind,
            reference,
            role: role.to_string(),
        }
    }

    fn relation(id: i64, members: Vec<crate::osm::OsmMember>) -> OsmRelation {
        OsmRelation {
            id,
            members,
            version: 4,
            tags: empty_tags(),
        }
    }

    #[test]
    fn relation_member_changes_are_reported_as_modified() {
        use crate::osm::MemberType::Way;
        let mut original = data(vec![], vec![]);
        original
            .relations
            .push(relation(5, vec![member(Way, 10, ""), member(Way, 11, "")]));
        original
            .relations
            .push(relation(6, vec![member(Way, 12, "")]));

        let diff = diff_osm_data(&original, &original);
        assert!(diff.is_empty());

        // Adding, retargeting, re-roling and reordering all count.
        for edit in [
            |r: &mut OsmRelation| r.members.push(member(Way, 13, "")),
            |r: &mut OsmRelation| r.members[0].reference = 99,
            |r: &mut OsmRelation| r.members[0].role = "outer".into(),
            |r: &mut OsmRelation| r.members.reverse(),
        ] {
            let mut current = original.clone();
            edit(&mut current.relations[0]);
            let diff = diff_osm_data(&original, &current);
            assert_eq!(
                diff.modified_relations
                    .iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>(),
                vec![5]
            );
            assert_eq!(diff.counts(), (0, 1, 0));
            assert!(!diff.is_empty());
        }

        // A tag change counts too.
        let mut current = original.clone();
        current.relations[1]
            .tags
            .insert("type".into(), "route".into());
        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.modified_relations[0].id, 6);
    }

    #[test]
    fn created_and_deleted_relations_are_reported_and_counted() {
        use crate::osm::MemberType::Way;
        let mut original = data(vec![], vec![]);
        original
            .relations
            .push(relation(5, vec![member(Way, 10, "")]));
        original
            .relations
            .push(relation(6, vec![member(Way, 12, "")]));

        let mut current = original.clone();
        current.relations.remove(0); // delete 5 (baseline version 4)
        current
            .relations
            .push(relation(-1, vec![member(Way, 12, "")]));
        current
            .relations
            .push(relation(-2, vec![member(Way, 12, "")]));

        let diff = diff_osm_data(&original, &current);
        let mut created: Vec<i64> = diff.created_relations.iter().map(|r| r.id).collect();
        created.sort_unstable();
        assert_eq!(created, vec![-2, -1]);
        assert_eq!(diff.deleted_relation_ids, vec![(5, 4)]);
        assert!(diff.modified_relations.is_empty());
        assert_eq!(diff.counts(), (2, 0, 1));
        assert!(!diff.is_empty());

        // Each alone makes the diff non-empty.
        let only_deleted = LayerDiff {
            deleted_relation_ids: vec![(1, 1)],
            ..Default::default()
        };
        assert!(!only_deleted.is_empty());
        let only_created = LayerDiff {
            created_relations: vec![relation(-1, vec![])],
            ..Default::default()
        };
        assert!(!only_created.is_empty());
    }

    #[test]
    fn counts_sums_created_modified_deleted() {
        let original = data(vec![node(1, 1.0, 1.0, 1), node(2, 2.0, 2.0, 1)], vec![]);
        let mut current = original.clone();
        current.nodes.remove(&2);
        current.nodes.insert(3, node(3, 3.0, 3.0, 1));
        current.nodes.get_mut(&1).unwrap().lat = 9.0;

        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.counts(), (1, 1, 1));
    }
}
