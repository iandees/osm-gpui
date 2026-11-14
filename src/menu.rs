//! Menu bar construction and the free-function menu action handlers.

use gpui::{
    actions, point, px, size, App, AppContext as _, Bounds, Menu, MenuItem, SystemMenuType,
    WindowOptions,
};

use gpui_component::menu::AppMenuBar;
use gpui_component::{GlobalState, ThemeMode};
use osm_gpui::custom_imagery_store::{self, CustomImageryEntry};
use osm_gpui::imagery;
use osm_gpui::osm::OsmParser;
use osm_gpui::tr;
use osm_gpui::ui::settings_window::SettingsPage;

use crate::gps_traces_ui::DownloadGpsTraces;
use crate::history_actions::ShowObjectHistory;
use crate::notes_ui::{NewNote, RefreshNotes, ToggleNotes};
use crate::selection_actions;
use crate::street_imagery_ui::{
    RefreshStreetImagery, ToggleFotoshi, ToggleMapillary, TogglePanoramax,
};
use crate::{
    with_map_viewer, with_map_viewer_in, AddCoordinateGrid, AddCustomImagery, AddImageryLayer,
    AddOsmCarto, AddSavedCustomImagery, ApplyNsiPreset, ChangeFeatureType, DeleteSelection,
    DeselectAll, DownloadFromOsm, EditMode, ExportOsmFile, Find, HideApp, HideOtherApps,
    ImageryLoadState, LayerRequest, MinimizeWindow, OpenOsmFile, OpenSettings, Quit, Redo,
    RunSelectionAction, ScriptSeedCustomImagery, SelectAll, SetMode, ShowAllApps,
    ToggleDebugOverlay, ToggleDownloadedArea, Undo, UploadToOsm, Validate, ZoomIn, ZoomOut,
    ZoomToData, ZoomToSelection, ZoomWindow, IMAGERY_INDEX,
};

/// The Settings window, once opened. Whether it is still open is read from
/// the app's window list, not remembered, so closing it any way at all
/// leaves nothing stale.
#[derive(Default)]
struct SettingsWindowSlot(Option<gpui::AnyWindowHandle>);

impl gpui::Global for SettingsWindowSlot {}

/// What asking for Settings does.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SettingsOpen {
    /// Open a window, on `page` when one was asked for.
    New { page: Option<SettingsPage> },
    /// There is one already: bring it to the front.
    Focus,
}

/// Settings is a single window: open it, or focus the one that exists.
pub(crate) fn settings_open_decision(
    already_open: bool,
    page: Option<SettingsPage>,
) -> SettingsOpen {
    if already_open {
        SettingsOpen::Focus
    } else {
        SettingsOpen::New { page }
    }
}

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
            .add_filter(
                tr!("menu.open_dialog.map_files").as_ref(),
                &["osm", "xml", "gpx", "geojson", "json"],
            )
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

/// What a background load of a file produced.
enum Loaded {
    Osm(Box<Result<(osm_gpui::osm::OsmData, osm_gpui::osm::FileEdits), String>>),
    Reference(Result<osm_gpui::reference::ReferenceData, String>),
}

/// Open a file by type: `.gpx` and `.geojson`/`.json` (and `.xml`/other
/// files that start like one) become read-only reference layers, everything
/// else is OSM XML. Parsed on the background executor.
async fn load_osm_path(path: std::path::PathBuf, cx: &mut gpui::AsyncApp) {
    let path_str = path.to_string_lossy().to_string();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("OSM")
        .to_string();
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&path_str)
        .to_string();
    cx.update(|cx| {
        with_map_viewer(cx, |v, cx| {
            v.set_status(tr!("status.opening", name = file_name));
            cx.notify();
        });
    });

    let bg_path = path.clone();
    let result = cx
        .background_executor()
        .spawn(async move {
            if let Some(format) = osm_gpui::reference::sniff_path(&bg_path) {
                return Loaded::Reference(
                    osm_gpui::reference::load_file(&bg_path, format).map_err(|e| e.to_string()),
                );
            }
            let parser = OsmParser::new();
            // With edits: a file saved by File > Export… (or JOSM) may
            // carry pending local changes, which must stay pending.
            Loaded::Osm(Box::new(
                parser
                    .parse_file_with_edits(&path_str)
                    .map_err(|e| e.to_string()),
            ))
        })
        .await;

    cx.update(|cx| {
        with_map_viewer(cx, |v, cx| {
            let failure = match result {
                Loaded::Osm(loaded) => match *loaded {
                    Ok((osm_data, edits)) => {
                        v.add_osm_dataset(stem, osm_data, &edits, cx);
                        v.note_last_layer_source(&path);
                        return;
                    }
                    Err(e) => e,
                },
                Loaded::Reference(Ok(data)) => {
                    v.add_reference_dataset(stem, data, cx);
                    return;
                }
                Loaded::Reference(Err(e)) => e,
            };
            v.set_status(tr!(
                "status.open_failed",
                path = path.display(),
                error = failure
            ));
            cx.notify();
        });
    });
}

