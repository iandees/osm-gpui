//! A model of the road junction at one node, for the Junction inspector:
//! the road *arms* that meet there, the lanes on each arm in and out of the
//! junction (from `lanes*` and `turn:lanes*`), the movements between arms
//! and which turn restrictions allow or ban them, plus tagging problems.
//!
//! Pure data code: the caller supplies the OSM objects through
//! [`JunctionSource`], so the model can be built from a live layer or from a
//! plain [`OsmData`] in tests.

use crate::coordinates::LatLon;
use crate::ids::{NodeId, RelationId, WayId};
use crate::lanes::{is_road, lane_group, lanes_split_issue, Arrow, CountSource, LaneGroup};
use crate::osm::{MemberType, OsmData, OsmRelation};
use crate::tags::Tags;

/// Whether a lane with `arrow` can lead to an exit of this turn kind.
fn arrow_serves(arrow: Arrow, turn: TurnKind) -> bool {
    use TurnKind::*;
    match arrow {
        Arrow::SharpLeft | Arrow::Left | Arrow::SlightLeft => {
            matches!(turn, SharpLeft | Left | SlightLeft)
        }
        Arrow::SharpRight | Arrow::Right | Arrow::SlightRight => {
            matches!(turn, SharpRight | Right | SlightRight)
        }
        Arrow::Through | Arrow::MergeToLeft | Arrow::MergeToRight => {
            matches!(turn, Straight | SlightLeft | SlightRight)
        }
        Arrow::Reverse => turn == UTurn,
    }
}

/// What the junction model needs to read.
pub trait JunctionSource {
    /// Where a node is.
    fn node_pos(&self, id: NodeId) -> Option<LatLon>;
    /// The ways that use a node.
    fn ways_at(&self, id: NodeId) -> Vec<WayId>;
    fn way_nodes(&self, id: WayId) -> Option<Vec<NodeId>>;
    fn way_tags(&self, id: WayId) -> Option<Tags>;
    /// Relations with this node or one of these ways as a member.
    fn relations_touching(&self, node: NodeId, ways: &[WayId]) -> Vec<OsmRelation>;
}

impl JunctionSource for OsmData {
    fn node_pos(&self, id: NodeId) -> Option<LatLon> {
        self.nodes.get(&id).map(|n| LatLon::new(n.lat, n.lon))
    }
    fn ways_at(&self, id: NodeId) -> Vec<WayId> {
        let mut ways: Vec<WayId> = self
            .ways
            .values()
            .filter(|w| w.nodes.contains(&id))
            .map(|w| w.id)
            .collect();
        ways.sort_unstable();
        ways
    }
    fn way_nodes(&self, id: WayId) -> Option<Vec<NodeId>> {
        self.ways.get(&id).map(|w| w.nodes.clone())
    }
    fn way_tags(&self, id: WayId) -> Option<Tags> {
        self.ways.get(&id).map(|w| w.tags.clone())
    }
    fn relations_touching(&self, node: NodeId, ways: &[WayId]) -> Vec<OsmRelation> {
        self.relations
            .iter()
            .filter(|r| {
                r.members.iter().any(|m| match m.member_type {
                    MemberType::Node => m.reference == node.0,
                    MemberType::Way => ways.iter().any(|w| w.0 == m.reference),
                    MemberType::Relation => false,
                })
            })
            .cloned()
            .collect()
    }
}

