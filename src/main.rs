use gpui::{actions, canvas, div, point, prelude::*, px, rgb, size, App, Application, Bounds, Context, KeyBinding, Keystroke, Menu, MenuItem, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, ScrollDelta, ScrollWheelEvent, SharedString, SystemMenuType, Window, WindowOptions};
use serde::Deserialize;
use schemars::JsonSchema;
use gpui::Action;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use osm_gpui::coordinates::lat_lon_to_mercator;
use osm_gpui::idle_tracker::IdleTracker;
use osm_gpui::imagery::{self, ImageryEntry};
use osm_gpui::tile_cache::TileCache;
use osm_gpui::osm::{OsmData, OsmParser};
use osm_gpui::viewport::Viewport;
use osm_gpui::layers::{LayerManager, tile_layer::TileLayer, osm_layer::OsmLayer, grid_layer::GridLayer};
use osm_gpui::tiles;
use osm_gpui::osm_api;
use osm_gpui::script::{self, runner::{AppHandle, Runner}};
use osm_gpui::capture;

actions!(osm_gpui, [OpenOsmFile, Quit, AddOsmCarto, DownloadFromOsm]);

/// Action for adding an imagery layer from the ELI by id.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
struct AddImageryLayer {
    id: SharedString,
}

/// Request to add a new layer from a menu action.
#[derive(Debug, Clone)]
enum LayerRequest {
    OsmCarto,
    Imagery { name: String, url_template: String },
}

/// Stores the full ELI list once loaded (populated on the background executor).
static IMAGERY_INDEX: OnceLock<Arc<Mutex<Vec<ImageryEntry>>>> = OnceLock::new();

/// Set to true when the imagery index is loaded (or failed) so the render loop
/// knows to refresh the menu.
static IMAGERY_LOAD_STATE: OnceLock<Arc<Mutex<ImageryLoadState>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq)]
enum ImageryLoadState {
    Loading,
    Ready,
    Failed,
}

// Replace single optional data store with a queue of datasets awaiting layer creation
static SHARED_OSM_DATA: std::sync::OnceLock<Arc<Mutex<Vec<(String, OsmData)>>>> =
    std::sync::OnceLock::new();

// Queue for layer addition requests
static LAYER_REQUESTS: std::sync::OnceLock<Arc<Mutex<Vec<LayerRequest>>>> =
    std::sync::OnceLock::new();

static DOWNLOAD_REQUESTS: std::sync::OnceLock<Arc<Mutex<Vec<()>>>> =
    std::sync::OnceLock::new();

// Global idle tracker shared with the script runner
static GLOBAL_IDLE: std::sync::OnceLock<Arc<IdleTracker>> = std::sync::OnceLock::new();

// Set to true while a script runner thread is active
static SCRIPT_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Script command channel (background thread → gpui main thread)
// ---------------------------------------------------------------------------
//
// The script runner runs on a background thread (so `std::thread::sleep` in
// `wait_frame` does not block the gpui event loop). It cannot hold `AsyncApp`
// because that type uses `Rc`-internals and is not `Send`.
//
// Instead the runner enqueues `ScriptCommand` values into a mutex-protected
// queue and waits for the main thread to execute them (signalled via a condvar).
//
// MapViewer's render fn drains this queue each frame and processes the commands
// directly, then signals completion. A second condvar signals "a frame was
// rendered" so `wait_frame` can wake up.

#[derive(Debug)]
enum ScriptCommand {
    /// pan_to + set_zoom + ensure tile layer
    SetViewport { lat: f64, lon: f64, zoom: f64 },
    /// Resize the window
    SetWindowSize { w: u32, h: u32 },
    /// Synthesize a left-button drag (from → to with sleep between steps)
    Drag { from: (f32, f32), to: (f32, f32) },
    /// Synthesize a mouse click
    Click { x: f32, y: f32, right: bool },
    /// Synthesize a scroll event
    Scroll { x: f32, y: f32, dx: f32, dy: f32 },
}

/// Shared state between the script-runner thread and the gpui main thread.
struct ScriptBus {
    /// Pending command for this frame. None when idle.
    pending: Mutex<Option<ScriptCommand>>,
    /// Signalled by the main thread when it has processed a pending command.
    done_cv: Condvar,
    /// Counts how many frames have been rendered (monotonically increasing).
    frame_count: Mutex<u64>,
    /// Signalled each time a frame is rendered.
    frame_cv: Condvar,
}

impl ScriptBus {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(None),
            done_cv: Condvar::new(),
            frame_count: Mutex::new(0),
            frame_cv: Condvar::new(),
        })
    }

    /// Submit a command and block until the main thread has processed it.
    fn submit(&self, cmd: ScriptCommand) {
        {
            let mut lock = self.pending.lock().unwrap();
            *lock = Some(cmd);
        }
        // Wait until the command is consumed.
        let _guard = self.done_cv.wait_while(
            self.pending.lock().unwrap(),
            |opt| opt.is_some(),
        ).unwrap();
    }

    /// Wait until at least one more render frame has completed.
    fn wait_frame(&self) {
        let current = *self.frame_count.lock().unwrap();
        let _guard = self.frame_cv.wait_while(
            self.frame_count.lock().unwrap(),
            |fc| *fc <= current,
        ).unwrap();
    }

    /// Called by MapViewer::render to drain and process the pending command.
    /// Returns the command if any was pending (caller processes it).
    fn take_pending(&self) -> Option<ScriptCommand> {
        self.pending.lock().unwrap().take()
    }

    /// Called by MapViewer::render after processing a command (or if no command).
    fn signal_done_and_frame(&self) {
        self.done_cv.notify_all();
        let mut fc = self.frame_count.lock().unwrap();
        *fc += 1;
        self.frame_cv.notify_all();
    }
}

