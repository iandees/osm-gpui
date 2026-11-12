//! Where to put a label inside a polygon.
//!
//! * [`polylabel`]: the pole of inaccessibility (the interior point farthest
//!   from the boundary; Mapbox's `polylabel` algorithm), for polygons with
//!   holes. Unlike a centroid it always lies inside, and in a concave shape
//!   it picks the fattest part.
//! * [`rect_inside`]: whether an axis-aligned rectangle lies wholly inside a
//!   polygon, which is the exact "does this text fit" test.
//! * [`clip_ring`]: Sutherland-Hodgman clipping of a ring to a rectangle, so
//!   a polygon bigger than the screen can be labelled at the pole of its
//!   visible part.
//!
//! Everything works on plain `(x, y)` pairs in any planar unit (the callers
//! use Web Mercator metres). A polygon is a slice of rings: the outer ring
//! first, then holes. A ring may or may not repeat its first point at the end.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

pub type Pt = (f64, f64);

/// The label point of a polygon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pole {
    pub x: f64,
    pub y: f64,
    /// Distance from `(x, y)` to the nearest edge (largest inscribed circle's
    /// radius). Not positive for a degenerate polygon.
    pub dist: f64,
}

/// A ring without its closing duplicate point.
fn open(ring: &[Pt]) -> &[Pt] {
    if ring.len() > 1 && ring.first() == ring.last() {
        &ring[..ring.len() - 1]
    } else {
        ring
    }
}

fn edges(ring: &[Pt]) -> impl Iterator<Item = (Pt, Pt)> + '_ {
    let r = open(ring);
    let n = r.len();
    (0..n).map(move |i| (r[i], r[(i + 1) % n]))
}

fn seg_dist2(p: Pt, a: Pt, b: Pt) -> f64 {
    let (mut x, mut y) = a;
    let (dx, dy) = (b.0 - x, b.1 - y);
    if dx != 0.0 || dy != 0.0 {
        let t = ((p.0 - x) * dx + (p.1 - y) * dy) / (dx * dx + dy * dy);
        if t > 1.0 {
            x = b.0;
            y = b.1;
        } else if t > 0.0 {
            x += dx * t;
            y += dy * t;
        }
    }
    let (ex, ey) = (p.0 - x, p.1 - y);
    ex * ex + ey * ey
}

/// Distance from `(x, y)` to the polygon's boundary: positive inside (even-odd
/// rule, so holes count as outside), negative outside.
pub fn signed_distance(rings: &[&[Pt]], x: f64, y: f64) -> f64 {
    let mut inside = false;
    let mut min2 = f64::INFINITY;
    for ring in rings {
        for (a, b) in edges(ring) {
            if (a.1 > y) != (b.1 > y) && x < (b.0 - a.0) * (y - a.1) / (b.1 - a.1) + a.0 {
                inside = !inside;
            }
            min2 = min2.min(seg_dist2((x, y), a, b));
        }
    }
    if min2.is_infinite() {
        return f64::NEG_INFINITY;
    }
    let d = min2.sqrt();
    if inside {
        d
    } else {
        -d
    }
}

#[derive(Clone, Copy)]
struct Cell {
    x: f64,
    y: f64,
    h: f64,
    d: f64,
    /// Upper bound on the distance achievable anywhere in the cell.
    max: f64,
}

impl Cell {
    fn new(x: f64, y: f64, h: f64, rings: &[&[Pt]]) -> Self {
        let d = signed_distance(rings, x, y);
        Cell {
            x,
            y,
            h,
            d,
            max: d + h * std::f64::consts::SQRT_2,
        }
    }
}

