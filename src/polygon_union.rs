//! The outline of two overlapping simple polygons joined into one. Pure
//! planar geometry on plain points: the caller scales longitude so the plane
//! is roughly metric, and maps the crossings back to coordinates.
//!
//! Only the plain case is handled: the outlines cross each other transversally
//! at points that are on no vertex, and the union is a single ring with no
//! hole. Anything else is reported, not guessed at.

/// A point in the plane.
pub type Pt = (f64, f64);

/// Why two outlines have no single-ring union this module can build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnionError {
    /// The outlines share a node, so the crossings are not transversal.
    SharedNode,
    /// An edge touches the other outline at a vertex, or runs along it.
    Degenerate,
    /// The outlines never cross: apart, or one inside the other.
    NoOverlap,
    /// The union would have a hole or be in several pieces.
    NotOneRing,
}

/// One vertex of the union's outline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Vertex {
    /// The `index`th vertex of the first (`which == 0`) or second outline,
    /// in the order the caller gave them.
    Original { which: usize, index: usize },
    /// The `n`th crossing of `Union::crossings`.
    Crossing(usize),
}

/// The union outline, open (the first vertex is not repeated at the end).
#[derive(Clone, Debug)]
pub struct Union {
    pub ring: Vec<Vertex>,
    /// Where the outlines cross.
    pub crossings: Vec<Pt>,
}

const EPS: f64 = 1e-9;

fn cross(a: Pt, b: Pt) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

fn sub(a: Pt, b: Pt) -> Pt {
    (a.0 - b.0, a.1 - b.1)
}

fn signed_area(ring: &[Pt]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| cross(ring[i], ring[(i + 1) % n]))
        .sum::<f64>()
        / 2.0
}

/// Whether `p` is inside `ring` (even-odd).
fn contains(ring: &[Pt], p: Pt) -> bool {
    let n = ring.len();
    let mut inside = false;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            inside = !inside;
        }
    }
    inside
}

