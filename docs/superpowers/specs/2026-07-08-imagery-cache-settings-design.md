# Imagery settings: rename to "Imagery", add a Tile Cache panel

## Problem

The settings window's "Imagery Sources" page only manages custom imagery
source definitions. There is no way to see how much disk space the tile
cache is using, no way to clear it (globally or per source), and no way to
change the 500MB eviction budget — it's a hardcoded constant.

## Goals

- Rename the "Imagery Sources" settings page to "Imagery".
- Move the existing custom-imagery management UI into a "Custom Sources"
  group within that page (no behavior change).
- Add a "Tile Cache" group to the same page showing total cache usage and a
  per-source breakdown, with the ability to clear an individual source's
  cache or clear everything.
- Let the user configure the cache size budget (MB), defaulting to 500,
  without immediately deleting anything when the budget shrinks — the new
  budget just takes effect at the next normal eviction sweep.

## Non-goals

- Reconciling cache directories back to friendly source names (e.g. "Bing
  Aerial imagery"). Rows are identified by their on-disk source key, which is
  a sanitized slug derived from the source's URL template — readable, but
  not a resolved display name.
- Per-source cache *budgets*. There is one global byte budget across all
  sources, same as today.
- Confirmation dialogs before clearing. Cache data is disposable and
  re-downloadable, unlike deleting a saved custom source.

## Architecture

### Per-source cache directories (`src/tile_cache.rs`)

Today every tile lands in one flat directory keyed only by a hash of its
resolved URL: `tiles/tile_<sha256(url)>.png`. This makes it impossible to
attribute disk usage to a specific imagery source or clear one source
without clearing everything.

The cache directory becomes `tiles/<source_key>/tile_<sha256(url)>.png`,
where `source_key` is derived from the *layer's URL template*
(`https://tile.openstreetmap.org/{z}/{x}/{y}.png`), not the resolved tile
URL. The template is used (rather than the resolved URL) because a custom
source using `{switch:a,b,c}` subdomain rotation resolves to different
hosts per tile; hashing the resolved URL would incorrectly split one
source's tiles across multiple buckets.

`source_key` is computed once, at `TileLayer` construction, by a new
function:

```rust
fn source_key_for_template(template: &str) -> String
```

It produces a sanitized, human-legible slug from the template's host and
path skeleton (numeric/placeholder path segments collapsed, non-alphanumeric
characters replaced), with a short hash suffix appended to guarantee
uniqueness even if two different templates happen to sanitize to the same
skeleton. Example: `tile.openstreetmap.org_z_x_y-a1b2c3d4`.

`TileAsset::Source` changes from a bare `String` to:

```rust
pub struct TileAssetKey {
    pub url: String,
    pub source_key: String,
}
```

(deriving `Clone, Hash, Eq, PartialEq`, as required by GPUI's `Asset`
trait). `TileAsset::load` uses `key.source_key` to pick the subdirectory and
`cache_filename(&key.url)` (unchanged) for the leaf filename. The two
`img(...)` call sites in `TileLayer::render_elements`
(`src/layers/tile_layer.rs`) construct a `TileAssetKey` using the layer's
stored `source_key` field instead of passing a bare URL.

Loose files that end up directly under `tiles/` (any pre-existing cache from
before this change, or any future stray write) are not attributed to a
source. They're reported as a synthetic `"(uncategorized)"` entry in cache
summaries and are removed by "Clear All" along with everything else.

### Eviction stays behavior-compatible, budget becomes dynamic

`evict_if_over_budget` is updated to walk one level of subdirectories (plus
any loose top-level files) instead of a single flat directory, summing sizes
and deleting oldest-by-mtime files first across the *whole* tree — same
oldest-first policy as today, just scoped over more directories. There is
still one global budget, not a per-source budget.

The budget itself stops being the hardcoded `MAX_CACHE_BYTES` constant and
becomes a live, process-global value:

```rust
static CACHE_BUDGET_BYTES: AtomicU64 = AtomicU64::new(DEFAULT_CACHE_BUDGET_BYTES);

pub fn set_budget_mb(mb: u64);
```

(mirroring the existing `TILE_IDLE_TRACKER` global pattern already used in
this file). `DEFAULT_CACHE_BUDGET_BYTES` stays `500 * 1024 * 1024`.
`evict_if_over_budget` reads the current value of `CACHE_BUDGET_BYTES` at
the moment it runs.

Eviction only ever runs as a side effect of `maybe_evict`, which fires every
`WRITES_BETWEEN_EVICTION_CHECKS` (25) tile writes. Lowering the budget via
`set_budget_mb` does **not** trigger an eviction sweep itself — it just
updates the atomic. The next normal write-triggered sweep (whenever it
naturally happens) reads the new, smaller budget and evicts down to it. This
means shrinking the budget from, say, 500MB to 100MB while the cache holds
300MB does not cause an immediate mass deletion; the cache drains down to
the new budget gradually as normal tile traffic continues, exactly matching
the requirement that shrinking the budget "just takes it into account for
the next normal cache reaping."

### New query/mutation API for the settings UI

```rust
pub struct CacheSourceUsage {
    pub key: String,      // source_key, or "(uncategorized)"
    pub bytes: u64,
    pub file_count: usize,
}

pub struct CacheSummary {
    pub total_bytes: u64,
    pub total_files: usize,
    pub sources: Vec<CacheSourceUsage>, // sorted by bytes, descending
}

pub fn cache_summary() -> CacheSummary;
pub fn clear_source(key: &str) -> std::io::Result<()>;
pub fn clear_all_cache() -> std::io::Result<()>;
```

`cache_summary()` does a full recursive directory scan on demand — this is
fine because it's only called when the settings window's Tile Cache group is
rendered (opening settings is rare), unlike the existing 1-second-TTL-cached
`cached_file_count()` used on the hot per-frame layer-stats path, which is
untouched by this change.

`clear_source` removes that one subdirectory; `clear_all_cache` removes the
whole `tiles/` directory (recreated lazily on the next tile write, as
happens today when the directory doesn't exist yet).

### Cache budget setting (`src/settings_store.rs`)

`AppSettings` gains a new field:

```rust
#[serde(default = "default_cache_budget_mb")]
pub cache_budget_mb: u64,
```

with `default_cache_budget_mb() -> u64 { 500 }`, so existing `settings.json`
files without this field continue to load correctly.

At startup (`main.rs`, alongside the existing `settings_store::init_store`
call, before `TileCache`'s first use), the loaded `AppSettings.cache_budget_mb`
seeds `tile_cache::set_budget_mb(...)` so the process-global atomic reflects
the persisted setting from the start.

## UI (`src/ui/settings_window.rs`)

- `imagery_page` is renamed in title only: `SettingPage::new("Imagery")`
  (was `"Imagery Sources"`). Icon unchanged.
- The page's single group becomes two groups:
  - **"Custom Sources"** — the existing add/edit/delete rows and "Add
    Source" button, unchanged in every other respect.
  - **"Tile Cache"** (new) — built by a new free function following the
    existing `render_entry_row`-style pattern:
    - A summary line: total size (human-readable, e.g. "342 MB across 5
      sources") and total file count, from `tile_cache::cache_summary()`.
    - A "Cache budget (MB)" field: a plain `InputState`
      (`self.edit_cache_budget: Entity<InputState>`, initialized from
      `self.app_settings.cache_budget_mb.to_string()`), laid out via the
      existing `field_row` helper. Saved the same way
      `save_custom_api_url`/`save_client_id` are today: parse as `u64`,
      reject non-numeric or values under a 10MB floor (surfaced via the
      existing `self.edit_error` label), then on success mutate
      `self.app_settings.cache_budget_mb`, call
      `settings_store::update_store(self.app_settings.clone())`, and call
      `tile_cache::set_budget_mb(...)`.
    - One row per `CacheSourceUsage`: the `key` string, formatted size, file
      count, and a "Clear" button that calls `tile_cache::clear_source(key)`
      then re-renders (recomputing `cache_summary()`).
    - A "Clear All" button at the bottom of the group calling
      `tile_cache::clear_all_cache()`.
    - No confirmation dialog on either Clear action (see Non-goals).

## Data flow summary

1. App startup: `AppSettings` loads from disk → `cache_budget_mb` seeds
   `tile_cache`'s global budget atomic.
2. Tile fetch: `TileLayer` builds a `TileAssetKey{url, source_key}` per
   tile → `TileAsset::load` reads/writes
   `tiles/<source_key>/tile_<hash>.png` → every 25th write triggers
   `evict_if_over_budget`, which reads the *current* budget atomic.
3. Settings window open → Tile Cache group calls `cache_summary()` (fresh
   scan) → renders per-source rows.
4. User edits budget field → validated → `AppSettings` persisted +
   `tile_cache::set_budget_mb` updates the atomic immediately; no eviction
   runs until the next natural write-triggered sweep.
5. User clicks Clear (source or all) → `tile_cache::clear_source`/
   `clear_all_cache` runs synchronously → UI re-fetches `cache_summary()`.

## Error handling

- `clear_source`/`clear_all_cache` return `std::io::Result<()>`; a failure
  (e.g. a file in use) is surfaced as an inline error label in the Tile
  Cache group, same visual treatment as `self.edit_error` elsewhere on this
  page. It does not block the rest of the settings window.
- Invalid budget input (non-numeric, empty, below the 10MB floor) is
  rejected before touching `AppSettings` or the atomic, with an inline error
  message — same pattern as zoom-field validation in the custom-imagery
  editor.
- A directory that disappears mid-scan (e.g. cleared from another process)
  is simply skipped in `cache_summary()`'s walk, not treated as a hard
  error — consistent with `evict_if_over_budget`'s existing
  treat-unreadable-as-benign approach.

## Testing

- `tile_cache.rs` unit tests (extending the existing `#[cfg(test)] mod
  tests` in that file, which already uses temp directories for eviction
  tests):
  - `source_key_for_template` is deterministic for the same template.
  - Different templates produce different keys.
  - `{switch:a,b,c}`-style templates produce one stable key independent of
    which resolved URL is hashed for the filename.
  - `cache_summary()` correctly totals bytes/files across multiple
    source-key subdirectories plus an uncategorized top-level file.
  - `clear_source` removes only the targeted subdirectory, leaving others
    intact.
  - `clear_all_cache` removes everything, including uncategorized loose
    files.
  - `evict_if_over_budget` still evicts oldest-first when scanning multiple
    subdirectories (extends the existing single-directory eviction test).
  - `set_budget_mb` updates what `evict_if_over_budget`'s default-budget
    caller reads, without itself deleting anything.
- No new UI/integration test coverage is planned beyond the existing
  osmscript-based settings-window checks, if any apply; this is a settings
  panel change verifiable by manual inspection per this project's existing
  `verify`/`run` skill workflow.
