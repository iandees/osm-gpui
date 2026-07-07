//! Persistent storage for app-wide settings (currently: which OSM API server to use).
//!
//! Stored as JSON in `<config_dir>/osm-gpui/settings.json`. Missing, unreadable, or
//! malformed files fall back to defaults (logged to stderr).

use crate::persist::{self, JsonStore, WriteOpts};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const PRIMARY_API_URL: &str = "https://api.openstreetmap.org";
pub const DEV_API_URL: &str = "https://master.apis.dev.openstreetmap.org";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiServerChoice {
    Primary,
    Dev,
    Custom,
}

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
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            api_server: ApiServerChoice::Primary,
            custom_api_url: String::new(),
            client_ids: HashMap::new(),
        }
    }
}

impl AppSettings {
    /// The OSM API base URL implied by the current selection (no trailing slash).
    pub fn api_base_url(&self) -> String {
        match self.api_server {
            ApiServerChoice::Primary => PRIMARY_API_URL.to_string(),
            ApiServerChoice::Dev => DEV_API_URL.to_string(),
            ApiServerChoice::Custom => self.custom_api_url.trim_end_matches('/').to_string(),
        }
    }
}

/// Global in-memory cache of app settings shared between the app and settings window.
static APP_SETTINGS: JsonStore<AppSettings> = JsonStore::new();

/// Initialize the global store with the loaded settings. Call this once at startup.
pub fn init_store(settings: AppSettings) {
    APP_SETTINGS.init(settings);
}

/// Replace the in-memory settings and persist to disk.
pub fn update_store(settings: AppSettings) {
    APP_SETTINGS.update("settings_store", |g| *g = settings.clone());
    save(&settings);
}

/// Return a snapshot of the current in-memory settings.
pub fn snapshot() -> AppSettings {
    APP_SETTINGS.snapshot("settings_store")
}

/// The OSM API base URL implied by the current settings.
pub fn api_base_url() -> String {
    snapshot().api_base_url()
}

pub fn load_from(path: &Path) -> AppSettings {
    persist::load_json(path, "settings_store")
}

pub fn save_to(path: &Path, settings: &AppSettings) -> std::io::Result<()> {
    persist::save_json(path, settings, WriteOpts::default())
}

/// Default on-disk location: `<config_dir>/osm-gpui/settings.json`.
pub fn default_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("osm-gpui").join("settings.json"))
}

/// Load from the default path. Defaults if unavailable.
pub fn load() -> AppSettings {
    match default_path() {
        Some(p) => load_from(&p),
        None => AppSettings::default(),
    }
}

/// Save to the default path. Silently succeeds (logging only) when there is no config dir.
pub fn save(settings: &AppSettings) {
    let Some(p) = default_path() else {
        eprintln!("settings_store: no config dir, skipping save");
        return;
    };
    if let Err(e) = save_to(&p, settings) {
        eprintln!("settings_store: save {:?} failed: {}", p, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("osm-gpui-settings-tests")
            .join(format!("{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trip() {
        let dir = tmp_dir("round-trip");
        let path = dir.join("settings.json");
        let settings = AppSettings {
            api_server: ApiServerChoice::Custom,
            custom_api_url: "https://example.com".into(),
            client_ids: HashMap::new(),
        };
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, settings);
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tmp_dir("missing");
        let path = dir.join("settings.json");
        let loaded = load_from(&path);
        assert_eq!(loaded, AppSettings::default());
    }

    #[test]
    fn corrupt_file_is_default() {
        let dir = tmp_dir("corrupt");
        let path = dir.join("settings.json");
        fs::write(&path, b"not valid json {{").unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, AppSettings::default());
    }

    #[test]
    fn base_url_matches_choice() {
        assert_eq!(
            AppSettings {
                api_server: ApiServerChoice::Primary,
                custom_api_url: String::new(),
                client_ids: HashMap::new(),
            }
            .api_base_url(),
            PRIMARY_API_URL
        );
        assert_eq!(
            AppSettings {
                api_server: ApiServerChoice::Dev,
                custom_api_url: String::new(),
                client_ids: HashMap::new(),
            }
            .api_base_url(),
            DEV_API_URL
        );
        assert_eq!(
            AppSettings {
                api_server: ApiServerChoice::Custom,
                custom_api_url: "https://custom.example.com/".into(),
                client_ids: HashMap::new(),
            }
            .api_base_url(),
            "https://custom.example.com"
        );
    }
}
