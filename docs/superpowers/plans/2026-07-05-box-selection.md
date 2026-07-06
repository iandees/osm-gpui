# Box Selection, Multi-Select Panel & Tag Aggregation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add click-and-drag bounding-box selection of map features (backed by an R-tree spatial index), turn `MapViewer.selected` into a multi-feature selection, and rework the right pane into three sections — Layers, Selection (scrollable list with count), and Tags (aggregated across the selection).

**Architecture:** Selection becomes `Vec<FeatureRef>` instead of `Option<FeatureRef>`. `OsmLayer` gains two `rstar` R-trees (node points, way bounding-boxes) built alongside its existing Mercator-space caches; a box query is `locate_in_envelope`, which returns entries **fully contained** in the query envelope — exactly the "node-in-rect" and "way-fully-enclosed" rules needed, with no separate geometry predicate to write. Left-click-drag past a 4px threshold enters box-select mode (rubber-band overlay); release runs the rect query and replaces the selection. The right pane's existing `collapsible_section` helper (from the prior stacked-panel change) gains a dynamic title and hosts a new Selection-list section and a new aggregated Tags section.

**Tech Stack:** Rust, gpui (pinned zed rev), gpui-component, `rstar` (new).

## Global Constraints

- Left-click (no drag) selects a single feature, replacing the selection (unchanged behavior). Left-click-drag past 4px enters box select; release **replaces** the selection with every feature in the box. No shift/ctrl to add — out of scope.
- Right-click-drag still pans (unchanged); box select only ever comes from the left button.
- **Ways** are selected only if **fully enclosed** by the drag rectangle (all vertices inside). **Nodes** are selected if their point is inside the rectangle (all nodes, including way vertices).
- **Tag aggregation**: union of keys across the selection. For each key, consider only features that have it; exactly one distinct value → show the value; more than one → `<N values>`. Features missing a key don't count as a value for it.
- Selection-list entries are plain labels `"{Kind} {id}"` (e.g. `"Node 123"`, `"Way 345"`). Clicking an entry **replaces the selection with just that feature**.
- Selection section header shows the count: `"Selection (N items)"` (singular `"Selection (1 item)"`; plain `"Selection"` when empty).
- Selection list shows at most 10 rows before scrolling.
- The old single-feature "View on openstreetmap.org" link is dropped (not carried into the new Selection or Tags sections).
- Do not touch dead files: `src/map.rs`, `src/data.rs`, `src/background.rs`, `src/mercator.rs`.
- Single-line git commit messages, no `Co-Authored-By` trailer.
- `cargo build`, `cargo clippy`, `cargo test` must stay clean/green throughout; the existing test suite (currently 107 tests) must not regress.
- The gpui window cannot be driven or screenshotted in this environment — GUI-only behavior (rubber-band drawing, list-row clicks, live highlight) is verified by build + tests + code reasoning, with a manual spot-check list for the human reviewer. Do not fabricate screenshots or claim visual confirmation that wasn't obtained.

---

### Task 1: Tag aggregation (pure, TDD)

**Files:**
- Modify: `src/selection.rs`

