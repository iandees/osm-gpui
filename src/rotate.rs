//! Rotate a group of nodes around their centre. Pure geometry in a local
//! metric plane (so shapes keep their proportions on the ground, whatever the
//! latitude); the interactive session lives in the binary crate.

use crate::geometry::{mean_point, LatLon, LocalProjection};

/// Smallest spread (metres) a selection needs to have an orientation worth
/// rotating: a handful of nodes in one spot would just spin in place.
const MIN_SPREAD_M: f64 = 1e-3;

/// The angle step (degrees) rotation snaps to while Shift is held.
pub const SNAP_STEP_DEG: f64 = 15.0;

/// The point a selection rotates around: the mean position of its nodes.
/// `None` for an empty or non-finite selection.
pub fn center(points: &[LatLon]) -> Option<LatLon> {
    if points.is_empty() || points.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return None;
    }
    Some(mean_point(points))
}

/// Whether rotating `points` would move anything: at least two nodes, not all
/// in the same place.
pub fn can_rotate(points: &[LatLon]) -> bool {
    let Some(c) = center(points) else {
        return false;
    };
    if points.len() < 2 {
        return false;
    }
    let projection = LocalProjection::new(c);
    points.iter().any(|&p| {
        let (x, y) = projection.project(p);
        x.hypot(y) > MIN_SPREAD_M
    })
}

/// `points` rotated by `angle_rad` around `center`, counter-clockwise on a
/// north-up map. An angle of zero returns the points untouched, bit for bit.
pub fn rotate_points(points: &[LatLon], center: LatLon, angle_rad: f64) -> Vec<LatLon> {
    if angle_rad == 0.0 || !angle_rad.is_finite() {
        return points.to_vec();
    }
    let projection = LocalProjection::new(center);
    let (sin, cos) = angle_rad.sin_cos();
    points
        .iter()
        .map(|&p| {
            let (x, y) = projection.project(p);
            projection.unproject((x * cos - y * sin, x * sin + y * cos))
        })
        .collect()
}

/// Angle (radians) of the pointer around the centre, both in screen pixels
/// (y grows downwards).
pub fn pointer_angle(center_px: (f64, f64), pointer_px: (f64, f64)) -> f64 {
    (pointer_px.1 - center_px.1).atan2(pointer_px.0 - center_px.0)
}

/// The map rotation (radians, counter-clockwise on a north-up map) for a
/// pointer that moved from `start` to `now` around the centre, both given as
/// [`pointer_angle`]s: the selection turns with the pointer. Normalised to
/// (-pi, pi] so a full turn of the pointer does not pile up.
pub fn rotation_for_pointer(start: f64, now: f64) -> f64 {
    // Screen y points down, so a clockwise pointer move has a growing screen
    // angle but is a negative angle on the north-up map.
    let delta = start - now;
    let tau = std::f64::consts::TAU;
    let wrapped = delta - tau * (delta / tau).round();
    if wrapped <= -std::f64::consts::PI {
        wrapped + tau
    } else {
        wrapped
    }
}

/// `angle_rad` rounded to the nearest multiple of `step_deg` degrees.
pub fn snap_angle(angle_rad: f64, step_deg: f64) -> f64 {
    let step = step_deg.to_radians();
    (angle_rad / step).round() * step
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    fn square(lat: f64, lon: f64) -> Vec<LatLon> {
        let d = 0.0005;
        vec![
            (lat - d, lon - d),
            (lat - d, lon + d),
            (lat + d, lon + d),
            (lat + d, lon - d),
        ]
    }

    fn metres(points: &[LatLon], c: LatLon) -> Vec<(f64, f64)> {
        let p = LocalProjection::new(c);
        points.iter().map(|&q| p.project(q)).collect()
    }

    #[test]
    fn rotation_preserves_distances_and_the_centre() {
        for lat in [0.0, 47.0, 70.0] {
            let pts = square(lat, 8.0);
            let c = center(&pts).unwrap();
            let out = rotate_points(&pts, c, 0.7);
            let (a, b) = (metres(&pts, c), metres(&out, c));
            for i in 0..a.len() {
                let (ra, rb) = (a[i].0.hypot(a[i].1), b[i].0.hypot(b[i].1));
                assert!((ra - rb).abs() < 1e-6, "radius changed at lat {lat}");
                for j in 0..i {
                    let da = (a[i].0 - a[j].0).hypot(a[i].1 - a[j].1);
                    let db = (b[i].0 - b[j].0).hypot(b[i].1 - b[j].1);
                    assert!((da - db).abs() < 1e-6, "shape changed at lat {lat}");
                }
            }
            let c2 = center(&out).unwrap();
            assert!((c2.0 - c.0).abs() < 1e-9 && (c2.1 - c.1).abs() < 1e-9);
        }
    }

    #[test]
    fn quarter_turn_is_counter_clockwise_on_a_north_up_map() {
        let c = (10.0, 20.0);
        let east = (10.0, 20.001);
        let out = rotate_points(&[east, (10.0, 19.999)], c, FRAC_PI_2);
        // East goes to north.
        assert!(out[0].0 > c.0 + 1e-4);
        assert!((out[0].1 - c.1).abs() < 1e-9);
        assert!(out[1].0 < c.0 - 1e-4);
    }

    #[test]
    fn zero_and_full_turns_leave_points_where_they_were() {
        let pts = square(51.0, -1.0);
        let c = center(&pts).unwrap();
        assert_eq!(rotate_points(&pts, c, 0.0), pts);
        for (a, b) in rotate_points(&pts, c, 2.0 * PI).iter().zip(&pts) {
            assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
        }
    }

    #[test]
    fn rotating_back_restores_the_original() {
        let pts = square(-33.0, 151.0);
        let c = center(&pts).unwrap();
        let there = rotate_points(&pts, c, 1.234);
        let back = rotate_points(&there, c, -1.234);
        for (a, b) in back.iter().zip(&pts) {
            assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
        }
    }

    #[test]
    fn can_rotate_needs_spread() {
        let p = (47.0, 8.0);
        assert!(!can_rotate(&[]));
        assert!(!can_rotate(&[p]));
        assert!(!can_rotate(&[p, p, p]));
        assert!(!can_rotate(&[p, (f64::NAN, 8.0)]));
        assert!(can_rotate(&[p, (47.001, 8.0)]));
    }

    #[test]
    fn clockwise_pointer_motion_turns_the_selection_clockwise() {
        let c = (100.0, 100.0);
        let start = pointer_angle(c, (200.0, 100.0)); // east of the centre
        let now = pointer_angle(c, (100.0, 200.0)); // south: clockwise on screen
        let angle = rotation_for_pointer(start, now);
        assert!((angle + FRAC_PI_2).abs() < 1e-9);
        // A quarter turn clockwise sends east to south.
        let out = rotate_points(&[(10.0, 20.001), (10.0, 19.999)], (10.0, 20.0), angle);
        assert!(out[0].0 < 10.0 - 1e-4);
    }

    #[test]
    fn pointer_rotation_wraps_instead_of_piling_up() {
        let a = rotation_for_pointer(PI - 0.1, -PI + 0.1);
        assert!(a.abs() < 0.3, "{a}");
    }

    #[test]
    fn snapping_rounds_to_the_step() {
        let snapped = snap_angle(20f64.to_radians(), SNAP_STEP_DEG);
        assert!((snapped - 15f64.to_radians()).abs() < 1e-12);
        let snapped = snap_angle(-23f64.to_radians(), SNAP_STEP_DEG);
        assert!((snapped + 30f64.to_radians()).abs() < 1e-12);
    }
}
