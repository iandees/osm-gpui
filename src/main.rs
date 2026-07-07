use gpui::{actions, canvas, div, point, prelude::*, px, rgb, size, App, Bounds, Context, KeyBinding, Keystroke, Menu, MenuItem, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, ScrollDelta, ScrollWheelEvent, SharedString, SystemMenuType, Window, WindowOptions};
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
use osm_gpui::custom_imagery_store::{self, CustomImageryEntry};
use osm_gpui::tile_cache::TileCache;
use osm_gpui::osm::{OsmData, OsmParser};
use osm_gpui::viewport::Viewport;
use osm_gpui::layers::{LayerManager, tile_layer::TileLayer, osm_layer::OsmLayer, grid_layer::GridLayer};
use osm_gpui::tiles;
use osm_gpui::osm_api;
use osm_gpui::script::{self, runner::{AppHandle, Runner}};
use osm_gpui::capture;
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    label::Label,
    menu::ContextMenuExt,
};

actions!(osm_gpui, [OpenOsmFile, Quit, AddOsmCarto, AddCoordinateGrid, DownloadFromOsm, ToggleDebugOverlay, AddCustomImagery, OpenSettings, Undo, Redo]);

/// Action for adding an imagery layer from the ELI by id.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
struct AddImageryLayer {
    id: SharedString,
}

/// Action for adding a saved custom imagery entry as a layer.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
struct AddSavedCustomImagery {
    index: usize,
}

/// Action to move the layer at `index` by `delta` positions (negative = up).
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = layers)]
#[serde(deny_unknown_fields)]
struct MoveLayer {
    index: usize,
    delta: i32,
}

/// Action to delete the layer at `index`.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = layers)]
#[serde(deny_unknown_fields)]
struct DeleteLayer {
    index: usize,
}

/// Request to add a new layer from a menu action.
#[derive(Debug, Clone)]
enum LayerRequest {
    OsmCarto,
    CoordinateGrid,
    Imagery {
        name: String,
        url_template: String,
        min_zoom: Option<u32>,
        max_zoom: Option<u32>,
    },
    /// Remove the layer at the given index in the `LayerManager`.
    Delete { index: usize },
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

static TOGGLE_DEBUG_OVERLAY: std::sync::OnceLock<Arc<Mutex<Vec<()>>>> =
    std::sync::OnceLock::new();

static OPEN_CUSTOM_IMAGERY_DIALOG: OnceLock<Arc<Mutex<Vec<()>>>> = OnceLock::new();

// Global idle tracker shared with the script runner
static GLOBAL_IDLE: std::sync::OnceLock<Arc<IdleTracker>> = std::sync::OnceLock::new();

/// Guard to prevent opening multiple settings windows simultaneously.
static SETTINGS_WINDOW_OPEN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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
    selected: Vec<osm_gpui::selection::FeatureRef>,
    mouse_down_pos: Option<gpui::Point<gpui::Pixels>>,
    /// Screen-space (start, current) points of an in-progress left-drag box
    /// select, or `None` when not dragging a box.
    box_select: Option<(gpui::Point<gpui::Pixels>, gpui::Point<gpui::Pixels>)>,
    /// In-progress move-drag of the current selection, or `None` when not
    /// dragging a move.
    move_drag: Option<MoveDrag>,
    frame_times: VecDeque<Instant>,
    /// Last (lat, lon) the Imagery menu was rebuilt for. None forces a rebuild.
    last_menu_center: Option<(f64, f64)>,
    /// Imagery load state observed on the previous frame; detect transitions.
    last_imagery_load_state: Option<ImageryLoadState>,
    /// Whether the debug info overlay is currently visible.
    show_debug_overlay: bool,
    /// Active custom imagery dialog, if open.
    custom_imagery_dialog: Option<gpui::Entity<osm_gpui::ui::custom_imagery_dialog::CustomImageryDialog>>,
    /// Active tag-edit dialog, if open, plus the context needed to apply
    /// its result.
    tag_edit_dialog: Option<(gpui::Entity<osm_gpui::ui::tag_edit_dialog::TagEditDialog>, TagEditContext)>,
    /// A dialog-open request recorded by a row/button click, to be acted on
    /// during the next `render()` — see `PendingTagEditOpen`'s doc comment.
    pending_tag_edit_open: Option<PendingTagEditOpen>,
    /// Indices of the currently-open accordion sections in the side panel.
    side_panel_open: Vec<usize>,
    /// Focus handle for the map area, so it can receive key events (e.g.
    /// Escape to cancel an in-progress move-drag).
    focus_handle: gpui::FocusHandle,
    /// Global undo/redo history of committed data mutations.
    undo_stack: UndoStack,
}

/// Per-layer node ids being moved, each with its pre-drag `(lat, lon)`.
type NodeMoveTargets = Vec<(String, Vec<(i64, f64, f64)>)>;

/// Per layer: node id -> (before (lat, lon), after (lat, lon)).
type NodeMoveUndoEntries = Vec<(String, Vec<(i64, (f64, f64), (f64, f64))>)>;

/// Which features a `TagEditDialog` targets and the row's original text
/// (used to detect whether the value box was actually touched before
/// applying — see `compute_tag_edit_entries`). `original_key`/
/// `original_value` are both empty for the "Add tag" flow.
struct TagEditContext {
    features: Vec<osm_gpui::selection::FeatureRef>,
    original_key: String,
    original_value: String,
    is_add: bool,
}

/// Recorded by a row's double-click or the "Add tag" button; consumed by
/// `check_for_pending_tag_edit_dialog` (called from `Render::render`) to
/// actually construct the dialog. This indirection exists so the dialog is
/// always built from inside a render pass — never directly inside a click
/// handler — which `TagEditDialog`'s deferred select-all-on-open (Task 4)
/// depends on.
struct PendingTagEditOpen {
    features: Vec<osm_gpui::selection::FeatureRef>,
    original_key: String,
    original_value: String,
    select: osm_gpui::ui::tag_edit_dialog::TagEditField,
    is_add: bool,
}

