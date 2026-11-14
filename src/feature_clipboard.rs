//! The feature clipboard behind Copy, Paste, Paste at Source Position and
//! Duplicate: a self-contained copy of some selected nodes, ways and
//! relations, and the logic that turns it back into new objects in a layer.
//!
//! The clipboard owns its data (ids, positions, tags), so it survives edits
//! to, or the deletion of, the layer it was copied from. Pasting always makes
//! new objects: negative ids, version 0, the same tags and geometry, and no
//! metadata from the originals.
//!
//! * A copied way brings all its nodes, selected or not. A node used by two
//!   copied ways is copied once, so the pasted ways meet at one new node.
//! * A pasted relation's members point at the pasted copies when the member
//!   was copied too and at the original object when it was not. A member the
//!   target layer cannot know about (an unsaved object of another layer) is
//!   dropped instead of left dangling.
//! * A paste is described by one [`PastedFeatures`] that can be undone and
//!   redone as a whole, with the same ids both times.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::coordinates::{lat_lon_to_mercator, mercator_to_lat_lon_wrapped};
use crate::layers::{EditableLayer, LayerId};
use crate::limits::{self, Limits};
use crate::osm::{MemberType, OsmMember};
use crate::osm_upload::xml_escape;
use crate::selection::{DeletedFeatureSnapshot, DeletedRelationSnapshot, FeatureKind, FeatureRef};
use crate::tr;

#[derive(Debug, Clone, PartialEq)]
pub struct ClipNode {
    pub id: i64,
    pub lat: f64,
    pub lon: f64,
    pub tags: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClipWay {
    pub id: i64,
    pub nodes: Vec<i64>,
    pub tags: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClipRelation {
    pub id: i64,
    pub members: Vec<OsmMember>,
    pub tags: Vec<(String, String)>,
}

/// Where a paste puts the copied features.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PastePosition {
    /// Exactly where they were copied from.
    Source,
    /// Moved so the centre of the copied features' bounding box lands here.
    CenterAt { lat: f64, lon: f64 },
}

/// Why a paste did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteError {
    /// The layer has no data to paste into.
    NoData,
    /// A pasted way would have more nodes than the server allows.
    WayTooLong { nodes: usize, max: usize },
    /// A pasted relation would have more members than the server allows.
    RelationTooLarge { members: usize, max: usize },
}

impl PasteError {
    /// The status-bar text for this refusal.
    pub fn message(&self) -> String {
        match *self {
            Self::NoData => tr!("action.no_data").to_string(),
            Self::WayTooLong { nodes, max } => tr!(
                "limits.edit.paste_way",
                nodes = limits::group_digits(nodes),
                max = limits::group_digits(max)
            )
            .to_string(),
            Self::RelationTooLarge { members, max } => tr!(
                "limits.edit.paste_relation",
                members = limits::group_digits(members),
                max = limits::group_digits(max)
            )
            .to_string(),
        }
    }
}

/// A self-contained set of copied features. See the module docs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FeatureClipboard {
    nodes: Vec<ClipNode>,
    ways: Vec<ClipWay>,
    relations: Vec<ClipRelation>,
    /// What the user had selected, in order: the features that are selected
    /// again after a paste (a way's nodes are copied but not selected).
    selected: Vec<(FeatureKind, i64)>,
}

impl FeatureClipboard {
    /// Copy the part of `selection` that lives in `layer` out of `editable`.
    /// `None` if nothing in it can be copied.
    pub fn copy(
        editable: &dyn EditableLayer,
        layer: LayerId,
        selection: &[FeatureRef],
    ) -> Option<Self> {
        let mut clip = Self::default();
        let mut node_ids: HashSet<i64> = HashSet::new();
        let mut way_ids: HashSet<i64> = HashSet::new();
        let mut relation_ids: HashSet<i64> = HashSet::new();

        for feature in selection.iter().filter(|f| f.layer_id == layer) {
            match feature.kind {
                FeatureKind::Node => {
                    if take_node(editable, layer, &mut clip.nodes, &mut node_ids, feature.id) {
                        clip.selected.push((FeatureKind::Node, feature.id));
                    }
                }
                FeatureKind::Way => {
                    if way_ids.contains(&feature.id) {
                        continue;
                    }
                    let Some(refs) = editable.way_node_ids(feature.id) else {
                        continue;
                    };
                    let nodes: Vec<i64> = refs
                        .into_iter()
                        .filter(|&id| {
                            take_node(editable, layer, &mut clip.nodes, &mut node_ids, id)
                        })
                        .collect();
                    if nodes.is_empty() {
                        continue;
                    }
                    way_ids.insert(feature.id);
                    clip.ways.push(ClipWay {
                        id: feature.id,
                        nodes,
                        tags: editable.feature_tags(feature).unwrap_or_default(),
                    });
                    clip.selected.push((FeatureKind::Way, feature.id));
                }
                FeatureKind::Relation => {
                    if relation_ids.contains(&feature.id) {
                        continue;
                    }
                    let Some(relation) = editable.relation(feature.id) else {
                        continue;
                    };
                    relation_ids.insert(feature.id);
                    let mut tags: Vec<(String, String)> = relation.tags.into_iter().collect();
                    tags.sort();
                    clip.relations.push(ClipRelation {
                        id: feature.id,
                        members: relation.members,
                        tags,
                    });
                    clip.selected.push((FeatureKind::Relation, feature.id));
                }
            }
        }
        (!clip.is_empty()).then_some(clip)
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.ways.is_empty() && self.relations.is_empty()
    }

    /// How many objects the clipboard holds (nodes, ways and relations).
    pub fn len(&self) -> usize {
        self.nodes.len() + self.ways.len() + self.relations.len()
    }

    pub fn nodes(&self) -> &[ClipNode] {
        &self.nodes
    }