static SCRIPT_BUS: std::sync::OnceLock<Arc<ScriptBus>> = std::sync::OnceLock::new();

// Keystroke commands need a separate queue since gpui `Keystroke` is not Send-safe
// (it only contains Strings, Modifiers — actually it IS Send). Let's use a simple
// OnceLock queue for keystrokes.
static KEYSTROKE_QUEUE: std::sync::OnceLock<Arc<Mutex<Vec<Keystroke>>>> =
    std::sync::OnceLock::new();

#[derive(Default)]
struct CliArgs {
    script: Option<PathBuf>,
    window_size: Option<(u32, u32)>,
    keep_open: bool,
}

fn parse_cli_args() -> CliArgs {
    let mut out = CliArgs::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--script" => {
                out.script = Some(PathBuf::from(
                    args.next().expect("--script needs a path"),
                ))
            }
            "--window-size" => {
                let v = args.next().expect("--window-size needs WxH");
                let (w, h) = v.split_once('x').expect("--window-size format WxH");
                out.window_size = Some((w.parse().expect("W"), h.parse().expect("H")));
            }
            "--keep-open" => out.keep_open = true,
            other => eprintln!("ignoring unknown arg: {}", other),
        }
    }
    out
}

struct MapViewer {
    viewport: Viewport,
    layer_manager: LayerManager,
    tile_cache: Arc<Mutex<TileCache>>,
    first_dataset_fitted: bool,
    status_message: Option<(String, Instant)>,
    selected: Option<osm_gpui::selection::FeatureRef>,
    mouse_down_pos: Option<gpui::Point<gpui::Pixels>>,
    frame_times: VecDeque<Instant>,
    /// Last (lat, lon) the Imagery menu was rebuilt for. None forces a rebuild.
    last_menu_center: Option<(f64, f64)>,
    /// Imagery load state observed on the previous frame; detect transitions.
    last_imagery_load_state: Option<ImageryLoadState>,
}

impl MapViewer {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let viewport = Viewport::new(40.7128, -74.0060, 11.0, gpui::size(px(800.0), px(600.0)));
        let executor = cx.background_executor().clone();
        // Use the global idle tracker (set before Application::new().run(...))
        let idle = GLOBAL_IDLE.get().cloned().unwrap_or_else(IdleTracker::new);
        let tile_cache = Arc::new(Mutex::new(TileCache::new(executor, idle)));
        let mut layer_manager = LayerManager::new();
        // Removed default tile layer - will be added via menu
        layer_manager.add_layer(Box::new(GridLayer::new()));

