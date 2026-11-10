//! Pure geometry for the one-shot selection actions (square, circularize,
//! reverse). Everything here works on `(lat, lon)` degree pairs but does its
//! maths in a locally projected metric plane, so angles and radii mean what
//! they look like on the ground rather than in raw degrees.

const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// A (lat, lon) pair in degrees.
pub type LatLon = (f64, f64);

/// Corners within this many degrees of a right angle (or of straight) are
/// pulled onto it; corners further away than that are left alone, so a
/// building with a deliberate 45 degree chamfer isn't mangled. Same threshold
/// as iD.
const SQUARE_THRESHOLD_DEG: f64 = 13.0;

/// Squaring stops once the summed corner error drops below this.
const SQUARE_EPSILON: f64 = 1e-9;
const SQUARE_MAX_ITERATIONS: usize = 5000;

/// An equirectangular projection around a fixed origin, accurate to well
/// under a millimetre across the size of a single way.
struct LocalProjection {
    lat0: f64,
    lon0: f64,
    cos_lat0: f64,
}

impl LocalProjection {
    fn new(origin: LatLon) -> Self {
        Self {
            lat0: origin.0,
            lon0: origin.1,
            cos_lat0: origin.0.to_radians().cos(),
        }
    }

    fn project(&self, p: LatLon) -> (f64, f64) {
        (
            (p.1 - self.lon0).to_radians() * self.cos_lat0 * EARTH_RADIUS_M,
            (p.0 - self.lat0).to_radians() * EARTH_RADIUS_M,
        )
    }

    fn unproject(&self, xy: (f64, f64)) -> LatLon {
        (
            self.lat0 + (xy.1 / EARTH_RADIUS_M).to_degrees(),
            self.lon0 + (xy.0 / (EARTH_RADIUS_M * self.cos_lat0)).to_degrees(),
        )
    }
}

fn mean_point(points: &[LatLon]) -> LatLon {
    let n = points.len() as f64;
    (
        points.iter().map(|p| p.0).sum::<f64>() / n,
        points.iter().map(|p| p.1).sum::<f64>() / n,
    )
}

/// Drop the closing duplicate of a ring, if `points` ends where it starts.
fn ring_vertices(points: &[LatLon]) -> &[LatLon] {
    if points.len() > 1 && points.first() == points.last() {
        &points[..points.len() - 1]
    } else {
        points
    }
}

/// True if `points` describes a closed ring (first == last, with at least
/// three distinct vertices).
pub fn is_closed_ring(points: &[LatLon]) -> bool {
    points.len() >= 4 && points.first() == points.last()
}

/// Whether [`orthogonalize`] has enough to work with: a closed ring of at
/// least three distinct vertices, or an open line of at least three.
pub fn can_square(points: &[LatLon]) -> bool {
    is_closed_ring(points) || points.len() >= 3
}

/// Whether [`circularize`] has enough to work with.
pub fn can_circularize(points: &[LatLon]) -> bool {
    is_closed_ring(points)
}

