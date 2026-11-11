//! Stroke geometry for ways: a polyline and a half-width become a list of
//! triangles, with joins at the bends and caps at the ends.
//!
//! The way pass emits raw triangles into a few shared paths instead of going
//! through a tessellator (see `OsmLayer::build_way_scene`), so joins and caps
//! are built by hand here, with the same budget in mind: a bend costs no
//! extra vertices when it can be mitered.
//!
//! * Joins. Each segment is a quad. With joins on, the quads of two
//!   neighbouring segments share the same end edge, running through the
//!   vertex along the bisector (a miter), so the outside of the bend is
//!   filled and the inside does not overlap: no extra triangles at all. Where
//!   a miter is not possible (the bend is sharper than `MITER_LIMIT` allows,
//!   or a neighbouring segment is too short for the inner corner) the quads
//!   end flat and a bevel triangle fills the outside instead; a bend of
//!   almost 180 degrees gets a round cap on its tip.
//! * Caps. The first and last vertex of an open polyline get a round cap, a
//!   half-disc of 2 to 4 triangles. A closed polyline (first point equal to
//!   the last) is joined at the closing vertex and has no caps.
//!
//! Strokes of a thin width do not get either (`JOIN_MIN_WIDTH_PX`,
//! `CAP_MIN_WIDTH_PX`): the callers ask for `Shape::PLAIN` (or use
//! `emit_segment`) and get plain quads.
//!
//! `Polyline::emit_layers` strokes the same polyline at several widths (the
//! route overlay, the casing and the stroke of one way) in a single pass, so
//! directions and miters are computed once per vertex.

use gpui::{point, px, Path, PathVertex, Pixels};

pub type Pt = (f32, f32);

/// Strokes narrower than this many pixels get no joins: the gap at a bend of
/// a hairline is under a pixel.
pub const JOIN_MIN_WIDTH_PX: f32 = 1.5;

/// Ways drawn narrower than this get no end caps (on any of their layers:
/// an even outline needs the caps of the stroke and its casing together).
/// A cap on a hairline is under two pixels long, and caps are the one part of
/// a stroke that costs vertices.
pub const CAP_MIN_WIDTH_PX: f32 = 2.0;

/// Ways whose bounding box is smaller than this many pixels across get
/// plain strokes: a bend of a way that small is a pixel or two.
pub const SHAPE_MIN_EXTENT_PX: f32 = 12.0;

/// The longest miter allowed, as a multiple of the half-width (the SVG
/// "miter limit" of 3: bends of up to about 141 degrees are mitered).
pub const MITER_LIMIT: f32 = 3.0;

/// A bevel whose open side is shorter than this is not drawn.
const BEVEL_MIN_CHORD_PX: f32 = 1.0;

/// Consecutive points closer than this are one point.
const DEDUP_EPS: f32 = 0.01;

/// `dot` of the two directions under which a bend is a hairpin.
const HAIRPIN_DOT: f32 = -0.95;

/// Receives the triangles of a stroke.
pub trait TriSink {
    fn tri(&mut self, a: Pt, b: Pt, c: Pt);

    /// A quad `a b c d` (in order around its edge).
    fn quad(&mut self, a: Pt, b: Pt, c: Pt, d: Pt) {
        self.tri(a, b, c);
        self.tri(a, c, d);
    }
}

impl<T: TriSink + ?Sized> TriSink for &mut T {
    #[inline]
    fn tri(&mut self, a: Pt, b: Pt, c: Pt) {
        (**self).tri(a, b, c);
    }

    #[inline]
    fn quad(&mut self, a: Pt, b: Pt, c: Pt, d: Pt) {
        (**self).quad(a, b, c, d);
    }
}

/// Collects the triangles, for tests.
impl TriSink for Vec<[Pt; 3]> {
    fn tri(&mut self, a: Pt, b: Pt, c: Pt) {
        self.push([a, b, c]);
    }
}

/// Appends raw triangles to a path the way the way pass batches them, and
/// widens the batch's bounds as it goes.
pub struct PathSink<'a> {
    pub path: &'a mut Option<Path<Pixels>>,
    pub bounds: &'a mut (f32, f32, f32, f32),
}

