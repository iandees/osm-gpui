# Drag-to-move selected OSM features

## Goal

When one or more features are selected, click-dragging on one of the
*currently selected* features moves those features. This is the first slice
of OSM data editing.

## Scope

- Dragging translates node positions. A selected `Node` moves just that
  node. A selected `Way` moves all of its member nodes together (so the
  way's shape translates as a whole). A mixed selection moves the union of
  affected node ids.
- Drag must start on a currently-selected feature (hit-tested against the
  selection only, same tolerances as normal hit-testing). Dragging elsewhere
  keeps today's behavior (box-select, or nothing).
- While dragging, node/way positions are rendered via a transient per-layer
  preview offset — `OsmData` is not mutated until mouse-up. This keeps
  large-dataset dragging smooth (no per-frame index rebuild).
- On mouse-up past the existing 4px click/drag threshold, the move commits:
  affected nodes' lat/lon are updated in a cloned `OsmData`, the owning
  layer's caches are rebuilt once (existing `set_osm_data` path), and the
  layer is marked modified.
- If the mouse never moved past the threshold, mouse-up is treated as an
  ordinary click (today's `handle_map_click`), not a (no-op) commit.
- Escape while dragging cancels: clears the transient preview, no mutation.
- A layer that has had a committed move sets `modified: bool`, shown as a
  small indicator in the Layers panel. No save/upload path exists yet (out
  of scope) — this is just a visible signal for future work.

## Out of scope

- Undo/redo beyond in-flight Escape-cancel.
- Persisting/exporting edited data (no `.osm` writer, no changeset upload).
- Snapping dragged nodes onto other nodes/ways.
- Multi-layer drags spanning more than one `OsmLayer` at once (selection is
  grouped by layer already; each layer's affected nodes are moved
  independently using the same screen delta).

## Data model changes

### `MapLayer` trait (`src/layers/mod.rs`)

New methods, default no-ops so `TileLayer`/`GridLayer` are unaffected:

```rust
fn set_drag_preview(&mut self, _node_ids: &std::collections::HashSet<i64>, _delta: gpui::Point<gpui::Pixels>) {}
fn clear_drag_preview(&mut self) {}
fn is_modified(&self) -> bool { false }
```

### `OsmLayer` (`src/layers/osm_layer.rs`)

- New fields: `drag_preview: Option<(HashSet<i64>, Point<Pixels>)>`,
  `modified: bool`.
- `way_vertices` changes from `Vec<Vec<(f64, f64)>>` to
  `Vec<Vec<(i64, f64, f64)>>` (node id + mercator x/y) so the render loop can
  match vertices against the drag-preview node-id set. `compute_way_tables`
  and every consumer (`render_canvas`) updated accordingly.
- `render_canvas`: after projecting a node or way vertex to screen, if its id
  is in `drag_preview`'s set, add the delta before painting. Lookup only
  happens when a preview is active (`Option` check first) — the normal
  no-drag path pays no extra cost.
- `render_highlight`: same delta applied so the selection ring/way outline
  tracks the live preview.
- New method (not part of the trait — used directly by `MapViewer` at
  commit time): `commit_node_moves(&mut self, moves: &[(i64, f64, f64)])`
  that clones the current `OsmData`, applies the id -> (lat, lon) updates,
  sets `modified = true`, and calls `self.set_osm_data(Arc::new(new_data))`.

### `LayerManager` (`src/layers/mod.rs`)

- New helper to hit-test only against a given selection list, mirroring
  `hit_test_rect_all` in shape:
  ```rust
  fn hit_test_selection(&self, viewport: &Viewport, screen_pt: Point<Pixels>, selected: &[FeatureRef]) -> Option<FeatureRef>
  ```
  Implemented by delegating to each layer's existing `hit_test`, filtering
  candidates down to ones present in `selected`, then reusing
  `selection::resolve_hits` semantics (nearest wins).

## Interaction state (`src/main.rs`)

```rust
struct MoveDrag {
    layer_name: String,
    /// Snapshot of (node_id, lat, lon) at drag start.
    originals: Vec<(i64, f64, f64)>,
}
```

- `MapViewer` gains `move_drag: Option<MoveDrag>`.
- Left mouse-down: try `layer_manager.hit_test_selection(...)` against
  `self.selected` first.
  - Hit: resolve node-id set (Node ref -> its id; Way ref -> all its member
    node ids, union'd, grouped per layer via `layer_name`), snapshot
    `(id, lat, lon)` from that layer's `OsmData`, store `move_drag`. Do not
    fall through to recording a box-select start.
  - No hit: unchanged — record `mouse_down_pos` for box-select/click as
    today.
- Mouse-move:
  - If `move_drag` is set: `delta_px = current - mouse_down_pos`. Once past
    the 4px threshold, call `layer.set_drag_preview(&ids, delta_px)` on the
    owning layer and `cx.notify()`.
  - Else: unchanged.
- Mouse-up:
  - If `move_drag` is set:
    - Movement below threshold: discard `move_drag` (clear any preview that
      was set), call `handle_map_click(up_pos)` as today.
    - Movement above threshold: for each `(id, lat, lon)` in `originals`,
      compute `anchor = viewport.geo_to_screen(lat, lon)`, `new_screen =
      anchor + delta_px`, `new_geo = viewport.screen_to_geo(new_screen)`.
      Call `layer.commit_node_moves(&[(id, new_lat, new_lon), ...])` and
      `layer.clear_drag_preview()`. Clear `move_drag`.
  - Else: unchanged (existing box-select-or-click path).
- Key-down: if `move_drag.is_some()` and Escape is pressed, clear
  `move_drag` and call `clear_drag_preview()` on the owning layer; no
  mutation.

## UI

- Layers panel (`main.rs`): each `OsmLayer` row shows a small dot/asterisk
  after its name when `is_modified()` is true.

## Testing

- `selection.rs` or a new module: pure function resolving a selection list
  (mix of `Node`/`Way` refs) into a per-layer node-id set — unit tested
  directly, no GUI needed.
- `osm_layer.rs`: unit tests that
  - a set preview shifts screen positions returned by the render path
    without touching `osm_data`;
  - `commit_node_moves` updates `OsmData` node coordinates, rebuilds
    `node_index`/`way_index`/`way_vertices`, and sets `modified`;
  - `clear_drag_preview` after a preview leaves `osm_data` untouched
    (cancel path).
- No GUI automation available in this sandbox (documented limitation); the
  end-to-end mouse gesture itself isn't exercised by tests, only the
  underlying logic and rendering math. Verify with `cargo build`/`cargo
  test`/`cargo clippy`.
