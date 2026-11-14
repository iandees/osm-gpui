//! Command-line parsing and the window title / map-origin arithmetic.

use crate::{menu, parse_cli_args_from, window_title, window_to_map_origin, CliArgs, MapViewer};
use gpui::{point, px};
use osm_gpui::settings_store;
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn window_to_map_removes_the_toolbar_and_any_menu_bar_row() {
    let p = point(px(300.0), px(200.0));
    let without_bar = window_to_map_origin(p, px(0.0));
    assert_eq!(without_bar.x, px(300.0 - MapViewer::MODE_PANEL_WIDTH));
    assert_eq!(without_bar.y, px(200.0));
    let with_bar = window_to_map_origin(p, px(menu::IN_WINDOW_MENU_BAR_HEIGHT));
    assert_eq!(with_bar.x, without_bar.x);
    assert_eq!(with_bar.y, px(200.0 - menu::IN_WINDOW_MENU_BAR_HEIGHT));
}

fn args(list: &[&str]) -> CliArgs {
    parse_cli_args_from(list.iter().map(|s| s.to_string()))
}

#[test]
fn cli_positional_files_mix_with_flags() {
    let a = args(&[
        "a.osm",
        "--window-size",
        "800x600",
        "dir/b b.osm",
        "--keep-open",
        "--language",
        "de",
        "--script",
        "s.osmscript",
        "c.osm",
    ]);
    assert_eq!(
        a.files,
        vec![
            PathBuf::from("a.osm"),
            PathBuf::from("dir/b b.osm"),
            PathBuf::from("c.osm")
        ]
    );
    assert_eq!(a.window_size, Some((800, 600)));
    assert!(a.keep_open);
    assert_eq!(a.language.as_deref(), Some("de"));
    assert_eq!(a.script, Some(PathBuf::from("s.osmscript")));
}

#[test]
fn cli_offline_and_http_fixtures_flags() {
    let a = args(&[
        "--script",
        "s.osmscript",
        "--offline",
        "--http-fixtures",
        "fx/m.json",
    ]);
    assert!(a.offline);
    assert_eq!(a.http_fixtures, Some(PathBuf::from("fx/m.json")));
    let b = args(&["--script", "s.osmscript"]);
    assert!(!b.offline);
    assert_eq!(b.http_fixtures, None);
}

#[test]
fn cli_api_server_flag_values() {
    assert_eq!(args(&[]).api_server, None);
    assert_eq!(
        args(&["--api-server", "dev"]).api_server.as_deref(),
        Some(settings_store::DEV_API_URL)
    );
    assert_eq!(
        args(&["x.osm", "--api-server", "primary"])
            .api_server
            .as_deref(),
        Some(settings_store::PRIMARY_API_URL)
    );
    let a = args(&["--api-server", "http://127.0.0.1:9/", "b.osm"]);
    assert_eq!(a.api_server.as_deref(), Some("http://127.0.0.1:9"));
    // The value is not mistaken for a file.
    assert_eq!(a.files, vec![PathBuf::from("b.osm")]);
}

#[test]
#[should_panic(expected = "--api-server")]
fn cli_api_server_flag_rejects_unknown_values() {
    args(&["--api-server", "production"]);
}

#[test]
fn cli_script_delay_flag() {
    assert_eq!(args(&[]).script_delay, None);
    assert_eq!(
        args(&["--script-delay-ms", "400"]).script_delay,
        Some(Duration::from_millis(400))
    );
}

#[test]
fn the_api_server_override_is_not_part_of_the_saved_settings() {
    // The flag's value never enters `AppSettings`, the thing that is
    // written to settings.json.
    let a = args(&["--api-server", "dev"]);
    let saved = settings_store::AppSettings::default();
    let url = settings_store::effective_base_url(&saved, a.api_server.as_deref());
    assert_eq!(url, settings_store::DEV_API_URL);
    let json = serde_json::to_string(&saved).unwrap();
    assert!(!json.contains("master.apis.dev"), "{json}");
    assert!(json.contains("\"Primary\""), "{json}");
}

#[test]
fn window_title_names_a_non_primary_server() {
    assert_eq!(
        window_title(settings_store::PRIMARY_API_URL),
        menu::APP_NAME
    );
    let title = window_title(settings_store::DEV_API_URL);
    assert!(title.starts_with(menu::APP_NAME), "{title}");
    assert!(
        title.contains("master.apis.dev.openstreetmap.org"),
        "{title}"
    );
}

#[test]
fn cli_flag_values_are_not_taken_as_files() {
    let a = args(&["--script", "x.osmscript", "--language", "de"]);
    assert!(a.files.is_empty());
}

#[test]
fn cli_unknown_flags_are_ignored_and_double_dash_forces_files() {
    let a = args(&["--bogus", "a.osm", "--", "-weird.osm", "--keep-open"]);
    assert_eq!(
        a.files,
        vec![
            PathBuf::from("a.osm"),
            PathBuf::from("-weird.osm"),
            PathBuf::from("--keep-open")
        ]
    );
    assert!(!a.keep_open);
}

#[test]
fn cli_no_args_means_no_files() {
    assert!(args(&[]).files.is_empty());
}