impl PathSink<'_> {
    #[inline]
    fn widen(&mut self, p: Pt) {
        let (min_x, max_x, min_y, max_y) = &mut *self.bounds;
        if p.0 < *min_x {
            *min_x = p.0;
        }
        if p.0 > *max_x {
            *max_x = p.0;
        }
        if p.1 < *min_y {
            *min_y = p.1;
        }
        if p.1 > *max_y {
            *max_y = p.1;
        }
    }

    #[inline]
    fn push(&mut self, pts: &[Pt]) {
        let st = point(0., 1.);
        let p = self
            .path
            .get_or_insert_with(|| Path::new(point(px(pts[0].0), px(pts[0].1))));
        for &(x, y) in pts {
            p.vertices.push(PathVertex {
                xy_position: point(px(x), px(y)),
                st_position: st,
                content_mask: Default::default(),
            });
        }
    }
}

impl TriSink for PathSink<'_> {
    #[inline]
    fn tri(&mut self, a: Pt, b: Pt, c: Pt) {
        self.widen(a);
        self.widen(b);
        self.widen(c);
        self.push(&[a, b, c]);
    }

    #[inline]
    fn quad(&mut self, a: Pt, b: Pt, c: Pt, d: Pt) {
        self.widen(a);
        self.widen(b);
        self.widen(c);
        self.widen(d);
        self.push(&[a, b, c, a, c, d]);
    }
}

/// What a stroke gets beyond plain rectangles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub joins: bool,
    pub caps: bool,
}

impl Shape {
    pub const PLAIN: Shape = Shape {
        joins: false,
        caps: false,
    };
    pub const FULL: Shape = Shape {
        joins: true,
        caps: true,
    };
}

/// One width of a polyline: where its triangles go and how wide it is.
pub struct Layer<S> {
    pub sink: S,
    pub hw: f32,
    pub shape: Shape,
}

/// A plain rectangle for one segment, the unshaped stroke without the cost
/// of preparing a `Polyline`. Nothing for a zero-length segment.
#[inline]
pub fn emit_segment<S: TriSink>(sink: &mut S, a: Pt, b: Pt, hw: f32) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < f32::EPSILON {
        return;
    }
    let inv = 1.0 / len;
    let (nx, ny) = (-dy * inv * hw, dx * inv * hw);
    sink.quad(
        (a.0 + nx, a.1 + ny),
        (a.0 - nx, a.1 - ny),
        (b.0 - nx, b.1 - ny),
        (b.0 + nx, b.1 + ny),
    );
}

/// A segment: unit direction and length.
#[derive(Clone, Copy)]
struct Seg {
    dx: f32,
    dy: f32,
    len: f32,
}

/// The bend at a vertex between two segments.
#[derive(Clone, Copy)]
struct Join {
    /// The miter vector for a half-width of 1 (to the left of the path);
    /// only meaningful when `within_limit`.
    kx: f32,
    ky: f32,
    /// Tangent of half the turning angle: how far back along a segment the
    /// inner corner of the miter lies, per unit of half-width.
    tan_half: f32,
    /// The miter is no longer than `MITER_LIMIT` half-widths.
    within_limit: bool,
    /// Cosine of the turning angle (1 straight on, -1 a hairpin).
    dot: f32,
    /// Sign of the turn (positive: towards the left of the path).
    cross: f32,
    /// The shorter of the two neighbouring segments.
    min_len: f32,
}

impl Join {
    fn between(a: Seg, b: Seg) -> Join {
        let dot = a.dx * b.dx + a.dy * b.dy;
        let cross = a.dx * b.dy - a.dy * b.dx;
        let denom = 1.0 + dot;
        // The miter vector is (n0 + n1) / (1 + dot), its length
        // 1 / cos(turn / 2), so |k|^2 = 2 / (1 + dot).
        let within_limit = denom > 2.0 / (MITER_LIMIT * MITER_LIMIT);
        let (kx, ky, tan_half) = if within_limit {
            let inv = 1.0 / denom;
            ((-a.dy - b.dy) * inv, (a.dx + b.dx) * inv, cross.abs() * inv)
        } else {
            (0.0, 0.0, f32::INFINITY)
        };
        Join {
            kx,
            ky,
            tan_half,
            within_limit,
            dot,
            cross,
            min_len: a.len.min(b.len),
        }
    }

