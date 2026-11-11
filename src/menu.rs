//! Menu bar construction and the free-function menu action handlers.

use gpui::{
    actions, point, px, size, App, AppContext as _, Bounds, Menu, MenuItem, SharedString,
    SystemMenuType, WindowOptions,
};

use gpui_component::menu::AppMenuBar;
use gpui_component::{GlobalState, Theme, ThemeMode};
use osm_gpui::custom_imagery_store::{self, CustomImageryEntry};
use osm_gpui::imagery;
use osm_gpui::osm::OsmParser;
use osm_gpui::tr;

use crate::selection_actions;
use crate::{
    with_map_viewer, with_map_viewer_in, AddCoordinateGrid, AddCustomImagery, AddImageryLayer,
    AddOsmCarto, AddSavedCustomImagery, ApplyNsiPreset, ChangeFeatureType, DeleteSelection,
    DeselectAll, DownloadFromOsm, EditModeAction, ExportOsmFile, HideApp, HideOtherApps,
    ImageryLoadState, LayerRequest, MinimizeWindow, OpenOsmFile, OpenSettings, Quit, Redo,
    RunSelectionAction, SelectAll, SetMode, ShowAllApps, ToggleDebugOverlay, ToggleDownloadedArea,
    Undo, UploadToOsm, ZoomIn, ZoomOut, ZoomToData, ZoomToSelection, ZoomWindow, IMAGERY_INDEX,
};

/// Guard to prevent opening multiple settings windows simultaneously.
static SETTINGS_WINDOW_OPEN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) fn custom_imagery_snapshot() -> Vec<CustomImageryEntry> {
    custom_imagery_store::snapshot()
}

// Handle the File > Open OSM File menu action. Picks a file (async, on the
// foreground executor so it can await the OS file-picker dialog), parses it
// on the background executor, then applies the result directly to the live
// `MapViewer` via `MAP_VIEWER_HANDLE` — mirrors the download flow in
// `MapViewer::request_download`.
pub(crate) fn open_osm_file(_: &OpenOsmFile, cx: &mut App) {
    cx.spawn(async move |cx| {
        let Some(file_path) = rfd::AsyncFileDialog::new()
            .add_filter(tr!("menu.open_dialog.osm_files").as_ref(), &["osm", "xml"])
            .add_filter(tr!("menu.open_dialog.all_files").as_ref(), &["*"])
            .set_title(tr!("menu.open_dialog.title").as_ref())
            .pick_file()
            .await
        else {
            return;
        };

        load_osm_path(file_path.path().to_path_buf(), cx).await;
    })
    .detach();
}

/// Open an `.osm` file given by path (command line, Finder, ...) as a new
/// layer, exactly as File > Open does after the picker; a file that cannot be
/// read or parsed shows a status message instead.
pub(crate) fn open_osm_path(path: std::path::PathBuf, cx: &mut App) {
    cx.spawn(async move |cx| load_osm_path(path, cx).await)
        .detach();
}

async fn load_osm_path(path: std::path::PathBuf, cx: &mut gpui::AsyncApp) {
    let path_str = path.to_string_lossy().to_string();

    let result = cx
        .background_executor()
        .spawn(async move {
            let parser = OsmParser::new();
            // With edits: a file saved by File > Export… (or JOSM) may
            // carry pending local changes, which must stay pending.
            parser.parse_file_with_edits(&path_str)
        })
        .await;

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("OSM")
        .to_string();
    cx.update(|cx| {
        with_map_viewer(cx, |v, cx| match result {
            Ok((osm_data, edits)) => v.add_osm_dataset(stem, osm_data, &edits, cx),
            Err(e) => {
                v.set_status(tr!("status.open_failed", path = path.display(), error = e));
                cx.notify();
            }
        });
    });
}

