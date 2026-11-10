//! Uploading more edits than one changeset may hold, as several changesets.
//!
//! The server refuses a changeset with more than `max_changeset_elements`
//! (10,000 on the public server) elements. [`plan_chunks`] splits the pending
//! diffs into an ordered list of chunks of at most that many elements;
//! [`run_chunked_upload`] sends them one after another, each in a changeset of
//! its own, carrying the server-assigned ids of earlier chunks into later ones.
//!
//! # Why cutting the sequence at the limit is valid
//!
//! Inside one diff the API applies elements in document order, and the editor
//! writes them in this order (see `osm_upload::build_osm_change_xml`):
//!
//! 1. create: nodes, ways, relations;
//! 2. modify: nodes, ways, relations;
//! 3. delete: relations, ways, nodes.
//!
//! The planner lays every element of every pending diff out in that one global
//! sequence and cuts it into consecutive runs. Uploading the runs in order
//! applies exactly the same elements in exactly the same order as one big
//! diff would, with a changeset boundary between some of them. So every
//! reference that is valid in the big diff is valid across the cut, provided
//! the thing referred to is in the same or an earlier chunk, which holds
//! because:
//!
//! - a way's new nodes, and a relation's new members, are created before them
//!   (all node creates precede way creates precede relation creates); modifies
//!   come after every create, so they only refer backwards too;
//! - created relations that are members of other created relations are
//!   ordered first (a topological order of the "has member" graph; relations
//!   in a cycle are one unit that is never split across a cut);
//! - deletes come last, after every create and modify, so by the time a
//!   delete runs, everything that used to refer to the deleted object has
//!   already been modified or deleted (relations, then ways, then nodes).
//!
//! To keep a new way next to its new nodes in the history, created nodes are
//! laid out just before the first way that uses them rather than all at the
//! front. That is still "every node before the way that uses it", and the
//! chunk count is the same either way (chunks are cut by element count), so
//! it costs nothing. Nodes no new way uses come after the ways, before the
//! relations. When everything fits in one chunk the diffs are passed through
//! untouched, so a small upload is byte for byte what it always was.
//!
//! A new object keeps a negative placeholder id until the server answers, and
//! the answer to chunk *k* is the first time later chunks can learn the real
//! id. [`IdMap`] collects the answers and [`IdMap::rewrite`] swaps placeholder
//! node and member references in each later chunk before it is serialized.
//! Every object is in exactly one chunk, so no id is ever answered twice.

use std::collections::{HashMap, HashSet};

use crate::http::HttpClient;
use crate::layers::diff::LayerDiff;
use crate::osm::{MemberType, OsmNode, OsmRelation, OsmWay};
use crate::osm_upload::{run_upload, UploadFailure, UploadResult};
use crate::tr;
use crate::transfer::TransferPhase;

