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
    let mut merged: BTreeMap<&str, &str> = BTreeMap::new();
    let mut conflict: Option<&str> = None;
    for (k, v) in ways.iter().flat_map(|w| w.tags.iter()) {
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
    if let Some(key) = conflict {
        return Err(JoinError::TagConflict(key.to_string()));
    }

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
        tags: merged
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        remove: ways.iter().map(|w| w.id).filter(|&id| id != keep).collect(),
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