        // No default OSM layer; loaded files add their own
        Self {
            viewport,
            layer_manager,
            tile_cache,
            first_dataset_fitted: false,
            status_message: None,
            selected: None,
            mouse_down_pos: None,
            frame_times: VecDeque::with_capacity(120),
            last_menu_center: None,
            last_imagery_load_state: None,
        }
    }

    /// Rebuild the Imagery menu if needed (center moved or load state changed).
    fn maybe_rebuild_imagery_menu(&mut self, cx: &mut Context<Self>) {
        let (lat, lon) = self.viewport.center();

        // Pull current load state.
        let current_state = IMAGERY_LOAD_STATE
            .get()
            .and_then(|s| s.lock().ok().map(|g| *g))
            .unwrap_or(ImageryLoadState::Loading);

        let state_changed = self.last_imagery_load_state != Some(current_state);
        let center_moved = match self.last_menu_center {
            None => true,
            Some((plat, plon)) => (plat - lat).abs() > 0.5 || (plon - lon).abs() > 0.5,
        };
        if !state_changed && !center_moved {
            return;
        }
        // Only refresh when the imagery index has reached a terminal state
        // (Ready or Failed). In Loading we don't have entries yet.
        rebuild_menus(&mut *cx, lat, lon, current_state);
        self.last_menu_center = Some((lat, lon));
        self.last_imagery_load_state = Some(current_state);
    }

    /// Record the current frame timestamp and return smoothed FPS over the
    /// retained sample window (last ~1s of frames).
    fn tick_fps(&mut self) -> f32 {
        let now = Instant::now();
        self.frame_times.push_back(now);
        while let Some(&front) = self.frame_times.front() {
            if now.duration_since(front) > Duration::from_secs(1) {
                self.frame_times.pop_front();
            } else {
                break;
            }
        }
        while self.frame_times.len() > 120 {
            self.frame_times.pop_front();
        }
        if self.frame_times.len() < 2 {
            return 0.0;
        }
        let span = now
            .duration_since(*self.frame_times.front().unwrap())
            .as_secs_f32();
        if span <= 0.0 {
            0.0
        } else {
            (self.frame_times.len() - 1) as f32 / span
        }
    }

    /// Fit view to show OSM data
    fn fit_to_osm_data(&mut self, osm_data: &OsmData) {
        if osm_data.nodes.is_empty() {
            return;
        }

        let mut min_lat = f64::INFINITY;
        let mut max_lat = f64::NEG_INFINITY;
        let mut min_lon = f64::INFINITY;
        let mut max_lon = f64::NEG_INFINITY;

        for node in osm_data.nodes.values() {
            min_lat = min_lat.min(node.lat);
            max_lat = max_lat.max(node.lat);
            min_lon = min_lon.min(node.lon);
            max_lon = max_lon.max(node.lon);
        }

        if min_lat != f64::INFINITY {
            let mut center_lat = (min_lat + max_lat) / 2.0;
            let mut center_lon = (min_lon + max_lon) / 2.0;

            // If bounding box height is zero, set to a small value
            if (max_lat - min_lat).abs() < 1e-6 {
                center_lat = min_lat;
                min_lat -= 0.005;
                max_lat += 0.005;
            }
            if (max_lon - min_lon).abs() < 1e-6 {
                center_lon = min_lon;
                min_lon -= 0.005;
                max_lon += 0.005;
            }

            // Calculate required zoom to fit bounding box
            let margin = 1.2; // Add 20% margin
            let viewport = &self.viewport;
            let screen_width = viewport.transform.screen_size.width.0 as f64;
            let screen_height = viewport.transform.screen_size.height.0 as f64;

            // Convert bounding box to Mercator
            let (min_x, min_y) = lat_lon_to_mercator(min_lat, min_lon);
            let (max_x, max_y) = lat_lon_to_mercator(max_lat, max_lon);
            let bbox_width = (max_x - min_x).abs();
            let bbox_height = (max_y - min_y).abs();

            // Calculate zoom to fit bbox in screen
            let world_width_meters = 40075016.686;
            let tile_size = 256.0;
            let zoom_x = ((screen_width * world_width_meters) / (bbox_width * tile_size * margin)).log2();
            let zoom_y = ((screen_height * world_width_meters) / (bbox_height * tile_size * margin)).log2();
            let zoom_level = zoom_x.min(zoom_y).max(1.0).min(18.0); // Clamp zoom to [1, 18]

            self.viewport.pan_to(center_lat, center_lon);
            self.viewport.set_zoom(zoom_level);
        }
    }

    fn toggle_layer_visibility(&mut self, layer_name: &str) {
        if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
            let current_visibility = layer.is_visible();
            layer.set_visible(!current_visibility);
        }
    }

    fn handle_mouse_down(&mut self, event: &MouseDownEvent) {
        // Adjust mouse coordinates to account for header offset
        let header_height = px(48.0);
        let adjusted_position = point(event.position.x, event.position.y - header_height);

        self.viewport.handle_mouse_down(adjusted_position);
        self.mouse_down_pos = Some(adjusted_position);
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        // Adjust mouse coordinates to account for header offset
        let header_height = px(48.0);
        let adjusted_position = point(event.position.x, event.position.y - header_height);

        if self.viewport.handle_mouse_move(adjusted_position) {
            cx.notify();
        }
    }

    fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let header_height = px(48.0);
        let up_pos = point(event.position.x, event.position.y - header_height);
        let was_click = match self.mouse_down_pos.take() {
            Some(down) => {
                let dx = up_pos.x.0 - down.x.0;
                let dy = up_pos.y.0 - down.y.0;
                (dx * dx + dy * dy).sqrt() < 4.0
            }
            None => false,
        };
        self.viewport.handle_mouse_up();
        if was_click {
            let before = self.selected.clone();
            self.handle_map_click(up_pos);
            if before != self.selected {
                cx.notify();
            }
        }
    }

    fn handle_map_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>) {
        let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
        self.selected = osm_gpui::selection::resolve_hits(per_layer);
    }

    fn sync_selection_to_layers(&mut self) {
        // Clear the selection if its owning layer is gone or hidden, so the
        // right panel never shows info for a feature not drawn on the map.
        if let Some(sel) = &self.selected {
            let still_live = self
                .layer_manager
                .find_layer(&sel.layer_name)
                .map(|l| l.is_visible())
                .unwrap_or(false);
            if !still_live {
                self.selected = None;
            }
        }
        let selected = self.selected.clone();
        for layer in self.layer_manager.layers_mut() {
            if let Some(sel) = &selected {
                if layer.name() == sel.layer_name {
                    layer.set_highlight(Some(sel.clone()));
                    continue;
                }
            }
            layer.set_highlight(None);
        }
    }

    fn handle_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let scroll_delta = match event.delta {
            gpui::ScrollDelta::Lines(delta) => gpui::Point {
                x: px(delta.x),
                y: px(delta.y),
            },
            gpui::ScrollDelta::Pixels(delta) => gpui::Point {
                x: px(delta.x.0 / 10.0),
                y: px(delta.y.0 / 10.0),
            },
        };

        // Adjust mouse coordinates to account for header offset
        let header_height = px(48.0);
        let adjusted_position = point(event.position.x, event.position.y - header_height);

        if self.viewport.handle_scroll(adjusted_position, scroll_delta) {
            cx.notify();
        }
    }

    fn check_for_new_osm_data(&mut self, cx: &mut Context<Self>) {
        if let Some(queue) = SHARED_OSM_DATA.get() {
            if let Ok(mut guard) = queue.try_lock() {
                if guard.is_empty() { return; }
                for (name, data) in guard.drain(..) {
                    let file_name = if name.is_empty() { "OSM".to_string() } else { name };
                    // Ensure unique layer name
                    let mut candidate = file_name.clone();
                    let mut i = 2;
                    while self.layer_manager.find_layer(&candidate).is_some() {
                        candidate = format!("{} ({})", file_name, i);
                        i += 1;
                    }
                    let data_arc = Arc::new(data.clone());
                    let layer = OsmLayer::new_with_data(candidate.clone(), data_arc.clone());
                    self.layer_manager.add_layer(Box::new(layer));
                    if !self.first_dataset_fitted {
                        self.fit_to_osm_data(&data);
                        self.first_dataset_fitted = true;
                    }
                }
                self.status_message = None;
                cx.notify();
            }
        }
    }

    fn check_for_layer_requests(&mut self, cx: &mut Context<Self>) {
        if let Some(requests) = LAYER_REQUESTS.get() {
            if let Ok(mut guard) = requests.try_lock() {
                if guard.is_empty() { return; }
                for req in guard.drain(..) {
                    match req {
                        LayerRequest::OsmCarto => {
                            if self.layer_manager.find_layer("OpenStreetMap Carto").is_none() {
                                let tile_layer = TileLayer::new(self.tile_cache.clone());
                                self.layer_manager.add_layer(Box::new(tile_layer));
                            }
                        }
                        LayerRequest::Imagery { name, url_template } => {
                            // Ensure unique name
                            let mut candidate = name.clone();
                            let mut i = 2;
                            while self.layer_manager.find_layer(&candidate).is_some() {
                                candidate = format!("{} ({})", name, i);
                                i += 1;
                            }
                            let layer = TileLayer::new_with_template(
                                candidate,
                                url_template,
                                self.tile_cache.clone(),
                            );
                            self.layer_manager.add_layer(Box::new(layer));
                        }
                    }
                }
                cx.notify();
            }
        }
    }

    fn get_layer_stats(&self) -> (usize, usize, usize) {
        let mut cached_files = 0;
        let mut osm_nodes = 0;
        let mut osm_ways = 0;

        for layer in self.layer_manager.layers() {
            let stats = layer.stats();
            for (key, value) in stats {
                match key.as_str() {
                    "Cached Files" => cached_files = value.parse().unwrap_or(0),
                    "Nodes" => osm_nodes = value.parse().unwrap_or(0),
                    "Ways" => osm_ways = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
        }

        // Calculate visible tiles
        let zoom_level = self.viewport.zoom_level();
        let tile_zoom = zoom_level.round().max(0.0).min(18.0) as u32;
        let bounds_geo = self.viewport.visible_bounds();
        let visible_tiles = tiles::get_tiles_for_bounds(
            bounds_geo.min_lat, bounds_geo.min_lon, bounds_geo.max_lat, bounds_geo.max_lon, tile_zoom
        );
        let total_tiles = visible_tiles.len();

        (total_tiles, cached_files, osm_nodes + osm_ways)
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = Some((message.into(), Instant::now()));
    }

    fn expire_status(&mut self) {
        if let Some((_, set_at)) = &self.status_message {
            if set_at.elapsed() > Duration::from_secs(5) {
                self.status_message = None;
            }
        }
    }

    fn check_for_download_requests(&mut self, cx: &mut Context<Self>) {
        let Some(requests) = DOWNLOAD_REQUESTS.get() else { return };
        let pending = if let Ok(mut guard) = requests.try_lock() {
            let n = guard.len();
            guard.clear();
            n
        } else {
            0
        };
        if pending == 0 { return }

        let bounds = self.viewport.visible_bounds();

        if let Err(e) = osm_api::check_area(&bounds) {
            self.set_status(e.to_string());
            cx.notify();
            return;
        }

        self.set_status("Downloading OSM data…");
        cx.notify();

        let label = format!(
            "OSM API ({:.4},{:.4},{:.4},{:.4})",
            bounds.min_lat, bounds.min_lon, bounds.max_lat, bounds.max_lon
        );

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { osm_api::fetch_bbox(bounds) })
                .await;

            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(data) => {
                        let data_arc = Arc::new(data);
                        let mut candidate = label.clone();
                        let mut i = 2;
                        while this.layer_manager.find_layer(&candidate).is_some() {
                            candidate = format!("{} ({})", label, i);
                            i += 1;
                        }
                        let layer = OsmLayer::new_with_data(candidate, data_arc);
                        this.layer_manager.add_layer(Box::new(layer));
                        this.status_message = None;
                    }
                    Err(e) => {
                        this.set_status(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Process any pending script command from the background runner thread.
    /// Called at the start of each render frame.
    fn process_script_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bus) = SCRIPT_BUS.get() else { return };

        let cmd = bus.take_pending();

        if let Some(cmd) = cmd {
            match cmd {
                ScriptCommand::SetViewport { lat, lon, zoom } => {
                    self.viewport.pan_to(lat, lon);
                    self.viewport.set_zoom(zoom);
                    // Ensure tile layer exists
                    if self.layer_manager.find_layer("OpenStreetMap Carto").is_none() {
                        let tile_layer = TileLayer::new(self.tile_cache.clone());
                        self.layer_manager.add_layer(Box::new(tile_layer));
                    }
                    cx.notify();
                }
                ScriptCommand::SetWindowSize { w, h } => {
                    window.resize(gpui::size(px(w as f32), px(h as f32)));
                    cx.notify();
                }
                ScriptCommand::Drag { from, to } => {
                    // For drag: just do down + single move + up; the sleep between steps
                    // happens in the runner thread, so here we do single events.
                    let ev = MouseDownEvent {
                        button: gpui::MouseButton::Left,
                        position: point(px(from.0), px(from.1)),
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                        first_mouse: false,
                    };
                    self.handle_mouse_down(&ev);
                    let ev = MouseMoveEvent {
                        position: point(px(to.0), px(to.1)),
                        pressed_button: Some(gpui::MouseButton::Left),
                        modifiers: gpui::Modifiers::none(),
                    };
                    self.handle_mouse_move(&ev, cx);
                    let ev = MouseUpEvent {
                        button: gpui::MouseButton::Left,
                        position: point(px(to.0), px(to.1)),
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                    };
                    self.handle_mouse_up(&ev, cx);
                    cx.notify();
                }
                ScriptCommand::Click { x, y, right } => {
                    let btn = if right { gpui::MouseButton::Right } else { gpui::MouseButton::Left };
                    let ev = MouseDownEvent {
                        button: btn,
                        position: point(px(x), px(y)),
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                        first_mouse: false,
                    };
                    self.handle_mouse_down(&ev);
                    let ev = MouseUpEvent {
                        button: btn,
                        position: point(px(x), px(y)),
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                    };
                    self.handle_mouse_up(&ev, cx);
                    cx.notify();
                }
                ScriptCommand::Scroll { x, y, dx, dy } => {
                    let ev = ScrollWheelEvent {
                        position: point(px(x), px(y)),
                        delta: ScrollDelta::Pixels(gpui::Point { x: px(dx), y: px(dy) }),
                        modifiers: gpui::Modifiers::none(),
                        touch_phase: gpui::TouchPhase::Moved,
                    };
                    self.handle_scroll(&ev, cx);
                }
            }
        }

        // Also drain keystroke queue (processed via Window so needs to be here)
        if let Some(ks_queue) = KEYSTROKE_QUEUE.get() {
            if let Ok(mut guard) = ks_queue.try_lock() {
                for ks in guard.drain(..) {
                    window.dispatch_keystroke(ks, &mut **cx);
                }
            }
        }

        // If a script runner thread is active, request an animation frame so
        // the render loop keeps going. This ensures the background thread never
        // starves waiting for a render that gpui wouldn't produce on its own.
        if SCRIPT_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
            window.request_animation_frame();
        }
    }

    fn render_selection_panel(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        use osm_gpui::selection::FeatureKind;

        let base = div()
            .id("selection-panel")
            .flex_1()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_3();

        let Some(sel) = self.selected.clone() else {
            return base.child(
                div()
                    .text_color(rgb(0x6b7280))
                    .text_sm()
                    .child("Click a feature to see its tags.")
            );
        };

        let kind_label = match sel.kind { FeatureKind::Node => "Node", FeatureKind::Way => "Way" };
        let url_kind = match sel.kind { FeatureKind::Node => "node", FeatureKind::Way => "way" };
        let tags_vec: Vec<(String, String)> = self
            .layer_manager
            .find_layer(&sel.layer_name)
            .and_then(|layer| layer.feature_tags(&sel))
            .unwrap_or_default();

        let header = div()
            .text_color(rgb(0xffffff))
            .text_lg()
            .font_weight(gpui::FontWeight::BOLD)
            .child(format!("{} #{}", kind_label, sel.id));

        let link_text = "View on openstreetmap.org ↗".to_string();
        let url = format!("https://www.openstreetmap.org/{}/{}", url_kind, sel.id);
        let link = div()
            .id(("osm-link", sel.id as usize))
            .text_color(rgb(0x60a5fa))
            .text_sm()
            .cursor_pointer()
            .child(link_text)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |_this, _ev: &MouseDownEvent, _, cx| {
                    cx.open_url(&url);
                }),
            );

        let tags_block = if tags_vec.is_empty() {
            div()
                .text_color(rgb(0x6b7280))
                .text_sm()
                .child("(no tags)")
                .into_any_element()
        } else {
            let mut col = div().flex().flex_col().gap_1();
            for (k, v) in tags_vec {
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            div()
                                .text_color(rgb(0xd1d5db))
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(k)
                        )
                        .child(
                            div()
                                .text_color(rgb(0xffffff))
                                .text_sm()
                                .child(v)
                        )
                );
            }
            col.into_any_element()
        };

        base.child(header).child(link).child(tags_block)
    }
}