/// Why the pending edits can't be split into valid chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    /// New relations that contain each other, more of them than one changeset
    /// holds. They have to be created together, so there is no valid split.
    CycleTooLarge { size: usize, max: usize },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            PlanError::CycleTooLarge { size, max } => write!(
                f,
                "{}",
                tr!(
                    "upload_chunks.cycle_too_large",
                    size = crate::limits::group_digits(size),
                    max = crate::limits::group_digits(max)
                )
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// Number of elements in `diff`.
fn element_count(diff: &LayerDiff) -> usize {
    let (created, modified, deleted) = diff.counts();
    created + modified + deleted
}

/// One element placed in the global upload sequence.
enum Item {
    CreateNode(OsmNode),
    CreateWay(OsmWay),
    CreateRelation(OsmRelation),
    ModifyNode(OsmNode),
    ModifyWay(OsmWay),
    ModifyRelation(OsmRelation),
    DeleteRelation(i64, i32),
    DeleteWay(i64, i32),
    DeleteNode(i64, i32),
}

impl Item {
    fn push_into(self, diff: &mut LayerDiff) {
        match self {
            Item::CreateNode(n) => diff.created_nodes.push(n),
            Item::CreateWay(w) => diff.created_ways.push(w),
            Item::CreateRelation(r) => diff.created_relations.push(r),
            Item::ModifyNode(n) => diff.modified_nodes.push(n),
            Item::ModifyWay(w) => diff.modified_ways.push(w),
            Item::ModifyRelation(r) => diff.modified_relations.push(r),
            Item::DeleteRelation(id, v) => diff.deleted_relation_ids.push((id, v)),
            Item::DeleteWay(id, v) => diff.deleted_way_ids.push((id, v)),
            Item::DeleteNode(id, v) => diff.deleted_node_ids.push((id, v)),
        }
    }
}

/// Items that must land in the same chunk. Almost always one item.
type Atom = Vec<Item>;

/// All of `diffs` as one diff, element order preserved.
fn merge(diffs: &[&LayerDiff]) -> LayerDiff {
    let mut out = LayerDiff::default();
    for d in diffs {
        out.created_nodes.extend(d.created_nodes.iter().cloned());
        out.modified_nodes.extend(d.modified_nodes.iter().cloned());
        out.deleted_node_ids.extend(&d.deleted_node_ids);
        out.created_ways.extend(d.created_ways.iter().cloned());
        out.modified_ways.extend(d.modified_ways.iter().cloned());
        out.deleted_way_ids.extend(&d.deleted_way_ids);
        out.modified_relations
            .extend(d.modified_relations.iter().cloned());
        out.created_relations
            .extend(d.created_relations.iter().cloned());
        out.deleted_relation_ids.extend(&d.deleted_relation_ids);
    }
    out
}

/// Created relations grouped so that every relation comes after the created
/// relations it has as members, and relations that contain each other (a
/// strongly connected component) form one group. Tarjan's algorithm emits a
/// component only after every component reachable from it, which for the
/// edge "relation -> member relation" is exactly dependencies first.
fn relation_groups(mut relations: Vec<&OsmRelation>) -> Vec<Vec<&OsmRelation>> {
    relations.sort_by_key(|r| r.id);
    let index: HashMap<i64, usize> = relations
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id, i))
        .collect();
    let adj: Vec<Vec<usize>> = relations
        .iter()
        .enumerate()
        .map(|(i, r)| {
            r.members
                .iter()
                .filter(|m| m.member_type == MemberType::Relation)
                .filter_map(|m| index.get(&m.reference).copied())
                .filter(|&j| j != i)
                .collect()
        })
        .collect();

    struct Tarjan<'a> {
        adj: &'a [Vec<usize>],
        order: Vec<Option<usize>>,
        low: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        next: usize,
        out: Vec<Vec<usize>>,
    }
    impl Tarjan<'_> {
        fn visit(&mut self, v: usize) {
            self.order[v] = Some(self.next);
            self.low[v] = self.next;
            self.next += 1;
            self.stack.push(v);
            self.on_stack[v] = true;
            let adj = self.adj;
            for &w in &adj[v] {
                match self.order[w] {
                    None => {
                        self.visit(w);
                        self.low[v] = self.low[v].min(self.low[w]);
                    }
                    Some(order_w) if self.on_stack[w] => {
                        self.low[v] = self.low[v].min(order_w);
                    }
                    Some(_) => {}
                }
            }
            if Some(self.low[v]) == self.order[v] {
                let mut component = Vec::new();
                while let Some(w) = self.stack.pop() {
                    self.on_stack[w] = false;
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                component.sort_unstable();
                self.out.push(component);
            }
        }
    }

    let n = relations.len();
    let mut t = Tarjan {
        adj: &adj,
        order: vec![None; n],
        low: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        next: 0,
        out: Vec::new(),
    };
    for v in 0..n {
        if t.order[v].is_none() {
            t.visit(v);
        }
    }
    t.out
        .into_iter()
        .map(|c| c.into_iter().map(|i| relations[i]).collect())
        .collect()
}

/// Every element of `diffs` in the global upload order, grouped into atoms.
fn sequence(diffs: &[&LayerDiff]) -> Vec<Atom> {
    let mut atoms: Vec<Atom> = Vec::new();

    // Creates. Each new node goes just before the first new way that uses it.
    let created_nodes: HashMap<i64, &OsmNode> = diffs
        .iter()
        .flat_map(|d| &d.created_nodes)
        .map(|n| (n.id, n))
        .collect();
    let mut emitted: HashSet<i64> = HashSet::new();
    for way in diffs.iter().flat_map(|d| &d.created_ways) {
        for nd in &way.nodes {
            if let Some(node) = created_nodes.get(nd) {
                if emitted.insert(*nd) {
                    atoms.push(vec![Item::CreateNode((*node).clone())]);
                }
            }
        }
        atoms.push(vec![Item::CreateWay(way.clone())]);
    }
    for node in diffs.iter().flat_map(|d| &d.created_nodes) {
        if emitted.insert(node.id) {
            atoms.push(vec![Item::CreateNode(node.clone())]);
        }
    }
    let created_relations: Vec<&OsmRelation> =
        diffs.iter().flat_map(|d| &d.created_relations).collect();
    for group in relation_groups(created_relations) {
        atoms.push(
            group
                .into_iter()
                .map(|r| Item::CreateRelation(r.clone()))
                .collect(),
        );
    }

    // Modifies.
    for d in diffs {
        atoms.extend(
            d.modified_nodes
                .iter()
                .map(|n| vec![Item::ModifyNode(n.clone())]),
        );
    }
    for d in diffs {
        atoms.extend(
            d.modified_ways
                .iter()
                .map(|w| vec![Item::ModifyWay(w.clone())]),
        );
    }
    for d in diffs {
        atoms.extend(
            d.modified_relations
                .iter()
                .map(|r| vec![Item::ModifyRelation(r.clone())]),
        );
    }

    // Deletes.
    for d in diffs {
        atoms.extend(
            d.deleted_relation_ids
                .iter()
                .map(|&(id, v)| vec![Item::DeleteRelation(id, v)]),
        );
    }
    for d in diffs {
        atoms.extend(
            d.deleted_way_ids
                .iter()
                .map(|&(id, v)| vec![Item::DeleteWay(id, v)]),
        );
    }
    for d in diffs {
        atoms.extend(
            d.deleted_node_ids
                .iter()
                .map(|&(id, v)| vec![Item::DeleteNode(id, v)]),
        );
    }
    atoms
}

