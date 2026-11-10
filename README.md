# osm-gpui

Experimental OpenStreetMap editor built on [GPUI](https://github.com/zed-industries/zed) (the framework behind Zed). The long-term goal is a JOSM-class editor that feels smoother and more native.

## Status (honest)

**Working**
- Navigation, clamped to zoom 1–20: trackpad two-finger scroll pans and pinch zooms; a mouse wheel zooms at the cursor (as does ⌘- or Ctrl-scroll on a trackpad); right-drag, middle-drag, or Space + left-drag pans; arrow keys pan and `+`/`-` zoom.
- Web Mercator projection (EPSG:3857) with lat clamped to ±85.051°.
- OSM XML loading via **File > Open** (⌘O). First loaded file auto-fits viewport.
- Data rendering tuned for tracing over imagery: thin colored ways with a dark casing (zoom ≥ 15), round outlined node markers (plain way vertices hidden below zoom 16), and direction arrows on one-way roads, roundabouts and flowing waterways (zoom ≥ 16). Hovering a feature in Select mode highlights what a click would select.
- Raster tiles via **Imagery** menu. Supports built-in OpenStreetMap Carto, plus custom imagery from the Editor Layer Index (ELI). Async download with `ureq`, PNG cached with size bounds and oldest-mtime eviction, loaded through GPUI's asset system. Tile attribution rendered on-screen.
- Adaptive lat/lon grid overlay (configurable via settings).
- Resizable right panel (drag its left edge; the width is remembered). Its layer list has an eye toggle per layer for visibility; clicking an OSM layer's row makes it the active layer that the drawing modes write into.
- Debug overlay: zoom, center coords, object/tile counts, cache stats.
- Feature selection: click a node or way to select it; Shift-click to toggle; rectangular box-select. Right panel shows feature type, OSM link, and all tags.
- **Editing primitives:** Node drag-to-move with per-feature updates (incremental rebuild). Tags are edited in place in the side panel's Tags table: click a key or value to edit it (Enter or clicking away commits, Esc abandons), "Add tag" appends a row. Create nodes (negative ID for new). Delete features. Edit multi-select to batch-apply tag changes.
- **Selection actions** (`src/selection_actions.rs`): right-click (a click, not a drag, which still pans) opens a context menu of the actions that apply to the selection; right-clicking an unselected feature selects it first. Square (iD-style orthogonalize), Circularize, Reverse Way, Join Ways, Separate Node, Extend Way, Split Way, Merge Nodes, Straighten Way, Copy/Paste Tags and Delete, each one undo step (Copy Tags only remembers tags).
- **Undo/redo stack** for moves, tag edits, creates, deletes. History panel. ⌘Z / ⌘Shift-Z.
- **OSM OAuth login** with keyring storage (macOS Keychain / Secret Service / Windows Credential Manager). Refresh-token support. Download OSM data for current viewport (respects 0.25-sq-deg limit); a download is merged by object id into the active layer, keeping local edits, so downloading again after a pan extends one dataset rather than stacking duplicate layers. Unsaved-changes warning on quit.
- **Upload to OSM** as a changeset (**File > Upload to OSM…**, ⌘⇧U), and **export to a file** (**File > Export…**, ⌘E): a JOSM-style `.osm` holding the whole layer with pending edits marked by `action` attributes and negative ids, so it can be reopened here or in JOSM with those edits still pending.
- Settings window: choose OSM API server (prod/dev/custom), per-server OAuth client IDs, custom imagery sources.
- **MapCSS-subset styling** (`node`/`way`/`area` selectors with `[k]`/`[k=v]`/`[k!=v]`; `color`, `width`, `symbol-size`, `fill-color`, `fill-opacity`). Zoom selectors, per-rule casing and dashes are not supported; the casing, vertex hiding and direction arrows above are fixed renderer behaviour.
- **Scripted screenshot harness** for visual regression and automation. `.osmscript` DSL with viewport, click, drag, key, load_osm, capture, wait_idle ops.

**Not implemented**
- Relation rendering or editing (parsed, but unused in display/selection).
- Text labels, POI icons.
- Search, full MapCSS (e.g., text, z-index, pattern fills).
- Resolving upload conflicts; saving back to the opened file (Export always asks where to write).

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

## Releases

The version lives in one place: `version` in `Cargo.toml`. The app reads it with `env!("CARGO_PKG_VERSION")` (`src/version.rs`) for the HTTP `User-Agent` (`osm-gpui/<version> (+https://github.com/iandees/osm-gpui)`) and the changeset `created_by` tag (`osm-gpui <version>`).

**Cutting a release**

1. Bump `version` in `Cargo.toml` (and let `Cargo.lock` update), commit, and merge to `main`.
2. Tag the commit and push the tag: `git tag vX.Y.Z` then `git push origin vX.Y.Z`.

`.github/workflows/release.yml` fails if the tag (without the `v`) differs from the `Cargo.toml` version. Otherwise it builds and attaches these files to a new GitHub Release with generated notes:

- `osm-gpui-X.Y.Z-macos-universal.zip` — `osm-gpui.app` (Apple Silicon + Intel)
- `osm-gpui-X.Y.Z-windows-x86_64.zip` — `osm-gpui.exe`
- `osm-gpui-X.Y.Z-linux-x86_64.tar.gz` — binary and a `.desktop` file
- `SHA256SUMS`

The same build jobs run (without publishing) on pull requests that touch the workflow, `scripts/`, `packaging/` or `Cargo.toml`, and on manual `workflow_dispatch` runs; the packages are uploaded as workflow artifacts.

**The macOS app is not notarized.** It is only ad-hoc signed (there is no Developer ID certificate), so Gatekeeper blocks the first launch of a downloaded copy. Right-click the app and choose Open, or run `xattr -dr com.apple.quarantine osm-gpui.app`.

**Building the `.app` locally**

```bash
cargo build --release
scripts/bundle-macos.sh        # writes dist/osm-gpui.app
```

Use `--binary PATH` to package a different executable (for example a `lipo`'d universal one) and `--out DIR` to change the output directory. If you use a custom target dir, set `CARGO_TARGET_DIR` so the script finds `release/osm-gpui`. There is no app icon in the repository yet, so the bundle has none.

## Architecture map

Entry point is `src/main.rs` — `src/lib.rs` re-exports a small public API but the real UI lives in `main.rs`.

### Live modules

| Module | Purpose |
|---|---|
| `src/main.rs` | GPUI app entry. `MapViewer` component, menus, key bindings, event wiring, layer & tag-edit & quit-confirm dialogs. Undo/redo queue, selection state, edit-context tracking. |
| `src/undo.rs` | `UndoStack`, `UndoableAction` enum (`MoveNodes`, `SetTags`, `CreateNode`, `DeleteFeature`), `NodeMoveTargets`, `MoveDrag` state machine for drag-to-completion. |
| `src/viewport.rs` | `Viewport` — pan/zoom state, mouse & scroll handling. Wraps `CoordinateTransform`. |
| `src/coordinates.rs` | `CoordinateTransform` (Web Mercator), `GeoBounds`, and `validate_coords` / `safe_point` helpers used to keep NaN out of Lyon paths. |
| `src/selection.rs` | `FeatureRef`, `Selection` (individual or multi-select across layers), hit-test logic, selection-update rules. |
| `src/osm.rs` | OSM XML parser (`quick-xml`). Types: `OsmData`, `OsmNode`, `OsmWay`, `OsmRelation`, `OsmParser`, `OsmParseError`. |
| `src/tiles.rs` | Tile math only — `TileCoord`, `lat_lon_to_tile`, `get_tiles_for_bounds`. |
| `src/tile_cache.rs` | `TileAsset` implementing GPUI's `Asset` trait. Downloads PNGs with `ureq`, validates magic bytes, caches with bounded eviction (oldest-mtime), converts RGBA→BGRA for GPUI. Concurrent fetches capped at 4 by a counting semaphore. |
| `src/osm_api.rs` | OSM API client: `/capabilities`, `/bbox/...` download, 30-second request timeout. |
| `src/auth.rs` | OAuth loopback browser flow (PKCE), bearer-token refresh logic, keyring + fallback-file storage with file permissions and atomic writes. |
| `src/imagery/mod.rs` | Editor Layer Index (ELI) fetch, parse, and cache. Per-imagery `ImageryEntry` with attribution. |
| `src/layers/mod.rs` | `MapLayer` trait (render, update, stats, modified-flag) and `LayerManager`. |
| `src/layers/tile_layer.rs` | Raster tile layer — calculates visible tiles, emits `img()` elements. |
| `src/layers/osm_layer.rs` | Vector OSM layer. Caches mercator coords, way vertices, style (color/width per feature). Hit-test via R-tree. Node/way paint using `paint_quad` + `paint_path` batching. |
| `src/layers/grid_layer.rs` | Lat/lon grid with zoom-adaptive spacing. |
| `src/style/mapcss.rs` | MapCSS-subset parser and matcher (node/way selectors, `[k]`/`[k=v]`, zoom ranges `|z12-`, color, width, casing, dashes, fill). |
| `src/nsi.rs` | NSI (Name Suggestion Index) integration (name/brand autocomplete — not yet wired to UI). |
| `src/custom_imagery_store.rs` | Persistent store for user-added imagery sources. |
| `src/settings_store.rs` | Persistent user settings (OSM API server choice, per-server OAuth client IDs). |
| `src/idle_tracker.rs` | `IdleTracker` — counts in-flight tile fetches. Powers `wait_idle` in the script harness. |
| `src/script/` | Line-DSL `.osmscript` parser and runner for scripted session capture. Ops: viewport, click, drag, key, load_osm, capture, wait_idle, etc. |
| `src/script_harness.rs` | Runner bridge from `script/` to the live `App` via the `AppHandle` trait. |
| `src/menu.rs` | Menu bar structure and action handlers (Open, Download, Settings, Help, About). |
| `src/ui/` | GPUI components for side panels (layers, tags, history), dialogs (tag-edit, quit-confirm, custom-imagery, settings), settings window. |
| `src/i18n.rs`, `locales/` | Localization: the `tr!` / `tr_count!` macros, locale selection, and the message catalogs (see Localization below). |

### Runtime flow

1. `main()` initializes `App`, registers `OpenOsmFile` / `Quit` actions, builds the menu bar.
2. `MapViewer::new` creates viewport (NYC, zoom 11), `LayerManager`, `TileCache`, and adds a `GridLayer` as the only default layer.
3. Each frame, `render` calls `check_for_new_osm_data()` / `check_for_layer_requests()` to drain the cross-thread queues, then `update_all()` on layers, then `render_all_elements()` (raster) followed by `render_all_canvas()` (vector) inside a GPUI canvas element.
4. **File > Open (⌘O)** → `rfd` dialog on a worker thread → parses XML → pushes `OsmData` into `SHARED_OSM_DATA` → next frame creates a new `OsmLayer`.
5. **Imagery > OpenStreetMap Carto** → pushes layer name into `LAYER_REQUESTS` → next frame constructs `TileLayer`.

### Key bindings

Defaults; all but the fixed window/app ones can be changed in **Settings > Keyboard Shortcuts** (the table lives in `src/keybindings.rs`).

- ⌘O open · ⌘⇧D download · ⌘⇧U upload · ⌘, settings · ⌘Q quit
- ⌘Z undo · ⌘⇧Z redo · ⌘A select all · Delete removes the selection · Esc cancels/deselects
- ⌘⇧C copy tags · ⌘⇧V paste tags (map focused, so they leave text fields alone; also in the **Edit** menu and the context menu). Copy remembers the tags of the one selected feature in an in-app tag clipboard (not the system pasteboard); Paste sets them on every selected feature of the active layer, overwriting values for the same keys and leaving other tags alone
- ⌘= / ⌘- zoom in/out · ⌘0 zoom to data · ⌘J zoom to selection
- `S` / `A` / `B` / `X` — Select / Add / Building / Extrude mode (map focused)
- Building mode: click three times (corner, corner, depth). With one squared closed way (e.g. a rectangular building) selected when you press `B`, new buildings align to it instead: click or drag from one corner to the opposite corner; Esc exits the mode
- `Q` square · `O` circularize · `R` reverse way · `C` join ways · `G` separate node · `E` extend way · `P` split way · `M` merge nodes · `T` straighten way — selection actions (map focused); also in the **Tools** menu and the right-click context menu
- Split (`P`): with one node selected that is inside a way (not its first or last node), or two nodes of a closed way, the way is cut there. The original keeps its id and the longer part; a new way gets the rest and a copy of all tags, and both share the split node. A node shared by several ways splits all of them, unless one way is selected too (then only that one). Ways that are relation members are refused, since relation edits aren't supported yet. Merge (`M`): two or more nodes become one: the server's node survives if exactly one is, else the first selected, at its own position; tags are unioned (differing values for a key refuse), ways use the survivor, repeats collapse, and the others are deleted (refused if that would leave a way with fewer than two distinct nodes, or deletes a relation member). Straighten (`T`): interior nodes of the selected open ways (3+ nodes) are projected onto the line between the first and last node; refused if any node would move more than 10% of the way's length. Reverse (`R`) also flips `:forward`/`:backward` and `:left`/`:right` in keys, `forward`/`backward`, `left`/`right`, `up`/`down` in `direction`/`incline`/`side` values, and the sign of a numeric `incline`; `oneway` is left alone
- Join (`C`): two or more ways that chain end to end become one (the server's way survives if exactly one is, else the first selected; tags are merged, and differing values for a key, a relation member that would be deleted, or a needed reversal of a `oneway` way refuse the join). Separate (`G`): with one node selected that several ways share, every way but the first gets its own copy; select a way as well to detach only that way. Extend (`E`): with an open way, or its first/last node, selected, switches to Add mode continuing that way from that end (Enter/Esc finish)
- Arrow keys pan a quarter of the view · `+` / `-` zoom a level · hold Space to pan with a left-drag (map focused; not customizable)
- ⌘M minimize · ⌘H hide · ⌥⌘H hide others

The status bar at the bottom of the window shows a hint for the current mode (or the latest status message / running operation), the unsaved-edit count, the signed-in account, the cursor's coordinates, and the zoom level.

## Localization

**Available languages:** English and German (Deutsch). The German catalog (`locales/de.yml`) is machine-drafted and has not yet been reviewed by a native speaker; corrections are welcome as pull requests or issues.

The app's own interface text (menus, toolbar, status bar, side panel, dialogs, Settings, shortcut names, error messages) lives in message catalogs under `locales/`, one YAML file per language named by its locale tag (`locales/en.yml`). They are compiled into the binary by [`rust-i18n`](https://crates.io/crates/rust-i18n), so there is nothing to install. English is the source language and the fallback: a key missing from another catalog shows its English text.

In code, use `tr!("menu.file.open")`, `tr!("status.exported", path = path.display())` for `%{path}` placeholders, and `tr_count!("status.pending_edits", n)` for plurals (the catalog has a `.one` / `.other` key per plural form; `i18n::plural_category` picks the form for a language). Keys are grouped by area (`menu.*`, `mode.*`, `status.*`, `panel.*`, `dialog.*`, `settings.*`, `shortcut.*`, `error.*`). Give translators whole sentences with named placeholders rather than gluing fragments together.

**Adding a language:** copy `locales/en.yml` to `locales/<tag>.yml` (for example `fr.yml` or `pt-BR.yml`), translate the values (keep the keys, the `%{placeholders}`, and the literal `{z}`/`{x}`/`{y}` in the custom-imagery messages), and set `meta.language_name` to the language's own name; that is what the Settings dropdown shows. Rebuild. If a language needs plural forms beyond `one`/`other`, add the category to `i18n::plural_category` and the matching keys to its catalog. `cargo test` checks that every key used in the source exists in `en.yml`.

**Choosing the language:** at startup the app uses the system locale (`fr-CA` falls back to `fr`, then to English). **Settings > Appearance > Language** overrides it (stored as `language` in `settings.json`; unset follows the system). The choice is read once at startup, so changing it takes effect after a restart. `--language <tag>` (for example `cargo run -- --language de`) overrides the stored setting for that run only and never writes to `settings.json`, which makes it handy for trying a language and for scripted screenshots.

**Preset and field labels:** the side panel's preset names ("Park", "Building"), field labels, field placeholders and option labels come from the bundled iD tagging schema ([`@openstreetmap/id-tagging-schema`](https://github.com/openstreetmap/id-tagging-schema), ISC licence) and follow the same language choice. Their translations are separate from the UI catalogs because upstream covers many more languages. They are `assets/presets/translations/<locale>.json` (about 270 KB each, embedded in the binary); only German (`de`) is bundled so far. The preset locale is resolved from the requested language on its own (`de-AT` falls back to `de`, then to English), and any string a translation lacks shows the schema's English text. The preset picker finds a preset by its translated name or search terms as well as its English name. **Adding a preset language:** run `cargo run --example update_preset_translations -- 6.19.2 <locale>` (use the schema version recorded in `assets/presets/LICENSE`; it trims upstream's `dist/translations/<locale>.min.json` to what the app uses), check the new file in, and rebuild; `build.rs` embeds every `*.json` in that directory. When the vendored schema is updated, regenerate the translations for the same version.

**Not translated yet:** the Name Suggestion Index data (brand names are proper names and stay as they are); tag keys and values (the Tags table shows the raw OSM tags); the debug overlay; layer names such as "OpenStreetMap Carto" and "Coordinate Grid"; log output and the script harness. Nothing sent to the OSM API or written to files (for example changeset tags) is localized.

## Scripted screenshots

Run a script of viewport/input/capture operations against the live app and produce PNGs. Useful for visual regression checks and LLM-driven testing where a headed browser test isn't available.

```bash
cargo run -- --script docs/screenshots/smoke.osmscript --window-size 1200x800
```

Flags:

- `--script <path>` — run a `.osmscript` file. Without this flag, the app launches normally.
- `--window-size WxH` — set the initial window size (default `1200x800`). Makes captures reproducible.
- `--keep-open` — don't exit after the last step, so you can poke at the final state.
- `--language <tag>` — use this UI language for the run instead of the saved setting (see Localization).

Script format is line-oriented with `#` comments:

```
window 1200 800
viewport 47.6062 -122.3321 12
wait_idle 10s
capture out/seattle.png

drag 600,400 300,400
wait_idle
capture out/panned.png

scroll 600,400 dy=-5
click 600,400
key cmd+o
wait 250ms
```

Ops: `window W H`, `viewport LAT LON ZOOM`, `wait_idle [TIMEOUT]`, `wait DURATION`, `drag X1,Y1 X2,Y2 [duration=Nms]`, `click X,Y [button=left|right]`, `scroll X,Y [dx=N] [dy=N]`, `hover X,Y`, `key CHORD` (e.g. `cmd+shift+a`), `load_osm PATH`, `capture PATH`, `log MSG`. Durations accept `Nms` or `Ns`.

`wait_idle` blocks until in-flight tile fetches drain (two consecutive idle frames), so captures don't show half-loaded maps. `capture` renders the current frame straight to a Metal texture and reads the pixels back in-process (via gpui's `Window::render_to_image`, enabled by the `test-support` feature) — no OS screenshot permission needed, and the window doesn't need focus or even to be on-screen.

`load_osm PATH` parses an OSM XML file and pushes it onto the dataset queue, the same pipeline used by **File > Open**. Follow it with `wait_idle` so the next frame creates the layer before subsequent clicks run.

Example script: `docs/screenshots/smoke.osmscript` exercises every op.

## Performance notes

Pan/zoom smoothness is sensitive to both build profile and render-path hygiene. A few things worth knowing before tuning:

### Always measure in `--release`

Debug builds are **~4× slower** on this workload. A representative Manhattan dataset (~3k objects) measured ~32 fps in `cargo run` and ~120 fps in `cargo run --release`. Never chase a perf problem in a debug build — reproduce it under `--release` first, or you'll optimize ghosts.

If debug feels too slow for day-to-day hacking, add a `[profile.dev]` `opt-level = 1` (or `2`) to `Cargo.toml` — it keeps debuginfo but turns on basic optimization.

### What the OSM layer does (and why)

`OsmLayer::render_canvas` is the hot path for large datasets. In rough order of impact, these are the tricks it uses — don't undo them without measuring:

1. **Per-node mercator cache** (`node_cache`). At `set_osm_data` time every node is projected once to Web Mercator meters. Per-frame projection to screen is then `(m - center) * pixels_per_meter` — two subs and a multiply, no trig. The hot loop never calls `lat_lon_to_mercator` (which is `tan`+`ln`).
2. **Per-way vertex lists** (`way_vertices`). Ways store `Vec<i64>` node refs; resolving those through `HashMap::get` per vertex, per frame, is cache-unfriendly. `way_vertices[i]` is a contiguous `Vec<(f64, f64)>` of mercator coords ready to iterate.
3. **Mercator-space bbox culling**. Every way has a cached mercator AABB; `viewport.mercator_view_bounds()` exposes the view AABB. An offscreen way is rejected with four `f64` compares before its vertex list is touched. There's also a layer-level AABB (`layer_bbox`) that short-circuits the whole `render_canvas` when the dataset is fully off-screen.
4. **One `paint_path` for all ways**. A single `PathBuilder::stroke` accumulates every visible way as subpaths (`move_to` starts a new one, `line_to` extends it). One build + one draw call per frame instead of N. When per-rule / MapCSS-style styling arrives, the same trick extends to one `paint_path` per `(stroke_width, color)` group.

### Gotchas we hit (and you might)

- **`paint_quad` vs `paint_path` for nodes.** Nodes render as individual `paint_quad` calls. Batching them into a single filled `PathBuilder` was tried and turned out *much* slower — Lyon's fill tessellator isn't built for thousands of tiny rectangles, and open sub-rect paths don't fill correctly without explicit closing. `paint_quad` is a direct GPU-quad primitive; prefer it for many uniform rects. `paint_path` is the right answer for strokes and complex fills (ways, polygons).
- **`CoordinateTransform::geo_to_screen` calls `lat_lon_to_mercator` twice per call** (once for the point, once for the center). The hot path uses `mercator_to_screen` instead, which is trig-free. Don't "simplify" by switching hot-loop calls back to `geo_to_screen`.
- **`Viewport`, `visible_bounds`, and `is_visible` operate in lat/lon.** They're fine for low-volume callers (hit-test, one-off queries). In per-vertex loops use `mercator_view_bounds` + explicit AABB compares instead.
- **Don't add per-frame allocations inside loops.** Builders and scratch vecs are allocated once per `render_canvas` call; keep it that way.

### If you need more performance

Levers we haven't pulled yet, in rough order of expected payoff:

- **Pixel-dedupe node dots at low zoom.** At z13 over Manhattan, thousands of nodes collide into the same pixels. A viewport-sized bitmap (or coarse grid) keyed by `(screen_x >> k, screen_y >> k)` would let you emit at most one `paint_quad` per small block with no visible change until zoomed in.
- **Sub-pixel way cull.** Skip ways whose mercator bbox projects to < 2 px. Lots of tiny buildings at low zoom contribute a single pixel but still walk the vertex list.
- **Persistent / reusable `PathBuilder` scratch buffers.** Minor; only if the allocator shows up in a profile.
- **Per-rule style batching.** When MapCSS-style rendering lands, group features by `(stroke_width, color)` / `(fill_color)` and keep the "one path per group" structure. Don't regress to one path per feature.

## Roadmap (realistic)

- Delete dead modules once confirmed unneeded.
- Feature picking / tag inspection panel.
- Render relations (multipolygons first).
- Begin editing primitives: select, move node, add node to way.
- Overpass API fetch for the current viewport.
- Persistent tile cache location (not `/tmp`).
