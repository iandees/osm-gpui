//! Resolving an upload conflict on one object, as pure operations on a
//! layer's two snapshots (`original`: what the server had when each object
//! was fetched; `current`: that plus local edits — see `merge`).
//!
//! Both resolutions work by changing what the layer believes about the
//! server, then letting the usual `diff` do the rest on the next upload:
//!
//! - **Keep mine** only moves the object's *base version* forward to the
//!   server's current one. The local edit is untouched, so the next diff
//!   still contains it, now claiming to be an edit of the latest version.
//! - **Use theirs** puts the server's copy into both snapshots, so the object
//!   no longer differs and drops out of the diff: the local change is gone.

use crate::ids::{NodeId, RelationId, WayId};
use crate::osm::{MemberType, OsmData, OsmNode};
use crate::osm_api::ServerElement;
use crate::selection::FeatureKind;

/// Set the base version of `(kind, id)` to `version` in whichever snapshots
/// hold it. The diff reads the version of a modified object from `current`
/// and that of a deleted one from `original`, so both are updated. Returns
/// whether either snapshot had the object.
pub fn set_base_version(
    original: &mut OsmData,
    current: &mut OsmData,
    kind: FeatureKind,
    id: i64,
    version: i32,
) -> bool {
    let mut found = false;
    for data in [original, current] {
        match kind {
            FeatureKind::Node => {
                if let Some(n) = data.nodes.get_mut(&NodeId(id)) {
                    n.version = version;
                    found = true;
                }
            }
            FeatureKind::Way => {
                if let Some(w) = data.ways.get_mut(&WayId(id)) {
                    w.version = version;
                    found = true;
                }
            }
            FeatureKind::Relation => {
                if let Some(r) = data.relations.iter_mut().find(|r| r.id.get() == id) {
                    r.version = version;
                    found = true;
                }
            }
        }
    }
    found
}

/// Whether anything uses node `id`: a way's node list or a relation member.
fn node_in_use(data: &OsmData, id: NodeId) -> bool {
    data.ways.values().any(|w| w.nodes.contains(&id))
        || data.relations.iter().any(|r| {
            r.members
                .iter()
                .any(|m| m.member_type == MemberType::Node && m.reference == id.get())
        })
}

/// Replace `(kind, id)` with the server's current version in both snapshots,
/// dropping the local change to it; or, when the server has deleted it,
/// remove it from both. Returns whether anything was done.
///
/// A way fetched in full brings its nodes along. Each is put into both
/// snapshots unless the user has moved or retagged that node themselves — an
/// edit to a different object, which stays pending. A node the user deleted
/// but the server's way still needs comes back. Nodes the old local way used
/// that exist only locally (negative ids), carry no tags and are used by
/// nothing else are dropped with it rather than left behind as strays.
pub fn adopt_server_element(
    original: &mut OsmData,
    current: &mut OsmData,
    kind: FeatureKind,
    id: i64,
    server: ServerElement,
) -> bool {
    let data = match server {
        ServerElement::Gone => return remove_element(original, current, kind, id),
        ServerElement::Present(data) => data,
    };
    match kind {
        FeatureKind::Node => {
            let id = NodeId(id);
            let Some(node) = data.nodes.get(&id) else {
                return false;
            };
            original.nodes.insert(id, node.clone());
            current.nodes.insert(id, node.clone());
            true
        }
        FeatureKind::Way => {
            let id = WayId(id);
            let Some(way) = data.ways.get(&id) else {
                return false;
            };
            let old_local_nodes: Vec<NodeId> = current
                .ways
                .get(&id)
                .map(|w| w.nodes.clone())
                .unwrap_or_default();
            original.ways.insert(id, way.clone());
            current.ways.insert(id, way.clone());
            for node in data.nodes.values() {
                adopt_node_unless_edited(original, current, node);
            }
            for node_id in old_local_nodes {
                let orphan = node_id.is_new()
                    && current
                        .nodes
                        .get(&node_id)
                        .is_some_and(|n| n.tags.is_empty())
                    && !node_in_use(current, node_id);
                if orphan {
                    current.nodes.remove(&node_id);
                }
            }
            true
        }
        FeatureKind::Relation => {
            let id = RelationId(id);
            let Some(relation) = data.relations.iter().find(|r| r.id == id) else {
                return false;
            };
            for snapshot in [original, current] {
                match snapshot.relations.iter_mut().find(|r| r.id == id) {
                    Some(slot) => *slot = relation.clone(),
                    None => snapshot.relations.push(relation.clone()),
                }
            }
            true
        }
    }
}

