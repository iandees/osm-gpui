# NSI-style Presets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user search the Name Suggestion Index (NSI) by brand name and apply a matched brand's tags (name, brand, brand:wikidata, shop/amenity, ...) to the single currently-selected OSM node or way, with full undo/redo support.

**Architecture:** A new `src/nsi.rs` module owns the NSI data model, JSON parsing, on-disk caching (mirroring `src/imagery/mod.rs`'s ELI fetch/cache pattern), and a small global store analogous to `settings_store`'s `APP_SETTINGS`. A background fetch is kicked off at app startup the same way the Editor Layer Index is today. A new `src/ui/nsi_dialog.rs` dialog (modeled on `src/ui/custom_imagery_dialog.rs`) provides a live-search text box + result list. `MapViewer` wires an `Edit > Apply NSI Preset…` menu item, entity-level action handler (same pattern as `on_undo`/`on_redo`), and a new `UndoableAction::SetTags` variant that reuses the existing `apply_undo_action`/`UndoStack` machinery. A new `OsmLayer::commit_tag_change` (mirroring `commit_node_moves`) performs the actual tag mutation.

**Tech Stack:** Rust, GPUI + gpui-component (`Input`/`InputState`, `Button`, `v_flex`), `serde_json` (loose `Value`-based parsing), `ureq` (HTTP fetch), `dirs` (cache directory), `anyhow` (fetch error type).

## Global Constraints

- No NSI data is bundled with the app binary or repo. The index is only ever populated by a background fetch at runtime, cached under `dirs::cache_dir()/osm-gpui/nsi.json`, refreshed if missing or older than 7 days (matches `src/imagery/mod.rs`'s `CACHE_TTL`).
- No `locationSet` (country/region) filtering in v1 — every parsed NSI entry is offered regardless of location. This is a known, documented limitation, not a bug.
- Only single-feature selection is supported — applying a preset requires `self.selected.len() == 1`. Multi-select is out of scope for v1.
- Tag merge on apply: existing tags on the feature are preserved; for any key the preset also specifies, the preset's value wins (overwrite). This is not a full tag replace.
- No general tag editor is being built here — this is preset-apply only. Editing/removing individual tags outside of this flow remains unsupported.
- Upstream data source: `https://raw.githubusercontent.com/osmlab/name-suggestion-index/main/dist/nsi.json`.

---

### Task 1: NSI data model, parsing, and search ranking

**Files:**
- Create: `src/nsi.rs`
- Modify: `src/lib.rs` (add `pub mod nsi;`)
- Test: inline `#[cfg(test)] mod tests` in `src/nsi.rs`

**Interfaces:**
- Produces: `pub struct NsiEntry { pub display_name: String, pub tags: std::collections::HashMap<String, String>, pub match_names: Vec<String> }` (derives `Debug, Clone`), `pub struct NsiIndex { entries: Vec<NsiEntry> }` with `impl NsiIndex { pub fn from_entries(entries: Vec<NsiEntry>) -> Self; pub fn len(&self) -> usize; pub fn search(&self, query: &str, limit: usize) -> Vec<&NsiEntry> }`, and `pub fn parse(body: &str) -> Vec<NsiEntry>`.
- Consumes: nothing from other tasks (this is the foundation task).

- [ ] **Step 1: Write the failing tests for parsing and search**

Create `src/nsi.rs` with just the types, an empty `parse`/`search`, and this test module:

```rust
//! Name Suggestion Index (NSI) brand-preset support.
//!
//! Downloads, caches, and parses the upstream `dist/nsi.json` build from
//! <https://github.com/osmlab/name-suggestion-index> into a searchable list
//! of brand name -> tag-set entries ("presets"). No `locationSet` (country)
//! filtering is applied — every entry is offered regardless of location.

use std::collections::HashMap;

/// One NSI brand entry: a display name, its aliases, and the tags to apply.
#[derive(Debug, Clone)]
pub struct NsiEntry {
    pub display_name: String,
    pub tags: HashMap<String, String>,
    pub match_names: Vec<String>,
}

/// A searchable collection of `NsiEntry` values.
pub struct NsiIndex {
    entries: Vec<NsiEntry>,
}

impl NsiIndex {
    pub fn from_entries(entries: Vec<NsiEntry>) -> Self {
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Search by normalized substring match against `display_name` or any
    /// `match_names` alias. Prefix matches rank before other substring
    /// matches; ties break by shorter `display_name` first. Empty query
    /// returns no results. Capped at `limit` results.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&NsiEntry> {
        let q = normalize(query);
        if q.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(u8, usize, &NsiEntry)> = self
            .entries
            .iter()
            .filter_map(|e| {
                let candidates = std::iter::once(e.display_name.as_str())
                    .chain(e.match_names.iter().map(|s| s.as_str()));
                let mut best: Option<u8> = None;
                for c in candidates {
                    let nc = normalize(c);
                    if nc.starts_with(&q) {
                        best = Some(0);
                        break;
                    }
                    if nc.contains(&q) {
                        best = best.max(Some(1));
                    }
                }
                best.map(|rank| (rank, e.display_name.len(), e))
            })
            .collect();

        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        scored.into_iter().take(limit).map(|(_, _, e)| e).collect()
    }
}

/// Lowercase and strip everything but ASCII alphanumerics, so "Mc Donald's"
/// and "mcdonalds" compare equal. Upstream `matchNames` are already close to
/// this form; normalizing both sides keeps user input forgiving too.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Parse the upstream `dist/nsi.json` body into a flat list of entries.
/// Tolerant of missing/unexpected fields: skips any item missing
/// `displayName` or `tags`. Ignores `locationSet` (no geo-filtering in v1).
pub fn parse(body: &str) -> Vec<NsiEntry> {
    let root: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("nsi: failed to parse nsi.json: {}", e);
            return Vec::new();
        }
    };

    let Some(categories) = root.get("nsi").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for category in categories.values() {
        let Some(items) = category.get("items").and_then(|v| v.as_array()) else {
            continue;
        };
        for item in items {
            if let Some(entry) = parse_item(item) {
                out.push(entry);
            }
        }
    }
    out
}

fn parse_item(item: &serde_json::Value) -> Option<NsiEntry> {
    let display_name = item.get("displayName")?.as_str()?.to_string();
    let tags_obj = item.get("tags")?.as_object()?;
    let mut tags = HashMap::with_capacity(tags_obj.len());
    for (k, v) in tags_obj {
        if let Some(s) = v.as_str() {
            tags.insert(k.clone(), s.to_string());
        }
    }
    if tags.is_empty() {
        return None;
    }
    let match_names = item
        .get("matchNames")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    Some(NsiEntry {
        display_name,
        tags,
        match_names,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
    {
      "nsi": {
        "brands/amenity/fast_food": {
          "properties": {"path": "brands/amenity/fast_food"},
          "items": [
            {
              "displayName": "McDonald's",
              "id": "mcdonalds-1",
              "tags": {
                "amenity": "fast_food",
                "name": "McDonald's",
                "brand": "McDonald's",
                "brand:wikidata": "Q38076"
              },
              "matchNames": ["mcdonalds", "mickey d's"]
            },
            {
              "displayName": "McDonaldland Toys",
              "id": "no-tags-1",
              "tags": {}
            },
            {
              "displayName": "Missing Tags Entry",
              "id": "missing-tags-1"
            }
          ]
        },
        "brands/amenity/cafe": {
          "properties": {"path": "brands/amenity/cafe"},
          "items": [
            {
              "displayName": "Starbucks",
              "id": "starbucks-1",
              "tags": {
                "amenity": "cafe",
                "name": "Starbucks",
                "brand": "Starbucks",
                "brand:wikidata": "Q37158"
              },
              "matchNames": ["starbucks coffee"]
            }
          ]
        }
      }
    }
    "#;

    #[test]
    fn parse_extracts_valid_entries_and_skips_tagless_ones() {
        let entries = parse(FIXTURE);
        assert_eq!(entries.len(), 2, "expected McDonald's and Starbucks only");
        assert!(entries.iter().any(|e| e.display_name == "McDonald's"));
        assert!(entries.iter().any(|e| e.display_name == "Starbucks"));
    }

    #[test]
    fn parse_captures_tags_and_match_names() {
        let entries = parse(FIXTURE);
        let mcd = entries
            .iter()
            .find(|e| e.display_name == "McDonald's")
            .unwrap();
        assert_eq!(mcd.tags.get("amenity"), Some(&"fast_food".to_string()));
        assert_eq!(mcd.tags.get("brand:wikidata"), Some(&"Q38076".to_string()));
        assert_eq!(mcd.match_names, vec!["mcdonalds", "mickey d's"]);
    }

    #[test]
    fn parse_malformed_json_returns_empty() {
        assert!(parse("not json").is_empty());
    }

    #[test]
    fn search_matches_display_name_case_insensitively() {
        let index = NsiIndex::from_entries(parse(FIXTURE));
        let results = index.search("starbucks", 30);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display_name, "Starbucks");
    }

    #[test]
    fn search_matches_via_match_names() {
        let index = NsiIndex::from_entries(parse(FIXTURE));
        let results = index.search("mickey d", 30);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display_name, "McDonald's");
    }

    #[test]
    fn search_prefix_ranks_before_substring() {
        let entries = vec![
            NsiEntry {
                display_name: "The Coffee Bean".to_string(),
                tags: HashMap::from([("shop".to_string(), "coffee".to_string())]),
                match_names: vec![],
            },
            NsiEntry {
                display_name: "Coffee Bean & Tea Leaf".to_string(),
                tags: HashMap::from([("shop".to_string(), "coffee".to_string())]),
                match_names: vec![],
            },
        ];
        let index = NsiIndex::from_entries(entries);
        let results = index.search("coffee", 30);
        assert_eq!(results.len(), 2);
        // "Coffee Bean & Tea Leaf" starts with "coffee" (prefix); "The Coffee
        // Bean" only contains it -> prefix match ranks first.
        assert_eq!(results[0].display_name, "Coffee Bean & Tea Leaf");
    }

    #[test]
    fn search_caps_at_limit() {
        let entries: Vec<NsiEntry> = (0..50)
            .map(|i| NsiEntry {
                display_name: format!("Brand {}", i),
                tags: HashMap::from([("shop".to_string(), "yes".to_string())]),
                match_names: vec![],
            })
            .collect();
        let index = NsiIndex::from_entries(entries);
        assert_eq!(index.search("brand", 30).len(), 30);
    }

    #[test]
    fn search_empty_query_returns_nothing() {
        let index = NsiIndex::from_entries(parse(FIXTURE));
        assert!(index.search("", 30).is_empty());
    }

    #[test]
    fn search_no_match_returns_empty() {
        let index = NsiIndex::from_entries(parse(FIXTURE));
        assert!(index.search("totally-not-a-brand-xyz", 30).is_empty());
    }
}
```

- [ ] **Step 2: Register the module**

In `src/lib.rs`, add `pub mod nsi;` alphabetically among the other `pub mod` lines (between `pub mod layers;` and `pub mod osm;`):

```rust
pub mod layers;
pub mod nsi;
pub mod osm;
```

- [ ] **Step 3: Run the tests and verify they pass**

Run: `cargo test --lib nsi::`
Expected: all 9 tests in `nsi::tests` PASS.

- [ ] **Step 4: Commit**

```bash
git add src/nsi.rs src/lib.rs
git commit -m "Add NSI data model, JSON parsing, and search ranking"
```

---

### Task 2: NSI background fetch, on-disk cache, and global store

**Files:**
- Modify: `src/nsi.rs` (append fetch/cache/store code below the existing content)
- Test: same `#[cfg(test)] mod tests` block in `src/nsi.rs`

**Interfaces:**
- Consumes: `NsiEntry`, `NsiIndex::from_entries`, `parse` from Task 1.
- Produces: `pub fn fetch_and_cache() -> anyhow::Result<String>`, `pub fn init_store()`, `pub fn set_index(index: NsiIndex)`, `pub fn current() -> Option<std::sync::Arc<NsiIndex>>`. `main.rs` (Task 6) calls `init_store()` once at startup and, in a background task, `fetch_and_cache()` -> `parse()` -> `set_index()`.

- [ ] **Step 1: Write the failing cache-staleness tests**

Add to the bottom of `src/nsi.rs` (above the existing `mod tests` closing brace — i.e. add these as new `#[test]` functions inside the same `mod tests` block, plus the non-test code above it):

```rust
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

const NSI_URL: &str = "https://raw.githubusercontent.com/osmlab/name-suggestion-index/main/dist/nsi.json";
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60); // 7 days

/// Fetch the NSI dist build (on-disk cache with a 7-day TTL, same policy as
/// `crate::imagery::fetch_and_cache`). On network failure, falls back to any
/// existing cache file regardless of age.
pub fn fetch_and_cache() -> anyhow::Result<String> {
    let cache_path = cache_file_path();
    if let Some(body) = read_fresh_cache(&cache_path) {
        return Ok(body);
    }

    match download() {
        Ok(body) => {
            if let Some(parent) = cache_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&cache_path, &body);
            Ok(body)
        }
        Err(e) => {
            if let Ok(body) = fs::read_to_string(&cache_path) {
                eprintln!("nsi: fetch failed ({}), using stale cache", e);
                Ok(body)
            } else {
                Err(e)
            }
        }
    }
}

fn cache_file_path() -> PathBuf {
    dirs::cache_dir()
        .map(|d| d.join("osm-gpui").join("nsi.json"))
        .unwrap_or_else(|| std::env::temp_dir().join("osm-gpui-nsi.json"))
}

fn read_fresh_cache(path: &PathBuf) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(mtime).ok()?;
    if age > CACHE_TTL {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn download() -> anyhow::Result<String> {
    let response = ureq::get(NSI_URL)
        .set(
            "User-Agent",
            "osm-gpui/0.1.0 (https://github.com/iandees/osm-gpui)",
        )
        .timeout(Duration::from_secs(60))
        .call()?;

    let mut body = String::new();
    response.into_reader().read_to_string(&mut body)?;
    Ok(body)
}

/// Global in-memory index, populated by a background fetch at startup.
/// `None` until the first successful parse completes.
static NSI_INDEX: OnceLock<Arc<Mutex<Option<Arc<NsiIndex>>>>> = OnceLock::new();

/// Initialize the global store. Call once at startup, before any background
/// fetch is spawned.
pub fn init_store() {
    let _ = NSI_INDEX.set(Arc::new(Mutex::new(None)));
}

/// Replace the current in-memory index (called once the background fetch +
/// parse completes).
pub fn set_index(index: NsiIndex) {
    if let Some(store) = NSI_INDEX.get() {
        if let Ok(mut guard) = store.lock() {
            *guard = Some(Arc::new(index));
        }
    }
}

/// The current index, if the background fetch has completed. `None` means
/// still loading (or `init_store` was never called).
pub fn current() -> Option<Arc<NsiIndex>> {
    NSI_INDEX.get()?.lock().ok()?.clone()
}
```

Then add these test functions inside the existing `mod tests { use super::*; ... }` block in `src/nsi.rs`:

```rust
    fn tmp_cache_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("osm-gpui-nsi-tests")
            .join(format!("{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn read_fresh_cache_returns_none_when_missing() {
        let dir = tmp_cache_dir("missing");
        let path = dir.join("nsi.json");
        assert!(read_fresh_cache(&path).is_none());
    }

    #[test]
    fn read_fresh_cache_returns_body_when_recent() {
        let dir = tmp_cache_dir("recent");
        let path = dir.join("nsi.json");
        fs::write(&path, "{\"nsi\":{}}").unwrap();
        assert_eq!(read_fresh_cache(&path), Some("{\"nsi\":{}}".to_string()));
    }

    #[test]
    fn read_fresh_cache_returns_none_when_stale() {
        let dir = tmp_cache_dir("stale");
        let path = dir.join("nsi.json");
        fs::write(&path, "{\"nsi\":{}}").unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
        let file = fs::File::open(&path).unwrap();
        file.set_modified(old_time).unwrap();
        assert!(read_fresh_cache(&path).is_none());
    }

    #[test]
    fn store_starts_empty_and_reflects_set_index() {
        // NSI_INDEX is a process-global OnceLock, so re-running init_store
        // in multiple tests is safe (set() is a no-op after the first call);
        // this test only asserts monotonic behavior, not a clean-slate start.
        init_store();
        set_index(NsiIndex::from_entries(vec![NsiEntry {
            display_name: "Test Brand".to_string(),
            tags: HashMap::from([("shop".to_string(), "yes".to_string())]),
            match_names: vec![],
        }]));
        let idx = current().expect("index should be set after set_index");
        assert_eq!(idx.len(), 1);
    }
```

- [ ] **Step 2: Run test to verify it fails first (before fetch/store code exists)**

Skip if Step 1's code was added in one shot — otherwise run `cargo test --lib nsi::` and confirm compile errors referencing the missing functions, then proceed to add the implementation code from Step 1's first code block.

- [ ] **Step 3: Run the tests and verify they pass**

Run: `cargo test --lib nsi::`
Expected: all previous tests plus `read_fresh_cache_returns_none_when_missing`, `read_fresh_cache_returns_body_when_recent`, `read_fresh_cache_returns_none_when_stale`, `store_starts_empty_and_reflects_set_index` PASS.

- [ ] **Step 4: Commit**

```bash
git add src/nsi.rs
git commit -m "Add NSI background fetch, on-disk cache, and global index store"
```

---

### Task 3: `OsmLayer::commit_tag_change` + `MapLayer` trait default

**Files:**
- Modify: `src/layers/mod.rs:104` (add trait method after `commit_node_moves`)
- Modify: `src/layers/osm_layer.rs:291-305` (add method after `commit_node_moves`)
- Test: inline in `src/layers/osm_layer.rs`'s existing test module (or a new `#[cfg(test)] mod tests` at the bottom of the file if none exists — check first with `grep -n "mod tests" src/layers/osm_layer.rs`)

**Interfaces:**
- Consumes: `crate::selection::FeatureKind` (from `src/selection.rs`), `crate::osm::OsmData` (`nodes: HashMap<i64, OsmNode>`, `ways: Vec<OsmWay>`, both with `pub tags: HashMap<String, String>`).
- Produces: `fn commit_tag_change(&mut self, kind: FeatureKind, id: i64, new_tags: HashMap<String, String>)` on the `MapLayer` trait (default no-op) and overridden on `OsmLayer`. Task 4 calls `layer.commit_tag_change(kind, id, tags)` through `LayerManager::find_layer_mut`.

- [ ] **Step 1: Add the trait method (default no-op)**

In `src/layers/mod.rs`, after the `commit_node_moves` method (currently the last method in the `MapLayer` trait, ending at line 104), add:

```rust
    /// Commit a full tag replacement for a single node or way, rebuilding
    /// derived caches once. Default: no-op.
    fn commit_tag_change(
        &mut self,
        _kind: crate::selection::FeatureKind,
        _id: i64,
        _new_tags: std::collections::HashMap<String, String>,
    ) {
    }
```

- [ ] **Step 2: Write the failing test in `src/layers/osm_layer.rs`**

First check whether a test module already exists:

Run: `grep -n "mod tests" src/layers/osm_layer.rs`

If none exists, add this at the bottom of `src/layers/osm_layer.rs`. If one exists, add the two `#[test]` functions into it instead of creating a new module.

```rust
#[cfg(test)]
mod commit_tag_change_tests {
    use super::*;
    use crate::osm::{OsmData, OsmNode, OsmWay};
    use crate::selection::FeatureKind;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn sample_data() -> OsmData {
        let mut nodes = HashMap::new();
        nodes.insert(
            1,
            OsmNode {
                id: 1,
                lat: 10.0,
                lon: 20.0,
                tags: HashMap::from([("addr:city".to_string(), "Seattle".to_string())]),
            },
        );
        OsmData {
            nodes,
            ways: vec![OsmWay {
                id: 2,
                nodes: vec![1],
                tags: HashMap::from([("highway".to_string(), "residential".to_string())]),
            }],
            relations: vec![],
            bounds: None,
        }
    }

    #[test]
    fn commit_tag_change_updates_node_tags_and_marks_modified() {
        let mut layer = OsmLayer::new_with_data("L", Arc::new(sample_data()));
        assert!(!layer.is_modified());

        let new_tags = HashMap::from([
            ("addr:city".to_string(), "Seattle".to_string()),
            ("amenity".to_string(), "cafe".to_string()),
            ("name".to_string(), "Starbucks".to_string()),
        ]);
        layer.commit_tag_change(FeatureKind::Node, 1, new_tags.clone());

        assert!(layer.is_modified());
        let data = layer.get_osm_data().unwrap();
        assert_eq!(data.nodes.get(&1).unwrap().tags, new_tags);
    }

    #[test]
    fn commit_tag_change_updates_way_tags() {
        let mut layer = OsmLayer::new_with_data("L", Arc::new(sample_data()));
        let new_tags = HashMap::from([("highway".to_string(), "primary".to_string())]);
        layer.commit_tag_change(FeatureKind::Way, 2, new_tags.clone());

        let data = layer.get_osm_data().unwrap();
        let way = data.ways.iter().find(|w| w.id == 2).unwrap();
        assert_eq!(way.tags, new_tags);
    }

    #[test]
    fn commit_tag_change_unknown_id_is_noop() {
        let mut layer = OsmLayer::new_with_data("L", Arc::new(sample_data()));
        layer.commit_tag_change(FeatureKind::Node, 999, HashMap::new());
        assert!(
            !layer.is_modified(),
            "no matching node/way should not mark the layer modified"
        );
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib commit_tag_change -- --nocapture`
Expected: FAIL — `commit_tag_change` either doesn't exist on `OsmLayer` (compile error) or (with only the trait default) never sets `modified`/updates tags.

- [ ] **Step 4: Implement `commit_tag_change` on `OsmLayer`**

In `src/layers/osm_layer.rs`, immediately after `commit_node_moves` (ends at line 305, just before the `get_osm_data` method), add:

```rust
    /// Commit a full tag replacement for a single node or way: clones the
    /// current `OsmData`, replaces that feature's tags with `new_tags`,
    /// marks the layer modified, and rebuilds every derived cache/index once
    /// via `set_osm_data`. No-op if this layer has no data or the id doesn't
    /// belong to this layer.
    pub fn commit_tag_change(
        &mut self,
        kind: FeatureKind,
        id: i64,
        new_tags: HashMap<String, String>,
    ) {
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        let changed = match kind {
            FeatureKind::Node => {
                if let Some(node) = data.nodes.get_mut(&id) {
                    node.tags = new_tags;
                    true
                } else {
                    false
                }
            }
            FeatureKind::Way => {
                if let Some(way) = data.ways.iter_mut().find(|w| w.id == id) {
                    way.tags = new_tags;
                    true
                } else {
                    false
                }
            }
        };
        if !changed {
            return;
        }
        self.modified = true;
        self.set_osm_data(Arc::new(data));
    }
```

Then add the trait override — find the existing `impl MapLayer for OsmLayer` block (search `grep -n "impl MapLayer for OsmLayer"`) and add inside it, near the existing `commit_node_moves` delegation if one exists (check with `grep -n "fn commit_node_moves" src/layers/osm_layer.rs` — the trait impl typically just calls the inherent method of the same name, so Rust resolves the inherent `pub fn commit_tag_change` directly; only add a trait-impl wrapper if `commit_node_moves` has one):

```rust
    fn commit_tag_change(
        &mut self,
        kind: FeatureKind,
        id: i64,
        new_tags: HashMap<String, String>,
    ) {
        OsmLayer::commit_tag_change(self, kind, id, new_tags);
    }
```

(If `commit_node_moves` in the trait impl is instead just inherited implicitly because the inherent method already satisfies the trait signature, skip adding a separate trait-impl block and rely on the same mechanism — verify by running the build in Step 5; if it fails with "not all trait items implemented", add the wrapper above.)

- [ ] **Step 5: Run tests and verify they pass**

Run: `cargo test --lib commit_tag_change`
Expected: all 3 new tests PASS. Also run `cargo build --release` to confirm the trait default + override compile cleanly.

- [ ] **Step 6: Commit**

```bash
git add src/layers/mod.rs src/layers/osm_layer.rs
git commit -m "Add OsmLayer::commit_tag_change for preset-apply tag updates"
```

---

### Task 4: `UndoableAction::SetTags` + undo/redo wiring

**Files:**
- Modify: `src/main.rs:291-308` (`UndoableAction` enum + `description()`)
- Modify: `src/main.rs:702-719` (`apply_undo_action`)
- Test: `src/main.rs`'s existing `mod undo_stack_tests`

**Interfaces:**
- Consumes: `UndoStack`, `UndoableAction` (existing), `osm_gpui::selection::FeatureKind` (existing), `LayerManager::find_layer_mut` (existing), `OsmLayer::commit_tag_change` (Task 3).
- Produces: `UndoableAction::SetTags { layer_name: String, kind: osm_gpui::selection::FeatureKind, id: i64, before: Vec<(String, String)>, after: Vec<(String, String)> }`. Task 6's apply logic constructs this variant and calls `self.undo_stack.push(...)`.

- [ ] **Step 1: Write the failing test**

Add to the existing `mod undo_stack_tests` block in `src/main.rs` (near `push_then_undo_then_redo_round_trips`):

```rust
    fn set_tags_one(
        id: i64,
        before: Vec<(&str, &str)>,
        after: Vec<(&str, &str)>,
    ) -> UndoableAction {
        UndoableAction::SetTags {
            layer_name: "L".to_string(),
            kind: osm_gpui::selection::FeatureKind::Node,
            id,
            before: before
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            after: after
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn set_tags_description_is_singular() {
        let action = set_tags_one(1, vec![], vec![("amenity", "cafe")]);
        assert_eq!(action.description(), "Applied preset to 1 feature");
    }

    #[test]
    fn set_tags_round_trips_through_stack() {
        let mut stack = UndoStack::default();
        stack.push(set_tags_one(
            1,
            vec![("addr:city", "Seattle")],
            vec![("addr:city", "Seattle"), ("amenity", "cafe")],
        ));

        let undone = stack.undo().expect("should have one action to undo");
        match undone {
            UndoableAction::SetTags { before, .. } => {
                assert_eq!(before, vec![("addr:city".to_string(), "Seattle".to_string())]);
            }
            _ => panic!("expected SetTags"),
        }

        let redone = stack.redo().expect("should be able to redo");
        match redone {
            UndoableAction::SetTags { after, .. } => {
                assert_eq!(
                    after,
                    vec![
                        ("addr:city".to_string(), "Seattle".to_string()),
                        ("amenity".to_string(), "cafe".to_string()),
                    ]
                );
            }
            _ => panic!("expected SetTags"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib undo_stack_tests::set_tags`
Expected: FAIL with a compile error — `UndoableAction::SetTags` does not exist.

- [ ] **Step 3: Add the `SetTags` variant and update `description()`**

In `src/main.rs`, change the `UndoableAction` enum (currently):

```rust
#[derive(Clone)]
enum UndoableAction {
    MoveNodes { per_layer: NodeMoveUndoEntries },
}
```

to:

```rust
#[derive(Clone)]
enum UndoableAction {
    MoveNodes { per_layer: NodeMoveUndoEntries },
    SetTags {
        layer_name: String,
        kind: osm_gpui::selection::FeatureKind,
        id: i64,
        before: Vec<(String, String)>,
        after: Vec<(String, String)>,
    },
}
```

and update `impl UndoableAction { fn description(&self) -> String { ... } }` to add a match arm:

```rust
    fn description(&self) -> String {
        match self {
            UndoableAction::MoveNodes { per_layer } => {
                let count: usize = per_layer.iter().map(|(_, entries)| entries.len()).sum();
                if count == 1 {
                    "Moved 1 node".to_string()
                } else {
                    format!("Moved {} nodes", count)
                }
            }
            UndoableAction::SetTags { .. } => "Applied preset to 1 feature".to_string(),
        }
    }
```

- [ ] **Step 4: Wire `apply_undo_action`**

In `src/main.rs`, extend the `match action` in `apply_undo_action` (currently only handling `MoveNodes`):

```rust
    fn apply_undo_action(&mut self, action: &UndoableAction, forward: bool) {
        match action {
            UndoableAction::MoveNodes { per_layer } => {
                for (layer_name, entries) in per_layer {
                    let moves: Vec<(i64, f64, f64)> = entries
                        .iter()
                        .map(|&(id, before, after)| {
                            let (lat, lon) = if forward { after } else { before };
                            (id, lat, lon)
                        })
                        .collect();
                    if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                        layer.commit_node_moves(&moves);
                    }
                }
            }
            UndoableAction::SetTags { layer_name, kind, id, before, after } => {
                let tags: std::collections::HashMap<String, String> = if forward {
                    after.iter().cloned().collect()
                } else {
                    before.iter().cloned().collect()
                };
                if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                    layer.commit_tag_change(*kind, *id, tags);
                }
            }
        }
    }
```

- [ ] **Step 5: Run tests and verify they pass**

Run: `cargo test --lib undo_stack_tests`
Expected: all existing tests plus `set_tags_description_is_singular` and `set_tags_round_trips_through_stack` PASS.

Run: `cargo build --release`
Expected: clean build (confirms `apply_undo_action`'s new match arm compiles against `commit_tag_change` from Task 3).

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "Add UndoableAction::SetTags for preset-apply undo/redo"
```

---

### Task 5: NSI preset search dialog (`src/ui/nsi_dialog.rs`)

**Files:**
- Create: `src/ui/nsi_dialog.rs`
- Modify: `src/ui/mod.rs` (add `pub mod nsi_dialog;`)
- Test: inline `#[cfg(test)] mod tests` in `src/ui/nsi_dialog.rs`

**Interfaces:**
- Consumes: `osm_gpui::nsi::{NsiEntry, current}` (Tasks 1-2).
- Produces: `pub enum DialogEvent { Submitted(std::collections::HashMap<String, String>), Cancelled }`, `pub struct NsiPresetDialog { ... }` with `pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self`, implementing `Focusable` + `Render` + `EventEmitter<DialogEvent>` (same shape as `CustomImageryDialog`). Also `pub fn format_tag_preview(entry: &NsiEntry) -> String` (pure, unit-tested). Task 6 creates this dialog the same way `check_for_dialog_queue` creates `CustomImageryDialog`, and matches on `DialogEvent::{Submitted, Cancelled}`.

- [ ] **Step 1: Write the failing test for `format_tag_preview`**

Create `src/ui/nsi_dialog.rs` with this content first (types + the pure helper + its test; the `Render`/dialog entity code comes in Step 3):

```rust
//! Modal dialog to search NSI brand presets and apply the matched tags to
//! the single currently-selected feature.

use gpui::{
    div, prelude::*, rgba, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, Window,
};
use gpui_component::{
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _,
};
use std::collections::HashMap;

use crate::nsi::NsiEntry;

const MAX_RESULTS: usize = 30;
const PREVIEW_TAG_COUNT: usize = 3;

/// Build the compact tag preview shown in a result row, e.g.
/// "amenity=cafe, brand=Starbucks, name=Starbucks" — up to
/// `PREVIEW_TAG_COUNT` tags, sorted by key for determinism.
pub fn format_tag_preview(entry: &NsiEntry) -> String {
    let mut kv: Vec<(&String, &String)> = entry.tags.iter().collect();
    kv.sort_by(|a, b| a.0.cmp(b.0));
    kv.into_iter()
        .take(PREVIEW_TAG_COUNT)
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tags: &[(&str, &str)]) -> NsiEntry {
        NsiEntry {
            display_name: "Test".to_string(),
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            match_names: vec![],
        }
    }

    #[test]
    fn preview_sorts_by_key_and_caps_at_three() {
        let e = entry(&[
            ("name", "Starbucks"),
            ("amenity", "cafe"),
            ("brand", "Starbucks"),
            ("brand:wikidata", "Q37158"),
        ]);
        assert_eq!(
            format_tag_preview(&e),
            "amenity=cafe, brand=Starbucks, brand:wikidata=Q37158"
        );
    }

    #[test]
    fn preview_empty_tags_is_empty_string() {
        assert_eq!(format_tag_preview(&entry(&[])), "");
    }
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --lib nsi_dialog::tests`
Expected: both tests PASS (this step has no "fails first" — the helper is pure and simple enough to write correct the first time, but confirm anyway before moving on).

- [ ] **Step 3: Add the dialog entity**

Append to `src/ui/nsi_dialog.rs`:

```rust
pub enum DialogEvent {
    Submitted(HashMap<String, String>),
    Cancelled,
}

pub struct NsiPresetDialog {
    query: Entity<InputState>,
    selected_index: usize,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for NsiPresetDialog {}

impl NsiPresetDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search brands…"));
        let focus_handle = cx.focus_handle();
        query.update(cx, |state, cx| state.focus(window, cx));
        Self {
            query,
            selected_index: 0,
            focus_handle,
        }
    }

    /// Current search results for whatever's typed in the query box, cloned
    /// out of the global index so the dialog doesn't hold onto the Arc's
    /// borrow across renders.
    fn results(&self, cx: &Context<Self>) -> Vec<NsiEntry> {
        let Some(index) = crate::nsi::current() else {
            return Vec::new();
        };
        let text = self.query.read(cx).value().to_string();
        index
            .search(&text, MAX_RESULTS)
            .into_iter()
            .cloned()
            .collect()
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn submit_selected(&mut self, cx: &mut Context<Self>) {
        let results = self.results(cx);
        if let Some(entry) = results.get(self.selected_index) {
            cx.emit(DialogEvent::Submitted(entry.tags.clone()));
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.cancel(cx),
            "enter" => self.submit_selected(cx),
            "down" => {
                let count = self.results(cx).len();
                if count > 0 {
                    self.selected_index = (self.selected_index + 1).min(count - 1);
                    cx.notify();
                }
            }
            "up" => {
                self.selected_index = self.selected_index.saturating_sub(1);
                cx.notify();
            }
            _ => {}
        }
    }
}

impl Focusable for NsiPresetDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NsiPresetDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let results = self.results(cx);
        self.selected_index = if results.is_empty() {
            0
        } else {
            self.selected_index.min(results.len() - 1)
        };

        let list: gpui::AnyElement = if crate::nsi::current().is_none() {
            Label::new("Downloading NSI data…")
                .text_sm()
                .text_color(muted)
                .into_any_element()
        } else if results.is_empty() {
            Label::new("No matches.")
                .text_sm()
                .text_color(muted)
                .into_any_element()
        } else {
            let selected_index = self.selected_index;
            div()
                .id("nsi-results")
                .flex()
                .flex_col()
                .h(gpui::px(240.0))
                .overflow_y_scroll()
                .children(results.iter().enumerate().map(|(i, entry)| {
                    let tags = entry.tags.clone();
                    let is_selected = i == selected_index;
                    div()
                        .id(("nsi-result", i))
                        .flex()
                        .flex_col()
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .when(is_selected, |el| el.bg(cx.theme().accent))
                        .hover(|el| el.bg(cx.theme().accent))
                        .child(Label::new(entry.display_name.clone()).text_sm())
                        .child(
                            Label::new(format_tag_preview(entry))
                                .text_xs()
                                .text_color(muted),
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _ev, _, cx| {
                                cx.emit(DialogEvent::Submitted(tags.clone()));
                                let _ = this;
                            }),
                        )
                }))
                .into_any_element()
        };

        let body = v_flex()
            .gap_3()
            .child(Input::new(&self.query))
            .child(list);

        let frame = v_flex()
            .w(gpui::px(420.0))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_color(cx.theme().foreground)
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Apply NSI Preset"),
            )
            .child(div().p_4().child(body));

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .absolute()
            .inset_0()
            .bg(rgba(0x00000099))
            .flex()
            .justify_center()
            .items_center()
            .child(frame)
    }
}
```

- [ ] **Step 4: Register the module**

In `src/ui/mod.rs`, add `pub mod nsi_dialog;`:

```rust
//! UI components shared across the app: dialogs.

pub mod custom_imagery_dialog;
pub mod nsi_dialog;
pub mod settings_window;
```

- [ ] **Step 5: Run the full build and tests**

Run: `cargo build --release`
Expected: clean build.

Run: `cargo test --lib nsi_dialog::tests`
Expected: both tests still PASS.

- [ ] **Step 6: Commit**

```bash
git add src/ui/nsi_dialog.rs src/ui/mod.rs
git commit -m "Add NSI preset search dialog"
```

---

### Task 6: Wire into `MapViewer` — menu item, action handler, apply + undo, startup fetch

**Files:**
- Modify: `src/main.rs`:
  - line 32 (`actions!` macro) — add `ApplyNsiPreset`
  - line ~270 (struct fields near `custom_imagery_dialog`) — add `nsi_dialog` field
  - line ~466 (struct init in `MapViewer::new`) — initialize the new field
  - near line 721-733 (`on_undo`/`on_redo`) — add `on_apply_nsi_preset` handler
  - near line 1025-1071 (`check_for_dialog_queue`) — add a `check_for_nsi_dialog` counterpart is **not** needed; instead the handler opens the dialog directly (see Task rationale below) and subscribes to its events inline, same shape as `check_for_dialog_queue`'s subscribe block
  - line 1718-1722 (`.on_action(...)` chain + `.children(...)`) — register the new action, render the dialog
  - line 2271-2274 (Edit menu items) — add the new `MenuItem::action`
  - inside `main()`, alongside the existing `IMAGERY_INDEX`/ELI background fetch (~line 1918-1957) — call `osm_gpui::nsi::init_store()` and spawn the NSI background fetch

**Interfaces:**
- Consumes: `osm_gpui::nsi::{init_store, fetch_and_cache, parse, set_index}` (Tasks 1-2), `osm_gpui::ui::nsi_dialog::{NsiPresetDialog, DialogEvent}` (Task 5), `UndoableAction::SetTags` (Task 4), `OsmLayer::commit_tag_change` via `MapLayer` trait (Task 3), existing `self.selected: Vec<FeatureRef>`, `self.layer_manager`, `self.undo_stack`.
- Produces: end-to-end feature — nothing further consumes this; it's the integration task.

- [ ] **Step 1: Register the action**

In `src/main.rs` line 32, extend the `actions!` macro:

```rust
actions!(osm_gpui, [OpenOsmFile, Quit, AddOsmCarto, AddCoordinateGrid, DownloadFromOsm, ToggleDebugOverlay, AddCustomImagery, OpenSettings, Undo, Redo, ApplyNsiPreset]);
```

- [ ] **Step 2: Add the dialog field**

In the `MapViewer` struct, right after the `custom_imagery_dialog` field (~line 270):

```rust
    custom_imagery_dialog: Option<gpui::Entity<osm_gpui::ui::custom_imagery_dialog::CustomImageryDialog>>,
    /// Active NSI preset search dialog, if open.
    nsi_dialog: Option<gpui::Entity<osm_gpui::ui::nsi_dialog::NsiPresetDialog>>,
```

In `MapViewer::new`, right after `custom_imagery_dialog: None,` (~line 466):

```rust
            custom_imagery_dialog: None,
            nsi_dialog: None,
```

- [ ] **Step 3: Add the action handler**

In `src/main.rs`, right after `on_redo` (~line 733), add:

```rust
    /// Handle the `ApplyNsiPreset` menu action / keybinding. Only opens the
    /// dialog when exactly one feature is selected — GPUI has no built-in
    /// disabled-menu-item support (see `no_op_imagery_info`), so this is a
    /// no-op otherwise rather than a disabled menu entry.
    fn on_apply_nsi_preset(
        &mut self,
        _: &ApplyNsiPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.len() != 1 || self.nsi_dialog.is_some() {
            return;
        }
        let target = self.selected[0].clone();

        let dialog = cx.new(|cx| osm_gpui::ui::nsi_dialog::NsiPresetDialog::new(window, cx));
        cx.subscribe(
            &dialog,
            move |this: &mut Self, _entity, event: &osm_gpui::ui::nsi_dialog::DialogEvent, cx| {
                use osm_gpui::ui::nsi_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.nsi_dialog = None;
                        cx.notify();
                    }
                    DialogEvent::Submitted(preset_tags) => {
                        this.apply_nsi_preset(&target, preset_tags.clone());
                        this.nsi_dialog = None;
                        cx.notify();
                    }
                }
            },
        )
        .detach();
        self.nsi_dialog = Some(dialog);
        cx.notify();
    }

    /// Merge `preset_tags` into `target`'s existing tags (preset wins on
    /// conflicting keys), commit the change, and push an undo entry.
    fn apply_nsi_preset(
        &mut self,
        target: &osm_gpui::selection::FeatureRef,
        preset_tags: std::collections::HashMap<String, String>,
    ) {
        let Some(layer) = self.layer_manager.find_layer(&target.layer_name) else {
            return;
        };
        let Some(existing) = layer.feature_tags(target) else {
            return;
        };

        let mut merged: std::collections::HashMap<String, String> =
            existing.iter().cloned().collect();
        for (k, v) in &preset_tags {
            merged.insert(k.clone(), v.clone());
        }
        let mut after: Vec<(String, String)> = merged.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        after.sort_by(|a, b| a.0.cmp(&b.0));

        if let Some(layer) = self.layer_manager.find_layer_mut(&target.layer_name) {
            layer.commit_tag_change(target.kind, target.id, merged);
        }

        self.undo_stack.push(UndoableAction::SetTags {
            layer_name: target.layer_name.clone(),
            kind: target.kind,
            id: target.id,
            before: existing,
            after,
        });
    }
```

- [ ] **Step 4: Register the action listener and render the dialog**

In `src/main.rs`, extend the `.on_action(...)` chain (~line 1718-1721):

```rust
            .on_action(cx.listener(Self::on_move_layer))
            .on_action(cx.listener(Self::on_delete_layer))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_apply_nsi_preset))
            .children(self.custom_imagery_dialog.clone())
            .children(self.nsi_dialog.clone())
```

- [ ] **Step 5: Add the menu item**

In `rebuild_menus`, extend the Edit menu (~line 2268-2275):

```rust
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Undo", Undo),
                MenuItem::action("Redo", Redo),
                MenuItem::separator(),
                MenuItem::action("Apply NSI Preset…", ApplyNsiPreset),
            ],
            disabled: false,
        },
```

- [ ] **Step 6: Wire the background fetch at startup**

In `main()`, right after the existing ELI background-fetch block (`cx.background_executor().spawn(async move { match imagery::fetch_and_cache() ... }).detach();`, ending ~line 1957) and before `let map_window = cx.open_window(` (~line 1959), add:

```rust
        // Kick off background fetch/parse of the Name Suggestion Index.
        osm_gpui::nsi::init_store();
        cx.background_executor()
            .spawn(async move {
                match osm_gpui::nsi::fetch_and_cache() {
                    Ok(body) => {
                        let entries = osm_gpui::nsi::parse(&body);
                        eprintln!("nsi: loaded {} brand entries", entries.len());
                        osm_gpui::nsi::set_index(osm_gpui::nsi::NsiIndex::from_entries(entries));
                    }
                    Err(e) => {
                        eprintln!("nsi: failed to load NSI data: {}", e);
                    }
                }
            })
            .detach();
```

(This requires `NsiIndex::from_entries` to be `pub`, which it already is from Task 1, and `NsiIndex` itself must be `pub` — confirm `pub struct NsiIndex` in `src/nsi.rs`.)

- [ ] **Step 7: Build and run the full test suite**

Run: `cargo build --release`
Expected: clean build.

Run: `cargo test --lib`
Expected: all tests across the whole crate PASS, including every test added in Tasks 1-4.

- [ ] **Step 8: Manual verification**

Run: `cargo run --release`

1. Confirm the app launches and the console prints either `nsi: loaded N brand entries` or `nsi: failed to load NSI data: ...` within a few seconds (network permitting).
2. **File > Open…**, load any `.osm` file with at least one node.
3. Click a single node to select it. Open **Edit > Apply NSI Preset…**.
4. If the index finished loading, type a well-known brand (e.g. "starbucks"), confirm matching rows appear with a tag preview, click one.
5. Confirm the side panel's Tags section now shows the merged tags (existing tags preserved, preset tags added/overwritten).
6. **Edit > Undo** — confirm tags revert to their pre-apply state. **Edit > Redo** — confirm they reapply.
7. Select zero or two-or-more features, open **Edit > Apply NSI Preset…** again — confirm nothing happens (no dialog opens).
8. Quit and relaunch the app — confirm the console no longer shows a slow fetch (cache hit) and NSI search still works immediately once the dialog opens.

Document the outcome of this manual pass in the PR description's test plan.

- [ ] **Step 9: Commit**

```bash
git add src/main.rs
git commit -m "Wire NSI preset dialog into Edit menu with apply + undo/redo"
```

---

## Self-Review Notes

- **Spec coverage:** Section 1 (data & fetch) → Tasks 1-2. Section 2 (search/matching) → Task 1. Section 3 (UI/dialog + menu) → Tasks 5-6. Section 4 (apply + undo) → Tasks 3-4, 6. Section 5 (testing) → unit tests embedded in every task plus Task 6 Step 8's manual pass.
- **Out-of-scope items from the spec** (multi-select apply, `locationSet` filtering, general tag editing, generic iD presets, manual refresh control) are not implemented by any task above — intentionally.
- **Type consistency check:** `FeatureKind`/`FeatureRef` from `src/selection.rs` are used identically across Tasks 3, 4, and 6 (`kind: FeatureKind`, `id: i64`, `layer_name: String`). `NsiEntry`/`NsiIndex` from Task 1 are consumed unchanged by Task 2 (`set_index`, `current`) and Task 5 (`results`, `format_tag_preview`). `commit_tag_change`'s signature `(kind: FeatureKind, id: i64, new_tags: HashMap<String, String>)` matches between the Task 3 trait declaration, its `OsmLayer` implementation, and its two call sites in Task 6 (`apply_nsi_preset`) and Task 4 (`apply_undo_action`).
