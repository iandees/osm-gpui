//! Parallel ways: a copy of a way offset sideways by a fixed distance (dual
//! carriageways, sidewalks, fences). Pure geometry; the selection action and
//! its dialog live in the binary crate.
//!
//! The work is done in a local metric plane around the way (see
//! [`LocalProjection`]), so "5 m" means 5 m on the ground. Each segment is
//! shifted sideways; consecutive shifted segments are joined where their
//! lines meet (a miter). On the outside of a bend, a miter whose tip would
//! stick out further than [`MITER_LIMIT`] times the offset is replaced by a
//! bevel (two points, one per shifted segment), so a sharp turn never makes
//! the line shoot off. On the inside of a bend the shifted segments cross;
//! the crossing is the join, or, when a segment is too short for that, the
//! small loop it leaves behind is cut out afterwards (see [`remove_loops`]).

use crate::geometry::{mean_point, LatLon, LocalProjection};

/// Longest miter, as a multiple of the offset distance, before the outside of
/// a bend is bevelled instead. 2.0 is reached at a turn of 120 degrees.
pub const MITER_LIMIT: f64 = 2.0;

/// The offset the dialog starts with, in metres.
pub const DEFAULT_OFFSET_M: f64 = 5.0;

/// The largest offset the dialog accepts, in metres either way.
pub const MAX_OFFSET_M: f64 = 1000.0;

/// Points closer together than this (metres) count as one.
const MIN_SEGMENT_M: f64 = 1e-6;

/// Loop removal is skipped for offset lines with more segments than this: it
/// compares every pair of segments.
const MAX_LOOP_SEGMENTS: usize = 4000;

/// What the Create Parallel Way dialog asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParallelParams {
    /// Offset in metres: positive is to the right of the way's direction,
    /// negative to the left.
    pub offset_m: f64,
    /// Give the copy the original's tags.
    pub copy_tags: bool,
    /// Reverse the copy's node order (the other carriageway).
    pub reverse: bool,
}

impl Default for ParallelParams {
    fn default() -> Self {
        Self {
            offset_m: DEFAULT_OFFSET_M,
            copy_tags: true,
            reverse: false,
        }
    }
}

/// Parse what was typed in the offset field: a non-zero number of metres with
/// an optional sign, a decimal point or comma and a trailing "m". `None` if
/// it is not a usable offset (not a number, zero, or beyond
/// [`MAX_OFFSET_M`]).
pub fn parse_offset(text: &str) -> Option<f64> {
    let t = text.trim();
    let t = t.strip_suffix('m').unwrap_or(t).trim().replace(',', ".");
    let v: f64 = t.parse().ok()?;
    (v.is_finite() && v != 0.0 && v.abs() <= MAX_OFFSET_M).then_some(v)
}

type P = (f64, f64);

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

fn dist(a: P, b: P) -> f64 {
    let d = sub(a, b);
    (d.0 * d.0 + d.1 * d.1).sqrt()
}

