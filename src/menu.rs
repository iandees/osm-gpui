//! Menu bar construction and the free-function menu action handlers.

use gpui::{actions, point, px, size, App, AppContext as _, Bounds, Menu, MenuItem, SystemMenuType, WindowOptions};

use osm_gpui::custom_imagery_store::{self, CustomImageryEntry};
use osm_gpui::imagery;
use osm_gpui::osm::OsmParser;

use crate::{
    AddCoordinateGrid, AddCustomImagery, AddImageryLayer, AddOsmCarto, AddSavedCustomImagery,
    DownloadFromOsm, ImageryLoadState, LayerRequest, OpenOsmFile, OpenSettings, Quit, Redo,
    ToggleDebugOverlay, Undo, DOWNLOAD_REQUESTS, IMAGERY_INDEX, LAYER_REQUESTS,
    OPEN_CUSTOM_IMAGERY_DIALOG, SHARED_OSM_DATA, TOGGLE_DEBUG_OVERLAY,
};

/// Guard to prevent opening multiple settings windows simultaneously.
static SETTINGS_WINDOW_OPEN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) fn custom_imagery_snapshot() -> Vec<CustomImageryEntry> {
    custom_imagery_store::snapshot()
}

// Handle the File > Open OSM File menu action
pub(crate) fn open_osm_file(_: &OpenOsmFile, cx: &mut App) {
    let executor = cx.background_executor().clone();
    let shared_queue = SHARED_OSM_DATA.get().unwrap().clone();

    // Spawn async file dialog
    executor
        .spawn(async move {
            if let Some(file_path) = rfd::AsyncFileDialog::new()
                .add_filter("OSM files", &["osm", "xml"])
                .add_filter("All files", &["*"])
                .set_title("Select OSM file to open")
                .pick_file()
                .await
            {
                let path = file_path.path().to_path_buf();
                let path_str = path.to_string_lossy().to_string();

                // Parse OSM file in background
                let parser = OsmParser::new();
                match parser.parse_file(&path_str) {
                    Ok(osm_data) => {
                        if let Ok(mut q) = shared_queue.lock() {
                            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("OSM").to_string();
                            q.push((stem, osm_data));
                        }
                    }
                    Err(e) => eprintln!("Failed to parse OSM file: {}", e),
                }
            }
        })
        .detach();
}

// Define the quit function that is registered with the App
pub(crate) fn quit(_: &Quit, cx: &mut App) {
    cx.quit();
}

