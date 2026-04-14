# Issue 26: Support min and max zooms from Editor Layer Index

## Context
ELI entries are parsed in `src/imagery/mod.rs` with `min_zoom: Option<u32>` and `max_zoom: Option<u32>` already populated, but those fields are dropped on the floor when an imagery menu pick is converted into a `TileLayer` (`src/main.rs` `add_imagery_layer` -> `LayerRequest::Imagery` -> `TileLayer::new_with_template` in `src/layers/tile_layer.rs`). As a result, layers like Mapbox/Maxar are requested at zooms outside their advertised range, which produces 404 spam at low zoom and blank/black tiles at high zoom. We need to plumb the optional bounds through to `TileLayer` and use them in `render_elements` (and the boundary debug pass in `render_canvas`) so out-of-range zooms either skip drawing entirely or fall back to overzoomed tiles at the layer's `max_zoom`.

## Approach
- `src/layers/tile_layer.rs`:
  - Add `min_zoom: Option<u32>` and `max_zoom: Option<u32>` fields on `TileLayer`. Default both to `None` in existing constructors. Add a new constructor (or builder methods `with_min_zoom`/`with_max_zoom`) that callers can use to set them; keep `new_with_template` working for the OSM Carto path by leaving the bounds at `None`.
  - Replace the local `let tile_zoom = zoom_level.round().max(0.0).min(18.0) as u32;` (used in both `render_elements` and `render_canvas`) with a shared helper, e.g. `fn effective_tile_zoom(&self, viewport_z: f64) -> Option<u32>` that returns:
    - `None` if `viewport_z.round() < min_zoom` (skip the layer entirely — return early with no elements / no boundary paint).
    - `None` if `viewport_z.round() > max_zoom + 1` (overzoom only one level past max; beyond that, stop drawing — per the issue's "overzoom to z+1 but then stop" wording).
    - `Some(max_zoom)` if `min_zoom <= viewport_z.round() <= max_zoom + 1` and viewport zoom rounds above `max_zoom` (clamp tile request zoom; existing tile-rect-to-screen math keeps the visual scale, so tiles will appear scaled up to ~2× — the desired overzoom behavior).
    - `Some(viewport_z.round() as u32)` otherwise, still capped at the existing hard limit of 18.
  - `render_elements` and `render_canvas` both early-return when the helper yields `None`, then use the returned zoom for both `get_tiles_for_bounds` and the per-tile URL/parent-fallback URL (parent fallback already uses `tile_coord.parent()`, so it composes correctly because the fallback is computed against the clamped tile coord, not the raw viewport zoom).
- `src/main.rs`:
  - Extend `LayerRequest::Imagery` with `min_zoom: Option<u32>` and `max_zoom: Option<u32>`.
  - In `add_imagery_layer`, copy `entry.min_zoom` / `entry.max_zoom` into the request.
  - In `check_for_layer_requests`, pass them into the `TileLayer` constructor (via the new builder methods or expanded constructor).
- `TileLayer::stats` (`src/layers/tile_layer.rs`): append `Min Zoom` / `Max Zoom` rows when set, so the debug panel reflects the active limits.

## Verification
- `cargo build --release`
- `cargo test --lib` (add a small unit test on the `effective_tile_zoom` helper in `src/layers/tile_layer.rs` covering: no bounds = passthrough; below min = `None`; at min/max = use viewport z; one above max = clamp to max; two above max = `None`).
- Render smoke test: pick an ELI entry with a low `max_zoom` (e.g. a regional aerial capped around z=14), zoom in past max+1 and confirm the layer disappears (other layers still draw); zoom to exactly max+1 and confirm a single overzoomed level renders. Zoom out below `min_zoom` and confirm no requests fire (watch the active-downloads stat or network).

## Out of scope
- Honoring ELI `min_zoom`/`max_zoom` for the imagery menu's enable/disable state per viewport zoom.
- WMS/Bing/other non-`tms` types (still filtered out at parse time).
- Per-layer x/y bounding-box clipping of tile requests — only zoom limits in this PR.
- Reworking the global `min(18.0)` zoom cap or the `TileCache` — leave both alone.
