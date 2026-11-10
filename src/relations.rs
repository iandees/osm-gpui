//! Pure rules for keeping relations consistent when ways and nodes are split,
//! merged, joined or deleted. Nothing here touches a layer or the UI: each
//! function takes a relation (or its member list) and the ids involved, and
//! returns the member list the relation should have afterwards.
//!
//! The rules are those of the editors this app follows (iD, JOSM), reduced to
//! what can be decided from the relation's tags and members alone:
//!
//! - **Split**: the new half joins every relation the original way is in, with
//!   the same role, right next to the original on the side that keeps the
//!   member sequence connected. Turn restrictions are special: only the half
//!   that touches the `via` member stays a `from`/`to` member.
//! - **Merge nodes**: the removed node's memberships move to the survivor.
//! - **Join ways**: the removed way's memberships fold into the survivor's.
//! - **Delete**: the memberships of the deleted feature are dropped, unless
//!   that would leave an empty relation or a broken turn restriction.

use crate::osm::{MemberType, OsmMember, OsmRelation};
use crate::tr;
use std::collections::{HashMap, HashSet};

/// The roles every turn restriction needs.
const RESTRICTION_ROLES: [&str; 3] = ["from", "via", "to"];

fn relation_type(tags: &HashMap<String, String>) -> Option<&str> {
    tags.get("type").map(String::as_str)
}

/// A turn restriction: `type=restriction` or one of its `restriction:*`
/// variants (`restriction:bus`, ...).
pub fn is_restriction(relation: &OsmRelation) -> bool {
    match relation_type(&relation.tags) {
        Some(kind) => kind == "restriction" || kind.starts_with("restriction:"),
        None => false,
    }
}

/// What to call a relation in the UI: its `name`, else `ref`, else its `type`,
/// else its id.
pub fn display_name(relation: &OsmRelation) -> String {
    ["name", "ref", "type"]
        .iter()
        .filter_map(|key| relation.tags.get(*key))
        .find(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| relation.id.to_string())
}

/// The roles `member_type`/`id` has in `relation`, in member order (empty
/// when it isn't a member; one entry per membership).
pub fn roles_of(relation: &OsmRelation, member_type: MemberType, id: i64) -> Vec<&str> {
    relation
        .members
        .iter()
        .filter(|m| m.member_type == member_type && m.reference == id)
        .map(|m| m.role.as_str())
        .collect()
}

fn is_way_member(member: &OsmMember, way_id: i64) -> bool {
    member.member_type == MemberType::Way && member.reference == way_id
}

// ---------------------------------------------------------------------------
// Members as map features

/// The nodes and ways a relation stands for on the map, in member order
/// without repeats: its own node and way members, plus those of the relations
/// among its members (one level of nesting; a nested relation's own relation
/// members are not followed). `find` looks a relation up by id.
pub fn flatten_members(
    relation: &OsmRelation,
    find: &dyn Fn(i64) -> Option<OsmRelation>,
) -> Vec<(MemberType, i64)> {
    fn push(
        out: &mut Vec<(MemberType, i64)>,
        seen: &mut HashSet<(MemberType, i64)>,
        member: &OsmMember,
    ) {
        let key = (member.member_type, member.reference);
        if seen.insert(key) {
            out.push(key);
        }
    }
    let mut out: Vec<(MemberType, i64)> = Vec::new();
    let mut seen = HashSet::new();
    for member in &relation.members {
        match member.member_type {
            MemberType::Node | MemberType::Way => push(&mut out, &mut seen, member),
            MemberType::Relation => {
                if member.reference == relation.id {
                    continue;
                }
                if let Some(nested) = find(member.reference) {
                    for inner in &nested.members {
                        if inner.member_type != MemberType::Relation {
                            push(&mut out, &mut seen, inner);
                        }
                    }
                }
            }
        }
    }
    out
}

/// How to resolve members to geometry; `None` from a lookup means the
/// element isn't loaded.
pub struct MemberLookup<'a> {
    /// A node's `(lat, lon)`.
    pub node: &'a dyn Fn(i64) -> Option<(f64, f64)>,
    /// A way's node ids.
    pub way_nodes: &'a dyn Fn(i64) -> Option<Vec<i64>>,
    pub relation: &'a dyn Fn(i64) -> Option<OsmRelation>,
}

