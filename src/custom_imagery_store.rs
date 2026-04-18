//! Persistent storage for user-defined custom imagery layers.
//!
//! Entries are stored as a JSON array in `<config_dir>/osm-gpui/custom-imagery.json`.
//! Missing, unreadable, or malformed files are treated as empty (logged to stderr).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Global in-memory cache of custom imagery entries shared between the app and settings window.
pub static CUSTOM_IMAGERY_STORE: OnceLock<Arc<Mutex<Vec<CustomImageryEntry>>>> = OnceLock::new();

/// Initialize the global store with the loaded entries. Call this once at startup.
pub fn init_store(entries: Vec<CustomImageryEntry>) {
    let _ = CUSTOM_IMAGERY_STORE.set(Arc::new(Mutex::new(entries)));
}

/// Replace the in-memory store contents and persist to disk.
pub fn update_store(entries: Vec<CustomImageryEntry>) {
    if let Some(store) = CUSTOM_IMAGERY_STORE.get() {
        if let Ok(mut g) = store.lock() {
            *g = entries.clone();
        }
    }
    save(&entries);
}

/// Return a snapshot of the current in-memory entries.
pub fn snapshot() -> Vec<CustomImageryEntry> {
    CUSTOM_IMAGERY_STORE
        .get()
        .and_then(|s| s.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

/// Append one entry to the in-memory store and persist to disk.
pub fn append(entry: CustomImageryEntry) {
    let Some(store) = CUSTOM_IMAGERY_STORE.get() else { return };
    let snapshot = {
        let Ok(mut g) = store.lock() else { return };
        g.push(entry);
        g.clone()
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
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!("custom_imagery_store: read {:?} failed: {}", path, e);
            return Vec::new();
        }
    };
    match serde_json::from_slice::<Vec<CustomImageryEntry>>(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("custom_imagery_store: parse {:?} failed: {}", path, e);
            Vec::new()
        }
    }
}

/// Atomically write entries to the given path. Writes to a sibling `.tmp` file
/// then renames into place.
pub fn save_to(path: &Path, entries: &[CustomImageryEntry]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(entries)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
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