pub(crate) fn open_settings(_: &OpenSettings, cx: &mut App) {
    if SETTINGS_WINDOW_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    SETTINGS_WINDOW_OPEN.store(true, std::sync::atomic::Ordering::Relaxed);

    let settings_window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(Bounds {
                    origin: point(px(200.0), px(200.0)),
                    size: size(px(600.0), px(500.0)),
                })),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some(tr!("settings.window_title")),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                focus: true,
                ..Default::default()
            },
            |window, cx| {
                let view =
                    cx.new(|cx| osm_gpui::ui::settings_window::SettingsWindow::new(window, cx));
                cx.subscribe(
                    &view,
                    |_entity, event: &osm_gpui::ui::settings_window::SettingsEvent, cx| match event
                    {
                        osm_gpui::ui::settings_window::SettingsEvent::KeybindingsChanged => {
                            apply_keybindings_change(cx);
                        }
                    },
                )
                .detach();
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        )
        .unwrap();

    let settings_window_id = settings_window.window_id();
    cx.on_window_closed(move |_cx, window_id| {
        if window_id == settings_window_id {
            SETTINGS_WINDOW_OPEN.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    })
    .detach();
}

// Handle the File > Download from OSM menu action
pub(crate) fn download_from_osm(_: &DownloadFromOsm, cx: &mut App) {
    with_map_viewer(cx, |v, cx| v.request_download(cx));
}

// Global fallback for the File > Upload to OSM menu action; the normal path
// is `MapViewer::on_upload_to_osm`, a window-scoped listener. A global
// handler runs while the map window is still checked out for the dispatch, so
// looking it up via `with_map_viewer_in` right here silently did nothing (the
// bug this fixes). Deferred like `quit` so the lookup succeeds when no
// window-scoped listener was in the dispatch path (e.g. nothing focused).
pub(crate) fn upload_to_osm(_: &UploadToOsm, cx: &mut App) {
    cx.defer(|cx| with_map_viewer_in(cx, |v, window, cx| v.open_upload_dialog(window, cx)));
}

// Global fallback for the `Quit` action, registered once at app startup
// alongside the other `cx.on_action` handlers above. `MapViewer::on_quit` is
// only reachable via the window/entity-scoped `.on_action(cx.listener(...))`
// listener installed during `MapViewer::render`, which doesn't exist until
// the first render pass completes -- so at cold launch, before that first
// paint, GPUI's `App::is_action_available` (which macOS's menu validation
// queries directly) sees no listener at all for `Quit` and greys out the
// menu item / disables Cmd+Q. Registering this global handler makes it
// available immediately at startup, fixing that window.
//
// This only actually runs pre-first-render (afterwards, `on_quit`'s
// window-scoped listener handles the dispatch first and stops propagation
// before this Bubble-phase global listener is reached). Deferred because
// global action listeners fire while `MapViewer`'s window is still checked
// out for its own dispatch -- the same hazard `with_map_viewer_in` exists to
// avoid (see its note above `on_quit`); `cx.defer` waits until the window is
// checked back in before touching it.
pub(crate) fn quit(_: &Quit, cx: &mut App) {
    cx.defer(|cx| {
        if crate::has_unsaved_changes(cx) {
            with_map_viewer_in(cx, |v, window, cx| v.show_quit_confirm_dialog(window, cx));
        } else {
            cx.quit();
        }
    });
}

// Handle the Imagery > OpenStreetMap Carto menu action
pub(crate) fn add_osm_carto(_: &AddOsmCarto, cx: &mut App) {
    with_map_viewer(cx, |v, cx| {
        v.apply_layer_request(LayerRequest::OsmCarto, cx)
    });
}

// Handle an ELI imagery menu action. Looks up the entry in the loaded index
// and applies it as a layer request directly to the live `MapViewer`.
pub(crate) fn add_imagery_layer(action: &AddImageryLayer, cx: &mut App) {
    let id = action.id.to_string();
    let Some(index) = IMAGERY_INDEX.get() else {
        return;
    };
    let entry = {
        let guard = match index.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        guard.iter().find(|e| e.id == id).cloned()
    };
    let Some(entry) = entry else { return };
    with_map_viewer(cx, |v, cx| {
        v.apply_layer_request(
            LayerRequest::Imagery {
                name: entry.name,
                url_template: entry.url_template,
                min_zoom: entry.min_zoom,
                max_zoom: entry.max_zoom,
                attribution: entry.attribution,
            },
            cx,
        );
    });
}

// Handle the View > Toggle Debug Overlay menu action
pub(crate) fn toggle_debug_overlay(_: &ToggleDebugOverlay, cx: &mut App) {
    with_map_viewer(cx, |v, cx| v.toggle_debug_overlay(cx));
}