/// The bounding box `(min_lat, max_lat, min_lon, max_lon)` of the members of
/// `relation` that are loaded (nested relations followed one level), or
/// `None` if none of them is.
pub fn members_bounds(
    relation: &OsmRelation,
    lookup: &MemberLookup,
) -> Option<(f64, f64, f64, f64)> {
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    let mut extend = |(lat, lon): (f64, f64)| {
        let b = bounds.get_or_insert((lat, lat, lon, lon));
        b.0 = b.0.min(lat);
        b.1 = b.1.max(lat);
        b.2 = b.2.min(lon);
        b.3 = b.3.max(lon);
    };
    for (member_type, id) in flatten_members(relation, lookup.relation) {
        match member_type {
            MemberType::Node => {
                if let Some(pos) = (lookup.node)(id) {
                    extend(pos);
                }
            }
            MemberType::Way => {
                for node_id in (lookup.way_nodes)(id).unwrap_or_default() {
                    if let Some(pos) = (lookup.node)(node_id) {
                        extend(pos);
                    }
                }
            }
            MemberType::Relation => {}
        }
    }
    bounds
}

// ---------------------------------------------------------------------------
// Split Way

/// One way being split, as the relation rules need to see it.
#[derive(Debug, Clone, Copy)]
pub struct SplitStep<'a> {
    /// The original way, which keeps its id and the `keep` nodes.
    pub way_id: i64,
    /// The id of the new way holding the `new` nodes.
    pub new_id: i64,
    /// The original way's node list before the split.
    pub before: &'a [i64],
    pub keep: &'a [i64],
    pub new: &'a [i64],
}

/// Why a way can't be split without breaking a relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitError {
    /// In a turn restriction the way is a `from`/`to` member and must follow
    /// whichever half touches the `via` member, but that can't be told.
    ViaUnknown { relation_id: i64, way_id: i64 },
}

impl SplitError {
    /// A short status-bar message.
    pub fn message(&self) -> String {
        match self {
            SplitError::ViaUnknown {
                relation_id,
                way_id,
            } => tr!(
                "action.split.error.restriction_via",
                way = way_id,
                relation = relation_id
            )
            .to_string(),
        }
    }
}

