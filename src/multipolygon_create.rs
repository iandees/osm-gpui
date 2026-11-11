//! Planning "Create Multipolygon" (JOSM `CreateMultipolygonAction`): from a
//! set of loose ways, find the rings they make and decide which ring is an
//! `outer` and which an `inner`.
//!
//! Pure: ways come in as node-id lists plus a planar coordinate lookup, and a
//! [`Plan`] (or the reason there is none) comes out. Building the relation and
//! its undo step is the caller's business.
//!
//! The rules, following JOSM:
//!
//! - Closed ways are rings. Open ways are chained end to end (either
//!   direction) into rings; a node where three or more open ways end is
//!   ambiguous and refused, as is a chain that does not close.
//! - A ring that touches itself at a node other than its closing one, or has
//!   no area, is refused.
//! - Two rings may touch at shared nodes but must not cross, overlap along an
//!   edge, or have a node lying on the other's edge without sharing a node
//!   there; such a set would be an invalid multipolygon.
//! - The role comes from nesting depth: a ring inside an even number of other
//!   rings (0, 2, ...) is `outer`, inside an odd number `inner`.

use std::collections::{HashMap, HashSet};

use crate::multipolygon::{point_in_ring, ring_area};

/// Role of a ring in the new relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Outer,
    Inner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Outer => "outer",
            Role::Inner => "inner",
        }
    }
}

/// One closed ring, made of one or more of the selected ways.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanRing {
    /// The ways of the ring, in chain order.
    pub ways: Vec<i64>,
    /// The ring's node ids, closing node repeated at the end.
    pub nodes: Vec<i64>,
    pub role: Role,
}

/// What Create Multipolygon should make: outer rings first, then inner ones,
/// each group in the order the selection first named them.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub rings: Vec<PlanRing>,
}

/// Why no multipolygon can be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// No ways to work with.
    Nothing,
    /// A way with fewer than two nodes (or a closed one with fewer than four).
    DegenerateWay(i64),
    /// A way names a node without a known position.
    MissingNode(i64),
    /// Three or more open ways end at this node.
    Branching(i64),
    /// An open way chain that does not close; carries one of its ways.
    Unclosed(i64),
    /// A ring passes through this node twice.
    SelfTouching(i64),
    /// A ring with no area.
    ZeroArea,
    /// Two edges (of the ways with these ids) cross or overlap.
    Crossing(i64, i64),
}

type Pt = (f64, f64);

/// Plan a multipolygon from `ways` (id and node list, each way once).
pub fn plan(ways: &[(i64, Vec<i64>)], coord: &dyn Fn(i64) -> Option<Pt>) -> Result<Plan, Problem> {
    if ways.is_empty() {
        return Err(Problem::Nothing);
    }
    let chains = chain(ways)?;
    let mut rings: Vec<Ring> = Vec::with_capacity(chains.len());
    for (ways, nodes, seg_ways) in chains {
        let body = &nodes[..nodes.len() - 1];
        let mut seen = HashSet::with_capacity(body.len());
        if let Some(&dup) = body.iter().find(|&&n| !seen.insert(n)) {
            return Err(Problem::SelfTouching(dup));
        }
        let pts = body
            .iter()
            .map(|&n| coord(n).ok_or(Problem::MissingNode(n)))
            .collect::<Result<Vec<_>, _>>()?;
        let area = ring_area(&pts);
        if area <= 0.0 || !area.is_finite() {
            return Err(Problem::ZeroArea);
        }
        let mut bbox = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for &(x, y) in &pts {
            bbox = [
                bbox[0].min(x),
                bbox[1].min(y),
                bbox[2].max(x),
                bbox[3].max(y),
            ];
        }
        rings.push(Ring {
            ways,
            node_set: body.iter().copied().collect(),
            nodes,
            seg_ways,
            pts,
            area,
            bbox,
        });
    }

    check_crossings(&rings)?;

    let depth: Vec<usize> = (0..rings.len())
        .map(|i| {
            (0..rings.len())
                .filter(|&j| {
                    j != i && rings[j].area > rings[i].area && contains(&rings[j], &rings[i])
                })
                .count()
        })
        .collect();
    let mut out = Vec::new();
    for role in [Role::Outer, Role::Inner] {
        for (i, ring) in rings.iter().enumerate() {
            let this = if depth[i].is_multiple_of(2) {
                Role::Outer
            } else {
                Role::Inner
            };
            if this == role {
                out.push(PlanRing {
                    ways: ring.ways.clone(),
                    nodes: ring.nodes.clone(),
                    role,
                });
            }
        }
    }
    Ok(Plan { rings: out })
}

struct Ring {
    ways: Vec<i64>,
    nodes: Vec<i64>,
    /// The way each edge `nodes[i] -> nodes[i+1]` belongs to.
    seg_ways: Vec<i64>,
    node_set: HashSet<i64>,
    pts: Vec<Pt>,
    area: f64,
    bbox: [f64; 4],
}

