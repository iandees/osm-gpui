# Imagery Settings: Rename + Tile Cache Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename the settings window's "Imagery Sources" page to "Imagery", move its existing content into a "Custom Sources" group, and add a "Tile Cache" group that shows per-source disk usage, lets the user clear a source's cache (or everything), and lets the user configure the cache size budget in MB (default 500).

**Architecture:** The on-disk tile cache moves from one flat directory (`tiles/tile_<hash>.png`) to per-source subdirectories (`tiles/<source_key>/tile_<hash>.png`), where `source_key` is a stable, human-legible slug derived from each imagery layer's URL *template* (not its resolved per-tile URL, since some templates rotate subdomains). This makes per-source size/clear operations possible. The cache's eviction budget moves from a hardcoded constant to a live, process-global `AtomicU64`, seeded from a new persisted `AppSettings.cache_budget_mb` field, so the settings UI can change it without a restart — and, critically, without evicting anything until the next normal write-triggered eviction sweep.

**Tech Stack:** Rust, GPUI + gpui-component (`SettingPage`/`SettingGroup`/`SettingItem`, `InputState`), `sha2` (already a dependency) for hashing, `serde`/JSON persistence (existing `persist::JsonStore` pattern).

## Global Constraints

- Cache budget: user-configurable in whole megabytes, default `500`, minimum accepted value `10` (reject lower with an inline error, same pattern as zoom-field validation in `custom_imagery_dialog.rs`).
- Lowering the budget must never delete anything immediately. It only changes what the *next* naturally-triggered eviction sweep (every 25 tile writes, see `WRITES_BETWEEN_EVICTION_CHECKS` in `src/tile_cache.rs`) evicts down to.
- No confirmation dialogs before "Clear" (per-source) or "Clear All" — cache data is disposable/re-downloadable, unlike the existing delete-custom-source confirmation flow.
- Cache panel rows are identified by their on-disk `source_key` string (a sanitized slug), not a resolved friendly source name — this is a deliberate simplification, not a bug.
- Follow existing code patterns exactly: `InputState` + parse-and-validate-on-save for numeric fields (mirrors `save_custom_api_url`/`save_client_id`/zoom fields), `SettingGroup`/`SettingItem`/`SettingField::render` for settings UI, `JsonStore`/`update_store`/`snapshot` for persistence. Do not introduce new UI widget types or a new persistence mechanism.
- Run `cargo fmt --check` and `cargo test` before every commit in this plan.

---

## Task 1: `source_key_for_template` — stable per-source cache key

**Files:**
- Modify: `src/tile_cache.rs` (add function near `cache_filename`, around line 53-58; add tests to the existing `#[cfg(test)] mod tests` block)

**Interfaces:**
- Produces: `fn source_key_for_template(template: &str) -> String` (crate-private for now; Task 2 will call it from `src/layers/tile_layer.rs` via `crate::tile_cache::source_key_for_template`, so mark it `pub(crate)`).

This function turns a layer's URL template (e.g.
`https://tile.openstreetmap.org/{z}/{x}/{y}.png`) into a stable, filesystem-safe,
human-legible slug used as a cache subdirectory name. It operates on the
*template*, not a resolved URL, so a `{switch:a,b,c}` rotating-subdomain
source always produces one key regardless of which subdomain a given tile
resolves to (see `tiles::url_from_template`, which anchors subdomain
substitution on the literal `"{switch:"` prefix followed by the next `}`).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/tile_cache.rs` (after the
existing `cache_filename_*` tests):

```rust
#[test]
fn source_key_deterministic_for_same_template() {
    let a = source_key_for_template("https://tile.openstreetmap.org/{z}/{x}/{y}.png");
    let b = source_key_for_template("https://tile.openstreetmap.org/{z}/{x}/{y}.png");
    assert_eq!(a, b);
}

#[test]
fn source_key_differs_for_different_templates() {
    let a = source_key_for_template("https://tile-a.example.test/{z}/{x}/{y}.png");
    let b = source_key_for_template("https://tile-b.example.test/{z}/{x}/{y}.png");
    assert_ne!(a, b);
}

