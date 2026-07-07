# osm-gpui Improvement Suggestions

Consolidated from a four-dimension code review (architecture, editing UX, rendering/UI, robustness) on 2026-07-06. Each item is scoped so an agent can implement it without re-doing the analysis. Line numbers reference the tree at commit `0b7536d`.

---

## Tier 1 — Core product gaps (the mapper's edit loop is broken at both ends)

### 1.1 No upload path — edits can never reach OSM
No `changeset`/`upload`/`osmChange` code exists anywhere in `src/`. `commit_node_moves` (`src/layers/osm_layer.rs:291`) only mutates the in-memory `OsmData` clone and sets `modified = true`; the File menu (`src/main.rs:2260-2267`) has only Open and Download. The OAuth flow requests `write_api` scope (`src/auth.rs:20`) that nothing uses.

**Implement:** an Upload action that (1) opens a changeset via `PUT /api/0.6/changeset/create` with `created_by` and a user-supplied `comment` tag, (2) builds an `osmChange` document from every layer where `is_modified()`, (3) `POST`s it to `/api/0.6/changeset/{id}/upload`, (4) closes the changeset. Reuse the bearer-token pattern from `check_for_download_requests` (`src/main.rs:1100-1102`). Include an upload dialog with a required changeset-comment field and a list of created/modified/deleted objects. Handle 409/410/412 conflict responses with a clear user-facing message (don't retry blindly — double-upload risk).

### 1.2 OSM objects carry no version field (prerequisite for 1.1)
`OsmNode`/`OsmWay`/`OsmRelation` (`src/osm.rs:16-35`) lack `version`, `changeset`, `timestamp`. The OSM API requires the current `version` on every modify/delete for optimistic locking.

**Implement:** add `version: i32` (plus `changeset: i64`, `timestamp: String` if convenient) to the three structs; parse them in the element handlers around `src/osm.rs:375-405`; carry them through `commit_node_moves`.

### 1.3 Tags are read-only
✅ Done — tag editing via dialog + side panel, EditTags undo variant, multi-select batch applies.

### 1.4 Generalize undo/redo before adding new edit types
✅ Done — UndoableAction generalized with CreateNode, DeleteFeature, SetTags variants; unified push sites; History panel.

### 1.5 No create/delete of features, no vertex-level way editing
✅ Done (partial) — Delete-key handler deletes selection; create-node mode via click+dragging or button. Vertex-level and split-way editing not yet implemented.

### 1.6 No unsaved-changes warning on quit
✅ Done — quit-confirm dialog checks is_modified() on layers; user can choose to quit or cancel.

### 1.7 Tile/imagery attribution is absent (legal requirement)
✅ Done — attribution parsed from ELI, stored in ImageryEntry, rendered as clickable bottom-right overlay on-screen.

---

## Tier 2 — Security & robustness

### 2.1 OAuth token stored world-readable in plaintext
✅ Done — tokens stored in platform keyring (Keychain/Secret Service/Credential Manager) with file (0600) fallback; refresh-token support.

### 2.2 OAuth `client_secret` hardcoded in a public repo
`src/auth.rs:18-19`, sent at `:155-164`. A PKCE loopback flow is a public client; the embedded secret protects nothing and is committed publicly.

**Implement:** re-register the OSM OAuth app as a public/PKCE client and drop `client_secret` from the token exchange — `code_verifier` already provides proof-of-possession.

### 2.3 No token expiry/refresh/401 handling
✅ Done — StoredToken stores refresh_token and expires_at; ensure_fresh_token() handles expiry and refresh and is called on the download path.

### 2.4 Missing request timeouts
✅ Done — 30-second timeouts on osm_api and auth ureq requests; fetch failures handled as retryable errors.

### 2.5 Tile disk cache grows without bound
✅ Done — cache bounded to 500 MB with oldest-mtime eviction; keyring by SHA-256(URL); stored in dirs::cache_dir().

### 2.6 Concurrent tile fetches are uncapped (violates OSM tile usage policy)
✅ Done — concurrent tile fetches capped at 4 via process-wide counting semaphore (Mutex + Condvar).

### 2.7 Non-atomic cache writes (tiles + ELI GeoJSON)
✅ Done — atomic temp-file-then-rename pattern for tile cache writes and ELI cache; ELI parse failures surfaced to UI.

### 2.8 Parser panics on non-UTF-8 input
~14 `std::str::from_utf8(...).unwrap()` calls in `src/osm.rs` (e.g. `:377-378, 404-405, 428-429, 448-449, 469-470, 489-490, 507-508`). One bad byte in a downloaded/opened `.osm` file kills the parse thread with no user-facing error.

**Implement:** helper `fn attr_str(b: &[u8]) -> Result<&str, OsmParseError>` mapping to the existing `OsmParseError::ParseError` variant; replace all unwraps.

### 2.9 OAuth loopback server: single-request, no path check, misleading denial error
`src/auth.rs:132-137` accepts exactly one request — a browser `favicon.ico` probe arriving first fails the login; `:141-153` reports user denial (`error=access_denied`) as "Login timed out".

**Implement:** loop on `recv_timeout` until a request whose path starts with `/callback` carries `code`/`state` (respond-and-discard others) up to the deadline; parse `error`/`error_description` into a distinct `AuthError::Denied`.

### 2.10 Narrow OAuth scope until upload exists
`SCOPES = "read_prefs write_api"` (`src/auth.rs:20`) requests write access nothing uses. Request only `read_prefs` now; add `write_api` when 1.1 lands. (Skip this if 1.1 is being implemented in the same milestone.)

---

## Tier 3 — Rendering quality & performance

> **Measured (2026-07-06):** `examples/perf_bench.rs` (run with `cargo run --release --example perf_bench`) benchmarks the CPU side of the render path on a synthetic downtown-scale dataset (106k nodes: 8k ways × 12 vertices + 10k POIs, 1600×1000 viewport). Headline numbers on this machine, release mode:
>
> | Path | Current | With proposed fix |
> |---|---|---|
> | Node paint loop (all visible, z14) | 2.69 ms/frame | 0.10 ms (cache per-node styles) |
> | Way paint loop (all visible, z14) | 3.93 ms/frame | 3.54 ms (cache styles), 2.68 ms (+1px decimation, −35% vertices) |
> | Way paint loop zoomed in (z17) | 0.17 ms | — (bbox culling already works well) |
> | Click hit-test | 1.90 ms | 0.012 ms (R-tree path, measured via `hit_test_rect`) |
> | `commit_node_moves` of ONE node | **52 ms** | sub-ms with incremental update (see 3.12) |
>
> So the OSM layer costs ~6.6 ms of CPU per frame while panning with everything visible (≈26 ms in a debug build at the ~4× factor, matching the known debug frame-time issue), and every drag commit or undo/redo step stalls ~52 ms. The commit path and node-style caching are the two highest-value fixes.

### 3.12 `commit_node_moves` does a full clone + rebuild — 52 ms hitch per edit and per undo/redo step
`commit_node_moves` (`src/layers/osm_layer.rs:291-305`) clones the entire `OsmData` (~28 ms for 106k nodes — every node, way, and tag `String`) then calls `set_osm_data`, which rebuilds the node cache, way tables, layer bbox, and **both R-trees** from scratch (~24 ms). This runs on every drag release and every undo/redo (`apply_undo_action` → `commit_node_moves`), and the cost scales linearly with dataset size — a city-scale download will stall noticeably.

**Implement:** an incremental path for node moves: (a) mutate only the affected `OsmNode`s (use `Arc::make_mut` on `OsmData` if uniquely held, or restructure so nodes live behind per-entry `Arc`s / the data isn't cloned wholesale); (b) update `node_cache.by_id`/`flat` entries for just the moved ids; (c) recompute bboxes/vertices only for ways containing a moved node (build a node→ways reverse index once at load); (d) update the R-trees with `remove`+`insert` for the affected entries instead of `bulk_load`ing all of them. Keep the full rebuild as the fallback for bulk operations.

### 3.1 Areas are never filled
`src/layers/osm_layer.rs:435-487` strokes every way as a polyline; buildings render as 1px gray outlines, water/landuse as bare lines. Largest visual gap vs iD/JOSM.

**Implement:** detect closed ways (first id == last, or area-implying tags: `building`, `landuse`, `natural`, `leisure`, `area=yes`); build a fill `Path` via `PathBuilder::fill` beneath the stroke. Requires `fill-color`/`fill-opacity` declarations and an `area` selector in `src/style/mapcss.rs` (the `Declaration` enum at `:96` currently has only `color`/`width`/`symbol-size`) plus stylesheet entries in `assets/default.mapcss`.

### 3.2 Nondeterministic way draw order (frame-to-frame z flicker)
`way_groups: HashMap<(u32,u32), WayGroup>` (`src/layers/osm_layer.rs:433`) is drained in randomized order at `:478`, so overlapping ways swap stacking between frames.

**Implement:** sort groups by a stable z key before painting (add `z-index` to MapCSS with sensible defaults: areas < casings < lines); short-term, a `BTreeMap` keyed by `(z, color, width)` suffices.

### 3.3 Click hit-testing is O(dataset) despite an existing R-tree — measured 1.9 ms vs 0.012 ms
✅ Done — hit_test uses R-tree queries for candidates, refines with cached mercator coords; geo_to_screen uses cached center.

### 3.4 Per-frame style re-resolution for every feature — measured 2.7 ms/frame for nodes alone
✅ Done — NodeStyle and WayStyle cached in node_cache and way_styles; invalidated on stylesheet change; per-node HashMap probe eliminated.

### 3.4b Sub-pixel segment decimation when zoomed out
✅ Done — decimate_segments() function emits segments only when they exceed 1 px screen length, reducing vertices 35%.

### 3.13 Tile layer: parent-fallback element built and asset-probed every frame for every tile
`render_elements` (`src/layers/tile_layer.rs:129-225`) rebuilds the full element tree per frame per visible tile, and *unconditionally* constructs the parent-fallback `img` (`:152-169`) with its own `use_asset` lookup — even when the child tile is already cached and fully covers it. That's 2× the asset-cache probes and double the element count for the steady-state (everything-loaded) case, and it keeps parent tiles pinned in the image cache. Each tile also pays 2 `geo_to_screen` calls (double trig each — see 3.3's center-caching fix).

**Implement:** probe the child asset first (`window.use_asset::<TileAsset>(&tile_url, cx)` returns `Option`); only attach the parent-fallback element when the child isn't ready yet. Project tile corners via `mercator_to_screen` with precomputed tile mercator bounds (tile edges are already mercator-aligned) instead of `geo_to_screen`.

### 3.5 Per-frame synchronous `read_dir` on the UI thread
`TileCache::stats()` (`src/tile_cache.rs:281-294`) counts the cache directory; called via `TileLayer::stats()` from `get_layer_stats` every frame (`src/main.rs:1562`).

**Implement:** only compute stats when `show_debug_overlay` is true, and cache the count with a ~1 s TTL or maintain an atomic counter updated on tile write.

### 3.6 Node markers are squares; ways lack joins/caps/AA
Nodes paint as hard-edged quads (`osm_layer.rs:513-520`); `push_segment_quad` (`:146-204`) emits disjoint per-segment quads with constant `st_position`, defeating GPUI edge AA and leaving notches at bends.

**Implement:** round node dots via `corner_radii(size/2)` (cheapest) or `PathBuilder::fill` circles; add a small cap quad at each vertex/endpoint for pseudo-round joins; optionally use the higher-quality Lyon stroke path for just the selected/hovered ways.

### 3.7 Selection styling: hardcoded magenta, square node ring, no halo, no hover
`SELECTION_ACCENT = 0xFF4081` (`osm_layer.rs:13`) draws a square outline for nodes (`:685-689`) and an uncased line for ways (`:717-723`); box-select uses a different color (`cx.theme().accent`, `main.rs:1704-1706`); no hover state exists.

**Implement:** two-pass halo (wide white/black casing under a themed accent stroke) so selection reads on any imagery; circular node ring matching the marker; single accent sourced from `cx.theme()`; hover highlight via hit-test in `handle_mouse_move` feeding a separate `set_hover` (cheap once 3.3 lands).

### 3.8 MapCSS subset is minimal
Only `node`/`way` selectors with `[k]`/`[k=v]`/`[k!=v]`, and `color`/`width`/`symbol-size`. Zoom selectors are parsed-and-discarded (`mapcss.rs:348-356`); no casing, dashes, text, icons, opacity, z-index, rgb()/rgba().

**Implement (priority order):** zoom-range selectors (`|z12-`) so road widths scale with zoom; `casing-width`/`casing-color`; `fill-*` (needed by 3.1); `dashes`. Text/icons come with 3.9.

### 3.9 No labels, POI icons, or one-way arrows
Render path draws only lines and dots; `name` tags, `oneway` chevrons, and amenity icons are invisible, making data hard to read.

**Implement:** a label pass using shaped text for `name` with simple collision-skip; one-way chevrons along the polyline (segment direction `nx,ny` is already computed in `push_segment_quad`); map common POI tags to glyph icons. Depends on MapCSS `text`/`icon-image` support (3.8).

### 3.10 Hi-DPI tiles
`tile_layer.rs:116-228` stretches 256px tiles across ~512 device pixels on Retina.

**Implement:** read `window.scale_factor()`; at ≥2 request one zoom level deeper for the same screen area (or `@2x` templates where ELI advertises them); size tile divs accordingly.

### 3.11 Map-area colors bypass the theme; tile pop-in is abrupt
Map background `rgb(0x1a202c)` (`main.rs:1567`), grid `rgb(0x374151)` (`grid_layer.rs:17`), tile boundaries (`tile_layer.rs:246`), full-tile crimson error blocks (`tile_layer.rs:207`) are all hardcoded; tiles appear with no fade.

**Implement:** source map background/grid/overlay colors from `cx.theme()`; fade tile opacity 0→1 on load; make the loading background match the map background; replace the crimson error tile with a small corner badge.

---

## Tier 4 — UX polish

### 4.1 Download UX: manual-only, failure-discovered limits, per-download layer spam
Download is Cmd-Shift-D only (`main.rs:2083`); the 0.25-sq-deg limit (`osm_api.rs:36-43`) is only discovered by triggering the error; every download spawns a new layer with "(2)", "(3)" suffixes (`main.rs:1114-1122`), so re-downloading after panning stacks duplicate objects — ambiguous for editing and future upload.

**Implement:** (a) merge fetches into one canonical editable OSM layer keyed by object id instead of a new layer per fetch; (b) grey/disable the Download action with a "Zoom in to download" hint when `check_area` would fail for `visible_bounds()`; (c) optionally auto-fetch on viewport settle above a zoom threshold, debounced via the existing `IdleTracker`; (d) optionally draw the downloaded bbox extents.

### 4.2 Selection modifiers and Escape-to-deselect
Click (`main.rs:849`) and box-select (`main.rs:827`) both replace the selection; no Shift-toggle. Escape only cancels a move-drag (`main.rs:1606-1610`).

**Implement:** when Shift is held (events carry `modifiers`), toggle hits in/out of `self.selected` instead of replacing; extend Escape: cancel drag if active, else clear selection.

### 4.3 Login state and pending-edit count invisible in main window
Login display lives only in Settings (`ui/settings_window.rs:311-343`); no aggregate modified count (only per-layer bullets, `main.rs:1450`).

**Implement:** a status strip or side-panel header showing "Logged in as X" / "Not logged in" plus total modified-object count — the natural home for the Upload button (1.1).

### 4.4 Transient toasts vs. in-progress operations
Status messages auto-expire after 5 s (`expire_status`, `main.rs:999-1005`) even while a download is still running; no spinner.

**Implement:** distinguish sticky "operation in progress" status (cleared on completion) from transient toasts; add a small activity spinner while downloading/uploading.

### 4.5 Trackpad/keyboard navigation
All vertical scroll zooms (`viewport.rs:66-76`); no pinch, no keyboard pan/zoom, no double-click zoom.

**Implement:** treat `ScrollDelta::Pixels` (trackpad) as pan and `Lines` (wheel) as zoom; arrow keys pan, `+`/`-` zoom on `focus_handle`; double-click = zoom-to-cursor (which is already correctly implemented in `coordinates.rs:zoom_at_point`).

### 4.6 Surface relation membership
Relations are parsed (`osm.rs:31`, `:107-158`) but invisible: `FeatureKind` has only `Node`/`Way` (`selection.rs:6-9`). Moving/deleting a multipolygon member silently breaks it.

**Implement (short-term):** show "member of relation N (type=...)" in the Selection/Tags panel for selected features; full relation rendering/editing is a later project.

---

## Tier 5 — Code health

### 5.1 Delete ~1,690 lines of dead code (5 files)
`src/map.rs`, `src/data.rs`, `src/background.rs`, `src/mercator.rs`, `src/http_image_loader.rs` are not declared as modules anywhere (`src/lib.rs:5-24`) and don't compile as part of the build; `map.rs`/`data.rs` reference obsolete GPUI APIs and hardcoded sample data. Deleting them also removes a second inconsistent rendering path and (with `data.rs`/`http_image_loader.rs` gone) potentially the `reqwest`/`tokio` dependency. Grep for stray references first.

### 5.2 Deduplicate `parse`/`parse_str` in `src/osm.rs`
`:59-209` and `:211-361` are ~150 near-identical lines. Extract `fn parse_events<R: BufRead>(reader: Reader<R>) -> Result<OsmData, OsmParseError>`; both entry points become one-liners.

### 5.3 Split up `main.rs` (2,295 lines)
✅ Done (partial) — UndoableAction/UndoStack/MoveDrag moved to src/undo.rs; menu.rs created; side_panel.rs created. Script-harness split partially (src/script_harness.rs exists).

### 5.4 Remove the `unsafe` pointer aliasing in `render`
`src/main.rs:1636-1639` uses `addr_of!` + `unsafe { &* }` to alias `self.layer_manager` inside the canvas closure. Restructure to clone `selected` first and capture plain immutable borrows of `layer_manager` and the viewport — the paint path only needs `&LayerManager` and `&[FeatureRef]`.

### 5.5 Extract `LayerManager::unique_name(&self, base: &str) -> String`
The suffix-numbering loop is copy-pasted at `main.rs:902-907`, `:944-949`, `:1114-1119`.

### 5.6 Replace `Arc<Mutex<Vec<()>>>` signal statics
`DOWNLOAD_REQUESTS`/`TOGGLE_DEBUG_OVERLAY`/`OPEN_CUSTOM_IMAGERY_DIALOG` use `Vec<()>` as counters with three structurally identical drain fns (`main.rs:1007-1082`). Replace with a small `Signal(AtomicUsize)` with `raise()`/`take_count()`.

### 5.7 Typed layer stats instead of parse-back strings
`get_layer_stats` (`main.rs:966-993`) string-matches `"Nodes"`/`"Ways"`/`"Cached Files"` and re-parses numbers every frame. Add `fn feature_counts(&self) -> FeatureCounts` to the `MapLayer` trait; keep `stats()` for display only.

### 5.8 `OsmMember.member_type: String` → enum
`src/osm.rs:39`; parse `OsmMemberType { Node, Way, Relation }` in `parse_member` (`:500-523`), erroring on unknown values.

### 5.9 Test the real `LayerManager`, not shadow copies
`src/layers/mod.rs:270-330` tests local `apply_move`/`apply_remove_at` re-implementations; the production `move_layer`/`remove_at` (`:126-142`) have zero coverage. Build a real `LayerManager` with a trivial test-only layer struct and delete the shadows. Also add tests for `on_move_layer` bounds math (`main.rs:596-603`).

### 5.10 Surface user-action failures instead of swallowing them
Pervasive `if let Ok(guard) = x.lock()` with no else (`main.rs:607-611, 1051-1059, 2096-2126`); `open_osm_file` parse failure is stderr-only (`main.rs:2034`); settings/token save failures (`auth.rs:328`, `settings_store.rs:126`) never reach the UI. Route user-triggered failures to `set_status(...)`; consider `parking_lot::Mutex` (no poisoning) for the queues.

### 5.11 Extract and test zoom-fit math
`fit_to_osm_data` (`main.rs:527-582`) inlines `40075016.686`, tile size, margin, and zoom clamps that duplicate constants in `tiles.rs`/`coordinates.rs`. Extract pure `zoom_to_fit(bbox, screen_size, margin)` and `bbox_of(nodes)` into `coordinates.rs` with unit tests.

### 5.12 Misc small fixes
- Unify User-Agent: `tile_cache.rs:208`, `http_image_loader.rs:8`, `imagery/mod.rs:120` hardcode `osm-gpui/0.1.0`; use one `const` built from `CARGO_PKG_VERSION` (as `osm_api.rs`/`auth.rs` already do).
- `check_area` (`osm_api.rs:36-43`) uses signed spans; normalize/assert `min <= max` so inverted bounds can't pass.
- `IdleTracker` (`idle_tracker.rs:23,32`) uses `debug_assert!`; in release an underflow wraps and permanently wedges `is_idle()` — use `saturating_sub` + warning.
- `side_panel_open: Vec<usize>` as a hand-rolled set (`main.rs:271, 1250-1253, 1361-1367`) → `[bool; 4]` indexed by a `PanelSection` enum.
- `OsmParser` is a stateless unit struct with `&self` methods (`osm.rs:52-57`) → associated functions.

---

## Suggested sequencing

1. **5.1** (delete dead code) — cheap, unblocks clean diffs for everything else.
2. **1.2 → 1.4 → 1.3 → 1.5 → 1.6 → 1.1** — the edit-loop arc: versions, generalized undo, tag editing, create/delete, quit guard, upload. (2.1–2.3 land alongside 1.1 since upload makes token security matter more.)
3. **1.7, 2.4–2.9** — legal + robustness, all small and independent (good parallel-agent fodder).
4. **3.12, 3.4, 3.3** — the measured perf wins (incremental commit path, cached styles, indexed hit-test), then **3.1–3.2** for the visual wins (fills, z-order).
5. Everything else as capacity allows; Tier 5 items are independent and safely parallelizable.
