//! Minimal hand-rolled MapCSS (JOSM variant) parser.
//!
//! Scope is intentionally small — see `docs/superpowers/plans/issue-18-mapcss.md`:
//!
//! - Selectors: `node` / `way` / `area` (closed ways only; `way:closed`
//!   is an alias for `area`), each with zero or more tag tests
//!   `[key]`, `[key=value]`, `[key!=value]`. Comma-separated lists of
//!   selectors share a declaration block.
//! - Declarations: `color`, `width`, `symbol-size` (plus `width` as an
//!   alias for `symbol-size` on nodes), and `fill-color`/`fill-opacity`
//!   (closed ways only). Other properties are ignored with a single
//!   warning.
//! - Colors: `#rgb`, `#rrggbb`, `rgb(r,g,b)`, `rgba(r,g,b,a)` or one of a
//!   small named set. The alpha of `rgba(..)` becomes `opacity` (for
//!   `color`) or `fill-opacity` (for `fill-color`).
//! - Stroke styling: `casing-width`, `casing-color`, `z-index`, `dashes`
//!   (`dash,gap` in pixels, or `none`) and `opacity`.
//! - Label properties for a later label-rendering pass: `text` (a tag key,
//!   e.g. `name`, or `none`), `font-size`, `text-color`, `text-halo-color`,
//!   `text-halo-radius`, `text-position` (`center` | `line`), `icon-image`
//!   and `icon-width`. They are parsed and resolved into [`TextStyle`] (see
//!   [`Stylesheet::way_text_style`] / [`Stylesheet::node_text_style`]) but
//!   nothing renders them yet.
//! - Comments: `/* ... */` (non-nesting).
//!
//! - Zoom-range selectors after the type: `way|z12-`, `way|z-14`,
//!   `way|z12-16`, `way|z12` (inclusive, tested against the integer zoom
//!   bucket, `floor(zoom)`).
//!
//! Anything outside that subset (other pseudo-classes, `eval(...)`, etc.)
//! is parsed permissively: unknown tokens are skipped and a warning is
//! logged once per parse.
//!
//! # Zoom and caching
//!
//! Resolved styles are cached per feature by the layer. A zoom-dependent
//! stylesheet cannot be cached per zoom per feature, so a feature's cached
//! [`WayStyle`] is resolved ignoring zoom ranges (a superset) and carries a
//! `sig`: an interned id of *which rules matched its tags*. The distinct
//! signatures are few, so [`Stylesheet::zoom_styles`] resolves all of them
//! for one zoom bucket (cached until the bucket changes) and the renderer
//! indexes that table by `sig`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Baseline node style — applied before any rule, so that an empty
/// stylesheet still renders features. Mirrors the old hardcoded
/// defaults in `OsmLayer`.
pub const DEFAULT_NODE_COLOR: u32 = 0xFFD700;
pub const DEFAULT_NODE_SIZE: f32 = 10.0;
pub const DEFAULT_WAY_COLOR: u32 = 0x4169E1;
pub const DEFAULT_WAY_WIDTH: f32 = 4.0;
pub const DEFAULT_FILL_OPACITY: f32 = 0.4;
/// Casing color when `casing-width` is set without `casing-color`.
pub const DEFAULT_CASING_COLOR: u32 = 0x1C1C1E;

/// Resolved area fill for a closed way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fill {
    pub color: u32,
    /// 0.0..=1.0
    pub opacity: f32,
}

/// Resolved node style for a single feature.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeStyle {
    pub color: u32,
    pub size: f32,
    /// Whether the node has any tags. Untagged nodes are plain way
    /// vertices, which the renderer hides when zoomed out. Not settable
    /// from MapCSS; it rides along here so every code path that keeps the
    /// per-node style cache fresh keeps this fresh too.
    pub tagged: bool,
}

impl Default for NodeStyle {
    fn default() -> Self {
        Self {
            color: DEFAULT_NODE_COLOR,
            size: DEFAULT_NODE_SIZE,
            tagged: false,
        }
    }
}

/// Which way direction arrows should point along a way, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Direction carries no meaning; draw no arrows.
    #[default]
    None,
    /// Arrows follow the way's node order.
    Forward,
    /// Arrows run against the way's node order (`oneway=-1`).
    Reverse,
}

impl Direction {
    /// The direction implied by a way's tags: an explicit `oneway`, a
    /// roundabout, or a flowing waterway.
    pub fn from_tags(tags: &HashMap<String, String>) -> Self {
        match tags.get("oneway").map(String::as_str) {
            Some("yes" | "1" | "true") => return Direction::Forward,
            Some("-1" | "reverse") => return Direction::Reverse,
            Some("no" | "0" | "false") => return Direction::None,
            _ => {}
        }
        let roundabout = tags.get("junction").map(String::as_str) == Some("roundabout");
        let flowing = matches!(
            tags.get("waterway").map(String::as_str),
            Some("river" | "stream" | "canal" | "drain" | "ditch")
        );
        if roundabout || flowing {
            Direction::Forward
        } else {
            Direction::None
        }
    }
}

/// Resolved way style for a single feature.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WayStyle {
    pub color: u32,
    pub width: f32,
    /// `Some` only when the way is closed and a `fill-color` applied.
    pub fill: Option<Fill>,
    /// Direction arrows to draw along the way. Derived from tags rather
    /// than MapCSS, for the same cache-freshness reason as
    /// [`NodeStyle::tagged`].
    pub direction: Direction,
    /// Extra width on each side of the stroke drawn as a casing beneath it
    /// (`casing-width`); 0 means no explicit casing.
    pub casing_width: f32,
    pub casing_color: u32,
    /// Draw order (`z-index`): higher draws on top. Fills, then lines in
    /// ascending order, whatever the index; 0 by default.
    pub z_index: f32,
    /// Stroke opacity, 0..=1.
    pub opacity: f32,
    /// `Some((dash, gap))` in pixels for a dashed stroke.
    pub dashes: Option<(f32, f32)>,
    /// Interned id of the rules that matched this way's tags, an index into
    /// [`Stylesheet::zoom_styles`]. [`NO_SIG`] when not interned.
    pub sig: u32,
}

/// `WayStyle::sig` of a style that was not resolved through the cache path.
pub const NO_SIG: u32 = u32::MAX;

impl Default for WayStyle {
    fn default() -> Self {
        Self {
            color: DEFAULT_WAY_COLOR,
            width: DEFAULT_WAY_WIDTH,
            fill: None,
            direction: Direction::None,
            casing_width: 0.0,
            casing_color: DEFAULT_CASING_COLOR,
            z_index: 0.0,
            opacity: 1.0,
            dashes: None,
            sig: NO_SIG,
        }
    }
}

/// Where a label sits relative to its feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextPosition {
    /// Centered on the feature (the default for points and areas).
    Center,
    /// Following the way's line.
    Line,
}

/// Label and icon properties resolved for one feature. Every field is
/// `None` until a rule sets it. Not used by the renderer yet.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextStyle {
    /// Tag key whose value is the label (`text: name`).
    pub text: Option<String>,
    pub font_size: Option<f32>,
    pub color: Option<u32>,
    pub halo_color: Option<u32>,
    pub halo_radius: Option<f32>,
    pub position: Option<TextPosition>,
    pub icon_image: Option<String>,
    pub icon_width: Option<f32>,
}

impl TextStyle {
    /// Whether no label property is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn apply(&mut self, d: &Declaration) {
        match d {
            Declaration::Text(t) => self.text = t.clone(),
            Declaration::FontSize(v) => self.font_size = Some(*v),
            Declaration::TextColor(c) => self.color = Some(*c),
            Declaration::TextHaloColor(c) => self.halo_color = Some(*c),
            Declaration::TextHaloRadius(v) => self.halo_radius = Some(*v),
            Declaration::TextPosition(p) => self.position = Some(*p),
            Declaration::IconImage(i) => self.icon_image = i.clone(),
            Declaration::IconWidth(v) => self.icon_width = Some(*v),
            _ => {}
        }
    }
}

