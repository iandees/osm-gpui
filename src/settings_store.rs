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

/// App-wide text size preference. See `TextScale` in `ui/style.rs` for the
/// pixel values each preset maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextSizePreset {
    Small,
    #[default]
    Medium,
    Large,
}

impl TextSizePreset {
    pub const ALL: [TextSizePreset; 3] = [Self::Small, Self::Medium, Self::Large];

    pub fn label(self) -> gpui::SharedString {
        match self {
            Self::Small => crate::tr!("settings.appearance.size_small"),
            Self::Medium => crate::tr!("settings.appearance.size_medium"),
            Self::Large => crate::tr!("settings.appearance.size_large"),
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_key() == key)
    }
}

/// Colour appearance preference: follow the OS, or force light or dark.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub const ALL: [ThemePreference; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> gpui::SharedString {
        match self {
            Self::System => crate::tr!("settings.appearance.theme_system"),
            Self::Light => crate::tr!("settings.appearance.theme_light"),
            Self::Dark => crate::tr!("settings.appearance.theme_dark"),
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_key() == key)
    }
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
    #[serde(default)]
    pub text_size_preset: TextSizePreset,
    /// Tile cache size budget, in megabytes. Defaults to 500 for
    /// `settings.json` files written before this field existed.
    #[serde(default = "default_cache_budget_mb")]
    pub cache_budget_mb: u64,
    /// User overrides for customizable keyboard shortcuts, keyed by the
    /// shortcut id (see `crate::keybindings::SHORTCUTS`), valued by a gpui
    /// keystroke spec (e.g. `"cmd-o"`). Sparse: only overridden shortcuts
    /// appear here — everything else uses its built-in default, so a future
    /// default change isn't silently frozen for users who never touched
    /// this setting.
    #[serde(default)]
    pub keybindings: HashMap<String, String>,
    /// Width of the main window's side panel in pixels, if the user has
    /// resized it; `None` means the default width.
    #[serde(default)]
    pub side_panel_width: Option<f32>,
    /// UI language as a locale tag (e.g. `"en"`). `None` follows the system
    /// locale. Read once at startup, so a change applies after a restart.
    #[serde(default)]
    pub language: Option<String>,
    /// Ids of the presets most recently applied in the preset picker, most
    /// recent first (see `crate::preset_recents`).
    #[serde(default)]
    pub recent_presets: Vec<String>,
    /// Light, dark, or follow the OS. Missing in older files, meaning System.
    #[serde(default)]
    pub theme: ThemePreference,
    /// Whether layers with unsaved edits are autosaved for crash recovery
    /// (see `crate::autosave`). On by default, including for settings files
    /// written before this field existed.
    #[serde(default = "default_autosave")]
    pub autosave: bool,
    /// True once the user closed the welcome card. Missing in older files,
    /// meaning the card is shown.
    #[serde(default)]
    pub welcome_dismissed: bool,
    /// Reverse the direction in which scrolling zooms the map. Missing in
    /// older files, meaning the default direction.
    #[serde(default)]
    pub invert_scroll_zoom: bool,
    /// The `source` text of the last upload, prefilled in the next upload
    /// dialog. Missing in older files, meaning empty.
    #[serde(default)]
    pub last_source: String,
    /// The last distinct changeset comments, most recent first (see
    /// `crate::changeset_tags::push_recent_comment`). Missing in older files,
    /// meaning none.
    #[serde(default)]
    pub recent_comments: Vec<String>,
    /// Whether the tag key/value inputs ask taginfo.openstreetmap.org for
    /// suggestions (see `crate::taginfo`). On by default, including for
    /// settings files written before this field existed.
    #[serde(default = "default_suggest_from_taginfo")]
    pub suggest_from_taginfo: bool,
    /// Simplify Way's Douglas-Peucker tolerance in metres. Use
    /// [`AppSettings::simplify_tolerance`] to read it: a value outside
    /// [`SIMPLIFY_TOLERANCE_RANGE_M`] (hand-edited file) counts as the default.
    #[serde(default = "default_simplify_tolerance_m")]
    pub simplify_tolerance_m: f64,
    /// Locations (local file paths or http(s) URLs) of the custom MapCSS
    /// styles the user added. Missing in older files, meaning none.
    #[serde(default)]
    pub map_styles: Vec<String>,
    /// The location in `map_styles` that is in use; `None` is the built-in
    /// style. Missing in older files, meaning the built-in style.
    #[serde(default)]
    pub active_map_style: Option<String>,
    /// Where the map was when the app last quit, reopened there next time.
    /// Missing in older files, meaning the built-in starting view.
    #[serde(default)]
    pub last_map_view: Option<MapView>,
    /// The main window's position and size when the app last quit. Missing
    /// in older files, meaning the default placement.
    #[serde(default)]
    pub window_bounds: Option<WindowRect>,
    /// How downloaded GPS traces draw (colours, width, age filter). Missing
    /// in older files, meaning the defaults.
    #[serde(default)]
    pub gps_trace_style: crate::trace_style::TraceStyle,
    /// Mapillary access token (a client token from the Mapillary developer
    /// dashboard), needed to show Mapillary street imagery. Empty when unset.
    #[serde(default)]
    pub mapillary_token: String,
}