/// Put the server's `node` into both snapshots unless it is present in both
/// and differs between them (the user's own edit of that node).
fn adopt_node_unless_edited(original: &mut OsmData, current: &mut OsmData, node: &OsmNode) {
    let edited = matches!(
        (original.nodes.get(&node.id), current.nodes.get(&node.id)),
        (Some(o), Some(c)) if o != c
    );
    if !edited {
        original.nodes.insert(node.id, node.clone());
        current.nodes.insert(node.id, node.clone());
    }
}

fn remove_element(
    original: &mut OsmData,
    current: &mut OsmData,
    kind: FeatureKind,
    id: i64,
) -> bool {
    let mut removed = false;
    for data in [original, current] {
        match kind {
            FeatureKind::Node => removed |= data.nodes.remove(&NodeId(id)).is_some(),
            FeatureKind::Way => removed |= data.ways.remove(&WayId(id)).is_some(),
            FeatureKind::Relation => {
                let before = data.relations.len();
                data.relations.retain(|r| r.id.get() != id);
                removed |= data.relations.len() != before;
            }
        }
    }
    removed
}

/// Forget a local deletion of `(kind, id)` that the server has already done
/// itself: remove the object from the baseline so the diff no longer asks to
/// delete it. Only acts when the object is deleted locally (in `original`,
/// not in `current`); returns whether it did.
pub fn forget_local_delete(
    original: &mut OsmData,
    current: &mut OsmData,
    kind: FeatureKind,
    id: i64,
) -> bool {
    let present_locally = match kind {
        FeatureKind::Node => current.nodes.contains_key(&NodeId(id)),
        FeatureKind::Way => current.ways.contains_key(&WayId(id)),
        FeatureKind::Relation => current.relations.iter().any(|r| r.id.get() == id),
    };
    if present_locally {
        return false;
    }
    match kind {
        FeatureKind::Node => original.nodes.remove(&NodeId(id)).is_some(),
        FeatureKind::Way => original.ways.remove(&WayId(id)).is_some(),
        FeatureKind::Relation => {
            let before = original.relations.len();
            original.relations.retain(|r| r.id.get() != id);
            original.relations.len() != before
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::diff::diff_osm_data;
    use crate::osm::{OsmRelation, OsmWay};
    use crate::tags::Tags;

    fn node(id: i64, lat: f64, version: i32) -> OsmNode {
        OsmNode {
            id: NodeId(id),
            lat,
            lon: 0.0,
            version,
            tags: Tags::new(),
        }
    }

    fn way(id: i64, nodes: Vec<i64>, version: i32) -> OsmWay {
        OsmWay {
            id: WayId(id),
            nodes: nodes.into_iter().map(NodeId).collect::<Vec<_>>(),
            version,
            tags: Tags::new(),
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

    #[test]
    fn keep_mine_moves_the_base_version_of_a_modified_object() {
        let mut original = data(vec![node(1, 1.0, 3)], vec![]);
        let mut current = original.clone();
        current.nodes.get_mut(&NodeId(1)).unwrap().lat = 9.0;

        assert!(set_base_version(
            &mut original,
            &mut current,
            FeatureKind::Node,
            1,
            4
        ));
        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.modified_nodes.len(), 1);
        assert_eq!(diff.modified_nodes[0].version, 4);
        assert_eq!(diff.modified_nodes[0].lat, 9.0, "the local edit stays");
    }

    #[test]
    fn keep_mine_moves_the_base_version_of_a_deleted_object() {
        let mut original = data(vec![node(1, 1.0, 3)], vec![]);
        let mut current = original.clone();
        current.nodes.remove(&NodeId(1));

        set_base_version(&mut original, &mut current, FeatureKind::Node, 1, 7);
        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.deleted_node_ids, vec![(NodeId(1), 7)]);
    }

    #[test]
    fn keep_mine_on_a_relation() {
        let mut original = data(vec![], vec![]);
        original.relations.push(OsmRelation {
            id: RelationId(5),
            members: vec![],
            version: 2,
            tags: Tags::new(),
        });
        let mut current = original.clone();
        current.relations[0]
            .tags
            .insert("type".to_string(), "route".to_string());
        assert!(set_base_version(
            &mut original,
            &mut current,
            FeatureKind::Relation,
            5,
            9
        ));
        let diff = diff_osm_data(&original, &current);
        assert_eq!(diff.modified_relations[0].version, 9);
        assert!(!set_base_version(
            &mut original,
            &mut current,
            FeatureKind::Relation,
            6,
            9
        ));
    }

    #[test]
    fn use_theirs_replaces_a_node_in_both_snapshots() {
        let mut original = data(vec![node(1, 1.0, 3)], vec![]);
        let mut current = original.clone();
        current.nodes.get_mut(&NodeId(1)).unwrap().lat = 9.0;

        let server = ServerElement::Present(data(vec![node(1, 2.0, 4)], vec![]));
        assert!(adopt_server_element(
            &mut original,
            &mut current,
            FeatureKind::Node,
            1,
            server
        ));
        assert_eq!(current.nodes[&NodeId(1)].lat, 2.0);
        assert_eq!(current.nodes[&NodeId(1)].version, 4);
        assert_eq!(original.nodes[&NodeId(1)], current.nodes[&NodeId(1)]);
        assert!(diff_osm_data(&original, &current).is_empty());
    }

    #[test]
    fn use_theirs_undoes_a_local_delete() {
        let mut original = data(vec![node(1, 1.0, 3)], vec![]);
        let mut current = original.clone();
        current.nodes.remove(&NodeId(1));
        let server = ServerElement::Present(data(vec![node(1, 1.5, 4)], vec![]));
        adopt_server_element(&mut original, &mut current, FeatureKind::Node, 1, server);
        assert_eq!(current.nodes[&NodeId(1)].version, 4);
        assert!(diff_osm_data(&original, &current).is_empty());
    }

    #[test]
    fn use_theirs_gone_removes_the_object_everywhere() {
        let mut original = data(vec![node(1, 1.0, 3)], vec![]);
        let mut current = original.clone();
        current.nodes.get_mut(&NodeId(1)).unwrap().lat = 9.0;
        assert!(adopt_server_element(
            &mut original,
            &mut current,
            FeatureKind::Node,
            1,
            ServerElement::Gone
        ));
        assert!(original.nodes.is_empty() && current.nodes.is_empty());
        assert!(diff_osm_data(&original, &current).is_empty());
    }

    #[test]
    fn use_theirs_way_takes_the_servers_nodes_but_keeps_other_local_edits() {
        // Way 10 = [1, 2, -1] locally (-1 is a new, untagged node); the
        // server's way 10 is [1, 3] now. Node 1 was moved locally.
        let mut original = data(
            vec![node(1, 1.0, 1), node(2, 2.0, 1)],
            vec![way(10, vec![1, 2], 5)],
        );
        let mut current = original.clone();
        current.nodes.get_mut(&NodeId(1)).unwrap().lat = 8.0;
        current.nodes.insert(NodeId(-1), node(-1, 5.0, 0));
        current.ways.get_mut(&WayId(10)).unwrap().nodes = vec![NodeId(1), NodeId(2), NodeId(-1)];

        let server = ServerElement::Present(data(
            vec![node(1, 1.0, 1), node(3, 3.0, 2)],
            vec![way(10, vec![1, 3], 6)],
        ));
        assert!(adopt_server_element(
            &mut original,
            &mut current,
            FeatureKind::Way,
            10,
            server
        ));
        assert_eq!(current.ways[&WayId(10)].nodes, vec![NodeId(1), NodeId(3)]);
        assert_eq!(current.ways[&WayId(10)].version, 6);
        assert_eq!(original.ways[&WayId(10)], current.ways[&WayId(10)]);
        assert!(current.nodes.contains_key(&NodeId(3)) && original.nodes.contains_key(&NodeId(3)));
        assert!(
            !current.nodes.contains_key(&NodeId(-1)),
            "stray new node dropped"
        );
        let diff = diff_osm_data(&original, &current);
        assert!(diff.modified_ways.is_empty());
        assert_eq!(
            diff.modified_nodes.len(),
            1,
            "the user's move of node 1 stays pending"
        );
        assert_eq!(diff.modified_nodes[0].lat, 8.0);
    }

    #[test]
    fn use_theirs_way_brings_back_a_node_the_server_still_uses() {
        let mut original = data(
            vec![node(1, 1.0, 1), node(2, 2.0, 1)],
            vec![way(10, vec![1, 2], 5)],
        );
        let mut current = original.clone();
        current.ways.remove(&WayId(10));
        current.nodes.remove(&NodeId(2));
        let server = ServerElement::Present(data(
            vec![node(1, 1.0, 1), node(2, 2.0, 1)],
            vec![way(10, vec![1, 2], 6)],
        ));
        adopt_server_element(&mut original, &mut current, FeatureKind::Way, 10, server);
        assert!(current.nodes.contains_key(&NodeId(2)));
        assert!(diff_osm_data(&original, &current).is_empty());
    }

    #[test]
    fn use_theirs_relation_replaces_or_removes() {
        let rel = |version, role: &str| OsmRelation {
            id: RelationId(5),
            members: vec![crate::osm::OsmMember {
                member_type: MemberType::Way,
                reference: 10,
                role: role.to_string(),
            }],
            version,
            tags: Tags::new(),
        };
        let mut original = data(vec![], vec![]);
        original.relations.push(rel(2, ""));
        let mut current = original.clone();
        current.relations[0] = rel(2, "outer");

        let mut server = data(vec![], vec![]);
        server.relations.push(rel(3, "inner"));
        adopt_server_element(
            &mut original,
            &mut current,
            FeatureKind::Relation,
            5,
            ServerElement::Present(server),
        );
        assert_eq!(current.relations[0], rel(3, "inner"));
        assert!(diff_osm_data(&original, &current).is_empty());

        adopt_server_element(
            &mut original,
            &mut current,
            FeatureKind::Relation,
            5,
            ServerElement::Gone,
        );
        assert!(original.relations.is_empty() && current.relations.is_empty());
    }

    #[test]
    fn forget_local_delete_only_acts_on_locally_deleted_objects() {
        let mut original = data(vec![node(1, 1.0, 3), node(2, 2.0, 3)], vec![]);
        let mut current = original.clone();
        current.nodes.remove(&NodeId(1));

        assert!(forget_local_delete(
            &mut original,
            &mut current,
            FeatureKind::Node,
            1
        ));
        assert!(diff_osm_data(&original, &current).is_empty());
        // Node 2 is still there locally: not a deletion to forget.
        assert!(!forget_local_delete(
            &mut original,
            &mut current,
            FeatureKind::Node,
            2
        ));
        assert!(original.nodes.contains_key(&NodeId(2)));
    }
}