pub(crate) fn open_settings(_: &OpenSettings, cx: &mut App) {
    open_settings_page(None, cx);
}

/// Open the Settings window on `page` (the first when `None`), or focus it if
/// it is already open.
pub(crate) fn open_settings_page(page: Option<SettingsPage>, cx: &mut App) {
    let existing = cx
        .try_global::<SettingsWindowSlot>()
        .and_then(|slot| slot.0)
        .filter(|handle| cx.windows().contains(handle));
    let page = match settings_open_decision(existing.is_some(), page) {
        SettingsOpen::Focus => {
            if let Some(handle) = existing {
                let _ = handle.update(cx, |_, window, _| window.activate_window());
            }
            return;
        }
        SettingsOpen::New { page } => page,
    };

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
                let view = cx
                    .new(|cx| osm_gpui::ui::settings_window::SettingsWindow::new(page, window, cx));
                cx.subscribe(
                    &view,
                    |_entity, event: &osm_gpui::ui::settings_window::SettingsEvent, cx| match event
                    {
                        osm_gpui::ui::settings_window::SettingsEvent::KeybindingsChanged => {
                            apply_keybindings_change(cx);
                        }
                        osm_gpui::ui::settings_window::SettingsEvent::MapStyleChanged => {
                            crate::with_map_viewer(cx, |v, cx| v.apply_map_style(cx));
                        }
                    },
                )
                .detach();
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        )
        .unwrap();
    cx.set_global(SettingsWindowSlot(Some(settings_window.into())));
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

// Global fallback for Tools > Validate, for the same reason as
// `upload_to_osm`; the normal path is `MapViewer::on_validate`.
pub(crate) fn validate(_: &Validate, cx: &mut App) {
    cx.defer(|cx| with_map_viewer_in(cx, |v, _window, cx| v.show_validation(cx)));
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
                imagery_id: Some(entry.id),
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
                imagery_id: None,
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
    cx.bind_keys(crate::key_bindings_from_settings(
        &settings,
        cfg!(target_os = "macos"),
    ));
    rebuild_menus_for_live_viewer(cx);
}

/// Scripted runs only (a no-op otherwise): add two sample entries to the
/// custom imagery list (which starts empty in a scripted run) in memory, never
/// to the user's file, and rebuild the menus so Imagery > Custom has something
/// in it.
fn script_seed_custom_imagery(_: &ScriptSeedCustomImagery, cx: &mut App) {
    if !crate::script_harness::script_mode() {
        return;
    }
    for name in ["Sample Imagery A", "Sample Imagery B"] {
        custom_imagery_store::append_in_memory(CustomImageryEntry {
            name: name.into(),
            url_template: "https://tile.example.invalid/{z}/{x}/{y}.png".into(),
            min_zoom: 0,
            max_zoom: 19,
        });
    }
    rebuild_menus_for_live_viewer(cx);
}

/// The Edit menu entries for the selection actions listed in `slot`.
fn edit_action_items(slot: selection_actions::MenuSlot) -> impl Iterator<Item = MenuItem> {
    selection_actions::in_menu(slot)
        .map(|a| MenuItem::action(a.label(), RunSelectionAction { id: a.id.into() }))
}

pub(crate) fn rebuild_menus_for_live_viewer(cx: &mut App) {
    // rebuild_menus needs a map center and imagery-load state; pull the
    // live map viewer's if it exists yet, else fall back to the same
    // placeholder used before the map viewer/imagery index are ready (see
    // the startup call to `rebuild_menus` in `main.rs`).
    let (lat, lon, state, show_downloaded_area, can_upload) = crate::MAP_VIEWER_HANDLE
        .get()
        .and_then(|h| h.upgrade())
        .map(|view| {
            let view = view.read(cx);
            let (lat, lon, state) = view.imagery_menu_context();
            (
                lat,
                lon,
                state,
                view.shows_downloaded_area(),
                view.has_uploadable_changes(),
            )
        })
        .unwrap_or((
            40.7128,
            -74.0060,
            crate::ImageryLoadState::Loading,
            true,
            false,
        ));
    rebuild_menus(cx, lat, lon, state, show_downloaded_area, can_upload);
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
    can_upload: bool,
) {
    cx.set_menus(build_menus(
        cfg!(target_os = "macos"),
        center_lat,
        center_lon,
        state,
        show_downloaded_area,
        can_upload,
    ));
    if in_window_menu_enabled() {
        // `Menu` is not `Clone`, so build the (cheap) list a second time for
        // the bar's owned copy.
        let owned = build_menus(
            in_window_menu_is_macos_layout(),
            center_lat,
            center_lon,
            state,
            show_downloaded_area,
            can_upload,
        )
        .into_iter()
        .map(|menu| fit_owned_menu(menu.owned()))
        .collect();
        sync_in_window_menu(cx, owned);
    }
}