/// A saved window placement in logical pixels (screen coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl WindowRect {
    pub const MIN_SIZE: (f32, f32) = (400.0, 300.0);
    pub const MAX_SIZE: (f32, f32) = (20000.0, 20000.0);
    /// How much of the window must overlap a display (width, height) for the
    /// user to be able to grab and move it.
    const MIN_VISIBLE: (f32, f32) = (100.0, 50.0);

    /// `self` if usable: finite, within the size limits, and visibly on at
    /// least one of `displays` (each `(x, y, width, height)`), so a window
    /// last seen on a disconnected monitor falls back to the default. With no
    /// displays known the screen check is skipped.
    pub fn validated(self, displays: &[(f32, f32, f32, f32)]) -> Option<Self> {
        let finite = [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite());
        let sized = (Self::MIN_SIZE.0..=Self::MAX_SIZE.0).contains(&self.width)
            && (Self::MIN_SIZE.1..=Self::MAX_SIZE.1).contains(&self.height);
        if !finite || !sized {
            return None;
        }
        let visible = displays.is_empty()
            || displays.iter().any(|&(dx, dy, dw, dh)| {
                let w = (self.x + self.width).min(dx + dw) - self.x.max(dx);
                let h = (self.y + self.height).min(dy + dh) - self.y.max(dy);
                w >= Self::MIN_VISIBLE.0 && h >= Self::MIN_VISIBLE.1
            });
        visible.then_some(self)
    }
}

/// A saved map position: the centre and zoom level of the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MapView {
    pub lat: f64,
    pub lon: f64,
    pub zoom: f64,
}

