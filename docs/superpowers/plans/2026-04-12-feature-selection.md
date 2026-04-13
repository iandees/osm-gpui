# Feature Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user click on an OSM node or way in the map to see its tags in a right-panel section below the existing layer list, with the selected feature highlighted on the map.

**Architecture:** A new `selection` module holds `FeatureRef` / `HitCandidate` types and pure hit-test math. `MapLayer` gains two optional methods (`hit_test`, `render_highlight`) that `OsmLayer` implements. `MapViewer` tracks `selected: Option<FeatureRef>`, runs cross-layer hit-testing on left-click-release (distinguished from drag by a 4px movement threshold), and renders the highlight + selection panel. A new `load_osm PATH` op in the screenshot-harness DSL enables in-situ scripted testing.

**Tech Stack:** Rust, GPUI (ways drawn via `PathBuilder`/`window.paint_path`; nodes as absolutely-positioned `div`s), existing `quick-xml` OSM parser, existing script harness in `src/script/`.

**Spec:** `docs/superpowers/specs/2026-04-12-feature-selection-design.md`

---

## File Structure

**Create:**
- `src/selection.rs` — `FeatureKind`, `FeatureRef`, `HitCandidate`, `point_to_segment_distance`, `resolve_hits`, unit tests.
- `docs/screenshots/fixtures/select.osm` — tiny hand-written OSM XML fixture.
- `docs/screenshots/select.osmscript` — scripted in-situ test.

**Modify:**
- `src/lib.rs` — export `selection` module.
- `src/layers/mod.rs` — add `hit_test` and `render_highlight` trait methods with default no-op impls.
- `src/layers/osm_layer.rs` — implement `hit_test`, `render_highlight`; add `set_selected_feature` hook so `render_elements` can draw the node ring.
- `src/main.rs` — `MapViewer.selected`, `MapViewer.mouse_down_pos`, click-vs-drag detection, cross-layer resolution, highlight invocation, right-panel split, selection panel UI, `load_osm` CLI wiring via `AppHandle` + `LiveApp`.
- `src/script/op.rs` — add `Op::LoadOsm { path: String }`.
- `src/script/parser.rs` — parse `load_osm PATH`, add unit test.
- `src/script/runner.rs` — dispatch `LoadOsm` via `AppHandle::load_osm`; update test fake.
- `README.md` — document `load_osm` op in the Scripted screenshots section.

---

## Task 1: Selection module — types and pure math

**Files:**
- Create: `src/selection.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/selection.rs`:

```rust
//! Selection types and pure hit-testing math.

use gpui::{Pixels, Point};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    Node,
    Way,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureRef {
    pub layer_name: String,
    pub kind: FeatureKind,
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HitCandidate {
    pub feature: FeatureRef,
    pub kind: FeatureKind,
    pub dist_px: f32,
}

/// Shortest distance (in screen pixels) from point `p` to line segment `a`-`b`.
/// Handles zero-length segments by returning the distance to the single point.
pub fn point_to_segment_distance(
    p: Point<Pixels>,
    a: Point<Pixels>,
    b: Point<Pixels>,
) -> f32 {
    let px = p.x.0;
    let py = p.y.0;
    let ax = a.x.0;
    let ay = a.y.0;
    let bx = b.x.0;
    let by = b.y.0;

    let dx = bx - ax;
    let dy = by - ay;
    let len_sq = dx * dx + dy * dy;
    if len_sq <= f32::EPSILON {
        // Degenerate segment: just return distance to `a`.
        let ex = px - ax;
        let ey = py - ay;
        return (ex * ex + ey * ey).sqrt();
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0);
    let qx = ax + t * dx;
    let qy = ay + t * dy;
    let ex = px - qx;
    let ey = py - qy;
    (ex * ex + ey * ey).sqrt()
}

/// Pick the winning feature across all visible OSM layers.
///
/// `per_layer` is expected in draw order (earliest-drawn first, topmost last).
/// Nearest candidate wins; on exact distance ties, later-drawn (topmost) wins.
pub fn resolve_hits(per_layer: Vec<Vec<HitCandidate>>) -> Option<FeatureRef> {
    let mut best: Option<(f32, usize, FeatureRef)> = None;
    for (layer_idx, candidates) in per_layer.into_iter().enumerate() {
        for c in candidates {
            match &best {
                None => best = Some((c.dist_px, layer_idx, c.feature)),
                Some((d, li, _)) => {
                    if c.dist_px < *d || (c.dist_px == *d && layer_idx >= *li) {
                        best = Some((c.dist_px, layer_idx, c.feature));
                    }
                }
            }
        }
    }
    best.map(|(_, _, f)| f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    fn pt(x: f32, y: f32) -> Point<Pixels> {
        point(px(x), px(y))
    }

    fn fref(name: &str, kind: FeatureKind, id: i64) -> FeatureRef {
        FeatureRef { layer_name: name.into(), kind, id }
    }

    #[test]
    fn orthogonal_midpoint_distance() {
        // Segment from (0,0) to (10,0); click at (5, 3) → distance 3.
        let d = point_to_segment_distance(pt(5.0, 3.0), pt(0.0, 0.0), pt(10.0, 0.0));
        assert!((d - 3.0).abs() < 1e-4, "got {}", d);
    }

    #[test]
    fn past_endpoint_falls_back_to_endpoint() {
        // Segment (0,0)-(10,0); click at (13, 4) → nearest point is (10,0), distance 5.
        let d = point_to_segment_distance(pt(13.0, 4.0), pt(0.0, 0.0), pt(10.0, 0.0));
        assert!((d - 5.0).abs() < 1e-4, "got {}", d);
    }

    #[test]
    fn zero_length_segment_returns_point_distance() {
        let d = point_to_segment_distance(pt(3.0, 4.0), pt(0.0, 0.0), pt(0.0, 0.0));
        assert!((d - 5.0).abs() < 1e-4, "got {}", d);
    }

    #[test]
    fn resolve_returns_none_on_empty() {
        assert!(resolve_hits(vec![]).is_none());
        assert!(resolve_hits(vec![vec![], vec![]]).is_none());
    }

    #[test]
    fn resolve_picks_nearest() {
        let a = HitCandidate {
            feature: fref("L0", FeatureKind::Node, 1),
            kind: FeatureKind::Node,
            dist_px: 5.0,
        };
        let b = HitCandidate {
            feature: fref("L0", FeatureKind::Way, 2),
            kind: FeatureKind::Way,
            dist_px: 3.0,
        };
        let winner = resolve_hits(vec![vec![a, b]]).unwrap();
        assert_eq!(winner.id, 2);
    }

    #[test]
    fn resolve_tie_prefers_later_layer() {
        let a = HitCandidate {
            feature: fref("bottom", FeatureKind::Node, 1),
            kind: FeatureKind::Node,
            dist_px: 2.0,
        };
        let b = HitCandidate {
            feature: fref("top", FeatureKind::Node, 99),
            kind: FeatureKind::Node,
            dist_px: 2.0,
        };
        let winner = resolve_hits(vec![vec![a], vec![b]]).unwrap();
        assert_eq!(winner.layer_name, "top");
        assert_eq!(winner.id, 99);
    }
}
```

