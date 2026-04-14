//! Minimal hand-rolled MapCSS (JOSM variant) parser.
//!
//! Scope is intentionally small — see `docs/superpowers/plans/issue-18-mapcss.md`:
//!
//! - Selectors: `node` / `way`, each with zero or more tag tests
//!   `[key]`, `[key=value]`, `[key!=value]`. Comma-separated lists of
//!   selectors share a declaration block.
//! - Declarations: `color`, `width`, `symbol-size` (plus `width` as an
//!   alias for `symbol-size` on nodes). Other properties are ignored
//!   with a single warning.
//! - Colors: `#rgb`, `#rrggbb`, or one of a small named set.
//! - Comments: `/* ... */` (non-nesting).
//!
//! Anything outside that subset (zoom selectors, pseudo-classes,
//! `eval(...)`, casing, etc.) is parsed permissively: unknown tokens
//! are skipped and a warning is logged once per parse.

use std::collections::HashMap;

/// Baseline node style — applied before any rule, so that an empty
/// stylesheet still renders features. Mirrors the old hardcoded
/// defaults in `OsmLayer`.
pub const DEFAULT_NODE_COLOR: u32 = 0xFFD700;
pub const DEFAULT_NODE_SIZE: f32 = 10.0;
pub const DEFAULT_WAY_COLOR: u32 = 0x4169E1;
pub const DEFAULT_WAY_WIDTH: f32 = 4.0;

/// Resolved node style for a single feature.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeStyle {
    pub color: u32,
    pub size: f32,
}

impl Default for NodeStyle {
    fn default() -> Self {
        Self { color: DEFAULT_NODE_COLOR, size: DEFAULT_NODE_SIZE }
    }
}

/// Resolved way style for a single feature.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WayStyle {
    pub color: u32,
    pub width: f32,
}

