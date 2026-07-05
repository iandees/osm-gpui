# Box Selection, Multi-Select Panel & Tag Aggregation

**Date:** 2026-07-05
**Status:** Approved (design)

## Summary

Add click-and-drag bounding-box selection of map features, backed by an R-tree
spatial index, and rework the right pane to show the multi-feature selection:

1. **Box select** — left-click-drag draws a rubber-band rectangle; on release the
   selection becomes every feature inside it. Left-click (no drag) still selects a
   single feature. Right-drag still pans.
2. **Selection section** — a scrollable list of the selected features
   (`Node 123`, `Way 345`, …), header shows the count (`Selection (100 items)`),
   ~10 rows visible then scrolls. Clicking a row narrows the selection to just
   that feature.
3. **Tags section** — a new 3rd collapsible section aggregating tags across the
   selection: single value shown directly; multiple distinct values shown as
   `<N values>`.

## Background & current state

- Selection is single: `MapViewer.selected: Option<FeatureRef>`, where
  `FeatureRef { layer_name, kind: Node|Way, id }` (`src/selection.rs`).
- Hit-testing is point-based: `MapLayer::hit_test(viewport, screen_pt)` →
  `Vec<HitCandidate>`; `LayerManager::hit_test_all` + `selection::resolve_hits`
  pick the nearest across visible layers.
- Highlight: `MapLayer::set_highlight(Option<FeatureRef>)` +
  `render_highlight(feature)`; the map canvas draws one highlight.
- The right pane (from the prior PR) is a hand-built collapsible stack via
  `MapViewer::collapsible_section`; `side_panel_open: Vec<usize>` tracks which
  sections are expanded (currently sections 0=Layers, 1=Selection).
- `OsmLayer` already keeps Mercator-space data aligned by index: `node_cache.flat`
  (`Vec<(id, mx, my)>`) and `way_bboxes` (`Vec<Option<WayBbox>>` in Mercator
  meters). All nodes are rendered (including way vertices).
- Viewport has `mercator_to_screen`, `geo_to_screen`, `screen_to_geo`;
  `coordinates::lat_lon_to_mercator` gives Mercator meters.

## Decisions (from brainstorming)

- Left-click = single select (replace). Left-drag = box select (**replace** the
  selection; no shift-to-add). Right-drag = pan. Box select spans all visible OSM
  layers.
- **Ways**: selected only if **fully enclosed** by the rect (all vertices inside).
  **Nodes**: selected if the node's point is inside the rect (all nodes, including
  way vertices — consistent with rendering).
- **Tag aggregation**: union of keys across the selection. For each key, consider
  only features that *have* it; 1 distinct value → show the value; >1 → `<N values>`.
  Features missing the key don't count as a value.
- Selection-list entries are plain labels; **clicking an entry replaces the
  selection with just that feature**. The old single-feature
  "View on openstreetmap.org" link is dropped.

## Architecture

### 1. Multi-select model
`MapViewer.selected: Option<FeatureRef>` → `selected: Vec<FeatureRef>` (ordered,
deduped; empty = nothing selected). Single-click produces a 0- or 1-element vec via
the existing point hit-test + `resolve_hits`.

`sync_selection_to_layers` retains only refs whose owning layer is present and
visible, then pushes the full set to each layer's `set_highlight`.

### 2. R-tree spatial index (`OsmLayer`)
Add the `rstar` crate. `OsmLayer` builds two indexes whenever data changes
(in the same path that computes `way_bboxes` / `node_cache`):

- **Node index**: `RTree<GeomWithData<[f64; 2], i64>>` — one entry per node at its
  Mercator `(mx, my)`, data = node id.
- **Way index**: `RTree<GeomWithData<Rectangle<[f64; 2]>, i64>>` — one entry per
  way whose bbox exists, envelope = the way's Mercator bbox, data = way id.

Both are (re)built on `set_data`/rebuild and cleared on `clear`.

### 3. Box hit-test
- New `Viewport::screen_to_mercator(Point<Pixels>) -> (f64, f64)` composing
  `screen_to_geo` → `lat_lon_to_mercator`.
- New trait method `MapLayer::hit_test_rect(&self, viewport, screen_rect: Bounds<Pixels>) -> Vec<FeatureRef>`
  (default empty). `OsmLayer` implements it:
  1. Convert the two screen-rect corners to Mercator, normalize into an
     `rstar::AABB::from_corners` envelope (screen↔Mercator is axis-aligned affine,
     so a screen AABB maps to a Mercator AABB; note screen-y is flipped vs
     Mercator-y, hence normalize min/max).
  2. **Nodes**: `node_index.locate_in_envelope(&env)` → node ids in rect.
  3. **Ways**: `way_index.locate_in_envelope(&env)` → ways whose bbox is *contained*
     in the rect. Because an AABB is contained in the rect iff all its corners are,
     and a way's bbox contains all its vertices, this is exactly "fully enclosed."
  4. Map ids to `FeatureRef { layer_name, kind, id }`.