    /// Whether the miter can be used at half-width `hw`: it is within the
    /// limit and the inner corner does not run past the middle of either
    /// neighbouring segment.
    #[inline]
    fn mitered(&self, hw: f32) -> bool {
        self.within_limit && hw * self.tan_half <= 0.5 * self.min_len
    }
}

/// A polyline prepared for stroking: points without duplicates or
/// non-finite values. Reusable scratch.
#[derive(Default)]
pub struct Polyline {
    pts: Vec<Pt>,
    closed: bool,
}

impl Polyline {
    /// Load `pts`, dropping non-finite points and consecutive duplicates. A
    /// polyline of at least three segments whose last point is its first is
    /// closed.
    pub fn set(&mut self, pts: impl IntoIterator<Item = Pt>) {
        self.pts.clear();
        self.closed = false;
        for (x, y) in pts {
            if !(x.is_finite() && y.is_finite()) {
                continue;
            }
            if let Some(&(lx, ly)) = self.pts.last() {
                if (x - lx) * (x - lx) + (y - ly) * (y - ly) < DEDUP_EPS * DEDUP_EPS {
                    continue;
                }
            }
            self.pts.push((x, y));
        }
        let n = self.pts.len();
        if n >= 4 {
            let (fx, fy) = self.pts[0];
            let (lx, ly) = self.pts[n - 1];
            if (fx - lx) * (fx - lx) + (fy - ly) * (fy - ly) < DEDUP_EPS * DEDUP_EPS {
                self.closed = true;
                self.pts[n - 1] = (fx, fy);
            }
        }
    }