**Interfaces:**
- Produces: `pub enum TagValue { Single(String), Multiple(usize) }` and
  `pub fn aggregate_tags(per_feature: &[Vec<(String, String)>]) -> Vec<(String, TagValue)>`
  (sorted by key). Task 5 calls this with per-selected-feature tag lists.

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` block at the bottom of `src/selection.rs` (after the existing tests, using the same `use super::*;` already in scope):

```rust
    #[test]
    fn aggregate_single_feature_single_value() {
        let per_feature = vec![vec![("highway".to_string(), "residential".to_string())]];
        let result = aggregate_tags(&per_feature);
        assert_eq!(
            result,
            vec![("highway".to_string(), TagValue::Single("residential".to_string()))]
        );
    }

    #[test]
    fn aggregate_multiple_distinct_values_counts_distinct_only() {
        let per_feature = vec![
            vec![("name".to_string(), "Main St".to_string())],
            vec![("name".to_string(), "Elm St".to_string())],
            vec![("name".to_string(), "Main St".to_string())], // duplicate value
        ];
        let result = aggregate_tags(&per_feature);
        assert_eq!(result, vec![("name".to_string(), TagValue::Multiple(2))]);
    }

    #[test]
    fn aggregate_missing_key_on_some_features_is_ignored() {
        let per_feature = vec![
            vec![("name".to_string(), "Main St".to_string())],
            vec![], // no tags at all on this feature
        ];
        let result = aggregate_tags(&per_feature);
        assert_eq!(
            result,
            vec![("name".to_string(), TagValue::Single("Main St".to_string()))]
        );
    }

    #[test]
    fn aggregate_union_of_keys_across_features() {
        let per_feature = vec![
            vec![("highway".to_string(), "residential".to_string())],
            vec![("surface".to_string(), "paved".to_string())],
        ];
        let result = aggregate_tags(&per_feature);
        assert_eq!(
            result,
            vec![
                ("highway".to_string(), TagValue::Single("residential".to_string())),
                ("surface".to_string(), TagValue::Single("paved".to_string())),
            ]
        );
    }

    #[test]
    fn aggregate_empty_input_returns_empty() {
        assert!(aggregate_tags(&[]).is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib aggregate_ -- --nocapture`
Expected: FAIL to compile — `aggregate_tags` and `TagValue` are not defined yet.

- [ ] **Step 3: Implement `TagValue` and `aggregate_tags`**

Add above the `#[cfg(test)]` block in `src/selection.rs` (after `resolve_hits`):

```rust
/// A key's aggregated value across a set of features: either every feature
/// that has the key agrees on one value, or they don't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagValue {
    Single(String),
    Multiple(usize),
}

/// Aggregate tags across multiple features' tag lists. Keys are the union
/// across all features. For each key, distinct values are counted only
/// among the features that *have* that key: exactly one distinct value
/// yields `Single(value)`; more than one yields `Multiple(distinct_count)`.
/// A feature missing a key does not affect that key's aggregation. Sorted
/// by key.
pub fn aggregate_tags(per_feature: &[Vec<(String, String)>]) -> Vec<(String, TagValue)> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut by_key: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for tags in per_feature {
        for (k, v) in tags {
            by_key.entry(k.clone()).or_default().insert(v.clone());
        }
    }

    by_key
        .into_iter()
        .map(|(k, values)| {
            let value = if values.len() == 1 {
                TagValue::Single(values.into_iter().next().unwrap())
            } else {
                TagValue::Multiple(values.len())
            };
            (k, value)
        })
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib aggregate_ -- --nocapture`
Expected: 5 passed, 0 failed.

- [ ] **Step 5: Run the full suite and commit**

Run: `cargo test`
Expected: all passing (existing 107 + 5 new = 112), no regressions.

```bash
git add src/selection.rs
git commit -m "Add pure tag-aggregation helper for multi-select tags panel"
```

---

### Task 2: R-tree spatial index and box hit-test (TDD)

**Files:**
- Modify: `Cargo.toml` (add `rstar`)
- Modify: `src/coordinates.rs` (add `CoordinateTransform::screen_to_mercator`)
- Modify: `src/viewport.rs` (add `Viewport::screen_to_mercator`)
- Modify: `src/layers/mod.rs` (add `MapLayer::hit_test_rect` default, `LayerManager::hit_test_rect_all`)
- Modify: `src/layers/osm_layer.rs` (node/way R-tree indexes, `hit_test_rect` impl, tests)

**Interfaces:**
- Consumes: `NodeCache.flat: Vec<(i64, f64, f64)>`, `way_bboxes: Vec<Option<WayBbox>>` (existing,
  already Mercator-space and index-aligned with `OsmData.ways`).
- Produces:
  - `Viewport::screen_to_mercator(&self, point: Point<Pixels>) -> (f64, f64)`.
  - `MapLayer::hit_test_rect(&self, viewport: &Viewport, rect: Bounds<Pixels>) -> Vec<FeatureRef>`
    (default: empty `Vec`).
  - `LayerManager::hit_test_rect_all(&self, viewport: &Viewport, rect: Bounds<Pixels>) -> Vec<FeatureRef>`.
  - Task 4 calls `LayerManager::hit_test_rect_all` on left-drag release.

- [ ] **Step 1: Add the `rstar` dependency**

In `Cargo.toml`, add to `[dependencies]` (alphabetical among the plain-version deps):

```toml
rstar = "0.13"
```

Run: `cargo build` — expect it to fetch/compile `rstar` and still succeed (nothing uses it yet).

- [ ] **Step 2: Write the failing test for `screen_to_mercator`**

In `src/coordinates.rs`, inside the existing `#[cfg(test)] mod tests` block (after `test_coordinate_conversion`):

```rust
    #[test]
    fn test_screen_to_mercator_round_trip() {
        let screen_size = size(px(800.0), px(600.0));
        let transform = CoordinateTransform::new(40.7128, -74.0060, 12.0, screen_size);

        let original_mx = 100.0;
        let original_my = -50.0;
        let screen_point = transform.mercator_to_screen(original_mx, original_my);
        let (mx, my) = transform.screen_to_mercator(screen_point);

        assert!((mx - original_mx).abs() < 0.01, "mx: {} vs {}", mx, original_mx);
        assert!((my - original_my).abs() < 0.01, "my: {} vs {}", my, original_my);
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --lib test_screen_to_mercator_round_trip`
Expected: FAIL to compile — `screen_to_mercator` is not defined yet.

- [ ] **Step 4: Implement `CoordinateTransform::screen_to_mercator`**

In `src/coordinates.rs`, add this method to `impl CoordinateTransform` right after `screen_to_geo` (which ends around line 221 by calling `mercator_to_lat_lon(merc_x, merc_y)`):

```rust
    /// Convert screen coordinates directly to Web Mercator (EPSG:3857) meters,
    /// skipping the final lat/lon conversion `screen_to_geo` does. Used by
    /// box-select hit-testing, which queries an R-tree built in Mercator space.
    pub fn screen_to_mercator(&self, point: GpuiPoint<Pixels>) -> (f64, f64) {
        let merc_x = self.mercator_center_x
            + (point.x - self.screen_size.width * 0.5).to_f64() / self.pixels_per_meter_x;
        let merc_y = self.mercator_center_y
            - (point.y - self.screen_size.height * 0.5).to_f64() / self.pixels_per_meter_y;

        let merc_x = if merc_x.is_finite() { merc_x } else { self.mercator_center_x };
        let merc_y = if merc_y.is_finite() { merc_y } else { self.mercator_center_y };

        (merc_x, merc_y)
    }
```

Then add the thin delegating wrapper to `impl Viewport` in `src/viewport.rs`, right after `screen_to_geo`:

```rust
    /// Convert screen coordinates directly to Web Mercator meters (no lat/lon
    /// round trip). Used by box-select hit-testing.
    pub fn screen_to_mercator(&self, point: Point<Pixels>) -> (f64, f64) {
        self.transform.screen_to_mercator(point)
    }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --lib test_screen_to_mercator_round_trip`
Expected: PASS.

- [ ] **Step 6: Write the failing tests for `OsmLayer::hit_test_rect`**

In `src/layers/osm_layer.rs`, inside the existing `#[cfg(test)] mod tests` block, add `Bounds` to the
existing `use gpui::{point, px, size};` line (making it `use gpui::{point, px, size, Bounds};`), then
add these tests after `hit_test_no_match_returns_empty`:

```rust
    #[test]
    fn hit_test_rect_selects_contained_nodes_and_fully_enclosed_ways() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        // n1 and n2 sit exactly at the viewport center (mercator-identical);
        // n3 is a full degree away, so its mercator position is far outside
        // any modest screen-space rect around the center.
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon, tags: empty_tags() };
        let n3 = OsmNode { id: 3, lat: center_lat + 1.0, lon: center_lon + 1.0, tags: empty_tags() };
        // way_in's bbox is the (degenerate) point at the center: fully enclosed.
        let way_in = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        // way_partial's bbox spans from the center to the far node: NOT fully
        // enclosed by a modest rect around the center.
        let way_partial = OsmWay { id: 20, nodes: vec![1, 3], tags: empty_tags() };
        let data = data_with(vec![n1, n2, n3], vec![way_in, way_partial]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data("L", data);

        // A screen-space box symmetric around the viewport's center screen
        // point (400, 300) — always brackets the center's mercator position
        // regardless of zoom level.
        let rect = Bounds {
            origin: point(px(300.0), px(200.0)),
            size: size(px(200.0), px(200.0)),
        };

        let hits = layer.hit_test_rect(&viewport, rect);
        let node_ids: Vec<i64> = hits
            .iter()
            .filter(|f| f.kind == FeatureKind::Node)
            .map(|f| f.id)
            .collect();
        let way_ids: Vec<i64> = hits
            .iter()
            .filter(|f| f.kind == FeatureKind::Way)
            .map(|f| f.id)
            .collect();

        assert!(node_ids.contains(&1) && node_ids.contains(&2), "got {:?}", node_ids);
        assert!(!node_ids.contains(&3), "far node should not be selected: {:?}", node_ids);
        assert!(way_ids.contains(&10), "fully-enclosed way should be selected: {:?}", way_ids);
        assert!(!way_ids.contains(&20), "partially-overlapping way should not be selected: {:?}", way_ids);
    }

    #[test]
    fn hit_test_rect_empty_when_no_data() {
        let layer = OsmLayer::new();
        let viewport = viewport_centered_on(40.0, -74.0);
        let rect = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(800.0), px(600.0)),
        };
        assert!(layer.hit_test_rect(&viewport, rect).is_empty());
    }
```

- [ ] **Step 7: Run the tests to verify they fail**

Run: `cargo test --lib hit_test_rect`
Expected: FAIL to compile — `hit_test_rect` is not defined yet.

- [ ] **Step 8: Add the trait default and `LayerManager::hit_test_rect_all`**

In `src/layers/mod.rs`, add to the `MapLayer` trait (right after the existing `hit_test` default,
around line 42):

```rust
    /// Return every feature inside a screen-space rectangle. Default: none.
    /// Nodes: point inside the rect. Ways: fully enclosed (all vertices
    /// inside). Implementations only return their own features.
    fn hit_test_rect(
        &self,
        _viewport: &Viewport,
        _rect: Bounds<Pixels>,
    ) -> Vec<crate::selection::FeatureRef> {
        Vec::new()
    }
```

And add to `impl LayerManager`, right after `hit_test_all` (around line 173):

```rust
    /// Run hit_test_rect against every visible layer, concatenated in draw order.
    pub fn hit_test_rect_all(
        &self,
        viewport: &Viewport,
        rect: Bounds<Pixels>,
    ) -> Vec<crate::selection::FeatureRef> {
        self.layers
            .iter()
            .filter(|layer| layer.is_visible())
            .flat_map(|layer| layer.hit_test_rect(viewport, rect))
            .collect()
    }
```

- [ ] **Step 9: Build the R-tree indexes in `OsmLayer`**

In `src/layers/osm_layer.rs`:

1. Update imports at the top: change `use crate::osm::OsmData;` to `use crate::osm::{OsmData, OsmWay};`,
   and add `use rstar::{RTree, AABB, primitives::GeomWithData};`.
2. Add two fields to the `OsmLayer` struct (after `way_vertices`):

```rust
    /// Spatial index of all nodes (mercator x/y -> node id), rebuilt whenever
    /// data changes. Used by box-select (`hit_test_rect`).
    node_index: RTree<GeomWithData<[f64; 2], i64>>,
    /// Spatial index of all way bounding boxes (mercator meters -> way id),
    /// rebuilt whenever data changes. `locate_in_envelope` on this index
    /// returns ways whose bbox is fully contained in the query rect, which is
    /// exactly the "fully enclosed" box-select rule for ways.
    way_index: RTree<GeomWithData<AABB<[f64; 2]>, i64>>,
```

3. Add two builder functions near `compute_way_tables` (after it):

```rust
/// Bulk-build a point index over every node's mercator position.
fn build_node_index(node_cache: &NodeCache) -> RTree<GeomWithData<[f64; 2], i64>> {
    let items: Vec<_> = node_cache
        .flat
        .iter()
        .map(|&(id, mx, my)| GeomWithData::new([mx, my], id))
        .collect();
    RTree::bulk_load(items)
}

/// Bulk-build a bounding-box index over every way's mercator bbox. Ways with
/// no valid bbox (no resolvable nodes) are skipped.
fn build_way_index(way_bboxes: &[Option<WayBbox>], ways: &[OsmWay]) -> RTree<GeomWithData<AABB<[f64; 2]>, i64>> {
    let items: Vec<_> = way_bboxes
        .iter()
        .zip(ways.iter())
        .filter_map(|(bbox, way)| {
            let b = bbox.as_ref()?;
            Some(GeomWithData::new(
                AABB::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
                way.id,
            ))
        })
        .collect();
    RTree::bulk_load(items)
}
```

4. Wire index construction into the three places that (re)build the caches:
   - `OsmLayer::new()`: add `node_index: RTree::new(), way_index: RTree::new(),` to the struct literal.
   - `OsmLayer::new_with_data()`: after `let layer_bbox = compute_layer_bbox(&node_cache);`, add
     `let node_index = build_node_index(&node_cache); let way_index = build_way_index(&way_bboxes, &osm_data.ways);`
     and add `node_index, way_index,` to the struct literal.
   - `OsmLayer::set_osm_data()`: after `self.layer_bbox = compute_layer_bbox(&self.node_cache);`, add
     `self.node_index = build_node_index(&self.node_cache); self.way_index = build_way_index(&self.way_bboxes, &osm_data.ways);`
     (note: `osm_data` here is the parameter, matching the existing method signature — set it before
     the final `self.osm_data = Some(osm_data);` line so the borrow is available).
   - `OsmLayer::clear_osm_data()`: add `self.node_index = RTree::new(); self.way_index = RTree::new();`.

- [ ] **Step 10: Implement `hit_test_rect`**

In `src/layers/osm_layer.rs`, add this method to `impl MapLayer for OsmLayer`, right after `hit_test`
(after its closing brace, before `feature_tags`):

```rust
    fn hit_test_rect(&self, viewport: &Viewport, rect: Bounds<Pixels>) -> Vec<FeatureRef> {
        let (x1, y1) = viewport.screen_to_mercator(rect.origin);
        let (x2, y2) = viewport.screen_to_mercator(point(
            rect.origin.x + rect.size.width,
            rect.origin.y + rect.size.height,
        ));
        let min_x = x1.min(x2);
        let max_x = x1.max(x2);
        let min_y = y1.min(y2);
        let max_y = y1.max(y2);
        let envelope = AABB::from_corners([min_x, min_y], [max_x, max_y]);

        let mut out = Vec::new();
        for item in self.node_index.locate_in_envelope(&envelope) {
            out.push(FeatureRef {
                layer_name: self.name.clone(),
                kind: FeatureKind::Node,
                id: item.data,
            });
        }
        for item in self.way_index.locate_in_envelope(&envelope) {
            out.push(FeatureRef {
                layer_name: self.name.clone(),
                kind: FeatureKind::Way,
                id: item.data,
            });
        }
        out
    }
```

(`point` is already in scope via the file's `use gpui::*;`.)

- [ ] **Step 11: Run the tests to verify they pass**

Run: `cargo test --lib hit_test_rect`
Expected: 2 passed, 0 failed.

- [ ] **Step 12: Run the full suite and commit**

Run: `cargo test`
Expected: all passing, no regressions.
Run: `cargo build && cargo clippy` — expect clean (fix any clippy findings in the new code before committing).

```bash
git add Cargo.toml Cargo.lock src/coordinates.rs src/viewport.rs src/layers/mod.rs src/layers/osm_layer.rs
git commit -m "Add R-tree spatial index and box hit-test to OsmLayer"
```

---

### Task 3: Multi-select model

Switches `MapViewer.selected` from `Option<FeatureRef>` to `Vec<FeatureRef>`, and `MapLayer::set_highlight`
from a single optional feature to a slice. This is the data-model task; box-select interaction (Task 4)
and the panel rework (Task 5) build on it. After this task, box-select isn't wired up yet — only the
model and single-click path change — so behavior is unchanged from the user's perspective except that
`main.rs` compiles against `Vec` throughout.

**Files:**
- Modify: `src/layers/mod.rs` (`MapLayer::set_highlight` signature)
- Modify: `src/layers/osm_layer.rs` (`set_highlight` impl, `highlight` field type)
- Modify: `src/main.rs` (`selected` field type, `handle_map_click`, `sync_selection_to_layers`, canvas
  highlight loop)

**Interfaces:**
- Consumes: `osm_gpui::selection::resolve_hits(Vec<Vec<HitCandidate>>) -> Option<FeatureRef>` (unchanged —
  still used for single-click; its `Option` result is converted to a 0-or-1-element `Vec` at the call site).
- Produces: `MapViewer.selected: Vec<FeatureRef>`; `MapLayer::set_highlight(&mut self, features: &[FeatureRef])`.
  Task 4 pushes new elements onto `selected` via box-select; Task 5's panel reads `selected` to render the
  list, count, and aggregated tags.

- [ ] **Step 1: Change the `set_highlight` trait signature**

In `src/layers/mod.rs`, replace:

```rust
    /// Tell the layer which feature (if any) is currently selected.
    /// Default: no-op. OsmLayer overrides this to drive `render_elements`.
    fn set_highlight(&mut self, _feature: Option<crate::selection::FeatureRef>) {}
```

with:

```rust
    /// Tell the layer which features are currently selected.
    /// Default: no-op. OsmLayer overrides this to store the set.
    fn set_highlight(&mut self, _features: &[crate::selection::FeatureRef]) {}
```

- [ ] **Step 2: Update `OsmLayer`'s `highlight` field and `set_highlight` impl**

In `src/layers/osm_layer.rs`:
1. Change the field (near `way_vertices`/`layer_bbox`):
   `highlight: Option<FeatureRef>,` → `highlight: Vec<FeatureRef>,`
2. Update both struct literals (`new()` and `new_with_data()`): `highlight: None,` → `highlight: Vec::new(),`
3. Update the impl:

```rust
    fn set_highlight(&mut self, features: &[FeatureRef]) {
        self.highlight = features.to_vec();
    }
```

(`self.highlight` remains write-only, matching its existing behavior before this change — nothing reads
it today; this task only changes its type to keep it representing "the current selection" without adding
unused lookup machinery.)

- [ ] **Step 3: Build to confirm the trait change compiles**

Run: `cargo build`
Expected: fails in `src/main.rs` (still uses `Option<FeatureRef>`) — proceed to fix it below.

- [ ] **Step 4: Switch `MapViewer.selected` to `Vec<FeatureRef>`**

In `src/main.rs`:

1. Field declaration (around line 252): `selected: Option<osm_gpui::selection::FeatureRef>,` →
   `selected: Vec<osm_gpui::selection::FeatureRef>,`
2. Constructor (around line 284): `selected: None,` → `selected: Vec::new(),`

- [ ] **Step 5: Update `handle_map_click`**

Replace:

```rust
    fn handle_map_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>) {
        let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
        self.selected = osm_gpui::selection::resolve_hits(per_layer);
    }
```

with:

```rust
    fn handle_map_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>) {
        let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
        self.selected = osm_gpui::selection::resolve_hits(per_layer)
            .into_iter()
            .collect();
    }
```

- [ ] **Step 6: Update `sync_selection_to_layers`**

Replace the whole method:

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

with:

```rust
    fn sync_selection_to_layers(&mut self) {
        // Drop any selected feature whose owning layer is gone or hidden, so
        // the right panel never shows info for a feature not drawn on the map.
        let layer_manager = &self.layer_manager;
        self.selected.retain(|sel| {
            layer_manager
                .find_layer(&sel.layer_name)
                .map(|l| l.is_visible())
                .unwrap_or(false)
        });

        let selected = self.selected.clone();
        for layer in self.layer_manager.layers_mut() {
            let matching: Vec<osm_gpui::selection::FeatureRef> = selected
                .iter()
                .filter(|s| s.layer_name == layer.name())
                .cloned()
                .collect();
            layer.set_highlight(&matching);
        }
    }
```

- [ ] **Step 7: Update the canvas highlight loop**

Around line 1167-1173, replace:

```rust
                                                let selected = self.selected.clone();
                                                move |bounds, _, window, _| {
                                                    let layer_manager = unsafe { &*layer_manager };
                                                    layer_manager.render_all_canvas(&viewport_clone, bounds, window);
                                                    if let Some(sel) = &selected {
                                                        layer_manager.render_highlight(sel, &viewport_clone, bounds, window);
                                                    }
                                                }
```

with:

```rust
                                                let selected = self.selected.clone();
                                                move |bounds, _, window, _| {
                                                    let layer_manager = unsafe { &*layer_manager };
                                                    layer_manager.render_all_canvas(&viewport_clone, bounds, window);
                                                    for sel in &selected {
                                                        layer_manager.render_highlight(sel, &viewport_clone, bounds, window);
                                                    }
                                                }
```

- [ ] **Step 8: Build**

Run: `cargo build`
Expected: still fails — `render_tags_section` (line ~1000) does `let Some(sel) = self.selected.clone() else`,
which no longer type-checks against `Vec`. **Leave this alone for now** — Task 5 replaces the whole
function. To get a clean intermediate build for this task's own verification, temporarily change that one
line to `let Some(sel) = self.selected.first().cloned() else` (keeps today's single-feature tag display
working, using the first selected feature, until Task 5 replaces the function outright).

- [ ] **Step 9: Build and test**

Run: `cargo build`
Expected: succeeds.
Run: `cargo test`
Expected: all passing (112 from Tasks 1-2), no regressions.
Run: `cargo run`, click a single feature — confirm it still highlights and its tags still show (via the
temporary `.first()` shim). This preserves today's behavior end-to-end before Task 4 adds box-select and
Task 5 reworks the panel.

- [ ] **Step 10: Commit**

```bash
git add src/layers/mod.rs src/layers/osm_layer.rs src/main.rs
git commit -m "Switch selection model from single Option to multi-select Vec"
```

---

### Task 4: Box-select interaction

Adds the rubber-band drag gesture on the left button and wires it to `hit_test_rect_all`.

**Files:**
- Modify: `src/main.rs` (`box_select` field, `handle_mouse_move`/`handle_mouse_up`, `normalize_rect`
  helper, rubber-band overlay render)

**Interfaces:**
- Consumes: `LayerManager::hit_test_rect_all` (Task 2), `MapViewer.selected: Vec<FeatureRef>` (Task 3).
- Produces: `MapViewer.box_select: Option<(Point<Pixels>, Point<Pixels>)>` — Task 5's panel doesn't need
  this directly, but the overlay render added here is the last piece of the map-area interaction.

- [ ] **Step 1: Add the `box_select` field**

In the `MapViewer` struct (near `mouse_down_pos`, around line 253):

```rust
    /// Screen-space (start, current) points of an in-progress left-drag box
    /// select, or `None` when not dragging a box.
    box_select: Option<(gpui::Point<gpui::Pixels>, gpui::Point<gpui::Pixels>)>,
```

In the constructor (near `mouse_down_pos: None,`, around line 285): `box_select: None,`

- [ ] **Step 2: Add the `normalize_rect` helper**

Add as a free function near the top of `src/main.rs` (after the existing helper functions, or directly
above `impl MapViewer`):

```rust
/// Normalize two arbitrary screen points into a `Bounds` with a top-left
/// origin and non-negative size, regardless of drag direction.
fn normalize_rect(
    a: gpui::Point<gpui::Pixels>,
    b: gpui::Point<gpui::Pixels>,
) -> gpui::Bounds<gpui::Pixels> {
    let min_x = a.x.as_f32().min(b.x.as_f32());
    let max_x = a.x.as_f32().max(b.x.as_f32());
    let min_y = a.y.as_f32().min(b.y.as_f32());
    let max_y = a.y.as_f32().max(b.y.as_f32());
    gpui::Bounds {
        origin: gpui::point(px(min_x), px(min_y)),
        size: gpui::size(px(max_x - min_x), px(max_y - min_y)),
    }
}
```

- [ ] **Step 3: Update `handle_mouse_move` to track the drag**

Replace:

```rust
    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let adjusted_position = event.position;

        if self.viewport.handle_mouse_move(adjusted_position) {
            cx.notify();
        }
    }
```

with:

```rust
    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let adjusted_position = event.position;

        if self.viewport.handle_mouse_move(adjusted_position) {
            cx.notify();
        }

        if event.pressed_button == Some(gpui::MouseButton::Left) {
            if let Some(start) = self.mouse_down_pos {
                let moved = (adjusted_position - start).magnitude() >= 4.0;
                if moved || self.box_select.is_some() {
                    self.box_select = Some((start, adjusted_position));
                    cx.notify();
                }
            }
        }
    }
```

- [ ] **Step 4: Update `handle_mouse_up` to resolve the box on release**

Replace:

```rust
    fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let up_pos = event.position;
        let was_click = match self.mouse_down_pos.take() {
            Some(down) => {
                (up_pos - down).magnitude() < 4.0
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
```

with:

```rust
    fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let up_pos = event.position;
        let down_pos = self.mouse_down_pos.take();
        self.viewport.handle_mouse_up();

        if let Some((start, _)) = self.box_select.take() {
            let rect = normalize_rect(start, up_pos);
            let before = self.selected.clone();
            self.selected = self.layer_manager.hit_test_rect_all(&self.viewport, rect);
            if before != self.selected {
                cx.notify();
            }
            return;
        }

        let was_click = match down_pos {
            Some(down) => (up_pos - down).magnitude() < 4.0,
            None => false,
        };
        if was_click {
            let before = self.selected.clone();
            self.handle_map_click(up_pos);
            if before != self.selected {
                cx.notify();
            }
        }
    }
```

- [ ] **Step 5: Render the rubber-band overlay**

In the map-area `div()`'s child list (around line 1181-1224, alongside the debug-info and status-message
overlay `.child({ ... })` blocks), add one more `.child({ ... })` following the same conditional-overlay
idiom already used there:

```rust
            .child({
                if let Some((start, current)) = self.box_select {
                    let rect = normalize_rect(start, current);
                    div()
                        .absolute()
                        .left(rect.origin.x)
                        .top(rect.origin.y)
                        .w(rect.size.width)
                        .h(rect.size.height)
                        .bg(cx.theme().accent)
                        .border_1()
                        .border_color(cx.theme().accent)
                        .opacity(0.35)
                        .into_any_element()
                } else {
                    div().into_any_element()
                }
            })
```

- [ ] **Step 6: Build, test, and run**

Run: `cargo build`
Expected: succeeds.
Run: `cargo test`
Expected: all passing (112), no regressions.
Run: `cargo run`. The gpui window can't be driven in this environment, so confirm launch-without-panic and
reason from the code (mouse handlers compile and match the click/drag threshold used elsewhere; the
overlay follows the exact idiom of the existing debug/status overlays). Do not fabricate a screenshot —
note this as a manual spot-check item: **left-drag on the map draws a translucent rectangle and releasing
it selects everything inside** (nodes in-rect, ways fully enclosed).

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "Add click-and-drag box selection with rubber-band overlay"
```

---

### Task 5: Right pane rework — Selection list and Tags section

Reworks the panel: `collapsible_section`'s title becomes dynamic, the old single-feature tag display is
split into a Selection-list section and a new aggregated Tags section, and `side_panel_open` defaults to
all three open.

**Files:**
- Modify: `src/main.rs` (`collapsible_section` signature, `render_side_panel`, new
  `render_selection_section`, replaced `render_tags_section`, `side_panel_open` default)

**Interfaces:**
- Consumes: `MapViewer.selected: Vec<FeatureRef>` (Task 3), `osm_gpui::selection::aggregate_tags` (Task 1),
  `MapLayer::feature_tags` (unchanged), `MapLayer::find_layer` (unchanged).
- Produces: the three-section panel described in the design spec. Terminal task — nothing downstream
  depends on this.

- [ ] **Step 1: Make `collapsible_section`'s title dynamic**

Change the signature (around line 897) from:

```rust
    fn collapsible_section(
        &self,
        title: &'static str,
        index: usize,
        open: bool,
        content: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
```

to:

```rust
    fn collapsible_section(
        &self,
        title: impl Into<gpui::SharedString>,
        index: usize,
        open: bool,
        content: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
```

The body is unchanged (`Label::new(title)` already accepts `impl Into<SharedString>`).

- [ ] **Step 2: Default all three sections open**

In the constructor (around line 291): `side_panel_open: vec![0, 1],` → `side_panel_open: vec![0, 1, 2],`

- [ ] **Step 3: Add the row-cap constants**

Near the top of `impl MapViewer` or just above `render_side_panel`, add:

```rust
    const SELECTION_ROW_HEIGHT: f32 = 22.0;
    const SELECTION_MAX_VISIBLE_ROWS: usize = 10;
```

(As associated constants on `impl MapViewer`; if the surrounding code style prefers free `const`s instead,
place them as plain top-of-file `const SELECTION_ROW_HEIGHT_PX: f32 = 22.0;` /
`const SELECTION_MAX_VISIBLE_ROWS: usize = 10;` and reference them unqualified — either is fine, pick
whichever reads more naturally at the edit site.)

- [ ] **Step 4: Add `render_selection_section`**

Add this new method to `impl MapViewer`, right before `render_layers_section`:

```rust
    /// The Selection accordion section: a scrollable list of the selected
    /// features (max ~10 rows visible, then scrolls). Clicking a row narrows
    /// the selection to just that feature.
    fn render_selection_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use osm_gpui::selection::FeatureKind;

        if self.selected.is_empty() {
            return Label::new("Click or drag to select.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let visible_rows = self.selected.len().min(Self::SELECTION_MAX_VISIBLE_ROWS);
        let list_height = px(visible_rows as f32 * Self::SELECTION_ROW_HEIGHT);

        div()
            .id("selection-list")
            .flex()
            .flex_col()
            .h(list_height)
            .overflow_y_scroll()
            .children(self.selected.iter().enumerate().map(|(i, feat)| {
                let kind_label = match feat.kind {
                    FeatureKind::Node => "Node",
                    FeatureKind::Way => "Way",
                };
                let row_feat = feat.clone();
                div()
                    .id(("selection-row", i))
                    .flex_shrink_0()
                    .h(px(Self::SELECTION_ROW_HEIGHT))
                    .px_1()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|this| this.bg(cx.theme().accent))
                    .child(format!("{} {}", kind_label, feat.id))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                            this.selected = vec![row_feat.clone()];
                            cx.notify();
                        }),
                    )
            }))
            .into_any_element()
    }
```

- [ ] **Step 5: Replace `render_tags_section` with the aggregated version**

Replace the entire existing method (from `fn render_tags_section` through its closing `}`, currently
around lines 997-1059) with:

```rust
    /// The Tags accordion section: tags aggregated across every selected
    /// feature. A key with one distinct value (among features that have it)
    /// shows that value; a key with several shows "<N values>".
    fn render_tags_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.selected.is_empty() {
            return Label::new("No selection.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let per_feature: Vec<Vec<(String, String)>> = self
            .selected
            .iter()
            .filter_map(|sel| {
                self.layer_manager
                    .find_layer(&sel.layer_name)
                    .and_then(|layer| layer.feature_tags(sel))
            })
            .collect();

        let aggregated = osm_gpui::selection::aggregate_tags(&per_feature);

        if aggregated.is_empty() {
            return DescriptionList::new()
                .child(DescriptionItem::new("").value(Label::new("(no tags)").into_any_element()))
                .into_any_element();
        }

        DescriptionList::new()
            .columns(1)
            .bordered(true)
            .children(aggregated.into_iter().map(|(k, v)| {
                let value = match v {
                    osm_gpui::selection::TagValue::Single(s) => s,
                    osm_gpui::selection::TagValue::Multiple(n) => format!("<{} values>", n),
                };
                DescriptionItem::new(k).value(Label::new(value).into_any_element())
            }))
            .into_any_element()
    }
```

- [ ] **Step 6: Wire the three sections into `render_side_panel`**

Replace the whole method:

```rust
    /// The right pane: Layers and Selection sections stacked top-to-bottom,
    /// each collapsible and sized to its content (the whole pane scrolls).
    fn render_side_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let layer_info: Vec<(String, bool)> = self
            .layer_manager
            .layers()
            .iter()
            .map(|layer| (layer.name().to_string(), layer.is_visible()))
            .collect();

        let layers_section = self.render_layers_section(&layer_info, cx);
        let tags_section = self.render_tags_section(cx);
        let open_layers = self.side_panel_open.contains(&0);
        let open_selection = self.side_panel_open.contains(&1);

        div()
            .w(px(280.0))
            .h_full()
            .bg(cx.theme().sidebar)
            .border_l_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .id("side-panel-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .child(self.collapsible_section("Layers", 0, open_layers, layers_section, cx))
                    .child(self.collapsible_section(
                        "Selection",
                        1,
                        open_selection,
                        tags_section,
                        cx,
                    )),
            )
    }
