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
pub struct LocalProjection {
    lat0: f64,
    lon0: f64,
    cos_lat0: f64,
}

impl LocalProjection {
    pub fn new(origin: LatLon) -> Self {
        Self {
            lat0: origin.0,
            lon0: origin.1,
            cos_lat0: origin.0.to_radians().cos(),
        }
    }

    pub fn project(&self, p: LatLon) -> (f64, f64) {
        (
            wrap_delta_lon(p.1 - self.lon0).to_radians() * self.cos_lat0 * EARTH_RADIUS_M,
            (p.0 - self.lat0).to_radians() * EARTH_RADIUS_M,
        )
    }

    pub fn unproject(&self, xy: (f64, f64)) -> LatLon {
        (
            self.lat0 + (xy.1 / EARTH_RADIUS_M).to_degrees(),
            wrap_lon(self.lon0 + (xy.0 / (EARTH_RADIUS_M * self.cos_lat0)).to_degrees()),
        )
    }
}

/// Fold a longitude difference into [-180, 180] so two points either side of
/// the antimeridian are a short hop apart rather than almost 360 degrees.
/// Differences already in range are returned untouched.
fn wrap_delta_lon(d: f64) -> f64 {
    if (-180.0..=180.0).contains(&d) {
        d
    } else {
        d - 360.0 * (d / 360.0).round()
    }
}

/// Wrap a longitude back into [-180, 180]; values already in range are
/// returned untouched.
fn wrap_lon(lon: f64) -> f64 {
    wrap_delta_lon(lon)
}

/// Mean position of `points`. Longitudes are averaged relative to the first
/// point, so a set straddling the antimeridian gives a centre near it (not near
/// longitude 0). Ordinary data gives the plain arithmetic mean.
pub fn mean_point(points: &[LatLon]) -> LatLon {
    let n = points.len() as f64;
    let lat = points.iter().map(|p| p.0).sum::<f64>() / n;
    let Some(first) = points.first() else {
        return (lat, points.iter().map(|p| p.1).sum::<f64>() / n);
    };
    let ref_lon = first.1;
    let spread: f64 = points
        .iter()
        .map(|p| {
            let d = p.1 - ref_lon;
            if d.abs() > 180.0 {
                wrap_delta_lon(d)
            } else {
                d
            }
        })
        .sum::<f64>();
    let lon = if points.iter().any(|p| (p.1 - ref_lon).abs() > 180.0) {
        wrap_lon(ref_lon + spread / n)
    } else {
        points.iter().map(|p| p.1).sum::<f64>() / n
    };
    (lat, lon)
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

/// The most an interior node may move when straightening, as a fraction of
/// the way's length (the sum of its segment lengths). A way that bends more
/// than this is a genuine curve, not a wobbly straight line, and is refused.
/// A quarter-circle arc deviates by about 19% of its length and a 20 degree
/// bend in the middle of a line by about 9%, so this keeps the second and
/// rejects the first.
pub const STRAIGHTEN_MAX_DEVIATION: f64 = 0.10;

/// Why [`straighten`] refused a way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StraightenError {
    /// Not an open way with at least three nodes; nothing to straighten.
    NotApplicable,
    /// The first and last node are in the same place, so there is no line.
    NoLine,
    /// An interior node is further from the line than
    /// [`STRAIGHTEN_MAX_DEVIATION`] allows.
    TooCurved,
}

/// How far (in metres) a vertex may sit off the simplified line before Simplify
/// Way keeps it. JOSM's default for `simplify-way.max-error` is 3 m.
pub const SIMPLIFY_TOLERANCE_M: f64 = 3.0;

/// Which vertices of a polyline survive Douglas-Peucker simplification with
/// `tolerance_m` metres of allowed error. `pinned[i]` forces vertex `i` to be
/// kept (junctions, tagged nodes, ...); the first and last vertex are always
/// kept. Distances are measured in the local metric plane around the first
/// point, so ways across the antimeridian work. A closed ring (first and last
/// the same place) is handled by treating the distance to a zero-length
/// segment as the distance to its point.
///
/// Returns one flag per input point. `pinned` should have the same length.
pub fn simplify_keep(points: &[LatLon], pinned: &[bool], tolerance_m: f64) -> Vec<bool> {
    let n = points.len();
    let mut keep: Vec<bool> = (0..n)
        .map(|i| i == 0 || i + 1 == n || pinned.get(i).copied().unwrap_or(false))
        .collect();
    if n < 3 {
        return keep;
    }
    let proj = LocalProjection::new(points[0]);
    let xy: Vec<(f64, f64)> = points.iter().map(|&p| proj.project(p)).collect();
    let anchors: Vec<usize> = (0..n).filter(|&i| keep[i]).collect();
    let mut stack: Vec<(usize, usize)> = anchors.windows(2).map(|w| (w[0], w[1])).collect();
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (mut far, mut far_d) = (a, -1.0f64);
        for (i, &p) in xy.iter().enumerate().take(b).skip(a + 1) {
            let d = point_segment_distance(p, xy[a], xy[b]);
            if d > far_d {
                far = i;
                far_d = d;
            }
        }
        if far_d > tolerance_m {
            keep[far] = true;
            stack.push((a, far));
            stack.push((far, b));
        }
    }
    keep
}

