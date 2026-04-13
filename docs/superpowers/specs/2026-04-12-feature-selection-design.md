# Feature Selection — Design Spec

Date: 2026-04-12
Status: Approved (ready for implementation plan)

## Goal

Let the user click on an OSM node or way in the map and see its tags in a
panel on the right, below the existing layer list. Read-only — no editing in
this iteration.

## Behavior summary

- Click near a node (≤ 8px) selects that node.
- Otherwise, click near a way (≤ 6px to any segment) selects that way.
- Selected feature is highlighted on the map.
- Right panel shows `Node #id` / `Way #id`, a link to the feature on
  openstreetmap.org, and its tag list.
- Clicking empty map area deselects.
- Drags never count as clicks (movement < 4px threshold between mouse-down
  and mouse-up).
- Relations are not selectable (out of scope).

## Architecture

### New module: `src/selection.rs`

```rust
pub enum FeatureKind { Node, Way }

pub struct FeatureRef {
    pub layer_name: String,
    pub kind: FeatureKind,
    pub id: i64,
}

pub struct HitCandidate {
    pub feature: FeatureRef,
    pub kind: FeatureKind,
    pub dist_px: f32,
}
```

Plus pure-function helpers:

- `point_to_segment_distance(p, a, b) -> f32` — standard projection clamped
  to `[0,1]`, falling back to endpoint distance when the click is past an
  endpoint.
- `resolve_hits(per_layer: Vec<Vec<HitCandidate>>) -> Option<FeatureRef>` —
  cross-layer resolver. Nodes shadow ways within the node tolerance
  (already enforced per-layer). Nearest overall wins; on exact distance
  ties the later-drawn (topmost) layer wins, so `per_layer` is passed in
  draw order.

### `MapLayer` trait extension (`src/layers/mod.rs`)

Two optional methods with default no-op impls so only `OsmLayer` needs to
care:

```rust
fn hit_test(&self, _viewport: &Viewport, _screen_pt: Point<Pixels>)
    -> Vec<HitCandidate> { Vec::new() }

fn render_highlight(
    &self,
    _viewport: &Viewport,
    _bounds: Bounds<Pixels>,
    _window: &mut Window,
    _feature: &FeatureRef,
) {}
```

`OsmLayer` implements both. Other layers inherit the defaults.

### `MapViewer` state

```rust
selected: Option<FeatureRef>,
mouse_down_pos: Option<Point<Pixels>>, // for click-vs-drag detection
```

## Hit testing

### Per-layer (`OsmLayer::hit_test`)

Only runs when the layer is visible and has data. In screen space, using the
same `viewport.geo_to_screen` used by rendering:

1. For each node whose projected position is on-screen, compute pixel
   distance to the click. Collect those with `dist ≤ 8.0`.
2. If any nodes were collected, return them and stop — ways are **not**
   considered for this layer.
3. Otherwise, for each way with ≥ 2 nodes visible, compute the minimum
   point-to-segment distance across its segments. Collect ways with
   `dist ≤ 6.0` (way stroke is 4px; extra 2px of slop).

### Cross-layer (`MapViewer`)

Iterate visible OSM layers in draw order, collecting each layer's
candidates into a `Vec<Vec<HitCandidate>>`, then call `resolve_hits`:

- Pick the minimum-distance candidate overall.
- On exact ties, the later-drawn (topmost) layer wins.

### Click vs. drag

- On `MouseDown`: record `mouse_down_pos`.
- On `MouseUp`: if `|up − down| < 4px`, treat as a click; run hit-test.
  Otherwise ignore (drag already consumed by viewport).
- Reset `mouse_down_pos` after every up.

## Rendering the highlight

`MapViewer` invokes `render_highlight` on the owning layer after normal
canvas rendering, so the highlight sits on top of both tile and vector
content.

- **Ways:** redraw the selected way using
  `PathBuilder::stroke(px(way_width + 4.0))` in accent color `#ff4081`.
  No glow.
- **Nodes:** inside the selected layer's `render_elements`, when the
  selection belongs to this layer, push one extra absolutely-positioned
  `div` — a 20×20 outlined square centered on the node with a 2px accent
  border and transparent fill — behind a redraw of the node itself so the
  original color is still visible.
- If the selected feature's layer is hidden, removed, or no longer
  contains the feature, `MapViewer` clears `selected` on the next render
  pass.

## Right panel layout

The existing 280px right panel becomes a vertical stack:

1. **Layer Controls** (unchanged header + layer list). No longer uses
   `flex_1`; takes natural height with internal scroll if long.
2. **Divider.**
3. **Selection panel** (new). Takes remaining space (`flex_1`),
   scrollable.

Selection panel contents:

- **Empty state:** muted text "Click a feature to see its tags."
- **Selected state:**
  - Bold header: `Node #1234567` or `Way #89101112`.
  - Clickable row: `View on openstreetmap.org ↗` — opens
    `https://www.openstreetmap.org/{node|way}/{id}` via `cx.open_url`.
  - Two-column tag list (key left, value right). If no tags, show muted
    "(no tags)".

Styling matches the existing panel palette; no edit controls.

## Testing

### Unit tests

Live in `src/selection.rs` (`#[cfg(test)]` module). No GPUI dependency —
all pure math / data.

- `point_to_segment_distance`:
  - Click orthogonal to segment midpoint.
  - Click past a segment endpoint falls back to endpoint distance.
  - Zero-length segment (a == b) returns distance to the point.
- `OsmLayer::hit_test` against a small synthetic `OsmData`:
  - Click within 8px of a node returns that node only, even when a way
    passes through the same spot.
  - Click > 8px from any node but within 6px of a way returns the way.
  - Click outside all tolerances returns an empty vector.
- `resolve_hits`:
  - Nearest candidate wins.
  - Exact-tie resolution: later-drawn layer wins.

### In-situ scripted tests

Use the screenshot harness (`--script`).

- **DSL extension:** add `load_osm PATH` op that calls the same OSM parse
  path as ⌘O, pushing the result into `SHARED_OSM_DATA`. Waits for the
  layer to appear before returning (treat as idle-blocking or use
  `wait_idle` afterwards).
- **Fixture:** `docs/screenshots/fixtures/select.osm` — a small hand-made
  OSM XML with:
  - One tagged node (a POI) at a known lat/lon.
  - One untagged node used only as way geometry, coincident with a way
    segment.
  - One tagged way.
- **Script:** `docs/screenshots/select.osmscript`:
  1. `window 1200 800`
  2. `viewport <lat> <lon> <zoom>` (frames the fixture)
  3. `load_osm docs/screenshots/fixtures/select.osm`
  4. `wait_idle 5s`
  5. `click` on the tagged node → `capture out/select-node.png`
  6. `click` on empty space → `capture out/select-empty.png`
  7. `click` on the tagged way (away from its nodes) →
     `capture out/select-way.png`
- Screenshots are the oracle (visual inspection / diff). A future `expect`
  op could add text assertions, but that's out of scope.
- macOS-only (same constraint as the harness). Unit tests still run in CI
  on all platforms.

## Out of scope

- Selecting relations.
- Multi-select / rubber-band selection.
- Editing tags.
- Keyboard-based selection (Escape to deselect, Tab to cycle, etc.).
- Hover preview.
- Adding an `expect`-style assertion op to the script DSL.
