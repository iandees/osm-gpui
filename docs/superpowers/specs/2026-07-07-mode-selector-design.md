# Mode selector: Select / Add / Building

## Goal

Introduce editing modes, surfaced as a new toolbar panel to the left of the
map. **Select** is today's existing click/drag/box-select behavior. **Add**
places nodes and chains them into a way with successive clicks. **Building**
places a rectangular `building=yes` way via a 3-click perpendicular-rectangle
gesture, with a live preview.

## Scope

### Left toolbar panel

- New narrow panel (~56px), analogous to `render_side_panel`, inserted as the
  first child of the top-level `flex_row()` in `MapViewer::render`
  (`src/main.rs:1177-1181`), i.e. to the left of the map area. The
  `panel_width`/`map_size` calculation (`src/main.rs:1159-1164`) is extended
  to also subtract this panel's width.
- Three icon buttons: Select, Add, Building. The active mode is highlighted
  (accent background). Add/Building are disabled (with a tooltip explaining
  why) when `active_layer` is `None`.
- New enum on `MapViewer`:
  ```rust
  enum EditMode { Select, Add, Building }
  ```
  Field `mode: EditMode`, default `Select`. Switching modes (via toolbar click,
  keybinding, or the Escape fallback below) clears any in-progress
  `add_progress` / `building_progress` state without committing it.
- New parameterized action (same pattern as `MoveLayer`/`DeleteLayer`):
  ```rust
  #[action(namespace = mode)]
  struct SetMode { mode: EditMode }
  ```
  dispatched from toolbar buttons and from two new keybindings:
  `KeyBinding::new("a", SetMode { mode: EditMode::Add }, None)` and
  `KeyBinding::new("b", SetMode { mode: EditMode::Building }, None)`,
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
- `remove_node(&mut self, node_id: i64)` / `remove_way(&mut self, way_id: i64)`
  — inverse operations, used only by undo (below); patch caches symmetrically.

### Undo (`src/undo.rs`)

Three new `UndoableAction` variants:

- `PlaceNode { layer_name: String, node_id: i64, lat: f64, lon: f64 }` — undo
  calls `remove_node`.
- `ExtendWay { layer_name: String, way_id: i64, node_id: i64, lat: f64, lon: f64, way_created: bool }`
  — undo removes `node_id` from the way's node list (and calls `remove_way` if
  `way_created`), then `remove_node(node_id)`.
- `CreateBuilding { layer_name: String, way_id: i64, node_ids: [i64; 4] }` —
  undo calls `remove_way` then `remove_node` for each of the 4 ids.

## Out of scope

- Deleting/undoing an in-progress Building preview via Escape.
- Any mode besides Select/Add/Building (e.g. delete-feature mode).
- Persisting/uploading newly created features (no `.osm` writer, no
  changeset upload) — same boundary as existing move/tag-edit work.
- Snapping newly placed nodes onto nearby existing nodes/ways except the
  explicit "click an existing node/way to finish" gesture in Add mode.
- Undo/redo for `active_layer` selection itself (not an undoable action).
- Multi-layer Add/Building (always targets the single `active_layer`).

## Testing

- `selection.rs` or a new module: pure functions for computing the Building
  mode rectangle's 4 corners from `corner_a`, `corner_b`, and a
  perpendicular-offset point — unit tested directly (geometry math, no GUI).
- `osm_layer.rs`: unit tests for `add_node`/`add_way`/`extend_way` and their
  inverses (`remove_node`/`remove_way`), verifying both `OsmData` and derived
  caches (`node_cache`, `node_index`, `way_index`, `way_vertices`, etc.) stay
  consistent after each operation and after undoing it.
- `undo.rs`: unit tests that `PlaceNode`/`ExtendWay`/`CreateBuilding` push and
  invert correctly against a layer.
- No GUI automation available in this sandbox (documented limitation): the
  end-to-end click sequence isn't exercised by tests, only the underlying
  mutation/geometry logic. Verify with `cargo build`/`cargo test`/`cargo
  clippy`.
