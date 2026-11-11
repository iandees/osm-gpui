//! Sort a relation's members:
//! the way members are put into connected, end-to-end order.
//!
//! Only the order changes. Roles are never touched and no way is reversed;
//! a way that has to be walked backwards simply sits where the walk uses it.
//!
//! * Members that are not sortable ways stay in the slot they have now:
//!   nodes, relations, ways that are not downloaded, and ways whose role says
//!   they are not part of the route (`stop…`, `platform…`).
//! * The sortable ways refill the slots the sortable ways occupied, in chain
//!   order. Chains are built from the first member that is not yet placed, so
//!   separate chains (gaps) keep their original relative order, a loop starts
//!   at its first member, and a list that is already in order is unchanged.
//! * A way with role `forward` is walked first node to last node, `backward`
//!   the other way round; any other role may be walked either way.
//! * A closed way (roundabout) connects to the neighbours on any of its nodes.
//! * Ways of different "kinds" are never linked: the roles `""`, `forward`
//!   and `backward` are one kind, every other role (`outer`, `inner`, ...) is
//!   its own.
//! * A way listed twice (an out-and-back route) is two separate entries.

use crate::osm::{MemberType, OsmMember};

/// The result of sorting: the new member list and how many separate chains
/// the sortable ways formed (1 means fully connected, 0 no sortable ways).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sorted {
    pub members: Vec<OsmMember>,
    pub chains: usize,
}

/// Which way an entry may be walked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Forward,
    Backward,
    Either,
}

struct Entry {
    /// Index into the member list.
    slot: usize,
    dir: Dir,
    kind: String,
    first: i64,
    last: i64,
    /// All nodes, when the way is closed (a roundabout or ring).
    ring: Option<Vec<i64>>,
}

impl Entry {
    /// The nodes this entry can be entered / left at when walked in `forward`
    /// direction (`true`: first to last).
    fn entry(&self, forward: bool) -> Vec<i64> {
        match &self.ring {
            Some(r) => r.clone(),
            None => vec![if forward { self.first } else { self.last }],
        }
    }
    fn exit(&self, forward: bool) -> Vec<i64> {
        self.entry(!forward)
    }
    fn allows(&self, forward: bool) -> bool {
        self.ring.is_some()
            || match self.dir {
                Dir::Either => true,
                Dir::Forward => forward,
                Dir::Backward => !forward,
            }
    }
}

fn kind_of(role: &str) -> String {
    match role {
        "" | "forward" | "backward" => String::new(),
        other => other.to_string(),
    }
}

fn touches(a: &[i64], b: &[i64]) -> bool {
    a.iter().any(|n| b.contains(n))
}

/// Sort the members of a relation. `way_nodes` returns the node ids of a way
/// member, or `None` if it is not downloaded.
pub fn sort_members(members: &[OsmMember], way_nodes: &dyn Fn(i64) -> Option<Vec<i64>>) -> Sorted {
    let mut entries: Vec<Entry> = Vec::new();
    for (slot, m) in members.iter().enumerate() {
        if m.member_type != MemberType::Way {
            continue;
        }
        if m.role.starts_with("stop") || m.role.starts_with("platform") {
            continue;
        }
        let Some(nodes) = way_nodes(m.reference) else {
            continue;
        };
        if nodes.len() < 2 {
            continue;
        }
        let first = nodes[0];
        let last = nodes[nodes.len() - 1];
        entries.push(Entry {
            slot,
            dir: match m.role.as_str() {
                "forward" => Dir::Forward,
                "backward" => Dir::Backward,
                _ => Dir::Either,
            },
            kind: kind_of(&m.role),
            first,
            last,
            ring: (first == last && nodes.len() > 2).then_some(nodes),
        });
    }

    let mut remaining: Vec<usize> = (0..entries.len()).collect();
    let mut order: Vec<usize> = Vec::with_capacity(entries.len());
    let mut chains = 0;
    while let Some(&start) = remaining.first() {
        let mut best: Option<Vec<(usize, bool)>> = None;
        for forward in [true, false] {
            if !entries[start].allows(forward) {
                continue;
            }
            let chain = build_chain(&entries, &remaining, start, forward);
            let better = match &best {
                None => true,
                Some(b) => preference(&chain) < preference(b),
            };
            if better {
                best = Some(chain);
            }
        }
        // `allows` holds for at least one direction, so `best` is set.
        let chain: Vec<usize> = best
            .unwrap_or_else(|| vec![(start, true)])
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        remaining.retain(|i| !chain.contains(i));
        order.extend(chain);
        chains += 1;
    }

    let mut out = members.to_vec();
    for (target, &e) in entries.iter().map(|e| e.slot).zip(order.iter()) {
        out[target] = members[entries[e].slot].clone();
    }
    Sorted {
        members: out,
        chains,
    }
}

