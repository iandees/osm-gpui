# osm-gpui Improvement Suggestions

Consolidated from a four-dimension code review (architecture, editing UX, rendering/UI, robustness) on 2026-07-06 and brought up to date against the merged PRs on 2026-11-11 (through PR #222). Each remaining item is scoped so an agent can implement it without re-doing the analysis. Done items are collapsed to one line with the PR that shipped them; file references for remaining items point at the current tree (line numbers are omitted because they drift quickly — grep for the named functions).

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
🚧 In progress — being worked on in a separate PR. Current state: ways are iterated in id-sorted order (`sorted_ways` in `src/layers/osm_layer.rs`) but there is still no `z-index` in MapCSS and no area < casing < line layering.

### 3.3 Click hit-testing is O(dataset)
✅ Done — R-tree candidate query plus refinement (#57).

### 3.4 Per-frame style re-resolution
✅ Done — cached `NodeStyle`/`WayStyle` (#57).

### 3.4b Sub-pixel segment decimation when zoomed out
✅ Done — `decimate_segments()` (#57).

### 3.13 Tile layer: parent-fallback element built and asset-probed every frame
Still open. `TileLayer::render_elements` (`src/layers/tile_layer.rs`) still builds the parent-fallback `img` (with its own `use_asset` lookup) for every visible tile unconditionally, even when the child tile is already loaded. Tile corners are still projected via `tile_screen_rect_shifted`, which does geo-to-screen math per tile. (Seam fixes #51, #62, #100 touched this code but did not change the per-frame cost.)

**Implement:** probe the child asset first (`window.use_asset::<TileAsset>` returns `Option`) and only attach the parent fallback when the child isn't ready; use precomputed mercator tile bounds.

✅ Done — the child asset is probed first and the parent fallback is only built while it is not ready (tile corners already use mercator bounds).

### 3.5 Per-frame synchronous `read_dir` on the UI thread
✅ Done — the cached-file count is recomputed at most once per second (`cached_file_count` / `STATS_TTL` in `src/tile_cache.rs`). (Stats are still gathered every frame by `get_layer_stats`, but that is now a cheap read.)

### 3.6 Node markers are squares; ways lack joins/caps/AA
✅ Done — round nodes and thin cased ways (#93), line joins and end caps (#169), untagged vertices drawn only when selected / at high zoom (#149, #158), way vertices shown on hover (#174).

### 3.7 Selection styling: hardcoded magenta, square node ring, no halo, no hover
✅ Done (partial) — two-pass halo, round markers and hover highlight shipped (#93), hover drawn above selection (#217). Remaining: the accent is still the `SELECTION_ACCENT` constant in `src/layers/osm_layer.rs` rather than sourced from `cx.theme()`, and box-select still uses a different color.

### 3.8 MapCSS subset is minimal
🚧 In progress — being worked on in a separate PR. Current state: `fill-color`/`fill-opacity` added (#70); zoom selectors are still parsed-and-discarded and `casing-*`, `dashes`, `text`, `icon-image`, `z-index`, `rgb()`/`rgba()` are not supported (see the header of `src/style/mapcss.rs`).

### 3.9 No labels, POI icons, or one-way arrows
🚧 In progress — being worked on in a separate PR. Current state: direction arrows on ways landed in #93 (and relations drawn via #139, #144), but there are no `name` labels and no POI icons on the map.

### 3.10 Hi-DPI tiles
🚧 In progress — being worked on in a separate PR. Current state: nothing in `src/` reads `scale_factor`, so 256px tiles are still stretched on Retina (edge-seam clipping, #100, does not address this).

✅ Done — `hidpi_tile_zoom()` requests z+1 tiles at scale factor >= 1.5 (stretching when z+1 exceeds the source's max zoom).

### 3.11 Map-area colors bypass the theme; tile pop-in is abrupt
Still open (partially mitigated by the theme work in #72, #143, #155). Hardcoded colors remain: grid `rgb(0x374151)` in `src/layers/grid_layer.rs`, tile placeholder `0x2d3748` and the full-tile crimson error block `0x9f1239` in `src/layers/tile_layer.rs`; tiles still appear with no fade.

**Implement:** source map background/grid/overlay colors from `cx.theme()`; fade tile opacity 0→1; replace the crimson error tile with a corner badge.

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
Partially done (#49, #69): `undo.rs`, `menu.rs`, `side_panel.rs`, `script_harness.rs` etc. were split out, but `src/main.rs` has since regrown to ~14,900 lines (including a large in-file test module). Worth another extraction pass (e.g. status/upload/notes/autosave driving code, test module into `src/`-level test files).

### 5.4 Remove the `unsafe` pointer aliasing in `render`
✅ Done — no `addr_of!`/`unsafe` aliasing remains in `main.rs` (#69).

### 5.5 Extract `LayerManager::unique_name`
✅ Done — `LayerManager::unique_name` exists and is used at all call sites (#69).

### 5.6 Replace `Arc<Mutex<Vec<()>>>` signal statics
✅ Done — the statics were removed in the event-driven refactor (#69).

### 5.7 Typed layer stats instead of parse-back strings
Still open. `get_layer_stats` in `src/main.rs` still string-matches `"Cached Files"`/`"Nodes"`/`"Ways"` from `MapLayer::stats()`. Add `fn feature_counts(&self) -> FeatureCounts` to the `MapLayer` trait (`src/layers/mod.rs`).

### 5.8 `OsmMember.member_type: String` → enum
✅ Done — `MemberType` enum parsed in `parse_member` (#54).

### 5.9 Test the real `LayerManager`, not shadow copies
✅ Done (partial) — tests now build a real `LayerManager` and call production `move_layer`/`remove_at` (`src/layers/mod.rs` tests). Remaining: bounds math in `on_move_layer` (`src/main.rs`) has no direct test.

### 5.10 Surface user-action failures instead of swallowing them
✅ Done (partial) — upload, keychain, restore and parse failures now route to status/notice dialogs (#98, #140, #151, #161). Remaining: audit the remaining `if let Ok(guard) = x.lock()` sites in `src/main.rs` and settings/token save failures (`src/auth.rs`, `src/settings_store.rs`) for silent drops.

### 5.11 Extract and test zoom-fit math
✅ Done — `fit_bounds_to_viewport` with unit tests in `src/coordinates.rs` (#69). (`fit_to_osm_data` in `main.rs` still computes the bbox inline but delegates the zoom math.)

### 5.12 Misc small fixes
- User-Agent: ✅ Done — single `USER_AGENT` const built from `CARGO_PKG_VERSION` in `src/version.rs` (#105).
- `check_area`: ✅ Done — now delegates to `capabilities::allows_download` using the server's limits (#121).
- `IdleTracker` (`src/idle_tracker.rs`): still open — `tile_fetch_finished`/`image_decode_finished` use `debug_assert!` and a plain `fetch_sub`, so a release-mode underflow would wrap and wedge `is_idle()`. Use a saturating decrement plus a warning.
- `side_panel_open`: ✅ Done as a fixed-size `[bool; 8]` set; an enum-indexed wrapper would still be tidier (low priority).
- `OsmParser` (`src/osm.rs`): still a stateless unit struct with `&self` methods; convert to associated functions (low priority).

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
- **Tooling:** scriptable screenshot/osmscript harness (#3, #64, #75, #147, #214), render-cost measurement and way-drawing speedups (#148, #157, docs/perf), live dev-server upload tests (`docs/live-test`).

---

## Suggested sequencing

The original edit-loop arc (versions, undo, tags, create/delete, quit guard, upload), the security/robustness tier, and the measured perf wins (3.12, 3.3, 3.4) have all shipped. What remains:

1. **In flight (separate PRs):** 3.8 MapCSS, 3.2 draw order, 3.9 labels/icons/one-way, 3.10 hi-DPI tiles. 3.2 and 3.9 depend on 3.8's `z-index`/`text`/`icon-image` support, so land 3.8 first.
2. **Small rendering items:** 3.13 (tile parent-fallback only when the child isn't ready), 3.11 (theme-sourced map colors, tile fade, error badge), 3.7 remainder (theme accent).
3. **UX gaps:** 4.1(c) auto-fetch on viewport settle, 4.5 double-click zoom.
4. **Code health, safely parallelizable:** 5.3 (re-split `main.rs`), 5.7 typed layer stats, 5.9 `on_move_layer` test, 5.10 remaining silent lock/save failures, 5.12 leftovers (`IdleTracker` saturating decrement, `OsmParser` associated functions).
