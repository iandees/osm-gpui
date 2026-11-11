//! Align and distribute nodes: put a loose selection of nodes on one straight
//! line, or on that line at even spacing. Pure geometry in a local metric
//! plane; the selection actions live in the binary crate.
//!
//! Both work from the same line. The principal axis of the points (the
//! best-fit direction) picks the two extreme nodes: the ones furthest apart
//! along it. Those two are the anchors and never move; the line runs through
//! them. This is also what JOSM does for a plain node selection.

use crate::geometry::{mean_point, LatLon, LocalProjection};

type P = (f64, f64);

/// The common set-up of both operations.
struct Axis {
    projection: LocalProjection,
    xy: Vec<P>,
    /// Index of the extreme node at the start of the line, and at its end.
    lo: usize,
    hi: usize,
    /// Unit direction from `xy[lo]` to `xy[hi]`, and the distance between them.
    dir: P,
    len: f64,
}

/// Smallest distance (metres) between the anchors for a line to exist.
const MIN_LINE_M: f64 = 1e-6;

/// A node already within this distance (metres) of its target keeps its exact
/// coordinates, so aligning an aligned selection changes nothing.
const SNAP_M: f64 = 1e-4;

fn axis(points: &[LatLon]) -> Option<Axis> {
    if points.len() < 3 || points.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return None;
    }
    let projection = LocalProjection::new(mean_point(points));
    let xy: Vec<P> = points.iter().map(|&p| projection.project(p)).collect();
    let n = xy.len() as f64;
    let c = (
        xy.iter().map(|p| p.0).sum::<f64>() / n,
        xy.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for p in &xy {
        let (dx, dy) = (p.0 - c.0, p.1 - c.1);
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    // Direction of the largest-variance axis (total least squares).
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let principal = (theta.cos(), theta.sin());
    let along = |p: &P| (p.0 - c.0) * principal.0 + (p.1 - c.1) * principal.1;
    let mut lo = 0;
    let mut hi = 0;
    for (i, p) in xy.iter().enumerate() {
        if along(p) < along(&xy[lo]) {
            lo = i;
        }
        if along(p) > along(&xy[hi]) {
            hi = i;
        }
    }
    let (a, b) = (xy[lo], xy[hi]);
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    if lo == hi || len < MIN_LINE_M {
        return None;
    }
    Some(Axis {
        projection,
        xy,
        lo,
        hi,
        dir: ((b.0 - a.0) / len, (b.1 - a.1) / len),
        len,
    })
}

/// Whether there are enough nodes to align or distribute.
pub fn can_align(points: &[LatLon]) -> bool {
    points.len() >= 3
}

/// New positions for `points` (same length and order) with every node moved
/// onto the line through the two extreme nodes, which stay where they are.
/// Nodes keep their place along the line. `None` for fewer than three nodes,
/// non-finite input, or when the extremes are in the same place (all nodes at
/// one point).
pub fn align_in_line(points: &[LatLon]) -> Option<Vec<LatLon>> {
    let axis = axis(points)?;
    let a = axis.xy[axis.lo];
    let out = axis
        .xy
        .iter()
        .zip(points)
        .enumerate()
        .map(|(i, (&p, &original))| {
            if i == axis.lo || i == axis.hi {
                return original;
            }
            let s = (p.0 - a.0) * axis.dir.0 + (p.1 - a.1) * axis.dir.1;
            let target = (a.0 + axis.dir.0 * s, a.1 + axis.dir.1 * s);
            if (target.0 - p.0).hypot(target.1 - p.1) < SNAP_M {
                original
            } else {
                axis.projection.unproject(target)
            }
        })
        .collect();
    Some(out)
}

/// New positions for `points` (same length and order) spaced evenly on the
/// line between the two extreme nodes, which stay where they are. The nodes
/// keep their order along the line (ties keep input order), so the result
/// never reorders them. Nodes off the line end up on it. `None` as for
/// [`align_in_line`].
pub fn distribute(points: &[LatLon]) -> Option<Vec<LatLon>> {
    let axis = axis(points)?;
    let a = axis.xy[axis.lo];
    let s = |i: usize| {
        let p = axis.xy[i];
        (p.0 - a.0) * axis.dir.0 + (p.1 - a.1) * axis.dir.1
    };
    let mut middle: Vec<usize> = (0..points.len())
        .filter(|&i| i != axis.lo && i != axis.hi)
        .collect();
    middle.sort_by(|&i, &j| s(i).total_cmp(&s(j)).then(i.cmp(&j)));
    let mut order = Vec::with_capacity(points.len());
    order.push(axis.lo);
    order.extend(middle);
    order.push(axis.hi);

    let steps = (order.len() - 1) as f64;
    let mut out = points.to_vec();
    for (k, &i) in order.iter().enumerate() {
        if i == axis.lo || i == axis.hi {
            continue;
        }
        let t = axis.len * k as f64 / steps;
        let target = (a.0 + axis.dir.0 * t, a.1 + axis.dir.1 * t);
        let p = axis.xy[i];
        if (target.0 - p.0).hypot(target.1 - p.1) >= SNAP_M {
            out[i] = axis.projection.unproject(target);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: LatLon = (47.0, 8.0);

    fn ll(points: &[P]) -> Vec<LatLon> {
        let p = LocalProjection::new(ORIGIN);
        points.iter().map(|&q| p.unproject(q)).collect()
    }

    fn xy(points: &[LatLon]) -> Vec<P> {
        let p = LocalProjection::new(ORIGIN);
        points.iter().map(|&q| p.project(q)).collect()
    }

    /// Largest distance of any point from the line through the first and last.
    fn line_error(points: &[P], a: P, b: P) -> f64 {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = dx.hypot(dy);
        points
            .iter()
            .map(|p| ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / len)
            .fold(0.0, f64::max)
    }

    #[test]
    fn nodes_end_up_collinear_with_the_anchors_unmoved() {
        let input = ll(&[
            (0.0, 0.0),
            (20.0, 3.0),
            (40.0, -4.0),
            (60.0, 2.0),
            (100.0, 10.0),
        ]);
        let out = align_in_line(&input).unwrap();
        assert_eq!(out[0], input[0]);
        assert_eq!(out[4], input[4]);
        let pts = xy(&out);
        assert!(line_error(&pts, pts[0], pts[4]) < 1e-6);
        // Order along the line is kept.
        let along: Vec<f64> = pts.iter().map(|p| p.0).collect();
        assert!(along.windows(2).all(|w| w[0] < w[1]), "{along:?}");
    }

    #[test]
    fn anchors_are_the_extremes_whatever_the_input_order() {
        let input = ll(&[(40.0, 5.0), (100.0, 0.0), (0.0, 0.0), (70.0, -6.0)]);
        let out = align_in_line(&input).unwrap();
        assert_eq!(out[1], input[1]);
        assert_eq!(out[2], input[2]);
        let pts = xy(&out);
        assert!(line_error(&pts, pts[2], pts[1]) < 1e-6);
    }

    #[test]
    fn aligning_a_vertical_and_a_diagonal_line_works() {
        let input = ll(&[(0.0, 0.0), (3.0, 40.0), (-2.0, 80.0), (0.0, 120.0)]);
        let pts = xy(&align_in_line(&input).unwrap());
        assert!(line_error(&pts, pts[0], pts[3]) < 1e-6);
        let input = ll(&[(0.0, 0.0), (40.0, 46.0), (80.0, 74.0), (100.0, 100.0)]);
        let pts = xy(&align_in_line(&input).unwrap());
        assert!(line_error(&pts, pts[0], pts[3]) < 1e-6);
    }

    #[test]
    fn already_aligned_nodes_do_not_change() {
        let input = ll(&[(0.0, 0.0), (30.0, 0.0), (100.0, 0.0)]);
        assert_eq!(align_in_line(&input).unwrap(), input);
    }

    #[test]
    fn distribute_spaces_evenly_between_the_ends_in_order() {
        let input = ll(&[
            (0.0, 0.0),
            (10.0, 0.0),
            (15.0, 0.0),
            (90.0, 0.0),
            (100.0, 0.0),
        ]);
        let out = distribute(&input).unwrap();
        assert_eq!(out[0], input[0]);
        assert_eq!(out[4], input[4]);
        let pts = xy(&out);
        for (k, p) in pts.iter().enumerate() {
            assert!((p.0 - 25.0 * k as f64).abs() < 1e-6, "{k}: {p:?}");
            assert!(p.1.abs() < 1e-6);
        }
    }

    #[test]
    fn distribute_keeps_the_order_of_shuffled_input() {
        // Input order is not the order along the line.
        let input = ll(&[(60.0, 1.0), (0.0, 0.0), (20.0, -1.0), (100.0, 0.0)]);
        let out = distribute(&input).unwrap();
        let pts = xy(&out);
        // Node 1 is the start, node 3 the end; 2 comes before 0.
        assert!((pts[2].0 - 100.0 / 3.0).abs() < 1e-6);
        assert!((pts[0].0 - 200.0 / 3.0).abs() < 1e-6);
        assert_eq!(out[1], input[1]);
        assert_eq!(out[3], input[3]);
    }

    #[test]
    fn distribute_puts_nodes_that_are_off_the_line_on_it() {
        let input = ll(&[(0.0, 0.0), (30.0, 8.0), (60.0, -7.0), (90.0, 0.0)]);
        let pts = xy(&distribute(&input).unwrap());
        assert!(line_error(&pts, pts[0], pts[3]) < 1e-6);
        assert!((pts[1].0 - 30.0).abs() < 1e-6 && (pts[2].0 - 60.0).abs() < 1e-6);
    }

    #[test]
    fn degenerate_input_gives_none() {
        let a = ORIGIN;
        // All nodes at one point, two nodes, none, and non-finite coordinates.
        assert!(align_in_line(&[a, a, a, a]).is_none());
        assert!(distribute(&[a, a, a]).is_none());
        assert!(align_in_line(&[a, (47.001, 8.0)]).is_none());
        assert!(distribute(&[]).is_none());
        assert!(align_in_line(&[a, (f64::NAN, 8.0), (47.002, 8.0)]).is_none());
        assert!(!can_align(&[a, a]));
        assert!(can_align(&[a, a, a]));
    }

    #[test]
    fn duplicate_interior_nodes_and_collinear_input_are_finite() {
        let input = ll(&[(0.0, 0.0), (50.0, 5.0), (50.0, 5.0), (100.0, 0.0)]);
        for out in [align_in_line(&input).unwrap(), distribute(&input).unwrap()] {
            assert!(out.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
        }
        let pts = xy(&distribute(&input).unwrap());
        assert!((pts[1].0 - 100.0 / 3.0).abs() < 1e-6);
        assert!((pts[2].0 - 200.0 / 3.0).abs() < 1e-6);
    }
}
