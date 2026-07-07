//! Name Suggestion Index (NSI) brand-preset support.
//!
//! Downloads, caches, and parses the upstream `dist/nsi.json` build of
//! <https://github.com/osmlab/name-suggestion-index>, fetched via jsDelivr's
//! npm CDN (the built `dist/` artifacts are published to npm but are not
//! committed to the git repo itself) into a searchable list of brand name ->
//! tag-set entries ("presets"). No `locationSet` (country) filtering is
//! applied — every entry is offered regardless of location.

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

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

const NSI_URL: &str = "https://cdn.jsdelivr.net/npm/name-suggestion-index/dist/nsi.json";
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
            write_cache_atomically(&cache_path, &body);
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

/// Write `body` to `path` atomically: write to a `.tmp` sibling first, then
/// `rename` it into place (atomic on the same filesystem). This avoids
/// leaving a truncated-but-fresh-mtime cache file behind if the process is
/// killed mid-write, which `read_fresh_cache` would otherwise treat as valid
/// for the full TTL. Mirrors `settings_store::save_to`. Errors are ignored:
/// the in-memory `body` is still returned to the caller even if disk-caching
/// fails.
fn write_cache_atomically(path: &Path, body: &str) {
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, path);
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

/// `GET` the NSI dist build with a small bounded retry on transport errors and
/// retryable HTTP status codes (see `crate::is_retryable_status`).
fn download() -> anyhow::Result<String> {
    download_with(&crate::http::UreqClient::new())
}

/// Same as `download`, but against an injected `HttpClient` so it's testable
/// without a real network.
fn download_with(client: &dyn crate::http::HttpClient) -> anyhow::Result<String> {
    let req = crate::http::HttpRequest::get(NSI_URL);
    let resp = crate::http::fetch_with_retries(client, &req, &crate::http::RetryPolicy::standard())
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    resp.into_string().map_err(|e| anyhow::anyhow!(e.to_string()))
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
    fn write_cache_atomically_writes_body_and_leaves_no_tmp_file() {
        let dir = tmp_cache_dir("atomic-write");
        let path = dir.join("nsi.json");
        write_cache_atomically(&path, "{\"nsi\":{}}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\"nsi\":{}}",
            "final cache file should contain the written body"
        );
        let tmp = path.with_extension("json.tmp");
        assert!(!tmp.exists(), "tmp sibling should be renamed away, not left behind");
    }

    #[test]
    fn write_cache_atomically_overwrites_existing_file() {
        let dir = tmp_cache_dir("atomic-overwrite");
        let path = dir.join("nsi.json");
        fs::write(&path, "stale-content").unwrap();
        write_cache_atomically(&path, "fresh-content");
        assert_eq!(fs::read_to_string(&path).unwrap(), "fresh-content");
    }

    #[test]
    fn download_with_retries_then_succeeds() {
        use crate::http::fake::{ok, status_err, FakeClient};
        let client = FakeClient::new(vec![status_err(503, "busy"), ok(200, "nsi body")]);
        let body = download_with(&client).unwrap();
        assert_eq!(body, "nsi body");
        assert_eq!(client.request_count(), 2);
    }

    #[test]
    fn download_with_does_not_retry_non_retryable_status() {
        use crate::http::fake::{status_err, FakeClient};
        let client = FakeClient::new(vec![status_err(400, "bad request")]);
        let err = download_with(&client);
        assert!(err.is_err());
        assert_eq!(client.request_count(), 1);
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
}