impl PartialEq for Cell {
    fn eq(&self, o: &Self) -> bool {
        self.max == o.max
    }
}
impl Eq for Cell {}
impl PartialOrd for Cell {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Cell {
    fn cmp(&self, o: &Self) -> Ordering {
        self.max.total_cmp(&o.max)
    }
}

/// Area-weighted centroid of a ring (the vertex mean when it has no area).
fn centroid(ring: &[Pt]) -> Pt {
    let (mut a2, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for (p, q) in edges(ring) {
        let f = p.0 * q.1 - q.0 * p.1;
        a2 += f;
        cx += (p.0 + q.0) * f;
        cy += (p.1 + q.1) * f;
    }
    if a2.abs() < 1e-12 {
        let r = open(ring);
        let n = r.len().max(1) as f64;
        return (
            r.iter().map(|p| p.0).sum::<f64>() / n,
            r.iter().map(|p| p.1).sum::<f64>() / n,
        );
    }
    (cx / (3.0 * a2), cy / (3.0 * a2))
}

/// Bounding box `(min_x, min_y, max_x, max_y)` of the outer ring.
pub fn bbox(ring: &[Pt]) -> Option<(f64, f64, f64, f64)> {
    let mut it = ring.iter();
    let &(x0, y0) = it.next()?;
    let mut b = (x0, y0, x0, y0);
    for &(x, y) in it {
        b.0 = b.0.min(x);
        b.1 = b.1.min(y);
        b.2 = b.2.max(x);
        b.3 = b.3.max(y);
    }
    Some(b)
}

/// The pole of inaccessibility of the polygon `rings` (outer ring first),
/// found to within `precision` of the true optimum. `None` for an empty
/// polygon. A degenerate polygon (a line, a point) returns a pole whose
/// `dist` is not positive; callers should check it.
pub fn polylabel(rings: &[&[Pt]], precision: f64) -> Option<Pole> {
    let outer = rings.first()?;
    let (x0, y0, x1, y1) = bbox(open(outer))?;
    let (w, h) = (x1 - x0, y1 - y0);
    let size = w.min(h);
    if size <= 0.0 {
        return Some(Pole {
            x: x0,
            y: y0,
            dist: 0.0,
        });
    }
    let precision = precision.max(size * 1e-4);
    let half = size / 2.0;
    let mut heap = BinaryHeap::new();
    let mut x = x0;
    while x < x1 {
        let mut y = y0;
        while y < y1 {
            heap.push(Cell::new(x + half, y + half, half, rings));
            y += size;
        }
        x += size;
    }
    let c = centroid(open(outer));
    let mut best = Cell::new(c.0, c.1, 0.0, rings);
    let mid = Cell::new(x0 + w / 2.0, y0 + h / 2.0, 0.0, rings);
    if mid.d > best.d {
        best = mid;
    }
    while let Some(cell) = heap.pop() {
        if cell.d > best.d {
            best = cell;
        }
        if cell.max - best.d <= precision {
            continue;
        }
        let hh = cell.h / 2.0;
        for (dx, dy) in [(-hh, -hh), (hh, -hh), (-hh, hh), (hh, hh)] {
            heap.push(Cell::new(cell.x + dx, cell.y + dy, hh, rings));
        }
    }
    Some(Pole {
        x: best.x,
        y: best.y,
        dist: best.d,
    })
}

/// Whether the closed segment `a`-`b` has a point in the open rectangle.
fn segment_touches_rect(a: Pt, b: Pt, r: (f64, f64, f64, f64)) -> bool {
    let (x0, y0, x1, y1) = r;
    // Liang-Barsky against the open rectangle.
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, a.0 - x0),
        (dx, x1 - a.0),
        (-dy, a.1 - y0),
        (dy, y1 - a.1),
    ] {
        if p == 0.0 {
            if q <= 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                if t > t1 {
                    return false;
                }
                t0 = t0.max(t);
            } else {
                if t < t0 {
                    return false;
                }
                t1 = t1.min(t);
            }
        }
    }
    // A segment that only grazes the boundary has t0 == t1 on the edge; that
    // does not enter the open interior.
    t0 < t1 || {
        let (px, py) = (a.0 + dx * t0, a.1 + dy * t0);
        px > x0 && px < x1 && py > y0 && py < y1
    }
}

/// Whether the rectangle `(x0, y0, x1, y1)` lies wholly inside the polygon:
/// its centre is inside and no edge of any ring (a hole's included) enters it.
pub fn rect_inside(rings: &[&[Pt]], r: (f64, f64, f64, f64)) -> bool {
    let (cx, cy) = ((r.0 + r.2) / 2.0, (r.1 + r.3) / 2.0);
    // Cheap bbox rejection ahead of the O(edges) walk.
    if let Some(b) = rings.first().and_then(|o| bbox(open(o))) {
        if r.0 < b.0 || r.1 < b.1 || r.2 > b.2 || r.3 > b.3 {
            return false;
        }
    }
    for ring in rings {
        for (a, b) in edges(ring) {
            if segment_touches_rect(a, b, r) {
                return false;
            }
        }
    }
    signed_distance(rings, cx, cy) > 0.0
}

