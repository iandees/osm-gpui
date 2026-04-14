# Issue 9: Don't flash black when OSM Carto tiles are loading

## Context
In `TileLayer::render_elements` (src/layers/tile_layer.rs:49–133) each tile div
paints `rgb(0x2d3748)` as a background (line 87) and shows a "Downloading…"
placeholder via `img(...).with_loading(...)` (lines 95–109) until gpui's asset
system produces the tile image. On a dark theme this loading state reads as a
black flash whenever the user pans to a region where tiles aren't yet cached.

The issue requests that we show the **parent tile** (zoom level z-1) scaled and
clipped to the child's footprint while the child loads — so the user sees a
blurrier version of the right content instead of a black cell.

## Approach

- **`src/tiles.rs`**
  - Add `TileCoord::parent(&self) -> Option<TileCoord>`. Returns `None` at z==0;
    otherwise `TileCoord { x: self.x/2, y: self.y/2, z: self.z-1 }`.
  - Add `TileCoord::quadrant(&self) -> (u32, u32)` returning `(self.x % 2, self.y % 2)`:
    column and row (0 or 1) within the parent tile.
  - Unit tests for both.

- **`src/layers/tile_layer.rs`** in `render_elements`:
  - Keep the existing outer positioned div for the child tile footprint, keep
    the `bg(rgb(0x2d3748))` as an ultimate fallback.
  - Add `.overflow_hidden()` to the outer div so anything 2× sized inside is
    clipped to the child footprint.
  - **Before** the child image element, add a sibling element that renders the
    parent tile image:
    - Wrap in a div sized `2 * tile_width` by `2 * tile_height`, positioned
      with `left`/`top` offset of `-tile_width * qx` and `-tile_height * qy`
      where `(qx, qy) = child.quadrant()`.
    - Inside that wrapper, `img(use_asset::<TileAsset>(parent_url))` with
      `size_full()` and `object_fit(Cover)`. No loading/fallback element —
      if the parent isn't loaded yet, nothing paints there and the default
      `bg` shows, which is fine.
  - Skip the parent render entirely when `tile_coord.z == 0` (no parent exists).
  - The child image div is appended **after** the parent wrapper so it paints
    on top; once the child loads, it covers the parent image.

- No change to `TileCache`, `TileAsset::load`, or the canvas-mode boundary rendering.

## Verification
- `cargo build --release` clean (no new warnings).
- `cargo test --lib` green (pre-existing 54 tests + 2 new parent/quadrant tests).
- Manual (document in PR test plan): with OSM Carto layer active, pan to a
  previously unvisited region at z ≥ 3. Cells should briefly show a blurry
  parent-tile rendering instead of flashing to the dark placeholder.
- Smoke: `cargo run --release -- --script docs/screenshots/smoke.osmscript` still
  runs to completion.

## Out of scope
- Multi-level parent fallback (z-2, z-3). Single level is enough to eliminate
  the flash for typical pan speeds.
- Synchronous disk-cache reads. We rely on gpui's asset system so parents
  share cache with children; no custom sync read path.
- Debounce / throttling of parent fetches. In the worst case a viewport of 16
  children shares 4 unique parents, which is a modest ~25% fetch increase.
- Any change to the downloading/fallback UI on other failure paths.