/// Distance from `p` to the segment `a`-`b` (to `a` if the segment has no
/// length).
fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// Whether [`straighten`] applies to a way of this shape: open, with at least
/// three nodes.
pub fn can_straighten(points: &[LatLon]) -> bool {
    points.len() >= 3 && points.first() != points.last()
}

/// Straighten an open way like iD: project every interior node onto the line
/// from the first to the last node, in the local metric plane. The end nodes
/// stay put and the interior nodes keep their order along the line (a node
/// that would land before its predecessor, or past the end, is clamped there).
///
/// Returns the new position of every input point (same length and order).
/// Refuses ways that are not open with 3+ nodes, whose ends coincide, or
/// where any interior node would move more than [`STRAIGHTEN_MAX_DEVIATION`]
/// of the way's length.
pub fn straighten(points: &[LatLon]) -> Result<Vec<LatLon>, StraightenError> {
    if !can_straighten(points) {
        return Err(StraightenError::NotApplicable);
    }
    let proj = LocalProjection::new(mean_point(points));
    let xy: Vec<(f64, f64)> = points.iter().map(|&p| proj.project(p)).collect();
    let (start, end) = (xy[0], xy[xy.len() - 1]);
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let chord_sq = dx * dx + dy * dy;
    if chord_sq < 1e-12 {
        return Err(StraightenError::NoLine);
    }

    let length: f64 = xy
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
        .sum();
    let chord = chord_sq.sqrt();
    let mut out = vec![points[0]];
    let mut previous_t = 0.0_f64;
    for (&p, &original) in xy[1..xy.len() - 1].iter().zip(&points[1..]) {
        let t = ((p.0 - start.0) * dx + (p.1 - start.1) * dy) / chord_sq;
        let deviation = ((p.0 - start.0) * dy - (p.1 - start.1) * dx).abs() / chord;
        if deviation > STRAIGHTEN_MAX_DEVIATION * length {
            return Err(StraightenError::TooCurved);
        }
        let t = t.clamp(previous_t, 1.0);
        previous_t = t;
        let target = (start.0 + t * dx, start.1 + t * dy);
        // A node already on the line (within 0.1 mm) keeps its exact
        // coordinates, so straightening a straight way changes nothing.
        out.push(if (target.0 - p.0).hypot(target.1 - p.1) < 1e-4 {
            original
        } else {
            proj.unproject(target)
        });
    }
    out.push(points[points.len() - 1]);
    Ok(out)
}