```

with:

```rust
    /// The right pane: Layers, Selection, and Tags sections stacked
    /// top-to-bottom, each collapsible and sized to its content (the whole
    /// pane scrolls).
    fn render_side_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let layer_info: Vec<(String, bool)> = self
            .layer_manager
            .layers()
            .iter()
            .map(|layer| (layer.name().to_string(), layer.is_visible()))
            .collect();

        let layers_section = self.render_layers_section(&layer_info, cx);
        let selection_section = self.render_selection_section(cx);
        let tags_section = self.render_tags_section(cx);

        let open_layers = self.side_panel_open.contains(&0);
        let open_selection = self.side_panel_open.contains(&1);
        let open_tags = self.side_panel_open.contains(&2);

        let selection_title = match self.selected.len() {
            0 => "Selection".to_string(),
            1 => "Selection (1 item)".to_string(),
            n => format!("Selection ({} items)", n),
        };

        div()
            .w(px(280.0))
            .h_full()
            .bg(cx.theme().sidebar)
            .border_l_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .id("side-panel-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .child(self.collapsible_section("Layers", 0, open_layers, layers_section, cx))
                    .child(self.collapsible_section(
                        selection_title,
                        1,
                        open_selection,
                        selection_section,
                        cx,
                    ))
                    .child(self.collapsible_section("Tags", 2, open_tags, tags_section, cx)),
            )
    }