- [ ] **Step 2: Wire the module into the library**

In `src/lib.rs`, add (near the other `pub mod` lines):

```rust
pub mod selection;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --lib selection -- --nocapture`
Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/selection.rs src/lib.rs
git commit -m "Add selection module with hit-test math"
```

---

## Task 2: Extend MapLayer trait with hit_test and render_highlight

**Files:**
- Modify: `src/layers/mod.rs`

- [ ] **Step 1: Add the two optional methods with no-op defaults**

In `src/layers/mod.rs`, inside `pub trait MapLayer`, add after the existing `stats` method:

```rust
    /// Return hit candidates near a screen-space point. Default: none.
    /// Implementations should only return candidates within their own tolerance.
    fn hit_test(
        &self,
        _viewport: &Viewport,
        _screen_pt: Point<Pixels>,
    ) -> Vec<crate::selection::HitCandidate> {
        Vec::new()
    }

    /// Draw a highlight overlay for `feature` if it belongs to this layer.
    /// Default: no-op.
    fn render_highlight(
        &self,
        _viewport: &Viewport,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _feature: &crate::selection::FeatureRef,
    ) {}
```

Also add a convenience helper on `LayerManager` for cross-layer hit collection:

```rust
    /// Run hit_test against every visible layer, returning results in draw order.
    pub fn hit_test_all(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<Vec<crate::selection::HitCandidate>> {
        self.layers
            .iter()
            .filter(|layer| layer.is_visible())
            .map(|layer| layer.hit_test(viewport, screen_pt))
            .collect()
    }

    /// Render `feature`'s highlight by asking the owning layer (matched by name).
    /// No-op if no layer with that name exists.
    pub fn render_highlight(
        &self,
        feature: &crate::selection::FeatureRef,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) {
        if let Some(layer) = self.find_layer(&feature.layer_name) {
            if layer.is_visible() {
                layer.render_highlight(viewport, bounds, window, feature);
            }
        }
    }
```

- [ ] **Step 2: Build to confirm the trait compiles**

Run: `cargo build`
Expected: clean build (no errors). Warnings about unused `crate::selection` re-import are OK.

- [ ] **Step 3: Commit**

```bash
git add src/layers/mod.rs
git commit -m "Extend MapLayer trait with hit_test and render_highlight"
```

---

## Task 3: OsmLayer hit_test

**Files:**
- Modify: `src/layers/osm_layer.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/layers/osm_layer.rs` (or create a `#[cfg(test)] mod tests` block at the end of the file if none exists):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::{OsmData, OsmNode, OsmWay};
    use crate::selection::FeatureKind;
    use crate::viewport::Viewport;
    use gpui::{point, px, size};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn empty_tags() -> HashMap<String, String> {
        HashMap::new()
    }

    /// Build a viewport whose center projects a chosen (lat, lon) to the middle
    /// of an 800x600 map area. Zoom 18 is high enough that a degree-scale offset
    /// in node positions translates to many pixels.
    fn viewport_centered_on(lat: f64, lon: f64) -> Viewport {
        Viewport::new(lat, lon, 18.0, size(px(800.0), px(600.0)))
    }

    fn data_with(nodes: Vec<OsmNode>, ways: Vec<OsmWay>) -> Arc<OsmData> {
        let mut map = HashMap::new();
        for n in nodes {
            map.insert(n.id, n);
        }
        Arc::new(OsmData {
            nodes: map,
            ways,
            relations: Vec::new(),
            bounds: None,
        })
    }

    #[test]
    fn hit_test_node_wins_over_coincident_way() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon + 0.001, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data("L", data);

        // Node #1 projects to viewport center (400, 300). Click exactly on it.
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, FeatureKind::Node);
        assert_eq!(hits[0].feature.id, 1);
    }

    #[test]
    fn hit_test_falls_through_to_way() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon - 0.001, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon + 0.001, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data("L", data);

        // Click at viewport center (400,300), which is on the way between n1 and n2
        // but far from either endpoint.
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits.iter().all(|h| h.kind == FeatureKind::Way));
        assert!(hits.iter().any(|h| h.feature.id == 10));
    }

    #[test]
    fn hit_test_no_match_returns_empty() {
        let n = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n], vec![]);
        let viewport = viewport_centered_on(40.0, -74.0);
        let layer = OsmLayer::new_with_data("L", data);

        // Node is at (400, 300); click far away.
        let hits = layer.hit_test(&viewport, point(px(50.0), px(50.0)));
        assert!(hits.is_empty(), "unexpected hits: {:?}", hits);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib layers::osm_layer`
Expected: FAIL — `hit_test` returns empty by default, so the "node wins" and "way" tests fail.

- [ ] **Step 3: Implement `hit_test`**

At the top of `src/layers/osm_layer.rs`, add:

```rust
use crate::selection::{FeatureKind, FeatureRef, HitCandidate, point_to_segment_distance};
```

Then add this method inside `impl MapLayer for OsmLayer`, after the existing methods (and remove the default for these two — the trait default no-ops won't be used for `OsmLayer`):

```rust
    fn hit_test(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
    ) -> Vec<HitCandidate> {
        const NODE_TOL: f32 = 8.0;
        const WAY_TOL: f32 = 6.0;

        let Some(ref data) = self.osm_data else { return Vec::new(); };

        // Phase 1: nodes within NODE_TOL.
        let mut node_hits: Vec<HitCandidate> = Vec::new();
        for node in data.nodes.values() {
            if let Some((lat, lon)) = validate_coords(node.lat, node.lon) {
                if !viewport.is_visible(lat, lon) { continue; }
                let sp = viewport.geo_to_screen(lat, lon);
                if !is_point_valid(sp) { continue; }
                let dx = sp.x.0 - screen_pt.x.0;
                let dy = sp.y.0 - screen_pt.y.0;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist <= NODE_TOL {
                    node_hits.push(HitCandidate {
                        feature: FeatureRef {
                            layer_name: self.name.clone(),
                            kind: FeatureKind::Node,
                            id: node.id,
                        },
                        kind: FeatureKind::Node,
                        dist_px: dist,
                    });
                }
            }
        }
        if !node_hits.is_empty() {
            return node_hits;
        }

        // Phase 2: ways within WAY_TOL. Compute shortest segment distance per way.
        let mut way_hits: Vec<HitCandidate> = Vec::new();
        for way in data.ways.iter() {
            if way.nodes.len() < 2 { continue; }
            let mut projected: Vec<Point<Pixels>> = Vec::with_capacity(way.nodes.len());
            for node_id in &way.nodes {
                if let Some(n) = data.nodes.get(node_id) {
                    if let Some((lat, lon)) = validate_coords(n.lat, n.lon) {
                        let sp = viewport.geo_to_screen(lat, lon);
                        if is_point_valid(sp) {
                            projected.push(sp);
                        }
                    }
                }
            }
            if projected.len() < 2 { continue; }
            let mut best = f32::INFINITY;
            for w in projected.windows(2) {
                let d = point_to_segment_distance(screen_pt, w[0], w[1]);
                if d < best { best = d; }
            }
            if best <= WAY_TOL {
                way_hits.push(HitCandidate {
                    feature: FeatureRef {
                        layer_name: self.name.clone(),
                        kind: FeatureKind::Way,
                        id: way.id,
                    },
                    kind: FeatureKind::Way,
                    dist_px: best,
                });
            }
        }
        way_hits
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib layers::osm_layer`
Expected: PASS for all three `hit_test_*` tests.

- [ ] **Step 5: Commit**

```bash
git add src/layers/osm_layer.rs
git commit -m "Implement OsmLayer::hit_test with node-then-way fallthrough"
```

---

## Task 4: OsmLayer render_highlight + selected-feature hook

**Files:**
- Modify: `src/layers/osm_layer.rs`

- [ ] **Step 1: Add accent-color constant and highlight-aware state**

Near the top of `src/layers/osm_layer.rs` (after the existing `use` lines), add:

```rust
const SELECTION_ACCENT: u32 = 0xFF4081;
```

Add a new field + setter on `OsmLayer`:

```rust
pub struct OsmLayer {
    name: String,
    visible: bool,
    osm_data: Option<Arc<OsmData>>,
    node_color: Rgba,
    way_color: Rgba,
    node_size: f32,
    way_width: f32,
    /// Feature to highlight in `render_elements` (set each frame by MapViewer).
    highlight: Option<FeatureRef>,
}
```

Initialize `highlight: None` in both `new()` and `new_with_data()`.

Add:

```rust
impl OsmLayer {
    /// Set (or clear) which feature should be drawn highlighted this frame.
    /// MapViewer calls this every frame based on the current selection.
    pub fn set_highlight(&mut self, feature: Option<FeatureRef>) {
        self.highlight = feature;
    }
}
```

- [ ] **Step 2: Extend `render_elements` to draw a node ring for the highlighted node**

Inside `impl MapLayer for OsmLayer`, modify `render_elements` so that when `self.highlight` matches a node in this layer, an extra "ring" div is pushed behind the normal node div. Replace the existing body of the `for node in osm_data.nodes.values()` loop with:

```rust
        for node in osm_data.nodes.values() {
            if let Some((valid_lat, valid_lon)) = validate_coords(node.lat, node.lon) {
                if viewport.is_visible(valid_lat, valid_lon) {
                    let screen_pos = viewport.geo_to_screen(valid_lat, valid_lon);
                    if is_point_valid(screen_pos) {
                        let is_selected = matches!(
                            &self.highlight,
                            Some(FeatureRef { layer_name, kind: FeatureKind::Node, id })
                                if layer_name == &self.name && *id == node.id
                        );

                        if is_selected {
                            let ring_size = self.node_size * 2.0;
                            let ring = div()
                                .absolute()
                                .left(px(screen_pos.x.0 - ring_size / 2.0))
                                .top(px(screen_pos.y.0 - ring_size / 2.0))
                                .w(px(ring_size))
                                .h(px(ring_size))
                                .border_2()
                                .border_color(rgb(SELECTION_ACCENT))
                                .into_any_element();
                            elements.push(ring);
                        }

                        let node_element = div()
                            .absolute()
                            .left(px(screen_pos.x.0 - self.node_size / 2.0))
                            .top(px(screen_pos.y.0 - self.node_size / 2.0))
                            .w(px(self.node_size))
                            .h(px(self.node_size))
                            .bg(self.node_color)
                            .into_any_element();
                        elements.push(node_element);
                    }
                }
            }
        }
```

- [ ] **Step 3: Implement `render_highlight` for ways (and clear for nodes)**

Add to `impl MapLayer for OsmLayer`:

```rust
    fn render_highlight(
        &self,
        viewport: &Viewport,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        feature: &FeatureRef,
    ) {
        if feature.layer_name != self.name { return; }
        let Some(ref osm_data) = self.osm_data else { return; };

        match feature.kind {
            FeatureKind::Node => {
                // Nodes are highlighted via render_elements (needs layout, not canvas).
            }
            FeatureKind::Way => {
                let Some(way) = osm_data.ways.iter().find(|w| w.id == feature.id) else { return; };
                if way.nodes.len() < 2 { return; }

                let origin_x = bounds.origin.x;
                let origin_y = bounds.origin.y;

                let mut pts: Vec<Point<Pixels>> = Vec::with_capacity(way.nodes.len());
                for node_id in &way.nodes {
                    if let Some(n) = osm_data.nodes.get(node_id) {
                        if let Some((lat, lon)) = validate_coords(n.lat, n.lon) {
                            let sp = viewport.geo_to_screen(lat, lon);
                            if is_point_valid(sp) {
                                pts.push(point(
                                    px(sp.x.0 + origin_x.0),
                                    px(sp.y.0 + origin_y.0),
                                ));
                            }
                        }
                    }
                }
                if pts.len() < 2 { return; }

                let mut builder = PathBuilder::stroke(px(self.way_width + 4.0));
                for (i, p) in pts.iter().enumerate() {
                    if i == 0 { builder.move_to(*p); } else { builder.line_to(*p); }
                }
                if let Ok(path) = builder.build() {
                    window.paint_path(path, rgb(SELECTION_ACCENT));
                }
            }
        }
    }
```

- [ ] **Step 4: Build to confirm it compiles**

Run: `cargo build`
Expected: clean build; a few unused-import warnings about `FeatureRef`/`FeatureKind` if the module-level `use` needs adjustment — add whatever the compiler asks for.

- [ ] **Step 5: Run existing tests**

Run: `cargo test --lib`
Expected: all tests pass (Task 3 tests still green).

- [ ] **Step 6: Commit**

```bash
git add src/layers/osm_layer.rs
git commit -m "Render selection highlight on OsmLayer nodes and ways"
```

---

## Task 5: MapViewer click-vs-drag detection and selection state

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add `selected` and `mouse_down_pos` to `MapViewer`**

In `src/main.rs`, update the struct:

```rust
struct MapViewer {
    viewport: Viewport,
    layer_manager: LayerManager,
    tile_cache: Arc<Mutex<TileCache>>,
    first_dataset_fitted: bool,
    status_message: Option<(String, Instant)>,
    selected: Option<osm_gpui::selection::FeatureRef>,
    mouse_down_pos: Option<Point<Pixels>>,
}
```

Add the new fields (`selected: None`, `mouse_down_pos: None`) to every `Self { ... }` construction in `MapViewer::new`.

Add a `Point` import if not already imported: `use gpui::Point;`.

- [ ] **Step 2: Record mouse-down position in the adjusted coordinate space**

Edit `handle_mouse_down`:

```rust
    fn handle_mouse_down(&mut self, event: &MouseDownEvent) {
        let header_height = px(48.0);
        let adjusted_position = point(event.position.x, event.position.y - header_height);
        self.viewport.handle_mouse_down(adjusted_position);
        self.mouse_down_pos = Some(adjusted_position);
    }
```

- [ ] **Step 3: On mouse up, detect click vs drag, run hit-test, and notify**

Replace `handle_mouse_up`:

```rust
    fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let header_height = px(48.0);
        let up_pos = point(event.position.x, event.position.y - header_height);
        let was_click = match self.mouse_down_pos.take() {
            Some(down) => {
                let dx = up_pos.x.0 - down.x.0;
                let dy = up_pos.y.0 - down.y.0;
                (dx * dx + dy * dy).sqrt() < 4.0
            }
            None => false,
        };
        self.viewport.handle_mouse_up();
        if was_click {
            let before = self.selected.clone();
            self.handle_map_click(up_pos);
            if before != self.selected {
                cx.notify();
            }
        }
    }

    fn handle_map_click(&mut self, screen_pt: Point<Pixels>) {
        let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
        self.selected = osm_gpui::selection::resolve_hits(per_layer);
    }