/// A single reversible data mutation, recorded on the global undo stack.
/// Only one kind exists today (produced by committing a drag-to-move), but
/// the enum leaves room for future mutation kinds (tag edits, deletes, ...)
/// without restructuring the stack.
#[derive(Clone)]
enum UndoableAction {
    MoveNodes { per_layer: NodeMoveUndoEntries },
    /// One entry per affected feature: which key, and its value before/
    /// after (`None` = key was/becomes absent). A key rename is modeled as
    /// two entries for the same feature — remove-old plus add-new — so
    /// this stays a single uniform apply loop.
    SetTags {
        entries: Vec<(osm_gpui::selection::FeatureRef, String, Option<String>, Option<String>)>,
    },
}

impl UndoableAction {
    /// Human-readable label for the history list, e.g. "Moved 3 nodes".
    fn description(&self) -> String {
        match self {
            UndoableAction::MoveNodes { per_layer } => {
                let count: usize = per_layer.iter().map(|(_, entries)| entries.len()).sum();
                if count == 1 {
                    "Moved 1 node".to_string()
                } else {
                    format!("Moved {} nodes", count)
                }
            }
            UndoableAction::SetTags { entries } => {
                if entries.len() == 1 {
                    "Changed 1 tag".to_string()
                } else {
                    format!("Changed {} tags", entries.len())
                }
            }
        }
    }
}

/// A global undo/redo stack of committed data mutations, shared across all
/// layers in the order actions happened.
#[derive(Default)]
struct UndoStack {
    actions: Vec<UndoableAction>,
    /// Index of the next action that would be redone. Equals
    /// `actions.len()` when at the tip (nothing to redo).
    cursor: usize,
}

impl UndoStack {
    fn push(&mut self, action: UndoableAction) {
        self.actions.truncate(self.cursor);
        self.actions.push(action);
        self.cursor = self.actions.len();
    }

    /// Returns the action to invert, and moves the cursor back. `None` if
    /// there's nothing to undo.
    fn undo(&mut self) -> Option<UndoableAction> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        Some(self.actions[self.cursor].clone())
    }

    /// Returns the action to reapply, and moves the cursor forward. `None`
    /// if there's nothing to redo.
    fn redo(&mut self) -> Option<UndoableAction> {
        if self.cursor == self.actions.len() {
            return None;
        }
        let action = self.actions[self.cursor].clone();
        self.cursor += 1;
        Some(action)
    }
}

#[cfg(test)]
mod undo_stack_tests {
    use super::{UndoStack, UndoableAction};

    fn move_one(id: i64, before: (f64, f64), after: (f64, f64)) -> UndoableAction {
        UndoableAction::MoveNodes {
            per_layer: vec![("L".to_string(), vec![(id, before, after)])],
        }
    }

    #[test]
    fn description_singular_and_plural() {
        let one = move_one(1, (0.0, 0.0), (1.0, 1.0));
        assert_eq!(one.description(), "Moved 1 node");

        let two = UndoableAction::MoveNodes {
            per_layer: vec![(
                "L".to_string(),
                vec![
                    (1, (0.0, 0.0), (1.0, 1.0)),
                    (2, (0.0, 0.0), (1.0, 1.0)),
                ],
            )],
        };
        assert_eq!(two.description(), "Moved 2 nodes");
    }

    #[test]
    fn undo_redo_at_empty_stack_is_none() {
        let mut stack = UndoStack::default();
        assert!(stack.undo().is_none());
        assert!(stack.redo().is_none());
    }

    #[test]
    fn push_then_undo_then_redo_round_trips() {
        let mut stack = UndoStack::default();
        stack.push(move_one(1, (0.0, 0.0), (1.0, 1.0)));

        assert!(stack.redo().is_none(), "nothing to redo right after a push");

        let undone = stack.undo().expect("should have one action to undo");
        assert_eq!(undone.description(), "Moved 1 node");
        assert!(stack.undo().is_none(), "only one action was pushed");

        let redone = stack.redo().expect("should be able to redo after undo");
        assert_eq!(redone.description(), "Moved 1 node");
        assert!(stack.redo().is_none(), "back at the tip, nothing left to redo");
    }

    #[test]
    fn push_after_undo_discards_redo_branch() {
        let mut stack = UndoStack::default();
        stack.push(move_one(1, (0.0, 0.0), (1.0, 1.0)));
        stack.push(move_one(2, (0.0, 0.0), (2.0, 2.0)));

        stack.undo(); // cursor now points at action 2 as redo-able
        stack.push(move_one(3, (0.0, 0.0), (3.0, 3.0))); // discards action 2

        // Only actions 1 and 3 remain; redo should have nothing left.
        assert!(stack.redo().is_none());
        let undone_3 = stack.undo().unwrap();
        assert_eq!(undone_3.description(), "Moved 1 node");
        let undone_1 = stack.undo().unwrap();
        assert_eq!(undone_1.description(), "Moved 1 node");
        assert!(stack.undo().is_none());
    }