/// Split the pending `diffs` into chunks of at most `max` elements, to be
/// uploaded in order (see the module comment for why that order is valid).
/// Nothing pending gives no chunks; edits that fit in one chunk give exactly
/// that, untouched.
pub fn plan_chunks(diffs: &[&LayerDiff], max: usize) -> Result<Vec<LayerDiff>, PlanError> {
    let max = max.max(1);
    let total: usize = diffs.iter().map(|d| element_count(d)).sum();
    if total == 0 {
        return Ok(Vec::new());
    }
    if total <= max {
        return Ok(vec![merge(diffs)]);
    }

    // Layers keep their data in hash maps, so the diff lists come in no
    // particular order. Settle one (new objects in creation order, the rest
    // by id) so the plan, and the history it produces, is the same every time.
    let mut all = merge(diffs);
    all.created_nodes.sort_by_key(|n| std::cmp::Reverse(n.id));
    all.created_ways.sort_by_key(|w| std::cmp::Reverse(w.id));
    all.modified_nodes.sort_by_key(|n| n.id);
    all.modified_ways.sort_by_key(|w| w.id);
    all.modified_relations.sort_by_key(|r| r.id);
    all.deleted_relation_ids.sort_unstable();
    all.deleted_way_ids.sort_unstable();
    all.deleted_node_ids.sort_unstable();

    let mut chunks = Vec::new();
    let mut current = LayerDiff::default();
    let mut size = 0;
    for atom in sequence(&[&all]) {
        if atom.len() > max {
            return Err(PlanError::CycleTooLarge {
                size: atom.len(),
                max,
            });
        }
        if size + atom.len() > max {
            chunks.push(std::mem::take(&mut current));
            size = 0;
        }
        size += atom.len();
        for item in atom {
            item.push_into(&mut current);
        }
    }
    if size > 0 {
        chunks.push(current);
    }
    Ok(chunks)
}

/// The ids the server has assigned so far, for rewriting later chunks.
#[derive(Debug, Default)]
pub struct IdMap {
    nodes: HashMap<i64, i64>,
    ways: HashMap<i64, i64>,
    relations: HashMap<i64, i64>,
}

impl IdMap {
    /// Remember the server ids a chunk's `diffResult` assigned to its created
    /// objects. Modified objects keep their id and need no entry.
    pub fn absorb(&mut self, result: &UploadResult) {
        for (map, remap) in [
            (&mut self.nodes, &result.node_id_remap),
            (&mut self.ways, &result.way_id_remap),
            (&mut self.relations, &result.relation_id_remap),
        ] {
            for (&old, &(new, _)) in remap {
                if old != new {
                    let previous = map.insert(old, new);
                    debug_assert!(previous.is_none(), "placeholder {old} answered twice");
                }
            }
        }
    }

    /// Replace placeholder references in `chunk` (way node lists and relation
    /// members, created or modified) with the server ids learned so far.
    pub fn rewrite(&self, chunk: &mut LayerDiff) {
        for way in chunk
            .created_ways
            .iter_mut()
            .chain(chunk.modified_ways.iter_mut())
        {
            for nd in &mut way.nodes {
                if let Some(&id) = self.nodes.get(nd) {
                    *nd = id;
                }
            }
        }
        for relation in chunk
            .created_relations
            .iter_mut()
            .chain(chunk.modified_relations.iter_mut())
        {
            for member in &mut relation.members {
                let map = match member.member_type {
                    MemberType::Node => &self.nodes,
                    MemberType::Way => &self.ways,
                    MemberType::Relation => &self.relations,
                };
                if let Some(&id) = map.get(&member.reference) {
                    member.reference = id;
                }
            }
        }
    }
}