// Handle the Imagery > Add Custom Imagery… menu action
// (Global fallback, deferred; see `upload_to_osm`.)
pub(crate) fn open_custom_imagery_dialog(_: &AddCustomImagery, cx: &mut App) {
    cx.defer(|cx| with_map_viewer_in(cx, |v, window, cx| v.open_custom_imagery_dialog(window, cx)));
}

// Handle the Imagery > Custom Imagery > <saved entry> menu action
pub(crate) fn add_saved_custom_imagery(action: &AddSavedCustomImagery, cx: &mut App) {
    let entries = custom_imagery_snapshot();
    let Some(entry) = entries.get(action.index).cloned() else {
        eprintln!("add_saved_custom_imagery: stale index {}", action.index);
        return;
    };
    with_map_viewer(cx, |v, cx| {
        v.apply_layer_request(
            LayerRequest::Imagery {
                name: entry.name,
                url_template: entry.url_template,
                min_zoom: Some(entry.min_zoom),
                max_zoom: Some(entry.max_zoom),
                attribution: None,
            },
            cx,
        );
    });
}

// Handle the Imagery > Coordinate Grid menu action
pub(crate) fn add_coordinate_grid(_: &AddCoordinateGrid, cx: &mut App) {
    with_map_viewer(cx, |v, cx| {
        v.apply_layer_request(LayerRequest::CoordinateGrid, cx)
    });
}

/// Re-apply the live `gpui` keymap from current settings and refresh the
/// native menu's shortcut labels, in response to a
/// `SettingsEvent::KeybindingsChanged` from the Settings window. Menu
/// shortcut labels are baked in at `cx.set_menus` time (they don't update
/// on their own), so this always re-triggers a full `rebuild_menus` too.
fn apply_keybindings_change(cx: &mut App) {
    let settings = osm_gpui::settings_store::snapshot();
    cx.clear_key_bindings();
    cx.bind_keys(crate::key_bindings_from_settings(&settings));

    // rebuild_menus needs a map center and imagery-load state; pull the
    // live map viewer's if it exists yet, else fall back to the same
    // placeholder used before the map viewer/imagery index are ready (see
    // the startup call to `rebuild_menus` in `main.rs`).
    let (lat, lon, state, show_downloaded_area) = crate::MAP_VIEWER_HANDLE
        .get()
        .and_then(|h| h.upgrade())
        .map(|view| {
            let view = view.read(cx);
            let (lat, lon, state) = view.imagery_menu_context();
            (lat, lon, state, view.shows_downloaded_area())
        })
        .unwrap_or((40.7128, -74.0060, crate::ImageryLoadState::Loading, true));
    rebuild_menus(cx, lat, lon, state, show_downloaded_area);
}

/// Build and install the menu bar, using the current viewport center to filter
/// the Imagery menu to relevant ELI entries.
///
/// The one definition of the app's menus is [`build_menus`]. On macOS it
/// becomes the native menu bar (`cx.set_menus`); where the in-window bar is
/// shown (see [`in_window_menu_enabled`]) the same menus are also handed to
/// it, so every rebuild (language change, shortcut change, imagery update,
/// checkmark toggle) is reflected there too.
pub(crate) fn rebuild_menus(
    cx: &mut App,
    center_lat: f64,
    center_lon: f64,
    state: ImageryLoadState,
    show_downloaded_area: bool,
) {
    cx.set_menus(build_menus(
        center_lat,
        center_lon,
        state,
        show_downloaded_area,
    ));
    if in_window_menu_enabled() {
        // `Menu` is not `Clone`, so build the (cheap) list a second time for
        // the bar's owned copy.
        let owned = build_menus(center_lat, center_lon, state, show_downloaded_area)
            .into_iter()
            .map(Menu::owned)
            .collect();
        sync_in_window_menu(cx, owned);
    }
}

/// Name of the environment variable that forces the in-window menu bar on
/// macOS, where the native menu bar is normally the only one. Set it to `1`
/// to draw the in-window bar in addition to the native menu (for testing the
/// Windows/Linux bar without a Windows/Linux machine).
pub(crate) const IN_WINDOW_MENU_ENV: &str = "OSM_GPUI_IN_WINDOW_MENU";