impl Render for MapViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Consume any pending script command first.
        self.process_script_command(window, cx);

        // Drain cross-thread queues BEFORE signalling the script bus, so
        // ops like `load_osm` (which push here and then call wait_frame)
        // observe the resulting layer on the same frame.
        self.check_for_new_osm_data(cx);
        self.check_for_layer_requests(cx);
        self.check_for_download_requests(cx);
        self.maybe_rebuild_imagery_menu(cx);

        // Now it's safe to signal: the effects of this frame's commands
        // and pushes are visible.
        if let Some(bus) = SCRIPT_BUS.get() {
            bus.signal_done_and_frame();
        }

        // Update viewport size to actual window dimensions minus the right panel and header
        let window_size = window.bounds().size;
        let panel_width = px(280.0);
        let header_height = px(48.0); // h_12() = 12 * 4px = 48px
        let map_size = gpui::size(
            window_size.width - panel_width,
            window_size.height - header_height
        );
        self.viewport.update_size(map_size);

        self.expire_status();

        // Update all layers
        self.layer_manager.update_all();
        self.sync_selection_to_layers();

        let (center_lat, center_lon) = self.viewport.center();
        let zoom_level = self.viewport.zoom_level();
        let (total_tiles, cached_files, osm_objects) = self.get_layer_stats();
        let fps = self.tick_fps();

        // Collect layer information for the UI
        let layer_info: Vec<(String, bool)> = self.layer_manager.layers()
            .iter()
            .map(|layer| (layer.name().to_string(), layer.is_visible()))
            .collect();

        div()
            .size_full()
            .bg(rgb(0x1a202c))
            .flex()
            .flex_row()
            .child(
                // Main content area (header + map)
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        // Header with menu
                        div()
                            .h_12()
                            .bg(rgb(0x111827))
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_4()
                            .child(
                                div()
                                    .text_color(rgb(0xffffff))
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("🗺️ OSM-GPUI Map Viewer (Layered)"),
                            )
                            .child(
                                div()
                                    .text_color(rgb(0x9ca3af))
                                    .text_sm()
                                    .child("Mouse to pan/zoom | 'T' tiles | Click layers to toggle"),
                            ),
                    )
                    .child(
                        // Map area
                        div()
                            .flex_1()
                            .relative()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                                    this.handle_mouse_down(ev);
                                }),
                            )
                            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                                this.handle_mouse_move(ev, cx);
                            }))
                            .on_mouse_up(
                                gpui::MouseButton::Left,
                                cx.listener(|this, ev: &MouseUpEvent, _, cx| {
                                    this.handle_mouse_up(ev, cx);
                                }),
                            )
                            .on_mouse_up_out(
                                gpui::MouseButton::Left,
                                cx.listener(|this, ev: &MouseUpEvent, _, cx| {
                                    this.handle_mouse_up(ev, cx);
                                }),
                            )
                            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                                this.handle_scroll(ev, cx);
                            }))
                            .child(
                                div()
                                    .size_full()
                                    .relative()
                                    .overflow_hidden() // Add clipping to prevent tiles from drawing outside viewport
                                    // Render all layer elements (raster content like tiles)
                                    .children(self.layer_manager.render_all_elements(&self.viewport))
                                    // Render canvas layers (vector content)
                                    .child(
                                        canvas(
                                            |_, _, _| {},
                                            {
                                                let viewport_clone = self.viewport.clone();
                                                let layer_manager = std::ptr::addr_of!(self.layer_manager);
                                                let selected = self.selected.clone();
                                                move |bounds, _, window, _| {
                                                    let layer_manager = unsafe { &*layer_manager };
                                                    layer_manager.render_all_canvas(&viewport_clone, bounds, window);
                                                    if let Some(sel) = &selected {
                                                        layer_manager.render_highlight(sel, &viewport_clone, bounds, window);
                                                    }
                                                }
                                            }
                                        )
                                        .absolute()
                                        .size_full() // Ensure canvas fills the entire map area
                                    )
                            )
                            .child(
                                // Debug info overlay
                                div()
                                    .absolute()
                                    .top_4()
                                    .left_4()
                                    .p_3()
                                    .bg(gpui::black())
                                    .rounded_lg()
                                    .text_color(rgb(0xffffff))
                                    .text_sm()
                                    .opacity(0.9)
                                    .min_w_64()
                                    .child(format!("🔍 Zoom: {:.1}", zoom_level))
                                    .child(format!("🌍 Center: {:.4}°N, {:.4}°W", center_lat, center_lon.abs()))
                                    .child(format!("📊 Objects: {}", osm_objects))
                                    .child(format!("🗺️ Tiles: {} visible", total_tiles))
                                    .child(format!("💾 Cache: {} files", cached_files))
                                    .child(format!("⚡ FPS: {:.0}", fps))
                            )
                            .child({
                                let status = self.status_message.clone();
                                if let Some((msg, _)) = status {
                                    div()
                                        .absolute()
                                        .top_4()
                                        .right_4()
                                        .p_3()
                                        .bg(gpui::black())
                                        .rounded_lg()
                                        .text_color(rgb(0xffffff))
                                        .text_sm()
                                        .opacity(0.9)
                                        .child(msg)
                                        .into_any_element()
                                } else {
                                    div().into_any_element()
                                }
                            }),
                    )
            )
            .child(
                // Right panel with layer controls
                div()
                    .w(px(280.0))
                    .h_full()
                    .bg(rgb(0x111827))
                    .border_l_1()
                    .border_color(rgb(0x374151))
                    .flex()
                    .flex_col()
                    .child(
                        // Panel header
                        div()
                            .h_12()
                            .bg(rgb(0x1f2937))
                            .flex()
                            .items_center()
                            .px_4()
                            .border_b_1()
                            .border_color(rgb(0x374151))
                            .child(
                                div()
                                    .text_color(rgb(0xffffff))
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("🏗️ Layer Controls")
                            )
                    )
                    .child(
                        // Layer list container
                        div()
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(
                                layer_info.iter().enumerate().map(|(index, (name, is_visible))| {
                                    let layer_name = name.clone();
                                    div()
                                        .id(("layer", index))
                                        .p_3()
                                        .bg(rgb(0x1f2937))
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(if *is_visible { rgb(0x10b981) } else { rgb(0x374151) })
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(move |this, _event: &MouseDownEvent, _, cx| {
                                                this.toggle_layer_visibility(&layer_name);
                                                cx.notify();
                                            }),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    // Checkbox
                                                    div()
                                                        .w(px(20.0))
                                                        .h(px(20.0))
                                                        .rounded_sm()
                                                        .border_2()
                                                        .border_color(if *is_visible { rgb(0x10b981) } else { rgb(0x6b7280) })
                                                        .bg(if *is_visible { rgb(0x10b981) } else { rgb(0x374151) })
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .when(*is_visible, |this| {
                                                            this.child(
                                                                div()
                                                                    .text_color(rgb(0xffffff))
                                                                    .text_sm()
                                                                    .font_weight(gpui::FontWeight::BOLD)
                                                                    .child("✓")
                                                            )
                                                        })
                                                )
                                                .child(
                                                    div()
                                                        .text_color(rgb(0xffffff))
                                                        .text_sm()
                                                        .font_weight(gpui::FontWeight::MEDIUM)
                                                        .child(name.clone())
                                                )
                                        )
                                        .child(
                                            // Layer order indicator
                                            div()
                                                .text_color(rgb(0x9ca3af))
                                                .text_xs()
                                                .child(format!("#{}", index + 1))
                                        )
                                })
                                .collect::<Vec<_>>()
                            )
                    )
                    // Divider between layer controls and selection panel
                    .child(
                        div()
                            .h(px(1.0))
                            .bg(rgb(0x374151))
                    )
                    // Selection panel (flex_1, scrollable)
                    .child(self.render_selection_panel(cx))
            )
    }
}