impl Default for WayStyle {
    fn default() -> Self {
        Self { color: DEFAULT_WAY_COLOR, width: DEFAULT_WAY_WIDTH }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    Node,
    Way,
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
}

impl Selector {
    fn matches(&self, kind: TargetKind, tags: &HashMap<String, String>) -> bool {
        if self.kind != kind {
            return false;
        }
        self.tests.iter().all(|t| t.matches(tags))
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
}

#[derive(Debug, Clone, PartialEq)]
struct Rule {
    selectors: Vec<Selector>,
    declarations: Vec<Declaration>,
}

/// Parsed MapCSS stylesheet.
#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    rules: Vec<Rule>,
}

/// Error from [`Stylesheet::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub position: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MapCSS parse error at {}: {}", self.position, self.message)
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
            match p.parse_rule(&mut warned)? {
                Some(rule) => rules.push(rule),
                None => {}
            }
        }
        Ok(Self { rules })
    }

    /// Load and parse the compiled-in default stylesheet.
    ///
    /// Panics if the embedded asset fails to parse — that would be a
    /// build-time bug.
    pub fn load_default() -> Self {
        let src = include_str!("../../assets/default.mapcss");
        Self::parse(src).expect("default.mapcss must parse")
    }

    /// Resolve a style for a node given its OSM tags.
    pub fn node_style(&self, tags: &HashMap<String, String>) -> NodeStyle {
        let mut s = NodeStyle::default();
        for rule in &self.rules {
            if !rule.selectors.iter().any(|sel| sel.matches(TargetKind::Node, tags)) {
                continue;
            }
            for d in &rule.declarations {
                match d {
                    Declaration::Color(c) => s.color = *c,
                    Declaration::SymbolSize(w) => s.size = *w,
                    // `width` on a pure-node rule aliases to symbol-size.
                    Declaration::Width(w) => {
                        if rule.selectors.iter().all(|sel| sel.kind == TargetKind::Node) {
                            s.size = *w;
                        }
                    }
                }
            }
        }
        s
    }

    /// Resolve a style for a way given its OSM tags.
    pub fn way_style(&self, tags: &HashMap<String, String>) -> WayStyle {
        let mut s = WayStyle::default();
        for rule in &self.rules {
            if !rule.selectors.iter().any(|sel| sel.matches(TargetKind::Way, tags)) {
                continue;
            }
            for d in &rule.declarations {
                match d {
                    Declaration::Color(c) => s.color = *c,
                    Declaration::Width(w) => s.width = *w,
                    // `symbol-size` on a way rule is meaningless — ignore.
                    Declaration::SymbolSize(_) => {}
                }
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
        ParseError { message: msg.into(), position: self.pos }
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
            Ok(Some(Rule { selectors, declarations: decls }))
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
        let kind = match ident.as_str() {
            "node" => TargetKind::Node,
            "way" => TargetKind::Way,
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
        loop {
            self.skip_ws();
            match self.peek() {
                Some('[') => {
                    self.bump();
                    let test = self.parse_tag_test()?;
                    tests.push(test);
                }
                Some('|') | Some(':') => {
                    // Zoom selector or pseudo-class — not supported. Skip.
                    if !*warned {
                        eprintln!("mapcss: ignoring unsupported selector suffix (zoom or pseudo-class)");
                        *warned = true;
                    }
                    self.skip_selector_tail();
                    break;
                }
                _ => break,
            }
        }

        Ok(Some(Selector { kind, tests }))
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
                "color" => match parse_color(&value) {
                    Some(c) => decls.push(Declaration::Color(c)),
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

/// Parse a CSS-ish color into a packed `0xRRGGBB` u32.
fn parse_color(s: &str) -> Option<u32> {
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

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn empty_stylesheet_uses_defaults() {
        let s = Stylesheet::parse("").unwrap();
        assert_eq!(s.node_style(&tags(&[])), NodeStyle::default());
        assert_eq!(s.way_style(&tags(&[])), WayStyle::default());
    }

    #[test]
    fn parse_minimal_way_rule() {
        let s = Stylesheet::parse("way { color: #ff0000; width: 2; }").unwrap();
        let w = s.way_style(&tags(&[]));
        assert_eq!(w.color, 0xFF0000);
        assert_eq!(w.width, 2.0);
    }

    #[test]
    fn parse_minimal_node_rule() {
        let s = Stylesheet::parse("node { color: #00ff00; symbol-size: 14; }").unwrap();
        let n = s.node_style(&tags(&[]));
        assert_eq!(n.color, 0x00FF00);
        assert_eq!(n.size, 14.0);
    }

    #[test]
    fn multi_selector_applies_to_both() {
        let s = Stylesheet::parse("way, node { color: red; }").unwrap();
        assert_eq!(s.way_style(&tags(&[])).color, 0xFF0000);
        assert_eq!(s.node_style(&tags(&[])).color, 0xFF0000);
    }

    #[test]
    fn later_rule_overrides_earlier() {
        let s = Stylesheet::parse("way { color: red; } way { color: blue; }").unwrap();
        assert_eq!(s.way_style(&tags(&[])).color, 0x0000FF);
    }

    #[test]
    fn tag_present_matches_any_value() {
        let s = Stylesheet::parse("way[highway] { color: orange; }").unwrap();
        assert_eq!(s.way_style(&tags(&[("highway", "residential")])).color, 0xFFA500);
        assert_eq!(s.way_style(&tags(&[("highway", "footway")])).color, 0xFFA500);
        // No tag => default way color.
        assert_eq!(s.way_style(&tags(&[])).color, DEFAULT_WAY_COLOR);
    }

    #[test]
    fn tag_equals_matches_exact() {
        let s = Stylesheet::parse("way[highway=residential] { color: orange; }").unwrap();
        assert_eq!(
            s.way_style(&tags(&[("highway", "residential")])).color,
            0xFFA500
        );
        assert_eq!(
            s.way_style(&tags(&[("highway", "footway")])).color,
            DEFAULT_WAY_COLOR
        );
    }

    #[test]
    fn tag_not_equals_matches_others() {
        let s = Stylesheet::parse("way[highway!=motorway] { color: green; }").unwrap();
        assert_eq!(
            s.way_style(&tags(&[("highway", "residential")])).color,
            0x008000
        );
        assert_eq!(
            s.way_style(&tags(&[("highway", "motorway")])).color,
            DEFAULT_WAY_COLOR
        );
        // No highway tag at all -> not-equals is true by convention.
        assert_eq!(s.way_style(&tags(&[])).color, 0x008000);
    }

    #[test]
    fn unknown_property_ignored() {
        let s = Stylesheet::parse("way { color: red; casing-width: 2; width: 3; }").unwrap();
        let w = s.way_style(&tags(&[]));
        assert_eq!(w.color, 0xFF0000);
        assert_eq!(w.width, 3.0);
    }

    #[test]
    fn comments_are_stripped() {
        let s = Stylesheet::parse("/* hi */ way { /* c */ color: red; }").unwrap();
        assert_eq!(s.way_style(&tags(&[])).color, 0xFF0000);
    }

    #[test]
    fn short_hex_color() {
        let s = Stylesheet::parse("way { color: #f00; }").unwrap();
        assert_eq!(s.way_style(&tags(&[])).color, 0xFF0000);
    }

    #[test]
    fn default_stylesheet_parses() {
        let s = Stylesheet::load_default();
        // Spot-check a couple of rules from the embedded asset.
        assert_eq!(
            s.way_style(&tags(&[("highway", "residential")])).color,
            0xff8c00
        );
        assert_eq!(
            s.way_style(&tags(&[("highway", "footway")])).color,
            0x8b4513
        );
        assert_eq!(
            s.node_style(&tags(&[("amenity", "pub")])).color,
            0xff1493
        );
    }

    #[test]
    fn width_on_node_aliases_symbol_size() {
        let s = Stylesheet::parse("node { width: 7; }").unwrap();
        assert_eq!(s.node_style(&tags(&[])).size, 7.0);
    }
}