```

- [ ] **Step 4: Update the mouse-up listeners to forward `cx`, and fix both script call sites**

In `src/main.rs`, find the two listener bindings for `on_mouse_up` and `on_mouse_up_out` (around line 627–638). Change each from:

```rust
    cx.listener(|this, ev: &MouseUpEvent, _, _| {
        this.handle_mouse_up(ev);
    }),
```

to:

```rust
    cx.listener(|this, ev: &MouseUpEvent, _, cx| {
        this.handle_mouse_up(ev, cx);
    }),
```

Also update the two call sites inside `process_script_command` (the `ScriptCommand::Click` and `ScriptCommand::Drag` branches — around lines 485–510) that construct a `MouseUpEvent` and call `self.handle_mouse_up(&ev)`. Change both to `self.handle_mouse_up(&ev, cx);` — `cx` is already in scope inside `process_script_command`.

Run: `cargo build`
Expected: clean build.

- [ ] **Step 5: Run tests to make sure nothing regressed**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "Detect click vs drag and set MapViewer.selected from hit test"
```

---

## Task 6: Apply selection to layers each frame and render highlight

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add `set_highlight` to the `MapLayer` trait**

In `src/layers/mod.rs`, add to `pub trait MapLayer`:

```rust
    /// Tell the layer which feature (if any) is currently selected.
    /// Default: no-op. OsmLayer overrides this to drive `render_elements`.
    fn set_highlight(&mut self, _feature: Option<crate::selection::FeatureRef>) {}
```

