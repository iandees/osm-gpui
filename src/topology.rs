//! Pure topology logic behind the join / separate / extend selection actions:
//! chaining ways end to end, merging their tags, splitting a shared node, and
//! finding which end of a way can be continued. Nothing here touches a layer
//! or the UI, so it is all unit-testable on plain ids and node lists.

use std::collections::{BTreeMap, VecDeque};

/// A way as the join planner sees it.
#[derive(Clone, Debug)]
pub struct JoinWay {
    pub id: i64,
    pub nodes: Vec<i64>,
    pub tags: Vec<(String, String)>,
}

/// Why a set of ways can't be joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinError {
    /// The ways can't all be chained end to end through shared end nodes.
    NotChainable,
    /// Two ways have different values for this key.
    TagConflict(String),
    /// Joining would reverse this way, but its tags depend on its direction.
    NeedsReversal(i64),
}

impl JoinError {
    /// A short status-bar message.
    pub fn message(&self) -> String {
        match self {
            JoinError::NotChainable => crate::tr!("action.join.error.not_chainable").to_string(),
            JoinError::TagConflict(key) => {
                crate::tr!("action.join.error.tag_conflict", key = key).to_string()
            }
            JoinError::NeedsReversal(id) => {
                crate::tr!("action.join.error.needs_reversal", id = id).to_string()
            }
        }
    }
}

/// The outcome of joining: which way survives with which nodes and tags, and
/// which ways go away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinPlan {
    /// The surviving way.
    pub keep: i64,
    /// Its new node list, shared end nodes appearing once.
    pub nodes: Vec<i64>,
    /// Its new tags: the union of every way's tags, sorted by key.
    pub tags: Vec<(String, String)>,
    /// The ways to delete, in selection order.
    pub remove: Vec<i64>,
}

/// Whether a way's tags change meaning if its node order is reversed.
pub fn is_direction_dependent(tags: &[(String, String)]) -> bool {
    tags.iter().any(|(k, v)| match k.as_str() {
        "oneway" => v != "no",
        "direction" | "incline" | "conveying" => true,
        k if k.starts_with("oneway:") => v != "no",
        k => k.contains(":forward") || k.contains(":backward"),
    })
}

/// One tag of a feature changing: the key, its value before and its value
/// after (`None` = absent).
pub type TagChange = (String, Option<String>, Option<String>);

/// Swap `forward`/`backward` and `left`/`right` in a key, but only when they
/// are a `:`-separated part after the first (`lanes:forward`,
/// `cycleway:left:width`), so a plain `left` or `forward` key is untouched.
fn flip_key(key: &str) -> String {
    key.split(':')
        .enumerate()
        .map(|(i, part)| match (i, part) {
            (0, _) => part,
            (_, "forward") => "backward",
            (_, "backward") => "forward",
            (_, "left") => "right",
            (_, "right") => "left",
            _ => part,
        })
        .collect::<Vec<_>>()
        .join(":")
}

/// Whether the value of `key` describes a direction relative to the way, i.e.
/// some part of the key is `direction`, `incline` or `side`.
fn has_directional_value(key: &str) -> bool {
    key.split(':')
        .any(|part| matches!(part, "direction" | "incline" | "side"))
}

/// Flip a direction-relative value: `forward`/`backward`, `left`/`right`,
/// `up`/`down`, and the sign of a numeric incline (`10%`, `-4°`, `5`).
fn flip_value(value: &str) -> String {
    match value {
        "forward" => "backward".to_string(),
        "backward" => "forward".to_string(),
        "left" => "right".to_string(),
        "right" => "left".to_string(),
        "up" => "down".to_string(),
        "down" => "up".to_string(),
        _ => {
            let unsigned = value.strip_prefix(['-', '+']).unwrap_or(value);
            let digits = unsigned.trim_end_matches(['%', '°']);
            let numeric = !digits.is_empty()
                && digits.parse::<f64>().is_ok()
                && digits.chars().all(|c| c.is_ascii_digit() || c == '.');
            if !numeric || digits.parse::<f64>() == Ok(0.0) {
                value.to_string()
            } else if value.starts_with('-') {
                unsigned.to_string()
            } else {
                format!("-{unsigned}")
            }
        }
    }
}

/// Keys (by their first `:`-part) whose values are free text, names or
/// identifiers, or deliberately keep their meaning under Reverse, so a value
/// that merely looks directional (`name=Left`, `conveying=forward`) is left
/// alone.
fn is_free_text_key(base: &str) -> bool {
    matches!(
        base,
        "name"
            | "alt_name"
            | "old_name"
            | "loc_name"
            | "short_name"
            | "official_name"
            | "int_name"
            | "nat_name"
            | "reg_name"
            | "ref"
            | "old_ref"
            | "note"
            | "description"
            | "fixme"
            | "FIXME"
            | "inscription"
            | "comment"
            | "source"
            | "type"
            | "operator"
            | "brand"
            | "network"
            | "destination"
            | "website"
            | "email"
            | "phone"
            | "image"
            | "wikipedia"
            | "wikidata"
            | "oneway"
            | "conveying"
            | "addr"
    )
}

/// The new value of `key=value` when the way is reversed.
fn reversed_value(key: &str, value: &str) -> String {
    if has_directional_value(key) {
        // A numeric `direction` is a compass bearing, which is absolute and
        // must not change sign; only a numeric incline is negated.
        let is_incline = key.split(':').any(|p| p == "incline");
        let is_word = matches!(
            value,
            "forward" | "backward" | "left" | "right" | "up" | "down"
        );
        if !is_incline && !is_word {
            return value.to_string();
        }
        return flip_value(value);
    }
    let mut parts = key.split(':');
    let base = parts.next().unwrap_or(key);
    let rest: Vec<&str> = parts.collect();
    // A side-specific oneway (`cycleway:left:oneway=-1`): the side swaps, and
    // so does the direction the lane runs relative to the way.
    if rest.last() == Some(&"oneway") && rest.iter().any(|p| matches!(*p, "left" | "right")) {
        return match value {
            "yes" => "-1".to_string(),
            "-1" => "yes".to_string(),
            _ => value.to_string(),
        };
    }
    if is_free_text_key(base) {
        return value.to_string();
    }
    match value {
        "forward" | "backward" | "left" | "right" => flip_value(value),
        _ => value.to_string(),
    }
}

fn changes_between(
    before: &BTreeMap<&str, &str>,
    after: &BTreeMap<String, String>,
) -> Vec<TagChange> {
    let mut keys: Vec<&str> = before.keys().copied().collect();
    keys.extend(after.keys().map(String::as_str));
    keys.sort_unstable();
    keys.dedup();
    keys.into_iter()
        .filter_map(|key| {
            let old = before.get(key).map(|v| v.to_string());
            let new = after.get(key).cloned();
            (old != new).then(|| (key.to_string(), old, new))
        })
        .collect()
}

/// The tag changes that keep a way's meaning when its node order is reversed
/// (what iD's and JOSM's Reverse do besides reordering the nodes):
///
/// - in keys, a `:forward` part becomes `:backward` and vice versa, and
///   `:left` becomes `:right` and vice versa (`lanes:forward`,
///   `cycleway:left`, `parking:lane:right:parallel`);
/// - in the values of `direction`, `incline` and `side` keys (including
///   prefixed ones like `traffic_signals:direction`), `forward`/`backward`,
///   `left`/`right` and `up`/`down` swap, and a numeric incline changes sign
///   (`10%` becomes `-10%`);
/// - in the values of any other key, a value that is exactly `forward`,
///   `backward`, `left` or `right` swaps too (`sidewalk=left`,
///   `priority=forward`), except for free-text keys like `name` or `note`;
/// - a side-specific oneway (`cycleway:left:oneway=-1`) flips `yes` and `-1`
///   along with the side;
/// - `oneway` itself is deliberately left alone: reversing a `oneway=yes` way
///   is how the user turns the traffic around, so the tag keeps its meaning.
///
/// Returns only the keys that end up different, sorted by key.
pub fn reversed_tag_changes(tags: &[(String, String)]) -> Vec<TagChange> {
    let before: BTreeMap<&str, &str> = tags.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut after: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in &before {
        after.insert(flip_key(key), reversed_value(key, value));
    }
    changes_between(&before, &after)
}