/// A road leaving the junction node: one end of a way that ends there, or
/// one side of a way that passes through.
#[derive(Debug, Clone, PartialEq)]
pub struct Arm {
    pub way: WayId,
    /// Runs with the way's node order away from the node.
    pub outward: bool,
    /// Degrees clockwise from north, from the node towards the arm.
    pub bearing: f32,
    pub label: String,
    /// Traffic driving towards the node; `None` when the arm is a oneway out.
    pub inbound: Option<LaneGroup>,
    /// Traffic driving away from the node; `None` when a oneway in.
    pub outbound: Option<LaneGroup>,
    /// The way's next node along the arm, which tells the arm's part apart
    /// after the way is split at the junction.
    pub next_node: NodeId,
    /// The way runs on past the junction node (rather than ending there).
    pub way_continues: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnKind {
    Straight,
    SlightLeft,
    Left,
    SharpLeft,
    SlightRight,
    Right,
    SharpRight,
    UTurn,
}

impl TurnKind {
    /// Classify a turn by its angle (degrees, positive right), as routers do.
    pub fn from_angle(a: f32) -> TurnKind {
        let m = a.abs();
        let left = a < 0.0;
        match m {
            m if m < 30.0 => TurnKind::Straight,
            m if m < 60.0 && left => TurnKind::SlightLeft,
            m if m < 60.0 => TurnKind::SlightRight,
            m if m < 140.0 && left => TurnKind::Left,
            m if m < 140.0 => TurnKind::Right,
            m if m < 170.0 && left => TurnKind::SharpLeft,
            m if m < 170.0 => TurnKind::SharpRight,
            _ => TurnKind::UTurn,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            TurnKind::Straight => "straight",
            TurnKind::SlightLeft => "slight_left",
            TurnKind::Left => "left",
            TurnKind::SharpLeft => "sharp_left",
            TurnKind::SlightRight => "slight_right",
            TurnKind::Right => "right",
            TurnKind::SharpRight => "sharp_right",
            TurnKind::UTurn => "u_turn",
        }
    }
}

/// Whether a movement may be driven.
#[derive(Debug, Clone, PartialEq)]
pub enum MovementState {
    Allowed,
    /// A `no_*` restriction bans it.
    Banned {
        relation: RelationId,
        value: String,
    },
    /// An `only_*` restriction from the same arm leads elsewhere.
    OnlyElsewhere {
        relation: RelationId,
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Movement {
    pub from: usize,
    pub to: usize,
    pub turn: TurnKind,
    pub state: MovementState,
    /// The `only_*` restriction that names this movement, if any.
    pub only: Option<RelationId>,
}

/// Something wrong with the junction's tagging.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// A stable id, such as `turn_lanes_count`.
    pub check: &'static str,
    /// The arm it concerns, if any.
    pub arm: Option<usize>,
    pub relation: Option<RelationId>,
    pub message: String,
}

/// A restriction touching the junction that is not drawn as a movement (its
/// via is a way, or it couldn't be resolved).
#[derive(Debug, Clone, PartialEq)]
pub struct ListedRestriction {
    pub relation: RelationId,
    pub value: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Junction {
    pub node: NodeId,
    pub arms: Vec<Arm>,
    pub movements: Vec<Movement>,
    pub listed: Vec<ListedRestriction>,
    pub issues: Vec<Issue>,
    /// The restrictions drawn as movements: id, the tag key that holds the
    /// value (`restriction` or `restriction:<mode>`), and the value.
    pub restrictions: Vec<(RelationId, String, String)>,
}

impl Junction {
    /// The movements out of arm `from`, in clockwise order of exit.
    pub fn movements_from(&self, from: usize) -> impl Iterator<Item = &Movement> {
        self.movements.iter().filter(move |m| m.from == from)
    }