In `src/layers/osm_layer.rs`, add inside `impl MapLayer for OsmLayer` (the inherent `set_highlight` from Task 4 can be deleted — this trait method replaces it):

```rust
    fn set_highlight(&mut self, feature: Option<FeatureRef>) {
        self.highlight = feature;
    }
```

- [ ] **Step 2: Add `sync_selection_to_layers` on `MapViewer`**

```rust
    fn sync_selection_to_layers(&mut self) {
        // Clear the selection if its owning layer is gone or hidden, so the
        // right panel never shows info for a feature not drawn on the map.
        if let Some(sel) = &self.selected {
            let still_live = self
                .layer_manager
                .find_layer(&sel.layer_name)
                .map(|l| l.is_visible())
                .unwrap_or(false);
            if !still_live {
                self.selected = None;
            }
        }
        let selected = self.selected.clone();
        for layer in self.layer_manager.layers_mut() {
            if let Some(sel) = &selected {
                if layer.name() == sel.layer_name {
                    layer.set_highlight(Some(sel.clone()));
                    continue;
                }
            }
            layer.set_highlight(None);
        }
    }
```

- [ ] **Step 3: Call the sync helper and paint the highlight in `render`**

In `impl Render for MapViewer::render`, right after the existing `self.layer_manager.update_all();` line, add:

```rust
        self.sync_selection_to_layers();
```

Then inside the canvas closure that currently calls `layer_manager.render_all_canvas`, add a highlight pass after the normal pass. Locate this block in `render`:

```rust
            canvas(
                |_, _, _| {},
                {
                    let viewport_clone = self.viewport.clone();
                    let layer_manager = std::ptr::addr_of!(self.layer_manager);
                    move |bounds, _, window, _| {
                        let layer_manager = unsafe { &*layer_manager };
                        layer_manager.render_all_canvas(&viewport_clone, bounds, window);
                    }
                }
            )
```

Replace the closure so it also draws the highlight. We need the selected feature accessible from inside the closure — capture it by clone:

```rust
            canvas(
                |_, _, _| {},
                {
                    let viewport_clone = self.viewport.clone();
                    let layer_manager = std::ptr::addr_of!(self.layer_manager);
                    let selected = self.selected.clone();
                    move |bounds, _, window, _| {
                        let layer_manager = unsafe { &*layer_manager };
                        layer_manager.render_all_canvas(&viewport_clone, bounds, window);
                        if let Some(sel) = &selected {
                            layer_manager.render_highlight(sel, &viewport_clone, bounds, window);
                        }
                    }
                }
            )
```

- [ ] **Step 4: Build and run tests**

Run: `cargo build && cargo test`
Expected: clean build; all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/layers/mod.rs src/layers/osm_layer.rs
git commit -m "Propagate selection to layers and paint highlight each frame"
```

---

## Task 7: Split the right panel — layer controls + empty selection panel

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Restructure the right panel**

In `MapViewer::render`, find the block that builds the right panel (the outer `.child(div().w(px(280.0))...)`). Replace it so the layer list is no longer `flex_1`, and a new selection section takes the remaining space.

Schematically, the panel becomes:

```rust
            .child(
                div()
                    .w(px(280.0))
                    .h_full()
                    .bg(rgb(0x111827))
                    .border_l_1()
                    .border_color(rgb(0x374151))
                    .flex()
                    .flex_col()
                    // --- Layer Controls header (unchanged) ---
                    .child(
                        div()
                            .h_12()
                            .bg(rgb(0x1f2937))
                            .flex()
                            .items_center()
                            .px_4()
                            .border_b_1()
                            .border_color(rgb(0x374151))
                            .child(
                                div()
                                    .text_color(rgb(0xffffff))
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("🏗️ Layer Controls")
                            )
                    )
                    // --- Layer list (NOT flex_1 anymore) ---
                    .child(
                        div()
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(
                                layer_info.iter().enumerate().map(|(index, (name, is_visible))| {
                                    /* ... existing layer row code unchanged ... */
                                })
                                .collect::<Vec<_>>()
                            )
                    )
                    // --- Divider ---
                    .child(
                        div()
                            .h(px(1.0))
                            .bg(rgb(0x374151))
                    )
                    // --- Selection panel (flex_1, scrollable) ---
                    .child(self.render_selection_panel(cx))
            )