/// The tag changes for a node on a way that is being reversed: a facing
/// `direction=forward`/`backward` (also `traffic_signals:direction` and
/// similar) swaps, as do `:forward`/`:backward` key parts such as
/// `traffic_sign:forward`. Compass and degree directions are absolute and
/// stay. Only the keys that end up different are returned, sorted by key.
pub fn reversed_node_tag_changes(tags: &[(String, String)]) -> Vec<TagChange> {
    let before: BTreeMap<&str, &str> = tags.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut after: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in &before {
        let new_key = key
            .split(':')
            .enumerate()
            .map(|(i, part)| match (i, part) {
                (0, _) => part,
                (_, "forward") => "backward",
                (_, "backward") => "forward",
                _ => part,
            })
            .collect::<Vec<_>>()
            .join(":");
        let new_value = if key.split(':').any(|p| p == "direction")
            && matches!(*value, "forward" | "backward")
        {
            flip_value(value)
        } else {
            value.to_string()
        };
        after.insert(new_key, new_value);
    }
    changes_between(&before, &after)
}

/// Order and orient `ways` into one chain: each entry is a way index and
/// whether it is traversed against its own node order. `None` if the ways
/// can't all be chained end to end.
fn chain_order(ways: &[JoinWay]) -> Option<VecDeque<(usize, bool)>> {
    if ways.len() < 2 || ways.iter().any(|w| w.nodes.len() < 2) {
        return None;
    }
    let mut used = vec![false; ways.len()];
    used[0] = true;
    let mut order = VecDeque::from([(0, false)]);
    let head = ways[0].nodes[0];
    let tail = *ways[0].nodes.last()?;
    extend_chain(ways, &mut used, &mut order, head, tail).then_some(order)
}

/// Depth-first search for a way to attach to either end of the chain; ends
/// shared by more than two ways are resolved by backtracking.
fn extend_chain(
    ways: &[JoinWay],
    used: &mut [bool],
    order: &mut VecDeque<(usize, bool)>,
    head: i64,
    tail: i64,
) -> bool {
    if order.len() == ways.len() {
        return true;
    }
    // A closed ring has no free end left.
    if head == tail {
        return false;
    }
    for i in 0..ways.len() {
        if used[i] {
            continue;
        }
        let first = ways[i].nodes[0];
        let last = *ways[i].nodes.last().unwrap();
        // (attach at the back?, reversed?, new head, new tail)
        let mut options = Vec::new();
        if first == tail {
            options.push((true, false, head, last));
        } else if last == tail {
            options.push((true, true, head, first));
        }
        if last == head {
            options.push((false, false, first, tail));
        } else if first == head {
            options.push((false, true, last, tail));
        }
        for (at_back, reversed, new_head, new_tail) in options {
            used[i] = true;
            if at_back {
                order.push_back((i, reversed));
            } else {
                order.push_front((i, reversed));
            }
            if extend_chain(ways, used, order, new_head, new_tail) {
                return true;
            }
            if at_back {
                order.pop_back();
            } else {
                order.pop_front();
            }
            used[i] = false;
        }
    }
    false
}

/// Whether `ways` form one end-to-end chain (ignoring tags), i.e. whether
/// joining is worth offering.
pub fn can_chain(ways: &[JoinWay]) -> bool {
    chain_order(ways).is_some()
}

/// Plan joining `ways` (in selection order) into one. The way that already
/// exists on the server (positive id) survives if exactly one does; otherwise
/// the first selected.
pub fn plan_join(ways: &[JoinWay]) -> Result<JoinPlan, JoinError> {
    let order = chain_order(ways).ok_or(JoinError::NotChainable)?;

    // Union of tags; differing values for one key block the join.
    let merged = union_tags(ways.iter().map(|w| w.tags.as_slice()))
        .map_err(|key| JoinError::TagConflict(key.to_string()))?;

    // The chain can be walked either way round; pick the direction that
    // doesn't reverse any way whose tags depend on direction, preferring
    // fewer reversals.
    let reversed_forward: Vec<usize> = order.iter().filter(|e| e.1).map(|e| e.0).collect();
    let reversed_backward: Vec<usize> = order.iter().filter(|e| !e.1).map(|e| e.0).collect();
    let offending = |reversed: &[usize]| {
        reversed
            .iter()
            .find(|&&i| is_direction_dependent(&ways[i].tags))
            .map(|&i| ways[i].id)
    };
    let flip = match (offending(&reversed_forward), offending(&reversed_backward)) {
        (None, None) => reversed_backward.len() < reversed_forward.len(),
        (None, Some(_)) => false,
        (Some(_), None) => true,
        (Some(id), Some(_)) => return Err(JoinError::NeedsReversal(id)),
    };

    let mut nodes: Vec<i64> = Vec::new();
    for &(i, reversed) in &order {
        let mut part = ways[i].nodes.clone();
        if reversed {
            part.reverse();
        }
        // The shared end node is already there from the previous way.
        nodes.extend(part.into_iter().skip(usize::from(!nodes.is_empty())));
    }
    if flip {
        nodes.reverse();
    }

    let positive: Vec<i64> = ways.iter().map(|w| w.id).filter(|&id| id > 0).collect();
    let keep = if positive.len() == 1 {
        positive[0]
    } else {
        ways[0].id
    };
    Ok(JoinPlan {
        keep,
        nodes,
        tags: merged,
        remove: ways.iter().map(|w| w.id).filter(|&id| id != keep).collect(),
    })
}

/// The union of several features' tags, sorted by key. If two features have
/// different values for one key, the (alphabetically first) such key is the
/// error.
fn union_tags<'a>(
    sets: impl Iterator<Item = &'a [(String, String)]>,
) -> Result<Vec<(String, String)>, String> {
    let mut merged: BTreeMap<&str, &str> = BTreeMap::new();
    let mut conflict: Option<&str> = None;
    for (k, v) in sets.flatten() {
        match merged.get(k.as_str()) {
            Some(existing) if *existing != v.as_str() => {
                if conflict.is_none_or(|c| k.as_str() < c) {
                    conflict = Some(k.as_str());
                }
            }
            _ => {
                merged.insert(k.as_str(), v.as_str());
            }
        }
    }
    match conflict {
        Some(key) => Err(key.to_string()),
        None => Ok(merged
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()),
    }
}

/// A node as the merge planner sees it.
#[derive(Clone, Debug)]
pub struct MergeNode {
    pub id: i64,
    pub tags: Vec<(String, String)>,
}

/// Why a set of nodes can't be merged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeError {
    /// Fewer than two distinct nodes.
    NotEnoughNodes,
    /// Two nodes have different values for this key.
    TagConflict(String),
    /// The way would be left with fewer than two distinct nodes.
    DegenerateWay(i64),
}

impl MergeError {
    /// A short status-bar message.
    pub fn message(&self) -> String {
        match self {
            MergeError::NotEnoughNodes => crate::tr!("action.merge.hint").to_string(),
            MergeError::TagConflict(key) => {
                crate::tr!("action.merge.error.tag_conflict", key = key).to_string()
            }
            MergeError::DegenerateWay(id) => {
                crate::tr!("action.merge.error.degenerate_way", id = id).to_string()
            }
        }
    }
}

/// The outcome of merging nodes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergePlan {
    /// The surviving node (keeps its position).
    pub keep: i64,
    /// The nodes to delete, in selection order.
    pub remove: Vec<i64>,
    /// The survivor's tags afterwards: the union of every node's tags.
    pub tags: Vec<(String, String)>,
    /// Every way that used a removed node, with its new node list.
    pub way_edits: Vec<SeparateEdit>,
}