/// A chunk the server accepted.
#[derive(Debug)]
pub struct CompletedChunk {
    pub changeset_id: u64,
    /// The chunk as it was sent: placeholder ids of earlier chunks already
    /// replaced by server ids, its own new objects still on placeholders
    /// (the keys of `result`).
    pub chunk: LayerDiff,
    pub result: UploadResult,
}

/// How far a chunked upload got.
#[derive(Debug)]
pub struct ChunkedUpload {
    /// Chunks planned (1 for an upload that fits in one changeset).
    pub total: usize,
    /// The chunks the server accepted, in order.
    pub completed: Vec<CompletedChunk>,
    /// Why the sequence stopped before the end, if it did. The chunks after
    /// `completed` were not sent (the one that failed was closed).
    pub failure: Option<UploadFailure>,
}

/// Which changeset of how many is being sent: `(index from 1, total)`. `None`
/// for an upload that is a single changeset.
pub type Part = Option<(usize, usize)>;

/// Upload `layers` in changesets of at most `max` elements, one after the
/// other, each opened, filled and closed by `run_upload` (so each is closed
/// even when its upload fails). Any error stops the sequence and is reported
/// in the result along with the chunks already accepted; a diff is never
/// re-sent here, so a lost response can't turn into duplicate creates.
pub fn run_chunked_upload(
    client: &dyn HttpClient,
    base_url: &str,
    token: &str,
    comment: &str,
    layers: &[(&str, LayerDiff)],
    max: usize,
    on_phase: &mut dyn FnMut(Part, TransferPhase),
) -> Result<ChunkedUpload, PlanError> {
    let diffs: Vec<&LayerDiff> = layers.iter().map(|(_, d)| d).collect();
    let chunks = plan_chunks(&diffs, max)?;
    let total = chunks.len();
    let mut out = ChunkedUpload {
        total,
        completed: Vec::new(),
        failure: None,
    };
    let mut ids = IdMap::default();
    for (i, mut chunk) in chunks.into_iter().enumerate() {
        let part: Part = (total > 1).then_some((i + 1, total));
        ids.rewrite(&mut chunk);
        on_phase(part, TransferPhase::OpeningChangeset);
        let sent = [("upload", chunk)];
        match run_upload(client, base_url, token, comment, &sent, &mut |phase| {
            on_phase(part, phase)
        }) {
            Ok((changeset_id, result)) => {
                ids.absorb(&result);
                let [(_, chunk)] = sent;
                out.completed.push(CompletedChunk {
                    changeset_id,
                    chunk,
                    result,
                });
            }
            Err(failure) => {
                out.failure = Some(failure);
                break;
            }
        }
    }
    Ok(out)
}