```

Keep the existing layer-row closure exactly as it is today; only the surrounding `div().flex_1().p_4()...` becomes `div().p_4()...` (drop `flex_1`).

- [ ] **Step 2: Add the empty-state selection panel renderer**

Add a method on `MapViewer`:

```rust
    fn render_selection_panel(&self, _cx: &mut Context<Self>) -> gpui::Div {
        let base = div()
            .id("selection-panel")
            .flex_1()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_2();

        match &self.selected {
            None => base.child(
                div()
                    .text_color(rgb(0x6b7280))
                    .text_sm()
                    .child("Click a feature to see its tags.")
            ),
            Some(_sel) => base.child(
                div()
                    .text_color(rgb(0xffffff))
                    .text_sm()
                    .child("(selection panel filled in next task)")
            ),
        }
    }
```

- [ ] **Step 3: Build to confirm**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "Add selection panel scaffold under the layer list"
```

---

## Task 8: Fill the selection panel contents

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Implement the filled selection panel**

Replace `render_selection_panel` from Task 7:

```rust
    fn render_selection_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        use osm_gpui::selection::FeatureKind;

        let base = div()
            .id("selection-panel")
            .flex_1()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_3();

        let Some(sel) = self.selected.clone() else {
            return base.child(
                div()
                    .text_color(rgb(0x6b7280))
                    .text_sm()
                    .child("Click a feature to see its tags.")
            );
        };

        let kind_label = match sel.kind { FeatureKind::Node => "Node", FeatureKind::Way => "Way" };
        let url_kind = match sel.kind { FeatureKind::Node => "node", FeatureKind::Way => "way" };
        let tags_vec: Vec<(String, String)> = self
            .layer_manager
            .find_layer(&sel.layer_name)
            .and_then(|layer| layer.feature_tags(&sel))
            .unwrap_or_default();

        let header = div()
            .text_color(rgb(0xffffff))
            .text_lg()
            .font_weight(gpui::FontWeight::BOLD)
            .child(format!("{} #{}", kind_label, sel.id));

        let link_text = "View on openstreetmap.org ↗".to_string();
        let url = format!("https://www.openstreetmap.org/{}/{}", url_kind, sel.id);
        let link = div()
            .id(("osm-link", sel.id as usize))
            .text_color(rgb(0x60a5fa))
            .text_sm()
            .cursor_pointer()
            .child(link_text)
            .on_mouse_down(
                gpui::MouseButton::Left,
                // `cx.listener` gives us the same `Context<Self>` type used
                // elsewhere in this file; `Context<Self>` derefs to `App`,
                // which provides `open_url`.
                cx.listener(move |_this, _ev: &MouseDownEvent, _, cx| {
                    cx.open_url(&url);
                }),
            );

        let tags_block: gpui::Div = if tags_vec.is_empty() {
            div()
                .text_color(rgb(0x6b7280))
                .text_sm()
                .child("(no tags)")
        } else {
            let mut col = div().flex().flex_col().gap_1();
            for (k, v) in tags_vec {
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            div()
                                .text_color(rgb(0xd1d5db))
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(k)
                        )
                        .child(
                            div()
                                .text_color(rgb(0xffffff))
                                .text_sm()
                                .child(v)
                        )
                );
            }
            col
        };

        base.child(header).child(link).child(tags_block)
    }
```

- [ ] **Step 2: Add `feature_tags` to the `MapLayer` trait**

In `src/layers/mod.rs`, add to the trait:

```rust
    /// Return key/value tags for the given feature if this layer owns it.
    /// Default: `None`.
    fn feature_tags(
        &self,
        _feature: &crate::selection::FeatureRef,
    ) -> Option<Vec<(String, String)>> {
        None
    }
```

In `src/layers/osm_layer.rs`, implement it:

```rust
    fn feature_tags(&self, feature: &FeatureRef) -> Option<Vec<(String, String)>> {
        if feature.layer_name != self.name { return None; }
        let data = self.osm_data.as_ref()?;
        let tags = match feature.kind {
            FeatureKind::Node => {
                let n = data.nodes.get(&feature.id)?;
                n.tags.clone()
            }
            FeatureKind::Way => {
                let w = data.ways.iter().find(|w| w.id == feature.id)?;
                w.tags.clone()
            }
        };
        let mut kv: Vec<(String, String)> = tags.into_iter().collect();
        kv.sort_by(|a, b| a.0.cmp(&b.0));
        Some(kv)
    }
```

- [ ] **Step 3: Build and test**