// ---------------------------------------------------------------------------
// LiveApp: AppHandle impl backed by ScriptBus (background-thread safe)
// ---------------------------------------------------------------------------

struct LiveApp {
    idle: Arc<IdleTracker>,
    bus: Arc<ScriptBus>,
    window_id: u32,
}

impl AppHandle for LiveApp {
    fn set_window_size(&mut self, w: u32, h: u32) {
        self.bus.submit(ScriptCommand::SetWindowSize { w, h });
    }

    fn set_viewport(&mut self, lat: f64, lon: f64, zoom: f32) {
        self.bus.submit(ScriptCommand::SetViewport { lat, lon, zoom: zoom as f64 });
    }

    fn dispatch_drag(&mut self, from: (f32, f32), to: (f32, f32), _duration: Duration) {
        // Submit as a single command; the render fn handles the full down/move/up.
        self.bus.submit(ScriptCommand::Drag { from, to });
    }

    fn dispatch_click(&mut self, at: (f32, f32), button: script::MouseButton) {
        let right = matches!(button, script::MouseButton::Right);
        self.bus.submit(ScriptCommand::Click { x: at.0, y: at.1, right });
    }

    fn dispatch_scroll(&mut self, at: (f32, f32), dx: f32, dy: f32) {
        self.bus.submit(ScriptCommand::Scroll { x: at.0, y: at.1, dx, dy });
    }

