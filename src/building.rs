//! Pure geometry for orienting new buildings to an existing "squared" way.
//!
//! All points are `(lat, lon)` in degrees. Angles are measured in a local
//! metric (equirectangular) projection centred near the points involved, so
//! that "perpendicular" really is perpendicular on the ground (a degree of
//! longitude is shorter than a degree of latitude away from the equator).
//! Orientations are bearings of a line, in radians, counter-clockwise from
//! the +x (east) axis, normalised to `[0, pi)` since a line has no direction.

const METERS_PER_DEGREE: f64 = 111_320.0;

/// Default tolerance for the squared test, in degrees.
pub const SQUARED_TOLERANCE_DEG: f64 = 10.0;

/// Edges shorter than this (metres) make a ring degenerate.
const MIN_EDGE_M: f64 = 1e-3;

fn project(p: (f64, f64), lat0: f64) -> (f64, f64) {
    let k = lat0.to_radians().cos();
    (p.1 * METERS_PER_DEGREE * k, p.0 * METERS_PER_DEGREE)
}

fn unproject(p: (f64, f64), lat0: f64) -> (f64, f64) {
    let k = lat0.to_radians().cos();
    (p.1 / METERS_PER_DEGREE, p.0 / (METERS_PER_DEGREE * k))
}

/// Projected vertices of a ring with any duplicated closing vertex dropped.
fn open_ring(ring: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut pts = ring.to_vec();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    if pts.is_empty() {
        return Vec::new();
    }
    let lat0 = pts.iter().map(|p| p.0).sum::<f64>() / pts.len() as f64;
    pts.iter().map(|&p| project(p, lat0)).collect()
}

/// True if `ring` (a closed way's vertices, closing vertex optional) has at
/// least four corners and every vertex turns by about 0 degrees (a straight
/// continuation) or about 90 degrees, within `tolerance_deg`.
pub fn is_squared(ring: &[(f64, f64)], tolerance_deg: f64) -> bool {
    let pts = open_ring(ring);
    let n = pts.len();
    if n < 4 {
        return false;
    }
    let mut right_angles = 0;
    for i in 0..n {
        let prev = pts[(i + n - 1) % n];
        let cur = pts[i];
        let next = pts[(i + 1) % n];
        let (ax, ay) = (cur.0 - prev.0, cur.1 - prev.1);
        let (bx, by) = (next.0 - cur.0, next.1 - cur.1);
        let (la, lb) = ((ax * ax + ay * ay).sqrt(), (bx * bx + by * by).sqrt());
        if la < MIN_EDGE_M || lb < MIN_EDGE_M {
            return false;
        }
        let cos = ((ax * bx + ay * by) / (la * lb)).clamp(-1.0, 1.0);
        let turn = cos.acos().to_degrees();
        if (turn - 90.0).abs() <= tolerance_deg {
            right_angles += 1;
        } else if turn > tolerance_deg {
            return false;
        }
    }
    right_angles >= 4
}

/// Orientation of the ring's longest edge, in radians in `[0, pi)`.
pub fn dominant_orientation(ring: &[(f64, f64)]) -> Option<f64> {
    let pts = open_ring(ring);
    let n = pts.len();
    if n < 2 {
        return None;
    }
    let mut best: Option<(f64, f64)> = None;
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        if len >= MIN_EDGE_M && best.is_none_or(|(l, _)| len > l) {
            best = Some((len, dy.atan2(dx)));
        }
    }
    best.map(|(_, ang)| ang.rem_euclid(std::f64::consts::PI))
}