/// A copy of `items` in reverse order.
pub fn reversed<T: Clone>(items: &[T]) -> Vec<T> {
    items.iter().rev().cloned().collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn projection_handles_longitudes_either_side_of_the_antimeridian() {
        let pts = [(10.0, 179.9995), (10.0, -179.9995), (10.0, 180.0)];
        let centre = mean_point(&pts);
        assert!(
            (centre.1.abs() - 180.0).abs() < 0.001,
            "centre {centre:?} should sit near 180"
        );
        let proj = LocalProjection::new(centre);
        for &p in &pts {
            let xy = proj.project(p);
            assert!(xy.0.abs() < 100.0, "{p:?} projected to {xy:?}");
            let back = proj.unproject(xy);
            assert!((back.0 - p.0).abs() < 1e-9);
            let dlon = (back.1 - p.1).rem_euclid(360.0);
            assert!(
                dlon < 1e-9 || 360.0 - dlon < 1e-9,
                "{p:?} came back {back:?}"
            );
            assert!((-180.0..=180.0).contains(&back.1));
        }
        // Ordinary data is the plain mean.
        assert_eq!(mean_point(&[(1.0, 10.0), (3.0, 20.0)]), (2.0, 15.0));
    }

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

    /// A line from the origin going east for `length` metres, with the
    /// interior nodes at `(along, across)` metres (across = north of the line).
    fn line_with(length: f64, interior: &[(f64, f64)]) -> Vec<LatLon> {
        let proj = LocalProjection::new((42.36, -71.06));
        let mut xy = vec![(0.0, 0.0)];
        xy.extend_from_slice(interior);
        xy.push((length, 0.0));
        xy.iter().map(|&v| proj.unproject(v)).collect()
    }

    fn across(points: &[LatLon], i: usize) -> f64 {
        let proj = LocalProjection::new(points[0]);
        proj.project(points[i]).1
    }

    #[test]
    fn straightening_projects_interior_nodes_onto_the_end_to_end_line() {
        let wobbly = line_with(100.0, &[(25.0, 3.0), (50.0, -4.0), (75.0, 2.0)]);
        let out = straighten(&wobbly).unwrap();
        assert_eq!(out.len(), wobbly.len());
        // Ends stay exactly where they were.
        assert_eq!(out[0], wobbly[0]);
        assert_eq!(out[4], wobbly[4]);
        let proj = LocalProjection::new(wobbly[0]);
        for (i, along) in [(1, 25.0), (2, 50.0), (3, 75.0)] {
            assert!(across(&out, i).abs() < 1e-4, "node {i} off the line");
            let x = proj.project(out[i]).0;
            assert!((x - along).abs() < 1e-3, "node {i} moved along the line");
        }
    }

    #[test]
    fn straightening_works_on_a_diagonal_line() {
        let proj = LocalProjection::new((42.36, -71.06));
        let pts: Vec<LatLon> = [(0.0, 0.0), (30.0, 40.0), (100.0, 100.0), (120.0, 160.0)]
            .iter()
            .map(|&v| proj.unproject(v))
            .collect();
        let out = straighten(&pts).unwrap();
        // Both interior nodes end up on the line (0,0)-(120,160).
        for p in &out[1..3] {
            let (x, y) = proj.project(*p);
            assert!((x * 160.0 - y * 120.0).abs() < 1e-3);
        }
    }

    #[test]
    fn straightening_a_straight_way_changes_nothing() {
        let pts = line_with(90.0, &[(30.0, 0.0), (60.0, 0.0)]);
        assert_eq!(straighten(&pts).unwrap(), pts);
    }

    #[test]
    fn straightening_refuses_a_curved_way_but_accepts_a_slight_bend() {
        // A quarter circle of radius 100 m: deviation ~19% of its length.
        let arc: Vec<(f64, f64)> = (1..8)
            .map(|i| {
                let a = std::f64::consts::FRAC_PI_2 * f64::from(i) / 8.0;
                (100.0 * a.sin(), 100.0 * (1.0 - a.cos()))
            })
            .collect();
        let quarter = {
            let proj = LocalProjection::new((42.36, -71.06));
            let mut v = vec![proj.unproject((0.0, 0.0))];
            v.extend(arc.iter().map(|&p| proj.unproject(p)));
            v.push(proj.unproject((100.0, 100.0)));
            v
        };
        assert_eq!(straighten(&quarter), Err(StraightenError::TooCurved));

        // A 15 m kink in the middle of a 100 m line is under 10% of the length.
        let kinked = line_with(100.0, &[(50.0, 8.0)]);
        assert!(straighten(&kinked).is_ok());
        // 25 m is not.
        let bent = line_with(100.0, &[(50.0, 25.0)]);
        assert_eq!(straighten(&bent), Err(StraightenError::TooCurved));
    }

    #[test]
    fn straightening_keeps_nodes_in_order_and_inside_the_ends() {
        // The third node sits behind the second and the last interior node
        // is beyond the end: both get clamped, never reordered.
        let pts = line_with(100.0, &[(60.0, 2.0), (40.0, -2.0), (110.0, 1.0)]);
        let out = straighten(&pts).unwrap();
        let proj = LocalProjection::new(out[0]);
        let xs: Vec<f64> = out.iter().map(|&p| proj.project(p).0).collect();
        assert!(xs.windows(2).all(|w| w[0] <= w[1] + 1e-9), "{xs:?}");
        assert!(xs[3] <= 100.0 + 1e-6 && xs[3] >= 99.0);
    }

    #[test]
    fn straightening_rejects_closed_short_and_degenerate_ways() {
        let ring = polygon(5, 0.0);
        assert!(!can_straighten(&ring));
        assert_eq!(straighten(&ring), Err(StraightenError::NotApplicable));
        let two = line_with(50.0, &[]);
        assert_eq!(straighten(&two), Err(StraightenError::NotApplicable));
        // Same exact spot at both ends is a ring, in effect.
        let p = (42.36, -71.06);
        let q = (42.3601, -71.0601);
        assert_eq!(straighten(&[p, q, p]), Err(StraightenError::NotApplicable));
        // Different nodes a hair apart: no line to project onto.
        let p2 = (42.36 + 1e-12, -71.06);
        assert_eq!(straighten(&[p, q, p2]), Err(StraightenError::NoLine));
    }

    #[test]
    fn reversing_twice_is_identity() {
        let v = vec![1_i64, 2, 3, 4, 5];
        assert_eq!(reversed(&v), vec![5, 4, 3, 2, 1]);
        assert_eq!(reversed(&reversed(&v)), v);
        assert!(reversed::<i64>(&[]).is_empty());
    }
}