```

- [ ] **Step 7: Build, test, and run**

Run: `cargo build`
Expected: succeeds — this also removes the Task 3 Step 8 temporary shim, since the whole function was
replaced.
Run: `cargo clippy`
Expected: clean.
Run: `cargo test`
Expected: all passing (112), no regressions.
Run: `cargo run`. The gpui window can't be driven in this environment, so confirm launch-without-panic and
reason from the code. Note these as manual spot-check items for the human reviewer:
- Selecting features (click or box-drag) shows `"Selection (N items)"` in the header, a scrollable list
  capped at 10 visible rows, each labeled `"Node {id}"` / `"Way {id}"`.
- Clicking a row narrows the selection to that one feature (list and Tags both update, map highlight
  updates to just that feature).
- The Tags section shows single values directly and `<N values>` for keys that differ across the
  selection; keys missing on some features don't spuriously show as multi-valued.
- All three sections (Layers, Selection, Tags) collapse and expand independently.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs
git commit -m "Rework right pane: selection list with count, aggregated tags section"
```

---

## Self-Review

**Spec coverage:**
- Box select (left-drag, replace selection, right-drag still pans) → Task 4. ✅
- R-tree-backed hit-test; nodes-in-rect, ways-fully-enclosed → Task 2 (verified against `rstar`'s
  documented `locate_in_envelope` containment semantics, plus a deterministic unit test distinguishing a
  fully-enclosed way from a partially-overlapping one). ✅