/// The way `points` shifted sideways by `offset_m` metres (positive: to the
/// right of its direction of travel, negative: to the left). A closed way
/// (first point equal to the last) gives a closed ring, outside for the
/// side that faces away from the inside.
///
/// Consecutive duplicate points are dropped first, so the result can have
/// fewer or, where a sharp outside bend is bevelled, more points than the
/// input. `None` if there is nothing to offset: fewer than two distinct
/// points (three for a ring), or a non-finite coordinate or offset.
pub fn offset_way(points: &[LatLon], offset_m: f64) -> Option<Vec<LatLon>> {
    if !offset_m.is_finite() || points.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return None;
    }
    let closed = points.len() >= 4 && points.first() == points.last();
    let vertices = if closed {
        &points[..points.len() - 1]
    } else {
        points
    };
    if vertices.is_empty() {
        return None;
    }
    let projection = LocalProjection::new(mean_point(vertices));
    let mut xy: Vec<P> = Vec::with_capacity(vertices.len());
    for &p in vertices {
        let q = projection.project(p);
        let apart = match xy.last() {
            Some(&last) => dist(last, q) > MIN_SEGMENT_M,
            None => true,
        };
        if apart {
            xy.push(q);
        }
    }
    if closed && xy.len() >= 2 && dist(xy[0], xy[xy.len() - 1]) <= MIN_SEGMENT_M {
        xy.pop();
    }
    let min_points = if closed { 3 } else { 2 };
    if xy.len() < min_points {
        return None;
    }

    let shifted = offset_points(&xy, closed, offset_m);
    let mut line = remove_loops(&shifted, closed).unwrap_or(shifted);
    if closed {
        line.push(line[0]);
    }
    let out: Vec<LatLon> = line.into_iter().map(|p| projection.unproject(p)).collect();
    out.iter()
        .all(|p| p.0.is_finite() && p.1.is_finite())
        .then_some(out)
}

/// Shift every vertex of the (de-duplicated) polyline `xy` by `d` to the right.
/// For a ring, `xy` has no closing duplicate and the result neither.
fn offset_points(xy: &[P], closed: bool, d: f64) -> Vec<P> {
    let n = xy.len();
    let segments = if closed { n } else { n - 1 };
    // Unit direction, length and right-hand normal of each segment.
    let mut dirs: Vec<(P, f64)> = Vec::with_capacity(segments);
    for k in 0..segments {
        let v = sub(xy[(k + 1) % n], xy[k]);
        let len = (v.0 * v.0 + v.1 * v.1).sqrt();
        dirs.push(((v.0 / len, v.1 / len), len));
    }
    let normal = |u: P| (u.1, -u.0);

    let mut out = Vec::with_capacity(n + 2);
    for i in 0..n {
        if !closed && i == 0 {
            let r = normal(dirs[0].0);
            out.push((xy[0].0 + r.0 * d, xy[0].1 + r.1 * d));
        } else if !closed && i == n - 1 {
            let r = normal(dirs[n - 2].0);
            out.push((xy[i].0 + r.0 * d, xy[i].1 + r.1 * d));
        } else {
            let a = (i + segments - 1) % segments;
            let b = i % segments;
            join(&mut out, xy[i], dirs[a], dirs[b], d);
        }
    }
    out
}

/// Push the offset line's point(s) at vertex `p`, where a segment with
/// direction and length `a` meets the next one, `b`.
fn join(out: &mut Vec<P>, p: P, a: (P, f64), b: (P, f64), d: f64) {
    let (ua, len_a) = a;
    let (ub, len_b) = b;
    let ra = (ua.1, -ua.0);
    let rb = (ub.1, -ub.0);
    let bevel = |out: &mut Vec<P>| {
        out.push((p.0 + ra.0 * d, p.1 + ra.1 * d));
        out.push((p.0 + rb.0 * d, p.1 + rb.1 * d));
    };
    // 1 + cos(turn) = 2 cos^2(turn / 2); the miter is d / cos(turn / 2) long.
    let denom = 1.0 + dot(ra, rb);
    if denom < 1e-9 {
        // A U-turn: no miter exists.
        bevel(out);
        return;
    }
    let ratio = (2.0 / denom).sqrt();
    // Turning away from the offset side makes it the outside of the bend.
    let outside = cross(ua, ub) * d > 0.0;
    let miter = ((ra.0 + rb.0) * d / denom, (ra.1 + rb.1) * d / denom);
    if outside {
        if ratio > MITER_LIMIT {
            bevel(out);
        } else {
            out.push((p.0 + miter.0, p.1 + miter.1));
        }
        return;
    }
    // Inside: the shifted segments cross at the miter point, provided both
    // are long enough to reach it. Otherwise keep both ends; the loop they
    // leave is cut out later.
    let back = -dot(miter, ua);
    let forward = dot(miter, ub);
    if back > len_a || forward > len_b {
        bevel(out);
    } else {
        out.push((p.0 + miter.0, p.1 + miter.1));
    }
}

