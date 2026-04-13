# Issue 12: Add support for Editor Layer Index

## Context

osm-gpui currently offers a single hardcoded imagery source ("OpenStreetMap Carto") as the only entry in the Imagery menu (`src/main.rs:1195-1198`). The `TileLayer` (`src/layers/tile_layer.rs`) and `TileCoord::to_url` (`src/tiles.rs:33`) are hardcoded to `tile.openstreetmap.org`. The Editor Layer Index (ELI) publishes a GeoJSON at `https://osmlab.github.io/editor-layer-index/imagery.geojson` describing imagery sources worldwide, each with a URL template, type (`tms`/`wms`/`bing`), optional bounding geometry, and a `best` flag. Adding ELI lets users pick locally-relevant imagery from the Imagery menu.

## Approach

**Scope:** Only `tms` type entries are supported this PR (WMS/Bing/etc explicitly out of scope per issue).

- **New module `src/imagery/mod.rs`**:
  - `ImageryEntry { id, name, url_template, min_zoom, max_zoom, bbox: Option<GeoBounds>, polygon: Option<Vec<Vec<(f64,f64)>>>, best: bool, country_code: Option<String>, icon_url: Option<String> }`.
  - `fn fetch_and_cache() -> Result<String>` — uses `ureq` (blocking), caches body to `std::env::temp_dir()/osm-gpui-imagery-index/imagery.geojson`. If cache exists and is <7 days old, use it; else fetch. On fetch failure with existing cache, fall back to cache.
  - `fn parse(geojson_body: &str) -> Vec<ImageryEntry>` — parse GeoJSON; filter to `type == "tms"`; skip entries with `overlay == true`; decode polygon/bbox.
  - `fn entries_for_viewport(all: &[ImageryEntry], center_lat, center_lon) -> Vec<ImageryEntry>` — return entries with no bbox (global) + entries whose bbox/polygon contains the viewport center, sorted with `best == true` first, then alphabetical. Cap to ~30 entries for menu sanity.
- **Generalize `TileCoord` / `TileLayer`**:
  - Add `src/tiles.rs`: `fn url_from_template(template: &str, tile: &TileCoord) -> String` that substitutes `{z}`, `{x}`, `{y}`, `{zoom}`, `{-y}` (TMS flip), and picks a random subdomain for `{switch:a,b,c}` or `{s}` (single subdomain placeholder only when a `switch:` list is provided — otherwise leave as-is). Keep `TileCoord::to_url()` for back-compat (delegates to template with OSM URL).
  - `TileLayer` gains field `url_template: String` and a constructor `new_with_template(name, url_template, tile_cache)`. The existing `render_elements` computes URL per tile via the template (instead of `tile.to_url()`). `TileCache::fetch` already accepts an arbitrary URL (via `tile_url` arg) — verify and keep.
- **Dynamic Imagery menu**:
  - Replace the single `AddOsmCarto` action with a parameterized `AddImageryLayer { id: SharedString }` action (derive `Action`, `Clone`, `PartialEq`, serde). The handler looks up the entry by id in a shared `OnceLock<Mutex<Vec<ImageryEntry>>>` and pushes a layer request carrying `(name, url_template)` onto `LAYER_REQUESTS` (change `LAYER_REQUESTS` from `Vec<String>` to `Vec<LayerRequest>` enum with `OsmCarto` and `Imagery { name, url_template }` variants — or similar).
  - On app startup: spawn a background task (`cx.background_executor().spawn`) that fetches/parses ELI, stores entries in the shared store, and sets a "needs menu refresh" flag.
  - `MapViewer::render` (or a dedicated frame hook) checks whether the menu needs rebuilding and calls `cx.set_menus(...)` with current entries for the current viewport. Throttle: only rebuild when viewport center moves beyond the bbox of the last-used center (track `last_menu_center: Option<(lat,lon)>`; rebuild if None or distance > ~0.5 degrees).
  - Keep "OpenStreetMap Carto" as a static first item in the Imagery menu (already works, so it's always available regardless of ELI load state). Add a separator, then the ELI entries. If ELI load failed, append a disabled/info item "(Imagery index unavailable)" — implement as a no-op action if GPUI lacks disabled items.
- **Icons**: Out of scope for this PR (GPUI menu icon support is nontrivial). Skip `icon_url` entirely; parser can still read it but we don't render. Call this out in the PR description.
- **"Best" marker**: Prefix "★ " to names of entries with `best == true` so users see them first.

## Verification

- `cargo build --release` — clean, no new warnings.
- `cargo test --lib` — including new tests:
  - `imagery::tests::parses_sample_geojson` — small embedded sample covering one tms, one wms (filtered out), one with polygon, one with bbox, one global.
  - `imagery::tests::bbox_contains_point` — point inside/outside bbox/polygon filter logic.
  - `tiles::tests::url_template_substitution` — `{z}/{x}/{y}`, `{-y}` TMS flip, `{switch:a,b,c}` substitution.
- Run `cargo run --release -- --script docs/screenshots/smoke.osmscript` to confirm app still starts with the new menu wiring (Carto tile layer still works).
- Manual sanity: visual inspection is gated on human review; note in PR that the dispatcher did not visually verify the menu contents.

## Out of scope

- WMS, Bing, scanex, or other non-tms imagery types.
- Rendering menu icons from `icon_url`.
- Persisting "last selected imagery" across runs.
- Per-layer attribution rendering on the map.
- Respecting ELI `min_zoom`/`max_zoom` when rendering (out of scope; log only).
- Refreshing menu on every viewport pan (only refresh when center moves substantially).
- Touching `src/map.rs`, `src/mercator.rs`, `src/background.rs`, `src/http_image_loader.rs`, `src/data.rs` (declared dead).
