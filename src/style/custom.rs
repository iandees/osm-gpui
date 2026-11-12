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

/// A MapCSS style shipped inside the app.
pub struct BuiltInStyle {
    /// Stable id, persisted in settings as `builtin:<id>` (the default style
    /// is persisted as no choice at all).
    pub id: &'static str,
    /// Locale key of the name shown in the Settings dropdown.
    pub label_key: &'static str,
    pub source: &'static str,
}

/// The shipped styles, the default first. Ids must never change: they are
/// saved in users' settings files.
pub const BUILT_IN_STYLES: [BuiltInStyle; 3] = [
    BuiltInStyle {
        id: "default",
        label_key: "settings.map_style.built_in",
        source: include_str!("../../assets/default.mapcss"),
    },
    BuiltInStyle {
        id: "wireframe",
        label_key: "settings.map_style.wireframe",
        source: include_str!("../../assets/wireframe.mapcss"),
    },
    BuiltInStyle {
        id: "id",
        label_key: "settings.map_style.id",
        source: include_str!("../../assets/id.mapcss"),
    },
];

const BUILT_IN_PREFIX: &str = "builtin:";

/// The settings value that selects the built-in style `id`; `None` for the
/// default style, which is stored as "no custom style".
pub fn built_in_location(id: &str) -> Option<String> {
    (id != BUILT_IN_STYLES[0].id).then(|| format!("{BUILT_IN_PREFIX}{id}"))
}

/// The shipped style a settings value names, if it names one.
pub fn built_in_for(location: &str) -> Option<&'static BuiltInStyle> {
    let id = location.strip_prefix(BUILT_IN_PREFIX)?;
    BUILT_IN_STYLES.iter().find(|s| s.id == id)
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
    if location.starts_with(BUILT_IN_PREFIX) {
        built_in_for(location)
            .map(|s| s.source.to_string())
            .ok_or_else(|| format!("unknown built-in style {location}"))
    } else if is_url(location) {
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
    if location.is_empty()
        || location.starts_with(BUILT_IN_PREFIX)
        || styles.iter().any(|s| s == location)
    {
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
    fn every_built_in_style_parses() {
        for style in &BUILT_IN_STYLES {
            Stylesheet::parse(style.source)
                .unwrap_or_else(|e| panic!("{} must parse: {e}", style.id));
        }
    }

    #[test]
    fn built_in_ids_are_stable() {
        let ids: Vec<_> = BUILT_IN_STYLES.iter().map(|s| s.id).collect();
        assert_eq!(ids, ["default", "wireframe", "id"]);
        assert_eq!(built_in_location("default"), None);
        assert_eq!(
            built_in_location("wireframe").as_deref(),
            Some("builtin:wireframe")
        );
        assert_eq!(built_in_location("id").as_deref(), Some("builtin:id"));
        assert_eq!(built_in_for("builtin:id").unwrap().id, "id");
        assert!(built_in_for("builtin:nope").is_none());
        assert!(built_in_for("/builtin:id").is_none());
    }

    #[test]
    fn built_in_locations_resolve_without_files_or_network() {
        let client = FakeClient::new(vec![]);
        for id in ["wireframe", "id"] {
            let loc = built_in_location(id).unwrap();
            let (sheet, err) = resolve(Some(&loc), &client);
            assert!(sheet.is_some() && err.is_none());
        }
        let (sheet, err) = resolve(Some("builtin:gone"), &client);
        assert!(sheet.is_none() && err.is_some());
        assert_eq!(client.request_count(), 0);
        let mut styles = Vec::new();
        assert!(!add_location(&mut styles, "builtin:id"));
    }

    #[test]
    fn wireframe_is_thin_and_unfilled_and_id_fills_buildings() {
        let tags = |k: &str, v: &str| -> std::collections::HashMap<String, String> {
            [(k.to_string(), v.to_string())].into_iter().collect()
        };
        let wire = Stylesheet::parse(BUILT_IN_STYLES[1].source).unwrap();
        for (k, v) in [
            ("highway", "motorway"),
            ("building", "yes"),
            ("natural", "water"),
        ] {
            let s = wire.way_style(&tags(k, v), true, 18.0);
            assert_eq!(s.width, 1.0, "{k}={v}");
            assert!(s.fill.is_none(), "{k}={v}");
        }
        let id = Stylesheet::parse(BUILT_IN_STYLES[2].source).unwrap();
        assert!(id
            .way_style(&tags("building", "yes"), true, 18.0)
            .fill
            .is_some());
        let motorway = id.way_style(&tags("highway", "motorway"), false, 15.0);
        let residential = id.way_style(&tags("highway", "residential"), false, 15.0);
        assert!(motorway.width > residential.width);
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