/// Inclusive range of integer zoom buckets a selector applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoomRange {
    pub min: i32,
    pub max: i32,
}

impl ZoomRange {
    pub const ALL: ZoomRange = ZoomRange {
        min: i32::MIN,
        max: i32::MAX,
    };

    fn is_all(&self) -> bool {
        *self == Self::ALL
    }

    fn contains(&self, bucket: i32) -> bool {
        (self.min..=self.max).contains(&bucket)
    }
}

/// The integer zoom bucket of a fractional zoom level.
pub fn zoom_bucket(zoom: f64) -> i32 {
    if zoom.is_finite() {
        zoom.floor() as i32
    } else {
        0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    Node,
    Way,
    /// `area` selector or `way:closed` — matches only closed ways.
    Area,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TagTest {
    /// `[key]` — key must be present with any value.
    Present(String),
    /// `[key=value]` — key must equal value exactly.
    Equals(String, String),
    /// `[key!=value]` — key absent or not equal to value.
    NotEquals(String, String),
}

impl TagTest {
    fn matches(&self, tags: &HashMap<String, String>) -> bool {
        match self {
            TagTest::Present(k) => tags.contains_key(k),
            TagTest::Equals(k, v) => tags.get(k).map(|s| s == v).unwrap_or(false),
            TagTest::NotEquals(k, v) => tags.get(k).map(|s| s != v).unwrap_or(true),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Selector {
    kind: TargetKind,
    tests: Vec<TagTest>,
    zoom: ZoomRange,
}

impl Selector {
    /// Kind and tag tests only; the zoom range is checked separately so a
    /// feature's matches can be cached independent of zoom.
    fn matches(&self, kind: TargetKind, closed: bool, tags: &HashMap<String, String>) -> bool {
        let kind_ok = match self.kind {
            TargetKind::Area => kind == TargetKind::Way && closed,
            k => k == kind,
        };
        kind_ok && self.tests.iter().all(|t| t.matches(tags))
    }
}

/// A single declaration within a rule block.
#[derive(Debug, Clone, PartialEq)]
enum Declaration {
    Color(u32),
    Width(f32),
    /// Node symbol size (also accepts the `width` alias on node rules,
    /// but at parse time we only mark selectors as node/way per rule —
    /// so we always store `SymbolSize` for node-applied width on node
    /// selectors. In mixed-selector rules, `width` is ambiguous, so we
    /// store it as `Width` and let the evaluator decide whether it
    /// applies to way width or node size.
    SymbolSize(f32),
    /// Area fill color; only takes effect on closed ways.
    FillColor(u32),
    /// Area fill opacity, clamped to 0..=1 at parse time.
    FillOpacity(f32),
    /// Extra width of the casing on each side of the stroke, in pixels.
    CasingWidth(f32),
    CasingColor(u32),
    /// Draw order within a feature class; higher draws on top.
    ZIndex(f32),
    /// Stroke opacity, clamped to 0..=1 at parse time.
    Opacity(f32),
    /// `dashes: dash, gap`; `None` for `dashes: none`.
    Dashes(Option<(f32, f32)>),
    /// `text: <tag key>`; `None` for `text: none`.
    Text(Option<String>),
    FontSize(f32),
    TextColor(u32),
    TextHaloColor(u32),
    TextHaloRadius(f32),
    TextPosition(TextPosition),
    /// `icon-image: <path or url>`; `None` for `icon-image: none`.
    IconImage(Option<String>),
    IconWidth(f32),
}

#[derive(Debug, Clone, PartialEq)]
struct Rule {
    selectors: Vec<Selector>,
    declarations: Vec<Declaration>,
}

impl Rule {
    /// Bitmask of the selectors (first 64) whose kind and tags match.
    fn tag_mask(&self, kind: TargetKind, closed: bool, tags: &HashMap<String, String>) -> u64 {
        let mut mask = 0u64;
        for (i, sel) in self.selectors.iter().enumerate().take(64) {
            if sel.matches(kind, closed, tags) {
                mask |= 1 << i;
            }
        }
        mask
    }

    /// Whether any selector in `mask` also covers `zoom` (`None` ignores
    /// zoom ranges).
    fn applies(&self, mask: u64, zoom: Option<i32>) -> bool {
        match zoom {
            None => mask != 0,
            Some(z) => self
                .selectors
                .iter()
                .enumerate()
                .take(64)
                .any(|(i, sel)| mask & (1 << i) != 0 && sel.zoom.contains(z)),
        }
    }
}

/// Which rules matched a feature's tags, ignoring zoom: `(rule index,
/// bitmask of matching selectors)`.
type Matches = Vec<(u32, u64)>;

#[derive(Debug, Default)]
struct SigCache {
    ids: HashMap<(bool, Matches), u32>,
    sigs: Vec<(bool, Matches)>,
    bucket: i32,
    table: Option<Arc<Vec<WayStyle>>>,
}

/// Parsed MapCSS stylesheet.
#[derive(Debug, Default)]
pub struct Stylesheet {
    rules: Vec<Rule>,
    /// Whether any selector carries a zoom range.
    zoom_dependent: bool,
    sigs: Mutex<SigCache>,
}

impl Clone for Stylesheet {
    fn clone(&self) -> Self {
        Self {
            rules: self.rules.clone(),
            zoom_dependent: self.zoom_dependent,
            sigs: Mutex::default(),
        }
    }
}

/// Error from [`Stylesheet::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub position: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MapCSS parse error at {}: {}",
            self.position, self.message
        )
    }
}

impl std::error::Error for ParseError {}

impl Stylesheet {
    /// Parse a MapCSS source string. Unknown properties and selector
    /// features are tolerated with a single warning; hard syntax errors
    /// (missing braces, etc.) return `Err`.
    pub fn parse(src: &str) -> Result<Self, ParseError> {
        let stripped = strip_comments(src);
        let mut p = Parser::new(&stripped);
        let mut rules = Vec::new();
        let mut warned = false;
        while !p.at_end() {
            p.skip_ws();
            if p.at_end() {
                break;
            }
            if let Some(rule) = p.parse_rule(&mut warned)? {
                rules.push(rule)
            }
        }
        let zoom_dependent = rules
            .iter()
            .any(|r| r.selectors.iter().any(|s| !s.zoom.is_all()));
        Ok(Self {
            rules,
            zoom_dependent,
            sigs: Mutex::default(),
        })
    }

    /// Load and parse the compiled-in default stylesheet.
    ///
    /// Panics if the embedded asset fails to parse — that would be a
    /// build-time bug.
    pub fn load_default() -> Self {
        let src = include_str!("../../assets/default.mapcss");
        Self::parse(src).expect("default.mapcss must parse")
    }

    /// Whether any rule has a zoom-range selector. When false, cached
    /// styles never need re-resolving on a zoom change.
    pub fn is_zoom_dependent(&self) -> bool {
        self.zoom_dependent
    }

    /// Resolve a style for a node given its OSM tags at `zoom`.
    pub fn node_style(&self, tags: &HashMap<String, String>, zoom: f64) -> NodeStyle {
        let bucket = zoom_bucket(zoom);
        let mut s = NodeStyle {
            tagged: !tags.is_empty(),
            ..NodeStyle::default()
        };
        for rule in &self.rules {
            let mask = rule.tag_mask(TargetKind::Node, false, tags);
            if mask == 0 || !rule.applies(mask, Some(bucket)) {
                continue;
            }
            for d in &rule.declarations {
                match d {
                    Declaration::Color(c) => s.color = *c,
                    Declaration::SymbolSize(w) => s.size = *w,
                    // `width` on a pure-node rule aliases to symbol-size.
                    Declaration::Width(w)
                        if rule
                            .selectors
                            .iter()
                            .all(|sel| sel.kind == TargetKind::Node) =>
                    {
                        s.size = *w;
                    }
                    _ => {}
                }
            }
        }
        s
    }

    /// Resolve a style for a way given its OSM tags and whether it forms a
    /// closed ring, at `zoom`. `area` and `way:closed` rules only match when
    /// `closed`; fill declarations only take effect on closed ways (JOSM
    /// semantics).
    pub fn way_style(&self, tags: &HashMap<String, String>, closed: bool, zoom: f64) -> WayStyle {
        let matches = self.match_way(tags, closed);
        self.resolve_way(&matches, closed, Some(zoom_bucket(zoom)), tags)
    }

    /// The style for caching per feature: zoom ranges ignored (every rule
    /// that matches the tags applies), with [`WayStyle::sig`] set so the
    /// renderer can fetch the real per-zoom style from
    /// [`Stylesheet::zoom_styles`].
    pub fn way_style_cached(&self, tags: &HashMap<String, String>, closed: bool) -> WayStyle {
        let matches = self.match_way(tags, closed);
        let mut s = self.resolve_way(&matches, closed, None, tags);
        s.sig = self.intern(closed, matches);
        s
    }

    /// Every interned signature resolved at `zoom`, indexed by
    /// [`WayStyle::sig`]. Rebuilt only when the zoom bucket changes or new
    /// signatures appeared. The `direction` of these entries is meaningless;
    /// keep the cached style's.
    pub fn zoom_styles(&self, zoom: f64) -> Arc<Vec<WayStyle>> {
        let bucket = zoom_bucket(zoom);
        let mut cache = self.sigs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = &cache.table {
            if cache.bucket == bucket && t.len() == cache.sigs.len() {
                return t.clone();
            }
        }
        let no_tags = HashMap::new();
        let table: Vec<WayStyle> = cache
            .sigs
            .iter()
            .enumerate()
            .map(|(i, (closed, m))| {
                let mut s = self.resolve_way(m, *closed, Some(bucket), &no_tags);
                s.sig = i as u32;
                s
            })
            .collect();
        let table = Arc::new(table);
        cache.bucket = bucket;
        cache.table = Some(table.clone());
        table
    }

    /// The label/icon properties that apply to a node at `zoom`.
    pub fn node_text_style(&self, tags: &HashMap<String, String>, zoom: f64) -> TextStyle {
        self.text_style(TargetKind::Node, false, tags, zoom)
    }

    /// The label/icon properties that apply to a way at `zoom`.
    pub fn way_text_style(
        &self,
        tags: &HashMap<String, String>,
        closed: bool,
        zoom: f64,
    ) -> TextStyle {
        self.text_style(TargetKind::Way, closed, tags, zoom)
    }

    fn text_style(
        &self,
        kind: TargetKind,
        closed: bool,
        tags: &HashMap<String, String>,
        zoom: f64,
    ) -> TextStyle {
        let bucket = zoom_bucket(zoom);
        let mut out = TextStyle::default();
        for rule in &self.rules {
            let mask = rule.tag_mask(kind, closed, tags);
            if mask == 0 || !rule.applies(mask, Some(bucket)) {
                continue;
            }
            for d in &rule.declarations {
                out.apply(d);
            }
        }
        out
    }

    fn intern(&self, closed: bool, matches: Matches) -> u32 {
        let mut cache = self.sigs.lock().unwrap_or_else(|e| e.into_inner());
        let key = (closed, matches);
        if let Some(&id) = cache.ids.get(&key) {
            return id;
        }
        let id = cache.sigs.len() as u32;
        cache.sigs.push(key.clone());
        cache.ids.insert(key, id);
        id
    }

    fn match_way(&self, tags: &HashMap<String, String>, closed: bool) -> Matches {
        let mut out = Vec::new();
        for (i, rule) in self.rules.iter().enumerate() {
            let mask = rule.tag_mask(TargetKind::Way, closed, tags);
            if mask != 0 {
                out.push((i as u32, mask));
            }
        }
        out
    }

    /// Apply the matched rules in order. `zoom` of `None` ignores ranges.
    fn resolve_way(
        &self,
        matches: &Matches,
        closed: bool,
        zoom: Option<i32>,
        tags: &HashMap<String, String>,
    ) -> WayStyle {
        let mut s = WayStyle {
            direction: Direction::from_tags(tags),
            ..WayStyle::default()
        };
        let mut fill_color: Option<u32> = None;
        let mut fill_opacity: Option<f32> = None;
        for &(ri, mask) in matches {
            let rule = &self.rules[ri as usize];
            if !rule.applies(mask, zoom) {
                continue;
            }
            for d in &rule.declarations {
                match d {
                    Declaration::Color(c) => s.color = *c,
                    Declaration::Width(w) => s.width = *w,
                    Declaration::FillColor(c) => fill_color = Some(*c),
                    Declaration::FillOpacity(o) => fill_opacity = Some(*o),
                    Declaration::CasingWidth(w) => s.casing_width = *w,
                    Declaration::CasingColor(c) => s.casing_color = *c,
                    Declaration::ZIndex(z) => s.z_index = *z,
                    Declaration::Opacity(o) => s.opacity = *o,
                    Declaration::Dashes(d) => s.dashes = *d,
                    // `symbol-size` on a way rule is meaningless — ignore.
                    _ => {}
                }
            }
        }
        if closed {
            if let Some(color) = fill_color {
                s.fill = Some(Fill {
                    color,
                    opacity: fill_opacity.unwrap_or(DEFAULT_FILL_OPACITY),
                });
            }
        }
        s
    }
}

/// Strip `/* ... */` comments. Non-nesting, single pass.
fn strip_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            } else {
                i = bytes.len();
            }
            // Preserve whitespace-equivalence so token positions remain roughly sensible.
            out.push(' ');
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn eat(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn err(&self, msg: impl Into<String>) -> ParseError {
        ParseError {
            message: msg.into(),
            position: self.pos,
        }
    }

    /// Parse one rule. Returns `Ok(None)` if the rule was skipped
    /// (unsupported selector type) but parse should continue.
    fn parse_rule(&mut self, warned: &mut bool) -> Result<Option<Rule>, ParseError> {
        let mut selectors = Vec::new();
        let mut skip_rule = false;
        loop {
            self.skip_ws();
            match self.parse_selector(warned)? {
                Some(sel) => selectors.push(sel),
                None => skip_rule = true,
            }
            self.skip_ws();
            if self.eat(',') {
                continue;
            }
            break;
        }
        self.skip_ws();
        if !self.eat('{') {
            return Err(self.err("expected '{' after selector"));
        }
        let decls = self.parse_block(warned)?;
        if skip_rule || selectors.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Rule {
                selectors,
                declarations: decls,
            }))
        }
    }

    /// Parse a single selector. Returns `Ok(None)` for a recognized-but-
    /// unsupported selector (e.g., `relation`, `*`, `canvas`) so the
    /// caller can skip the rule.
    fn parse_selector(&mut self, warned: &mut bool) -> Result<Option<Selector>, ParseError> {
        let ident = self.read_ident();
        if ident.is_empty() {
            return Err(self.err("expected selector name"));
        }
        let mut kind = match ident.as_str() {
            "node" => TargetKind::Node,
            "way" => TargetKind::Way,
            "area" => TargetKind::Area,
            _ => {
                if !*warned {
                    eprintln!(
                        "mapcss: ignoring unsupported selector type '{}' (only 'node' and 'way' are recognized)",
                        ident
                    );
                    *warned = true;
                }
                // Still consume any tag-tests / zoom / pseudoclass tokens
                // for this selector so the outer parser can find the
                // next comma or brace.
                self.skip_selector_tail();
                return Ok(None);
            }
        };

        let mut tests = Vec::new();
        let mut zoom = ZoomRange::ALL;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('[') => {
                    self.bump();
                    let test = self.parse_tag_test()?;
                    tests.push(test);
                }
                Some(':') => {
                    // `way:closed` is area semantics; every other
                    // pseudo-class is unsupported and skipped.
                    self.bump();
                    let pseudo = self.read_ident();
                    if pseudo == "closed" && kind == TargetKind::Way {
                        kind = TargetKind::Area;
                    } else {
                        if !*warned {
                            eprintln!("mapcss: ignoring unsupported pseudo-class ':{}'", pseudo);
                            *warned = true;
                        }
                        self.skip_selector_tail();
                        break;
                    }
                }
                Some('|') => {
                    self.bump();
                    zoom = self.parse_zoom_range()?;
                }
                _ => break,
            }
        }

        Ok(Some(Selector { kind, tests, zoom }))
    }

    /// The part of a zoom selector after `|`: `z12`, `z12-`, `z-14`,
    /// `z12-16`.
    fn parse_zoom_range(&mut self) -> Result<ZoomRange, ParseError> {
        if !self.eat('z') {
            return Err(self.err("expected 'z' in zoom selector"));
        }
        let min = self.read_uint();
        let (min, max) = if self.eat('-') {
            (min, self.read_uint())
        } else {
            match min {
                Some(m) => (Some(m), Some(m)),
                None => return Err(self.err("expected zoom level after 'z'")),
            }
        };
        let range = ZoomRange {
            min: min.unwrap_or(i32::MIN),
            max: max.unwrap_or(i32::MAX),
        };
        if range.min > range.max {
            return Err(self.err("empty zoom range"));
        }
        Ok(range)
    }

    fn read_uint(&mut self) -> Option<i32> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.bump();
        }
        self.src[start..self.pos].parse().ok()
    }

    /// Skip forward until we hit `,` or `{` at the top level.
    fn skip_selector_tail(&mut self) {
        while let Some(c) = self.peek() {
            if c == ',' || c == '{' {
                break;
            }
            if c == '[' {
                self.bump();
                while let Some(c2) = self.peek() {
                    self.bump();
                    if c2 == ']' {
                        break;
                    }
                }
                continue;
            }
            self.bump();
        }
    }

    fn parse_tag_test(&mut self) -> Result<TagTest, ParseError> {
        self.skip_ws();
        let key = self.read_tag_ident();
        if key.is_empty() {
            return Err(self.err("expected tag key"));
        }
        self.skip_ws();
        let test = if self.eat('=') {
            self.skip_ws();
            let value = self.read_tag_value()?;
            TagTest::Equals(key, value)
        } else if self.peek() == Some('!') {
            self.bump();
            if !self.eat('=') {
                return Err(self.err("expected '=' after '!'"));
            }
            self.skip_ws();
            let value = self.read_tag_value()?;
            TagTest::NotEquals(key, value)
        } else {
            TagTest::Present(key)
        };
        self.skip_ws();
        if !self.eat(']') {
            return Err(self.err("expected ']' to close tag test"));
        }
        Ok(test)
    }

    fn parse_block(&mut self, warned: &mut bool) -> Result<Vec<Declaration>, ParseError> {
        let mut decls = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some('}') => {
                    self.bump();
                    return Ok(decls);
                }
                None => return Err(self.err("unexpected end of input inside block")),
                _ => {}
            }
            let prop = self.read_ident();
            if prop.is_empty() {
                return Err(self.err("expected property name"));
            }
            self.skip_ws();
            if !self.eat(':') {
                return Err(self.err("expected ':' after property"));
            }
            self.skip_ws();
            let value = self.read_value();
            // Consume trailing ';' if present.
            self.skip_ws();
            self.eat(';');

            match prop.as_str() {
                "color" => match parse_color_alpha(&value) {
                    Some((c, alpha)) => {
                        decls.push(Declaration::Color(c));
                        if let Some(a) = alpha {
                            decls.push(Declaration::Opacity(a));
                        }
                    }
                    None => {
                        if !*warned {
                            eprintln!("mapcss: ignoring unrecognized color '{}'", value);
                            *warned = true;
                        }
                    }
                },
                "width" => match value.trim().parse::<f32>() {
                    Ok(w) if w.is_finite() && w >= 0.0 => decls.push(Declaration::Width(w)),
                    _ => {
                        if !*warned {
                            eprintln!("mapcss: ignoring invalid width '{}'", value);
                            *warned = true;
                        }
                    }
                },
                "fill-color" => match parse_color_alpha(&value) {
                    Some((c, alpha)) => {
                        decls.push(Declaration::FillColor(c));
                        if let Some(a) = alpha {
                            decls.push(Declaration::FillOpacity(a));
                        }
                    }
                    None => {
                        if !*warned {
                            eprintln!("mapcss: ignoring unrecognized fill-color '{}'", value);
                            *warned = true;
                        }
                    }
                },
                "fill-opacity" => match value.trim().parse::<f32>() {
                    Ok(o) if o.is_finite() => {
                        decls.push(Declaration::FillOpacity(o.clamp(0.0, 1.0)))
                    }
                    _ => {
                        if !*warned {
                            eprintln!("mapcss: ignoring invalid fill-opacity '{}'", value);
                            *warned = true;
                        }
                    }
                },
                "casing-width" => match value.trim().parse::<f32>() {
                    Ok(w) if w.is_finite() && w >= 0.0 => decls.push(Declaration::CasingWidth(w)),
                    _ => {
                        if !*warned {
                            eprintln!("mapcss: ignoring invalid casing-width '{}'", value);
                            *warned = true;
                        }
                    }
                },
                "casing-color" => match parse_color_alpha(&value) {
                    Some((c, _)) => decls.push(Declaration::CasingColor(c)),
                    None => {
                        if !*warned {
                            eprintln!("mapcss: ignoring unrecognized casing-color '{}'", value);
                            *warned = true;
                        }
                    }
                },
                "opacity" => match value.trim().parse::<f32>() {
                    Ok(o) if o.is_finite() => decls.push(Declaration::Opacity(o.clamp(0.0, 1.0))),
                    _ => {
                        if !*warned {
                            eprintln!("mapcss: ignoring invalid opacity '{}'", value);
                            *warned = true;
                        }
                    }
                },
                "dashes" => match parse_dashes(&value) {
                    Some(d) => decls.push(Declaration::Dashes(d)),
                    None => {
                        if !*warned {
                            eprintln!("mapcss: ignoring invalid dashes '{}'", value);
                            *warned = true;
                        }
                    }
                },
                "text" => {
                    let v = unquote(&value);
                    if v.is_empty() {
                        warn_once(warned, "text", &value);
                    } else if v.eq_ignore_ascii_case("none") {
                        decls.push(Declaration::Text(None));
                    } else {
                        decls.push(Declaration::Text(Some(v.to_string())));
                    }
                }
                "font-size" => match parse_positive(&value) {
                    Some(v) => decls.push(Declaration::FontSize(v)),
                    None => warn_once(warned, "font-size", &value),
                },
                "text-color" => match parse_color_alpha(&value) {
                    Some((c, _)) => decls.push(Declaration::TextColor(c)),
                    None => warn_once(warned, "text-color", &value),
                },
                "text-halo-color" => match parse_color_alpha(&value) {
                    Some((c, _)) => decls.push(Declaration::TextHaloColor(c)),
                    None => warn_once(warned, "text-halo-color", &value),
                },
                "text-halo-radius" => match value.trim().parse::<f32>() {
                    Ok(v) if v.is_finite() && v >= 0.0 => {
                        decls.push(Declaration::TextHaloRadius(v))
                    }
                    _ => warn_once(warned, "text-halo-radius", &value),
                },
                "text-position" => match value.trim().to_ascii_lowercase().as_str() {
                    "center" => decls.push(Declaration::TextPosition(TextPosition::Center)),
                    "line" => decls.push(Declaration::TextPosition(TextPosition::Line)),
                    _ => warn_once(warned, "text-position", &value),
                },
                "icon-image" => {
                    let v = unquote(&value);
                    let v = v
                        .strip_prefix("url(")
                        .and_then(|r| r.strip_suffix(')'))
                        .map(unquote)
                        .unwrap_or(v);
                    if v.is_empty() {
                        warn_once(warned, "icon-image", &value);
                    } else if v.eq_ignore_ascii_case("none") {
                        decls.push(Declaration::IconImage(None));
                    } else {
                        decls.push(Declaration::IconImage(Some(v.to_string())));
                    }
                }
                "icon-width" => match parse_positive(&value) {
                    Some(v) => decls.push(Declaration::IconWidth(v)),
                    None => warn_once(warned, "icon-width", &value),
                },
                "z-index" => match value.trim().parse::<f32>() {
                    Ok(z) if z.is_finite() => decls.push(Declaration::ZIndex(z)),
                    _ => {
                        if !*warned {
                            eprintln!("mapcss: ignoring invalid z-index '{}'", value);
                            *warned = true;
                        }
                    }
                },
                "symbol-size" => match value.trim().parse::<f32>() {
                    Ok(w) if w.is_finite() && w >= 0.0 => decls.push(Declaration::SymbolSize(w)),
                    _ => {
                        if !*warned {
                            eprintln!("mapcss: ignoring invalid symbol-size '{}'", value);
                            *warned = true;
                        }
                    }
                },
                _ => {
                    if !*warned {
                        eprintln!("mapcss: ignoring unknown property '{}'", prop);
                        *warned = true;
                    }
                }
            }
        }
    }

    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                self.bump();
            } else {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }

    /// Tag keys allow colons (e.g. `addr:street`) and ASCII letters, digits,
    /// dashes, underscores.
    fn read_tag_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':' {
                self.bump();
            } else {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }

    /// Tag value — bareword or quoted string.
    fn read_tag_value(&mut self) -> Result<String, ParseError> {
        if self.peek() == Some('"') {
            self.bump();
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c == '"' {
                    let s = self.src[start..self.pos].to_string();
                    self.bump();
                    return Ok(s);
                }
                self.bump();
            }
            Err(self.err("unterminated string"))
        } else {
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.' {
                    self.bump();
                } else {
                    break;
                }
            }
            Ok(self.src[start..self.pos].to_string())
        }
    }

    /// Read a property value up to `;` or `}`.
    fn read_value(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == ';' || c == '}' {
                break;
            }
            self.bump();
        }
        self.src[start..self.pos].trim().to_string()
    }
}

