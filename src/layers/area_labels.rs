//! Labels for areas (closed ways and multipolygon relations): buildings,
//! parks, lakes, landuse.
//!
//! The pieces:
//!
//! * [`AreaLabel`] / [`resolve_area_label`]: the viewport-independent text,
//!   colours and priority, from tags and the MapCSS [`TextStyle`] of an `area`
//!   rule (`area|z18-[building] { text: name; }`).
//! * [`AreaGeom`]: the polygons of one area in Mercator metres, each with its
//!   pole of inaccessibility ([`crate::polylabel`]), computed once. The cache
//!   ([`AreaCaches`]) is keyed by a [`Fingerprint`] of the vertex coordinates,
//!   so an edit that moves a vertex invalidates exactly the areas it changed,
//!   with no hooks into the edit paths, and cannot go stale.
//! * [`locate`]: where on screen the text goes and at what size. The text
//!   must fit inside the polygon (an exact rectangle-in-polygon test, not an
//!   inscribed-circle estimate), possibly shrunk a step; otherwise the area
//!   gets no label. A polygon bigger than the screen is clipped to the view
//!   and the label goes at the pole of the visible part, so a large park
//!   keeps its name while panning.
//!
//! The placed label goes through the same collision grid as point labels
//! (`labels::place`) as a [`labels::Anchor::Center`] item.
//!
//! Limits: the text is a single line (no wrapping onto a second line), and
//! the fit test uses the text's predicted size at the shrunk step.

use crate::polylabel::{self, Pole, Pt};
use crate::style::{TextPosition, TextStyle};
use gpui::SharedString;
use std::collections::HashMap;
use std::sync::Arc;

use crate::layers::labels::{label_text, Key, DEFAULT_FONT_PX, DEFAULT_HALO_COLOR, LABEL_MIN_ZOOM};

/// Area text colour when the stylesheet gives none.
pub const DEFAULT_AREA_COLOR: u32 = 0xE8E8E8;
/// Halo radius when the stylesheet gives none.
pub const DEFAULT_AREA_HALO: f32 = 1.5;
/// Smallest font an area label is shrunk to before it is dropped.
pub const MIN_AREA_FONT_PX: f32 = 9.0;
/// Pixels of polygon kept clear around the text.
const FIT_PAD: f32 = 3.0;
/// Pixels from the view edge the text must stay (placement uses 2 + 2).
const EDGE_PAD: f32 = 5.0;
/// Candidate areas considered per placement (top by priority).
pub const MAX_AREA_CANDIDATES: usize = 150;
/// Areas whose visible part is re-analysed (clipped) per placement at most.
pub const CLIP_BUDGET: usize = 6;
/// Polygons of one multipolygon kept (largest inscribed circles first).
const MAX_POLYS: usize = 8;
/// Entries before the geometry cache is dropped wholesale (deleted ways
/// would otherwise accumulate).
const GEOM_CACHE_CAP: usize = 6000;

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// An area's label, independent of the viewport.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaLabel {
    pub text: SharedString,
    pub font_px: f32,
    pub color: u32,
    pub halo_color: u32,
    pub halo_radius: f32,
    /// Tier (see [`area_priority`]); the size bonus is added per placement.
    pub priority: i32,
}

/// Importance of an area's label. All tiers sit below named nodes (400) and
/// above house numbers (100); within a tier bigger areas win (see
/// [`size_bonus`]), and tiers are 20+ apart so the 40-point hysteresis bonus
/// may reorder neighbours but the size bonus (<= 30) keeps classes apart.
pub fn area_priority(tags: &HashMap<String, String>) -> i32 {
    if tags.contains_key("place") {
        return 340;
    }
    let is = |k: &str, vs: &[&str]| tags.get(k).is_some_and(|v| vs.contains(&v.as_str()));
    if is("natural", &["water", "bay", "strait"])
        || is("waterway", &["riverbank", "dock"])
        || is("landuse", &["reservoir", "basin"])
    {
        return 320;
    }
    if is("leisure", &["park", "nature_reserve", "garden", "common"])
        || is("natural", &["wood", "scrub", "heath", "wetland"])
        || is(
            "landuse",
            &["forest", "grass", "meadow", "recreation_ground", "cemetery"],
        )
    {
        return 300;
    }
    if tags.contains_key("building") {
        return 250;
    }
    280
}

