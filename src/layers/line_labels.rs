//! Street-name labels laid out along ways, with rotated glyphs.
//!
//! GPUI cannot rotate text (`paint_glyph` and `ShapedLine::paint` hard-code an
//! identity transform), so this module bypasses its text pipeline: it shapes
//! the label with rustybuzz (kerning included) using a bundled font, pulls the
//! glyph outlines out of the font file, flattens and triangulates them once
//! per (glyph, size), and per frame only transforms those cached triangles
//! onto the way and appends them to a raw-triangle batch, the same way the way
//! pass batches strokes. A halo is a stroked copy of the outline, also cached.
//!
//! Three caches keep per-frame work small:
//! - glyph meshes, keyed by (glyph id, size), in pixel units with the origin
//!   on the glyph's baseline-left;
//! - per-label layouts, keyed by way id (and checked against the label text
//!   and the way's geometry), stored relative to the way's first vertex's
//!   screen position at one zoom level. At a fixed zoom, panning is a pure
//!   translation, so a pan frame never re-lays-out anything. The cache is
//!   dropped when the zoom changes;
//! - nothing else: collision against already-placed labels is redone per
//!   frame, it is a handful of rectangle tests.

use std::collections::HashMap;
use std::sync::Arc;

use crate::layers::way_stroke::{Polyline, Shape};

/// A screen-space point in pixels.
pub type Pt = (f32, f32);

/// Bundled label font (IBM Plex Sans SemiBold, SIL OFL 1.1; see
/// `assets/fonts/IBMPlexSans-LICENSE.txt`).
static FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf");

/// Maximum turn at any single vertex inside a label window, in degrees.
pub const MAX_VERTEX_TURN_DEG: f32 = 25.0;
/// Maximum summed absolute turn inside a label window, in degrees.
pub const MAX_TOTAL_TURN_DEG: f32 = 45.0;
/// The way must be at least this much longer than the label.
pub const FIT_FACTOR: f32 = 1.2;
/// Smallest zoom at which line labels are drawn.
pub const MIN_ZOOM: f64 = 15.0;

/// Visual parameters, fixed for the prototype.
#[derive(Debug, Clone, Copy)]
pub struct LabelStyle {
    pub size_px: f32,
    /// Halo thickness outside the glyph outline, in pixels.
    pub halo_px: f32,
    /// Flatten tolerance for the fill outline, pixels.
    pub fill_tol: f32,
    /// Simplification tolerance for the halo outline, pixels.
    pub halo_tol: f32,
    /// Padding added to each end of the label window, pixels.
    pub pad_px: f32,
    /// Distance between repeated labels along a long way, pixels.
    pub spacing_px: f32,
    /// Most labels one way can get.
    pub max_per_way: usize,
}

impl Default for LabelStyle {
    fn default() -> Self {
        Self {
            size_px: 12.0,
            halo_px: 1.5,
            fill_tol: 0.1,
            halo_tol: 0.2,
            pad_px: 4.0,
            spacing_px: 300.0,
            max_per_way: 40,
        }
    }
}

// ---------------------------------------------------------------------------
// Curve flattening and contour helpers (pure).
// ---------------------------------------------------------------------------

/// Number of segments needed to keep a quadratic Bezier within `tol` of its
/// flattened form.
pub fn quad_segments(p0: Pt, p1: Pt, p2: Pt, tol: f32) -> usize {
    let dx = p0.0 - 2.0 * p1.0 + p2.0;
    let dy = p0.1 - 2.0 * p1.1 + p2.1;
    let dd = (dx * dx + dy * dy).sqrt();
    ((dd / (4.0 * tol.max(1e-4))).sqrt().ceil() as usize).clamp(1, 64)
}

/// Append the flattened quadratic (excluding `p0`, including `p2`).
pub fn flatten_quad(p0: Pt, p1: Pt, p2: Pt, tol: f32, out: &mut Vec<Pt>) {
    let n = quad_segments(p0, p1, p2, tol);
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        out.push((
            u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0,
            u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1,
        ));
    }
}

/// Append the flattened cubic (excluding `p0`, including `p3`).
pub fn flatten_cubic(p0: Pt, p1: Pt, p2: Pt, p3: Pt, tol: f32, out: &mut Vec<Pt>) {
    let d1 = (p0.0 - 2.0 * p1.0 + p2.0, p0.1 - 2.0 * p1.1 + p2.1);
    let d2 = (p1.0 - 2.0 * p2.0 + p3.0, p1.1 - 2.0 * p2.1 + p3.1);
    let m = (d1.0 * d1.0 + d1.1 * d1.1)
        .sqrt()
        .max((d2.0 * d2.0 + d2.1 * d2.1).sqrt());
    let n = ((3.0 * m / (4.0 * tol.max(1e-4))).sqrt().ceil() as usize).clamp(1, 64);
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        out.push((
            a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
            a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
        ));
    }
}

