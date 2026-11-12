//! Point labels (names of nodes) and POI icons.
//!
//! The pieces, from the bottom up:
//!
//! * [`CollisionGrid`]: an insert-only uniform-grid index of screen
//!   rectangles. Generic: line labels can reserve their boxes in one too.
//! * [`place`]: greedy priority placement. Icons first (they win over text),
//!   then text, each trying several [`Anchor`]s. Pure: it works on widths, so
//!   it is unit-testable and benchmarkable without a window.
//! * [`resolve_node_label`]: a node's tags + MapCSS [`TextStyle`] to a
//!   viewport-independent [`NodeLabel`] (text, style, icon, priority).
//! * [`ShapeCache`]: an LRU of shaped text lines.
//! * [`LabelCache`]: the per-layer state: node candidates, the shaped-text
//!   cache, and the last [`Placement`]. Placement is recomputed when the
//!   camera settles, not on every pan frame: a placed label stores its
//!   Mercator anchor and a screen offset, so a pan just re-projects it.
//! * [`crate::layers::area_labels`]: names of closed ways and multipolygons,
//!   placed through the same grid as [`Anchor::Center`] items.
//! * [`paint_text`], [`paint_icon`]: drawing. Text goes through
//!   `Window::paint_glyph` directly (not `ShapedLine::paint`, which opens a
//!   paint layer per call), so a halo is the same glyphs repainted at offsets
//!   in the halo colour.
//!
//! Labels are never hit-tested: nothing here is in any spatial index the hit
//! tests use.

use crate::layers::area_labels::AreaCaches;
use crate::presets;
use crate::style::{TextPosition, TextStyle};
use gpui::{
    point, px, quad, size, BorderStyle, Bounds, Hsla, Pixels, Point, Rgba, ShapedLine,
    SharedString, TransformationMatrix, Window,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Labels are drawn from this zoom up.
pub const LABEL_MIN_ZOOM: f64 = 16.0;
/// POI icons are drawn from this zoom up.
pub const ICON_MIN_ZOOM: f64 = 17.0;
/// Candidates considered per placement (top by priority).
pub const MAX_CANDIDATES: usize = 400;
/// Labels placed per frame at most.
pub const MAX_PLACED: usize = 300;
/// How long the camera must be still before placement is recomputed.
pub const SETTLE: Duration = Duration::from_millis(120);
/// Cell size of the collision grid, in pixels.
pub const GRID_CELL: f32 = 64.0;
/// Shaped lines kept.
pub const SHAPE_CACHE_CAP: usize = 2000;
/// Default font size in pixels.
pub const DEFAULT_FONT_PX: f32 = 12.0;
pub const DEFAULT_TEXT_COLOR: u32 = 0xFFFFFF;
pub const DEFAULT_HALO_COLOR: u32 = 0x000000;
pub const DEFAULT_HALO_RADIUS: f32 = 1.5;
/// Glyph size of an icon without `icon-width`.
pub const DEFAULT_ICON_PX: f32 = 12.0;
/// Extra backing circle diameter around the glyph.
pub const ICON_BACKING_EXTRA: f32 = 6.0;
const HALO_ALPHA: f32 = 0.9;
/// Longest label, in characters; longer text is cut with an ellipsis.
const MAX_TEXT_CHARS: usize = 32;
/// A previously placed label gets this much extra priority so a pan or a
/// small zoom does not swap which of two overlapping labels wins.
pub const HYSTERESIS_BONUS: i32 = 40;

// ---------------------------------------------------------------------------
// Geometry and collision
// ---------------------------------------------------------------------------

/// An axis-aligned screen rectangle (`x0 <= x1`, `y0 <= y1`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    /// A rectangle of half extents `hw`, `hh` around `(cx, cy)`.
    pub fn around(cx: f32, cy: f32, hw: f32, hh: f32) -> Self {
        Self::new(cx - hw, cy - hh, cx + hw, cy + hh)
    }

    pub fn inflate(&self, p: f32) -> Self {
        Self::new(self.x0 - p, self.y0 - p, self.x1 + p, self.y1 + p)
    }

    /// Whether the interiors overlap (touching edges do not).
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }

    pub fn contains(&self, o: &Rect) -> bool {
        o.x0 >= self.x0 && o.y0 >= self.y0 && o.x1 <= self.x1 && o.y1 <= self.y1
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }
}

/// Owner id of an obstacle that belongs to no item.
pub const NO_OWNER: u32 = u32::MAX;

/// An insert-only uniform-grid index of rectangles, each tagged with an
/// owner so an item's own marker never blocks its own label.
#[derive(Debug, Clone)]
pub struct CollisionGrid {
    cell: f32,
    origin: (f32, f32),
    cols: usize,
    rows: usize,
    cells: Vec<Vec<u32>>,
    items: Vec<(Rect, u32)>,
}

