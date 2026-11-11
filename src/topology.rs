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
/// fewer than two distinct nodes. (Relation memberships are the caller's
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
        if distinct.len() < 2 {
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

/// One way split in two: it keeps `keep` and `new` becomes a new way, both
/// containing the split node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitPlan {
    pub way_id: i64,
    /// The way's node list before the split.
    pub before: Vec<i64>,
    /// The node list the original way keeps (the longer part; the first
    /// when they tie).
    pub keep: Vec<i64>,
    /// The node list of the new way.
    pub new: Vec<i64>,
}

/// Split one node list at the selected node(s), as `(kept part, new part)`.
///
/// - An open way is split at one selected node that occurs exactly once and
///   isn't an end node, giving `[..=at]` and `[at..]`.
/// - A closed way is split at two selected nodes that each occur in the ring
///   once, giving the two arcs between them, each an open way.
///
/// Anything else (other counts, an end node, a node that isn't in the way, a
/// node the way passes through twice) can't be split.
fn split_parts(nodes: &[i64], selected: &[i64]) -> Option<(Vec<i64>, Vec<i64>)> {
    let closed = nodes.len() >= 4 && nodes.first() == nodes.last();
    let ring = if closed {
        &nodes[..nodes.len() - 1]
    } else {
        nodes
    };
    let once = |node: i64| {
        let mut at = (0..ring.len()).filter(|&p| ring[p] == node);
        let first = at.next()?;
        at.next().is_none().then_some(first)
    };
    let (first, second) = match (closed, selected) {
        (false, &[node]) => {
            let at = once(node)?;
            // Not an end node.
            if at == 0 || at + 1 == ring.len() {
                return None;
            }
            (nodes[..=at].to_vec(), nodes[at..].to_vec())
        }
        (true, &[a, b]) if a != b => {
            let (i, j) = (once(a)?, once(b)?);
            let (i, j) = (i.min(j), i.max(j));
            let mut around = ring[j..].to_vec();
            around.extend_from_slice(&ring[..=i]);
            (ring[i..=j].to_vec(), around)
        }
        _ => return None,
    };
    Some(if first.len() >= second.len() {
        (first, second)
    } else {
        (second, first)
    })
}

/// Plan splitting ways at the selected node(s) (JOSM "Split Way"). `ways`
/// is every way that uses a selected node, as `(id, node list)`; with
/// `only_way`, just that one is split, otherwise every one that can be. The
/// result is in the order of `ways`; empty if nothing can be split.
pub fn plan_split(
    selected: &[i64],
    only_way: Option<i64>,
    ways: &[(i64, Vec<i64>)],
) -> Vec<SplitPlan> {
    ways.iter()
        .filter(|(id, _)| only_way.is_none_or(|only| only == *id))
        .filter_map(|(id, nodes)| {
            let (keep, new) = split_parts(nodes, selected)?;
            Some(SplitPlan {
                way_id: *id,
                before: nodes.clone(),
                keep,
                new,
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn split_an_open_way_keeps_the_longer_part() {
        let w = ways(&[(10, &[1, 2, 3, 4, 5])]);
        let plans = plan_split(&[4], None, &w);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].way_id, 10);
        assert_eq!(plans[0].before, vec![1, 2, 3, 4, 5]);
        assert_eq!(plans[0].keep, vec![1, 2, 3, 4]);
        assert_eq!(plans[0].new, vec![4, 5]);
        // Splitting nearer the start: the original keeps the far part.
        let plans = plan_split(&[2], None, &w);
        assert_eq!(plans[0].keep, vec![2, 3, 4, 5]);
        assert_eq!(plans[0].new, vec![1, 2]);
        // Equal halves: the original keeps the first.
        let plans = plan_split(&[3], None, &w);
        assert_eq!(plans[0].keep, vec![1, 2, 3]);
        assert_eq!(plans[0].new, vec![3, 4, 5]);
    }

    #[test]
    fn split_needs_an_interior_node_that_occurs_once() {
        let w = ways(&[(10, &[1, 2, 3])]);
        // End nodes, strangers, and several nodes on an open way: no.
        assert!(plan_split(&[1], None, &w).is_empty());
        assert!(plan_split(&[3], None, &w).is_empty());
        assert!(plan_split(&[9], None, &w).is_empty());
        assert!(plan_split(&[], None, &w).is_empty());
        assert!(plan_split(&[2, 3], None, &w).is_empty());
        // A way that passes through the node twice is ambiguous.
        assert!(plan_split(&[2], None, &ways(&[(10, &[1, 2, 3, 2, 4])])).is_empty());
    }

    #[test]
    fn split_a_ring_at_two_nodes_makes_two_open_ways() {
        let ring = ways(&[(10, &[1, 2, 3, 4, 5, 1])]);
        let plans = plan_split(&[4, 2], None, &ring);
        assert_eq!(plans.len(), 1);
        // The arc 2-3-4 has 3 nodes, the other 4-5-1-2 has 4: that stays.
        assert_eq!(plans[0].keep, vec![4, 5, 1, 2]);
        assert_eq!(plans[0].new, vec![2, 3, 4]);
        // A split through the ring's start/end node works too.
        let plans = plan_split(&[1, 3], None, &ring);
        assert_eq!(plans[0].keep, vec![3, 4, 5, 1]);
        assert_eq!(plans[0].new, vec![1, 2, 3]);
        // The order the nodes were selected in doesn't matter.
        assert_eq!(plan_split(&[3, 1], None, &ring), plans);
    }

    #[test]
    fn split_a_ring_needs_exactly_two_distinct_nodes() {
        let ring = ways(&[(10, &[1, 2, 3, 4, 1])]);
        assert!(plan_split(&[2], None, &ring).is_empty());
        assert!(plan_split(&[2, 2], None, &ring).is_empty());
        assert!(plan_split(&[1, 2, 3], None, &ring).is_empty());
        assert!(plan_split(&[2, 9], None, &ring).is_empty());
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
        assert_eq!(only[0].keep, vec![2, 5, 6]);
        assert_eq!(only[0].new, vec![4, 2]);
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
        // A ring squeezed to two nodes is still two distinct nodes.
        let ring = ways(&[(10, &[1, 2, 3, 1])]);
        let plan = plan_merge_nodes(&[node(2, &[]), node(3, &[])], &ring).unwrap();
        assert_eq!(plan.way_edits[0].after, vec![1, 2, 1]);
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