    fn dispatch_key(&mut self, chord: &script::Chord) {
        // Keystroke is Send (only contains String + bools), use the dedicated queue.
        let ks = Keystroke {
            modifiers: gpui::Modifiers {
                control: chord.ctrl,
                alt: chord.alt,
                shift: chord.shift,
                platform: chord.cmd,
                function: false,
            },
            key: chord.key.clone(),
            key_char: None,
        };
        if let Some(q) = KEYSTROKE_QUEUE.get() {
            if let Ok(mut guard) = q.lock() {
                guard.push(ks);
            }
        }
        // Wait for next frame so gpui processes the keystroke.
        self.bus.wait_frame();
    }

    fn wait_frame(&mut self) {
        self.bus.wait_frame();
    }

    fn load_osm(&mut self, path: &std::path::Path) -> Result<(), String> {
        let parser = OsmParser::new();
        let path_str = path.to_string_lossy().to_string();
        let data = parser.parse_file(&path_str).map_err(|e| e.to_string())?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("OSM").to_string();
        if let Some(q) = SHARED_OSM_DATA.get() {
            if let Ok(mut guard) = q.lock() {
                guard.push((stem, data));
            } else {
                return Err("SHARED_OSM_DATA mutex poisoned".into());
            }
        } else {
            return Err("SHARED_OSM_DATA not initialized".into());
        }
        // Thanks to the reorder in render(), the next frame drains the queue
        // before signalling — so after wait_frame the layer exists.
        self.bus.wait_frame();
        Ok(())
    }
}

