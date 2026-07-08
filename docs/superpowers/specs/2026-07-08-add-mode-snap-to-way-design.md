# Add mode: snap new nodes onto existing ways

## Goal

In Add mode (`docs/superpowers/specs/2026-07-07-mode-selector-design.md`),
clicking near an existing way's line geometry should snap the newly created
node onto that way — splicing it into the way's node list at the correct
position — instead of creating a free-standing node that merely sits close
to the line. Holding Control bypasses all snapping (both the existing
snap-to-node and the new snap-to-way) for that click, always producing a
fully independent node, matching today's behavior.

## Scope

### Existing building blocks (reused, not modified)

- `OsmLayer::hit_test_segment(&self, viewport, screen_pt, tol_px) -> Option<(way_id, node_id_a, node_id_b, segment_index)>`
  (`src/layers/osm_layer.rs:1707`) — rtree-backed nearest-segment query,
  already used by Extrude mode and by double-click "insert node on segment"
  (`insert_node_on_segment`, `src/main.rs:1612`).
- `selection::point_to_segment_distance(p, a, b) -> f32` (`src/selection.rs:44`).
- `EditableLayer::insert_node_into_way(&mut self, way_id, index, lat, lon) -> i64`
  (`src/layers/osm_layer.rs:1291`) — creates a node and splices it into an
  existing way's node list at `index`.
- `UndoableAction::InsertNodeIntoWay { layer, way_id, index, node_id }`
  (`src/undo.rs:83`) — undo removes the node from the way and deletes it;
  redo is a no-op (documented scope boundary shared by every Add/Extrude/
  Building undo entry — see `src/undo.rs:887-897` for the rationale).

### New pure geometry helpers

- `selection::nearest_point_on_segment(p: Point<Pixels>, a: Point<Pixels>, b: Point<Pixels>) -> Point<Pixels>`
  — same clamped-projection math as `point_to_segment_distance`, returning
  the projected point instead of just its distance.
- `OsmLayer::snap_to_way(&self, viewport: &Viewport, screen_pt: Point<Pixels>, tol_px: f32) -> Option<(way_id, node_id_a, node_id_b, segment_index, lat, lon)>`
  — same rtree query and segment loop as `hit_test_segment`, but for the
  winning segment also computes the nearest point (via
  `nearest_point_on_segment` on the two endpoints' projected screen
  positions) and converts it back to `(lat, lon)` via
  `viewport.screen_to_geo`. Uses the same `6.0` px tolerance already used by
  Extrude mode's `hit_test_segment` call (`src/main.rs:685`).

### `handle_add_click` changes (`src/main.rs:1655`)

- Gains a `ctrl_held: bool` parameter, threaded from `event.modifiers.control`
  at both call sites in `handle_map_click` (mirroring how `shift_held` is
  already threaded today, `src/main.rs:1431` and `:1455`).
- The existing "click hits an existing node → connect instead of creating"
  check (`src/main.rs:1663-1683`, only active when `add_progress.is_some()`)
  is skipped entirely when `ctrl_held` is true.
- When not `ctrl_held`, before creating a node (in both the `None` and
  `Some(progress)` arms of the `match self.add_progress.take()`), try
  `snap_to_way` first:
  - **No hit** (or `ctrl_held`): unchanged today's behavior — free `add_node`.
  - **Hit, first click of a sequence** (`add_progress` was `None`):
    `insert_node_into_way` at the resolved index instead of `add_node`;
    push the existing `UndoableAction::InsertNodeIntoWay` unchanged.
    `add_progress` becomes `Some(AddProgress { way_id: None, last_node_id: new_id })`,
    same as the free-node case — the next click still starts a *new* way
    from this node, it does not implicitly continue the snapped-onto way.
  - **Hit, 2nd+ click**: `insert_node_into_way` splices the new node into the
    snapped-onto way (`snap_way_id`/`snap_index`), then that same node is
    folded into the way being drawn exactly as today (`add_way`/
    `extend_way` via `add_extend_or_start_way`). This is two mutations from
    one click, so it needs one new compound undo entry (below) rather than
    two stack entries, to preserve today's "one click = one undo step"
    invariant (see `src/undo.rs:48-58`).

