use gpui::Action;
use gpui::{
    actions, canvas, div, fill, point, prelude::*, px, rgb, size, App, Bounds, Context, KeyBinding,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Render, ScrollWheelEvent,
    SharedString, Window, WindowOptions,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

mod fields_section;
mod menu;
mod mode_panel;
mod script_harness;
mod side_panel;
mod undo;

use crate::menu::{
    add_coordinate_grid, add_imagery_layer, add_osm_carto, add_saved_custom_imagery,
    download_from_osm, no_op_imagery_info, open_custom_imagery_dialog, open_osm_file,
    open_settings, rebuild_menus, toggle_debug_overlay, upload_to_osm,
};
use crate::script_harness::{LiveApp, ScriptBus, KEYSTROKE_QUEUE, SCRIPT_ACTIVE, SCRIPT_BUS};
use crate::undo::{NodeMoveUndoEntries, NodeMoveUndoEntry, UndoStack, UndoableAction};

use gpui_component::ActiveTheme;
use osm_gpui::auth;
use osm_gpui::custom_imagery_store::{self, CustomImageryEntry};
use osm_gpui::idle_tracker::IdleTracker;
use osm_gpui::imagery::{self, ImageryEntry};
use osm_gpui::interaction::{self, Interaction, NodeMoveTargets};
use osm_gpui::layers::{
    grid_layer::GridLayer, osm_layer::OsmLayer, tile_layer::TileLayer, LayerId, LayerManager,
};
use osm_gpui::osm::OsmData;
use osm_gpui::osm_api;
use osm_gpui::osm_upload;
use osm_gpui::script::{self, runner::Runner};
use osm_gpui::settings_store;
use osm_gpui::tile_cache::TileCache;
use osm_gpui::tiles;
use osm_gpui::viewport::Viewport;

actions!(
    osm_gpui,
    [
        OpenOsmFile,
        Quit,
        AddOsmCarto,
        AddCoordinateGrid,
        DownloadFromOsm,
        ToggleDebugOverlay,
        AddCustomImagery,
        OpenSettings,
        Undo,
        Redo,
        ApplyNsiPreset,
        ChangeFeatureType,
        UploadToOsm
    ]
);

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

/// Action to set the layer at `index` as the active (editable) layer.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = layers)]
#[serde(deny_unknown_fields)]
struct SetActiveLayer {
    index: usize,
}

/// The current map-interaction mode. `Select` is today's existing click/
/// drag/box-select behavior; the others place new geometry (see
/// docs/superpowers/specs/2026-07-07-mode-selector-design.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditMode {
    Select,
    Add,
    Building,
    Extrude,
}

/// Action to switch the current `EditMode`.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = mode)]
#[serde(deny_unknown_fields)]
struct SetMode {
    mode: EditModeAction,
}

/// `EditMode` isn't itself `Deserialize`/`JsonSchema` (gpui's `Action` derive
/// requires both on every field); this mirrors it 1:1 purely so `SetMode`
/// can carry a mode value through the action-dispatch system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
enum EditModeAction {
    Select,
    Add,
    Building,
    Extrude,
}

impl From<EditModeAction> for EditMode {
    fn from(a: EditModeAction) -> Self {
        match a {
            EditModeAction::Select => EditMode::Select,
            EditModeAction::Add => EditMode::Add,
            EditModeAction::Building => EditMode::Building,
            EditModeAction::Extrude => EditMode::Extrude,
        }
    }
}

/// Add mode's in-progress way-building state: the last-placed node, and
/// which way (if any) it belongs to. `None` on `MapViewer` means "no node
/// placed yet in this continuation" — the next click starts fresh.
struct AddProgress {
    way_id: Option<i64>,
    last_node_id: i64,
}

/// Building mode's in-progress rectangle: corner A is fixed after click 1;
/// corner B after click 2. Both are geo (lat, lon) coordinates.
#[derive(Clone, Copy)]
struct BuildingProgress {
    corner_a: (f64, f64),
    corner_b: Option<(f64, f64)>,
}

/// Extrude mode's in-progress drag: the way segment being extruded from,
/// and the two endpoint node ids used to build the preview/final rectangle.
struct ExtrudeDrag {
    layer: LayerId,
    way_id: i64,
    node_a: i64,
    node_b: i64,
    /// Screen position of the mouse-down that started this drag — needed
    /// for the click/drag threshold check on release, since (unlike Select
    /// mode's move-drag) this doesn't go through the shared `Interaction`
    /// state machine.
    down: gpui::Point<gpui::Pixels>,
}

/// Request to add a new layer, applied directly to the live `MapViewer` (via
/// `MapViewer::apply_layer_request`) by menu handlers and the custom-imagery
/// dialog's `Submitted` event.
#[derive(Debug, Clone)]
enum LayerRequest {
    OsmCarto,
    CoordinateGrid,
    Imagery {
        name: String,
        url_template: String,
        min_zoom: Option<u32>,
        max_zoom: Option<u32>,
        attribution: Option<imagery::AttributionInfo>,
    },
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

/// Weak handle to the live `MapViewer` view, set once when the main window is
/// created. This is the one bridge that lets free functions/global handlers
/// which only have `&mut App` (menu action handlers, and app-level window
/// callbacks like the `on_window_should_close` hook) reach the *real* view
/// and either query its live state or call ordinary `MapViewer` methods on
/// it directly — no polling queues involved.
///
/// Note: actions that fire via keybinding/menu while `MapViewer`'s own
/// window is dispatching (e.g. `Quit`) should NOT be routed through this
/// handle — the window is checked out of `App` for the duration of that
/// dispatch, so `with_map_viewer`/`with_map_viewer_in` silently no-op (see
/// `MapViewer::on_quit`). Prefer a window/entity-scoped `.on_action`
/// listener (`cx.listener(Self::on_x)`) registered on the render tree for
/// those; reserve this handle for callbacks that genuinely originate
/// outside that window's own dispatch (background tasks, other windows).
pub(crate) static MAP_VIEWER_HANDLE: OnceLock<gpui::WeakEntity<MapViewer>> = OnceLock::new();

/// Ask the live `MapViewer` (via `MAP_VIEWER_HANDLE`) whether any layer
/// currently has unsaved changes. This performs a fresh per-layer
/// `is_modified()` query against the real view every time it's called — no
/// value is cached or pre-aggregated across frames. Only safe to call from
/// contexts where `MapViewer`'s window isn't already checked out for
/// dispatch (e.g. the `on_window_should_close` hook, which is a separate
/// top-level callback, not nested inside `MapViewer`'s own dispatch) — see
/// the note on `MAP_VIEWER_HANDLE`.
pub(crate) fn has_unsaved_changes(cx: &App) -> bool {
    MAP_VIEWER_HANDLE
        .get()
        .and_then(|handle| handle.upgrade())
        .map(|view| {
            view.read(cx)
                .layer_manager
                .layers()
                .iter()
                .any(|l| l.is_modified())
        })
        .unwrap_or(false)
}

/// Run `f` against the live `MapViewer` (via `MAP_VIEWER_HANDLE`), if it
/// still exists. This is the standard way for menu action handlers (which
/// only have `&mut App`) to call an ordinary `MapViewer` method — replacing
/// the old push-into-a-queue-and-wait-for-render-to-drain-it pattern.
pub(crate) fn with_map_viewer(
    cx: &mut App,
    f: impl FnOnce(&mut MapViewer, &mut Context<MapViewer>),
) {
    if let Some(view) = MAP_VIEWER_HANDLE.get().and_then(|h| h.upgrade()) {
        view.update(cx, f);
    }
}

/// Like `with_map_viewer`, but for callers that need `&mut Window` too (e.g.
/// to construct a dialog entity), and which don't already have a `Window` in
/// scope — it looks the window up via the entity's window id.
pub(crate) fn with_map_viewer_in(
    cx: &mut App,
    f: impl FnOnce(&mut MapViewer, &mut Window, &mut Context<MapViewer>),
) {
    if let Some(handle) = MAP_VIEWER_HANDLE.get() {
        // `WeakEntity::update_in` (unlike `Entity::update_in`) only needs
        // `AppContext`, not `VisualContext` — it looks the window up by the
        // entity's window id via `App::with_window`, which is exactly what's
        // needed here since callers only have `&mut App`.
        let _ = handle.update_in(cx, f);
    }
}

// Global idle tracker shared with the script runner
static GLOBAL_IDLE: std::sync::OnceLock<Arc<IdleTracker>> = std::sync::OnceLock::new();

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
                out.script = Some(PathBuf::from(args.next().expect("--script needs a path")))
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
    /// Current mouse interaction: idle, a pending press, an in-progress
    /// box-select, or an in-progress move-drag of the selection. See
    /// `osm_gpui::interaction` for the pure click/drag/box-select decision
    /// tree this drives.
    interaction: Interaction,
    frame_times: VecDeque<Instant>,
    /// Last (lat, lon) the Imagery menu was rebuilt for. None forces a rebuild.
    last_menu_center: Option<(f64, f64)>,
    /// Imagery load state observed on the previous frame; detect transitions.
    last_imagery_load_state: Option<ImageryLoadState>,
    /// Whether the debug info overlay is currently visible.
    show_debug_overlay: bool,
    /// Active custom imagery dialog, if open.
    custom_imagery_dialog:
        Option<gpui::Entity<osm_gpui::ui::custom_imagery_dialog::CustomImageryDialog>>,
    /// Active "unsaved changes" quit-confirmation dialog, if open.
    quit_confirm_dialog: Option<gpui::Entity<osm_gpui::ui::quit_confirm_dialog::QuitConfirmDialog>>,
    /// Active upload-review dialog, if open.
    upload_dialog: Option<gpui::Entity<osm_gpui::ui::upload_dialog::UploadDialog>>,
    /// Active tag-edit dialog, if open, plus the context needed to apply
    /// its result.
    tag_edit_dialog: Option<(
        gpui::Entity<osm_gpui::ui::tag_edit_dialog::TagEditDialog>,
        TagEditContext,
    )>,
    /// A dialog-open request recorded by a row/button click, to be acted on
    /// during the next `render()` — see `PendingTagEditOpen`'s doc comment.
    pending_tag_edit_open: Option<PendingTagEditOpen>,
    /// Active NSI preset search dialog, if open.
    nsi_dialog: Option<gpui::Entity<osm_gpui::ui::nsi_dialog::NsiPresetDialog>>,
    /// Active "change feature type" preset picker dialog, if open.
    preset_picker_dialog:
        Option<gpui::Entity<osm_gpui::ui::preset_picker_dialog::PresetPickerDialog>>,
    /// Whether each side-panel accordion section (Layers, Selection, Fields,
    /// Tags, History, in that order) is expanded.
    side_panel_open: [bool; 5],
    /// Live `InputState` entities for the Fields section's text widgets,
    /// keyed by field id. Rebuilt whenever the selected feature changes so
    /// stale entities from a previous feature never leak into a new one.
    fields_text_inputs:
        std::collections::HashMap<String, gpui::Entity<gpui_component::input::InputState>>,
    /// Field ids that already have a `cx.subscribe` registered on their
    /// `fields_text_inputs` entity, so `text_field_input` never subscribes
    /// twice for the same field across re-renders. Cleared alongside
    /// `fields_text_inputs`.
    fields_text_subscribed: std::collections::HashSet<String>,
    /// Which field's combo/multiCombo option list is currently expanded,
    /// if any (`None` = all collapsed). Only one at a time.
    fields_open_combo: Option<String>,
    /// Field ids promoted from a preset's `more_fields` into the visible
    /// list for the current editing session. Cleared on selection change.
    fields_promoted_more_fields: std::collections::HashSet<String>,
    /// Focus handle for the map area, so it can receive key events (e.g.
    /// Escape to cancel an in-progress move-drag).
    focus_handle: gpui::FocusHandle,
    /// Global undo/redo history of committed data mutations.
    undo_stack: UndoStack,
    /// The current map-interaction mode (Select/Add/Building/Extrude).
    mode: EditMode,
    /// Id of the OSM layer that Add/Building/Extrude write into, or `None`
    /// if no layer is designated (those modes are disabled then).
    active_layer: Option<LayerId>,
    /// In-progress Add-mode way-building state, or `None` between
    /// continuations (see `AddProgress`).
    add_progress: Option<AddProgress>,
    /// In-progress Building-mode rectangle state, or `None` between
    /// continuations (see `BuildingProgress`).
    building_progress: Option<BuildingProgress>,
    /// Last known mouse position within the map area, updated on every
    /// mouse-move; used to drive the live Building-mode preview (which
    /// needs a cursor position outside of a drag) during `render()`.
    last_mouse_pos: Option<gpui::Point<gpui::Pixels>>,
    /// Extrude mode's in-progress drag, or `None` when not dragging (see
    /// `ExtrudeDrag`).
    extrude_drag: Option<ExtrudeDrag>,
}

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

