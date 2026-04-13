# Issue 6: Don't include coordinate grid layer by default

## Context
The `GridLayer` is unconditionally added in `MapViewer::new()` (src/main.rs:183), so every run shows a coordinate grid that confuses users who don't know what it's for. The issue asks to remove it from the default stack and expose it via a menu item so users can opt in.

## Approach
- `src/main.rs`:
  - Remove `layer_manager.add_layer(Box::new(GridLayer::new()));` from `MapViewer::new()` (line 183).
  - Drop the now-unused `grid_layer::GridLayer` import from the `use osm_gpui::layers::...` line — no, keep it: we still need `GridLayer::new()` at runtime when the menu action fires.
  - Add a new action `AddCoordinateGrid` to the `actions!` macro (line 18).
  - Handle it in `check_for_layer_requests` (src/main.rs:409) by pushing the string `"Coordinate Grid"` into `LAYER_REQUESTS` from the action fn, and branching on that string to add a `GridLayer` if `find_layer("Coordinate Grid")` is `None`.
  - Add a standalone action fn `add_coordinate_grid` mirroring `add_osm_carto`.
  - Register the action with `cx.on_action(add_coordinate_grid);` in the `Application::new().run(...)` block.
  - Extend the `Imagery` menu (src/main.rs:1195-1198) to include a `"Coordinate Grid"` item — rename the menu to `"Layers"` is out of scope; keep the name `Imagery` to minimize churn, add a separator, then the grid item. (Alternative: new "View" menu with just Coordinate Grid. Choose Imagery to keep diff small.)

Final choice: add a second menu entry under the existing Imagery menu, since a grid is effectively an overlay users toggle on. The menu name stays `Imagery`.

## Verification
- `cargo build --release` clean.
- `cargo test --lib` passes.
- `cargo run --release -- --script docs/screenshots/smoke.osmscript` starts the app without the grid visible. (Smoke check only — the grid does not need to appear in the screenshot output.)
- Manually: confirm the new menu item exists and, when invoked, the grid becomes visible and stays visible; invoking it a second time is a no-op (not a toggle — matches the `AddOsmCarto` pattern).

## Out of scope
- Converting this into a true toggle/remove-layer flow (the existing Imagery item is also add-only; a toggle is a separate UX change).
- Persisting the user's layer preference across runs.
- Changing `GridLayer`'s rendering, spacing, or styling.
- Reordering, renaming, or reorganizing the menu bar beyond adding one item.