/// Square the corners of a way, like iD's / JOSM's orthogonalize.
///
/// `points` is the way's node positions in order; for a closed way the last
/// equals the first. Returns the new position of every input point (same
/// length and order), or `None` if the shape can't be squared or no corner
/// is close enough to 90 degrees (or to straight) to be adjusted.
///
/// The algorithm is iterative: each corner is nudged along the bisector of its
/// two edges in proportion to how far it is from a right angle, and the whole
/// shape is relaxed until the corners settle. Near-straight vertices are
/// pulled straight rather than into a corner. Endpoints of an open way are
/// not corners and are left where they are.
pub fn orthogonalize(points: &[LatLon]) -> Option<Vec<LatLon>> {
    if !can_square(points) {
        return None;
    }
    let closed = is_closed_ring(points);
    let ring = if closed {
        ring_vertices(points)
    } else {
        points
    };
    let proj = LocalProjection::new(mean_point(ring));
    let mut xy: Vec<(f64, f64)> = ring.iter().map(|&p| proj.project(p)).collect();
    let n = xy.len();

    let lower = ((90.0 - SQUARE_THRESHOLD_DEG).to_radians()).cos();
    let upper = SQUARE_THRESHOLD_DEG.to_radians().cos();
    let filter = |dot: f64| {
        let a = dot.abs();
        if a < lower || a > upper {
            dot
        } else {
            0.0
        }
    };

    // The motion of vertex `i`, and its contribution to the error score.
    let motion = |xy: &[(f64, f64)], i: usize| -> Option<((f64, f64), f64)> {
        let (prev, next) = if closed {
            ((i + n - 1) % n, (i + 1) % n)
        } else if i == 0 || i == n - 1 {
            return None;
        } else {
            (i - 1, i + 1)
        };
        let b = xy[i];
        let p = (xy[prev].0 - b.0, xy[prev].1 - b.1);
        let q = (xy[next].0 - b.0, xy[next].1 - b.1);
        let (lp, lq) = (p.0.hypot(p.1), q.0.hypot(q.1));
        if lp < 1e-9 || lq < 1e-9 {
            return None;
        }
        let scale = 2.0 * lp.min(lq);
        let (p, q) = ((p.0 / lp, p.1 / lp), (q.0 / lq, q.1 / lq));
        let mut dot = filter(p.0 * q.0 + p.1 * q.1);
        // Almost-straight vertices (angle nearer 180 than 90): treat the
        // deviation from straight as the error, so they are pulled straight.
        if n > 3 && dot < -std::f64::consts::FRAC_1_SQRT_2 {
            dot += 1.0;
        }
        let sum = (p.0 + q.0, p.1 + q.1);
        let len = sum.0.hypot(sum.1);
        if len < 1e-12 {
            return None;
        }
        let k = 0.1 * dot * scale / len;
        Some(((sum.0 * k, sum.1 * k), dot.abs()))
    };

    let original = xy.clone();
    let mut score = f64::MAX;
    for _ in 0..SQUARE_MAX_ITERATIONS {
        let motions: Vec<Option<((f64, f64), f64)>> = (0..n).map(|i| motion(&xy, i)).collect();
        score = motions.iter().flatten().map(|m| m.1).sum();
        if score < SQUARE_EPSILON {
            break;
        }
        for (v, m) in xy.iter_mut().zip(&motions) {
            if let Some(((dx, dy), _)) = m {
                v.0 += dx;
                v.1 += dy;
            }
        }
    }
    if !score.is_finite() {
        return None;
    }

    let moved = xy
        .iter()
        .zip(&original)
        .any(|(a, b)| (a.0 - b.0).hypot(a.1 - b.1) > 1e-4);
    if !moved {
        return None;
    }

    let mut out: Vec<LatLon> = xy.iter().map(|&v| proj.unproject(v)).collect();
    if closed {
        out.push(out[0]);
    }
    Some(out)
}

/// Move the vertices of a closed way onto a circle around its centroid, with
/// the mean distance from the centroid as the radius. Each vertex keeps its
/// bearing from the centroid, so node order is preserved and no node is added.
/// Returns the new position of every input point (including the repeated
/// closing point), or `None` if `points` isn't a closed ring.
pub fn circularize(points: &[LatLon]) -> Option<Vec<LatLon>> {
    if !can_circularize(points) {
        return None;
    }
    let ring = ring_vertices(points);
    let proj = LocalProjection::new(mean_point(ring));
    let xy: Vec<(f64, f64)> = ring.iter().map(|&p| proj.project(p)).collect();
    let (cx, cy) = polygon_centroid(&xy);

    let radii: Vec<f64> = xy.iter().map(|v| (v.0 - cx).hypot(v.1 - cy)).collect();
    let radius = radii.iter().sum::<f64>() / radii.len() as f64;
    if radius < 1e-6 || radii.iter().any(|&r| r < 1e-9) {
        return None;
    }

    let mut out: Vec<LatLon> = xy
        .iter()
        .zip(&radii)
        .map(|(v, &r)| {
            let k = radius / r;
            proj.unproject((cx + (v.0 - cx) * k, cy + (v.1 - cy) * k))
        })
        .collect();
    out.push(out[0]);
    Some(out)
}