    pub fn ways(&self) -> &[ClipWay] {
        &self.ways
    }

    pub fn relations(&self) -> &[ClipRelation] {
        &self.relations
    }

    /// The centre of the copied nodes' bounding box, as `(lat, lon)`; `None`
    /// when no node was copied (a lone relation has no position of its own).
    /// The box is taken in Web Mercator, so "centre" means the middle of what
    /// the map shows.
    pub fn center(&self) -> Option<(f64, f64)> {
        self.mercator_center()
            .map(|(x, y)| mercator_to_lat_lon_wrapped(x, y))
    }

    fn mercator_center(&self) -> Option<(f64, f64)> {
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        for node in &self.nodes {
            let (x, y) = lat_lon_to_mercator(node.lat, node.lon);
            let b = bounds.get_or_insert((x, y, x, y));
            b.0 = b.0.min(x);
            b.1 = b.1.min(y);
            b.2 = b.2.max(x);
            b.3 = b.3.max(y);
        }
        bounds.map(|(min_x, min_y, max_x, max_y)| ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0))
    }

    /// The Mercator shift that puts the centre at `position`; `(0, 0)` for
    /// the source position, for a clipboard without nodes and for a
    /// position that is not a number.
    fn mercator_shift(&self, position: PastePosition) -> (f64, f64) {
        let PastePosition::CenterAt { lat, lon } = position else {
            return (0.0, 0.0);
        };
        if !lat.is_finite() || !lon.is_finite() {
            return (0.0, 0.0);
        }
        let Some((cx, cy)) = self.mercator_center() else {
            return (0.0, 0.0);
        };
        let (tx, ty) = lat_lon_to_mercator(lat, lon);
        (tx - cx, ty - cy)
    }

    /// Where the copied node `node` ends up under `position`.
    pub fn pasted_position(&self, node: &ClipNode, position: PastePosition) -> (f64, f64) {
        let (dx, dy) = self.mercator_shift(position);
        if dx == 0.0 && dy == 0.0 {
            return (node.lat, node.lon);
        }
        let (x, y) = lat_lon_to_mercator(node.lat, node.lon);
        mercator_to_lat_lon_wrapped(x + dx, y + dy)
    }

    /// Check the clipboard against the server's size limits, before anything
    /// is created.
    fn check_limits(&self, limits: &Limits) -> Result<(), PasteError> {
        if let Some(way) = self.ways.iter().find(|w| w.nodes.len() > limits.way_nodes) {
            return Err(PasteError::WayTooLong {
                nodes: way.nodes.len(),
                max: limits.way_nodes,
            });
        }
        if let Some(relation) = self
            .relations
            .iter()
            .find(|r| r.members.len() > limits.relation_members)
        {
            return Err(PasteError::RelationTooLarge {
                members: relation.members.len(),
                max: limits.relation_members,
            });
        }
        Ok(())
    }

    /// The order to create the relations in: one that is a member of another
    /// copied relation comes first, so the parent can point at it. (Cyclic
    /// membership cannot be ordered; the member that closes the cycle keeps
    /// pointing at the original.)
    fn relation_order(&self) -> Vec<usize> {
        let index: HashMap<i64, usize> = self
            .relations
            .iter()
            .enumerate()
            .map(|(i, r)| (r.id, i))
            .collect();
        let mut order = Vec::with_capacity(self.relations.len());
        let mut state = vec![0u8; self.relations.len()]; // 0 new, 1 visiting, 2 done
        for start in 0..self.relations.len() {
            if state[start] != 0 {
                continue;
            }
            state[start] = 1;
            // Depth-first without recursion: a stack of (relation, next member).
            let mut stack = vec![(start, 0usize)];
            while let Some((current, next)) = stack.last_mut() {
                let current = *current;
                let members = &self.relations[current].members;
                if let Some(member) = members.get(*next) {
                    *next += 1;
                    if member.member_type != MemberType::Relation {
                        continue;
                    }
                    if let Some(&child) = index.get(&member.reference) {
                        if state[child] == 0 {
                            state[child] = 1;
                            stack.push((child, 0));
                        }
                    }
                } else {
                    state[current] = 2;
                    order.push(current);
                    stack.pop();
                }
            }
        }
        order
    }

    /// Create new objects in `editable` (which is `layer`) from the clipboard.
    ///
    /// `previous_selection` is recorded so undo can bring it back. On `Err`
    /// nothing was changed.
    pub fn paste(
        &self,
        editable: &mut dyn EditableLayer,
        layer: LayerId,
        position: PastePosition,
        limits: &Limits,
        previous_selection: &[FeatureRef],
    ) -> Result<PastedFeatures, PasteError> {
        self.check_limits(limits)?;
        if self.nodes.is_empty() && self.ways.is_empty() && editable.all_features().is_empty() {
            // Only relations to paste, and nothing in the layer they could
            // refer to: a layer without data cannot hold a relation.
            return Err(PasteError::NoData);
        }

        let mut pasted = PastedFeatures {
            layer,
            nodes: Vec::new(),
            ways: Vec::new(),
            relations: Vec::new(),
            selection: Vec::new(),
            previous_selection: previous_selection.to_vec(),
            dropped_members: 0,
            duplicate: false,
        };

        let mut new_nodes: HashMap<i64, i64> = HashMap::new();
        for node in &self.nodes {
            let (lat, lon) = self.pasted_position(node, position);
            let id = editable.add_node(lat, lon);
            for (key, value) in &node.tags {
                editable.set_tag(FeatureKind::Node, id, key, value);
            }
            new_nodes.insert(node.id, id);
            pasted.nodes.push(DeletedFeatureSnapshot {
                kind: FeatureKind::Node,
                id,
                tags: node.tags.clone(),
                way_nodes: Vec::new(),
                node_lat_lon: Some((lat, lon)),
                version: 0,
            });
        }

        let mut new_ways: HashMap<i64, i64> = HashMap::new();
        for way in &self.ways {
            let nodes: Vec<i64> = way.nodes.iter().map(|id| new_nodes[id]).collect();
            let id = editable.add_way(nodes.clone(), way.tags.clone());
            new_ways.insert(way.id, id);
            pasted.ways.push(DeletedFeatureSnapshot {
                kind: FeatureKind::Way,
                id,
                tags: way.tags.clone(),
                way_nodes: nodes,
                node_lat_lon: None,
                version: 0,
            });
        }

        let mut new_relations: HashMap<i64, i64> = HashMap::new();
        for at in self.relation_order() {
            let relation = &self.relations[at];
            let mut members = Vec::with_capacity(relation.members.len());
            for member in &relation.members {
                let copied = match member.member_type {
                    MemberType::Node => new_nodes.get(&member.reference),
                    MemberType::Way => new_ways.get(&member.reference),
                    MemberType::Relation => new_relations.get(&member.reference),
                };
                let reference = match copied {
                    Some(&new_id) => new_id,
                    // Not copied: keep pointing at the original, if the
                    // layer can have it. A server object (positive id) is
                    // fine even when it is not loaded (relations may be
                    // incomplete); an unsaved one of another layer is not.
                    None if member.reference > 0
                        || editable.has_feature(
                            match member.member_type {
                                MemberType::Node => FeatureKind::Node,
                                MemberType::Way => FeatureKind::Way,
                                MemberType::Relation => FeatureKind::Relation,
                            },
                            member.reference,
                        ) =>
                    {
                        member.reference
                    }
                    None => {
                        pasted.dropped_members += 1;
                        continue;
                    }
                };
                members.push(OsmMember {
                    member_type: member.member_type,
                    reference,
                    role: member.role.clone(),
                });
            }
            let Some(id) = editable.create_relation(relation.tags.clone(), members) else {
                // Cannot happen once the layer has data; undo what was done.
                pasted.undo(editable);
                return Err(PasteError::NoData);
            };
            new_relations.insert(relation.id, id);
            let index = editable
                .relation_ids()
                .iter()
                .position(|&r| r == id)
                .unwrap_or(0);
            if let Some(created) = editable.relation(id) {
                pasted.relations.push(DeletedRelationSnapshot {
                    relation: created,
                    index,
                });
            }
        }

        for &(kind, old) in &self.selected {
            let id = match kind {
                FeatureKind::Node => new_nodes.get(&old),
                FeatureKind::Way => new_ways.get(&old),
                FeatureKind::Relation => new_relations.get(&old),
            };
            if let Some(&id) = id {
                pasted.selection.push(FeatureRef {
                    layer_id: layer,
                    kind,
                    id,
                });
            }
        }
        // A relation is only ever selected on its own.
        if pasted
            .selection
            .iter()
            .any(|f| f.kind != FeatureKind::Relation)
        {
            pasted.selection.retain(|f| f.kind != FeatureKind::Relation);
        }
        Ok(pasted)
    }

    /// The copied features as OSM XML, for the system clipboard: every object
    /// renumbered with fresh negative ids, so it reads as new data wherever
    /// it is pasted. Relation members that were not copied are left out.
    pub fn to_osm_xml(&self) -> String {
        let mut next = 0i64;
        let mut fresh = || {
            next -= 1;
            next
        };
        let node_ids: HashMap<i64, i64> = self.nodes.iter().map(|n| (n.id, fresh())).collect();
        let way_ids: HashMap<i64, i64> = self.ways.iter().map(|w| (w.id, fresh())).collect();
        let relation_ids: HashMap<i64, i64> =
            self.relations.iter().map(|r| (r.id, fresh())).collect();

        let mut out = String::new();
        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        let _ = writeln!(
            out,
            "<osm version=\"0.6\" generator=\"{}\">",
            xml_escape(crate::CREATED_BY)
        );
        let tags = |out: &mut String, tags: &[(String, String)]| {
            for (k, v) in tags {
                let _ = writeln!(
                    out,
                    "    <tag k=\"{}\" v=\"{}\"/>",
                    xml_escape(k),
                    xml_escape(v)
                );
            }
        };
        for node in &self.nodes {
            let _ = writeln!(
                out,
                "  <node id=\"{}\" action=\"modify\" visible=\"true\" lat=\"{}\" lon=\"{}\">",
                node_ids[&node.id], node.lat, node.lon
            );
            tags(&mut out, &node.tags);
            out.push_str("  </node>\n");
        }
        for way in &self.ways {
            let _ = writeln!(
                out,
                "  <way id=\"{}\" action=\"modify\" visible=\"true\">",
                way_ids[&way.id]
            );
            for node in &way.nodes {
                let _ = writeln!(out, "    <nd ref=\"{}\"/>", node_ids[node]);
            }
            tags(&mut out, &way.tags);
            out.push_str("  </way>\n");
        }
        for relation in &self.relations {
            let _ = writeln!(
                out,
                "  <relation id=\"{}\" action=\"modify\" visible=\"true\">",
                relation_ids[&relation.id]
            );
            for member in &relation.members {
                let renumbered = match member.member_type {
                    MemberType::Node => node_ids.get(&member.reference),
                    MemberType::Way => way_ids.get(&member.reference),
                    MemberType::Relation => relation_ids.get(&member.reference),
                };
                if let Some(id) = renumbered {
                    let _ = writeln!(
                        out,
                        "    <member type=\"{}\" ref=\"{}\" role=\"{}\"/>",
                        member.member_type,
                        id,
                        xml_escape(&member.role)
                    );
                }
            }
            tags(&mut out, &relation.tags);
            out.push_str("  </relation>\n");
        }
        out.push_str("</osm>\n");
        out
    }
}

