//! Assembling `type=multipolygon` / `type=boundary` relations into polygons.
//!
//! A multipolygon relation lists member ways with the roles `outer` and
//! `inner`. Several ways may make up one ring, in any order and direction;
//! this module joins them end to end into closed rings, sorts the rings into
//! outer and inner ones, and gives each inner ring (a hole) to the outer ring
//! that contains it. Everything here is pure: ways come in as node-id lists
//! and a coordinate lookup, polygons go out as node-id rings, so it is tested
//! without any layer or window.
//!
//! The policy for bad data is "don't guess": any relation that can't be
//! assembled unambiguously into valid rings is reported as a [`Problem`] and
//! gets no polygons at all (no fill), rather than a half-right shape. The
//! checks, in order:
//!
//! - a member way that isn't loaded, appears twice, has fewer than two nodes,
//!   or has a node without a known position;
//! - an open way chain that doesn't close, or a node where three or more open
//!   ways end (the join would be a guess);
//! - a ring that touches itself at a node other than its closing one, or has
//!   no area;
//! - no outer ring at all, or an inner ring that no outer ring contains.
//!
//! Members that aren't ways, and ways whose role is neither `outer`, `inner`
//! nor empty (`label`, `admin_centre`, ...) are ignored. An empty role counts
//! as `outer`, except that when *every* way of a ring has an empty role the
//! ring is classified by containment instead: a ring nested inside an odd
//! number of other such rings is a hole.
//!
//! Not checked: whether two rings cross each other. Such data renders as
//! drawn, with the holes still subtracted by the even-odd-like triangulation.

use std::collections::{HashMap, HashSet};

/// Whether `tags` describe a relation that is drawn as an area: a
/// multipolygon or a boundary.
pub fn is_area_relation(tags: &HashMap<String, String>) -> bool {
    matches!(
        tags.get("type").map(String::as_str),
        Some("multipolygon" | "boundary")
    )
}

/// One way member of a relation, as the assembler needs it.
#[derive(Debug, Clone, Copy)]
pub struct WayMember<'a> {
    pub way_id: i64,
    pub role: &'a str,
    /// The way's node ids in order, or `None` when the way isn't loaded.
    pub nodes: Option<&'a [i64]>,
}

/// Why a relation couldn't be assembled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// A member way is not in the data (not downloaded).
    MissingWay(i64),
    /// The same way is listed twice.
    DuplicateMember(i64),
    /// A member way has fewer than two nodes (or a closed one fewer than
    /// four).
    DegenerateWay(i64),
    /// A node has no known position (not downloaded or invalid).
    MissingNode(i64),
    /// Three or more open ways end at this node, so joining them would be a
    /// guess.
    Branching(i64),
    /// Ways chain up but never close into a ring.
    Unclosed,
    /// A ring visits the same node twice (other than closing).
    SelfTouching(i64),
    /// A ring encloses no area.
    ZeroArea,
    /// No outer ring.
    NoOuter,
    /// An inner ring that no outer ring contains.
    UnmatchedInner,
}

/// One outer ring and the holes inside it. Rings are closed node-id lists
/// (first id equals last id).
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    pub outer: Vec<i64>,
    pub inners: Vec<Vec<i64>>,
    /// Outer area minus the holes', in the units of the coordinate lookup
    /// squared.
    pub area: f64,
}

/// A successfully assembled relation.
#[derive(Debug, Clone, PartialEq)]
pub struct Multipolygon {
    pub polygons: Vec<Polygon>,
}

impl Multipolygon {
    /// Sum of the polygons' areas (holes excluded).
    pub fn area(&self) -> f64 {
        self.polygons.iter().map(|p| p.area).sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Outer,
    /// Empty role: outer unless containment says otherwise.
    Unspecified,
    Inner,
}

struct Ring {
    nodes: Vec<i64>,
    /// Positions without the closing duplicate.
    pts: Vec<(f64, f64)>,
    area: f64,
    /// `[min_x, min_y, max_x, max_y]`.
    bbox: [f64; 4],
    class: Class,
}

/// Signed-area magnitude of a ring given as points (closing duplicate
/// optional).
pub fn ring_area(pts: &[(f64, f64)]) -> f64 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[(i + 1) % n];
        sum += x0 * y1 - x1 * y0;
    }
    (sum / 2.0).abs()
}