    fn tag_change(
        feature: osm_gpui::selection::FeatureRef,
        key: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> (osm_gpui::selection::FeatureRef, String, Option<String>, Option<String>) {
        (
            feature,
            key.to_string(),
            before.map(|s| s.to_string()),
            after.map(|s| s.to_string()),
        )
    }

    #[test]
    fn set_tags_description_singular_and_plural() {
        use osm_gpui::selection::{FeatureKind, FeatureRef};
        let f = FeatureRef { layer_name: "L".to_string(), kind: FeatureKind::Node, id: 1 };

        let one = UndoableAction::SetTags {
            entries: vec![tag_change(f.clone(), "highway", None, Some("residential"))],
        };
        assert_eq!(one.description(), "Changed 1 tag");

        let two = UndoableAction::SetTags {
            entries: vec![
                tag_change(f.clone(), "highway", None, Some("residential")),
                tag_change(f, "surface", None, Some("paved")),
            ],
        };
        assert_eq!(two.description(), "Changed 2 tags");
    }
}

/// Snapshot of the nodes being moved by an in-progress drag: which layer
/// they belong to, and each affected node's id and pre-drag (lat, lon).
struct MoveDrag {
    per_layer: NodeMoveTargets,
}

/// Normalize two arbitrary screen points into a `Bounds` with a top-left
/// origin and non-negative size, regardless of drag direction.
fn normalize_rect(
    a: gpui::Point<gpui::Pixels>,
    b: gpui::Point<gpui::Pixels>,
) -> gpui::Bounds<gpui::Pixels> {
    let min_x = a.x.as_f32().min(b.x.as_f32());
    let max_x = a.x.as_f32().max(b.x.as_f32());
    let min_y = a.y.as_f32().min(b.y.as_f32());
    let max_y = a.y.as_f32().max(b.y.as_f32());
    gpui::Bounds {
        origin: gpui::point(px(min_x), px(min_y)),
        size: gpui::size(px(max_x - min_x), px(max_y - min_y)),
    }
}

impl MapViewer {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let viewport = Viewport::new(40.7128, -74.0060, 11.0, gpui::size(px(800.0), px(600.0)));
        let executor = cx.background_executor().clone();
        // Use the global idle tracker (set before Application::new().run(...))
        let idle = GLOBAL_IDLE.get().cloned().unwrap_or_else(IdleTracker::new);
        let tile_cache = Arc::new(Mutex::new(TileCache::new(executor, idle)));
        let layer_manager = LayerManager::new();
        // No default layers; tile and grid layers are added via the menu.

        // No default OSM layer; loaded files add their own
        Self {
            viewport,
            layer_manager,
            tile_cache,
            first_dataset_fitted: false,
            status_message: None,
            selected: Vec::new(),
            mouse_down_pos: None,
            box_select: None,
            move_drag: None,
            frame_times: VecDeque::with_capacity(120),
            last_menu_center: None,
            last_imagery_load_state: None,
            show_debug_overlay: false,
            custom_imagery_dialog: None,
            tag_edit_dialog: None,
            pending_tag_edit_open: None,
            side_panel_open: vec![0, 1, 2],
            focus_handle: cx.focus_handle(),
            undo_stack: UndoStack::default(),
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
            let screen_width = viewport.transform.screen_size.width.to_f64();
            let screen_height = viewport.transform.screen_size.height.to_f64();

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

    fn reorder_layer(&mut self, from: usize, to: usize) {
        self.layer_manager.move_layer(from, to);
    }

    /// Handle the `MoveLayer` context-menu action.
    fn on_move_layer(&mut self, action: &MoveLayer, _: &mut Window, cx: &mut Context<Self>) {
        let total = self.layer_manager.layers().len();
        let target = action.index as i32 + action.delta;
        if target >= 0 && (target as usize) < total {
            self.reorder_layer(action.index, target as usize);
            cx.notify();
        }
    }

    /// Handle the `DeleteLayer` context-menu action (routes through LAYER_REQUESTS).
    fn on_delete_layer(&mut self, action: &DeleteLayer, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(reqs) = LAYER_REQUESTS.get() {
            if let Ok(mut guard) = reqs.lock() {
                guard.push(LayerRequest::Delete { index: action.index });
            }
        }
        cx.notify();
    }

    fn handle_mouse_down(&mut self, event: &MouseDownEvent) {
        let adjusted_position = event.position;

        self.viewport.handle_mouse_down(adjusted_position);
        self.mouse_down_pos = Some(adjusted_position);
    }

    /// Left-button mouse-down: if the point hits a currently-selected
    /// feature, start a move-drag instead of the usual box-select/click
    /// tracking. Always records `mouse_down_pos` either way, since both
    /// paths need it to distinguish a click from a drag on release.
    fn handle_map_mouse_down(&mut self, position: gpui::Point<gpui::Pixels>) {
        self.mouse_down_pos = Some(position);
        if self.selected.is_empty() {
            return;
        }
        if self
            .layer_manager
            .hit_test_selection(&self.viewport, position, &self.selected)
            .is_none()
        {
            return;
        }
        let per_layer = self.resolve_move_targets();
        if !per_layer.is_empty() {
            self.move_drag = Some(MoveDrag { per_layer });
        }
    }

    /// Resolve the current selection into, per owning layer, the set of node
    /// ids to translate: a selected node contributes its own id; a selected
    /// way contributes every one of its member node ids. Each id's current
    /// (lat, lon) is snapshotted for use as the drag's translation anchor.
    fn resolve_move_targets(&self) -> NodeMoveTargets {
        use osm_gpui::selection::FeatureKind;
        use std::collections::{HashMap, HashSet};

        let mut ids_by_layer: HashMap<String, HashSet<i64>> = HashMap::new();
        for feat in &self.selected {
            let Some(layer) = self.layer_manager.find_layer(&feat.layer_name) else { continue; };
            let entry = ids_by_layer.entry(feat.layer_name.clone()).or_default();
            match feat.kind {
                FeatureKind::Node => {
                    entry.insert(feat.id);
                }
                FeatureKind::Way => {
                    if let Some(node_ids) = layer.way_node_ids(feat.id) {
                        entry.extend(node_ids);
                    }
                }
            }
        }

        ids_by_layer
            .into_iter()
            .filter_map(|(layer_name, ids)| {
                let layer = self.layer_manager.find_layer(&layer_name)?;
                let originals: Vec<(i64, f64, f64)> = ids
                    .into_iter()
                    .filter_map(|id| layer.node_lat_lon(id).map(|(lat, lon)| (id, lat, lon)))
                    .collect();
                if originals.is_empty() {
                    None
                } else {
                    Some((layer_name, originals))
                }
            })
            .collect()
    }

    /// Cancel an in-progress move-drag: clears the preview on every affected
    /// layer without mutating any data.
    fn cancel_move_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.move_drag.take() {
            for (layer_name, _) in &drag.per_layer {
                if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                    layer.clear_drag_preview();
                }
            }
            cx.notify();
        }
    }