Run: `cargo build && cargo test`
Expected: clean build; all tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs src/layers/mod.rs src/layers/osm_layer.rs
git commit -m "Render selected feature header, OSM link, and tag list"
```

---

## Task 9: Extend script DSL with `load_osm PATH`

**Files:**
- Modify: `src/script/op.rs`, `src/script/parser.rs`

- [ ] **Step 1: Add `Op::LoadOsm`**

In `src/script/op.rs`, extend the enum:

```rust
pub enum Op {
    Window { w: u32, h: u32 },
    Viewport { lat: f64, lon: f64, zoom: f32 },
    WaitIdle { timeout: Duration },
    Wait { duration: Duration },
    Drag { from: Point2, to: Point2, duration: Duration },
    Click { at: Point2, button: MouseButton },
    Scroll { at: Point2, dx: f32, dy: f32 },
    Key { chord: Chord },
    Capture { path: String },
    Log { message: String },
    LoadOsm { path: String },
}
```

- [ ] **Step 2: Write the failing parser test**

In `src/script/parser.rs`, add to `mod tests`:

```rust
    #[test]
    fn load_osm_captures_path() {
        assert_eq!(
            parse("load_osm path/to/fixture.osm").unwrap()[0].op,
            Op::LoadOsm { path: "path/to/fixture.osm".into() }
        );
    }

    #[test]
    fn load_osm_requires_path() {
        assert!(parse("load_osm").is_err());
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib script::parser::tests::load_osm`
Expected: FAIL — `unknown op 'load_osm'`.

- [ ] **Step 4: Implement the parser branch**

In the `match head { ... }` block of `parse_line`, add:

```rust
        "load_osm" => parse_load_osm(line_no, &rest),
```

Add the function:

```rust
fn parse_load_osm(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 {
        return Err(err(line_no, "load_osm: want PATH"));
    }
    Ok(Op::LoadOsm { path: rest[0].to_string() })
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib script`
Expected: all parser tests pass, including the two new ones.

- [ ] **Step 6: Commit**

```bash
git add src/script/op.rs src/script/parser.rs
git commit -m "Add load_osm op to script DSL"
```

---

## Task 10: Wire `load_osm` into the runner and live app

**Files:**
- Modify: `src/script/runner.rs`, `src/main.rs`

- [ ] **Step 1: Extend `AppHandle`**

In `src/script/runner.rs`, add to the trait:

```rust
    /// Parse and load an OSM file, making it a new layer on the map.
    fn load_osm(&mut self, path: &std::path::Path) -> Result<(), String>;
```

Update the test `Fake`:

```rust
        fn load_osm(&mut self, _p: &std::path::Path) -> Result<(), String> { Ok(()) }
```

Dispatch the op in `run_step`:

```rust
            Op::LoadOsm { path } => {
                let pb = std::path::PathBuf::from(path);
                app.load_osm(&pb)
                    .map_err(|e| RunError { line_no: step.line_no, message: format!("load_osm: {}", e) })?;
                Ok(())
            }
```

Add it to `describe`:

```rust
        Op::LoadOsm { path } => format!("load_osm {}", path),
```

- [ ] **Step 2: Reorder `render()` so queue drains happen before the frame signal**

Today `process_script_command` is called first and its last line is `bus.signal_done_and_frame()`, which wakes any waiting `wait_frame`. The queue drains (`check_for_new_osm_data`, `check_for_layer_requests`, `check_for_download_requests`) run *after* that signal. That means a thread that pushes onto `SHARED_OSM_DATA` and then calls `wait_frame` can wake up before its data has been picked up.

Fix: split the signal out of `process_script_command`, drain the queues in between, then signal explicitly.

In `src/main.rs`, find the end of `process_script_command` and remove the last call:

```rust
        // Signal that this render frame has happened (and any command is done).
        bus.signal_done_and_frame();
```

(The `request_animation_frame` line below it stays — leave it in place.)

In `Render for MapViewer`, update the top of `render` to look like:

```rust
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Consume any pending script command first.
        self.process_script_command(window, cx);

        // Drain cross-thread queues BEFORE signalling the script bus, so
        // ops like `load_osm` (which push here and then call wait_frame)
        // observe the resulting layer on the same frame.
        self.check_for_new_osm_data(cx);
        self.check_for_layer_requests(cx);
        self.check_for_download_requests(cx);

        // Now it's safe to signal: the effects of this frame's commands
        // and pushes are visible.
        if let Some(bus) = SCRIPT_BUS.get() {
            bus.signal_done_and_frame();
        }

        // ... existing body continues (window size, layer updates, UI build) ...
```

Delete the later duplicate calls to `check_for_new_osm_data`, `check_for_layer_requests`, and `check_for_download_requests` further down in `render` — they've moved to the top.

- [ ] **Step 3: Implement `LiveApp::load_osm`**

In `src/main.rs`, inside `impl AppHandle for LiveApp`, add:

```rust
    fn load_osm(&mut self, path: &std::path::Path) -> Result<(), String> {
        let parser = OsmParser::new();
        let path_str = path.to_string_lossy().to_string();
        let data = parser.parse_file(&path_str).map_err(|e| e.to_string())?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("OSM").to_string();
        if let Some(q) = SHARED_OSM_DATA.get() {
            if let Ok(mut guard) = q.lock() {
                guard.push((stem, data));
            } else {
                return Err("SHARED_OSM_DATA mutex poisoned".into());
            }
        } else {
            return Err("SHARED_OSM_DATA not initialized".into());
        }
        // Thanks to the reorder in Step 2, the next frame drains the queue
        // before signalling — so after wait_frame the layer exists.
        self.bus.wait_frame();
        Ok(())
    }
```

- [ ] **Step 4: Build and run tests**

Run: `cargo build && cargo test`
Expected: clean build; all tests pass, including the existing `wait_idle_*` tests (their `Fake` now also implements `load_osm`).

- [ ] **Step 5: Commit**

```bash
git add src/script/runner.rs src/main.rs
git commit -m "Implement load_osm in the script runner"
```

---

## Task 11: In-situ scripted test — fixture, script, docs

**Files:**
- Create: `docs/screenshots/fixtures/select.osm`
- Create: `docs/screenshots/select.osmscript`
- Modify: `README.md`

- [ ] **Step 1: Write the fixture**

Create `docs/screenshots/fixtures/select.osm`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6" generator="handwritten">
  <bounds minlat="40.7080" minlon="-74.0100" maxlat="40.7160" maxlon="-74.0000"/>

  <!-- Two geometry-only nodes forming a way -->
  <node id="1001" lat="40.7100" lon="-74.0080"/>
  <node id="1002" lat="40.7140" lon="-74.0020"/>

  <!-- A tagged POI node, off the way -->
  <node id="2001" lat="40.7120" lon="-74.0060">
    <tag k="amenity" v="cafe"/>
    <tag k="name" v="Fixture Cafe"/>
  </node>

  <!-- A tagged way between the two geometry nodes -->
  <way id="3001">
    <nd ref="1001"/>
    <nd ref="1002"/>
    <tag k="highway" v="residential"/>
    <tag k="name" v="Fixture Street"/>
  </way>
</osm>
```

- [ ] **Step 2: Write the script**

Create `docs/screenshots/select.osmscript`:

```
# Load fixture data and exercise feature selection.
#
# Coordinate model: `click X Y` uses raw window coords. MapViewer subtracts
# 48px (header) from y before hit-testing. With window 1200x800 and the
# 280px right panel, the map area is 920x752 and the viewport center
# projects to map-area (460, 376) = window (460, 424).
#
# Fixture layout at zoom 18, centered on (40.7120, -74.0060):
#   node 1001  (40.7100, -74.0080)  → window ~(240, 424)
#   node 1002  (40.7140, -74.0020)  → window ~(680, 424)
#   node 2001  (40.7120, -74.0060)  → window  (460, 424)   # tagged POI
#   way  3001  1001 → 1002          # passes through (460,424) too, so
#                                   # we click it at (570, 424) — clear of
#                                   # the POI's 8px node tolerance.

window 1200 800
# Load BEFORE setting the viewport: the first loaded dataset triggers
# fit_to_osm_data which would otherwise clobber an earlier `viewport` call.
load_osm docs/screenshots/fixtures/select.osm
viewport 40.7120 -74.0060 18
wait_idle 5s

# Click the tagged POI node.
click 460,424
wait_idle 2s
capture out/select-node.png

# Click far from any fixture feature to deselect.
click 100,700
wait_idle 2s
capture out/select-empty.png

# Click the way mid-segment (clear of POI and endpoint nodes).
click 570,424
wait_idle 2s
capture out/select-way.png
```

*If features don't land where the header comment says, re-check the actual window size GPUI opened (some platforms honor `--window-size` strictly, others don't), and adjust coordinates from the first-run screenshots.*

- [ ] **Step 3: Run the script end-to-end and inspect the captures**

Run: `cargo run -- --script docs/screenshots/select.osmscript --window-size 1200x800`
Expected: three PNGs appear under `out/`. Inspect:

- `select-node.png`: POI node wrapped in a magenta ring; right panel shows `Node #2001`, OSM link, and `amenity=cafe` / `name=Fixture Cafe`.
- `select-empty.png`: no ring or accent highlight; right panel shows the empty-state prompt.
- `select-way.png`: way drawn in a thicker magenta stroke; right panel shows `Way #3001`, OSM link, `highway=residential` / `name=Fixture Street`.

If positions are off, adjust the `click` coordinates in the script until each capture matches the description.

- [ ] **Step 4: Document `load_osm` in the README**

In `README.md`, find the paragraph under "Scripted screenshots" that starts with `Ops: window W H, viewport LAT LON ZOOM, ...`. Append `load_osm PATH` to the op list. Also add a one-liner explaining it:

```
Ops: `window W H`, `viewport LAT LON ZOOM`, `wait_idle [TIMEOUT]`, `wait DURATION`, `drag X1,Y1 X2,Y2 [duration=Nms]`, `click X,Y [button=left|right]`, `scroll X,Y [dx=N] [dy=N]`, `key CHORD` (e.g. `cmd+shift+a`), `load_osm PATH`, `capture PATH`, `log MSG`. Durations accept `Nms` or `Ns`.
```

And immediately after the paragraph about `wait_idle`, add:

```
`load_osm PATH` parses an OSM XML file and pushes it onto the dataset queue, the same pipeline used by **File > Open**. Follow it with `wait_idle` so the next frame creates the layer before subsequent clicks run.
```

- [ ] **Step 5: Commit**

```bash
git add docs/screenshots/fixtures/select.osm docs/screenshots/select.osmscript README.md
git commit -m "Add in-situ scripted test for feature selection"
```

---

## Self-review notes (completed before handoff)

- **Spec coverage:** All 5 spec sections (Architecture, Hit testing, Rendering, Right panel, Testing) are implemented. `load_osm` DSL extension from spec §Testing is Task 9–10. Fixture + script from §Testing is Task 11.
- **Types consistent:** `FeatureRef`, `FeatureKind`, `HitCandidate` are defined in Task 1 and referenced unchanged in Tasks 2–10. `set_highlight` / `feature_tags` / `hit_test` / `render_highlight` trait methods are defined in `layers/mod.rs` and implemented in `osm_layer.rs` consistently.
- **No placeholders:** Every code step shows full code. The only "adjust coordinates" note (Task 11 Step 3) is the inherent cost of scripting against pixel positions; the script is still runnable as written.
- **TDD order:** Tasks 1, 3, 9 lead with failing tests. Tasks 2, 4–8, 10, 11 are integration glue with no unit-testable surface beyond what's already covered, verified via `cargo build && cargo test` plus the in-situ script.