    /// The arm to show movements from by default: the first with a
    /// restriction, else the first one traffic can enter from.
    pub fn default_from_arm(&self) -> Option<usize> {
        self.movements
            .iter()
            .find(|m| m.state != MovementState::Allowed)
            .or_else(|| self.movements.first())
            .map(|m| m.from)
    }
}

/// How far along an arm its bearing is measured, in metres.
const BEARING_SAMPLE_M: f64 = 20.0;

/// Local metres east/north of `b` from `a`.
fn delta_m(a: LatLon, b: LatLon) -> (f64, f64) {
    let k = a.lat.to_radians().cos();
    ((b.lon - a.lon) * 111_320.0 * k, (b.lat - a.lat) * 110_540.0)
}

/// The bearing from `nodes[0]` (the junction) along `nodes`, measured at a
/// point up to [`BEARING_SAMPLE_M`] along.
fn arm_bearing(src: &dyn JunctionSource, nodes: &[NodeId]) -> Option<f32> {
    let origin = src.node_pos(*nodes.first()?)?;
    let mut prev = origin;
    let mut walked = 0.0;
    let mut target = None;
    for &n in &nodes[1..] {
        let p = src.node_pos(n)?;
        let (dx, dy) = delta_m(prev, p);
        let seg = dx.hypot(dy);
        if walked + seg >= BEARING_SAMPLE_M && seg > 0.0 {
            let t = (BEARING_SAMPLE_M - walked) / seg;
            target = Some(LatLon::new(
                prev.lat + (p.lat - prev.lat) * t,
                prev.lon + (p.lon - prev.lon) * t,
            ));
            break;
        }
        walked += seg;
        prev = p;
        target = Some(p);
    }
    let (dx, dy) = delta_m(origin, target?);
    if dx == 0.0 && dy == 0.0 {
        return None;
    }
    Some((dx.atan2(dy).to_degrees() as f32).rem_euclid(360.0))
}

fn arm_label(way: WayId, tags: &Tags) -> String {
    match (tags.get("name"), tags.get("ref")) {
        (Some(n), _) => n.to_string(),
        (None, Some(r)) => r.to_string(),
        (None, None) => format!("w{}", way.0),
    }
}

/// The restriction value: `restriction`, else the first `restriction:<mode>`.
fn restriction_value(tags: &Tags) -> Option<(String, Option<String>)> {
    if let Some(v) = tags.get("restriction") {
        return Some((v.to_string(), None));
    }
    let mut keys: Vec<&String> = tags
        .keys()
        .filter(|k| k.starts_with("restriction:"))
        .collect();
    keys.sort();
    let k = keys.first()?;
    Some((
        tags.get(k)?.to_string(),
        Some(k["restriction:".len()..].to_string()),
    ))
}

fn is_restriction(tags: &Tags) -> bool {
    tags.get("type")
        .is_some_and(|t| t == "restriction" || t.starts_with("restriction:"))
}

/// The turn kinds a restriction value names, to check it against the
/// geometry. `None` for values that don't name one.
fn value_turns(value: &str) -> Option<&'static [TurnKind]> {
    use TurnKind::*;
    let turn = value
        .strip_prefix("no_")
        .or_else(|| value.strip_prefix("only_"))?;
    Some(match turn {
        "left_turn" => &[SlightLeft, Left, SharpLeft, UTurn],
        "right_turn" => &[SlightRight, Right, SharpRight],
        "straight_on" => &[Straight, SlightLeft, SlightRight],
        "u_turn" => &[UTurn, SharpLeft, SharpRight],
        _ => return None,
    })
}

/// Build the junction at `node`. `None` unless roads meet there: at least
/// three arms, from at least two ways.
pub fn build(src: &dyn JunctionSource, node: NodeId) -> Option<Junction> {
    let mut arms = Vec::new();
    let mut split_issues = Vec::new();
    for way in src.ways_at(node) {
        let Some(tags) = src.way_tags(way) else {
            continue;
        };
        if !is_road(&tags) {
            continue;
        }
        if let Some(message) = lanes_split_issue(&tags) {
            split_issues.push(Issue {
                check: "lanes_split",
                arm: None,
                relation: None,
                message: format!("{}: {message}", arm_label(way, &tags)),
            });
        }
        let Some(nodes) = src.way_nodes(way) else {
            continue;
        };
        let closed = nodes.len() > 2 && nodes.first() == nodes.last();
        let last = nodes.len() - 1;
        for (i, &n) in nodes.iter().enumerate() {
            if n != node || (closed && i == last) {
                continue;
            }
            // Outward along the node order, and back against it.
            let mut sides: Vec<(bool, Vec<NodeId>)> = Vec::new();
            if i < last {
                sides.push((true, nodes[i..].to_vec()));
            } else if closed {
                sides.push((true, nodes[..].to_vec()));
            }
            if i > 0 {
                sides.push((false, nodes[..=i].iter().rev().copied().collect()));
            } else if closed {
                sides.push((false, nodes.iter().rev().copied().collect()));
            }
            for (outward, path) in sides {
                let Some(bearing) = arm_bearing(src, &path) else {
                    continue;
                };
                // Driving away from the node along the node order is the
                // way's forward direction.
                arms.push(Arm {
                    way,
                    outward,
                    bearing,
                    label: arm_label(way, &tags),
                    inbound: lane_group(&tags, !outward),
                    outbound: lane_group(&tags, outward),
                    next_node: path[1],
                    way_continues: closed || (i > 0 && i < last),
                });
            }
        }
    }
    // A junction joins other roads: a vertex in the middle of one road, or
    // where one road is split into two ways, has only two arms.
    let mut ways: Vec<WayId> = arms.iter().map(|a| a.way).collect();
    ways.sort_unstable();
    ways.dedup();
    if arms.len() < 3 || ways.len() < 2 {
        return None;
    }
    arms.sort_by(|a, b| a.bearing.total_cmp(&b.bearing));

    let mut movements = Vec::new();
    for (from, a) in arms.iter().enumerate() {
        if a.inbound.is_none() {
            continue;
        }
        for (to, b) in arms.iter().enumerate() {
            if b.outbound.is_none() {
                continue;
            }
            let turn = if from == to {
                TurnKind::UTurn
            } else {
                // Heading in is the arm's bearing reversed.
                let a_in = a.bearing + 180.0;
                let ang = (b.bearing - a_in + 540.0).rem_euclid(360.0) - 180.0;
                TurnKind::from_angle(ang)
            };
            movements.push(Movement {
                from,
                to,
                turn,
                state: MovementState::Allowed,
                only: None,
            });
        }
    }

    let mut j = Junction {
        node,
        arms,
        movements,
        listed: Vec::new(),
        issues: split_issues,
        restrictions: Vec::new(),
    };
    apply_restrictions(src, &mut j);
    lane_issues(&mut j);
    Some(j)
}

fn apply_restrictions(src: &dyn JunctionSource, j: &mut Junction) {
    let mut ways: Vec<WayId> = j.arms.iter().map(|a| a.way).collect();
    ways.dedup();
    let mut relations = src.relations_touching(j.node, &ways);
    relations.sort_by_key(|r| r.id);
    relations.dedup_by_key(|r| r.id);

    for r in relations.iter().filter(|r| is_restriction(&r.tags)) {
        let Some((value, mode)) = restriction_value(&r.tags) else {
            j.issues.push(Issue {
                check: "restriction_broken",
                arm: None,
                relation: Some(r.id),
                message: format!("r{} has no restriction value", r.id.0),
            });
            continue;
        };
        let shown = match (mode.as_deref(), r.tags.get("except")) {
            (Some(m), _) => format!("{value} ({m})"),
            (None, Some(e)) => format!("{value} (except {e})"),
            (None, None) => value.clone(),
        };
        let role = |name: &str| {
            r.members
                .iter()
                .filter(move |m| m.role == name)
                .collect::<Vec<_>>()
        };
        let (from, via, to) = (role("from"), role("via"), role("to"));
        let via_here = via
            .iter()
            .any(|m| m.member_type == MemberType::Node && m.reference == j.node.0);
        if !via_here {
            // Through a way, or via another node: listed if it uses an arm.
            if via.iter().any(|m| m.member_type == MemberType::Way) {
                j.listed.push(ListedRestriction {
                    relation: r.id,
                    value: shown,
                    note: "via a way".into(),
                });
            }
            continue;
        }
        let arms_of = |members: &[&crate::osm::OsmMember]| -> Vec<usize> {
            members
                .iter()
                .filter(|m| m.member_type == MemberType::Way)
                .flat_map(|m| {
                    j.arms
                        .iter()
                        .enumerate()
                        .filter(move |(_, a)| a.way.0 == m.reference)
                        .map(|(i, _)| i)
                })
                .collect()
        };
        let (from_arms, to_arms) = (arms_of(&from), arms_of(&to));
        if from_arms.is_empty() || to_arms.is_empty() {
            j.issues.push(Issue {
                check: "restriction_broken",
                arm: None,
                relation: Some(r.id),
                message: format!(
                    "r{} {}: its {} way doesn't reach this node",
                    r.id.0,
                    value,
                    if from_arms.is_empty() { "from" } else { "to" }
                ),
            });
            continue;
        }
        // The (from, to) movements the relation names. A from/to way that
        // continues through the node is two arms; the turn the value names
        // (`no_left_turn`) picks the side that makes geometric sense.
        let all_pairs: Vec<(usize, usize, TurnKind)> = j
            .movements
            .iter()
            .filter(|m| from_arms.contains(&m.from) && to_arms.contains(&m.to))
            .map(|m| (m.from, m.to, m.turn))
            .collect();
        let expected = value_turns(&value);
        let matching: Vec<(usize, usize, TurnKind)> = all_pairs
            .iter()
            .copied()
            .filter(|(_, _, t)| expected.is_none_or(|e| e.contains(t)))
            .collect();
        let pairs = if matching.is_empty() {
            if let Some((_, _, t)) = all_pairs.first() {
                j.issues.push(Issue {
                    check: "restriction_kind",
                    arm: None,
                    relation: Some(r.id),
                    message: format!(
                        "r{} is {} but the geometry makes it a {} turn",
                        r.id.0,
                        value,
                        t.id().replace('_', " ")
                    ),
                });
            }
            all_pairs
        } else {
            matching
        };
        if pairs.len() > 1 {
            j.issues.push(Issue {
                check: "restriction_ambiguous",
                arm: None,
                relation: Some(r.id),
                message: format!(
                    "r{} {}: its from/to way continues through this node, so it's unclear which side is meant",
                    r.id.0, value
                ),
            });
        }
        let key = match &mode {
            Some(m) => format!("restriction:{m}"),
            None => "restriction".to_string(),
        };
        j.restrictions.push((r.id, key, value.clone()));
        let only = value.starts_with("only_");
        for m in j.movements.iter_mut() {
            if !pairs.iter().any(|&(f, _, _)| f == m.from) {
                continue;
            }
            let named = pairs.iter().any(|&(f, t, _)| f == m.from && t == m.to);
            if only && named {
                m.only = Some(r.id);
            }
            if !only && named {
                m.state = MovementState::Banned {
                    relation: r.id,
                    value: shown.clone(),
                };
            } else if only && !named && m.state == MovementState::Allowed {
                m.state = MovementState::OnlyElsewhere {
                    relation: r.id,
                    value: shown.clone(),
                };
            }
        }
    }
}

fn lane_issues(j: &mut Junction) {
    let mut issues = Vec::new();
    for (i, arm) in j.arms.iter().enumerate() {
        if let Some(g) = &arm.inbound {
            if let (Some(arrows), Some(key)) = (&g.arrows, &g.arrows_key) {
                if g.source != CountSource::Unknown && arrows.len() as u32 != g.count {
                    issues.push(Issue {
                        check: "turn_lanes_count",
                        arm: Some(i),
                        relation: None,
                        message: format!(
                            "{}: {key} lists {} lanes but there are {}",
                            arm.label,
                            arrows.len(),
                            g.count
                        ),
                    });
                }
                let exits: Vec<&Movement> = j.movements_from(i).collect();
                let mut seen = Vec::new();
                for &arrow in arrows.iter().flatten() {
                    if seen.contains(&arrow) {
                        continue;
                    }
                    seen.push(arrow);
                    let served: Vec<&&Movement> = exits
                        .iter()
                        .filter(|m| arrow_serves(arrow, m.turn))
                        .collect();
                    let name = format!("{arrow:?}").to_lowercase();
                    if served.is_empty() {
                        issues.push(Issue {
                            check: "arrow_no_exit",
                            arm: Some(i),
                            relation: None,
                            message: format!(
                                "{}: a lane turns {name} but no road goes that way",
                                arm.label
                            ),
                        });
                    } else if served.iter().all(|m| m.state != MovementState::Allowed) {
                        issues.push(Issue {
                            check: "arrow_banned",
                            arm: Some(i),
                            relation: None,
                            message: format!(
                                "{}: a lane turns {name} but restrictions ban every turn that way",
                                arm.label
                            ),
                        });
                    }
                }
            }
        }
    }
    j.issues.extend(issues);
}

/// One end of a restriction to create: an arm's way, and the way's next node
/// along the arm (to find the arm's part if the way is split first).
#[derive(Debug, Clone, PartialEq)]
pub struct ArmEnd {
    pub way: WayId,
    pub next_node: NodeId,
    /// The way runs on past the junction, so it must be split there for the
    /// restriction to name only this side.
    pub needs_split: bool,
}

/// What clicking a movement does to the restrictions.
#[derive(Debug, Clone, PartialEq)]
pub enum RestrictionEdit {
    /// Ban an allowed movement: a new `no_*` restriction.
    Create {
        from: ArmEnd,
        via: NodeId,
        to: ArmEnd,
        value: String,
    },
    /// Turn the movement's `no_*` restriction into an `only_*` one.
    Retag {
        relation: RelationId,
        key: String,
        value: String,
    },
    /// Allow the movement again by removing the restriction.
    Delete { relation: RelationId },
}

/// The restriction value suffix for a movement of this turn kind.
pub fn turn_value(turn: TurnKind) -> &'static str {
    match turn {
        TurnKind::Straight | TurnKind::SlightLeft | TurnKind::SlightRight => "straight_on",
        TurnKind::Left | TurnKind::SharpLeft => "left_turn",
        TurnKind::Right | TurnKind::SharpRight => "right_turn",
        TurnKind::UTurn => "u_turn",
    }
}

/// The edit that moves the movement `from` → `to` one step along allowed →
/// `no_*` → `only_*` → allowed. A movement ruled out by an `only_*`
/// restriction from its approach is allowed again by removing that
/// restriction. `None` if there is no such movement.
pub fn toggle(j: &Junction, from: usize, to: usize) -> Option<RestrictionEdit> {
    let m = j.movements.iter().find(|m| m.from == from && m.to == to)?;
    let end = |arm: &Arm| ArmEnd {
        way: arm.way,
        next_node: arm.next_node,
        needs_split: arm.way_continues,
    };
    Some(match &m.state {
        MovementState::Allowed => match m.only {
            Some(relation) => RestrictionEdit::Delete { relation },
            None => RestrictionEdit::Create {
                from: end(&j.arms[from]),
                via: j.node,
                to: end(&j.arms[to]),
                value: format!("no_{}", turn_value(m.turn)),
            },
        },
        MovementState::Banned { relation, .. } => {
            let (_, key, value) = j.restrictions.iter().find(|(id, _, _)| id == relation)?;
            match value.strip_prefix("no_") {
                Some(rest) => RestrictionEdit::Retag {
                    relation: *relation,
                    key: key.clone(),
                    value: format!("only_{rest}"),
                },
                None => RestrictionEdit::Delete {
                    relation: *relation,
                },
            }
        }
        MovementState::OnlyElsewhere { relation, .. } => RestrictionEdit::Delete {
            relation: *relation,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::{OsmMember, OsmNode, OsmWay};

    /// `(way id, node ids, tags)`.
    type WaySpec<'a> = (i64, &'a [i64], &'a [(&'a str, &'a str)]);

    fn data(nodes: &[(i64, f64, f64)], ways: &[WaySpec]) -> OsmData {
        let mut d = OsmData::default();
        for &(id, lat, lon) in nodes {
            d.nodes.insert(
                NodeId(id),
                OsmNode {
                    id: NodeId(id),
                    lat,
                    lon,
                    version: 1,
                    tags: Tags::default(),
                },
            );
        }
        for &(id, ns, tags) in ways {
            d.ways.insert(
                WayId(id),
                OsmWay {
                    id: WayId(id),
                    nodes: ns.iter().map(|&n| NodeId(n)).collect(),
                    version: 1,
                    tags: Tags::from(tags),
                },
            );
        }
        d
    }

    fn restriction(id: i64, value: &str, from: i64, via: i64, to: i64) -> OsmRelation {
        let m = |t, r, role: &str| OsmMember {
            member_type: t,
            reference: r,
            role: role.into(),
        };
        OsmRelation {
            id: RelationId(id),
            members: vec![
                m(MemberType::Way, from, "from"),
                m(MemberType::Node, via, "via"),
                m(MemberType::Way, to, "to"),
            ],
            version: 1,
            tags: Tags::from([("type", "restriction"), ("restriction", value)]),
        }
    }

    const D: f64 = 0.001; // ~110 m

    /// A plus-shaped crossing at node 1: way 10 runs south→north through it,
    /// way 20 west→east.
    fn crossing(ns: &[(&str, &str)], ew: &[(&str, &str)]) -> OsmData {
        data(
            &[
                (1, 0.0, 0.0),
                (2, -D, 0.0),
                (3, D, 0.0),
                (4, 0.0, -D),
                (5, 0.0, D),
            ],
            &[(10, &[2, 1, 3], ns), (20, &[4, 1, 5], ew)],
        )
    }

    fn arm(j: &Junction, way: i64, outward: bool) -> usize {
        j.arms
            .iter()
            .position(|a| a.way.0 == way && a.outward == outward)
            .unwrap()
    }

    fn movement(j: &Junction, from: usize, to: usize) -> &Movement {
        j.movements
            .iter()
            .find(|m| m.from == from && m.to == to)
            .unwrap()
    }

    #[test]
    fn a_through_way_is_two_arms_with_bearings() {
        let d = crossing(&[("highway", "primary")], &[("highway", "residential")]);
        let j = build(&d, NodeId(1)).unwrap();
        let b: Vec<i32> = j.arms.iter().map(|a| a.bearing.round() as i32).collect();
        assert_eq!(b, [0, 90, 180, 270]);
        assert_eq!(j.arms[0].way, WayId(10));
        assert!(j.arms[0].outward);
    }

    #[test]
    fn turns_are_classified_from_the_approach() {
        let d = crossing(&[("highway", "primary")], &[("highway", "residential")]);
        let j = build(&d, NodeId(1)).unwrap();
        let south = arm(&j, 10, false); // arriving from the south, heading north
        let east = arm(&j, 20, true);
        let west = arm(&j, 20, false);
        let north = arm(&j, 10, true);
        assert_eq!(movement(&j, south, north).turn, TurnKind::Straight);
        assert_eq!(movement(&j, south, east).turn, TurnKind::Right);
        assert_eq!(movement(&j, south, west).turn, TurnKind::Left);
        assert_eq!(movement(&j, south, south).turn, TurnKind::UTurn);
    }

    #[test]
    fn lanes_follow_the_direction_of_travel() {
        let d = crossing(
            &[
                ("highway", "primary"),
                ("lanes", "3"),
                ("lanes:forward", "2"),
                ("lanes:backward", "1"),
                ("turn:lanes:forward", "left|through"),
            ],
            &[("highway", "residential"), ("lanes", "2")],
        );
        let j = build(&d, NodeId(1)).unwrap();
        // From the south, traffic runs with the way (forward).
        let south = &j.arms[arm(&j, 10, false)];
        let inbound = south.inbound.as_ref().unwrap();
        assert_eq!(inbound.count, 2);
        assert_eq!(
            inbound.arrows.as_deref(),
            Some(&[vec![Arrow::Left], vec![Arrow::Through]][..])
        );
        assert_eq!(south.outbound.as_ref().unwrap().count, 1);
        // An even `lanes` splits in half.
        let east = &j.arms[arm(&j, 20, true)];
        assert_eq!(east.inbound.as_ref().unwrap().count, 1);
        assert_eq!(east.inbound.as_ref().unwrap().source, CountSource::Inferred);
        assert!(j.issues.is_empty(), "{:?}", j.issues);
    }

    #[test]
    fn oneways_have_lanes_one_way_and_no_movements_against() {
        let d = crossing(
            &[("highway", "primary"), ("oneway", "yes"), ("lanes", "2")],
            &[("highway", "residential")],
        );
        let j = build(&d, NodeId(1)).unwrap();
        let south = arm(&j, 10, false);
        let north = arm(&j, 10, true);
        assert!(j.arms[south].outbound.is_none());
        assert!(j.arms[north].inbound.is_none());
        assert_eq!(j.arms[south].inbound.as_ref().unwrap().count, 2);
        assert!(j.movements.iter().all(|m| m.to != south && m.from != north));
    }

    #[test]
    fn no_and_only_restrictions_set_movement_states() {
        let mut d = crossing(&[("highway", "primary")], &[("highway", "residential")]);
        d.relations
            .push(restriction(100, "no_left_turn", 10, 1, 20));
        let j = build(&d, NodeId(1)).unwrap();
        let (south, north) = (arm(&j, 10, false), arm(&j, 10, true));
        let (east, west) = (arm(&j, 20, true), arm(&j, 20, false));
        // Both ways continue through the node: the left turns are south→west
        // and north→east, so which one is meant is ambiguous.
        assert!(j.issues.iter().any(|i| i.check == "restriction_ambiguous"));
        assert!(matches!(
            movement(&j, south, west).state,
            MovementState::Banned { .. }
        ));
        assert_eq!(movement(&j, south, north).state, MovementState::Allowed);
        assert_eq!(movement(&j, south, east).state, MovementState::Allowed);

        // A T where the from way ends at the node is not ambiguous.
        let mut d = data(
            &[(1, 0.0, 0.0), (2, -D, 0.0), (4, 0.0, -D), (5, 0.0, D)],
            &[
                (10, &[2, 1], &[("highway", "primary")]),
                (20, &[4, 1, 5], &[("highway", "residential")]),
            ],
        );
        d.relations
            .push(restriction(101, "only_right_turn", 10, 1, 20));
        let j = build(&d, NodeId(1)).unwrap();
        let south = arm(&j, 10, false);
        let east = arm(&j, 20, true);
        let west = arm(&j, 20, false);
        // `to` is way 20 on both sides; only the east side is a right turn.
        assert!(j.issues.is_empty(), "{:?}", j.issues);
        assert_eq!(movement(&j, south, east).state, MovementState::Allowed);
        for to in [west, south] {
            assert!(matches!(
                movement(&j, south, to).state,
                MovementState::OnlyElsewhere { .. }
            ));
        }

        // A value that matches no movement is reported.
        let mut d = d.clone();
        d.relations[0] = restriction(101, "no_straight_on", 10, 1, 20);
        let j = build(&d, NodeId(1)).unwrap();
        assert!(j.issues.iter().any(|i| i.check == "restriction_kind"));
    }

    #[test]
    fn turn_lanes_problems_are_reported() {
        let d = data(
            &[(1, 0.0, 0.0), (2, -D, 0.0), (3, D, 0.0), (5, 0.0, D)],
            &[
                (
                    10,
                    &[2, 1, 3],
                    &[
                        ("highway", "primary"),
                        ("lanes:forward", "3"),
                        ("lanes:backward", "1"),
                        ("turn:lanes:forward", "left|through"),
                    ],
                ),
                (20, &[1, 5], &[("highway", "residential")]),
            ],
        );
        let j = build(&d, NodeId(1)).unwrap();
        let checks: Vec<&str> = j.issues.iter().map(|i| i.check).collect();
        // Two arrows for three lanes, and no road to the left (only east).
        assert!(checks.contains(&"turn_lanes_count"), "{checks:?}");
        assert!(checks.contains(&"arrow_no_exit"), "{checks:?}");
    }

    /// A T: way 10 ends at node 1 from the south, way 20 runs west→east
    /// through it.
    fn tee() -> OsmData {
        data(
            &[(1, 0.0, 0.0), (2, -D, 0.0), (4, 0.0, -D), (5, 0.0, D)],
            &[
                (10, &[2, 1], &[("highway", "primary")]),
                (20, &[4, 1, 5], &[("highway", "residential")]),
            ],
        )
    }

    #[test]
    fn toggling_cycles_allowed_no_only_allowed() {
        let mut d = tee();
        let j = build(&d, NodeId(1)).unwrap();
        let (south, west) = (arm(&j, 10, false), arm(&j, 20, false));
        // Allowed: a new no_left_turn; the way through must be split.
        let Some(RestrictionEdit::Create {
            from,
            via,
            to,
            value,
        }) = toggle(&j, south, west)
        else {
            panic!("expected a create");
        };
        assert_eq!(value, "no_left_turn");
        assert_eq!(via, NodeId(1));
        assert_eq!(
            (from.way, from.next_node, from.needs_split),
            (WayId(10), NodeId(2), false)
        );
        assert_eq!(
            (to.way, to.next_node, to.needs_split),
            (WayId(20), NodeId(4), true)
        );

        // Banned: becomes only_left_turn.
        d.relations
            .push(restriction(100, "no_left_turn", 10, 1, 20));
        let j = build(&d, NodeId(1)).unwrap();
        assert_eq!(
            toggle(&j, south, west),
            Some(RestrictionEdit::Retag {
                relation: RelationId(100),
                key: "restriction".into(),
                value: "only_left_turn".into(),
            })
        );

        // Only: the movement it names, and every one it rules out, delete it.
        d.relations[0] = restriction(100, "only_left_turn", 10, 1, 20);
        let j = build(&d, NodeId(1)).unwrap();
        let east = arm(&j, 20, true);
        for to in [west, east] {
            assert_eq!(
                toggle(&j, south, to),
                Some(RestrictionEdit::Delete {
                    relation: RelationId(100)
                })
            );
        }
    }

    #[test]
    fn a_plain_vertex_or_a_split_road_is_no_junction() {
        let road = &[("highway", "primary")][..];
        // The end of a road.
        let d = data(&[(1, 0.0, 0.0), (2, -D, 0.0)], &[(10, &[2, 1], road)]);
        assert!(build(&d, NodeId(1)).is_none());
        // A vertex in the middle of one road.
        let d = data(
            &[(1, 0.0, 0.0), (2, -D, 0.0), (3, D, 0.0)],
            &[(10, &[2, 1, 3], road)],
        );
        assert!(build(&d, NodeId(1)).is_none());
        // One road split into two ways there.
        let d = data(
            &[(1, 0.0, 0.0), (2, -D, 0.0), (3, D, 0.0)],
            &[(10, &[2, 1], road), (11, &[1, 3], road)],
        );
        assert!(build(&d, NodeId(1)).is_none());
        // A side road joining it makes a junction.
        let d = data(
            &[(1, 0.0, 0.0), (2, -D, 0.0), (3, D, 0.0), (4, 0.0, D)],
            &[(10, &[2, 1, 3], road), (12, &[1, 4], road)],
        );
        assert!(build(&d, NodeId(1)).is_some());
    }
}