/// Plan merging `nodes` (in selection order, duplicates ignored) into one.
/// `ways` holds every way that uses any of them, as `(id, node list)`.
///
/// The node already on the server (positive id) survives if exactly one is;
/// otherwise the first selected. Tags are unioned (a conflict names its key).
/// Ways use the survivor in place of each removed node, and consecutive
/// repeats that creates collapse into one. Refused if a way would be left with
/// fewer than two distinct nodes, or a way that was closed with fewer than three.
/// (Relation memberships are the caller's
/// business: see `relations::merge_node_members`.)
pub fn plan_merge_nodes(
    nodes: &[MergeNode],
    ways: &[(i64, Vec<i64>)],
) -> Result<MergePlan, MergeError> {
    let mut unique: Vec<&MergeNode> = Vec::new();
    for node in nodes {
        if !unique.iter().any(|n| n.id == node.id) {
            unique.push(node);
        }
    }
    if unique.len() < 2 {
        return Err(MergeError::NotEnoughNodes);
    }
    let positive: Vec<i64> = unique.iter().map(|n| n.id).filter(|&id| id > 0).collect();
    let keep = if positive.len() == 1 {
        positive[0]
    } else {
        unique[0].id
    };
    let tags =
        union_tags(unique.iter().map(|n| n.tags.as_slice())).map_err(MergeError::TagConflict)?;
    let remove: Vec<i64> = unique
        .iter()
        .map(|n| n.id)
        .filter(|&id| id != keep)
        .collect();

    let mut way_edits = Vec::new();
    for (way_id, before) in ways {
        if !before.iter().any(|n| remove.contains(n)) {
            continue;
        }
        let mut after: Vec<i64> = Vec::with_capacity(before.len());
        for &node in before {
            let node = if remove.contains(&node) { keep } else { node };
            if after.last() != Some(&node) {
                after.push(node);
            }
        }
        let mut distinct = after.clone();
        distinct.sort_unstable();
        distinct.dedup();
        let closed = before.len() >= 3 && before.first() == before.last();
        if distinct.len() < if closed { 3 } else { 2 } {
            return Err(MergeError::DegenerateWay(*way_id));
        }
        way_edits.push(SeparateEdit {
            way_id: *way_id,
            before: before.clone(),
            after,
        });
    }
    Ok(MergePlan {
        keep,
        remove,
        tags,
        way_edits,
    })
}

/// One way's node list before and after separating a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeparateEdit {
    pub way_id: i64,
    pub before: Vec<i64>,
    pub after: Vec<i64>,
}

/// Each place `node` is used, as the way index and the positions in that
/// way's list. The ring-closing first/last pair of a closed way is one use.
/// Ways sort existing-on-server first then by creation order, with
/// `detach_only` last so the node stays with the other ways.
fn node_uses(
    node: i64,
    ways: &[(i64, Vec<i64>)],
    detach_only: Option<i64>,
) -> Vec<(usize, Vec<usize>)> {
    let mut idx: Vec<usize> = (0..ways.len()).collect();
    idx.sort_by_key(|&i| {
        let id = ways[i].0;
        (Some(id) == detach_only, id < 0, id.abs())
    });
    let mut uses = Vec::new();
    for i in idx {
        let nodes = &ways[i].1;
        let closed = nodes.len() >= 3 && nodes.first() == nodes.last();
        let mut at: Vec<usize> = (0..nodes.len()).filter(|&p| nodes[p] == node).collect();
        if closed && at.first() == Some(&0) && at.last() == Some(&(nodes.len() - 1)) {
            let last = at.pop().unwrap();
            at.remove(0);
            uses.push((i, vec![0, last]));
        }
        uses.extend(at.into_iter().map(|p| (i, vec![p])));
    }
    uses
}

/// Whether separating `node` would change anything: it is used in two or
/// more places (or, with `detach_only`, that way uses it and something else
/// does too).
pub fn can_separate(node: i64, ways: &[(i64, Vec<i64>)], detach_only: Option<i64>) -> bool {
    if let Some(only) = detach_only {
        if !ways.iter().any(|(id, n)| *id == only && n.contains(&node)) {
            return false;
        }
    }
    node_uses(node, ways, detach_only).len() >= 2
}

/// Plan separating `node`: the first use keeps it, every other use gets a
/// node from `alloc` (called once per use, in order). With `detach_only`,
/// that way is moved off the node instead (unless it's the only user).
/// `None` if there is nothing to separate.
pub fn plan_separate(
    node: i64,
    ways: &[(i64, Vec<i64>)],
    detach_only: Option<i64>,
    mut alloc: impl FnMut() -> i64,
) -> Option<Vec<SeparateEdit>> {
    if !can_separate(node, ways, detach_only) {
        return None;
    }
    let uses = node_uses(node, ways, detach_only);
    // Everything keeps the node except the later uses; when detaching one
    // way, that way's uses sort last, so all the other ways' uses keep it.
    let keepers = match detach_only {
        Some(only) => uses.iter().filter(|u| ways[u.0].0 != only).count().max(1),
        None => 1,
    };
    let mut edits: Vec<SeparateEdit> = Vec::new();
    for (way_idx, positions) in uses.into_iter().skip(keepers) {
        let (way_id, before) = &ways[way_idx];
        let new_node = alloc();
        let pos = match edits.iter().position(|e| e.way_id == *way_id) {
            Some(p) => p,
            None => {
                edits.push(SeparateEdit {
                    way_id: *way_id,
                    before: before.clone(),
                    after: before.clone(),
                });
                edits.len() - 1
            }
        };
        for p in positions {
            edits[pos].after[p] = new_node;
        }
    }
    Some(edits)
}

/// One way split into several parts at the selected node(s). Every part but
/// one becomes a new way; the longest part (the first of equal ones) stays in
/// the original way, which keeps the id and version. A closed way with a
/// single split node is not cut at all: it is rewritten to start and end at
/// that node, so `parts` has one entry and nothing new is created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitPlan {
    pub way_id: i64,
    /// The way's node list before the split.
    pub before: Vec<i64>,
    /// The parts in travel order (consecutive parts share their joint node;
    /// for a ring the last one ends where the first starts).
    pub parts: Vec<Vec<i64>>,
    /// Index into `parts` of the part the original way keeps.
    pub keeper: usize,
}

/// One binary cut of a `SplitPlan`, applied in order. Cut `i` creates new way
/// number `i + 1` (way 0 is the original), so a relation sees the same member
/// lists as if the user had split one node at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitCut {
    /// The way being cut: 0 for the original, `k` for the new way created by
    /// cut `k - 1`.
    pub holder: usize,
    /// The holder's node list before this cut.
    pub before: Vec<i64>,
    /// What the holder keeps.
    pub keep: Vec<i64>,
    /// What the new way created by this cut gets.
    pub new: Vec<i64>,
}

impl SplitPlan {
    /// The nodes the original way ends up with.
    pub fn keep(&self) -> &[i64] {
        &self.parts[self.keeper]
    }

    /// The node lists of the new ways, in travel order.
    pub fn new_parts(&self) -> Vec<&[i64]> {
        (0..self.parts.len())
            .filter(|&k| k != self.keeper)
            .map(|k| self.parts[k].as_slice())
            .collect()
    }

    /// The plan as a sequence of two-way cuts, in which the original ends up
    /// with the `keeper` part. Empty for a closed way that is only rewritten.
    pub fn cuts(&self) -> Vec<SplitCut> {
        if self.parts.len() == 1 {
            return Vec::new();
        }
        let mut line = self.parts[0].clone();
        for part in &self.parts[1..] {
            line.extend_from_slice(&part[1..]);
        }
        // Index in `line` where each part starts.
        let mut starts = vec![0usize];
        for part in &self.parts[..self.parts.len() - 1] {
            starts.push(starts.last().unwrap() + part.len() - 1);
        }
        let mut cuts = Vec::new();
        let mut holder = 0usize;
        for k in 0..self.parts.len() - 1 {
            let before = if k == 0 {
                self.before.clone()
            } else {
                line[starts[k]..].to_vec()
            };
            let left = self.parts[k].clone();
            let rest = line[starts[k + 1]..].to_vec();
            // The original keeps the left part if that is its final one,
            // otherwise it carries on with the rest; a new way always keeps
            // its left part and carries on with the rest.
            let (keep, new, next) = if holder == 0 && self.keeper == k {
                (left, rest, k + 1)
            } else if holder == 0 {
                (rest, left, 0)
            } else {
                (left, rest, k + 1)
            };
            cuts.push(SplitCut {
                holder,
                before,
                keep,
                new,
            });
            holder = next;
        }
        cuts
    }
}