/// Log a rejected property value, once per parse.
fn warn_once(warned: &mut bool, prop: &str, value: &str) {
    if !*warned {
        eprintln!("mapcss: ignoring invalid {} '{}'", prop, value);
        *warned = true;
    }
}

/// A finite number greater than zero.
fn parse_positive(s: &str) -> Option<f32> {
    s.trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
}

/// `s` trimmed, without one pair of surrounding double quotes.
fn unquote(s: &str) -> &str {
    let t = s.trim();
    t.strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(t)
        .trim()
}

/// Parse a `dashes` value: `none`/`0`, or `dash, gap` (comma or space
/// separated, pixels; further pairs are ignored). `Some(None)` is a valid
/// "no dashes"; `None` is invalid.
fn parse_dashes(s: &str) -> Option<Option<(f32, f32)>> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("none") {
        return Some(None);
    }
    let nums: Vec<f32> = t
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<f32>().ok())
        .collect::<Option<_>>()?;
    if nums.iter().any(|n| !n.is_finite() || *n < 0.0) {
        return None;
    }
    match nums.as_slice() {
        [] => None,
        [0.0] => Some(None),
        // A lone number is both the dash and the gap.
        [d] => Some(Some((d.max(MIN_DASH_PX), d.max(MIN_DASH_PX)))),
        [d, g, ..] if *d > 0.0 && *g > 0.0 => Some(Some((d.max(MIN_DASH_PX), g.max(MIN_DASH_PX)))),
        _ => None,
    }
}