/// `(ways, closed node list, way of each edge)` per ring, in the order the
/// selection first names them.
type Chained = (Vec<i64>, Vec<i64>, Vec<i64>);

fn chain(ways: &[(i64, Vec<i64>)]) -> Result<Vec<Chained>, Problem> {
    let mut rings: Vec<(usize, Chained)> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for (i, (id, nodes)) in ways.iter().enumerate() {
        if nodes.len() < 2 {
            return Err(Problem::DegenerateWay(*id));
        }
        if nodes.first() == nodes.last() {
            if nodes.len() < 4 {
                return Err(Problem::DegenerateWay(*id));
            }
            rings.push((i, (vec![*id], nodes.clone(), vec![*id; nodes.len() - 1])));
        } else {
            open.push(i);
        }
    }
    let mut by_end: HashMap<i64, Vec<usize>> = HashMap::new();
    for &i in &open {
        let nodes = &ways[i].1;
        by_end.entry(nodes[0]).or_default().push(i);
        by_end.entry(nodes[nodes.len() - 1]).or_default().push(i);
    }
    if let Some(node) = by_end
        .iter()
        .filter(|(_, v)| v.len() > 2)
        .map(|(&n, _)| n)
        .min()
    {
        return Err(Problem::Branching(node));
    }
    let mut used = vec![false; ways.len()];
    for &start in &open {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut chain_ways = vec![ways[start].0];
        let mut nodes = ways[start].1.clone();
        let mut seg_ways = vec![ways[start].0; nodes.len() - 1];
        while nodes.first() != nodes.last() {
            let end = nodes[nodes.len() - 1];
            let next = by_end
                .get(&end)
                .and_then(|v| v.iter().copied().find(|&j| !used[j]));
            let Some(j) = next else {
                return Err(Problem::Unclosed(ways[start].0));
            };
            used[j] = true;
            let (id, w) = (&ways[j].0, &ways[j].1);
            chain_ways.push(*id);
            if w[0] == end {
                nodes.extend_from_slice(&w[1..]);
            } else {
                nodes.extend(w.iter().rev().skip(1));
            }
            seg_ways.extend(std::iter::repeat_n(*id, w.len() - 1));
        }
        rings.push((start, (chain_ways, nodes, seg_ways)));
    }
    rings.sort_by_key(|(first, _)| *first);
    Ok(rings.into_iter().map(|(_, r)| r).collect())
}

/// Whether `outer` contains `inner`: boxes first, then a node of `inner` that
/// `outer` does not use (a touching ring shares nodes, which makes the test
/// on those ambiguous).
fn contains(outer: &Ring, inner: &Ring) -> bool {
    let (a, b) = (&outer.bbox, &inner.bbox);
    if b[0] < a[0] || b[1] < a[1] || b[2] > a[2] || b[3] > a[3] {
        return false;
    }
    let rep = inner
        .nodes
        .iter()
        .zip(&inner.pts)
        .find(|(id, _)| !outer.node_set.contains(id))
        .map(|(_, &p)| p)
        .unwrap_or_else(|| {
            let (p, q) = (inner.pts[0], inner.pts[1]);
            ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0)
        });
    point_in_ring(rep.0, rep.1, &outer.pts, |&p| p)
}