impl MapView {
    /// The view to start at: `self` if it is usable (finite, on the map, a
    /// sane zoom), so a hand-edited file can't leave the map unusable.
    pub fn validated(self) -> Option<Self> {
        let ok = self.lat.is_finite()
            && self.lon.is_finite()
            && self.zoom.is_finite()
            && (-85.0..=85.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
            && (0.0..=24.0).contains(&self.zoom);
        ok.then_some(self)
    }
}

/// The allowed range of the Simplify Way tolerance, in metres (inclusive).
pub const SIMPLIFY_TOLERANCE_RANGE_M: (f64, f64) = (0.1, 100.0);

fn default_simplify_tolerance_m() -> f64 {
    crate::geometry::SIMPLIFY_TOLERANCE_M
}

/// `value` if it is a usable Simplify Way tolerance (finite, within
/// [`SIMPLIFY_TOLERANCE_RANGE_M`]).
pub fn valid_simplify_tolerance(value: f64) -> Option<f64> {
    let (lo, hi) = SIMPLIFY_TOLERANCE_RANGE_M;
    (value.is_finite() && (lo..=hi).contains(&value)).then_some(value)
}

/// Parse the text of the tolerance field (a decimal point or comma is fine).
pub fn parse_simplify_tolerance(text: &str) -> Option<f64> {
    valid_simplify_tolerance(text.trim().replace(',', ".").parse::<f64>().ok()?)
}

fn default_cache_budget_mb() -> u64 {
    500
}

fn default_suggest_from_taginfo() -> bool {
    true
}

fn default_autosave() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            api_server: ApiServerChoice::Primary,
            custom_api_url: String::new(),
            client_ids: HashMap::new(),
            text_size_preset: TextSizePreset::default(),
            cache_budget_mb: default_cache_budget_mb(),
            keybindings: HashMap::new(),
            side_panel_width: None,
            language: None,
            recent_presets: Vec::new(),
            theme: ThemePreference::default(),
            autosave: true,
            welcome_dismissed: false,
            invert_scroll_zoom: false,
            last_source: String::new(),
            recent_comments: Vec::new(),
            suggest_from_taginfo: true,
            simplify_tolerance_m: default_simplify_tolerance_m(),
            map_styles: Vec::new(),
            active_map_style: None,
            last_map_view: None,
            window_bounds: None,
            gps_trace_style: Default::default(),
            mapillary_token: String::new(),
        }
    }
}

impl AppSettings {
    /// The Simplify Way tolerance in metres, the default if the stored value
    /// is out of range.
    pub fn simplify_tolerance(&self) -> f64 {
        valid_simplify_tolerance(self.simplify_tolerance_m)
            .unwrap_or_else(default_simplify_tolerance_m)
    }

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
    // The server may have changed: learn its capabilities in the background
    // (a no-op when already known). Never from unit tests: no network there.
    #[cfg(not(test))]
    {
        let base = settings.api_base_url();
        if base.starts_with("http://") || base.starts_with("https://") {
            crate::capabilities::ensure_fetched(&base);
        }
    }
}

/// Replace the in-memory settings only, without writing the settings file.
pub fn update_store_in_memory(settings: AppSettings) {
    APP_SETTINGS.update("settings_store", |g| *g = settings);
}

/// Set only the welcome-card flag, on top of the store's current settings so
/// nothing else is touched. Written to disk when `persist` is true.
pub fn set_welcome_dismissed(dismissed: bool, persist: bool) {
    let mut settings = snapshot();
    settings.welcome_dismissed = dismissed;
    if persist {
        update_store(settings);
    } else {
        update_store_in_memory(settings);
    }
}

/// Set only the invert-scroll-zoom flag, on top of the store's current
/// settings so nothing else is touched. Written to disk when `persist` is true.
pub fn set_invert_scroll_zoom(invert: bool, persist: bool) {
    let mut settings = snapshot();
    settings.invert_scroll_zoom = invert;
    if persist {
        update_store(settings);
    } else {
        update_store_in_memory(settings);
    }
}

/// Set only the taginfo-suggestions flag, on top of the store's current
/// settings so nothing else is touched. Written to disk when `persist` is true.
pub fn set_suggest_from_taginfo(on: bool, persist: bool) {
    let mut settings = snapshot();
    settings.suggest_from_taginfo = on;
    if persist {
        update_store(settings);
    } else {
        update_store_in_memory(settings);
    }
}

/// Set only the Mapillary access token, on top of the store's current
/// settings. Surrounding whitespace is dropped. Written to disk when
/// `persist` is true.
pub fn set_mapillary_token(token: &str, persist: bool) {
    let mut settings = snapshot();
    settings.mapillary_token = token.trim().to_string();
    if persist {
        update_store(settings);
    } else {
        update_store_in_memory(settings);
    }
}

/// Set only the Simplify Way tolerance (metres), on top of the store's current
/// settings. Values outside the allowed range are ignored (`false`). Written
/// to disk when `persist` is true.
pub fn set_simplify_tolerance(metres: f64, persist: bool) -> bool {
    let Some(metres) = valid_simplify_tolerance(metres) else {
        return false;
    };
    let mut settings = snapshot();
    settings.simplify_tolerance_m = metres;
    if persist {
        update_store(settings);
    } else {
        update_store_in_memory(settings);
    }
    true
}