/// Height of the in-window menu bar row, in pixels.
pub(crate) const IN_WINDOW_MENU_BAR_HEIGHT: f32 = 30.0;

/// Whether the in-window menu bar is wanted: always off macOS (GPUI draws no
/// native menu bar on Windows, Linux or FreeBSD), and on macOS only when the
/// test override is exactly `1`.
pub(crate) fn in_window_menu_wanted(is_macos: bool, override_value: Option<&str>) -> bool {
    !is_macos || override_value == Some("1")
}

/// [`in_window_menu_wanted`] for this process. Decided once. Always false in
/// unit tests, so the layout tests see the same window on every platform.
pub(crate) fn in_window_menu_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        !cfg!(test)
            && in_window_menu_wanted(
                cfg!(target_os = "macos"),
                std::env::var(IN_WINDOW_MENU_ENV).ok().as_deref(),
            )
    })
}

/// Height the in-window menu bar takes from the top of the window content
/// (zero when it is not shown).
pub(crate) fn top_chrome_height() -> gpui::Pixels {
    if in_window_menu_enabled() {
        px(IN_WINDOW_MENU_BAR_HEIGHT)
    } else {
        px(0.0)
    }
}

struct InWindowMenuBar(gpui::Entity<AppMenuBar>);

impl gpui::Global for InWindowMenuBar {}

/// Give the in-window bar the latest menus, creating it on first use.
fn sync_in_window_menu(cx: &mut App, menus: Vec<gpui::OwnedMenu>) {
    GlobalState::global_mut(cx).set_app_menus(menus);
    if let Some(bar) = cx.try_global::<InWindowMenuBar>().map(|g| g.0.clone()) {
        bar.update(cx, |bar, cx| bar.reload(cx));
    } else {
        let bar = AppMenuBar::new(cx);
        cx.set_global(InWindowMenuBar(bar));
    }
}

/// The in-window menu bar entity, if it is enabled and the menus have been
/// built.
pub(crate) fn in_window_menu_bar(cx: &App) -> Option<gpui::Entity<AppMenuBar>> {
    if !in_window_menu_enabled() {
        return None;
    }
    cx.try_global::<InWindowMenuBar>().map(|g| g.0.clone())
}