/// The union of the outlines `a` and `b` (each open, at least three points,
/// either winding). `shared_node` says whether vertex `i` of `a` and vertex
/// `j` of `b` are the same node.
pub fn union(
    a: &[Pt],
    b: &[Pt],
    shared_node: impl Fn(usize, usize) -> bool,
) -> Result<Union, UnionError> {
    let (na, nb) = (a.len(), b.len());
    if na < 3 || nb < 3 {
        return Err(UnionError::NoOverlap);
    }
    if (0..na).any(|i| (0..nb).any(|j| shared_node(i, j))) {
        return Err(UnionError::SharedNode);
    }
    // Walk both counter-clockwise; `order` maps walk position to input index.
    let order = |ring: &[Pt]| -> Vec<usize> {
        let n = ring.len();
        if signed_area(ring) >= 0.0 {
            (0..n).collect()
        } else {
            (0..n).rev().collect()
        }
    };
    let (oa, ob) = (order(a), order(b));
    let pa: Vec<Pt> = oa.iter().map(|&i| a[i]).collect();
    let pb: Vec<Pt> = ob.iter().map(|&i| b[i]).collect();

    // Crossings: (edge in a, t along it, edge in b, u along it, point).
    let mut crossings: Vec<(usize, f64, usize, f64, Pt)> = Vec::new();
    for i in 0..na {
        let (p, p2) = (pa[i], pa[(i + 1) % na]);
        let r = sub(p2, p);
        for j in 0..nb {
            let (q, q2) = (pb[j], pb[(j + 1) % nb]);
            let s = sub(q2, q);
            let d = cross(r, s);
            let qp = sub(q, p);
            if d.abs() < 1e-18 {
                // Parallel: running along each other is not handled.
                if cross(qp, r).abs() < 1e-18 {
                    let len2 = r.0 * r.0 + r.1 * r.1;
                    let t0 = (qp.0 * r.0 + qp.1 * r.1) / len2;
                    let t1 = t0 + (s.0 * r.0 + s.1 * r.1) / len2;
                    if t0.max(t1) > EPS && t0.min(t1) < 1.0 - EPS {
                        return Err(UnionError::Degenerate);
                    }
                }
                continue;
            }
            let t = cross(qp, s) / d;
            let u = cross(qp, r) / d;
            let near = |v: f64| v > -EPS && v < 1.0 + EPS;
            if !near(t) || !near(u) {
                continue;
            }
            if t < EPS || t > 1.0 - EPS || u < EPS || u > 1.0 - EPS {
                return Err(UnionError::Degenerate);
            }
            crossings.push((i, t, j, u, (p.0 + r.0 * t, p.1 + r.1 * t)));
        }
    }
    if crossings.is_empty() {
        return Err(UnionError::NoOverlap);
    }

    // Each outline's vertices with the crossings on its edges spliced in.
    #[derive(Clone, Copy, PartialEq)]
    enum Node {
        Vertex(usize),
        Cross(usize),
    }
    let build = |n: usize, edge_of: &dyn Fn(usize) -> (usize, f64)| -> Vec<Node> {
        let mut out = Vec::new();
        for edge in 0..n {
            out.push(Node::Vertex(edge));
            let mut on_edge: Vec<(f64, usize)> = (0..crossings.len())
                .filter(|&k| edge_of(k).0 == edge)
                .map(|k| (edge_of(k).1, k))
                .collect();
            on_edge.sort_by(|x, y| x.0.total_cmp(&y.0));
            out.extend(on_edge.into_iter().map(|(_, k)| Node::Cross(k)));
        }
        out
    };
    let seq = [
        build(na, &|k| (crossings[k].0, crossings[k].1)),
        build(nb, &|k| (crossings[k].2, crossings[k].3)),
    ];
    let rings = [&pa, &pb];
    let others = [&pb, &pa];

    // Start on a vertex that lies outside the other outline: it is on the
    // union's boundary.
    let start = (0..2).find_map(|w| {
        seq[w]
            .iter()
            .position(|n| matches!(n, Node::Vertex(v) if !contains(others[w], rings[w][*v])))
            .map(|pos| (w, pos))
    });
    let Some((mut which, start_pos)) = start else {
        return Err(UnionError::Degenerate);
    };
    let first = which;
    let mut pos = start_pos;
    let mut ring = Vec::new();
    let mut seen_crossings = 0;
    for _ in 0..=(seq[0].len() + seq[1].len()) {
        match seq[which][pos] {
            Node::Vertex(v) => {
                let index = if which == 0 { oa[v] } else { ob[v] };
                ring.push(Vertex::Original { which, index });
            }
            Node::Cross(k) => {
                ring.push(Vertex::Crossing(k));
                seen_crossings += 1;
                which = 1 - which;
                pos = seq[which]
                    .iter()
                    .position(|n| *n == Node::Cross(k))
                    .expect("a crossing is on both outlines");
            }
        }
        pos = (pos + 1) % seq[which].len();
        if which == first && pos == start_pos {
            if seen_crossings != crossings.len() {
                return Err(UnionError::NotOneRing);
            }
            return Ok(Union {
                ring,
                crossings: crossings.iter().map(|c| c.4).collect(),
            });
        }
    }
    Err(UnionError::NotOneRing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, size: f64) -> Vec<Pt> {
        vec![(x, y), (x + size, y), (x + size, y + size), (x, y + size)]
    }

    fn none(_: usize, _: usize) -> bool {
        false
    }

    #[test]
    fn two_offset_squares_unite_into_an_eight_point_outline() {
        let u = union(&square(0.0, 0.0, 2.0), &square(1.0, 1.0, 2.0), none).unwrap();
        assert_eq!(u.crossings.len(), 2);
        assert_eq!(u.ring.len(), 8);
        let originals = |w| {
            u.ring
                .iter()
                .filter(|v| matches!(v, Vertex::Original { which, .. } if *which == w))
                .count()
        };
        // Each square loses its corner inside the other.
        assert_eq!((originals(0), originals(1)), (3, 3));
    }

    #[test]
    fn the_outline_encloses_both_inputs_whatever_their_winding() {
        let a = square(0.0, 0.0, 2.0);
        let mut b = square(1.0, 1.0, 2.0);
        b.reverse();
        let u = union(&a, &b, none).unwrap();
        let pts: Vec<Pt> = u
            .ring
            .iter()
            .map(|v| match *v {
                Vertex::Original { which: 0, index } => a[index],
                Vertex::Original { index, .. } => b[index],
                Vertex::Crossing(k) => u.crossings[k],
            })
            .collect();
        assert!((signed_area(&pts).abs() - 7.0).abs() < 1e-9);
    }

    #[test]
    fn a_plus_shaped_overlap_makes_four_crossings() {
        let wide = vec![(0.0, 1.0), (3.0, 1.0), (3.0, 2.0), (0.0, 2.0)];
        let tall = vec![(1.0, 0.0), (2.0, 0.0), (2.0, 3.0), (1.0, 3.0)];
        let u = union(&wide, &tall, none).unwrap();
        assert_eq!(u.crossings.len(), 4);
        assert_eq!(u.ring.len(), 12);
    }

    #[test]
    fn apart_or_nested_outlines_do_not_overlap() {
        assert_eq!(
            union(&square(0.0, 0.0, 1.0), &square(5.0, 5.0, 1.0), none).unwrap_err(),
            UnionError::NoOverlap
        );
        assert_eq!(
            union(&square(0.0, 0.0, 4.0), &square(1.0, 1.0, 1.0), none).unwrap_err(),
            UnionError::NoOverlap
        );
    }

    #[test]
    fn a_shared_node_is_refused() {
        let r = union(&square(0.0, 0.0, 2.0), &square(1.0, 1.0, 2.0), |i, j| {
            i == 2 && j == 0
        });
        assert_eq!(r.unwrap_err(), UnionError::SharedNode);
    }

    #[test]
    fn an_edge_through_a_vertex_is_degenerate() {
        // b's corner lands exactly on a's top edge.
        let r = union(
            &square(0.0, 0.0, 2.0),
            &[(1.0, 2.0), (3.0, 1.0), (3.0, 3.0)],
            none,
        );
        assert_eq!(r.unwrap_err(), UnionError::Degenerate);
    }

    #[test]
    fn a_union_with_a_hole_is_refused() {
        // A "C" whose open end is closed by a bar leaves a hole.
        let c = vec![
            (0.0, 0.0),
            (4.0, 0.0),
            (4.0, 1.0),
            (1.0, 1.0),
            (1.0, 3.0),
            (4.0, 3.0),
            (4.0, 4.0),
            (0.0, 4.0),
        ];
        let bar = vec![(3.0, 0.5), (3.5, 0.5), (3.5, 3.5), (3.0, 3.5)];
        assert_eq!(union(&c, &bar, none).unwrap_err(), UnionError::NotOneRing);
    }
}
