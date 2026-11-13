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
use crate::http::{HttpClient, HttpRequest};
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

    fn tags(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn wireframe_draws_every_way_alike_on_black_without_imagery_or_fills() {
        let wire = Stylesheet::parse(BUILT_IN_STYLES[1].source).unwrap();
        let canvas = wire.canvas_style();
        assert_eq!(canvas.background, Some(0x000000));
        assert!(!canvas.show_imagery);

        let plain = wire.way_style(&tags(&[]), false, 18.0);
        for (k, v) in [
            ("highway", "motorway"),
            ("building", "yes"),
            ("natural", "water"),
            ("railway", "rail"),
        ] {
            let s = wire.way_style(&tags(&[(k, v)]), true, 18.0);
            assert_eq!(s.width, 1.0, "{k}={v}");
            assert!(s.fill.is_none(), "{k}={v}");
            assert_eq!(s.color, plain.color, "{k}={v}");
            assert_eq!(s.casing_width, 0.0, "{k}={v}");
            assert!(s.dashes.is_none(), "{k}={v}");
        }
        // Dark blue: blue dominates, and it is not bright.
        let (r, g, b) = (
            plain.color >> 16,
            (plain.color >> 8) & 0xff,
            plain.color & 0xff,
        );
        assert!(b > r && b > g && b < 0xf0);
    }

    #[test]
    fn wireframe_outlines_relation_members_in_cyan() {
        let wire = Stylesheet::parse(BUILT_IN_STYLES[1].source).unwrap();
        assert!(wire.uses_relation_membership());
        let t = tags(&[("highway", "residential")]);
        let alone = wire.way_style_cached(&t, false, false);
        let member = wire.way_style_cached(&t, false, true);
        assert_eq!(alone.casing_width, 0.0);
        assert!(member.casing_width > 0.0);
        assert_eq!(member.width, 1.0);
        assert_eq!(member.color, alone.color);
        // Cyan or teal: green and blue both well above red.
        let c = member.casing_color;
        assert!((c >> 16) < 0x40 && ((c >> 8) & 0xff) > 0x80 && (c & 0xff) > 0x80);
        assert_ne!(alone.sig, member.sig);
    }

    #[test]
    fn wireframe_nodes_are_squares_and_crosses_by_role() {
        use crate::style::mapcss::NodeShape;
        let wire = Stylesheet::parse(BUILT_IN_STYLES[1].source).unwrap();
        let vertex = wire.node_style(&tags(&[]), 18.0);
        assert!(vertex.has_custom_marker());
        assert_eq!(vertex.color, 0xffd60a);
        // A vertex in one way: a small square. In two: a larger square.
        let one = vertex.visual(1);
        let shared = vertex.visual(2);
        assert_eq!(one.shape, NodeShape::Square);
        assert_eq!(shared.shape, NodeShape::Square);
        assert!(shared.size > one.size);
        // Free-standing nodes and tagged nodes are crosses.
        assert_eq!(vertex.visual(0).shape, NodeShape::Cross);
        let poi = wire.node_style(&tags(&[("amenity", "cafe")]), 18.0);
        assert_eq!(poi.visual(1).shape, NodeShape::Cross);
        assert_eq!(poi.visual(2).shape, NodeShape::Cross);
        assert_eq!(poi.visual(1).color, 0xffd60a);
    }

    #[test]
    fn wireframe_has_no_labels_or_icons() {
        let wire = Stylesheet::parse(BUILT_IN_STYLES[1].source).unwrap();
        let t = tags(&[("name", "X"), ("amenity", "cafe")]);
        assert!(wire.node_text_style(&t, 19.0).is_empty());
        assert!(wire.way_text_style(&t, true, 19.0).is_empty());
    }

    #[test]
    fn the_default_style_keeps_the_theme_background_and_imagery() {
        let default = Stylesheet::parse(BUILT_IN_STYLES[0].source).unwrap();
        assert_eq!(default.canvas_style(), Default::default());
        assert!(default.canvas_style().show_imagery);
        assert!(!default.uses_relation_membership());
        assert!(!default.node_style(&tags(&[]), 18.0).has_custom_marker());
    }

    #[test]
    fn id_colours_roads_by_class_with_casings_and_dashed_paths() {
        let id = Stylesheet::parse(BUILT_IN_STYLES[2].source).unwrap();
        assert_eq!(id.canvas_style(), Default::default());
        let road = |v: &str, z: f64| id.way_style(&tags(&[("highway", v)]), false, z);
        // Colour and width follow the class.
        let motorway = road("motorway", 17.0);
        let primary = road("primary", 17.0);
        let secondary = road("secondary", 17.0);
        let tertiary = road("tertiary", 17.0);
        let residential = road("residential", 17.0);
        let service = road("service", 17.0);
        assert_eq!(residential.color, 0xffffff);
        assert_eq!(service.color, 0xffffff);
        assert_ne!(motorway.color, primary.color);
        assert_ne!(primary.color, secondary.color);
        assert_ne!(secondary.color, tertiary.color);
        assert!(motorway.width > primary.width);
        assert!(primary.width > secondary.width);
        assert!(secondary.width > residential.width);
        assert!(residential.width > service.width);
        // Every road class is drawn inside a casing, darker than its fill.
        for s in [motorway, primary, secondary, tertiary, residential, service] {
            assert!(s.casing_width > 0.0);
            assert!(s.casing_color != s.color);
        }
        // Roads widen with zoom, and keep their ordering.
        for v in [
            "motorway",
            "primary",
            "secondary",
            "tertiary",
            "residential",
            "service",
        ] {
            assert!(road(v, 16.5).width > road(v, 14.0).width, "{v} z16");
            assert!(road(v, 18.5).width > road(v, 16.5).width, "{v} z18");
            assert!(road(v, 19.5).width > road(v, 18.5).width, "{v} z19");
        }
        // Paths are dashed, thin and uncased; the cycleway is blue.
        for v in ["footway", "path", "track", "cycleway"] {
            let s = road(v, 17.0);
            assert!(s.dashes.is_some(), "{v}");
            assert_eq!(s.casing_width, 0.0, "{v}");
            assert!(s.width < residential.width, "{v}");
        }
        let cycleway = road("cycleway", 17.0);
        assert!((cycleway.color & 0xff) > (cycleway.color >> 16));
        // Railways are dark and dashed.
        let rail = id.way_style(&tags(&[("railway", "rail")]), false, 17.0);
        assert!(rail.dashes.is_some());
        let luma = |c: u32| (c >> 16) + ((c >> 8) & 0xff) + (c & 0xff);
        assert!(luma(rail.color) < 0x180);
        // One-way roads carry arrows.
        let oneway = tags(&[("highway", "residential"), ("oneway", "yes")]);
        assert_ne!(
            id.way_style_cached(&oneway, false, false).direction,
            crate::style::Direction::None
        );
    }

    #[test]
    fn id_draws_areas_translucent_with_an_outline() {
        let id = Stylesheet::parse(BUILT_IN_STYLES[2].source).unwrap();
        for (k, v) in [
            ("leisure", "park"),
            ("landuse", "forest"),
            ("landuse", "residential"),
            ("natural", "water"),
            ("natural", "wood"),
            ("building", "yes"),
        ] {
            let s = id.way_style(&tags(&[(k, v)]), true, 18.0);
            let fill = s.fill.unwrap_or_else(|| panic!("{k}={v} has a fill"));
            assert!(fill.opacity > 0.0 && fill.opacity < 0.6, "{k}={v}");
            assert!(s.width >= 1.5, "{k}={v} has an outline");
            // Open ways are not filled.
            assert!(id.way_style(&tags(&[(k, v)]), false, 18.0).fill.is_none());
        }
        // Green for park and wood, blue for water.
        let green = |c: u32| ((c >> 8) & 0xff) > (c >> 16) && ((c >> 8) & 0xff) > (c & 0xff);
        let park = id.way_style(&tags(&[("leisure", "park")]), true, 18.0);
        assert!(green(park.fill.unwrap().color));
        let water = id.way_style(&tags(&[("natural", "water")]), true, 18.0);
        assert!((water.fill.unwrap().color & 0xff) > (water.fill.unwrap().color >> 16));
    }

    #[test]
    fn id_labels_are_dark_on_a_white_halo() {
        let id = Stylesheet::parse(BUILT_IN_STYLES[2].source).unwrap();
        let cafe = tags(&[("amenity", "cafe"), ("name", "C")]);
        let label = id.node_text_style(&cafe, 18.0);
        assert_eq!(label.halo_color, Some(0xffffff));
        let text = label.color.unwrap();
        assert!(((text >> 16) + ((text >> 8) & 0xff) + (text & 0xff)) < 0x180);
        assert!(label.icon_image.is_some());
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