/// The app's menus: the single definition behind both the native macOS menu
/// bar and the in-window bar.
fn build_menus(
    center_lat: f64,
    center_lon: f64,
    state: ImageryLoadState,
    show_downloaded_area: bool,
) -> Vec<Menu> {
    let custom = custom_imagery_snapshot();
    let mut custom_items: Vec<MenuItem> = vec![MenuItem::action(
        tr!("menu.imagery.custom_add"),
        AddCustomImagery,
    )];
    if !custom.is_empty() {
        custom_items.push(MenuItem::separator());
        for (idx, entry) in custom.iter().enumerate() {
            custom_items.push(MenuItem::action(
                entry.name.clone(),
                AddSavedCustomImagery { index: idx },
            ));
        }
    }

    let mut imagery_items: Vec<MenuItem> = vec![
        MenuItem::submenu(Menu {
            name: tr!("menu.imagery.custom"),
            items: custom_items,
            disabled: false,
        }),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.imagery.osm_carto"), AddOsmCarto),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.imagery.coordinate_grid"), AddCoordinateGrid),
    ];

    match state {
        ImageryLoadState::Loading => {
            imagery_items.push(MenuItem::separator());
            imagery_items.push(MenuItem::action(
                tr!("menu.imagery.loading"),
                NoOpImageryInfo,
            ));
        }
        ImageryLoadState::Failed => {
            imagery_items.push(MenuItem::separator());
            imagery_items.push(MenuItem::action(
                tr!("menu.imagery.unavailable"),
                NoOpImageryInfo,
            ));
        }
        ImageryLoadState::Ready => {
            let entries = IMAGERY_INDEX
                .get()
                .and_then(|i| i.lock().ok().map(|g| g.clone()))
                .unwrap_or_default();
            let shown = imagery::entries_for_viewport(&entries, center_lat, center_lon);
            if !shown.is_empty() {
                imagery_items.push(MenuItem::separator());
                for entry in shown {
                    let label = if entry.best {
                        tr!("menu.imagery.best_entry", name = entry.name)
                    } else {
                        entry.name.clone().into()
                    };
                    imagery_items.push(MenuItem::action(
                        label,
                        AddImageryLayer {
                            id: entry.id.clone().into(),
                        },
                    ));
                }
            }
        }
    }

    let mode_item =
        |label: SharedString, mode: EditModeAction| MenuItem::action(label, SetMode { mode });

    vec![
        Menu {
            name: APP_NAME.into(),
            items: vec![
                MenuItem::action(tr!("menu.app.about", app = APP_NAME), ShowAbout),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.app.settings"), OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu(tr!("menu.app.services"), SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.app.hide", app = APP_NAME), HideApp),
                MenuItem::action(tr!("menu.app.hide_others"), HideOtherApps),
                MenuItem::action(tr!("menu.app.show_all"), ShowAllApps),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.app.quit", app = APP_NAME), Quit),
            ],
            disabled: false,
        },
        Menu {
            name: tr!("menu.file.title"),
            items: vec![
                MenuItem::action(tr!("menu.file.open"), OpenOsmFile),
                MenuItem::action(tr!("menu.file.export"), ExportOsmFile),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.file.download"), DownloadFromOsm),
                MenuItem::action(tr!("menu.file.upload"), UploadToOsm),
            ],
            disabled: false,
        },
        Menu {
            name: tr!("menu.edit.title"),
            items: vec![
                MenuItem::action(tr!("menu.edit.undo"), Undo),
                MenuItem::action(tr!("menu.edit.redo"), Redo),
                MenuItem::separator(),
                MenuItem::action(
                    tr!("menu.edit.copy_tags"),
                    RunSelectionAction {
                        id: "copy_tags".into(),
                    },
                ),
                MenuItem::action(
                    tr!("menu.edit.paste_tags"),
                    RunSelectionAction {
                        id: "paste_tags".into(),
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.edit.delete"), DeleteSelection),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.edit.select_all"), SelectAll),
                MenuItem::action(tr!("menu.edit.deselect_all"), DeselectAll),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.edit.change_feature_type"), ChangeFeatureType),
                MenuItem::action(tr!("menu.edit.apply_nsi_preset"), ApplyNsiPreset),
            ],
            disabled: false,
        },
        Menu {
            name: tr!("menu.tools.title"),
            // Every selection action, applicable or not: running one that
            // doesn't fit the selection explains why in the status bar.
            items: selection_actions::ACTIONS
                .iter()
                .filter(|a| a.in_tools_menu)
                .map(|a| MenuItem::action(a.label(), RunSelectionAction { id: a.id.into() }))
                .collect(),
            disabled: false,
        },
        Menu {
            name: tr!("menu.mode.title"),
            items: vec![
                mode_item(tr!("mode.select.name"), EditModeAction::Select),
                mode_item(tr!("mode.add.name"), EditModeAction::Add),
                mode_item(tr!("mode.building.name"), EditModeAction::Building),
                mode_item(tr!("mode.extrude.name"), EditModeAction::Extrude),
            ],
            disabled: false,
        },
        Menu {
            name: tr!("menu.imagery.title"),
            items: imagery_items,
            disabled: false,
        },
        Menu {
            name: tr!("menu.view.title"),
            items: vec![
                MenuItem::action(tr!("menu.view.zoom_in"), ZoomIn),
                MenuItem::action(tr!("menu.view.zoom_out"), ZoomOut),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.view.zoom_to_data"), ZoomToData),
                MenuItem::action(tr!("menu.view.zoom_to_selection"), ZoomToSelection),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.view.show_downloaded_area"), ToggleDownloadedArea)
                    .checked(show_downloaded_area),
                MenuItem::action(tr!("menu.view.toggle_debug_overlay"), ToggleDebugOverlay),
            ],
            disabled: false,
        },
        Menu {
            name: tr!("menu.window.title"),
            items: vec![
                MenuItem::action(tr!("menu.window.minimize"), MinimizeWindow),
                MenuItem::action(tr!("menu.window.zoom"), ZoomWindow),
            ],
            disabled: false,
        },
        Menu {
            name: tr!("menu.help.title"),
            items: vec![
                MenuItem::action(tr!("menu.help.map_features_wiki"), OpenMapFeaturesWiki),
                MenuItem::action(
                    tr!("menu.help.project_page", app = APP_NAME),
                    OpenProjectPage,
                ),
            ],
            disabled: false,
        },
    ]
}