/// Set only the last map view, on top of the store's current settings so
/// nothing else is touched. Written to disk when `persist` is true.
pub fn set_last_map_view(view: MapView, persist: bool) {
    let mut settings = snapshot();
    settings.last_map_view = Some(view);
    if persist {
        update_store(settings);
    } else {
        update_store_in_memory(settings);
    }
}

/// Set only the window bounds, on top of the store's current settings so
/// nothing else is touched. Written to disk when `persist` is true.
pub fn set_window_bounds(rect: WindowRect, persist: bool) {
    let mut settings = snapshot();
    settings.window_bounds = Some(rect);
    if persist {
        update_store(settings);
    } else {
        update_store_in_memory(settings);
    }
}

/// Set only the GPS trace style, on top of the store's current settings so
/// nothing else is touched. Written to disk when `persist` is true.
pub fn set_gps_trace_style(style: crate::trace_style::TraceStyle, persist: bool) {
    let mut settings = snapshot();
    settings.gps_trace_style = style;
    if persist {
        update_store(settings);
    } else {
        update_store_in_memory(settings);
    }
}

/// Remember the source text and the comment of an upload that went through:
/// the source is prefilled next time and the comment joins the recent list.
/// Applied on top of the store's current settings so nothing else is touched.
/// Written to disk when `persist` is true.
pub fn remember_upload_text(source: &str, comment: &str, persist: bool) {
    let mut settings = snapshot();
    settings.last_source = source.trim().to_string();
    crate::changeset_tags::push_recent_comment(&mut settings.recent_comments, comment);
    if persist {
        update_store(settings);
    } else {
        update_store_in_memory(settings);
    }
}

/// Return a snapshot of the current in-memory settings.
pub fn snapshot() -> AppSettings {
    APP_SETTINGS.snapshot("settings_store")
}

/// API server chosen on the command line (`--api-server`) for this run only. Kept
/// apart from `APP_SETTINGS` on purpose: the store is what gets saved to
/// `settings.json`, so an override living there could leak to disk.
static RUN_OVERRIDE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Override the API server base URL for this run only. Never written to
/// `settings.json`.
pub fn set_run_override(base_url: Option<String>) {
    *RUN_OVERRIDE.lock().unwrap() = base_url;
}

/// The command-line API server override for this run, if any.
pub fn run_override() -> Option<String> {
    RUN_OVERRIDE.lock().unwrap().clone()
}

/// The base URL for `settings` with an optional per-run override applied.
pub fn effective_base_url(settings: &AppSettings, over: Option<&str>) -> String {
    match over {
        Some(url) => url.trim_end_matches('/').to_string(),
        None => settings.api_base_url(),
    }
}

/// Parse the value of `--api-server`: `primary`, `dev`, or an `http(s)://` base
/// URL. `None` for anything else.
pub fn parse_api_server_arg(value: &str) -> Option<String> {
    match value {
        "primary" => Some(PRIMARY_API_URL.to_string()),
        "dev" => Some(DEV_API_URL.to_string()),
        url if url.starts_with("https://") || url.starts_with("http://") => {
            Some(url.trim_end_matches('/').to_string())
        }
        _ => None,
    }
}

/// The OSM API base URL in effect: the saved settings, unless `--api-server`
/// overrides them for this run.
pub fn api_base_url() -> String {
    effective_base_url(&snapshot(), run_override().as_deref())
}