- `LayerManager::hit_test_rect_all(viewport, screen_rect) -> Vec<FeatureRef>`
  concatenates results across visible layers (draw order).

### 4. Interaction (main.rs)
- Left-down: record `mouse_down_pos` and set `left_drag_start: Option<Point<Pixels>>`.
- Mouse-move (left pressed, moved > 4px from start): set/update
  `box_select: Option<(Point<Pixels>, Point<Pixels>)>` (start, current) and notify;
  this drives the rubber-band overlay.
- Left-up:
  - If `box_select` is set → `selected = hit_test_rect_all(normalized_rect)`; clear
    `box_select`.
  - Else (movement < 4px) → existing single-click select path (nearest hit or clear).
  - Clear `left_drag_start`.
- Right-drag panning is unchanged.
- **Rubber-band overlay**: while `box_select` is set, render an absolutely
  positioned `div` at the normalized rect with a translucent fill + 1px border
  (theme accent), inside the map area.

### 5. Multi-highlight
`set_highlight(&[FeatureRef])` replaces `set_highlight(Option<FeatureRef>)`;
`OsmLayer` stores the highlighted refs (e.g. `HashSet<(FeatureKind,i64)>` for O(1)
lookup). The map canvas highlight pass loops over `self.selected` and calls the
existing per-feature `render_highlight`. (Many outlines for large selections —
acceptable; noted under Costs.)

### 6. Panel: Selection + Tags sections
`side_panel_open` default becomes `[0, 1, 2]` (Layers, Selection, Tags).

- **Selection** (`render_selection_section`): header title
  `"Selection (N items)"` (or `"Selection"` when empty; `"1 item"` singular). Body:
  a scrollable container with a max height (~10 rows) and `overflow_y_scroll`,
  one clickable row per selected `FeatureRef` labeled `"{Kind} {id}"`. Clicking a
  row sets `selected = vec![that ref]` and notifies. Empty selection → muted
  "Click or drag to select." The section header count is separate from the generic
  collapsible header, so `collapsible_section` gains a way to pass a dynamic title
  string (already `&'static str`; change to `impl Into<SharedString>` or `String`).
- **Tags** (`render_tags_section`, now section 2): aggregated tags as a
  `DescriptionList`. Empty selection → muted "No selection." Otherwise one row per
  key (union across selection), value = the single value or `"<N values>"`.

### 7. Tag aggregation (pure, tested)
In `src/selection.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagValue { Single(String), Multiple(usize) }

/// Aggregate tags across the selected features' tag lists.
/// Keys are the union across all features. For each key, distinct values are
/// counted only among features that have the key: exactly one distinct value
/// -> Single(value); more than one -> Multiple(distinct_count). Sorted by key.
pub fn aggregate_tags(per_feature: &[Vec<(String, String)>]) -> Vec<(String, TagValue)>;
```

`MapViewer` gathers `Vec<Vec<(String,String)>>` by calling the owning layer's
`feature_tags` for each selected ref, then calls `aggregate_tags` to render the
Tags section.

## Files touched

- `Cargo.toml` — add `rstar`.
- `src/selection.rs` — `point_in_rect` (+ helper for rect normalization),
  `TagValue`, `aggregate_tags`; unit tests.
- `src/viewport.rs` — `screen_to_mercator`.
- `src/layers/mod.rs` — trait `hit_test_rect` + `set_highlight(&[FeatureRef])`;
  `LayerManager::hit_test_rect_all`; `render_highlight` loop over the set.
- `src/layers/osm_layer.rs` — build/query the two R-trees; `hit_test_rect`;
  multi-highlight storage.
- `src/main.rs` — `selected: Vec`, box-select state + handlers, rubber-band
  overlay, `render_selection_section` + reworked `render_tags_section`, list-row
  click, `collapsible_section` dynamic title, `side_panel_open` default `[0,1,2]`.

## Testing

- **Unit** (pure, in `selection.rs`): `point_in_rect` (inside/edge/outside);
  `aggregate_tags` (single value, multiple → correct count, missing-key ignored,
  union of keys, sorted). The "fully enclosed" way rule is realized by
  `locate_in_envelope` semantics; a small unit test on an rstar way-index confirms
  a partially-overlapping way is excluded and a fully-contained one is included.
- **Build/behavioral**: `cargo build`, `cargo clippy`, `cargo test` green. The gpui
  window can't be driven in this environment, so rubber-band drawing, list-row
  clicks, and multi-highlight are verified by build + code reasoning, with a manual
  spot-check list in the PR.

## Costs / notes

- Box query is O(log n + k) via the R-tree. Building the trees adds a bulk-load
  pass on data load (cheap; `rstar` bulk_load is efficient).
- The Tags panel calls `feature_tags` per selected feature; `OsmLayer::feature_tags`
  finds ways via linear `ways.iter().find`, so a large way selection is
  O(selection × ways). Fine for typical downloads; a follow-up could index ways by
  id if it bites.

## Out of scope

- Shift/ctrl to add or subtract from a selection.
- Per-entry OSM links / editing of features.
- Selecting features from raster/tile layers (OSM vector layers only).
