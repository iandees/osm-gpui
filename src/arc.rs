//! Arc mode's geometry: the circular arc from one point to another that bends
//! through a third, sampled at a fixed angle step.
//!
//! Pure functions over a planar frame. Callers work in screen pixels (which
//! are isotropic, unlike raw lat/lon) and convert the result back to geo
//! coordinates, the way Building mode's rectangle does.

use std::f64::consts::TAU;

/// Angle between consecutive generated nodes, in degrees (JOSM's default).
pub const DEFAULT_STEP_DEGREES: f64 = 5.0;

/// Points within this distance of being collinear (or coincident) have no
/// usable circle through them.
const DEGENERATE_EPSILON: f64 = 1e-9;

/// The arc from `start` to `end` that passes through `through`, as a polyline
/// that begins exactly at `start` and ends exactly at `end`, with a vertex
/// every `step_degrees` of arc in between (the sweep is divided evenly, so the
/// step is at most `step_degrees`). `None` if the three points do not define a
/// circle (collinear, or two coincide).
pub fn arc_through(
    start: (f64, f64),
    through: (f64, f64),
    end: (f64, f64),
    step_degrees: f64,
) -> Option<Vec<(f64, f64)>> {
    let (center, radius) = circumcircle(start, through, end)?;

    let angle = |p: (f64, f64)| (p.1 - center.1).atan2(p.0 - center.0);
    let theta_start = angle(start);
    // Counter-clockwise sweeps (in the math sense) from `start`.
    let ccw = |p: (f64, f64)| (angle(p) - theta_start).rem_euclid(TAU);
    let sweep_through = ccw(through);
    let sweep_end = ccw(end);

    // Whichever way round reaches `through` before `end` is the arc we want.
    let signed_sweep = if sweep_through < sweep_end {
        sweep_end
    } else {
        sweep_end - TAU
    };

    let step = step_degrees.max(0.1).to_radians();
    // The epsilon keeps an exact multiple (180 / 5) from rounding up a step.
    let segments = ((signed_sweep.abs() / step) - 1e-9).ceil().max(2.0) as usize;

    let mut points = Vec::with_capacity(segments + 1);
    points.push(start);
    for i in 1..segments {
        let theta = theta_start + signed_sweep * (i as f64 / segments as f64);
        points.push((
            center.0 + radius * theta.cos(),
            center.1 + radius * theta.sin(),
        ));
    }
    points.push(end);
    Some(points)
}

/// Center and radius of the circle through three points, or `None` if they
/// are collinear or coincident.
fn circumcircle(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> Option<((f64, f64), f64)> {
    let d = 2.0 * (a.0 * (b.1 - c.1) + b.0 * (c.1 - a.1) + c.0 * (a.1 - b.1));
    // Scale the tolerance by the points' extent so it holds at any zoom.
    let extent = [a, b, c]
        .iter()
        .flat_map(|p| [p.0.abs(), p.1.abs()])
        .fold(1.0_f64, f64::max);
    if d.abs() <= DEGENERATE_EPSILON * extent * extent {
        return None;
    }
    let (a2, b2, c2) = (
        a.0 * a.0 + a.1 * a.1,
        b.0 * b.0 + b.1 * b.1,
        c.0 * c.0 + c.1 * c.1,
    );
    let ux = (a2 * (b.1 - c.1) + b2 * (c.1 - a.1) + c2 * (a.1 - b.1)) / d;
    let uy = (a2 * (c.0 - b.0) + b2 * (a.0 - c.0) + c2 * (b.0 - a.0)) / d;
    let radius = ((a.0 - ux).powi(2) + (a.1 - uy).powi(2)).sqrt();
    Some(((ux, uy), radius))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
    }

    #[test]
    fn semicircle_runs_through_the_bulge_point() {
        // Unit circle at the origin: (1,0) -> (0,1) -> (-1,0) is the upper half.
        let pts = arc_through((1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), 5.0).unwrap();
        assert_eq!(pts.first(), Some(&(1.0, 0.0)));
        assert_eq!(pts.last(), Some(&(-1.0, 0.0)));
        // 180 degrees at <=5 degrees per step.
        assert_eq!(pts.len(), 37);
        assert!(pts
            .iter()
            .all(|p| (dist(*p, (0.0, 0.0)) - 1.0).abs() < 1e-9));
        // The upper half only: nothing dips below the chord.
        assert!(pts.iter().all(|p| p.1 >= -1e-9));
        // Even sweep means the midpoint vertex is the bulge point.
        assert!(dist(pts[18], (0.0, 1.0)) < 1e-9);
    }

    #[test]
    fn bulging_the_other_way_takes_the_other_arc() {
        let pts = arc_through((1.0, 0.0), (0.0, -1.0), (-1.0, 0.0), 5.0).unwrap();
        assert!(pts.iter().all(|p| p.1 <= 1e-9));
    }

    #[test]
    fn reflex_arc_is_the_long_way_round() {
        // Start and end are close together; the bulge is on the far side, so
        // the arc is more than half the circle.
        let a = (1.0, 0.0);
        let end = (0.0, 1.0);
        let through = (-1.0, 0.0);
        let pts = arc_through(a, through, end, 5.0).unwrap();
        // 270 degrees at <=5 degrees per step.
        assert_eq!(pts.len(), 55);
        assert!(pts.iter().any(|p| dist(*p, through) < 1e-9));
    }

    #[test]
    fn step_never_exceeds_the_requested_angle() {
        let pts = arc_through((1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), 7.0).unwrap();
        let max_step = pts
            .windows(2)
            .map(|w| 2.0 * (dist(w[0], w[1]) / 2.0).asin())
            .fold(0.0, f64::max);
        assert!(max_step <= 7.0_f64.to_radians() + 1e-9);
    }

    #[test]
    fn tiny_arc_still_gets_a_middle_node() {
        let pts = arc_through((0.0, 0.0), (1.0, 0.01), (2.0, 0.0), 5.0).unwrap();
        assert!(pts.len() >= 3);
    }

    #[test]
    fn collinear_points_have_no_arc() {
        assert!(arc_through((0.0, 0.0), (1.0, 1.0), (2.0, 2.0), 5.0).is_none());
    }

    #[test]
    fn coincident_points_have_no_arc() {
        assert!(arc_through((3.0, 4.0), (3.0, 4.0), (9.0, 1.0), 5.0).is_none());
    }

    #[test]
    fn works_at_screen_pixel_magnitudes() {
        let pts = arc_through((800.0, 600.0), (900.0, 500.0), (1000.0, 600.0), 5.0).unwrap();
        assert_eq!(pts.first(), Some(&(800.0, 600.0)));
        assert_eq!(pts.last(), Some(&(1000.0, 600.0)));
    }
}