### New undo action: `UndoableAction::SnapExtendWay`

```rust
/// Add mode's 2nd+ click when it snaps onto an existing way: a node
/// spliced into `snap_way_id` at `snap_index` (like `InsertNodeIntoWay`),
/// then that same node folded into the way being drawn — `way_id`,
/// created fresh if `way_created`, otherwise extended (like `ExtendWay`
/// with `node_created: true`). Undo reverses both, in the order that
/// matches how they were applied: detach from the drawn way first
/// (deleting it if `way_created`, without deleting the node — it is
/// still referenced by `snap_way_id`), then remove the node from
/// `snap_way_id` and delete it.
SnapExtendWay {
    layer: LayerId,
    way_id: i64,
    way_created: bool,
    snap_way_id: i64,
    snap_index: usize,
    node_id: i64,
},
```

Redo is a no-op, matching `ExtendWay`/`InsertNodeIntoWay`'s existing
documented scope boundary (`src/undo.rs:887-897`, `:964-968`).

`add_extend_or_start_way` (`src/main.rs:1759`) gains an optional
`snap: Option<(i64, usize)>` parameter (the snapped-onto way id + index, if
this node was just created via `snap_to_way`); when `Some`, it pushes
`SnapExtendWay` instead of `ExtendWay`. The existing "connect to an
already-existing node" call site (`src/main.rs:1668`, `node_created: false`)
always passes `snap: None` — snapping only applies to newly-created nodes.

### Out of scope

- Snapping while extending a way *onto the same way already in progress*
  (`snap_way_id == way_id`) is not special-cased — it's handled by the
  generic two-mutation path above and produces whatever topology that
  implies (e.g. a branch). Not disallowed, not given special UX.
- No visual snap indicator/preview (e.g. highlighting the candidate segment
  before click) — this spec only covers the click-time snapping behavior.
- No new tolerance configuration; reuses the existing `6.0` px constant used
  by Extrude mode.

## Testing

- **Pure geometry unit tests** (`src/selection.rs`, `src/layers/osm_layer.rs`
  `#[cfg(test)] mod tests`, following the existing style e.g.
  `hit_test_segment_finds_nearest_segment_and_endpoint_indices`,
  `src/layers/osm_layer.rs:2521`): `nearest_point_on_segment` on and off the
  segment, `snap_to_way` tolerance boundary, mid-segment vs.
  endpoint-adjacent snap, correct `(way_id, segment_index)` resolution.
- **App-level tests** (new, first of their kind in `src/main.rs`): this repo
  already depends on `gpui`/`gpui_platform` with the `test-support` feature
  (`Cargo.toml:19-20`), currently unused beyond enabling `render_to_image`
  for the `.osmscript` screenshot harness (`src/script_harness.rs`). Add a
  `#[cfg(test)] mod tests` in `main.rs` using `#[gpui::test]` +
  `TestAppContext`: build a `MapViewer` in a headless test window
  (`cx.add_window(|window, cx| MapViewer::new(window, cx))`), load a small
  in-memory `OsmData` fixture with one way, dispatch real
  `MouseDownEvent`/`MouseUpEvent` (with `Modifiers { control: true, .. }`
  where needed) directly to `handle_mouse_down`/`handle_mouse_up`, and
  assert on `layer.get_osm_data()` (way node lists, node existence/
  coordinates) and `self.undo_stack` (including running undo and asserting
  the way/node reverts). Covers: first-click snap onto a way, 2nd-click
  snap-and-fold (`SnapExtendWay` apply + undo), Ctrl bypassing both node-
  and way-snap, and no-snap-in-range falling back to today's free-node
  behavior.
- **`.osmscript` fix (small, secondary)**: the script `click`/`drag` ops
  currently always synthesize `gpui::Modifiers::none()`
  (`src/script_harness.rs:176,184,190,205,213`; `Op::Click`,
  `src/script/op.rs:48`, has no modifier field). Add an optional modifier
  flag to the `click` op (parser + `Op::Click` + `ScriptCommand::Click` +
  `process_script_command`) so a `.osmscript` file can demonstrate/
  screenshot the Ctrl-bypass gesture. This is a manual/visual aid, not part
  of the automated regression suite above.