/// Split one node list at the selected node(s) into parts (see `SplitPlan`).
///
/// - An open way is cut at every interior occurrence of every selected node
///   (JOSM): a node the way passes through twice is cut at both passes, and
///   an occurrence at the first or last position is not a cut. Selected
///   nodes that are only end nodes (or not in the way) are ignored.
/// - A closed way is cut at two or more selected nodes that each occur in the
///   ring once, giving the arcs between them, each an open way.
/// - A closed way with exactly one selected node (when `rotate` allows) is
///   rewritten to start and end at it.
///
/// Anything else (no cut, a cut that would leave a one-node part because the
/// way lists the node twice in a row, a ring node visited twice) can't be
/// split.
fn split_parts(nodes: &[i64], selected: &[i64], rotate: bool) -> Option<(Vec<Vec<i64>>, usize)> {
    let closed = nodes.len() >= 4 && nodes.first() == nodes.last();
    let (line, cuts): (Vec<i64>, Vec<usize>) = if closed {
        let ring = &nodes[..nodes.len() - 1];
        let mut at = Vec::new();
        for &node in selected {
            let mut found = (0..ring.len()).filter(|&p| ring[p] == node);
            let Some(first) = found.next() else { continue };
            if found.next().is_some() {
                return None;
            }
            at.push(first);
        }
        at.sort_unstable();
        match at.as_slice() {
            [] => return None,
            [p] => {
                if !rotate || selected.len() != 1 || *p == 0 {
                    return None;
                }
                let mut line = ring[*p..].to_vec();
                line.extend_from_slice(&ring[..=*p]);
                return Some((vec![line], 0));
            }
            [first, rest @ ..] => {
                let mut line = ring[*first..].to_vec();
                line.extend_from_slice(&ring[..=*first]);
                (line, rest.iter().map(|p| p - first).collect())
            }
        }
    } else {
        let cuts: Vec<usize> = (1..nodes.len().saturating_sub(1))
            .filter(|&p| selected.contains(&nodes[p]))
            .collect();
        (nodes.to_vec(), cuts)
    };
    if cuts.is_empty() {
        return None;
    }
    // The same node twice in a row would leave a part of one place.
    if cuts
        .iter()
        .any(|&p| line[p - 1] == line[p] || line[p + 1] == line[p])
    {
        return None;
    }
    let mut parts = Vec::new();
    let mut from = 0;
    for &p in cuts.iter().chain(std::iter::once(&(line.len() - 1))) {
        parts.push(line[from..=p].to_vec());
        from = p;
    }
    // The longest part stays; the first of equal ones.
    let keeper = (0..parts.len())
        .max_by_key(|&k| (parts[k].len(), std::cmp::Reverse(k)))
        .unwrap_or(0);
    Some((parts, keeper))
}

/// Plan splitting ways at the selected node(s) (JOSM "Split Way"). `ways`
/// is every way that uses a selected node, as `(id, node list)`; with
/// `only_way`, just that one is split, otherwise every one that can be. The
/// result is in the order of `ways`; empty if nothing can be split.
///
/// A closed way with a single selected node is only rewritten to start at it
/// when it is the only way to act on (named by `only_way`, or the only way
/// using the node), so splitting a street at a junction doesn't also turn a
/// neighbouring building.
pub fn plan_split(
    selected: &[i64],
    only_way: Option<i64>,
    ways: &[(i64, Vec<i64>)],
) -> Vec<SplitPlan> {
    let mut distinct: Vec<i64> = Vec::new();
    for &n in selected {
        if !distinct.contains(&n) {
            distinct.push(n);
        }
    }
    let candidates: Vec<&(i64, Vec<i64>)> = ways
        .iter()
        .filter(|(id, _)| only_way.is_none_or(|only| only == *id))
        .collect();
    let rotate = only_way.is_some() || candidates.len() == 1;
    candidates
        .into_iter()
        .filter_map(|(id, nodes)| {
            let (parts, keeper) = split_parts(nodes, &distinct, rotate)?;
            Some(SplitPlan {
                way_id: *id,
                before: nodes.clone(),
                parts,
                keeper,
            })
        })
        .collect()
}

/// Where Add mode should pick a way back up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Continuation {
    pub way_id: i64,
    /// The end node new nodes attach next to.
    pub from_node: i64,
    /// True to prepend (continuing from the first node).
    pub at_start: bool,
}

/// Continue an open way from its last node.
pub fn continue_way(way_id: i64, nodes: &[i64]) -> Option<Continuation> {
    if nodes.len() < 2 || nodes.first() == nodes.last() {
        return None;
    }
    Some(Continuation {
        way_id,
        from_node: *nodes.last()?,
        at_start: false,
    })
}

/// Continue the one open way that has `node` as its first or last node.
pub fn continue_from_node(node: i64, ways: &[(i64, Vec<i64>)]) -> Option<Continuation> {
    let mut ends = ways.iter().filter(|(_, nodes)| {
        nodes.len() >= 2
            && nodes.first() != nodes.last()
            && (nodes.first() == Some(&node) || nodes.last() == Some(&node))
    });
    let (way_id, nodes) = ends.next()?;
    if ends.next().is_some() {
        return None;
    }
    Some(Continuation {
        way_id: *way_id,
        from_node: node,
        at_start: nodes.first() == Some(&node),
    })
}

/// Whether unjoining `node` from `way` would change anything useful: the
/// node is a vertex of that way (see `is_unjoinable_vertex`) and some other
/// way uses it too.
pub fn can_unjoin(node: i64, ways: &[(i64, Vec<i64>)], way: i64) -> bool {
    ways.iter()
        .find(|(id, _)| *id == way)
        .is_some_and(|(_, nodes)| is_unjoinable_vertex(nodes, node))
        && ways
            .iter()
            .any(|(id, nodes)| *id != way && nodes.contains(&node))
}

/// Plan unjoining `node` from `way`: every place the way uses the node gets
/// the one node `alloc` returns (called once). `None` if `can_unjoin` is false.
pub fn plan_unjoin(
    node: i64,
    ways: &[(i64, Vec<i64>)],
    way: i64,
    alloc: impl FnOnce() -> i64,
) -> Option<Vec<SeparateEdit>> {
    if !can_unjoin(node, ways, way) {
        return None;
    }
    let (_, before) = ways.iter().find(|(id, _)| *id == way)?;
    let new_node = alloc();
    let after = before
        .iter()
        .map(|&n| if n == node { new_node } else { n })
        .collect();
    Some(vec![SeparateEdit {
        way_id: way,
        before: before.clone(),
        after,
    }])
}

/// Whether `node` can be unjoined from the way with these nodes: it is a
/// vertex of the way, not one of its two ends. A closed way's closing node
/// counts as a vertex (a ring has no ends), an open way's first and last
/// nodes do not (there is nothing to pull them off).
pub fn is_unjoinable_vertex(nodes: &[i64], node: i64) -> bool {
    if !nodes.contains(&node) {
        return false;
    }
    let closed = nodes.len() >= 4 && nodes.first() == nodes.last();
    if closed {
        return true;
    }
    // An open way that is visited again in the middle still has a vertex use.
    let last = nodes.len().saturating_sub(1);
    (1..last).any(|p| nodes[p] == node)
}

/// Why a segment can't be deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentError {
    /// The two nodes are not next to each other in the way.
    NotAdjacent,
    /// The way runs along the same two nodes more than once.
    Ambiguous,
    /// The segment is all there is (a two-node way): delete the way instead.
    WholeWay,
}

/// Deleting the segment between two adjacent nodes of a way: what is left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentDeletion {
    pub way_id: i64,
    pub before: Vec<i64>,
    /// The two nodes of the segment, as the way lists them.
    pub segment: [i64; 2],
    /// What remains, in travel order: one or two open pieces of at least two
    /// nodes each. A closed way leaves one piece (starting after the
    /// segment, ending before it); an open way leaves the part before and
    /// the part after, and a part of a single node is dropped.
    pub pieces: Vec<Vec<i64>>,
}

impl SegmentDeletion {
    /// The nodes of the segment that no remaining piece uses.
    pub fn dropped_nodes(&self) -> Vec<i64> {
        self.segment
            .iter()
            .copied()
            .filter(|n| !self.pieces.iter().any(|p| p.contains(n)))
            .collect()
    }