/// Most entries (separators and submenu rows included) a menu of the
/// in-window bar may have for its submenus to work.
///
/// gpui-component's popup menu draws a submenu inside its parent's element
/// tree, and makes a parent scrollable once it has MORE than 20 entries
/// (`PopupMenu::with_menu_items`); a scrollable parent clips the submenu away
/// (documented on `PopupMenuItem::Submenu`: only supported when the parent is
/// not `scrollable`). A long menu is therefore kept at or under the limit by
/// moving its longest run of plain items into a submenu; that submenu is
/// itself the scrollable leaf. The native macOS menu is not touched.
pub(crate) const IN_WINDOW_MENU_MAX_ITEMS: usize = 20;

/// [`fit_owned_items`] for a top-level menu. The Imagery menu's long list gets
/// a name of its own; anything else that overflows goes under "More".
fn fit_owned_menu(mut menu: gpui::OwnedMenu) -> gpui::OwnedMenu {
    let label = if menu.name.as_str() == tr!("menu.imagery.title").as_str() {
        tr!("menu.imagery.available").to_string()
    } else {
        tr!("menu.more").to_string()
    };
    menu.items = fit_owned_items(menu.items, IN_WINDOW_MENU_MAX_ITEMS, &label);
    menu
}

/// Make `items` work as a menu of the in-window bar: at most `limit` entries,
/// by moving the longest run of consecutive action items into a submenu named
/// `label` (further groups are named "More") until it fits. Submenus already in
/// the list are fitted the same way, and one that still ends up over the limit
/// (so it scrolls) has any submenu inside it flattened away, since a scrolling
/// menu cannot show those. If no run of two or more actions is left to group,
/// the submenus of the list are flattened instead: a menu that scrolls never
/// keeps a submenu that cannot open.
fn fit_owned_items(
    items: Vec<gpui::OwnedMenuItem>,
    limit: usize,
    label: &str,
) -> Vec<gpui::OwnedMenuItem> {
    let mut items: Vec<gpui::OwnedMenuItem> = items
        .into_iter()
        .map(|item| match item {
            gpui::OwnedMenuItem::Submenu(mut sub) => {
                sub.items = fit_owned_items(sub.items, limit, &tr!("menu.more"));
                if sub.items.len() > limit {
                    sub.items = flatten_owned_items(sub.items);
                }
                gpui::OwnedMenuItem::Submenu(sub)
            }
            other => other,
        })
        .collect();
    let mut name = label.to_string();
    while items.len() > limit {
        let Some((start, len)) = longest_action_run(&items) else {
            return flatten_owned_items(items);
        };
        let run: Vec<_> = items.drain(start..start + len).collect();
        items.insert(
            start,
            gpui::OwnedMenuItem::Submenu(gpui::OwnedMenu {
                name: name.clone().into(),
                items: run,
                disabled: false,
            }),
        );
        name = tr!("menu.more").to_string();
    }
    items
}

/// Start and length of the longest run of two or more consecutive action
/// items (the first such run when there is a tie).
fn longest_action_run(items: &[gpui::OwnedMenuItem]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    let mut start = 0;
    while start < items.len() {
        let len = items[start..]
            .iter()
            .take_while(|i| matches!(i, gpui::OwnedMenuItem::Action { .. }))
            .count();
        if len >= 2 && best.is_none_or(|(_, b)| len > b) {
            best = Some((start, len));
        }
        start += len.max(1);
    }
    best
}

/// Replace every submenu in `items` by a disabled header line followed by its
/// own (recursively flattened) items. Order is kept, no action is dropped, and
/// a submenu with nothing in it is omitted altogether.
fn flatten_owned_items(items: Vec<gpui::OwnedMenuItem>) -> Vec<gpui::OwnedMenuItem> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            gpui::OwnedMenuItem::Submenu(sub) => {
                let inner = flatten_owned_items(sub.items);
                if inner.is_empty() {
                    continue;
                }
                out.push(gpui::OwnedMenuItem::Action {
                    name: sub.name.to_string(),
                    action: Box::new(NoOpImageryInfo),
                    os_action: None,
                    checked: false,
                    disabled: true,
                });
                out.extend(inner);
            }
            other => out.push(other),
        }
    }
    out
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
    !is_macos || matches!(override_value, Some("1" | IN_WINDOW_MENU_OTHER))
}