fn main() {
    eprintln!("🚀 Starting OSM-GPUI Map Viewer with Tile Loading");

    let args = parse_cli_args();
    let (win_w, win_h) = args.window_size.unwrap_or((1200, 800));

    // Initialize the global idle tracker before the app starts so TileCache
    // picks up the same Arc.
    let idle = IdleTracker::new();
    GLOBAL_IDLE.set(idle.clone()).ok();

    // Initialize script bus
    let bus = ScriptBus::new();
    SCRIPT_BUS.set(bus.clone()).ok();
    KEYSTROKE_QUEUE.set(Arc::new(Mutex::new(Vec::new()))).ok();

    // Initialize shared OSM data
    SHARED_OSM_DATA.set(Arc::new(Mutex::new(Vec::new()))).unwrap();
    LAYER_REQUESTS.set(Arc::new(Mutex::new(Vec::new()))).unwrap();
    DOWNLOAD_REQUESTS.set(Arc::new(Mutex::new(Vec::new()))).unwrap();
    IMAGERY_INDEX.set(Arc::new(Mutex::new(Vec::new()))).unwrap();
    IMAGERY_LOAD_STATE
        .set(Arc::new(Mutex::new(ImageryLoadState::Loading)))
        .unwrap();

    // If there's a script, spawn it on a background OS thread before the app
    // starts. The thread blocks until the window is visible, then drives the
    // live app via ScriptBus.
    if let Some(script_path) = args.script {
        let keep_open = args.keep_open;
        let idle_for_runner = idle.clone();
        let bus_for_runner = bus.clone();

        std::thread::spawn(move || {
            SCRIPT_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
            // Wait for the window to be on-screen.
            std::thread::sleep(Duration::from_millis(500));

            // Find the window's OS-level ID.
            let window_id = match capture::find_own_window_id() {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("script: could not find window id: {}", e);
                    std::process::exit(1);
                }
            };

            // Parse the script file.
            let script_text = match std::fs::read_to_string(&script_path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("script: could not read {:?}: {}", script_path, e);
                    std::process::exit(1);
                }
            };
            let steps = match script::parse(&script_text) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("script: parse error: {}", e);
                    std::process::exit(1);
                }
            };

            let runner = Runner {
                idle: idle_for_runner,
                window_id,
            };

            let mut live_app = LiveApp {
                idle: idle.clone(),
                bus: bus_for_runner,
                window_id,
            };

            match runner.run(&mut live_app, &steps) {
                Ok(()) => {
                    SCRIPT_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
                    if !keep_open {
                        std::process::exit(0);
                    }
                }
                Err(e) => {
                    SCRIPT_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
                    eprintln!("script error: {}", e);
                    std::process::exit(1);
                }
            }
        });
    }

    Application::new().run(move |cx: &mut App| {
        // Bring the menu bar to the foreground
        cx.activate(true);

        // Register the open file action
        cx.on_action(open_osm_file);
        cx.on_action(quit);
        cx.on_action(add_osm_carto);
        cx.on_action(download_from_osm);
        cx.on_action(add_imagery_layer);
        cx.on_action(no_op_imagery_info);

        // Initial menu (before ELI loads). MapViewer's render loop will call
        // rebuild_menus again whenever the load state or viewport changes.
        rebuild_menus(cx, 40.7128, -74.0060, ImageryLoadState::Loading);

        // Kick off background download/parse of the Editor Layer Index.
        cx.background_executor()
            .spawn(async move {
                match imagery::fetch_and_cache() {
                    Ok(body) => {
                        let entries = imagery::parse(&body);
                        eprintln!("imagery: loaded {} ELI entries", entries.len());
                        if let Some(index) = IMAGERY_INDEX.get() {
                            if let Ok(mut guard) = index.lock() {
                                *guard = entries;
                            }
                        }
                        if let Some(state) = IMAGERY_LOAD_STATE.get() {
                            if let Ok(mut g) = state.lock() {
                                *g = ImageryLoadState::Ready;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("imagery: failed to load ELI: {}", e);
                        if let Some(state) = IMAGERY_LOAD_STATE.get() {
                            if let Ok(mut g) = state.lock() {
                                *g = ImageryLoadState::Failed;
                            }
                        }
                    }
                }
            })
            .detach();

        cx.open_window(
            WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(Bounds {
                    origin: point(px(100.0), px(100.0)),
                    size: size(px(win_w as f32), px(win_h as f32)),
                })),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("OSM-GPUI Map Viewer".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                focus: true,
                ..Default::default()
            },
            |window, cx| {
                // Register keyboard bindings in the window context
                cx.bind_keys([
                    KeyBinding::new("cmd-o", OpenOsmFile, None),
                    KeyBinding::new("cmd-shift-d", DownloadFromOsm, None),
                    KeyBinding::new("cmd-q", Quit, None),
                ]);
                cx.new(|cx| MapViewer::new(window, cx))
            },
        )
        .unwrap();

        cx.on_window_closed(|cx| {
            cx.quit();
        })
        .detach();
    });
}