/// One `UploadResult` holding every completed chunk's answers.
pub fn merge_results(completed: &[CompletedChunk]) -> UploadResult {
    let mut merged = UploadResult::default();
    for c in completed {
        merged
            .node_id_remap
            .extend(c.result.node_id_remap.iter().map(|(k, v)| (*k, *v)));
        merged
            .way_id_remap
            .extend(c.result.way_id_remap.iter().map(|(k, v)| (*k, *v)));
        merged
            .relation_id_remap
            .extend(c.result.relation_id_remap.iter().map(|(k, v)| (*k, *v)));
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, status_err, FakeClient};
    use crate::osm::OsmMember;
    use crate::osm_upload::{build_osm_change_xml, ConflictKind, UploadError};

    const BASE: &str = "https://osm.test";

    fn node(id: i64) -> OsmNode {
        OsmNode {
            id,
            lat: 1.0,
            lon: 2.0,
            version: if id > 0 { 1 } else { 0 },
            tags: HashMap::new(),
        }
    }

    fn way(id: i64, nodes: &[i64]) -> OsmWay {
        OsmWay {
            id,
            nodes: nodes.to_vec(),
            version: if id > 0 { 1 } else { 0 },
            tags: HashMap::new(),
        }
    }

    fn member(kind: MemberType, reference: i64) -> OsmMember {
        OsmMember {
            member_type: kind,
            reference,
            role: String::new(),
        }
    }

    fn relation(id: i64, members: Vec<OsmMember>) -> OsmRelation {
        OsmRelation {
            id,
            members,
            version: if id > 0 { 1 } else { 0 },
            tags: HashMap::new(),
        }
    }

    /// The (kind, id) of every element of `chunk` in upload order.
    fn order(chunk: &LayerDiff) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(chunk.created_nodes.iter().map(|n| format!("cn{}", n.id)));
        out.extend(chunk.created_ways.iter().map(|w| format!("cw{}", w.id)));
        out.extend(
            chunk
                .created_relations
                .iter()
                .map(|r| format!("cr{}", r.id)),
        );
        out.extend(chunk.modified_nodes.iter().map(|n| format!("mn{}", n.id)));
        out.extend(chunk.modified_ways.iter().map(|w| format!("mw{}", w.id)));
        out.extend(
            chunk
                .modified_relations
                .iter()
                .map(|r| format!("mr{}", r.id)),
        );
        out.extend(
            chunk
                .deleted_relation_ids
                .iter()
                .map(|d| format!("dr{}", d.0)),
        );
        out.extend(chunk.deleted_way_ids.iter().map(|d| format!("dw{}", d.0)));
        out.extend(chunk.deleted_node_ids.iter().map(|d| format!("dn{}", d.0)));
        out
    }

    /// 6 new nodes (-1..-6), 2 new ways over them, 2 modified nodes, 2 deletes.
    fn mixed_diff() -> LayerDiff {
        LayerDiff {
            created_nodes: (1..=6).map(|i| node(-i)).collect(),
            created_ways: vec![way(-1, &[-1, -2, -3]), way(-2, &[-4, -5, -6])],
            modified_nodes: vec![node(10), node(11)],
            deleted_way_ids: vec![(20, 1)],
            deleted_node_ids: vec![(30, 1)],
            ..Default::default()
        }
    }

    #[test]
    fn nothing_pending_plans_nothing() {
        assert!(plan_chunks(&[&LayerDiff::default()], 5).unwrap().is_empty());
        assert!(plan_chunks(&[], 5).unwrap().is_empty());
    }

    #[test]
    fn edits_that_fit_are_one_untouched_chunk() {
        let d = mixed_diff();
        let chunks = plan_chunks(&[&d], 13).unwrap();
        assert_eq!(chunks.len(), 1);
        let layers = [("L", d.clone())];
        assert_eq!(
            build_osm_change_xml(7, &[("x", chunks[0].clone())]),
            build_osm_change_xml(7, &layers)
        );
    }

    #[test]
    fn splits_at_the_limit_in_upload_order() {
        let d = mixed_diff(); // 12 elements
        let chunks = plan_chunks(&[&d], 5).unwrap();
        let sizes: Vec<usize> = chunks.iter().map(element_count).collect();
        assert_eq!(sizes, vec![5, 5, 2]);
        // Creates, then modifies, then deletes, across the cuts; each way is
        // laid out right after its own nodes (within a chunk the document
        // lists nodes before ways).
        assert_eq!(
            order(&chunks[0]),
            vec!["cn-1", "cn-2", "cn-3", "cn-4", "cw-1"]
        );
        assert_eq!(
            order(&chunks[1]),
            vec!["cn-5", "cn-6", "cw-2", "mn10", "mn11"]
        );
        assert_eq!(order(&chunks[2]), vec!["dw20", "dn30"]);
    }

    #[test]
    fn deletes_and_modifies_of_each_kind_keep_the_global_order() {
        let d = LayerDiff {
            created_relations: vec![relation(-1, vec![member(MemberType::Node, 1)])],
            modified_nodes: vec![node(1)],
            modified_ways: vec![way(2, &[1, 1])],
            modified_relations: vec![relation(3, vec![])],
            deleted_relation_ids: vec![(4, 1)],
            deleted_way_ids: vec![(5, 1)],
            deleted_node_ids: vec![(6, 1)],
            ..Default::default()
        };
        let chunks = plan_chunks(&[&d], 2).unwrap();
        let flat: Vec<String> = chunks.iter().flat_map(order).collect();
        assert_eq!(flat, vec!["cr-1", "mn1", "mw2", "mr3", "dr4", "dw5", "dn6"]);
        assert!(chunks.iter().all(|c| element_count(c) <= 2));
    }

    #[test]
    fn layers_are_merged_into_the_same_sequence() {
        let a = LayerDiff {
            created_nodes: vec![node(-1)],
            deleted_node_ids: vec![(5, 1)],
            ..Default::default()
        };
        let b = LayerDiff {
            created_nodes: vec![node(-2)],
            deleted_node_ids: vec![(6, 1)],
            ..Default::default()
        };
        let chunks = plan_chunks(&[&a, &b], 3).unwrap();
        let flat: Vec<String> = chunks.iter().flat_map(order).collect();
        assert_eq!(flat, vec!["cn-1", "cn-2", "dn5", "dn6"]);
    }

    #[test]
    fn a_relation_that_contains_another_new_relation_comes_after_it() {
        // -1 contains -2 contains -3; listed in the wrong order, with limit 1
        // so every relation is its own chunk.
        let d = LayerDiff {
            created_relations: vec![
                relation(-1, vec![member(MemberType::Relation, -2)]),
                relation(-3, vec![]),
                relation(-2, vec![member(MemberType::Relation, -3)]),
            ],
            ..Default::default()
        };
        let chunks = plan_chunks(&[&d], 1).unwrap();
        let flat: Vec<String> = chunks.iter().flat_map(order).collect();
        assert_eq!(flat, vec!["cr-3", "cr-2", "cr-1"]);
    }

    #[test]
    fn relations_in_a_cycle_stay_in_one_chunk() {
        // -1 and -2 contain each other; -3 is independent; -4 contains -1.
        let d = LayerDiff {
            created_relations: vec![
                relation(-4, vec![member(MemberType::Relation, -1)]),
                relation(-1, vec![member(MemberType::Relation, -2)]),
                relation(-2, vec![member(MemberType::Relation, -1)]),
                relation(-3, vec![]),
            ],
            ..Default::default()
        };
        let chunks = plan_chunks(&[&d], 2).unwrap();
        let positions: HashMap<String, usize> = chunks
            .iter()
            .enumerate()
            .flat_map(|(i, c)| order(c).into_iter().map(move |e| (e, i)))
            .collect();
        assert_eq!(positions["cr-1"], positions["cr-2"]);
        assert!(positions["cr-4"] > positions["cr-1"]);
        assert!(chunks.iter().all(|c| element_count(c) <= 2));
    }

    #[test]
    fn a_cycle_bigger_than_a_changeset_is_refused() {
        let d = LayerDiff {
            created_relations: vec![
                relation(-1, vec![member(MemberType::Relation, -2)]),
                relation(-2, vec![member(MemberType::Relation, -3)]),
                relation(-3, vec![member(MemberType::Relation, -1)]),
            ],
            created_nodes: vec![node(-1)],
            ..Default::default()
        };
        let err = plan_chunks(&[&d], 2).unwrap_err();
        assert_eq!(err, PlanError::CycleTooLarge { size: 3, max: 2 });
        assert!(err.to_string().contains('3') && err.to_string().contains('2'));
        // It fits when the limit allows the whole thing.
        assert!(plan_chunks(&[&d], 4).is_ok());
    }

    /// The validity argument, checked on a larger shuffled example: walking
    /// the chunks in order, every reference is to an object that already
    /// exists (created in this or an earlier chunk, or pre-existing), and
    /// nothing is deleted while a later element still refers to it.
    #[test]
    fn every_reference_points_backwards_across_chunks() {
        let mut d = LayerDiff::default();
        for i in 1..=20 {
            d.created_nodes.push(node(-i));
        }
        for i in 1..=5i64 {
            let nodes: Vec<i64> = ((i - 1) * 4 + 1..=i * 4).map(|n| -n).collect();
            d.created_ways.push(way(-i, &nodes));
        }
        d.created_relations = vec![
            relation(
                -1,
                vec![
                    member(MemberType::Relation, -2),
                    member(MemberType::Way, -1),
                    member(MemberType::Node, -20),
                ],
            ),
            relation(-2, vec![member(MemberType::Way, -5)]),
        ];
        d.modified_ways = vec![way(100, &[-3, 7])];
        d.modified_relations = vec![relation(
            200,
            vec![
                member(MemberType::Relation, -1),
                member(MemberType::Way, 100),
            ],
        )];
        d.deleted_way_ids = vec![(300, 1)];
        d.deleted_node_ids = vec![(7, 1)];
        let total = element_count(&d);
        for max in [3, 4, 7, 10] {
            let chunks = plan_chunks(&[&d], max).unwrap();
            assert_eq!(chunks.iter().map(element_count).sum::<usize>(), total);
            let mut nodes: HashSet<i64> = HashSet::new();
            let mut ways: HashSet<i64> = HashSet::new();
            let mut rels: HashSet<i64> = HashSet::new();
            let known = |set: &HashSet<i64>, id: i64| id > 0 || set.contains(&id);
            for c in &chunks {
                assert!(element_count(c) <= max);
                nodes.extend(c.created_nodes.iter().map(|n| n.id));
                for w in &c.created_ways {
                    assert!(w.nodes.iter().all(|&n| known(&nodes, n)), "max {max}");
                    ways.insert(w.id);
                }
                for r in &c.created_relations {
                    rels.insert(r.id);
                }
                for r in c.created_relations.iter().chain(&c.modified_relations) {
                    for m in &r.members {
                        let set = match m.member_type {
                            MemberType::Node => &nodes,
                            MemberType::Way => &ways,
                            MemberType::Relation => &rels,
                        };
                        // A relation in a cycle may name a sibling; none here.
                        assert!(known(set, m.reference), "max {max}: {m:?}");
                    }
                }
                for w in &c.modified_ways {
                    assert!(w.nodes.iter().all(|&n| known(&nodes, n)));
                }
            }
            // Deletes are all in the last chunks, after every create/modify.
            let last_non_delete = chunks
                .iter()
                .rposition(|c| {
                    !c.created_nodes.is_empty()
                        || !c.modified_ways.is_empty()
                        || !c.modified_relations.is_empty()
                })
                .unwrap();
            let first_delete = chunks
                .iter()
                .position(|c| !c.deleted_way_ids.is_empty() || !c.deleted_node_ids.is_empty())
                .unwrap();
            assert!(first_delete >= last_non_delete);
        }
    }

    #[test]
    fn each_object_is_in_exactly_one_chunk() {
        let d = mixed_diff();
        let chunks = plan_chunks(&[&d], 4).unwrap();
        let all: Vec<String> = chunks.iter().flat_map(order).collect();
        let unique: HashSet<&String> = all.iter().collect();
        assert_eq!(all.len(), unique.len());
        assert_eq!(all.len(), element_count(&d));
    }

    // --- id rewriting and the upload loop ---------------------------------

    fn rewritten_ids() -> IdMap {
        let mut ids = IdMap::default();
        let mut r = UploadResult::default();
        r.node_id_remap.insert(-1, (9001, 1));
        r.node_id_remap.insert(50, (50, 4)); // a modify: no entry needed
        r.way_id_remap.insert(-1, (8001, 1));
        r.relation_id_remap.insert(-1, (7001, 1));
        ids.absorb(&r);
        ids
    }

    #[test]
    fn later_chunks_get_server_ids_for_earlier_placeholders() {
        let ids = rewritten_ids();
        let mut chunk = LayerDiff {
            created_ways: vec![way(-2, &[-1, 12, -9])],
            modified_ways: vec![way(5, &[-1, 50])],
            created_relations: vec![relation(
                -2,
                vec![
                    member(MemberType::Node, -1),
                    member(MemberType::Way, -1),
                    member(MemberType::Relation, -1),
                    member(MemberType::Way, -2),
                ],
            )],
            ..Default::default()
        };
        ids.rewrite(&mut chunk);
        assert_eq!(chunk.created_ways[0].nodes, vec![9001, 12, -9]);
        assert_eq!(chunk.modified_ways[0].nodes, vec![9001, 50]);
        let refs: Vec<i64> = chunk.created_relations[0]
            .members
            .iter()
            .map(|m| m.reference)
            .collect();
        // Node -1 and way -1 are answered; way -2 is this chunk's own.
        assert_eq!(refs, vec![9001, 8001, 7001, -2]);
    }

    fn diff_result(nodes: &[(i64, i64)], ways: &[(i64, i64)]) -> String {
        let mut s = String::from("<diffResult>");
        for (old, new) in nodes {
            s.push_str(&format!(
                r#"<node old_id="{old}" new_id="{new}" new_version="1"/>"#
            ));
        }
        for (old, new) in ways {
            s.push_str(&format!(
                r#"<way old_id="{old}" new_id="{new}" new_version="1"/>"#
            ));
        }
        s.push_str("</diffResult>");
        s
    }

    fn upload_bodies(client: &FakeClient) -> Vec<String> {
        client
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.url.ends_with("/upload"))
            .map(|r| match &r.body {
                Some(crate::http::Body::Text(xml)) => xml.clone(),
                other => panic!("no xml body: {other:?}"),
            })
            .collect()
    }

    /// Three nodes (-1,-2,-3) and a way over them, limit 2: chunk 1 holds
    /// nodes -1 and -2, chunk 2 node -3 and the way. 4 elements, 2 chunks.
    fn node_way_diff() -> LayerDiff {
        LayerDiff {
            created_nodes: vec![node(-1), node(-2), node(-3)],
            created_ways: vec![way(-1, &[-1, -2, -3])],
            ..Default::default()
        }
    }

    #[test]
    fn a_way_in_a_later_chunk_is_sent_with_the_server_ids_of_its_nodes() {
        let client = FakeClient::new(vec![
            ok(200, "101"),
            ok(200, diff_result(&[(-1, 5001), (-2, 5002)], &[])),
            ok(200, ""),
            ok(200, "102"),
            ok(200, diff_result(&[(-3, 5003)], &[(-1, 6001)])),
            ok(200, ""),
        ]);
        let layers = [("L", node_way_diff())];
        let mut seen = Vec::new();
        let out = run_chunked_upload(&client, BASE, "tok", "c", &layers, 2, &mut |part, p| {
            seen.push((part, p))
        })
        .unwrap();
        assert!(out.failure.is_none());
        assert_eq!(out.total, 2);
        assert_eq!(out.completed.len(), 2);
        let bodies = upload_bodies(&client);
        assert!(bodies[0].contains(r#"<node id="-1""#) && bodies[0].contains(r#"changeset="101""#));
        // Chunk 2: the way names nodes 5001, 5002 (earlier chunk) and -3 (own).
        assert!(
            bodies[1].contains(r#"<nd ref="5001"/><nd ref="5002"/><nd ref="-3"/>"#),
            "{}",
            bodies[1]
        );
        assert!(bodies[1].contains(r#"changeset="102""#));
        // Progress carries the part, one changeset opened per chunk.
        assert_eq!(seen[0].0, Some((1, 2)));
        assert!(seen.iter().any(|(p, _)| *p == Some((2, 2))));
        let lines: Vec<String> = client
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.url.clone())
            .collect();
        assert_eq!(lines.iter().filter(|u| u.ends_with("/create")).count(), 2);
        assert_eq!(lines.iter().filter(|u| u.ends_with("/close")).count(), 2);
        let merged = merge_results(&out.completed);
        assert_eq!(merged.node_id_remap.len(), 3);
        assert_eq!(merged.way_id_remap.get(&-1), Some(&(6001, 1)));
    }

    #[test]
    fn small_uploads_are_one_changeset_with_the_same_osm_change_as_before() {
        let d = mixed_diff();
        let client = FakeClient::new(vec![ok(200, "7"), ok(200, "<diffResult/>"), ok(200, "")]);
        let layers = [("L", d.clone())];
        let mut parts = Vec::new();
        let out = run_chunked_upload(
            &client,
            BASE,
            "tok",
            "c",
            &layers,
            10_000,
            &mut |part, _| parts.push(part),
        )
        .unwrap();
        assert_eq!(out.total, 1);
        assert!(parts.iter().all(|p| p.is_none()));
        assert_eq!(client.request_count(), 3);
        assert_eq!(
            upload_bodies(&client),
            vec![build_osm_change_xml(7, &layers)]
        );
    }

    #[test]
    fn a_failure_stops_the_sequence_and_keeps_the_finished_chunks() {
        let client = FakeClient::new(vec![
            ok(200, "101"),
            ok(200, diff_result(&[(-1, 5001), (-2, 5002)], &[])),
            ok(200, ""),
            ok(200, "102"),
            status_err(409, "Version mismatch: Provided 1, server had: 2 of Node 5"),
            ok(200, ""),
        ]);
        let layers = [("L", node_way_diff())];
        let out =
            run_chunked_upload(&client, BASE, "tok", "c", &layers, 2, &mut |_, _| {}).unwrap();
        assert_eq!(out.completed.len(), 1);
        assert_eq!(out.completed[0].changeset_id, 101);
        let failure = out.failure.unwrap();
        assert_eq!(failure.changeset_id, Some(102));
        match failure.error {
            UploadError::Conflict(c) => {
                assert!(matches!(c.kind, ConflictKind::VersionMismatch { .. }))
            }
            other => panic!("{other:?}"),
        }
        // create, upload, close, create, upload (once), close: no re-send.
        assert_eq!(client.request_count(), 6);
        assert_eq!(upload_bodies(&client).len(), 2);
    }

    #[test]
    fn a_transport_failure_is_not_retried_into_another_chunk() {
        let client = FakeClient::new(vec![ok(200, "101"), status_err(400, "bad"), ok(200, "")]);
        let layers = [("L", node_way_diff())];
        let out =
            run_chunked_upload(&client, BASE, "tok", "c", &layers, 2, &mut |_, _| {}).unwrap();
        assert!(out.completed.is_empty());
        assert!(out.failure.is_some());
        assert_eq!(upload_bodies(&client).len(), 1);
    }
}