fn check_crossings(rings: &[Ring]) -> Result<(), Problem> {
    for i in 0..rings.len() {
        for j in i..rings.len() {
            let (a, b) = (&rings[i], &rings[j]);
            if i != j
                && (a.bbox[2] < b.bbox[0]
                    || b.bbox[2] < a.bbox[0]
                    || a.bbox[3] < b.bbox[1]
                    || b.bbox[3] < a.bbox[1])
            {
                continue;
            }
            for s in 0..a.pts.len() {
                let s_nodes = (a.nodes[s], a.nodes[s + 1]);
                let (p, q) = (a.pts[s], a.pts[(s + 1) % a.pts.len()]);
                let (lo, hi) = (p.0.min(q.0), p.0.max(q.0));
                // Within one ring only later edges, so each pair is seen once.
                let first = if i == j { s + 1 } else { 0 };
                for t in first..b.pts.len() {
                    let t_nodes = (b.nodes[t], b.nodes[t + 1]);
                    let (r, u) = (b.pts[t], b.pts[(t + 1) % b.pts.len()]);
                    if r.0.max(u.0) < lo || r.0.min(u.0) > hi {
                        continue;
                    }
                    let shared = [t_nodes.0, t_nodes.1]
                        .iter()
                        .filter(|n| **n == s_nodes.0 || **n == s_nodes.1)
                        .count();
                    let bad = match shared {
                        // Both ends shared: the same edge twice.
                        2 => true,
                        // Neighbouring edges (or rings meeting at a node):
                        // fine unless one runs back along the other.
                        1 => runs_along(s_nodes, (p, q), t_nodes, (r, u)),
                        _ => segments_meet(p, q, r, u),
                    };
                    if bad {
                        return Err(Problem::Crossing(a.seg_ways[s], b.seg_ways[t]));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Two edges that share exactly one node and leave it in the same direction.
fn runs_along(s: (i64, i64), sp: (Pt, Pt), t: (i64, i64), tp: (Pt, Pt)) -> bool {
    let (c, x) = if t.0 == s.0 || t.1 == s.0 {
        (sp.0, sp.1)
    } else {
        (sp.1, sp.0)
    };
    let y = if t.0 == s.0 || t.0 == s.1 { tp.1 } else { tp.0 };
    orient(c, x, y) == 0.0 && (x.0 - c.0) * (y.0 - c.0) + (x.1 - c.1) * (y.1 - c.1) > 0.0
}

fn orient(a: Pt, b: Pt, c: Pt) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

fn on_segment(a: Pt, b: Pt, c: Pt) -> bool {
    c.0 >= a.0.min(b.0) && c.0 <= a.0.max(b.0) && c.1 >= a.1.min(b.1) && c.1 <= a.1.max(b.1)
}

/// Whether segments `pq` and `ru` have a point in common (touching counts).
fn segments_meet(p: Pt, q: Pt, r: Pt, u: Pt) -> bool {
    let (o1, o2, o3, o4) = (
        orient(p, q, r),
        orient(p, q, u),
        orient(r, u, p),
        orient(r, u, q),
    );
    if o1 * o2 < 0.0 && o3 * o4 < 0.0 {
        return true;
    }
    (o1 == 0.0 && on_segment(p, q, r))
        || (o2 == 0.0 && on_segment(p, q, u))
        || (o3 == 0.0 && on_segment(r, u, p))
        || (o4 == 0.0 && on_segment(r, u, q))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(id: i64) -> Option<Pt> {
        Some(((id / 100) as f64, (id % 100) as f64))
    }
    fn n(x: i64, y: i64) -> i64 {
        x * 100 + y
    }
    fn square(x0: i64, y0: i64, x1: i64, y1: i64) -> Vec<i64> {
        vec![n(x0, y0), n(x1, y0), n(x1, y1), n(x0, y1), n(x0, y0)]
    }
    fn run(ways: &[(i64, Vec<i64>)]) -> Result<Plan, Problem> {
        plan(ways, &grid)
    }
    fn roles(p: &Plan) -> Vec<(Vec<i64>, Role)> {
        p.rings.iter().map(|r| (r.ways.clone(), r.role)).collect()
    }

    #[test]
    fn nesting_depth_alternates_outer_and_inner() {
        let p = run(&[
            (1, square(2, 2, 3, 3)),
            (2, square(0, 0, 10, 10)),
            (3, square(1, 1, 5, 5)),
        ])
        .unwrap();
        assert_eq!(
            roles(&p),
            vec![
                (vec![1], Role::Outer),
                (vec![2], Role::Outer),
                (vec![3], Role::Inner)
            ]
        );
    }

    #[test]
    fn open_ways_chain_in_either_direction() {
        let a = vec![n(0, 0), n(4, 0), n(4, 4)];
        let b = vec![n(0, 0), n(0, 4), n(4, 4)];
        let p = run(&[(1, a), (2, b)]).unwrap();
        assert_eq!(p.rings.len(), 1);
        assert_eq!(p.rings[0].ways, vec![1, 2]);
        assert_eq!(p.rings[0].nodes.first(), p.rings[0].nodes.last());
    }

    #[test]
    fn refusals() {
        assert_eq!(run(&[]), Err(Problem::Nothing));
        assert_eq!(
            run(&[(1, vec![n(0, 0), n(4, 0), n(4, 4)])]),
            Err(Problem::Unclosed(1))
        );
        // Bow tie crossing.
        let bow = vec![n(0, 0), n(6, 4), n(6, 0), n(0, 3), n(0, 0)];
        assert_eq!(run(&[(1, bow)]), Err(Problem::Crossing(1, 1)));
        // Two squares overlapping.
        assert_eq!(
            run(&[(1, square(0, 0, 4, 4)), (2, square(2, 2, 6, 6))]),
            Err(Problem::Crossing(1, 2))
        );
    }

    #[test]
    fn rings_sharing_an_edge_are_refused_but_a_shared_corner_is_fine() {
        assert!(matches!(
            run(&[(1, square(0, 0, 4, 4)), (2, square(4, 0, 8, 4))]),
            Err(Problem::Crossing(..))
        ));
        assert!(run(&[(1, square(0, 0, 4, 4)), (2, square(4, 4, 8, 8))]).is_ok());
    }
}