/// Dash and gap lengths below this are raised to it, so a stroke never
/// turns into millions of dashes.
const MIN_DASH_PX: f32 = 1.0;

/// Like [`parse_color`], also returning the alpha (0..=1) of `rgba(..)`.
fn parse_color_alpha(s: &str) -> Option<(u32, Option<f32>)> {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    for (prefix, has_alpha) in [("rgba(", true), ("rgb(", false)] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let inner = rest.strip_suffix(')')?;
            let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            if parts.len() != 3 + has_alpha as usize {
                return None;
            }
            let mut rgb = 0u32;
            for p in &parts[..3] {
                let v: f32 = match p.strip_suffix('%') {
                    Some(pct) => pct.trim().parse::<f32>().ok()? * 2.55,
                    None => p.parse::<f32>().ok()?,
                };
                if !v.is_finite() {
                    return None;
                }
                rgb = (rgb << 8) | v.round().clamp(0.0, 255.0) as u32;
            }
            let alpha = if has_alpha {
                let a = parts[3].parse::<f32>().ok()?;
                if !a.is_finite() {
                    return None;
                }
                Some(a.clamp(0.0, 1.0))
            } else {
                None
            };
            return Some((rgb, alpha));
        }
    }
    parse_color_plain(t).map(|c| (c, None))
}