/// Convert a gpui screen point to the plain `(f32, f32)` pair the pure
/// `interaction` module operates on.
fn to_pt(p: gpui::Point<gpui::Pixels>) -> interaction::Pt {
    (p.x.as_f32(), p.y.as_f32())
}

/// Convert a plain `(f32, f32)` pair back to a gpui screen point (or
/// vector/delta — `gpui::Point<Pixels>` doubles as both).
fn from_pt(p: interaction::Pt) -> gpui::Point<gpui::Pixels> {
    gpui::point(px(p.0), px(p.1))
}

/// Normalize two arbitrary screen points into a `Bounds` with a top-left
/// origin and non-negative size, regardless of drag direction. Thin gpui
/// wrapper around `interaction::normalize_rect`.
fn normalize_rect(
    a: gpui::Point<gpui::Pixels>,
    b: gpui::Point<gpui::Pixels>,
) -> gpui::Bounds<gpui::Pixels> {
    let rect = interaction::normalize_rect(to_pt(a), to_pt(b));
    gpui::Bounds {
        origin: gpui::point(px(rect.x), px(rect.y)),
        size: gpui::size(px(rect.width), px(rect.height)),
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
            interaction: Interaction::Idle,
            frame_times: VecDeque::with_capacity(120),
            last_menu_center: None,
            last_imagery_load_state: None,
            show_debug_overlay: false,
            custom_imagery_dialog: None,
            quit_confirm_dialog: None,
            upload_dialog: None,
            tag_edit_dialog: None,
            pending_tag_edit_open: None,
            nsi_dialog: None,
            preset_picker_dialog: None,
            side_panel_open: [true, true, true, true, true],
            fields_text_inputs: std::collections::HashMap::new(),
            fields_text_subscribed: std::collections::HashSet::new(),
            fields_open_combo: None,
            fields_promoted_more_fields: std::collections::HashSet::new(),
            focus_handle: cx.focus_handle(),
            undo_stack: UndoStack::default(),
            mode: EditMode::Select,
            active_layer: None,
            add_progress: None,
            building_progress: None,
            last_mouse_pos: None,
            extrude_drag: None,
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
            let screen_width = self.viewport.transform.screen_size.width.to_f64();
            let screen_height = self.viewport.transform.screen_size.height.to_f64();
            let (center_lat, center_lon, zoom_level) =
                osm_gpui::coordinates::fit_bounds_to_viewport(
                    min_lat,
                    max_lat,
                    min_lon,
                    max_lon,
                    screen_width,
                    screen_height,
                );

            self.viewport.pan_to(center_lat, center_lon);
            self.viewport.set_zoom(zoom_level);
        }
    }

    fn toggle_layer_visibility(&mut self, layer_id: LayerId) {
        if let Some(layer) = self.layer_manager.find_layer_mut(layer_id) {
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

    /// Handle the `DeleteLayer` context-menu action. This handler already has
    /// `&mut Context<Self>`, so it mutates `layer_manager` directly rather
    /// than going through `LayerRequest`.
    fn on_delete_layer(&mut self, action: &DeleteLayer, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self
            .layer_manager
            .layers()
            .get(action.index)
            .map(|l| l.id())
        {
            if self.active_layer == Some(id) {
                self.active_layer = None;
            }
        }
        let _ = self.layer_manager.remove_at(action.index);
        cx.notify();
    }

    /// Handle the `SetActiveLayer` context-menu action: makes the OSM layer
    /// at `index` the active (editable) layer, gating the Add/Building/
    /// Extrude mode buttons on it.
    fn on_set_active_layer(
        &mut self,
        action: &SetActiveLayer,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self
            .layer_manager
            .layers()
            .get(action.index)
            .filter(|l| l.as_editable().is_some())
            .map(|l| l.id())
        {
            self.active_layer = Some(id);
            cx.notify();
        }
    }

    /// Handle the `SetMode` action: switch modes, discarding any
    /// in-progress add/building/extrude state without committing it (Tasks
    /// 6-8 populate these fields; they don't exist until then, so this
    /// handler is added here as a no-op placeholder for those clears and
    /// extended in each later task).
    fn on_set_mode(&mut self, action: &SetMode, _: &mut Window, cx: &mut Context<Self>) {
        self.mode = action.mode.into();
        self.add_progress = None;
        self.building_progress = None;
        self.extrude_drag = None;
        cx.notify();
    }

    /// Convert a raw window-space mouse position (as delivered by every
    /// gpui mouse event) into map-area-local coordinates, i.e. relative to
    /// the map div's own top-left corner. Needed because the map area is no
    /// longer flush against the window's left edge — the mode-selector
    /// toolbar (`render_mode_panel`) sits before it in the flex row,
    /// offsetting the map's actual on-screen origin by
    /// `Self::MODE_PANEL_WIDTH`. `Viewport`'s hit-testing/projection math
    /// assumes (0,0) is the map area's own top-left, matching how rendering
    /// already adds back `bounds.origin` (the map div's real window
    /// position) when painting — this is the input-side counterpart of
    /// that.
    fn window_to_map(&self, position: gpui::Point<gpui::Pixels>) -> gpui::Point<gpui::Pixels> {
        gpui::point(position.x - px(Self::MODE_PANEL_WIDTH), position.y)
    }

    fn handle_mouse_down(&mut self, event: &MouseDownEvent) {
        let adjusted_position = self.window_to_map(event.position);

        self.viewport.handle_mouse_down(adjusted_position);
        self.interaction =
            interaction::record_mouse_down(&self.interaction, to_pt(adjusted_position));
    }

    /// Left-button mouse-down: if the point hits a currently-selected
    /// feature, start a move-drag instead of the usual box-select/click
    /// tracking. Always records the mouse-down position either way, since
    /// both paths need it to distinguish a click from a drag on release.
    /// `position` must already be in map-local coordinates (see
    /// `window_to_map`) — callers pass the raw event position through
    /// `window_to_map` first.
    fn handle_map_mouse_down(&mut self, position: gpui::Point<gpui::Pixels>) {
        if self.mode == EditMode::Extrude {
            if let Some(layer_id) = self.active_layer {
                if let Some(layer) = self.layer_manager.find_layer(layer_id) {
                    if let Some(osm_layer) = layer.as_any().downcast_ref::<OsmLayer>() {
                        if let Some((way_id, node_a, node_b, _idx)) =
                            osm_layer.hit_test_segment(&self.viewport, position, 6.0)
                        {
                            self.extrude_drag = Some(ExtrudeDrag {
                                layer: layer_id,
                                way_id,
                                node_a,
                                node_b,
                                down: position,
                            });
                        }
                    }
                }
            }
            return;
        }

        let hit_move_targets = if self.selected.is_empty()
            || self
                .layer_manager
                .hit_test_selection(&self.viewport, position, &self.selected)
                .is_none()
        {
            None
        } else {
            let per_layer = self.resolve_move_targets();
            if per_layer.is_empty() {
                None
            } else {
                Some(per_layer)
            }
        };
        self.interaction = interaction::on_left_mouse_down(to_pt(position), hit_move_targets);
    }

    /// Resolve the current selection into, per owning layer, the set of node
    /// ids to translate: a selected node contributes its own id; a selected
    /// way contributes every one of its member node ids. Each id's current
    /// (lat, lon) is snapshotted for use as the drag's translation anchor.
    fn resolve_move_targets(&self) -> NodeMoveTargets {
        use osm_gpui::selection::FeatureKind;
        use std::collections::{HashMap, HashSet};

        let mut ids_by_layer: HashMap<LayerId, HashSet<i64>> = HashMap::new();
        for feat in &self.selected {
            let Some(layer) = self.layer_manager.find_layer(feat.layer_id) else {
                continue;
            };
            let Some(editable) = layer.as_editable() else {
                continue;
            };
            let entry = ids_by_layer.entry(feat.layer_id).or_default();
            match feat.kind {
                FeatureKind::Node => {
                    entry.insert(feat.id);
                }
                FeatureKind::Way => {
                    if let Some(node_ids) = editable.way_node_ids(feat.id) {
                        entry.extend(node_ids);
                    }
                }
            }
        }

        ids_by_layer
            .into_iter()
            .filter_map(|(layer_id, ids)| {
                let layer = self.layer_manager.find_layer(layer_id)?;
                let editable = layer.as_editable()?;
                let originals: Vec<(i64, f64, f64)> = ids
                    .into_iter()
                    .filter_map(|id| editable.node_lat_lon(id).map(|(lat, lon)| (id, lat, lon)))
                    .collect();
                if originals.is_empty() {
                    None
                } else {
                    Some((layer_id, originals))
                }
            })
            .collect()
    }

    /// Cancel an in-progress move-drag: clears the preview on every affected
    /// layer without mutating any data.
    fn cancel_move_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(targets) = interaction::cancel_move_drag(&mut self.interaction) {
            for (layer_id, _) in &targets {
                if let Some(layer) = self.layer_manager.find_layer_mut(*layer_id) {
                    if let Some(editable) = layer.as_editable_mut() {
                        editable.clear_drag_preview();
                    }
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
                for (layer_id, entries) in per_layer {
                    let moves: Vec<(i64, f64, f64)> = entries
                        .iter()
                        .map(|&(id, before, after)| {
                            let (lat, lon) = if forward { after } else { before };
                            (id, lat, lon)
                        })
                        .collect();
                    if let Some(layer) = self.layer_manager.find_layer_mut(*layer_id) {
                        if let Some(editable) = layer.as_editable_mut() {
                            editable.commit_node_moves(&moves);
                        }
                    }
                }
            }
            UndoableAction::SetTags { entries } => {
                for (feature, key, before, after) in entries {
                    let Some(layer) = self.layer_manager.find_layer_mut(feature.layer_id) else {
                        continue;
                    };
                    let Some(editable) = layer.as_editable_mut() else {
                        continue;
                    };
                    let value = if forward { after } else { before };
                    match value {
                        Some(v) => editable.set_tag(feature.kind, feature.id, key, v),
                        None => editable.remove_tag(feature.kind, feature.id, key),
                    }
                }
            }
            UndoableAction::CreateNode {
                layer,
                id,
                lat,
                lon,
            } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                if forward {
                    // Redo: recreate the node at the exact same id, so any
                    // later action referencing this id (e.g. a subsequent
                    // tag edit) still targets the right feature.
                    editable.create_node(*lat, *lon, Some(*id));
                } else {
                    editable.delete_feature(osm_gpui::selection::FeatureKind::Node, *id);
                }
            }
            UndoableAction::DeleteFeature { layer, snapshot } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                if forward {
                    editable.delete_feature(snapshot.kind, snapshot.id);
                } else {
                    editable.restore_feature(snapshot.clone());
                }
            }
            UndoableAction::ExtendWay {
                layer,
                way_id,
                node_id,
                way_created,
                node_created,
            } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                if !forward {
                    // Detach `node_id` from the way: if this click created
                    // the way, the whole way goes away (no need to also
                    // remove the node from a way that's being deleted);
                    // otherwise just pull it out of the existing way's node
                    // list.
                    if *way_created {
                        editable.remove_way(*way_id);
                    } else {
                        let node_ids = editable.way_node_ids(*way_id).unwrap_or_default();
                        if let Some(idx) = node_ids.iter().rposition(|id| id == node_id) {
                            editable.remove_node_from_way(*way_id, idx);
                        }
                    }
                    // Only delete the node itself if this click created it —
                    // a pre-existing node the user connected to may be
                    // shared with other ways or carry its own tags, so undo
                    // must leave it alone.
                    if *node_created {
                        editable.remove_node(*node_id);
                    }
                }
                // Redo (forward) is intentionally a no-op, matching
                // `CreateBuilding`'s documented scope boundary: undo deletes
                // the node this click created (and the way too, if this
                // click created it), so a straightforward "recreate" redo
                // would need to hand back the exact same placeholder ids —
                // but `way_id`'s id would come from a fresh `add_way` call,
                // not the one recorded here, breaking any later undo entry
                // that still references the original `way_id` (e.g. a
                // subsequent click extending the same way). Redo beyond the
                // immediate action is out of scope for this plan (see the
                // spec's "Out of scope" section).
            }
            UndoableAction::CreateBuilding {
                layer,
                way_id,
                node_ids,
            } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                if !forward {
                    editable.remove_way(*way_id);
                    for id in node_ids {
                        editable.remove_node(*id);
                    }
                }
                // Redo (forward) is out of scope for Building mode's atomic
                // commit path in this plan: Building mode always creates a
                // *new* placeholder id on each commit, so a straightforward
                // redo-by-recreation isn't id-stable across a redo after
                // other edits. Matches this plan's scope (see spec's "Out
                // of scope": undo/redo depth beyond the immediate action).
            }
            UndoableAction::ExtrudeWay {
                layer,
                way_id,
                new_node_ids,
            } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                if !forward {
                    editable.remove_way(*way_id);
                    for id in new_node_ids {
                        editable.remove_node(*id);
                    }
                }
                // Redo (forward) is intentionally a no-op, same as
                // `ExtendWay`/`CreateBuilding`: undo deletes the way and its
                // new nodes, but a redo-by-recreation would allocate fresh
                // placeholder ids rather than reproducing `way_id`/
                // `new_node_ids`, breaking any later undo entry that still
                // references them. Out of scope for this plan.
            }
            UndoableAction::InsertNodeIntoWay {
                layer,
                way_id,
                index,
                node_id,
                ..
            } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                if !forward {
                    editable.remove_node_from_way(*way_id, *index);
                    editable.remove_node(*node_id);
                }
                // Redo (forward) is intentionally a no-op, same reasoning as
                // `ExtendWay`/`CreateBuilding`/`ExtrudeWay`: recreating the
                // node via a fresh `add_node` call wouldn't reproduce the
                // original `node_id`, breaking any later undo entry that
                // still references it. Out of scope for this plan.
            }
            UndoableAction::SnapExtendWay {
                layer,
                way_id,
                way_created,
                snap_way_id,
                snap_index,
                node_id,
            } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                if !forward {
                    // Detach from the drawn way first (deleting it if this
                    // click created it), without deleting the node — it's
                    // still referenced by `snap_way_id` until the next step.
                    if *way_created {
                        editable.remove_way(*way_id);
                    } else {
                        let node_ids = editable.way_node_ids(*way_id).unwrap_or_default();
                        if let Some(idx) = node_ids.iter().rposition(|id| id == node_id) {
                            editable.remove_node_from_way(*way_id, idx);
                        }
                    }
                    // Then remove it from the way it was snapped onto, and
                    // delete the node itself.
                    editable.remove_node_from_way(*snap_way_id, *snap_index);
                    editable.remove_node(*node_id);
                }
                // Redo (forward) is intentionally a no-op, same reasoning as
                // `ExtendWay`/`InsertNodeIntoWay` above.
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
                    .find_layer(sel.layer_id)
                    .and_then(|layer| layer.as_editable())
                    .and_then(|editable| editable.feature_tags(sel))
                    .map(|tags| (*sel, tags))
            })
            .collect()
    }

    /// Snapshot every currently-selected feature's tags — see
    /// `feature_tag_snapshots`.
    fn selected_feature_tag_snapshots(
        &self,
    ) -> Vec<(osm_gpui::selection::FeatureRef, Vec<(String, String)>)> {
        self.feature_tag_snapshots(&self.selected)
    }

    /// If a row/button click recorded a pending tag-edit-dialog open
    /// request, construct the dialog now. Called from `Render::render` (see
    /// Step 6) — never call this, or construct `TagEditDialog` directly,
    /// from inside a click/action listener: `TagEditDialog`'s deferred
    /// select-all-on-open (Task 4) only lands correctly when the dialog is
    /// built during the same draw pass that first paints it. This is the one
    /// dialog with that constraint — `CustomImageryDialog` and
    /// `QuitConfirmDialog` have no such requirement and are built directly
    /// from `MapViewer::open_custom_imagery_dialog` /
    /// `show_quit_confirm_dialog` outside of `render()`.
    fn check_for_pending_tag_edit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_tag_edit_open.take() else {
            return;
        };
        if self.tag_edit_dialog.is_some() {
            return; // one at a time; drop the request rather than queue it
        }
        let PendingTagEditOpen {
            features,
            original_key,
            original_value,
            select,
            is_add,
        } = pending;
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
            TagEditContext {
                features,
                original_key,
                original_value,
                is_add,
            },
        ));
        cx.notify();
    }

    /// Apply a submitted tag-edit dialog result: compute the per-feature
    /// mutations via `compute_tag_edit_entries`, apply them immediately,
    /// and push one `UndoableAction::SetTags` (skipped entirely if there
    /// were no actual changes).
    fn apply_tag_edit(&mut self, key: &str, value: &str) {
        let Some((_, ctx)) = self.tag_edit_dialog.take() else {
            return;
        };
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
            let Some(layer) = self.layer_manager.find_layer_mut(feature.layer_id) else {
                continue;
            };
            let Some(editable) = layer.as_editable_mut() else {
                continue;
            };
            match after {
                Some(v) => editable.set_tag(feature.kind, feature.id, k, v),
                None => editable.remove_tag(feature.kind, feature.id, k),
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
            if let Some(layer) = self.layer_manager.find_layer_mut(feature.layer_id) {
                if let Some(editable) = layer.as_editable_mut() {
                    editable.remove_tag(feature.kind, feature.id, k);
                }
            }
        }
        self.undo_stack.push(UndoableAction::SetTags { entries });
        cx.notify();
    }

    /// Delete every currently-selected feature (Delete/Backspace key).
    /// Node deletion is refused per-feature if the node is still referenced
    /// by a way — see `OsmLayer::delete_feature`'s doc comment for that v1
    /// limitation (a future version might offer to also delete the way, or
    /// warn specifically, like JOSM/iD). Refused/not-found features are
    /// simply skipped rather than surfacing a per-feature error; one
    /// `DeleteFeature` undo action is pushed per feature actually deleted,
    /// so undo restores them one at a time (in reverse order).
    fn delete_selected_features(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            return;
        }
        let features = std::mem::take(&mut self.selected);
        let mut deleted = 0usize;
        for feature in &features {
            let Some(layer) = self.layer_manager.find_layer_mut(feature.layer_id) else {
                continue;
            };
            let Some(editable) = layer.as_editable_mut() else {
                continue;
            };
            if let Some(snapshot) = editable.delete_feature(feature.kind, feature.id) {
                self.undo_stack.push(UndoableAction::DeleteFeature {
                    layer: feature.layer_id,
                    snapshot,
                });
                deleted += 1;
            }
        }
        if deleted == 0 {
            self.set_status("Nothing to delete");
        } else if deleted == 1 {
            self.set_status("Deleted 1 feature");
        } else {
            self.set_status(format!("Deleted {} features", deleted));
        }
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

    /// Handle the `ApplyNsiPreset` menu action / keybinding. Only opens the
    /// dialog when exactly one feature is selected — GPUI has no built-in
    /// disabled-menu-item support (see `no_op_imagery_info`), so this is a
    /// no-op otherwise rather than a disabled menu entry.
    fn on_apply_nsi_preset(
        &mut self,
        _: &ApplyNsiPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.len() != 1 || self.nsi_dialog.is_some() {
            return;
        }
        let target = self.selected[0];

        let dialog = cx.new(|cx| osm_gpui::ui::nsi_dialog::NsiPresetDialog::new(window, cx));
        cx.subscribe(
            &dialog,
            move |this: &mut Self, _entity, event: &osm_gpui::ui::nsi_dialog::DialogEvent, cx| {
                use osm_gpui::ui::nsi_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.nsi_dialog = None;
                        cx.notify();
                    }
                    DialogEvent::Submitted(preset_tags) => {
                        this.apply_nsi_preset(&target, preset_tags.clone());
                        this.nsi_dialog = None;
                        cx.notify();
                    }
                }
            },
        )
        .detach();
        self.nsi_dialog = Some(dialog);
        cx.notify();
    }

    /// Handle the `ChangeFeatureType` action: opens the preset picker dialog
    /// for the single selected feature, letting the user deliberately
    /// override whatever `PresetIndex::match_feature` auto-matched. Mirrors
    /// `on_apply_nsi_preset` exactly, but resolves the feature's `Geometry`
    /// first (the picker filters results by it) and reuses the same
    /// `apply_nsi_preset` tag-mutation function on submit.
    fn on_change_feature_type(
        &mut self,
        _: &ChangeFeatureType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.len() != 1 || self.preset_picker_dialog.is_some() {
            return;
        }
        let target = self.selected[0];

        let Some(layer) = self.layer_manager.find_layer(target.layer_id) else {
            return;
        };
        let Some(editable) = layer.as_editable() else {
            return;
        };
        let Some(geometry) = editable.feature_geometry(&target, osm_gpui::presets::area_keys())
        else {
            return;
        };

        let dialog = cx.new(|cx| {
            osm_gpui::ui::preset_picker_dialog::PresetPickerDialog::new(geometry, window, cx)
        });
        cx.subscribe(
            &dialog,
            move |this: &mut Self,
                  _entity,
                  event: &osm_gpui::ui::preset_picker_dialog::DialogEvent,
                  cx| {
                use osm_gpui::ui::preset_picker_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.preset_picker_dialog = None;
                        cx.notify();
                    }
                    DialogEvent::Submitted(preset_tags) => {
                        this.apply_nsi_preset(&target, preset_tags.clone());
                        this.preset_picker_dialog = None;
                        cx.notify();
                    }
                }
            },
        )
        .detach();
        self.preset_picker_dialog = Some(dialog);
        cx.notify();
    }

    /// Apply `preset_tags` to `target`: for each preset key whose value
    /// differs from what the feature already has, set it via `set_tag` and
    /// record a `(feature, key, before, after)` entry; push one
    /// `UndoableAction::SetTags` covering every changed key (skipped
    /// entirely if the preset didn't actually change anything). Existing
    /// tags the preset doesn't mention (e.g. `addr:*`) are left untouched —
    /// this only ever adds/overwrites keys, never removes any.
    fn apply_nsi_preset(
        &mut self,
        target: &osm_gpui::selection::FeatureRef,
        preset_tags: std::collections::HashMap<String, String>,
    ) {
        let Some(layer) = self.layer_manager.find_layer(target.layer_id) else {
            return;
        };
        let Some(editable) = layer.as_editable() else {
            return;
        };
        let Some(existing) = editable.feature_tags(target) else {
            return;
        };

        let mut keys: Vec<&String> = preset_tags.keys().collect();
        keys.sort();

        let entries: Vec<_> = keys
            .into_iter()
            .filter_map(|key| {
                let after = preset_tags.get(key).cloned();
                let before = existing
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone());
                if before == after {
                    return None;
                }
                Some((*target, key.clone(), before, after))
            })
            .collect();
        if entries.is_empty() {
            return;
        }

        for (feature, key, _before, after) in &entries {
            if let Some(layer) = self.layer_manager.find_layer_mut(feature.layer_id) {
                if let Some(editable) = layer.as_editable_mut() {
                    if let Some(v) = after {
                        editable.set_tag(feature.kind, feature.id, key, v);
                    }
                }
            }
        }
        self.undo_stack.push(UndoableAction::SetTags { entries });
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let adjusted_position = self.window_to_map(event.position);
        let left_pressed = event.pressed_button == Some(gpui::MouseButton::Left);
        self.last_mouse_pos = Some(adjusted_position);
        if self.building_progress.is_some()
            || self.extrude_drag.is_some()
            || self.add_progress.is_some()
        {
            cx.notify(); // repaint the live preview every move while building/extruding/adding
        }

        if let Interaction::MoveDrag { down, targets } = &self.interaction {
            if let Some(delta_pt) =
                interaction::move_drag_delta(*down, to_pt(adjusted_position), left_pressed)
            {
                let delta = from_pt(delta_pt);
                let targets = targets.clone();
                for (layer_id, originals) in &targets {
                    if let Some(layer) = self.layer_manager.find_layer_mut(*layer_id) {
                        if let Some(editable) = layer.as_editable_mut() {
                            let ids: std::collections::HashSet<i64> =
                                originals.iter().map(|&(id, _, _)| id).collect();
                            editable.set_drag_preview(&ids, delta);
                        }
                    }
                }
                cx.notify();
            }
            return;
        }

        if self.viewport.handle_mouse_move(adjusted_position) {
            cx.notify();
        }

        if interaction::update_box_select(
            &mut self.interaction,
            to_pt(adjusted_position),
            left_pressed,
        ) {
            cx.notify();
        }
    }

    fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let up_pos = self.window_to_map(event.position);
        self.viewport.handle_mouse_up();

        if let Some(drag) = self.extrude_drag.take() {
            let moved = (up_pos - drag.down).magnitude() >= 4.0;
            if moved {
                self.commit_extrude(&drag, up_pos);
            } else if event.click_count == 2 {
                self.insert_node_on_segment(&drag, up_pos);
            }
            cx.notify();
            return;
        }

        match interaction::on_mouse_up(&mut self.interaction, to_pt(up_pos)) {
            interaction::Gesture::MoveCommitted { targets, delta } => {
                let delta = from_pt(delta);
                for (layer_id, _) in &targets {
                    if let Some(layer) = self.layer_manager.find_layer_mut(*layer_id) {
                        if let Some(editable) = layer.as_editable_mut() {
                            editable.clear_drag_preview();
                        }
                    }
                }

                let mut undo_per_layer: NodeMoveUndoEntries = Vec::new();
                for (layer_id, originals) in &targets {
                    let mut moves: Vec<(i64, f64, f64)> = Vec::with_capacity(originals.len());
                    let mut undo_entries: Vec<NodeMoveUndoEntry> =
                        Vec::with_capacity(originals.len());
                    for &(id, lat, lon) in originals {
                        let anchor = self.viewport.geo_to_screen(lat, lon);
                        let new_screen = anchor + delta;
                        let (new_lat, new_lon) = self.viewport.screen_to_geo(new_screen);
                        moves.push((id, new_lat, new_lon));
                        undo_entries.push((id, (lat, lon), (new_lat, new_lon)));
                    }
                    if let Some(layer) = self.layer_manager.find_layer_mut(*layer_id) {
                        if let Some(editable) = layer.as_editable_mut() {
                            editable.commit_node_moves(&moves);
                        }
                    }
                    undo_per_layer.push((*layer_id, undo_entries));
                }
                self.undo_stack.push(UndoableAction::MoveNodes {
                    per_layer: undo_per_layer,
                });
                cx.notify();
            }
            interaction::Gesture::MoveCancelledAsClick { targets, at } => {
                for (layer_id, _) in &targets {
                    if let Some(layer) = self.layer_manager.find_layer_mut(*layer_id) {
                        if let Some(editable) = layer.as_editable_mut() {
                            editable.clear_drag_preview();
                        }
                    }
                }
                self.handle_map_click(
                    from_pt(at),
                    event.modifiers.shift,
                    event.click_count,
                    event.modifiers.control,
                );
                // Always notify, regardless of whether the selection changed:
                // `self.interaction` just transitioned back to `Idle` and the
                // drag-preview clear above needs a repaint to actually reach
                // the screen, or both would appear to "stick" until some
                // unrelated redraw (e.g. a pan) happened to pick it up.
                cx.notify();
            }
            interaction::Gesture::BoxSelected { rect } => {
                let rect = normalize_rect(from_pt(rect.0), from_pt(rect.1));
                self.selected = self.layer_manager.hit_test_rect_all(&self.viewport, rect);
                self.fields_text_inputs.clear();
                self.fields_text_subscribed.clear();
                self.fields_open_combo = None;
                self.fields_promoted_more_fields.clear();
                // Always notify: the box-select overlay is driven off
                // `self.interaction`, which just transitioned back to `Idle`.
                // If the box hit nothing, `self.selected` wouldn't otherwise
                // change and the stale rectangle would stay on screen until
                // some unrelated redraw happened to pick up the new state.
                cx.notify();
            }
            interaction::Gesture::Click { at } => {
                let before = self.selected.clone();
                self.handle_map_click(
                    from_pt(at),
                    event.modifiers.shift,
                    event.click_count,
                    event.modifiers.control,
                );
                // Add mode re-selects the same way on every extend click (the
                // way id doesn't change), so the before/after diff alone
                // misses that the way's geometry grew a node — always
                // notify for Add mode regardless of the selection diff.
                if before != self.selected || self.mode == EditMode::Add {
                    cx.notify();
                }
            }
            interaction::Gesture::None => {}
        }
    }

    /// Dispatch a plain map click by the current `EditMode`.
    fn handle_map_click(
        &mut self,
        screen_pt: gpui::Point<gpui::Pixels>,
        shift_held: bool,
        click_count: usize,
        ctrl_held: bool,
    ) {
        match self.mode {
            EditMode::Select => self.handle_select_click(screen_pt, shift_held, click_count),
            EditMode::Add => self.handle_add_click(screen_pt, ctrl_held),
            EditMode::Building => self.handle_building_click(screen_pt),
            EditMode::Extrude => {
                // Extrude doesn't use the plain-click path (Task 8 hooks
                // mouse-down/mouse-move/mouse-up directly); a stray click
                // here (e.g. a zero-movement mouse-up while extruding) is a
                // no-op.
            }
        }
    }

    /// Building mode: click 1 sets corner A, click 2 sets corner B (fixing
    /// the first edge), click 3 commits the rectangle. See
    /// docs/superpowers/specs/2026-07-07-mode-selector-design.md "Building mode".
    fn handle_building_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>) {
        let Some(layer_id) = self.active_layer else {
            return;
        };
        let (lat, lon) = self.viewport.screen_to_geo(screen_pt);

        match self.building_progress.take() {
            None => {
                self.building_progress = Some(BuildingProgress {
                    corner_a: (lat, lon),
                    corner_b: None,
                });
            }
            Some(BuildingProgress {
                corner_a,
                corner_b: None,
            }) => {
                self.building_progress = Some(BuildingProgress {
                    corner_a,
                    corner_b: Some((lat, lon)),
                });
            }
            Some(BuildingProgress {
                corner_a,
                corner_b: Some(corner_b),
            }) => {
                self.commit_building(layer_id, corner_a, corner_b, (lat, lon));
                self.building_progress = None;
            }
        }
    }

    /// Compute the final rectangle (corner_a, corner_b as one edge, offset
    /// by `cursor`'s perpendicular distance) and commit 4 new nodes + a
    /// closed `building=yes` way as one undo action.
    fn commit_building(
        &mut self,
        layer_id: LayerId,
        corner_a: (f64, f64),
        corner_b: (f64, f64),
        cursor: (f64, f64),
    ) {
        let (far_a, far_b) = osm_gpui::selection::rectangle_from_edge(corner_a, corner_b, cursor);
        let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
            return;
        };
        let Some(editable) = layer.as_editable_mut() else {
            return;
        };

        let n0 = editable.add_node(corner_a.0, corner_a.1);
        let n1 = editable.add_node(corner_b.0, corner_b.1);
        let n2 = editable.add_node(far_b.0, far_b.1);
        let n3 = editable.add_node(far_a.0, far_a.1);
        let way_id = editable.add_way(
            vec![n0, n1, n2, n3, n0],
            vec![("building".to_string(), "yes".to_string())],
        );

        self.undo_stack.push(UndoableAction::CreateBuilding {
            layer: layer_id,
            way_id,
            node_ids: [n0, n1, n2, n3],
        });
        self.selected = vec![osm_gpui::selection::FeatureRef {
            layer_id,
            kind: osm_gpui::selection::FeatureKind::Way,
            id: way_id,
        }];
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }

    /// Commit an Extrude drag: compute the far 2 corners via
    /// `rectangle_from_edge` (using `up_pos` for the perpendicular offset),
    /// create 2 new nodes + a closed `building=yes` way, push one
    /// `ExtrudeWay` undo action.
    fn commit_extrude(&mut self, drag: &ExtrudeDrag, up_pos: gpui::Point<gpui::Pixels>) {
        let Some(layer) = self.layer_manager.find_layer(drag.layer) else {
            return;
        };
        let Some(editable) = layer.as_editable() else {
            return;
        };
        let Some(a_geo) = editable.node_lat_lon(drag.node_a) else {
            return;
        };
        let Some(b_geo) = editable.node_lat_lon(drag.node_b) else {
            return;
        };
        let cursor_geo = self.viewport.screen_to_geo(up_pos);

        let (far_a, far_b) = osm_gpui::selection::rectangle_from_edge(a_geo, b_geo, cursor_geo);
        let Some(layer) = self.layer_manager.find_layer_mut(drag.layer) else {
            return;
        };
        let Some(editable) = layer.as_editable_mut() else {
            return;
        };
        let new_a = editable.add_node(far_a.0, far_a.1);
        let new_b = editable.add_node(far_b.0, far_b.1);
        let way_id = editable.add_way(
            vec![drag.node_a, drag.node_b, new_b, new_a, drag.node_a],
            vec![("building".to_string(), "yes".to_string())],
        );

        self.undo_stack.push(UndoableAction::ExtrudeWay {
            layer: drag.layer,
            way_id,
            new_node_ids: [new_a, new_b],
        });
        self.selected = vec![osm_gpui::selection::FeatureRef {
            layer_id: drag.layer,
            kind: osm_gpui::selection::FeatureKind::Way,
            id: way_id,
        }];
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }

    /// Double-click on a segment (no drag): insert a new node at the
    /// double-click position, splitting that segment.
    fn insert_node_on_segment(&mut self, drag: &ExtrudeDrag, up_pos: gpui::Point<gpui::Pixels>) {
        let (lat, lon) = self.viewport.screen_to_geo(up_pos);
        let Some(layer) = self.layer_manager.find_layer_mut(drag.layer) else {
            return;
        };
        let Some(editable) = layer.as_editable_mut() else {
            return;
        };
        // The segment's start index within the way's node list: `node_a`'s
        // position (the segment is node_a -> node_b, consecutive).
        let Some(node_ids) = editable.way_node_ids(drag.way_id) else {
            return;
        };
        let Some(idx_a) = node_ids.iter().position(|&id| id == drag.node_a) else {
            return;
        };
        let insert_index = idx_a + 1;

        let new_id = editable.insert_node_into_way(drag.way_id, insert_index, lat, lon);
        self.undo_stack.push(UndoableAction::InsertNodeIntoWay {
            layer: drag.layer,
            way_id: drag.way_id,
            index: insert_index,
            node_id: new_id,
        });
    }

    /// Resolve a plain click into a selection change. `shift_held` toggles
    /// the hit feature in/out of the existing selection (add if absent,
    /// remove if already selected) instead of replacing it; a shift-click
    /// that hits nothing is a no-op, leaving the existing selection intact.
    fn handle_select_click(
        &mut self,
        screen_pt: gpui::Point<gpui::Pixels>,
        shift_held: bool,
        click_count: usize,
    ) {
        let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
        let mut hit = osm_gpui::selection::resolve_hits(per_layer);
        // Double-click inside a closed way's interior (away from its
        // outline, which a plain click already selects) selects the way.
        if hit.is_none() && click_count == 2 {
            let interior = self
                .layer_manager
                .hit_test_interior_all(&self.viewport, screen_pt);
            hit = osm_gpui::selection::resolve_hits(interior);
        }
        self.selected = osm_gpui::selection::apply_click_selection(&self.selected, hit, shift_held);
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }

    /// Add mode: place a node, or extend/connect the in-progress way. See
    /// docs/superpowers/specs/2026-07-07-mode-selector-design.md "Add mode"
    /// and docs/superpowers/specs/2026-07-08-add-mode-snap-to-way-design.md
    /// for the snap-to-way behavior. `ctrl_held` bypasses both snapping onto
    /// a nearby existing node and snapping onto a nearby way's line
    /// geometry, always producing a fully independent node.
    fn handle_add_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>, ctrl_held: bool) {
        let Some(layer_id) = self.active_layer else {
            return;
        };
        let (lat, lon) = self.viewport.screen_to_geo(screen_pt);

        // Clicking an existing node/way finishes the in-progress way by
        // connecting to it. Skipped entirely when Ctrl is held.
        if !ctrl_held && self.add_progress.is_some() {
            let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
            if let Some(hit) = osm_gpui::selection::resolve_hits(per_layer) {
                if hit.layer_id == layer_id {
                    if let osm_gpui::selection::FeatureKind::Node = hit.kind {
                        let way_id = self.add_extend_or_start_way(layer_id, hit.id, false, None);
                        self.add_progress = None;
                        self.selected = vec![osm_gpui::selection::FeatureRef {
                            layer_id,
                            kind: osm_gpui::selection::FeatureKind::Way,
                            id: way_id,
                        }];
                        self.fields_text_inputs.clear();
                        self.fields_text_subscribed.clear();
                        self.fields_open_combo = None;
                        self.fields_promoted_more_fields.clear();
                        return;
                    }
                }
            }
        }

        // Try to snap onto a nearby way's line geometry, unless Ctrl is
        // held. `snap` carries the snapped-onto way's id and splice index,
        // if any, threaded through to `add_extend_or_start_way` for the
        // 2nd+ click case.
        let snap_hit = if ctrl_held {
            None
        } else {
            self.layer_manager
                .find_layer(layer_id)
                .and_then(|layer| layer.as_any().downcast_ref::<OsmLayer>())
                .and_then(|osm_layer| osm_layer.snap_to_way(&self.viewport, screen_pt, 6.0))
        };

        // Note: `find_layer_mut` is re-called in each arm below (rather than
        // binding `layer` once above the match) so its mutable borrow ends
        // before the arm needs `&mut self` again for `self.add_progress`/
        // `self.add_extend_or_start_way`/`self.undo_stack` — binding it once
        // outside the match would keep the borrow alive across those calls
        // and fail to compile.
        match self.add_progress.take() {
            None => {
                // First click of a fresh continuation: a lone node (or, if
                // snapped, a node spliced into the snapped-onto way), no way
                // of its own yet — the next click always starts a *new* way
                // from this node, whether or not this one landed on top of
                // an existing way.
                let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                let new_id = match snap_hit {
                    Some((way_id, _, _, idx, snap_lat, snap_lon)) => {
                        let new_id =
                            editable.insert_node_into_way(way_id, idx + 1, snap_lat, snap_lon);
                        self.undo_stack.push(UndoableAction::InsertNodeIntoWay {
                            layer: layer_id,
                            way_id,
                            index: idx + 1,
                            node_id: new_id,
                        });
                        new_id
                    }
                    None => {
                        // Reuses the pre-existing `CreateNode` undo action
                        // (same one the retired Cmd+Click gesture used to
                        // use) — this is the same underlying mutation, just
                        // triggered by Add mode instead.
                        let new_id = editable.add_node(lat, lon);
                        self.undo_stack.push(UndoableAction::CreateNode {
                            layer: layer_id,
                            id: new_id,
                            lat,
                            lon,
                        });
                        new_id
                    }
                };
                self.add_progress = Some(AddProgress {
                    way_id: None,
                    last_node_id: new_id,
                });
                self.selected = vec![osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Node,
                    id: new_id,
                }];
            }
            Some(progress) => {
                // 2nd+ click: create the node (or snap it onto a way) and
                // fold it into the way being drawn in one step.
                // `add_extend_or_start_way` pushes the matching undo entry
                // that covers both the node creation and the way
                // mutation(s) (one click = one undo step).
                let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                let (new_id, snap) = match snap_hit {
                    Some((way_id, _, _, idx, snap_lat, snap_lon)) => (
                        editable.insert_node_into_way(way_id, idx + 1, snap_lat, snap_lon),
                        Some((way_id, idx + 1)),
                    ),
                    None => (editable.add_node(lat, lon), None),
                };
                self.add_progress = Some(progress);
                let way_id = self.add_extend_or_start_way(layer_id, new_id, true, snap);
                self.add_progress = Some(AddProgress {
                    way_id: Some(way_id),
                    last_node_id: new_id,
                });
                self.selected = vec![osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Way,
                    id: way_id,
                }];
            }
        }
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }

    /// Shared by the "continue clicking" and "connect to existing feature"
    /// paths: start a new 2-node way if none exists yet, or extend the
    /// existing one, pushing the matching undo entry. Returns the way id
    /// (new or existing). `node_created` must reflect whether `node_id` was
    /// just created by this click (vs. an existing node the user clicked to
    /// connect) — it's recorded on the undo entry so undo never deletes a
    /// node it didn't create. `snap`, when `Some((snap_way_id,
    /// snap_index))`, means `node_id` was just spliced into `snap_way_id` at
    /// `snap_index` by the caller (via `snap_to_way`/`insert_node_into_way`)
    /// — this click is a compound mutation, so it pushes `SnapExtendWay`
    /// instead of `ExtendWay` to undo both steps together.
    fn add_extend_or_start_way(
        &mut self,
        layer_id: LayerId,
        node_id: i64,
        node_created: bool,
        snap: Option<(i64, usize)>,
    ) -> i64 {
        let progress_way_id = self.add_progress.as_ref().and_then(|p| p.way_id);
        let last_node_id = self
            .add_progress
            .as_ref()
            .map(|p| p.last_node_id)
            .unwrap_or(node_id);
        let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
            return progress_way_id.unwrap_or(0);
        };
        let Some(editable) = layer.as_editable_mut() else {
            return progress_way_id.unwrap_or(0);
        };

        let (way_id, way_created) = match progress_way_id {
            Some(way_id) => {
                editable.extend_way(way_id, node_id);
                (way_id, false)
            }
            None => {
                let way_id = editable.add_way(vec![last_node_id, node_id], Vec::new());
                (way_id, true)
            }
        };

        match snap {
            Some((snap_way_id, snap_index)) => {
                self.undo_stack.push(UndoableAction::SnapExtendWay {
                    layer: layer_id,
                    way_id,
                    way_created,
                    snap_way_id,
                    snap_index,
                    node_id,
                });
            }
            None => {
                self.undo_stack.push(UndoableAction::ExtendWay {
                    layer: layer_id,
                    way_id,
                    node_id,
                    way_created,
                    node_created,
                });
            }
        }
        way_id
    }

    fn sync_selection_to_layers(&mut self) {
        // Drop any selected feature whose owning layer is gone or hidden, so
        // the right panel never shows info for a feature not drawn on the map.
        let layer_manager = &self.layer_manager;
        self.selected.retain(|sel| {
            layer_manager
                .find_layer(sel.layer_id)
                .map(|l| l.is_visible())
                .unwrap_or(false)
        });

        let selected = self.selected.clone();
        for layer in self.layer_manager.layers_mut() {
            let layer_id = layer.id();
            let Some(editable) = layer.as_editable_mut() else {
                continue;
            };
            let matching: Vec<osm_gpui::selection::FeatureRef> = selected
                .iter()
                .filter(|s| s.layer_id == layer_id)
                .cloned()
                .collect();
            editable.set_highlight(&matching);
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

        let adjusted_position = self.window_to_map(event.position);

        if self.viewport.handle_scroll(adjusted_position, scroll_delta) {
            cx.notify();
        }
    }

    /// Add a newly-loaded OSM dataset as a new layer. Called directly by
    /// `menu::open_osm_file` (once its background parse completes) and by
    /// the script harness's `ScriptCommand::LoadOsm` — replaces the old
    /// `SHARED_OSM_DATA` queue drained once per frame.
    pub(crate) fn add_osm_dataset(&mut self, name: String, data: OsmData, cx: &mut Context<Self>) {
        let file_name = if name.is_empty() {
            "OSM".to_string()
        } else {
            name
        };
        let candidate = self.layer_manager.unique_name(&file_name);
        let data_arc = Arc::new(data.clone());
        let layer_id = self.layer_manager.alloc_id();
        let layer = OsmLayer::new_with_data(layer_id, candidate, data_arc);
        self.layer_manager.add_layer(Box::new(layer));
        if self.active_layer.is_none() {
            self.active_layer = Some(layer_id);
        }
        if !self.first_dataset_fitted {
            self.fit_to_osm_data(&data);
            self.first_dataset_fitted = true;
        }
        self.status_message = None;
        cx.notify();
    }

    /// Apply a `LayerRequest`, adding the corresponding layer. Called
    /// directly by menu handlers (via `with_map_viewer`) and by the custom
    /// imagery dialog's `Submitted` event — replaces the old `LAYER_REQUESTS`
    /// queue drained once per frame.
    fn apply_layer_request(&mut self, req: LayerRequest, cx: &mut Context<Self>) {
        match req {
            LayerRequest::OsmCarto => {
                if self
                    .layer_manager
                    .layer_named("OpenStreetMap Carto")
                    .is_none()
                {
                    let layer_id = self.layer_manager.alloc_id();
                    let tile_layer = TileLayer::new(layer_id, self.tile_cache.clone());
                    self.layer_manager.add_layer(Box::new(tile_layer));
                }
            }
            LayerRequest::CoordinateGrid => {
                if self.layer_manager.layer_named("Coordinate Grid").is_none() {
                    let layer_id = self.layer_manager.alloc_id();
                    self.layer_manager
                        .add_layer(Box::new(GridLayer::new(layer_id)));
                }
            }
            LayerRequest::Imagery {
                name,
                url_template,
                min_zoom,
                max_zoom,
                attribution,
            } => {
                let candidate = self.layer_manager.unique_name(&name);
                let layer_id = self.layer_manager.alloc_id();
                let layer = TileLayer::new_with_template(
                    layer_id,
                    candidate,
                    url_template,
                    self.tile_cache.clone(),
                )
                .with_min_zoom(min_zoom)
                .with_max_zoom(max_zoom)
                .with_attribution(attribution);
                self.layer_manager.add_layer(Box::new(layer));
            }
        }
        cx.notify();
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
        let tile_zoom = zoom_level.round().clamp(0.0, 18.0) as u32;
        let bounds_geo = self.viewport.visible_bounds();
        let visible_tiles = tiles::get_tiles_for_bounds(
            bounds_geo.min_lat,
            bounds_geo.min_lon,
            bounds_geo.max_lat,
            bounds_geo.max_lon,
            tile_zoom,
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

    /// Flip the debug overlay. Called directly from the `View > Toggle Debug
    /// Overlay` menu handler (via `with_map_viewer`) — replaces the old
    /// `TOGGLE_DEBUG_OVERLAY` queue (which used push-count parity to emulate
    /// a single flip per click; a direct call needs no such trick).
    pub(crate) fn toggle_debug_overlay(&mut self, cx: &mut Context<Self>) {
        self.show_debug_overlay = !self.show_debug_overlay;
        cx.notify();
    }

    /// Open the "Add Custom Imagery…" dialog, if one isn't already open.
    /// Called directly from the menu handler (via `with_map_viewer_in`) —
    /// replaces the old `OPEN_CUSTOM_IMAGERY_DIALOG` queue. Unlike the
    /// tag-edit dialog, this dialog has no post-paint focus requirement, so
    /// it's safe to construct straight from the menu-triggered `update_in`
    /// call rather than deferring to the next render pass.
    pub(crate) fn open_custom_imagery_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.custom_imagery_dialog.is_some() {
            return;
        }
        let dialog =
            cx.new(|cx| osm_gpui::ui::custom_imagery_dialog::CustomImageryDialog::new(window, cx));
        cx.subscribe(
            &dialog,
            |this, _entity, event: &osm_gpui::ui::custom_imagery_dialog::DialogEvent, cx| {
                use osm_gpui::ui::custom_imagery_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.custom_imagery_dialog = None;
                        cx.notify();
                    }
                    DialogEvent::Submitted(entry) => {
                        append_custom_imagery(entry.clone());
                        this.apply_layer_request(
                            LayerRequest::Imagery {
                                name: entry.name.clone(),
                                url_template: entry.url_template.clone(),
                                min_zoom: Some(entry.min_zoom),
                                max_zoom: Some(entry.max_zoom),
                                attribution: None,
                            },
                            cx,
                        );
                        this.custom_imagery_dialog = None;
                        this.last_menu_center = None;
                        cx.notify();
                    }
                }
            },
        )
        .detach();
        self.custom_imagery_dialog = Some(dialog);
        cx.notify();
    }

    /// Handle the `Quit` action (Cmd+Q / File > Quit menu item). Registered
    /// as a window/entity-scoped `.on_action` listener (like `on_undo`,
    /// `on_move_layer`, etc.) rather than dispatched through the old
    /// `menu::quit` free function + `MAP_VIEWER_HANDLE` lookup: GPUI
    /// dispatches actions to a window while that window is "checked out" of
    /// `App` for the duration of the dispatch (it's how `Window::dispatch_*`
    /// gets exclusive access), so any attempt to re-acquire it via
    /// `with_map_viewer_in`'s `WeakEntity::update_in` (which looks the
    /// window up again by id) silently fails and is swallowed — which is
    /// exactly why Cmd+Q appeared to do nothing whenever there were unsaved
    /// changes (the no-changes path took a window-free `cx.quit()` and
    /// happened to work). A listener registered directly on the render tree
    /// gets `&mut Window` handed to it by the in-progress dispatch, so no
    /// re-acquisition is needed.
    fn on_quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.layer_manager.layers().iter().any(|l| l.is_modified()) {
            self.show_quit_confirm_dialog(window, cx);
        } else {
            cx.quit();
        }
    }

    /// Open the "unsaved changes" quit-confirmation dialog, if one isn't
    /// already open. Called directly by `on_quit` and by the
    /// `on_window_should_close` hook (which already has a `Window` in
    /// scope) — replaces the old `SHOW_QUIT_CONFIRM` queue. Like
    /// `open_custom_imagery_dialog`, this dialog has no post-paint focus
    /// requirement, so it's safe to construct directly rather than
    /// deferring to the next render pass.
    pub(crate) fn show_quit_confirm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_confirm_dialog.is_some() {
            return;
        }
        let dialog =
            cx.new(|cx| osm_gpui::ui::quit_confirm_dialog::QuitConfirmDialog::new(window, cx));
        cx.subscribe(
            &dialog,
            |this, _entity, event: &osm_gpui::ui::quit_confirm_dialog::DialogEvent, cx| {
                use osm_gpui::ui::quit_confirm_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.quit_confirm_dialog = None;
                        cx.notify();
                    }
                    DialogEvent::ConfirmQuit => {
                        this.quit_confirm_dialog = None;
                        cx.quit();
                    }
                }
            },
        )
        .detach();
        self.quit_confirm_dialog = Some(dialog);
        cx.notify();
    }

    /// Open the upload-review dialog, if one isn't already open — either
    /// showing the "nothing to upload" status or the dialog itself,
    /// depending on whether any layer has changes. Called directly by
    /// `menu::upload_to_osm` (via `with_map_viewer_in`) — replaces the old
    /// `SHOW_UPLOAD_DIALOG` queue, same as `show_quit_confirm_dialog`
    /// replaced `SHOW_QUIT_CONFIRM`.
    pub(crate) fn open_upload_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.upload_dialog.is_some() {
            return;
        }

        let summaries: Vec<osm_gpui::ui::upload_dialog::LayerSummary> = self
            .layer_manager
            .layers()
            .iter()
            .filter(|l| l.is_modified())
            .map(|l| {
                let (created, modified, deleted) = l.diff_for_upload().counts();
                osm_gpui::ui::upload_dialog::LayerSummary {
                    layer_name: l.name().to_string(),
                    created,
                    modified,
                    deleted,
                }
            })
            .collect();

        if summaries.is_empty() || summaries.iter().all(|s| s.is_empty()) {
            self.set_status("Nothing to upload");
            cx.notify();
            return;
        }

        let dialog =
            cx.new(|cx| osm_gpui::ui::upload_dialog::UploadDialog::new(window, cx, summaries));
        cx.subscribe(
            &dialog,
            |this, _entity, event: &osm_gpui::ui::upload_dialog::DialogEvent, cx| {
                use osm_gpui::ui::upload_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.upload_dialog = None;
                        cx.notify();
                    }
                    DialogEvent::Upload { comment } => {
                        this.upload_dialog = None;
                        this.start_upload(comment.clone(), cx);
                    }
                }
            },
        )
        .detach();
        self.upload_dialog = Some(dialog);
        cx.notify();
    }

    /// Run the full upload sequence (create changeset -> build+upload
    /// osmChange -> close changeset -> reconcile local layers) on the
    /// background executor, mirroring `request_download`'s `cx.spawn`/
    /// `background_executor().spawn` structure.
    ///
    /// Known v1 limitation: if the changeset is created but the upload
    /// itself fails, we do NOT attempt automatic rollback/retry — the
    /// changeset is left open on the server (harmless; it auto-closes after
    /// a period of inactivity, or the user can close it manually) and the
    /// status message includes its id so it isn't silently lost track of.
    fn start_upload(&mut self, comment: String, cx: &mut Context<Self>) {
        // Layers are looked up by `LayerId`, not name (names aren't unique
        // identity — see `LayerId`'s doc comment), so both the id and the
        // display name are captured up front: the id for `find_layer`/
        // `find_layer_mut`, the name only for the outgoing changeset XML.
        let modified: Vec<(LayerId, String)> = self
            .layer_manager
            .layers()
            .iter()
            .filter(|l| l.is_modified())
            .map(|l| (l.id(), l.name().to_string()))
            .collect();
        let diffs: Vec<osm_gpui::layers::diff::LayerDiff> = modified
            .iter()
            .filter_map(|(id, _)| {
                self.layer_manager
                    .find_layer(*id)
                    .map(|l| l.diff_for_upload())
            })
            .collect();

        if diffs.iter().all(|d| d.is_empty()) {
            self.set_status("Nothing to upload");
            cx.notify();
            return;
        }

        self.set_status("Uploading…");
        cx.notify();

        let base_url = settings_store::api_base_url();
        let oauth_base = auth::oauth_base_for(&base_url);
        let names_for_bg: Vec<String> = modified.iter().map(|(_, name)| name.clone()).collect();
        let layer_ids: Vec<LayerId> = modified.iter().map(|(id, _)| *id).collect();

        cx.spawn(async move |this, cx| {
            let result: Result<(u64, osm_upload::UploadResult), String> = cx
                .background_executor()
                .spawn(async move {
                    let token = auth::ensure_fresh_token(&oauth_base).map_err(|e| e.to_string())?;
                    let changeset_id =
                        osm_upload::create_changeset(&base_url, &token.access_token, &comment)
                            .map_err(|e| e.to_string())?;

                    let layers_for_xml: Vec<(&str, osm_gpui::layers::diff::LayerDiff)> = names_for_bg
                        .iter()
                        .map(|s| s.as_str())
                        .zip(diffs)
                        .collect();
                    let xml = osm_upload::build_osm_change_xml(changeset_id, &layers_for_xml);

                    match osm_upload::upload_changes(&base_url, &token.access_token, changeset_id, &xml) {
                        Ok(upload_result) => {
                            // Best-effort close: a failure here doesn't
                            // undo the (already-applied) upload, so it
                            // isn't treated as a fatal error — the
                            // changeset just stays open, which OSM handles
                            // fine (manual or automatic close later).
                            let _ = osm_upload::close_changeset(&base_url, &token.access_token, changeset_id);
                            Ok((changeset_id, upload_result))
                        }
                        Err(e) => Err(format!(
                            "{} (changeset {} was opened but not uploaded — you may want to close it manually)",
                            e, changeset_id
                        )),
                    }
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((changeset_id, upload_result)) => {
                        for id in &layer_ids {
                            if let Some(layer) = this.layer_manager.find_layer_mut(*id) {
                                layer.apply_upload_result(&upload_result);
                            }
                        }
                        this.set_status(format!("Uploaded changes (changeset {})", changeset_id));
                    }
                    Err(e) => {
                        this.set_status(e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Kick off a background fetch of the OSM data within the current
    /// viewport bounds, applying the result to `self` on completion. Called
    /// directly from the `File > Download from OSM` menu handler (via
    /// `with_map_viewer`) — replaces the old `DOWNLOAD_REQUESTS` queue (the
    /// queue only ever carried a trigger; the actual background-fetch and
    /// apply-on-completion logic already used `cx.spawn` and is unchanged).
    pub(crate) fn request_download(&mut self, cx: &mut Context<Self>) {
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

        let base_url = settings_store::api_base_url();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // ensure_fresh_token does a network refresh if the stored token is
                    // expired, so run it here on the background thread rather than the
                    // UI thread. Any error (not logged in, refresh failed) falls back
                    // to an anonymous request, same as when there's no stored login.
                    let token = auth::ensure_fresh_token(&auth::oauth_base_for(&base_url))
                        .ok()
                        .map(|t| t.access_token);
                    osm_api::fetch_bbox(bounds, &base_url, token.as_deref())
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(data) => {
                        let data_arc = Arc::new(data);
                        let candidate = this.layer_manager.unique_name(&label);
                        let layer_id = this.layer_manager.alloc_id();
                        let layer = OsmLayer::new_with_data(layer_id, candidate, data_arc);
                        this.layer_manager.add_layer(Box::new(layer));
                        if this.active_layer.is_none() {
                            this.active_layer = Some(layer_id);
                        }
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
}

impl Render for MapViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Consume any pending script command first.
        self.process_script_command(window, cx);

        // The tag-edit dialog is the one remaining case that must be opened
        // from inside a render pass (its deferred select-all-on-open depends
        // on being constructed after paint — see its doc comment). Every
        // other former polling queue (new OSM data, layer requests, download
        // requests, debug-overlay toggle, the two other dialogs) is now
        // applied directly to `self` by menu handlers / background-task
        // completions via `MAP_VIEWER_HANDLE`, so there's nothing left to
        // drain here.
        self.check_for_pending_tag_edit_dialog(window, cx);
        self.maybe_rebuild_imagery_menu(cx);

        // Now it's safe to signal: the effects of this frame's commands
        // and pushes are visible.
        if let Some(bus) = SCRIPT_BUS.get() {
            bus.signal_done_and_frame();
        }

        // Update viewport size to actual window dimensions minus the right panel
        let window_size = window.bounds().size;
        let panel_width = px(osm_gpui::ui::style::SIDE_PANEL_WIDTH);
        let left_panel_width = px(Self::MODE_PANEL_WIDTH);
        let map_size = gpui::size(
            window_size.width - panel_width - left_panel_width,
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
            .bg(cx.theme().background)
            .text_size(osm_gpui::ui::style::current_text_scale().body)
            .flex()
            .flex_row()
            .child(self.render_mode_panel(cx))
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
                            let position = this.window_to_map(ev.position);
                            this.handle_map_mouse_down(position);
                        }),
                    )
                    .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                        if ev.keystroke.key == "escape" {
                            this.cancel_move_drag(cx);
                            if this.mode == EditMode::Add {
                                if this.add_progress.take().is_some() {
                                    cx.notify();
                                } else {
                                    this.mode = EditMode::Select;
                                    cx.notify();
                                }
                            }
                        } else if ev.keystroke.key == "enter" && this.mode == EditMode::Add {
                            if this.add_progress.take().is_some() {
                                cx.notify();
                            }
                        } else if ev.keystroke.key == "delete" || ev.keystroke.key == "backspace" {
                            this.delete_selected_features(cx);
                        } else if ev.keystroke.key == "a" {
                            // Mode-switch shortcuts are handled here rather
                            // than as global key bindings so they only fire
                            // while the map area has focus (see the comment
                            // by `cx.bind_keys` in `main()`).
                            this.on_set_mode(&SetMode { mode: EditModeAction::Add }, window, cx);
                        } else if ev.keystroke.key == "b" {
                            this.on_set_mode(&SetMode { mode: EditModeAction::Building }, window, cx);
                        } else if ev.keystroke.key == "x" {
                            this.on_set_mode(&SetMode { mode: EditModeAction::Extrude }, window, cx);
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
                                canvas(|_, _, _| {}, {
                                    let viewport_clone = self.viewport.clone();
                                    // Rather than aliasing `&self.layer_manager` with a
                                    // raw pointer (the paint closure runs after `render`
                                    // returns, so a borrow can't outlive this function),
                                    // capture a cheap `Entity<Self>` handle and re-borrow
                                    // `self` safely through it once paint actually runs —
                                    // the `&mut App` the canvas API hands the paint
                                    // closure is exactly what `Entity::read` needs.
                                    let entity = cx.entity();
                                    let selected = self.selected.clone();
                                    move |bounds, _, window, cx| {
                                        let this = entity.read(cx);
                                        this.layer_manager.render_all_canvas(
                                            &viewport_clone,
                                            bounds,
                                            window,
                                        );
                                        for sel in &selected {
                                            this.layer_manager.render_highlight(
                                                sel,
                                                &viewport_clone,
                                                bounds,
                                                window,
                                            );
                                        }

                                        if let Some(progress) = this.building_progress {
                                            let origin_x = bounds.origin.x;
                                            let origin_y = bounds.origin.y;
                                            let a_screen = viewport_clone
                                                .geo_to_screen(progress.corner_a.0, progress.corner_a.1);
                                            match progress.corner_b {
                                                None => {
                                                    // Only corner A placed: draw a marker at the
                                                    // fixed corner plus a rubber-band line to the
                                                    // cursor (no edge yet to offset a rectangle from).
                                                    let half = px(4.0);
                                                    let quad_bounds = Bounds {
                                                        origin: point(
                                                            a_screen.x + origin_x - half,
                                                            a_screen.y + origin_y - half,
                                                        ),
                                                        size: size(px(8.0), px(8.0)),
                                                    };
                                                    window.paint_quad(fill(quad_bounds, rgb(0x3b82f6)));

                                                    if let Some(mouse_pos) = this.last_mouse_pos {
                                                        let p0 = point(
                                                            a_screen.x + origin_x,
                                                            a_screen.y + origin_y,
                                                        );
                                                        let p1 = point(
                                                            mouse_pos.x + origin_x,
                                                            mouse_pos.y + origin_y,
                                                        );
                                                        let mut builder = PathBuilder::stroke(px(2.0));
                                                        builder.move_to(p0);
                                                        builder.line_to(p1);
                                                        if let Ok(path) = builder.build() {
                                                            window.paint_path(path, rgb(0x3b82f6));
                                                        }
                                                    }
                                                }
                                                Some(corner_b) => {
                                                    let cursor_geo = this
                                                        .last_mouse_pos
                                                        .map(|p| viewport_clone.screen_to_geo(p))
                                                        .unwrap_or(corner_b);
                                                    let (far_a, far_b) = osm_gpui::selection::rectangle_from_edge(
                                                        progress.corner_a, corner_b, cursor_geo,
                                                    );
                                                    let b_screen = viewport_clone.geo_to_screen(corner_b.0, corner_b.1);
                                                    let far_a_screen = viewport_clone.geo_to_screen(far_a.0, far_a.1);
                                                    let far_b_screen = viewport_clone.geo_to_screen(far_b.0, far_b.1);
                                                    let pts = [a_screen, b_screen, far_b_screen, far_a_screen, a_screen];
                                                    let mut builder = PathBuilder::stroke(px(2.0));
                                                    for (i, p) in pts.iter().enumerate() {
                                                        let p = point(p.x + origin_x, p.y + origin_y);
                                                        if i == 0 {
                                                            builder.move_to(p);
                                                        } else {
                                                            builder.line_to(p);
                                                        }
                                                    }
                                                    if let Ok(path) = builder.build() {
                                                        window.paint_path(path, rgb(0x3b82f6));
                                                    }
                                                }
                                            }
                                        }

                                        if let Some(drag) = &this.extrude_drag {
                                            if let Some(mouse_pos) = this.last_mouse_pos {
                                                if let Some(layer) = this.layer_manager.find_layer(drag.layer) {
                                                    if let Some(editable) = layer.as_editable() {
                                                        if let (Some(a_geo), Some(b_geo)) = (
                                                            editable.node_lat_lon(drag.node_a),
                                                            editable.node_lat_lon(drag.node_b),
                                                        ) {
                                                            let origin_x = bounds.origin.x;
                                                            let origin_y = bounds.origin.y;
                                                            let cursor_geo = viewport_clone.screen_to_geo(mouse_pos);
                                                            let (far_a, far_b) = osm_gpui::selection::rectangle_from_edge(
                                                                a_geo, b_geo, cursor_geo,
                                                            );
                                                            let a_screen = viewport_clone.geo_to_screen(a_geo.0, a_geo.1);
                                                            let b_screen = viewport_clone.geo_to_screen(b_geo.0, b_geo.1);
                                                            let far_a_screen = viewport_clone.geo_to_screen(far_a.0, far_a.1);
                                                            let far_b_screen = viewport_clone.geo_to_screen(far_b.0, far_b.1);
                                                            let pts = [a_screen, b_screen, far_b_screen, far_a_screen, a_screen];
                                                            let mut builder = PathBuilder::stroke(px(2.0));
                                                            for (i, p) in pts.iter().enumerate() {
                                                                let p = point(p.x + origin_x, p.y + origin_y);
                                                                if i == 0 {
                                                                    builder.move_to(p);
                                                                } else {
                                                                    builder.line_to(p);
                                                                }
                                                            }
                                                            if let Ok(path) = builder.build() {
                                                                window.paint_path(path, rgb(0x3b82f6));
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        if let Some(progress) = &this.add_progress {
                                            if let Some(mouse_pos) = this.last_mouse_pos {
                                                if let Some(layer_id) = this.active_layer {
                                                    if let Some(layer) =
                                                        this.layer_manager.find_layer(layer_id)
                                                    {
                                                        if let Some(editable) = layer.as_editable() {
                                                            if let Some(last_geo) = editable
                                                                .node_lat_lon(progress.last_node_id)
                                                            {
                                                                let origin_x = bounds.origin.x;
                                                                let origin_y = bounds.origin.y;
                                                                let last_screen = viewport_clone
                                                                    .geo_to_screen(last_geo.0, last_geo.1);
                                                                let p0 = point(
                                                                    last_screen.x + origin_x,
                                                                    last_screen.y + origin_y,
                                                                );
                                                                let p1 = point(
                                                                    mouse_pos.x + origin_x,
                                                                    mouse_pos.y + origin_y,
                                                                );
                                                                let mut builder =
                                                                    PathBuilder::stroke(px(2.0));
                                                                builder.move_to(p0);
                                                                builder.line_to(p1);
                                                                if let Ok(path) = builder.build() {
                                                                    window.paint_path(
                                                                        path,
                                                                        rgb(0x3b82f6),
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                })
                                .absolute()
                                .size_full(), // Ensure canvas fills the entire map area
                            ),
                    )
                    .child({
                        // Debug info overlay (toggleable via View menu)
                        if self.show_debug_overlay {
                            div()
                                .absolute()
                                .top_4()
                                .left_4()
                                .p_3()
                                .bg(cx.theme().popover)
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded_lg()
                                .text_color(cx.theme().popover_foreground)
                                .opacity(0.9)
                                .min_w_64()
                                .child(format!("🔍 Zoom: {:.1}", zoom_level))
                                .child(format!(
                                    "🌍 Center: {:.4}°N, {:.4}°W",
                                    center_lat,
                                    center_lon.abs()
                                ))
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
                                .bg(cx.theme().popover)
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded_lg()
                                .text_color(cx.theme().popover_foreground)
                                .opacity(0.9)
                                .child(msg)
                                .into_any_element()
                        } else {
                            div().into_any_element()
                        }
                    })
                    .child({
                        if let Some((start, current)) = self.interaction.box_select_rect() {
                            let rect = normalize_rect(from_pt(start), from_pt(current));
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
                    })
                    .child({
                        // Legally-required tile/imagery attribution for
                        // every currently-visible layer that has one,
                        // deduplicated (e.g. shared OSM Carto credit).
                        // Entries with a link are clickable and open
                        // it in the system browser.
                        let raw_credits = self.layer_manager.layers().iter().filter_map(|layer| {
                            if !layer.is_visible() {
                                return None;
                            }
                            layer.attribution().map(|a| (a.text.clone(), a.url.clone()))
                        });
                        let credits = interaction::dedupe_attributions(raw_credits);
                        if credits.is_empty() {
                            div().into_any_element()
                        } else {
                            let n = credits.len();
                            let link_hover = cx.theme().link_hover;
                            div()
                                .absolute()
                                .bottom_4()
                                .right_4()
                                .px_2()
                                .py_1()
                                .bg(cx.theme().popover)
                                .rounded_lg()
                                .text_color(cx.theme().popover_foreground)
                                .text_size(osm_gpui::ui::style::muted_text_size())
                                .opacity(0.75)
                                .flex()
                                .flex_row()
                                .children(credits.into_iter().enumerate().map(
                                    |(i, (text, url))| {
                                        let separator = if i + 1 < n { " | " } else { "" };
                                        let label = format!("{text}{separator}");
                                        if let Some(url) = url {
                                            div()
                                                .id(("attribution-link", i))
                                                .cursor_pointer()
                                                .hover(move |this| this.text_color(link_hover))
                                                .on_mouse_down(
                                                    gpui::MouseButton::Left,
                                                    move |_ev: &MouseDownEvent, _, _| {
                                                        let _ = open::that(&url);
                                                    },
                                                )
                                                .child(label)
                                                .into_any_element()
                                        } else {
                                            div().child(label).into_any_element()
                                        }
                                    },
                                ))
                                .into_any_element()
                        }
                    }),
            )
            .child(
                // Right panel with layer controls
                self.render_side_panel(window, cx),
            )
            .on_action(cx.listener(Self::on_move_layer))
            .on_action(cx.listener(Self::on_delete_layer))
            .on_action(cx.listener(Self::on_set_active_layer))
            .on_action(cx.listener(Self::on_set_mode))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_apply_nsi_preset))
            .on_action(cx.listener(Self::on_change_feature_type))
            .on_action(cx.listener(Self::on_quit))
            .children(self.custom_imagery_dialog.clone())
            .children(
                self.tag_edit_dialog
                    .as_ref()
                    .map(|(dialog, _)| dialog.clone()),
            )
            .children(self.quit_confirm_dialog.clone())
            .children(self.nsi_dialog.clone())
            .children(self.preset_picker_dialog.clone())
            .children(self.upload_dialog.clone())
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
            // Wait for the app to complete its first render pass.
            std::thread::sleep(Duration::from_millis(500));

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
            };

            let mut live_app = LiveApp {
                _idle: idle.clone(),
                bus: bus_for_runner,
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

    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);

            // Bring the menu bar to the foreground
            cx.activate(true);

            // Register the open file action
            cx.on_action(open_osm_file);
            cx.on_action(add_osm_carto);
            cx.on_action(add_coordinate_grid);
            cx.on_action(download_from_osm);
            cx.on_action(toggle_debug_overlay);
            cx.on_action(add_imagery_layer);
            cx.on_action(add_saved_custom_imagery);
            cx.on_action(no_op_imagery_info);
            cx.on_action(open_custom_imagery_dialog);
            cx.on_action(open_settings);
            cx.on_action(upload_to_osm);

            // Load persisted custom imagery entries.
            let loaded = custom_imagery_store::load();
            custom_imagery_store::init_store(loaded);

            // Load persisted app settings (OSM API server choice) and OAuth login.
            settings_store::init_store(settings_store::load());
            auth::init_store(auth::load());

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

            // Kick off background fetch/parse of the Name Suggestion Index.
            osm_gpui::nsi::init_store();
            cx.background_executor()
                .spawn(async move {
                    match osm_gpui::nsi::fetch_and_cache() {
                        Ok(body) => {
                            let entries = osm_gpui::nsi::parse(&body);
                            eprintln!("nsi: loaded {} brand entries", entries.len());
                            osm_gpui::nsi::set_index(osm_gpui::nsi::NsiIndex::from_entries(
                                entries,
                            ));
                        }
                        Err(e) => {
                            eprintln!("nsi: failed to load NSI data: {}", e);
                        }
                    }
                })
                .detach();

            let map_window = cx
                .open_window(
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
                            KeyBinding::new("cmd-shift-u", UploadToOsm, None),
                            KeyBinding::new("cmd-q", Quit, None),
                            KeyBinding::new("cmd-,", OpenSettings, None),
                            KeyBinding::new("cmd-z", Undo, None),
                            KeyBinding::new("cmd-shift-z", Redo, None),
                            // Note: the "a"/"b"/"x" mode-switch shortcuts are
                            // deliberately NOT registered here as global key
                            // bindings. Unlike the cmd-modified bindings above,
                            // these are plain unmodified letter keys, which would
                            // otherwise risk firing while the user is typing into a
                            // text input elsewhere in the app (e.g. the tag-edit
                            // dialog's key/value fields). Instead they're handled in
                            // the map area's local `on_key_down` handler below,
                            // which only fires when the map area itself has focus —
                            // see the `on_key_down` closure in `render()`.
                        ]);
                        let view = cx.new(|cx| MapViewer::new(window, cx));

                        // Publish a weak handle to the live view so the
                        // `on_window_should_close` closure below (which only has
                        // `&mut Window`/`&mut App`, not `Context<MapViewer>`) can
                        // reach it and query its *live* `layer_manager` state at
                        // decision time, rather than a cached boolean.
                        let _ = MAP_VIEWER_HANDLE.set(view.downgrade());

                        // Intercept the OS window-close button (traffic-light / titlebar
                        // close, or Cmd+W-equivalent) via gpui's cancelable pre-close
                        // hook: unlike `on_window_closed` below (which only fires *after*
                        // the window is already gone and can't stop anything), this one
                        // can veto the close by returning `false`. If there are unsaved
                        // changes (checked live, per layer, via `has_unsaved_changes`)
                        // we cancel the close and ask the live `MapViewer` to show the
                        // same confirmation dialog as Cmd+Q, directly (this closure
                        // already has a `Window`, so no need to go through
                        // `with_map_viewer_in`'s window-lookup-by-entity-id path);
                        // otherwise we let the close proceed, which triggers
                        // `on_window_closed` -> `cx.quit()` as before.
                        window.on_window_should_close(cx, |window, cx| {
                            if has_unsaved_changes(cx) {
                                if let Some(view) =
                                    MAP_VIEWER_HANDLE.get().and_then(|h| h.upgrade())
                                {
                                    view.update(cx, |v, cx| v.show_quit_confirm_dialog(window, cx));
                                }
                                false
                            } else {
                                true
                            }
                        });

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

fn append_custom_imagery(entry: CustomImageryEntry) {
    custom_imagery_store::append(entry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size, TestAppContext};
    use osm_gpui::osm::{OsmNode, OsmWay};
    use std::collections::HashMap;

    fn empty_tags() -> HashMap<String, String> {
        HashMap::new()
    }

    /// A single 2-node way `10 = [1, 2]`, both nodes at `center_lat`, lon
    /// `center_lon - 0.001` / `+ 0.001` — a flat horizontal line through the
    /// viewport's center at zoom 18 (matches the convention used by
    /// `OsmLayer`'s own tests in `src/layers/osm_layer.rs`).
    fn way_fixture(center_lat: f64, center_lon: f64) -> OsmData {
        let n1 = OsmNode {
            id: 1,
            lat: center_lat,
            lon: center_lon - 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat,
            lon: center_lon + 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let mut nodes = HashMap::new();
        nodes.insert(1, n1);
        nodes.insert(2, n2);
        let mut ways = HashMap::new();
        ways.insert(10, way);
        OsmData {
            nodes,
            ways,
            relations: Vec::new(),
            bounds: None,
        }
    }

    const CENTER_LAT: f64 = 40.0;
    const CENTER_LON: f64 = -74.0;

    /// Builds a `MapViewer` in a headless test window, viewport centered on
    /// `way_fixture`'s line (so screen `(400, 300)` lands exactly on it, per
    /// the same convention as `OsmLayer`'s own tests), with one active OSM
    /// layer containing that fixture, in Add mode.
    fn setup(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
        cx.update(gpui_component::init);
        let window = cx.add_window(MapViewer::new);
        window
            .update(cx, |view, _window, _cx| {
                view.viewport =
                    Viewport::new(CENTER_LAT, CENTER_LON, 18.0, size(px(800.0), px(600.0)));
                let layer_id = view.layer_manager.alloc_id();
                let layer = OsmLayer::new_with_data(
                    layer_id,
                    "L".to_string(),
                    Arc::new(way_fixture(CENTER_LAT, CENTER_LON)),
                );
                view.layer_manager.add_layer(Box::new(layer));
                view.active_layer = Some(layer_id);
                view.mode = EditMode::Add;
            })
            .unwrap();
        window
    }

    fn way_nodes(view: &MapViewer, layer_id: LayerId, way_id: i64) -> Vec<i64> {
        view.layer_manager
            .find_layer(layer_id)
            .and_then(|l| l.as_editable())
            .and_then(|e| e.way_node_ids(way_id))
            .unwrap_or_default()
    }

    #[gpui::test]
    fn first_click_in_add_mode_snaps_onto_existing_way(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);

                // On the line, exactly at the viewport's projected center.
                view.handle_add_click(point(px(400.0), px(300.0)), false);

                let nodes = way_nodes(view, layer_id, 10);
                assert_eq!(nodes.len(), 3, "expected a node spliced in: {:?}", nodes);
                assert_eq!((nodes[0], nodes[2]), (1, 2));
                let new_id = nodes[1];

                // Add mode's own progress continues from the snapped node,
                // not from an implicit continuation of way 10.
                assert!(view.add_progress.is_some());
                assert_eq!(view.add_progress.as_ref().unwrap().way_id, None);
                assert_eq!(view.add_progress.as_ref().unwrap().last_node_id, new_id);

                // Undo removes the splice and deletes the node.
                let action = view.undo_stack.undo().expect("expected an undo entry");
                view.apply_undo_action(&action, false);
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
                let layer = view.layer_manager.find_layer(layer_id).unwrap();
                assert_eq!(layer.as_editable().unwrap().node_lat_lon(new_id), None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn second_click_snaps_and_folds_with_compound_undo(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();

                // First click: a free node A, far from way 10.
                view.handle_add_click(point(px(50.0), px(50.0)), false);
                let node_a = view.add_progress.as_ref().unwrap().last_node_id;
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);

                // Second click: on way 10's line — snaps a new node B into
                // way 10, and folds B into a brand-new way (A -> B).
                view.handle_add_click(point(px(400.0), px(300.0)), false);

                let nodes10 = way_nodes(view, layer_id, 10);
                assert_eq!(
                    nodes10.len(),
                    3,
                    "expected a node spliced in: {:?}",
                    nodes10
                );
                assert_eq!((nodes10[0], nodes10[2]), (1, 2));
                let node_b = nodes10[1];

                let drawn_way_id = view.add_progress.as_ref().unwrap().way_id.unwrap();
                assert_eq!(
                    way_nodes(view, layer_id, drawn_way_id),
                    vec![node_a, node_b]
                );

                // Undo reverses both mutations from that single click: the
                // drawn way goes away entirely (it was created by this
                // click), way 10 reverts, and node B is deleted — but node A
                // (created by the *first* click, a separate undo entry) is
                // untouched.
                let action = view.undo_stack.undo().expect("expected an undo entry");
                view.apply_undo_action(&action, false);
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
                let layer = view.layer_manager.find_layer(layer_id).unwrap();
                let editable = layer.as_editable().unwrap();
                assert_eq!(editable.node_lat_lon(node_b), None);
                assert!(editable.node_lat_lon(node_a).is_some());
                assert_eq!(editable.way_node_ids(drawn_way_id), None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn ctrl_held_bypasses_both_node_and_way_snap(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                let n1_screen = view.viewport.geo_to_screen(CENTER_LAT, CENTER_LON - 0.001);

                // First click: a free node A, far from way 10 (Ctrl
                // irrelevant here — nothing nearby to snap to).
                view.handle_add_click(point(px(50.0), px(50.0)), false);
                let node_a = view.add_progress.as_ref().unwrap().last_node_id;

                // Second click, Ctrl held, exactly on top of existing node 1:
                // without Ctrl this would connect to node 1 (see the "click
                // hits an existing node" path); with Ctrl it must create a
                // brand-new independent node instead.
                view.handle_add_click(n1_screen, true);
                let node_b = view.add_progress.as_ref().unwrap().last_node_id;
                assert_ne!(node_b, 1, "Ctrl should not have connected to node 1");
                assert_eq!(
                    way_nodes(view, layer_id, 10),
                    vec![1, 2],
                    "way 10 untouched"
                );

                // Third click, Ctrl held, on way 10's line: without Ctrl this
                // would snap into way 10 (as in the sibling tests above);
                // with Ctrl it must stay a free node.
                view.handle_add_click(point(px(400.0), px(300.0)), true);
                let node_c = view.add_progress.as_ref().unwrap().last_node_id;
                assert_eq!(
                    way_nodes(view, layer_id, 10),
                    vec![1, 2],
                    "Ctrl should not have snapped onto way 10"
                );

                let drawn_way_id = view.add_progress.as_ref().unwrap().way_id.unwrap();
                assert_eq!(
                    way_nodes(view, layer_id, drawn_way_id),
                    vec![node_a, node_b, node_c]
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn click_with_no_way_nearby_falls_back_to_free_node(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();

                // Far from way 10's line — outside the 6px snap tolerance.
                view.handle_add_click(point(px(50.0), px(50.0)), false);

                assert_eq!(
                    way_nodes(view, layer_id, 10),
                    vec![1, 2],
                    "way 10 untouched"
                );
                let new_id = view.add_progress.as_ref().unwrap().last_node_id;
                let layer = view.layer_manager.find_layer(layer_id).unwrap();
                assert!(layer.as_editable().unwrap().node_lat_lon(new_id).is_some());
            })
            .unwrap();
    }
}
