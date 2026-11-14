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
use crate::layers::line_labels::{parse_bench, BenchKnobs};
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
/// Below this zoom no kind of label is drawn, so the label pass does nothing:
/// the smallest of the kinds' own minimums (point and area text
/// [`LABEL_MIN_ZOOM`], street names [`crate::layers::line_labels::MIN_ZOOM`],
/// icons [`ICON_MIN_ZOOM`]).
pub const ANY_LABEL_MIN_ZOOM: f64 = LABEL_MIN_ZOOM
    .min(crate::layers::line_labels::MIN_ZOOM)
    .min(ICON_MIN_ZOOM);
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
/// Glyph size of an icon without `icon-width`.
pub const DEFAULT_ICON_PX: f32 = 12.0;
/// Extra backing circle diameter around the glyph.
pub const ICON_BACKING_EXTRA: f32 = 6.0;
/// Length of the fade-in of newly placed labels.
pub const FADE: Duration = Duration::from_millis(140);
/// While zooming, the previous placement stays on screen (re-projected from
/// its Mercator anchors) as long as the zoom differs by at most this much.
pub const ZOOM_KEEP: f64 = 1.0;
/// Longest label, in characters; longer text is cut with an ellipsis.
const MAX_TEXT_CHARS: usize = 32;
/// A previously placed label gets this much extra priority so a pan or a
/// small zoom does not swap which of two overlapping labels wins.
pub const HYSTERESIS_BONUS: i32 = 40;

/// How a label's text is drawn, whatever kind of label it is: the size, the
/// fill colour and the halo (`0xRRGGBB` colours).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextLook {
    pub font_px: f32,
    pub color: u32,
    pub halo_color: u32,
    /// Halo thickness outside the glyphs, in pixels.
    pub halo_radius: f32,
    /// Halo opacity (`text-halo-color` alpha or `text-halo-opacity`).
    pub halo_alpha: f32,
}

impl TextLook {
    /// Defaults per kind of label, for what the stylesheet leaves unset.
    /// Point labels sit on the dark base map, street names on the light road
    /// fill, areas on the tinted area fill.
    pub const POINT: TextLook = TextLook {
        font_px: 12.0,
        color: 0xFFFFFF,
        halo_color: 0x000000,
        halo_radius: 1.5,
        halo_alpha: 0.9,
    };
    pub const AREA: TextLook = TextLook {
        color: 0xE8E8E8,
        ..Self::POINT
    };
    pub const LINE: TextLook = TextLook {
        color: 0x1f2937,
        halo_color: 0xffffff,
        ..Self::POINT
    };

    /// The look a MapCSS style asks for, `defaults` where it is silent.
    pub fn resolve(style: &TextStyle, defaults: TextLook) -> TextLook {
        TextLook {
            font_px: style.font_size.unwrap_or(defaults.font_px),
            color: style.color.unwrap_or(defaults.color),
            halo_color: style.halo_color.unwrap_or(defaults.halo_color),
            halo_radius: style.halo_radius.unwrap_or(defaults.halo_radius),
            halo_alpha: style.halo_opacity.unwrap_or(defaults.halo_alpha),
        }
    }
}

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

/// Clear distance between a marker or icon and its text, in pixels.
pub const TEXT_GAP: f32 = 3.0;

impl Item {
    /// Where the text goes when the item is forced and nothing else is in
    /// the way: its first-choice anchor, clear of the marker. This is what
    /// [`place`] gives a lone forced item, without building a grid; the
    /// per-frame fallback for a selected label that placement dropped uses it.
    pub fn forced_text_rect(&self) -> Option<(Anchor, Rect)> {
        let (w, h, pref) = self.text?;
        Some((
            pref,
            label_rect(pref, self.center, self.marker_r, TEXT_GAP, w, h),
        ))
    }
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
            gap: TEXT_GAP,
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
    pub look: TextLook,
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
        look: TextLook::resolve(style, TextLook::POINT),
        pref: Anchor::from_position(style.position),
        icon,
    })
}

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

#[derive(Clone)]
pub struct PlacedText {
    pub line: Arc<ShapedLine>,
    /// Top-left of the text box relative to the feature's screen position.
    pub dx: f32,
    pub dy: f32,
    pub look: TextLook,
}

