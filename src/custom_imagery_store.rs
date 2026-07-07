//! Persistent storage for user-defined custom imagery layers.
//!
//! Entries are stored as a JSON array in `<config_dir>/osm-gpui/custom-imagery.json`.
//! Missing, unreadable, or malformed files are treated as empty (logged to stderr).

use crate::persist::{self, JsonStore, WriteOpts};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Global in-memory cache of custom imagery entries shared between the app and settings window.
static CUSTOM_IMAGERY_STORE: JsonStore<Vec<CustomImageryEntry>> = JsonStore::new();

/// Initialize the global store with the loaded entries. Call this once at startup.
pub fn init_store(entries: Vec<CustomImageryEntry>) {
    CUSTOM_IMAGERY_STORE.init(entries);
}

/// Replace the in-memory store contents and persist to disk.
pub fn update_store(entries: Vec<CustomImageryEntry>) {
    CUSTOM_IMAGERY_STORE.update("custom_imagery_store", |g| *g = entries.clone());
    save(&entries);
}

/// Return a snapshot of the current in-memory entries.
pub fn snapshot() -> Vec<CustomImageryEntry> {
    CUSTOM_IMAGERY_STORE.snapshot("custom_imagery_store")
}

/// Append one entry to the in-memory store and persist to disk.
pub fn append(entry: CustomImageryEntry) {
    let Some(snapshot) = CUSTOM_IMAGERY_STORE.update("custom_imagery_store", |g| g.push(entry)) else {
        return;
    };
    save(&snapshot);
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomImageryEntry {
    pub name: String,
    pub url_template: String,
    pub min_zoom: u32,
    pub max_zoom: u32,
}

/// Load entries from the given file path. Returns an empty vec on missing file,
/// unreadable file, or parse error (logged to stderr).
pub fn load_from(path: &Path) -> Vec<CustomImageryEntry> {
    persist::load_json(path, "custom_imagery_store")
}

/// Atomically write entries to the given path. Writes to a sibling temp file
/// then renames into place.
pub fn save_to(path: &Path, entries: &[CustomImageryEntry]) -> std::io::Result<()> {
    persist::save_json(path, entries, WriteOpts::default())
}

/// Default on-disk location: `<config_dir>/osm-gpui/custom-imagery.json`.
/// Returns `None` if the OS has no conventional config dir (e.g., exotic platforms).
pub fn default_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("osm-gpui").join("custom-imagery.json"))
}

/// Load from the default path. Empty vec if unavailable.
pub fn load() -> Vec<CustomImageryEntry> {
    match default_path() {
        Some(p) => load_from(&p),
        None => Vec::new(),
    }
}

/// Save to the default path. Silently succeeds (logging only) when there is no config dir.
pub fn save(entries: &[CustomImageryEntry]) {
    let Some(p) = default_path() else {
        eprintln!("custom_imagery_store: no config dir, skipping save");
        return;
    };
    if let Err(e) = save_to(&p, entries) {
        eprintln!("custom_imagery_store: save {:?} failed: {}", p, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("osm-gpui-custom-imagery-tests")
            .join(format!("{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample() -> Vec<CustomImageryEntry> {
        vec![
            CustomImageryEntry {
                name: "Example".into(),
                url_template: "https://tile.example.com/{z}/{x}/{y}.png".into(),
                min_zoom: 0,
                max_zoom: 19,
            },
            CustomImageryEntry {
                name: "Other".into(),
                url_template: "https://other.example.com/{z}/{x}/{-y}.png".into(),
                min_zoom: 4,
                max_zoom: 18,
            },
        ]
    }

    #[test]
    fn round_trip() {
        let dir = tmp_dir("round-trip");
        let path = dir.join("custom-imagery.json");
        save_to(&path, &sample()).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, sample());
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tmp_dir("missing");
        let path = dir.join("custom-imagery.json");
        let loaded = load_from(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn corrupt_file_is_empty() {
        let dir = tmp_dir("corrupt");
        let path = dir.join("custom-imagery.json");
        fs::write(&path, b"not valid json {{").unwrap();
        let loaded = load_from(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn save_overwrites_previous_content() {
        let dir = tmp_dir("overwrite");
        let path = dir.join("custom-imagery.json");
        save_to(&path, &sample()).unwrap();
        save_to(&path, &[]).unwrap();
        let loaded = load_from(&path);
        assert!(loaded.is_empty());
    }
}