/// The chain through `start` (walked in direction `start_forward`), grown at
/// the end first and then at the beginning, as entry indices in order.
fn build_chain(
    entries: &[Entry],
    remaining: &[usize],
    start: usize,
    start_forward: bool,
) -> Vec<(usize, bool)> {
    let mut used = vec![start];
    let mut chain = std::collections::VecDeque::from([(start, start_forward)]);
    let kind = &entries[start].kind;
    let mut tail = entries[start].exit(start_forward);
    loop {
        let next = remaining.iter().copied().find_map(|c| {
            if used.contains(&c) || entries[c].kind != *kind {
                return None;
            }
            [true, false]
                .into_iter()
                .find(|&f| entries[c].allows(f) && touches(&tail, &entries[c].entry(f)))
                .map(|f| (c, f))
        });
        let Some((c, f)) = next else { break };
        tail = entries[c].exit(f);
        used.push(c);
        chain.push_back((c, f));
    }
    let mut head = entries[start].entry(start_forward);
    loop {
        let prev = remaining.iter().copied().find_map(|c| {
            if used.contains(&c) || entries[c].kind != *kind {
                return None;
            }
            [true, false]
                .into_iter()
                .find(|&f| entries[c].allows(f) && touches(&head, &entries[c].exit(f)))
                .map(|f| (c, f))
        });
        let Some((c, f)) = prev else { break };
        head = entries[c].entry(f);
        used.push(c);
        chain.push_front((c, f));
    }
    chain.into_iter().collect()
}