fn parse_color_plain(s: &str) -> Option<u32> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let r = u32::from_str_radix(&hex[0..1], 16).ok()?;
                let g = u32::from_str_radix(&hex[1..2], 16).ok()?;
                let b = u32::from_str_radix(&hex[2..3], 16).ok()?;
                Some(((r * 17) << 16) | ((g * 17) << 8) | (b * 17))
            }
            6 => u32::from_str_radix(hex, 16).ok(),
            _ => None,
        };
    }
    match t.to_ascii_lowercase().as_str() {
        "red" => Some(0xFF0000),
        "green" => Some(0x008000),
        "blue" => Some(0x0000FF),
        "yellow" => Some(0xFFFF00),
        "black" => Some(0x000000),
        "white" => Some(0xFFFFFF),
        "gray" | "grey" => Some(0x808080),
        "orange" => Some(0xFFA500),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zoom at which no zoom-range selector in these tests matters.
    const Z: f64 = 16.0;

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn empty_stylesheet_uses_defaults() {
        let s = Stylesheet::parse("").unwrap();
        assert_eq!(s.node_style(&tags(&[]), Z), NodeStyle::default());
        assert_eq!(s.way_style(&tags(&[]), false, Z), WayStyle::default());
    }

    #[test]
    fn parse_minimal_way_rule() {
        let s = Stylesheet::parse("way { color: #ff0000; width: 2; }").unwrap();
        let w = s.way_style(&tags(&[]), false, Z);
        assert_eq!(w.color, 0xFF0000);
        assert_eq!(w.width, 2.0);
    }

    #[test]
    fn parse_minimal_node_rule() {
        let s = Stylesheet::parse("node { color: #00ff00; symbol-size: 14; }").unwrap();
        let n = s.node_style(&tags(&[]), Z);
        assert_eq!(n.color, 0x00FF00);
        assert_eq!(n.size, 14.0);
    }

    #[test]
    fn multi_selector_applies_to_both() {
        let s = Stylesheet::parse("way, node { color: red; }").unwrap();
        assert_eq!(s.way_style(&tags(&[]), false, Z).color, 0xFF0000);
        assert_eq!(s.node_style(&tags(&[]), Z).color, 0xFF0000);
    }

    #[test]
    fn later_rule_overrides_earlier() {
        let s = Stylesheet::parse("way { color: red; } way { color: blue; }").unwrap();
        assert_eq!(s.way_style(&tags(&[]), false, Z).color, 0x0000FF);
    }

    #[test]
    fn tag_present_matches_any_value() {
        let s = Stylesheet::parse("way[highway] { color: orange; }").unwrap();
        assert_eq!(
            s.way_style(&tags(&[("highway", "residential")]), false, Z)
                .color,
            0xFFA500
        );
        assert_eq!(
            s.way_style(&tags(&[("highway", "footway")]), false, Z)
                .color,
            0xFFA500
        );
        // No tag => default way color.
        assert_eq!(s.way_style(&tags(&[]), false, Z).color, DEFAULT_WAY_COLOR);
    }

    #[test]
    fn tag_equals_matches_exact() {
        let s = Stylesheet::parse("way[highway=residential] { color: orange; }").unwrap();
        assert_eq!(
            s.way_style(&tags(&[("highway", "residential")]), false, Z)
                .color,
            0xFFA500
        );
        assert_eq!(
            s.way_style(&tags(&[("highway", "footway")]), false, Z)
                .color,
            DEFAULT_WAY_COLOR
        );
    }

    #[test]
    fn tag_not_equals_matches_others() {
        let s = Stylesheet::parse("way[highway!=motorway] { color: green; }").unwrap();
        assert_eq!(
            s.way_style(&tags(&[("highway", "residential")]), false, Z)
                .color,
            0x008000
        );
        assert_eq!(
            s.way_style(&tags(&[("highway", "motorway")]), false, Z)
                .color,
            DEFAULT_WAY_COLOR
        );
        // No highway tag at all -> not-equals is true by convention.
        assert_eq!(s.way_style(&tags(&[]), false, Z).color, 0x008000);
    }

    #[test]
    fn unknown_property_ignored() {
        let s = Stylesheet::parse("way { color: red; frobnicate: 2; width: 3; }").unwrap();
        let w = s.way_style(&tags(&[]), false, Z);
        assert_eq!(w.color, 0xFF0000);
        assert_eq!(w.width, 3.0);
    }

    #[test]
    fn casing_declarations_resolve_on_ways() {
        let s = Stylesheet::parse(
            "way[highway] { width: 3; casing-width: 1.5; casing-color: #102030; } way[highway=x] { casing-width: 0; }",
        )
        .unwrap();
        let w = s.way_style(&tags(&[("highway", "primary")]), false, Z);
        assert_eq!(w.casing_width, 1.5);
        assert_eq!(w.casing_color, 0x102030);
        // A later rule can turn the casing off again.
        assert_eq!(
            s.way_style(&tags(&[("highway", "x")]), false, Z)
                .casing_width,
            0.0
        );
        // No casing by default; a casing-width alone uses the default color.
        assert_eq!(WayStyle::default().casing_width, 0.0);
        let s = Stylesheet::parse("way { casing-width: 1; }").unwrap();
        let w = s.way_style(&tags(&[]), false, Z);
        assert_eq!(
            (w.casing_width, w.casing_color),
            (1.0, DEFAULT_CASING_COLOR)
        );
    }

    #[test]
    fn invalid_casing_values_are_ignored() {
        let s = Stylesheet::parse("way { casing-width: -2; casing-color: nope; }").unwrap();
        let w = s.way_style(&tags(&[]), false, Z);
        assert_eq!(w.casing_width, 0.0);
        assert_eq!(w.casing_color, DEFAULT_CASING_COLOR);
    }

    #[test]
    fn casing_does_not_apply_to_nodes() {
        let s = Stylesheet::parse("node { casing-width: 2; }").unwrap();
        assert_eq!(s.node_style(&tags(&[]), Z), NodeStyle::default());
    }

    #[test]
    fn z_index_parses_and_later_rules_override() {
        let s = Stylesheet::parse(
            "way[highway] { z-index: 3; } way[highway=x] { z-index: -1.5; } way[bad] { z-index: up; }",
        )
        .unwrap();
        let z = |pairs: &[(&str, &str)]| s.way_style(&tags(pairs), false, Z).z_index;
        assert_eq!(z(&[("highway", "primary")]), 3.0);
        assert_eq!(z(&[("highway", "x")]), -1.5);
        // Invalid values are dropped, leaving the default.
        assert_eq!(z(&[("bad", "1")]), 0.0);
        assert_eq!(z(&[]), 0.0);
    }

    #[test]
    fn default_stylesheet_ranks_roads_by_importance() {
        let s = Stylesheet::load_default();
        let z = |hw: &str| s.way_style(&tags(&[("highway", hw)]), false, Z).z_index;
        assert!(z("motorway") > z("primary"));
        assert!(z("primary") > z("secondary"));
        assert!(z("secondary") > z("tertiary"));
        assert!(z("tertiary") > z("residential"));
    }

    fn way_with(css: &str) -> WayStyle {
        Stylesheet::parse(css)
            .unwrap()
            .way_style(&tags(&[]), true, Z)
    }

    #[test]
    fn rgb_and_rgba_colors() {
        assert_eq!(way_with("way { color: rgb(255, 0, 128); }").color, 0xFF0080);
        assert_eq!(way_with("way { color: RGB(0,255,0); }").color, 0x00FF00);
        assert_eq!(
            way_with("way { color: rgb(100%, 0%, 0%); }").color,
            0xFF0000
        );
        // Out-of-range channels clamp.
        assert_eq!(way_with("way { color: rgb(300, -5, 0); }").color, 0xFF0000);
        let w = way_with("way { color: rgba(10, 20, 30, 0.5); }");
        assert_eq!(w.color, 0x0A141E);
        assert!((w.opacity - 0.5).abs() < 1e-6);
        // An rgb() color with no alpha leaves opacity alone.
        assert_eq!(way_with("way { color: rgb(1,2,3); }").opacity, 1.0);
        // Malformed ones are ignored, keeping the default.
        for bad in [
            "rgb(1,2)",
            "rgba(1,2,3)",
            "rgb(1,2,3,4)",
            "rgb(a,b,c)",
            "rgb(1,2,3",
        ] {
            let w = way_with(&format!("way {{ color: {bad}; }}"));
            assert_eq!(w.color, DEFAULT_WAY_COLOR, "{bad}");
        }
    }

    #[test]
    fn rgba_fill_color_sets_fill_opacity() {
        let w = way_with("area { fill-color: rgba(255, 0, 0, 0.25); }");
        let f = w.fill.unwrap();
        assert_eq!(f.color, 0xFF0000);
        assert!((f.opacity - 0.25).abs() < 1e-6);
        // An explicit fill-opacity afterwards wins.
        let w = way_with("area { fill-color: rgba(255, 0, 0, 0.25); fill-opacity: 0.9; }");
        assert!((w.fill.unwrap().opacity - 0.9).abs() < 1e-6);
    }

    #[test]
    fn opacity_property() {
        assert_eq!(way_with("way { opacity: 0.4; }").opacity, 0.4);
        assert_eq!(way_with("way { opacity: 7; }").opacity, 1.0);
        assert_eq!(way_with("way { opacity: -1; }").opacity, 0.0);
        assert_eq!(way_with("way { opacity: lots; }").opacity, 1.0);
        // Later wins over the alpha of an earlier rgba().
        assert_eq!(
            way_with("way { color: rgba(0,0,0,0.2); opacity: 0.7; }").opacity,
            0.7
        );
    }

    #[test]
    fn dashes_property() {
        assert_eq!(way_with("way { dashes: 6,3; }").dashes, Some((6.0, 3.0)));
        assert_eq!(way_with("way { dashes: 6 3; }").dashes, Some((6.0, 3.0)));
        // Extra pairs are tolerated; only the first pair is kept.
        assert_eq!(
            way_with("way { dashes: 6,3,2,2; }").dashes,
            Some((6.0, 3.0))
        );
        assert_eq!(way_with("way { dashes: 4; }").dashes, Some((4.0, 4.0)));
        assert_eq!(
            way_with("way { dashes: 0.1,0.1; }").dashes,
            Some((1.0, 1.0))
        );
        assert_eq!(way_with("way { dashes: 6,3; }").dashes, Some((6.0, 3.0)));
        // `none` clears an earlier dash pattern.
        assert_eq!(way_with("way { dashes: 6,3; dashes: none; }").dashes, None);
        for bad in ["", "x,y", "-1,2", "6,0"] {
            let w = way_with(&format!("way {{ dashes: {bad}; }}"));
            assert_eq!(w.dashes, None, "{bad:?}");
        }
        assert_eq!(WayStyle::default().dashes, None);
    }

    fn text_of(css: &str) -> TextStyle {
        Stylesheet::parse(css)
            .unwrap()
            .way_text_style(&tags(&[("highway", "primary")]), false, Z)
    }

    #[test]
    fn label_properties_are_parsed_and_resolved() {
        let t = text_of(
            "way[highway] { text: name; font-size: 11; text-color: #112233; \
             text-halo-color: rgba(255,255,255,0.8); text-halo-radius: 2; \
             text-position: line; icon-image: \"icons/shield.svg\"; icon-width: 16; }",
        );
        assert_eq!(
            t,
            TextStyle {
                text: Some("name".into()),
                font_size: Some(11.0),
                color: Some(0x112233),
                halo_color: Some(0xFFFFFF),
                halo_radius: Some(2.0),
                position: Some(TextPosition::Line),
                icon_image: Some("icons/shield.svg".into()),
                icon_width: Some(16.0),
            }
        );
        assert!(!t.is_empty());
    }

    #[test]
    fn label_properties_do_not_disturb_stroke_styling() {
        let s =
            Stylesheet::parse("way { color: red; width: 3; text: name; font-size: 12; }").unwrap();
        let w = s.way_style(&tags(&[]), false, Z);
        assert_eq!((w.color, w.width), (0xFF0000, 3.0));
        assert_eq!(s.node_style(&tags(&[]), Z), NodeStyle::default());
    }

    #[test]
    fn label_rules_cascade_and_can_be_cleared() {
        let t = text_of(
            "way[highway] { text: ref; font-size: 9; } way[highway=primary] { text: name; } \
             way { icon-image: url(\"a.png\"); } way[highway] { icon-image: none; }",
        );
        assert_eq!(t.text.as_deref(), Some("name"));
        assert_eq!(t.font_size, Some(9.0));
        assert_eq!(t.icon_image, None);
        let t = text_of("way { text: name; } way { text: none; }");
        assert_eq!(t.text, None);
    }

    #[test]
    fn label_properties_follow_selectors_zoom_and_kind() {
        let s = Stylesheet::parse(
            "node[amenity] { text: name; } way|z15-[highway] { text: name; text-position: line; } \
             area[building] { text: addr:housenumber; }",
        )
        .unwrap();
        let hw = tags(&[("highway", "primary")]);
        assert!(s.way_text_style(&hw, false, 14.0).is_empty());
        assert_eq!(
            s.way_text_style(&hw, false, 15.0).position,
            Some(TextPosition::Line)
        );
        assert!(s.node_text_style(&hw, Z).is_empty());
        assert_eq!(
            s.node_text_style(&tags(&[("amenity", "cafe")]), Z).text,
            Some("name".into())
        );
        // `area` only labels closed ways.
        let b = tags(&[("building", "yes")]);
        assert!(s.way_text_style(&b, false, Z).is_empty());
        assert_eq!(
            s.way_text_style(&b, true, Z).text.as_deref(),
            Some("addr:housenumber")
        );
    }

    #[test]
    fn invalid_label_values_are_ignored() {
        let t = text_of(
            "way { font-size: 0; text-color: nope; text-halo-radius: -1; text-position: top; \
             icon-width: big; text: ; icon-image: ; }",
        );
        assert!(t.is_empty());
    }

    #[test]
    fn comments_are_stripped() {
        let s = Stylesheet::parse("/* hi */ way { /* c */ color: red; }").unwrap();
        assert_eq!(s.way_style(&tags(&[]), false, Z).color, 0xFF0000);
    }

    #[test]
    fn short_hex_color() {
        let s = Stylesheet::parse("way { color: #f00; }").unwrap();
        assert_eq!(s.way_style(&tags(&[]), false, Z).color, 0xFF0000);
    }

    #[test]
    fn default_stylesheet_parses() {
        let s = Stylesheet::load_default();
        // Spot-check a couple of rules from the embedded asset.
        let residential = s.way_style(&tags(&[("highway", "residential")]), false, 12.0);
        assert_eq!((residential.color, residential.width), (0xffffff, 2.0));
        // A specific road class overrides the generic highway rule, including
        // when it's the second selector of a multi-line selector list.
        let trunk = s.way_style(&tags(&[("highway", "trunk_link")]), false, 12.0);
        assert_eq!((trunk.color, trunk.width), (0xff7a90, 3.0));
        assert_eq!(
            s.way_style(&tags(&[("highway", "footway")]), false, Z)
                .color,
            0xff9a8a
        );
        // Fractional widths parse.
        assert_eq!(s.way_style(&tags(&[]), false, Z).width, 1.5);
        assert_eq!(
            s.node_style(&tags(&[("amenity", "pub")]), Z).color,
            0xff5fa2
        );
        // Untagged vertices are smaller than any tagged node.
        assert!(
            s.node_style(&tags(&[]), Z).size < s.node_style(&tags(&[("shop", "bakery")]), Z).size
        );
        let building = s.way_style(&tags(&[("building", "yes")]), true, Z);
        let fill = building
            .fill
            .expect("default stylesheet must fill closed buildings");
        assert_eq!(fill.color, 0xe08fc0);
        // Open way with the same tag gets no fill.
        assert_eq!(
            s.way_style(&tags(&[("building", "yes")]), false, Z).fill,
            None
        );
        let water = s.way_style(&tags(&[("natural", "water")]), true, Z);
        assert_eq!(water.fill.expect("water fill").color, 0x4aa8ff);
        assert_eq!(water.color, 0x4aa8ff);
    }

    #[test]
    fn node_style_records_whether_the_node_is_tagged() {
        let s = Stylesheet::parse("").unwrap();
        assert!(!s.node_style(&tags(&[]), Z).tagged);
        assert!(s.node_style(&tags(&[("amenity", "cafe")]), Z).tagged);
    }

    #[test]
    fn direction_comes_from_oneway_roundabout_and_waterway_tags() {
        let dir = |pairs: &[(&str, &str)]| Direction::from_tags(&tags(pairs));
        assert_eq!(dir(&[("highway", "residential")]), Direction::None);
        assert_eq!(dir(&[("oneway", "yes")]), Direction::Forward);
        assert_eq!(dir(&[("oneway", "-1")]), Direction::Reverse);
        assert_eq!(dir(&[("junction", "roundabout")]), Direction::Forward);
        assert_eq!(dir(&[("waterway", "stream")]), Direction::Forward);
        assert_eq!(dir(&[("waterway", "dam")]), Direction::None);
        // An explicit oneway=no wins over an implied direction.
        assert_eq!(
            dir(&[("junction", "roundabout"), ("oneway", "no")]),
            Direction::None
        );
        // way_style carries it through.
        let s = Stylesheet::parse("").unwrap();
        assert_eq!(
            s.way_style(&tags(&[("oneway", "yes")]), false, Z).direction,
            Direction::Forward
        );
    }

    #[test]
    fn area_rule_fills_closed_ways_only() {
        let s = Stylesheet::parse("area[building] { fill-color: #808080; fill-opacity: 0.4; }")
            .unwrap();
        let closed = s.way_style(&tags(&[("building", "yes")]), true, Z);
        let fill = closed.fill.expect("closed building way must have a fill");
        assert_eq!(fill.color, 0x808080);
        assert!((fill.opacity - 0.4).abs() < 1e-6);
        // Same tags, open way: area rule must not match.
        assert_eq!(
            s.way_style(&tags(&[("building", "yes")]), false, Z).fill,
            None
        );
    }

    #[test]
    fn way_closed_pseudo_class_is_area_equivalent() {
        let s = Stylesheet::parse("way:closed[building] { fill-color: #112233; }").unwrap();
        assert!(s
            .way_style(&tags(&[("building", "yes")]), true, Z)
            .fill
            .is_some());
        assert_eq!(
            s.way_style(&tags(&[("building", "yes")]), false, Z).fill,
            None
        );
    }

    #[test]
    fn fill_color_without_opacity_defaults() {
        let s = Stylesheet::parse("area { fill-color: red; }").unwrap();
        let fill = s.way_style(&tags(&[]), true, Z).fill.unwrap();
        assert_eq!(fill.color, 0xFF0000);
        assert!((fill.opacity - DEFAULT_FILL_OPACITY).abs() < 1e-6);
    }

    #[test]
    fn fill_opacity_without_color_is_no_fill() {
        let s = Stylesheet::parse("area { fill-opacity: 0.9; }").unwrap();
        assert_eq!(s.way_style(&tags(&[]), true, Z).fill, None);
    }

    #[test]
    fn fill_opacity_is_clamped() {
        let s = Stylesheet::parse("area { fill-color: red; fill-opacity: 7; }").unwrap();
        assert!((s.way_style(&tags(&[]), true, Z).fill.unwrap().opacity - 1.0).abs() < 1e-6);
    }

    #[test]
    fn fill_on_plain_way_rule_applies_only_when_closed() {
        // JOSM semantics: `way { fill-color: … }` fills closed ways, is
        // ignored on open ways.
        let s = Stylesheet::parse("way { fill-color: blue; }").unwrap();
        assert!(s.way_style(&tags(&[]), true, Z).fill.is_some());
        assert_eq!(s.way_style(&tags(&[]), false, Z).fill, None);
    }

    #[test]
    fn area_rule_stroke_props_apply_to_closed_way() {
        let s = Stylesheet::parse("area[building] { color: #ff0000; width: 2; }").unwrap();
        let st = s.way_style(&tags(&[("building", "yes")]), true, Z);
        assert_eq!(st.color, 0xFF0000);
        assert_eq!(st.width, 2.0);
        // Open way with same tags keeps defaults.
        let st = s.way_style(&tags(&[("building", "yes")]), false, Z);
        assert_eq!(st.color, DEFAULT_WAY_COLOR);
    }

    #[test]
    fn area_selector_never_matches_nodes() {
        let s = Stylesheet::parse("area { color: red; }").unwrap();
        assert_eq!(s.node_style(&tags(&[]), Z).color, DEFAULT_NODE_COLOR);
    }

    fn width_at(s: &Stylesheet, zoom: f64) -> f32 {
        s.way_style(&tags(&[("highway", "primary")]), false, zoom)
            .width
    }

    #[test]
    fn zoom_selector_open_ended_min() {
        let s = Stylesheet::parse("way[highway] { width: 1; } way|z12-[highway] { width: 5; }")
            .unwrap();
        assert_eq!(width_at(&s, 11.9), 1.0);
        assert_eq!(width_at(&s, 12.0), 5.0);
        assert_eq!(width_at(&s, 20.0), 5.0);
    }

    #[test]
    fn zoom_selector_open_ended_max() {
        let s = Stylesheet::parse("way[highway] { width: 1; } way|z-14[highway] { width: 5; }")
            .unwrap();
        assert_eq!(width_at(&s, 3.0), 5.0);
        // Inclusive of the whole of zoom 14.
        assert_eq!(width_at(&s, 14.9), 5.0);
        assert_eq!(width_at(&s, 15.0), 1.0);
    }

    #[test]
    fn zoom_selector_closed_range_and_single_level() {
        let s = Stylesheet::parse(
            "way[highway] { width: 1; } way|z12-16[highway] { width: 5; } way|z18[highway] { width: 9; }",
        )
        .unwrap();
        assert_eq!(width_at(&s, 11.0), 1.0);
        assert_eq!(width_at(&s, 12.0), 5.0);
        assert_eq!(width_at(&s, 16.5), 5.0);
        assert_eq!(width_at(&s, 17.0), 1.0);
        assert_eq!(width_at(&s, 18.2), 9.0);
        assert_eq!(width_at(&s, 19.0), 1.0);
    }

    #[test]
    fn zoom_selector_combines_with_area_and_pseudo_class() {
        let s = Stylesheet::parse("area|z14-[building] { fill-color: red; }").unwrap();
        let b = tags(&[("building", "yes")]);
        assert!(s.way_style(&b, true, 13.0).fill.is_none());
        assert!(s.way_style(&b, true, 14.0).fill.is_some());
        let s = Stylesheet::parse("way|z14-:closed[building] { fill-color: red; }").unwrap();
        assert!(s.way_style(&b, true, 14.0).fill.is_some());
        assert!(s.way_style(&b, false, 14.0).fill.is_none());
    }

    #[test]
    fn zoom_selector_syntax_errors() {
        assert!(Stylesheet::parse("way|12- { width: 1; }").is_err());
        assert!(Stylesheet::parse("way|z { width: 1; }").is_err());
        assert!(Stylesheet::parse("way|z16-12 { width: 1; }").is_err());
    }

    #[test]
    fn one_selector_of_a_list_can_carry_the_zoom_range() {
        let s = Stylesheet::parse("way|z15-[highway=primary], node { width: 5; }").unwrap();
        assert_eq!(width_at(&s, 14.0), DEFAULT_WAY_WIDTH);
        assert_eq!(width_at(&s, 15.0), 5.0);
    }

    #[test]
    fn node_styles_honor_zoom_too() {
        let s = Stylesheet::parse("node|z17- { symbol-size: 12; }").unwrap();
        assert_eq!(s.node_style(&tags(&[]), 16.0).size, DEFAULT_NODE_SIZE);
        assert_eq!(s.node_style(&tags(&[]), 17.0).size, 12.0);
    }

    #[test]
    fn cached_style_ignores_zoom_and_zoom_table_resolves_it() {
        let s = Stylesheet::parse(
            "way[highway] { width: 1; } way|z12-[highway] { width: 5; } way|z-11[highway] { width: 2; }",
        )
        .unwrap();
        assert!(s.is_zoom_dependent());
        let t = tags(&[("highway", "primary")]);
        let cached = s.way_style_cached(&t, false);
        assert_ne!(cached.sig, NO_SIG);
        // Same rules matched: same signature. Different rules: different.
        assert_eq!(s.way_style_cached(&t, false).sig, cached.sig);
        assert_ne!(
            s.way_style_cached(&tags(&[("waterway", "river")]), false)
                .sig,
            cached.sig
        );
        let at = |z: f64| s.zoom_styles(z)[cached.sig as usize].width;
        assert_eq!(at(10.0), 2.0);
        assert_eq!(at(14.0), 5.0);
        // Back to the first bucket: the table is rebuilt, not stale.
        assert_eq!(at(10.5), 2.0);
    }

    #[test]
    fn zoom_table_grows_with_new_signatures() {
        let s = Stylesheet::parse("way|z12-[highway] { width: 5; }").unwrap();
        let a = s.way_style_cached(&tags(&[("highway", "x")]), false);
        assert_eq!(s.zoom_styles(14.0)[a.sig as usize].width, 5.0);
        let b = s.way_style_cached(&tags(&[("waterway", "x")]), false);
        assert_eq!(s.zoom_styles(14.0)[b.sig as usize].width, DEFAULT_WAY_WIDTH);
    }

    #[test]
    fn stylesheet_without_zoom_selectors_is_not_zoom_dependent() {
        assert!(!Stylesheet::parse("way { width: 2; }")
            .unwrap()
            .is_zoom_dependent());
    }

    #[test]
    fn default_stylesheet_widens_major_roads_with_zoom() {
        let s = Stylesheet::load_default();
        assert!(s.is_zoom_dependent());
        let w = |hw: &str, z: f64| s.way_style(&tags(&[("highway", hw)]), false, z).width;
        for hw in [
            "motorway",
            "primary",
            "secondary",
            "tertiary",
            "residential",
        ] {
            assert!(w(hw, 18.5) > w(hw, 12.0), "{hw} must widen with zoom");
            assert!(w(hw, 16.0) >= w(hw, 12.0), "{hw} must not narrow");
        }
        // Major roads get a casing from zoom 14; minor ones rely on the
        // renderer's default.
        let cased = |hw: &str, z: f64| {
            s.way_style(&tags(&[("highway", hw)]), false, z)
                .casing_width
        };
        assert_eq!(cased("primary", 13.0), 0.0);
        assert!(cased("primary", 14.0) > 0.0);
        assert_eq!(cased("residential", 18.0), 0.0);
        // Minor paths keep their width.
        assert_eq!(w("footway", 18.5), w("footway", 12.0));
    }

    #[test]
    fn width_on_node_aliases_symbol_size() {
        let s = Stylesheet::parse("node { width: 7; }").unwrap();
        assert_eq!(s.node_style(&tags(&[]), Z).size, 7.0);
    }
}