// Handle the File > Open OSM File menu action
fn open_osm_file(_: &OpenOsmFile, cx: &mut App) {
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
fn quit(_: &Quit, cx: &mut App) {
    cx.quit();
}

// Handle the File > Download from OSM menu action
fn download_from_osm(_: &DownloadFromOsm, _cx: &mut App) {
    if let Some(requests) = DOWNLOAD_REQUESTS.get() {
        if let Ok(mut q) = requests.lock() {
            q.push(());
        }
    }
}

// Handle the Imagery > OpenStreetMap Carto menu action
fn add_osm_carto(_: &AddOsmCarto, _cx: &mut App) {
    if let Some(requests) = LAYER_REQUESTS.get() {
        if let Ok(mut queue) = requests.lock() {
            queue.push(LayerRequest::OsmCarto);
        }
    }
}

// Handle an ELI imagery menu action. Looks up the entry in the loaded index
// and enqueues a layer request.
fn add_imagery_layer(action: &AddImageryLayer, _cx: &mut App) {
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
            });
        }
    }
}

/// Build and install the menu bar, using the current viewport center to filter
/// the Imagery menu to relevant ELI entries.
fn rebuild_menus(cx: &mut App, center_lat: f64, center_lon: f64, state: ImageryLoadState) {
    let mut imagery_items: Vec<MenuItem> =
        vec![MenuItem::action("OpenStreetMap Carto", AddOsmCarto)];

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
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit\t⌘Q", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open…\t⌘O", OpenOsmFile),
                MenuItem::action("Download from OSM\t⌘⇧D", DownloadFromOsm),
            ],
        },
        Menu {
            name: "Imagery".into(),
            items: imagery_items,
        },
    ]);
}

// Dummy action used for disabled-style "info" entries in the Imagery menu.
// (GPUI does not support disabled menu items directly, so we use a no-op.)
actions!(osm_gpui, [NoOpImageryInfo]);

fn no_op_imagery_info(_: &NoOpImageryInfo, _cx: &mut App) {}