    /// Apply an undo/redo step: `forward = true` reapplies the action's
    /// "after" state (redo), `forward = false` reverts to its "before"
    /// state (undo). Both directions reuse the same commit path as a live
    /// drag, so caches rebuild exactly once per affected layer.
    fn apply_undo_action(&mut self, action: &UndoableAction, forward: bool) {
        match action {
            UndoableAction::MoveNodes { per_layer } => {
                for (layer_name, entries) in per_layer {
                    let moves: Vec<(i64, f64, f64)> = entries
                        .iter()
                        .map(|&(id, before, after)| {
                            let (lat, lon) = if forward { after } else { before };
                            (id, lat, lon)
                        })
                        .collect();
                    if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                        layer.commit_node_moves(&moves);
                    }
                }
            }
            UndoableAction::SetTags { entries } => {
                for (feature, key, before, after) in entries {
                    let Some(layer) = self.layer_manager.find_layer_mut(&feature.layer_name) else { continue; };
                    let value = if forward { after } else { before };
                    match value {
                        Some(v) => layer.set_tag(feature.kind, feature.id, key, v),
                        None => layer.remove_tag(feature.kind, feature.id, key),
                    }
                }
            }
        }
    }

    /// Snapshot each of `features`' tags from its owning layer, as
    /// `(FeatureRef, Vec<(String, String)>)` — the shape
    /// `compute_tag_edit_entries` expects.
    fn feature_tag_snapshots(
        &self,
        features: &[osm_gpui::selection::FeatureRef],
    ) -> Vec<(osm_gpui::selection::FeatureRef, Vec<(String, String)>)> {
        features
            .iter()
            .filter_map(|sel| {
                self.layer_manager
                    .find_layer(&sel.layer_name)
                    .and_then(|layer| layer.feature_tags(sel))
                    .map(|tags| (sel.clone(), tags))
            })
            .collect()
    }

    /// Snapshot every currently-selected feature's tags — see
    /// `feature_tag_snapshots`.
    fn selected_feature_tag_snapshots(&self) -> Vec<(osm_gpui::selection::FeatureRef, Vec<(String, String)>)> {
        self.feature_tag_snapshots(&self.selected)
    }

    /// If a row/button click recorded a pending tag-edit-dialog open
    /// request, construct the dialog now. Called from `Render::render` (see
    /// Step 6) — never call this, or construct `TagEditDialog` directly,
    /// from inside a click/action listener: `TagEditDialog`'s deferred
    /// select-all-on-open (Task 4) only lands correctly when the dialog is
    /// built during the same draw pass that first paints it, exactly like
    /// `check_for_dialog_queue` builds `CustomImageryDialog`.
    fn check_for_pending_tag_edit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_tag_edit_open.take() else { return };
        if self.tag_edit_dialog.is_some() {
            return; // one at a time; drop the request rather than queue it
        }
        let PendingTagEditOpen { features, original_key, original_value, select, is_add } = pending;
        let title = if is_add { "Add tag" } else { "Edit tag" };
        let dialog = cx.new(|cx| {
            osm_gpui::ui::tag_edit_dialog::TagEditDialog::new(
                window,
                cx,
                title,
                original_key.clone(),
                original_value.clone(),
                select,
            )
        });
        cx.subscribe(&dialog, |this, _entity, event, cx| {
            use osm_gpui::ui::tag_edit_dialog::DialogEvent;
            match event {
                DialogEvent::Cancelled => {
                    this.tag_edit_dialog = None;
                    cx.notify();
                }
                DialogEvent::Submitted { key, value } => {
                    // apply_tag_edit already clears tag_edit_dialog via take().
                    this.apply_tag_edit(key, value);
                    cx.notify();
                }
            }
        })
        .detach();
        self.tag_edit_dialog = Some((
            dialog,
            TagEditContext { features, original_key, original_value, is_add },
        ));
        cx.notify();
    }

    /// Apply a submitted tag-edit dialog result: compute the per-feature
    /// mutations via `compute_tag_edit_entries`, apply them immediately,
    /// and push one `UndoableAction::SetTags` (skipped entirely if there
    /// were no actual changes).
    fn apply_tag_edit(&mut self, key: &str, value: &str) {
        let Some((_, ctx)) = self.tag_edit_dialog.take() else { return };
        let snapshots = self.feature_tag_snapshots(&ctx.features);

        let entries = osm_gpui::selection::compute_tag_edit_entries(
            &snapshots,
            &ctx.original_key,
            &ctx.original_value,
            key,
            value,
            ctx.is_add,
        );
        if entries.is_empty() {
            return;
        }

        for (feature, k, _before, after) in &entries {
            let Some(layer) = self.layer_manager.find_layer_mut(&feature.layer_name) else { continue };
            match after {
                Some(v) => layer.set_tag(feature.kind, feature.id, k, v),
                None => layer.remove_tag(feature.kind, feature.id, k),
            }
        }
        self.undo_stack.push(UndoableAction::SetTags { entries });
    }

    /// Delete `key` from every currently-selected feature that has it,
    /// applying immediately and pushing one `UndoableAction::SetTags` (no
    /// dialog involved).
    fn delete_tag(&mut self, key: &str, cx: &mut Context<Self>) {
        let entries: Vec<_> = self
            .selected_feature_tag_snapshots()
            .into_iter()
            .filter_map(|(feature, tags)| {
                tags.into_iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| (feature, key.to_string(), Some(v), None))
            })
            .collect();
        if entries.is_empty() {
            return;
        }

        for (feature, k, _before, _after) in &entries {
            if let Some(layer) = self.layer_manager.find_layer_mut(&feature.layer_name) {
                layer.remove_tag(feature.kind, feature.id, k);
            }
        }
        self.undo_stack.push(UndoableAction::SetTags { entries });
        cx.notify();
    }

    fn on_undo(&mut self, _: &Undo, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(action) = self.undo_stack.undo() {
            self.apply_undo_action(&action, false);
            cx.notify();
        }
    }

    fn on_redo(&mut self, _: &Redo, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(action) = self.undo_stack.redo() {
            self.apply_undo_action(&action, true);
            cx.notify();
        }
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let adjusted_position = event.position;

        if self.move_drag.is_some() {
            if event.pressed_button == Some(gpui::MouseButton::Left) {
                if let Some(start) = self.mouse_down_pos {
                    let delta = adjusted_position - start;
                    let per_layer = self
                        .move_drag
                        .as_ref()
                        .map(|d| d.per_layer.clone())
                        .unwrap_or_default();
                    for (layer_name, originals) in &per_layer {
                        if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                            let ids: std::collections::HashSet<i64> =
                                originals.iter().map(|&(id, _, _)| id).collect();
                            layer.set_drag_preview(&ids, delta);
                        }
                    }
                    cx.notify();
                }
            }
            return;
        }

        if self.viewport.handle_mouse_move(adjusted_position) {
            cx.notify();
        }

        if event.pressed_button == Some(gpui::MouseButton::Left) {
            if let Some(start) = self.mouse_down_pos {
                let moved = (adjusted_position - start).magnitude() >= 4.0;
                if moved || self.box_select.is_some() {
                    self.box_select = Some((start, adjusted_position));
                    cx.notify();
                }
            }
        }
    }

    fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let up_pos = event.position;
        let down_pos = self.mouse_down_pos.take();
        self.viewport.handle_mouse_up();

        if let Some(drag) = self.move_drag.take() {
            let moved = match down_pos {
                Some(down) => (up_pos - down).magnitude() >= 4.0,
                None => false,
            };
            let delta = down_pos.map(|down| up_pos - down).unwrap_or_default();

            for (layer_name, _) in &drag.per_layer {
                if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                    layer.clear_drag_preview();
                }
            }

            if !moved {
                // Not actually a drag: treat as a plain click on the map.
                let before = self.selected.clone();
                self.handle_map_click(up_pos);
                if before != self.selected {
                    cx.notify();
                }
                return;
            }

            let mut undo_per_layer: NodeMoveUndoEntries = Vec::new();
            for (layer_name, originals) in &drag.per_layer {
                let mut moves: Vec<(i64, f64, f64)> = Vec::with_capacity(originals.len());
                let mut undo_entries: Vec<(i64, (f64, f64), (f64, f64))> = Vec::with_capacity(originals.len());
                for &(id, lat, lon) in originals {
                    let anchor = self.viewport.geo_to_screen(lat, lon);
                    let new_screen = anchor + delta;
                    let (new_lat, new_lon) = self.viewport.screen_to_geo(new_screen);
                    moves.push((id, new_lat, new_lon));
                    undo_entries.push((id, (lat, lon), (new_lat, new_lon)));
                }
                if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                    layer.commit_node_moves(&moves);
                }
                undo_per_layer.push((layer_name.clone(), undo_entries));
            }
            self.undo_stack.push(UndoableAction::MoveNodes { per_layer: undo_per_layer });
            cx.notify();
            return;
        }

        if let Some((start, _)) = self.box_select.take() {
            let rect = normalize_rect(start, up_pos);
            let before = self.selected.clone();
            self.selected = self.layer_manager.hit_test_rect_all(&self.viewport, rect);
            if before != self.selected {
                cx.notify();
            }
            return;
        }

        let was_click = match down_pos {
            Some(down) => (up_pos - down).magnitude() < 4.0,
            None => false,
        };
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
        self.selected = osm_gpui::selection::resolve_hits(per_layer)
            .into_iter()
            .collect();
    }

    fn sync_selection_to_layers(&mut self) {
        // Drop any selected feature whose owning layer is gone or hidden, so
        // the right panel never shows info for a feature not drawn on the map.
        let layer_manager = &self.layer_manager;
        self.selected.retain(|sel| {
            layer_manager
                .find_layer(&sel.layer_name)
                .map(|l| l.is_visible())
                .unwrap_or(false)
        });

        let selected = self.selected.clone();
        for layer in self.layer_manager.layers_mut() {
            let matching: Vec<osm_gpui::selection::FeatureRef> = selected
                .iter()
                .filter(|s| s.layer_name == layer.name())
                .cloned()
                .collect();
            layer.set_highlight(&matching);
        }
    }

    fn handle_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let scroll_delta = match event.delta {
            gpui::ScrollDelta::Lines(delta) => gpui::Point {
                x: px(delta.x),
                y: px(delta.y),
            },
            gpui::ScrollDelta::Pixels(delta) => gpui::Point {
                x: delta.x * 0.1,
                y: delta.y * 0.1,
            },
        };

        let adjusted_position = event.position;

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
                        LayerRequest::Delete { index } => {
                            let _ = self.layer_manager.remove_at(index);
                        }
                        LayerRequest::CoordinateGrid => {
                            if self.layer_manager.find_layer("Coordinate Grid").is_none() {
                                self.layer_manager.add_layer(Box::new(GridLayer::new()));
                            }
                        }
                        LayerRequest::Imagery { name, url_template, min_zoom, max_zoom } => {
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
                            )
                            .with_min_zoom(min_zoom)
                            .with_max_zoom(max_zoom);
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

    fn check_for_toggle_debug_overlay(&mut self, cx: &mut Context<Self>) {
        let Some(requests) = TOGGLE_DEBUG_OVERLAY.get() else { return };
        let pending = if let Ok(mut guard) = requests.try_lock() {
            let n = guard.len();
            guard.clear();
            n
        } else {
            0
        };
        if pending > 0 {
            // Parity of toggles: odd = flip, even = no-op
            if pending % 2 == 1 {
                self.show_debug_overlay = !self.show_debug_overlay;
            }
            cx.notify();
        }
    }

    fn check_for_dialog_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let should_open = if let Some(queue) = OPEN_CUSTOM_IMAGERY_DIALOG.get() {
            if let Ok(mut g) = queue.try_lock() {
                let had_requests = !g.is_empty();
                g.clear();
                had_requests && self.custom_imagery_dialog.is_none()
            } else {
                false
            }
        } else {
            false
        };

        if should_open {
            let dialog = cx.new(|cx| {
                osm_gpui::ui::custom_imagery_dialog::CustomImageryDialog::new(window, cx)
            });
            cx.subscribe(&dialog, |this, _entity, event: &osm_gpui::ui::custom_imagery_dialog::DialogEvent, cx| {
                use osm_gpui::ui::custom_imagery_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.custom_imagery_dialog = None;
                        cx.notify();
                    }
                    DialogEvent::Submitted(entry) => {
                        append_custom_imagery(entry.clone());
                        if let Some(requests) = LAYER_REQUESTS.get() {
                            if let Ok(mut q) = requests.lock() {
                                q.push(LayerRequest::Imagery {
                                    name: entry.name.clone(),
                                    url_template: entry.url_template.clone(),
                                    min_zoom: Some(entry.min_zoom),
                                    max_zoom: Some(entry.max_zoom),
                                });
                            }
                        }
                        this.custom_imagery_dialog = None;
                        this.last_menu_center = None;
                        cx.notify();
                    }
                }
            })
            .detach();
            self.custom_imagery_dialog = Some(dialog);
            cx.notify();
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

    const SELECTION_ROW_HEIGHT: f32 = 22.0;
    const SELECTION_MAX_VISIBLE_ROWS: usize = 10;

    /// The right pane: Layers, Selection, and Tags sections stacked
    /// top-to-bottom, each collapsible and sized to its content (the whole
    /// pane scrolls).
    fn render_side_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let layer_info: Vec<(String, bool, bool)> = self
            .layer_manager
            .layers()
            .iter()
            .map(|layer| (layer.name().to_string(), layer.is_visible(), layer.is_modified()))
            .collect();

        let layers_section = self.render_layers_section(&layer_info, cx);
        let selection_section = self.render_selection_section(cx);
        let tags_section = self.render_tags_section(cx);
        let history_section = self.render_history_section(cx);

        let open_layers = self.side_panel_open.contains(&0);
        let open_selection = self.side_panel_open.contains(&1);
        let open_tags = self.side_panel_open.contains(&2);
        let open_history = self.side_panel_open.contains(&3);

        let selection_title = match self.selected.len() {
            0 => "Selection".to_string(),
            1 => "Selection (1 item)".to_string(),
            n => format!("Selection ({} items)", n),
        };

        div()
            .w(px(280.0))
            .h_full()
            .bg(cx.theme().sidebar)
            .border_l_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .id("side-panel-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .child(self.collapsible_section("Layers", 0, open_layers, layers_section, cx))
                    .child(self.collapsible_section(
                        selection_title,
                        1,
                        open_selection,
                        selection_section,
                        cx,
                    ))
                    .child(self.collapsible_section("Tags", 2, open_tags, tags_section, cx))
                    .child(self.collapsible_section("History", 3, open_history, history_section, cx)),
            )
    }

    /// The History accordion section: a passive list of every undoable
    /// action in order. The most recently applied action (the stack's
    /// current position) is highlighted; anything after it is available to
    /// redo but not currently applied, and renders dimmed.
    fn render_history_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.undo_stack.actions.is_empty() {
            return Label::new("No actions yet.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let cursor = self.undo_stack.cursor;
        div()
            .flex()
            .flex_col()
            .children(self.undo_stack.actions.iter().enumerate().map(|(i, action)| {
                let is_current = i + 1 == cursor;
                let is_future = i >= cursor;
                let mut row = div()
                    .px_1()
                    .py_0p5()
                    .text_sm()
                    .child(action.description());
                if is_current {
                    row = row.bg(cx.theme().accent);
                } else if is_future {
                    row = row.text_color(cx.theme().muted_foreground).italic();
                }
                row
            }))
            .into_any_element()
    }

    /// A single collapsible section: a clickable header (chevron + title) that
    /// toggles `side_panel_open[index]`, with its content rendered below when
    /// open. Sizes to content so sections stack instead of splitting the height.
    fn collapsible_section(
        &self,
        title: impl Into<gpui::SharedString>,
        index: usize,
        open: bool,
        content: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let header = div()
            .id(("section-header", index))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .cursor_pointer()
            .border_b_1()
            .border_color(cx.theme().border)
            .hover(|this| this.bg(cx.theme().accent))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(cx.theme().muted_foreground),
            )
            .child(
                Label::new(title)
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD),
            )
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                if let Some(pos) = this.side_panel_open.iter().position(|&i| i == index) {
                    this.side_panel_open.remove(pos);
                } else {
                    this.side_panel_open.push(index);
                }
                cx.notify();
            }));

        div()
            .flex()
            .flex_col()
            .child(header)
            .when(open, |this| this.child(div().px_2().py_1p5().child(content)))
    }

    /// The Selection accordion section: a scrollable list of the selected
    /// features (max ~10 rows visible, then scrolls). Clicking a row narrows
    /// the selection to just that feature.
    fn render_selection_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use osm_gpui::selection::FeatureKind;

        if self.selected.is_empty() {
            return Label::new("Click or drag to select.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let visible_rows = self.selected.len().min(Self::SELECTION_MAX_VISIBLE_ROWS);
        let list_height = px(visible_rows as f32 * Self::SELECTION_ROW_HEIGHT);

        div()
            .id("selection-list")
            .flex()
            .flex_col()
            .h(list_height)
            .overflow_y_scroll()
            .children(self.selected.iter().enumerate().map(|(i, feat)| {
                let kind_label = match feat.kind {
                    FeatureKind::Node => "Node",
                    FeatureKind::Way => "Way",
                };
                let row_feat = feat.clone();
                div()
                    .id(("selection-row", i))
                    .flex_shrink_0()
                    .h(px(Self::SELECTION_ROW_HEIGHT))
                    .px_1()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|this| this.bg(cx.theme().accent))
                    .child(format!("{} {}", kind_label, feat.id))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                            this.selected = vec![row_feat.clone()];
                            cx.notify();
                        }),
                    )
            }))
            .into_any_element()
    }

    /// The Layers accordion section: a Checkbox row per layer with a right-click
    /// context menu offering Move up / Move down / Delete.
    fn render_layers_section(
        &self,
        layer_info: &[(String, bool, bool)],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let total = layer_info.len();
        if total == 0 {
            return Label::new("No layers yet. Add one from the menu.").into_any_element();
        }

        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                layer_info
                    .iter()
                    .enumerate()
                    .map(|(index, (name, is_visible, is_modified))| {
                        let layer_name = name.clone();
                        let label = if *is_modified {
                            format!("{} \u{2022}", name)
                        } else {
                            name.clone()
                        };
                        Checkbox::new(("layer", index))
                            .checked(*is_visible)
                            .label(label)
                            .on_click(cx.listener(move |this, _checked: &bool, _, cx| {
                                this.toggle_layer_visibility(&layer_name);
                                cx.notify();
                            }))
                            .context_menu(move |menu, _window, _cx| {
                                let mut menu = menu;
                                if index > 0 {
                                    menu = menu
                                        .menu("Move up", Box::new(MoveLayer { index, delta: -1 }));
                                }
                                if index + 1 < total {
                                    menu = menu
                                        .menu("Move down", Box::new(MoveLayer { index, delta: 1 }));
                                }
                                menu.separator()
                                    .menu("Delete", Box::new(DeleteLayer { index }))
                            })
                    })
                    .collect::<Vec<_>>(),
            )
            .into_any_element()
    }

    /// The Tags accordion section: tags aggregated across every selected
    /// feature. A key shows its value only if every selected feature has
    /// that exact same value (a feature missing the key counts as its own
    /// distinct state); otherwise it shows "<N values>". Double-
    /// clicking the key or value opens the tag-edit dialog with that field
    /// pre-selected; the trailing "x" removes the tag immediately. An "Add
    /// tag" button below the list opens the same dialog with empty fields.
    fn render_tags_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use osm_gpui::ui::tag_edit_dialog::TagEditField;

        if self.selected.is_empty() {
            return Label::new("No selection.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let per_feature: Vec<Vec<(String, String)>> = self
            .selected
            .iter()
            .filter_map(|sel| {
                self.layer_manager
                    .find_layer(&sel.layer_name)
                    .and_then(|layer| layer.feature_tags(sel))
            })
            .collect();

        let aggregated = osm_gpui::selection::aggregate_tags(&per_feature);
        let selection = self.selected.clone();

        let mut list = div().flex().flex_col();

        if aggregated.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("(no tags)"),
            );
        } else {
            list = list.children(aggregated.into_iter().map(|(k, v)| {
                let value_text = match v {
                    osm_gpui::selection::TagValue::Single(s) => s,
                    osm_gpui::selection::TagValue::Multiple(n) => format!("<{} values>", n),
                };

                let key_for_key_click = k.clone();
                let value_for_key_click = value_text.clone();
                let selection_for_key_click = selection.clone();

                let key_for_value_click = k.clone();
                let value_for_value_click = value_text.clone();
                let selection_for_value_click = selection.clone();

                let key_for_delete = k.clone();

                div()
                    .id(SharedString::from(format!("tag-row-{k}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .cursor_pointer()
                            .child(k.clone())
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                                    if ev.click_count == 2 {
                                        this.pending_tag_edit_open = Some(PendingTagEditOpen {
                                            features: selection_for_key_click.clone(),
                                            original_key: key_for_key_click.clone(),
                                            original_value: value_for_key_click.clone(),
                                            select: TagEditField::Key,
                                            is_add: false,
                                        });
                                        cx.notify();
                                    }
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .cursor_pointer()
                            .child(value_text.clone())
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                                    if ev.click_count == 2 {
                                        this.pending_tag_edit_open = Some(PendingTagEditOpen {
                                            features: selection_for_value_click.clone(),
                                            original_key: key_for_value_click.clone(),
                                            original_value: value_for_value_click.clone(),
                                            select: TagEditField::Value,
                                            is_add: false,
                                        });
                                        cx.notify();
                                    }
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("tag-delete-{k}")))
                            .cursor_pointer()
                            .text_color(cx.theme().muted_foreground)
                            .hover(|this| this.text_color(cx.theme().danger))
                            .child("x")
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                                    this.delete_tag(&key_for_delete, cx);
                                }),
                            ),
                    )
                    .into_any_element()
            }));
        }

        let add_selection = selection.clone();
        list.child(
            Button::new("add-tag")
                .label("Add tag")
                .primary()
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.pending_tag_edit_open = Some(PendingTagEditOpen {
                        features: add_selection.clone(),
                        original_key: String::new(),
                        original_value: String::new(),
                        select: TagEditField::None,
                        is_add: true,
                    });
                    cx.notify();
                })),
        )
        .into_any_element()
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
        self.check_for_toggle_debug_overlay(cx);
        self.check_for_dialog_queue(window, cx);
        self.check_for_pending_tag_edit_dialog(window, cx);
        self.maybe_rebuild_imagery_menu(cx);

        // Now it's safe to signal: the effects of this frame's commands
        // and pushes are visible.
        if let Some(bus) = SCRIPT_BUS.get() {
            bus.signal_done_and_frame();
        }

        // Update viewport size to actual window dimensions minus the right panel
        let window_size = window.bounds().size;
        let panel_width = px(280.0);
        let map_size = gpui::size(
            window_size.width - panel_width,
            window_size.height,
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

        div()
            .size_full()
            .bg(rgb(0x1a202c))
            .flex()
            .flex_row()
            .child(
                // Map area
                div()
                    .flex_1()
                    .relative()
                            .track_focus(&self.focus_handle)
                            // Right button drives panning.
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                                    this.handle_mouse_down(ev);
                                }),
                            )
                            .on_mouse_up(
                                gpui::MouseButton::Right,
                                cx.listener(|this, _ev: &MouseUpEvent, _, cx| {
                                    this.viewport.handle_mouse_up();
                                    cx.notify();
                                }),
                            )
                            .on_mouse_up_out(
                                gpui::MouseButton::Right,
                                cx.listener(|this, _ev: &MouseUpEvent, _, cx| {
                                    this.viewport.handle_mouse_up();
                                    cx.notify();
                                }),
                            )
                            // Left button: selection, box-select, or move-drag if the
                            // press lands on an already-selected feature.
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                                    window.focus(&this.focus_handle, cx);
                                    this.handle_map_mouse_down(ev.position);
                                }),
                            )
                            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                                if ev.keystroke.key == "escape" {
                                    this.cancel_move_drag(cx);
                                }
                            }))
                            .on_mouse_up(
                                gpui::MouseButton::Left,
                                cx.listener(|this, ev: &MouseUpEvent, _, cx| {
                                    this.handle_mouse_up(ev, cx);
                                }),
                            )
                            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                                this.handle_mouse_move(ev, cx);
                            }))
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
                                                    for sel in &selected {
                                                        layer_manager.render_highlight(sel, &viewport_clone, bounds, window);
                                                    }
                                                }
                                            }
                                        )
                                        .absolute()
                                        .size_full() // Ensure canvas fills the entire map area
                                    )
                            )
                            .child({
                                // Debug info overlay (toggleable via View menu)
                                if self.show_debug_overlay {
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
                                        .into_any_element()
                                } else {
                                    div().into_any_element()
                                }
                            })
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
                            })
                            .child({
                                if let Some((start, current)) = self.box_select {
                                    let rect = normalize_rect(start, current);
                                    div()
                                        .absolute()
                                        .left(rect.origin.x)
                                        .top(rect.origin.y)
                                        .w(rect.size.width)
                                        .h(rect.size.height)
                                        .bg(cx.theme().accent)
                                        .border_1()
                                        .border_color(cx.theme().accent)
                                        .opacity(0.35)
                                        .into_any_element()
                                } else {
                                    div().into_any_element()
                                }
                            }),
                    )
            .child(
                // Right panel with layer controls
                self.render_side_panel(cx)
            )
            .on_action(cx.listener(Self::on_move_layer))
            .on_action(cx.listener(Self::on_delete_layer))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .children(self.custom_imagery_dialog.clone())
            .children(self.tag_edit_dialog.as_ref().map(|(dialog, _)| dialog.clone()))
    }
}