#[test]
fn source_key_is_filesystem_safe() {
    let key = source_key_for_template(
        "https://tile.example.test/a?b={z}/{x}/{y}.png&key=SECRET123",
    );
    assert!(key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
}

#[test]
fn source_key_ignores_switch_subdomain_rotation() {
    // A `{switch:a,b,c}` template must produce one stable key, since
    // `url_from_template` picks a different literal subdomain per tile.
    let template = "https://{switch:a,b,c}.tile.example.test/{z}/{x}/{y}.png";
    let key1 = source_key_for_template(template);
    let key2 = source_key_for_template(template);
    assert_eq!(key1, key2);
}

#[test]
fn source_key_has_readable_prefix() {
    let key = source_key_for_template("https://tile.openstreetmap.org/{z}/{x}/{y}.png");
    assert!(key.starts_with("tile_openstreetmap_org_z_x_y"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tile_cache::tests::source_key -- --nocapture`
Expected: FAIL with "cannot find function `source_key_for_template`"

- [ ] **Step 3: Write the implementation**

Add this function to `src/tile_cache.rs`, near `cache_filename` (it already
imports `Sha256`, `Digest`):

```rust
/// Derive a stable, human-legible cache subdirectory name from an imagery
/// source's URL *template* (e.g. `https://tile.openstreetmap.org/{z}/{x}/{y}.png`),
/// not a resolved per-tile URL. Using the template means a `{switch:a,b,c}`
/// rotating-subdomain source always maps to one stable key, regardless of
/// which subdomain a given tile happens to resolve to.
pub(crate) fn source_key_for_template(template: &str) -> String {
    let mut normalized = template.to_string();

    // Collapse a `{switch:a,b,c}` span to a single marker, mirroring the
    // span-detection logic in `tiles::url_from_template` (anchored on the
    // literal "{switch:" prefix, then the next '}').
    if let Some(start) = normalized.find("{switch:") {
        if let Some(rel_end) = normalized[start..].find('}') {
            let end = start + rel_end;
            normalized.replace_range(start..=end, "s");
        }
    }
    normalized = normalized.replace("{s}", "s");

    normalized = normalized.replace("{zoom}", "z");
    normalized = normalized.replace("{z}", "z");
    normalized = normalized.replace("{x}", "x");
    normalized = normalized.replace("{-y}", "negy");
    normalized = normalized.replace("{y}", "y");

    let without_scheme = normalized
        .strip_prefix("https://")
        .or_else(|| normalized.strip_prefix("http://"))
        .unwrap_or(&normalized);

    // Sanitize into a filesystem-safe slug: keep alphanumerics, collapse
    // every run of other characters (dots, slashes, query separators, …)
    // into a single underscore.
    let mut slug = String::with_capacity(without_scheme.len());
    let mut last_was_sep = false;
    for c in without_scheme.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            last_was_sep = false;
        } else if !last_was_sep {
            slug.push('_');
            last_was_sep = true;
        }
    }
    let slug = slug.trim_matches('_');
    let slug: String = slug.chars().take(60).collect();

    // Short hash suffix of the *original* template guarantees uniqueness
    // even if two different templates sanitize to the same slug, or the
    // slug was truncated.
    let mut hasher = Sha256::new();
    hasher.update(template.as_bytes());
    let digest = hasher.finalize();
    let hash_suffix = format!("{:x}", digest)[..8].to_string();

    if slug.is_empty() {
        hash_suffix
    } else {
        format!("{}-{}", slug, hash_suffix)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tile_cache::tests::source_key`
Expected: PASS (5 tests)

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
git add src/tile_cache.rs
git commit -m "Add source_key_for_template for per-source tile cache keys"
```

---

## Task 2: Per-source cache directories (`TileAssetKey`)

**Files:**
- Modify: `src/tile_cache.rs` (change `TileAsset::Source` from `String` to a new `TileAssetKey` struct; restructure the fetch/write path to use per-source subdirectories; add `tile_file_path` helper + tests)
- Modify: `src/layers/tile_layer.rs` (add `source_key: String` field to `TileLayer`, compute it at construction, thread `TileAssetKey` through both `img(...)` call sites)

**Interfaces:**
- Consumes: `source_key_for_template(template: &str) -> String` (Task 1)
- Produces: `pub struct TileAssetKey { pub url: String, pub source_key: String }` (deriving `Clone, Hash, PartialEq, Eq`) — this becomes `TileAsset::Source`, consumed by `tile_layer.rs`'s `img(...)` closures. `TileLayer` gains a `source_key: String` field, readable by later tasks if needed (none currently need it directly — `tile_cache.rs`'s public cache-summary API in Task 4 works purely off the filesystem).

Today `TileAsset::Source = String` (the resolved tile URL), and every tile
from every layer lands in one flat cache directory. This task makes the
cache directory layout `tiles/<source_key>/tile_<sha256(url)>.png`.

- [ ] **Step 1: Write the failing test**

Add to `src/tile_cache.rs`'s test module:

```rust
#[test]
fn tile_file_path_differs_by_source_key() {
    let base = Path::new("/cache/tiles");
    let url = "https://tile.example.test/1/2/3.png";
    let a = tile_file_path(base, "source-a", url);
    let b = tile_file_path(base, "source-b", url);
    assert_ne!(a, b);
    assert!(a.starts_with(base.join("source-a")));
    assert!(b.starts_with(base.join("source-b")));
}

#[test]
fn tile_file_path_deterministic() {
    let base = Path::new("/cache/tiles");
    let url = "https://tile.example.test/1/2/3.png";
    assert_eq!(
        tile_file_path(base, "source-a", url),
        tile_file_path(base, "source-a", url)
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tile_cache::tests::tile_file_path`
Expected: FAIL with "cannot find function `tile_file_path`"

- [ ] **Step 3: Add `TileAssetKey` and `tile_file_path`, restructure `TileAsset`**

In `src/tile_cache.rs`, add near `pub struct TileAsset;`:

```rust
/// Identifies both the concrete tile URL to fetch and the cache
/// subdirectory (derived from the source's URL template via
/// `source_key_for_template`) it belongs in.
#[derive(Clone, Hash, PartialEq, Eq)]
pub struct TileAssetKey {
    pub url: String,
    pub source_key: String,
}

/// Compose the on-disk path for one cached tile: `<base_dir>/<source_key>/tile_<hash>.png`.
fn tile_file_path(base_dir: &Path, source_key: &str, url: &str) -> PathBuf {
    base_dir.join(source_key).join(cache_filename(url))
}
```

Replace the `impl Asset for TileAsset` block's `type Source` and `load`
body. The overall control flow (check-exists → mkdir → download → validate
→ write_atomic → maybe_evict → load) is unchanged; only the source type and
the path computation change:

```rust
impl Asset for TileAsset {
    type Source = TileAssetKey;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        key: Self::Source,
        cx: &mut gpui::App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let executor = cx.background_executor().clone();
        let idle = TILE_IDLE_TRACKER.get().cloned();
        let TileAssetKey { url, source_key } = key;

        async move {
            if let Some(ref tracker) = idle {
                tracker.tile_fetch_started();
            }
            let result = executor
                .spawn(async move {
                    let base_dir = cache_dir();
                    let file_path = tile_file_path(&base_dir, &source_key, &url);
                    let source_dir = file_path
                        .parent()
                        .expect("tile_file_path always has a parent")
                        .to_path_buf();

                    if file_path.exists() {
                        match load_image_from_file(&file_path) {
                            Ok(image) => {
                                clear_error(&url);
                                return Ok(Arc::new(image));
                            }
                            Err(_) => {
                                let _ = fs::remove_file(&file_path);
                            }
                        }
                    }

                    if let Err(e) = fs::create_dir_all(&source_dir) {
                        let reason = TileFetchError::Io(format!("mkdir: {}", e)).to_string();
                        record_error(&url, reason.clone());
                        return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason))));
                    }

                    match download_file_sync(&url) {
                        Ok(bytes) => {
                            if bytes.is_empty() {
                                let reason = TileFetchError::EmptyBody.to_string();
                                record_error(&url, reason.clone());
                                return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(
                                    reason
                                ))));
                            }

                            let is_png = bytes.len() >= 8 && &bytes[1..4] == b"PNG";
                            let is_jpeg = bytes.len() >= 3
                                && bytes[0] == 0xFF
                                && bytes[1] == 0xD8
                                && bytes[2] == 0xFF;
                            if !is_png && !is_jpeg {
                                let reason = TileFetchError::NotImage.to_string();
                                record_error(&url, reason.clone());
                                return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(
                                    reason
                                ))));
                            }

                            if let Err(e) = write_atomic(&file_path, &bytes) {
                                let reason =
                                    TileFetchError::Io(format!("write: {}", e)).to_string();
                                record_error(&url, reason.clone());
                                return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(
                                    reason
                                ))));
                            }
                            maybe_evict(&base_dir);

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
                        }
                        Err(e) => {
                            let reason = e.to_string();
                            record_error(&url, reason.clone());
                            Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason))))
                        }
                    }
                })
                .await;
            if let Some(ref tracker) = idle {
                tracker.tile_fetch_finished();
            }
            result
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tile_cache::tests::tile_file_path`
Expected: PASS (2 tests). Note the crate will not yet compile as a whole —
`src/layers/tile_layer.rs` still passes bare `String`s into
`window.use_asset::<TileAsset>(...)`. Fix that next before running the full
test suite.

- [ ] **Step 5: Wire `TileLayer` to build `TileAssetKey`s**

In `src/layers/tile_layer.rs`:

Add a `source_key` field to the struct (after `url_template`):

```rust
pub struct TileLayer {
    id: LayerId,
    name: String,
    url_template: String,
    source_key: String,
    visible: bool,
    tile_cache: Arc<Mutex<TileCache>>,
    show_boundaries: bool,
    min_zoom: Option<u32>,
    max_zoom: Option<u32>,
    attribution: Option<AttributionInfo>,
}
```

Compute it in `new_with_template`:

```rust
pub fn new_with_template(
    id: LayerId,
    name: String,
    url_template: String,
    tile_cache: Arc<Mutex<TileCache>>,
) -> Self {
    let source_key = crate::tile_cache::source_key_for_template(&url_template);
    Self {
        id,
        name,
        url_template,
        source_key,
        visible: true,
        tile_cache,
        show_boundaries: false,
        min_zoom: None,
        max_zoom: None,
        attribution: None,
    }
}
```

In `render_elements`, replace the parent-fallback block's URL handling:

```rust
let parent_fallback = tile_coord.parent().map(|parent_coord| {
    let (qx, qy) = tile_coord.quadrant_in_parent();
    let parent_url = url_from_template(&self.url_template, &parent_coord);
    let parent_key = crate::tile_cache::TileAssetKey {
        url: parent_url,
        source_key: self.source_key.clone(),
    };
    div()
        .absolute()
        .left(-tile_width * qx as f32)
        .top(-tile_height * qy as f32)
        .w(tile_width * 2.0)
        .h(tile_height * 2.0)
        .child(
            img(move |window: &mut gpui::Window, cx: &mut gpui::App| {
                window.use_asset::<crate::tile_cache::TileAsset>(&parent_key, cx)
            })
            .size_full()
            .object_fit(gpui::ObjectFit::Cover),
        )
        .into_any_element()
});
```

And the main tile's URL/asset handling:

```rust
// Generate tile URL via the layer's URL template.
let tile_url = url_from_template(&self.url_template, tile_coord);
```

stays as-is, but where it's later used, replace:

```rust
let char_budget = ((f32::from(tile_width) / 6.0) as usize).clamp(8, 40);
let fallback_url = tile_url.clone();
let asset_key = crate::tile_cache::TileAssetKey {
    url: tile_url,
    source_key: self.source_key.clone(),
};
```

(replacing the old `let asset_url = tile_url;` line), and update the `img`
call:

```rust
img(move |window: &mut gpui::Window, cx: &mut gpui::App| {
    window.use_asset::<crate::tile_cache::TileAsset>(&asset_key, cx)
})
```

(`fallback_url` is unrelated to the cache path — it's only used for
`crate::tile_cache::last_error(&fallback_url)` lookups by URL — so it's
untouched.)

- [ ] **Step 6: Add a wiring regression test**

Add to `src/layers/tile_layer.rs`'s existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn built_in_layer_source_key_matches_template_derivation() {
    use crate::tile_cache::source_key_for_template;
    use crate::layers::LayerId;
    use std::sync::{Arc, Mutex};

    let cache = Arc::new(Mutex::new(crate::tile_cache::TileCache::new(
        gpui::BackgroundExecutor::test(),
        Arc::new(crate::idle_tracker::IdleTracker::new()),
    )));
    let layer = super::TileLayer::new(LayerId::new(1), cache);
    assert_eq!(layer.source_key, source_key_for_template(super::OSM_CARTO_TEMPLATE));
}
```

If `gpui::BackgroundExecutor::test()` or `LayerId::new` don't match this
codebase's actual test-construction helpers, check how other tests in
`src/layers/tile_layer.rs` or `src/layers/mod.rs` construct a `TileLayer`/
`LayerId`/`IdleTracker` for tests and use the same helpers — the point of
this test is solely to assert `layer.source_key == source_key_for_template(OSM_CARTO_TEMPLATE)`
so future refactors of either function can't silently desync them. Since
`source_key` is a private field, this test must live inside
`src/layers/tile_layer.rs`'s own test module (which already has
`use super::{...}` access to private items).

- [ ] **Step 7: Run the full test suite and build**

Run: `cargo build && cargo test --lib`
Expected: builds cleanly; all existing and new tests pass.

- [ ] **Step 8: Format and commit**

```bash
cargo fmt
git add src/tile_cache.rs src/layers/tile_layer.rs
git commit -m "Cache tiles under per-source subdirectories via TileAssetKey"
```

---

## Task 3: Recursive eviction across per-source subdirectories

**Files:**
- Modify: `src/tile_cache.rs` (`evict_if_over_budget`, `cached_file_count`; add `collect_cache_files`; extend tests)

**Interfaces:**
- Consumes: nothing new from other tasks.
- Produces: `fn collect_cache_files(dir: &Path, files: &mut Vec<(PathBuf, u64, SystemTime)>, total: &mut u64)`, used by Task 4's `cache_summary_at`.

After Task 2, tiles live in per-source subdirectories, but
`evict_if_over_budget` and `cached_file_count` still only scan the top level
of `tiles/` with a single `fs::read_dir` — they need to recurse.

- [ ] **Step 1: Write the failing tests**

Add to `src/tile_cache.rs`'s test module:

```rust
#[test]
fn collect_cache_files_sums_nested_directories() {
    let dir = std::env::temp_dir().join(format!(
        "osm-gpui-test-collect-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("source-a")).unwrap();
    fs::create_dir_all(dir.join("source-b")).unwrap();
    fs::write(dir.join("source-a").join("a.png"), vec![0u8; 100]).unwrap();
    fs::write(dir.join("source-b").join("b.png"), vec![0u8; 50]).unwrap();
    fs::write(dir.join("loose.png"), vec![0u8; 10]).unwrap();

    let mut files = Vec::new();
    let mut total = 0u64;
    collect_cache_files(&dir, &mut files, &mut total);

    assert_eq!(total, 160);
    assert_eq!(files.len(), 3);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn evict_if_over_budget_scans_nested_source_directories() {
    let dir = std::env::temp_dir().join(format!(
        "osm-gpui-test-evict-nested-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    let source_a = dir.join("source-a");
    let source_b = dir.join("source-b");
    fs::create_dir_all(&source_a).unwrap();
    fs::create_dir_all(&source_b).unwrap();

    let now = SystemTime::now();
    let write = |path: &Path, age_secs: u64| {
        fs::write(path, vec![0u8; 100]).unwrap();
        let file = fs::File::open(path).unwrap();
        file.set_modified(now - std::time::Duration::from_secs(age_secs))
            .unwrap();
    };
    write(&source_a.join("oldest.png"), 180);
    write(&source_b.join("middle.png"), 120);
    write(&source_a.join("newest.png"), 60);

    // Budget allows only ~1 file; the two oldest (across both source
    // directories) should be evicted, leaving "newest.png" behind.
    evict_if_over_budget(&dir, 150);

    let mut files = Vec::new();
    let mut total = 0u64;
    collect_cache_files(&dir, &mut files, &mut total);
    let mut remaining: Vec<String> = files
        .into_iter()
        .map(|(path, _, _)| path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    remaining.sort();
    assert_eq!(remaining, vec!["newest.png".to_string()]);

    let _ = fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tile_cache::tests::collect_cache_files tile_cache::tests::evict_if_over_budget_scans_nested_source_directories`
Expected: FAIL — `collect_cache_files` doesn't exist yet, and the nested-eviction test fails because `evict_if_over_budget` doesn't currently recurse into subdirectories.

- [ ] **Step 3: Implement `collect_cache_files` and use it**

Add to `src/tile_cache.rs`:

```rust
/// Recursively collect every cache file under `dir` — one level of
/// source-key subdirectories, plus any loose top-level files left over
/// from before per-source directories existed — appending
/// `(path, size, mtime)` tuples and accumulating `total`.
fn collect_cache_files(dir: &Path, files: &mut Vec<(PathBuf, u64, SystemTime)>, total: &mut u64) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect_cache_files(&path, files, total);
        } else if meta.is_file() {
            let size = meta.len();
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            *total += size;
            files.push((path, size, mtime));
        }
    }
}
```

Replace the body of `evict_if_over_budget` (keep its signature
`fn evict_if_over_budget(dir: &Path, max_bytes: u64)` unchanged):

```rust
fn evict_if_over_budget(dir: &Path, max_bytes: u64) {
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total: u64 = 0;
    collect_cache_files(dir, &mut files, &mut total);

    if total <= max_bytes {
        return;
    }

    // Oldest first.
    files.sort_by_key(|(_, _, mtime)| *mtime);

    for (path, size, _) in files {
        if total <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}
```

Update `cached_file_count` to scan recursively too (it previously did a
flat `read_dir(...).count()`, which would now undercount by treating each
source subdirectory as a single entry):

```rust
fn cached_file_count() -> usize {
    let cell = STATS_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = match cell.lock() {
        Ok(g) => g,
        Err(_) => return 0,
    };

    if let Some((last, count)) = *guard {
        if last.elapsed() < STATS_TTL {
            return count;
        }
    }

    let dir = cache_dir();
    let count = if dir.exists() {
        let mut files = Vec::new();
        let mut total = 0u64;
        collect_cache_files(&dir, &mut files, &mut total);
        files.len()
    } else {
        0
    };
    *guard = Some((Instant::now(), count));
    count
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tile_cache::tests`
Expected: PASS — all tests in `tile_cache.rs`, including the pre-existing
`evict_if_over_budget_removes_oldest_first` (still a flat single-directory
case, still covered by the new recursive implementation since a directory
with no subdirectories is just the base case of the recursion).

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
git add src/tile_cache.rs
git commit -m "Make tile cache eviction and file counting recurse into per-source subdirectories"
```

---

## Task 4: `cache_summary`, `clear_source`, `clear_all_cache`, `format_bytes`

**Files:**
- Modify: `src/tile_cache.rs` (add public query/mutation API + tests)

**Interfaces:**
- Consumes: `collect_cache_files` (Task 3)
- Produces (all `pub`, consumed by Task 6's settings UI):
  - `pub struct CacheSourceUsage { pub key: String, pub bytes: u64, pub file_count: usize }`
  - `pub struct CacheSummary { pub total_bytes: u64, pub total_files: usize, pub sources: Vec<CacheSourceUsage> }` (sources sorted by `bytes` descending)
  - `pub fn cache_summary() -> CacheSummary`
  - `pub fn clear_source(key: &str) -> std::io::Result<()>`
  - `pub fn clear_all_cache() -> std::io::Result<()>`
  - `pub fn format_bytes(bytes: u64) -> String`

- [ ] **Step 1: Write the failing tests**

Add to `src/tile_cache.rs`'s test module:

```rust
#[test]
fn cache_summary_groups_by_source_and_buckets_loose_files() {
    let dir = std::env::temp_dir().join(format!(
        "osm-gpui-test-summary-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("source-a")).unwrap();
    fs::create_dir_all(dir.join("source-b")).unwrap();
    fs::write(dir.join("source-a").join("a1.png"), vec![0u8; 100]).unwrap();
    fs::write(dir.join("source-a").join("a2.png"), vec![0u8; 50]).unwrap();
    fs::write(dir.join("source-b").join("b1.png"), vec![0u8; 20]).unwrap();
    fs::write(dir.join("stray.png"), vec![0u8; 5]).unwrap();

    let summary = cache_summary_at(&dir);

    assert_eq!(summary.total_bytes, 175);
    assert_eq!(summary.total_files, 4);
    assert_eq!(summary.sources.len(), 3);

    let source_a = summary
        .sources
        .iter()
        .find(|s| s.key == "source-a")
        .unwrap();
    assert_eq!(source_a.bytes, 150);
    assert_eq!(source_a.file_count, 2);

    let uncategorized = summary
        .sources
        .iter()
        .find(|s| s.key == UNCATEGORIZED_KEY)
        .unwrap();
    assert_eq!(uncategorized.bytes, 5);
    assert_eq!(uncategorized.file_count, 1);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cache_summary_sorts_sources_by_size_descending() {
    let dir = std::env::temp_dir().join(format!(
        "osm-gpui-test-summary-sort-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("small")).unwrap();
    fs::create_dir_all(dir.join("big")).unwrap();
    fs::write(dir.join("small").join("f.png"), vec![0u8; 10]).unwrap();
    fs::write(dir.join("big").join("f.png"), vec![0u8; 1000]).unwrap();

    let summary = cache_summary_at(&dir);
    assert_eq!(summary.sources[0].key, "big");
    assert_eq!(summary.sources[1].key, "small");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cache_summary_missing_dir_is_empty() {
    let dir = std::env::temp_dir().join(format!(
        "osm-gpui-test-summary-missing-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    let summary = cache_summary_at(&dir);
    assert_eq!(summary.total_bytes, 0);
    assert_eq!(summary.total_files, 0);
    assert!(summary.sources.is_empty());
}

#[test]
fn clear_source_removes_only_targeted_subdirectory() {
    let dir = std::env::temp_dir().join(format!(
        "osm-gpui-test-clear-source-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("source-a")).unwrap();
    fs::create_dir_all(dir.join("source-b")).unwrap();
    fs::write(dir.join("source-a").join("a.png"), vec![0u8; 10]).unwrap();
    fs::write(dir.join("source-b").join("b.png"), vec![0u8; 10]).unwrap();

    clear_source_at(&dir, "source-a").unwrap();

    assert!(!dir.join("source-a").exists());
    assert!(dir.join("source-b").join("b.png").exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn clear_source_uncategorized_removes_only_loose_files() {
    let dir = std::env::temp_dir().join(format!(
        "osm-gpui-test-clear-uncategorized-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("source-a")).unwrap();
    fs::write(dir.join("source-a").join("a.png"), vec![0u8; 10]).unwrap();
    fs::write(dir.join("stray.png"), vec![0u8; 10]).unwrap();

    clear_source_at(&dir, UNCATEGORIZED_KEY).unwrap();

    assert!(!dir.join("stray.png").exists());
    assert!(dir.join("source-a").join("a.png").exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn clear_all_cache_removes_everything() {
    let dir = std::env::temp_dir().join(format!(
        "osm-gpui-test-clear-all-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("source-a")).unwrap();
    fs::write(dir.join("source-a").join("a.png"), vec![0u8; 10]).unwrap();
    fs::write(dir.join("stray.png"), vec![0u8; 10]).unwrap();

    clear_all_cache_at(&dir).unwrap();

    assert!(!dir.exists());
}

#[test]
fn format_bytes_picks_appropriate_unit() {
    assert_eq!(format_bytes(500), "500 B");
    assert_eq!(format_bytes(2048), "2.0 KB");
    assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tile_cache::tests::cache_summary tile_cache::tests::clear_ tile_cache::tests::format_bytes`
Expected: FAIL — none of `cache_summary_at`, `clear_source_at`,
`clear_all_cache_at`, `format_bytes`, or `UNCATEGORIZED_KEY` exist yet.

- [ ] **Step 3: Implement the query/mutation API**

Add to `src/tile_cache.rs`:

```rust
/// Per-source-key aggregate cache usage, as reported by `cache_summary`.
pub struct CacheSourceUsage {
    pub key: String,
    pub bytes: u64,
    pub file_count: usize,
}

/// Aggregate tile cache usage across every source, plus a per-source
/// breakdown. Computed via a fresh, on-demand directory scan — safe to call
/// only occasionally (e.g. when a settings panel is open), not on a hot
/// per-frame path (see `cached_file_count` for that).
pub struct CacheSummary {
    pub total_bytes: u64,
    pub total_files: usize,
    /// Sorted by `bytes`, descending.
    pub sources: Vec<CacheSourceUsage>,
}

/// Bucket for cache files that aren't inside any source-key subdirectory —
/// e.g. leftovers from before per-source directories existed.
const UNCATEGORIZED_KEY: &str = "(uncategorized)";

pub fn cache_summary() -> CacheSummary {
    cache_summary_at(&cache_dir())
}

fn cache_summary_at(dir: &Path) -> CacheSummary {
    let mut per_source: HashMap<String, (u64, usize)> = HashMap::new();
    let mut total_bytes = 0u64;
    let mut total_files = 0usize;

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            return CacheSummary {
                total_bytes: 0,
                total_files: 0,
                sources: Vec::new(),
            };
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            let key = entry.file_name().to_string_lossy().to_string();
            let mut files = Vec::new();
            let mut bytes = 0u64;
            collect_cache_files(&path, &mut files, &mut bytes);
            total_bytes += bytes;
            total_files += files.len();
            per_source.insert(key, (bytes, files.len()));
        } else if meta.is_file() {
            let size = meta.len();
            total_bytes += size;
            total_files += 1;
            let bucket = per_source
                .entry(UNCATEGORIZED_KEY.to_string())
                .or_insert((0, 0));
            bucket.0 += size;
            bucket.1 += 1;
        }
    }

    let mut sources: Vec<CacheSourceUsage> = per_source
        .into_iter()
        .map(|(key, (bytes, file_count))| CacheSourceUsage {
            key,
            bytes,
            file_count,
        })
        .collect();
    sources.sort_by(|a, b| b.bytes.cmp(&a.bytes));

    CacheSummary {
        total_bytes,
        total_files,
        sources,
    }
}

/// Remove all cached tiles for one source (or the `"(uncategorized)"`
/// bucket of loose top-level files). A missing directory is not an error.
pub fn clear_source(key: &str) -> std::io::Result<()> {
    clear_source_at(&cache_dir(), key)
}

fn clear_source_at(dir: &Path, key: &str) -> std::io::Result<()> {
    if key == UNCATEGORIZED_KEY {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                fs::remove_file(&path)?;
            }
        }
        return Ok(());
    }

    let source_dir = dir.join(key);
    if source_dir.exists() {
        fs::remove_dir_all(&source_dir)?;
    }
    Ok(())
}

/// Remove the entire tile cache. Recreated lazily on the next tile write.
pub fn clear_all_cache() -> std::io::Result<()> {
    clear_all_cache_at(&cache_dir())
}

fn clear_all_cache_at(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

/// Format a byte count as a short, human-readable string (e.g. "342 MB",
/// "1.2 GB"), for display in the cache settings panel.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib tile_cache::tests`
Expected: PASS — all tests in the module.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
git add src/tile_cache.rs
git commit -m "Add cache_summary, clear_source, clear_all_cache, format_bytes"
```

---

## Task 5: Configurable cache budget (`AppSettings.cache_budget_mb`)

**Files:**
- Modify: `src/tile_cache.rs` (replace the hardcoded `MAX_CACHE_BYTES` constant with a live `AtomicU64` budget + `set_budget_mb`)
- Modify: `src/settings_store.rs` (add `cache_budget_mb` field to `AppSettings`, update `Default` impl and existing test struct literals)
- Modify: `src/main.rs` (seed the budget from persisted settings at startup)

**Interfaces:**
- Produces: `pub fn set_budget_mb(mb: u64)` in `tile_cache.rs` (consumed by Task 6's settings UI and by `main.rs`'s startup wiring); `AppSettings.cache_budget_mb: u64` (consumed by Task 6).

- [ ] **Step 1: Write the failing tests**

Add to `src/tile_cache.rs`'s test module:

```rust
#[test]
fn set_budget_mb_updates_current_budget_without_touching_disk() {
    set_budget_mb(10);
    assert_eq!(current_budget_bytes(), 10 * 1024 * 1024);
    // Restore the default so other tests in this process (CACHE_BUDGET_BYTES
    // is a process-global static) aren't affected by this one.
    set_budget_mb(500);
    assert_eq!(current_budget_bytes(), DEFAULT_CACHE_BUDGET_BYTES);
}
```

Add to `src/settings_store.rs`'s test module:

```rust
#[test]
fn missing_cache_budget_field_defaults_to_500() {
    let dir = tmp_dir("missing-budget");
    let path = dir.join("settings.json");
    // Simulate a settings.json written before cache_budget_mb existed.
    fs::write(
        &path,
        br#"{"api_server":"Primary","custom_api_url":"","client_ids":{}}"#,
    )
    .unwrap();
    let loaded = load_from(&path);
    assert_eq!(loaded.cache_budget_mb, 500);
}

#[test]
fn cache_budget_round_trips() {
    let dir = tmp_dir("cache-budget-round-trip");
    let path = dir.join("settings.json");
    let settings = AppSettings {
        api_server: ApiServerChoice::Primary,
        custom_api_url: String::new(),
        client_ids: HashMap::new(),
        cache_budget_mb: 250,
    };
    save_to(&path, &settings).unwrap();
    let loaded = load_from(&path);
    assert_eq!(loaded.cache_budget_mb, 250);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tile_cache::tests::set_budget_mb settings_store::tests::missing_cache_budget settings_store::tests::cache_budget`
Expected: FAIL to compile — `set_budget_mb`/`current_budget_bytes`/
`DEFAULT_CACHE_BUDGET_BYTES` don't exist yet in `tile_cache.rs`, and
`AppSettings` has no `cache_budget_mb` field yet (this will also break
compilation of the *existing* `settings_store.rs` tests that construct
`AppSettings` literals — that's expected and fixed in the next step).

- [ ] **Step 3: Add the budget atomic to `tile_cache.rs`**

Replace the existing `const MAX_CACHE_BYTES: u64 = 500 * 1024 * 1024;`
(with its doc comment) with:

```rust
/// Default cache budget (500 MB), used until overridden by
/// `set_budget_mb` (normally seeded from persisted `AppSettings.cache_budget_mb`
/// at startup).
const DEFAULT_CACHE_BUDGET_BYTES: u64 = 500 * 1024 * 1024;

/// Live, process-global cache size budget in bytes. Changing it via
/// `set_budget_mb` never triggers an eviction sweep itself — it only
/// changes what the *next* `maybe_evict`-triggered sweep evicts down to, so
/// shrinking the budget doesn't cause an immediate mass deletion.
static CACHE_BUDGET_BYTES: AtomicU64 = AtomicU64::new(DEFAULT_CACHE_BUDGET_BYTES);

/// Update the live cache budget. Does not evict anything itself — see
/// `CACHE_BUDGET_BYTES`'s doc comment.
pub fn set_budget_mb(mb: u64) {
    CACHE_BUDGET_BYTES.store(mb.saturating_mul(1024 * 1024), Ordering::Relaxed);
}

fn current_budget_bytes() -> u64 {
    CACHE_BUDGET_BYTES.load(Ordering::Relaxed)
}
```

Update `maybe_evict` to read the live budget instead of the old constant:

```rust
fn maybe_evict(dir: &Path) {
    let count = WRITES_SINCE_EVICTION.fetch_add(1, Ordering::Relaxed) + 1;
    if count.is_multiple_of(WRITES_BETWEEN_EVICTION_CHECKS) {
        evict_if_over_budget(dir, current_budget_bytes());
    }
}
```

- [ ] **Step 4: Add `cache_budget_mb` to `AppSettings`**

In `src/settings_store.rs`, update the struct:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    pub api_server: ApiServerChoice,
    pub custom_api_url: String,
    /// OAuth client IDs, keyed by OAuth base URL (see `auth::oauth_base_for`). OSM's
    /// primary and dev instances have entirely separate app registrations, so a
    /// client_id registered on one is unknown to the other; this lets each server use
    /// its own registered app instead of sharing a single hardcoded client_id.
    #[serde(default)]
    pub client_ids: HashMap<String, String>,
    /// Tile cache size budget, in megabytes. Defaults to 500 for
    /// `settings.json` files written before this field existed.
    #[serde(default = "default_cache_budget_mb")]
    pub cache_budget_mb: u64,
}

fn default_cache_budget_mb() -> u64 {
    500
}
```

Update `Default for AppSettings`:

```rust
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            api_server: ApiServerChoice::Primary,
            custom_api_url: String::new(),
            client_ids: HashMap::new(),
            cache_budget_mb: default_cache_budget_mb(),
        }
    }
}
```

Fix the existing test struct literals that construct `AppSettings` directly
(they'll otherwise fail to compile with "missing field `cache_budget_mb`"):
in `round_trip`, add `cache_budget_mb: 500,`; in `base_url_matches_choice`,
add `cache_budget_mb: 500,` to each of its three `AppSettings { ... }`
literals.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib tile_cache::tests::set_budget_mb settings_store::tests`
Expected: PASS — all `settings_store` tests (existing + 2 new) and the new
`tile_cache` budget test.

- [ ] **Step 6: Seed the budget at startup in `main.rs`**

Find the existing lines:

```rust
// Load persisted app settings (OSM API server choice) and OAuth login.
settings_store::init_store(settings_store::load());
auth::init_store(auth::load());
```

Replace with:

```rust
// Load persisted app settings (OSM API server choice, cache budget) and
// OAuth login. Seed the tile cache's live budget from the persisted value
// before the cache does any work.
let app_settings = settings_store::load();
crate::tile_cache::set_budget_mb(app_settings.cache_budget_mb);
settings_store::init_store(app_settings);
auth::init_store(auth::load());
```

- [ ] **Step 7: Run the full test suite and build**

Run: `cargo build && cargo test --lib`
Expected: builds cleanly; all tests pass.

- [ ] **Step 8: Format and commit**

```bash
cargo fmt
git add src/tile_cache.rs src/settings_store.rs src/main.rs
git commit -m "Make tile cache budget configurable via AppSettings.cache_budget_mb"
```

---

## Task 6: Settings UI — "Imagery" page with Custom Sources and Tile Cache groups

**Files:**
- Modify: `src/ui/settings_window.rs` (rename the page, split its group, add the new Tile Cache group)

**Interfaces:**
- Consumes: `crate::tile_cache::{cache_summary, clear_source, clear_all_cache, format_bytes, set_budget_mb}` (Tasks 4-5), `settings_store::{update_store}` and `AppSettings.cache_budget_mb` (Task 5).
- Produces: nothing consumed by other tasks — this is the final task.

There is no existing unit-test scaffolding for `settings_window.rs` (it has
no `#[cfg(test)]` module today), so this task's verification is a
successful build plus manual verification in the running app, consistent
with how the rest of this file's UI code is treated.

- [ ] **Step 1: Add new `SettingsWindow` state**

In `src/ui/settings_window.rs`, add three fields to the `SettingsWindow`
struct (after `client_id_input`):

```rust
    edit_cache_budget: Entity<InputState>,
    cache_budget_error: Option<SharedString>,
    cache_clear_error: Option<SharedString>,
```

In `SettingsWindow::new`, after the `client_id_input` construction, add:

```rust
        let edit_cache_budget = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("500")
                .default_value(app_settings.cache_budget_mb.to_string())
        });
```

Add the three new fields to the `Self { ... }` literal:

```rust
            edit_cache_budget,
            cache_budget_error: None,
            cache_clear_error: None,
```

- [ ] **Step 2: Add the budget-save and cache-clear methods**

Add to `impl SettingsWindow` (near `save_client_id`):

```rust
    fn save_cache_budget(&mut self, cx: &mut Context<Self>) {
        let raw = self.edit_cache_budget.read(cx).value().trim().to_string();
        match raw.parse::<u64>() {
            Ok(mb) if mb >= 10 => {
                self.cache_budget_error = None;
                self.app_settings.cache_budget_mb = mb;
                settings_store::update_store(self.app_settings.clone());
                crate::tile_cache::set_budget_mb(mb);
            }
            Ok(_) => {
                self.cache_budget_error = Some("Budget must be at least 10 MB".into());
            }
            Err(_) => {
                self.cache_budget_error = Some("Enter a whole number of megabytes".into());
            }
        }
        cx.notify();
    }

    fn clear_cache_source(&mut self, key: String, cx: &mut Context<Self>) {
        if let Err(e) = crate::tile_cache::clear_source(&key) {
            self.cache_clear_error = Some(format!("Failed to clear {}: {}", key, e).into());
        } else {
            self.cache_clear_error = None;
        }
        cx.notify();
    }

    fn clear_all_tile_cache(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = crate::tile_cache::clear_all_cache() {
            self.cache_clear_error = Some(format!("Failed to clear cache: {}", e).into());
        } else {
            self.cache_clear_error = None;
        }
        cx.notify();
    }
```

(Named `clear_all_tile_cache` rather than `clear_all_cache` to avoid
shadowing/confusion with `crate::tile_cache::clear_all_cache`.)

- [ ] **Step 3: Rename the page and add the Tile Cache group**

Replace the tail end of `imagery_page` — the final `SettingPage::new(...)`
expression — from:

```rust
        SettingPage::new("Imagery Sources")
            .icon(Icon::new(IconName::Map))
            .group(
                SettingGroup::new()
                    .title("Custom Imagery Sources")
                    .items(items),
            )
    }
```

to:

```rust
        SettingPage::new("Imagery")
            .icon(Icon::new(IconName::Map))
            .groups(vec![
                SettingGroup::new().title("Custom Sources").items(items),
                self.cache_group(view),
            ])
    }

    fn cache_group(&self, view: Entity<Self>) -> SettingGroup {
        let summary = crate::tile_cache::cache_summary();

        let summary_text: SharedString = format!(
            "{} across {} tile{} in {} source{}",
            crate::tile_cache::format_bytes(summary.total_bytes),
            summary.total_files,
            if summary.total_files == 1 { "" } else { "s" },
            summary.sources.len(),
            if summary.sources.len() == 1 { "" } else { "s" },
        )
        .into();

        let clear_error = self.cache_clear_error.clone();
        let mut items = vec![SettingItem::render(move |_options, _window, cx| {
            render_cache_usage_summary(summary_text.clone(), clear_error.clone(), cx)
        })];

        let budget_view = view.clone();
        let budget_input = self.edit_cache_budget.clone();
        let budget_error = self.cache_budget_error.clone();
        items.push(
            SettingItem::new(
                "Cache budget (MB)",
                SettingField::render(move |_options, window, cx| {
                    render_cache_budget(
                        budget_view.clone(),
                        budget_input.clone(),
                        budget_error.clone(),
                        window,
                        cx,
                    )
                }),
            )
            .description(
                "Maximum on-disk tile cache size. Lowering this doesn't delete anything \
                 immediately — the cache shrinks to the new limit the next time it's \
                 written to.",
            )
            .layout(Axis::Vertical),
        );

        if summary.sources.is_empty() {
            items.push(SettingItem::render(|_options, _window, _cx| {
                Label::new("No cached tiles yet.")
            }));
        }

        for source in summary.sources {
            let row_view = view.clone();
            let key = source.key.clone();
            let size_label: SharedString = format!(
                "{} · {} tile{}",
                crate::tile_cache::format_bytes(source.bytes),
                source.file_count,
                if source.file_count == 1 { "" } else { "s" },
            )
            .into();
            items.push(
                SettingItem::new(
                    source.key.clone(),
                    SettingField::render(move |_options, _window, _cx| {
                        render_cache_source_row(row_view.clone(), key.clone())
                    }),
                )
                .description(size_label),
            );
        }

        let clear_all_view = view;
        items.push(SettingItem::render(move |_options, _window, _cx| {
            let clear_all_view = clear_all_view.clone();
            Button::new("clear-all-cache")
                .label("Clear All")
                .ghost()
                .compact()
                .on_click(move |_ev, _window, cx| {
                    clear_all_view.update(cx, |this, cx| this.clear_all_tile_cache(cx));
                })
        }));

        SettingGroup::new().title("Tile Cache").items(items)
    }
}
```

Note the closing `}` above ends `impl SettingsWindow` — make sure you're
replacing the correct span (the end of `imagery_page`'s body through the
`impl SettingsWindow`'s closing brace) rather than duplicating it.

- [ ] **Step 4: Add the render helper functions**

Add these free functions near the existing `render_entry_row`/
`render_client_id` helpers (after `render_client_id`, for example):

```rust
fn render_cache_usage_summary(
    summary_text: SharedString,
    clear_error: Option<SharedString>,
    cx: &mut App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;
    let mut col = v_flex()
        .gap_1()
        .child(Label::new(summary_text).text_sm().text_color(muted));
    if let Some(err) = clear_error {
        col = col.child(Label::new(err).text_sm().text_color(danger));
    }
    col.into_any_element()
}

fn render_cache_budget(
    view: Entity<SettingsWindow>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    _window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    let mut row = v_flex().gap_2().child(field_row("Megabytes", &input, muted));
    if let Some(err) = error {
        row = row.child(Label::new(err).text_sm().text_color(danger));
    }
    row.child(
        Button::new("save-cache-budget")
            .label("Save")
            .primary()
            .compact()
            .on_click(move |_ev, _window, cx| {
                view.update(cx, |this, cx| this.save_cache_budget(cx));
            }),
    )
}

fn render_cache_source_row(view: Entity<SettingsWindow>, key: String) -> AnyElement {
    let button_id = format!("clear-cache-source-{key}");
    Button::new(button_id)
        .label("Clear")
        .ghost()
        .compact()
        .on_click(move |_ev, _window, cx| {
            view.update(cx, |this, cx| this.clear_cache_source(key.clone(), cx));
        })
        .into_any_element()
}
```

If `Button::new(button_id)` fails to compile because `String` doesn't
satisfy `Button::new`'s `impl Into<ElementId>` bound, wrap it as
`Button::new(SharedString::from(button_id))` instead — check how
`gpui_component::button::Button::new`'s signature is declared (or how
other dynamically-generated ids are built elsewhere in this codebase) if
neither works.

- [ ] **Step 5: Build and fix any compile errors**

Run: `cargo build`
Expected: PASS. Fix any type mismatches following the existing patterns in
this file (e.g. `render_entry_row`/`render_custom_api_url` for the general
shape of a `View`-taking render helper).

- [ ] **Step 6: Run the full test suite**

Run: `cargo test --lib`
Expected: PASS — no test in this crate directly exercises
`settings_window.rs`, so this is confirming the refactor didn't break
anything elsewhere.

- [ ] **Step 7: Manually verify in the running app**

Launch the app (see this project's `run` skill / existing osmscript-based
UI-verification approach) and open Settings:

- Confirm the sidebar/page now reads "Imagery" (not "Imagery Sources"),
  containing two groups: "Custom Sources" (existing add/edit/delete rows,
  unchanged) and "Tile Cache".
- Confirm "Tile Cache" shows a usage summary line, a "Cache budget (MB)"
  field pre-filled with the current value (500 by default), one row per
  on-disk source directory (or "No cached tiles yet." if the cache is
  empty) each with a "Clear" button, and a "Clear All" button.
- Change the budget field to a value below 10 and click Save — confirm the
  inline error appears and nothing is persisted.
- Change it to a valid value (e.g. 200) and click Save — confirm no error,
  and check `~/Library/Caches/osm-gpui/settings.json` (or platform
  equivalent) to confirm `cache_budget_mb` was updated.
- Pan/zoom the map to load some tiles, reopen Settings, and confirm a
  source row appears with nonzero size; click its "Clear" button and
  confirm the corresponding subdirectory under
  `~/Library/Caches/osm-gpui/tiles/` is removed and the row disappears
  (or the summary updates) on next open.

- [ ] **Step 8: Format and commit**

```bash
cargo fmt
git add src/ui/settings_window.rs
git commit -m "Add Tile Cache group to Imagery settings page"
```

---

## Self-Review Notes

- **Spec coverage:** page rename (Task 6), Custom Sources group (Task 6),
  Tile Cache group with usage + per-source rows + clear buttons (Task 6),
  per-source cache directories (Task 2), configurable budget defaulting to
  500 with no eager eviction on shrink (Tasks 3, 5), `{switch:...}`-safe
  source keys (Task 1) — all spec sections are covered.
- **Type consistency:** `TileAssetKey`, `CacheSourceUsage`, `CacheSummary`,
  `source_key_for_template`, `cache_summary`/`clear_source`/
  `clear_all_cache`/`format_bytes`/`set_budget_mb` are used with identical
  names and signatures everywhere they're referenced across tasks.
- **No placeholders:** every step has concrete code; the one conditional
  fallback (Task 6 Step 4's `Button::new` id note) is a concrete fallback
  instruction, not an unresolved TODO.