/// Clip `ring` to the rectangle `(x0, y0, x1, y1)` (Sutherland-Hodgman).
/// Returns an open ring (no repeated end point), empty when nothing is left.
/// For a concave ring the result can have zero-width "bridges" along the
/// rectangle's edge; they lie on the boundary of the clip window, which is
/// harmless for finding a label point inside it.
pub fn clip_ring(ring: &[Pt], r: (f64, f64, f64, f64)) -> Vec<Pt> {
    let mut pts: Vec<Pt> = open(ring).to_vec();
    // Each clip edge: is-inside test and the intersection with the edge line.
    type Inside = fn(Pt, (f64, f64, f64, f64)) -> bool;
    type Cut = fn(Pt, Pt, (f64, f64, f64, f64)) -> Pt;
    let steps: [(Inside, Cut); 4] = [
        (
            |p, r| p.0 >= r.0,
            |a, b, r| (r.0, a.1 + (b.1 - a.1) * (r.0 - a.0) / (b.0 - a.0)),
        ),
        (
            |p, r| p.0 <= r.2,
            |a, b, r| (r.2, a.1 + (b.1 - a.1) * (r.2 - a.0) / (b.0 - a.0)),
        ),
        (
            |p, r| p.1 >= r.1,
            |a, b, r| (a.0 + (b.0 - a.0) * (r.1 - a.1) / (b.1 - a.1), r.1),
        ),
        (
            |p, r| p.1 <= r.3,
            |a, b, r| (a.0 + (b.0 - a.0) * (r.3 - a.1) / (b.1 - a.1), r.3),
        ),
    ];
    for (inside, cut) in steps {
        if pts.is_empty() {
            break;
        }
        let mut out = Vec::with_capacity(pts.len() + 4);
        let mut prev = *pts.last().unwrap();
        for &cur in &pts {
            match (inside(prev, r), inside(cur, r)) {
                (true, true) => out.push(cur),
                (true, false) => out.push(cut(prev, cur, r)),
                (false, true) => {
                    out.push(cut(prev, cur, r));
                    out.push(cur);
                }
                (false, false) => {}
            }
            prev = cur;
        }
        pts = out;
    }
    pts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<Pt> {
        vec![(x, y), (x + s, y), (x + s, y + s), (x, y + s)]
    }

    #[test]
    fn square_pole_is_the_centre() {
        let sq = square(0.0, 0.0, 10.0);
        let p = polylabel(&[&sq], 0.01).unwrap();
        assert!(
            (p.x - 5.0).abs() < 0.05 && (p.y - 5.0).abs() < 0.05,
            "{p:?}"
        );
        assert!((p.dist - 5.0).abs() < 0.05);
    }

    #[test]
    fn closed_and_open_rings_agree() {
        let open_ring = square(0.0, 0.0, 10.0);
        let mut closed = open_ring.clone();
        closed.push(closed[0]);
        let a = polylabel(&[&open_ring], 0.01).unwrap();
        let b = polylabel(&[&closed], 0.01).unwrap();
        assert!((a.dist - b.dist).abs() < 0.05);
    }

    #[test]
    fn concave_c_shape_pole_is_inside_the_thick_part() {
        // A "C": thick vertical bar on the left (x 0..4), thin arms right.
        let c = vec![
            (0.0, 0.0),
            (20.0, 0.0),
            (20.0, 3.0),
            (4.0, 3.0),
            (4.0, 17.0),
            (20.0, 17.0),
            (20.0, 20.0),
            (0.0, 20.0),
        ];
        let p = polylabel(&[&c], 0.01).unwrap();
        // The centroid of this shape is (about) x=7.4, y=10, which is in the
        // notch (outside); the pole must be inside, in the spine.
        assert!(signed_distance(&[&c], p.x, p.y) > 0.0);
        assert!(p.dist > 1.9, "{p:?}");
        assert!(p.x < 4.5, "pole in the spine, got {p:?}");
        let cen = centroid(&c);
        assert!(
            signed_distance(&[&c], cen.0, cen.1) < 0.0,
            "centroid {cen:?} is outside"
        );
    }

    #[test]
    fn a_hole_pushes_the_pole_out_of_the_middle() {
        let outer = square(0.0, 0.0, 100.0);
        let hole = square(30.0, 30.0, 40.0);
        let p = polylabel(&[&outer, &hole], 0.1).unwrap();
        assert!(signed_distance(&[&outer, &hole], p.x, p.y) > 0.0);
        // The roomiest spot is a corner of the frame, where the distance to
        // the outer edge x equals the distance to the hole's corner,
        // sqrt(2) * (30 - x): x = 30 * sqrt2 / (1 + sqrt2) = 17.57.
        assert!((p.dist - 17.57).abs() < 0.3, "{p:?}");
        let in_hole = p.x > 30.0 && p.x < 70.0 && p.y > 30.0 && p.y < 70.0;
        assert!(!in_hole);
    }

    #[test]
    fn degenerate_polygons_do_not_panic_and_report_no_room() {
        assert!(polylabel(&[], 1.0).is_none());
        let line = vec![(0.0, 0.0), (10.0, 0.0), (5.0, 0.0)];
        let p = polylabel(&[&line], 0.1).unwrap();
        assert!(p.dist <= 0.0);
        let dot = vec![(1.0, 1.0)];
        assert!(polylabel(&[&dot], 0.1).unwrap().dist <= 0.0);
        let two = vec![(0.0, 0.0), (1.0, 1.0)];
        assert!(polylabel(&[&two], 0.1).unwrap().dist <= 0.0);
        // A sliver is positive but tiny.
        let sliver = vec![(0.0, 0.0), (100.0, 0.0), (100.0, 0.1), (0.0, 0.1)];
        let p = polylabel(&[&sliver], 0.001).unwrap();
        assert!(p.dist > 0.0 && p.dist <= 0.06, "{p:?}");
    }

    #[test]
    fn rect_inside_respects_edges_and_holes() {
        let outer = square(0.0, 0.0, 100.0);
        let hole = square(40.0, 40.0, 20.0);
        let rings: [&[Pt]; 2] = [&outer, &hole];
        assert!(rect_inside(&rings, (5.0, 5.0, 30.0, 15.0)));
        // Straddles the outer edge.
        assert!(!rect_inside(&rings, (90.0, 5.0, 110.0, 15.0)));
        // Covers the hole entirely: no edge of the outer ring crosses it, but
        // the hole's edges lie inside the rectangle.
        assert!(!rect_inside(&rings, (30.0, 30.0, 70.0, 70.0)));
        // Partly over the hole.
        assert!(!rect_inside(&rings, (35.0, 45.0, 45.0, 55.0)));
        // Just beside the hole, touching its edge is allowed.
        assert!(rect_inside(&rings, (60.0, 45.0, 80.0, 55.0)));
        // Entirely inside the hole (centre outside the polygon).
        assert!(!rect_inside(&rings, (45.0, 45.0, 55.0, 55.0)));
        // Entirely outside.
        assert!(!rect_inside(&rings, (200.0, 200.0, 210.0, 210.0)));
    }

    #[test]
    fn rect_inside_concave_notch() {
        let c = vec![
            (0.0, 0.0),
            (20.0, 0.0),
            (20.0, 3.0),
            (4.0, 3.0),
            (4.0, 17.0),
            (20.0, 17.0),
            (20.0, 20.0),
            (0.0, 20.0),
        ];
        // In the spine.
        assert!(rect_inside(&[&c], (0.5, 5.0, 3.5, 15.0)));
        // Bridging the notch (the centroid side).
        assert!(!rect_inside(&[&c], (2.0, 8.0, 12.0, 12.0)));
        // Along the top arm.
        assert!(rect_inside(&[&c], (5.0, 17.2, 19.0, 19.5)));
    }

    #[test]
    fn clip_keeps_the_visible_part() {
        let big = square(-1000.0, -1000.0, 3000.0);
        let c = clip_ring(&big, (0.0, 0.0, 100.0, 50.0));
        let b = bbox(&c).unwrap();
        assert_eq!(b, (0.0, 0.0, 100.0, 50.0));
        // Entirely outside: nothing.
        assert!(clip_ring(&square(500.0, 500.0, 10.0), (0.0, 0.0, 100.0, 50.0)).is_empty());
        // Entirely inside: unchanged.
        let small = square(10.0, 10.0, 5.0);
        assert_eq!(clip_ring(&small, (0.0, 0.0, 100.0, 50.0)).len(), 4);
    }

    #[test]
    fn pole_of_a_clipped_giant_is_inside_the_window() {
        let big = square(-1000.0, -1000.0, 1050.0); // covers x,y in -1000..50
        let win = (0.0, 0.0, 200.0, 100.0);
        let clipped = clip_ring(&big, win);
        let p = polylabel(&[&clipped], 0.1).unwrap();
        assert!(p.x > 0.0 && p.x < 50.0 && p.y > 0.0 && p.y < 50.0, "{p:?}");
        assert!((p.dist - 25.0).abs() < 0.5, "{p:?}");
    }
}