// ---------------------------------------------------------------------------
// LiveApp: AppHandle impl backed by ScriptBus (background-thread safe)
// ---------------------------------------------------------------------------

struct LiveApp {
    _idle: Arc<IdleTracker>,
    bus: Arc<ScriptBus>,
    _window_id: u32,
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
    TOGGLE_DEBUG_OVERLAY.set(Arc::new(Mutex::new(Vec::new()))).unwrap();
    let _ = OPEN_CUSTOM_IMAGERY_DIALOG.set(Arc::new(Mutex::new(Vec::new())));
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
                _idle: idle.clone(),
                bus: bus_for_runner,
                _window_id: window_id,
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

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);

        // Bring the menu bar to the foreground
        cx.activate(true);

        // Register the open file action
        cx.on_action(open_osm_file);
        cx.on_action(quit);
        cx.on_action(add_osm_carto);
        cx.on_action(add_coordinate_grid);
        cx.on_action(download_from_osm);
        cx.on_action(toggle_debug_overlay);
        cx.on_action(add_imagery_layer);
        cx.on_action(add_saved_custom_imagery);
        cx.on_action(no_op_imagery_info);
        cx.on_action(open_custom_imagery_dialog);
        cx.on_action(open_settings);

        // Load persisted custom imagery entries.
        let loaded = custom_imagery_store::load();
        custom_imagery_store::init_store(loaded);

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

        let map_window = cx.open_window(
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
                    KeyBinding::new("cmd-,", OpenSettings, None),
                    KeyBinding::new("cmd-z", Undo, None),
                    KeyBinding::new("cmd-shift-z", Redo, None),
                ]);
                let view = cx.new(|cx| MapViewer::new(window, cx));
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        )
        .unwrap();

        let map_window_id = map_window.window_id();
        cx.on_window_closed(move |cx, window_id| {
            if window_id == map_window_id {
                cx.quit();
            }
        })
        .detach();
    });
}