/// Override value (`OSM_GPUI_IN_WINDOW_MENU=other`) that forces the in-window
/// bar on macOS like `1` does, but fills it with the Windows/Linux menu
/// contents instead of the macOS ones. The native menu stays the macOS one.
/// A developer aid for seeing the non-macOS layout without that platform.
pub(crate) const IN_WINDOW_MENU_OTHER: &str = "other";

/// Whether the in-window bar shows the macOS menu layout: only on macOS, and
/// not when the override asks for the `other` platform's contents.
pub(crate) fn in_window_menu_uses_macos_layout(
    is_macos: bool,
    override_value: Option<&str>,
) -> bool {
    is_macos && override_value != Some(IN_WINDOW_MENU_OTHER)
}

fn in_window_menu_is_macos_layout() -> bool {
    in_window_menu_uses_macos_layout(
        cfg!(target_os = "macos"),
        std::env::var(IN_WINDOW_MENU_ENV).ok().as_deref(),
    )
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

/// Whether a dropdown of the in-window bar is open: the popup holds the focus
/// then, and the bar's key context is above it.
pub(crate) fn in_window_menu_open(window: &gpui::Window) -> bool {
    in_window_menu_enabled()
        && window
            .context_stack()
            .iter()
            .any(|c| c.contains("AppMenuBar"))
}

/// Close whatever dropdown of the in-window bar is open (reloading the bar
/// drops its selection and popups).
pub(crate) fn close_in_window_menu(cx: &mut App) {
    if let Some(bar) = in_window_menu_bar(cx) {
        bar.update(cx, |bar, cx| bar.reload(cx));
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
        // Escape closes the whole menu. The popup's own Escape does nothing
        // while a submenu row of it is selected (it keeps the submenu), so
        // the bar is closed here, before the keystroke reaches any action.
        cx.intercept_keystrokes(|event, window, cx| {
            if event.keystroke.key == "escape" && in_window_menu_open(window) {
                close_in_window_menu(cx);
                if let Some(view) = crate::MAP_VIEWER_HANDLE.get().and_then(|h| h.upgrade()) {
                    let focus = view.read(cx).focus_handle.clone();
                    window.focus(&focus, cx);
                }
                cx.stop_propagation();
            }
        })
        .detach();
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
///
/// `is_macos` selects the platform's layout. macOS gets the app-name menu with
/// About, Settings, Services, Hide, Hide Others, Show All and Quit. Elsewhere
/// that menu is dropped, because Hide/Hide Others/Show All/Services do nothing
/// off macOS, and the rest moves to where Windows and Linux users look for it:
/// Quit at the end of File, Settings at the end of Edit, About at the end of
/// Help.
fn build_menus(
    is_macos: bool,
    center_lat: f64,
    center_lon: f64,
    state: ImageryLoadState,
    show_downloaded_area: bool,
    can_upload: bool,
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
        MenuItem::separator(),
        MenuItem::action(
            tr!("menu.imagery.adjust_offset"),
            crate::imagery_offset_ui::AdjustImageryOffset,
        ),
        MenuItem::action(
            tr!("menu.imagery.reset_offset"),
            crate::imagery_offset_ui::ResetImageryOffset,
        ),
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

    // Off macOS there is no app-name menu (see `build_menus`): Quit goes at
    // the end of File, Settings at the end of Edit, About at the end of Help.
    let mut file_items = vec![
        MenuItem::action(tr!("menu.file.open"), OpenOsmFile),
        MenuItem::action(tr!("menu.file.export"), ExportOsmFile),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.file.download"), DownloadFromOsm),
        MenuItem::action(tr!("gps_traces.menu.download"), DownloadGpsTraces),
        MenuItem::action(tr!("menu.file.upload"), UploadToOsm).disabled(!can_upload),
    ];
    let mut edit_tail = vec![];
    let mut help_items = vec![
        MenuItem::action(tr!("menu.help.map_features_wiki"), OpenMapFeaturesWiki),
        MenuItem::action(
            tr!("menu.help.project_page", app = APP_NAME),
            OpenProjectPage,
        ),
    ];
    // Minimize and Zoom work on every platform (GPUI implements both for
    // macOS, Windows and Linux/X11/Wayland), so the Window menu stays.
    let window_items = vec![
        MenuItem::action(tr!("menu.window.minimize"), MinimizeWindow),
        MenuItem::action(tr!("menu.window.zoom"), ZoomWindow),
    ];

    let mut menus = vec![];
    if is_macos {
        menus.push(Menu {
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
        });
    } else {
        file_items.push(MenuItem::separator());
        file_items.push(MenuItem::action(tr!("menu.app.quit", app = APP_NAME), Quit));
        edit_tail.push(MenuItem::separator());
        edit_tail.push(MenuItem::action(tr!("menu.app.settings"), OpenSettings));
        help_items.push(MenuItem::separator());
        help_items.push(MenuItem::action(
            tr!("menu.app.about", app = APP_NAME),
            ShowAbout,
        ));
    }

    menus.extend([
        Menu {
            name: tr!("menu.file.title"),
            items: file_items,
            disabled: false,
        },
        Menu {
            name: tr!("menu.edit.title"),
            items: std::iter::empty()
                .chain(vec![
                    MenuItem::action(tr!("menu.edit.undo"), Undo),
                    MenuItem::action(tr!("menu.edit.redo"), Redo),
                    MenuItem::separator(),
                ])
                .chain(edit_action_items(
                    selection_actions::MenuSlot::EditClipboard,
                ))
                .chain([MenuItem::separator()])
                .chain(edit_action_items(selection_actions::MenuSlot::EditTags))
                .chain(vec![
                    MenuItem::separator(),
                    MenuItem::action(tr!("menu.edit.delete"), DeleteSelection),
                    MenuItem::separator(),
                    MenuItem::action(tr!("menu.edit.select_all"), SelectAll),
                    MenuItem::action(tr!("menu.edit.deselect_all"), DeselectAll),
                    MenuItem::action(tr!("menu.edit.find"), Find),
                    MenuItem::action(tr!("menu.edit.show_history"), ShowObjectHistory),
                    MenuItem::separator(),
                    MenuItem::action(tr!("menu.edit.change_feature_type"), ChangeFeatureType),
                    MenuItem::action(tr!("menu.edit.apply_nsi_preset"), ApplyNsiPreset),
                ])
                .chain(edit_tail)
                .collect(),
            disabled: false,
        },
        Menu {
            name: tr!("menu.tools.title"),
            // Every selection action, applicable or not: running one that
            // doesn't fit the selection explains why in the status bar.
            items: std::iter::once(MenuItem::action(tr!("validator.menu"), Validate))
                .chain(std::iter::once(MenuItem::separator()))
                .chain(edit_action_items(selection_actions::MenuSlot::Tools))
                .chain([
                    MenuItem::separator(),
                    MenuItem::action(tr!("notes.menu.new"), NewNote),
                ])
                .collect(),
            disabled: false,
        },
        Menu {
            name: tr!("menu.mode.title"),
            items: EditMode::ALL
                .into_iter()
                .map(|mode| MenuItem::action(mode.name(), SetMode { mode }))
                .collect(),
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
                MenuItem::action(tr!("notes.menu.show"), ToggleNotes)
                    .checked(osm_gpui::notes::menu_shows_notes()),
                MenuItem::action(tr!("notes.menu.refresh"), RefreshNotes),
                MenuItem::separator(),
                MenuItem::action(tr!("street_imagery.menu.mapillary"), ToggleMapillary).checked(
                    osm_gpui::street_imagery::menu_shows(
                        osm_gpui::street_imagery::Provider::Mapillary,
                    ),
                ),
                MenuItem::action(tr!("street_imagery.menu.fotoshi"), ToggleFotoshi).checked(
                    osm_gpui::street_imagery::menu_shows(
                        osm_gpui::street_imagery::Provider::Fotoshi,
                    ),
                ),
                MenuItem::action(tr!("street_imagery.menu.panoramax"), TogglePanoramax).checked(
                    osm_gpui::street_imagery::menu_shows(
                        osm_gpui::street_imagery::Provider::Panoramax,
                    ),
                ),
                MenuItem::action(tr!("street_imagery.menu.refresh"), RefreshStreetImagery),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.view.toggle_debug_overlay"), ToggleDebugOverlay),
            ],
            disabled: false,
        },
        Menu {
            name: tr!("menu.window.title"),
            items: window_items,
            disabled: false,
        },
        Menu {
            name: tr!("menu.help.title"),
            items: help_items,
            disabled: false,
        },
    ]);
    menus
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
    cx.on_action(crate::welcome::dismiss_welcome_action);
    cx.on_action(script_seed_custom_imagery);
    cx.on_action(|_: &SetThemeLight, cx: &mut App| {
        osm_gpui::appearance::apply_run_override(ThemeMode::Light, cx)
    });
    cx.on_action(|_: &SetThemeDark, cx: &mut App| {
        osm_gpui::appearance::apply_run_override(ThemeMode::Dark, cx)
    });
    cx.on_action(|action: &SetAppearance, cx: &mut App| {
        // A scripted run is read-only (`persist::Policy`): this updates the
        // in-memory setting and the file write is skipped.
        osm_gpui::appearance::set_preference(action.mode, true, cx);
    });
    cx.on_action(|action: &SetInvertScrollZoom, _cx: &mut App| {
        osm_gpui::settings_store::set_invert_scroll_zoom(action.on, true);
    });
}

// Run-only theme switches with no menu item: reachable from the script
// harness (`action SetThemeDark`) and mirrored by the `--theme` flag. They
// override the saved Appearance setting for this run and are never saved.
actions!(osm_gpui, [SetThemeLight, SetThemeDark]);

/// Script/harness action (`action SetAppearance {"mode":"Dark"}`; `mode` is
/// `System`, `Light` or `Dark`), shown in no menu. The user-facing control is
/// Settings > Appearance.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetAppearance {
    pub mode: osm_gpui::settings_store::ThemePreference,
}

/// Script/harness action (`action SetInvertScrollZoom {"on":true}`), shown in
/// no menu. The user-facing control is Settings > Appearance. In script mode
/// it changes the in-memory setting only, never the settings file.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetInvertScrollZoom {
    pub on: bool,
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

    fn menus_for(is_macos: bool) -> Vec<gpui::OwnedMenu> {
        build_menus(is_macos, 40.0, -74.0, ImageryLoadState::Loading, true, true)
            .into_iter()
            .map(Menu::owned)
            .collect()
    }

    /// Every action and system-menu entry in the menus, submenus included.
    fn walk(items: &[gpui::OwnedMenuItem], out: &mut Vec<Box<dyn gpui::Action>>, sys: &mut usize) {
        for item in items {
            match item {
                gpui::OwnedMenuItem::Action { action, .. } => out.push(action.boxed_clone()),
                gpui::OwnedMenuItem::Submenu(m) => walk(&m.items, out, sys),
                gpui::OwnedMenuItem::SystemMenu(_) => *sys += 1,
                gpui::OwnedMenuItem::Separator => {}
            }
        }
    }

    fn all_actions(menus: &[gpui::OwnedMenu]) -> (Vec<Box<dyn gpui::Action>>, usize) {
        let (mut out, mut sys) = (vec![], 0);
        for m in menus {
            walk(&m.items, &mut out, &mut sys);
        }
        (out, sys)
    }

    fn has<T: gpui::Action>(actions: &[Box<dyn gpui::Action>]) -> bool {
        actions.iter().any(|a| a.as_any().is::<T>())
    }

    fn act(name: &str, action: impl gpui::Action) -> gpui::OwnedMenuItem {
        MenuItem::action(name.to_string(), action).owned()
    }

    fn sub(name: &str, items: Vec<gpui::OwnedMenuItem>) -> gpui::OwnedMenuItem {
        gpui::OwnedMenuItem::Submenu(gpui::OwnedMenu {
            name: name.to_string().into(),
            items,
            disabled: false,
        })
    }

    /// Item names in order, `-` for a separator, `?` for anything else.
    fn shape(items: &[gpui::OwnedMenuItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                gpui::OwnedMenuItem::Action { name, .. } => name.clone(),
                gpui::OwnedMenuItem::Separator => "-".into(),
                _ => "?".into(),
            })
            .collect()
    }

    #[test]
    fn flattening_puts_submenu_items_inline_under_a_disabled_header() {
        let items = vec![
            act("Before", Undo),
            sub(
                "Custom",
                vec![act("Add", AddCustomImagery), act("One", Redo)],
            ),
            gpui::OwnedMenuItem::Separator,
            act("After", Quit),
        ];
        let flat = flatten_owned_items(items);
        assert_eq!(
            shape(&flat),
            ["Before", "Custom", "Add", "One", "-", "After"]
        );
        match &flat[1] {
            gpui::OwnedMenuItem::Action { disabled, .. } => assert!(*disabled),
            _ => panic!("header is an action line"),
        }
        let (actions, _) = {
            let (mut out, mut sys) = (vec![], 0);
            walk(&flat, &mut out, &mut sys);
            (out, sys)
        };
        assert!(has::<AddCustomImagery>(&actions));
        assert!(has::<Redo>(&actions));
        assert!(has::<Quit>(&actions));
    }

    #[test]
    fn flattening_handles_nested_submenus_and_omits_empty_ones() {
        let items = vec![
            sub("Empty", vec![]),
            sub(
                "Outer",
                vec![
                    sub("Inner", vec![act("Leaf", Undo)]),
                    sub("Nothing", vec![]),
                ],
            ),
        ];
        let flat = flatten_owned_items(items);
        assert_eq!(shape(&flat), ["Outer", "Inner", "Leaf"]);
    }

    /// Menus as the in-window bar gets them, with a large fake imagery list
    /// loaded, for either platform's contents.
    fn in_window_menus_with_big_imagery_list(is_macos: bool) -> Vec<gpui::OwnedMenu> {
        let index = crate::IMAGERY_INDEX.get_or_init(Default::default);
        *index.lock().unwrap() = (0..500)
            .map(|i| osm_gpui::imagery::ImageryEntry {
                id: format!("fake-{i}"),
                name: format!("Fake imagery {i:03}"),
                url_template: "https://example.invalid/{z}/{x}/{y}.png".into(),
                min_zoom: None,
                max_zoom: None,
                bbox: None,
                polygon: None,
                best: i < 3,
                country_code: None,
                icon_url: None,
                attribution: None,
            })
            .collect();
        build_menus(is_macos, 40.0, -74.0, ImageryLoadState::Ready, true, true)
            .into_iter()
            .map(|m| fit_owned_menu(m.owned()))
            .collect()
    }

    #[test]
    fn settings_opens_one_window_and_focuses_it_afterwards() {
        assert_eq!(
            settings_open_decision(false, None),
            SettingsOpen::New { page: None }
        );
        assert_eq!(
            settings_open_decision(false, Some(SettingsPage::Imagery)),
            SettingsOpen::New {
                page: Some(SettingsPage::Imagery)
            }
        );
        assert_eq!(settings_open_decision(true, None), SettingsOpen::Focus);
        assert_eq!(
            settings_open_decision(true, Some(SettingsPage::Imagery)),
            SettingsOpen::Focus
        );
    }

    /// No menu is over the limit unless it is a leaf without submenus.
    fn check_menu_fits(name: &str, items: &[gpui::OwnedMenuItem]) {
        let has_submenu = items
            .iter()
            .any(|i| matches!(i, gpui::OwnedMenuItem::Submenu(_)));
        if items.len() > IN_WINDOW_MENU_MAX_ITEMS {
            assert!(!has_submenu, "{name}: a scrolling menu holds a submenu");
        }
        for item in items {
            if let gpui::OwnedMenuItem::Submenu(sub) = item {
                check_menu_fits(sub.name.as_str(), &sub.items);
            }
        }
    }

    #[test]
    fn in_window_menus_with_submenus_stay_within_the_limit_on_both_layouts() {
        for is_macos in [true, false] {
            let menus = in_window_menus_with_big_imagery_list(is_macos);
            for menu in &menus {
                let has_submenu = menu
                    .items
                    .iter()
                    .any(|i| matches!(i, gpui::OwnedMenuItem::Submenu(_)));
                if has_submenu {
                    assert!(
                        menu.items.len() <= IN_WINDOW_MENU_MAX_ITEMS,
                        "{} has {} entries and submenus",
                        menu.name,
                        menu.items.len()
                    );
                }
                check_menu_fits(menu.name.as_str(), &menu.items);
            }
            // The Imagery menu keeps Custom Imagery and gains the long list
            // as submenus, with no imagery lost.
            let imagery = menus
                .iter()
                .find(|m| m.name.as_str() == tr!("menu.imagery.title").as_str())
                .expect("Imagery menu");
            let subs: Vec<_> = imagery
                .items
                .iter()
                .filter_map(|i| match i {
                    gpui::OwnedMenuItem::Submenu(s) => Some(s),
                    _ => None,
                })
                .collect();
            assert_eq!(subs.len(), 2, "Custom Imagery and Available Imagery");
            assert_eq!(subs[0].name.as_str(), tr!("menu.imagery.custom").as_str());
            assert_eq!(
                subs[1].name.as_str(),
                tr!("menu.imagery.available").as_str()
            );
            assert!(subs[1].items.len() > IN_WINDOW_MENU_MAX_ITEMS);
            let (actions, _) = all_actions(&menus);
            assert!(has::<AddCustomImagery>(&actions));
            assert!(has::<AddImageryLayer>(&actions));
            assert!(has::<AddOsmCarto>(&actions));
        }
    }

    #[test]
    fn fitting_moves_the_longest_run_into_a_labelled_submenu_and_keeps_order() {
        let mut items = vec![act("A", Undo), gpui::OwnedMenuItem::Separator];
        items.extend((0..30).map(|i| act(&format!("L{i}"), Redo)));
        items.push(gpui::OwnedMenuItem::Separator);
        items.push(act("Z", Quit));
        let fit = fit_owned_items(items, 20, "Lots");
        assert_eq!(shape(&fit), ["A", "-", "?", "-", "Z"]);
        match &fit[2] {
            gpui::OwnedMenuItem::Submenu(s) => {
                assert_eq!(s.name.as_str(), "Lots");
                assert_eq!(s.items.len(), 30);
                assert_eq!(shape(&s.items)[0], "L0");
                assert_eq!(shape(&s.items)[29], "L29");
            }
            _ => panic!("the run became a submenu"),
        }
    }

    #[test]
    fn fitting_leaves_short_menus_alone_and_flattens_what_cannot_be_grouped() {
        let short = vec![act("A", Undo), sub("S", vec![act("B", Redo)])];
        assert_eq!(shape(&fit_owned_items(short, 20, "More")), ["A", "?"]);
        // Alternating action/separator: no run to group, so submenus go.
        let mut odd = vec![sub("S", vec![act("In", Redo)])];
        for i in 0..12 {
            odd.push(act(&format!("A{i}"), Undo));
            odd.push(gpui::OwnedMenuItem::Separator);
        }
        let fit = fit_owned_items(odd, 20, "More");
        assert!(fit
            .iter()
            .all(|i| !matches!(i, gpui::OwnedMenuItem::Submenu(_))));
        assert!(shape(&fit).contains(&"In".to_string()));
    }

    #[test]
    fn a_long_submenu_never_holds_a_submenu() {
        let mut inner: Vec<_> = (0..25).map(|i| act(&format!("I{i}"), Redo)).collect();
        inner.push(sub("Deep", vec![act("Leaf", Undo)]));
        let fit = fit_owned_items(vec![sub("Outer", inner)], 20, "More");
        match &fit[0] {
            gpui::OwnedMenuItem::Submenu(s) => {
                check_menu_fits("Outer", &s.items);
                let (mut out, mut sys) = (vec![], 0);
                walk(&s.items, &mut out, &mut sys);
                assert!(has::<Undo>(&out), "the leaf action is kept");
                assert_eq!(out.len(), 26);
            }
            _ => panic!("still a submenu"),
        }
    }

    #[test]
    fn macos_menus_keep_the_app_menu_with_the_macos_only_items() {
        let menus = menus_for(true);
        assert_eq!(menus[0].name.as_str(), APP_NAME);
        let (actions, system_menus) = all_actions(&menus);
        assert!(has::<HideApp>(&actions));
        assert!(has::<HideOtherApps>(&actions));
        assert!(has::<ShowAllApps>(&actions));
        assert_eq!(system_menus, 1, "the Services submenu");
        assert!(has::<MinimizeWindow>(&actions));
        assert!(has::<ZoomWindow>(&actions));
    }

    #[test]
    fn other_platform_menus_drop_the_macos_only_items_and_keep_every_function() {
        let menus = menus_for(false);
        let (actions, system_menus) = all_actions(&menus);
        assert!(!has::<HideApp>(&actions));
        assert!(!has::<HideOtherApps>(&actions));
        assert!(!has::<ShowAllApps>(&actions));
        assert_eq!(system_menus, 0);
        for menu in &menus {
            assert!(
                menu.items
                    .iter()
                    .any(|i| !matches!(i, gpui::OwnedMenuItem::Separator)),
                "empty menu {:?}",
                menu.name
            );
        }
        assert!(
            menus.iter().all(|m| m.name.as_str() != APP_NAME),
            "no app-name menu off macOS"
        );
        assert!(has::<MinimizeWindow>(&actions));
        assert!(has::<ZoomWindow>(&actions));
        assert!(has::<Quit>(&actions));
        assert!(has::<OpenSettings>(&actions));
        assert!(has::<ShowAbout>(&actions));
        assert!(has::<OpenOsmFile>(&actions));
        assert!(has::<ExportOsmFile>(&actions));
        assert!(has::<DownloadFromOsm>(&actions));
        assert!(has::<UploadToOsm>(&actions));
        assert!(has::<Validate>(&actions));
        assert!(has::<OpenMapFeaturesWiki>(&actions));
        assert!(has::<OpenProjectPage>(&actions));
        assert!(has::<Undo>(&actions));
        assert!(has::<ToggleDebugOverlay>(&actions));
    }

    #[test]
    fn every_macos_action_not_macos_only_is_in_the_other_menus_too() {
        let (mac, _) = all_actions(&menus_for(true));
        let (other, _) = all_actions(&menus_for(false));
        for a in &mac {
            let macos_only = a.as_any().is::<HideApp>()
                || a.as_any().is::<HideOtherApps>()
                || a.as_any().is::<ShowAllApps>();
            if !macos_only {
                assert!(
                    other.iter().any(|o| o.partial_eq(a.as_ref())),
                    "missing off macOS: {}",
                    a.name()
                );
            }
        }
    }

    #[test]
    fn in_window_bar_layout_follows_the_platform_unless_overridden_to_other() {
        assert!(in_window_menu_wanted(true, Some("other")));
        assert!(in_window_menu_uses_macos_layout(true, None));
        assert!(in_window_menu_uses_macos_layout(true, Some("1")));
        assert!(!in_window_menu_uses_macos_layout(true, Some("other")));
        assert!(!in_window_menu_uses_macos_layout(false, None));
        assert!(!in_window_menu_uses_macos_layout(false, Some("1")));
    }

    #[test]
    fn menus_convert_to_owned_with_every_top_level_menu_and_the_file_items() {
        let menus = menus_for(true);
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
        // Open, Export, Download, Download GPS Traces, Upload.
        assert_eq!(actions, 5);
    }
}