    /// Whether there is anything to stroke.
    pub fn is_empty(&self) -> bool {
        self.pts.len() < 2
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    #[inline]
    fn seg(&self, i: usize) -> Seg {
        let (x0, y0) = self.pts[i];
        let (x1, y1) = self.pts[i + 1];
        let (ex, ey) = (x1 - x0, y1 - y0);
        let len = (ex * ex + ey * ey).sqrt();
        let inv = 1.0 / len;
        Seg {
            dx: ex * inv,
            dy: ey * inv,
            len,
        }
    }

    /// The triangles of the stroke at half-width `hw`.
    pub fn emit<S: TriSink>(&self, sink: &mut S, hw: f32, shape: Shape) {
        self.emit_layers(&mut [Some(Layer { sink, hw, shape })]);
    }

    /// The triangles of the stroke at each of `layers`' half-widths, in one
    /// pass. With joins the bends are joined, with caps the open ends are
    /// capped; without either every segment is a plain rectangle.
    pub fn emit_layers<S: TriSink>(&self, layers: &mut [Option<Layer<S>>]) {
        let n = self.pts.len();
        if n < 2 {
            return;
        }
        let m = n - 1;
        let mut cur = self.seg(0);
        // The join at the start of segment `i`: only a closed polyline has
        // one at its first vertex.
        let mut start_join = if self.closed {
            Some(Join::between(self.seg(m - 1), cur))
        } else {
            None
        };
        for i in 0..m {
            let nxt = if i + 1 < m {
                Some(self.seg(i + 1))
            } else if self.closed {
                Some(self.seg(0))
            } else {
                None
            };
            let end_join = nxt.map(|b| Join::between(cur, b));
            let (x0, y0) = self.pts[i];
            let (x1, y1) = self.pts[i + 1];
            let (n0x, n0y) = (-cur.dy, cur.dx);
            for layer in layers.iter_mut().flatten() {
                let hw = layer.hw;
                let joins = layer.shape.joins;
                let (sx, sy) = match &start_join {
                    Some(j) if joins && j.mitered(hw) => (j.kx * hw, j.ky * hw),
                    _ => (n0x * hw, n0y * hw),
                };
                let (ex, ey) = match &end_join {
                    Some(j) if joins && j.mitered(hw) => (j.kx * hw, j.ky * hw),
                    _ => (n0x * hw, n0y * hw),
                };
                layer.sink.quad(
                    (x0 + sx, y0 + sy),
                    (x0 - sx, y0 - sy),
                    (x1 - ex, y1 - ey),
                    (x1 + ex, y1 + ey),
                );
                if joins {
                    if let (Some(j), Some(b)) = (&end_join, &nxt) {
                        if !j.mitered(hw) {
                            bevel(&mut layer.sink, (x1, y1), cur, *b, j, hw, layer.shape.caps);
                        }
                    }
                }
                if layer.shape.caps && !self.closed {
                    if i == 0 {
                        cap(&mut layer.sink, (x0, y0), (-cur.dx, -cur.dy), hw);
                    }
                    if i + 1 == m {
                        cap(&mut layer.sink, (x1, y1), (cur.dx, cur.dy), hw);
                    }
                }
            }
            start_join = end_join;
            if let Some(b) = nxt {
                cur = b;
            }
        }
    }
}

/// What fills the outside of a bend that was not mitered: a bevel triangle,
/// or a round cap on a hairpin tip.
fn bevel<S: TriSink>(sink: &mut S, c: Pt, a: Seg, b: Seg, j: &Join, hw: f32, caps: bool) {
    if j.dot < HAIRPIN_DOT {
        if caps {
            cap(sink, c, (a.dx, a.dy), hw);
        }
        return;
    }
    // The outside of the bend is opposite the side it turns to.
    let side = if j.cross > 0.0 { -hw } else { hw };
    let pa = (c.0 - side * a.dy, c.1 + side * a.dx);
    let pb = (c.0 - side * b.dy, c.1 + side * b.dx);
    let chord2 = (pa.0 - pb.0) * (pa.0 - pb.0) + (pa.1 - pb.1) * (pa.1 - pb.1);
    if chord2 >= BEVEL_MIN_CHORD_PX * BEVEL_MIN_CHORD_PX {
        sink.tri(c, pa, pb);
    }
}

/// The outline of a cap in units of the half-width, as `(along, across)`
/// from the end edge's left corner to its right one. Small strokes (under
/// `FINE_CAP_HALF_WIDTH`) get a chamfered square, two triangles: it reaches
/// the full half-width past the node and its corners are cut to about the
/// round shape. Wider ones get a half-octagon fan.
const COARSE_CAP: [(f32, f32); 4] = [(0.0, -1.0), (1.0, -0.5), (1.0, 0.5), (0.0, 1.0)];
const FINE_CAP: [(f32, f32); 5] = [
    (0.0, -1.0),
    (0.707_106_77, -0.707_106_77),
    (1.0, 0.0),
    (0.707_106_77, 0.707_106_77),
    (0.0, 1.0),
];
const FINE_CAP_HALF_WIDTH: f32 = 4.0;

/// A round cap of radius `hw` centred on `c`, bulging in the direction `out`
/// (a unit vector). Its straight edge is the end edge of the stroke's last
/// quad.
fn cap<S: TriSink>(sink: &mut S, c: Pt, out: Pt, hw: f32) {
    // `n` is `out` turned a quarter to the left.
    let n = (-out.1, out.0);
    let at = |(along, across): (f32, f32)| {
        (
            c.0 + hw * (along * out.0 + across * n.0),
            c.1 + hw * (along * out.1 + across * n.1),
        )
    };
    if hw < FINE_CAP_HALF_WIDTH {
        let [a, b, c2, d] = COARSE_CAP.map(at);
        sink.tri(a, b, c2);
        sink.tri(a, c2, d);
    } else {
        for w in FINE_CAP.windows(2) {
            sink.tri(c, at(w[0]), at(w[1]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tris(pts: &[Pt], hw: f32, shaped: bool) -> Vec<[Pt; 3]> {
        let mut poly = Polyline::default();
        poly.set(pts.iter().copied());
        let mut out = Vec::new();
        let shape = if shaped { Shape::FULL } else { Shape::PLAIN };
        poly.emit(&mut out, hw, shape);
        out
    }

    fn area(t: &[Pt; 3]) -> f32 {
        ((t[1].0 - t[0].0) * (t[2].1 - t[0].1) - (t[2].0 - t[0].0) * (t[1].1 - t[0].1)) / 2.0
    }

    fn covers(tris: &[[Pt; 3]], p: Pt) -> bool {
        const E: f32 = 1e-4;
        tris.iter().any(|t| {
            let side = |a: Pt, b: Pt| (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
            let (s0, s1, s2) = (side(t[0], t[1]), side(t[1], t[2]), side(t[2], t[0]));
            (s0 >= -E && s1 >= -E && s2 >= -E) || (s0 <= E && s1 <= E && s2 <= E)
        })
    }

    fn all_points(tris: &[[Pt; 3]]) -> impl Iterator<Item = Pt> + '_ {
        tris.iter().flat_map(|t| t.iter().copied())
    }

    fn assert_sound(tris: &[[Pt; 3]]) {
        for t in tris {
            for p in t {
                assert!(p.0.is_finite() && p.1.is_finite(), "NaN in {t:?}");
            }
        }
    }

    #[test]
    fn unshaped_strokes_are_one_rectangle_per_segment() {
        let t = tris(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], 2.0, false);
        assert_eq!(t.len(), 4);
        // The inside of the bend overlaps and the outside has a hole.
        assert!(!covers(&t, (12.0, -2.0)));
    }

    #[test]
    fn a_right_angle_bend_covers_its_outer_corner() {
        let hw = 2.0;
        let t = tris(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], hw, true);
        assert_sound(&t);
        // The turn is to the right in screen space (y down), so the outside
        // is the top right: the miter tip is (12, -2).
        assert!(covers(&t, (12.0 - 0.01, -2.0 + 0.01)));
        assert!(covers(&t, (10.0, 0.0)));
        // Nothing beyond the miter.
        assert!(!covers(&t, (12.5, -2.5)));
        // A bend costs no extra triangles when it can be mitered: two
        // quads, and a cap at each end.
        assert_eq!(t.len(), 4 + 2 * 2);
    }

    #[test]
    fn the_miter_works_for_either_turning_direction() {
        let t = tris(&[(0.0, 0.0), (10.0, 0.0), (10.0, -10.0)], 2.0, true);
        assert!(covers(&t, (12.0 - 0.01, 2.0 - 0.01)));
        let t = tris(&[(10.0, 10.0), (10.0, 0.0), (0.0, 0.0)], 2.0, true);
        assert!(covers(&t, (12.0 - 0.01, -2.0 + 0.01)) || covers(&t, (12.0 - 0.01, 2.0 - 0.01)));
        assert!(covers(&t, (10.0 + 1.99, 0.0 - 1.99)) || covers(&t, (10.0 - 1.99, 0.0 - 1.99)));
    }

    #[test]
    fn miter_quads_do_not_overlap_each_other() {
        let mut poly = Polyline::default();
        poly.set([(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
        let mut t = Vec::new();
        poly.emit(
            &mut t,
            2.0,
            Shape {
                joins: true,
                caps: false,
            },
        );
        // The two quads tile the stroke exactly: 40 + 40 with the corner
        // overlap replaced by the miter.
        assert_eq!(t.len(), 4);
        let quads: f32 = t.iter().map(|x| area(x).abs()).sum();
        assert!((quads - 80.0).abs() < 1e-3, "{quads}");
    }

    #[test]
    fn a_very_sharp_angle_falls_back_to_bevel_within_the_miter_limit() {
        let hw = 2.0;
        // A 20 degree interior angle: far past the miter limit.
        let a = 20.0_f32.to_radians();
        let pts = [(100.0, 0.0), (0.0, 0.0), (100.0 * a.cos(), 100.0 * a.sin())];
        let t = tris(&pts, hw, true);
        assert_sound(&t);
        for p in all_points(&t) {
            let near = (p.0 * p.0 + p.1 * p.1).sqrt();
            // Near the vertex nothing reaches beyond the half-width: no
            // spike (the miter would be 5.8 half-widths long).
            if near < 20.0 {
                assert!(near <= MITER_LIMIT * hw + 1e-3, "{p:?}");
            }
        }
        let far = all_points(&t)
            .map(|p| (p.0 * p.0 + p.1 * p.1).sqrt())
            .filter(|d| *d < 20.0)
            .fold(0.0_f32, f32::max);
        assert!(far <= hw + 1e-3, "{far}");
        // Two quads, a bevel triangle, two caps.
        assert_eq!(t.len(), 4 + 1 + 2 * 2);
    }

    #[test]
    fn a_hairpin_gets_a_cap_instead_of_a_degenerate_bevel() {
        let t = tris(&[(0.0, 0.0), (20.0, 0.0), (0.0, 0.0001)], 2.0, true);
        assert_sound(&t);
        assert!(t.iter().all(|x| x.iter().all(|p| p.0 < 22.1)));
        assert!(covers(&t, (21.9, 0.0)));
    }

    #[test]
    fn collinear_points_add_no_join_geometry() {
        let straight = tris(&[(0.0, 0.0), (10.0, 0.0), (20.0, 0.0)], 2.0, true);
        let single = tris(&[(0.0, 0.0), (20.0, 0.0)], 2.0, true);
        // One more quad (two triangles) and nothing else.
        assert_eq!(straight.len(), single.len() + 2);
        assert_sound(&straight);
        // A bend too slight to see is mitered, never bevelled.
        let slight = tris(&[(0.0, 0.0), (10.0, 0.0), (20.0, 0.1)], 2.0, true);
        assert_eq!(slight.len(), straight.len());
    }

    #[test]
    fn a_closed_ring_is_joined_at_the_closing_vertex_and_has_no_caps() {
        let hw = 2.0;
        let ring = [
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ];
        let mut poly = Polyline::default();
        poly.set(ring);
        assert!(poly.is_closed());
        let mut t = Vec::new();
        poly.emit(&mut t, hw, Shape::FULL);
        assert_sound(&t);
        // Four quads, nothing else.
        assert_eq!(t.len(), 8);
        for corner in [(-2.0, -2.0), (12.0, -2.0), (12.0, 12.0), (-2.0, 12.0)] {
            let inward = (
                if corner.0 < 0.0 { 0.01 } else { -0.01 },
                if corner.1 < 0.0 { 0.01 } else { -0.01 },
            );
            assert!(
                covers(&t, (corner.0 + inward.0, corner.1 + inward.1)),
                "{corner:?}"
            );
        }
        // Without the closing join the first corner would be notched.
        let mut open = Polyline::default();
        open.set(ring[..4].iter().copied());
        let mut o = Vec::new();
        open.emit(&mut o, hw, Shape::FULL);
        assert!(!covers(&o, (-1.99, -1.99)));
    }

    #[test]
    fn zero_length_segments_and_duplicate_points_are_harmless() {
        let pts = [
            (0.0, 0.0),
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (10.0, 10.0),
        ];
        let t = tris(&pts, 2.0, true);
        assert_sound(&t);
        assert!(t.iter().all(|x| area(x).abs() > 1e-4));
        assert_eq!(t.len(), tris(&[pts[0], pts[2], pts[5]], 2.0, true).len());
        // Nothing at all from a single point or a pile of identical ones.
        assert!(tris(&[(1.0, 1.0)], 2.0, true).is_empty());
        assert!(tris(&[(1.0, 1.0), (1.0, 1.0), (1.0, 1.0)], 2.0, true).is_empty());
        assert!(tris(&[], 2.0, true).is_empty());
        // Non-finite points are dropped.
        let t = tris(&[(0.0, 0.0), (f32::NAN, 1.0), (10.0, 0.0)], 2.0, true);
        assert_sound(&t);
        assert!(!t.is_empty());
    }

    #[test]
    fn a_short_segment_between_bends_falls_back_without_flipping() {
        // The inner corners of a 2 px wide stroke would pass each other on a
        // 0.5 px long segment: flat ends and bevels instead.
        let pts = [(0.0, 0.0), (10.0, 0.0), (10.0, 0.5), (20.0, 0.5)];
        let t = tris(&pts, 2.0, true);
        assert_sound(&t);
        for p in all_points(&t) {
            assert!(p.0 > -2.1 && p.0 < 22.1 && p.1 > -2.1 && p.1 < 2.6, "{p:?}");
        }
    }

    #[test]
    fn caps_extend_past_the_end_nodes_by_the_half_width() {
        for hw in [1.0, 2.5, 8.0] {
            let t = tris(&[(0.0, 0.0), (20.0, 0.0)], hw, true);
            assert_sound(&t);
            let max_x = all_points(&t).map(|p| p.0).fold(f32::MIN, f32::max);
            let min_x = all_points(&t).map(|p| p.0).fold(f32::MAX, f32::min);
            assert!((max_x - (20.0 + hw)).abs() < 1e-3, "{max_x}");
            assert!((min_x + hw).abs() < 1e-3, "{min_x}");
            // The cap is round: the corner of a square end is not covered.
            assert!(!covers(&t, (20.0 + hw * 0.95, hw * 0.95)));
            assert!(covers(&t, (20.0 + hw * 0.5, hw * 0.5)));
            assert!(t.iter().all(|x| area(x).abs() > 1e-4));
        }
        // Unshaped strokes end square on the node.
        let t = tris(&[(0.0, 0.0), (20.0, 0.0)], 2.0, false);
        let max_x = all_points(&t).map(|p| p.0).fold(f32::MIN, f32::max);
        assert_eq!(max_x, 20.0);
    }

    #[test]
    fn concentric_widths_keep_the_same_miter_axis() {
        let pts = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        let fill = tris(&pts, 1.5, true);
        let casing = tris(&pts, 2.5, true);
        assert!(covers(&casing, (10.0 + 1.5 - 0.01, -1.5 + 0.01)));
        assert!(covers(&fill, (10.0 + 1.5 - 0.01, -1.5 + 0.01)));
        assert!(covers(&casing, (10.0 + 2.5 - 0.01, -2.5 + 0.01)));
        assert!(!covers(&fill, (10.0 + 2.5 - 0.01, -2.5 + 0.01)));
    }

    #[test]
    fn several_layers_in_one_pass_equal_separate_passes() {
        let pts = [
            (0.0, 0.0),
            (10.0, 0.0),
            (14.0, 6.0),
            (30.0, 7.0),
            (31.0, 7.2),
        ];
        let mut poly = Polyline::default();
        poly.set(pts);
        let mut casing: Vec<[Pt; 3]> = Vec::new();
        let mut fill: Vec<[Pt; 3]> = Vec::new();
        poly.emit_layers(&mut [
            Some(Layer {
                sink: &mut casing,
                hw: 2.5,
                shape: Shape::FULL,
            }),
            Some(Layer {
                sink: &mut fill,
                hw: 1.5,
                shape: Shape::FULL,
            }),
        ]);
        assert_eq!(casing, tris(&pts, 2.5, true));
        assert_eq!(fill, tris(&pts, 1.5, true));
    }

    #[test]
    fn joins_without_caps_leave_the_ends_square() {
        let mut poly = Polyline::default();
        poly.set([(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
        let mut t = Vec::new();
        poly.emit(
            &mut t,
            2.0,
            Shape {
                joins: true,
                caps: false,
            },
        );
        assert_eq!(t.len(), 4);
        assert!(covers(&t, (11.9, -1.9)));
        let min_x = all_points(&t).map(|p| p.0).fold(f32::MAX, f32::min);
        assert_eq!(min_x, 0.0);
    }

    #[test]
    fn emit_segment_matches_the_plain_polyline() {
        let mut a = Vec::new();
        emit_segment(&mut a, (0.0, 0.0), (10.0, 5.0), 2.0);
        let b = tris(&[(0.0, 0.0), (10.0, 5.0)], 2.0, false);
        assert_eq!(a, b);
        let mut none = Vec::new();
        emit_segment(&mut none, (1.0, 1.0), (1.0, 1.0), 2.0);
        assert!(none.is_empty());
    }

    #[test]
    fn path_sink_tracks_bounds() {
        let mut path = None;
        let mut bounds = (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        );
        let mut poly = Polyline::default();
        poly.set([(0.0, 0.0), (10.0, 0.0)]);
        poly.emit(
            &mut PathSink {
                path: &mut path,
                bounds: &mut bounds,
            },
            2.0,
            Shape::FULL,
        );
        assert_eq!(bounds, (-2.0, 12.0, -2.0, 2.0));
        assert!(!path.unwrap().vertices.is_empty());
    }
}
