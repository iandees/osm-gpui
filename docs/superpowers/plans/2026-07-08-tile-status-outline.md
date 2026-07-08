# Tile Download Status Outline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Draw a muted-color outline around every visible map tile that hasn't finished loading, so users can see the tile grid is populating instead of seeing a blank/dark area.

**Architecture:** Add a process-global "loaded tile URLs" set in `tile_cache.rs`, following the exact same pattern as the existing `TILE_LOAD_ERRORS` map. `TileAsset::load` marks a URL loaded on every successful decode path and unmarks it on failure. `TileLayer::render_canvas` then draws a 1px stroked rectangle (reusing the existing debug-boundary drawing code path) around every visible tile whose URL isn't in the loaded set — unconditionally, not gated behind the existing `show_boundaries` debug toggle. Applies to every `TileLayer` instance (base OSM layer and imagery layers alike).

**Tech Stack:** Rust, gpui (`PathBuilder`, `window.paint_path`), existing `tile_cache.rs` / `tile_layer.rs` modules.

## Global Constraints

- Follow existing code patterns in `tile_cache.rs` (global `OnceLock<Mutex<...>>` maps keyed by tile URL string) — don't introduce a different mechanism.
- Do not change the `MapLayer` trait signature (`render_canvas` only has `&Window`, no `cx` — the loaded-set lookup must not need `cx`).
- Reuse the existing muted color already used for the debug boundary overlay: `rgb(0x4a5568)`.
- Run `cargo fmt --check` before every commit (CI enforces it).

---

### Task 1: Track loaded tile URLs in `tile_cache.rs`

**Files:**
- Modify: `src/tile_cache.rs`
- Test: `src/tile_cache.rs` (inline `#[cfg(test)]` module — check the existing one at the bottom of the file and add tests there)