/// Ray-casting point-in-ring test over any vertex type; `xy` reads a
/// vertex's position. The ring may or may not repeat its first vertex at the
/// end.
pub fn point_in_ring<T>(x: f64, y: f64, ring: &[T], xy: impl Fn(&T) -> (f64, f64)) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = xy(&ring[i]);
        let (xj, yj) = xy(&ring[j]);
        if (yi > y) != (yj > y) {
            let x_cross = xi + (y - yi) * (xj - xi) / (yj - yi);
            if x < x_cross {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Join `ways` (class plus node list) end to end into closed rings.
fn join_ways(ways: &[(i64, Class, &[i64])]) -> Result<Vec<(Class, Vec<i64>)>, Problem> {
    let mut rings: Vec<(Class, Vec<i64>)> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for (i, &(way_id, class, nodes)) in ways.iter().enumerate() {
        if nodes.len() < 2 {
            return Err(Problem::DegenerateWay(way_id));
        }
        if nodes.first() == nodes.last() {
            if nodes.len() < 4 {
                return Err(Problem::DegenerateWay(way_id));
            }
            rings.push((class, nodes.to_vec()));
        } else {
            open.push(i);
        }
    }

    // Where open ways end. A node shared by more than two ends is a branch.
    let mut by_end: HashMap<i64, Vec<usize>> = HashMap::new();
    for &i in &open {
        let nodes = ways[i].2;
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
        let mut nodes = ways[start].2.to_vec();
        let mut class = ways[start].1;
        while nodes.first() != nodes.last() {
            let end = nodes[nodes.len() - 1];
            let next = by_end
                .get(&end)
                .and_then(|v| v.iter().copied().find(|&j| !used[j]));
            let Some(j) = next else {
                return Err(Problem::Unclosed);
            };
            used[j] = true;
            if ways[j].1 == Class::Outer {
                class = Class::Outer;
            }
            let w = ways[j].2;
            if w[0] == end {
                nodes.extend_from_slice(&w[1..]);
            } else {
                nodes.extend(w.iter().rev().skip(1));
            }
        }
        rings.push((class, nodes));
    }
    Ok(rings)
}

/// Whether `outer` contains `inner`: boxes first, then one representative
/// point of `inner` that isn't a node of `outer` (an inner ring commonly
/// touches its outer at a shared node, which makes the test ambiguous).
fn contains(outer: &Ring, outer_nodes: &HashSet<i64>, inner: &Ring) -> bool {
    let (a, b) = (&outer.bbox, &inner.bbox);
    if b[0] < a[0] || b[1] < a[1] || b[2] > a[2] || b[3] > a[3] {
        return false;
    }
    let rep = inner
        .nodes
        .iter()
        .zip(&inner.pts)
        .find(|(id, _)| !outer_nodes.contains(id))
        .map(|(_, &p)| p)
        .unwrap_or_else(|| {
            let (p, q) = (inner.pts[0], inner.pts[1]);
            ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0)
        });
    point_in_ring(rep.0, rep.1, &outer.pts, |&p| p)
}

/// Assemble a multipolygon from its way members. `coord` gives a node's
/// position in any planar coordinate system (areas and containment are
/// computed in it). See the module docs for the rules.
pub fn assemble(
    members: &[WayMember],
    coord: &dyn Fn(i64) -> Option<(f64, f64)>,
) -> Result<Multipolygon, Problem> {
    let mut seen: HashSet<i64> = HashSet::new();
    let mut non_inner: Vec<(i64, Class, &[i64])> = Vec::new();
    let mut inner: Vec<(i64, Class, &[i64])> = Vec::new();
    for m in members {
        let class = match m.role {
            "outer" => Class::Outer,
            "" => Class::Unspecified,
            "inner" => Class::Inner,
            _ => continue,
        };
        if !seen.insert(m.way_id) {
            return Err(Problem::DuplicateMember(m.way_id));
        }
        let Some(nodes) = m.nodes else {
            return Err(Problem::MissingWay(m.way_id));
        };
        let entry = (m.way_id, class, nodes);
        if class == Class::Inner {
            inner.push(entry);
        } else {
            non_inner.push(entry);
        }
    }

    let mut rings: Vec<Ring> = Vec::new();
    for (group, is_inner_group) in [(&non_inner, false), (&inner, true)] {
        for (class, nodes) in join_ways(group)? {
            let class = if is_inner_group { Class::Inner } else { class };
            let body = &nodes[..nodes.len() - 1];
            let mut distinct: HashSet<i64> = HashSet::with_capacity(body.len());
            if let Some(&dup) = body.iter().find(|&&n| !distinct.insert(n)) {
                return Err(Problem::SelfTouching(dup));
            }
            let mut pts = Vec::with_capacity(body.len());
            for &n in body {
                pts.push(coord(n).ok_or(Problem::MissingNode(n))?);
            }
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
                bbox[0] = bbox[0].min(x);
                bbox[1] = bbox[1].min(y);
                bbox[2] = bbox[2].max(x);
                bbox[3] = bbox[3].max(y);
            }
            rings.push(Ring {
                nodes: body.to_vec(),
                pts,
                area,
                bbox,
                class,
            });
        }
    }

    let node_sets: Vec<HashSet<i64>> = if rings.len() > 1 {
        rings
            .iter()
            .map(|r| r.nodes.iter().copied().collect())
            .collect()
    } else {
        Vec::new()
    };

    // Rings with no role anywhere: a ring nested inside an odd number of
    // larger such rings is a hole.
    let nested_odd: Vec<usize> = (0..rings.len())
        .filter(|&i| rings[i].class == Class::Unspecified)
        .filter(|&i| {
            let depth = (0..rings.len())
                .filter(|&j| {
                    j != i
                        && rings[j].class != Class::Inner
                        && rings[j].area > rings[i].area
                        && contains(&rings[j], &node_sets[j], &rings[i])
                })
                .count();
            depth % 2 == 1
        })
        .collect();
    for i in nested_odd {
        rings[i].class = Class::Inner;
    }
    let is_inner = |c: Class| c == Class::Inner;
    let outers: Vec<usize> = (0..rings.len())
        .filter(|&i| !is_inner(rings[i].class))
        .collect();
    if outers.is_empty() {
        return Err(Problem::NoOuter);
    }

    let mut polygons: Vec<Polygon> = outers
        .iter()
        .map(|&i| Polygon {
            outer: close(&rings[i].nodes),
            inners: Vec::new(),
            area: rings[i].area,
        })
        .collect();
    for (k, ring) in rings.iter().enumerate() {
        if !is_inner(ring.class) {
            continue;
        }
        let owner = outers
            .iter()
            .enumerate()
            .filter(|&(_, &o)| o != k && contains(&rings[o], &node_sets[o], ring))
            .min_by(|a, b| rings[*a.1].area.total_cmp(&rings[*b.1].area))
            .map(|(p, _)| p);
        let Some(p) = owner else {
            return Err(Problem::UnmatchedInner);
        };
        polygons[p].inners.push(close(&ring.nodes));
        polygons[p].area -= ring.area;
    }
    Ok(Multipolygon { polygons })
}