/// Shoelace area (signed).
pub fn signed_area(c: &[Pt]) -> f32 {
    let mut a = 0.0;
    for i in 0..c.len() {
        let (x0, y0) = c[i];
        let (x1, y1) = c[(i + 1) % c.len()];
        a += x0 * y1 - x1 * y0;
    }
    a * 0.5
}

fn point_in_poly(p: Pt, poly: &[Pt]) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > p.1) != (yj > p.1) && p.0 < (xj - xi) * (p.1 - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Ramer-Douglas-Peucker on a closed contour (kept closed: the first point is
/// the anchor and the farthest point from it is the second anchor).
pub fn simplify_closed(c: &[Pt], tol: f32) -> Vec<Pt> {
    if c.len() <= 4 {
        return c.to_vec();
    }
    let far = (1..c.len())
        .max_by(|&a, &b| {
            let da = dist2(c[0], c[a]);
            let db = dist2(c[0], c[b]);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(1);
    let mut out = Vec::with_capacity(c.len());
    rdp(&c[..=far], tol, &mut out);
    let mut second: Vec<Pt> = c[far..].to_vec();
    second.push(c[0]);
    rdp(&second, tol, &mut out);
    out
}

fn dist2(a: Pt, b: Pt) -> f32 {
    (a.0 - b.0) * (a.0 - b.0) + (a.1 - b.1) * (a.1 - b.1)
}

/// Pushes every kept point of `pts` except the last one.
fn rdp(pts: &[Pt], tol: f32, out: &mut Vec<Pt>) {
    let n = pts.len();
    if n < 2 {
        return;
    }
    let (a, b) = (pts[0], pts[n - 1]);
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let mut worst = 0.0f32;
    let mut at = 0;
    for (i, &p) in pts.iter().enumerate().take(n - 1).skip(1) {
        let d = if len2 < 1e-12 {
            dist2(a, p).sqrt()
        } else {
            ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / len2.sqrt()
        };
        if d > worst {
            worst = d;
            at = i;
        }
    }
    if worst > tol {
        rdp(&pts[..=at], tol, out);
        rdp(&pts[at..], tol, out);
    } else {
        out.push(a);
    }
}

/// Triangulate glyph contours (any winding convention) into a flat list of
/// triangle vertices (3 per triangle). Holes are the contours whose winding
/// is opposite to the largest contour's.
pub fn triangulate_contours(contours: &[Vec<Pt>]) -> Vec<Pt> {
    let cs: Vec<&Vec<Pt>> = contours
        .iter()
        .filter(|c| c.len() >= 3 && signed_area(c).abs() > 1e-6)
        .collect();
    let Some(biggest) = cs.iter().max_by(|a, b| {
        signed_area(a)
            .abs()
            .partial_cmp(&signed_area(b).abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    }) else {
        return Vec::new();
    };
    let outer_sign = signed_area(biggest) > 0.0;
    let outers: Vec<&Vec<Pt>> = cs
        .iter()
        .copied()
        .filter(|c| (signed_area(c) > 0.0) == outer_sign)
        .collect();
    let mut holes_of: Vec<Vec<&Vec<Pt>>> = vec![Vec::new(); outers.len()];
    for h in cs
        .iter()
        .copied()
        .filter(|c| (signed_area(c) > 0.0) != outer_sign)
    {
        // Smallest outer containing the hole's first point.
        let owner = outers
            .iter()
            .enumerate()
            .filter(|(_, o)| point_in_poly(h[0], o))
            .min_by(|(_, a), (_, b)| {
                signed_area(a)
                    .abs()
                    .partial_cmp(&signed_area(b).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i);
        if let Some(i) = owner {
            holes_of[i].push(h);
        }
    }
    let mut tris = Vec::new();
    for (o, holes) in outers.iter().zip(holes_of) {
        let mut flat: Vec<f64> = Vec::new();
        let mut verts: Vec<Pt> = Vec::new();
        let mut hole_idx = Vec::new();
        for &p in o.iter() {
            flat.extend([p.0 as f64, p.1 as f64]);
            verts.push(p);
        }
        for h in holes {
            hole_idx.push(verts.len());
            for &p in h.iter() {
                flat.extend([p.0 as f64, p.1 as f64]);
                verts.push(p);
            }
        }
        if let Ok(idx) = earcutr::earcut(&flat, &hole_idx, 2) {
            for i in idx {
                tris.push(verts[i]);
            }
        }
    }
    tris
}

// ---------------------------------------------------------------------------
// Glyph meshes.
// ---------------------------------------------------------------------------

/// Triangles for one glyph at one size: `fill` is the glyph, `halo` the
/// stroked outline. Pixel units, y down, origin on the baseline-left.
#[derive(Debug, Default)]
pub struct GlyphMesh {
    pub fill: Vec<Pt>,
    pub halo: Vec<Pt>,
}

struct OutlineCollector {
    scale: f32,
    tol: f32,
    contours: Vec<Vec<Pt>>,
    cur: Vec<Pt>,
}

impl OutlineCollector {
    fn p(&self, x: f32, y: f32) -> Pt {
        (x * self.scale, -y * self.scale)
    }
}

impl rustybuzz::ttf_parser::OutlineBuilder for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.cur.clear();
        let p = self.p(x, y);
        self.cur.push(p);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.cur.push(p);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let p0 = *self.cur.last().unwrap_or(&(0.0, 0.0));
        let (p1, p2) = (self.p(x1, y1), self.p(x, y));
        flatten_quad(p0, p1, p2, self.tol, &mut self.cur);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let p0 = *self.cur.last().unwrap_or(&(0.0, 0.0));
        let (p1, p2, p3) = (self.p(x1, y1), self.p(x2, y2), self.p(x, y));
        flatten_cubic(p0, p1, p2, p3, self.tol, &mut self.cur);
    }
    fn close(&mut self) {
        let mut c = std::mem::take(&mut self.cur);
        if c.len() > 1 && dist2(c[0], c[c.len() - 1]) < 1e-8 {
            c.pop();
        }
        if c.len() >= 3 {
            self.contours.push(c);
        }
    }
}

/// Build the mesh for one glyph.
fn build_mesh(face: &rustybuzz::Face<'_>, glyph: u16, style: &LabelStyle) -> GlyphMesh {
    let upem = face.units_per_em() as f32;
    let mut oc = OutlineCollector {
        scale: style.size_px / upem,
        tol: style.fill_tol,
        contours: Vec::new(),
        cur: Vec::new(),
    };
    if face
        .outline_glyph(rustybuzz::ttf_parser::GlyphId(glyph), &mut oc)
        .is_none()
    {
        return GlyphMesh::default();
    }
    let fill = triangulate_contours(&oc.contours);
    let mut halo = Vec::new();
    if style.halo_px > 0.0 {
        let mut pl = Polyline::default();
        for c in &oc.contours {
            let s = simplify_closed(c, style.halo_tol);
            if s.len() < 3 {
                continue;
            }
            pl.set(s.iter().copied().chain(std::iter::once(s[0])));
            let mut tris: Vec<[Pt; 3]> = Vec::new();
            pl.emit(&mut tris, style.halo_px, Shape::FULL);
            for t in tris {
                halo.extend(t);
            }
        }
    }
    GlyphMesh { fill, halo }
}

/// One shaped glyph: id, pen offset from the label start, advance (pixels).
#[derive(Debug, Clone, Copy)]
pub struct ShapedGlyph {
    pub id: u16,
    pub x: f32,
    pub advance: f32,
}

/// Text shaped with kerning. Returns `None` when a character is missing from
/// the font (the label is then skipped rather than drawn with a notdef box).
fn shape(face: &rustybuzz::Face<'_>, text: &str, size_px: f32) -> Option<(Vec<ShapedGlyph>, f32)> {
    let mut buf = rustybuzz::UnicodeBuffer::new();
    buf.push_str(text);
    buf.guess_segment_properties();
    let out = rustybuzz::shape(face, &[], buf);
    let k = size_px / face.units_per_em() as f32;
    let mut pen = 0.0;
    let mut glyphs = Vec::new();
    for (info, pos) in out.glyph_infos().iter().zip(out.glyph_positions()) {
        if info.glyph_id == 0 {
            return None;
        }
        glyphs.push(ShapedGlyph {
            id: info.glyph_id as u16,
            x: pen + pos.x_offset as f32 * k,
            advance: pos.x_advance as f32 * k,
        });
        pen += pos.x_advance as f32 * k;
    }
    Some((glyphs, pen))
}

// ---------------------------------------------------------------------------
// Placement on a polyline (pure).
// ---------------------------------------------------------------------------

/// Cumulative arc length and per-vertex absolute turn of a polyline.
#[derive(Debug, Clone)]
pub struct Metrics {
    /// `cum[i]` = arc length from vertex 0 to vertex i.
    pub cum: Vec<f32>,
    /// `turn[i]` = absolute turning angle at vertex i, degrees (0 at ends).
    pub turn: Vec<f32>,
}

/// Drop consecutive duplicate points.
pub fn dedup(pts: &[Pt]) -> Vec<Pt> {
    let mut out: Vec<Pt> = Vec::with_capacity(pts.len());
    for &p in pts {
        if !(p.0.is_finite() && p.1.is_finite()) {
            continue;
        }
        if out.last().is_none_or(|&l| dist2(l, p) > 1e-6) {
            out.push(p);
        }
    }
    out
}

pub fn metrics(pts: &[Pt]) -> Metrics {
    let n = pts.len();
    let mut cum = vec![0.0; n];
    let mut turn = vec![0.0; n];
    for i in 1..n {
        cum[i] = cum[i - 1] + dist2(pts[i - 1], pts[i]).sqrt();
    }
    for i in 1..n.saturating_sub(1) {
        let a = (pts[i].0 - pts[i - 1].0, pts[i].1 - pts[i - 1].1);
        let b = (pts[i + 1].0 - pts[i].0, pts[i + 1].1 - pts[i].1);
        let cross = a.0 * b.1 - a.1 * b.0;
        let dot = a.0 * b.0 + a.1 * b.1;
        turn[i] = cross.atan2(dot).abs().to_degrees();
    }
    Metrics { cum, turn }
}

/// Whether the window `[s0, s1]` (arc lengths) is straight enough: every
/// interior vertex turns at most `max_vertex` degrees and the turns sum to
/// at most `max_total`.
pub fn window_ok(m: &Metrics, s0: f32, s1: f32, max_vertex: f32, max_total: f32) -> bool {
    let lo = m.cum.partition_point(|&c| c <= s0 + 1e-3);
    let hi = m.cum.partition_point(|&c| c < s1 - 1e-3);
    let mut total = 0.0;
    for i in lo..hi {
        if m.turn[i] > max_vertex {
            return false;
        }
        total += m.turn[i];
        if total > max_total {
            return false;
        }
    }
    true
}

/// Start arc lengths for labels of `length` along a polyline: evenly spread
/// slots (about one per `length + spacing`), each taking the straight window
/// closest to its slot centre. Empty when the way is too short.
pub fn choose_windows(m: &Metrics, length: f32, spacing: f32, max_labels: usize) -> Vec<f32> {
    let total = *m.cum.last().unwrap_or(&0.0);
    if length <= 0.0 || total < length * FIT_FACTOR {
        return Vec::new();
    }
    const STEP: f32 = 4.0;
    let mut valid: Vec<f32> = Vec::new();
    let mut s = 0.0;
    while s + length <= total + 1e-3 {
        if window_ok(m, s, s + length, MAX_VERTEX_TURN_DEG, MAX_TOTAL_TURN_DEG) {
            valid.push(s);
        }
        s += STEP;
    }
    if valid.is_empty() {
        return Vec::new();
    }
    let k = (((total + spacing) / (length + spacing)).floor() as usize).clamp(1, max_labels);
    let slot = total / k as f32;
    let mut out: Vec<f32> = Vec::new();
    for j in 0..k {
        let centre = slot * (j as f32 + 0.5);
        let best = valid
            .iter()
            .copied()
            .filter(|&s| (s + length / 2.0 - centre).abs() <= slot / 2.0)
            .filter(|&s| out.last().is_none_or(|&p| s >= p + length + 10.0))
            .min_by(|&a, &b| {
                (a + length / 2.0 - centre)
                    .abs()
                    .partial_cmp(&(b + length / 2.0 - centre).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some(b) = best {
            out.push(b);
        }
    }
    out
}

/// Whether text running along `(dx, dy)` (screen coords, y down) would be
/// upside down, i.e. its angle is outside (-90, 90].
pub fn needs_flip(dx: f32, dy: f32) -> bool {
    dx < -1e-4 || (dx.abs() <= 1e-4 && dy < 0.0)
}

/// Point and tangent angle (radians) at arc length `s`.
pub fn point_at(pts: &[Pt], m: &Metrics, s: f32) -> (Pt, f32) {
    let n = pts.len();
    let i = m.cum.partition_point(|&c| c <= s).clamp(1, n - 1) - 1;
    let (a, b) = (pts[i], pts[i + 1]);
    let seg = (m.cum[i + 1] - m.cum[i]).max(1e-6);
    let t = ((s - m.cum[i]) / seg).clamp(0.0, 1.0);
    (
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t),
        (b.1 - a.1).atan2(b.0 - a.0),
    )
}

// ---------------------------------------------------------------------------
// Label layout and the frame builder.
// ---------------------------------------------------------------------------

/// One glyph placed on the way: offset of its baseline-left origin from the
/// layout reference point and the rotation to apply.
#[derive(Debug, Clone)]
pub struct PlacedGlyph {
    pub mesh: Arc<GlyphMesh>,
    pub x: f32,
    pub y: f32,
    pub cos: f32,
    pub sin: f32,
}

/// One label occurrence on a way.
#[derive(Debug, Clone)]
pub struct PlacedLabel {
    pub glyphs: Vec<PlacedGlyph>,
    /// Collision boxes `[min_x, min_y, max_x, max_y]`, relative to the
    /// reference point.
    pub boxes: Vec<[f32; 4]>,
    pub bbox: [f32; 4],
}

struct CacheEntry {
    text: String,
    sig: (usize, u64, u64),
    labels: Vec<PlacedLabel>,
}

/// What `frame` produces: triangle vertex lists (3 per triangle) in screen
/// space, halos to be painted first.
#[derive(Debug, Default)]
pub struct FrameOut {
    pub halo: Vec<Pt>,
    pub fill: Vec<Pt>,
    pub labels: usize,
    pub layouts_built: usize,
}

/// A way that wants a label this frame.
pub struct Candidate<'a> {
    pub way_id: i64,
    pub text: &'a str,
    /// `(node id, mercator x, mercator y)`.
    pub verts: &'a [(i64, f64, f64)],
}

pub struct LineLabelCache {
    face: rustybuzz::Face<'static>,
    pub style: LabelStyle,
    zoom_bits: u64,
    glyphs: HashMap<(u16, u32), Arc<GlyphMesh>>,
    layouts: HashMap<i64, CacheEntry>,
}

impl Default for LineLabelCache {
    fn default() -> Self {
        Self::new(LabelStyle::default())
    }
}

fn geometry_sig(verts: &[(i64, f64, f64)]) -> (usize, u64, u64) {
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    for &(_, x, y) in verts {
        sx += x;
        sy += y;
    }
    (verts.len(), sx.to_bits(), sy.to_bits())
}

impl LineLabelCache {
    pub fn new(style: LabelStyle) -> Self {
        Self {
            face: rustybuzz::Face::from_slice(FONT_BYTES, 0).expect("bundled font parses"),
            style,
            zoom_bits: 0,
            glyphs: HashMap::new(),
            layouts: HashMap::new(),
        }
    }

    pub fn glyph_cache_len(&self) -> usize {
        self.glyphs.len()
    }

    /// Total (fill, halo) vertex counts over all cached glyph meshes.
    pub fn mesh_vertex_totals(&self) -> (usize, usize) {
        self.glyphs
            .values()
            .fold((0, 0), |(f, h), m| (f + m.fill.len(), h + m.halo.len()))
    }

    fn mesh(&mut self, id: u16) -> Arc<GlyphMesh> {
        let key = (id, (self.style.size_px * 4.0).round() as u32);
        if let Some(m) = self.glyphs.get(&key) {
            return m.clone();
        }
        let m = Arc::new(build_mesh(&self.face, id, &self.style));
        self.glyphs.insert(key, m.clone());
        m
    }

    /// Lay `text` out along screen polyline `pts` (relative to its first
    /// point; no reference offset is applied).
    pub fn layout(&mut self, text: &str, pts: &[Pt]) -> Vec<PlacedLabel> {
        let pts = dedup(pts);
        if pts.len() < 2 {
            return Vec::new();
        }
        let Some((shaped, width)) = shape(&self.face, text, self.style.size_px) else {
            return Vec::new();
        };
        let size = self.style.size_px;
        let pad = self.style.pad_px;
        let length = width + 2.0 * pad;
        let m = metrics(&pts);
        let starts = choose_windows(&m, length, self.style.spacing_px, self.style.max_per_way);
        let mut labels = Vec::new();
        for s0 in starts {
            let (a, _) = point_at(&pts, &m, s0 + pad);
            let (b, _) = point_at(&pts, &m, s0 + length - pad);
            let flip = needs_flip(b.0 - a.0, b.1 - a.1);
            let mut glyphs = Vec::with_capacity(shaped.len());
            let mut boxes: Vec<[f32; 4]> = Vec::new();
            let mut group = [
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            ];
            let mut bbox = group;
            for (gi, g) in shaped.iter().enumerate() {
                // Arc length of the glyph's centre along the way.
                let along = g.x + g.advance / 2.0;
                let s = if flip {
                    s0 + length - pad - along
                } else {
                    s0 + pad + along
                };
                let ((px_, py_), tangent) = point_at(&pts, &m, s);
                let angle = if flip {
                    tangent + std::f32::consts::PI
                } else {
                    tangent
                };
                let (sin, cos) = angle.sin_cos();
                // Local frame: x along the text, y down. Put the glyph's
                // baseline-left so the x-height centre sits on the way.
                let (lx, ly) = (-g.advance / 2.0, 0.35 * size);
                let ox = px_ + lx * cos - ly * sin;
                let oy = py_ + lx * sin + ly * cos;
                let mesh = self.mesh(g.id);
                // Glyph box in local frame, with halo margin.
                let mg = self.style.halo_px + 1.0;
                for (cx, cy) in [
                    (-mg, -0.8 * size - mg),
                    (g.advance + mg, -0.8 * size - mg),
                    (g.advance + mg, 0.25 * size + mg),
                    (-mg, 0.25 * size + mg),
                ] {
                    let (x, y) = (ox + cx * cos - cy * sin, oy + cx * sin + cy * cos);
                    group = [
                        group[0].min(x),
                        group[1].min(y),
                        group[2].max(x),
                        group[3].max(y),
                    ];
                }
                if !mesh.fill.is_empty() {
                    glyphs.push(PlacedGlyph {
                        mesh,
                        x: ox,
                        y: oy,
                        cos,
                        sin,
                    });
                }
                if gi % 3 == 2 || gi + 1 == shaped.len() {
                    boxes.push(group);
                    bbox = [
                        bbox[0].min(group[0]),
                        bbox[1].min(group[1]),
                        bbox[2].max(group[2]),
                        bbox[3].max(group[3]),
                    ];
                    group = [
                        f32::INFINITY,
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                        f32::NEG_INFINITY,
                    ];
                }
            }
            labels.push(PlacedLabel {
                glyphs,
                boxes,
                bbox,
            });
        }
        labels
    }

    /// Build the triangle batches for one frame. `to_screen` maps mercator
    /// meters to screen pixels relative to `origin`'s frame; `view` is the
    /// canvas size in pixels.
    pub fn frame(
        &mut self,
        cands: &[Candidate<'_>],
        zoom: f64,
        to_screen: impl Fn(f64, f64) -> Pt,
        origin: Pt,
        view: (f32, f32),
        out: &mut FrameOut,
    ) {
        if zoom < MIN_ZOOM {
            return;
        }
        if zoom.to_bits() != self.zoom_bits {
            self.layouts.clear();
            self.zoom_bits = zoom.to_bits();
        }
        let mut placed: Vec<[f32; 4]> = Vec::new();
        for c in cands {
            if c.verts.len() < 2 {
                continue;
            }
            let (rx, ry) = to_screen(c.verts[0].1, c.verts[0].2);
            let sig = geometry_sig(c.verts);
            let fresh = self
                .layouts
                .get(&c.way_id)
                .is_some_and(|e| e.sig == sig && e.text == c.text);
            if !fresh {
                let rel: Vec<Pt> = c
                    .verts
                    .iter()
                    .map(|&(_, x, y)| {
                        let (sx, sy) = to_screen(x, y);
                        (sx - rx, sy - ry)
                    })
                    .collect();
                let labels = self.layout(c.text, &rel);
                self.layouts.insert(
                    c.way_id,
                    CacheEntry {
                        text: c.text.to_string(),
                        sig,
                        labels,
                    },
                );
                out.layouts_built += 1;
            }
            let entry = &self.layouts[&c.way_id];
            for label in &entry.labels {
                let b = label.bbox;
                if rx + b[2] < 0.0 || ry + b[3] < 0.0 || rx + b[0] > view.0 || ry + b[1] > view.1 {
                    continue;
                }
                let hit = label.boxes.iter().any(|lb| {
                    let r = [rx + lb[0], ry + lb[1], rx + lb[2], ry + lb[3]];
                    placed
                        .iter()
                        .any(|p| r[0] < p[2] && r[2] > p[0] && r[1] < p[3] && r[3] > p[1])
                });
                if hit {
                    continue;
                }
                for lb in &label.boxes {
                    placed.push([rx + lb[0], ry + lb[1], rx + lb[2], ry + lb[3]]);
                }
                out.labels += 1;
                let (ax, ay) = (rx + origin.0, ry + origin.1);
                for g in &label.glyphs {
                    let (gx, gy) = (ax + g.x, ay + g.y);
                    let xf = |&(vx, vy): &Pt| {
                        (gx + vx * g.cos - vy * g.sin, gy + vx * g.sin + vy * g.cos)
                    };
                    out.halo.extend(g.mesh.halo.iter().map(xf));
                    out.fill.extend(g.mesh.fill.iter().map(xf));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn straight(len: f32) -> Vec<Pt> {
        vec![(0.0, 0.0), (len / 2.0, 0.0), (len, 0.0)]
    }

    #[test]
    fn flip_keeps_text_upright() {
        assert!(!needs_flip(1.0, 0.0));
        assert!(needs_flip(-1.0, 0.0));
        assert!(!needs_flip(1.0, 5.0));
        assert!(!needs_flip(-0.0, 1.0)); // N-S reading downward
        assert!(needs_flip(0.0, -1.0)); // reading upward is flipped
        assert!(needs_flip(-1.0, -1.0));
        assert!(!needs_flip(-0.00001, 1.0));
    }

    #[test]
    fn flatten_quad_hits_endpoint_and_tolerance() {
        let (p0, p1, p2) = ((0.0, 0.0), (5.0, 10.0), (10.0, 0.0));
        let mut out = Vec::new();
        flatten_quad(p0, p1, p2, 0.05, &mut out);
        assert_eq!(*out.last().unwrap(), p2);
        // Midpoint of the true curve is (5, 5); the polyline must be within
        // tolerance of it.
        let mut prev = p0;
        let mut best = f32::MAX;
        for &p in &out {
            // Distance from (5,5) to segment prev-p.
            let (dx, dy) = (p.0 - prev.0, p.1 - prev.1);
            let t =
                (((5.0 - prev.0) * dx + (5.0 - prev.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            let q = (prev.0 + t * dx, prev.1 + t * dy);
            best = best.min(dist2(q, (5.0, 5.0)).sqrt());
            prev = p;
        }
        assert!(best <= 0.05 + 1e-3, "deviation {best}");
        // Looser tolerance, fewer points.
        let mut coarse = Vec::new();
        flatten_quad(p0, p1, p2, 1.0, &mut coarse);
        assert!(coarse.len() < out.len());
    }

    #[test]
    fn flatten_cubic_hits_endpoint() {
        let mut out = Vec::new();
        flatten_cubic(
            (0.0, 0.0),
            (0.0, 10.0),
            (10.0, 10.0),
            (10.0, 0.0),
            0.1,
            &mut out,
        );
        assert_eq!(*out.last().unwrap(), (10.0, 0.0));
        assert!(out.len() > 3);
        let mut line = Vec::new();
        flatten_cubic(
            (0.0, 0.0),
            (1.0, 0.0),
            (2.0, 0.0),
            (3.0, 0.0),
            0.1,
            &mut line,
        );
        assert_eq!(line.len(), 1);
    }

    #[test]
    fn triangulate_square_with_hole() {
        let outer = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let hole = vec![(3.0, 3.0), (3.0, 7.0), (7.0, 7.0), (7.0, 3.0)];
        let tris = triangulate_contours(&[outer, hole]);
        assert_eq!(tris.len() % 3, 0);
        let area: f32 = tris.chunks(3).map(|t| signed_area(t).abs()).sum();
        assert!((area - (100.0 - 16.0)).abs() < 1e-3, "area {area}");
    }

    #[test]
    fn straight_way_gets_a_centred_window() {
        let pts = straight(200.0);
        let m = metrics(&pts);
        let w = choose_windows(&m, 60.0, 300.0, 10);
        assert_eq!(w.len(), 1);
        assert!((w[0] + 30.0 - 100.0).abs() <= 4.0);
    }

    #[test]
    fn too_short_way_gets_nothing() {
        let m = metrics(&straight(60.0));
        assert!(choose_windows(&m, 60.0, 300.0, 10).is_empty());
        assert!(choose_windows(&m, 50.0, 300.0, 10).is_empty()); // 50 * 1.2 = 60 fits
    }

    #[test]
    fn long_way_repeats_labels_without_overlap() {
        let m = metrics(&straight(1500.0));
        let w = choose_windows(&m, 80.0, 300.0, 10);
        assert!(w.len() >= 3);
        for pair in w.windows(2) {
            assert!(pair[1] >= pair[0] + 80.0);
        }
    }

    #[test]
    fn sharp_corner_blocks_window_but_leaves_straight_part() {
        // 100 px east, then a 90 degree turn, then 100 px south.
        let pts = vec![(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)];
        let m = metrics(&pts);
        assert!((m.turn[1] - 90.0).abs() < 1e-3);
        assert!(!window_ok(&m, 70.0, 130.0, 25.0, 45.0));
        assert!(window_ok(&m, 0.0, 90.0, 25.0, 45.0));
        let w = choose_windows(&m, 60.0, 300.0, 10);
        assert_eq!(w.len(), 1);
        assert!(w[0] + 60.0 <= 100.0 + 1e-3 || w[0] >= 100.0 - 1e-3);
    }

    #[test]
    fn total_turn_limit_applies() {
        // Three 20 degree bends: each under 25, sum 60 over 45.
        let mut pts = vec![(0.0, 0.0)];
        let mut ang = 0.0f32;
        let mut p = (0.0f32, 0.0f32);
        for _ in 0..4 {
            p = (p.0 + 30.0 * ang.cos(), p.1 + 30.0 * ang.sin());
            pts.push(p);
            ang += 20.0f32.to_radians();
        }
        let m = metrics(&pts);
        assert!(window_ok(&m, 0.0, 120.0, 25.0, 100.0));
        assert!(!window_ok(&m, 0.0, 120.0, 25.0, 45.0));
    }

    #[test]
    fn point_at_interpolates() {
        let pts = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        let m = metrics(&pts);
        let (p, a) = point_at(&pts, &m, 15.0);
        assert!((p.0 - 10.0).abs() < 1e-4 && (p.1 - 5.0).abs() < 1e-4);
        assert!((a - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
    }

    #[test]
    fn layout_flips_westbound_way_and_caches_glyph_meshes() {
        let mut cache = LineLabelCache::default();
        let east: Vec<Pt> = vec![(0.0, 0.0), (150.0, 0.0), (300.0, 0.0)];
        let west: Vec<Pt> = east.iter().rev().copied().collect();
        let a = cache.layout("Main Street", &east);
        let b = cache.layout("Main Street", &west);
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        // Both read left to right: first glyph rotation is 0 either way.
        assert!((a[0].glyphs[0].sin).abs() < 1e-3 && a[0].glyphs[0].cos > 0.99);
        assert!((b[0].glyphs[0].sin).abs() < 1e-3 && b[0].glyphs[0].cos > 0.99);
        assert!(a[0].glyphs[0].x < a[0].glyphs.last().unwrap().x);
        assert!(b[0].glyphs[0].x < b[0].glyphs.last().unwrap().x);
        let n = cache.glyph_cache_len();
        cache.layout("Main Street", &east);
        assert_eq!(cache.glyph_cache_len(), n);
    }

    #[test]
    fn layout_vertical_way_reads_downward() {
        let mut cache = LineLabelCache::default();
        let up: Vec<Pt> = vec![(0.0, 300.0), (0.0, 150.0), (0.0, 0.0)];
        let l = cache.layout("Oak Ave", &up);
        assert_eq!(l.len(), 1);
        // Reads top to bottom: rotation +90 degrees.
        let g = &l[0].glyphs[0];
        assert!(g.cos.abs() < 1e-3 && g.sin > 0.99);
    }

    #[test]
    fn missing_glyph_skips_label() {
        let mut cache = LineLabelCache::default();
        assert!(cache
            .layout("\u{4e2d}\u{6587}", &straight(300.0))
            .is_empty());
    }

    #[test]
    fn frame_skips_colliding_labels_and_reuses_layouts() {
        let mut cache = LineLabelCache::default();
        // Two identical overlapping ways at z17.
        let verts: Vec<(i64, f64, f64)> = vec![(1, 0.0, 0.0), (2, 100.0, 0.0), (3, 200.0, 0.0)];
        let cands = [
            Candidate {
                way_id: 1,
                text: "Elm St",
                verts: &verts,
            },
            Candidate {
                way_id: 2,
                text: "Elm St",
                verts: &verts,
            },
        ];
        let to_screen = |x: f64, y: f64| (x as f32 * 1.5 + 100.0, -(y as f32) * 1.5 + 100.0);
        let mut out = FrameOut::default();
        cache.frame(
            &cands,
            17.0,
            to_screen,
            (0.0, 0.0),
            (800.0, 600.0),
            &mut out,
        );
        assert_eq!(out.labels, 1);
        assert_eq!(out.layouts_built, 2);
        assert!(!out.fill.is_empty() && out.fill.len() % 3 == 0);
        assert!(!out.halo.is_empty());
        let mut out2 = FrameOut::default();
        cache.frame(
            &cands,
            17.0,
            to_screen,
            (0.0, 0.0),
            (800.0, 600.0),
            &mut out2,
        );
        assert_eq!(out2.layouts_built, 0);
        let mut low = FrameOut::default();
        cache.frame(
            &cands,
            14.0,
            to_screen,
            (0.0, 0.0),
            (800.0, 600.0),
            &mut low,
        );
        assert_eq!(low.labels, 0);
    }
}
