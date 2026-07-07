//! Snapshot-and-diff model used to build changeset upload payloads.
//!
//! `diff_osm_data` compares an "original" (last-known-server) `OsmData`
//! snapshot against the current, possibly-edited one and reports exactly
//! what changed, by id presence/absence and by field comparison — no
//! dependency on the undo stack or on how the edit was made. This means it
//! automatically covers node/way creation and deletion the moment
//! `OsmLayer` gains `create_node`/`delete_feature` methods, with zero
//! changes required here.

use crate::osm::{OsmData, OsmNode, OsmWay};

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
    }

    /// Total number of created/modified/deleted elements (nodes + ways),
    /// for building a human-readable per-layer summary.
    pub fn counts(&self) -> (usize, usize, usize) {
        let created = self.created_nodes.len() + self.created_ways.len();
        let modified = self.modified_nodes.len() + self.modified_ways.len();
        let deleted = self.deleted_node_ids.len() + self.deleted_way_ids.len();
        (created, modified, deleted)
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