impl CollisionGrid {
    /// A grid covering `area` (rectangles outside it are clamped into the
    /// edge cells, which stays correct, just a little slower).
    pub fn new(area: Rect, cell: f32) -> Self {
        let cols = ((area.width() / cell).ceil() as usize).clamp(1, 512);
        let rows = (((area.y1 - area.y0) / cell).ceil() as usize).clamp(1, 512);
        Self {
            cell,
            origin: (area.x0, area.y0),
            cols,
            rows,
            cells: vec![Vec::new(); cols * rows],
            items: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn span(&self, r: &Rect) -> (usize, usize, usize, usize) {
        let cx = |x: f32| {
            (((x - self.origin.0) / self.cell).floor().max(0.0) as usize).min(self.cols - 1)
        };
        let cy = |y: f32| {
            (((y - self.origin.1) / self.cell).floor().max(0.0) as usize).min(self.rows - 1)
        };
        (cx(r.x0), cx(r.x1), cy(r.y0), cy(r.y1))
    }

    pub fn insert(&mut self, r: Rect, owner: u32) {
        let id = self.items.len() as u32;
        self.items.push((r, owner));
        let (c0, c1, r0, r1) = self.span(&r);
        for y in r0..=r1 {
            for x in c0..=c1 {
                self.cells[y * self.cols + x].push(id);
            }
        }
    }

    /// Whether `r` overlaps any rectangle not owned by `ignore`.
    pub fn collides(&self, r: &Rect, ignore: u32) -> bool {
        let (c0, c1, r0, r1) = self.span(r);
        for y in r0..=r1 {
            for x in c0..=c1 {
                for &id in &self.cells[y * self.cols + x] {
                    let (o, owner) = &self.items[id as usize];
                    if *owner != ignore && o.intersects(r) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Which side of its marker a point label sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anchor {
    Right,
    Left,
    Above,
    Below,
    /// Centred on the item's position (area labels). Never falls back to a
    /// side.
    Center,
}

impl Anchor {
    pub fn from_position(p: Option<TextPosition>) -> Anchor {
        match p {
            Some(TextPosition::Left) => Anchor::Left,
            Some(TextPosition::Above) => Anchor::Above,
            Some(TextPosition::Below) => Anchor::Below,
            _ => Anchor::Right,
        }
    }
}

/// The anchors to try, in order: the one used last time (hysteresis), the
/// preferred one, then right, left, above, below.
pub fn anchor_order(pref: Anchor, prev: Option<Anchor>) -> Vec<Anchor> {
    if pref == Anchor::Center {
        return vec![Anchor::Center];
    }
    let mut out = Vec::with_capacity(5);
    for a in prev.into_iter().chain([
        pref,
        Anchor::Right,
        Anchor::Left,
        Anchor::Above,
        Anchor::Below,
    ]) {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// The text box of a `w` x `h` label at `anchor` of a marker of radius
/// `reach` centred on `c`, `gap` pixels clear of it.
pub fn label_rect(anchor: Anchor, c: (f32, f32), reach: f32, gap: f32, w: f32, h: f32) -> Rect {
    let d = reach + gap;
    match anchor {
        Anchor::Right => Rect::new(c.0 + d, c.1 - h / 2.0, c.0 + d + w, c.1 + h / 2.0),
        Anchor::Left => Rect::new(c.0 - d - w, c.1 - h / 2.0, c.0 - d, c.1 + h / 2.0),
        Anchor::Above => Rect::new(c.0 - w / 2.0, c.1 - d - h, c.0 + w / 2.0, c.1 - d),
        Anchor::Below => Rect::new(c.0 - w / 2.0, c.1 + d, c.0 + w / 2.0, c.1 + d + h),
        Anchor::Center => Rect::around(c.0, c.1, w / 2.0, h / 2.0),
    }
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

/// A feature key: `(kind, id)`, so node and way ids never clash.
pub type Key = (u8, i64);
pub const KIND_NODE: u8 = 0;
/// Area labels: a closed way, or a multipolygon relation.
pub const KIND_WAY: u8 = 1;
pub const KIND_REL: u8 = 2;

/// One thing to place: a marker, optionally with an icon and/or a text.
#[derive(Debug, Clone)]
pub struct Item {
    pub key: Key,
    /// Screen position of the feature.
    pub center: (f32, f32),
    /// Radius of the marker drawn there (0 when none is drawn).
    pub marker_r: f32,
    /// Radius of the icon backing, if the item wants an icon.
    pub icon_r: Option<f32>,
    /// `(width, height, preferred anchor)` of the text, if any.
    pub text: Option<(f32, f32, Anchor)>,
    pub priority: i32,
    /// Selected or hovered: placed first and never culled by collision.
    pub forced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// The visible area; text must lie inside it inset by `edge_pad`.
    pub view: Rect,
    pub edge_pad: f32,
    /// Clear distance between a marker or icon and its text.
    pub gap: f32,
    /// Padding added around text boxes when colliding.
    pub text_pad: f32,
    /// Padding added around icon backings when colliding.
    pub icon_pad: f32,
    pub max_text: usize,
}

impl Config {
    pub fn for_view(w: f32, h: f32) -> Self {
        Self {
            view: Rect::new(0.0, 0.0, w, h),
            edge_pad: 2.0,
            gap: 3.0,
            text_pad: 2.0,
            icon_pad: 1.0,
            max_text: MAX_PLACED,
        }
    }
}

/// What the previous placement did with a feature (for hysteresis).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prev {
    pub icon: bool,
    pub anchor: Option<Anchor>,
}

/// The outcome for one [`Item`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    /// Index into the `items` slice.
    pub item: usize,
    pub icon: bool,
    pub text: Option<(Anchor, Rect)>,
}

/// Greedy priority placement. `fixed` are obstacles that belong to no item
/// (markers of nodes without labels). Order: forced first, then priority
/// (plus a bonus for what was placed last time), then key, so the result is
/// deterministic. Icons are placed before any text, so text yields to icons.
pub fn place(
    items: &[Item],
    fixed: &[Rect],
    prev: &HashMap<Key, Prev>,
    cfg: &Config,
) -> Vec<Placed> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    let eff = |i: usize| -> i32 {
        let it = &items[i];
        it.priority
            + if prev.contains_key(&it.key) {
                HYSTERESIS_BONUS
            } else {
                0
            }
    };
    order.sort_by(|&a, &b| {
        items[b]
            .forced
            .cmp(&items[a].forced)
            .then(eff(b).cmp(&eff(a)))
            .then(items[a].key.cmp(&items[b].key))
    });

    let mut grid = CollisionGrid::new(cfg.view.inflate(GRID_CELL), GRID_CELL);
    for r in fixed {
        grid.insert(*r, NO_OWNER);
    }
    for (i, it) in items.iter().enumerate() {
        if it.marker_r > 0.0 {
            grid.insert(
                Rect::around(it.center.0, it.center.1, it.marker_r, it.marker_r),
                i as u32,
            );
        }
    }

    let mut icon_placed = vec![false; items.len()];
    for &i in &order {
        let it = &items[i];
        let Some(r) = it.icon_r else { continue };
        let rect = Rect::around(it.center.0, it.center.1, r, r);
        let padded = rect.inflate(cfg.icon_pad);
        if it.forced || !grid.collides(&padded, i as u32) {
            grid.insert(rect, i as u32);
            icon_placed[i] = true;
        }
    }

    let mut text_placed: Vec<Option<(Anchor, Rect)>> = vec![None; items.len()];
    let inner = Rect::new(
        cfg.view.x0 + cfg.edge_pad,
        cfg.view.y0 + cfg.edge_pad,
        cfg.view.x1 - cfg.edge_pad,
        cfg.view.y1 - cfg.edge_pad,
    );
    let mut n_text = 0usize;
    for &i in &order {
        let it = &items[i];
        let Some((w, h, pref)) = it.text else {
            continue;
        };
        if n_text >= cfg.max_text && !it.forced {
            continue;
        }
        let reach = if icon_placed[i] {
            it.icon_r.unwrap_or(it.marker_r)
        } else {
            it.marker_r
        };
        let prev_anchor = prev.get(&it.key).and_then(|p| p.anchor);
        for a in anchor_order(pref, prev_anchor) {
            let rect = label_rect(a, it.center, reach, cfg.gap, w, h);
            let padded = rect.inflate(cfg.text_pad);
            let fits = inner.contains(&rect);
            if it.forced || (fits && !grid.collides(&padded, i as u32)) {
                grid.insert(padded, i as u32);
                text_placed[i] = Some((a, rect));
                n_text += 1;
                break;
            }
        }
    }

    order
        .into_iter()
        .filter(|&i| icon_placed[i] || text_placed[i].is_some())
        .map(|i| Placed {
            item: i,
            icon: icon_placed[i],
            text: text_placed[i],
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Node label resolution
// ---------------------------------------------------------------------------

/// The icon a node gets.
#[derive(Debug, Clone, PartialEq)]
pub struct IconChoice {
    /// GPUI asset path, `preset-icons/<name>.svg`.
    pub path: SharedString,
    /// Glyph size in pixels.
    pub size: f32,
}

impl IconChoice {
    /// Diameter of the backing circle.
    pub fn backing(&self) -> f32 {
        self.size + ICON_BACKING_EXTRA
    }
}

/// A node's label and icon, independent of the viewport.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeLabel {
    pub text: Option<SharedString>,
    pub font_px: f32,
    pub color: u32,
    pub halo_color: u32,
    pub halo_radius: f32,
    pub pref: Anchor,
    pub icon: Option<IconChoice>,
    pub priority: i32,
}

/// Tag keys that appear in any preset's matching tags. Other tags (names,
/// addresses, sources) cannot change which preset matches, so they are left
/// out of the icon-cache key.
fn preset_keys() -> &'static HashSet<&'static str> {
    static KEYS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    KEYS.get_or_init(|| {
        presets::preset_index()
            .iter()
            .flat_map(|p| p.tags.keys().map(String::as_str))
            .collect()
    })
}

/// Preset icons usable on the map: Maki and Temaki (CC0). The colour iD
/// illustrations and CC BY sets are left to the preset picker.
fn is_map_icon(name: &str) -> bool {
    name.starts_with("maki-") || name.starts_with("temaki-")
}

/// Cache of preset icon choices by the signature of the preset-relevant tags.
pub type IconCache = HashMap<String, Option<&'static str>>;

/// The icon name of the preset matching a point's tags, if it is a map icon.
pub fn preset_icon_name(
    tags: &HashMap<String, String>,
    cache: &mut IconCache,
) -> Option<&'static str> {
    let keys = preset_keys();
    let mut sig: Vec<(&str, &str)> = tags
        .iter()
        .filter(|(k, _)| keys.contains(k.as_str()))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    if sig.is_empty() {
        return None;
    }
    sig.sort_unstable();
    let mut key = String::new();
    for (k, v) in sig {
        key.push_str(k);
        key.push('=');
        key.push_str(v);
        key.push('|');
    }
    if let Some(hit) = cache.get(&key) {
        return *hit;
    }
    let index = presets::preset_index();
    let found = [presets::Geometry::Point, presets::Geometry::Vertex]
        .into_iter()
        .find_map(|g| index.match_feature(tags, g))
        .and_then(|p| p.icon.as_deref())
        .filter(|n| is_map_icon(n) && presets::icon_svg(n).is_some());
    cache.insert(key, found);
    found
}

/// Resolve MapCSS `icon-image` for a node: `preset`, an embedded icon name
/// (optionally `.svg` / `preset-icons/` qualified), or nothing.
pub fn choose_icon(
    image: Option<&str>,
    width: Option<f32>,
    tags: &HashMap<String, String>,
    cache: &mut IconCache,
) -> Option<IconChoice> {
    let image = image?.trim();
    let name: &str = if image.eq_ignore_ascii_case("preset") {
        preset_icon_name(tags, cache)?
    } else {
        let n = image.strip_prefix(presets::ICON_ASSET_DIR).unwrap_or(image);
        let n = n.strip_suffix(".svg").unwrap_or(n);
        presets::icon_svg(n)?;
        presets::PRESET_ICONS
            .binary_search_by(|(k, _)| (*k).cmp(n))
            .ok()
            .map(|i| presets::PRESET_ICONS[i].0)?
    };
    Some(IconChoice {
        path: format!("{}{name}.svg", presets::ICON_ASSET_DIR).into(),
        size: width.unwrap_or(DEFAULT_ICON_PX),
    })
}

/// The label text for `keys` (`name` or `name, ref`): the first non-empty tag.
pub fn label_text(keys: &str, tags: &HashMap<String, String>) -> Option<String> {
    let v = keys
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .find_map(|k| tags.get(k).map(|v| v.trim()).filter(|v| !v.is_empty()))?;
    if v.chars().count() > MAX_TEXT_CHARS {
        let cut: String = v.chars().take(MAX_TEXT_CHARS - 1).collect();
        Some(format!("{cut}\u{2026}"))
    } else {
        Some(v.to_string())
    }
}

/// Importance of a node's label: bigger places first. Tiers are 100 apart so
/// the hysteresis bonus reorders only within a tier.
pub fn node_priority(
    tags: &HashMap<String, String>,
    has_icon: bool,
    has_text_key_name: bool,
) -> i32 {
    if let Some(p) = tags.get("place") {
        return match p.as_str() {
            "city" => 1000,
            "town" => 950,
            "village" => 900,
            "suburb" | "neighbourhood" | "quarter" => 850,
            "hamlet" => 800,
            _ => 700,
        };
    }
    if has_icon {
        500
    } else if has_text_key_name {
        400
    } else {
        100
    }
}

/// Build a node's label from its tags and resolved MapCSS properties.
/// `None` when it has neither text nor an icon.
pub fn resolve_node_label(
    style: &TextStyle,
    tags: &HashMap<String, String>,
    icons: &mut IconCache,
    zoom: f64,
) -> Option<NodeLabel> {
    let text = if zoom >= LABEL_MIN_ZOOM {
        style.text.as_deref().and_then(|k| label_text(k, tags))
    } else {
        None
    };
    let icon = if zoom >= ICON_MIN_ZOOM {
        choose_icon(style.icon_image.as_deref(), style.icon_width, tags, icons)
    } else {
        None
    };
    if text.is_none() && icon.is_none() {
        return None;
    }
    let named = tags.contains_key("name");
    Some(NodeLabel {
        priority: node_priority(tags, icon.is_some(), named),
        text: text.map(SharedString::from),
        font_px: style.font_size.unwrap_or(DEFAULT_FONT_PX),
        color: style.color.unwrap_or(DEFAULT_TEXT_COLOR),
        halo_color: style.halo_color.unwrap_or(DEFAULT_HALO_COLOR),
        halo_radius: style.halo_radius.unwrap_or(DEFAULT_HALO_RADIUS),
        pref: Anchor::from_position(style.position),
        icon,
    })
}

// ---------------------------------------------------------------------------
// Shaping cache
// ---------------------------------------------------------------------------

/// An LRU of shaped lines keyed by text and size.
#[derive(Default)]
pub struct ShapeCache {
    map: HashMap<(SharedString, u32), (Arc<ShapedLine>, u64)>,
    tick: u64,
    pub misses: usize,
}

impl ShapeCache {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The shaped line for `text` at `font_px`, shaping it on a miss.
    pub fn get(&mut self, window: &Window, text: &SharedString, font_px: f32) -> Arc<ShapedLine> {
        self.tick += 1;
        let key = (text.clone(), (font_px * 10.0).round() as u32);
        if let Some((line, used)) = self.map.get_mut(&key) {
            *used = self.tick;
            return line.clone();
        }
        self.misses += 1;
        let font = window.text_style().font();
        let run = gpui::TextRun {
            len: text.len(),
            font,
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = Arc::new(window.text_system().shape_line(
            text.clone(),
            px(font_px),
            &[run],
            None,
        ));
        if self.map.len() >= SHAPE_CACHE_CAP {
            self.evict();
        }
        self.map.insert(key, (line.clone(), self.tick));
        line
    }

    /// Drop the older half.
    fn evict(&mut self) {
        let mut ticks: Vec<u64> = self.map.values().map(|(_, t)| *t).collect();
        ticks.sort_unstable();
        let cut = ticks[ticks.len() / 2];
        self.map.retain(|_, (_, t)| *t > cut);
    }
}

// ---------------------------------------------------------------------------
// Placement results and per-layer state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PlacedText {
    pub line: Arc<ShapedLine>,
    /// Top-left of the text box relative to the feature's screen position.
    pub dx: f32,
    pub dy: f32,
    pub color: u32,
    pub halo_color: u32,
    pub halo_radius: f32,
}

#[derive(Clone)]
pub struct PlacedIcon {
    pub choice: IconChoice,
    /// Backing circle fill (the node's marker colour).
    pub fill: u32,
}

#[derive(Clone)]
pub struct PlacedLabel {
    /// `KIND_NODE` for points; area labels use `KIND_WAY` / `KIND_REL`.
    pub kind: u8,
    pub id: i64,
    pub mx: f64,
    pub my: f64,
    pub icon: Option<PlacedIcon>,
    pub text: Option<PlacedText>,
    anchor: Option<Anchor>,
}

impl PlacedLabel {
    pub fn new(
        id: i64,
        mx: f64,
        my: f64,
        icon: Option<PlacedIcon>,
        text: Option<(PlacedText, Anchor)>,
    ) -> Self {
        let (text, anchor) = match text {
            Some((t, a)) => (Some(t), Some(a)),
            None => (None, None),
        };
        Self {
            kind: KIND_NODE,
            id,
            mx,
            my,
            icon,
            text,
            anchor,
        }
    }

    /// A text-only label of an area (`KIND_WAY` / `KIND_REL`), centred on
    /// the Mercator point `(mx, my)`.
    pub fn new_area(kind: u8, id: i64, mx: f64, my: f64, text: PlacedText) -> Self {
        Self {
            kind,
            id,
            mx,
            my,
            icon: None,
            text: Some(text),
            anchor: Some(Anchor::Center),
        }
    }
}

/// Identifies what a [`Placement`] was computed for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraSig {
    pub zoom: f64,
    /// Mercator-space view origin (min x, max y) and the pixel size.
    pub min_x: f64,
    pub max_y: f64,
    /// Mercator width of the view, for pixels per metre.
    pub span_x: f64,
    pub w: f32,
    pub h: f32,
}

pub struct Placement {
    pub cam: CameraSig,
    pub revision: u64,
    pub sheet: usize,
    pub labels: Vec<PlacedLabel>,
    pub by_id: HashMap<i64, usize>,
}

/// What `LabelCache::step` tells the caller to do this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Zoomed out, nothing to do.
    Off,
    /// Recompute the placement now.
    Recompute,
    /// Draw the stored placement, re-projected.
    Draw,
    /// Draw the stored placement and ask for another frame (waiting to
    /// settle).
    DrawAndWait,
    /// Draw nothing (the scale changed and the camera has not settled) and
    /// ask for another frame.
    HideAndWait,
}

/// Per-layer label state; lives in a `RefCell` on the layer.
pub struct LabelCache {
    pub shapes: ShapeCache,
    pub icons: IconCache,
    /// Lazily resolved labels of nodes seen in view, valid for `built_for`.
    pub nodes: HashMap<i64, Option<Arc<NodeLabel>>>,
    /// Area labels and their cached label points; see [`area_labels`].
    pub areas: AreaCaches,
    built_for: Option<(u64, usize, i32)>,
    pub placement: Option<Placement>,
    last_cam: Option<CameraSig>,
    last_change: Instant,
}

impl Default for LabelCache {
    fn default() -> Self {
        Self {
            shapes: ShapeCache::default(),
            icons: IconCache::new(),
            nodes: HashMap::new(),
            areas: AreaCaches::default(),
            built_for: None,
            placement: None,
            last_cam: None,
            last_change: Instant::now(),
        }
    }
}

impl LabelCache {
    /// Forget resolved node labels if the data revision, stylesheet or zoom
    /// bucket they were resolved for changed.
    pub fn validate(&mut self, revision: u64, sheet: usize, bucket: i32) {
        if self.built_for != Some((revision, sheet, bucket)) {
            self.nodes.clear();
            self.areas.clear_labels();
            self.built_for = Some((revision, sheet, bucket));
        }
    }

    /// Decide what to do this frame (see [`Step`]). `now` is injected for
    /// tests.
    pub fn step(&mut self, cam: CameraSig, revision: u64, sheet: usize, now: Instant) -> Step {
        if self.last_cam != Some(cam) {
            self.last_cam = Some(cam);
            self.last_change = now;
        }
        let settled = now.duration_since(self.last_change) >= SETTLE;
        let Some(p) = &self.placement else {
            return Step::Recompute;
        };
        if p.revision != revision || p.sheet != sheet {
            return Step::Recompute;
        }
        let same_scale = p.cam.zoom == cam.zoom && p.cam.w == cam.w && p.cam.h == cam.h;
        if !same_scale {
            return if settled {
                Step::Recompute
            } else {
                Step::HideAndWait
            };
        }
        if p.cam == cam {
            return Step::Draw;
        }
        // Panned by a pure translation: pixels moved, from the Mercator
        // shift at this zoom (cam.w covers the view's Mercator width).
        let ppm = cam.w as f64 / cam.span_x.max(1e-9);
        let moved = ((cam.min_x - p.cam.min_x)
            .abs()
            .max((cam.max_y - p.cam.max_y).abs()))
            * ppm;
        if moved > 0.4 * cam.w.min(cam.h) as f64 {
            return Step::Recompute;
        }
        if settled {
            Step::Recompute
        } else {
            Step::DrawAndWait
        }
    }

    pub fn store(&mut self, p: Placement) {
        self.placement = Some(p);
    }

    /// The previous placement as hysteresis input.
    pub fn prev_state(&self) -> HashMap<Key, Prev> {
        self.placement
            .as_ref()
            .map(|p| {
                p.labels
                    .iter()
                    .map(|l| {
                        (
                            (l.kind, l.id),
                            Prev {
                                icon: l.icon.is_some(),
                                anchor: l.anchor,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

fn rgb_hsla(c: u32, alpha: f32) -> Hsla {
    let mut h: Hsla = Rgba {
        r: ((c >> 16) & 0xFF) as f32 / 255.0,
        g: ((c >> 8) & 0xFF) as f32 / 255.0,
        b: (c & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
    .into();
    h.a = alpha;
    h
}

fn paint_glyphs(window: &mut Window, line: &ShapedLine, origin: Point<Pixels>, color: Hsla) {
    for run in &line.runs {
        for g in &run.glyphs {
            if g.is_emoji || line.text.as_bytes().get(g.index) == Some(&b' ') {
                continue;
            }
            let at = point(origin.x + g.position.x, origin.y + g.position.y);
            let _ = window.paint_glyph(at, run.font_id, g.id, line.font_size, color);
        }
    }
}

/// Offsets (in pixels) at which the halo is painted for radius `r`: the four
/// axis directions, plus the diagonals from a radius of 2 (each pass costs about 0.3 us per glyph,
/// so the default 1.5 px halo is four passes).
pub fn halo_offsets(r: f32) -> Vec<(f32, f32)> {
    if r <= 0.0 {
        return Vec::new();
    }
    let mut v = vec![(r, 0.0), (-r, 0.0), (0.0, r), (0.0, -r)];
    if r >= 2.0 {
        let d = r * std::f32::consts::FRAC_1_SQRT_2;
        v.extend([(d, d), (-d, d), (d, -d), (-d, -d)]);
    }
    v
}

/// Paint `text` with its top-left at `top_left`: the halo glyphs first, then
/// the fill.
pub fn paint_text(window: &mut Window, t: &PlacedText, top_left: Point<Pixels>) {
    let baseline = point(top_left.x, top_left.y + t.line.ascent);
    let halo = rgb_hsla(t.halo_color, HALO_ALPHA);
    for (ox, oy) in halo_offsets(t.halo_radius) {
        paint_glyphs(
            window,
            &t.line,
            point(baseline.x + px(ox), baseline.y + px(oy)),
            halo,
        );
    }
    paint_glyphs(window, &t.line, baseline, rgb_hsla(t.color, 1.0));
}

/// Paint an icon centred on `center`: a round backing in `fill` with a dark
/// outline, then the glyph in white.
pub fn paint_icon(window: &mut Window, cx: &gpui::App, icon: &PlacedIcon, center: Point<Pixels>) {
    let d = icon.choice.backing();
    let b = Bounds {
        origin: point(center.x - px(d / 2.0), center.y - px(d / 2.0)),
        size: size(px(d), px(d)),
    };
    window.paint_quad(quad(
        b,
        px(d / 2.0),
        rgb_hsla(icon.fill, 1.0),
        px(1.0),
        rgb_hsla(0x1C1C1E, 1.0),
        BorderStyle::Solid,
    ));
    let s = icon.choice.size;
    let gb = Bounds {
        origin: point(center.x - px(s / 2.0), center.y - px(s / 2.0)),
        size: size(px(s), px(s)),
    };
    let _ = window.paint_svg(
        gb,
        icon.choice.path.clone(),
        None,
        TransformationMatrix::unit(),
        rgb_hsla(0xFFFFFF, 1.0),
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn cfg() -> Config {
        Config::for_view(400.0, 300.0)
    }

    fn item(id: i64, x: f32, y: f32, w: f32, priority: i32) -> Item {
        Item {
            key: (KIND_NODE, id),
            center: (x, y),
            marker_r: 4.0,
            icon_r: None,
            text: Some((w, 14.0, Anchor::Right)),
            priority,
            forced: false,
        }
    }

    #[test]
    fn grid_finds_overlaps_and_ignores_touching_and_own() {
        let mut g = CollisionGrid::new(Rect::new(0.0, 0.0, 400.0, 300.0), GRID_CELL);
        g.insert(Rect::new(10.0, 10.0, 50.0, 30.0), 1);
        assert!(g.collides(&Rect::new(40.0, 20.0, 80.0, 40.0), NO_OWNER));
        assert!(
            !g.collides(&Rect::new(50.0, 10.0, 90.0, 30.0), NO_OWNER),
            "touching"
        );
        assert!(
            !g.collides(&Rect::new(40.0, 20.0, 80.0, 40.0), 1),
            "own owner"
        );
        assert!(!g.collides(&Rect::new(200.0, 200.0, 220.0, 210.0), NO_OWNER));
        // A box spanning several cells is found from any of them.
        g.insert(Rect::new(100.0, 100.0, 300.0, 110.0), 2);
        assert!(g.collides(&Rect::new(250.0, 95.0, 255.0, 105.0), NO_OWNER));
        // Outside the grid still collides with itself.
        g.insert(Rect::new(-500.0, -500.0, -490.0, -490.0), 3);
        assert!(g.collides(&Rect::new(-495.0, -495.0, -492.0, -492.0), NO_OWNER));
    }

    #[test]
    fn anchor_order_is_prev_then_pref_then_the_rest() {
        use Anchor::*;
        assert_eq!(anchor_order(Right, None), vec![Right, Left, Above, Below]);
        assert_eq!(anchor_order(Above, None), vec![Above, Right, Left, Below]);
        assert_eq!(
            anchor_order(Right, Some(Below)),
            vec![Below, Right, Left, Above]
        );
    }

    #[test]
    fn label_rect_sits_clear_of_the_marker_on_each_side() {
        let c = (100.0, 100.0);
        let r = label_rect(Anchor::Right, c, 4.0, 3.0, 40.0, 14.0);
        assert_eq!((r.x0, r.x1, r.y0, r.y1), (107.0, 147.0, 93.0, 107.0));
        let l = label_rect(Anchor::Left, c, 4.0, 3.0, 40.0, 14.0);
        assert_eq!((l.x0, l.x1), (53.0, 93.0));
        let a = label_rect(Anchor::Above, c, 4.0, 3.0, 40.0, 14.0);
        assert_eq!((a.x0, a.x1, a.y0, a.y1), (80.0, 120.0, 79.0, 93.0));
        let b = label_rect(Anchor::Below, c, 4.0, 3.0, 40.0, 14.0);
        assert_eq!((b.y0, b.y1), (107.0, 121.0));
    }

    #[test]
    fn a_lone_label_takes_its_preferred_anchor() {
        let items = [item(1, 100.0, 100.0, 40.0, 400)];
        let out = place(&items, &[], &HashMap::new(), &cfg());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text.unwrap().0, Anchor::Right);
    }

    #[test]
    fn a_blocked_anchor_falls_back_to_the_next() {
        // A fixed obstacle sits where the right-hand label would go.
        let items = [item(1, 100.0, 100.0, 40.0, 400)];
        let fixed = [Rect::new(105.0, 90.0, 160.0, 110.0)];
        let out = place(&items, &fixed, &HashMap::new(), &cfg());
        assert_eq!(out[0].text.unwrap().0, Anchor::Left);
    }

    #[test]
    fn a_label_never_covers_its_neighbours_marker() {
        let items = [
            item(1, 100.0, 100.0, 60.0, 400),
            Item {
                text: None,
                ..item(2, 130.0, 100.0, 0.0, 100)
            },
        ];
        let out = place(&items, &[], &HashMap::new(), &cfg());
        let (a, rect) = out.iter().find(|p| p.item == 0).unwrap().text.unwrap();
        assert_ne!(a, Anchor::Right);
        assert!(!rect.intersects(&Rect::around(130.0, 100.0, 4.0, 4.0)));
    }

    #[test]
    fn higher_priority_wins_a_contested_spot_and_ties_break_by_id() {
        // Two labels whose every anchor overlaps: pinned in a corridor.
        let mut a = item(1, 100.0, 100.0, 80.0, 400);
        let mut b = item(2, 101.0, 100.0, 80.0, 900);
        a.marker_r = 0.0;
        b.marker_r = 0.0;
        a.text = Some((80.0, 14.0, Anchor::Right));
        b.text = Some((80.0, 14.0, Anchor::Right));
        let out = place(&[a.clone(), b.clone()], &[], &HashMap::new(), &cfg());
        // Both fit by changing sides, but the high priority one keeps Right.
        let b_anchor = out.iter().find(|p| p.item == 1).unwrap().text.unwrap().0;
        assert_eq!(b_anchor, Anchor::Right);
        // Equal priority: lower id is placed first, so it keeps Right.
        let mut a2 = a;
        a2.priority = 500;
        let mut b2 = b;
        b2.priority = 500;
        let out = place(&[b2.clone(), a2.clone()], &[], &HashMap::new(), &cfg());
        let a_anchor = out.iter().find(|p| p.item == 1).unwrap().text.unwrap().0;
        assert_eq!(a_anchor, Anchor::Right);
        // Input order does not matter.
        let out2 = place(&[a2, b2], &[], &HashMap::new(), &cfg());
        let a_anchor2 = out2.iter().find(|p| p.item == 0).unwrap().text.unwrap().0;
        assert_eq!(a_anchor2, Anchor::Right);
    }

    #[test]
    fn hysteresis_keeps_the_previous_winner_within_a_tier() {
        // Both labels want the same single spot (a wall of fixed boxes
        // leaves only Right free); 1 would win by priority 410 vs 400.
        let mut one = item(1, 100.0, 100.0, 60.0, 410);
        let mut two = item(2, 100.0, 100.0, 60.0, 400);
        one.marker_r = 0.0;
        two.marker_r = 0.0;
        let fixed = [
            Rect::new(0.0, 0.0, 400.0, 88.0),
            Rect::new(0.0, 112.0, 400.0, 300.0),
            Rect::new(0.0, 88.0, 99.0, 112.0),
        ];
        let first = place(&[one.clone(), two.clone()], &fixed, &HashMap::new(), &cfg());
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].item, 0, "higher priority wins cold");
        // Now 2 was on screen last time: it keeps its place against 1.
        let prev: HashMap<Key, Prev> = [(
            two.key,
            Prev {
                icon: false,
                anchor: Some(Anchor::Right),
            },
        )]
        .into();
        let again = place(&[one, two], &fixed, &prev, &cfg());
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].item, 1, "previous winner keeps the spot");
    }

    #[test]
    fn hysteresis_does_not_cross_priority_tiers() {
        let hi = item(1, 100.0, 100.0, 60.0, 500);
        let lo = item(2, 100.0, 100.0, 60.0, 400);
        let mut hi0 = hi.clone();
        let mut lo0 = lo.clone();
        hi0.marker_r = 0.0;
        lo0.marker_r = 0.0;
        let fixed = [
            Rect::new(0.0, 0.0, 400.0, 88.0),
            Rect::new(0.0, 112.0, 400.0, 300.0),
            Rect::new(0.0, 88.0, 99.0, 112.0),
        ];
        let prev: HashMap<Key, Prev> = [(
            lo.key,
            Prev {
                icon: false,
                anchor: Some(Anchor::Right),
            },
        )]
        .into();
        let out = place(&[hi0, lo0], &fixed, &prev, &cfg());
        assert_eq!(out[0].item, 0);
    }

    #[test]
    fn icons_beat_text_and_each_other_by_priority() {
        let mut a = item(1, 100.0, 100.0, 50.0, 500);
        a.icon_r = Some(9.0);
        // A text-only neighbour whose label would sit on the icon.
        let b = item(2, 45.0, 100.0, 60.0, 900);
        let out = place(&[a.clone(), b], &[], &HashMap::new(), &cfg());
        let pa = out.iter().find(|p| p.item == 0).unwrap();
        assert!(
            pa.icon,
            "icon placed even though the text has higher priority"
        );
        let pb = out.iter().find(|p| p.item == 1).unwrap();
        let (_, r) = pb.text.unwrap();
        assert!(!r.intersects(&Rect::around(100.0, 100.0, 9.0, 9.0)));
        // Two icons that overlap: the higher priority one stays.
        let mut c = item(3, 106.0, 100.0, 0.0, 600);
        c.text = None;
        c.icon_r = Some(9.0);
        c.marker_r = 0.0;
        a.marker_r = 0.0;
        let out = place(&[a, c], &[], &HashMap::new(), &cfg());
        let icons: Vec<usize> = out.iter().filter(|p| p.icon).map(|p| p.item).collect();
        assert_eq!(icons, vec![1]);
    }

    #[test]
    fn text_clears_the_icon_not_just_the_marker() {
        let mut a = item(1, 100.0, 100.0, 40.0, 500);
        a.icon_r = Some(9.0);
        let out = place(&[a], &[], &HashMap::new(), &cfg());
        let (_, r) = out[0].text.unwrap();
        assert!(r.x0 >= 100.0 + 9.0 + 3.0 - 1e-3);
    }

    #[test]
    fn labels_stay_inside_the_view_and_the_cap_is_honoured() {
        // Near the right edge, Right does not fit: Left is used.
        let items = [item(1, 380.0, 100.0, 60.0, 400)];
        let out = place(&items, &[], &HashMap::new(), &cfg());
        assert_eq!(out[0].text.unwrap().0, Anchor::Left);
        // Far too many labels: capped.
        let many: Vec<Item> = (0..50)
            .map(|i| {
                item(
                    i,
                    20.0 + (i % 10) as f32 * 38.0,
                    20.0 + (i / 10) as f32 * 50.0,
                    20.0,
                    400,
                )
            })
            .collect();
        let mut c = cfg();
        c.max_text = 7;
        let out = place(&many, &[], &HashMap::new(), &c);
        assert_eq!(out.iter().filter(|p| p.text.is_some()).count(), 7);
    }

    #[test]
    fn forced_items_are_placed_first_even_when_blocked() {
        let mut f = item(1, 100.0, 100.0, 40.0, 100);
        f.forced = true;
        let fixed = [Rect::new(0.0, 0.0, 400.0, 300.0)];
        let out = place(&[f], &fixed, &HashMap::new(), &cfg());
        assert_eq!(out.len(), 1);
        assert!(out[0].text.is_some());
    }

    #[test]
    fn placement_is_deterministic_under_input_reordering() {
        let mut items: Vec<Item> = (0..40)
            .map(|i| {
                item(
                    i,
                    30.0 + (i % 8) as f32 * 45.0,
                    30.0 + (i / 8) as f32 * 40.0,
                    70.0,
                    400,
                )
            })
            .collect();
        let a = place(&items, &[], &HashMap::new(), &cfg());
        let key_set = |items: &[Item], out: &[Placed]| {
            let mut v: Vec<(i64, Anchor)> = out
                .iter()
                .map(|p| (items[p.item].key.1, p.text.unwrap().0))
                .collect();
            v.sort_by_key(|x| x.0);
            v
        };
        let ka = key_set(&items, &a);
        items.reverse();
        let b = place(&items, &[], &HashMap::new(), &cfg());
        assert_eq!(ka, key_set(&items, &b));
    }

    #[test]
    fn label_text_picks_the_first_present_key_and_truncates() {
        let t = tags(&[("ref", "A1"), ("name", "")]);
        assert_eq!(label_text("name, ref", &t).as_deref(), Some("A1"));
        assert_eq!(label_text("name", &t), None);
        let long = tags(&[("name", &"x".repeat(80))]);
        let s = label_text("name", &long).unwrap();
        assert_eq!(s.chars().count(), MAX_TEXT_CHARS);
        assert!(s.ends_with('\u{2026}'));
    }

    fn style(css: &str, t: &HashMap<String, String>, zoom: f64) -> TextStyle {
        crate::style::Stylesheet::parse(css)
            .unwrap()
            .node_text_style(t, zoom)
    }

    #[test]
    fn min_zoom_gates_text_and_icons() {
        let t = tags(&[("amenity", "cafe"), ("name", "Joe")]);
        let st = style(
            "node[amenity] { text: name; icon-image: preset; }",
            &t,
            18.0,
        );
        let mut ic = IconCache::new();
        assert!(resolve_node_label(&st, &t, &mut ic, 15.5).is_none());
        let l16 = resolve_node_label(&st, &t, &mut ic, 16.5).unwrap();
        assert!(
            l16.text.is_some() && l16.icon.is_none(),
            "text only before the icon zoom"
        );
        let l17 = resolve_node_label(&st, &t, &mut ic, 17.0).unwrap();
        assert!(l17.text.is_some() && l17.icon.is_some());
    }

    #[test]
    fn preset_icons_come_from_the_matching_preset_and_are_cached() {
        let mut ic = IconCache::new();
        let cafe = tags(&[
            ("amenity", "cafe"),
            ("name", "Joe"),
            ("addr:housenumber", "5"),
        ]);
        let n = preset_icon_name(&cafe, &mut ic).expect("cafe icon");
        assert!(n.starts_with("maki-") || n.starts_with("temaki-"), "{n}");
        // Names and addresses are not part of the cache key.
        let other = tags(&[("amenity", "cafe"), ("name", "Other")]);
        assert_eq!(preset_icon_name(&other, &mut ic), Some(n));
        assert_eq!(ic.len(), 1);
        assert_eq!(preset_icon_name(&tags(&[("name", "x")]), &mut ic), None);
    }

    #[test]
    fn mapcss_icon_image_overrides_the_preset() {
        let mut ic = IconCache::new();
        let t = tags(&[("amenity", "cafe")]);
        let c = choose_icon(Some("maki-bank"), Some(16.0), &t, &mut ic).unwrap();
        assert_eq!(c.path.as_ref(), "preset-icons/maki-bank.svg");
        assert_eq!(c.size, 16.0);
        let c = choose_icon(Some("preset-icons/maki-bank.svg"), None, &t, &mut ic).unwrap();
        assert_eq!(c.size, DEFAULT_ICON_PX);
        assert!(choose_icon(Some("no-such-icon"), None, &t, &mut ic).is_none());
        assert!(choose_icon(None, None, &t, &mut ic).is_none());
    }

    #[test]
    fn only_cc0_icons_are_chosen_from_presets() {
        let mut ic = IconCache::new();
        // Whatever the preset, an iD / Font Awesome icon is never chosen.
        for t in [
            tags(&[("highway", "residential")]),
            tags(&[("barrier", "gate")]),
            tags(&[("shop", "bakery")]),
        ] {
            if let Some(n) = preset_icon_name(&t, &mut ic) {
                assert!(is_map_icon(n), "{n}");
            }
        }
    }

    #[test]
    fn priority_ranks_places_above_pois_above_names_above_numbers() {
        let city = node_priority(&tags(&[("place", "city")]), false, true);
        let town = node_priority(&tags(&[("place", "town")]), false, true);
        let poi = node_priority(&tags(&[("amenity", "cafe")]), true, true);
        let named = node_priority(&tags(&[("man_made", "x")]), false, true);
        let num = node_priority(&tags(&[("addr:housenumber", "1")]), false, false);
        assert!(city > town && town > poi && poi > named && named > num);
        assert!(poi - named >= HYSTERESIS_BONUS);
    }

    fn cam(zoom: f64, min_x: f64) -> CameraSig {
        CameraSig {
            zoom,
            min_x,
            max_y: 0.0,
            span_x: 400.0,
            w: 800.0,
            h: 600.0,
        }
    }

    #[test]
    fn step_translates_while_panning_and_recomputes_on_settle() {
        let t0 = Instant::now();
        let mut c = LabelCache::default();
        assert_eq!(c.step(cam(17.0, 0.0), 1, 1, t0), Step::Recompute);
        c.store(Placement {
            cam: cam(17.0, 0.0),
            revision: 1,
            sheet: 1,
            labels: vec![],
            by_id: HashMap::new(),
        });
        assert_eq!(c.step(cam(17.0, 0.0), 1, 1, t0), Step::Draw);
        // A tiny pan: translate, ask for a settle frame.
        let m = 1.0; // 1 Mercator metre = a few px at z17: well under 40%.
        assert_eq!(c.step(cam(17.0, m), 1, 1, t0), Step::DrawAndWait);
        // Still panning 50 ms later.
        assert_eq!(
            c.step(cam(17.0, 2.0 * m), 1, 1, t0 + Duration::from_millis(50)),
            Step::DrawAndWait
        );
        // Settled.
        assert_eq!(
            c.step(cam(17.0, 2.0 * m), 1, 1, t0 + Duration::from_millis(200)),
            Step::Recompute
        );
        // A long drag recomputes at once.
        assert_eq!(
            c.step(cam(17.0, 1.0e6), 1, 1, t0 + Duration::from_millis(210)),
            Step::Recompute
        );
    }

    #[test]
    fn step_hides_during_zoom_and_recomputes_on_data_change() {
        let t0 = Instant::now();
        let mut c = LabelCache::default();
        c.store(Placement {
            cam: cam(17.0, 0.0),
            revision: 1,
            sheet: 1,
            labels: vec![],
            by_id: HashMap::new(),
        });
        let _ = c.step(cam(17.0, 0.0), 1, 1, t0);
        assert_eq!(c.step(cam(17.2, 0.0), 1, 1, t0), Step::HideAndWait);
        assert_eq!(
            c.step(cam(17.2, 0.0), 1, 1, t0 + Duration::from_millis(300)),
            Step::Recompute
        );
        c.store(Placement {
            cam: cam(17.2, 0.0),
            revision: 1,
            sheet: 1,
            labels: vec![],
            by_id: HashMap::new(),
        });
        assert_eq!(
            c.step(cam(17.2, 0.0), 2, 1, t0 + Duration::from_millis(310)),
            Step::Recompute,
            "an edit re-places at once"
        );
    }

    #[test]
    fn node_labels_are_invalidated_by_revision_sheet_and_bucket() {
        let mut c = LabelCache::default();
        c.validate(1, 1, 17);
        c.nodes.insert(5, None);
        c.validate(1, 1, 17);
        assert_eq!(c.nodes.len(), 1);
        c.validate(2, 1, 17);
        assert!(c.nodes.is_empty());
        c.nodes.insert(5, None);
        c.validate(2, 1, 18);
        assert!(c.nodes.is_empty());
    }

    #[test]
    fn halo_offsets_scale_with_radius() {
        assert!(halo_offsets(0.0).is_empty());
        assert_eq!(halo_offsets(0.5).len(), 4);
        assert_eq!(halo_offsets(1.5).len(), 4);
        assert_eq!(halo_offsets(2.0).len(), 8);
    }

    fn area_item(id: i64, x: f32, y: f32, w: f32, priority: i32) -> Item {
        Item {
            key: (KIND_WAY, id),
            center: (x, y),
            marker_r: 0.0,
            icon_r: None,
            text: Some((w, 12.0, Anchor::Center)),
            priority,
            forced: false,
        }
    }

    #[test]
    fn an_area_label_is_centred_and_never_slides_to_a_side() {
        let out = place(
            &[area_item(1, 200.0, 150.0, 60.0, 300)],
            &[],
            &HashMap::new(),
            &cfg(),
        );
        let (a, r) = out[0].text.unwrap();
        assert_eq!(a, Anchor::Center);
        assert_eq!((r.x0, r.x1, r.y0, r.y1), (170.0, 230.0, 144.0, 156.0));
        // Blocked: dropped, not moved.
        let fixed = [Rect::new(190.0, 140.0, 210.0, 160.0)];
        assert!(place(
            &[area_item(1, 200.0, 150.0, 60.0, 300)],
            &fixed,
            &HashMap::new(),
            &cfg()
        )
        .is_empty());
        // Near the edge so the centred box leaves the view: dropped.
        assert!(place(
            &[area_item(1, 10.0, 150.0, 60.0, 300)],
            &[],
            &HashMap::new(),
            &cfg()
        )
        .is_empty());
        assert_eq!(
            anchor_order(Anchor::Center, Some(Anchor::Left)),
            vec![Anchor::Center]
        );
    }

    #[test]
    fn area_labels_yield_to_node_labels_and_each_other_by_priority() {
        // The node's label (priority 400) sits where the area's centred text is.
        let mut node = item(1, 150.0, 150.0, 50.0, 400);
        node.marker_r = 3.0;
        let area = area_item(2, 200.0, 150.0, 80.0, 330);
        let out = place(&[node, area], &[], &HashMap::new(), &cfg());
        assert!(
            out.iter().all(|p| p.item != 1),
            "the area label is dropped, the node's wins"
        );
        // Two overlapping areas: the higher tier wins; a node id never clashes
        // with a way id (distinct kinds).
        let lake = area_item(5, 200.0, 150.0, 80.0, 320);
        let bld = Item {
            key: (KIND_NODE, 5),
            ..area_item(5, 210.0, 150.0, 80.0, 250)
        };
        let out = place(&[bld, lake], &[], &HashMap::new(), &cfg());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].item, 1);
    }

    #[test]
    fn a_selected_area_is_kept_even_where_it_collides() {
        let big = area_item(1, 200.0, 150.0, 80.0, 330);
        let mut sel = area_item(2, 210.0, 150.0, 80.0, 250);
        sel.forced = true;
        let out = place(&[big, sel], &[], &HashMap::new(), &cfg());
        assert!(out.iter().any(|p| p.item == 1 && p.text.is_some()));
    }

    #[test]
    fn hysteresis_applies_to_area_keys() {
        let a = area_item(1, 200.0, 150.0, 90.0, 300);
        let b = area_item(2, 215.0, 150.0, 90.0, 320);
        assert_eq!(
            place(&[a.clone(), b.clone()], &[], &HashMap::new(), &cfg())[0].item,
            1
        );
        let prev: HashMap<Key, Prev> = [(
            a.key,
            Prev {
                icon: false,
                anchor: Some(Anchor::Center),
            },
        )]
        .into();
        let out = place(&[a, b], &[], &prev, &cfg());
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].item, 0,
            "the area shown last time keeps its spot within a tier"
        );
    }
}