/// Area centroid of a simple polygon, falling back to the vertex mean when
/// the polygon is degenerate (zero area).
fn polygon_centroid(xy: &[(f64, f64)]) -> (f64, f64) {
    let n = xy.len();
    let (mut a2, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (p, q) = (xy[i], xy[(i + 1) % n]);
        let cross = p.0 * q.1 - q.0 * p.1;
        a2 += cross;
        cx += (p.0 + q.0) * cross;
        cy += (p.1 + q.1) * cross;
    }
    if a2.abs() < 1e-9 {
        let m = n as f64;
        return (
            xy.iter().map(|v| v.0).sum::<f64>() / m,
            xy.iter().map(|v| v.1).sum::<f64>() / m,
        );
    }
    (cx / (3.0 * a2), cy / (3.0 * a2))
}

/// A copy of `items` in reverse order.
pub fn reversed<T: Clone>(items: &[T]) -> Vec<T> {
    items.iter().rev().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Interior angle at `b` in degrees, measured in the local metric plane.
    fn angle_deg(a: LatLon, b: LatLon, c: LatLon) -> f64 {
        let proj = LocalProjection::new(b);
        let (a, c) = (proj.project(a), proj.project(c));
        let dot = a.0 * c.0 + a.1 * c.1;
        (dot / (a.0.hypot(a.1) * c.0.hypot(c.1)))
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees()
    }

    /// A rectangle near Boston, roughly 40 m by 20 m, with its top edge
    /// sheared so the corners are about 81 and 99 degrees.
    fn skewed_rect() -> Vec<LatLon> {
        let proj = LocalProjection::new((42.36, -71.06));
        let pts = [
            (0.0, 0.0),
            (40.0, 0.0),
            (43.0, 20.0),
            (3.0, 20.0),
            (0.0, 0.0),
        ];
        pts.iter().map(|&p| proj.unproject(p)).collect()
    }

    #[test]
    fn squaring_a_skewed_rectangle_gives_right_angles() {
        let rect = skewed_rect();
        let before = angle_deg(rect[3], rect[0], rect[1]);
        assert!((before - 90.0).abs() > 5.0, "fixture should be skewed");

        let squared = orthogonalize(&rect).expect("should square");
        assert_eq!(squared.len(), rect.len());
        assert_eq!(squared.first(), squared.last());
        for i in 0..4 {
            let a = squared[(i + 3) % 4];
            let b = squared[i];
            let c = squared[(i + 1) % 4];
            let angle = angle_deg(a, b, c);
            assert!((angle - 90.0).abs() < 0.1, "corner {i} is {angle} degrees");
        }
    }

    #[test]
    fn squaring_keeps_the_shape_roughly_in_place() {
        let rect = skewed_rect();
        let squared = orthogonalize(&rect).unwrap();
        let proj = LocalProjection::new(rect[0]);
        for (a, b) in rect.iter().zip(&squared) {
            let (a, b) = (proj.project(*a), proj.project(*b));
            assert!((a.0 - b.0).hypot(a.1 - b.1) < 8.0);
        }
    }

    #[test]
    fn squaring_an_already_square_shape_changes_nothing() {
        let proj = LocalProjection::new((10.0, 20.0));
        let sq: Vec<LatLon> = [
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ]
        .iter()
        .map(|&p| proj.unproject(p))
        .collect();
        assert!(orthogonalize(&sq).is_none());
    }

    #[test]
    fn squaring_leaves_deliberate_diagonals_alone() {
        // An equilateral-ish triangle has no corner near 90 or 180.
        let proj = LocalProjection::new((10.0, 20.0));
        let tri: Vec<LatLon> = [(0.0, 0.0), (10.0, 0.0), (5.0, 8.66), (0.0, 0.0)]
            .iter()
            .map(|&p| proj.unproject(p))
            .collect();
        assert!(orthogonalize(&tri).is_none());
    }

    #[test]
    fn squaring_an_open_way_keeps_its_endpoints() {
        let proj = LocalProjection::new((42.0, -71.0));
        // An L whose middle corner is 80 degrees.
        let line: Vec<LatLon> = [(0.0, 0.0), (30.0, 0.0), (35.0, 28.0)]
            .iter()
            .map(|&p| proj.unproject(p))
            .collect();
        let squared = orthogonalize(&line).unwrap();
        assert_eq!(squared[0], line[0]);
        assert_eq!(squared[2], line[2]);
        let angle = angle_deg(squared[0], squared[1], squared[2]);
        assert!((angle - 90.0).abs() < 0.1, "corner is {angle} degrees");
    }

    #[test]
    fn squaring_straightens_a_near_straight_vertex() {
        let proj = LocalProjection::new((42.0, -71.0));
        // A rectangle with an extra vertex on the bottom edge, nudged 0.3 m
        // off the line.
        let pts: Vec<LatLon> = [
            (0.0, 0.0),
            (20.0, 0.3),
            (40.0, 0.0),
            (40.0, 20.0),
            (0.0, 20.0),
            (0.0, 0.0),
        ]
        .iter()
        .map(|&p| proj.unproject(p))
        .collect();
        let squared = orthogonalize(&pts).unwrap();
        let angle = angle_deg(squared[0], squared[1], squared[2]);
        assert!((angle - 180.0).abs() < 0.1, "vertex is {angle} degrees");
    }

    #[test]
    fn squaring_needs_three_points() {
        assert!(orthogonalize(&[(0.0, 0.0), (1.0, 1.0)]).is_none());
        assert!(!can_square(&[(0.0, 0.0), (1.0, 1.0)]));
    }

    fn polygon(n: usize, jitter: f64) -> Vec<LatLon> {
        let proj = LocalProjection::new((42.0, -71.0));
        let mut pts: Vec<LatLon> = (0..n)
            .map(|i| {
                let t = std::f64::consts::TAU * i as f64 / n as f64;
                // Alternate radii to make the ring clearly non-circular.
                let r = 30.0 + if i % 2 == 0 { jitter } else { -jitter };
                proj.unproject((r * t.cos(), r * t.sin()))
            })
            .collect();
        pts.push(pts[0]);
        pts
    }

    #[test]
    fn circularizing_gives_equal_radii() {
        let ring = polygon(8, 8.0);
        let out = circularize(&ring).expect("should circularize");
        assert_eq!(out.len(), ring.len());
        assert_eq!(out.first(), out.last());

        let verts = &out[..out.len() - 1];
        let proj = LocalProjection::new(mean_point(verts));
        let xy: Vec<_> = verts.iter().map(|&p| proj.project(p)).collect();
        let (cx, cy) = polygon_centroid(&xy);
        let radii: Vec<f64> = xy.iter().map(|v| (v.0 - cx).hypot(v.1 - cy)).collect();
        let mean = radii.iter().sum::<f64>() / radii.len() as f64;
        for r in &radii {
            assert!((r - mean).abs() < 1e-3, "radius {r} vs mean {mean}");
        }
        assert!((mean - 30.0).abs() < 2.0, "mean radius {mean}");
    }

    #[test]
    fn circularizing_preserves_node_order() {
        let ring = polygon(8, 8.0);
        let out = circularize(&ring).unwrap();
        let centre = mean_point(&ring[..8]);
        let proj = LocalProjection::new(centre);
        let bearing = |p: LatLon| {
            let v = proj.project(p);
            v.1.atan2(v.0)
        };
        for (a, b) in ring.iter().zip(&out) {
            let d = (bearing(*a) - bearing(*b)).abs();
            assert!(d < 1e-3 || (std::f64::consts::TAU - d) < 1e-3);
        }
    }

    #[test]
    fn circularizing_rejects_open_ways() {
        let mut ring = polygon(6, 0.0);
        ring.pop();
        assert!(circularize(&ring).is_none());
        assert!(!can_circularize(&ring));
    }

    #[test]
    fn reversing_twice_is_identity() {
        let v = vec![1_i64, 2, 3, 4, 5];
        assert_eq!(reversed(&v), vec![5, 4, 3, 2, 1]);
        assert_eq!(reversed(&reversed(&v)), v);
        assert!(reversed::<i64>(&[]).is_empty());
    }
}