/// Where segments `a0-a1` and `b0-b1` cross, if they cross strictly inside
/// both.
fn crossing(a0: P, a1: P, b0: P, b1: P) -> Option<P> {
    let r = sub(a1, a0);
    let s = sub(b1, b0);
    let denom = cross(r, s);
    if denom.abs() < 1e-12 {
        return None;
    }
    let q = sub(b0, a0);
    let t = cross(q, s) / denom;
    let u = cross(q, r) / denom;
    const EPS: f64 = 1e-9;
    (t > EPS && t < 1.0 - EPS && u > EPS && u < 1.0 - EPS)
        .then_some((a0.0 + r.0 * t, a0.1 + r.1 * t))
}

/// Cut out the loops where the offset line crosses itself (the inside of
/// tight bends): at the first pair of non-adjacent segments that cross, the
/// part between them is replaced by the crossing point. Repeats until no
/// crossing is left. `None` if there is nothing to cut, the line is too long
/// to check, or cutting would leave too little of it.
///
/// This also straightens out a way that really crosses itself, and a ring
/// offset by more than its own size is not repaired (its loops are cut but
/// the ring that remains may be the wrong shape); both are rare for the
/// sidewalk-sized offsets this is meant for.
fn remove_loops(line: &[P], closed: bool) -> Option<Vec<P>> {
    let min_points = if closed { 3 } else { 2 };
    let mut poly: Vec<P> = line.to_vec();
    if closed {
        poly.push(poly[0]);
    }
    if poly.len() - 1 > MAX_LOOP_SEGMENTS {
        return None;
    }
    let mut changed = false;
    for _ in 0..32 {
        let m = poly.len() - 1;
        let mut found = None;
        'search: for i in 0..m {
            for j in i + 2..m {
                if closed && i == 0 && j == m - 1 {
                    continue;
                }
                if let Some(x) = crossing(poly[i], poly[i + 1], poly[j], poly[j + 1]) {
                    found = Some((i, j, x));
                    break 'search;
                }
            }
        }
        let Some((i, j, x)) = found else {
            break;
        };
        let mut next: Vec<P> = poly[..=i].to_vec();
        next.push(x);
        next.extend_from_slice(&poly[j + 1..]);
        poly = next;
        changed = true;
    }
    if !changed {
        return None;
    }
    if closed {
        poly.pop();
    }
    (poly.len() >= min_points).then_some(poly)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: LatLon = (47.0, 8.0);

    fn proj() -> LocalProjection {
        LocalProjection::new(ORIGIN)
    }

    /// Metre coordinates (east, north) around `ORIGIN` as lat/lon.
    fn ll(points: &[P]) -> Vec<LatLon> {
        let p = proj();
        points.iter().map(|&q| p.unproject(q)).collect()
    }

    fn xy(points: &[LatLon]) -> Vec<P> {
        let p = proj();
        points.iter().map(|&q| p.project(q)).collect()
    }

    fn close(a: P, b: P, tol: f64) -> bool {
        dist(a, b) < tol
    }

    fn point_to_segment(p: P, a: P, b: P) -> f64 {
        let ab = sub(b, a);
        let len2 = dot(ab, ab);
        let t = if len2 == 0.0 {
            0.0
        } else {
            (dot(sub(p, a), ab) / len2).clamp(0.0, 1.0)
        };
        dist(p, (a.0 + ab.0 * t, a.1 + ab.1 * t))
    }

    fn point_to_polyline(p: P, line: &[P]) -> f64 {
        line.windows(2)
            .map(|w| point_to_segment(p, w[0], w[1]))
            .fold(f64::INFINITY, f64::min)
    }

    fn signed_area(ring: &[P]) -> f64 {
        ring.windows(2).map(|w| cross(w[0], w[1])).sum::<f64>() / 2.0
    }

    #[test]
    fn straight_line_is_offset_to_the_right_by_the_distance() {
        // Heading east: right is south.
        let line = ll(&[(0.0, 0.0), (50.0, 0.0), (100.0, 0.0)]);
        let out = xy(&offset_way(&line, 5.0).unwrap());
        assert_eq!(out.len(), 3);
        for (p, x) in out.iter().zip([0.0, 50.0, 100.0]) {
            assert!(close(*p, (x, -5.0), 1e-3), "{p:?}");
        }
    }

    #[test]
    fn negative_offset_goes_to_the_left() {
        let line = ll(&[(0.0, 0.0), (100.0, 0.0)]);
        let out = xy(&offset_way(&line, -7.5).unwrap());
        assert!(close(out[0], (0.0, 7.5), 1e-3));
        assert!(close(out[1], (100.0, 7.5), 1e-3));
    }

    #[test]
    fn the_copy_is_at_the_offset_distance_on_the_ground_at_high_latitude() {
        let line = vec![(70.0, 20.0), (70.001, 20.002), (70.002, 20.004)];
        let out = offset_way(&line, 12.0).unwrap();
        let proj = LocalProjection::new(mean_point(&line));
        let original: Vec<P> = line.iter().map(|&q| proj.project(q)).collect();
        for q in &out {
            let d = point_to_polyline(proj.project(*q), &original);
            assert!((d - 12.0).abs() < 0.01, "{d}");
        }
    }

    #[test]
    fn right_angle_bend_is_mitered() {
        // East then north: a left turn, so the right-hand side is the outside.
        let line = ll(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]);
        let out = xy(&offset_way(&line, 5.0).unwrap());
        assert_eq!(out.len(), 3);
        assert!(close(out[0], (0.0, -5.0), 1e-3));
        assert!(close(out[1], (105.0, -5.0), 1e-3), "{:?}", out[1]);
        assert!(close(out[2], (105.0, 100.0), 1e-3));
        // The inside of the same bend meets at (95, 5).
        let out = xy(&offset_way(&line, -5.0).unwrap());
        assert_eq!(out.len(), 3);
        assert!(close(out[1], (95.0, 5.0), 1e-3), "{:?}", out[1]);
    }

    #[test]
    fn sharp_outside_bend_is_bevelled_not_mitered() {
        // East, then back west at a shallow angle: a turn of about 174 degrees.
        let line = ll(&[(0.0, 0.0), (100.0, 0.0), (0.0, 10.0)]);
        let out = xy(&offset_way(&line, 5.0).unwrap());
        // Start, two bevel points, end.
        assert_eq!(out.len(), 4);
        let vertex = (100.0, 0.0);
        assert!((dist(out[1], vertex) - 5.0).abs() < 1e-3);
        assert!((dist(out[2], vertex) - 5.0).abs() < 1e-3);
        let original = xy(&line);
        for p in &out {
            assert!(point_to_polyline(*p, &original) <= 5.0 * MITER_LIMIT + 1e-3);
        }
    }

    #[test]
    fn mild_outside_bend_within_the_limit_still_gets_a_miter() {
        // A 90 degree turn is under the limit; so is 100 degrees.
        let t = 100f64.to_radians();
        let line = ll(&[
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0 + 100.0 * t.cos(), 100.0 * t.sin()),
        ]);
        let out = offset_way(&line, 5.0).unwrap();
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn tight_inside_bend_with_a_short_leg_keeps_every_point_near_the_line() {
        // The second leg is much shorter than the offset needs, on the inside.
        let line = ll(&[(0.0, 0.0), (100.0, 0.0), (95.0, 10.0), (0.0, 12.0)]);
        let original = xy(&line);
        for d in [5.0, -5.0, 20.0, -20.0] {
            let out = xy(&offset_way(&line, d).unwrap());
            for p in &out {
                let near = point_to_polyline(*p, &original);
                assert!(near <= d.abs() * MITER_LIMIT + 1e-3, "{d}: {p:?} is {near}");
            }
        }
    }

    #[test]
    fn inside_loop_is_cut_out() {
        // Offsetting a short hook inwards by more than its leg crosses itself.
        let line = ll(&[(0.0, 0.0), (100.0, 0.0), (100.0, 4.0), (0.0, 4.0)]);
        let out = xy(&offset_way(&line, 3.0).unwrap());
        // No two non-adjacent segments cross any more.
        for i in 0..out.len() - 1 {
            for j in i + 2..out.len() - 1 {
                assert!(
                    crossing(out[i], out[i + 1], out[j], out[j + 1]).is_none(),
                    "segments {i} and {j} cross in {out:?}"
                );
            }
        }
    }

    fn square(side: f64) -> Vec<P> {
        // Counter-clockwise, closed.
        vec![
            (0.0, 0.0),
            (side, 0.0),
            (side, side),
            (0.0, side),
            (0.0, 0.0),
        ]
    }

    #[test]
    fn closed_ring_offsets_outwards_or_inwards_by_sign() {
        let ring = ll(&square(100.0));
        // Counter-clockwise: the right-hand side is outside.
        let out = xy(&offset_way(&ring, 10.0).unwrap());
        assert_eq!(out.first(), out.last());
        assert_eq!(out.len(), 5);
        assert!(
            (signed_area(&out) - 120.0 * 120.0).abs() < 0.5,
            "{}",
            signed_area(&out)
        );
        let inward = xy(&offset_way(&ring, -10.0).unwrap());
        assert_eq!(inward.first(), inward.last());
        assert!((signed_area(&inward) - 80.0 * 80.0).abs() < 0.5);
    }

    #[test]
    fn clockwise_ring_flips_which_sign_is_outside() {
        let mut pts = square(100.0);
        pts.reverse();
        let ring = ll(&pts);
        let out = xy(&offset_way(&ring, 10.0).unwrap());
        // Clockwise: right is inside; the ring shrinks (area is negative).
        assert!((signed_area(&out).abs() - 80.0 * 80.0).abs() < 0.5);
    }

    #[test]
    fn degenerate_input_gives_none_or_finite_points() {
        assert!(offset_way(&[], 5.0).is_none());
        assert!(offset_way(&[ORIGIN], 5.0).is_none());
        assert!(offset_way(&[ORIGIN, ORIGIN], 5.0).is_none());
        assert!(offset_way(&[ORIGIN, ORIGIN, ORIGIN, ORIGIN], 5.0).is_none());
        let a = (47.0, 8.0);
        let b = (47.001, 8.0);
        assert!(offset_way(&[a, b], f64::NAN).is_none());
        assert!(offset_way(&[a, (f64::NAN, 8.0)], 5.0).is_none());
        // Duplicate points and a collinear run are fine.
        let out = offset_way(&[a, a, b, b, (47.002, 8.0)], 5.0).unwrap();
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
        // A full U-turn does not produce NaN.
        let out = offset_way(&[a, b, a, b], 5.0);
        if let Some(out) = out {
            assert!(out.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
        }
    }

    #[test]
    fn offset_text_parsing() {
        assert_eq!(parse_offset("5"), Some(5.0));
        assert_eq!(parse_offset(" -3,5 m "), Some(-3.5));
        assert_eq!(parse_offset("12.25m"), Some(12.25));
        assert_eq!(parse_offset("0"), None);
        assert_eq!(parse_offset(""), None);
        assert_eq!(parse_offset("abc"), None);
        assert_eq!(parse_offset("NaN"), None);
        assert_eq!(parse_offset("inf"), None);
        assert_eq!(parse_offset("1001"), None);
    }
}