fn custom_imagery_snapshot() -> Vec<CustomImageryEntry> {
    custom_imagery_store::snapshot()
}

fn append_custom_imagery(entry: CustomImageryEntry) {
    custom_imagery_store::append(entry);
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

fn open_settings(_: &OpenSettings, cx: &mut App) {
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
            let view = cx.new(|cx| osm_gpui::ui::settings_window::SettingsWindow::new(cx));
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
fn download_from_osm(_: &DownloadFromOsm, cx: &mut App) {
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
fn add_osm_carto(_: &AddOsmCarto, cx: &mut App) {
    if let Some(requests) = LAYER_REQUESTS.get() {
        if let Ok(mut queue) = requests.lock() {
            queue.push(LayerRequest::OsmCarto);
        }
    }
    cx.refresh_windows();
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
                min_zoom: entry.min_zoom,
                max_zoom: entry.max_zoom,
            });
        }
    }
}

// Handle the View > Toggle Debug Overlay menu action
fn toggle_debug_overlay(_: &ToggleDebugOverlay, cx: &mut App) {
    if let Some(requests) = TOGGLE_DEBUG_OVERLAY.get() {
        if let Ok(mut queue) = requests.lock() {
            queue.push(());
        }
    }
    cx.refresh_windows();
}

// Handle the Imagery > Add Custom Imagery… menu action
fn open_custom_imagery_dialog(_: &AddCustomImagery, cx: &mut App) {
    if let Some(queue) = OPEN_CUSTOM_IMAGERY_DIALOG.get() {
        if let Ok(mut g) = queue.lock() {
            g.push(());
        }
    }
    cx.refresh_windows();
}

// Handle the Imagery > Custom Imagery > <saved entry> menu action
fn add_saved_custom_imagery(action: &AddSavedCustomImagery, cx: &mut App) {
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
fn add_coordinate_grid(_: &AddCoordinateGrid, cx: &mut App) {
    if let Some(requests) = LAYER_REQUESTS.get() {
        if let Ok(mut queue) = requests.lock() {
            queue.push(LayerRequest::CoordinateGrid);
        }
    }
    cx.refresh_windows();
}

/// Build and install the menu bar, using the current viewport center to filter
/// the Imagery menu to relevant ELI entries.
fn rebuild_menus(cx: &mut App, center_lat: f64, center_lon: f64, state: ImageryLoadState) {
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

fn no_op_imagery_info(_: &NoOpImageryInfo, _cx: &mut App) {}