/// Sort key for a candidate chain (smaller is better): longest first, then
/// one that keeps the members' current order, then the one that walks fewer
/// ways against their stored direction, then the smaller index sequence.
fn preference(chain: &[(usize, bool)]) -> (std::cmp::Reverse<usize>, bool, usize, Vec<usize>) {
    let indices: Vec<usize> = chain.iter().map(|&(c, _)| c).collect();
    let ascending = indices.windows(2).all(|w| w[0] < w[1]);
    let backward = chain.iter().filter(|&&(_, f)| !f).count();
    (
        std::cmp::Reverse(chain.len()),
        !ascending,
        backward,
        indices,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn way(id: i64, role: &str) -> OsmMember {
        OsmMember {
            member_type: MemberType::Way,
            reference: id,
            role: role.to_string(),
        }
    }
    fn node(id: i64) -> OsmMember {
        OsmMember {
            member_type: MemberType::Node,
            reference: id,
            role: "stop".to_string(),
        }
    }
    fn ids(m: &[OsmMember]) -> Vec<i64> {
        m.iter().map(|m| m.reference).collect()
    }
    fn sort(members: &[OsmMember], ways: &[(i64, &[i64])]) -> Sorted {
        let map: HashMap<i64, Vec<i64>> = ways.iter().map(|&(i, n)| (i, n.to_vec())).collect();
        sort_members(members, &|id| map.get(&id).cloned())
    }

    #[test]
    fn orders_a_scrambled_chain_without_reversing() {
        let ways: &[(i64, &[i64])] = &[(1, &[1, 2]), (2, &[2, 3]), (3, &[3, 4])];
        let r = sort(&[way(3, ""), way(1, ""), way(2, "")], ways);
        assert_eq!(ids(&r.members), vec![1, 2, 3]);
        assert_eq!(r.chains, 1);
    }

    #[test]
    fn reversed_way_keeps_its_place_in_the_walk() {
        // Way 2 is stored 4->3 but is walked 3->4 backwards.
        let ways: &[(i64, &[i64])] = &[(1, &[1, 3]), (2, &[4, 3]), (3, &[4, 5])];
        let r = sort(&[way(2, ""), way(3, ""), way(1, "")], ways);
        assert_eq!(ids(&r.members), vec![1, 2, 3]);
    }

    #[test]
    fn already_sorted_is_unchanged() {
        let ways: &[(i64, &[i64])] = &[(1, &[1, 2]), (2, &[2, 3]), (3, &[3, 1])];
        let m = [way(1, ""), way(2, ""), way(3, "")];
        assert_eq!(sort(&m, ways).members, m.to_vec());
    }

    #[test]
    fn roles_travel_with_their_members() {
        let ways: &[(i64, &[i64])] = &[(1, &[1, 2]), (2, &[2, 3])];
        let r = sort(&[way(2, "backward"), way(1, "forward")], ways);
        // 1 forward ends at 2; 2 backward (3 -> 2) cannot follow, so the
        // order stays two chains in original order.
        assert_eq!(ids(&r.members), vec![2, 1]);
        assert_eq!(r.chains, 2);
        let r = sort(&[way(2, "forward"), way(1, "forward")], ways);
        assert_eq!(ids(&r.members), vec![1, 2]);
        assert_eq!(r.members[0].role, "forward");
    }

    #[test]
    fn non_ways_and_incomplete_ways_stay_in_place() {
        let ways: &[(i64, &[i64])] = &[(1, &[1, 2]), (2, &[2, 3]), (3, &[3, 4])];
        let m = [node(50), way(3, ""), way(900, ""), way(1, ""), way(2, "")];
        let r = sort(&m, ways);
        assert_eq!(ids(&r.members), vec![50, 1, 900, 2, 3]);
    }

    #[test]
    fn platform_ways_are_not_chained() {
        let ways: &[(i64, &[i64])] = &[(1, &[1, 2]), (2, &[2, 3]), (3, &[3, 4]), (7, &[7, 8])];
        let r = sort(
            &[way(3, ""), way(7, "platform"), way(1, ""), way(2, "")],
            ways,
        );
        assert_eq!(ids(&r.members), vec![1, 7, 2, 3]);
    }

    #[test]
    fn gaps_keep_chain_order() {
        let ways: &[(i64, &[i64])] = &[
            (1, &[1, 2]),
            (2, &[2, 3]),
            (5, &[3, 4]),
            (3, &[10, 11]),
            (4, &[11, 12]),
        ];
        let m = [way(2, ""), way(4, ""), way(5, ""), way(3, ""), way(1, "")];
        let r = sort(&m, ways);
        assert_eq!(ids(&r.members), vec![1, 2, 5, 4, 3]);
        assert_eq!(r.chains, 2);
    }

    #[test]
    fn out_and_back_duplicates_are_kept() {
        let ways: &[(i64, &[i64])] = &[(1, &[1, 2]), (2, &[2, 3])];
        let m = [way(1, ""), way(2, ""), way(2, ""), way(1, "")];
        assert_eq!(sort(&m, ways).members, m.to_vec());
        // Also a valid walk (2 backwards first), so it is left alone.
        let m = [way(2, ""), way(1, ""), way(1, ""), way(2, "")];
        assert_eq!(sort(&m, ways).members, m.to_vec());
    }

    #[test]
    fn roundabout_connects_on_any_node() {
        // Ring 9 over nodes 2,5,6; way 1 ends at 2, way 3 starts at 6.
        let ways: &[(i64, &[i64])] = &[(1, &[1, 2]), (9, &[2, 5, 6, 2]), (3, &[6, 7])];
        let r = sort(&[way(9, ""), way(3, ""), way(1, "")], ways);
        assert_eq!(ids(&r.members), vec![1, 9, 3]);
        assert_eq!(r.chains, 1);
    }

    #[test]
    fn loop_starts_at_first_member() {
        let ways: &[(i64, &[i64])] = &[(1, &[1, 2]), (2, &[2, 3]), (3, &[3, 4]), (4, &[4, 1])];
        let r = sort(&[way(2, ""), way(4, ""), way(1, ""), way(3, "")], ways);
        assert_eq!(ids(&r.members), vec![2, 3, 4, 1]);
        assert_eq!(r.chains, 1);
    }

    #[test]
    fn different_role_kinds_do_not_link() {
        let ways: &[(i64, &[i64])] = &[(1, &[1, 2]), (2, &[2, 3]), (3, &[3, 1])];
        let m = [way(1, "outer"), way(3, "inner"), way(2, "outer")];
        let r = sort(&m, ways);
        assert_eq!(ids(&r.members), vec![1, 2, 3]);
        assert_eq!(r.members[1].role, "outer");
    }

    #[test]
    fn no_ways_is_zero_chains() {
        let r = sort(&[node(1)], &[]);
        assert_eq!(r.chains, 0);
    }
}
