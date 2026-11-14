# osm-gpui Improvement Suggestions

Consolidated from a four-dimension code review (architecture, editing UX, rendering/UI, robustness) on 2026-07-06 and brought up to date against the merged PRs on 2026-11-13 (through PR #288). Each remaining item is scoped so an agent can implement it without re-doing the analysis. Done items are collapsed to one line with the PR that shipped them; file references for remaining items point at the current tree (line numbers are omitted because they drift quickly — grep for the named functions).

---

## Tier 1 — Core product gaps

### 1.1 No upload path — edits can never reach OSM
✅ Done — changeset upload shipped in #63 (upload dialog with comment, osmChange diff, close changeset). Hardened since: sign-in prompt when signed out (#98), no-retry on unknown outcome (#132), large edits split into several changesets (#136, #140), typed conflict errors with keep mine / use theirs (#128), server size limits enforced (#124), follow-ups (#145), keychain wait message (#151), sources/imagery/review-request fields (#178), disabled when nothing to upload (#222).

### 1.2 OSM objects carry no version field
✅ Done — version tracking added to the parser and carried through edits and upload (#54).

### 1.3 Tags are read-only
✅ Done — tag editing via dialog + side panel, EditTags undo variant, multi-select batch applies (#48, #94).

### 1.4 Generalize undo/redo before adding new edit types
✅ Done — UndoableAction generalized with CreateNode, DeleteFeature, SetTags variants; unified push sites; History panel.

### 1.5 No create/delete of features, no vertex-level way editing
✅ Done — node create/delete with undo (#61), Add mode with snapping onto ways and way extension (#68, #78), vertex delete with cleanup of orphaned nodes and too-short ways (#196), split way (#107, #208), join/unjoin node (#209, #211), drag vertices (#174).

### 1.6 No unsaved-changes warning on quit
✅ Done — quit-confirm dialog (#60), Cmd+Q fix (#74), confirmation before deleting a layer with unsaved edits (#152).

### 1.7 Tile/imagery attribution is absent (legal requirement)
✅ Done — attribution overlay (#55), clickable links (#59), smaller corner placement (#189).

---

## Tier 2 — Security & robustness

### 2.1 OAuth token stored world-readable in plaintext
✅ Done — platform keyring with 0600 file fallback; refresh-token support (#56).

### 2.2 OAuth `client_secret` hardcoded in a public repo
✅ Done — `client_secret` dropped from the token exchange (PKCE public client) (#56); per-server client IDs (#65). The comment at the top of `src/auth.rs` notes the OSM OAuth app must be registered as non-confidential.

### 2.3 No token expiry/refresh/401 handling
✅ Done — `StoredToken` stores `refresh_token`/`expires_at`; `ensure_fresh_token()` (#56).

### 2.4 Missing request timeouts
✅ Done — 30-second timeouts and retryable fetch errors (#58).

### 2.5 Tile disk cache grows without bound
✅ Done — 500 MB bound with oldest-mtime eviction, SHA-256 keys, `dirs::cache_dir()` (#53); cache management panel (#86).

### 2.6 Concurrent tile fetches are uncapped
✅ Done — capped at 4 via a process-wide semaphore (#53, #58).

### 2.7 Non-atomic cache writes
✅ Done — temp-file-then-rename for tile and ELI caches (#53, #58).

### 2.8 Parser panics on non-UTF-8 input
✅ Done — bytes-to-str conversion returns `OsmParseError` instead of unwrapping (#54).

### 2.9 OAuth loopback server: single-request, no path check, misleading denial error
✅ Done — callback loop ignores stray requests and denial is reported as `AuthError::Denied` (#56).

### 2.10 Narrow OAuth scope until upload exists
✅ Moot — upload shipped (#63), so `write_api` is used; scopes are now `read_prefs write_api write_notes` (notes, #179).

---

## Tier 3 — Rendering quality & performance

> **Measured (2026-07-06):** `examples/perf_bench.rs` benchmarked the CPU side of the render path on a synthetic downtown-scale dataset. The headline fixes (cached styles, R-tree hit-test, incremental commit, segment decimation) all landed in #57; a per-frame render-cost instrumentation and report followed in #148 and a way-drawing speedup in #157 (see `docs/perf/`).

### 3.12 `commit_node_moves` does a full clone + rebuild
✅ Done — incremental node-move commit, no whole-dataset clone/rebuild (#57).

### 3.1 Areas are never filled
✅ Done — MapCSS `fill-color`/`fill-opacity` with an `area` selector and `assets/default.mapcss` entries (#70); area presets for closed ways (#131).

### 3.2 Nondeterministic way draw order (frame-to-frame z flicker)
✅ Done — MapCSS `z-index` plus a stable layered draw order (area fills < casings < lines per `z-index` layer, id-sorted within a layer) (#225). See `z_key` and the batch ordering in `src/layers/osm_layer.rs`.

### 3.3 Click hit-testing is O(dataset)
✅ Done — R-tree candidate query plus refinement (#57).

### 3.4 Per-frame style re-resolution
✅ Done — cached `NodeStyle`/`WayStyle` (#57).

### 3.4b Sub-pixel segment decimation when zoomed out
✅ Done — `decimate_segments()` (#57).

### 3.13 Tile layer: parent-fallback element built and asset-probed every frame
✅ Done — the child asset is probed first and the parent fallback is only built while it is not ready; tile corners use mercator bounds (#224).

### 3.5 Per-frame synchronous `read_dir` on the UI thread
✅ Done — the cached-file count is recomputed at most once per second (`cached_file_count` / `STATS_TTL` in `src/tile_cache.rs`). (Stats are still gathered every frame by `get_layer_stats`, but that is now a cheap read.)

### 3.6 Node markers are squares; ways lack joins/caps/AA
✅ Done — round nodes and thin cased ways (#93), line joins and end caps (#169), untagged vertices drawn only when selected / at high zoom (#149, #158), way vertices shown on hover (#174).

### 3.7 Selection styling: hardcoded magenta, square node ring, no halo, no hover
✅ Done (partial) — two-pass halo, round markers and hover highlight shipped (#93), hover drawn above selection (#217). Remaining: the accent is still the `SELECTION_ACCENT` constant in `src/layers/osm_layer.rs` rather than sourced from `cx.theme()`, and box-select still uses a different color.

### 3.8 MapCSS subset is minimal
✅ Done — #225 added zoom-range selectors (`way|z12-16`, tested against `floor(zoom)`; ways use a per-signature zoom table, see the "Zoom and caching" header of `src/style/mapcss.rs`), `casing-width`/`casing-color`, `z-index`, `dashes`, `opacity`, `rgb()`/`rgba()` (alpha becomes opacity), and parsed label properties (`text`, `font-size`, `text-color`, `text-halo-color`/`-radius`, `text-offset`, `text-position`, `icon-image`, `icon-width`). Still unsupported: other pseudo-classes, `eval()`, and any `label-priority`-style property (see 3.14). Node *stroke/marker* styles are still resolved once at `NODE_STYLE_ZOOM = 18.0` (`src/layers/osm_layer.rs`), so a zoom selector on a node only affects its marker at zoom 18; node *labels and icons* do resolve at the real zoom (`resolve_node_label`).

### 3.9 No labels, POI icons, or one-way arrows
✅ Done (mostly) — shipped in pieces:
- Point labels and preset POI icons (`src/layers/labels.rs`): uniform-grid collision index, greedy priority placement with several anchors, placement recomputed when the camera settles (120 ms) with pans just re-projecting, hysteresis so winners stay put, text halo, MapCSS wiring (`text`, `icon-image`, `text-position`, ...) (#227).
- Paint cost and polish: halo modes, translucent backing quad beyond 40 text labels, labels fade in and survive a one-level zoom change, halo alpha (#228).
- Area labels at the pole of inaccessibility (`src/polylabel.rs`, `src/layers/area_labels.rs`), shown only where the text fits (#229).
- Street names along ways with rotated glyphs (`src/layers/line_labels.rs`), `text-offset`, coarse halo (#226).
- One-way arrows already existed before this work: triangles every 96 px of path from z16 (`ARROW_SPACING_PX`, `ARROW_MIN_ZOOM` in `src/layers/osm_layer.rs`, #93).

Honest remainder (each is scoped under 3.14):
- Arrows are fixed triangles; there is no chevron style and no MapCSS toggle (`Direction` is derived from tags in `src/style/mapcss.rs`, not from a property).
- Area labels are one line only: no two-line wrapping, no italics.
- Street names lose to every point label and icon (point items are passed to line labels as obstacles), and the 45-degree total-turn rule (`MAX_TOTAL_TURN_DEG`) skips long names on tight arcs.
- No `label-priority` MapCSS property; priority is fixed in code (`node_priority`, area size bonus).
- No settings toggle for labels or icons.
- Collision is per layer (each `OsmLayer` owns its own `LabelCache` and line-label state), so labels from two layers can overlap.
- Halo cost: the outline halo (4 or 8 glyph repaints) is used only up to `OUTLINE_MAX_LABELS = 40` text labels; beyond that a translucent backing quad is used. Paint was measured at about 1.27 ms for 300 labels against a 0.8 ms goal (release build). GPU frame time at 300+ labels was never cleanly re-measured on an unloaded machine, and the dark-imagery legibility case was only checked with a stand-in.
- Street names do not use `labels::HaloMode`; they use their own coarse stroked-outline halo (`line_labels::HaloMode::StrokeCoarse`).
- GPUI cannot rotate text (`paint_glyph` has an identity transform), so street names are triangulated glyph outlines drawn with the bundled IBM Plex Sans SemiBold (`assets/fonts`, SIL OFL), not the UI font.

### 3.10 Hi-DPI tiles
✅ Done — `hidpi_tile_zoom()` requests z+1 tiles at scale factor >= 1.5 (stretching when z+1 exceeds the source's max zoom) (#224).

### 3.11 Map-area colors bypass the theme; tile pop-in is abrupt
Still open (partially mitigated by the theme work in #72, #143, #155). The coordinate grid layer (and its hardcoded grid color) has since been removed. Hardcoded colors remain: tile placeholder `0x2d3748` and the full-tile crimson error block `0x9f1239` in `src/layers/tile_layer.rs`; tiles still appear with no fade.

**Implement:** source map background/overlay colors from `cx.theme()`; fade tile opacity 0→1; replace the crimson error tile with a corner badge.

### 3.14 Label rendering follow-ups
Concrete next steps after the label work (#226-#229). Each was re-checked against the code; grep for the named symbols.

1. **`label-priority` property.** Add `label-priority` to the MapCSS parser (`src/style/mapcss.rs`, `TextStyle`) and use it in `labels::node_priority` and the area ranking in `osm_layer.rs` (`rank` closure near `area_labels::size_bonus`) instead of the hardcoded tiers. Also define priority between street names and icons: today `line_labels::LineLabelCache::place` takes point-label rectangles as `obstacles`, so a street name always loses to any point label or icon. Option: give line labels an `Item` priority in the same `labels::place` pass.
2. **Area labels: wrapping and italics.** `area_labels::locate` fits a single line (see its module doc, "Limits"). Add two-line wrapping at a space (try 1 then 2 lines in `font_scales`/fit loop) and an italic option for natural/water classes (needs an italic face or a skew; the UI font via `ShapedLine` has no italic hook).
3. **Settings toggles for labels and icons.** Nothing in `src/settings_store.rs` mentions labels. Add booleans (labels, POI icons, street names, area labels), pass them into the `OsmLayer` render (they gate `LABEL_MIN_ZOOM`/`ICON_MIN_ZOOM` paths and `line_labels::bench_knobs().labels`), and add a View menu item plus a settings row.
4. **Cross-layer collision.** Each `OsmLayer` has its own `label_cache` and `line_placed`. Share one `CollisionGrid` across layers (owned by `LayerManager`, filled in layer z-order) so two downloaded layers do not overprint.
5. **Text size by zoom or road class.** Font size is a fixed `font-size` per rule (area labels only shrink one step to fit). Add per-zoom `font-size` rules in `assets/default.mapcss` (the zoom selectors now allow it, e.g. `way|z18-[highway=primary]`), or a scale curve in `resolve_node_label`/`line_labels` `LabelStyle`.
6. **Street-name halo through `HaloMode`.** `line_labels` has its own `HaloMode` enum and ignores `OSM_GPUI_LABEL_HALO` and the 40-label budget in `labels::halo_for`. Unify, or document why the stroked outline is kept.
7. **Chevron and one-way styling via MapCSS.** Arrow geometry and spacing are constants in `osm_layer.rs` (`ARROW_*`). Add properties (e.g. `direction-style: arrow|chevron|none`, `direction-spacing`) read from `WayStyle`, and a chevron shape next to `arrow_triangle`.
8. **Per-class offset for wide roads.** `line_labels::clear_offset` already pushes text off wide strokes, but `default.mapcss` uses one `text-offset: 6.2` for every highway. Tune per class (motorway/trunk vs residential) in the stylesheet.
9. **Zoom selectors on nodes.** Node marker styles are cached once at `NODE_STYLE_ZOOM = 18.0`. Apply the per-signature `zoom_styles` approach used for ways to `NodeStyle` so `node|z16-` marker rules work at the real zoom.
10. **Measure GPU frame time on a clean 60/120 Hz machine** with 300+ labels (script: `docs/perf/line-labels-gpu.osmscript`, plus `docs/perf/render-benchmark.osmscript`), and re-decide `OUTLINE_MAX_LABELS` and the quad fallback from real numbers (goal 0.8 ms paint; last measured about 1.27 ms for 300 labels).
11. **Satellite-imagery legibility.** Only a stand-in background was used to judge halo contrast on dark imagery; check real Maxar/Esri tiles and adjust halo alpha/radius defaults in `assets/default.mapcss`.

Note: labels already fade in (point, area and street labels share `labels::fade_alpha`), so no fade work is needed for line labels.

---

## Tier 4 — UX polish

### 4.1 Download UX: manual-only, failure-discovered limits, per-download layer spam
✅ Done (partial) — downloads merge into the active layer keeping local edits (#95); the area limit comes from the server's capabilities (#121, #122) with a "zoom in to download" hint (#97); progress shown (#119); downloaded bounds drawn with a Show Downloaded Area toggle (#120, #126). Remaining: no auto-fetch on viewport settle (option (c): debounce via `IdleTracker`).

### 4.2 Selection modifiers and Escape-to-deselect
✅ Done — shift+click toggle (#66); Escape cancels drags/modes and a Deselect All action exists.

### 4.3 Login state and pending-edit count invisible in main window
✅ Done — status bar shows the signed-in account and the pending-edit count (#91).

### 4.4 Transient toasts vs. in-progress operations
✅ Done — running download/upload shown with a spinner in the status bar, separate from transient messages (#119).

### 4.5 Trackpad/keyboard navigation
✅ Done (partial) — trackpad pan, pinch zoom, keyboard navigation (#92); scroll direction setting (#166, #170). Remaining: double-click on empty map does not zoom-to-cursor (double-click is used for selecting closed-way interiors and inserting a node on a segment).

### 4.6 Surface relation membership
✅ Done — relations are parsed, selected, edited and shown in the side panel (member-of rows, roles, Add to Relation) (#110, #111, #112, #114), drawn on the map (#139, #144), and sortable (#213).

---

## Tier 5 — Code health

### 5.1 Delete ~1,690 lines of dead code
✅ Done — dead modules removed (#49).

### 5.2 Deduplicate `parse`/`parse_str` in `src/osm.rs`
✅ Done — parsing unified (#54).

### 5.3 Split up `main.rs`
✅ Done — `undo.rs`, `menu.rs`, `side_panel.rs`, `script_harness.rs` etc. were split out (#49, #69); the in-file test modules moved to per-area files under `src/viewer_tests/` (#270); mode code moved to `src/modes/` with one module per edit mode (#262, #288); upload driving moved to a shared `UploadDriver` (#277). `src/main.rs` is down from ~14,900 to 7,684 lines (`wc -l`). It is still the largest file (the `MapViewer` impl); see "Still open from the architecture review" for the field grouping.

### 5.4 Remove the `unsafe` pointer aliasing in `render`
✅ Done — no `addr_of!`/`unsafe` aliasing remains in `main.rs` (#69).

### 5.5 Extract `LayerManager::unique_name`
✅ Done — `LayerManager::unique_name` exists and is used at all call sites (#69).

### 5.6 Replace `Arc<Mutex<Vec<()>>>` signal statics
✅ Done — the statics were removed in the event-driven refactor (#69).

### 5.7 Typed layer stats instead of parse-back strings
✅ Done — `LayerManager::layer_stats` returns a typed `LayerStats` (`cached_files`, `osm_nodes`, `osm_ways`) and `get_layer_stats` in `src/main.rs` destructures it; no string matching remains (#267, with `MapLayer` shrunk to typed accessors).

### 5.8 `OsmMember.member_type: String` → enum
✅ Done — `MemberType` enum parsed in `parse_member` (#54).

### 5.9 Test the real `LayerManager`, not shadow copies
✅ Done (partial) — tests now build a real `LayerManager` and call production `move_layer`/`remove_at` (`src/layers/mod.rs` tests), and the viewer tests build real layers from one shared fixture (#266, #270). Remaining: the bounds check in `on_move_layer` (`src/main.rs`) still has no direct test.

### 5.10 Surface user-action failures instead of swallowing them
✅ Done (partial) — upload, keychain, restore and parse failures now route to status/notice dialogs (#98, #140, #151, #161). Since then: refused edit primitives return `EditError` instead of being ignored (#272); an upload with an unknown outcome asks before re-uploading (#265); unreadable settings files are backed up and unknown or missing fields tolerated (#261); script ops fail when nothing handles an action (#276); the settings and token save failures in `src/settings_store.rs` and `src/auth.rs` are logged to stderr rather than dropped. Remaining: those save failures are not shown to the user, and the `if let Ok(guard) = x.lock()` sites (the ELI index in `src/main.rs`, `src/nsi.rs`, `src/tile_cache.rs` error log, `label_pass.rs`, `scene.rs`) still ignore a poisoned lock.

### 5.11 Extract and test zoom-fit math
✅ Done — `fit_bounds_to_viewport` with unit tests in `src/coordinates.rs` (#69). (`fit_to_osm_data` in `main.rs` still computes the bbox inline but delegates the zoom math.)

### 5.12 Misc small fixes
- User-Agent: ✅ Done — single `USER_AGENT` const built from `CARGO_PKG_VERSION` in `src/version.rs` (#105).
- `check_area`: ✅ Done — now delegates to `capabilities::allows_download` using the server's limits (#121).
- `IdleTracker` (`src/idle_tracker.rs`): ✅ Done — `tile_fetch_finished`/`image_decode_finished` decrement saturating at zero, keep the `debug_assert!`, and warn once on stderr in release when an underflow is prevented.
- `side_panel_open`: ✅ Done as a fixed-size `[bool; 8]` set; an enum-indexed wrapper would still be tidier (low priority).
- `OsmParser` (`src/osm.rs`): still open — a stateless unit struct with `&self` methods (callers use `OsmParser::new().parse_str(..)`); convert to associated functions (low priority).
- Date arithmetic: ✅ Done — one `src/civil_date.rs` replaces the three copies of the days/civil conversion in `notes.rs`, `object_history.rs` and `street_imagery.rs`.
- `OsmLayer` constructors: ✅ Done — `new_styled`, `new_with_data_styled` and `new_with_edits_styled` take the stylesheet; the app passes `style::custom::active_stylesheet()`, and the plain `new`/`new_with_data`/`new_with_edits` use the built-in sheet for tests, examples and benches.

---

## Shipped since this review

Major capabilities merged that were not in the original list:

- **Releases and packaging:** release workflow, macOS app bundle, version-aware User-Agent (#105); MIT license (#127); Linux Wayland/X11 backends (#113); releases 0.2.0 (#109), 0.3.0 (#123), 0.4.0 (#141), 0.5.0 (#156), 0.6.0 (#168), 0.7.0 (#182), 0.8.0 (#215).
- **Platform and settings:** gpui-component migration (#41), settings window and custom imagery (#40, #133), keyboard shortcut settings (#87), text size setting (#85), System/Light/Dark appearance (#155, #164, #188), in-window menu bar and Ctrl shortcuts on Windows/Linux (#153, #154, #172), status bar and fuller menus (#91), command-line/OS file opening (#134), localization with German (#103, #104, #106, #108).
- **Imagery:** Editor Layer Index (#19), custom imagery (#39), ELI zoom limits (#33), imagery offset with an adjust mode (#159), tile seam fixes (#51, #62, #100), open on OSM Carto with a welcome card (#116), welcome card controls (#163).
- **Tag editing and presets:** tag editor dialog (#48), iD presets with field-based editor (#71), NSI brand presets (#52), preset translations (#106), area presets (#131), recents (#142), bundled preset icons (#137), preset field inheritance (#191), taginfo/schema/loaded-data autocomplete (#192) and related-key suggestions (#216, #219), change-type pencil button (#190).
- **Editing modes and tools:** Select/Add/Building/Extrude modes (#68, #76, #89, #133), squaring/circularize/reverse/delete selection actions (#101), join/separate/extend (#102), split, merge nodes, straighten, copy/paste tags (#107), align new buildings to a squared way (#99), align and distribute nodes (#176), create parallel way (#171), copy/paste/duplicate features (#175), simplify way (#212), rotate (#218), purge (#210), unjoin node and delete way segment (#211), join node to way with tag-aware snapping (#209, #221), multi-node split (#208), find by query (#162), direction-aware reverse (#194, #201, #203).
- **Relations and multipolygons:** relation-aware editing, create/delete and editor (#110, #111, #112, #114), multipolygon and route/restriction rendering (#139, #144), Create Multipolygon (#207), Sort members (#213).
- **Data layers and reference data:** GPX and GeoJSON reference layers (#180), OSM notes show/read/comment/close/reopen/create (#179, #184), object history view with revert tags (#177), File > Export to .osm (#96), merge downloads into active layer (#95), downloaded-area display (#120, #126).
- **Safety and quality:** validator with Tools > Validate before upload (#160), autosave and crash restore (#161), persistence policy guarding real state in tests (#173), upload hardening and chunking (#124, #128, #132, #136, #140, #145, #206), edge-case test catalogue for editing operations (#193-#205).
- **Rendering, labels and MapCSS:** Hi-DPI tile requests (#224), MapCSS zoom ranges, casing, `z-index` and stable draw order, dashes, opacity, `rgba()` and parsed label properties (#225), point labels and preset POI icons with collision placement (#227), label paint cost, halo modes, zoom fade (#228), area labels at the pole of inaccessibility (#229), street names along ways with rotated glyphs (#226).
- **Tooling:** scriptable screenshot/osmscript harness (#3, #64, #75, #147, #214), render-cost measurement and way-drawing speedups (#148, #157, docs/perf), live dev-server upload tests (`docs/live-test`).

Merged between #229 and #288:

- **Releases and packaging:** releases 0.9.0 (#231), 0.10.0 (#244), 0.11.0 (#254); cheaper GitHub Actions with a quick Linux PR check and release only on tags (#241); README trimmed of stale claims (#240).
- **Map and imagery:** remembered map position and window bounds (#236), zoom to z23 with tiles fetched to each source's own max zoom (#237), custom MapCSS styles from a file or URL and refined Wireframe/iD styles (#238, #239), standard-zoom OSM Carto tiles on high-DPI displays (#247), tile downloads cancelled for tiles that scroll off the map (#248).
- **Editing modes and tools:** Arc mode (#242) with snapping onto ways (#250), Improve Way mode (#251), Extract Point from a closed way (#255), double-click node insertion in Extrude mode on the segment (#249), preset picker closes on an outside click (#252).
- **Street imagery and traces:** Mapillary and Fotoshi street imagery layers with a zoomable photo viewer (#258), Panoramax, street imagery from vector tiles and an opt-in Fotoshi lookup for selected features (#259), public GPS traces from OSM (#257).
- **Upload safety:** an upload-in-progress marker recorded before each changeset is sent, with a warning on restore and in the upload dialog (#284); a confirmation before re-uploading after an unknown outcome (#265).
- **Fixes:** deleted relations are written without tags (#279), purged relation indices recorded before removal and `set_base_version` no longer bumps the revision on a miss (#280), street-name label layouts keyed on vertex order (#264), stale side-panel Relations and untagged-vertex clicks fixed in six UI scripts (#281).

---

## Architecture review, 2026-11

A structural review of the code base, implemented as the PRs below (all merged unless noted). One line each.

- ✅ **One `EditMode`:** a single shared enum with per-mode facts, one `reset_mode_progress`, and a shortcut-binding coverage test (#262).
- ✅ **`ModeState`:** edit-mode state held in one enum, with pointer, key and preview handling dispatched through per-mode modules in `src/modes/` (#288).
- ✅ **One modal list and shared dialog shell:** focus, render, any-dialog-open and the Find guard derive from one list of open modals (#260); every dialog is built on the shared scrim, card and key classifier (#278).
- ✅ **Settings:** all writes go through one update function, unknown or missing fields are tolerated, unreadable files are backed up, and the Settings window honors `--api-server` (#261); validators, page list and login state extracted and tested (#285).
- ✅ **Redo for drawing actions:** redo recreates drawn buildings, arcs, extrusions and Add-mode clicks with their original ids (#263).
- ✅ **`MapLayer` shrunk:** only the universal methods remain, with layer-specific behavior behind typed accessors (#267).
- ✅ **Selection actions:** dispatched through a typed `Run`, with the clipboards, imagery load state and Settings window flag moved off globals (#269).
- ✅ **Edit primitives:** one definition each, duplicates merged, and refused edits return `EditError` (#272).
- ✅ **`osm_layer` split:** the file became a module directory and every incremental edit goes through one `Change` / `apply_change` (#287).
- ✅ **Tiles:** tile maths has one home, the tile cache's state lives in one `TileStore`, and tile loading is split into tested steps (#271); tiles load through the installed HTTP client and only real tiles count for very wide views (#273).
- ✅ **HTTP and errors:** one HTTP client injected into the viewer, with typed errors replacing `String` and `anyhow` (#268).
- ✅ **Shared `UploadDriver`:** chunked uploads run through one driver used by the app and the tests (#277).
- ✅ **Upload-in-progress marker:** recorded before each changeset is sent, with a warning on restore and in the upload dialog (#284).
- ✅ **Unknown-outcome uploads:** ask before re-uploading, keep autosaves from other formats, and guard script ops exhaustively (#265).
- ✅ **Script harness:** the script action op fails when nothing handles the action and every `AppHandle` method must be implemented (#276); every osmscript is parsed in `cargo test`, the ops are documented in `docs/osmscript.md`, and the viewer tests share one fixture (#266).
- ✅ **Tests out of production files:** the `main.rs` and `osm_layer.rs` test modules moved into per-area files (`src/viewer_tests/`, `src/layers/osm_layer_tests/`) with shared test support (#270).
- ✅ **Comment passes:** stale comments corrected and narration, banners and history removed across the service, label, style, dialog, network, persistence, data-model, edit-algorithm and layer modules (#274, #282, #283, #286).
- ✅ **Small cleanups (this branch):** `IdleTracker` saturating decrement, one civil-date module, and the stylesheet passed into `OsmLayer` constructors instead of read from the active-style global.

In progress on other branches (not listed as done or open here): reversible edit operations for undo, unified label interfaces, and layer click hit-testing on the `MapLayer` trait.

### Still open from the architecture review

Each item was checked against the code.

- **Typed coordinates.** Latitude/longitude travel as bare `(f64, f64)` tuples whose order differs between modules (`Viewport::center` and `screen_to_geo` return `(lat, lon)`, while mercator values are `(x, y)` pairs of the same type), and the mercator bounds are tuples too. Introduce `LatLon` and `Mercator` newtypes and a bounds struct.
- **`MapViewer` field grouping.** `MapViewer` in `src/main.rs` is still one flat struct; group related fields (viewport and interaction, dialogs, autosave, imagery) into sub-structs.
- **Imagery provider secrets in the keychain.** `mapillary_token` and `fotoshi_api_key` are plain fields of `settings.json` (`src/settings_store.rs`); store them in the keyring like the OAuth token.
- **Fully offline script runs.** Scripts still use the real HTTP client for anything not seeded by a fixture; add a script-mode `HttpClient` that serves only fixtures and fails loudly otherwise.
- **Flaky UI scripts when the window is covered.** Scripts can hang at `capture` or `scroll` after an idle wait when another window covers the app; not yet diagnosed.
- **3D building follow-ups.** The 3D view work lives on the unmerged `building-3d` branch; its follow-ups stay open until it merges.
- **Compiler-checked dispatch for the last two mode hooks.** `refresh_snap_preview` and `handle_modifiers_changed` in `src/main.rs` still test `self.mode() == EditMode::...` instead of matching on `ModeState`, so a new mode does not get a compile error there.

---

## Suggested sequencing

The original edit-loop arc (versions, undo, tags, create/delete, quit guard, upload), the security/robustness tier, the measured perf wins (3.12, 3.3, 3.4) and the 2026-11 architecture work have all shipped. What remains (the label items overlap work in progress on unified label interfaces, so check open branches first):

1. **Label follow-ups (3.14):** start with the cheap, independent ones: settings toggles (3), `label-priority` and street-name vs icon priority (1), street-name halo unification (6). Then cross-layer collision (4) and area-label wrapping (2). The GPU frame-time measurement (10) and satellite legibility check (11) should precede any further halo-cost work.
2. **MapCSS leftovers:** node zoom selectors beyond z18 (3.14 item 9), chevron/one-way properties (7).
3. **Small rendering items:** 3.11 (theme-sourced map colors, tile fade, error badge), 3.7 remainder (theme accent).
4. **UX gaps:** 4.1(c) auto-fetch on viewport settle, 4.5 double-click zoom.
5. **Code health, safely parallelizable:** 5.9 `on_move_layer` test, 5.10 remaining silent lock/save failures, 5.12 `OsmParser` associated functions, then the "Still open from the architecture review" list (typed coordinates, `MapViewer` field grouping, imagery secrets in the keychain, a script-mode HTTP client, the covered-window script hangs, compiler-checked dispatch for the last two mode hooks).