/// The app's user-visible name: the app menu title, its Hide/Quit items,
/// and the main window's title.
pub(crate) const APP_NAME: &str = "osm-gpui";

actions!(osm_gpui, [OpenMapFeaturesWiki, OpenProjectPage, ShowAbout]);

/// Register the app-level (window-independent) handlers for the standard
/// macOS app-menu items and the Help menu links.
pub(crate) fn register_app_actions(cx: &mut App) {
    cx.on_action(|_: &HideApp, cx: &mut App| cx.hide());
    cx.on_action(|_: &HideOtherApps, cx: &mut App| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApps, cx: &mut App| cx.unhide_other_apps());
    cx.on_action(|_: &OpenMapFeaturesWiki, _: &mut App| {
        let _ = open::that("https://wiki.openstreetmap.org/wiki/Map_features");
    });
    cx.on_action(|_: &OpenProjectPage, _: &mut App| {
        let _ = open::that(env!("CARGO_PKG_REPOSITORY"));
    });
    cx.on_action(|_: &SetThemeLight, cx: &mut App| set_theme_mode(ThemeMode::Light, cx));
    cx.on_action(|_: &SetThemeDark, cx: &mut App| set_theme_mode(ThemeMode::Dark, cx));
}

// Debug-friendly theme switches with no menu item: reachable from the script
// harness (`action SetThemeDark`) and mirrored by the `--theme` flag. The app
// does not follow the system appearance; it starts in the light theme.
actions!(osm_gpui, [SetThemeLight, SetThemeDark]);

/// Switch the gpui-component theme and repaint every window.
pub(crate) fn set_theme_mode(mode: ThemeMode, cx: &mut App) {
    Theme::change(mode, None, cx);
    Theme::sync_scrollbar_appearance(cx);
    cx.refresh_windows();
}

// Dummy action used for disabled-style "info" entries in the Imagery menu.
// (GPUI does not support disabled menu items directly, so we use a no-op.)
actions!(osm_gpui, [NoOpImageryInfo]);

pub(crate) fn no_op_imagery_info(_: &NoOpImageryInfo, _cx: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_window_menu_is_wanted_off_macos_regardless_of_override() {
        assert!(in_window_menu_wanted(false, None));
        assert!(in_window_menu_wanted(false, Some("0")));
        assert!(in_window_menu_wanted(false, Some("1")));
    }

    #[test]
    fn in_window_menu_on_macos_needs_the_override_set_to_one() {
        assert!(!in_window_menu_wanted(true, None));
        assert!(!in_window_menu_wanted(true, Some("")));
        assert!(!in_window_menu_wanted(true, Some("0")));
        assert!(!in_window_menu_wanted(true, Some("true")));
        assert!(in_window_menu_wanted(true, Some("1")));
    }

    #[test]
    fn in_window_menu_is_off_in_unit_tests() {
        assert!(!in_window_menu_enabled());
        assert_eq!(top_chrome_height(), px(0.0));
    }

    #[test]
    fn menus_convert_to_owned_with_every_top_level_menu_and_the_file_items() {
        let menus: Vec<gpui::OwnedMenu> = build_menus(40.0, -74.0, ImageryLoadState::Loading, true)
            .into_iter()
            .map(Menu::owned)
            .collect();
        let names: Vec<String> = menus.iter().map(|m| m.name.to_string()).collect();
        assert_eq!(names.len(), 9, "{names:?}");
        assert_eq!(names[0], APP_NAME);
        let file = menus
            .iter()
            .find(|m| m.name.as_str() == tr!("menu.file.title").as_ref())
            .expect("File menu");
        let actions = file
            .items
            .iter()
            .filter(|i| matches!(i, gpui::OwnedMenuItem::Action { .. }))
            .count();
        assert_eq!(actions, 4);
    }
}
