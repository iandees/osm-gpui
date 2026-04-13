# Issue 22: Don't load OpenStreetMap Carto tile when loading another layer's tile

## Context
PR #16 added a parent-tile (z-1) fallback so the previous zoom's tile is shown while the current-zoom tile downloads, avoiding a dark flash. The fallback URL is built by calling `parent_coord.to_url()` at `src/layers/tile_layer.rs:102`, but `TileCoord::to_url()` (`src/tiles.rs:33-38`) hardcodes `https://tile.openstreetmap.org/{z}/{x}/{y}.png`. The rest of the layer renders child tiles via `url_from_template(&self.url_template, tile_coord)` (line 95), so for any non-Carto `TileLayer` the parent fallback incorrectly fetches/displays a Carto tile instead of the same provider's parent tile.

## Approach
- `src/layers/tile_layer.rs:100-117` (the `parent_fallback` closure inside `render_elements`):
  - Replace `let parent_url = parent_coord.to_url();` with `let parent_url = url_from_template(&self.url_template, &parent_coord);` so the fallback uses the layer's own template (already imported at line 7).
- `src/tiles.rs:32-38` — `TileCoord::to_url()`:
  - This method is now an attractive nuisance that encodes Carto's URL on a generic coordinate type. Check callers (there is at least one test usage at tiles.rs:227 and 307). Either:
    - Leave as-is (minimal change), or
    - Remove it and update the two in-file tests to use `url_from_template` with an explicit template.
  - Recommended: remove `to_url()` to prevent regressions, since it has no non-test production callers after this fix. Verify via Grep for `to_url(` across `src/` before removing.

## Verification
- `cargo build --release`
- `cargo test --lib` (covers the tiles.rs unit tests that reference `to_url`)
- Manual: run the app, switch to a non-Carto tile layer (e.g. any alternate template registered via `TileLayer::new_with_template`), pan/zoom, and confirm that while new tiles load the visible parent tiles match the active layer's style rather than Carto.

## Out of scope
- Any caching/eviction changes in `TileCache`.
- Multi-level parent fallback (z-2, z-3, etc.) — this PR keeps the existing single-level behavior.
- Refactoring `MapLayer` trait, layer-picker UI, or introducing a registry of named templates.
- Touching dead files listed in the README (`src/map.rs`, `src/mercator.rs`, `src/background.rs`, `src/http_image_loader.rs`, `src/data.rs`).