/// A bonus for big areas, `0..=30`, from the larger side of the polygon's
/// screen bounding box.
pub fn size_bonus(bbox_px: f32) -> i32 {
    ((bbox_px / 20.0) as i32).clamp(0, 30)
}

/// Build an area's label from the MapCSS properties that apply to it.
/// `None` when the style has no text, the zoom is below the label zoom, the
/// rule asks for a line label (`text-position: line` is for street names),
/// or the tag is missing.
pub fn resolve_area_label(
    style: &TextStyle,
    tags: &HashMap<String, String>,
    zoom: f64,
) -> Option<AreaLabel> {
    if zoom < LABEL_MIN_ZOOM || style.position == Some(TextPosition::Line) {
        return None;
    }
    let text = label_text(style.text.as_deref()?, tags)?;
    Some(AreaLabel {
        text: text.into(),
        font_px: style.font_size.unwrap_or(DEFAULT_FONT_PX),
        color: style.color.unwrap_or(DEFAULT_AREA_COLOR),
        halo_color: style.halo_color.unwrap_or(DEFAULT_HALO_COLOR),
        halo_radius: style.halo_radius.unwrap_or(DEFAULT_AREA_HALO),
        priority: area_priority(tags),
    })
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// A hash of vertex coordinates; two equal fingerprints mean (to within a
/// 64-bit collision) the same geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint(u64);

impl Default for Fingerprint {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fingerprint {
    fn mix(&mut self, v: u64) {
        // FNV-1a over 64-bit words with a multiply-xorshift finish per word.
        self.0 ^= v;
        self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        self.0 ^= self.0 >> 29;
    }

    /// Add one ring (its length too, so ring boundaries matter).
    pub fn ring(&mut self, pts: impl Iterator<Item = Pt>) {
        let mut n = 0u64;
        for (x, y) in pts {
            self.mix(x.to_bits());
            self.mix(y.to_bits());
            n += 1;
        }
        self.mix(n ^ 0x9e37_79b9_7f4a_7c15);
    }
}

/// One polygon of an area with its label point.
#[derive(Debug, Clone)]
pub struct PolyPole {
    /// Outer ring first, then holes; Mercator metres.
    pub rings: Vec<Vec<Pt>>,
    pub pole: Pole,
    /// `(min_x, min_y, max_x, max_y)` of the outer ring.
    pub bbox: (f64, f64, f64, f64),
}

/// The label geometry of a closed way or a multipolygon relation.
#[derive(Debug, Clone)]
pub struct AreaGeom {
    /// Polygons with room for a label, widest inscribed circle first.
    pub polys: Vec<PolyPole>,
}

impl AreaGeom {
    /// Compute poles for `polygons` (each: outer ring then holes). `None`
    /// when no polygon has any interior.
    pub fn build(polygons: Vec<Vec<Vec<Pt>>>) -> Option<AreaGeom> {
        let mut polys: Vec<PolyPole> = Vec::with_capacity(polygons.len());
        for rings in polygons {
            let Some(bbox) = rings.first().and_then(|o| polylabel::bbox(o)) else {
                continue;
            };
            let size = (bbox.2 - bbox.0).min(bbox.3 - bbox.1);
            if size <= 0.0 {
                continue;
            }
            let refs: Vec<&[Pt]> = rings.iter().map(Vec::as_slice).collect();
            // About 1% of the polygon's smaller side: invisible as label
            // jitter, and keeps the search to a few hundred cells.
            let Some(pole) = polylabel::polylabel(&refs, size * 0.01) else {
                continue;
            };
            if pole.dist > 0.0 {
                polys.push(PolyPole { rings, pole, bbox });
            }
        }
        if polys.is_empty() {
            return None;
        }
        polys.sort_by(|a, b| b.pole.dist.total_cmp(&a.pole.dist));
        polys.truncate(MAX_POLYS);
        Some(AreaGeom { polys })
    }
}

/// Resolved labels and label geometry, per area. Lives in the layer's
/// `LabelCache`.
#[derive(Default)]
pub struct AreaCaches {
    /// Text and style, valid for one (data revision, stylesheet, zoom
    /// bucket); cleared by [`AreaCaches::clear_labels`].
    labels: HashMap<Key, Option<Arc<AreaLabel>>>,
    /// Geometry with the fingerprint it was built from. `None` geometry is
    /// remembered too (a degenerate area is not retried every placement).
    geoms: HashMap<Key, (Fingerprint, Option<Arc<AreaGeom>>)>,
    /// Geometries built since creation (tests and the bench read this).
    pub builds: usize,
}

impl AreaCaches {
    pub fn clear_labels(&mut self) {
        self.labels.clear();
    }

    /// The cached label of `key`, resolving it with `f` on a miss.
    pub fn label(
        &mut self,
        key: Key,
        f: impl FnOnce() -> Option<AreaLabel>,
    ) -> Option<Arc<AreaLabel>> {
        self.labels
            .entry(key)
            .or_insert_with(|| f().map(Arc::new))
            .clone()
    }

    /// The geometry of `key` if it was built from `fp`; otherwise build it
    /// with `f` (an edit that moved a vertex changes `fp`).
    pub fn geom(
        &mut self,
        key: Key,
        fp: Fingerprint,
        f: impl FnOnce() -> Option<AreaGeom>,
    ) -> Option<Arc<AreaGeom>> {
        if let Some((have, g)) = self.geoms.get(&key) {
            if *have == fp {
                return g.clone();
            }
        }
        if self.geoms.len() >= GEOM_CACHE_CAP {
            self.geoms.clear();
        }
        self.builds += 1;
        let g = f().map(Arc::new);
        self.geoms.insert(key, (fp, g.clone()));
        g
    }

    pub fn geom_count(&self) -> usize {
        self.geoms.len()
    }
}

// ---------------------------------------------------------------------------
// Locating a label on screen
// ---------------------------------------------------------------------------

/// The view a label is located in: Mercator bounds and the pixel scale.
#[derive(Debug, Clone, Copy)]
pub struct AreaView {
    pub min_x: f64,
    pub max_y: f64,
    /// Pixels per Mercator metre.
    pub ppm: f64,
    pub w: f32,
    pub h: f32,
}

impl AreaView {
    pub fn new(min_x: f64, max_x: f64, max_y: f64, w: f32, h: f32) -> Self {
        Self {
            min_x,
            max_y,
            ppm: w as f64 / (max_x - min_x).max(1e-9),
            w,
            h,
        }
    }

    pub fn to_screen(&self, mx: f64, my: f64) -> (f32, f32) {
        (
            ((mx - self.min_x) * self.ppm) as f32,
            ((self.max_y - my) * self.ppm) as f32,
        )
    }

    /// The window text must stay in, as Mercator `(x0, y0, x1, y1)`.
    fn inner_merc(&self) -> (f64, f64, f64, f64) {
        let m = EDGE_PAD as f64 / self.ppm;
        (
            self.min_x + m,
            self.max_y - (self.h as f64 / self.ppm) + m,
            self.min_x + self.w as f64 / self.ppm - m,
            self.max_y - m,
        )
    }
}

/// Where an area's label goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Located {
    /// Centre of the text, Mercator metres.
    pub mx: f64,
    pub my: f64,
    /// The font scale chosen (1.0 = the style's size).
    pub scale: f32,
    /// Placed at the pole of the visible part rather than the whole
    /// polygon's pole.
    pub clipped: bool,
}

/// Try rectangles of `tw` x `th` pixels (times `s`) around `pole`, then
/// shifted sideways along the text direction, inside `rings` and `win`.
fn fit_at(
    rings: &[&[Pt]],
    pole: &Pole,
    (tw, th): (f32, f32),
    s: f32,
    ppm: f64,
    win: (f64, f64, f64, f64),
) -> Option<(f64, f64)> {
    let hw = ((tw * s) / 2.0 + FIT_PAD) as f64 / ppm;
    let hh = ((th * s) / 2.0 + FIT_PAD) as f64 / ppm;
    let in_win = |cx: f64, cy: f64| {
        cx - hw >= win.0 && cx + hw <= win.2 && cy - hh >= win.1 && cy + hh <= win.3
    };
    let try_at = |cx: f64, cy: f64| {
        in_win(cx, cy) && polylabel::rect_inside(rings, (cx - hw, cy - hh, cx + hw, cy + hh))
    };
    // Fast accept: the rectangle's circumscribed circle is inside the
    // inscribed circle.
    let r_px = ((tw * s / 2.0 + FIT_PAD).powi(2) + (th * s / 2.0 + FIT_PAD).powi(2)).sqrt();
    if pole.dist * ppm >= r_px as f64 && in_win(pole.x, pole.y) {
        return Some((pole.x, pole.y));
    }
    if try_at(pole.x, pole.y) {
        return Some((pole.x, pole.y));
    }
    // Slide along x: an elongated polygon often fits the text off the pole.
    let step = (tw * s) as f64 / 4.0 / ppm;
    for k in 1..=4 {
        for sign in [1.0, -1.0] {
            let cx = pole.x + sign * step * k as f64;
            if try_at(cx, pole.y) {
                return Some((cx, pole.y));
            }
        }
    }
    None
}

/// Where the `tw` x `th` pixel text (at scale 1) of an area goes, trying each
/// font scale in `scales` (largest first), or `None` when it fits nowhere.
/// `forced` (selected or hovered) always yields a position: the pole of the
/// visible part, unfitted, at the first scale. `budget` counts the clipped
/// polygons analysed per placement.
pub fn locate(
    geom: &AreaGeom,
    v: &AreaView,
    (tw, th): (f32, f32),
    scales: &[f32],
    forced: bool,
    budget: &mut usize,
) -> Option<Located> {
    let win = v.inner_merc();
    let view_bbox = (
        v.min_x,
        v.max_y - v.h as f64 / v.ppm,
        v.min_x + v.w as f64 / v.ppm,
        v.max_y,
    );
    for poly in &geom.polys {
        let b = poly.bbox;
        let size_px = |lo: f64, hi: f64| ((hi - lo) * v.ppm) as f32;
        let (bw, bh) = (size_px(b.0, b.2), size_px(b.1, b.3));
        if b.2 < view_bbox.0 || b.0 > view_bbox.2 || b.3 < view_bbox.1 || b.1 > view_bbox.3 {
            continue;
        }
        let refs: Vec<&[Pt]> = poly.rings.iter().map(Vec::as_slice).collect();
        // Whole-polygon pole first (stable while panning).
        for &s in scales {
            if bw < tw * s || bh < th * s {
                continue;
            }
            if let Some((mx, my)) = fit_at(&refs, &poly.pole, (tw, th), s, v.ppm, win) {
                return Some(Located {
                    mx,
                    my,
                    scale: s,
                    clipped: false,
                });
            }
        }
        // The polygon pokes out of the window: label the visible part.
        let pokes_out = b.0 < win.0 || b.1 < win.1 || b.2 > win.2 || b.3 > win.3;
        if pokes_out && (*budget > 0 || forced) {
            *budget = budget.saturating_sub(1);
            let clipped: Vec<Vec<Pt>> = poly
                .rings
                .iter()
                .map(|r| polylabel::clip_ring(r, win))
                .filter(|r| r.len() >= 3)
                .collect();
            // The outer ring must survive (it is first); a vanished outer
            // ring leaves holes only.
            let outer_ok = poly
                .rings
                .first()
                .is_some_and(|o| polylabel::clip_ring(o, win).len() >= 3);
            if !outer_ok || clipped.is_empty() {
                continue;
            }
            let crefs: Vec<&[Pt]> = clipped.iter().map(Vec::as_slice).collect();
            let Some((x0, y0, x1, y1)) = polylabel::bbox(&clipped[0]) else {
                continue;
            };
            let size = (x1 - x0).min(y1 - y0);
            if size <= 0.0 {
                continue;
            }
            let Some(cp) = polylabel::polylabel(&crefs, size * 0.01) else {
                continue;
            };
            if cp.dist <= 0.0 {
                continue;
            }
            for &s in scales {
                if let Some((mx, my)) = fit_at(&crefs, &cp, (tw, th), s, v.ppm, win) {
                    return Some(Located {
                        mx,
                        my,
                        scale: s,
                        clipped: true,
                    });
                }
            }
            if forced {
                return Some(Located {
                    mx: cp.x,
                    my: cp.y,
                    scale: scales.first().copied().unwrap_or(1.0),
                    clipped: true,
                });
            }
        }
    }
    if forced {
        // Selected but nowhere to fit it: the largest polygon's pole.
        let p = geom.polys.first()?.pole;
        return Some(Located {
            mx: p.x,
            my: p.y,
            scale: scales.first().copied().unwrap_or(1.0),
            clipped: false,
        });
    }
    None
}

/// The font scales to try for a label of `font_px`: the full size, then one
/// step smaller if that stays above [`MIN_AREA_FONT_PX`].
pub fn font_scales(font_px: f32) -> Vec<f32> {
    let mut v = vec![1.0];
    if font_px * 0.85 >= MIN_AREA_FONT_PX {
        v.push(0.85);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(x: f64, y: f64, w: f64, h: f64) -> Vec<Pt> {
        vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
    }

    fn geom(rings: Vec<Vec<Pt>>) -> AreaGeom {
        AreaGeom::build(vec![rings]).unwrap()
    }

    /// 1000 m x 800 m view at 1 px = 1 m, origin top-left at (0, 800).
    fn view() -> AreaView {
        AreaView::new(0.0, 1000.0, 800.0, 1000.0, 800.0)
    }

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn text_fits_a_big_building_and_is_hidden_in_a_small_one() {
        let big = geom(vec![sq(400.0, 300.0, 100.0, 60.0)]);
        let small = geom(vec![sq(400.0, 300.0, 20.0, 12.0)]);
        let mut b = CLIP_BUDGET;
        let at = locate(&big, &view(), (60.0, 14.0), &[1.0], false, &mut b).unwrap();
        assert!(
            (at.mx - 450.0).abs() < 2.0 && (at.my - 330.0).abs() < 2.0,
            "{at:?}"
        );
        assert!(!at.clipped && at.scale == 1.0);
        assert!(locate(&small, &view(), (60.0, 14.0), &[1.0, 0.85], false, &mut b).is_none());
    }

    #[test]
    fn a_shrunk_step_is_used_when_only_it_fits() {
        // 70 x 22 interior: 66 px of text at scale 1 (+padding) is too wide,
        // 56 px at 0.85 fits.
        let g = geom(vec![sq(400.0, 300.0, 70.0, 22.0)]);
        let mut b = CLIP_BUDGET;
        let at = locate(&g, &view(), (66.0, 13.0), &[1.0, 0.85], false, &mut b).unwrap();
        assert_eq!(at.scale, 0.85);
        assert!(locate(&g, &view(), (66.0, 13.0), &[1.0], false, &mut b).is_none());
    }

    #[test]
    fn the_label_avoids_a_hole() {
        let outer = sq(300.0, 200.0, 400.0, 400.0);
        let hole = sq(380.0, 280.0, 240.0, 240.0);
        let g = geom(vec![outer, hole]);
        let mut b = CLIP_BUDGET;
        let at = locate(&g, &view(), (50.0, 12.0), &[1.0], false, &mut b).unwrap();
        let in_hole = at.mx > 380.0 && at.mx < 620.0 && at.my > 280.0 && at.my < 520.0;
        assert!(!in_hole, "{at:?}");
    }

    #[test]
    fn a_long_concave_shape_takes_a_slid_label_when_the_pole_is_too_tight() {
        // An L: a tall thin stem (x 100..120) and a long arm (y 100..150).
        // The widest circle sits in the arm's bend; the text is wider than
        // the stem so it must go along the arm.
        let l = vec![
            (100.0, 100.0),
            (500.0, 100.0),
            (500.0, 150.0),
            (120.0, 150.0),
            (120.0, 600.0),
            (100.0, 600.0),
        ];
        let g = geom(vec![l]);
        let mut b = CLIP_BUDGET;
        let at = locate(&g, &view(), (120.0, 14.0), &[1.0], false, &mut b).unwrap();
        // On the arm (y between 100 and 150), not the stem.
        assert!(at.my >= 100.0 + 7.0 && at.my <= 150.0 - 7.0, "{at:?}");
    }

    #[test]
    fn a_giant_polygon_is_labelled_at_the_pole_of_its_visible_part() {
        // A park far larger than the view whose own pole is far off-screen.
        let g = geom(vec![sq(-50_000.0, -50_000.0, 50_000.0 + 300.0, 100_000.0)]);
        assert!(g.polys[0].pole.x < 0.0);
        let mut b = CLIP_BUDGET;
        let at = locate(&g, &view(), (80.0, 14.0), &[1.0], false, &mut b).unwrap();
        assert!(at.clipped);
        assert!(at.mx > 0.0 && at.mx < 300.0, "{at:?}");
        assert!(at.my > 0.0 && at.my < 800.0);
        assert_eq!(b, CLIP_BUDGET - 1);
        // Out of budget: the panned-away fallback is not computed.
        let mut none = 0usize;
        assert!(locate(&g, &view(), (80.0, 14.0), &[1.0], false, &mut none).is_none());
    }

    #[test]
    fn a_polygon_entirely_off_screen_gets_nothing_unless_forced_picks_its_pole() {
        let g = geom(vec![sq(5000.0, 5000.0, 100.0, 100.0)]);
        let mut b = CLIP_BUDGET;
        assert!(locate(&g, &view(), (30.0, 12.0), &[1.0], false, &mut b).is_none());
        assert!(locate(&g, &view(), (30.0, 12.0), &[1.0], true, &mut b).is_some());
    }

    #[test]
    fn forced_areas_get_a_position_even_when_the_text_does_not_fit() {
        let g = geom(vec![sq(400.0, 300.0, 20.0, 12.0)]);
        let mut b = CLIP_BUDGET;
        let at = locate(&g, &view(), (80.0, 14.0), &[1.0], true, &mut b).unwrap();
        assert!((at.mx - 410.0).abs() < 1.0);
    }

    #[test]
    fn degenerate_polygons_have_no_geometry() {
        assert!(AreaGeom::build(vec![vec![vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0)]]]).is_none());
        assert!(AreaGeom::build(vec![vec![]]).is_none());
        assert!(AreaGeom::build(vec![]).is_none());
    }

    #[test]
    fn multipolygon_labels_the_roomiest_polygon() {
        let a = vec![sq(100.0, 100.0, 10.0, 10.0)];
        let b = vec![sq(400.0, 300.0, 80.0, 80.0)];
        let g = AreaGeom::build(vec![a, b]).unwrap();
        assert_eq!(g.polys.len(), 2);
        assert!(g.polys[0].pole.dist > g.polys[1].pole.dist);
        let mut bud = CLIP_BUDGET;
        let at = locate(&g, &view(), (40.0, 12.0), &[1.0], false, &mut bud).unwrap();
        assert!(at.mx > 400.0 && at.mx < 480.0);
    }

    #[test]
    fn geometry_cache_rebuilds_only_when_the_vertices_change() {
        let mut c = AreaCaches::default();
        let key = (1u8, 7i64);
        let ring_a = sq(0.0, 0.0, 50.0, 50.0);
        let mut ring_b = ring_a.clone();
        ring_b[2] = (60.0, 50.0); // a vertex moved
        let fp = |r: &[Pt]| {
            let mut f = Fingerprint::default();
            f.ring(r.iter().copied());
            f
        };
        let build = |r: &[Pt]| AreaGeom::build(vec![vec![r.to_vec()]]);
        let g1 = c.geom(key, fp(&ring_a), || build(&ring_a)).unwrap();
        let g2 = c
            .geom(key, fp(&ring_a), || panic!("must be cached"))
            .unwrap();
        assert!(Arc::ptr_eq(&g1, &g2));
        assert_eq!(c.builds, 1);
        let g3 = c.geom(key, fp(&ring_b), || build(&ring_b)).unwrap();
        assert_eq!(c.builds, 2, "an edit rebuilds");
        assert!(g3.polys[0].bbox.2 > g1.polys[0].bbox.2);
        // Degenerate results are cached too.
        let line = vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)];
        assert!(c.geom((1, 9), fp(&line), || build(&line)).is_none());
        assert!(c
            .geom((1, 9), fp(&line), || panic!("cached None"))
            .is_none());
        assert_eq!(c.builds, 3);
    }

    #[test]
    fn fingerprint_depends_on_coordinates_and_ring_boundaries() {
        let f = |rings: &[&[Pt]]| {
            let mut f = Fingerprint::default();
            for r in rings {
                f.ring(r.iter().copied());
            }
            f
        };
        let a = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)];
        let b = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.000001)];
        assert_eq!(f(&[&a]), f(&[&a]));
        assert_ne!(f(&[&a]), f(&[&b]));
        assert_ne!(f(&[&a[..2], &a[2..]]), f(&[&a]));
    }

    #[test]
    fn labels_resolve_from_area_rules_with_zoom_and_class() {
        let sheet = crate::style::Stylesheet::parse(
            "area|z18-[building] { text: name; font-size: 11; }\n\
             area|z16-[natural=water] { text: name; text-color: #7cc4ff; }",
        )
        .unwrap();
        let bld = tags(&[("building", "yes"), ("name", "Hall")]);
        let z17 = sheet.way_text_style(&bld, true, 17.5);
        assert!(resolve_area_label(&z17, &bld, 17.5).is_none(), "below z18");
        let z18 = sheet.way_text_style(&bld, true, 18.2);
        let l = resolve_area_label(&z18, &bld, 18.2).unwrap();
        assert_eq!(
            (l.text.as_ref(), l.font_px, l.priority),
            ("Hall", 11.0, 250)
        );
        // An unclosed way never matches an `area` rule.
        assert!(sheet.way_text_style(&bld, false, 18.2).is_empty());
        let lake = tags(&[("natural", "water"), ("name", "Mirror Lake")]);
        let st = sheet.way_text_style(&lake, true, 16.0);
        let l = resolve_area_label(&st, &lake, 16.0).unwrap();
        assert_eq!((l.color, l.priority), (0x7cc4ff, 320));
        // No name tag, no label.
        let anon = tags(&[("natural", "water")]);
        assert!(resolve_area_label(&st, &anon, 16.0).is_none());
        // A line label rule is not an area label.
        let line =
            crate::style::Stylesheet::parse("way[name] { text: name; text-position: line; }")
                .unwrap()
                .way_text_style(&lake, true, 17.0);
        assert!(resolve_area_label(&line, &lake, 17.0).is_none());
    }

    #[test]
    fn priorities_sit_between_named_nodes_and_house_numbers() {
        use crate::layers::labels::node_priority;
        let named = node_priority(&tags(&[("man_made", "x")]), false, true);
        let num = node_priority(&tags(&[("addr:housenumber", "1")]), false, false);
        for t in [
            tags(&[("building", "yes")]),
            tags(&[("leisure", "park")]),
            tags(&[("natural", "water")]),
            tags(&[("landuse", "retail")]),
            tags(&[("place", "square")]),
        ] {
            let p = area_priority(&t) + size_bonus(10_000.0);
            assert!(p < named && p > num + HYST, "{t:?} {p}");
        }
        assert!(
            area_priority(&tags(&[("natural", "water")]))
                > area_priority(&tags(&[("building", "yes")])) + 30
        );
    }
    const HYST: i32 = 40;

    #[test]
    fn font_scales_stop_at_the_minimum() {
        assert_eq!(font_scales(12.0), vec![1.0, 0.85]);
        assert_eq!(font_scales(10.0), vec![1.0]);
    }
}