**Interfaces:**
- Produces: `pub fn is_loaded(url: &str) -> bool` — returns `true` once a tile URL has completed a successful decode; `false` otherwise (never fetched, in flight, or most-recently failed).

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block in `src/tile_cache.rs` (find it near the bottom of the file; if the module doesn't already import what you need, add `use super::*;` at the top of the block as the existing tests do):

```rust
    #[test]
    fn is_loaded_false_for_unknown_url() {
        assert!(!is_loaded("https://example.test/never-fetched.png"));
    }

    #[test]
    fn is_loaded_true_after_mark_loaded() {
        let url = "https://example.test/mark-loaded-test.png";
        mark_loaded(url);
        assert!(is_loaded(url));
    }

    #[test]
    fn is_loaded_false_after_unmark_loaded() {
        let url = "https://example.test/unmark-loaded-test.png";
        mark_loaded(url);
        unmark_loaded(url);
        assert!(!is_loaded(url));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tile_cache::tests::is_loaded -- --test-threads=1`
Expected: FAIL with "cannot find function `is_loaded`" (and `mark_loaded`/`unmark_loaded`) — they don't exist yet.

- [ ] **Step 3: Implement the loaded-URL tracker**

In `src/tile_cache.rs`, right after the existing `TILE_LOAD_ERRORS` block (after the `last_error` function, currently ending around line 182), add:

```rust
/// Set of tile URLs that have completed a successful decode at least once
/// (most recently — a subsequent failure removes the URL again). Read by
/// `TileLayer::render_canvas` to decide whether to draw a "still loading"
/// status outline over a tile; populated by `TileAsset::load`.
static TILE_LOADED_URLS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn loaded_urls() -> &'static Mutex<HashSet<String>> {
    TILE_LOADED_URLS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn mark_loaded(url: &str) {
    if let Ok(mut set) = loaded_urls().lock() {
        set.insert(url.to_string());
    }
}

fn unmark_loaded(url: &str) {
    if let Ok(mut set) = loaded_urls().lock() {
        set.remove(url);
    }
}

/// Whether `url` has completed a successful tile decode. Used to decide
/// whether to draw a "still loading" status outline over the tile.
pub fn is_loaded(url: &str) -> bool {
    loaded_urls()
        .lock()
        .map(|set| set.contains(url))
        .unwrap_or(false)
}
```

Add `HashSet` to the existing `use std::collections::HashMap;` import line:

```rust
use std::collections::{HashMap, HashSet};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tile_cache::tests::is_loaded -- --test-threads=1`
Expected: PASS (3 tests)

- [ ] **Step 5: Wire `mark_loaded`/`unmark_loaded` into `TileAsset::load`**

In `TileAsset::load` (same file), there are three places that currently call `clear_error(&url)` on success — the disk cache-hit branch, and the freshly-downloaded-and-decoded branch — and several places that call `record_error(&url, ...)` on failure. Add the mirroring `mark_loaded`/`unmark_loaded` call next to each:

Cache-hit success branch (currently):
```rust
                    if file_path.exists() {
                        match load_image_from_file(&file_path) {
                            Ok(image) => {
                                clear_error(&url);
                                return Ok(Arc::new(image));
                            }
```
becomes:
```rust
                    if file_path.exists() {
                        match load_image_from_file(&file_path) {
                            Ok(image) => {
                                clear_error(&url);
                                mark_loaded(&url);
                                return Ok(Arc::new(image));
                            }
```

Freshly-downloaded success branch (currently):
```rust
                            match load_image_from_file(&file_path) {
                                Ok(image) => {
                                    clear_error(&url);
                                    Ok(Arc::new(image))
                                }
                                Err(e) => {
                                    let reason = format!("Decode: {}", e);
                                    record_error(&url, reason.clone());
                                    Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason))))
                                }
                            }
```
becomes:
```rust
                            match load_image_from_file(&file_path) {
                                Ok(image) => {
                                    clear_error(&url);
                                    mark_loaded(&url);
                                    Ok(Arc::new(image))
                                }
                                Err(e) => {
                                    let reason = format!("Decode: {}", e);
                                    record_error(&url, reason.clone());
                                    unmark_loaded(&url);
                                    Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason))))
                                }
                            }
```

Every other `record_error(&url, ...)` call site in `TileAsset::load` (empty body, not-an-image, mkdir failure, write failure, transport error) should get a matching `unmark_loaded(&url);` immediately after it, so a tile that fails after previously loading (e.g. a corrupted re-fetch) goes back to showing the status outline. There are 5 such call sites total (including the one already shown above) — add `unmark_loaded(&url);` right after each `record_error(&url, ...)` line in the function.

- [ ] **Step 6: Run the full tile_cache test suite**

Run: `cargo test --lib tile_cache::`
Expected: PASS, no regressions.

- [ ] **Step 7: Format and commit**

Run: `cargo fmt`
Run: `cargo fmt --check`

```bash
git add src/tile_cache.rs
git commit -m "Track successfully-loaded tile URLs in tile_cache"
```

---

### Task 2: Draw the muted status outline in `TileLayer::render_canvas`

**Files:**
- Modify: `src/layers/tile_layer.rs`

**Interfaces:**
- Consumes: `crate::tile_cache::is_loaded(url: &str) -> bool` (Task 1).
- Consumes existing: `tile_screen_rect(viewport: &Viewport, tile: &TileCoord) -> (Pixels, Pixels, Pixels, Pixels)`, `get_tiles_for_bounds`, `url_from_template`, `crate::coordinates::is_point_valid`.

- [ ] **Step 1: Read the current `render_canvas` body**

It's at `src/layers/tile_layer.rs:322-364`. It currently only draws tile boundaries when `self.show_boundaries` is `true`, and returns immediately otherwise (`if !self.show_boundaries { return; }`).

- [ ] **Step 2: Restructure `render_canvas` to always compute visible tiles, and draw two independent things per tile**

Replace the full `render_canvas` function body with:

```rust
    fn render_canvas(&self, viewport: &Viewport, _bounds: Bounds<Pixels>, window: &mut Window) {
        let zoom_level = viewport.zoom_level();
        let Some(tile_zoom) = self.effective_tile_zoom(zoom_level) else {
            return;
        };
        let bounds_geo = viewport.visible_bounds();
        let (min_lat, min_lon, max_lat, max_lon) = (
            bounds_geo.min_lat,
            bounds_geo.min_lon,
            bounds_geo.max_lat,
            bounds_geo.max_lon,
        );
        let visible_tiles = get_tiles_for_bounds(min_lat, min_lon, max_lat, max_lon, tile_zoom);

        let debug_boundary_color = rgb(0x4a5568);
        let status_outline_color = rgb(0x4a5568);

        use crate::coordinates::is_point_valid;

        for tile_coord in &visible_tiles {
            // Use the same positioning as render_elements for consistency
            let (tile_x, tile_y, tile_width, tile_height) = tile_screen_rect(viewport, tile_coord);
            let screen_top_left = point(tile_x, tile_y);
            let screen_bottom_right = point(tile_x + tile_width, tile_y + tile_height);

            if !is_point_valid(screen_top_left) || !is_point_valid(screen_bottom_right) {
                continue;
            }

            // Always-on: outline tiles that haven't finished loading yet, in
            // a muted color, so the user can see the grid populating instead
            // of a blank/dark area.
            let tile_url = url_from_template(&self.url_template, tile_coord);
            if !crate::tile_cache::is_loaded(&tile_url) {
                let mut builder = PathBuilder::stroke(px(1.0));
                builder.move_to(point(screen_top_left.x, screen_top_left.y));
                builder.line_to(point(screen_bottom_right.x, screen_top_left.y));
                builder.line_to(point(screen_bottom_right.x, screen_bottom_right.y));
                builder.line_to(point(screen_top_left.x, screen_bottom_right.y));
                builder.close();
                if let Ok(path) = builder.build() {
                    window.paint_path(path, status_outline_color);
                }
            }

            // Debug-only: outline every tile regardless of load status, when
            // the "show tile boundaries" toggle is on.
            if self.show_boundaries {
                let mut builder = PathBuilder::stroke(px(1.0));
                builder.move_to(point(screen_top_left.x, screen_top_left.y));
                builder.line_to(point(screen_bottom_right.x, screen_top_left.y));
                builder.line_to(point(screen_bottom_right.x, screen_bottom_right.y));
                builder.line_to(point(screen_top_left.x, screen_bottom_right.y));
                builder.close();
                if let Ok(path) = builder.build() {
                    window.paint_path(path, debug_boundary_color);
                }
            }
        }
    }
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: builds with no errors or new warnings.

- [ ] **Step 4: Run the existing tile_layer test suite**

Run: `cargo test --lib layers::tile_layer::`
Expected: PASS, no regressions (these tests exercise `tile_screen_rect`/`tile_mercator_bounds`/`compute_effective_tile_zoom`, which are unchanged).

- [ ] **Step 5: Manually verify in the running app**

Use the `run` skill (or `--script`/`osmscript` harness per project convention) to launch the app with an imagery or base tile layer visible, pan/zoom to force new tiles into view, and confirm:
- Tiles not yet loaded show a thin muted-gray outline.
- Once a tile's imagery appears, its outline disappears.
- Toggling "show tile boundaries" (existing debug feature) still shows boundaries on every tile, in addition to the status outline on unloaded ones.

- [ ] **Step 6: Format and commit**

Run: `cargo fmt`
Run: `cargo fmt --check`

```bash
git add src/layers/tile_layer.rs
git commit -m "Draw muted status outline on tiles that have not finished loading"
```

---

### Task 3: Full verification pass

**Files:** none (verification only)

- [ ] **Step 1: Run the full test suite**

Run: `cargo test`
Expected: PASS, no regressions anywhere in the workspace.

- [ ] **Step 2: Run fmt and clippy**

Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: both clean.

- [ ] **Step 3: Final manual check**

Repeat Task 2 Step 5's manual verification once more after the full test/lint pass, to catch anything a lint fix might have changed visually.
