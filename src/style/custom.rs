//! User-chosen MapCSS styles (the equivalent of JOSM's "Map Paint Styles").
//!
//! A style is identified by its *location*: a local file path or an
//! `http(s)://` URL. The settings hold the list of known locations and which
//! one is active; the built-in `default.mapcss` is used when none is active
//! or when the active one cannot be loaded or parsed.
//!
//! The resolved stylesheet lives in a process-wide slot so every `OsmLayer`
//! constructor picks it up; already-open layers are restyled explicitly via
//! `MapLayer::set_stylesheet`.

use super::Stylesheet;
use crate::http::{HttpClient, HttpRequest, UreqClient};
use std::sync::{Arc, OnceLock, RwLock};

static ACTIVE: RwLock<Option<Arc<Stylesheet>>> = RwLock::new(None);
static BUILT_IN: OnceLock<Arc<Stylesheet>> = OnceLock::new();

/// The compiled-in default stylesheet, parsed once.
pub fn built_in() -> Arc<Stylesheet> {
    BUILT_IN
        .get_or_init(|| Arc::new(Stylesheet::load_default()))
        .clone()
}

/// The stylesheet new layers should use: the active custom style if one is
/// set, else the built-in one.
pub fn active_stylesheet() -> Arc<Stylesheet> {
    ACTIVE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(built_in)
}

/// Make `sheet` the active stylesheet (`None` restores the built-in one).
pub fn set_active(sheet: Option<Arc<Stylesheet>>) {
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = sheet;
}

/// Whether `location` names a remote style rather than a local file.
pub fn is_url(location: &str) -> bool {
    location.starts_with("http://") || location.starts_with("https://")
}

/// A short label for a style in menus and lists: the file name of a path, or
/// the last path segment (else the host) of a URL.
pub fn display_name(location: &str) -> String {
    let trimmed = location.trim().trim_end_matches('/');
    let without_scheme = trimmed.split("://").last().unwrap_or(trimmed);
    let last = without_scheme
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_scheme);
    if last.is_empty() {
        trimmed.to_string()
    } else {
        last.to_string()
    }
}

/// Read the MapCSS text at `location`: over HTTP for a URL, from disk for a
/// path. Blocking, so call it off the UI thread.
pub fn read_source(location: &str, client: &dyn HttpClient) -> Result<String, String> {
    if is_url(location) {
        client
            .request(HttpRequest::get(location))
            .map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string())
    } else {
        std::fs::read_to_string(location).map_err(|e| e.to_string())
    }
}

/// Load and parse the style at `location`.
pub fn load(location: &str, client: &dyn HttpClient) -> Result<Stylesheet, String> {
    let src = read_source(location, client)?;
    Stylesheet::parse(&src).map_err(|e| e.to_string())
}

/// [`load`] with the real HTTP client.
pub fn load_blocking(location: &str) -> Result<Stylesheet, String> {
    load(location, &UreqClient::new())
}

/// The stylesheet for the `active` choice. `(Some(sheet), None)` when a
/// custom style loaded; `(None, None)` when no custom style is chosen (use
/// the built-in one); `(None, Some(reason))` when the chosen style could not
/// be read or parsed, so the built-in one applies.
pub fn resolve(
    active: Option<&str>,
    client: &dyn HttpClient,
) -> (Option<Arc<Stylesheet>>, Option<String>) {
    let Some(location) = active.map(str::trim).filter(|l| !l.is_empty()) else {
        return (None, None);
    };
    match load(location, client) {
        Ok(sheet) => (Some(Arc::new(sheet)), None),
        Err(e) => (None, Some(e)),
    }
}

/// Add `location` to `styles` unless already present. Returns whether it was
/// added.
pub fn add_location(styles: &mut Vec<String>, location: &str) -> bool {
    let location = location.trim();
    if location.is_empty() || styles.iter().any(|s| s == location) {
        return false;
    }
    styles.push(location.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, transport_err, FakeClient};
    use std::path::PathBuf;

    const SHEET: &str = "way[highway=residential] { color: #ff0000; width: 7; }";

    fn temp_file(tag: &str, contents: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("osm-gpui-mapcss-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("style.mapcss");
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn display_names_are_short() {
        assert_eq!(display_name("/home/me/styles/roads.mapcss"), "roads.mapcss");
        assert_eq!(display_name("C:\\styles\\roads.mapcss"), "roads.mapcss");
        assert_eq!(
            display_name("https://example.com/css/roads.mapcss"),
            "roads.mapcss"
        );
        assert_eq!(display_name("https://example.com/"), "example.com");
    }

    #[test]
    fn urls_are_told_from_paths() {
        assert!(is_url("https://a.example/x.mapcss"));
        assert!(is_url("http://a.example/x.mapcss"));
        assert!(!is_url("/tmp/x.mapcss"));
    }

    #[test]
    fn no_choice_means_built_in_without_error() {
        let client = FakeClient::new(vec![]);
        for choice in [None, Some("  ")] {
            let (sheet, err) = resolve(choice, &client);
            assert!(sheet.is_none() && err.is_none());
        }
    }

    #[test]
    fn loads_a_style_from_a_file() {
        let path = temp_file("file", SHEET);
        let client = FakeClient::new(vec![]);
        let (sheet, err) = resolve(path.to_str(), &client);
        assert!(err.is_none());
        let tags = [("highway".to_string(), "residential".to_string())]
            .into_iter()
            .collect();
        let style = sheet.unwrap().way_style(&tags, false, 18.0);
        assert_eq!(style.width, 7.0);
        assert_eq!(client.request_count(), 0);
    }

    #[test]
    fn loads_a_style_from_a_url() {
        let client = FakeClient::new(vec![ok(200, SHEET)]);
        let (sheet, err) = resolve(Some("https://example.com/s.mapcss"), &client);
        assert!(err.is_none());
        assert!(sheet.is_some());
        assert_eq!(client.request_count(), 1);
    }

    #[test]
    fn a_missing_file_falls_back_with_an_error() {
        let client = FakeClient::new(vec![]);
        let (sheet, err) = resolve(Some("/definitely/not/here.mapcss"), &client);
        assert!(sheet.is_none());
        assert!(err.is_some());
    }

    #[test]
    fn a_failed_download_falls_back_with_an_error() {
        let client = FakeClient::new(vec![transport_err("offline")]);
        let (sheet, err) = resolve(Some("https://example.com/s.mapcss"), &client);
        assert!(sheet.is_none());
        assert!(err.unwrap().contains("offline"));
    }

    #[test]
    fn a_syntax_error_falls_back_with_an_error() {
        let path = temp_file("bad", "way[highway=residential] { color: #ff0000;");
        let client = FakeClient::new(vec![]);
        let (sheet, err) = resolve(path.to_str(), &client);
        assert!(sheet.is_none());
        assert!(err.is_some());
    }

    #[test]
    fn locations_are_added_once() {
        let mut styles = Vec::new();
        assert!(add_location(&mut styles, " /a.mapcss "));
        assert!(!add_location(&mut styles, "/a.mapcss"));
        assert!(!add_location(&mut styles, "   "));
        assert_eq!(styles, vec!["/a.mapcss".to_string()]);
    }
}