pub(crate) fn open_settings(_: &OpenSettings, cx: &mut App) {
    if SETTINGS_WINDOW_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    SETTINGS_WINDOW_OPEN.store(true, std::sync::atomic::Ordering::Relaxed);

    let settings_window = cx.open_window(
        WindowOptions {
            window_bounds: Some(gpui::WindowBounds::Windowed(Bounds {
                origin: point(px(200.0), px(200.0)),
                size: size(px(600.0), px(500.0)),
            })),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Settings".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            focus: true,
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| osm_gpui::ui::settings_window::SettingsWindow::new(window, cx));
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
    if let Some(requests) = DOWNLOAD_REQUESTS.get() {
        if let Ok(mut q) = requests.lock() {
            q.push(());
        }
    }
    // Wake the render loop so MapViewer drains the queue on the next frame
    // instead of waiting for an unrelated input event.
    cx.refresh_windows();
}

// Handle the Imagery > OpenStreetMap Carto menu action
pub(crate) fn add_osm_carto(_: &AddOsmCarto, cx: &mut App) {
    if let Some(requests) = LAYER_REQUESTS.get() {
        if let Ok(mut queue) = requests.lock() {
            queue.push(LayerRequest::OsmCarto);
        }
    }
    cx.refresh_windows();
}

// Handle an ELI imagery menu action. Looks up the entry in the loaded index
// and enqueues a layer request.
pub(crate) fn add_imagery_layer(action: &AddImageryLayer, _cx: &mut App) {
    let id = action.id.to_string();
    let Some(index) = IMAGERY_INDEX.get() else { return };
    let entry = {
        let guard = match index.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        guard.iter().find(|e| e.id == id).cloned()
    };
    let Some(entry) = entry else { return };
    if let Some(requests) = LAYER_REQUESTS.get() {
        if let Ok(mut queue) = requests.lock() {
            queue.push(LayerRequest::Imagery {
                name: entry.name,
                url_template: entry.url_template,
                min_zoom: entry.min_zoom,
                max_zoom: entry.max_zoom,
            });
        }
    }
}

// Handle the View > Toggle Debug Overlay menu action
pub(crate) fn toggle_debug_overlay(_: &ToggleDebugOverlay, cx: &mut App) {
    if let Some(requests) = TOGGLE_DEBUG_OVERLAY.get() {
        if let Ok(mut queue) = requests.lock() {
            queue.push(());
        }
    }
    cx.refresh_windows();
}

// Handle the Imagery > Add Custom Imagery… menu action
pub(crate) fn open_custom_imagery_dialog(_: &AddCustomImagery, cx: &mut App) {
    if let Some(queue) = OPEN_CUSTOM_IMAGERY_DIALOG.get() {
        if let Ok(mut g) = queue.lock() {
            g.push(());
        }
    }
    cx.refresh_windows();
}

// Handle the Imagery > Custom Imagery > <saved entry> menu action
pub(crate) fn add_saved_custom_imagery(action: &AddSavedCustomImagery, cx: &mut App) {
    let entries = custom_imagery_snapshot();
    let Some(entry) = entries.get(action.index).cloned() else {
        eprintln!("add_saved_custom_imagery: stale index {}", action.index);
        return;
    };
    if let Some(requests) = LAYER_REQUESTS.get() {
        if let Ok(mut q) = requests.lock() {
            q.push(LayerRequest::Imagery {
                name: entry.name,
                url_template: entry.url_template,
                min_zoom: Some(entry.min_zoom),
                max_zoom: Some(entry.max_zoom),
            });
        }
    }
    cx.refresh_windows();
}

// Handle the Imagery > Coordinate Grid menu action
pub(crate) fn add_coordinate_grid(_: &AddCoordinateGrid, cx: &mut App) {
    if let Some(requests) = LAYER_REQUESTS.get() {
        if let Ok(mut queue) = requests.lock() {
            queue.push(LayerRequest::CoordinateGrid);
        }
    }
    cx.refresh_windows();
}

/// Build and install the menu bar, using the current viewport center to filter
/// the Imagery menu to relevant ELI entries.
pub(crate) fn rebuild_menus(cx: &mut App, center_lat: f64, center_lon: f64, state: ImageryLoadState) {
    let custom = custom_imagery_snapshot();
    let mut custom_items: Vec<MenuItem> = vec![
        MenuItem::action("Add…", AddCustomImagery),
    ];
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
            name: "Custom Imagery".into(),
            items: custom_items,
            disabled: false,
        }),
        MenuItem::separator(),
        MenuItem::action("OpenStreetMap Carto", AddOsmCarto),
        MenuItem::separator(),
        MenuItem::action("Coordinate Grid", AddCoordinateGrid),
    ];

    match state {
        ImageryLoadState::Loading => {
            imagery_items.push(MenuItem::separator());
            imagery_items.push(MenuItem::action(
                "(Loading imagery index…)",
                NoOpImageryInfo,
            ));
        }
        ImageryLoadState::Failed => {
            imagery_items.push(MenuItem::separator());
            imagery_items.push(MenuItem::action(
                "(Imagery index unavailable)",
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
                        format!("★ {}", entry.name)
                    } else {
                        entry.name.clone()
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

    cx.set_menus(vec![
        Menu {
            name: "OSM Viewer".into(),
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open…", OpenOsmFile),
                MenuItem::action("Download from OSM", DownloadFromOsm),
            ],
            disabled: false,
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Undo", Undo),
                MenuItem::action("Redo", Redo),
            ],
            disabled: false,
        },
        Menu {
            name: "Imagery".into(),
            items: imagery_items,
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Toggle Debug Overlay", ToggleDebugOverlay),
            ],
            disabled: false,
        },
    ]);
}

// Dummy action used for disabled-style "info" entries in the Imagery menu.
// (GPUI does not support disabled menu items directly, so we use a no-op.)
actions!(osm_gpui, [NoOpImageryInfo]);

pub(crate) fn no_op_imagery_info(_: &NoOpImageryInfo, _cx: &mut App) {}