/// The node of the `before` list where the restriction's `via` member meets
/// the way: the `via` node member, or the end of a `via` way member that lies
/// on the way. `None` unless there is exactly one such node.
fn via_node(
    relation: &OsmRelation,
    before: &[i64],
    way_nodes: &dyn Fn(i64) -> Option<Vec<i64>>,
) -> Option<i64> {
    let via = || relation.members.iter().filter(|m| m.role == "via");
    let mut candidates: Vec<i64> = via()
        .filter(|m| m.member_type == MemberType::Node && before.contains(&m.reference))
        .map(|m| m.reference)
        .collect();
    let has_via_node = via().any(|m| m.member_type == MemberType::Node);
    if !has_via_node {
        for member in via().filter(|m| m.member_type == MemberType::Way) {
            let Some(nodes) = way_nodes(member.reference) else {
                continue;
            };
            let ends = [nodes.first().copied(), nodes.last().copied()];
            candidates.extend(ends.into_iter().flatten().filter(|n| before.contains(n)));
        }
    }
    candidates.sort_unstable();
    candidates.dedup();
    match candidates.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// Which side of the original member the new way belongs on: `Some(true)` for
/// after it, `Some(false)` for before it, `None` when the neighbours don't
/// say. Looks at the nearest other way member on each side; a neighbour that
/// shares a node with exactly one of the halves says that half is adjacent on
/// its side.
fn new_goes_after(
    members: &[OsmMember],
    index: usize,
    step: &SplitStep,
    way_nodes: &dyn Fn(i64) -> Option<Vec<i64>>,
) -> Option<bool> {
    let is_other_way =
        |m: &&OsmMember| m.member_type == MemberType::Way && m.reference != step.way_id;
    let previous = members[..index].iter().rev().find(is_other_way);
    let next = members[index + 1..].iter().find(is_other_way);

    // Which half (true = the original's `keep`) a neighbour touches.
    let touches = |neighbour: Option<&OsmMember>| -> Option<bool> {
        let nodes = way_nodes(neighbour?.reference)?;
        let shares = |half: &[i64]| half.iter().any(|n| nodes.contains(n));
        match (shares(step.keep), shares(step.new)) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            _ => None,
        }
    };
    // Original first, new second, is "after"; so a previous neighbour on the
    // kept half, or a next neighbour on the new half, both mean "after".
    let from_previous = touches(previous);
    let from_next = touches(next).map(|keep_is_last| !keep_is_last);
    match (from_previous, from_next) {
        (Some(a), Some(b)) if a != b => None,
        (Some(a), _) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// The member list `relation` should have after `step` splits one of its
/// ways, or `None` if the way isn't a member.
///
/// Every membership of the way gets a sibling for the new way with the same
/// role (so a closed multipolygon ring split in two keeps both halves, and a
/// route keeps all its parts), inserted before or after the original so
/// consecutive ways stay connected, defaulting to after. In a turn
/// restriction, a `from`/`to` member instead follows the half that touches the
/// `via` member: unchanged if that is the original, re-pointed to the new way
/// if that is the new half; if the half can't be determined the split is
/// refused.
pub fn split_members(
    relation: &OsmRelation,
    step: &SplitStep,
    way_nodes: &dyn Fn(i64) -> Option<Vec<i64>>,
) -> Result<Option<Vec<OsmMember>>, SplitError> {
    let positions: Vec<usize> = (0..relation.members.len())
        .filter(|&i| is_way_member(&relation.members[i], step.way_id))
        .collect();
    if positions.is_empty() {
        return Ok(None);
    }
    let restriction = is_restriction(relation);
    let mut members = relation.members.clone();
    // Back to front, so earlier positions are still valid after an insert.
    for &i in positions.iter().rev() {
        let role = relation.members[i].role.as_str();
        if restriction && (role == "from" || role == "to") {
            let unknown = || SplitError::ViaUnknown {
                relation_id: relation.id,
                way_id: step.way_id,
            };
            let via = via_node(relation, step.before, way_nodes).ok_or_else(unknown)?;
            match (step.keep.contains(&via), step.new.contains(&via)) {
                (true, false) => {}
                (false, true) => members[i].reference = step.new_id,
                _ => return Err(unknown()),
            }
            continue;
        }
        let after = new_goes_after(&relation.members, i, step, way_nodes).unwrap_or(true);
        let sibling = OsmMember {
            reference: step.new_id,
            ..relation.members[i].clone()
        };
        members.insert(if after { i + 1 } else { i }, sibling);
    }
    Ok(Some(members))
}

// ---------------------------------------------------------------------------
// Merge Nodes

/// The member list after the nodes in `removed` are replaced by `keep`, or
/// `None` if none of them is a member. A replacement that leaves two equal
/// members (same type, ref and role) next to each other drops the second.
pub fn merge_node_members(
    members: &[OsmMember],
    removed: &[i64],
    keep: i64,
) -> Option<Vec<OsmMember>> {
    let is_removed =
        |m: &OsmMember| m.member_type == MemberType::Node && removed.contains(&m.reference);
    if !members.iter().any(is_removed) {
        return None;
    }
    let mut out: Vec<OsmMember> = Vec::with_capacity(members.len());
    // Whether the last member written is one that was just replaced.
    let mut last_replaced = false;
    for member in members {
        let replaced = is_removed(member);
        let member = if replaced {
            OsmMember {
                reference: keep,
                ..member.clone()
            }
        } else {
            member.clone()
        };
        if (replaced || last_replaced) && out.last() == Some(&member) {
            last_replaced = true;
            continue;
        }
        last_replaced = replaced;
        out.push(member);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Join Ways

/// Why joining ways would break a relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinError {
    /// A way to be removed is part of a turn restriction.
    Restriction { relation_id: i64, way_id: i64 },
    /// A way to be removed has a different role than the surviving way, which
    /// is already a member of the same relation.
    RoleMismatch { relation_id: i64, way_id: i64 },
}

impl JoinError {
    /// A short status-bar message.
    pub fn message(&self) -> String {
        match self {
            JoinError::Restriction {
                relation_id,
                way_id,
            } => tr!(
                "action.join.error.restriction",
                way = way_id,
                relation = relation_id
            )
            .to_string(),
            JoinError::RoleMismatch {
                relation_id,
                way_id,
            } => tr!(
                "action.join.error.role_mismatch",
                way = way_id,
                relation = relation_id
            )
            .to_string(),
        }
    }
}

/// The member list after the ways in `removed` are folded into `keep`, or
/// `None` if none of them is a member.
///
/// Per membership of a removed way: if `keep` is a member with the same role,
/// the membership is dropped; if `keep` is not a member at all, the
/// membership points at `keep` instead; if `keep` is a member only with other
/// roles, or the relation is a turn restriction, the join is refused.
pub fn join_members(
    relation: &OsmRelation,
    keep: i64,
    removed: &[i64],
) -> Result<Option<Vec<OsmMember>>, JoinError> {
    let mut members = relation.members.clone();
    let mut changed = false;
    for &way_id in removed {
        while let Some(at) = members.iter().position(|m| is_way_member(m, way_id)) {
            if is_restriction(relation) {
                return Err(JoinError::Restriction {
                    relation_id: relation.id,
                    way_id,
                });
            }
            changed = true;
            let role = members[at].role.clone();
            let survivor_roles = roles_of_members(&members, keep);
            if survivor_roles.is_empty() {
                members[at].reference = keep;
            } else if survivor_roles.contains(&role) {
                members.remove(at);
            } else {
                return Err(JoinError::RoleMismatch {
                    relation_id: relation.id,
                    way_id,
                });
            }
        }
    }
    Ok(changed.then_some(members))
}

fn roles_of_members(members: &[OsmMember], way_id: i64) -> Vec<String> {
    members
        .iter()
        .filter(|m| is_way_member(m, way_id))
        .map(|m| m.role.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Delete

/// Why a node or way can't be deleted without breaking a relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteError {
    /// It is all that is left of the relation, and relations can't be
    /// deleted yet.
    WouldEmpty { relation_id: i64 },
    /// It is the turn restriction's only member with this role.
    RestrictionRole {
        relation_id: i64,
        role: &'static str,
    },
}

impl DeleteError {
    /// A short status-bar message naming the feature (`what` is "way" or
    /// "node", localized by the caller).
    pub fn message(&self, what: &str, id: i64) -> String {
        match self {
            DeleteError::WouldEmpty { relation_id } => tr!(
                "action.delete.error.relation_empty",
                what = what,
                id = id,
                relation = relation_id
            )
            .to_string(),
            DeleteError::RestrictionRole { relation_id, role } => tr!(
                "action.delete.error.restriction_role",
                what = what,
                id = id,
                relation = relation_id,
                role = role
            )
            .to_string(),
        }
    }
}

/// The member list after every membership of `member_type`/`id` is dropped,
/// or `None` if it isn't a member. Refused if that would leave the relation
/// without members, or a turn restriction without a `from`, `via` or `to`.
pub fn delete_member(
    relation: &OsmRelation,
    member_type: MemberType,
    id: i64,
) -> Result<Option<Vec<OsmMember>>, DeleteError> {
    let is_target = |m: &OsmMember| m.member_type == member_type && m.reference == id;
    if !relation.members.iter().any(is_target) {
        return Ok(None);
    }
    let members: Vec<OsmMember> = relation
        .members
        .iter()
        .filter(|m| !is_target(m))
        .cloned()
        .collect();
    if members.is_empty() {
        return Err(DeleteError::WouldEmpty {
            relation_id: relation.id,
        });
    }
    if is_restriction(relation) {
        let has = |list: &[OsmMember], role: &str| list.iter().any(|m| m.role == role);
        for role in RESTRICTION_ROLES {
            if has(&relation.members, role) && !has(&members, role) {
                return Err(DeleteError::RestrictionRole {
                    relation_id: relation.id,
                    role,
                });
            }
        }
    }
    Ok(Some(members))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(member_type: MemberType, reference: i64, role: &str) -> OsmMember {
        OsmMember {
            member_type,
            reference,
            role: role.to_string(),
        }
    }

    fn way(reference: i64, role: &str) -> OsmMember {
        member(MemberType::Way, reference, role)
    }

    fn node(reference: i64, role: &str) -> OsmMember {
        member(MemberType::Node, reference, role)
    }

    fn relation(kind: &str, members: Vec<OsmMember>) -> OsmRelation {
        OsmRelation {
            id: 900,
            version: 1,
            tags: HashMap::from([("type".to_string(), kind.to_string())]),
            members,
        }
    }

    fn refs(members: &[OsmMember]) -> Vec<(i64, &str)> {
        members
            .iter()
            .map(|m| (m.reference, m.role.as_str()))
            .collect()
    }

    /// Way geometry for the lookups: ways by id.
    fn lookup(ways: &[(i64, &[i64])]) -> impl Fn(i64) -> Option<Vec<i64>> {
        let ways: Vec<(i64, Vec<i64>)> = ways.iter().map(|(i, n)| (*i, n.to_vec())).collect();
        move |id| ways.iter().find(|(i, _)| *i == id).map(|(_, n)| n.clone())
    }

    const NEW: i64 = -7;

    /// Way 20 = [1,2,3,4,5] split at 3 into keep [1,2,3] and new [3,4,5].
    fn step() -> SplitStep<'static> {
        SplitStep {
            way_id: 20,
            new_id: NEW,
            before: &[1, 2, 3, 4, 5],
            keep: &[1, 2, 3],
            new: &[3, 4, 5],
        }
    }

    #[test]
    fn display_name_prefers_name_then_ref_then_type_then_id() {
        let mut r = relation("route", vec![]);
        assert_eq!(display_name(&r), "route");
        r.tags.insert("ref".into(), "42".into());
        assert_eq!(display_name(&r), "42");
        r.tags.insert("name".into(), "Blue Line".into());
        assert_eq!(display_name(&r), "Blue Line");
        r.tags.insert("name".into(), "  ".into());
        assert_eq!(display_name(&r), "42");
        r.tags.clear();
        assert_eq!(display_name(&r), "900");
    }

    #[test]
    fn restriction_types_include_the_prefixed_variants() {
        assert!(is_restriction(&relation("restriction", vec![])));
        assert!(is_restriction(&relation("restriction:bus", vec![])));
        assert!(!is_restriction(&relation("route", vec![])));
        assert!(!is_restriction(&relation("restrictions", vec![])));
    }

    #[test]
    fn roles_of_lists_every_membership() {
        let r = relation("route", vec![way(1, "a"), node(1, "x"), way(1, "b")]);
        assert_eq!(roles_of(&r, MemberType::Way, 1), vec!["a", "b"]);
        assert_eq!(roles_of(&r, MemberType::Node, 1), vec!["x"]);
        assert!(roles_of(&r, MemberType::Way, 2).is_empty());
    }

    // -- split --

    #[test]
    fn split_without_a_membership_changes_nothing() {
        let r = relation("route", vec![way(10, ""), way(30, "")]);
        assert_eq!(split_members(&r, &step(), &lookup(&[])), Ok(None));
    }

    #[test]
    fn split_inserts_the_new_way_after_the_original_with_the_same_role() {
        // No way neighbours at all: default is after.
        let r = relation("route", vec![node(9, "stop"), way(20, "forward")]);
        let m = split_members(&r, &step(), &lookup(&[])).unwrap().unwrap();
        assert_eq!(
            refs(&m),
            vec![(9, "stop"), (20, "forward"), (NEW, "forward")]
        );
        assert_eq!(m[2].member_type, MemberType::Way);
    }

    #[test]
    fn split_orders_the_halves_by_the_previous_neighbour() {
        // Previous way ends at node 5, which belongs to the *new* half, so
        // the route runs 5 -> 3 -> 1: the new half comes first.
        let ways = lookup(&[(10, &[7, 6, 5]), (30, &[1, 0])]);
        let r = relation("route", vec![way(10, ""), way(20, ""), way(30, "")]);
        let m = split_members(&r, &step(), &ways).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(10, ""), (NEW, ""), (20, ""), (30, "")]);

        // Previous way ends at node 1 (the kept half): original first.
        let ways = lookup(&[(10, &[0, 1]), (30, &[5, 6])]);
        let m = split_members(&r, &step(), &ways).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(10, ""), (20, ""), (NEW, ""), (30, "")]);
    }

    #[test]
    fn split_orders_the_halves_by_the_next_neighbour_when_there_is_no_previous() {
        // Next way touches the kept half, so the new half goes first.
        let ways = lookup(&[(30, &[1, 0])]);
        let r = relation("route", vec![way(20, ""), way(30, "")]);
        let m = split_members(&r, &step(), &ways).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(NEW, ""), (20, ""), (30, "")]);
        // Next way touches the new half: original first, as by default.
        let ways = lookup(&[(30, &[5, 6])]);
        let m = split_members(&r, &step(), &ways).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(20, ""), (NEW, ""), (30, "")]);
    }

    #[test]
    fn split_skips_non_way_members_and_ignores_unconnected_or_ambiguous_neighbours() {
        let r = relation(
            "route",
            vec![way(10, ""), node(9, "stop"), way(20, ""), way(30, "")],
        );
        // The nearest way before is 10 even with a stop in between.
        let ways = lookup(&[(10, &[7, 5]), (30, &[8, 9])]);
        let m = split_members(&r, &step(), &ways).unwrap().unwrap();
        assert_eq!(m[2].reference, NEW);
        assert_eq!(m[3].reference, 20);
        // A neighbour touching neither half, or both (the split node), says
        // nothing: after.
        let ways = lookup(&[(10, &[7, 8]), (30, &[3, 9])]);
        let m = split_members(&r, &step(), &ways).unwrap().unwrap();
        assert_eq!(refs(&m)[3], (NEW, ""));
        // Neighbours that disagree: after.
        let ways = lookup(&[(10, &[0, 1]), (30, &[1, 0])]);
        let m = split_members(&r, &step(), &ways).unwrap().unwrap();
        assert_eq!(refs(&m)[3], (NEW, ""));
    }

    #[test]
    fn split_handles_a_way_that_is_a_member_twice() {
        let r = relation(
            "route",
            vec![way(20, "forward"), node(1, ""), way(20, "backward")],
        );
        let m = split_members(&r, &step(), &lookup(&[])).unwrap().unwrap();
        assert_eq!(
            refs(&m),
            vec![
                (20, "forward"),
                (NEW, "forward"),
                (1, ""),
                (20, "backward"),
                (NEW, "backward")
            ]
        );
    }

    #[test]
    fn split_of_a_multipolygon_ring_keeps_both_halves_with_the_role() {
        // A closed way [1,2,3,4,1] split at 2 and 4 into two open arcs.
        let step = SplitStep {
            way_id: 20,
            new_id: NEW,
            before: &[1, 2, 3, 4, 1],
            keep: &[4, 1, 2],
            new: &[2, 3, 4],
        };
        let r = relation("multipolygon", vec![way(20, "outer"), way(21, "inner")]);
        let m = split_members(&r, &step, &lookup(&[(21, &[7, 8, 9, 7])]))
            .unwrap()
            .unwrap();
        assert_eq!(refs(&m), vec![(20, "outer"), (NEW, "outer"), (21, "inner")]);
    }

    fn restriction(members: Vec<OsmMember>) -> OsmRelation {
        relation("restriction", members)
    }

    #[test]
    fn restriction_from_way_follows_the_half_touching_the_via_node() {
        let r = restriction(vec![way(20, "from"), node(5, "via"), way(40, "to")]);
        // Via node 5 is on the new half: the member is re-pointed to it.
        let m = split_members(&r, &step(), &lookup(&[])).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(NEW, "from"), (5, "via"), (40, "to")]);
        assert_eq!(m.len(), 3, "the other half is not added");
        // Via node 1 is on the original's half: nothing changes.
        let r = restriction(vec![way(20, "from"), node(1, "via"), way(40, "to")]);
        let m = split_members(&r, &step(), &lookup(&[])).unwrap().unwrap();
        assert_eq!(m, r.members);
    }

    #[test]
    fn restriction_to_way_works_the_same_and_a_via_way_gives_its_shared_end() {
        let r = restriction(vec![way(40, "from"), way(50, "via"), way(20, "to")]);
        // Via way 50 ends at node 1, on the original's half.
        let ways = lookup(&[(50, &[9, 1])]);
        let m = split_members(&r, &step(), &ways).unwrap().unwrap();
        assert_eq!(m, r.members);
        // Via way ends at node 5: the "to" member becomes the new way.
        let ways = lookup(&[(50, &[5, 9])]);
        let m = split_members(&r, &step(), &ways).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(40, "from"), (50, "via"), (NEW, "to")]);
    }

    #[test]
    fn restriction_variants_are_handled_like_restrictions() {
        let r = relation(
            "restriction:hgv",
            vec![way(20, "from"), node(5, "via"), way(40, "to")],
        );
        let m = split_members(&r, &step(), &lookup(&[])).unwrap().unwrap();
        assert_eq!(m[0].reference, NEW);
    }

    #[test]
    fn restriction_with_an_unknown_via_refuses_the_split() {
        let refused = |r: OsmRelation, ways: &dyn Fn(i64) -> Option<Vec<i64>>| {
            split_members(&r, &step(), ways)
        };
        let err = SplitError::ViaUnknown {
            relation_id: 900,
            way_id: 20,
        };
        // No via at all.
        let r = restriction(vec![way(20, "from"), way(40, "to")]);
        assert_eq!(refused(r, &lookup(&[])), Err(err.clone()));
        // Via node not on the way.
        let r = restriction(vec![way(20, "from"), node(99, "via"), way(40, "to")]);
        assert_eq!(refused(r, &lookup(&[])), Err(err.clone()));
        // Via node is the split node: both halves touch it.
        let r = restriction(vec![way(20, "from"), node(3, "via"), way(40, "to")]);
        assert_eq!(refused(r, &lookup(&[])), Err(err.clone()));
        // Via way unknown, or sharing no end with the way.
        let r = restriction(vec![way(20, "from"), way(50, "via"), way(40, "to")]);
        assert_eq!(refused(r.clone(), &lookup(&[])), Err(err.clone()));
        assert_eq!(refused(r, &lookup(&[(50, &[8, 9])])), Err(err));
    }

    #[test]
    fn split_error_message_names_the_way_and_relation() {
        let msg = SplitError::ViaUnknown {
            relation_id: 900,
            way_id: 20,
        }
        .message();
        assert!(msg.contains("20") && msg.contains("900"), "{msg}");
    }

    #[test]
    fn a_via_way_of_a_restriction_is_split_like_any_member() {
        // Splitting the via way itself keeps both halves, in order.
        let r = restriction(vec![way(40, "from"), way(20, "via"), way(50, "to")]);
        let ways = lookup(&[(40, &[8, 5]), (50, &[1, 9])]);
        let m = split_members(&r, &step(), &ways).unwrap().unwrap();
        assert_eq!(
            refs(&m),
            vec![(40, "from"), (NEW, "via"), (20, "via"), (50, "to")]
        );
    }

    // -- merge nodes --

    #[test]
    fn merge_replaces_removed_nodes_and_keeps_the_role() {
        let members = vec![way(1, ""), node(5, "stop"), node(6, "stop_entry_only")];
        let m = merge_node_members(&members, &[5, 6], 4).unwrap();
        assert_eq!(refs(&m), vec![(1, ""), (4, "stop"), (4, "stop_entry_only")]);
        assert!(merge_node_members(&members, &[99], 4).is_none());
        // A way with the same id as a removed node is not touched.
        let members = vec![way(5, "")];
        assert!(merge_node_members(&members, &[5], 4).is_none());
    }

    #[test]
    fn merge_drops_an_adjacent_exact_duplicate_that_results() {
        let members = vec![
            node(4, "stop"),
            node(5, "stop"),
            way(1, ""),
            node(5, "stop"),
        ];
        let m = merge_node_members(&members, &[5], 4).unwrap();
        // 4,5 collapse; the later 5 is not adjacent to a 4 and stays.
        assert_eq!(refs(&m), vec![(4, "stop"), (1, ""), (4, "stop")]);
        // Different role: kept.
        let members = vec![node(4, "stop"), node(5, "platform")];
        let m = merge_node_members(&members, &[5], 4).unwrap();
        assert_eq!(m.len(), 2);
        // Several removed nodes collapsing into a run keep one.
        let members = vec![node(5, ""), node(6, ""), node(4, "")];
        let m = merge_node_members(&members, &[5, 6], 4).unwrap();
        assert_eq!(refs(&m), vec![(4, "")]);
        // Duplicates that were already there are left alone.
        let members = vec![node(8, ""), node(8, ""), node(5, "")];
        let m = merge_node_members(&members, &[5], 4).unwrap();
        assert_eq!(m.len(), 3);
    }

    // -- join ways --

    #[test]
    fn join_drops_a_removed_way_when_the_survivor_has_the_same_role() {
        let r = relation("route", vec![way(1, "fwd"), way(2, "fwd"), way(3, "fwd")]);
        let m = join_members(&r, 1, &[2]).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(1, "fwd"), (3, "fwd")]);
        // Several removed ways at once.
        let m = join_members(&r, 1, &[2, 3]).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(1, "fwd")]);
    }

    #[test]
    fn join_points_the_membership_at_the_survivor_when_it_is_not_a_member() {
        let r = relation("route", vec![way(7, ""), way(2, "fwd"), way(8, "")]);
        let m = join_members(&r, 1, &[2]).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(7, ""), (1, "fwd"), (8, "")]);
        // Two removed ways with the same role fold into one membership.
        let r = relation("route", vec![way(2, "fwd"), way(3, "fwd")]);
        let m = join_members(&r, 1, &[2, 3]).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(1, "fwd")]);
    }

    #[test]
    fn join_refuses_different_roles_and_restrictions() {
        let r = relation("route", vec![way(1, "forward"), way(2, "backward")]);
        let err = JoinError::RoleMismatch {
            relation_id: 900,
            way_id: 2,
        };
        assert_eq!(join_members(&r, 1, &[2]), Err(err.clone()));
        assert!(err.message().contains("role"), "{}", err.message());
        // Removed ways with different roles can't both become the survivor.
        let r = relation("route", vec![way(2, "a"), way(3, "b")]);
        assert!(matches!(
            join_members(&r, 1, &[2, 3]),
            Err(JoinError::RoleMismatch { way_id: 3, .. })
        ));
        let r = restriction(vec![way(1, "from"), way(2, "from")]);
        assert_eq!(
            join_members(&r, 1, &[2]),
            Err(JoinError::Restriction {
                relation_id: 900,
                way_id: 2
            })
        );
    }

    #[test]
    fn join_leaves_unrelated_relations_alone() {
        let r = relation("route", vec![way(1, ""), way(5, "")]);
        assert_eq!(join_members(&r, 1, &[2]), Ok(None));
        // The survivor alone being a member is no change either.
        let r = restriction(vec![way(1, "from")]);
        assert_eq!(join_members(&r, 1, &[2]), Ok(None));
    }

    // -- delete --

    #[test]
    fn delete_drops_every_membership_of_the_feature() {
        let r = relation(
            "route",
            vec![way(1, "a"), way(2, "b"), way(1, "c"), node(1, "")],
        );
        let m = delete_member(&r, MemberType::Way, 1).unwrap().unwrap();
        assert_eq!(refs(&m), vec![(2, "b"), (1, "")]);
        assert_eq!(delete_member(&r, MemberType::Way, 99), Ok(None));
    }

    #[test]
    fn delete_refuses_to_empty_a_relation() {
        let r = relation("route", vec![way(1, ""), way(1, "x")]);
        assert_eq!(
            delete_member(&r, MemberType::Way, 1),
            Err(DeleteError::WouldEmpty { relation_id: 900 })
        );
    }

    #[test]
    fn delete_refuses_to_break_a_restriction() {
        let r = restriction(vec![way(1, "from"), node(2, "via"), way(3, "to")]);
        for (kind, id, role) in [
            (MemberType::Way, 1, "from"),
            (MemberType::Node, 2, "via"),
            (MemberType::Way, 3, "to"),
        ] {
            assert_eq!(
                delete_member(&r, kind, id),
                Err(DeleteError::RestrictionRole {
                    relation_id: 900,
                    role
                })
            );
        }
        // A restriction with two `from` members can lose one.
        let r = restriction(vec![
            way(1, "from"),
            way(4, "from"),
            node(2, "via"),
            way(3, "to"),
        ]);
        assert!(delete_member(&r, MemberType::Way, 1).unwrap().is_some());
        // Other roles are free to go, and the variants count as restrictions.
        let r = relation(
            "restriction:hgv",
            vec![
                way(1, "from"),
                node(2, "via"),
                way(3, "to"),
                node(9, "hint"),
            ],
        );
        assert!(delete_member(&r, MemberType::Node, 9).unwrap().is_some());
        assert!(delete_member(&r, MemberType::Way, 1).is_err());
    }

    #[test]
    fn delete_error_messages_name_the_feature_and_relation() {
        let msg = DeleteError::WouldEmpty { relation_id: 900 }.message("way", 5);
        assert!(msg.contains("way 5") && msg.contains("900"), "{msg}");
        let msg = DeleteError::RestrictionRole {
            relation_id: 900,
            role: "via",
        }
        .message("node", 5);
        assert!(msg.contains("via") && msg.contains("900"), "{msg}");
    }

    // -- members as map features --

    fn rel(id: i64, members: Vec<OsmMember>) -> OsmRelation {
        OsmRelation {
            id,
            ..relation("route", members)
        }
    }

    #[test]
    fn flatten_members_follows_nested_relations_one_level_without_repeats() {
        let top = rel(
            1,
            vec![
                way(10, ""),
                member(MemberType::Relation, 2, ""),
                node(5, ""),
                member(MemberType::Relation, 404, ""),
                // A relation naming itself is not followed.
                member(MemberType::Relation, 1, ""),
            ],
        );
        let child = rel(
            2,
            vec![
                way(10, ""),
                way(11, ""),
                member(MemberType::Relation, 3, ""),
            ],
        );
        let find = |id: i64| (id == 2).then(|| child.clone());
        assert_eq!(
            flatten_members(&top, &find),
            vec![
                (MemberType::Way, 10),
                (MemberType::Way, 11),
                (MemberType::Node, 5)
            ]
        );
    }

    #[test]
    fn members_bounds_covers_loaded_members_only() {
        let r = rel(
            1,
            vec![
                node(1, ""),
                way(20, ""),
                way(21, ""),
                node(99, ""),
                member(MemberType::Relation, 2, ""),
            ],
        );
        let nested = rel(2, vec![node(3, "")]);
        let node_pos = |id: i64| match id {
            1 => Some((10.0, 20.0)),
            2 => Some((12.0, 18.0)),
            3 => Some((5.0, 30.0)),
            _ => None,
        };
        let way_nodes = |id: i64| (id == 20).then(|| vec![2, 777]);
        let find = |id: i64| (id == 2).then(|| nested.clone());
        let lookup = MemberLookup {
            node: &node_pos,
            way_nodes: &way_nodes,
            relation: &find,
        };
        assert_eq!(members_bounds(&r, &lookup), Some((5.0, 12.0, 18.0, 30.0)));

        let empty = rel(3, vec![node(99, ""), way(21, "")]);
        assert_eq!(members_bounds(&empty, &lookup), None);
    }
}