/// The four corners (in ring order) of the rectangle whose sides are
/// parallel/perpendicular to `orientation` and which has `a` and `b` as
/// opposite corners. `None` if the rectangle would be degenerate (a side
/// shorter than a millimetre).
pub fn rectangle_from_opposite_corners(
    a: (f64, f64),
    b: (f64, f64),
    orientation: f64,
) -> Option<[(f64, f64); 4]> {
    let lat0 = (a.0 + b.0) / 2.0;
    let (pa, pb) = (project(a, lat0), project(b, lat0));
    let (dx, dy) = (pb.0 - pa.0, pb.1 - pa.1);
    let (c, s) = (orientation.cos(), orientation.sin());
    // Components of a->b along the orientation axis and its perpendicular.
    let u = dx * c + dy * s;
    let v = -dx * s + dy * c;
    if u.abs() < MIN_EDGE_M || v.abs() < MIN_EDGE_M {
        return None;
    }
    let c1 = (pa.0 + u * c, pa.1 + u * s);
    let c2 = (pa.0 - v * s, pa.1 + v * c);
    Some([a, unproject(c1, lat0), b, unproject(c2, lat0)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

    /// A rectangle `w` x `h` metres at latitude 45, rotated by `rot`
    /// radians, as a closed (lat, lon) ring.
    fn rotated_rect(w: f64, h: f64, rot: f64) -> Vec<(f64, f64)> {
        let (c, s) = (rot.cos(), rot.sin());
        let mut ring: Vec<(f64, f64)> = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]
            .iter()
            .map(|&(x, y)| unproject((x * c - y * s, x * s + y * c), 45.0))
            .map(|(lat, lon)| (45.0 + lat, -80.0 + lon))
            .collect();
        ring.push(ring[0]);
        ring
    }

    fn ang_diff(a: f64, b: f64) -> f64 {
        let d = (a - b).rem_euclid(PI);
        d.min(PI - d)
    }

    #[test]
    fn rectangle_is_squared() {
        assert!(is_squared(&rotated_rect(20.0, 10.0, 0.6), 10.0));
    }

    #[test]
    fn l_shape_is_squared() {
        let l = [
            (0.0, 0.0),
            (0.0, 0.0002),
            (0.0001, 0.0002),
            (0.0001, 0.0001),
            (0.0002, 0.0001),
            (0.0002, 0.0),
            (0.0, 0.0),
        ];
        assert!(is_squared(&l, 10.0));
    }

    #[test]
    fn collinear_vertex_is_allowed() {
        let ring = [
            (0.0, 0.0),
            (0.0, 0.0001),
            (0.0, 0.0002),
            (0.0001, 0.0002),
            (0.0001, 0.0),
        ];
        assert!(is_squared(&ring, 10.0));
    }

    #[test]
    fn triangle_and_skewed_quad_are_not_squared() {
        let tri = [(0.0, 0.0), (0.0, 0.0002), (0.0002, 0.0001)];
        assert!(!is_squared(&tri, 10.0));
        let parallelogram = [
            (0.0, 0.0),
            (0.0, 0.0002),
            (0.0001, 0.0003),
            (0.0001, 0.0001),
        ];
        assert!(!is_squared(&parallelogram, 10.0));
    }

    #[test]
    fn slightly_off_corner_is_within_tolerance_but_not_beyond() {
        // Corner turn of about 95 degrees at (0,0.0002) in metric space.
        let near = |dy: f64| {
            vec![
                (0.0, 0.0),
                (0.0, 0.0002),
                (0.0001, 0.0002 + dy),
                (0.0001, 0.0),
            ]
        };
        assert!(is_squared(&near(0.00001), 10.0));
        assert!(!is_squared(&near(0.0001), 10.0));
    }

    #[test]
    fn degenerate_rings_are_not_squared() {
        assert!(!is_squared(&[], 10.0));
        assert!(!is_squared(
            &[(0.0, 0.0), (0.0, 0.0), (0.0, 0.0), (0.0, 0.0)],
            10.0
        ));
    }

    #[test]
    fn orientation_is_longest_edge_bearing() {
        for rot in [0.0, 0.3, FRAC_PI_4, 1.2, FRAC_PI_2 + 0.2] {
            let ring = rotated_rect(30.0, 10.0, rot);
            let o = dominant_orientation(&ring).unwrap();
            assert!(ang_diff(o, rot) < 1e-3, "rot {rot}: got {o}");
            assert!((0.0..PI).contains(&o));
        }
        // Long side vertical.
        let ring = rotated_rect(10.0, 30.0, 0.0);
        assert!(ang_diff(dominant_orientation(&ring).unwrap(), FRAC_PI_2) < 1e-3);
    }

    #[test]
    fn rectangle_corners_are_right_angled_and_aligned() {
        let a = (45.0, -80.0);
        let b = (45.0003, -79.9995);
        let rot = 0.5;
        let r = rectangle_from_opposite_corners(a, b, rot).unwrap();
        assert_eq!(r[0], a);
        assert_eq!(r[2], b);
        assert!(is_squared(&r, 0.01));
        let o = dominant_orientation(&r).unwrap();
        assert!(ang_diff(o, rot) < 1e-3 || ang_diff(o, rot + FRAC_PI_2) < 1e-3);
    }

    #[test]
    fn rectangle_axis_aligned_has_expected_corners() {
        let r = rectangle_from_opposite_corners((10.0, 20.0), (10.001, 20.002), 0.0).unwrap();
        assert!((r[1].0 - 10.0).abs() < 1e-9 && (r[1].1 - 20.002).abs() < 1e-9);
        assert!((r[3].0 - 10.001).abs() < 1e-9 && (r[3].1 - 20.0).abs() < 1e-9);
    }

    #[test]
    fn rectangle_works_for_any_drag_direction() {
        let a = (45.0, -80.0);
        for b in [
            (45.0002, -79.9998),
            (44.9998, -79.9998),
            (45.0002, -80.0002),
            (44.9998, -80.0002),
        ] {
            let r = rectangle_from_opposite_corners(a, b, 0.7).unwrap();
            assert!(is_squared(&r, 0.01));
        }
    }

    #[test]
    fn degenerate_rectangle_is_none() {
        let a = (45.0, -80.0);
        assert!(rectangle_from_opposite_corners(a, a, 0.3).is_none());
        // b lies exactly along the orientation axis: zero width.
        let along = unproject((10.0 * 0.3f64.cos(), 10.0 * 0.3f64.sin()), 45.0);
        let b = (45.0 + along.0, -80.0 + along.1);
        assert!(rectangle_from_opposite_corners(a, b, 0.3).is_none());
    }
}