fn close(nodes: &[i64]) -> Vec<i64> {
    let mut v = nodes.to_vec();
    v.push(nodes[0]);
    v
}

/// Earcut triangle indices for one polygon: `rings[0]` is the outer ring,
/// the rest are holes, each as points without a closing duplicate. Indices
/// refer to the rings' points concatenated in order. `None` when
/// triangulation fails or yields nothing; never panics.
pub fn triangulate(rings: &[&[(f64, f64)]]) -> Option<Vec<u32>> {
    let total: usize = rings.iter().map(|r| r.len()).sum();
    let mut flat = Vec::with_capacity(total * 2);
    let mut holes = Vec::with_capacity(rings.len().saturating_sub(1));
    let mut offset = 0;
    for (i, ring) in rings.iter().enumerate() {
        if i > 0 {
            holes.push(offset);
        }
        for &(x, y) in ring.iter() {
            flat.push(x);
            flat.push(y);
        }
        offset += ring.len();
    }
    let tris = earcutr::earcut(&flat, &holes, 2).ok()?;
    if tris.is_empty() {
        return None;
    }
    Some(tris.into_iter().map(|i| i as u32).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Node id -> position for tests: ids encode a grid, `id = y * 100 + x`.
    fn grid(id: i64) -> Option<(f64, f64)> {
        if id < 0 {
            return None;
        }
        Some(((id % 100) as f64, (id / 100) as f64))
    }

    fn n(x: i64, y: i64) -> i64 {
        y * 100 + x
    }

    /// Closed square ring from (x0,y0) to (x1,y1), as node ids.
    fn square(x0: i64, y0: i64, x1: i64, y1: i64) -> Vec<i64> {
        vec![n(x0, y0), n(x1, y0), n(x1, y1), n(x0, y1), n(x0, y0)]
    }

    fn member<'a>(way_id: i64, role: &'a str, nodes: &'a [i64]) -> WayMember<'a> {
        WayMember {
            way_id,
            role,
            nodes: Some(nodes),
        }
    }

    fn run(members: &[WayMember]) -> Result<Multipolygon, Problem> {
        assemble(members, &grid)
    }

    #[test]
    fn a_single_closed_way_is_a_ring() {
        let w = square(0, 0, 10, 10);
        let mp = run(&[member(1, "outer", &w)]).unwrap();
        assert_eq!(mp.polygons.len(), 1);
        assert_eq!(mp.polygons[0].outer, w);
        assert!(mp.polygons[0].inners.is_empty());
        assert_eq!(mp.area(), 100.0);
    }

    #[test]
    fn an_inner_ring_becomes_a_hole_of_the_outer_that_contains_it() {
        let outer = square(0, 0, 10, 10);
        let hole = square(3, 3, 6, 6);
        let mp = run(&[member(1, "outer", &outer), member(2, "inner", &hole)]).unwrap();
        assert_eq!(mp.polygons.len(), 1);
        assert_eq!(mp.polygons[0].inners, vec![hole]);
        assert_eq!(mp.area(), 100.0 - 9.0);
    }

    #[test]
    fn two_ways_join_into_one_ring_even_when_reversed() {
        // Left half then right half; the second is stored backwards.
        let a = vec![n(5, 0), n(0, 0), n(0, 10), n(5, 10)];
        let b = vec![n(5, 0), n(10, 0), n(10, 10), n(5, 10)];
        let mp = run(&[member(1, "outer", &a), member(2, "outer", &b)]).unwrap();
        assert_eq!(mp.polygons.len(), 1);
        let ring = &mp.polygons[0].outer;
        assert_eq!(ring.first(), ring.last());
        assert_eq!(ring.len(), 7);
        assert_eq!(mp.area(), 100.0);
    }

    #[test]
    fn three_ways_in_scrambled_order_make_a_ring() {
        let a = vec![n(0, 0), n(10, 0)];
        let b = vec![n(10, 10), n(10, 0)];
        let c = vec![n(10, 10), n(0, 10), n(0, 0)];
        let mp = run(&[
            member(3, "outer", &c),
            member(1, "outer", &a),
            member(2, "outer", &b),
        ])
        .unwrap();
        assert_eq!(mp.polygons.len(), 1);
        assert_eq!(mp.area(), 100.0);
    }

    #[test]
    fn several_outers_each_collect_their_own_holes() {
        let o1 = square(0, 0, 10, 10);
        let o2 = square(20, 0, 30, 10);
        let h2 = square(22, 2, 25, 5);
        let mp = run(&[
            member(1, "outer", &o1),
            member(2, "outer", &o2),
            member(3, "inner", &h2),
        ])
        .unwrap();
        assert_eq!(mp.polygons.len(), 2);
        let with_hole = mp.polygons.iter().find(|p| p.outer == o2).unwrap();
        assert_eq!(with_hole.inners, vec![h2]);
        assert!(mp
            .polygons
            .iter()
            .find(|p| p.outer == o1)
            .unwrap()
            .inners
            .is_empty());
    }

    #[test]
    fn an_island_inside_a_hole_is_its_own_outer() {
        let lake = square(0, 0, 30, 30);
        let bay = square(5, 5, 25, 25);
        let island = square(10, 10, 15, 15);
        let mp = run(&[
            member(1, "outer", &lake),
            member(2, "inner", &bay),
            member(3, "outer", &island),
        ])
        .unwrap();
        assert_eq!(mp.polygons.len(), 2);
        let big = mp.polygons.iter().find(|p| p.outer == lake).unwrap();
        assert_eq!(big.inners, vec![bay]);
    }

    #[test]
    fn an_inner_goes_to_the_smallest_outer_containing_it() {
        let big = square(0, 0, 40, 40);
        let nested_outer = square(10, 10, 30, 30);
        let hole = square(15, 15, 20, 20);
        let mp = run(&[
            member(1, "outer", &big),
            member(2, "outer", &nested_outer),
            member(3, "inner", &hole),
        ])
        .unwrap();
        let owner = mp.polygons.iter().find(|p| !p.inners.is_empty()).unwrap();
        assert_eq!(owner.outer, nested_outer);
    }

    #[test]
    fn an_inner_touching_the_outer_at_a_shared_node_is_still_contained() {
        let outer = square(0, 0, 10, 10);
        // Shares the corner node (0,0) with the outer ring.
        let hole = vec![n(0, 0), n(4, 1), n(1, 4), n(0, 0)];
        let mp = run(&[member(1, "outer", &outer), member(2, "inner", &hole)]).unwrap();
        assert_eq!(mp.polygons[0].inners.len(), 1);
    }

    #[test]
    fn empty_roles_count_as_outer() {
        let w = square(0, 0, 10, 10);
        let mp = run(&[member(1, "", &w)]).unwrap();
        assert_eq!(mp.polygons.len(), 1);
    }

    #[test]
    fn rings_without_roles_are_classified_by_nesting() {
        let outer = square(0, 0, 20, 20);
        let hole = square(5, 5, 15, 15);
        let island = square(8, 8, 12, 12);
        let mp = run(&[
            member(1, "", &outer),
            member(2, "", &hole),
            member(3, "", &island),
        ])
        .unwrap();
        assert_eq!(mp.polygons.len(), 2);
        let big = mp.polygons.iter().find(|p| p.outer == outer).unwrap();
        assert_eq!(big.inners, vec![hole]);
    }

    #[test]
    fn a_ring_with_an_explicit_outer_role_is_not_reclassified_by_nesting() {
        let outer = square(0, 0, 20, 20);
        let inside = square(5, 5, 15, 15);
        let mp = run(&[member(1, "outer", &outer), member(2, "outer", &inside)]).unwrap();
        assert_eq!(mp.polygons.len(), 2);
    }

    #[test]
    fn members_with_other_roles_are_ignored() {
        let w = square(0, 0, 10, 10);
        let junk = vec![n(50, 50)];
        let mp = run(&[member(1, "outer", &w), member(2, "label", &junk)]).unwrap();
        assert_eq!(mp.polygons.len(), 1);
    }

    #[test]
    fn a_missing_way_invalidates_the_relation() {
        let w = square(0, 0, 10, 10);
        let gone = WayMember {
            way_id: 2,
            role: "inner",
            nodes: None,
        };
        assert_eq!(
            run(&[member(1, "outer", &w), gone]),
            Err(Problem::MissingWay(2))
        );
    }

    #[test]
    fn a_missing_node_invalidates_the_relation() {
        let w = vec![n(0, 0), n(10, 0), -5, n(0, 10), n(0, 0)];
        assert_eq!(
            run(&[member(1, "outer", &w)]),
            Err(Problem::MissingNode(-5))
        );
    }

    #[test]
    fn a_ring_that_never_closes_is_invalid() {
        let a = vec![n(0, 0), n(10, 0), n(10, 10)];
        let b = vec![n(10, 10), n(0, 10)];
        assert_eq!(
            run(&[member(1, "outer", &a), member(2, "outer", &b)]),
            Err(Problem::Unclosed)
        );
    }

    #[test]
    fn a_single_open_way_is_invalid() {
        let a = vec![n(0, 0), n(10, 0), n(10, 10)];
        assert_eq!(run(&[member(1, "outer", &a)]), Err(Problem::Unclosed));
    }

    #[test]
    fn a_duplicate_member_is_invalid() {
        let w = square(0, 0, 10, 10);
        assert_eq!(
            run(&[member(1, "outer", &w), member(1, "outer", &w)]),
            Err(Problem::DuplicateMember(1))
        );
    }

    #[test]
    fn a_ring_touching_itself_is_invalid() {
        // A figure eight through node (5,5).
        let w = vec![
            n(0, 0),
            n(5, 5),
            n(10, 0),
            n(10, 10),
            n(5, 5),
            n(0, 10),
            n(0, 0),
        ];
        assert_eq!(
            run(&[member(1, "outer", &w)]),
            Err(Problem::SelfTouching(n(5, 5)))
        );
    }

    #[test]
    fn a_node_where_three_open_ways_end_is_a_branch_not_a_guess() {
        let a = vec![n(0, 0), n(5, 5)];
        let b = vec![n(5, 5), n(10, 0)];
        let c = vec![n(5, 5), n(5, 10)];
        assert_eq!(
            run(&[
                member(1, "outer", &a),
                member(2, "outer", &b),
                member(3, "outer", &c)
            ]),
            Err(Problem::Branching(n(5, 5)))
        );
    }

    #[test]
    fn an_inner_outside_every_outer_is_invalid() {
        let outer = square(0, 0, 10, 10);
        let stray = square(20, 20, 25, 25);
        assert_eq!(
            run(&[member(1, "outer", &outer), member(2, "inner", &stray)]),
            Err(Problem::UnmatchedInner)
        );
    }

    #[test]
    fn no_outer_is_invalid() {
        let hole = square(0, 0, 10, 10);
        assert_eq!(run(&[member(1, "inner", &hole)]), Err(Problem::NoOuter));
        assert_eq!(run(&[]), Err(Problem::NoOuter));
    }

    #[test]
    fn degenerate_ways_never_panic() {
        let one = vec![n(0, 0)];
        let empty: Vec<i64> = vec![];
        let tiny = vec![n(0, 0), n(1, 1), n(0, 0)];
        let flat = vec![n(0, 0), n(5, 0), n(10, 0), n(0, 0)];
        assert_eq!(
            run(&[member(1, "outer", &one)]),
            Err(Problem::DegenerateWay(1))
        );
        assert_eq!(
            run(&[member(1, "outer", &empty)]),
            Err(Problem::DegenerateWay(1))
        );
        assert_eq!(
            run(&[member(1, "outer", &tiny)]),
            Err(Problem::DegenerateWay(1))
        );
        assert_eq!(run(&[member(1, "outer", &flat)]), Err(Problem::ZeroArea));
    }

    #[test]
    fn a_roleless_relation_with_one_open_and_one_closed_way_is_invalid_not_a_panic() {
        let closed = square(0, 0, 10, 10);
        let open = vec![n(20, 20), n(30, 30)];
        assert!(run(&[member(1, "", &closed), member(2, "", &open)]).is_err());
    }

    #[test]
    fn a_large_ring_assembles_quickly_and_triangulates_with_a_hole() {
        // A 4000-vertex circle split into 40 ways, plus a 2000-vertex hole.
        let coord = |id: i64| -> Option<(f64, f64)> {
            let (r, k, total) = if id >= 100_000 {
                (400.0, (id - 100_000) as f64, 2000.0)
            } else {
                (1000.0, id as f64, 4000.0)
            };
            let a = k / total * std::f64::consts::TAU;
            Some((r * a.cos(), r * a.sin()))
        };
        let ring: Vec<i64> = (0..4000).collect();
        let ways: Vec<Vec<i64>> = (0..40)
            .map(|i| {
                let lo = i * 100;
                let hi = ((i + 1) * 100).min(3999);
                let mut w: Vec<i64> = ring[lo..=hi].to_vec();
                if i == 39 {
                    w.push(0);
                }
                w
            })
            .collect();
        let mut hole: Vec<i64> = (100_000..102_000).collect();
        hole.push(100_000);
        let mut members: Vec<WayMember> = ways
            .iter()
            .enumerate()
            .map(|(i, w)| member(i as i64 + 1, "outer", w))
            .collect();
        members.push(member(1000, "inner", &hole));
        let mp = assemble(&members, &coord).unwrap();
        assert_eq!(mp.polygons.len(), 1);
        let p = &mp.polygons[0];
        let outer_pts: Vec<(f64, f64)> = p.outer[..p.outer.len() - 1]
            .iter()
            .map(|&i| coord(i).unwrap())
            .collect();
        let hole_pts: Vec<(f64, f64)> = p.inners[0][..p.inners[0].len() - 1]
            .iter()
            .map(|&i| coord(i).unwrap())
            .collect();
        let tris = triangulate(&[&outer_pts, &hole_pts]).unwrap();
        assert_eq!(tris.len() % 3, 0);
        assert!(tris.len() > 3 * 1000);
    }

    #[test]
    fn triangulation_leaves_the_hole_empty() {
        let outer = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let hole = [(3.0, 3.0), (3.0, 7.0), (7.0, 7.0), (7.0, 3.0)];
        let tris = triangulate(&[&outer, &hole]).unwrap();
        let all: Vec<(f64, f64)> = outer.iter().chain(hole.iter()).copied().collect();
        let mut area = 0.0;
        for t in tris.chunks(3) {
            area += ring_area(&[all[t[0] as usize], all[t[1] as usize], all[t[2] as usize]]);
        }
        assert!((area - 84.0).abs() < 1e-9, "triangle area was {area}");
    }

    #[test]
    fn triangulating_garbage_does_not_panic() {
        assert!(triangulate(&[]).is_none());
        assert!(triangulate(&[&[(0.0, 0.0)]]).is_none());
        let line = [(0.0, 0.0), (1.0, 1.0), (2.0, 2.0)];
        let _ = triangulate(&[&line]);
    }

    #[test]
    fn is_area_relation_accepts_multipolygon_and_boundary_only() {
        let tags = |t: &str| HashMap::from([("type".to_string(), t.to_string())]);
        assert!(is_area_relation(&tags("multipolygon")));
        assert!(is_area_relation(&tags("boundary")));
        assert!(!is_area_relation(&tags("route")));
        assert!(!is_area_relation(&HashMap::new()));
    }

    #[test]
    fn point_in_ring_works_with_and_without_a_repeated_first_vertex() {
        let open = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let mut closed = open.to_vec();
        closed.push((0.0, 0.0));
        for ring in [&open[..], &closed[..]] {
            assert!(point_in_ring(5.0, 5.0, ring, |&p| p));
            assert!(!point_in_ring(15.0, 5.0, ring, |&p| p));
        }
    }
}