impl PlacedText {
    /// Every placed text is built here: `line` with its box's top-left `(dx,
    /// dy)` from the feature's screen position.
    pub fn new(line: Arc<ShapedLine>, dx: f32, dy: f32, look: TextLook) -> Self {
        Self { line, dx, dy, look }
    }

    /// `line` in the box `rect`, for a feature drawn at `center`.
    pub fn in_rect(line: Arc<ShapedLine>, rect: &Rect, center: (f32, f32), look: TextLook) -> Self {
        Self::new(line, rect.x0 - center.0, rect.y0 - center.1, look)
    }
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
    /// When this label started fading in; `None` for one that was already
    /// on screen (it stays fully opaque across a re-placement).
    pub born: Option<Instant>,
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
            born: None,
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
            born: None,
        }
    }
}

/// Opacity of a label born at `born` (`None`: already opaque) at `now`.
pub fn fade_alpha(born: Option<Instant>, now: Instant) -> f32 {
    match born {
        None => 1.0,
        Some(b) => {
            (now.saturating_duration_since(b).as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
        }
    }
}

/// Which labels the last placement showed, so a new placement fades in only
/// the labels that were not already on screen. `K` identifies a label across
/// placements (a feature key for point labels, `(way, slot)` for street
/// names).
#[derive(Debug, Clone)]
pub struct FadeSet<K> {
    /// Zoom of the last placement.
    zoom: f64,
    shown: HashSet<K>,
}

impl<K> Default for FadeSet<K> {
    fn default() -> Self {
        Self {
            zoom: 0.0,
            shown: HashSet::new(),
        }
    }
}

impl<K: std::hash::Hash + Eq> FadeSet<K> {
    /// Record `items` as the labels now shown, placed at `zoom`. An item
    /// that was shown at a zoom within `keep` levels of this one gets
    /// `set_born(item, None)` (it stays opaque); every other gets
    /// `set_born(item, Some(now))` and fades in over `fade`. Returns when
    /// the last fade-in finishes, `None` if nothing is new.
    #[allow(clippy::too_many_arguments)]
    pub fn replace<T>(
        &mut self,
        items: &mut [T],
        zoom: f64,
        keep: f64,
        now: Instant,
        fade: Duration,
        key: impl Fn(&T) -> K,
        set_born: impl Fn(&mut T, Option<Instant>),
    ) -> Option<Instant> {
        let carry = (self.zoom - zoom).abs() <= keep;
        let mut shown = HashSet::with_capacity(items.len());
        let mut any = false;
        for item in items {
            let k = key(item);
            if carry && self.shown.contains(&k) {
                set_born(item, None);
            } else {
                set_born(item, Some(now));
                any = true;
            }
            shown.insert(k);
        }
        self.shown = shown;
        self.zoom = zoom;
        any.then(|| now + fade)
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
    /// When the last fade-in finishes; frames are requested until then.
    pub fade_end: Option<Instant>,
    /// How this placement's text halos are drawn (see
    /// [`LabelTuning::halo_for`]).
    pub halo: HaloMode,
}

impl Placement {
    /// The screen rectangles the placed icons and texts occupy, for a later
    /// pass (street names) to avoid. `to_screen` maps a label's Mercator
    /// anchor to screen pixels.
    pub fn occupied(&self, to_screen: impl Fn(f64, f64) -> (f32, f32)) -> Vec<Rect> {
        let mut out = Vec::new();
        for l in &self.labels {
            let (cx, cy) = to_screen(l.mx, l.my);
            if let Some(icon) = &l.icon {
                // The icon backing plus the padding placement keeps clear.
                let r = icon.choice.backing() / 2.0 + 1.0;
                out.push(Rect::around(cx, cy, r, r));
            }
            if let Some(t) = &l.text {
                let (w, h) = (
                    f32::from(t.line.width),
                    f32::from(t.line.ascent + t.line.descent),
                );
                out.push(Rect::new(
                    cx + t.dx,
                    cy + t.dy,
                    cx + t.dx + w,
                    cy + t.dy + h,
                ));
            }
        }
        out
    }
}

/// Whether to ask for another frame: while waiting for the camera to settle
/// and while a fade-in runs, never when everything is at rest.
pub fn wants_frame(step: Step, fade_end: Option<Instant>, now: Instant) -> bool {
    matches!(step, Step::DrawAndWait | Step::HideAndWait) || fade_end.is_some_and(|e| now < e)
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

/// Per-layer label state; lives in a `Mutex` on the layer.
pub struct LabelCache {
    pub shapes: ShapeCache,
    pub icons: IconCache,
    /// Lazily resolved labels of nodes seen in view, valid for `built_for`.
    pub nodes: HashMap<i64, Option<Arc<NodeLabel>>>,
    /// Area labels and their cached label points; see [`area_labels`].
    pub areas: AreaCaches,
    /// This layer's overrides (environment and script knobs).
    pub tuning: LabelTuning,
    built_for: Option<(u64, usize, i32)>,
    pub placement: Option<Placement>,
    fade: FadeSet<Key>,
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
            tuning: LabelTuning::from_env(),
            built_for: None,
            placement: None,
            fade: FadeSet::default(),
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
            if settled {
                return Step::Recompute;
            }
            // A zoom in progress: keep showing the old labels, moving with
            // their anchors, unless the scale has changed too much for them
            // to mean anything (or the window was resized).
            let keep =
                p.cam.w == cam.w && p.cam.h == cam.h && (p.cam.zoom - cam.zoom).abs() <= ZOOM_KEEP;
            return if keep {
                Step::DrawAndWait
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

    /// Keep a new placement. Labels that were on screen in the previous one
    /// (at a comparable zoom) stay opaque; the others fade in from `now`.
    pub fn store(&mut self, mut p: Placement, now: Instant) {
        // By (kind, id): area ids may equal node ids.
        p.fade_end = self.fade.replace(
            &mut p.labels,
            p.cam.zoom,
            ZOOM_KEEP,
            now,
            FADE,
            |l| (l.kind, l.id),
            |l, born| l.born = born,
        );
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

/// How a label's halo is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaloMode {
    /// The glyphs repainted at 4 (8 from a radius of 2) offsets: the best
    /// outline, the most expensive.
    Offsets,
    /// One repaint, offset diagonally: a drop shadow.
    Shadow,
    /// One translucent rounded rectangle behind the text.
    Quad,
    /// No halo (for measurement).
    Off,
}

impl HaloMode {
    /// The mode `OSM_GPUI_LABEL_HALO` names.
    pub fn from_name(name: &str) -> Option<HaloMode> {
        match name {
            "offsets" => Some(HaloMode::Offsets),
            "shadow" => Some(HaloMode::Shadow),
            "quad" => Some(HaloMode::Quad),
            "off" => Some(HaloMode::Off),
            _ => None,
        }
    }
}

/// Environment variable forcing the halo of point and area labels
/// (`offsets` | `shadow` | `quad` | `off`).
pub const HALO_ENV: &str = "OSM_GPUI_LABEL_HALO";

/// The outline (`Offsets`) is the most legible but paints every glyph several
/// times, so it is used only while there are few labels; with more than
/// [`OUTLINE_MAX_LABELS`] placed, the much cheaper backing quad takes over.
pub const OUTLINE_MAX_LABELS: usize = 40;

/// What a layer's labels are tuned with: overrides for measuring, never
/// changed by normal use. Owned by the layer's [`LabelCache`] and passed
/// wherever placement or painting needs it, so two layers never share a knob.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelTuning {
    /// Forces how point and area labels draw their halo; `None` picks by
    /// label count (see [`LabelTuning::halo_for`]).
    pub halo_override: Option<HaloMode>,
    /// Text labels up to which the outline halo is used.
    pub outline_max_labels: usize,
    /// Street-name benchmark knobs, set by the `LabelBench` script action.
    pub bench: BenchKnobs,
}

impl Default for LabelTuning {
    fn default() -> Self {
        Self {
            halo_override: None,
            outline_max_labels: OUTLINE_MAX_LABELS,
            bench: BenchKnobs::default(),
        }
    }
}

impl LabelTuning {
    /// The defaults with `OSM_GPUI_LABEL_HALO` applied; read once, when the
    /// layer's label cache is made.
    pub fn from_env() -> Self {
        Self {
            halo_override: std::env::var(HALO_ENV)
                .ok()
                .and_then(|v| HaloMode::from_name(&v)),
            ..Self::default()
        }
    }

    /// The halo for a placement with `n_text` text labels.
    pub fn halo_for(&self, n_text: usize) -> HaloMode {
        self.halo_override
            .unwrap_or(if n_text <= self.outline_max_labels {
                HaloMode::Offsets
            } else {
                HaloMode::Quad
            })
    }

    /// The knobs street names are placed and painted with. Street names do
    /// not follow `halo_override` or `outline_max_labels` (their halo is a
    /// cached mesh, see [`crate::layers::line_labels::MeshHalo`]); to make them follow, map those here.
    pub fn line_knobs(&self) -> BenchKnobs {
        self.bench
    }

    /// Apply a `LabelBench` spec (see [`parse_bench`]).
    pub fn set_bench(&mut self, spec: &str) -> Result<(), String> {
        let mut k = parse_bench(spec)?;
        k.repeat = k.repeat.clamp(1, 255);
        self.bench = k;
        Ok(())
    }

    /// Differs between any two settings, so a placement keyed on it is
    /// redone when a knob changes. Cheap enough to compute every frame.
    pub fn stamp(&self) -> usize {
        let halo = match self.halo_override {
            None => 0,
            Some(m) => m as u64 + 1,
        };
        let h = (self.bench.stamp() as u64)
            ^ halo.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (self.outline_max_labels as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        (h as usize) << 3
    }
}

/// Offsets (in pixels) at which the halo is painted for radius `r`: the four
/// axis directions, plus the diagonals from a radius of 2 (each offset is one
/// more pass over every glyph, so the default 1.5 px halo is four passes).
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

/// Paint `text` with its top-left at `top_left`, at opacity `alpha`: the halo
/// first (see [`HaloMode`]), then the fill.
pub fn paint_text(
    window: &mut Window,
    t: &PlacedText,
    top_left: Point<Pixels>,
    alpha: f32,
    mode: HaloMode,
) {
    let baseline = point(top_left.x, top_left.y + t.line.ascent);
    let look = &t.look;
    if look.halo_radius > 0.0 {
        let halo = rgb_hsla(look.halo_color, look.halo_alpha * alpha);
        match mode {
            HaloMode::Offsets => {
                for (ox, oy) in halo_offsets(look.halo_radius) {
                    paint_glyphs(
                        window,
                        &t.line,
                        point(baseline.x + px(ox), baseline.y + px(oy)),
                        halo,
                    );
                }
            }
            HaloMode::Shadow => {
                let d = look.halo_radius * std::f32::consts::FRAC_1_SQRT_2;
                paint_glyphs(
                    window,
                    &t.line,
                    point(baseline.x + px(d), baseline.y + px(d)),
                    halo,
                );
            }
            HaloMode::Quad => {
                let pad = look.halo_radius + 1.0;
                let b = Bounds {
                    origin: point(top_left.x - px(pad), top_left.y),
                    size: size(t.line.width + px(2.0 * pad), t.line.ascent + t.line.descent),
                };
                let mut c = halo;
                c.a *= 0.6;
                window.paint_quad(gpui::fill(b, c).corner_radii(px(3.0)));
            }
            HaloMode::Off => {}
        }
    }
    paint_glyphs(window, &t.line, baseline, rgb_hsla(look.color, alpha));
}

/// Paint an icon centred on `center`: a round backing in `fill` with a dark
/// outline, then the glyph in white.
pub fn paint_icon(
    window: &mut Window,
    cx: &gpui::App,
    icon: &PlacedIcon,
    center: Point<Pixels>,
    alpha: f32,
) {
    let d = icon.choice.backing();
    let b = Bounds {
        origin: point(center.x - px(d / 2.0), center.y - px(d / 2.0)),
        size: size(px(d), px(d)),
    };
    window.paint_quad(quad(
        b,
        px(d / 2.0),
        rgb_hsla(icon.fill, alpha),
        px(1.0),
        rgb_hsla(0x1C1C1E, alpha),
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
        rgb_hsla(0xFFFFFF, alpha),
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
        // Two labels that both want the right-hand spot.
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
        c.store(
            Placement {
                cam: cam(17.0, 0.0),
                revision: 1,
                sheet: 1,
                labels: vec![],
                by_id: HashMap::new(),
                fade_end: None,
                halo: HaloMode::Offsets,
            },
            t0,
        );
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
        c.store(
            Placement {
                cam: cam(17.0, 0.0),
                revision: 1,
                sheet: 1,
                labels: vec![],
                by_id: HashMap::new(),
                fade_end: None,
                halo: HaloMode::Offsets,
            },
            t0,
        );
        let _ = c.step(cam(17.0, 0.0), 1, 1, t0);
        // A zoom in progress keeps the old labels, moving with their anchors.
        assert_eq!(c.step(cam(17.2, 0.0), 1, 1, t0), Step::DrawAndWait);
        // A zoom far from the placement's hides them.
        assert_eq!(c.step(cam(19.0, 0.0), 1, 1, t0), Step::HideAndWait);
        let _ = c.step(cam(17.2, 0.0), 1, 1, t0);
        assert_eq!(
            c.step(cam(17.2, 0.0), 1, 1, t0 + Duration::from_millis(300)),
            Step::Recompute
        );
        c.store(
            Placement {
                cam: cam(17.2, 0.0),
                revision: 1,
                sheet: 1,
                labels: vec![],
                by_id: HashMap::new(),
                fade_end: None,
                halo: HaloMode::Offsets,
            },
            t0,
        );
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

    fn placed(id: i64) -> PlacedLabel {
        PlacedLabel::new(id, 0.0, 0.0, None, None)
    }

    fn placement(zoom: f64, ids: &[i64]) -> Placement {
        Placement {
            cam: cam(zoom, 0.0),
            revision: 1,
            sheet: 1,
            labels: ids.iter().map(|&i| placed(i)).collect(),
            by_id: ids.iter().enumerate().map(|(n, &i)| (i, n)).collect(),
            fade_end: None,
            halo: HaloMode::Offsets,
        }
    }

    #[test]
    fn fade_alpha_ramps_from_zero_to_one() {
        let t0 = Instant::now();
        assert_eq!(fade_alpha(None, t0), 1.0);
        assert_eq!(fade_alpha(Some(t0), t0), 0.0);
        let half = fade_alpha(Some(t0), t0 + FADE / 2);
        assert!((half - 0.5).abs() < 0.01, "{half}");
        assert_eq!(fade_alpha(Some(t0), t0 + FADE * 3), 1.0);
        // A clock that steps back does not go negative.
        assert_eq!(fade_alpha(Some(t0 + FADE), t0), 0.0);
    }

    #[test]
    fn the_first_placement_fades_in_entirely() {
        let t0 = Instant::now();
        let mut c = LabelCache::default();
        c.store(placement(17.0, &[1, 2]), t0);
        let p = c.placement.as_ref().unwrap();
        assert!(p.labels.iter().all(|l| l.born == Some(t0)));
        assert_eq!(p.fade_end, Some(t0 + FADE));
    }

    #[test]
    fn a_replacement_fades_in_only_the_new_labels() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);
        let mut c = LabelCache::default();
        c.store(placement(17.0, &[1, 2]), t0);
        // After a zoom to 17.4 the same labels stay opaque; 3 is new.
        c.store(placement(17.4, &[1, 3]), t1);
        let p = c.placement.as_ref().unwrap();
        assert_eq!(p.labels[0].born, None, "label 1 was on screen");
        assert_eq!(p.labels[1].born, Some(t1), "label 3 is new");
        assert_eq!(p.fade_end, Some(t1 + FADE));
        // Nothing new: no fade, so no frames are requested.
        c.store(placement(17.4, &[1, 3]), t1 + Duration::from_secs(1));
        let p = c.placement.as_ref().unwrap();
        assert!(p.labels.iter().all(|l| l.born.is_none()));
        assert_eq!(p.fade_end, None);
    }

    #[test]
    fn area_labels_are_kept_across_a_replacement_by_kind_and_id() {
        let t0 = Instant::now();
        let mut c = LabelCache::default();
        let mut p = placement(17.0, &[7]);
        let mut area = p.labels[0].clone();
        area.kind = KIND_WAY; // same id as the node
        p.labels.push(area);
        c.store(p, t0);
        let mut p2 = placement(17.2, &[7]);
        let mut area2 = p2.labels[0].clone();
        area2.kind = KIND_WAY;
        p2.labels.push(area2);
        let mut other = p2.labels[0].clone();
        other.kind = KIND_REL;
        p2.labels.push(other);
        c.store(p2, t0 + Duration::from_secs(1));
        let l = &c.placement.as_ref().unwrap().labels;
        assert_eq!(
            (l[0].born, l[1].born),
            (None, None),
            "node 7 and way 7 stay"
        );
        assert!(l[2].born.is_some(), "relation 7 is new");
    }

    #[test]
    fn a_far_zoom_fades_everything_in_again() {
        let t0 = Instant::now();
        let mut c = LabelCache::default();
        c.store(placement(15.0, &[1, 2]), t0);
        c.store(placement(18.0, &[1, 2]), t0 + Duration::from_secs(1));
        let p = c.placement.as_ref().unwrap();
        assert!(p.labels.iter().all(|l| l.born.is_some()));
    }

    #[test]
    fn a_resize_hides_instead_of_scaling() {
        let t0 = Instant::now();
        let mut c = LabelCache::default();
        c.store(placement(17.0, &[1]), t0);
        let _ = c.step(cam(17.0, 0.0), 1, 1, t0);
        let mut resized = cam(17.0, 0.0);
        resized.w = 900.0;
        assert_eq!(c.step(resized, 1, 1, t0), Step::HideAndWait);
    }

    #[test]
    fn frames_are_requested_only_while_something_is_in_flight() {
        let t0 = Instant::now();
        assert!(!wants_frame(Step::Draw, None, t0), "at rest");
        assert!(!wants_frame(Step::Draw, Some(t0), t0 + FADE), "fade done");
        assert!(wants_frame(Step::Draw, Some(t0 + FADE), t0), "fading");
        assert!(wants_frame(Step::DrawAndWait, None, t0), "settling");
        assert!(wants_frame(Step::HideAndWait, None, t0));
        assert!(!wants_frame(Step::Recompute, None, t0));
    }

    /// The zoom at which each kind of label first appears, found by asking
    /// the code that gates it on a half-zoom grid.
    #[test]
    fn effective_minimum_zoom_per_kind() {
        use crate::layers::{area_labels, line_labels};
        let first_zoom = |shown: &mut dyn FnMut(f64) -> bool| {
            (28..=40)
                .map(|z| z as f64 / 2.0)
                .find(|&z| shown(z))
                .unwrap()
        };
        let t = tags(&[("amenity", "cafe"), ("name", "Joe")]);
        let st = style(
            "node[amenity] { text: name; icon-image: preset; }",
            &t,
            20.0,
        );
        let node = |z: f64| {
            resolve_node_label(&st, &t, &mut IconCache::new(), z)
                .map(|l| (l.text.is_some(), l.icon.is_some()))
        };
        assert_eq!(first_zoom(&mut |z| node(z).is_some_and(|l| l.0)), 16.0);
        assert_eq!(first_zoom(&mut |z| node(z).is_some_and(|l| l.1)), 17.0);
        let area_style = TextStyle {
            text: Some("name".into()),
            ..TextStyle::default()
        };
        let area = |z: f64| area_labels::resolve_area_label(&area_style, &t, z).is_some();
        assert_eq!(first_zoom(&mut |z| area(z)), 16.0);
        let way = [(1, 0.0, 0.0), (2, 100.0, 0.0), (3, 200.0, 0.0)];
        let cand = line_labels::Candidate {
            way_id: 1,
            text: "Elm Street".into(),
            verts: &way,
            style: line_labels::LabelStyle::default(),
            priority: 50,
        };
        let mut cache = line_labels::LineLabelCache::default();
        let mut street = |z: f64| {
            !cache
                .place(
                    std::slice::from_ref(&cand),
                    z,
                    &BenchKnobs::default(),
                    |x, y| (x as f32 * 1.5 + 100.0, -(y as f32) * 1.5 + 100.0),
                    (800.0, 600.0),
                    &[],
                    &HashSet::new(),
                )
                .0
                .is_empty()
        };
        assert_eq!(first_zoom(&mut street), 16.0);
        // The label pass skips everything below the earliest of them.
        assert_eq!(ANY_LABEL_MIN_ZOOM, 16.0);
    }

    #[test]
    fn a_forced_text_rect_is_where_place_puts_a_lone_forced_item() {
        for (pref, marker_r) in [
            (Anchor::Right, 4.0),
            (Anchor::Above, 7.5),
            (Anchor::Left, 0.0),
            (Anchor::Center, 0.0),
        ] {
            let it = Item {
                key: (KIND_NODE, 1),
                center: (200.0, 150.0),
                marker_r,
                icon_r: None,
                text: Some((60.0, 12.0, pref)),
                priority: 400,
                forced: true,
            };
            let placed = place(std::slice::from_ref(&it), &[], &HashMap::new(), &cfg());
            assert_eq!(placed[0].text, it.forced_text_rect(), "{pref:?}");
        }
        let mut none = item(1, 0.0, 0.0, 10.0, 1);
        none.text = None;
        assert_eq!(none.forced_text_rect(), None);
    }

    #[test]
    fn occupied_lists_each_icon_and_text_box_of_a_placement() {
        let icon = PlacedIcon {
            choice: IconChoice {
                path: "x.svg".into(),
                size: 12.0,
            },
            fill: 0,
        };
        let mut p = placement(17.0, &[1, 2]);
        p.labels[0].icon = Some(icon);
        let at = |mx: f64, my: f64| (mx as f32 * 2.0, my as f32 * 2.0);
        let boxes = p.occupied(at);
        // The icon's backing (12 + 6) / 2 plus a pixel of padding, around its anchor.
        assert_eq!(boxes.len(), 1);
        let (cx, cy) = at(p.labels[0].mx, p.labels[0].my);
        assert_eq!(boxes[0], Rect::around(cx, cy, 10.0, 10.0));
    }

    /// `(id, born)` pairs, born starting `None`.
    fn fade_items(ids: &[u32]) -> Vec<(u32, Option<Instant>)> {
        ids.iter().map(|&i| (i, None)).collect()
    }

    fn fade_replace(
        set: &mut FadeSet<u32>,
        items: &mut [(u32, Option<Instant>)],
        zoom: f64,
        now: Instant,
    ) -> Option<Instant> {
        set.replace(items, zoom, 1.0, now, FADE, |i| i.0, |i, b| i.1 = b)
    }

    #[test]
    fn a_fade_set_fades_in_only_what_was_not_shown() {
        let (t0, t1) = (Instant::now(), Instant::now() + Duration::from_secs(1));
        let mut set = FadeSet::default();
        let mut first = fade_items(&[1, 2]);
        assert_eq!(
            fade_replace(&mut set, &mut first, 17.0, t0),
            Some(t0 + FADE)
        );
        assert!(first.iter().all(|i| i.1 == Some(t0)), "everything is new");
        let mut second = fade_items(&[1, 3]);
        assert_eq!(
            fade_replace(&mut set, &mut second, 17.4, t1),
            Some(t1 + FADE)
        );
        assert_eq!(second[0].1, None, "1 was on screen");
        assert_eq!(second[1].1, Some(t1), "3 is new");
        let mut same = fade_items(&[3, 1]);
        assert_eq!(fade_replace(&mut set, &mut same, 17.4, t1), None);
        assert!(same.iter().all(|i| i.1.is_none()));
    }

    #[test]
    fn a_fade_set_forgets_labels_across_a_far_zoom_or_a_gap() {
        let t0 = Instant::now();
        let mut set = FadeSet::default();
        fade_replace(&mut set, &mut fade_items(&[1, 2]), 15.0, t0);
        let mut far = fade_items(&[1, 2]);
        assert!(fade_replace(&mut set, &mut far, 18.0, t0).is_some());
        assert!(far.iter().all(|i| i.1.is_some()), "a far zoom fades all in");
        // Exactly the keep distance still counts as comparable.
        let mut edge = fade_items(&[1, 2]);
        assert_eq!(fade_replace(&mut set, &mut edge, 19.0, t0), None);
        // A label missing from one placement is new when it returns.
        fade_replace(&mut set, &mut fade_items(&[1]), 19.0, t0);
        let mut back = fade_items(&[1, 2]);
        fade_replace(&mut set, &mut back, 19.0, t0);
        assert_eq!((back[0].1, back[1].1), (None, Some(t0)));
    }

    #[test]
    fn a_fade_set_keys_by_whatever_the_caller_says() {
        // Point labels key on (kind, id): equal ids of different kinds differ.
        let t0 = Instant::now();
        let mut set: FadeSet<(u8, i64)> = FadeSet::default();
        let mut items = [((KIND_NODE, 5), None), ((KIND_WAY, 5), None)];
        let go = |set: &mut FadeSet<(u8, i64)>, items: &mut [((u8, i64), Option<Instant>)]| {
            set.replace(items, 17.0, 1.0, t0, FADE, |i| i.0, |i, b| i.1 = b)
        };
        go(&mut set, &mut items);
        let mut next = [((KIND_WAY, 5), None), ((KIND_REL, 5), None)];
        go(&mut set, &mut next);
        assert_eq!((next[0].1, next[1].1), (None, Some(t0)));
    }

    #[test]
    fn the_cheap_halo_takes_over_past_the_outline_budget() {
        let t = LabelTuning::default();
        assert_eq!(t.halo_for(OUTLINE_MAX_LABELS), HaloMode::Offsets);
        assert_eq!(t.halo_for(OUTLINE_MAX_LABELS + 1), HaloMode::Quad);
        let tight = LabelTuning {
            outline_max_labels: 3,
            ..t
        };
        assert_eq!(tight.halo_for(3), HaloMode::Offsets);
        assert_eq!(tight.halo_for(4), HaloMode::Quad);
    }

    #[test]
    fn a_halo_override_wins_over_the_label_count() {
        let t = LabelTuning {
            halo_override: HaloMode::from_name("shadow"),
            ..LabelTuning::default()
        };
        assert_eq!(t.halo_for(0), HaloMode::Shadow);
        assert_eq!(t.halo_for(10_000), HaloMode::Shadow);
        assert_eq!(HaloMode::from_name("off"), Some(HaloMode::Off));
        assert_eq!(HaloMode::from_name("wat"), None);
    }

    #[test]
    fn the_tuning_stamp_changes_with_every_knob() {
        let base = LabelTuning::default();
        let mut seen = vec![base.stamp()];
        let mut check = |t: LabelTuning| {
            let s = t.stamp();
            assert!(!seen.contains(&s), "stamp {s} repeated for {t:?}");
            seen.push(s);
        };
        for spec in [
            "off",
            "halo=none",
            "halo=stroke",
            "halo=offset4",
            "repeat=4",
            "offset=0",
            "offset=9",
        ] {
            let mut t = base;
            t.set_bench(spec).unwrap();
            check(t);
        }
        check(LabelTuning {
            halo_override: Some(HaloMode::Quad),
            ..base
        });
        check(LabelTuning {
            halo_override: Some(HaloMode::Off),
            ..base
        });
        check(LabelTuning {
            outline_max_labels: 5,
            ..base
        });
        // Setting the defaults explicitly is not a change.
        let mut same = base;
        same.set_bench("on halo=coarse repeat=1").unwrap();
        assert_eq!(same.stamp(), base.stamp());
        assert!(LabelTuning::default().set_bench("halo=wat").is_err());
    }

    #[test]
    fn bench_repeat_is_kept_within_what_the_stamp_can_tell_apart() {
        let mut t = LabelTuning::default();
        t.set_bench("repeat=0").unwrap();
        assert_eq!(t.bench.repeat, 1);
        t.set_bench("repeat=1000").unwrap();
        assert_eq!(t.bench.repeat, 255);
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
    fn node_label_look_resolves_defaults_and_every_property() {
        let t = tags(&[("name", "X")]);
        let bare = TextStyle {
            text: Some("name".into()),
            ..TextStyle::default()
        };
        let l = resolve_node_label(&bare, &t, &mut IconCache::new(), 18.0).unwrap();
        assert_eq!(
            (l.look.font_px, l.look.color, l.look.halo_color),
            (12.0, 0xFFFFFF, 0x000000)
        );
        assert_eq!((l.look.halo_radius, l.look.halo_alpha), (1.5, 0.9));
        let all = TextStyle {
            text: Some("name".into()),
            font_size: Some(15.0),
            color: Some(0x112233),
            halo_color: Some(0x445566),
            halo_radius: Some(2.5),
            halo_opacity: Some(0.4),
            ..TextStyle::default()
        };
        let l = resolve_node_label(&all, &t, &mut IconCache::new(), 18.0).unwrap();
        assert_eq!(
            (l.look.font_px, l.look.color, l.look.halo_color),
            (15.0, 0x112233, 0x445566)
        );
        assert_eq!((l.look.halo_radius, l.look.halo_alpha), (2.5, 0.4));
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
