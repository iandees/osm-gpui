# Mode selector: Select / Add / Building / Extrude

## Goal

Introduce editing modes, surfaced as a new toolbar panel to the left of the
map. **Select** is today's existing click/drag/box-select behavior. **Add**
places nodes and chains them into a way with successive clicks. **Building**
places a rectangular `building=yes` way via a 3-click perpendicular-rectangle
gesture, with a live preview. **Extrude** draws a rectangular `building=yes`
way off an existing way segment by dragging perpendicular to it, and also
lets you insert a node into a way by double-clicking a segment.

## Scope

### Left toolbar panel

- New narrow panel (~56px), analogous to `render_side_panel`, inserted as the
  first child of the top-level `flex_row()` in `MapViewer::render`
  (`src/main.rs:1177-1181`), i.e. to the left of the map area. The
  `panel_width`/`map_size` calculation (`src/main.rs:1159-1164`) is extended
  to also subtract this panel's width.
- Four icon buttons: Select, Add, Building, Extrude. The active mode is
  highlighted (accent background). Add/Building/Extrude are disabled (with a
  tooltip explaining why) when `active_layer` is `None`.
- New enum on `MapViewer`:
  ```rust
  enum EditMode { Select, Add, Building, Extrude }
  ```
  Field `mode: EditMode`, default `Select`. Switching modes (via toolbar click,
  keybinding, or the Escape fallback below) clears any in-progress
  `add_progress` / `building_progress` / `extrude_drag` state without
  committing it.
- New parameterized action (same pattern as `MoveLayer`/`DeleteLayer`):
  ```rust
  #[action(namespace = mode)]
  struct SetMode { mode: EditMode }
  ```
  dispatched from toolbar buttons and from three new keybindings:
  `KeyBinding::new("a", SetMode { mode: EditMode::Add }, None)`,
  `KeyBinding::new("b", SetMode { mode: EditMode::Building }, None)`, and
  `KeyBinding::new("x", SetMode { mode: EditMode::Extrude }, None)`,
  registered alongside the existing `cmd-z`/`cmd-shift-z` bindings
  (`src/main.rs:~1573`). No dedicated keybinding for Select; Escape returns to
  Select when there is no in-progress state to cancel (see below).

### Active layer

- `MapViewer` gains `active_layer: Option<String>` (layer name).
- In `render_layers_section` (`src/side_panel.rs`), clicking a layer row
  outside the checkbox sets `active_layer` to that layer's name. Only rows
  backed by `OsmLayer` (i.e. layers that support `add_node`/`add_way`) are
  eligible; tile/imagery/grid layer rows do not respond to this click. The
  active row renders with a persistent highlight, distinct from hover.
- If the active layer is deleted, `active_layer` resets to `None`.

### Add mode

Internal (non-`selected`) state:

```rust
struct AddProgress {
    layer_name: String,
    way_id: Option<i64>, // None until a 2nd point starts a way
    last_node_id: i64,
}
```

`MapViewer` gains `add_progress: Option<AddProgress>`.

- Click on empty map space while `add_progress` is `None`: create a new node
  (placeholder id, see below) in the active layer at the clicked geo
  position, select it (`self.selected = vec![that node]`), and set
  `add_progress = Some(AddProgress { way_id: None, last_node_id: new_id, .. })`.
  Undo action pushed: `PlaceNode`.
- Click on empty map space while `add_progress` is `Some`: create another new
  node. If `way_id` is `None` (this is the 2nd point overall), create a new
  way containing `[last_node_id, new_node_id]` and set `way_id = Some(new_way_id)`;
  otherwise append `new_node_id` to the existing way. Update
  `last_node_id = new_node_id`, select the way. Undo action pushed:
  `ExtendWay`.
- Click on an existing node/way while `add_progress` is `Some`: connect to it
  — append the existing node's id to the way (creating the way first if this
  is only the 2nd point) — and finish: clear `add_progress`, keep the
  finished way selected, remain in Add mode.
- Escape or Enter while `add_progress` is `Some`: finish the way as currently
  placed (no additional mutation), clear `add_progress`, remain in Add mode.
- Escape while `add_progress` is `None`: switch `mode` to `Select`.
- After a way finishes (by any path above), mode stays `Add`, ready for the
  next node.

### Building mode

Internal state:

```rust
struct BuildingProgress {
    layer_name: String,
    corner_a: (f64, f64),           // geo coords
    corner_b: Option<(f64, f64)>,   // geo coords, set after 2nd click
}
```

`MapViewer` gains `building_progress: Option<BuildingProgress>`.

- Click 1: record `corner_a` (geo position of the click). Nothing is
  committed to `OsmData` yet.
- Mouse-move after click 1, before click 2: render a straight preview line
  from `corner_a` to the cursor (no data mutation).
- Click 2: record `corner_b`, fixing edge A–B.
- Mouse-move after click 2, before click 3: compute the rectangle with A–B as
  one edge and the perpendicular offset determined by the cursor's distance
  from line A–B (projected perpendicular distance in screen space, converted
  to geo per corner). Render this as a preview polygon overlay (new transient
  render state, analogous to `box_select`'s rectangle overlay).
- Click 3: compute final 4 corners the same way, using the click position for
  the perpendicular offset. Commit: create 4 new nodes (placeholder ids) and
  one new closed way (`nodes = [n0, n1, n2, n3, n0]`) tagged `building=yes` in
  the active layer, in a single undo action (`CreateBuilding`). Clear
  `building_progress`, select the new way, remain in Building mode.
- No Escape-to-cancel for an in-progress rectangle (out of scope per user
  decision) — switching modes away from Building abandons `building_progress`
  silently (nothing was committed, so there's nothing to undo).

### Extrude mode

Internal state:

```rust
struct ExtrudeDrag {
    layer_name: String,
    way_id: i64,
    // Indices into way.nodes of the two segment endpoints (adjacent, a < b).
    seg_start_idx: usize,
    seg_end_idx: usize,
    node_a: i64,
    node_b: i64,
}
```

`MapViewer` gains `extrude_drag: Option<ExtrudeDrag>`.

- Hit-testing a "way segment under the cursor" reuses
  `selection::point_to_segment_distance` against each way's consecutive node
  pairs (same tolerance as existing hit-testing), returning the owning way id
  and the segment's two node ids/indices.
- Mouse-down on a way segment (not a node) in Extrude mode: record
  `extrude_drag` with that segment's two endpoint node ids/indices. This does
  not select anything and does not start a box-select.
- Mouse-drag: compute the perpendicular offset of the current cursor position
  relative to the segment `node_a`–`node_b` (project the drag vector onto the
  segment's perpendicular, mirroring the perpendicular-offset math used for
  Building mode's rectangle). Render a live preview rectangle: `node_a`,
  `node_b` as one fixed edge, two new points offset perpendicular by that
  amount as the other edge — same transient-preview mechanism as Building
  mode (no `OsmData` mutation yet).
- Mouse-up past the existing 4px click/drag threshold: commit. Create 2 new
  nodes (placeholder ids) at the offset positions, create a new closed way
  `[node_a, node_b, new_far_b, new_far_a, node_a]` tagged `building=yes` in
  the active layer, as one undo action (`ExtrudeWay`). Clear `extrude_drag`,
  select the new way, remain in Extrude mode. `node_a`/`node_b` (the original
  segment's nodes) are untouched — they remain shared between the original
  way and the new building way.
- Mouse-up at or below the threshold: no-op (treated as an aborted drag, not
  a click — Extrude mode has no plain-click behavior of its own).
- Double-click on a way segment in Extrude mode (not a drag): insert a new
  node (placeholder id) into that way's node list, between the segment's two
  endpoint indices, at the double-click position. This is a single undo
  action (`InsertNodeIntoWay`) and does not change `mode` or selection. This
  behavior is specific to Extrude mode (out of scope for Select mode, see
  below).

### Placeholder ids

- `OsmLayer` gains two counters, `next_placeholder_node_id: i64` and
  `next_placeholder_way_id: i64`, both initialized to `-1` and decremented
  after each use (`-1, -2, -3, ...`), following the JOSM convention that
  negative ids are locally-created, not-yet-uploaded features. No upload path
  exists yet, so no id-remapping is needed (out of scope, same as existing
  `modified` flag handling).

### `OsmLayer` mutation API (`src/layers/osm_layer.rs`)

New methods, following the existing clone-mutate-patch-caches pattern used by
`commit_node_moves`/`set_tag`:

- `add_node(&mut self, lat: f64, lon: f64) -> i64` — clones `OsmData`, inserts
  a new `OsmNode` with the next placeholder id, patches `node_cache` and
  `node_index`, sets `modified = true`, returns the new id.
- `add_way(&mut self, node_ids: Vec<i64>, tags: Vec<(String, String)>) -> i64`
  — clones `OsmData`, inserts a new `OsmWay`, patches `way_bboxes`,
  `way_vertices`, `way_styles`, `way_index`, `way_id_to_index`,
  `node_to_ways`, `layer_bbox`, sets `modified = true`, returns the new id.
- `extend_way(&mut self, way_id: i64, node_id: i64)` — appends `node_id` to an
  existing way's node list and patches the same derived caches as `add_way`
  for that one way.
- `insert_node_into_way(&mut self, way_id: i64, index: usize, lat: f64, lon: f64) -> i64`
  — clones `OsmData`, inserts a new `OsmNode` at the next placeholder id,
  splices it into `way.nodes` at `index`, patches the same derived caches as
  `extend_way` (this way's `way_vertices`/`way_bboxes`/`way_styles`,
  `node_cache`, `node_index`, `node_to_ways`), sets `modified = true`, returns
  the new node id.
- `remove_node(&mut self, node_id: i64)` / `remove_way(&mut self, way_id: i64)`
  — inverse operations, used only by undo (below); patch caches symmetrically.
- `remove_node_from_way(&mut self, way_id: i64, index: usize)` — inverse of
  `insert_node_into_way`; splices the node out of `way.nodes` at `index` and
  patches caches (does not itself delete the node — callers combine with
  `remove_node`).

### Undo (`src/undo.rs`)

Five new `UndoableAction` variants:

- `PlaceNode { layer_name: String, node_id: i64, lat: f64, lon: f64 }` — undo
  calls `remove_node`.
- `ExtendWay { layer_name: String, way_id: i64, node_id: i64, lat: f64, lon: f64, way_created: bool }`
  — undo removes `node_id` from the way's node list (and calls `remove_way` if
  `way_created`), then `remove_node(node_id)`.
- `CreateBuilding { layer_name: String, way_id: i64, node_ids: [i64; 4] }` —
  undo calls `remove_way` then `remove_node` for each of the 4 ids.
- `ExtrudeWay { layer_name: String, way_id: i64, new_node_ids: [i64; 2] }` —
  undo calls `remove_way` then `remove_node` for each of the 2 new ids
  (`node_a`/`node_b`, belonging to the original segment, are untouched).
- `InsertNodeIntoWay { layer_name: String, way_id: i64, index: usize, node_id: i64, lat: f64, lon: f64 }`
  — undo calls `remove_node_from_way(way_id, index)` then `remove_node(node_id)`.

## Out of scope

- Deleting/undoing an in-progress Building or Extrude preview via Escape.
- Any mode besides Select/Add/Building/Extrude (e.g. delete-feature mode).
- Persisting/uploading newly created features (no `.osm` writer, no
  changeset upload) — same boundary as existing move/tag-edit work.
- Snapping newly placed nodes onto nearby existing nodes/ways except the
  explicit "click an existing node/way to finish" gesture in Add mode.
- Undo/redo for `active_layer` selection itself (not an undoable action).
- Multi-layer Add/Building/Extrude (always targets the single `active_layer`).
- Double-click-to-insert-a-node in Select mode (Extrude mode only, per design
  decision).

## Testing

- `selection.rs` or a new module: pure functions for computing the Building/
  Extrude rectangle's corners from a fixed edge and a perpendicular-offset
  point — unit tested directly (geometry math, no GUI). Shared between the
  two modes since the math is identical.
- `osm_layer.rs`: unit tests for `add_node`/`add_way`/`extend_way`/
  `insert_node_into_way` and their inverses (`remove_node`/`remove_way`/
  `remove_node_from_way`), verifying both `OsmData` and derived caches
  (`node_cache`, `node_index`, `way_index`, `way_vertices`, etc.) stay
  consistent after each operation and after undoing it.
- `undo.rs`: unit tests that `PlaceNode`/`ExtendWay`/`CreateBuilding`/
  `ExtrudeWay`/`InsertNodeIntoWay` push and invert correctly against a layer.
- No GUI automation available in this sandbox (documented limitation): the
  end-to-end click sequence isn't exercised by tests, only the underlying
  mutation/geometry logic. Verify with `cargo build`/`cargo test`/`cargo
  clippy`.