    /// When two pieces remain: the cut as a three-part split (before, the
    /// segment, after) in which the original way keeps the longer piece (the
    /// first of equal ones). The middle part is the one to throw away.
    pub fn as_split(&self) -> Option<SplitPlan> {
        let [left, right] = self.pieces.as_slice() else {
            return None;
        };
        Some(SplitPlan {
            way_id: self.way_id,
            before: self.before.clone(),
            parts: vec![left.clone(), self.segment.to_vec(), right.clone()],
            keeper: if right.len() > left.len() { 2 } else { 0 },
        })
    }
}

/// Plan deleting the segment between the adjacent nodes `a` and `b` of the
/// way `nodes`. The way is cut at the segment and the segment itself is
/// removed: an open way becomes the
/// piece before and the piece after; a segment at an end just shortens it;
/// a closed way is opened at the segment.
pub fn plan_delete_segment(
    way_id: i64,
    nodes: &[i64],
    a: i64,
    b: i64,
) -> Result<SegmentDeletion, SegmentError> {
    let closed = nodes.len() >= 4 && nodes.first() == nodes.last();
    let at: Vec<usize> = (0..nodes.len().saturating_sub(1))
        .filter(|&i| (nodes[i] == a && nodes[i + 1] == b) || (nodes[i] == b && nodes[i + 1] == a))
        .collect();
    let i = match at.as_slice() {
        [] => return Err(SegmentError::NotAdjacent),
        [i] => *i,
        _ => return Err(SegmentError::Ambiguous),
    };
    let segment = [nodes[i], nodes[i + 1]];
    let pieces = if closed {
        let ring = &nodes[..nodes.len() - 1];
        let mut line = ring[i + 1..].to_vec();
        line.extend_from_slice(&ring[..=i]);
        vec![line]
    } else {
        [nodes[..=i].to_vec(), nodes[i + 1..].to_vec()]
            .into_iter()
            .filter(|p| p.len() >= 2)
            .collect()
    };
    if pieces.is_empty() {
        return Err(SegmentError::WholeWay);
    }
    Ok(SegmentDeletion {
        way_id,
        before: nodes.to_vec(),
        segment,
        pieces,
    })
}

/// How Extract Point divides a closed way's tags between the way and the new
/// point.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ExtractSplit {
    /// Tags that leave the way and go onto the new node.
    pub moved: Vec<(String, String)>,
    /// Tags the node gets a copy of while the way keeps them (`source`).
    pub copied: Vec<(String, String)>,
    /// The way would lose everything that makes it an area, so it gets
    /// `area=yes` to stay one.
    pub add_area: bool,
}