/// Add node `id` to `nodes` unless it is already there. `false` if the layer
/// has no position for it (then it is left out, and so is the way's reference
/// to it).
fn take_node(
    editable: &dyn EditableLayer,
    layer: LayerId,
    nodes: &mut Vec<ClipNode>,
    seen: &mut HashSet<i64>,
    id: i64,
) -> bool {
    if seen.contains(&id) {
        return true;
    }
    let Some((lat, lon)) = editable.node_lat_lon(id) else {
        return false;
    };
    seen.insert(id);
    let feature = FeatureRef {
        layer_id: layer,
        kind: FeatureKind::Node,
        id,
    };
    nodes.push(ClipNode {
        id,
        lat,
        lon,
        tags: editable.feature_tags(&feature).unwrap_or_default(),
    });
    true
}

/// What one paste created, enough to undo it and redo it with the same ids.
#[derive(Debug, Clone, PartialEq)]
pub struct PastedFeatures {
    pub layer: LayerId,
    /// The new nodes, ways and relations, in creation order.
    pub nodes: Vec<DeletedFeatureSnapshot>,
    pub ways: Vec<DeletedFeatureSnapshot>,
    pub relations: Vec<DeletedRelationSnapshot>,
    /// What to select after the paste (and after a redo).
    pub selection: Vec<FeatureRef>,
    /// What was selected before it (restored by undo).
    pub previous_selection: Vec<FeatureRef>,
    /// Relation members left out because the target layer cannot have them.
    pub dropped_members: usize,
    /// Whether this was a Duplicate rather than a Paste (only the history
    /// text differs).
    pub duplicate: bool,
}

