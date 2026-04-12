# osm-gpui

Experimental OpenStreetMap editor built on [GPUI](https://github.com/zed-industries/zed) (the framework behind Zed). The long-term goal is a JOSM-class editor that feels smoother and more native. Right now it's a viewer — editing is not implemented.

## Status (honest)

**Working**
- Pan (left-drag) and zoom (scroll wheel, zoom-at-cursor), clamped to zoom 1–20.
- Web Mercator projection (EPSG:3857) with lat clamped to ±85.051°.
- OSM XML loading via **File > Open** (⌘O). Renders nodes as yellow squares and ways as blue polylines. First loaded file auto-fits viewport.
- Raster tiles via **Imagery > OpenStreetMap Carto**. Async download with `ureq`, PNG cached to `/tmp/osm-gpui-tiles/`, loaded through GPUI's asset system.
- Adaptive lat/lon grid overlay (always on by default).
- Layer list in right panel with click-to-toggle visibility.
- Debug overlay: zoom, center coords, object/tile counts, cache stats.

**Not implemented**
- Any editing (node/way create, modify, delete, upload).
- Feature picking / tag inspection on click.
- Relation rendering (parsed, but unused).
- GeoJSON loading in the UI (code exists in `src/data.rs` but is dead).
- Layer reordering, style editing, search, export, undo/redo.

## Build & run

```bash
cargo run
```

### Prerequisites / gotchas

- **Metal Toolchain required.** GPUI compiles Metal shaders at build time. If you see `cannot execute tool 'metal' due to missing Metal Toolchain`, run:
  ```bash
  xcodebuild -downloadComponent MetalToolchain
  ```
- **Out-of-tree target dir.** `.cargo/config.toml` points `target-dir` to `~/.rust/osm-gpui/target` so build artifacts (~1 GB) stay out of the Dropbox/Synology-synced project folder. The `.cargo/` directory is gitignored because the path is user-specific. If cloning fresh on another machine, recreate it.
- `gpui` is pulled from the `zed-industries/zed` git repo, so the first build takes several minutes.

## Architecture map

Entry point is `src/main.rs` — `src/lib.rs` re-exports a small public API but the real UI lives in `main.rs`.

### Live modules

| Module | Purpose |
|---|---|
| `src/main.rs` | GPUI app entry. `MapViewer` component, menus, key bindings, event wiring, layer panel UI. Uses `OnceLock<Mutex<…>>` queues (`SHARED_OSM_DATA`, `LAYER_REQUESTS`) to hand file-dialog results back to the main thread. |
| `src/viewport.rs` | `Viewport` — pan/zoom state, mouse & scroll handling. Wraps `CoordinateTransform`. |
| `src/coordinates.rs` | `CoordinateTransform` (Web Mercator), `GeoBounds`, and `validate_coords` / `safe_point` helpers used to keep NaN out of Lyon paths. |
| `src/osm.rs` | OSM XML parser (`quick-xml`). Types: `OsmData`, `OsmNode`, `OsmWay`, `OsmRelation`, `OsmParser`, `OsmParseError`. |
| `src/tiles.rs` | Tile math only — `TileCoord`, `lat_lon_to_tile`, `get_tiles_for_bounds`. (Legacy `TileManager`/`Tile`/`TileLoadState` types in this file are stubs; ignore.) |
| `src/tile_cache.rs` | `TileAsset` implementing GPUI's `Asset` trait. Downloads PNGs with `ureq`, validates magic bytes, caches to `/tmp/osm-gpui-tiles/`, converts RGBA→BGRA for GPUI. |
| `src/layers/mod.rs` | `MapLayer` trait (`render_elements` for raster, `render_canvas` for vector paths, plus `name`/`is_visible`/`update`/`stats`) and `LayerManager`. |
| `src/layers/tile_layer.rs` | Raster tile layer — calculates visible tiles, emits `img()` elements via `window.use_asset::<TileAsset>`. |
| `src/layers/osm_layer.rs` | Vector OSM layer — nodes as absolutely-positioned divs, ways as `PathBuilder` + `window.paint_path`. Holds `Arc<OsmData>`. |
| `src/layers/grid_layer.rs` | Lat/lon grid. Spacing adapts to zoom (10° → 0.001°). |

### Dead code — do not extend without asking

These files compile but aren't wired into the app. Left over from refactors; candidates for deletion.

| Module | Why dead |
|---|---|
| `src/map.rs` | Old `MapView` component, fully replaced by `MapViewer` in `main.rs`. |
| `src/mercator.rs` | Duplicate Mercator math; `coordinates.rs` is canonical. |
| `src/background.rs` | Old tile renderer, references removed `TileManager` API. |
| `src/http_image_loader.rs` | Async `reqwest` downloader; superseded by `ureq` in `tile_cache.rs`. |
| `src/data.rs` | GeoJSON loader + `MapDataLoader` sample data; never called. |
| `examples/` | Empty/stale. The stale examples referenced in older docs don't exist. |

### Runtime flow

1. `main()` initializes `App`, registers `OpenOsmFile` / `Quit` actions, builds the menu bar.
2. `MapViewer::new` creates viewport (NYC, zoom 11), `LayerManager`, `TileCache`, and adds a `GridLayer` as the only default layer.
3. Each frame, `render` calls `check_for_new_osm_data()` / `check_for_layer_requests()` to drain the cross-thread queues, then `update_all()` on layers, then `render_all_elements()` (raster) followed by `render_all_canvas()` (vector) inside a GPUI canvas element.
4. **File > Open (⌘O)** → `rfd` dialog on a worker thread → parses XML → pushes `OsmData` into `SHARED_OSM_DATA` → next frame creates a new `OsmLayer`.
5. **Imagery > OpenStreetMap Carto** → pushes layer name into `LAYER_REQUESTS` → next frame constructs `TileLayer`.

### Key bindings

- ⌘O — Open OSM file
- ⌘Q — Quit

No other bindings are wired. (The old `map.rs` had `T`/`L`/`G`/`D`/`F`/`R` toggles; they are not in the current app.)

## Roadmap (realistic)

- Delete dead modules once confirmed unneeded.
- Feature picking / tag inspection panel.
- Render relations (multipolygons first).
- Begin editing primitives: select, move node, add node to way.
- Overpass API fetch for the current viewport.
- Persistent tile cache location (not `/tmp`).
