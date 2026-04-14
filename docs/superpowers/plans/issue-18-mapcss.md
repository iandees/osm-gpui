# Issue 18: Initial support for MapCSS

## Context

The renderer currently hardcodes node/way colors and sizes in `OsmLayer`
(`src/layers/osm_layer.rs:139-142` and `159-162`: node `0xFFD700`, way
`0x4169E1`, node size `10.0`, way width `4.0`). The issue asks for
initial MapCSS support — scoped explicitly to "line weight, color, and
node styling and color".

MapCSS (JOSM variant) is CSS-like. A full parser is out of scope for
"initial". This PR lands a minimal subset parser, a default embedded
stylesheet, and wires the renderer to pick per-feature styles by
matching each node/way's OSM tags against the loaded rules.

## Approach

### New module: `src/style/` (`mod.rs`, `mapcss.rs`)

Hand-rolled parser — no new crate dependency.

- Selectors: `node` or `way`, each optionally followed by one or more
  tag conditions `[key]`, `[key=value]`, `[key!=value]`. Multiple
  comma-separated selectors per rule.
- Declaration block `{ … }` with semicolon-separated properties.
- Supported properties:
  - `way`: `color` (hex or named), `width` (float px).
  - `node`: `color`, `symbol-size` (float px; also accept `width` as
    an alias — some MapCSS dialects use it).
- Colors: `#rgb`, `#rrggbb`, plus a small named set (`red`, `blue`,
  `green`, `yellow`, `black`, `white`, `gray`, `orange`).
- Comments: `/* … */` stripped before parse.
- Unknown properties / unsupported selectors: ignore and emit a single
  parse-time warning via `log::warn!` (forward-compat for richer
  stylesheets).

Public surface:

- `Stylesheet` — parsed ruleset.
- `Stylesheet::parse(&str) -> Result<Stylesheet, ParseError>`.
- `Stylesheet::default()` — `include_str!("../../assets/default.mapcss")`
  parsed at startup; if parse fails, panic with a clear message (the
  asset is compiled in, so a failure is a build-time bug).
- `NodeStyle { color: u32, size: f32 }`,
  `WayStyle { color: u32, width: f32 }`. Pack color as `u32` so
  `(color, width.to_bits())` is trivially hashable for grouping.
- `Stylesheet::node_style(&HashMap<String,String>) -> NodeStyle`
  and `Stylesheet::way_style(…) -> WayStyle` — walk rules in source
  order; matching rules merge property-by-property (last wins). A
  hardcoded baseline (current defaults: gold node, royal blue way)
  acts as the starting point, so an empty stylesheet still renders.

### Embedded default: `assets/default.mapcss`

```mapcss
way                  { color: #4169E1; width: 4; }
way[highway]         { color: #ff8c00; width: 5; }
way[highway=footway] { color: #8b4513; width: 2; }
way[waterway]        { color: #1e90ff; width: 3; }
way[building]        { color: #808080; width: 1; }

node           { color: #FFD700; symbol-size: 10; }
node[amenity]  { color: #ff1493; symbol-size: 12; }
node[shop]     { color: #32cd32; symbol-size: 12; }
```

### Render wiring: `src/layers/osm_layer.rs`

- Add `stylesheet: Arc<Stylesheet>` field on `OsmLayer`. Replace the
  four hardcoded fields (`node_color`, `way_color`, `node_size`,
  `way_width`) with this single field.
- `OsmLayer::new()` / `new_with_data()` load `Stylesheet::default()`.
- Refactor the ways loop (`src/layers/osm_layer.rs:251-299`) to group
  ways by `(color_u32, width_bits)`: build a
  `HashMap<(u32, u32), PathBuilder>`, then emit one `paint_path` per
  group after the loop. Keeps the batching gains from PR #5 while
  honoring per-way style.
- Nodes loop (`:307-322`): look up each node's style via
  `stylesheet.node_style(&node.tags)` and size/color the quad from the
  result.
- `set_style(...)` method at `:198`: only callers are internal to
  `osm_layer.rs` (verified via grep). Delete it.
- The highlight/selection paths that currently read `self.node_size`
  and `self.way_width` (e.g. `:445`, `:483`) need to look up a style
  too — use the selected feature's tags to get its style, then derive
  the highlight from that style's width/size.
- Tags-table debug output (`:330-331`): replace the `Node Size`/`Way
  Width` entries with something more useful once they're per-feature,
  or drop them.
- `lib.rs`: `pub mod style;`.

### Testing: unit tests in `src/style/mapcss.rs`

At the bottom of the file in `#[cfg(test)] mod tests`:

- Parse minimal rule → expected selector, color, width.
- Multi-selector rule (`way, node { color: red; }`) applies to both.
- Later rule with same selector overrides earlier.
- Tag match semantics: `[highway]` matches any `highway=*`;
  `[highway=residential]` matches exact; `[highway!=motorway]` matches
  others.
- Unknown properties ignored; parse still succeeds.
- Default stylesheet (`include_str!`) parses cleanly.

## Verification

- `cargo build --release` (clean; no new warnings beyond current main)
- `cargo test --lib`
- `cargo run --release -- --script docs/screenshots/smoke.osmscript` —
  app starts, default stylesheet applied, rendering works.

## Out of scope

- Zoom-range selectors (`|z14-16`).
- Pseudo-classes (`:closed`, `:hover`).
- Eval/expressions (`eval(...)`).
- Icons, labels, text styling.
- Area fills for closed ways.
- User-supplied stylesheet loading at runtime (CLI flag / menu) —
  follow-up once the parser is proven.
- Casing (`casing-color`, `casing-width`).
- Live-reload of the stylesheet.
- Any changes to `src/map.rs`, `src/mercator.rs`, `src/background.rs`,
  `src/http_image_loader.rs`, `src/data.rs` (declared dead).
