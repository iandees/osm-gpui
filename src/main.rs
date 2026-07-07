use gpui::Action;
use gpui::{
    actions, canvas, div, point, prelude::*, px, rgb, size, App, Bounds, Context, KeyBinding,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, ScrollWheelEvent, SharedString, Window,
    WindowOptions,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

mod menu;
mod script_harness;
mod side_panel;
mod undo;

use crate::menu::{
    add_coordinate_grid, add_imagery_layer, add_osm_carto, add_saved_custom_imagery,
    download_from_osm, no_op_imagery_info, open_custom_imagery_dialog, open_osm_file,
    open_settings, quit, rebuild_menus, toggle_debug_overlay, upload_to_osm,
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
/// which only have `&mut App` (menu action handlers like `menu::quit`, and
/// app-level window callbacks like the `on_window_should_close` hook) reach
/// the *real* view and either query its live state or call ordinary
/// `MapViewer` methods on it directly — no polling queues involved.
pub(crate) static MAP_VIEWER_HANDLE: OnceLock<gpui::WeakEntity<MapViewer>> = OnceLock::new();

/// Ask the live `MapViewer` (via `MAP_VIEWER_HANDLE`) whether any layer
/// currently has unsaved changes. This performs a fresh per-layer
/// `is_modified()` query against the real view every time it's called — no
/// value is cached or pre-aggregated across frames.
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
    /// Whether each side-panel accordion section (Layers, Selection, Tags,
    /// History, in that order) is expanded.
    side_panel_open: [bool; 4],
    /// Focus handle for the map area, so it can receive key events (e.g.
    /// Escape to cancel an in-progress move-drag).
    focus_handle: gpui::FocusHandle,
    /// Global undo/redo history of committed data mutations.
    undo_stack: UndoStack,
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
            side_panel_open: [true, true, true, false],
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
        let _ = self.layer_manager.remove_at(action.index);
        cx.notify();
    }

    fn handle_mouse_down(&mut self, event: &MouseDownEvent) {
        let adjusted_position = event.position;

        self.viewport.handle_mouse_down(adjusted_position);
        self.interaction =
            interaction::record_mouse_down(&self.interaction, to_pt(adjusted_position));
    }

    /// Left-button mouse-down: if the point hits a currently-selected
    /// feature, start a move-drag instead of the usual box-select/click
    /// tracking. Always records the mouse-down position either way, since
    /// both paths need it to distinguish a click from a drag on release.
    fn handle_map_mouse_down(&mut self, position: gpui::Point<gpui::Pixels>) {
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
            UndoableAction::ExtendWay { layer_name, way_id, node_id, lat: _, lon: _, way_created } => {
                let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return };
                if forward {
                    if *way_created {
                        layer.add_way(vec![*node_id], Vec::new());
                    } else {
                        layer.extend_way(*way_id, *node_id);
                    }
                } else if *way_created {
                    layer.remove_way(*way_id);
                    layer.remove_node(*node_id);
                } else {
                    let node_ids = layer.way_node_ids(*way_id).unwrap_or_default();
                    if let Some(idx) = node_ids.iter().rposition(|id| id == node_id) {
                        layer.remove_node_from_way(*way_id, idx);
                    }
                    layer.remove_node(*node_id);
                }
            }
            UndoableAction::CreateBuilding { layer_name, way_id, node_ids } => {
                let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return };
                if !forward {
                    layer.remove_way(*way_id);
                    for id in node_ids {
                        layer.remove_node(*id);
                    }
                }
                // Redo (forward) is out of scope for Building mode's atomic
                // commit path in this plan: Building mode always creates a
                // *new* placeholder id on each commit, so a straightforward
                // redo-by-recreation isn't id-stable across a redo after
                // other edits. Matches this plan's scope (see spec's "Out
                // of scope": undo/redo depth beyond the immediate action).
            }
            UndoableAction::ExtrudeWay { layer_name, way_id, new_node_ids } => {
                let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return };
                if !forward {
                    layer.remove_way(*way_id);
                    for id in new_node_ids {
                        layer.remove_node(*id);
                    }
                }
            }
            UndoableAction::InsertNodeIntoWay { layer_name, way_id, index, node_id, .. } => {
                let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return };
                if !forward {
                    layer.remove_node_from_way(*way_id, *index);
                    layer.remove_node(*node_id);
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

    /// v1 "create node" gesture: Cmd+Click (the platform modifier) on the
    /// map creates a new, tag-less standalone node at the clicked point,
    /// selects it, and pushes a `CreateNode` undo action. This is a
    /// deliberately minimal, discoverable-only-via-this-comment interaction
    /// since the app has no toolbar/mode-toggle concept yet; a real
    /// "Add Node" mode (like JOSM/iD) would be a better long-term UX and
    /// should replace this gesture. No-op (with a status message) if no
    /// layer is willing to accept a new node — see
    /// `create_node_on_target_layer`.
    fn create_node_at_screen_point(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let (lat, lon) = self.viewport.screen_to_geo(position);
        let Some((layer_id, id)) = self.create_node_on_target_layer(lat, lon) else {
            self.set_status("No layer to add a node to");
            cx.notify();
            return;
        };
        self.undo_stack.push(UndoableAction::CreateNode {
            layer: layer_id,
            id,
            lat,
            lon,
        });
        self.selected = vec![osm_gpui::selection::FeatureRef {
            layer_id,
            kind: osm_gpui::selection::FeatureKind::Node,
            id,
        }];
        self.set_status(format!("Created node {}", id));
        cx.notify();
    }

    /// Pick the target layer for a newly created node: the first layer (in
    /// draw/layer-list order) that accepts it, i.e. the first `OsmLayer`
    /// with data loaded — mirrors how move/tag edits always operate on
    /// whichever layer already owns the feature, just with "owns" relaxed to
    /// "has OSM data at all" since a brand-new node has no owning layer yet.
    /// `None` if no layer accepts (e.g. no OSM data loaded anywhere).
    fn create_node_on_target_layer(&mut self, lat: f64, lon: f64) -> Option<(LayerId, i64)> {
        for layer in self.layer_manager.layers_mut() {
            let layer_id = layer.id();
            if let Some(editable) = layer.as_editable_mut() {
                if let Some(id) = editable.create_node(lat, lon, None) {
                    return Some((layer_id, id));
                }
            }
        }
        None
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
        let adjusted_position = event.position;
        let left_pressed = event.pressed_button == Some(gpui::MouseButton::Left);

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
        let up_pos = event.position;
        self.viewport.handle_mouse_up();

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
                self.handle_map_click(from_pt(at), event.modifiers.shift);
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
                // Always notify: the box-select overlay is driven off
                // `self.interaction`, which just transitioned back to `Idle`.
                // If the box hit nothing, `self.selected` wouldn't otherwise
                // change and the stale rectangle would stay on screen until
                // some unrelated redraw happened to pick up the new state.
                cx.notify();
            }
            interaction::Gesture::Click { at } => {
                let before = self.selected.clone();
                self.handle_map_click(from_pt(at), event.modifiers.shift);
                if before != self.selected {
                    cx.notify();
                }
            }
            interaction::Gesture::None => {}
        }
    }

    /// Resolve a plain click into a selection change. `shift_held` toggles
    /// the hit feature in/out of the existing selection (add if absent,
    /// remove if already selected) instead of replacing it; a shift-click
    /// that hits nothing is a no-op, leaving the existing selection intact.
    fn handle_map_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>, shift_held: bool) {
        let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
        let hit = osm_gpui::selection::resolve_hits(per_layer);
        self.selected = osm_gpui::selection::apply_click_selection(&self.selected, hit, shift_held);
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

        let adjusted_position = event.position;

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

    /// Open the "unsaved changes" quit-confirmation dialog, if one isn't
    /// already open. Called directly by `menu::quit` (via
    /// `with_map_viewer_in`) and by the `on_window_should_close` hook
    /// (which already has a `Window` in scope) — replaces the old
    /// `SHOW_QUIT_CONFIRM` queue. Like `open_custom_imagery_dialog`, this
    /// dialog has no post-paint focus requirement, so it's safe to construct
    /// directly rather than deferring to the next render pass.
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
        let panel_width = px(280.0);
        let map_size = gpui::size(window_size.width - panel_width, window_size.height);
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
                            // v1 "create node" gesture: Cmd+Click (see
                            // `create_node_at_screen_point`'s doc comment).
                            if ev.modifiers.platform {
                                this.create_node_at_screen_point(ev.position, cx);
                            } else {
                                this.handle_map_mouse_down(ev.position);
                            }
                        }),
                    )
                    .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "escape" {
                            this.cancel_move_drag(cx);
                        } else if ev.keystroke.key == "delete" || ev.keystroke.key == "backspace" {
                            this.delete_selected_features(cx);
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
                                .bg(gpui::black())
                                .rounded_lg()
                                .text_color(rgb(0xffffff))
                                .text_sm()
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
                            div()
                                .absolute()
                                .bottom_4()
                                .right_4()
                                .px_2()
                                .py_1()
                                .bg(gpui::black())
                                .rounded_lg()
                                .text_color(rgb(0xffffff))
                                .text_xs()
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
                                                .hover(|this| this.text_color(rgb(0xaad4ff)))
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
                self.render_side_panel(cx),
            )
            .on_action(cx.listener(Self::on_move_layer))
            .on_action(cx.listener(Self::on_delete_layer))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_apply_nsi_preset))
            .children(self.custom_imagery_dialog.clone())
            .children(
                self.tag_edit_dialog
                    .as_ref()
                    .map(|(dialog, _)| dialog.clone()),
            )
            .children(self.quit_confirm_dialog.clone())
            .children(self.nsi_dialog.clone())
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
                        ]);
                        let view = cx.new(|cx| MapViewer::new(window, cx));

                        // Publish a weak handle to the live view so `menu::quit` (a
                        // free function with only `&mut App`) and the
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