/// Whether a tag describes the way itself (the building, its address, its
/// shape) rather than something located in it, so Extract Point leaves it.
fn stays_on_area(key: &str) -> bool {
    matches!(
        key,
        "building"
            | "area"
            | "type"
            | "height"
            | "min_height"
            | "layer"
            | "architect"
            | "nycdoitt:bin"
            | "ref:nycdoitt:bin"
    ) || ["addr:", "building:", "roof:"]
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

/// Split a closed way's tags for Extract Point (what iD's and JOSM's Extract
/// do): the building and address tags stay, `source` stays and is also
/// copied, and every other tag (the point of interest) moves to the new
/// node. `moved` is empty when there is nothing to extract.
pub fn extract_split(tags: &[(String, String)]) -> ExtractSplit {
    let mut split = ExtractSplit::default();
    for (key, value) in tags {
        let pair = (key.clone(), value.clone());
        if key == "source" || key.starts_with("source:") {
            split.copied.push(pair);
        } else if !stays_on_area(key) {
            split.moved.push(pair);
        }
    }
    if split.moved.is_empty() {
        split.copied.clear();
        return split;
    }
    split.add_area = !tags.iter().any(|(k, _)| k == "building" || k == "area");
    split
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_moves_the_poi_and_keeps_building_and_address() {
        let split = extract_split(&tags(&[
            ("building", "retail"),
            ("building:levels", "2"),
            ("addr:housenumber", "5"),
            ("shop", "bakery"),
            ("name", "Bread"),
            ("source", "survey"),
        ]));
        assert_eq!(split.moved, tags(&[("shop", "bakery"), ("name", "Bread")]));
        assert_eq!(split.copied, tags(&[("source", "survey")]));
        assert!(!split.add_area);
    }

    #[test]
    fn extract_has_nothing_to_move_for_a_plain_building() {
        let split = extract_split(&tags(&[("building", "yes"), ("addr:street", "Elm")]));
        assert!(split.moved.is_empty() && split.copied.is_empty() && !split.add_area);
        assert_eq!(
            extract_split(&tags(&[("source", "x")])),
            ExtractSplit::default()
        );
    }

    #[test]
    fn extract_keeps_a_non_building_area_an_area() {
        let split = extract_split(&tags(&[("amenity", "parking")]));
        assert_eq!(split.moved, tags(&[("amenity", "parking")]));
        assert!(split.add_area);
        let split = extract_split(&tags(&[("amenity", "parking"), ("area", "yes")]));
        assert!(!split.add_area);
    }

    fn way(id: i64, nodes: &[i64], tags: &[(&str, &str)]) -> JoinWay {
        JoinWay {
            id,
            nodes: nodes.to_vec(),
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn joins_end_to_end_with_shared_node_once() {
        let plan = plan_join(&[way(-1, &[1, 2, 3], &[]), way(-2, &[3, 4, 5], &[])]).unwrap();
        assert_eq!(plan.nodes, vec![1, 2, 3, 4, 5]);
        assert_eq!(plan.keep, -1);
        assert_eq!(plan.remove, vec![-2]);
    }

    #[test]
    fn reverses_and_reorders_as_needed() {
        // Selection order isn't chain order, and the middle way is flipped.
        let plan = plan_join(&[
            way(-1, &[3, 4], &[]),
            way(-2, &[1, 2], &[]),
            way(-3, &[3, 2], &[]),
        ])
        .unwrap();
        // Walks 1-2-3-4 (the first way is the start of its own direction
        // only half the time; either direction is acceptable).
        assert!(plan.nodes == vec![1, 2, 3, 4] || plan.nodes == vec![4, 3, 2, 1]);
        assert_eq!(plan.remove.len(), 2);
    }

    #[test]
    fn disconnected_ways_cannot_chain() {
        let ways = [way(-1, &[1, 2], &[]), way(-2, &[3, 4], &[])];
        assert_eq!(plan_join(&ways), Err(JoinError::NotChainable));
        assert!(!can_chain(&ways));
        // Touching at a middle node isn't an end-to-end join.
        assert!(!can_chain(&[
            way(-1, &[1, 2, 3], &[]),
            way(-2, &[2, 4], &[])
        ]));
        assert!(!can_chain(&[way(-1, &[1, 2], &[])]));
    }

    #[test]
    fn a_third_way_at_the_same_end_is_resolved_by_backtracking() {
        // 2 is the end of three ways; only two of them can continue a chain
        // that must still include 3-4.
        let plan = plan_join(&[
            way(-1, &[1, 2], &[]),
            way(-2, &[2, 3], &[]),
            way(-3, &[3, 4], &[]),
        ])
        .unwrap();
        assert_eq!(plan.nodes.len(), 4);
        // A genuine Y (three ways meeting at one end) can't be one line.
        assert!(!can_chain(&[
            way(-1, &[1, 2], &[]),
            way(-2, &[3, 2], &[]),
            way(-3, &[4, 2], &[]),
        ]));
    }

    #[test]
    fn closing_the_ring_is_allowed_and_a_ring_takes_no_more() {
        let plan = plan_join(&[way(-1, &[1, 2, 3], &[]), way(-2, &[3, 4, 1], &[])]).unwrap();
        assert_eq!(plan.nodes, vec![1, 2, 3, 4, 1]);
        assert!(!can_chain(&[
            way(-1, &[1, 2, 3, 1], &[]),
            way(-2, &[1, 5], &[])
        ]));
    }

    #[test]
    fn keeps_the_only_server_way_else_the_first_selected() {
        let plan = plan_join(&[way(-1, &[1, 2], &[]), way(77, &[2, 3], &[])]).unwrap();
        assert_eq!(plan.keep, 77);
        assert_eq!(plan.remove, vec![-1]);
        let plan = plan_join(&[way(5, &[1, 2], &[]), way(77, &[2, 3], &[])]).unwrap();
        assert_eq!(plan.keep, 5);
        let plan = plan_join(&[way(-4, &[1, 2], &[]), way(-9, &[2, 3], &[])]).unwrap();
        assert_eq!(plan.keep, -4);
    }

    #[test]
    fn tags_union_and_conflicts_name_the_key() {
        let plan = plan_join(&[
            way(-1, &[1, 2], &[("highway", "residential"), ("name", "Main")]),
            way(
                -2,
                &[2, 3],
                &[("highway", "residential"), ("surface", "paved")],
            ),
        ])
        .unwrap();
        assert_eq!(
            plan.tags,
            vec![
                ("highway".to_string(), "residential".to_string()),
                ("name".to_string(), "Main".to_string()),
                ("surface".to_string(), "paved".to_string()),
            ]
        );
        let err = plan_join(&[
            way(-1, &[1, 2], &[("name", "Main"), ("ref", "1")]),
            way(-2, &[2, 3], &[("name", "Elm"), ("ref", "2")]),
        ])
        .unwrap_err();
        assert_eq!(err, JoinError::TagConflict("name".to_string()));
        assert_eq!(
            err.message(),
            "Can't join: the ways have different values for 'name'"
        );
    }

    #[test]
    fn oneway_blocks_a_join_only_when_it_must_be_reversed() {
        // Second way runs against the first: someone must flip.
        let err = plan_join(&[
            way(-1, &[1, 2], &[("oneway", "yes")]),
            way(-2, &[3, 2], &[("oneway", "yes")]),
        ])
        .unwrap_err();
        assert!(matches!(err, JoinError::NeedsReversal(_)));

        // Only the plain way needs flipping: fine, in whichever direction
        // leaves the oneway way alone.
        let plan = plan_join(&[
            way(-1, &[3, 2], &[]),
            way(-2, &[1, 2], &[("oneway", "yes")]),
        ])
        .unwrap();
        assert_eq!(plan.nodes, vec![1, 2, 3]);

        // Chain walked from the plain way would flip the oneway; the planner
        // flips the whole chain instead.
        let plan = plan_join(&[
            way(-1, &[2, 3], &[]),
            way(-2, &[2, 1], &[("oneway", "yes")]),
        ])
        .unwrap();
        assert_eq!(plan.nodes, vec![3, 2, 1]);
    }

    #[test]
    fn direction_dependent_tags() {
        let t = |k: &str, v: &str| vec![(k.to_string(), v.to_string())];
        assert!(is_direction_dependent(&t("oneway", "yes")));
        assert!(is_direction_dependent(&t("oneway", "-1")));
        assert!(!is_direction_dependent(&t("oneway", "no")));
        assert!(is_direction_dependent(&t("incline", "up")));
        assert!(is_direction_dependent(&t("lanes:forward", "2")));
        assert!(!is_direction_dependent(&t("highway", "residential")));
    }

    fn new_of(p: &SplitPlan) -> Vec<Vec<i64>> {
        p.new_parts().into_iter().map(|n| n.to_vec()).collect()
    }

    /// Replaying the cuts gives the same final lists as the plan's parts.
    fn replay(p: &SplitPlan) -> Vec<Vec<i64>> {
        let mut lists = vec![p.before.clone()];
        for (i, cut) in p.cuts().iter().enumerate() {
            assert_eq!(
                cut.before, lists[cut.holder],
                "cut {i} starts from its holder"
            );
            lists[cut.holder] = cut.keep.clone();
            lists.push(cut.new.clone());
        }
        lists
    }

    #[test]
    fn split_an_open_way_keeps_the_longer_part() {
        let w = ways(&[(10, &[1, 2, 3, 4, 5])]);
        let plans = plan_split(&[4], None, &w);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].way_id, 10);
        assert_eq!(plans[0].before, vec![1, 2, 3, 4, 5]);
        assert_eq!(plans[0].keep(), [1, 2, 3, 4]);
        assert_eq!(new_of(&plans[0]), vec![vec![4, 5]]);
        // Splitting nearer the start: the original keeps the far part.
        let plans = plan_split(&[2], None, &w);
        assert_eq!(plans[0].keep(), [2, 3, 4, 5]);
        assert_eq!(new_of(&plans[0]), vec![vec![1, 2]]);
        // Equal halves: the original keeps the first.
        let plans = plan_split(&[3], None, &w);
        assert_eq!(plans[0].keep(), [1, 2, 3]);
        assert_eq!(new_of(&plans[0]), vec![vec![3, 4, 5]]);
    }

    #[test]
    fn split_needs_an_interior_occurrence() {
        let w = ways(&[(10, &[1, 2, 3])]);
        // End nodes, strangers and nothing at all: no.
        assert!(plan_split(&[1], None, &w).is_empty());
        assert!(plan_split(&[3], None, &w).is_empty());
        assert!(plan_split(&[9], None, &w).is_empty());
        assert!(plan_split(&[], None, &w).is_empty());
        // An end node next to an interior one is ignored, not an error.
        let plans = plan_split(&[2, 3], None, &w);
        assert_eq!(plans[0].parts, vec![vec![1, 2], vec![2, 3]]);
        // The same node twice in a row would leave a one-place part.
        assert!(plan_split(&[2], None, &ways(&[(10, &[1, 2, 2, 3])])).is_empty());
    }

    #[test]
    fn split_an_open_way_at_several_nodes_gives_several_parts() {
        let w = ways(&[(10, &[1, 2, 3, 4, 5, 6, 7])]);
        let plans = plan_split(&[6, 3], None, &w);
        assert_eq!(
            plans[0].parts,
            vec![vec![1, 2, 3], vec![3, 4, 5, 6], vec![6, 7]]
        );
        // The longest (middle) part keeps the id.
        assert_eq!(plans[0].keeper, 1);
        assert_eq!(plans[0].keep(), [3, 4, 5, 6]);
        assert_eq!(new_of(&plans[0]), vec![vec![1, 2, 3], vec![6, 7]]);
        // Selection order and repeats don't matter.
        assert_eq!(plan_split(&[3, 6, 3], None, &w), plans);
        // On a tie the first of the longest parts keeps the id.
        let tie = plan_split(&[3, 5], None, &ways(&[(10, &[1, 2, 3, 4, 5, 6, 7])]));
        assert_eq!(tie[0].keeper, 0);
    }

    #[test]
    fn cuts_replay_to_the_same_parts_whichever_part_keeps_the_id() {
        let w = ways(&[(10, &[1, 2, 3, 4, 5, 6, 7, 8, 9])]);
        // Cut after 2 parts of different lengths, so every keeper position is
        // exercised in turn.
        for selected in [
            &[2, 3, 4][..],
            &[5, 6, 7],
            &[2, 3, 8],
            &[2, 8, 9],
            &[3, 6],
            &[2, 3, 4, 5, 6, 7, 8],
        ] {
            let plan = &plan_split(selected, None, &w)[0];
            let lists = replay(plan);
            assert_eq!(lists.len(), plan.parts.len());
            assert_eq!(lists[0], plan.keep(), "{selected:?}");
            // Same parts, as sets.
            let mut got = lists.clone();
            let mut want = plan.parts.clone();
            got.sort();
            want.sort();
            assert_eq!(got, want, "{selected:?}");
        }
    }

    #[test]
    fn split_at_a_node_the_way_passes_through_twice_cuts_both_passes() {
        // Figure eight through node 3.
        let w = ways(&[(10, &[1, 2, 3, 4, 5, 3, 6])]);
        let plans = plan_split(&[3], None, &w);
        assert_eq!(
            plans[0].parts,
            vec![vec![1, 2, 3], vec![3, 4, 5, 3], vec![3, 6]]
        );
        assert_eq!(plans[0].keeper, 1);
        // P shape ending on node 2: only the interior pass is a cut.
        let p = plan_split(&[2], None, &ways(&[(11, &[1, 2, 3, 4, 2])]));
        assert_eq!(p[0].parts, vec![vec![1, 2], vec![2, 3, 4, 2]]);
        // And starting on it.
        let q = plan_split(&[2], None, &ways(&[(11, &[2, 3, 4, 2, 5])]));
        assert_eq!(q[0].parts, vec![vec![2, 3, 4, 2], vec![2, 5]]);
    }

    #[test]
    fn split_a_ring_at_two_nodes_makes_two_open_ways() {
        let ring = ways(&[(10, &[1, 2, 3, 4, 5, 1])]);
        let plans = plan_split(&[4, 2], None, &ring);
        assert_eq!(plans.len(), 1);
        // The arc 2-3-4 has 3 nodes, the other 4-5-1-2 has 4: that stays.
        assert_eq!(plans[0].keep(), [4, 5, 1, 2]);
        assert_eq!(new_of(&plans[0]), vec![vec![2, 3, 4]]);
        // A split through the ring's start/end node works too.
        let plans = plan_split(&[1, 3], None, &ring);
        assert_eq!(plans[0].keep(), [3, 4, 5, 1]);
        assert_eq!(new_of(&plans[0]), vec![vec![1, 2, 3]]);
        // The order the nodes were selected in doesn't matter.
        assert_eq!(plan_split(&[3, 1], None, &ring), plans);
    }

    #[test]
    fn split_a_ring_at_three_nodes_makes_three_arcs() {
        let ring = ways(&[(10, &[1, 2, 3, 4, 5, 6, 1])]);
        let plans = plan_split(&[5, 1, 3], None, &ring);
        assert_eq!(
            plans[0].parts,
            vec![vec![1, 2, 3], vec![3, 4, 5], vec![5, 6, 1]]
        );
        assert_eq!(plans[0].keeper, 0);
        assert_eq!(replay(&plans[0]).len(), 3);
    }

    #[test]
    fn split_a_ring_at_one_node_only_rewrites_it_to_start_there() {
        let ring = ways(&[(10, &[1, 2, 3, 4, 1])]);
        let plans = plan_split(&[3], None, &ring);
        assert_eq!(plans[0].parts, vec![vec![3, 4, 1, 2, 3]]);
        assert_eq!(plans[0].keeper, 0);
        assert!(plans[0].cuts().is_empty());
        // Already the start: nothing to do.
        assert!(plan_split(&[1], None, &ring).is_empty());
        // Only when the ring is the only way involved, or chosen.
        let both = ways(&[(10, &[1, 2, 3, 4, 1]), (11, &[5, 3, 6])]);
        let plans = plan_split(&[3], None, &both);
        assert_eq!(plans.iter().map(|p| p.way_id).collect::<Vec<_>>(), [11]);
        let plans = plan_split(&[3], Some(10), &both);
        assert_eq!(plans[0].keep(), [3, 4, 1, 2, 3]);
    }

    #[test]
    fn split_a_ring_refuses_strangers_and_repeated_nodes() {
        let ring = ways(&[(10, &[1, 2, 3, 4, 1])]);
        // One node of the ring plus a stranger is not "one node".
        assert!(plan_split(&[2, 9], None, &ring).is_empty());
        // A ring node visited twice is ambiguous.
        let eight = ways(&[(10, &[1, 2, 3, 2, 4, 1])]);
        assert!(plan_split(&[2, 4], None, &eight).is_empty());
    }

    #[test]
    fn split_covers_every_way_at_the_node_or_just_the_chosen_one() {
        let w = ways(&[(10, &[1, 2, 3]), (11, &[4, 2, 5, 6]), (12, &[2, 7])]);
        let all = plan_split(&[2], None, &w);
        // Way 12 ends at the node, so it isn't split.
        assert_eq!(all.iter().map(|p| p.way_id).collect::<Vec<_>>(), [10, 11]);
        let only = plan_split(&[2], Some(11), &w);
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].way_id, 11);
        assert_eq!(only[0].keep(), [2, 5, 6]);
        assert_eq!(new_of(&only[0]), vec![vec![4, 2]]);
        // Naming a way that can't be split at the node gives nothing.
        assert!(plan_split(&[2], Some(12), &w).is_empty());
        assert!(plan_split(&[2], Some(99), &w).is_empty());
    }

    fn node(id: i64, tags: &[(&str, &str)]) -> MergeNode {
        MergeNode {
            id,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn merge_keeps_the_only_server_node_else_the_first_selected() {
        let plan = plan_merge_nodes(&[node(-1, &[]), node(7, &[]), node(-2, &[])], &[]).unwrap();
        assert_eq!((plan.keep, plan.remove), (7, vec![-1, -2]));
        let plan = plan_merge_nodes(&[node(-3, &[]), node(-1, &[])], &[]).unwrap();
        assert_eq!((plan.keep, plan.remove), (-3, vec![-1]));
        // Several server nodes: the first selected wins.
        let plan = plan_merge_nodes(&[node(9, &[]), node(7, &[])], &[]).unwrap();
        assert_eq!((plan.keep, plan.remove), (9, vec![7]));
        // Listing a node twice doesn't make it two nodes.
        assert_eq!(
            plan_merge_nodes(&[node(9, &[]), node(9, &[])], &[]),
            Err(MergeError::NotEnoughNodes)
        );
    }

    #[test]
    fn merge_unions_tags_and_names_a_conflicting_key() {
        let plan = plan_merge_nodes(
            &[
                node(1, &[("highway", "crossing"), ("name", "A")]),
                node(2, &[("name", "A"), ("crossing", "zebra")]),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(
            plan.tags,
            tags(&[
                ("crossing", "zebra"),
                ("highway", "crossing"),
                ("name", "A")
            ])
        );
        let err = plan_merge_nodes(
            &[
                node(1, &[("name", "A"), ("ref", "1")]),
                node(2, &[("name", "B"), ("ref", "2")]),
            ],
            &[],
        )
        .unwrap_err();
        assert_eq!(err, MergeError::TagConflict("name".to_string()));
        assert_eq!(
            err.message(),
            "Can't merge: the nodes have different values for 'name'"
        );
    }

    #[test]
    fn merge_points_ways_at_the_survivor_and_collapses_repeats() {
        let w = ways(&[
            (10, &[1, 2, 3]),
            (11, &[4, 5]),
            (12, &[2, 3, 6]),
            (13, &[7, 8]),
        ]);
        // 2 and 3 are neighbours in ways 10 and 12: they collapse to one.
        let plan = plan_merge_nodes(&[node(2, &[]), node(3, &[])], &w).unwrap();
        assert_eq!(plan.keep, 2);
        assert_eq!(plan.way_edits.len(), 2);
        assert_eq!(plan.way_edits[0].after, vec![1, 2]);
        assert_eq!(plan.way_edits[0].before, vec![1, 2, 3]);
        assert_eq!(plan.way_edits[1].way_id, 12);
        assert_eq!(plan.way_edits[1].after, vec![2, 6]);
        // Ways that don't touch a removed node aren't edited.
        assert!(plan.way_edits.iter().all(|e| e.way_id != 13));
    }

    #[test]
    fn merge_joins_two_ways_end_to_end_through_the_survivor() {
        let w = ways(&[(10, &[1, 2]), (11, &[3, 4])]);
        let plan = plan_merge_nodes(&[node(2, &[]), node(3, &[])], &w).unwrap();
        // Way 10 already ends at the survivor: only way 11 is edited.
        assert_eq!(plan.way_edits.len(), 1);
        assert_eq!(plan.way_edits[0].way_id, 11);
        assert_eq!(plan.way_edits[0].after, vec![2, 4]);
    }

    #[test]
    fn merge_refuses_a_way_left_with_one_node() {
        // Merging the only two nodes of a way would leave a single node.
        let w = ways(&[(10, &[1, 2]), (11, &[2, 3])]);
        assert_eq!(
            plan_merge_nodes(&[node(1, &[]), node(2, &[])], &w),
            Err(MergeError::DegenerateWay(10))
        );
        // A ring squeezed to two distinct nodes is degenerate too.
        let ring = ways(&[(10, &[1, 2, 3, 1])]);
        assert_eq!(
            plan_merge_nodes(&[node(2, &[]), node(3, &[])], &ring),
            Err(MergeError::DegenerateWay(10))
        );
        // But three-way merge of a triangle leaves nothing.
        assert_eq!(
            plan_merge_nodes(&[node(1, &[]), node(2, &[]), node(3, &[])], &ring),
            Err(MergeError::DegenerateWay(10))
        );
    }

    fn tags(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn change(key: &str, before: Option<&str>, after: Option<&str>) -> TagChange {
        (
            key.to_string(),
            before.map(str::to_string),
            after.map(str::to_string),
        )
    }

    #[test]
    fn reversing_swaps_forward_and_backward_keys() {
        let changes = reversed_tag_changes(&tags(&[
            ("lanes:forward", "2"),
            ("lanes:backward", "1"),
            ("maxspeed:forward", "50"),
        ]));
        assert_eq!(
            changes,
            vec![
                change("lanes:backward", Some("1"), Some("2")),
                change("lanes:forward", Some("2"), Some("1")),
                // Only the forward value exists: it moves to the backward key.
                change("maxspeed:backward", None, Some("50")),
                change("maxspeed:forward", Some("50"), None),
            ]
        );
    }

    #[test]
    fn reversing_swaps_left_and_right_anywhere_after_the_first_part() {
        let changes = reversed_tag_changes(&tags(&[
            ("cycleway:left", "lane"),
            ("sidewalk:right", "yes"),
            ("parking:lane:left:parallel", "no_stopping"),
        ]));
        assert_eq!(
            changes,
            vec![
                change("cycleway:left", Some("lane"), None),
                change("cycleway:right", None, Some("lane")),
                change("parking:lane:left:parallel", Some("no_stopping"), None),
                change("parking:lane:right:parallel", None, Some("no_stopping")),
                change("sidewalk:left", None, Some("yes")),
                change("sidewalk:right", Some("yes"), None),
            ]
        );
        // A bare `left` key has no `:` before it and stays put.
        assert!(reversed_tag_changes(&tags(&[("left", "x")])).is_empty());
    }

    #[test]
    fn reversing_flips_direction_incline_and_side_values() {
        let changes = reversed_tag_changes(&tags(&[
            ("direction", "forward"),
            ("incline", "up"),
            ("traffic_signals:direction", "backward"),
            ("side", "left"),
        ]));
        assert_eq!(
            changes,
            vec![
                change("direction", Some("forward"), Some("backward")),
                change("incline", Some("up"), Some("down")),
                change("side", Some("left"), Some("right")),
                change(
                    "traffic_signals:direction",
                    Some("backward"),
                    Some("forward")
                ),
            ]
        );
    }

    #[test]
    fn reversing_negates_numeric_inclines() {
        let flipped = |v: &str| {
            reversed_tag_changes(&tags(&[("incline", v)]))
                .pop()
                .and_then(|c| c.2)
        };
        assert_eq!(flipped("10%").as_deref(), Some("-10%"));
        assert_eq!(flipped("-10%").as_deref(), Some("10%"));
        assert_eq!(flipped("+4.5°").as_deref(), Some("-4.5°"));
        assert_eq!(flipped("7").as_deref(), Some("-7"));
        // Zero and non-numbers have nothing to flip.
        assert_eq!(flipped("0%"), None);
        assert_eq!(flipped("steep"), None);
    }

    #[test]
    fn reversing_leaves_oneway_and_unrelated_tags_alone() {
        assert!(reversed_tag_changes(&tags(&[
            ("highway", "residential"),
            ("oneway", "yes"),
            ("name", "Left Street"),
            ("lanes", "2"),
        ]))
        .is_empty());
        // `oneway=-1` is not rewritten to `yes` either.
        assert!(reversed_tag_changes(&tags(&[("oneway", "-1")])).is_empty());
        // Non-directional keys keep their `forward`/`left` values.
        assert!(reversed_tag_changes(&tags(&[("note", "left")])).is_empty());
    }

    #[test]
    fn reversing_twice_gives_the_original_tags() {
        let original = tags(&[
            ("lanes:forward", "2"),
            ("cycleway:left", "lane"),
            ("incline", "12%"),
            ("direction", "forward"),
        ]);
        let apply = |tags: &[(String, String)]| {
            let mut map: BTreeMap<String, String> = tags.iter().cloned().collect();
            for (key, _, after) in reversed_tag_changes(tags) {
                match after {
                    Some(v) => map.insert(key, v),
                    None => map.remove(&key),
                };
            }
            map.into_iter().collect::<Vec<_>>()
        };
        let once = apply(&original);
        assert_ne!(once, original);
        let mut sorted = original.clone();
        sorted.sort();
        assert_eq!(apply(&once), sorted);
    }

    fn ways(list: &[(i64, &[i64])]) -> Vec<(i64, Vec<i64>)> {
        list.iter().map(|(id, n)| (*id, n.to_vec())).collect()
    }

    fn counter(start: i64) -> impl FnMut() -> i64 {
        let mut next = start;
        move || {
            next -= 1;
            next
        }
    }

    #[test]
    fn separate_gives_every_way_but_the_first_its_own_node() {
        let w = ways(&[(-20, &[1, 9, 2]), (10, &[3, 9, 4]), (11, &[5, 9])]);
        assert!(can_separate(9, &w, None));
        let edits = plan_separate(9, &w, None, counter(-100)).unwrap();
        // Way 10 (server) keeps it; -20 and 11 are the others, 11 first.
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].way_id, 11);
        assert_eq!(edits[0].after, vec![5, -101]);
        assert_eq!(edits[1].way_id, -20);
        assert_eq!(edits[1].after, vec![1, -102, 2]);
    }

    #[test]
    fn separate_with_a_way_detaches_just_that_way() {
        let w = ways(&[(10, &[3, 9, 4]), (11, &[5, 9]), (12, &[9, 7])]);
        let edits = plan_separate(9, &w, Some(11), counter(-100)).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].way_id, 11);
        assert_eq!(edits[0].after, vec![5, -101]);
        // A way that doesn't use the node, or the only user, can't detach.
        assert!(!can_separate(9, &w, Some(99)));
        assert!(!can_separate(9, &ways(&[(10, &[3, 9, 4])]), Some(10)));
    }

    #[test]
    fn separate_needs_more_than_one_use() {
        assert!(!can_separate(9, &ways(&[(10, &[3, 9, 4])]), None));
        // A closed way's ring closure is one use, not two.
        assert!(!can_separate(1, &ways(&[(10, &[1, 2, 3, 1])]), None));
        assert!(plan_separate(1, &ways(&[(10, &[1, 2, 3, 1])]), None, counter(0)).is_none());
    }

    #[test]
    fn separate_splits_a_way_that_touches_itself() {
        let w = ways(&[(10, &[1, 2, 3, 2, 4])]);
        let edits = plan_separate(2, &w, None, counter(-100)).unwrap();
        assert_eq!(edits[0].after, vec![1, 2, 3, -101, 4]);
    }

    #[test]
    fn separate_replaces_both_ends_of_a_ring_in_another_way() {
        let w = ways(&[(10, &[7, 1, 8]), (-5, &[1, 2, 3, 1])]);
        let edits = plan_separate(1, &w, None, counter(-100)).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].way_id, -5);
        assert_eq!(edits[0].after, vec![-101, 2, 3, -101]);
    }

    #[test]
    fn continuation_from_a_way_or_an_end_node() {
        assert_eq!(
            continue_way(5, &[1, 2, 3]),
            Some(Continuation {
                way_id: 5,
                from_node: 3,
                at_start: false
            })
        );
        assert_eq!(continue_way(5, &[1, 2, 3, 1]), None);
        assert_eq!(continue_way(5, &[1]), None);

        let w = ways(&[(5, &[1, 2, 3]), (6, &[7, 2, 8]), (9, &[4, 5, 4])]);
        let at_start = continue_from_node(1, &w).unwrap();
        assert!(at_start.at_start);
        assert_eq!((at_start.way_id, at_start.from_node), (5, 1));
        let at_end = continue_from_node(3, &w).unwrap();
        assert!(!at_end.at_start);
        // A middle node, and the end of a closed way, aren't continuable.
        assert_eq!(continue_from_node(2, &w), None);
        assert_eq!(continue_from_node(4, &w), None);
        // First of one way and last of another: ambiguous.
        let w = ways(&[(5, &[1, 2]), (6, &[3, 1])]);
        assert_eq!(continue_from_node(1, &w), None);
    }
}