/// The host part of an API base URL, lowercased, without port or path.
fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or("");
    authority
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Whether `url` is the OSM development server (exact host match, so look-alikes
/// such as `master.apis.dev.openstreetmap.org.evil.example` do not count).
pub fn is_dev_server(url: &str) -> bool {
    url_host(url) == url_host(DEV_API_URL)
}

/// Whether `url` is the production OSM API.
pub fn is_primary_server(url: &str) -> bool {
    url_host(url) == url_host(PRIMARY_API_URL)
}

/// The host to show in the status bar and window title when the active server is
/// not the production one; `None` for the production server.
pub fn non_primary_host_label(url: &str) -> Option<String> {
    (!is_primary_server(url)).then(|| url_host(url))
}

pub fn load_from(path: &Path) -> AppSettings {
    persist::load_json(path, "settings_store")
}

pub fn save_to(path: &Path, settings: &AppSettings) -> std::io::Result<()> {
    persist::save_json(path, settings, WriteOpts::default())
}

/// Default on-disk location: `<config_dir>/osm-gpui/settings.json`. `None`
/// where the persistence policy forbids touching real state (see
/// `persist::Policy`; the default, so tests never reach the real file).
pub fn default_path() -> Option<PathBuf> {
    persist::config_file("settings.json")
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
    fn default_save_entry_points_never_reach_the_real_file() {
        // The precondition is checked before anything is saved.
        assert_eq!(default_path(), None);
        let real = dirs::config_dir().map(|d| d.join("osm-gpui").join("settings.json"));
        if let Some(real) = &real {
            assert!(!persist::write_permitted(real));
        }
        let before = real
            .as_ref()
            .and_then(|p| fs::metadata(p).ok()?.modified().ok());
        update_store(AppSettings::default());
        save(&AppSettings::default());
        set_welcome_dismissed(true, true);
        set_invert_scroll_zoom(true, true);
        set_suggest_from_taginfo(false, true);
        set_simplify_tolerance(5.0, true);
        let after = real
            .as_ref()
            .and_then(|p| fs::metadata(p).ok()?.modified().ok());
        assert_eq!(before, after);
    }

    #[test]
    fn round_trip() {
        let dir = tmp_dir("round-trip");
        let path = dir.join("settings.json");
        let settings = AppSettings {
            api_server: ApiServerChoice::Custom,
            custom_api_url: "https://example.com".into(),
            client_ids: HashMap::new(),
            text_size_preset: TextSizePreset::Large,
            cache_budget_mb: 500,
            keybindings: HashMap::new(),
            side_panel_width: Some(340.0),
            language: Some("en".into()),
            recent_presets: vec!["building".into(), "natural/tree".into()],
            theme: ThemePreference::Dark,
            autosave: false,
            welcome_dismissed: true,
            invert_scroll_zoom: true,
            last_source: "survey; GPS".into(),
            recent_comments: vec!["Add a cafe".into(), "Fix \"quotes\" #tag".into()],
            suggest_from_taginfo: false,
            simplify_tolerance_m: 7.5,
            map_styles: vec!["/s/a.mapcss".into()],
            active_map_style: Some("/s/a.mapcss".into()),
            last_map_view: Some(MapView {
                lat: 51.5,
                lon: -0.12,
                zoom: 14.25,
            }),
            window_bounds: Some(WindowRect {
                x: 10.0,
                y: 20.0,
                width: 900.0,
                height: 700.0,
            }),
            gps_trace_style: crate::trace_style::TraceStyle {
                color_by: crate::trace_style::ColorBy::Speed,
                ..Default::default()
            },
            mapillary_token: "MLY|123|abc".into(),
        };
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, settings);
        assert_eq!(loaded.simplify_tolerance(), 7.5);
    }

    #[test]
    fn simplify_tolerance_defaults_when_missing_and_validates() {
        // A file written before the field existed.
        let old: AppSettings =
            serde_json::from_str(r#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        assert_eq!(old.simplify_tolerance(), 3.0);
        assert_eq!(AppSettings::default().simplify_tolerance(), 3.0);
        for bad in [0.0, 0.05, -2.0, 100.5, 1e9, f64::NAN, f64::INFINITY] {
            let s = AppSettings {
                simplify_tolerance_m: bad,
                ..AppSettings::default()
            };
            assert_eq!(s.simplify_tolerance(), 3.0, "{bad} falls back");
            assert_eq!(valid_simplify_tolerance(bad), None);
        }
        for good in [0.1, 3.0, 100.0] {
            assert_eq!(valid_simplify_tolerance(good), Some(good));
        }
        assert_eq!(parse_simplify_tolerance(" 2,5 "), Some(2.5));
        assert_eq!(parse_simplify_tolerance("12"), Some(12.0));
        for bad in ["", "abc", "0", "0.01", "101", "-1", "NaN", "inf"] {
            assert_eq!(parse_simplify_tolerance(bad), None, "{bad:?}");
        }
        // An out-of-range value in the file is read back as the default.
        let bad_file: AppSettings = serde_json::from_str(
            r#"{"api_server":"Primary","custom_api_url":"","simplify_tolerance_m":5000}"#,
        )
        .unwrap();
        assert_eq!(bad_file.simplify_tolerance(), 3.0);
    }

    #[test]
    fn run_override_changes_the_url_but_not_the_settings() {
        let saved = AppSettings::default();
        assert_eq!(effective_base_url(&saved, None), PRIMARY_API_URL);
        assert_eq!(effective_base_url(&saved, Some(DEV_API_URL)), DEV_API_URL);
        assert_eq!(
            effective_base_url(&saved, Some("http://127.0.0.1:9/")),
            "http://127.0.0.1:9"
        );
        // The settings value itself (what `update_store` would write) is untouched.
        assert_eq!(saved.api_server, ApiServerChoice::Primary);
        let json = serde_json::to_string(&saved).unwrap();
        assert!(json.contains("\"Primary\"") && !json.contains("Dev"));
    }

    #[test]
    fn api_server_argument_values() {
        assert_eq!(parse_api_server_arg("dev").as_deref(), Some(DEV_API_URL));
        assert_eq!(
            parse_api_server_arg("primary").as_deref(),
            Some(PRIMARY_API_URL)
        );
        assert_eq!(
            parse_api_server_arg("https://example.com/").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(parse_api_server_arg("prod"), None);
        assert_eq!(parse_api_server_arg(""), None);
        assert_eq!(parse_api_server_arg("ftp://x"), None);
    }

    #[test]
    fn dev_server_match_is_exact_on_the_host() {
        assert!(is_dev_server("https://master.apis.dev.openstreetmap.org"));
        assert!(is_dev_server("https://master.apis.dev.openstreetmap.org/"));
        assert!(is_dev_server(
            "https://MASTER.apis.dev.openstreetmap.org:443/api"
        ));
        assert!(!is_dev_server("https://api.openstreetmap.org"));
        assert!(!is_dev_server(
            "https://master.apis.dev.openstreetmap.org.evil.example"
        ));
        assert!(!is_dev_server(
            "https://evil.example/master.apis.dev.openstreetmap.org"
        ));
        assert!(!is_dev_server(
            "https://master.apis.dev.openstreetmap.org@evil.example"
        ));
        assert!(!is_dev_server("https://xmaster.apis.dev.openstreetmap.org"));
    }

    #[test]
    fn host_label_only_for_non_primary_servers() {
        assert_eq!(non_primary_host_label(PRIMARY_API_URL), None);
        assert_eq!(
            non_primary_host_label(DEV_API_URL).as_deref(),
            Some("master.apis.dev.openstreetmap.org")
        );
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tmp_dir("missing");
        let path = dir.join("settings.json");
        let loaded = load_from(&path);
        assert_eq!(loaded, AppSettings::default());
    }

    #[test]
    fn missing_text_size_preset_defaults_to_medium() {
        let dir = tmp_dir("legacy-no-text-size");
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.text_size_preset, TextSizePreset::Medium);
    }

    #[test]
    fn missing_language_defaults_to_system() {
        let dir = tmp_dir("legacy-no-language");
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        assert_eq!(load_from(&path).language, None);
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
                ..Default::default()
            }
            .api_base_url(),
            PRIMARY_API_URL
        );
        assert_eq!(
            AppSettings {
                api_server: ApiServerChoice::Dev,
                ..Default::default()
            }
            .api_base_url(),
            DEV_API_URL
        );
        assert_eq!(
            AppSettings {
                api_server: ApiServerChoice::Custom,
                custom_api_url: "https://custom.example.com/".into(),
                ..Default::default()
            }
            .api_base_url(),
            "https://custom.example.com"
        );
    }

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
            cache_budget_mb: 250,
            ..Default::default()
        };
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.cache_budget_mb, 250);
    }

    #[test]
    fn round_trip_with_keybindings() {
        let dir = tmp_dir("round-trip-keybindings");
        let path = dir.join("settings.json");
        let mut keybindings = HashMap::new();
        keybindings.insert("undo".to_string(), "cmd-alt-z".to_string());
        let settings = AppSettings {
            api_server: ApiServerChoice::Primary,
            custom_api_url: String::new(),
            client_ids: HashMap::new(),
            text_size_preset: TextSizePreset::default(),
            cache_budget_mb: default_cache_budget_mb(),
            keybindings,
            side_panel_width: None,
            language: None,
            recent_presets: Vec::new(),
            theme: ThemePreference::default(),
            autosave: true,
            welcome_dismissed: false,
            invert_scroll_zoom: false,
            last_source: String::new(),
            recent_comments: Vec::new(),
            suggest_from_taginfo: true,
            simplify_tolerance_m: 3.0,
            map_styles: Vec::new(),
            active_map_style: None,
            last_map_view: None,
            window_bounds: None,
            gps_trace_style: Default::default(),
            mapillary_token: String::new(),
        };
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, settings);
    }

    #[test]
    fn map_view_is_absent_in_older_files_and_validated() {
        let old: AppSettings =
            serde_json::from_str(r#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        assert_eq!(old.last_map_view, None);
        let good = MapView {
            lat: 10.0,
            lon: 20.0,
            zoom: 12.5,
        };
        assert_eq!(good.validated(), Some(good));
        for bad in [
            MapView {
                lat: f64::NAN,
                ..good
            },
            MapView { lat: 90.0, ..good },
            MapView { lon: 181.0, ..good },
            MapView { zoom: -1.0, ..good },
            MapView {
                zoom: f64::INFINITY,
                ..good
            },
        ] {
            assert_eq!(bad.validated(), None);
        }
    }

    #[test]
    fn window_rect_validation() {
        let screen = [(0.0, 0.0, 1920.0, 1080.0)];
        let good = WindowRect {
            x: 100.0,
            y: 100.0,
            width: 1200.0,
            height: 800.0,
        };
        assert_eq!(good.validated(&screen), Some(good));
        assert_eq!(good.validated(&[]), Some(good));
        for bad in [
            WindowRect {
                x: f32::NAN,
                ..good
            },
            WindowRect {
                width: f32::INFINITY,
                ..good
            },
            WindowRect {
                width: 100.0,
                ..good
            },
            WindowRect {
                height: 50.0,
                ..good
            },
            WindowRect {
                width: 1.0e6,
                ..good
            },
            // Last seen on a monitor that is no longer connected.
            WindowRect { x: 3000.0, ..good },
            WindowRect { y: -2000.0, ..good },
            // Only a sliver still on screen.
            WindowRect { x: 1900.0, ..good },
        ] {
            assert_eq!(bad.validated(&screen), None, "{bad:?}");
        }
        // Partly off-screen but still grabbable.
        let partly = WindowRect { x: -600.0, ..good };
        assert_eq!(partly.validated(&screen), Some(partly));
        // On a second display.
        let two = [(0.0, 0.0, 1920.0, 1080.0), (1920.0, 0.0, 1920.0, 1080.0)];
        let right = WindowRect { x: 2200.0, ..good };
        assert_eq!(right.validated(&two), Some(right));
    }

    #[test]
    fn window_rect_round_trips_and_is_absent_in_older_files() {
        let old: AppSettings =
            serde_json::from_str(r#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        assert_eq!(old.window_bounds, None);
        let dir = tmp_dir("window-rect");
        let path = dir.join("settings.json");
        let settings = AppSettings {
            window_bounds: Some(WindowRect {
                x: -5.5,
                y: 30.0,
                width: 1000.0,
                height: 640.0,
            }),
            ..Default::default()
        };
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).window_bounds, settings.window_bounds);
    }

    #[test]
    fn map_view_round_trips_through_the_file() {
        let dir = tmp_dir("map-view");
        let path = dir.join("settings.json");
        let settings = AppSettings {
            last_map_view: Some(MapView {
                lat: -33.86,
                lon: 151.2,
                zoom: 9.0,
            }),
            ..Default::default()
        };
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).last_map_view, settings.last_map_view);
    }

    #[test]
    fn taginfo_suggestions_are_on_by_default_and_for_older_settings_files() {
        assert!(AppSettings::default().suggest_from_taginfo);
        let dir = tmp_dir("legacy-no-taginfo");
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        assert!(load_from(&path).suggest_from_taginfo);
    }

    #[test]
    fn missing_changeset_memory_means_no_source_and_no_recent_comments() {
        let dir = tmp_dir("legacy-no-changeset-memory");
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.last_source, "");
        assert!(loaded.recent_comments.is_empty());
        assert_eq!(AppSettings::default().last_source, "");
    }

    #[test]
    fn missing_invert_scroll_zoom_means_not_inverted() {
        let dir = tmp_dir("legacy-no-invert-scroll");
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        assert!(!load_from(&path).invert_scroll_zoom);
    }

    #[test]
    fn missing_welcome_dismissed_means_show_the_card() {
        let dir = tmp_dir("legacy-no-welcome");
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        assert!(!load_from(&path).welcome_dismissed);
    }

    #[test]
    fn missing_theme_defaults_to_system() {
        let dir = tmp_dir("legacy-no-theme");
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        assert_eq!(load_from(&path).theme, ThemePreference::System);
    }

    #[test]
    fn older_settings_files_use_the_built_in_map_style() {
        let dir = tmp_dir("legacy-no-map-style");
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        let loaded = load_from(&path);
        assert!(loaded.map_styles.is_empty());
        assert_eq!(loaded.active_map_style, None);
    }

    #[test]
    fn map_style_choice_round_trips() {
        let dir = tmp_dir("map-style-round-trip");
        let path = dir.join("settings.json");
        let settings = AppSettings {
            map_styles: vec!["/a/b.mapcss".into(), "https://x.example/c.mapcss".into()],
            active_map_style: Some("/a/b.mapcss".into()),
            ..AppSettings::default()
        };
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path), settings);
    }

    #[test]
    fn built_in_map_style_choice_round_trips_beside_user_styles() {
        let dir = tmp_dir("built-in-map-style");
        let path = dir.join("settings.json");
        let settings = AppSettings {
            map_styles: vec!["/a/b.mapcss".into()],
            active_map_style: crate::style::custom::built_in_location("wireframe"),
            ..AppSettings::default()
        };
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, settings);
        assert_eq!(
            loaded.active_map_style.as_deref(),
            Some("builtin:wireframe")
        );
        assert_eq!(loaded.map_styles, vec!["/a/b.mapcss".to_string()]);
        assert!(crate::style::custom::built_in_for("builtin:wireframe").is_some());
    }

    #[test]
    fn autosave_is_on_by_default_and_for_older_settings_files() {
        assert!(AppSettings::default().autosave);
        let dir = tmp_dir("legacy-no-autosave");
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"api_server":"Primary","custom_api_url":""}"#).unwrap();
        assert!(load_from(&path).autosave);
    }

    #[test]
    fn turning_autosave_off_is_saved() {
        let dir = tmp_dir("autosave-off");
        let path = dir.join("settings.json");
        let settings = AppSettings {
            autosave: false,
            ..Default::default()
        };
        save_to(&path, &settings).unwrap();
        assert!(!load_from(&path).autosave);
    }

    #[test]
    fn theme_preference_keys_round_trip() {
        for p in ThemePreference::ALL {
            assert_eq!(ThemePreference::from_key(p.as_key()), Some(p));
        }
        assert_eq!(ThemePreference::from_key("sepia"), None);
    }
}