- Selection section: scrollable, max 10 visible, count in header, `"Node 123"`/`"Way 345"` labels,
  click-to-narrow → Task 5. ✅
- Third Tags section, aggregation rule (union of keys, single vs `<N values>`, missing-key ignored) →
  Task 1 (pure logic + tests) + Task 5 (wiring). ✅
- Old per-feature OSM link dropped → Task 5 Step 5 (the new `render_tags_section` has no link/header,
  matching the spec's explicit instruction). ✅
- Multi-select model, `set_highlight` generalized, canvas multi-highlight loop → Task 3. ✅

**Placeholder scan:** No "TBD"/"handle appropriately". Every step has complete code. The one place a
judgment call is offered instead of a single fixed answer is Task 5 Step 3 (associated vs. free
`const`s) — an explicit, harmless style choice with both options fully specified, not a gap.

**Type consistency:** `FeatureRef`, `FeatureKind`, `HitCandidate`, `TagValue` used consistently with their
definitions across tasks. `MapLayer::set_highlight(&[FeatureRef])`, `hit_test_rect(&Viewport,
Bounds<Pixels>) -> Vec<FeatureRef>`, `LayerManager::hit_test_rect_all(...) -> Vec<FeatureRef>`,
`Viewport::screen_to_mercator(Point<Pixels>) -> (f64, f64)`, `aggregate_tags(&[Vec<(String,String)>]) ->
Vec<(String, TagValue)>` are each defined once and referenced identically by every later task that calls
them.

**Note on TDD scope:** Tasks 1 and 2 contain genuine pure logic (tag aggregation; the fully-enclosed/
in-rect box rule) and get full RED/GREEN TDD treatment with deterministic unit tests. Tasks 3-5 are
data-model plumbing and GPUI rendering/interaction wiring with no new pure logic of their own — consistent
with this project's established precedent (see the prior gpui-component migration plan), their
verification is `cargo build` + `cargo clippy` + the existing test suite + code-reasoning, with explicit
manual spot-check items called out for the human reviewer wherever the gpui window itself would need to
be driven to fully confirm behavior.