impl PastedFeatures {
    /// Every object this paste created.
    pub fn created(&self) -> Vec<FeatureRef> {
        let feature = |kind, id| FeatureRef {
            layer_id: self.layer,
            kind,
            id,
        };
        self.nodes
            .iter()
            .map(|s| feature(FeatureKind::Node, s.id))
            .chain(self.ways.iter().map(|s| feature(FeatureKind::Way, s.id)))
            .chain(
                self.relations
                    .iter()
                    .map(|s| feature(FeatureKind::Relation, s.relation.id)),
            )
            .collect()
    }

    /// Remove everything the paste created. Relations go first, then ways,
    /// then nodes, the reverse of creation (a node cannot be deleted while a
    /// way uses it, nor a relation while another has it as a member).
    pub fn undo(&self, editable: &mut dyn EditableLayer) {
        for snapshot in self.relations.iter().rev() {
            editable.delete_relation(snapshot.relation.id);
        }
        for snapshot in self.ways.iter().rev() {
            editable.delete_feature(FeatureKind::Way, snapshot.id);
        }
        for snapshot in self.nodes.iter().rev() {
            editable.delete_feature(FeatureKind::Node, snapshot.id);
        }
    }

    /// Create the same objects again, under the same ids.
    pub fn redo(&self, editable: &mut dyn EditableLayer) {
        for snapshot in &self.nodes {
            editable.restore_feature(snapshot.clone());
        }
        for snapshot in &self.ways {
            editable.restore_feature(snapshot.clone());
        }
        for snapshot in &self.relations {
            editable.restore_relation(snapshot.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::MERCATOR_HALF_WORLD_M;
    use crate::layers::osm_layer::OsmLayer;
    use crate::osm::{OsmData, OsmNode, OsmRelation, OsmWay};
    use std::sync::Arc;

    /// Pasting far from the source can overshoot the antimeridian; the
    /// longitude wraps (unlike `coordinates::mercator_to_lat_lon`, which clamps).
    #[test]
    fn mercator_to_lat_lon_wraps_longitude() {
        let (_, lon) = mercator_to_lat_lon_wrapped(MERCATOR_HALF_WORLD_M * 1.5, 0.0);
        assert!((lon - -90.0).abs() < 1e-9, "got {lon}");
        let (lat, lon) = mercator_to_lat_lon_wrapped(MERCATOR_HALF_WORLD_M, 0.0);
        assert!(lat.abs() < 1e-9 && (lon - 180.0).abs() < 1e-9);
    }

    const A: LayerId = LayerId(1);
    const B: LayerId = LayerId(2);

    fn tags(list: &[(&str, &str)]) -> HashMap<String, String> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn node(id: i64, lat: f64, lon: f64, t: &[(&str, &str)]) -> OsmNode {
        OsmNode {
            id,
            lat,
            lon,
            version: 3,
            tags: tags(t),
        }
    }

    fn member(kind: MemberType, reference: i64, role: &str) -> OsmMember {
        OsmMember {
            member_type: kind,
            reference,
            role: role.into(),
        }
    }

    /// Nodes 1-6; way 10 = [1,2,3], way 11 = [3,4,5] (they share node 3);
    /// node 6 is a tagged bench; relation 20 holds both ways and the bench;
    /// relation 21 holds relation 20.
    fn data() -> OsmData {
        let mut nodes = HashMap::new();
        for (id, lat, lon) in [
            (1, 47.000, 8.000),
            (2, 47.001, 8.000),
            (3, 47.001, 8.002),
            (4, 47.003, 8.002),
            (5, 47.003, 8.004),
            (6, 47.005, 8.005),
        ] {
            let t: &[(&str, &str)] = if id == 6 {
                &[("amenity", "bench")]
            } else {
                &[]
            };
            nodes.insert(id, node(id, lat, lon, t));
        }
        let mut ways = HashMap::new();
        ways.insert(
            10,
            OsmWay {
                id: 10,
                nodes: vec![1, 2, 3],
                version: 2,
                tags: tags(&[("highway", "residential"), ("name", "Main")]),
            },
        );
        ways.insert(
            11,
            OsmWay {
                id: 11,
                nodes: vec![3, 4, 5],
                version: 5,
                tags: tags(&[("highway", "service")]),
            },
        );
        OsmData {
            nodes,
            ways,
            relations: vec![
                OsmRelation {
                    id: 20,
                    members: vec![
                        member(MemberType::Way, 10, "outer"),
                        member(MemberType::Way, 11, "inner"),
                        member(MemberType::Node, 6, "label"),
                    ],
                    version: 4,
                    tags: tags(&[("type", "multipolygon")]),
                },
                OsmRelation {
                    id: 21,
                    members: vec![member(MemberType::Relation, 20, "part")],
                    version: 1,
                    tags: tags(&[("type", "collection")]),
                },
            ],
            bounds: Vec::new(),
        }
    }

    fn layer(id: LayerId) -> OsmLayer {
        OsmLayer::new_with_data(id, format!("layer {}", id.0), Arc::new(data()))
    }

    fn limits() -> Limits {
        Limits {
            way_nodes: 2000,
            relation_members: 32_000,
            changeset_elements: 10_000,
        }
    }

    fn feature(kind: FeatureKind, id: i64) -> FeatureRef {
        FeatureRef {
            layer_id: A,
            kind,
            id,
        }
    }

    fn select(kind: FeatureKind, ids: &[i64]) -> Vec<FeatureRef> {
        ids.iter().map(|&id| feature(kind, id)).collect()
    }

    fn copy(layer: &OsmLayer, selection: &[FeatureRef]) -> FeatureClipboard {
        FeatureClipboard::copy(layer, A, selection).expect("something to copy")
    }

    fn paste(
        clip: &FeatureClipboard,
        layer: &mut OsmLayer,
        layer_id: LayerId,
        position: PastePosition,
    ) -> PastedFeatures {
        clip.paste(layer, layer_id, position, &limits(), &[])
            .expect("paste")
    }

    fn center_of(layer: &OsmLayer, nodes: &[i64]) -> (f64, f64) {
        let merc: Vec<(f64, f64)> = nodes
            .iter()
            .map(|&id| {
                let (lat, lon) = layer.node_lat_lon(id).unwrap();
                lat_lon_to_mercator(lat, lon)
            })
            .collect();
        let (min_x, max_x) = merc.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
            (lo.min(p.0), hi.max(p.0))
        });
        let (min_y, max_y) = merc.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
            (lo.min(p.1), hi.max(p.1))
        });
        mercator_to_lat_lon_wrapped((min_x + max_x) / 2.0, (min_y + max_y) / 2.0)
    }

    fn all(layer: &OsmLayer) -> Vec<FeatureRef> {
        layer.all_features()
    }

    #[test]
    fn copying_a_way_copies_all_its_nodes_even_unselected() {
        let layer = layer(A);
        let clip = copy(&layer, &select(FeatureKind::Way, &[10]));
        let ids: Vec<i64> = clip.nodes().iter().map(|n| n.id).collect();
        assert_eq!(ids, [1, 2, 3]);
        assert_eq!(clip.ways().len(), 1);
        assert_eq!(clip.ways()[0].nodes, [1, 2, 3]);
        assert_eq!(
            clip.ways()[0].tags,
            [
                ("highway".to_string(), "residential".to_string()),
                ("name".to_string(), "Main".to_string())
            ]
        );
        assert_eq!(clip.selected, [(FeatureKind::Way, 10)]);
        assert!(clip.relations().is_empty());
        assert_eq!(clip.len(), 4);
    }

    #[test]
    fn copying_nothing_or_another_layers_features_gives_no_clipboard() {
        let layer = layer(A);
        assert!(FeatureClipboard::copy(&layer, A, &[]).is_none());
        let elsewhere = [FeatureRef {
            layer_id: B,
            kind: FeatureKind::Node,
            id: 1,
        }];
        assert!(FeatureClipboard::copy(&layer, A, &elsewhere).is_none());
        // A node the layer does not have.
        assert!(FeatureClipboard::copy(&layer, A, &select(FeatureKind::Node, &[999])).is_none());
    }

    #[test]
    fn pasting_makes_new_objects_with_the_same_tags_and_geometry() {
        let mut layer = layer(A);
        let clip = copy(&layer, &select(FeatureKind::Way, &[10]));
        let before = all(&layer);
        let pasted = paste(&clip, &mut layer, A, PastePosition::Source);

        // 3 nodes + 1 way, all new: negative ids, version 0, same place.
        let created = pasted.created();
        assert_eq!(created.len(), 4);
        assert!(created.iter().all(|f| f.id < 0 && !before.contains(f)));
        let way = pasted.ways[0].clone();
        assert_eq!(way.version, 0);
        assert_eq!(
            way.tags,
            [
                ("highway".to_string(), "residential".to_string()),
                ("name".to_string(), "Main".to_string())
            ]
        );
        let new_nodes = layer.way_node_ids(way.id).unwrap();
        assert_eq!(new_nodes.len(), 3);
        for (new, old) in new_nodes.iter().zip([1, 2, 3]) {
            assert!(*new < 0);
            assert_eq!(layer.node_lat_lon(*new), layer.node_lat_lon(old));
        }
        // The originals are untouched.
        assert_eq!(layer.way_node_ids(10).unwrap(), [1, 2, 3]);
        // What becomes the selection: the pasted way (its nodes are not
        // selected).
        assert_eq!(
            pasted.selection,
            [FeatureRef {
                layer_id: A,
                kind: FeatureKind::Way,
                id: way.id
            }]
        );
    }

    #[test]
    fn tagged_nodes_keep_their_tags_and_get_no_server_metadata() {
        let mut layer = layer(A);
        let clip = copy(&layer, &select(FeatureKind::Node, &[6]));
        let pasted = paste(&clip, &mut layer, A, PastePosition::Source);
        let snapshot = &pasted.nodes[0];
        assert!(snapshot.id < 0);
        assert_eq!(snapshot.version, 0);
        assert_eq!(
            snapshot.tags,
            [("amenity".to_string(), "bench".to_string())]
        );
        let tags = layer
            .feature_tags(&feature(FeatureKind::Node, snapshot.id))
            .unwrap();
        assert_eq!(tags, [("amenity".to_string(), "bench".to_string())]);
    }

    #[test]
    fn nodes_shared_by_copied_ways_stay_shared_in_the_copy() {
        let mut layer = layer(A);
        let clip = copy(&layer, &select(FeatureKind::Way, &[10, 11]));
        // Node 3 is copied once, not once per way.
        assert_eq!(clip.nodes().len(), 5);
        let pasted = paste(&clip, &mut layer, A, PastePosition::Source);
        assert_eq!(pasted.nodes.len(), 5);
        let first = layer.way_node_ids(pasted.ways[0].id).unwrap();
        let second = layer.way_node_ids(pasted.ways[1].id).unwrap();
        assert_eq!(first[2], second[0], "the two ways still meet at one node");
        assert_eq!(layer.ways_using_node(first[2]).len(), 2);
        // The copy shares nothing with the originals.
        assert!(first.iter().chain(&second).all(|id| *id < 0));
    }

    #[test]
    fn relation_members_point_at_copies_when_copied_else_at_the_original() {
        let mut layer = layer(A);
        // Way 10 and relation 20 are copied; way 11 and node 6 are not.
        let mut selection = select(FeatureKind::Way, &[10]);
        selection.extend(select(FeatureKind::Relation, &[20]));
        let clip = copy(&layer, &selection);
        let pasted = paste(&clip, &mut layer, A, PastePosition::Source);

        let relation_id = pasted.relations[0].relation.id;
        assert!(relation_id < 0);
        let relation = layer.relation(relation_id).unwrap();
        assert_eq!(relation.version, 0);
        assert_eq!(relation.tags, tags(&[("type", "multipolygon")]));
        let new_way = pasted.ways[0].id;
        assert_eq!(
            relation.members,
            [
                member(MemberType::Way, new_way, "outer"),
                member(MemberType::Way, 11, "inner"),
                member(MemberType::Node, 6, "label"),
            ]
        );
        // The original relation still holds the original way.
        assert_eq!(layer.relation(20).unwrap().members[0].reference, 10);
        // Ways are in the selection; the relation is not mixed in with them.
        assert_eq!(
            pasted.selection,
            [feature(FeatureKind::Way, new_way)],
            "a relation is only selected on its own"
        );
    }

    #[test]
    fn a_lone_relation_is_pasted_with_its_members_untouched_and_selected() {
        let mut layer = layer(A);
        let clip = copy(&layer, &select(FeatureKind::Relation, &[20]));
        assert!(clip.nodes().is_empty());
        let pasted = paste(&clip, &mut layer, A, PastePosition::Source);
        let id = pasted.relations[0].relation.id;
        assert_eq!(
            layer.relation(id).unwrap().members,
            layer.relation(20).unwrap().members
        );
        assert_eq!(pasted.selection, [feature(FeatureKind::Relation, id)]);
        // No node, so there is nothing to move: the centre is unknown and a
        // paste at a position is the same as at the source.
        assert_eq!(clip.center(), None);
        let moved = paste(
            &clip,
            &mut layer,
            A,
            PastePosition::CenterAt {
                lat: 10.0,
                lon: 10.0,
            },
        );
        assert_eq!(moved.relations.len(), 1);
    }

    #[test]
    fn nested_relations_are_remapped_children_first() {
        let mut layer = layer(A);
        // Select the parent first: the child must still be created first.
        let clip = copy(&layer, &select(FeatureKind::Relation, &[21, 20]));
        let pasted = paste(&clip, &mut layer, A, PastePosition::Source);
        assert_eq!(pasted.relations.len(), 2);
        let child = pasted.relations[0].relation.id;
        let parent = pasted.relations[1].relation.id;
        assert_eq!(
            layer.relation(parent).unwrap().members,
            [member(MemberType::Relation, child, "part")]
        );
        // Undo removes the parent before the child it refers to, which the
        // layer would otherwise refuse.
        let before = all(&layer);
        pasted.undo(&mut layer);
        assert!(layer.relation(parent).is_none() && layer.relation(child).is_none());
        assert_eq!(all(&layer).len(), before.len() - 2);
        pasted.redo(&mut layer);
        assert!(layer.relation(parent).is_some() && layer.relation(child).is_some());
    }

    #[test]
    fn cyclic_relations_do_not_hang() {
        let mut data = data();
        data.relations = vec![
            OsmRelation {
                id: 30,
                members: vec![member(MemberType::Relation, 31, "")],
                version: 1,
                tags: HashMap::new(),
            },
            OsmRelation {
                id: 31,
                members: vec![member(MemberType::Relation, 30, "")],
                version: 1,
                tags: HashMap::new(),
            },
        ];
        let mut layer = OsmLayer::new_with_data(A, "cycle", Arc::new(data));
        let clip = copy(&layer, &select(FeatureKind::Relation, &[30, 31]));
        let pasted = paste(&clip, &mut layer, A, PastePosition::Source);
        assert_eq!(pasted.relations.len(), 2);
    }

    #[test]
    fn paste_puts_the_centre_of_the_copy_at_the_target() {
        let mut layer = layer(A);
        let clip = copy(&layer, &select(FeatureKind::Way, &[10, 11]));
        let (target_lat, target_lon) = (51.5, -0.12);
        let pasted = paste(
            &clip,
            &mut layer,
            A,
            PastePosition::CenterAt {
                lat: target_lat,
                lon: target_lon,
            },
        );
        let ids: Vec<i64> = pasted.nodes.iter().map(|s| s.id).collect();
        let (lat, lon) = center_of(&layer, &ids);
        assert!((lat - target_lat).abs() < 1e-7, "{lat}");
        assert!((lon - target_lon).abs() < 1e-7, "{lon}");

        // The shape is kept: each pasted node is offset from its original by
        // the same Mercator vector.
        let merc = |layer: &OsmLayer, id: i64| {
            let (lat, lon) = layer.node_lat_lon(id).unwrap();
            lat_lon_to_mercator(lat, lon)
        };
        let new_ids: Vec<i64> = pasted.nodes.iter().map(|s| s.id).collect();
        let shift = |old: i64, new: i64| {
            let (ox, oy) = merc(&layer, old);
            let (nx, ny) = merc(&layer, new);
            (nx - ox, ny - oy)
        };
        let first = shift(clip.nodes()[0].id, new_ids[0]);
        for (old, new) in clip.nodes().iter().zip(&new_ids) {
            let s = shift(old.id, *new);
            assert!((s.0 - first.0).abs() < 1e-3 && (s.1 - first.1).abs() < 1e-3);
        }
    }

    #[test]
    fn the_source_position_is_exact() {
        let layer_a = layer(A);
        let clip = copy(&layer_a, &select(FeatureKind::Way, &[10]));
        for node in clip.nodes() {
            assert_eq!(
                clip.pasted_position(node, PastePosition::Source),
                (node.lat, node.lon)
            );
        }
        // An unusable target is the source position too.
        let nan = PastePosition::CenterAt {
            lat: f64::NAN,
            lon: 0.0,
        };
        for node in clip.nodes() {
            assert_eq!(clip.pasted_position(node, nan), (node.lat, node.lon));
        }
    }

    #[test]
    fn extreme_targets_never_panic_or_leave_the_world() {
        let layer_a = layer(A);
        let clip = copy(&layer_a, &select(FeatureKind::Way, &[10, 11]));
        for (lat, lon) in [
            (89.9, 179.99),
            (-89.9, -179.99),
            (0.0, 180.0),
            (0.0, -180.0),
            (85.0, 0.0),
            (1e9, 1e9),
            (f64::INFINITY, 0.0),
        ] {
            for node in clip.nodes() {
                let (nlat, nlon) = clip.pasted_position(node, PastePosition::CenterAt { lat, lon });
                assert!(nlat.is_finite() && (-85.06..=85.06).contains(&nlat));
                assert!(
                    nlon.is_finite() && (-180.0..=180.0).contains(&nlon),
                    "{nlon}"
                );
            }
        }
        // A copy that straddles the antimeridian still has some centre.
        let mut data = data();
        data.nodes.get_mut(&1).unwrap().lon = 179.9999;
        data.nodes.get_mut(&2).unwrap().lon = -179.9999;
        let layer_b = OsmLayer::new_with_data(A, "x", Arc::new(data));
        let clip = copy(&layer_b, &select(FeatureKind::Way, &[10]));
        assert!(clip.center().is_some());
    }

    #[test]
    fn a_paste_is_undone_whole_and_redone_with_the_same_ids() {
        let mut layer = layer(A);
        let mut selection = select(FeatureKind::Way, &[10, 11]);
        selection.extend(select(FeatureKind::Relation, &[20]));
        let clip = copy(&layer, &selection);
        let before = all(&layer);
        let pasted = clip
            .paste(
                &mut layer,
                A,
                PastePosition::CenterAt {
                    lat: 40.0,
                    lon: 9.0,
                },
                &limits(),
                &select(FeatureKind::Node, &[1]),
            )
            .unwrap();
        assert_eq!(
            pasted.previous_selection,
            select(FeatureKind::Node, &[1]),
            "undo can restore the selection from before"
        );
        let after_paste = all(&layer);
        assert!(after_paste.len() > before.len());
        let created = pasted.created();
        assert_eq!(created.len(), after_paste.len() - before.len());
        assert!(created.iter().all(|f| after_paste.contains(f)));

        let describe = |layer: &OsmLayer| {
            created
                .iter()
                .map(|f| {
                    (
                        *f,
                        layer.feature_tags(f),
                        layer
                            .node_lat_lon(f.id)
                            .filter(|_| f.kind == FeatureKind::Node),
                        layer
                            .way_node_ids(f.id)
                            .filter(|_| f.kind == FeatureKind::Way),
                        layer
                            .relation(f.id)
                            .filter(|_| f.kind == FeatureKind::Relation)
                            .map(|r| r.members),
                    )
                })
                .collect::<Vec<_>>()
        };
        let pasted_state = describe(&layer);

        pasted.undo(&mut layer);
        assert_eq!(all(&layer), before, "everything it created is gone");
        assert!(!layer.is_modified() || layer.diff_for_upload().counts() == (0, 0, 0));

        pasted.redo(&mut layer);
        assert_eq!(all(&layer), after_paste, "the same ids are back");
        assert_eq!(describe(&layer), pasted_state, "with the same content");
        assert_eq!(layer.diff_for_upload().counts().0, created.len());
    }

    #[test]
    fn pasting_into_another_layer_works_and_leaves_the_source_alone() {
        let source = layer(A);
        let mut other = OsmLayer::new_with_data(
            B,
            "other",
            Arc::new(OsmData {
                nodes: HashMap::new(),
                ways: HashMap::new(),
                relations: Vec::new(),
                bounds: Vec::new(),
            }),
        );
        let clip = copy(&source, &select(FeatureKind::Way, &[10]));
        let pasted = clip
            .paste(&mut other, B, PastePosition::Source, &limits(), &[])
            .unwrap();
        assert_eq!(pasted.layer, B);
        assert_eq!(other.all_features().len(), 4);
        assert!(pasted.selection.iter().all(|f| f.layer_id == B));
        assert_eq!(source.all_features().len(), 10);
        pasted.undo(&mut other);
        assert!(other.all_features().is_empty());
    }

    #[test]
    fn members_the_target_layer_cannot_have_are_left_out_not_dangling() {
        let mut data = data();
        // An unsaved way only layer A knows, and a server relation.
        data.ways.insert(
            -5,
            OsmWay {
                id: -5,
                nodes: vec![1, 2],
                version: 0,
                tags: HashMap::new(),
            },
        );
        data.relations.push(OsmRelation {
            id: 22,
            members: vec![
                member(MemberType::Way, -5, "a"),
                member(MemberType::Way, 777, "b"),
                member(MemberType::Node, 1, "c"),
            ],
            version: 1,
            tags: HashMap::new(),
        });
        let source = OsmLayer::new_with_data(A, "a", Arc::new(data));
        let mut other = layer(B);
        let clip = copy(&source, &select(FeatureKind::Relation, &[22]));
        let pasted = clip
            .paste(&mut other, B, PastePosition::Source, &limits(), &[])
            .unwrap();
        // Way -5 does not exist in B and has no server id: dropped. Way 777
        // is a server id: kept (an incomplete relation is legitimate). Node
        // 1 exists in B too.
        assert_eq!(pasted.dropped_members, 1);
        let members = other
            .relation(pasted.relations[0].relation.id)
            .unwrap()
            .members;
        assert_eq!(
            members,
            [
                member(MemberType::Way, 777, "b"),
                member(MemberType::Node, 1, "c")
            ]
        );
    }

    #[test]
    fn the_clipboard_keeps_its_own_copy_of_the_data() {
        let mut layer = layer(A);
        let clip = copy(&layer, &select(FeatureKind::Way, &[10]));
        // Edit the source: retag, move a node, then delete the way.
        layer.set_tag(FeatureKind::Way, 10, "highway", "motorway");
        layer.commit_node_moves(&[(1, 10.0, 10.0)]);
        layer.delete_feature(FeatureKind::Way, 10);
        let pasted = paste(&clip, &mut layer, A, PastePosition::Source);
        let way = &pasted.ways[0];
        assert!(way
            .tags
            .contains(&("highway".to_string(), "residential".to_string())));
        let first = layer.way_node_ids(way.id).unwrap()[0];
        assert_eq!(layer.node_lat_lon(first), Some((47.000, 8.000)));

        // And it outlives the source layer altogether.
        drop(layer);
        let mut elsewhere = OsmLayer::new_with_data(
            B,
            "b",
            Arc::new(OsmData {
                nodes: HashMap::new(),
                ways: HashMap::new(),
                relations: Vec::new(),
                bounds: Vec::new(),
            }),
        );
        let pasted = clip
            .paste(&mut elsewhere, B, PastePosition::Source, &limits(), &[])
            .unwrap();
        assert_eq!(pasted.created().len(), 4);
    }

    #[test]
    fn size_limits_refuse_before_anything_is_created() {
        let mut layer = layer(A);
        let clip = copy(&layer, &select(FeatureKind::Way, &[10]));
        let before = all(&layer);
        let tight = Limits {
            way_nodes: 2,
            ..limits()
        };
        assert_eq!(
            clip.paste(&mut layer, A, PastePosition::Source, &tight, &[])
                .unwrap_err(),
            PasteError::WayTooLong { nodes: 3, max: 2 }
        );
        assert_eq!(all(&layer), before);

        let rel = copy(&layer, &select(FeatureKind::Relation, &[20]));
        let tight = Limits {
            relation_members: 2,
            ..limits()
        };
        assert_eq!(
            rel.paste(&mut layer, A, PastePosition::Source, &tight, &[])
                .unwrap_err(),
            PasteError::RelationTooLarge { members: 3, max: 2 }
        );
        assert_eq!(all(&layer), before);
        assert!(PasteError::WayTooLong { nodes: 3, max: 2 }
            .message()
            .contains('3'));
    }

    #[test]
    fn a_layer_without_data_takes_nodes_and_ways_but_not_a_lone_relation() {
        let source = layer(A);
        let mut empty = OsmLayer::new(B);
        let rel = copy(&source, &select(FeatureKind::Relation, &[20]));
        assert_eq!(
            rel.paste(&mut empty, B, PastePosition::Source, &limits(), &[])
                .unwrap_err(),
            PasteError::NoData
        );
        let way = copy(&source, &select(FeatureKind::Way, &[10]));
        let pasted = way
            .paste(&mut empty, B, PastePosition::Source, &limits(), &[])
            .unwrap();
        assert_eq!(pasted.created().len(), 4);
    }

    #[test]
    fn the_text_form_is_osm_xml_with_fresh_ids() {
        let layer = layer(A);
        let mut selection = select(FeatureKind::Way, &[10]);
        selection.extend(select(FeatureKind::Relation, &[20]));
        let xml = copy(&layer, &selection).to_osm_xml();
        let parsed = crate::osm::OsmParser::new().parse_str(&xml).unwrap();
        assert_eq!(parsed.nodes.len(), 3);
        assert_eq!(parsed.ways.len(), 1);
        assert_eq!(parsed.relations.len(), 1);
        assert!(parsed.nodes.keys().all(|id| *id < 0));
        let way = parsed.ways.values().next().unwrap();
        assert!(way.nodes.iter().all(|id| parsed.nodes.contains_key(id)));
        assert_eq!(
            way.tags,
            tags(&[("highway", "residential"), ("name", "Main")])
        );
        // Only the member that was copied is kept, renumbered.
        let relation = &parsed.relations[0];
        assert_eq!(relation.members.len(), 1);
        assert_eq!(relation.members[0].reference, way.id);
        assert_eq!(relation.members[0].role, "outer");
    }
}
