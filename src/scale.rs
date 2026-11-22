//! Scale a group of nodes about their centre. Pure geometry in the same local
//! metric plane Rotate uses, so shapes keep their proportions on the ground;
//! the interactive session lives in the binary crate.

use crate::geometry::{LatLon, LocalProjection};

/// Smallest and largest factor a scale session reaches. Zero would collapse
/// the selection onto one point, which no undo-free cancel could recover.
pub const MIN_FACTOR: f64 = 0.01;
pub const MAX_FACTOR: f64 = 100.0;

/// The factor step scaling snaps to while Shift is held.
pub const SNAP_STEP: f64 = 0.05;

/// The pointer distance (pixels) below which a session's start distance is
/// treated as this much, so a pointer that begins on the centre does not make
/// every later move a huge factor.
const MIN_START_PX: f64 = 12.0;

/// `points` scaled by `factor` about `center`. A factor of one returns the
/// points untouched, bit for bit.
pub fn scale_points(points: &[LatLon], center: LatLon, factor: f64) -> Vec<LatLon> {
    if factor == 1.0 || !factor.is_finite() {
        return points.to_vec();
    }
    let projection = LocalProjection::new(center);
    points
        .iter()
        .map(|&p| {
            let (x, y) = projection.project(p);
            projection.unproject((x * factor, y * factor))
        })
        .collect()
}

/// Distance (pixels) of the pointer from the centre, both in screen pixels.
pub fn pointer_distance(center_px: (f64, f64), pointer_px: (f64, f64)) -> f64 {
    (pointer_px.0 - center_px.0).hypot(pointer_px.1 - center_px.1)
}

/// The scale factor for a pointer that was `start` pixels from the centre and
/// is now `now` pixels from it, clamped to the supported range.
pub fn factor_for_pointer(start: f64, now: f64) -> f64 {
    (now / start.max(MIN_START_PX)).clamp(MIN_FACTOR, MAX_FACTOR)
}

/// `factor` rounded to the nearest multiple of [`SNAP_STEP`], never below it.
pub fn snap_factor(factor: f64) -> f64 {
    ((factor / SNAP_STEP).round() * SNAP_STEP).max(SNAP_STEP)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts() -> Vec<LatLon> {
        vec![
            LatLon::new(47.0, 8.0),
            LatLon::new(47.001, 8.0),
            LatLon::new(47.001, 8.002),
        ]
    }

    fn centre(points: &[LatLon]) -> LatLon {
        crate::rotate::center(points).unwrap()
    }

    #[test]
    fn factor_one_is_the_identity() {
        let p = pts();
        assert_eq!(scale_points(&p, centre(&p), 1.0), p);
    }

    #[test]
    fn scaling_multiplies_distances_from_the_centre() {
        let p = pts();
        let c = centre(&p);
        let out = scale_points(&p, c, 2.0);
        let proj = LocalProjection::new(c);
        for (a, b) in p.iter().zip(&out) {
            let (ax, ay) = proj.project(*a);
            let (bx, by) = proj.project(*b);
            assert!((bx - 2.0 * ax).abs() < 1e-6 && (by - 2.0 * ay).abs() < 1e-6);
        }
    }

    #[test]
    fn scaling_back_restores_the_points() {
        let p = pts();
        let c = centre(&p);
        let back = scale_points(&scale_points(&p, c, 3.0), c, 1.0 / 3.0);
        for (a, b) in p.iter().zip(&back) {
            assert!((a.lat - b.lat).abs() < 1e-9 && (a.lon - b.lon).abs() < 1e-9);
        }
    }

    #[test]
    fn pointer_factor_is_the_distance_ratio_and_clamped() {
        assert!((factor_for_pointer(100.0, 150.0) - 1.5).abs() < 1e-12);
        assert_eq!(factor_for_pointer(100.0, 0.0), MIN_FACTOR);
        assert_eq!(factor_for_pointer(100.0, 1e9), MAX_FACTOR);
        // A start on the centre does not blow up.
        assert!((factor_for_pointer(0.0, 24.0) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn snapping_rounds_to_steps_and_keeps_a_floor() {
        assert!((snap_factor(1.37) - 1.35).abs() < 1e-9);
        assert!((snap_factor(0.001) - SNAP_STEP).abs() < 1e-9);
    }
}
