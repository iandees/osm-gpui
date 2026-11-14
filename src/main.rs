// Release builds on Windows are GUI-subsystem executables so no console window
// opens; as a result stdout/stderr (e.g. `--script` output) is not shown.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

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

#[cfg(test)]
mod add_click_tests;
#[cfg(test)]
mod arc_click_tests;
mod autosave_driver;
mod clipboard_actions;
#[cfg(test)]
mod dialog_tests;
#[cfg(test)]
mod edge_case_tests;
mod fields_section;
mod gps_traces_ui;
mod history_actions;
#[cfg(test)]
mod http_injection_tests;
mod imagery_offset_ui;
#[cfg(test)]
mod imagery_state_tests;
#[cfg(test)]
mod improve_click_tests;
mod menu;
mod mode_panel;
mod notes_ui;
#[cfg(test)]
mod reference_tests;
mod script_harness;
mod selection_actions;
#[cfg(test)]
mod selection_dispatch_tests;
mod side_panel;
mod side_panel_layout;
mod status_bar;
mod street_imagery_ui;
mod tag_suggest_ui;
#[cfg(test)]
mod test_support;
mod undo;
mod validation_panel;
#[cfg(test)]
mod viewer_tests;
mod welcome;

use crate::menu::{
    add_coordinate_grid, add_imagery_layer, add_osm_carto, add_saved_custom_imagery,
    download_from_osm, no_op_imagery_info, open_custom_imagery_dialog, open_osm_file,
    open_settings, quit, rebuild_menus, toggle_debug_overlay, upload_to_osm, validate, ShowAbout,
};
use crate::script_harness::{LiveApp, ScriptBus, KEYSTROKE_QUEUE, SCRIPT_ACTIVE, SCRIPT_BUS};
use crate::undo::{
    roll_back, write_tag_entries, CreatedWay, NewNode, NodeMoveUndoEntries, NodeMoveUndoEntry,
    UndoStack, UndoableAction,
};

use gpui_component::ActiveTheme;
use osm_gpui::auth;
use osm_gpui::custom_imagery_store::{self, CustomImageryEntry};
use osm_gpui::edit_mode::EditMode;
use osm_gpui::idle_tracker::IdleTracker;
use osm_gpui::imagery::{self, ImageryEntry};
use osm_gpui::interaction::{self, Interaction, NodeMoveTargets};
use osm_gpui::layers::{
    grid_layer::GridLayer, osm_layer::OsmLayer, tile_layer::TileLayer, HighlightKind, LayerId,
    LayerManager, MapLayer,
};
use osm_gpui::osm::OsmData;
use osm_gpui::osm_api;
use osm_gpui::osm_upload;
use osm_gpui::persist;
use osm_gpui::script::{self, runner::Runner};
use osm_gpui::settings_store;
use osm_gpui::tile_cache::TileCache;
use osm_gpui::tiles;
use osm_gpui::transfer::{TransferHandle, TransferKind, TransferPhase};
use osm_gpui::viewport::Viewport;
use osm_gpui::{tr, tr_count};

actions!(
    osm_gpui,
    [
        OpenOsmFile,
        Quit,
        AddOsmCarto,
        AddCoordinateGrid,
        DownloadFromOsm,
        ToggleDebugOverlay,
        ToggleDownloadedArea,
        AddCustomImagery,
        ScriptSeedCustomImagery,
        OpenSettings,
        Undo,
        Redo,
        ApplyNsiPreset,
        ChangeFeatureType,
        UploadToOsm,
        Validate,
        ExportOsmFile,
        SelectAll,
        DeselectAll,
        Find,
        DeleteSelection,
        ZoomIn,
        ZoomOut,
        ZoomToData,
        ZoomToSelection,
        MinimizeWindow,
        ZoomWindow,
        HideApp,
        HideOtherApps,
        ShowAllApps
    ]
);

/// Key context set on `MapViewer`'s root element. Bindings scoped to it lose
/// to bindings scoped to a deeper context, which is what lets `cmd-a` mean
/// "select all text" inside a focused text input and "select all features"
/// everywhere else in the window — and likewise `cmd-z` undo typing in an
/// input rather than the last map edit. (A binding with no context at all
/// would outrank the input's own.)
const MAP_VIEWER_KEY_CONTEXT: &str = "MapViewer";

/// Most selected features whose highlight is drawn each frame.
const MAX_SELECTION_HIGHLIGHTS: usize = 10_000;

/// Build the live `gpui::KeyBinding` list for the modifier-key actions from
/// the current effective shortcut settings, plus the fixed macOS-standard
/// window/app bindings. The bare-letter mode-switch shortcuts (`mode_*`) are
/// deliberately excluded — see the comment where this is called.
pub(crate) fn key_bindings_from_settings(
    settings: &osm_gpui::settings_store::AppSettings,
    is_macos: bool,
) -> Vec<KeyBinding> {
    // Minimize and Hide are macOS conventions: off macOS `cmd` maps to the
    // Windows/Super key, where `cmd-m`/`cmd-h` would clash with the OS, and
    // Hide/Hide Others do nothing.
    let fixed = if is_macos {
        vec![
            KeyBinding::new("cmd-m", MinimizeWindow, None),
            KeyBinding::new("cmd-h", HideApp, None),
            KeyBinding::new("alt-cmd-h", HideOtherApps, None),
        ]
    } else {
        vec![]
    };
    osm_gpui::keybindings::effective_bindings(settings)
        .into_iter()
        .filter_map(|(id, spec)| key_binding_for(id, &spec))
        .chain(fixed)
        .collect()
}

/// The live binding for the modifier shortcut `id`, or `None` for the
/// bare-key categories, which the map canvas's key handler matches instead.
fn key_binding_for(id: &'static str, spec: &str) -> Option<KeyBinding> {
    match id {
        "select_all" => Some(KeyBinding::new(
            spec,
            SelectAll,
            Some(MAP_VIEWER_KEY_CONTEXT),
        )),
        "find" => Some(KeyBinding::new(spec, Find, Some(MAP_VIEWER_KEY_CONTEXT))),
        "show_history" => Some(KeyBinding::new(
            spec,
            history_actions::ShowObjectHistory,
            Some(MAP_VIEWER_KEY_CONTEXT),
        )),
        // Copy/Paste Tags run the same selection actions as the menus.
        // Scoped to the map like Select All, so they never steal the
        // keys from a focused text input.
        "copy_tags" | "paste_tags" | "copy_features" | "paste_features" | "duplicate_features" => {
            Some(KeyBinding::new(
                spec,
                RunSelectionAction { id: id.into() },
                Some(MAP_VIEWER_KEY_CONTEXT),
            ))
        }
        "zoom_in" => Some(KeyBinding::new(spec, ZoomIn, None)),
        "zoom_out" => Some(KeyBinding::new(spec, ZoomOut, None)),
        "zoom_to_data" => Some(KeyBinding::new(spec, ZoomToData, None)),
        "zoom_to_selection" => Some(KeyBinding::new(spec, ZoomToSelection, None)),
        "open_settings" => Some(KeyBinding::new(spec, OpenSettings, None)),
        "quit" => Some(KeyBinding::new(spec, Quit, None)),
        "open_osm_file" => Some(KeyBinding::new(spec, OpenOsmFile, None)),
        "download_osm" => Some(KeyBinding::new(spec, DownloadFromOsm, None)),
        "upload_osm" => Some(KeyBinding::new(spec, UploadToOsm, None)),
        "export_osm_file" => Some(KeyBinding::new(spec, ExportOsmFile, None)),
        "undo" => Some(KeyBinding::new(spec, Undo, Some(MAP_VIEWER_KEY_CONTEXT))),
        "redo" => Some(KeyBinding::new(spec, Redo, Some(MAP_VIEWER_KEY_CONTEXT))),
        _ => None,
    }
}

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

/// Action to toggle the visibility of the layer at `index`.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = layers)]
#[serde(deny_unknown_fields)]
struct ToggleLayerVisibilityAction {
    index: usize,
}

/// Action to run the selection action (see `selection_actions::ACTIONS`)
/// with the given id on the current selection. Carried by the Tools menu and
/// the right-click context menu.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = selection)]
#[serde(deny_unknown_fields)]
struct RunSelectionAction {
    id: SharedString,
}

/// Action to switch the current `EditMode`.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = mode)]
#[serde(deny_unknown_fields)]
struct SetMode {
    mode: EditMode,
}

/// Add mode's in-progress way-building state: the last-placed node, and
/// which way (if any) it belongs to. `None` on `MapViewer` means "no node
/// placed yet in this continuation" — the next click starts fresh.
struct AddProgress {
    way_id: Option<i64>,
    last_node_id: i64,
    /// Continuing an existing way from its first node (Extend Way on a way's
    /// start): new nodes are prepended to `way_id` instead of appended.
    at_start: bool,
}

/// Building mode's in-progress rectangle: corner A is fixed after click 1;
/// corner B after click 2. Both are geo (lat, lon) coordinates.
#[derive(Clone, Copy)]
struct BuildingProgress {
    corner_a: (f64, f64),
    corner_b: Option<(f64, f64)>,
}

/// Arc mode's in-progress arc: the start point is fixed after click 1, the
/// end point after click 2; the third click picks a point the arc passes
/// through. Both are geo (lat, lon) coordinates.
#[derive(Clone, Copy)]
struct ArcProgress {
    start: ArcPoint,
    end: Option<ArcPoint>,
}

/// What an Arc-mode click snapped onto.
#[derive(Clone, Copy)]
enum ArcSnap {
    /// An existing node, which the arc then uses as that endpoint.
    Node(i64),
    /// A way's segment (`way_id`, index of the segment's first node): the
    /// endpoint is a new node spliced into the way.
    Way(i64, usize),
}

/// A clicked Arc-mode point: its geo position (snapped onto a node or way
/// when the click was near one) and what it snapped onto.
#[derive(Clone, Copy)]
struct ArcPoint {
    geo: (f64, f64),
    snap: Option<ArcSnap>,
}

/// What a click would do to Improve Way mode's target way: the way, its node
/// ids and their screen positions, and the planned edit (in screen space).
struct ImprovePlan {
    layer_id: LayerId,
    way_id: i64,
    ids: Vec<i64>,
    points: Vec<osm_gpui::improve_way::Pt>,
    plan: osm_gpui::improve_way::Plan,
}

/// The squared way whose orientation Building mode borrows: captured when
/// the mode is entered with exactly that way selected, and kept (even though
/// the selection moves on to each new building) until the mode is exited.
#[derive(Clone, Copy)]
struct BuildingReference {
    layer_id: LayerId,
    way_id: i64,
    /// Bearing of the way's longest edge in a local metric projection, in
    /// radians in `[0, pi)` (see `osm_gpui::building`).
    orientation: f64,
}

/// A left press in oriented Building mode, awaiting its release.
#[derive(Clone, Copy)]
struct BuildingPress {
    down: gpui::Point<gpui::Pixels>,
    /// True if this press placed the first corner (so releasing in place is
    /// just a click and the building waits for a second one).
    placed_first_corner: bool,
}

/// What a pointer at a given spot would snap onto (see `osm_gpui::snap`):
/// shown as a highlight while it applies, and what a click (Add mode) or
/// drag release (a single node) then does.
#[derive(Clone, Debug, PartialEq)]
enum SnapTarget {
    /// An existing node, reused / merged into.
    Node { id: i64, lat: f64, lon: f64 },
    /// A way segment: the node becomes a new vertex at `index + 1`, at
    /// `(lat, lon)`, the nearest point of the segment.
    Way {
        way_id: i64,
        a: (f64, f64),
        b: (f64, f64),
        index: usize,
        lat: f64,
        lon: f64,
    },
}

type Tags = Vec<(String, String)>;

/// Tags used to classify way `way_id` as a snap target: its own, or, for a
/// way with nothing recognisable (an untagged multipolygon member), those of
/// the first classifiable relation that contains it.
fn snap_target_tags(
    editable: &dyn osm_gpui::layers::EditableLayer,
    layer_id: LayerId,
    way_id: i64,
) -> Tags {
    let tags = editable
        .feature_tags(&osm_gpui::selection::FeatureRef {
            layer_id,
            kind: osm_gpui::selection::FeatureKind::Way,
            id: way_id,
        })
        .unwrap_or_default();
    if osm_gpui::snap::classify(&tags).is_some() {
        return tags;
    }
    editable
        .relations_containing_way(way_id)
        .into_iter()
        .filter_map(|r| editable.relation(r))
        .map(|r| r.tags.into_iter().collect::<Tags>())
        .find(|t| osm_gpui::snap::classify(t).is_some())
        .unwrap_or(tags)
}

/// The tag sets that decide what node `node` may snap onto: its own tags
/// and those of every way it is part of.
fn snap_node_sources(
    editable: &dyn osm_gpui::layers::EditableLayer,
    layer_id: LayerId,
    node: i64,
) -> Vec<Tags> {
    let mut sources = vec![editable
        .feature_tags(&osm_gpui::selection::FeatureRef {
            layer_id,
            kind: osm_gpui::selection::FeatureKind::Node,
            id: node,
        })
        .unwrap_or_default()];
    for way in editable.ways_using_node(node) {
        sources.push(snap_target_tags(editable, layer_id, way));
    }
    sources
}

/// Whether way `way_id` may be snapped onto by something with `sources`
/// tags, given `node` (the node being moved, if any) must not already be in
/// it.
fn snap_way_ok(
    editable: &dyn osm_gpui::layers::EditableLayer,
    layer_id: LayerId,
    sources: &[Tags],
    node: Option<i64>,
    way_id: i64,
) -> bool {
    if let Some(node) = node {
        if editable
            .way_node_ids(way_id)
            .is_some_and(|nodes| nodes.contains(&node))
        {
            return false;
        }
    }
    let refs: Vec<&[(String, String)]> = sources.iter().map(|t| t.as_slice()).collect();
    osm_gpui::snap::may_snap(&refs, &snap_target_tags(editable, layer_id, way_id))
}

/// Whether existing node `target` may be snapped onto: not the moving node
/// itself, and every way it belongs to must suit `sources`.
fn snap_node_ok(
    editable: &dyn osm_gpui::layers::EditableLayer,
    layer_id: LayerId,
    sources: &[Tags],
    node: Option<i64>,
    target: i64,
) -> bool {
    if node == Some(target) {
        return false;
    }
    let refs: Vec<&[(String, String)]> = sources.iter().map(|t| t.as_slice()).collect();
    editable
        .ways_using_node(target)
        .into_iter()
        .all(|w| osm_gpui::snap::may_snap(&refs, &snap_target_tags(editable, layer_id, w)))
}

/// Highlight a snap target: the segment it would join (amber) and a ring
/// around the point the node would land on.
fn paint_snap_target(
    target: &SnapTarget,
    viewport: &Viewport,
    origin: gpui::Point<gpui::Pixels>,
    window: &mut Window,
) {
    let amber = rgb(0xf59e0b);
    let at = |lat: f64, lon: f64| {
        let p = viewport.geo_to_screen(lat, lon);
        point(p.x + origin.x, p.y + origin.y)
    };
    let (lat, lon) = match target {
        SnapTarget::Node { lat, lon, .. } => (*lat, *lon),
        SnapTarget::Way { a, b, lat, lon, .. } => {
            let mut builder = PathBuilder::stroke(px(4.0));
            builder.move_to(at(a.0, a.1));
            builder.line_to(at(b.0, b.1));
            if let Ok(path) = builder.build() {
                window.paint_path(path, amber);
            }
            (*lat, *lon)
        }
    };
    let c = at(lat, lon);
    let half = px(7.0);
    let bounds = Bounds {
        origin: point(c.x - half, c.y - half),
        size: size(half * 2.0, half * 2.0),
    };
    window.paint_quad(gpui::quad(
        bounds,
        half,
        gpui::transparent_black(),
        px(2.5),
        amber,
        gpui::BorderStyle::default(),
    ));
}

/// What an Extrude-mode drag does, fixed by the modifier keys held at
/// mouse-down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExtrudeStyle {
    /// Default: reshape the way, growing or notching it (JOSM's plain drag).
    Reshape,
    /// Cmd (Ctrl off macOS): translate the segment's endpoints, no new nodes.
    Move,
    /// Alt/Option: pull a new separate, untagged way out of the segment.
    NewWay,
}

impl ExtrudeStyle {
    fn from_modifiers(m: gpui::Modifiers) -> Self {
        if m.alt {
            ExtrudeStyle::NewWay
        } else if m.secondary() {
            ExtrudeStyle::Move
        } else {
            ExtrudeStyle::Reshape
        }
    }
}

/// Window-space position to map-area-local, given the height of the chrome
/// (in-window menu bar) above the map. See `MapViewer::window_to_map`.
fn window_to_map_origin(
    position: gpui::Point<gpui::Pixels>,
    top_chrome: gpui::Pixels,
) -> gpui::Point<gpui::Pixels> {
    gpui::point(
        position.x - px(MapViewer::MODE_PANEL_WIDTH),
        position.y - top_chrome,
    )
}

/// The index `i` such that `node_ids[i] == a && node_ids[i + 1] == b`,
/// preferring `hint` (the hit-test's index) when it is consistent.
fn segment_index_in_way(node_ids: &[i64], hint: usize, a: i64, b: i64) -> Option<usize> {
    let matches = |i: usize| node_ids.get(i) == Some(&a) && node_ids.get(i + 1) == Some(&b);
    if matches(hint) {
        return Some(hint);
    }
    (0..node_ids.len().saturating_sub(1)).find(|&i| matches(i))
}

/// An Extrude drag resolved against the current cursor: the plan to apply and
/// the resulting outline in screen coordinates, for the live preview.
struct ExtrudeResult {
    plan: osm_gpui::extrude::ExtrudePlan,
    /// The way's node list after the edit, as screen positions.
    outline: Vec<(f64, f64)>,
}

/// An interactive Rotate in progress: the selection's nodes turn around their
/// centre as the pointer circles it. The nodes are really moved on every
/// pointer move (so the map redraws the true result); `originals` is what a
/// cancel puts back and what the undo step's "before" is.
struct RotateSession {
    layer: LayerId,
    /// The nodes being turned with their positions when the session began.
    originals: Vec<(i64, osm_gpui::geometry::LatLon)>,
    center: osm_gpui::geometry::LatLon,
    /// The pointer's angle around the centre when the session began; set by
    /// the first pointer move if the pointer position was not known yet.
    start_angle: Option<f64>,
    /// The rotation currently applied, radians counter-clockwise (map).
    angle: f64,
    /// Whether the pointer has moved since the session began. A click only
    /// commits after that, so the click that picked Rotate in the context
    /// menu cannot also finish it.
    moved: bool,
}

/// A pointer position as plain f64 pixels.
fn pointer_px(p: gpui::Point<gpui::Pixels>) -> (f64, f64) {
    (f64::from(f32::from(p.x)), f64::from(f32::from(p.y)))
}

/// Extrude mode's in-progress drag: the way segment being extruded from,
/// and the two endpoint node ids used to build the preview/final rectangle.
struct ExtrudeDrag {
    layer: LayerId,
    way_id: i64,
    node_a: i64,
    node_b: i64,
    /// Index of the segment (`node_a`) in the way's node list.
    segment: usize,
    style: ExtrudeStyle,
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
        /// Stable id of the source (the ELI id) that imagery offsets are
        /// remembered under; `None` for a custom template.
        imagery_id: Option<String>,
    },
}

/// The full ELI list, filled by `MapViewer::imagery_index_loaded` once the
/// background load finishes; the menus read it.
static IMAGERY_INDEX: OnceLock<Arc<Mutex<Vec<ImageryEntry>>>> = OnceLock::new();

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
/// it directly.
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

/// Whether the app may write the owner's settings file: not during a
/// `--script` run (the harness keeps changes in memory) and never in a unit
/// test.
pub(crate) fn may_persist_settings() -> bool {
    !cfg!(test) && !script_harness::script_mode()
}

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
                .filter_map(|l| l.as_osm())
                .any(|l| l.is_modified())
        })
        .unwrap_or(false)
}

/// Run `f` against the live `MapViewer` (via `MAP_VIEWER_HANDLE`), if it
/// still exists. This is the standard way for menu action handlers (which
/// only have `&mut App`) to call an ordinary `MapViewer` method.
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
    /// UI language for this run, overriding the saved setting.
    language: Option<String>,
    /// Colour theme for this run (`--theme light|dark`); the app otherwise
    /// starts light and does not follow the system appearance.
    theme: Option<gpui_component::ThemeMode>,
    /// API server base URL for this run only (`--api-server dev|primary|URL`),
    /// overriding the saved setting. Never written to `settings.json`.
    api_server: Option<String>,
    /// Pause between script ops (`--script-delay-ms N`), so a person can
    /// follow a scripted run on screen.
    script_delay: Option<Duration>,
    /// Keep autosaves in this directory (`--autosave-dir PATH`) and look for
    /// a crashed run's autosaves there. The only way to turn autosave on in
    /// a scripted run, which otherwise never touches any autosave directory.
    autosave_dir: Option<PathBuf>,
    /// `.osm` files to open as layers at startup (positional arguments).
    files: Vec<PathBuf>,
}

fn parse_cli_args() -> CliArgs {
    parse_cli_args_from(std::env::args().skip(1))
}

fn parse_cli_args_from(args: impl IntoIterator<Item = String>) -> CliArgs {
    let mut out = CliArgs::default();
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            // Everything after `--` is a file, even if it starts with `-`.
            "--" => out.files.extend(args.by_ref().map(PathBuf::from)),
            "--script" => {
                out.script = Some(PathBuf::from(args.next().expect("--script needs a path")))
            }
            "--window-size" => {
                let v = args.next().expect("--window-size needs WxH");
                let (w, h) = v.split_once('x').expect("--window-size format WxH");
                out.window_size = Some((w.parse().expect("W"), h.parse().expect("H")));
            }
            "--keep-open" => out.keep_open = true,
            "--language" => {
                out.language = Some(args.next().expect("--language needs a locale tag"))
            }
            "--api-server" => {
                let v = args
                    .next()
                    .expect("--api-server needs dev, primary or a URL");
                out.api_server = Some(
                    settings_store::parse_api_server_arg(&v)
                        .expect("--api-server needs dev, primary or an http(s) URL"),
                );
            }
            "--autosave-dir" => {
                out.autosave_dir = Some(PathBuf::from(
                    args.next().expect("--autosave-dir needs a directory"),
                ))
            }
            "--script-delay-ms" => {
                let v = args.next().expect("--script-delay-ms needs a number");
                out.script_delay = Some(Duration::from_millis(
                    v.parse().expect("--script-delay-ms needs a number"),
                ));
            }
            "--theme" => {
                out.theme = match args.next().as_deref() {
                    Some("light") => Some(gpui_component::ThemeMode::Light),
                    Some("dark") => Some(gpui_component::ThemeMode::Dark),
                    _ => panic!("--theme needs light or dark"),
                }
            }
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("ignoring unknown arg: {}", other)
            }
            other => out.files.push(PathBuf::from(other)),
        }
    }
    out
}

/// The features an accepted upload chunk created (their placeholder ids are
/// replaced by server ids) or deleted. Undo steps that name any of them refer
/// to objects that no longer exist under that id, so they are dropped from the
/// history: replaying them would create objects with positive ids, leave a
/// split's uploaded half behind or re-create a deleted object.
pub(crate) fn uploaded_taint(
    layers: &[LayerId],
    chunk: &osm_gpui::layers::diff::LayerDiff,
) -> Vec<osm_gpui::selection::FeatureRef> {
    use osm_gpui::selection::FeatureKind::{Node, Relation, Way};
    let mut ids: Vec<(osm_gpui::selection::FeatureKind, i64)> = Vec::new();
    ids.extend(chunk.created_nodes.iter().map(|n| (Node, n.id)));
    ids.extend(chunk.created_ways.iter().map(|w| (Way, w.id)));
    ids.extend(chunk.created_relations.iter().map(|r| (Relation, r.id)));
    ids.extend(chunk.deleted_node_ids.iter().map(|&(id, _)| (Node, id)));
    ids.extend(chunk.deleted_way_ids.iter().map(|&(id, _)| (Way, id)));
    ids.extend(
        chunk
            .deleted_relation_ids
            .iter()
            .map(|&(id, _)| (Relation, id)),
    );
    layers
        .iter()
        .flat_map(|&layer_id| {
            ids.iter()
                .map(move |&(kind, id)| osm_gpui::selection::FeatureRef { layer_id, kind, id })
        })
        .collect()
}

use osm_gpui::upload_chunks::UploadStop;

struct MapViewer {
    viewport: Viewport,
    layer_manager: LayerManager,
    tile_cache: Arc<Mutex<TileCache>>,
    /// The HTTP client every network feature of this window uses (see
    /// `osm_gpui::http::install_default_client`); cloned into worker tasks.
    http: Arc<dyn osm_gpui::http::HttpClient>,
    first_dataset_fitted: bool,
    /// The main window's bounds as last rendered, saved on quit.
    window_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    status_message: Option<(SharedString, Instant)>,
    /// The running download or upload, shown with a spinner in the status
    /// bar until it completes. The worker thread updates the shared state;
    /// a poll task re-renders. Unlike `status_message` it never expires on
    /// its own, and at most one transfer runs at a time.
    transfer: Option<TransferHandle>,
    /// Whether Space is currently held with the map focused; a left-drag
    /// then pans instead of selecting.
    space_held: bool,
    /// Bumped whenever layer data changes outside the undo stack (upload
    /// reconciliation, layer add/delete); part of `pending_edits`' cache key.
    data_generation: u64,
    /// Cached pending-edit count for the status bar, keyed on
    /// `(undo_stack.revision, data_generation)`.
    pending_edits: Option<((u64, u64), usize)>,
    selected: Vec<osm_gpui::selection::FeatureRef>,
    /// What Copy and Copy Tags set aside, for Paste and Paste Tags.
    clipboards: clipboard_actions::Clipboards,
    /// Current mouse interaction: idle, a pending press, an in-progress
    /// box-select, or an in-progress move-drag of the selection. See
    /// `osm_gpui::interaction` for the pure click/drag/box-select decision
    /// tree this drives.
    interaction: Interaction,
    frame_times: VecDeque<Instant>,
    /// Last (lat, lon) the Imagery menu was rebuilt for. None forces a rebuild.
    last_menu_center: Option<(f64, f64)>,
    /// Where the Editor Layer Index load stands.
    imagery_load_state: ImageryLoadState,
    /// The load state the Imagery menu was last rebuilt for.
    last_imagery_load_state: Option<ImageryLoadState>,
    /// Whether the debug info overlay is currently visible.
    show_debug_overlay: bool,
    /// Whether the downloaded-area wash (everything outside the downloaded
    /// bounds dimmed) is drawn. View > Show Downloaded Area; on by default and
    /// not persisted (the settings store holds no other View preferences).
    show_downloaded_area: bool,
    /// Whether the menus were last built with File > Upload to OSM enabled.
    upload_menu_enabled: bool,
    /// Active custom imagery dialog, if open.
    custom_imagery_dialog:
        Option<gpui::Entity<osm_gpui::ui::custom_imagery_dialog::CustomImageryDialog>>,
    /// Active "unsaved changes" quit-confirmation dialog, if open.
    quit_confirm_dialog: Option<gpui::Entity<osm_gpui::ui::quit_confirm_dialog::QuitConfirmDialog>>,
    delete_layer_dialog: Option<gpui::Entity<osm_gpui::ui::delete_layer_dialog::DeleteLayerDialog>>,
    /// The startup offer to restore autosaved layers, if showing.
    restore_dialog: Option<gpui::Entity<osm_gpui::ui::restore_dialog::RestoreDialog>>,
    /// Autosave and crash recovery for this run; `None` when off (see
    /// `autosave_driver`).
    autosave: Option<autosave_driver::AutosaveState>,
    /// The object history view and what was learned from the server's
    /// version history of objects (see `history_actions`).
    object_history: history_actions::HistoryState,
    /// Edit > Find, while open.
    find_dialog: Option<gpui::Entity<osm_gpui::ui::find_dialog::FindDialog>>,
    /// The last few queries run through Find this session, newest first.
    find_history: osm_gpui::search::QueryHistory,
    /// The mode Find was last used in; the dialog opens with it.
    find_mode: osm_gpui::search::SearchMode,
    about_dialog: Option<gpui::Entity<osm_gpui::ui::about_dialog::AboutDialog>>,
    /// Active upload-review dialog, if open.
    upload_dialog: Option<gpui::Entity<osm_gpui::ui::upload_dialog::UploadDialog>>,
    /// The side panel's Validation section, once Tools > Validate has run
    /// (see `validation_panel.rs`).
    validation: Option<validation_panel::ValidationPanel>,
    /// The changeset comment typed in the upload dialog, kept when the dialog
    /// closes without uploading (Cancel, or Show on a conflict) and put back
    /// when it reopens. Cleared after a successful upload.
    upload_comment_draft: String,
    /// The sources text kept when the upload dialog closed without
    /// uploading; `None` means prefill the last uploaded one.
    upload_source_draft: Option<String>,
    sign_in_dialog: Option<gpui::Entity<osm_gpui::ui::sign_in_dialog::SignInDialog>>,
    /// Active "can't do that" notice (an upload refused for a server size
    /// limit), if open.
    notice_dialog: Option<gpui::Entity<osm_gpui::ui::notice_dialog::NoticeDialog>>,
    /// The tag currently being edited in place in the Tags table, if any.
    tag_inline_edit: Option<InlineTagEdit>,
    /// A request to start an inline tag edit, recorded by a cell/button
    /// click and acted on during the next `render()` — see
    /// `PendingTagEditOpen`'s doc comment.
    pending_tag_edit_open: Option<PendingTagEditOpen>,
    /// Autocomplete for the tag inputs: layer indexes, taginfo bookkeeping.
    tag_suggest: tag_suggest_ui::TagSuggestState,
    /// The role of a relation member being edited in place in the Members
    /// list, if any.
    role_edit: Option<RoleEdit>,
    /// A request to start a role edit, acted on during the next `render()`
    /// for the same reason as `pending_tag_edit_open`.
    pending_role_edit_open: Option<(MemberRef, String)>,
    /// Whether the Members list shows every member of a long relation
    /// instead of the first few.
    members_show_all: bool,
    /// One focus handle per Members row, grouping that row's buttons so the
    /// row can tell whether focus is inside it (grown lazily while rendering).
    member_controls_focus: std::cell::RefCell<Vec<gpui::FocusHandle>>,
    /// Set by "Add to Relation…"; `render` opens the picker (it needs the
    /// window) and clears it.
    pending_relation_picker: bool,
    /// Active "add to relation" picker dialog, if open.
    relation_picker_dialog:
        Option<gpui::Entity<osm_gpui::ui::relation_picker_dialog::RelationPickerDialog>>,
    /// Set by "Create Parallel Way…"; `render` opens the dialog and clears it.
    pending_parallel_dialog: bool,
    /// Active "create parallel way" dialog, if open.
    parallel_dialog: Option<gpui::Entity<osm_gpui::ui::parallel_dialog::ParallelDialog>>,
    /// What the parallel-way dialog was last submitted with, offered again
    /// next time (this session only).
    parallel_params: osm_gpui::parallel::ParallelParams,
    /// The feature whose Fields section has its "Add field" list expanded.
    /// Keyed by feature so the list is closed again for any other selection
    /// without every selection-changing path having to reset it.
    fields_more_open_for: Option<osm_gpui::selection::FeatureRef>,
    /// Current width of the side panel in pixels (user-resizable).
    side_panel_width: f32,
    /// An in-progress drag of the side panel's resize handle: the window x
    /// and panel width at mouse-down.
    side_panel_resize: Option<(f32, f32)>,
    /// Active NSI preset search dialog, if open.
    nsi_dialog: Option<gpui::Entity<osm_gpui::ui::nsi_dialog::NsiPresetDialog>>,
    /// Active "change feature type" preset picker dialog, if open.
    preset_picker_dialog:
        Option<gpui::Entity<osm_gpui::ui::preset_picker_dialog::PresetPickerDialog>>,
    /// Whether each side-panel accordion section (Layers, Selection, Fields,
    /// Tags, History, Relations, Members, Validation, by the indices
    /// `render_side_panel` gives them) is expanded.
    side_panel_open: [bool; 8],
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
    /// What the pointer would snap onto right now (Add mode hover, or a
    /// single node being dragged), drawn as a highlight. See `SnapTarget`.
    snap_preview: Option<SnapTarget>,
    /// In-progress Building-mode rectangle state, or `None` between
    /// continuations (see `BuildingProgress`).
    building_progress: Option<BuildingProgress>,
    /// In-progress Arc-mode state, or `None` between arcs (see `ArcProgress`).
    arc_progress: Option<ArcProgress>,
    /// The modifier keys as of the last mouse move, click or modifier change;
    /// Improve Way mode reads it to preview and pick its operation.
    last_modifiers: gpui::Modifiers,
    /// The squared way Building mode is aligned to, if any (see
    /// `BuildingReference`). When set, a building takes two corners instead
    /// of three clicks.
    building_reference: Option<BuildingReference>,
    /// The pending left press in oriented Building mode.
    building_press: Option<BuildingPress>,
    /// Last known mouse position within the map area, updated on every
    /// mouse-move; used to drive the live Building-mode preview (which
    /// needs a cursor position outside of a drag) during `render()`.
    last_mouse_pos: Option<gpui::Point<gpui::Pixels>>,
    /// The feature under the cursor that a click would select, if any
    /// (Select mode only). Drawn with the hover highlight.
    hovered: Option<osm_gpui::selection::FeatureRef>,
    /// A visible vertex (previewed, or on a selected way) that was pressed
    /// without being selected: the move-drag it started moves just this node,
    /// and the node becomes the selection once the drag passes the threshold
    /// (a release without moving is a plain click and selects it that way).
    vertex_grab: Option<osm_gpui::selection::FeatureRef>,
    /// Extrude mode's in-progress drag, or `None` when not dragging (see
    /// `ExtrudeDrag`).
    extrude_drag: Option<ExtrudeDrag>,
    /// The interactive Rotate in progress, if any (see `RotateSession`).
    rotate_session: Option<RotateSession>,
    /// Window position of the last right-button press on the map, kept until
    /// the button is released: a release within `CLICK_SLOP` of it is a click
    /// (opens the context menu), anything further was a pan.
    right_press: Option<gpui::Point<gpui::Pixels>>,
    /// The left press now in progress began while an in-window menu was open
    /// (so it only closes the menu): the map ignores it and its release.
    menu_swallow_press: bool,
    /// Whether the window has been drawn once, so there is a dispatch tree to
    /// ask whether focus is inside a dialog (see `keep_dialog_focus`).
    drawn_once: bool,
    /// The open right-click context menu, if any.
    context_menu: Option<ContextMenuState>,
    /// The "Adjust imagery offset" mode, while it is on (see
    /// `imagery_offset_ui`).
    offset_adjust: Option<imagery_offset_ui::OffsetAdjust>,
    /// OSM notes: the Notes layer, the open note, writes (see `notes_ui`).
    notes: notes_ui::NotesState,
    /// Downloaded OSM GPS traces (see `gps_traces_ui`).
    gps_traces: gps_traces_ui::GpsTracesState,
    /// Street-level photos from Mapillary and Fotoshi (see `street_imagery_ui`).
    imagery: street_imagery_ui::ImageryState,
}

/// An open selection-actions context menu: the `PopupMenu` entity, where it
/// was opened (window coordinates), and the subscription that clears this
/// state when the menu dismisses itself (Esc, an item chosen, a click out).
struct ContextMenuState {
    menu: gpui::Entity<gpui_component::menu::PopupMenu>,
    position: gpui::Point<gpui::Pixels>,
    _dismiss: gpui::Subscription,
}

/// Which features an inline tag edit targets and the row's original text
/// (used to detect whether the value was actually touched before applying
/// — see `compute_tag_edit_entries`). `original_key`/`original_value` are
/// both empty for the "Add tag" flow.
struct TagEditContext {
    features: Vec<osm_gpui::selection::FeatureRef>,
    original_key: String,
    original_value: String,
    is_add: bool,
}

/// An in-place edit of one tag in the Tags table. Both inputs always exist
/// and are seeded with the row's original text, so committing reads them
/// uniformly; `field` says which of them the table actually shows as an
/// input (`Key` or `Value` for an existing row, `None` for the add row,
/// which shows both).
struct InlineTagEdit {
    ctx: TagEditContext,
    field: osm_gpui::ui::tag_edit::TagEditField,
    key: gpui::Entity<gpui_component::input::InputState>,
    value: gpui::Entity<gpui_component::input::InputState>,
    /// The autocomplete list under whichever input is being typed in.
    suggest: tag_suggest_ui::Dropdown,
}

/// Recorded by a tag cell click or the "Add tag" button; consumed by
/// `check_for_pending_tag_edit` (called from `Render::render`) to actually
/// build the inline inputs. This indirection exists so the inputs are
/// always built from inside a render pass — never directly inside a click
/// handler — which the deferred select-all-on-open depends on (see
/// `check_for_pending_tag_edit`).
struct PendingTagEditOpen {
    features: Vec<osm_gpui::selection::FeatureRef>,
    original_key: String,
    original_value: String,
    select: osm_gpui::ui::tag_edit::TagEditField,
    is_add: bool,
}

/// One member of a relation, by position: roles can repeat and so can
/// members, so the index is the identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MemberRef {
    layer_id: LayerId,
    relation_id: i64,
    index: usize,
}

/// An in-place edit of one member's role in the Members list.
struct RoleEdit {
    member: MemberRef,
    input: gpui::Entity<gpui_component::input::InputState>,
}

/// The mouse cursor to show over the map: a closed hand while pan-dragging,
/// an open hand while Space arms a pan, a crosshair in the modes that place
/// points, and a pointing hand over something a click would select.
fn map_cursor(
    mode: EditMode,
    panning: bool,
    space_held: bool,
    hovering: bool,
) -> gpui::CursorStyle {
    use gpui::CursorStyle;
    if panning {
        CursorStyle::ClosedHand
    } else if space_held {
        CursorStyle::OpenHand
    } else {
        match mode {
            EditMode::Add | EditMode::Building | EditMode::Arc | EditMode::Improve => {
                CursorStyle::Crosshair
            }
            EditMode::Select if hovering => CursorStyle::PointingHand,
            EditMode::Select | EditMode::Extrude => CursorStyle::Arrow,
        }
    }
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
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        // If the map loses focus while Space is held, its key-up goes
        // elsewhere; don't leave the map stuck in pan-drag mode.
        cx.on_blur(&focus_handle, window, |this, _, cx| {
            if this.space_held {
                this.space_held = false;
                cx.notify();
            }
        })
        .detach();
        // Reopen where the map was left; scripted runs always use the
        // built-in view so captures are deterministic.
        let start = if script_harness::script_mode() {
            None
        } else {
            settings_store::snapshot()
                .last_map_view
                .and_then(|v| v.validated())
        };
        let (start_lat, start_lon, start_zoom) = start
            .map(|v| (v.lat, v.lon, v.zoom))
            .unwrap_or((40.7128, -74.0060, 11.0));
        let viewport = Viewport::new(
            start_lat,
            start_lon,
            start_zoom,
            gpui::size(px(800.0), px(600.0)),
        );
        // Remember the view on any normal quit.
        cx.on_app_quit(|this, _cx| {
            let (lat, lon) = this.viewport.center();
            let view = settings_store::MapView {
                lat,
                lon,
                zoom: this.viewport.zoom_level(),
            };
            let persist = may_persist_settings();
            let rect = this.window_bounds.map(|b| settings_store::WindowRect {
                x: b.origin.x.as_f32(),
                y: b.origin.y.as_f32(),
                width: b.size.width.as_f32(),
                height: b.size.height.as_f32(),
            });
            async move {
                if let Some(rect) = rect {
                    if persist && rect.validated(&[]).is_some() {
                        settings_store::set_window_bounds(rect, true);
                    }
                }
                if view.validated().is_some() && persist {
                    settings_store::set_last_map_view(view, true);
                }
            }
        })
        .detach();
        let executor = cx.background_executor().clone();
        // Use the global idle tracker (set before Application::new().run(...))
        let idle = GLOBAL_IDLE.get().cloned().unwrap_or_else(IdleTracker::new);
        let tile_cache = Arc::new(Mutex::new(TileCache::new(executor, idle)));
        let layer_manager = LayerManager::new();
        // No layers here; the window setup adds OSM Carto (see
        // `add_default_layers`), other imagery and the grid come from the
        // menu, and loaded files add their own OSM layer.
        Self {
            viewport,
            layer_manager,
            tile_cache,
            http: osm_gpui::http::default_client(),
            first_dataset_fitted: false,
            window_bounds: None,
            status_message: None,
            transfer: None,
            space_held: false,
            data_generation: 0,
            pending_edits: None,
            selected: Vec::new(),
            clipboards: Default::default(),
            interaction: Interaction::Idle,
            frame_times: VecDeque::with_capacity(120),
            last_menu_center: None,
            imagery_load_state: ImageryLoadState::Loading,
            last_imagery_load_state: None,
            show_debug_overlay: false,
            show_downloaded_area: true,
            upload_menu_enabled: false,
            custom_imagery_dialog: None,
            quit_confirm_dialog: None,
            delete_layer_dialog: None,
            restore_dialog: None,
            autosave: None,
            object_history: Default::default(),
            find_dialog: None,
            find_history: Default::default(),
            find_mode: Default::default(),
            about_dialog: None,
            upload_dialog: None,
            validation: None,
            upload_comment_draft: String::new(),
            upload_source_draft: None,
            sign_in_dialog: None,
            notice_dialog: None,
            tag_inline_edit: None,
            tag_suggest: tag_suggest_ui::TagSuggestState::new(),
            fields_more_open_for: None,
            // Scripted runs use the default width, not the user's saved one,
            // so scripts' click coordinates hold on every machine.
            side_panel_width: osm_gpui::ui::style::initial_side_panel_width(
                settings_store::snapshot()
                    .side_panel_width
                    .filter(|_| !script_harness::script_mode()),
            ),
            side_panel_resize: None,
            pending_tag_edit_open: None,
            role_edit: None,
            pending_relation_picker: false,
            relation_picker_dialog: None,
            pending_parallel_dialog: false,
            parallel_dialog: None,
            parallel_params: osm_gpui::parallel::ParallelParams::default(),
            pending_role_edit_open: None,
            members_show_all: false,
            member_controls_focus: std::cell::RefCell::new(Vec::new()),
            nsi_dialog: None,
            preset_picker_dialog: None,
            // Everything open except History and Validation, which keep to
            // their headers until the user opens them.
            side_panel_open: {
                let mut open = [true; 8];
                open[side_panel::HISTORY_SECTION] = false;
                open[validation_panel::SECTION] = false;
                open
            },
            fields_text_inputs: std::collections::HashMap::new(),
            fields_text_subscribed: std::collections::HashSet::new(),
            fields_open_combo: None,
            fields_promoted_more_fields: std::collections::HashSet::new(),
            focus_handle,
            undo_stack: UndoStack::default(),
            mode: EditMode::Select,
            active_layer: None,
            add_progress: None,
            snap_preview: None,
            building_progress: None,
            arc_progress: None,
            last_modifiers: gpui::Modifiers::none(),
            building_reference: None,
            building_press: None,
            last_mouse_pos: None,
            hovered: None,
            vertex_grab: None,
            extrude_drag: None,
            rotate_session: None,
            right_press: None,
            menu_swallow_press: false,
            drawn_once: false,
            context_menu: None,
            offset_adjust: None,
            notes: Default::default(),
            gps_traces: Default::default(),
            imagery: Default::default(),
        }
    }

    /// The `(center_lat, center_lon, ImageryLoadState)` triple `rebuild_menus`
    /// needs, as currently known to this viewer. Exposed so callers outside
    /// the per-frame `maybe_rebuild_imagery_menu` loop (e.g. the Settings
    /// window's keybindings-changed handler in `menu.rs`) can trigger an
    /// immediate menu rebuild without duplicating this lookup.
    pub(crate) fn imagery_menu_context(&self) -> (f64, f64, ImageryLoadState) {
        let (lat, lon) = self.viewport.center();
        (lat, lon, self.imagery_load_state)
    }

    /// Whether View > Show Downloaded Area is on (drives its menu checkmark).
    pub(crate) fn shows_downloaded_area(&self) -> bool {
        self.show_downloaded_area
    }

    /// Load the Editor Layer Index in the background; the viewer is told when
    /// it finishes.
    fn load_imagery_index(&mut self, cx: &mut Context<Self>) {
        let http = self.http.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    imagery::fetch_and_cache(http.as_ref())
                        .map(|body| imagery::parse(&body))
                        .map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| this.imagery_index_loaded(result, cx));
        })
        .detach();
    }

    /// The Editor Layer Index load finished: keep the entries for the menus
    /// and rebuild the Imagery menu.
    fn imagery_index_loaded(
        &mut self,
        result: Result<Vec<ImageryEntry>, String>,
        cx: &mut Context<Self>,
    ) {
        self.imagery_load_state = match result {
            Ok(entries) => {
                eprintln!("imagery: loaded {} ELI entries", entries.len());
                if let Some(index) = IMAGERY_INDEX.get() {
                    if let Ok(mut guard) = index.lock() {
                        *guard = entries;
                    }
                }
                ImageryLoadState::Ready
            }
            Err(e) => {
                eprintln!("imagery: failed to load ELI: {e}");
                ImageryLoadState::Failed
            }
        };
        self.maybe_rebuild_imagery_menu(cx);
    }

    /// Rebuild the Imagery menu if needed (center moved or load state changed).
    fn maybe_rebuild_imagery_menu(&mut self, cx: &mut Context<Self>) {
        let (lat, lon) = self.viewport.center();

        let current_state = self.imagery_load_state;

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
        rebuild_menus(
            &mut *cx,
            lat,
            lon,
            current_state,
            self.show_downloaded_area,
            self.has_uploadable_changes(),
        );
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
    ///
    /// A layer with unsaved edits asks first (`show_delete_layer_dialog`);
    /// one without (imagery, the grid, a data layer with nothing pending)
    /// goes straight away.
    fn on_delete_layer(
        &mut self,
        action: &DeleteLayer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.edits_blocked(cx) {
            return;
        }
        let Some(layer) = self.layer_manager.layers().get(action.index) else {
            return;
        };
        let (id, name) = (layer.id(), layer.name().to_string());
        let pending = Self::unsaved_edit_count(layer.as_ref());
        if pending == 0 {
            self.delete_layer_now(id, cx);
            // The layer list's context menu held focus and is gone; without
            // focus in the window the View/Edit menu items have no handler
            // and all grey out.
            window.focus(&self.focus_handle, cx);
        } else {
            self.show_delete_layer_dialog(id, name, pending, window, cx);
        }
    }

    /// The layer's unsaved edits, counted as the status bar's "N unsaved
    /// edits" does (and so the quit warning's `is_modified`).
    fn unsaved_edit_count(layer: &dyn osm_gpui::layers::MapLayer) -> usize {
        let Some(osm) = layer.as_osm().filter(|osm| osm.is_modified()) else {
            return 0;
        };
        let (created, modified, deleted) = osm.diff_for_upload().counts();
        created + modified + deleted
    }

    /// Remove layer `id`, if it still exists, with everything that refers to
    /// it. Deleting is not undoable: the layer and its unsaved edits are gone
    /// for good, so its history goes with it. The layer is looked up by id,
    /// not index, since the list may have changed since a dialog opened.
    fn delete_layer_now(&mut self, id: LayerId, cx: &mut Context<Self>) {
        let Some(index) = self
            .layer_manager
            .layers()
            .iter()
            .position(|l| l.id() == id)
        else {
            return;
        };
        if let Some(removed) = self.layer_manager.remove_at(index) {
            self.forget_layer(removed.id());
            // Its autosave goes with it: the layer's edits are discarded.
            self.autosave_forget_layer(removed.id(), cx);
        }
        self.data_generation += 1;
        cx.notify();
    }

    /// Open the "delete layer with unsaved edits" confirmation, unless one is
    /// already open. Cancel is the default; nothing happens until confirmed.
    fn show_delete_layer_dialog(
        &mut self,
        id: LayerId,
        name: String,
        pending: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.delete_layer_dialog.is_some() {
            return;
        }
        let dialog = cx.new(|cx| {
            osm_gpui::ui::delete_layer_dialog::DeleteLayerDialog::new(id, name, pending, window, cx)
        });
        cx.subscribe(
            &dialog,
            |this, _entity, event: &osm_gpui::ui::delete_layer_dialog::DialogEvent, cx| {
                use osm_gpui::ui::delete_layer_dialog::DialogEvent;
                this.delete_layer_dialog = None;
                match event {
                    DialogEvent::Cancelled => cx.notify(),
                    DialogEvent::Confirmed(id) => {
                        // An upload may have started while the dialog was
                        // open; the guard applies here as well.
                        if !this.edits_blocked(cx) {
                            this.delete_layer_now(*id, cx);
                        }
                    }
                }
            },
        )
        .detach();
        self.delete_layer_dialog = Some(dialog);
        cx.notify();
    }

    /// Drop everything that refers to the just-removed layer `id`: its undo
    /// and redo steps (all of them, see `UndoStack::remove_layer`), the
    /// selection, hover and in-progress gestures, and the active-layer
    /// designation. Returns how many history steps went.
    fn forget_layer(&mut self, id: LayerId) -> usize {
        let removed = self.undo_stack.remove_layer(id);
        let was_active = self.active_layer == Some(id);
        if was_active {
            self.active_layer = None;
            // Mode progress is relative to the active layer.
            self.reset_mode_progress();
        }
        if self.building_reference.is_some_and(|r| r.layer_id == id) {
            self.building_reference = None;
        }
        if self.extrude_drag.as_ref().is_some_and(|d| d.layer == id) {
            self.extrude_drag = None;
        }
        if self.rotate_session.as_ref().is_some_and(|r| r.layer == id) {
            self.rotate_session = None;
        }
        self.interaction = Interaction::Idle;
        if self.hovered.is_some_and(|h| h.layer_id == id) {
            self.hovered = None;
        }
        if self.selected.iter().any(|f| f.layer_id == id) {
            let kept = self
                .selected
                .iter()
                .copied()
                .filter(|f| f.layer_id != id)
                .collect();
            self.set_selection(kept);
        }
        if self.fields_more_open_for.is_some_and(|f| f.layer_id == id) {
            self.fields_more_open_for = None;
        }
        // Inline editors aimed at the deleted layer would commit nowhere.
        if self
            .tag_inline_edit
            .as_ref()
            .is_some_and(|e| e.ctx.features.iter().any(|f| f.layer_id == id))
        {
            self.tag_inline_edit = None;
        }
        if self
            .pending_tag_edit_open
            .as_ref()
            .is_some_and(|p| p.features.iter().any(|f| f.layer_id == id))
        {
            self.pending_tag_edit_open = None;
        }
        if self
            .role_edit
            .as_ref()
            .is_some_and(|e| e.member.layer_id == id)
        {
            self.role_edit = None;
        }
        if self
            .pending_role_edit_open
            .as_ref()
            .is_some_and(|(m, _)| m.layer_id == id)
        {
            self.pending_role_edit_open = None;
        }
        removed
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

    /// Handle the `ToggleLayerVisibilityAction` context-menu action: same
    /// effect as clicking the layer row's checkbox.
    fn on_toggle_layer_visibility(
        &mut self,
        action: &ToggleLayerVisibilityAction,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self
            .layer_manager
            .layers()
            .get(action.index)
            .map(|l| l.id())
        {
            self.toggle_layer_visibility(id);
            cx.notify();
        }
    }

    /// Handle the `SetMode` action: switch modes, discarding any
    /// in-progress add/building/extrude state without committing it (Tasks
    /// 6-8 populate these fields; they don't exist until then, so this
    /// handler is added here as a no-op placeholder for those clears and
    /// extended in each later task).
    fn on_set_mode(&mut self, action: &SetMode, window: &mut Window, cx: &mut Context<Self>) {
        let mode = action.mode;
        // The drawing modes need a layer to write into. The toolbar buttons
        // are disabled in that state; this covers the keyboard and menu paths.
        if mode != EditMode::Select && self.active_layer.is_none() {
            self.set_status(tr!("status.open_data_first"));
            cx.notify();
            return;
        }
        // Picking a tool ends the imagery offset adjustment, keeping it.
        self.finish_offset_adjust(true, window, cx);
        // Entering Building mode with one squared way selected borrows its
        // orientation; staying in Building mode keeps the existing one.
        self.building_reference = match mode {
            EditMode::Building if self.mode == EditMode::Building => self.building_reference,
            EditMode::Building => self.squared_selection_reference(),
            _ => None,
        };
        self.mode = mode;
        self.hovered = None;
        self.clear_vertex_preview();
        self.reset_mode_progress();
        self.cancel_rotate();
        cx.notify();
    }

    /// Discard every mode's half-finished gesture without committing it.
    /// Every path that leaves a mode or invalidates the layer the gesture
    /// refers to calls this, so a mode's state can't survive its mode.
    /// `building_reference` is not progress: it outlives a Building session's
    /// gestures and is managed by whoever changes the mode.
    pub(crate) fn reset_mode_progress(&mut self) {
        self.add_progress = None;
        self.building_progress = None;
        self.building_press = None;
        self.arc_progress = None;
        self.extrude_drag = None;
    }

    /// If the selection is exactly one closed, squared way, the reference a
    /// Building-mode session should align to.
    fn squared_selection_reference(&self) -> Option<BuildingReference> {
        use osm_gpui::selection::FeatureKind;
        let [feature] = self.selected.as_slice() else {
            return None;
        };
        if feature.kind != FeatureKind::Way {
            return None;
        }
        let editable = self
            .layer_manager
            .find_layer(feature.layer_id)?
            .as_editable()?;
        let node_ids = editable.way_node_ids(feature.id)?;
        if node_ids.len() < 4 || node_ids.first() != node_ids.last() {
            return None;
        }
        let ring: Vec<(f64, f64)> = node_ids
            .iter()
            .map(|&id| editable.node_lat_lon(id))
            .collect::<Option<_>>()?;
        if !osm_gpui::building::is_squared(&ring, osm_gpui::building::SQUARED_TOLERANCE_DEG) {
            return None;
        }
        Some(BuildingReference {
            layer_id: feature.layer_id,
            way_id: feature.id,
            orientation: osm_gpui::building::dominant_orientation(&ring)?,
        })
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
    ///
    /// The in-window menu bar (Windows/Linux, see `menu::in_window_menu_enabled`)
    /// is a row above the map, so it shifts the map's origin down too.
    fn window_to_map(&self, position: gpui::Point<gpui::Pixels>) -> gpui::Point<gpui::Pixels> {
        window_to_map_origin(position, menu::top_chrome_height())
    }

    /// Left-button mouse-down: if the point hits a currently-selected
    /// feature, start a move-drag instead of the usual box-select/click
    /// tracking. Always records the mouse-down position either way, since
    /// both paths need it to distinguish a click from a drag on release.
    /// `position` must already be in map-local coordinates (see
    /// `window_to_map`) — callers pass the raw event position through
    /// `window_to_map` first.
    fn handle_map_mouse_down(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        modifiers: gpui::Modifiers,
    ) {
        // Adjusting the imagery offset: the press starts moving the imagery.
        if self.offset_adjust_mouse_down(position) {
            self.interaction = Interaction::Idle;
            return;
        }
        // Oriented Building mode owns the left button entirely (no
        // box-select, and no move-drag of the selected reference way): the
        // press places the first corner, the release decides click vs drag.
        if self.mode == EditMode::Building && self.building_reference.is_some() {
            let placed_first_corner = self.building_progress.is_none();
            if placed_first_corner {
                let corner = self.viewport.screen_to_geo(position);
                self.building_progress = Some(BuildingProgress {
                    corner_a: corner,
                    corner_b: None,
                });
            }
            self.building_press = Some(BuildingPress {
                down: position,
                placed_first_corner,
            });
            self.interaction = Interaction::Idle;
            return;
        }
        if self.mode == EditMode::Extrude {
            if let Some(layer_id) = self.active_layer {
                if let Some(layer) = self.layer_manager.find_layer(layer_id) {
                    if let Some(osm_layer) = layer.as_osm() {
                        // A segment of the selected way wins over a nearer
                        // segment of an overlapping one.
                        let selected_way = self
                            .selected
                            .iter()
                            .find(|f| {
                                f.layer_id == layer_id
                                    && f.kind == osm_gpui::selection::FeatureKind::Way
                            })
                            .map(|f| f.id);
                        if let Some((way_id, node_a, node_b, idx)) = osm_layer
                            .hit_test_segment_preferring(
                                &self.viewport,
                                position,
                                6.0,
                                selected_way,
                            )
                        {
                            let segment = layer
                                .as_editable()
                                .and_then(|e| e.way_node_ids(way_id))
                                .and_then(|ids| segment_index_in_way(&ids, idx, node_a, node_b));
                            if let Some(segment) = segment {
                                self.extrude_drag = Some(ExtrudeDrag {
                                    layer: layer_id,
                                    way_id,
                                    node_a,
                                    node_b,
                                    segment,
                                    style: ExtrudeStyle::from_modifiers(modifiers),
                                    down: position,
                                });
                            }
                        }
                    }
                }
            }
            return;
        }

        self.vertex_grab = None;
        let mut hit_move_targets = if self.selected.is_empty()
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
        // A press on a visible vertex that is not selected (one of the
        // previewed way's, or of a selected way) grabs just that node, as in
        // iD and JOSM: select-and-move in one gesture. Shift keeps the
        // toggle-on-click meaning, and the other modes are untouched.
        if hit_move_targets.is_none() && self.mode == EditMode::Select && !modifiers.shift {
            if let Some((feature, targets)) = self.unselected_node_grab(position) {
                self.vertex_grab = Some(feature);
                hit_move_targets = Some(targets);
            }
        }
        self.interaction = interaction::on_left_mouse_down(to_pt(position), hit_move_targets);
    }

    /// The node a press at `position` lands on, if it is not already
    /// selected, with the one-node move target for dragging it.
    fn unselected_node_grab(
        &self,
        position: gpui::Point<gpui::Pixels>,
    ) -> Option<(osm_gpui::selection::FeatureRef, NodeMoveTargets)> {
        let hit = osm_gpui::selection::resolve_hits(
            self.layer_manager.hit_test_all(&self.viewport, position),
        )?;
        if hit.kind != osm_gpui::selection::FeatureKind::Node || self.selected.contains(&hit) {
            return None;
        }
        let (lat, lon) = self
            .layer_manager
            .find_layer(hit.layer_id)?
            .as_editable()?
            .node_lat_lon(hit.id)?;
        Some((hit, vec![(hit.layer_id, vec![(hit.id, lat, lon)])]))
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
                // A relation has no geometry of its own to drag.
                FeatureKind::Relation => {}
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
    /// Returns whether a move-drag was actually in progress (and cancelled).
    fn cancel_move_drag(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(targets) = interaction::cancel_move_drag(&mut self.interaction) else {
            return false;
        };
        self.vertex_grab = None;
        for (layer_id, _) in &targets {
            if let Some(layer) = self.layer_manager.find_layer_mut(*layer_id) {
                if let Some(editable) = layer.as_editable_mut() {
                    editable.clear_drag_preview();
                }
            }
        }
        cx.notify();
        true
    }

    /// Apply an undo/redo step: `forward = true` reapplies the action's
    /// "after" state (redo), `forward = false` reverts to its "before"
    /// state (undo). Both directions reuse the same commit path as a live
    /// drag, so caches rebuild exactly once per affected layer.
    fn apply_undo_action(&mut self, action: &UndoableAction, forward: bool) {
        // A refused step means the history and the data disagree. There is
        // no way to repair that here, so say so and apply the rest.
        let mut failures = Vec::new();
        action.apply(&mut self.layer_manager, forward, &mut failures);
        for failure in &failures {
            eprintln!(
                "{} of \"{}\" was refused: {failure}",
                if forward { "redo" } else { "undo" },
                action.description()
            );
        }
        // A layer undone back to its server baseline has an empty diff and
        // must stop reporting itself as modified (layer dot, quit warning,
        // delete-layer confirmation).
        for id in action.layers() {
            if let Some(layer) = self
                .layer_manager
                .find_layer_mut(id)
                .and_then(|l| l.as_osm_mut())
            {
                layer.refresh_modified();
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

    /// If a cell/button click recorded a pending inline tag edit, build its
    /// inputs now. Called from `Render::render` — never build them directly
    /// from inside a click/action listener: `Window::dispatch_action`
    /// resolves its target in the last *painted* frame, so the deferred
    /// select-all below only reaches the new input when it is created
    /// during the same draw pass that first paints it.
    fn check_for_pending_tag_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputState, SelectAll};
        use osm_gpui::ui::tag_edit::TagEditField;

        let Some(pending) = self.pending_tag_edit_open.take() else {
            return;
        };
        if self.edits_blocked(cx) {
            return;
        }
        // Finish any edit that is still open: its input doesn't blur when
        // the click that got us here landed on a button.
        self.commit_tag_inline_edit(cx);

        let PendingTagEditOpen {
            features,
            original_key,
            original_value,
            select,
            is_add,
        } = pending;

        let mut new_input = |placeholder: SharedString, text: String| {
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                state.set_value(text, window, cx);
                state
            })
        };
        let key = new_input(tr!("panel.tags.key_placeholder"), original_key.clone());
        let value = new_input(tr!("panel.tags.value_placeholder"), original_value.clone());
        cx.subscribe_in(&key, window, |this, input, event, window, cx| {
            this.on_tag_input_event(TagEditField::Key, input, event, window, cx);
        })
        .detach();
        cx.subscribe_in(&value, window, |this, input, event, window, cx| {
            this.on_tag_input_event(TagEditField::Value, input, event, window, cx);
        })
        .detach();

        let focused = if select == TagEditField::Value {
            &value
        } else {
            &key
        };
        focused.update(cx, |state, cx| state.focus(window, cx));
        if !is_add {
            // Select the existing text so typing replaces it.
            window.on_next_frame(|window, cx| {
                window.dispatch_action(Box::new(SelectAll), cx);
            });
        }

        self.tag_inline_edit = Some(InlineTagEdit {
            ctx: TagEditContext {
                features,
                original_key,
                original_value,
                is_add,
            },
            field: select,
            key,
            value,
            suggest: Default::default(),
        });
        // Have the layers' tag counts ready (in the background) for the
        // first keystroke.
        self.sync_tag_indexes(cx);
        // A new row starts empty: offer suggestions right away. (The input's
        // Focus event can fire before the edit above exists, so don't rely
        // on it.)
        if is_add {
            let first = if select == TagEditField::Value {
                TagEditField::Value
            } else {
                TagEditField::Key
            };
            // A moment after the first paint, so the input's position is
            // known and the list is placed beside it, not over it.
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(150))
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.tag_inline_edit.is_some() {
                        this.refresh_tag_suggestions(first, window, cx);
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// React to Enter or loss of focus in one of the inline tag edit's two
    /// inputs (`which`). Enter commits — except in the add row's key input,
    /// where it moves on to the value. Losing focus commits too, so clicking
    /// away keeps what was typed; the add row's key input is the exception
    /// again, since focus normally leaves it for the value input (an empty
    /// key there abandons the row instead).
    fn on_tag_input_event(
        &mut self,
        which: osm_gpui::ui::tag_edit::TagEditField,
        input: &gpui::Entity<gpui_component::input::InputState>,
        event: &gpui_component::input::InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::InputEvent;
        use osm_gpui::ui::tag_edit::TagEditField;

        let Some(edit) = &self.tag_inline_edit else {
            return;
        };
        // Ignore late events from an edit that was already committed or
        // cancelled (its inputs outlive it until their subscriptions drop).
        let current = if which == TagEditField::Key {
            &edit.key
        } else {
            &edit.value
        };
        if current != input {
            return;
        }
        let add_row_key = edit.ctx.is_add && which == TagEditField::Key;

        let add_row = edit.ctx.is_add;

        match event {
            // Typing: show matching suggestions under the input.
            InputEvent::Change => {
                self.refresh_tag_suggestions(which, window, cx);
                return;
            }
            // Arriving in a new row's key input: offer the keys that usually
            // go with the feature's main tag (e.g. atm= for amenity=bank).
            InputEvent::Focus if add_row => {
                self.refresh_tag_suggestions(which, window, cx);
                return;
            }
            InputEvent::PressEnter { .. } if add_row_key => {
                let value = edit.value.clone();
                self.close_tag_suggestions();
                value.update(cx, |state, cx| state.focus(window, cx));
            }
            InputEvent::PressEnter { .. } => {
                self.close_tag_suggestions();
                self.commit_tag_inline_edit(cx);
                window.focus(&self.focus_handle, cx);
            }
            InputEvent::Blur if add_row_key => {
                let empty = edit.key.read(cx).value().trim().is_empty();
                self.close_tag_suggestions();
                if empty {
                    self.tag_inline_edit = None;
                }
            }
            InputEvent::Blur => {
                self.close_tag_suggestions();
                self.commit_tag_inline_edit(cx);
            }
            _ => return,
        }
        cx.notify();
    }

    /// Apply the open inline tag edit, if any, and close it. An empty key
    /// discards the edit: silently for the add row, with a status message
    /// for an existing row (which would otherwise look like a failed save).
    fn commit_tag_inline_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.tag_inline_edit.take() else {
            return;
        };
        let key = edit.key.read(cx).value().trim().to_string();
        let value = edit.value.read(cx).value().to_string();
        if key.is_empty() {
            if !edit.ctx.is_add {
                self.set_status(tr!("status.tag_needs_key"));
            }
            return;
        }
        self.apply_tag_edit(&edit.ctx, &key, &value);
    }

    /// Abandon the open inline tag edit (Escape) and hand focus back to the
    /// map.
    pub(crate) fn cancel_tag_inline_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tag_inline_edit.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    /// Apply a tag edit to the features in `ctx`: compute the per-feature
    /// mutations via `compute_tag_edit_entries`, apply them immediately,
    /// and push one `UndoableAction::SetTags` (skipped entirely if there
    /// were no actual changes).
    fn apply_tag_edit(&mut self, ctx: &TagEditContext, key: &str, value: &str) {
        if self.refuse_edit_during_upload() {
            return;
        }
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

        let indexed = self.current_tag_index_layers();
        let entries = write_tag_entries(&mut self.layer_manager, entries);
        if entries.is_empty() {
            return;
        }
        self.note_tag_changes(&indexed, &entries);
        self.undo_stack.push(UndoableAction::SetTags { entries });
    }

    /// Delete `key` from every currently-selected feature that has it,
    /// applying immediately and pushing one `UndoableAction::SetTags` (no
    /// dialog involved).
    fn delete_tag(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.refuse_edit_during_upload() {
            return;
        }
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

        let indexed = self.current_tag_index_layers();
        let entries = write_tag_entries(&mut self.layer_manager, entries);
        if entries.is_empty() {
            return;
        }
        self.note_tag_changes(&indexed, &entries);
        self.undo_stack.push(UndoableAction::SetTags { entries });
        cx.notify();
    }

    /// A copy of relation `relation_id` of `layer_id`, if the layer has it.
    pub(crate) fn relation_of(
        &self,
        layer_id: LayerId,
        relation_id: i64,
    ) -> Option<osm_gpui::osm::OsmRelation> {
        self.layer_manager
            .find_layer(layer_id)?
            .as_editable()?
            .relation(relation_id)
    }

    /// Replace the member list of a relation with `after` as one undo step.
    /// Does nothing if the layer has no such relation.
    fn change_relation_members(
        &mut self,
        layer_id: LayerId,
        relation_id: i64,
        after: Vec<osm_gpui::osm::OsmMember>,
    ) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|layer| layer.as_editable_mut())
        else {
            return;
        };
        let Ok(before) = editable.set_relation_members(relation_id, after.clone()) else {
            return;
        };
        self.undo_stack.push(UndoableAction::SetRelationMembers {
            layer: layer_id,
            relation_id,
            before,
            after,
        });
    }

    /// Move member `member.index` of a relation by `delta` places.
    pub(crate) fn move_relation_member(&mut self, member: MemberRef, delta: isize) {
        let Some(relation) = self.relation_of(member.layer_id, member.relation_id) else {
            return;
        };
        if let Some(after) =
            osm_gpui::relations::move_member(&relation.members, member.index, delta)
        {
            self.change_relation_members(member.layer_id, member.relation_id, after);
        }
    }

    /// Put the way members of a relation into connected order as one undo
    /// step, or say in the status bar that there is nothing to change.
    pub(crate) fn sort_relation_members(&mut self, layer_id: LayerId, relation_id: i64) {
        let Some(relation) = self.relation_of(layer_id, relation_id) else {
            return;
        };
        let Some(editable) = self
            .layer_manager
            .find_layer(layer_id)
            .and_then(|layer| layer.as_editable())
        else {
            return;
        };
        let sorted = osm_gpui::relation_sort::sort_members(&relation.members, &|way| {
            editable.way_node_ids(way)
        });
        if sorted.members == relation.members {
            self.set_status(tr!("relation_sort.already"));
            return;
        }
        let chains = sorted.chains;
        self.change_relation_members(layer_id, relation_id, sorted.members);
        self.set_status(tr_count!("relation_sort.done", chains));
    }

    /// Remove member `member.index` from its relation, or say in the status
    /// bar why that is refused.
    pub(crate) fn remove_relation_member(&mut self, member: MemberRef) {
        let Some(relation) = self.relation_of(member.layer_id, member.relation_id) else {
            return;
        };
        match osm_gpui::relations::remove_member_at(&relation, member.index) {
            Ok(Some(after)) => {
                self.change_relation_members(member.layer_id, member.relation_id, after)
            }
            Ok(None) => {}
            Err(e) => self.set_status(e.remove_message()),
        }
    }

    /// Give member `member.index` the role `role`.
    fn set_relation_member_role(&mut self, member: MemberRef, role: &str) {
        let Some(relation) = self.relation_of(member.layer_id, member.relation_id) else {
            return;
        };
        if let Some(after) =
            osm_gpui::relations::set_member_role(&relation.members, member.index, role)
        {
            self.change_relation_members(member.layer_id, member.relation_id, after);
        }
    }

    /// If a role click recorded a pending edit, build its input now. Called
    /// from `Render::render`, for the reason `check_for_pending_tag_edit`
    /// gives.
    fn check_for_pending_role_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputState, SelectAll};

        let Some((member, original)) = self.pending_role_edit_open.take() else {
            return;
        };
        if self.edits_blocked(cx) {
            return;
        }
        // Finish any edit that is still open.
        self.commit_role_edit(cx);

        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder(tr!("panel.members.role"));
            state.set_value(original, window, cx);
            state
        });
        cx.subscribe_in(&input, window, |this, input, event, window, cx| {
            this.on_role_input_event(input, event, window, cx);
        })
        .detach();
        input.update(cx, |state, cx| state.focus(window, cx));
        window.on_next_frame(|window, cx| {
            window.dispatch_action(Box::new(SelectAll), cx);
        });
        self.role_edit = Some(RoleEdit { member, input });
        cx.notify();
    }

    /// Enter and loss of focus commit the open role edit.
    fn on_role_input_event(
        &mut self,
        input: &gpui::Entity<gpui_component::input::InputState>,
        event: &gpui_component::input::InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::InputEvent;

        // Ignore late events from an edit that was already finished.
        if self.role_edit.as_ref().map(|edit| &edit.input) != Some(input) {
            return;
        }
        match event {
            InputEvent::PressEnter { .. } => {
                self.commit_role_edit(cx);
                window.focus(&self.focus_handle, cx);
            }
            InputEvent::Blur => self.commit_role_edit(cx),
            _ => return,
        }
        cx.notify();
    }

    /// Apply the open role edit, if any, and close it. An empty role is a
    /// valid role.
    fn commit_role_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.role_edit.take() else {
            return;
        };
        let role = edit.input.read(cx).value().trim().to_string();
        self.set_relation_member_role(edit.member, &role);
    }

    /// Abandon the open role edit (Escape) and hand focus back to the map.
    pub(crate) fn cancel_role_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.role_edit.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    /// Delete every currently-selected feature (Delete/Backspace key, and
    /// Edit > Delete). Runs the "delete" entry of the selection-action table;
    /// silent when nothing is selected, so a stray key press doesn't flash a
    /// status message. Node deletion is refused per feature if the node is
    /// still referenced by a way (see `OsmLayer::delete_feature`).
    fn delete_selected_features(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            return;
        }
        self.run_selection_action("delete", cx);
    }

    /// Run the selection action `id` on the current selection: refuse it
    /// during an upload if it edits, check it is applicable (else show why in
    /// the status bar), carry it out as its `Run` says, and record the result
    /// as a single undo step. Every entry point (menus, shortcuts, the context
    /// menu) ends here, so this is the one place the upload guard lives.
    fn run_selection_action(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(action) = selection_actions::find(id) else {
            return;
        };
        if action.run.edits() && self.edits_blocked(cx) {
            return;
        }
        // A rotation in progress owns the selection until it is applied or
        // cancelled (Enter, click, Escape).
        if self.rotate_session.is_some() {
            return;
        }
        let env = selection_actions::ActionEnv {
            selection: &self.selected,
            active_layer: self.active_layer,
            layers: &self.layer_manager,
            clipboards: &self.clipboards,
        };
        if !(action.is_applicable)(&env) {
            let why = action.why_not(&env);
            self.set_status(why);
            cx.notify();
            return;
        }
        match action.run {
            selection_actions::Run::Apply(apply) => {
                let result = apply(&mut self.layer_manager, &self.selected, self.active_layer);
                self.finish_selection_action(result);
            }
            selection_actions::Run::TagClipboard(apply) => {
                let result = apply(
                    &mut self.layer_manager,
                    &self.selected,
                    self.active_layer,
                    &mut self.clipboards,
                );
                self.finish_selection_action(result);
            }
            selection_actions::Run::Clipboard(op) => self.run_clipboard_op(op, cx),
            selection_actions::Run::Dialog(dialog) => match dialog {
                // `render` opens the dialog (it needs the window); the answer
                // comes back through `add_selection_to_relation` or
                // `create_parallel_ways`.
                selection_actions::Dialog::RelationPicker => self.pending_relation_picker = true,
                selection_actions::Dialog::ParallelOffset => self.pending_parallel_dialog = true,
            },
            selection_actions::Run::Interactive(selection_actions::Interactive::Rotate) => {
                self.start_rotate()
            }
        }
        cx.notify();
    }

    /// Start an interactive Rotate of the selection: the nodes of the selected
    /// nodes and ways turn around their mean position as the pointer circles
    /// it (Shift snaps to 15 degrees). A click or Enter applies it as one
    /// undo step; Escape puts everything back.
    fn start_rotate(&mut self) {
        let Some(layer) = self.active_layer else {
            return;
        };
        let Some(editable) = self
            .layer_manager
            .find_layer(layer)
            .and_then(|l| l.as_editable())
        else {
            return;
        };
        let originals = selection_actions::rotatable_nodes(editable, layer, &self.selected);
        let points: Vec<_> = originals.iter().map(|&(_, p)| p).collect();
        if !osm_gpui::rotate::can_rotate(&points) {
            self.set_status(tr!("action.rotate.hint"));
            return;
        }
        let Some(center) = osm_gpui::rotate::center(&points) else {
            return;
        };
        let start_angle = self
            .last_mouse_pos
            .map(|p| osm_gpui::rotate::pointer_angle(self.rotate_center_px(center), pointer_px(p)));
        self.status_message = None;
        self.rotate_session = Some(RotateSession {
            layer,
            originals,
            center,
            start_angle,
            angle: 0.0,
            moved: false,
        });
    }

    /// The rotation centre in map pixels under the current viewport.
    fn rotate_center_px(&self, center: osm_gpui::geometry::LatLon) -> (f64, f64) {
        pointer_px(self.viewport.geo_to_screen(center.0, center.1))
    }

    /// Turn the nodes of the Rotate in progress to follow the pointer.
    fn update_rotate(&mut self, pointer: gpui::Point<gpui::Pixels>, snap: bool) {
        let Some(session) = self.rotate_session.as_ref() else {
            return;
        };
        let now = osm_gpui::rotate::pointer_angle(
            self.rotate_center_px(session.center),
            pointer_px(pointer),
        );
        let Some(session) = self.rotate_session.as_mut() else {
            return;
        };
        let start = *session.start_angle.get_or_insert(now);
        let mut angle = osm_gpui::rotate::rotation_for_pointer(start, now);
        if snap {
            angle = osm_gpui::rotate::snap_angle(angle, osm_gpui::rotate::SNAP_STEP_DEG);
        }
        session.moved = true;
        session.angle = angle;
        self.apply_rotation();
    }

    /// Move the session's nodes to `originals` turned by the session's angle.
    fn apply_rotation(&mut self) {
        let Some(session) = self.rotate_session.as_ref() else {
            return;
        };
        let before: Vec<_> = session.originals.iter().map(|&(_, p)| p).collect();
        let after = osm_gpui::rotate::rotate_points(&before, session.center, session.angle);
        let moves: Vec<(i64, f64, f64)> = session
            .originals
            .iter()
            .zip(&after)
            .map(|(&(id, _), &(lat, lon))| (id, lat, lon))
            .collect();
        let layer = session.layer;
        if let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer)
            .and_then(|l| l.as_editable_mut())
        {
            if let Err(e) = editable.commit_node_moves(&moves) {
                eprintln!("rotating nodes was refused: {e}");
            }
        }
    }

    /// The Rotate in progress, in degrees clockwise as the user sees it.
    fn rotate_degrees(&self) -> Option<f64> {
        self.rotate_session.as_ref().map(|s| -s.angle.to_degrees())
    }

    /// What the status bar says while a Rotate is in progress.
    fn rotate_status(&self) -> Option<SharedString> {
        let degrees = self.rotate_degrees()?;
        Some(tr!(
            "action.rotate.status",
            degrees = format!("{degrees:.0}")
        ))
    }

    /// Abandon the Rotate in progress, putting the nodes back. Returns
    /// whether there was one.
    fn cancel_rotate(&mut self) -> bool {
        let Some(session) = self.rotate_session.as_mut() else {
            return false;
        };
        session.angle = 0.0;
        self.apply_rotation();
        if let Some(session) = self.rotate_session.take() {
            if let Some(layer) = self
                .layer_manager
                .find_layer_mut(session.layer)
                .and_then(|l| l.as_osm_mut())
            {
                layer.refresh_modified();
            }
        }
        true
    }

    /// Apply the Rotate in progress as one undo step.
    fn finish_rotate(&mut self) {
        let Some(session) = self.rotate_session.take() else {
            return;
        };
        let degrees = -session.angle.to_degrees();
        let before: Vec<_> = session.originals.iter().map(|&(_, p)| p).collect();
        let after = osm_gpui::rotate::rotate_points(&before, session.center, session.angle);
        let entries: Vec<NodeMoveUndoEntry> = session
            .originals
            .iter()
            .zip(&after)
            .filter(|((_, b), a)| b != *a)
            .map(|(&(id, b), &a)| (id, b, a))
            .collect();
        if entries.is_empty() {
            if let Some(layer) = self
                .layer_manager
                .find_layer_mut(session.layer)
                .and_then(|l| l.as_osm_mut())
            {
                layer.refresh_modified();
            }
            return;
        }
        self.undo_stack.push(UndoableAction::Batch {
            label: tr!("history.rotated").to_string(),
            actions: vec![UndoableAction::MoveNodes {
                select: None,
                per_layer: vec![(session.layer, entries)],
            }],
        });
        self.set_status(tr!("action.rotate.done", degrees = format!("{degrees:.0}")));
    }

    /// Record what a selection action produced: its undo step, selection
    /// change and status message, or the reason it refused.
    fn finish_selection_action(
        &mut self,
        result: Result<selection_actions::ActionOutcome, String>,
    ) {
        match result {
            Ok(outcome) => {
                if let Some(undo) = outcome.undo {
                    self.undo_stack.push(undo);
                }
                if outcome.clear_selection {
                    self.set_selection(Vec::new());
                }
                if let Some(selection) = outcome.select {
                    self.set_selection(selection);
                }
                if let Some(from) = outcome.continue_way {
                    self.continue_way_in_add_mode(from);
                }
                self.set_status(outcome.status);
            }
            Err(message) => self.set_status(message),
        }
    }

    /// Open the "Add to Relation" picker if the action asked for it, listing
    /// the relations of the active layer. Called from `Render::render`, as the
    /// dialog needs the window.
    fn check_for_pending_relation_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use osm_gpui::ui::relation_picker_dialog::{DialogEvent, RelationPickerDialog};

        if !std::mem::take(&mut self.pending_relation_picker)
            || self.relation_picker_dialog.is_some()
        {
            return;
        }
        let Some(editable) = self
            .active_layer
            .and_then(|id| self.layer_manager.find_layer(id))
            .and_then(|layer| layer.as_editable())
        else {
            return;
        };
        let relations: Vec<osm_gpui::osm::OsmRelation> = editable
            .relation_ids()
            .into_iter()
            .filter_map(|id| editable.relation(id))
            .collect();
        let choices = osm_gpui::relations::relation_choices(&relations);
        let dialog = cx.new(|cx| RelationPickerDialog::new(choices, window, cx));
        cx.subscribe(
            &dialog,
            |this: &mut Self, _entity, event: &DialogEvent, cx| {
                match event {
                    DialogEvent::Cancelled => {}
                    DialogEvent::Submitted(relation_id) => {
                        this.add_selection_to_relation(*relation_id);
                    }
                }
                this.relation_picker_dialog = None;
                cx.notify();
            },
        )
        .detach();
        self.relation_picker_dialog = Some(dialog);
        cx.notify();
    }

    /// Open the "Create Parallel Way" dialog if the action asked for it.
    /// Called from `Render::render`, as the dialog needs the window.
    fn check_for_pending_parallel_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use osm_gpui::ui::parallel_dialog::{DialogEvent, ParallelDialog};

        if !std::mem::take(&mut self.pending_parallel_dialog) || self.parallel_dialog.is_some() {
            return;
        }
        let initial = self.parallel_params;
        let dialog = cx.new(|cx| ParallelDialog::new(initial, window, cx));
        cx.subscribe(
            &dialog,
            |this: &mut Self, _entity, event: &DialogEvent, cx| {
                match event {
                    DialogEvent::Cancelled => {}
                    DialogEvent::Submitted(params) => {
                        this.parallel_params = *params;
                        this.create_parallel_ways(*params);
                    }
                }
                this.parallel_dialog = None;
                cx.notify();
            },
        )
        .detach();
        self.parallel_dialog = Some(dialog);
        cx.notify();
    }

    /// Create the parallel copies of the selected ways as one undo step (the
    /// dialog's answer).
    fn create_parallel_ways(&mut self, params: osm_gpui::parallel::ParallelParams) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let result = selection_actions::create_parallel_ways(
            &mut self.layer_manager,
            &self.selected,
            self.active_layer,
            params,
        );
        self.finish_selection_action(result);
    }

    /// Append the selected nodes and ways to relation `relation_id` as one
    /// undo step (the picker's choice).
    fn add_selection_to_relation(&mut self, relation_id: i64) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let result = selection_actions::add_to_relation(
            &mut self.layer_manager,
            &self.selected,
            self.active_layer,
            relation_id,
        );
        self.finish_selection_action(result);
    }

    /// Menu bar (Tools) and context menu entry point for the selection
    /// actions. Registered window-scoped on the root element in `render`.
    fn on_run_selection_action(
        &mut self,
        action: &RunSelectionAction,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_selection_action(&action.id, cx);
    }

    /// Switch to Add mode already drawing the existing way `from.way_id`:
    /// the next click attaches a node next to `from.from_node` (before the
    /// way's first node if `from.at_start`), as if the user had just placed
    /// that end node while drawing.
    fn continue_way_in_add_mode(&mut self, from: osm_gpui::topology::Continuation) {
        self.mode = EditMode::Add;
        self.hovered = None;
        self.clear_vertex_preview();
        self.reset_mode_progress();
        self.building_reference = None;
        self.add_progress = Some(AddProgress {
            way_id: Some(from.way_id),
            last_node_id: from.from_node,
            at_start: from.at_start,
        });
    }

    /// Replace the selection, resetting the Fields section's per-feature
    /// editing state so nothing from the previous selection leaks through.
    fn set_selection(&mut self, selection: Vec<osm_gpui::selection::FeatureRef>) {
        self.selected = selection;
        // The Members list belongs to the selected relation.
        self.role_edit = None;
        self.pending_role_edit_open = None;
        self.members_show_all = false;
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }

    /// Select relation `id` of `layer_id` on its own (relations can't be
    /// clicked on the map; this is the side panel's way in).
    pub(crate) fn select_relation(&mut self, layer_id: LayerId, id: i64) {
        self.set_selection(vec![osm_gpui::selection::FeatureRef {
            layer_id,
            kind: osm_gpui::selection::FeatureKind::Relation,
            id,
        }]);
    }

    /// Edit > Select All: every node, way and relation in the active layer.
    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let all = self
            .active_layer
            .and_then(|id| self.layer_manager.find_layer(id))
            .and_then(|layer| layer.as_editable())
            .map(|editable| editable.all_features())
            .unwrap_or_default();
        self.set_selection(all);
        cx.notify();
    }

    fn on_deselect_all(&mut self, _: &DeselectAll, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            self.set_selection(Vec::new());
            cx.notify();
        }
    }

    /// Run `query` over the active data layer: every match, in `all_features`
    /// order. `None` when there is no data layer to search.
    fn search_active_layer(
        &self,
        query: &osm_gpui::search::Query,
        only_selected: bool,
    ) -> Option<Vec<osm_gpui::selection::FeatureRef>> {
        let layer_id = self.active_layer?;
        let editable = self.layer_manager.find_layer(layer_id)?.as_editable()?;
        let selected: std::collections::HashSet<_> = self
            .selected
            .iter()
            .filter(|f| f.layer_id == layer_id)
            .map(|f| (f.kind, f.id))
            .collect();
        let view = Some(self.viewport.visible_bounds());
        Some(editable.search_features(query, &selected, only_selected, view))
    }

    /// How many features `text` matches in the active layer, as the Find
    /// dialog's live count (`None` if it does not parse or there is no layer).
    fn find_match_count(&self, text: &str, mode: osm_gpui::search::SearchMode) -> Option<usize> {
        let query = osm_gpui::search::parse(text).ok()?;
        self.search_active_layer(
            &query,
            mode == osm_gpui::search::SearchMode::FindInSelection,
        )
        .map(|found| found.len())
    }

    /// Whether any selected feature is (partly) inside the map view.
    fn selection_in_view(&self) -> bool {
        let Ok(in_view) = osm_gpui::search::parse("inview") else {
            return false;
        };
        self.search_active_layer(&in_view, true)
            .is_some_and(|found| !found.is_empty())
    }

    /// Apply a Find query in `mode`: the selection becomes the result
    /// combined with the old selection as the mode says, the status bar
    /// reports it, and the view moves only if nothing selected is in view.
    /// Returns how many features matched, or why the query did not parse.
    fn apply_find(
        &mut self,
        text: &str,
        mode: osm_gpui::search::SearchMode,
    ) -> Result<usize, osm_gpui::search::ParseError> {
        use osm_gpui::search::SearchMode;

        let query = osm_gpui::search::parse(text)?;
        let Some(found) = self.search_active_layer(&query, mode == SearchMode::FindInSelection)
        else {
            self.set_status(tr!("status.find_no_layer"));
            return Ok(0);
        };
        let next = osm_gpui::search::combine(mode, &self.selected, &found);
        self.set_selection(next);
        if found.is_empty() {
            self.set_status(tr!("status.find_no_matches"));
        } else {
            self.set_status(tr_count!("status.find_selected", self.selected.len()));
            if !self.selected.is_empty() && !self.selection_in_view() {
                let selected = self.selected.clone();
                self.fit_to_features(&selected);
            }
        }
        Ok(found.len())
    }

    /// Edit > Find: open the query dialog on the active data layer.
    fn on_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_dialog.is_some() {
            return;
        }
        let other_dialog_open = self.any_dialog_open(cx);
        if other_dialog_open {
            return;
        }
        let has_data_layer = self
            .active_layer
            .and_then(|id| self.layer_manager.find_layer(id))
            .is_some_and(|layer| layer.as_editable().is_some());
        if !has_data_layer {
            self.set_status(tr!("status.find_no_layer"));
            cx.notify();
            return;
        }

        let history = self.find_history.entries().to_vec();
        let mode = self.find_mode;
        let dialog =
            cx.new(|cx| osm_gpui::ui::find_dialog::FindDialog::new(mode, history, window, cx));
        cx.subscribe(
            &dialog,
            |this: &mut Self, entity, event: &osm_gpui::ui::find_dialog::DialogEvent, cx| {
                use osm_gpui::ui::find_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.find_dialog = None;
                        cx.notify();
                    }
                    DialogEvent::Preview { query, mode } => {
                        if let Some(count) = this.find_match_count(query, *mode) {
                            entity.update(cx, |dialog, cx| dialog.set_count(query, count, cx));
                        }
                    }
                    DialogEvent::Submitted { query, mode } => {
                        this.find_dialog = None;
                        this.find_mode = *mode;
                        this.find_history.push(query);
                        // The dialog parsed it already; an error cannot happen.
                        let _ = this.apply_find(query, *mode);
                        cx.notify();
                    }
                }
            },
        )
        .detach();
        self.find_dialog = Some(dialog);
        cx.notify();
    }

    fn on_delete_selection(&mut self, _: &DeleteSelection, _: &mut Window, cx: &mut Context<Self>) {
        if self.edits_blocked(cx) {
            return;
        }
        self.delete_selected_features(cx);
    }

    /// Zoom by `delta` levels around the center of the map area.
    fn zoom_by(&mut self, delta: f64, cx: &mut Context<Self>) {
        let size = self.viewport.transform.screen_size;
        let center = gpui::point(size.width / 2.0, size.height / 2.0);
        self.viewport.transform.zoom_at_point(center, delta);
        cx.notify();
    }

    fn on_zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(1.0, cx);
    }

    fn on_zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(-1.0, cx);
    }

    /// Pan/zoom so every given feature is in view. Returns `false` (leaving
    /// the viewport untouched) if none of them resolve to a position.
    fn fit_to_features(&mut self, features: &[osm_gpui::selection::FeatureRef]) -> bool {
        use osm_gpui::selection::FeatureKind;

        // (min_lat, max_lat, min_lon, max_lon)
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        let mut extend = |(lat, lon): (f64, f64)| {
            let b = bounds.get_or_insert((lat, lat, lon, lon));
            b.0 = b.0.min(lat);
            b.1 = b.1.max(lat);
            b.2 = b.2.min(lon);
            b.3 = b.3.max(lon);
        };
        for feature in features {
            let Some(editable) = self
                .layer_manager
                .find_layer(feature.layer_id)
                .and_then(|layer| layer.as_editable())
            else {
                continue;
            };
            match feature.kind {
                FeatureKind::Node => {
                    if let Some(pos) = editable.node_lat_lon(feature.id) {
                        extend(pos);
                    }
                }
                FeatureKind::Way => {
                    for node_id in editable.way_node_ids(feature.id).unwrap_or_default() {
                        if let Some(pos) = editable.node_lat_lon(node_id) {
                            extend(pos);
                        }
                    }
                }
                // The box around the members that are loaded.
                FeatureKind::Relation => {
                    let Some(relation) = editable.relation(feature.id) else {
                        continue;
                    };
                    let node = |id: i64| editable.node_lat_lon(id);
                    let way_nodes = |id: i64| editable.way_node_ids(id);
                    let find = |id: i64| editable.relation(id);
                    let lookup = osm_gpui::relations::MemberLookup {
                        node: &node,
                        way_nodes: &way_nodes,
                        relation: &find,
                    };
                    if let Some((min_lat, max_lat, min_lon, max_lon)) =
                        osm_gpui::relations::members_bounds(&relation, &lookup)
                    {
                        extend((min_lat, min_lon));
                        extend((max_lat, max_lon));
                    }
                }
            }
        }

        let Some((min_lat, max_lat, min_lon, max_lon)) = bounds else {
            return false;
        };
        let size = self.viewport.transform.screen_size;
        let (lat, lon, zoom) = osm_gpui::coordinates::fit_bounds_to_viewport(
            min_lat,
            max_lat,
            min_lon,
            max_lon,
            size.width.to_f64(),
            size.height.to_f64(),
        );
        self.viewport.pan_to(lat, lon);
        self.viewport.set_zoom(zoom);
        true
    }

    /// View > Zoom to Data: fit every visible OSM layer's data in view.
    fn on_zoom_to_data(&mut self, _: &ZoomToData, _: &mut Window, cx: &mut Context<Self>) {
        use osm_gpui::selection::FeatureKind;

        let nodes: Vec<osm_gpui::selection::FeatureRef> = self
            .layer_manager
            .layers()
            .iter()
            .filter(|layer| layer.is_visible())
            .filter_map(|layer| layer.as_editable())
            .flat_map(|editable| editable.all_features())
            .filter(|feature| feature.kind == FeatureKind::Node)
            .collect();
        if !self.fit_to_features(&nodes) {
            // No OSM data: fit the visible reference layers instead.
            let extents = self
                .layer_manager
                .layers()
                .iter()
                .filter(|layer| layer.is_visible())
                .filter_map(|layer| layer.as_reference())
                .filter_map(|layer| layer.data().extent())
                .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1), a.2.min(b.2), a.3.max(b.3)));
            match extents {
                Some(extent) => self.fit_to_extent(extent),
                None => self.set_status(tr!("status.no_data_to_zoom")),
            }
        }
        cx.notify();
    }

    fn on_zoom_to_selection(
        &mut self,
        _: &ZoomToSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.selected.clone();
        if !self.fit_to_features(&selected) {
            // A relation none of whose members is loaded selects something
            // but has nowhere to zoom to.
            self.set_status(if selected.is_empty() {
                tr!("status.nothing_selected")
            } else {
                tr!("status.no_data_to_zoom")
            });
        }
        cx.notify();
    }

    fn on_minimize_window(
        &mut self,
        _: &MinimizeWindow,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.minimize_window();
    }

    fn on_zoom_window(&mut self, _: &ZoomWindow, window: &mut Window, _: &mut Context<Self>) {
        window.zoom_window();
    }

    /// Whether an upload is running, which freezes the layers (see
    /// `transfer::blocks_edits`). When it is, say so in the status bar.
    fn edits_blocked(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.refuse_edit_during_upload() {
            return false;
        }
        cx.notify();
        true
    }

    /// The same check for code paths without a `cx` (the edit primitives the
    /// side panel and map gestures end in): true, with the status message
    /// set, while an upload runs. Every way of changing the layers checks
    /// this, so a field typed into, a preset picked or a member reordered
    /// during an upload is refused as well as undo and delete.
    fn refuse_edit_during_upload(&mut self) -> bool {
        if !osm_gpui::transfer::blocks_edits(self.transfer.as_ref()) {
            return false;
        }
        self.set_status(tr!("status.upload_blocks_edits"));
        true
    }

    fn on_undo(&mut self, _: &Undo, _window: &mut Window, cx: &mut Context<Self>) {
        if self.edits_blocked(cx) {
            return;
        }
        self.cancel_rotate();
        if let Some(action) = self.undo_stack.undo() {
            self.run_history_step(&action, false);
            cx.notify();
        }
    }

    /// Whether every layer `action` touches still exists and is editable.
    /// `forget_layer` prunes the history when a layer is deleted, so this
    /// should always hold; it is checked anyway so a step can never be
    /// half-applied (a `Batch` with one part in a missing layer).
    fn history_step_applicable(&self, action: &UndoableAction) -> bool {
        action.layers().iter().all(|id| {
            self.layer_manager
                .find_layer(*id)
                .is_some_and(|l| l.as_editable().is_some())
        })
    }

    /// Apply one undo (`forward = false`) or redo step and select what it
    /// touched; a step whose layer is gone is skipped with a status message.
    fn run_history_step(&mut self, action: &UndoableAction, forward: bool) {
        if !self.history_step_applicable(action) {
            self.set_status(tr!("status.history_step_skipped"));
            return;
        }
        self.apply_undo_action(action, forward);
        self.select_after_history_step(action);
        self.select_after_paste_step(action, forward);
    }

    /// A paste is undone to the selection it started from, and redone to the
    /// pasted features, rather than to "what the step touched" (which is
    /// nothing, after an undo). Left alone while Add or Building mode is
    /// placing points, like `select_after_history_step`.
    fn select_after_paste_step(&mut self, action: &UndoableAction, forward: bool) {
        let UndoableAction::PasteFeatures { pasted } = action else {
            return;
        };
        if self.add_progress.is_some()
            || self.building_progress.is_some()
            || self.arc_progress.is_some()
        {
            return;
        }
        let wanted = if forward {
            &pasted.selection
        } else {
            &pasted.previous_selection
        };
        let next: Vec<_> = wanted
            .iter()
            .copied()
            .filter(|feature| self.feature_exists(feature))
            .collect();
        if next != self.selected {
            self.set_selection(next);
        }
    }

    fn on_redo(&mut self, _: &Redo, _window: &mut Window, cx: &mut Context<Self>) {
        if self.edits_blocked(cx) {
            return;
        }
        self.cancel_rotate();
        if let Some(action) = self.undo_stack.redo() {
            self.run_history_step(&action, true);
            cx.notify();
        }
    }

    /// Whether `feature` currently exists in its layer.
    fn feature_exists(&self, feature: &osm_gpui::selection::FeatureRef) -> bool {
        use osm_gpui::selection::FeatureKind;
        let Some(editable) = self
            .layer_manager
            .find_layer(feature.layer_id)
            .and_then(|layer| layer.as_editable())
        else {
            return false;
        };
        match feature.kind {
            FeatureKind::Node => editable.node_lat_lon(feature.id).is_some(),
            FeatureKind::Way => editable.way_node_ids(feature.id).is_some(),
            FeatureKind::Relation => editable.relation(feature.id).is_some(),
        }
    }

    /// After an undo/redo step, select the features it affected that still
    /// exist (a restored delete, a redone create, a moved/retagged feature).
    /// The selection never keeps ids that no longer exist. While Add or
    /// Building mode is mid-way through placing points the selection is left
    /// alone (apart from that pruning), so the drawing session isn't disturbed.
    /// Never moves the viewport.
    fn select_after_history_step(&mut self, action: &UndoableAction) {
        let drawing = self.add_progress.is_some()
            || self.building_progress.is_some()
            || self.arc_progress.is_some();
        let next: Vec<osm_gpui::selection::FeatureRef> = if drawing {
            self.selected.clone()
        } else {
            action.affected_features()
        };
        let mut next: Vec<_> = next
            .into_iter()
            .filter(|feature| self.feature_exists(feature))
            .collect();
        // A relation is only ever selected on its own (the side panel shows
        // one relation's Members, not a mix). When a step touched a relation
        // only as a side effect (a deleted way leaving or rejoining its
        // members), the nodes and ways win.
        if next
            .iter()
            .any(|f| f.kind != osm_gpui::selection::FeatureKind::Relation)
        {
            next.retain(|f| f.kind != osm_gpui::selection::FeatureKind::Relation);
        }
        if next != self.selected {
            self.set_selection(next);
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
        if self.edits_blocked(cx) {
            return;
        }
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
        if self.edits_blocked(cx) {
            return;
        }
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
        // A closed way is offered both area and line presets, best first.
        let geometries =
            editable.feature_geometry_candidates(&target, osm_gpui::presets::area_keys());
        if geometries.is_empty() {
            return;
        }

        let dialog = cx.new(|cx| {
            osm_gpui::ui::preset_picker_dialog::PresetPickerDialog::new(geometries, window, cx)
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
                    DialogEvent::Submitted {
                        tags,
                        geometry,
                        preset_id,
                    } => {
                        this.apply_preset(&target, tags, geometry);
                        osm_gpui::preset_recents::record(
                            &mut osm_gpui::preset_recents::SettingsRecents,
                            preset_id,
                        );
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

    /// Apply a schema preset chosen in the picker: its tags (see
    /// `presets::preset_tag_changes`, which also adds `area=yes` for an
    /// area preset on a closed way that wouldn't otherwise be an area, and
    /// removes it for a line preset) as one `SetTags` undo step.
    fn apply_preset(
        &mut self,
        target: &osm_gpui::selection::FeatureRef,
        preset_tags: &std::collections::HashMap<String, String>,
        preset_geometry: &[osm_gpui::presets::Geometry],
    ) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(editable) = self
            .layer_manager
            .find_layer(target.layer_id)
            .and_then(|l| l.as_editable())
        else {
            return;
        };
        let Some(existing) = editable.feature_tags(target) else {
            return;
        };
        let existing: std::collections::HashMap<String, String> = existing.into_iter().collect();
        let closed = editable.feature_is_closed_way(target);
        let entries: Vec<_> = osm_gpui::presets::preset_tag_changes(
            preset_tags,
            preset_geometry,
            &existing,
            closed,
            osm_gpui::presets::area_keys(),
        )
        .into_iter()
        .map(|(key, after)| {
            let before = existing.get(&key).cloned();
            (*target, key, before, after)
        })
        .collect();
        if entries.is_empty() {
            return;
        }
        let entries = write_tag_entries(&mut self.layer_manager, entries);
        if entries.is_empty() {
            return;
        }
        self.undo_stack.push(UndoableAction::SetTags { entries });
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
        let values = preset_tags.into_iter().map(|(k, v)| (k, Some(v))).collect();
        self.apply_tag_values(target, values);
    }

    /// A Fields-section edit: set each key to its value, or remove the tag
    /// where the value is empty (clearing a field removes its tag rather than
    /// leaving `key=`). One undo step for all the keys.
    fn apply_field_values(
        &mut self,
        target: &osm_gpui::selection::FeatureRef,
        values: std::collections::HashMap<String, String>,
    ) {
        let values = values
            .into_iter()
            .map(|(k, v)| {
                let v = v.trim().to_string();
                (k, if v.is_empty() { None } else { Some(v) })
            })
            .collect();
        self.apply_tag_values(target, values);
    }

    /// The shared tag write behind [`Self::apply_nsi_preset`] and
    /// [`Self::apply_field_values`]: `Some(v)` sets a key, `None` removes it;
    /// keys that already have the wanted value are skipped, and what changed
    /// is one `SetTags` undo step (none if nothing changed).
    fn apply_tag_values(
        &mut self,
        target: &osm_gpui::selection::FeatureRef,
        preset_tags: std::collections::HashMap<String, Option<String>>,
    ) {
        if self.refuse_edit_during_upload() {
            return;
        }
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
                let after = preset_tags.get(key).cloned().flatten();
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

        let entries = write_tag_entries(&mut self.layer_manager, entries);
        if entries.is_empty() {
            return;
        }
        self.undo_stack.push(UndoableAction::SetTags { entries });
    }

    /// The dialog host's focus rule: while a modal dialog is open, focus is
    /// inside it. Called from `render`, after every action and effect of the
    /// frame has run, so it also undoes a focus grab that came later than the
    /// dialog's own (the in-window menu bar gives focus back to the element it
    /// took it from once an item is chosen). Skipped while a menu is open,
    /// which owns the keyboard then. Returns whether focus moved.
    fn keep_dialog_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // Asking the dispatch tree anything before the first frame is drawn
        // trips an assertion; a dialog open at launch focused itself anyway.
        if !std::mem::replace(&mut self.drawn_once, true) {
            return false;
        }
        if menu::in_window_menu_open(window) {
            return false;
        }
        let open = self.open_modals(cx);
        osm_gpui::ui::modal::ensure_modal_focus(&open, window, cx)
    }

    /// Every open modal dialog, in mount order, so the last one is the one
    /// drawn on top. The only list of dialogs: focus, rendering and
    /// `any_dialog_open` all derive from it.
    fn open_modals(&self, cx: &App) -> Vec<osm_gpui::ui::modal::OpenModal> {
        use osm_gpui::ui::modal::open_modal;
        [
            open_modal(&self.custom_imagery_dialog, cx),
            open_modal(&self.quit_confirm_dialog, cx),
            open_modal(&self.delete_layer_dialog, cx),
            open_modal(&self.restore_dialog, cx),
            open_modal(&self.find_dialog, cx),
            open_modal(&self.about_dialog, cx),
            open_modal(&self.nsi_dialog, cx),
            open_modal(&self.preset_picker_dialog, cx),
            open_modal(&self.relation_picker_dialog, cx),
            open_modal(&self.parallel_dialog, cx),
            open_modal(&self.upload_dialog, cx),
            open_modal(&self.sign_in_dialog, cx),
            open_modal(&self.notice_dialog, cx),
            open_modal(&self.new_note_dialog_element(), cx),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Whether a modal dialog is open (the vertex preview is suppressed).
    fn any_dialog_open(&self, cx: &App) -> bool {
        !self.open_modals(cx).is_empty()
    }

    /// Drop the vertex preview (the pointer left the map, or the mode changed).
    fn clear_vertex_preview(&mut self) {
        self.layer_manager.update_vertex_preview(
            &self.viewport,
            gpui::point(px(0.0), px(0.0)),
            None,
            false,
        );
    }

    /// What `screen_pt` would snap onto in `layer_id`, for something with
    /// tags `sources` (see `osm_gpui::snap::may_snap`). `node` is the node
    /// being moved, if any: never a target itself, and no way that already
    /// contains it is one. With `with_nodes`, an existing node under the
    /// pointer wins over a way segment.
    fn find_snap_target(
        &self,
        layer_id: LayerId,
        screen_pt: gpui::Point<gpui::Pixels>,
        node: Option<i64>,
        sources: &[Tags],
        with_nodes: bool,
    ) -> Option<SnapTarget> {
        let layer = self.layer_manager.find_layer(layer_id)?;
        let osm_layer = layer.as_osm()?;
        let editable = layer.as_editable()?;
        if with_nodes {
            let hit = osm_layer.snap_to_node_where(
                &self.viewport,
                screen_pt,
                osm_gpui::layers::node_markers::FULL_HIT_RADIUS,
                |n| snap_node_ok(editable, layer_id, sources, node, n),
            );
            if let Some(id) = hit {
                let (lat, lon) = editable.node_lat_lon(id)?;
                return Some(SnapTarget::Node { id, lat, lon });
            }
        }
        let (way_id, a, b, _loaded_index, lat, lon) = osm_layer.snap_to_way_where(
            &self.viewport,
            screen_pt,
            osm_gpui::snap::WAY_SNAP_TOLERANCE_PX,
            |w| snap_way_ok(editable, layer_id, sources, node, w),
        )?;
        // `index` counts only the way's nodes that are loaded, so with some
        // missing (a partial download) it is not a position in the node
        // list. Find the segment by its two end nodes instead; if they are
        // not neighbours there, the stretch between them is not loaded and
        // nothing can be dropped on it.
        let index = editable
            .way_node_ids(way_id)?
            .windows(2)
            .position(|w| w[0] == a && w[1] == b)?;
        Some(SnapTarget::Way {
            way_id,
            a: editable.node_lat_lon(a)?,
            b: editable.node_lat_lon(b)?,
            index,
            lat,
            lon,
        })
    }

    /// The single node a move-drag is carrying, with its layer, if it is
    /// just one node (a way or a multi-selection drag never snaps).
    fn single_dragged_node(targets: &NodeMoveTargets) -> Option<(LayerId, i64)> {
        match targets.as_slice() {
            [(layer, nodes)] => match nodes.as_slice() {
                [(id, _, _)] => Some((*layer, *id)),
                _ => None,
            },
            _ => None,
        }
    }

    /// What a single dragged node released at `pos` snaps onto, honouring the
    /// snapping-off modifiers.
    fn drag_snap_target(
        &self,
        pos: gpui::Point<gpui::Pixels>,
        modifiers: gpui::Modifiers,
        dragged: (LayerId, i64),
    ) -> Option<SnapTarget> {
        let (layer_id, node) = dragged;
        if osm_gpui::snap::snapping_disabled(modifiers.alt, modifiers.control)
            || self.active_layer != Some(layer_id)
        {
            return None;
        }
        let sources = self
            .layer_manager
            .find_layer(layer_id)
            .and_then(|l| l.as_editable())
            .map(|e| snap_node_sources(e, layer_id, node))?;
        self.find_snap_target(layer_id, pos, Some(node), &sources, true)
    }

    /// Refresh `snap_preview` for a pointer at `pos`: while drawing in Add
    /// mode, or while a single node is dragged. Cleared whenever snapping is
    /// off (a modifier is held) or nothing is in reach. Returns whether the
    /// highlight changed.
    fn update_snap_preview(
        &mut self,
        pos: gpui::Point<gpui::Pixels>,
        modifiers: gpui::Modifiers,
        dragged: Option<(LayerId, i64)>,
    ) -> bool {
        let target = if let Some(dragged) = dragged {
            self.drag_snap_target(pos, modifiers, dragged)
        } else if self.mode == EditMode::Add
            && !osm_gpui::snap::snapping_disabled(modifiers.alt, modifiers.control)
        {
            self.active_layer.and_then(|layer_id| {
                let sources = self.add_snap_sources(layer_id);
                self.find_snap_target(layer_id, pos, None, &sources, self.add_progress.is_some())
            })
        } else {
            None
        };
        let changed = self.snap_preview != target;
        self.snap_preview = target;
        changed
    }

    /// The tags that decide what Add mode may snap onto: those of the way
    /// being drawn, if it has any. A way with none yet (the usual case; tags
    /// come after drawing) snaps to anything, see `osm_gpui::snap`.
    fn add_snap_sources(&self, layer_id: LayerId) -> Vec<Tags> {
        let Some(way_id) = self.add_progress.as_ref().and_then(|p| p.way_id) else {
            return Vec::new();
        };
        self.layer_manager
            .find_layer(layer_id)
            .and_then(|l| l.as_editable())
            .and_then(|e| {
                e.feature_tags(&osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Way,
                    id: way_id,
                })
            })
            .map(|t| vec![t])
            .unwrap_or_default()
    }

    /// Recompute the snap highlight for a pointer at `pos` (map-local):
    /// while a single node is dragged with the left button, or while hovering
    /// in Add mode; otherwise it is cleared.
    fn refresh_snap_preview(
        &mut self,
        pos: gpui::Point<gpui::Pixels>,
        modifiers: gpui::Modifiers,
        left_pressed: bool,
        no_button: bool,
    ) {
        let dragged = match &self.interaction {
            Interaction::MoveDrag { targets, .. } if left_pressed => {
                Self::single_dragged_node(targets)
            }
            _ => None,
        };
        if dragged.is_some() || (self.mode == EditMode::Add && no_button) {
            self.update_snap_preview(pos, modifiers, dragged);
        } else {
            self.snap_preview = None;
        }
    }

    /// A modifier key went down or up with the pointer standing still: the
    /// snap highlight depends on Alt / Ctrl (they turn snapping off), so
    /// refresh it from the last pointer position instead of waiting for the
    /// next mouse move.
    fn handle_modifiers_changed(&mut self, modifiers: gpui::Modifiers, cx: &mut Context<Self>) {
        self.last_modifiers = modifiers;
        if self.mode == EditMode::Improve {
            // The preview depends on the held keys.
            cx.notify();
        }
        let Some(pos) = self.last_mouse_pos else {
            return;
        };
        // A held move-drag means the left button is down.
        let dragging = matches!(self.interaction, Interaction::MoveDrag { .. });
        let idle = matches!(self.interaction, Interaction::Idle);
        let before = self.snap_preview.clone();
        self.refresh_snap_preview(pos, modifiers, dragging, idle);
        if self.snap_preview != before {
            cx.notify();
        }
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let adjusted_position = self.window_to_map(event.position);
        let left_pressed = event.pressed_button == Some(gpui::MouseButton::Left);
        self.last_mouse_pos = Some(adjusted_position);
        self.last_modifiers = event.modifiers;
        // Rotating: the selection follows the pointer around its centre.
        if self.rotate_session.is_some() {
            self.update_rotate(adjusted_position, event.modifiers.shift);
            cx.notify();
            return;
        }
        // What a click here would select — only meaningful in Select mode
        // with no button down and no pan pending.
        let hover_enabled = self.mode == EditMode::Select
            && event.pressed_button.is_none()
            && !self.space_held
            && self.offset_adjust.is_none();
        let hit_here = |this: &Self| {
            osm_gpui::selection::resolve_hits(
                this.layer_manager
                    .hit_test_all(&this.viewport, adjusted_position),
            )
        };
        self.hovered = if hover_enabled { hit_here(self) } else { None };
        // Which way's vertices to preview. Nothing to do while a move-drag
        // holds the button (the dragged node's way stays previewed); a press
        // that is not a move-drag (a box-select) or any suppressing state
        // (other modes, imagery offset, an open dialog) clears it. When the
        // preview way changes, its vertices just became targets, so the
        // hover is resolved once more; otherwise there is no second scan.
        let move_dragging = event.pressed_button.is_some()
            && matches!(self.interaction, Interaction::MoveDrag { .. });
        if !move_dragging {
            let enabled = hover_enabled && !self.any_dialog_open(cx);
            let changed = self.layer_manager.update_vertex_preview(
                &self.viewport,
                adjusted_position,
                self.hovered.as_ref(),
                enabled,
            );
            if changed
                && enabled
                && self
                    .hovered
                    .is_some_and(|h| h.kind == osm_gpui::selection::FeatureKind::Way)
            {
                self.hovered = hit_here(self);
            }
        }
        // Highlight what a click / release here would snap onto.
        self.refresh_snap_preview(
            adjusted_position,
            event.modifiers,
            left_pressed,
            event.pressed_button.is_none(),
        );
        // Repaint on every move: the status bar shows the cursor's
        // coordinates, and Add/Building/Extrude draw a live preview to it.
        cx.notify();

        // Adjusting the imagery offset: a left-drag moves the imagery.
        if self.offset_adjust_mouse_move(adjusted_position, left_pressed, cx) {
            return;
        }

        if let Interaction::MoveDrag { down, targets } = &self.interaction {
            if let Some(delta_pt) =
                interaction::move_drag_delta(*down, to_pt(adjusted_position), left_pressed)
            {
                let delta = from_pt(delta_pt);
                let targets = targets.clone();
                // A grabbed vertex becomes the selection once the press has
                // really become a drag.
                if delta_pt.0.hypot(delta_pt.1) >= interaction::DRAG_THRESHOLD {
                    if let Some(grabbed) = self.vertex_grab.take() {
                        self.set_selection(vec![grabbed]);
                    }
                }
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

        // A pan-drag whose button was released outside the map never saw its
        // mouse-up; end it here rather than panning with no button held.
        if self.viewport.is_dragging && event.pressed_button.is_none() {
            self.viewport.handle_mouse_up();
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
        if self.rotate_session.is_some() {
            // A click applies the rotation, once the pointer has moved (the
            // click that chose Rotate from a menu does not).
            if self.rotate_session.as_ref().is_some_and(|r| r.moved) {
                self.finish_rotate();
            }
            cx.notify();
            return;
        }
        self.viewport.handle_mouse_up();

        if self.offset_adjust_mouse_up() {
            cx.notify();
            return;
        }

        if let Some(press) = self.building_press.take() {
            self.finish_oriented_building_press(press, up_pos);
            cx.notify();
            return;
        }

        if let Some(drag) = self.extrude_drag.take() {
            let moved = (up_pos - drag.down).magnitude() >= 4.0;
            if moved {
                match drag.style {
                    ExtrudeStyle::NewWay => self.commit_extrude(&drag, up_pos),
                    ExtrudeStyle::Reshape | ExtrudeStyle::Move => {
                        self.commit_extrude_reshape(&drag, up_pos)
                    }
                }
            } else if event.click_count == 2 {
                self.insert_node_on_segment(&drag, up_pos);
            }
            cx.notify();
            return;
        }

        match interaction::on_mouse_up(&mut self.interaction, to_pt(up_pos)) {
            interaction::Gesture::MoveCommitted { targets, delta } => {
                if let Some(grabbed) = self.vertex_grab.take() {
                    self.set_selection(vec![grabbed]);
                }
                let delta = from_pt(delta);
                for (layer_id, _) in &targets {
                    if let Some(layer) = self.layer_manager.find_layer_mut(*layer_id) {
                        if let Some(editable) = layer.as_editable_mut() {
                            editable.clear_drag_preview();
                        }
                    }
                }
                if self.refuse_edit_during_upload() {
                    cx.notify();
                    return;
                }

                // A single node released on a way or another node snaps onto
                // it (unless a snapping-off modifier is held).
                let snap = Self::single_dragged_node(&targets).and_then(|dragged| {
                    self.drag_snap_target(up_pos, event.modifiers, dragged)
                        .map(|target| (dragged, target))
                });
                self.snap_preview = None;
                if let Some((
                    (layer_id, node),
                    SnapTarget::Way {
                        way_id,
                        index,
                        lat,
                        lon,
                        ..
                    },
                )) = &snap
                {
                    let joined = self
                        .layer_manager
                        .find_layer_mut(*layer_id)
                        .and_then(|l| l.as_editable_mut())
                        .map(|editable| {
                            selection_actions::join_node_into_way(
                                editable,
                                *layer_id,
                                *node,
                                *way_id,
                                Some(*index),
                                Some((*lat, *lon)),
                            )
                        });
                    match joined {
                        Some(Ok(actions)) => {
                            let label = tr!("action.join_node_way.done").to_string();
                            self.undo_stack.push(UndoableAction::Batch {
                                label: label.clone(),
                                actions,
                            });
                            self.set_status(label);
                            cx.notify();
                            return;
                        }
                        // Refused (way full, ...): the node just moves.
                        Some(Err(message)) => self.set_status(message),
                        None => {}
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
                        let (mut new_lat, mut new_lon) = self.viewport.screen_to_geo(new_screen);
                        // Onto an existing node: land exactly on it.
                        if let Some((_, SnapTarget::Node { lat, lon, .. })) = &snap {
                            (new_lat, new_lon) = (*lat, *lon);
                        }
                        moves.push((id, new_lat, new_lon));
                        undo_entries.push((id, (lat, lon), (new_lat, new_lon)));
                    }
                    let moved = self
                        .layer_manager
                        .find_layer_mut(*layer_id)
                        .and_then(|layer| layer.as_editable_mut())
                        .map(|editable| editable.commit_node_moves(&moves));
                    match moved {
                        Some(Ok(())) => undo_per_layer.push((*layer_id, undo_entries)),
                        Some(Err(e)) => eprintln!("moving nodes was refused: {e}"),
                        None => {}
                    }
                }
                if undo_per_layer.is_empty() {
                    cx.notify();
                    return;
                }
                let move_action = UndoableAction::MoveNodes {
                    select: (!self.selected.is_empty()).then(|| self.selected.clone()),
                    per_layer: undo_per_layer,
                };
                if let Some(((layer_id, node), SnapTarget::Node { id: target, .. })) = snap {
                    // Merge the dropped node with the one it landed on, as
                    // one undo step with the move.
                    let pick = |id| osm_gpui::selection::FeatureRef {
                        layer_id,
                        kind: osm_gpui::selection::FeatureKind::Node,
                        id,
                    };
                    match selection_actions::apply_merge(
                        &mut self.layer_manager,
                        &[pick(node), pick(target)],
                        self.active_layer,
                    ) {
                        Ok(outcome) => {
                            let label = outcome.status.clone();
                            let mut actions = vec![move_action];
                            actions.extend(outcome.undo);
                            self.undo_stack.push(UndoableAction::Batch {
                                label: label.clone(),
                                actions,
                            });
                            if let Some(selection) = outcome.select {
                                self.set_selection(selection);
                            }
                            self.set_status(label);
                        }
                        Err(message) => {
                            self.undo_stack.push(move_action);
                            self.set_status(message);
                        }
                    }
                } else {
                    self.undo_stack.push(move_action);
                }
                cx.notify();
            }
            interaction::Gesture::MoveCancelledAsClick { targets, at } => {
                self.vertex_grab = None;
                for (layer_id, _) in &targets {
                    if let Some(layer) = self.layer_manager.find_layer_mut(*layer_id) {
                        if let Some(editable) = layer.as_editable_mut() {
                            editable.clear_drag_preview();
                        }
                    }
                }
                self.last_modifiers = event.modifiers;
                self.handle_map_click(
                    from_pt(at),
                    event.modifiers.shift,
                    event.click_count,
                    osm_gpui::snap::snapping_disabled(event.modifiers.alt, event.modifiers.control),
                    cx,
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
                self.last_modifiers = event.modifiers;
                self.handle_map_click(
                    from_pt(at),
                    event.modifiers.shift,
                    event.click_count,
                    osm_gpui::snap::snapping_disabled(event.modifiers.alt, event.modifiers.control),
                    cx,
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
        cx: &mut Context<Self>,
    ) {
        // New Note placing its pin, or a click on a note's pin.
        if self.notes_click(screen_pt) {
            return;
        }
        if self.imagery_click(screen_pt) {
            return;
        }
        match self.mode {
            EditMode::Select => self.handle_select_click(screen_pt, shift_held, click_count),
            EditMode::Add => self.handle_add_click(screen_pt, ctrl_held),
            EditMode::Building => self.handle_building_click(screen_pt),
            EditMode::Arc => self.handle_arc_click(screen_pt, ctrl_held),
            EditMode::Improve => self.handle_improve_click(screen_pt, cx),
            EditMode::Extrude => {
                // Extrude works from mouse-down/move/up directly; a stray
                // click here (a zero-movement mouse-up) does nothing.
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

    /// Arc mode: click 1 sets the arc's start, click 2 its end, click 3 a
    /// point the arc passes through and commits it. Each click snaps onto a
    /// nearby way (unless Ctrl/Alt is held).
    fn handle_arc_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>, no_snap: bool) {
        let Some(layer_id) = self.active_layer else {
            return;
        };
        let point = self.arc_point_at(layer_id, screen_pt, no_snap);

        match self.arc_progress.take() {
            None => {
                self.arc_progress = Some(ArcProgress {
                    start: point,
                    end: None,
                });
            }
            Some(ArcProgress { start, end: None }) => {
                self.arc_progress = Some(ArcProgress {
                    start,
                    end: Some(point),
                });
            }
            Some(ArcProgress {
                start,
                end: Some(end),
            }) => {
                // A third point in line with the first two defines no arc;
                // keep waiting for a better one.
                if self.arc_from_geo(start.geo, end.geo, point.geo).is_some() {
                    self.commit_arc(layer_id, start, end, point.geo);
                } else {
                    self.arc_progress = Some(ArcProgress {
                        start,
                        end: Some(end),
                    });
                }
            }
        }
    }

    /// Improve Way mode's target: the one way that is selected, if any.
    fn improve_target(&self) -> Option<(LayerId, i64)> {
        match self.selected.as_slice() {
            [f] if f.kind == osm_gpui::selection::FeatureKind::Way => Some((f.layer_id, f.id)),
            _ => None,
        }
    }

    /// The operation the held modifiers select in Improve Way mode: Shift
    /// deletes, Ctrl/Cmd/Alt insert, none moves.
    fn improve_op(&self) -> osm_gpui::improve_way::Op {
        let m = self.last_modifiers;
        osm_gpui::improve_way::Op::from_modifiers(m.shift, m.control || m.platform || m.alt)
    }

    /// What a click at `cursor` would do to the target way: its node ids,
    /// their screen positions and the planned edit (in screen space).
    fn improve_plan_at(&self, cursor: gpui::Point<gpui::Pixels>) -> Option<ImprovePlan> {
        let (layer_id, way_id) = self.improve_target()?;
        let editable = self.layer_manager.find_layer(layer_id)?.as_editable()?;
        let ids = editable.way_node_ids(way_id)?;
        let points: Vec<osm_gpui::improve_way::Pt> = ids
            .iter()
            .map(|&id| {
                let (lat, lon) = editable.node_lat_lon(id)?;
                let p = self.viewport.geo_to_screen(lat, lon);
                Some((p.x.to_f64(), p.y.to_f64()))
            })
            .collect::<Option<_>>()?;
        let plan = osm_gpui::improve_way::plan(
            &points,
            &ids,
            (cursor.x.to_f64(), cursor.y.to_f64()),
            self.improve_op(),
        )?;
        Some(ImprovePlan {
            layer_id,
            way_id,
            ids,
            points,
            plan,
        })
    }

    /// Improve Way mode: with no way chosen, a click picks the way under the
    /// cursor; with one chosen, a click edits it (move the nearest node to
    /// the cursor, insert a node along the way, or delete the nearest node).
    fn handle_improve_click(
        &mut self,
        screen_pt: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        use osm_gpui::improve_way::Plan;
        use osm_gpui::selection::{FeatureKind, FeatureRef};

        if self.improve_target().is_none() {
            self.handle_select_click(screen_pt, false, 1);
            if self.improve_target().is_none() {
                self.set_selection(Vec::new());
            }
            return;
        }
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(ImprovePlan {
            layer_id,
            way_id,
            plan,
            ..
        }) = self.improve_plan_at(screen_pt)
        else {
            return;
        };
        let viewport = self.viewport.clone();
        let to_geo = move |p: osm_gpui::improve_way::Pt| {
            viewport.screen_to_geo(gpui::point(px(p.0 as f32), px(p.1 as f32)))
        };
        let way_ref = FeatureRef {
            layer_id,
            kind: FeatureKind::Way,
            id: way_id,
        };
        match plan {
            Plan::Move { node, to } => {
                let (lat, lon) = to_geo(to);
                let Some(editable) = self
                    .layer_manager
                    .find_layer_mut(layer_id)
                    .and_then(|l| l.as_editable_mut())
                else {
                    return;
                };
                let Some(before) = editable.node_lat_lon(node) else {
                    return;
                };
                if editable.commit_node_moves(&[(node, lat, lon)]).is_err() {
                    return;
                }
                self.undo_stack.push(UndoableAction::MoveNodes {
                    per_layer: vec![(layer_id, vec![(node, before, (lat, lon))])],
                    select: Some(vec![way_ref]),
                });
            }
            Plan::Insert { index, at } => {
                if self.refuse_way_growth(layer_id, way_id, 1) {
                    return;
                }
                let (lat, lon) = to_geo(at);
                let Some(editable) = self
                    .layer_manager
                    .find_layer_mut(layer_id)
                    .and_then(|l| l.as_editable_mut())
                else {
                    return;
                };
                let Ok(node_id) = editable.insert_node_into_way(way_id, index, lat, lon) else {
                    return;
                };
                self.undo_stack.push(UndoableAction::InsertNodeIntoWay {
                    layer: layer_id,
                    way_id,
                    index,
                    node_id,
                    node_lat_lon: (lat, lon),
                });
            }
            Plan::Delete { node } => {
                // The ordinary Delete action does the bookkeeping (other
                // ways, relations, orphans) as one undo step.
                self.set_selection(vec![FeatureRef {
                    layer_id,
                    kind: FeatureKind::Node,
                    id: node,
                }]);
                self.run_selection_action("delete", cx);
                // Keep working on the way if it survived.
                let still_there = self
                    .layer_manager
                    .find_layer(layer_id)
                    .and_then(|l| l.as_editable())
                    .is_some_and(|e| e.has_feature(FeatureKind::Way, way_id));
                self.set_selection(if still_there {
                    vec![way_ref]
                } else {
                    Vec::new()
                });
            }
        }
        cx.notify();
    }

    /// The geo position for an Arc-mode click at `screen_pt`: snapped onto the
    /// nearest way in `layer_id` when one is within tolerance.
    fn arc_point_at(
        &self,
        layer_id: LayerId,
        screen_pt: gpui::Point<gpui::Pixels>,
        no_snap: bool,
    ) -> ArcPoint {
        let plain = ArcPoint {
            geo: self.viewport.screen_to_geo(screen_pt),
            snap: None,
        };
        if no_snap {
            return plain;
        }
        let sources: Vec<Tags> = Vec::new();
        // An existing node close by wins over a way.
        let node_hit = self.layer_manager.find_layer(layer_id).and_then(|layer| {
            let osm_layer = layer.as_osm()?;
            let editable = layer.as_editable()?;
            let id = osm_layer.snap_to_node_where(
                &self.viewport,
                screen_pt,
                osm_gpui::layers::node_markers::FULL_HIT_RADIUS,
                |id| snap_node_ok(editable, layer_id, &sources, None, id),
            )?;
            Some((id, editable.node_lat_lon(id)?))
        });
        if let Some((id, geo)) = node_hit {
            return ArcPoint {
                geo,
                snap: Some(ArcSnap::Node(id)),
            };
        }
        let hit = self.layer_manager.find_layer(layer_id).and_then(|layer| {
            let osm_layer = layer.as_osm()?;
            let editable = layer.as_editable()?;
            osm_layer.snap_to_way_where(
                &self.viewport,
                screen_pt,
                osm_gpui::snap::WAY_SNAP_TOLERANCE_PX,
                |w| snap_way_ok(editable, layer_id, &sources, None, w),
            )
        });
        match hit {
            Some((way_id, _, _, idx, lat, lon)) => ArcPoint {
                geo: (lat, lon),
                snap: Some(ArcSnap::Way(way_id, idx)),
            },
            None => plain,
        }
    }

    /// The arc from `start` to `end` bending through `through` (all geo
    /// coordinates), as geo points including both ends. The circle is worked
    /// out in screen-pixel space, which is isotropic, so it is a true circle
    /// on screen (see `rectangle_from_geo_edge`). `None` if the three points
    /// are in a line.
    fn arc_from_geo(
        &self,
        start: (f64, f64),
        end: (f64, f64),
        through: (f64, f64),
    ) -> Option<Vec<(f64, f64)>> {
        let to_screen = |g: (f64, f64)| {
            let p = self.viewport.geo_to_screen(g.0, g.1);
            (p.x.to_f64(), p.y.to_f64())
        };
        let points = osm_gpui::arc::arc_through(
            to_screen(start),
            to_screen(through),
            to_screen(end),
            osm_gpui::arc::DEFAULT_STEP_DEGREES,
        )?;
        let last = points.len() - 1;
        Some(
            points
                .into_iter()
                .enumerate()
                .map(|(i, (x, y))| match i {
                    // Keep the endpoints exact rather than round-tripping them.
                    0 => start,
                    i if i == last => end,
                    _ => self
                        .viewport
                        .screen_to_geo(gpui::point(px(x as f32), px(y as f32))),
                })
                .collect(),
        )
    }

    /// Create the arc's nodes and one open, untagged way through them as one
    /// undo action, and select the new way.
    fn commit_arc(
        &mut self,
        layer_id: LayerId,
        start: ArcPoint,
        end: ArcPoint,
        through: (f64, f64),
    ) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(points) = self.arc_from_geo(start.geo, end.geo, through) else {
            return;
        };
        // Endpoints snapped onto a node use that node; ones snapped onto a way
        // get a new node spliced into it (so the arc joins that way), which
        // grows the way.
        let way_snap = |p: &ArcPoint| match p.snap {
            Some(ArcSnap::Way(way_id, idx)) => Some((way_id, idx)),
            _ => None,
        };
        let mut splices: Vec<(usize, ArcPoint)> = [(0, start), (points.len() - 1, end)]
            .into_iter()
            .filter(|(_, p)| way_snap(p).is_some())
            .collect();
        for (_, p) in &splices {
            let (way_id, _) = way_snap(p).unwrap();
            let by = splices
                .iter()
                .filter(|(_, q)| way_snap(q).map(|s| s.0) == Some(way_id))
                .count();
            if self.refuse_way_growth(layer_id, way_id, by) {
                return;
            }
        }
        // Splice in descending segment order within a way so earlier indices
        // stay valid; on a shared segment, the point nearer its far end first.
        let seg_start = |p: &ArcPoint| {
            let (way_id, idx) = way_snap(p).unwrap();
            let editable = self
                .layer_manager
                .find_layer(layer_id)
                .and_then(|l| l.as_editable())?;
            let id = *editable.way_node_ids(way_id)?.get(idx)?;
            editable.node_lat_lon(id)
        };
        let along = |p: &ArcPoint| {
            seg_start(p).map_or(0.0, |s| (p.geo.0 - s.0).powi(2) + (p.geo.1 - s.1).powi(2))
        };
        splices.sort_by(|(_, a), (_, b)| {
            let (aw, ai) = way_snap(a).unwrap();
            let (bw, bi) = way_snap(b).unwrap();
            aw.cmp(&bw)
                .then(bi.cmp(&ai))
                .then_with(|| along(b).total_cmp(&along(a)))
        });
        let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
            return;
        };
        let Some(editable) = layer.as_editable_mut() else {
            return;
        };

        // Nodes the arc doesn't own: reused ones, and ones spliced into other
        // ways (undone by their own entries).
        let mut node_ids: Vec<Option<i64>> = vec![None; points.len()];
        let mut actions = Vec::new();
        for (i, p) in [(0, start), (points.len() - 1, end)] {
            if let Some(ArcSnap::Node(id)) = p.snap {
                node_ids[i] = Some(id);
            }
        }
        for (i, p) in &splices {
            let (way_id, idx) = way_snap(p).unwrap();
            let Ok(id) = editable.insert_node_into_way(way_id, idx + 1, p.geo.0, p.geo.1) else {
                // Put back the splices already made, so nothing is half done.
                roll_back(&mut self.layer_manager, &actions);
                return;
            };
            actions.push(UndoableAction::InsertNodeIntoWay {
                layer: layer_id,
                way_id,
                index: idx + 1,
                node_id: id,
                node_lat_lon: p.geo,
            });
            node_ids[*i] = Some(id);
        }
        let borrowed: Vec<i64> = node_ids.iter().flatten().copied().collect();
        let all_ids: Vec<i64> = node_ids
            .iter()
            .zip(&points)
            .map(|(id, &(lat, lon))| id.unwrap_or_else(|| editable.add_node(lat, lon)))
            .collect();
        let way_id = editable.add_way(all_ids.clone(), Vec::new());
        let new_nodes: Vec<NewNode> = all_ids
            .iter()
            .zip(&points)
            .filter(|(id, _)| !borrowed.contains(id))
            .map(|(&id, &(lat, lon))| NewNode { id, lat, lon })
            .collect();
        // The whole arc, splices included, is one undo step: undoing removes
        // the arc first, then the splices.
        actions.push(UndoableAction::CreateArc {
            layer: layer_id,
            created: CreatedWay::new(way_id, all_ids, Vec::new(), new_nodes),
        });
        self.undo_stack.push(UndoableAction::Batch {
            label: tr!("history.created_arc").to_string(),
            actions,
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

    /// Release of an oriented-Building press. A release within a few pixels
    /// of the press is a click: the first one leaves the corner placed and
    /// waits, the second finishes the building at that point. A release
    /// farther away is a drag and finishes the building at the release point.
    fn finish_oriented_building_press(
        &mut self,
        press: BuildingPress,
        up_pos: gpui::Point<gpui::Pixels>,
    ) {
        let (Some(reference), Some(layer_id), Some(progress)) = (
            self.building_reference,
            self.active_layer,
            self.building_progress,
        ) else {
            return;
        };
        let moved = (up_pos - press.down).magnitude() >= interaction::DRAG_THRESHOLD as f64;
        if press.placed_first_corner && !moved {
            return;
        }
        let opposite = self.viewport.screen_to_geo(up_pos);
        if let Some(corners) = osm_gpui::building::rectangle_from_opposite_corners(
            progress.corner_a,
            opposite,
            reference.orientation,
        ) {
            self.commit_building_corners(layer_id, corners);
            self.building_progress = None;
        }
    }

    /// Compute the far 2 corners of a rectangle whose near edge is
    /// `a`..`b` (geo coordinates), offset by `cursor`'s perpendicular
    /// distance from that edge. The offset math is done in screen-pixel
    /// space rather than raw lat/lon: longitude degrees are foreshortened
    /// relative to latitude degrees (Web Mercator), so a "perpendicular"
    /// vector computed from raw lat/lon deltas is only actually
    /// perpendicular on screen for edges running exactly north-south or
    /// east-west — for any other edge it renders as a trapezoid. Screen
    /// pixels (and Mercator meters) are isotropic here, so doing the offset
    /// there and converting back yields a true rectangle on screen.
    fn rectangle_from_geo_edge(
        &self,
        a: (f64, f64),
        b: (f64, f64),
        cursor: (f64, f64),
    ) -> ((f64, f64), (f64, f64)) {
        let a_screen = self.viewport.geo_to_screen(a.0, a.1);
        let b_screen = self.viewport.geo_to_screen(b.0, b.1);
        let cursor_screen = self.viewport.geo_to_screen(cursor.0, cursor.1);
        let (far_a_screen, far_b_screen) = osm_gpui::selection::rectangle_from_edge(
            (a_screen.x.to_f64(), a_screen.y.to_f64()),
            (b_screen.x.to_f64(), b_screen.y.to_f64()),
            (cursor_screen.x.to_f64(), cursor_screen.y.to_f64()),
        );
        let far_a = self.viewport.screen_to_geo(gpui::point(
            px(far_a_screen.0 as f32),
            px(far_a_screen.1 as f32),
        ));
        let far_b = self.viewport.screen_to_geo(gpui::point(
            px(far_b_screen.0 as f32),
            px(far_b_screen.1 as f32),
        ));
        (far_a, far_b)
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
        let (far_a, far_b) = self.rectangle_from_geo_edge(corner_a, corner_b, cursor);
        self.commit_building_corners(layer_id, [corner_a, corner_b, far_b, far_a]);
    }

    /// Create 4 nodes and a closed `building=yes` way through them (corners
    /// in ring order) as one undo action, and select the new way.
    fn commit_building_corners(&mut self, layer_id: LayerId, corners: [(f64, f64); 4]) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
            return;
        };
        let Some(editable) = layer.as_editable_mut() else {
            return;
        };

        let [n0, n1, n2, n3] = corners.map(|(lat, lon)| editable.add_node(lat, lon));
        let tags = vec![("building".to_string(), "yes".to_string())];
        let way_id = editable.add_way(vec![n0, n1, n2, n3, n0], tags.clone());

        let new_nodes = [n0, n1, n2, n3]
            .into_iter()
            .zip(corners)
            .map(|(id, (lat, lon))| NewNode { id, lat, lon })
            .collect();
        self.undo_stack.push(UndoableAction::CreateBuilding {
            layer: layer_id,
            created: CreatedWay::new(way_id, vec![n0, n1, n2, n3, n0], tags, new_nodes),
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
    /// `rectangle_from_geo_edge` (using `up_pos` for the perpendicular
    /// offset), create 2 new nodes + a closed way with no tags (extrude
    /// works on any way segment, not just buildings, so it must not assume
    /// `building=yes`), push one `ExtrudeWay` undo action.
    fn commit_extrude(&mut self, drag: &ExtrudeDrag, up_pos: gpui::Point<gpui::Pixels>) {
        if self.refuse_edit_during_upload() {
            return;
        }
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

        let (far_a, far_b) = self.rectangle_from_geo_edge(a_geo, b_geo, cursor_geo);
        let Some(layer) = self.layer_manager.find_layer_mut(drag.layer) else {
            return;
        };
        let Some(editable) = layer.as_editable_mut() else {
            return;
        };
        let new_a = editable.add_node(far_a.0, far_a.1);
        let new_b = editable.add_node(far_b.0, far_b.1);
        let way_nodes = vec![drag.node_a, drag.node_b, new_b, new_a, drag.node_a];
        let way_id = editable.add_way(way_nodes.clone(), Vec::new());

        self.undo_stack.push(UndoableAction::ExtrudeWay {
            layer: drag.layer,
            created: CreatedWay::new(
                way_id,
                way_nodes,
                Vec::new(),
                vec![
                    NewNode {
                        id: new_a,
                        lat: far_a.0,
                        lon: far_a.1,
                    },
                    NewNode {
                        id: new_b,
                        lat: far_b.0,
                        lon: far_b.1,
                    },
                ],
            ),
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

    /// Resolve a Reshape/Move extrude drag against the cursor at
    /// `cursor_screen` (map-local pixels): the plan to apply plus the way's
    /// resulting outline in screen pixels. Works in screen space, which is
    /// isotropic, so "perpendicular" means perpendicular on screen. `None`
    /// for anything that would not change the way.
    fn compute_extrude(
        &self,
        drag: &ExtrudeDrag,
        cursor_screen: gpui::Point<gpui::Pixels>,
    ) -> Option<ExtrudeResult> {
        use osm_gpui::extrude::{ExtrudeKind, PlanNode, SegmentNode};
        use osm_gpui::selection::{FeatureKind, FeatureRef};

        let editable = self.layer_manager.find_layer(drag.layer)?.as_editable()?;
        let ids = editable.way_node_ids(drag.way_id)?;
        let screen_of = |id: i64| -> Option<(f64, f64)> {
            let (lat, lon) = editable.node_lat_lon(id)?;
            let p = self.viewport.geo_to_screen(lat, lon);
            Some((p.x.to_f64(), p.y.to_f64()))
        };
        let mut nodes = Vec::with_capacity(ids.len());
        for &id in &ids {
            // A node may be moved (rather than duplicated) only when nothing
            // else depends on it: no other way, no relation, no tags.
            let movable = editable
                .ways_using_node(id)
                .iter()
                .all(|&w| w == drag.way_id)
                && editable.relations_containing_node(id).is_empty()
                && editable
                    .feature_tags(&FeatureRef {
                        layer_id: drag.layer,
                        kind: FeatureKind::Node,
                        id,
                    })
                    .is_none_or(|t| t.is_empty());
            nodes.push(SegmentNode {
                id,
                pos: screen_of(id)?,
                movable,
            });
        }
        let down = (drag.down.x.to_f64(), drag.down.y.to_f64());
        let cursor = (cursor_screen.x.to_f64(), cursor_screen.y.to_f64());
        let kind = match drag.style {
            ExtrudeStyle::Move => ExtrudeKind::Move,
            _ => ExtrudeKind::Reshape,
        };
        let plan = osm_gpui::extrude::plan_extrude(
            &nodes,
            drag.segment,
            (cursor.0 - down.0, cursor.1 - down.1),
            kind,
        )?;
        let outline = plan
            .nodes
            .iter()
            .map(|n| match *n {
                PlanNode::New(i) => plan.new_nodes[i],
                PlanNode::Existing(id) => plan
                    .moved
                    .iter()
                    .find(|m| m.0 == id)
                    .map(|m| m.1)
                    .or_else(|| nodes.iter().find(|n| n.id == id).map(|n| n.pos))
                    .unwrap_or_default(),
            })
            .collect();
        Some(ExtrudeResult { plan, outline })
    }

    /// Commit a Reshape/Move extrude drag as one undo step: new nodes
    /// (`CreateNode`), the way's rewritten node list (`SetWayNodes`) and the
    /// moved nodes (`MoveNodes`), in that order so undo runs them backwards.
    fn commit_extrude_reshape(&mut self, drag: &ExtrudeDrag, up_pos: gpui::Point<gpui::Pixels>) {
        if self.refuse_edit_during_upload() {
            return;
        }
        use osm_gpui::extrude::PlanNode;

        let Some(result) = self.compute_extrude(drag, up_pos) else {
            return;
        };
        let plan = result.plan;
        if !plan.new_nodes.is_empty()
            && self.refuse_way_growth(drag.layer, drag.way_id, plan.new_nodes.len())
        {
            return;
        }
        let to_geo = |p: (f64, f64)| {
            self.viewport
                .screen_to_geo(gpui::point(px(p.0 as f32), px(p.1 as f32)))
        };
        let new_geo: Vec<(f64, f64)> = plan.new_nodes.iter().map(|&p| to_geo(p)).collect();
        let moved_geo: Vec<(i64, (f64, f64))> =
            plan.moved.iter().map(|&(id, p)| (id, to_geo(p))).collect();

        let Some(editable) = self
            .layer_manager
            .find_layer_mut(drag.layer)
            .and_then(|l| l.as_editable_mut())
        else {
            return;
        };
        let Some(before) = editable.way_node_ids(drag.way_id) else {
            return;
        };
        let mut actions = Vec::new();
        let mut new_ids = Vec::with_capacity(new_geo.len());
        for &(lat, lon) in &new_geo {
            let Ok(id) = editable.create_node(lat, lon, None) else {
                return;
            };
            new_ids.push(id);
            actions.push(UndoableAction::CreateNode {
                layer: drag.layer,
                id,
                lat,
                lon,
            });
        }
        let after: Vec<i64> = plan
            .nodes
            .iter()
            .map(|n| match *n {
                PlanNode::Existing(id) => id,
                PlanNode::New(i) => new_ids[i],
            })
            .collect();
        if after != before {
            if editable.set_way_nodes(drag.way_id, after.clone()).is_err() {
                roll_back(&mut self.layer_manager, &actions);
                return;
            }
            actions.push(UndoableAction::SetWayNodes {
                layer: drag.layer,
                way_id: drag.way_id,
                before,
                after,
            });
        }
        if !moved_geo.is_empty() {
            let mut entries: Vec<NodeMoveUndoEntry> = Vec::new();
            let mut moves = Vec::new();
            for &(id, (lat, lon)) in &moved_geo {
                if let Some(old) = editable.node_lat_lon(id) {
                    entries.push((id, old, (lat, lon)));
                    moves.push((id, lat, lon));
                }
            }
            if editable.commit_node_moves(&moves).is_err() {
                roll_back(&mut self.layer_manager, &actions);
                return;
            }
            actions.push(UndoableAction::MoveNodes {
                select: None,
                per_layer: vec![(drag.layer, entries)],
            });
        }
        if actions.is_empty() {
            return;
        }
        self.undo_stack.push(UndoableAction::Batch {
            label: osm_gpui::tr!("history.extruded_segment").to_string(),
            actions,
        });
        self.selected = vec![osm_gpui::selection::FeatureRef {
            layer_id: drag.layer,
            kind: osm_gpui::selection::FeatureKind::Way,
            id: drag.way_id,
        }];
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }

    /// Double-click on a segment (no drag): insert a new node on that
    /// segment, at the point nearest the double-click, splitting it.
    fn insert_node_on_segment(&mut self, drag: &ExtrudeDrag, up_pos: gpui::Point<gpui::Pixels>) {
        if self.refuse_edit_during_upload() {
            return;
        }
        if self.refuse_way_growth(drag.layer, drag.way_id, 1) {
            return;
        }
        let Some(layer) = self.layer_manager.find_layer_mut(drag.layer) else {
            return;
        };
        let Some(editable) = layer.as_editable_mut() else {
            return;
        };
        // Place the node on the segment itself (the point nearest the click),
        // not under the cursor, so the way's shape is unchanged.
        let (Some(a_geo), Some(b_geo)) = (
            editable.node_lat_lon(drag.node_a),
            editable.node_lat_lon(drag.node_b),
        ) else {
            return;
        };
        let (lat, lon) = {
            let a = self.viewport.geo_to_screen(a_geo.0, a_geo.1);
            let b = self.viewport.geo_to_screen(b_geo.0, b_geo.1);
            let (dx, dy) = ((b.x - a.x).to_f64(), (b.y - a.y).to_f64());
            let len_sq = dx * dx + dy * dy;
            let t = if len_sq <= f64::EPSILON {
                0.0
            } else {
                (((up_pos.x - a.x).to_f64() * dx + (up_pos.y - a.y).to_f64() * dy) / len_sq)
                    .clamp(0.0, 1.0)
            };
            self.viewport.screen_to_geo(gpui::point(
                px((a.x.to_f64() + t * dx) as f32),
                px((a.y.to_f64() + t * dy) as f32),
            ))
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

        let Ok(new_id) = editable.insert_node_into_way(drag.way_id, insert_index, lat, lon) else {
            return;
        };
        self.undo_stack.push(UndoableAction::InsertNodeIntoWay {
            layer: drag.layer,
            way_id: drag.way_id,
            index: insert_index,
            node_id: new_id,
            node_lat_lon: (lat, lon),
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
        // Data underneath always wins. A click on nothing but a read-only
        // reference feature (GPX, GeoJSON) shows what it is in the status bar.
        if hit.is_none() && !shift_held {
            if let Some(text) = self.reference_feature_at(screen_pt) {
                self.set_status(text);
            }
        }
        self.selected = osm_gpui::selection::apply_click_selection(&self.selected, hit, shift_held);
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }

    /// Edit-time server limit: whether growing way `way_id` by `by` nodes
    /// would pass the server's way-node maximum. If so, say so in the status
    /// bar and return true (the caller abandons the edit). A way already over
    /// the limit stays editable otherwise: only growing it is refused.
    fn refuse_way_growth(&mut self, layer_id: LayerId, way_id: i64, by: usize) -> bool {
        let len = self
            .layer_manager
            .find_layer(layer_id)
            .and_then(|l| l.as_editable())
            .and_then(|e| e.way_node_ids(way_id))
            .map_or(0, |nodes| nodes.len());
        match osm_gpui::limits::way_over_limit(len + by, osm_gpui::limits::way_full_message) {
            Some(message) => {
                self.set_status(message);
                true
            }
            None => false,
        }
    }

    /// Add mode: place a node, or extend/connect the in-progress way. See
    /// docs/superpowers/specs/2026-07-07-mode-selector-design.md "Add mode"
    /// and docs/superpowers/specs/2026-07-08-add-mode-snap-to-way-design.md
    /// for the snap-to-way behavior. `ctrl_held` (true when a snapping-off
    /// modifier, Alt or Ctrl, is held; see `osm_gpui::snap`) bypasses both
    /// snapping onto a nearby existing node and onto a nearby way's line
    /// geometry, always producing a fully independent node. Only targets
    /// `osm_gpui::snap::may_snap` allows for the way being drawn qualify.
    fn handle_add_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>, ctrl_held: bool) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(layer_id) = self.active_layer else {
            return;
        };
        let (mut lat, mut lon) = self.viewport.screen_to_geo(screen_pt);

        // Every click after the first grows the way being drawn by a node.
        let drawing_way = self.add_progress.as_ref().and_then(|p| p.way_id);
        if let Some(way_id) = drawing_way {
            if self.refuse_way_growth(layer_id, way_id, 1) {
                return;
            }
        }

        // Clicking an existing node/way finishes the in-progress way by
        // connecting to it. Skipped entirely when Ctrl is held.
        if !ctrl_held && self.add_progress.is_some() {
            let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
            let sources = self.add_snap_sources(layer_id);
            let node_allowed = |id: i64| {
                self.layer_manager
                    .find_layer(layer_id)
                    .and_then(|l| l.as_editable())
                    .is_some_and(|e| snap_node_ok(e, layer_id, &sources, None, id))
            };
            let hit_node = osm_gpui::selection::resolve_hits(per_layer)
                .filter(|hit| {
                    hit.layer_id == layer_id && hit.kind == osm_gpui::selection::FeatureKind::Node
                })
                .map(|hit| hit.id)
                .filter(|&id| node_allowed(id))
                // Vertices that are not drawn (untagged ones, unless
                // selected) are not hit targets, but still snap targets.
                .or_else(|| {
                    self.layer_manager
                        .find_layer(layer_id)
                        .and_then(|layer| layer.as_osm())
                        .and_then(|osm_layer| {
                            osm_layer.snap_to_node_where(
                                &self.viewport,
                                screen_pt,
                                osm_gpui::layers::node_markers::FULL_HIT_RADIUS,
                                node_allowed,
                            )
                        })
                });
            if let Some(node_id) = hit_node {
                let way_id = self.add_extend_or_start_way(layer_id, node_id, false, None);
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

        // Try to snap onto a nearby way's line geometry, unless Ctrl is
        // held. `snap` carries the snapped-onto way's id and splice index,
        // if any, threaded through to `add_extend_or_start_way` for the
        // 2nd+ click case.
        let snap_hit = if ctrl_held {
            None
        } else {
            let sources = self.add_snap_sources(layer_id);
            self.layer_manager.find_layer(layer_id).and_then(|layer| {
                let osm_layer = layer.as_osm()?;
                let editable = layer.as_editable()?;
                osm_layer.snap_to_way_where(
                    &self.viewport,
                    screen_pt,
                    osm_gpui::snap::WAY_SNAP_TOLERANCE_PX,
                    |w| snap_way_ok(editable, layer_id, &sources, None, w),
                )
            })
        };

        // With no OSM node or way to snap to (they win), a reference line or
        // point (GPX, GeoJSON) under the cursor pulls the new, ordinary node
        // onto itself: onto a vertex, else onto the nearest point of a segment.
        if snap_hit.is_none() && !ctrl_held {
            if let Some((snap_lat, snap_lon)) = self.reference_snap_at(screen_pt) {
                lat = snap_lat;
                lon = snap_lon;
            }
        }

        // Snapping splices a node into the way under the cursor.
        if let Some((snap_way, ..)) = snap_hit {
            let by = 1 + usize::from(drawing_way == Some(snap_way));
            if self.refuse_way_growth(layer_id, snap_way, by) {
                return;
            }
        }

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
                        let Ok(new_id) =
                            editable.insert_node_into_way(way_id, idx + 1, snap_lat, snap_lon)
                        else {
                            return;
                        };
                        self.undo_stack.push(UndoableAction::InsertNodeIntoWay {
                            layer: layer_id,
                            way_id,
                            index: idx + 1,
                            node_id: new_id,
                            node_lat_lon: (snap_lat, snap_lon),
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
                    at_start: false,
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
                    Some((way_id, _, _, idx, snap_lat, snap_lon)) => {
                        let Ok(id) =
                            editable.insert_node_into_way(way_id, idx + 1, snap_lat, snap_lon)
                        else {
                            return;
                        };
                        (id, Some((way_id, idx + 1)))
                    }
                    None => (editable.add_node(lat, lon), None),
                };
                let at_start = progress.at_start;
                self.add_progress = Some(progress);
                let way_id = self.add_extend_or_start_way(layer_id, new_id, true, snap);
                self.add_progress = Some(AddProgress {
                    way_id: Some(way_id),
                    last_node_id: new_id,
                    at_start,
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
        let at_start = self.add_progress.as_ref().is_some_and(|p| p.at_start);
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
        let node_lat_lon = editable.node_lat_lon(node_id).unwrap_or_default();

        let attached = match progress_way_id {
            Some(way_id) if at_start => {
                let mut nodes = editable.way_node_ids(way_id).unwrap_or_default();
                nodes.insert(0, node_id);
                editable
                    .set_way_nodes(way_id, nodes)
                    .map(|()| (way_id, false))
            }
            Some(way_id) => editable
                .extend_way(way_id, node_id)
                .map(|()| (way_id, false)),
            None => Ok((
                editable.add_way(vec![last_node_id, node_id], Vec::new()),
                true,
            )),
        };
        let (way_id, way_created) = match attached {
            Ok(done) => done,
            Err(e) => {
                eprintln!("adding to the way was refused: {e}");
                // Take back the node this click made, so none is left over.
                let made = match snap {
                    Some((way_id, index)) => Some(UndoableAction::InsertNodeIntoWay {
                        layer: layer_id,
                        way_id,
                        index,
                        node_id,
                        node_lat_lon,
                    }),
                    None if node_created => Some(UndoableAction::CreateNode {
                        layer: layer_id,
                        id: node_id,
                        lat: node_lat_lon.0,
                        lon: node_lat_lon.1,
                    }),
                    None => None,
                };
                roll_back(
                    &mut self.layer_manager,
                    &made.into_iter().collect::<Vec<_>>(),
                );
                return progress_way_id.unwrap_or(0);
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
                    node_lat_lon,
                    prev_node_id: last_node_id,
                    at_start,
                });
            }
            None => {
                self.undo_stack.push(UndoableAction::ExtendWay {
                    layer: layer_id,
                    way_id,
                    node_id,
                    node_lat_lon,
                    prev_node_id: last_node_id,
                    way_created,
                    node_created,
                    at_start,
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

    /// A notched mouse wheel (line deltas) zooms at the cursor. A trackpad or
    /// other precise-scrolling device (pixel deltas) pans, as in native Mac
    /// apps — unless Cmd or Ctrl is held, which makes it zoom instead.
    fn handle_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let position = self.window_to_map(event.position);
        let zoom_modifier = event.modifiers.platform || event.modifiers.control;
        // Read live so the Settings switch applies at once, without a restart.
        // Only the zoom paths are inverted; panning and pinch are not.
        let invert = osm_gpui::settings_store::snapshot().invert_scroll_zoom;
        let changed = match event.delta {
            gpui::ScrollDelta::Lines(delta) => self
                .viewport
                .zoom_by_wheel(position, Viewport::scroll_zoom_step(delta.y, invert)),
            gpui::ScrollDelta::Pixels(delta) if zoom_modifier => {
                let step = Viewport::scroll_zoom_step(delta.y.to_f64() as f32, invert);
                self.viewport.zoom_by_scroll_pixels(position, px(step))
            }
            gpui::ScrollDelta::Pixels(delta) => self.viewport.pan_by_scroll(delta),
        };
        if changed {
            cx.notify();
        }
    }

    /// Trackpad pinch: zoom around the point under the fingers.
    fn handle_pinch(&mut self, event: &gpui::PinchEvent, cx: &mut Context<Self>) {
        let position = self.window_to_map(event.position);
        if self.viewport.zoom_by_pinch(position, event.delta) {
            cx.notify();
        }
    }

    /// Start a pan-drag of the map (right or middle button, or the left
    /// button while Space is held). Deliberately leaves `self.interaction`
    /// alone so the drag can never turn into a click or a box-select.
    fn start_pan_drag(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        self.viewport.handle_mouse_down(position);
        cx.notify();
    }

    fn end_pan_drag(&mut self, cx: &mut Context<Self>) {
        self.viewport.handle_mouse_up();
        cx.notify();
    }

    /// Pointer travel (in px) below which a right-button press/release pair
    /// counts as a click rather than a pan.
    const CLICK_SLOP: f64 = 4.0;

    /// Right-button release on the map. Panning has already been ended by the
    /// caller; if the pointer barely moved since the press this was a click,
    /// so open the context menu at the cursor.
    fn finish_right_press(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(down) = self.right_press.take() else {
            return;
        };
        if (position - down).magnitude() < Self::CLICK_SLOP {
            self.open_context_menu(position, window, cx);
        }
    }

    /// Open the selection-actions context menu at `position` (window
    /// coordinates). A right-click on a feature outside the selection first
    /// selects that feature, replacing the selection (as in Finder and iD);
    /// on empty map with nothing selected no menu opens. Only the actions
    /// applicable to the selection are listed.
    fn open_context_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::menu::{PopupMenu, PopupMenuItem};

        if self.mode != EditMode::Select {
            return;
        }
        let map_pos = self.window_to_map(position);
        let per_layer = self.layer_manager.hit_test_all(&self.viewport, map_pos);
        if let Some(hit) = osm_gpui::selection::resolve_hits(per_layer) {
            if !self.selected.contains(&hit) {
                self.set_selection(vec![hit]);
                cx.notify();
            }
        }
        if self.selected.is_empty() {
            return;
        }

        let env = selection_actions::ActionEnv {
            selection: &self.selected,
            active_layer: self.active_layer,
            layers: &self.layer_manager,
            clipboards: &self.clipboards,
        };
        let actions: Vec<&'static selection_actions::SelectionAction> =
            selection_actions::SelectionAction::applicable(&env).collect();
        if actions.is_empty() {
            return;
        }

        let settings = settings_store::snapshot();
        let focus = self.focus_handle.clone();
        // One existing object: its server history can be shown.
        let history_item = self.object_history_unavailable().is_none();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            menu = menu.action_context(focus).min_w(px(190.0));
            for (i, action) in actions.iter().enumerate() {
                // Keep the destructive action apart from the edits.
                if i > 0 && action.destructive {
                    menu = menu.separator();
                }
                let label = action.label();
                let shortcut = action.shortcut_label(&settings);
                menu =
                    menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            gpui_component::h_flex()
                                .w_full()
                                .gap_6()
                                .justify_between()
                                .child(label.clone())
                                .children(shortcut.clone().map(|s| {
                                    div().text_color(cx.theme().muted_foreground).child(s)
                                }))
                        })
                        .action(Box::new(RunSelectionAction {
                            id: action.id.into(),
                        })),
                    );
            }
            if history_item {
                let label = tr!("menu.edit.show_history");
                menu = menu.separator().item(
                    PopupMenuItem::element(move |_, _| div().child(label.clone()))
                        .action(Box::new(history_actions::ShowObjectHistory)),
                );
            }
            menu
        });
        let dismiss = cx.subscribe(&menu, |this, _menu, _: &gpui::DismissEvent, cx| {
            // The menu has already handed focus back to the map.
            this.context_menu = None;
            cx.notify();
        });
        self.context_menu = Some(ContextMenuState {
            menu,
            position,
            _dismiss: dismiss,
        });
        cx.notify();
    }

    /// A click outside the open context menu closes it.
    fn dismiss_context_menu(
        &mut self,
        _: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// The open context menu as an element: a window-sized transparent
    /// backdrop that swallows the click that dismisses the menu (so it
    /// doesn't also change the selection underneath), with the menu itself
    /// anchored at the click point and nudged to stay inside the window.
    fn render_context_menu(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use gpui::Focusable as _;

        let state = self.context_menu.as_ref()?;
        let menu = state.menu.clone();
        // The menu takes Esc and arrow keys through its own focus.
        if !menu.focus_handle(cx).contains_focused(window, cx) {
            menu.focus_handle(cx).focus(window, cx);
        }
        let size = window.viewport_size();
        Some(
            gpui::deferred(
                gpui::anchored().child(
                    div()
                        .w(size.width)
                        .h(size.height)
                        .occlude()
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(Self::dismiss_context_menu),
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Right,
                            cx.listener(Self::dismiss_context_menu),
                        )
                        .child(
                            gpui::anchored()
                                .position(state.position)
                                .snap_to_window_with_margin(px(8.0))
                                .anchor(gpui::Anchor::TopLeft)
                                .child(menu),
                        ),
                ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// Bare-key map navigation: arrows pan a quarter of the view, `+`/`=`
    /// and `-` zoom a level. Returns whether `key` was one of those.
    fn handle_navigation_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let size = self.viewport.transform.screen_size;
        if let Some((dx, dy)) =
            interaction::arrow_key_pan(key, (size.width.as_f32(), size.height.as_f32()))
        {
            self.viewport.transform.pan_by_pixels(px(dx), px(dy));
            cx.notify();
            return true;
        }
        match key {
            "=" | "+" => self.zoom_by(1.0, cx),
            "-" => self.zoom_by(-1.0, cx),
            _ => return false,
        }
        true
    }

    /// Add a newly-loaded OSM dataset as a new layer. Called directly by
    /// `menu::open_osm_file` (once its background parse completes) and by
    /// the script harness's `ScriptCommand::LoadOsm`.
    ///
    /// `edits` are the pending local edits the file recorded (empty for a
    /// plain server extract); they stay pending in the new layer.
    pub(crate) fn add_osm_dataset(
        &mut self,
        name: String,
        data: OsmData,
        edits: &osm_gpui::osm::FileEdits,
        cx: &mut Context<Self>,
    ) {
        let file_name = if name.is_empty() {
            "OSM".to_string()
        } else {
            name
        };
        let candidate = self.layer_manager.unique_name(&file_name);
        let data_arc = Arc::new(data.clone());
        let layer_id = self.layer_manager.alloc_id();
        let layer = OsmLayer::new_with_edits(layer_id, candidate, data_arc, edits);
        self.layer_manager.add_layer(Box::new(layer));
        if self.active_layer.is_none() {
            self.active_layer = Some(layer_id);
        }
        if !self.first_dataset_fitted {
            self.fit_to_osm_data(&data);
            self.first_dataset_fitted = true;
        }
        self.data_generation += 1;
        self.status_message = None;
        cx.notify();
    }

    /// Add a parsed GPX or GeoJSON file as a read-only reference layer (see
    /// `layers::reference_layer`) above the imagery and below the data
    /// layers, and zoom to its extent. It never becomes the active layer.
    pub(crate) fn add_reference_dataset(
        &mut self,
        name: String,
        data: osm_gpui::reference::ReferenceData,
        cx: &mut Context<Self>,
    ) {
        let name = if name.is_empty() {
            data.format.tag().to_string()
        } else {
            name
        };
        let candidate = self.layer_manager.unique_name(&name);
        let extent = data.extent();
        let layer_id = self.layer_manager.alloc_id();
        let layer =
            osm_gpui::layers::reference_layer::ReferenceLayer::new(layer_id, candidate, data);
        self.layer_manager.add_reference_layer(Box::new(layer));
        if let Some(extent) = extent {
            self.fit_to_extent(extent);
        }
        self.status_message = None;
        cx.notify();
    }

    /// Pan/zoom to a `(min_lat, max_lat, min_lon, max_lon)` extent; a single
    /// point is centred at an editing zoom (see `fit_bounds_to_viewport`).
    fn fit_to_extent(
        &mut self,
        (min_lat, max_lat, min_lon, max_lon): osm_gpui::reference::GeoExtent,
    ) {
        let size = self.viewport.transform.screen_size;
        let (lat, lon, zoom) = osm_gpui::coordinates::fit_bounds_to_viewport(
            min_lat,
            max_lat,
            min_lon,
            max_lon,
            size.width.to_f64(),
            size.height.to_f64(),
        );
        self.viewport.pan_to(lat, lon);
        self.viewport.set_zoom(zoom);
    }

    /// The topmost visible reference feature under `screen_pt`, described
    /// for the status bar.
    fn reference_feature_at(&self, screen_pt: gpui::Point<gpui::Pixels>) -> Option<String> {
        self.layer_manager
            .layers()
            .iter()
            .rev()
            .filter(|layer| layer.is_visible())
            .filter_map(|layer| layer.as_reference())
            .find_map(|layer| layer.hit(&self.viewport, screen_pt))
            .map(osm_gpui::reference::describe)
    }

    /// The point of a visible reference layer to snap a new node to: the
    /// nearest vertex, marker or point on a segment within the snap
    /// tolerance, as `(lat, lon)`. Topmost layer first.
    fn reference_snap_at(&self, screen_pt: gpui::Point<gpui::Pixels>) -> Option<(f64, f64)> {
        self.layer_manager
            .layers()
            .iter()
            .rev()
            .filter(|layer| layer.is_visible())
            .filter_map(|layer| layer.as_reference())
            .find_map(|layer| {
                layer.snap(
                    &self.viewport,
                    screen_pt,
                    osm_gpui::layers::reference_layer::HIT_TOLERANCE_PX,
                )
            })
    }

    /// The `(name, .osm document)` of the layer File > Export… writes: the
    /// active OSM layer, or with none active the first layer that has
    /// anything to export.
    fn exportable_layer(&self) -> Option<(String, String)> {
        self.active_layer
            .and_then(|id| self.layer_manager.find_layer(id))
            .into_iter()
            .chain(
                self.layer_manager
                    .layers()
                    .iter()
                    .map(|layer| layer.as_ref()),
            )
            .find_map(|layer| Some((layer.name().to_string(), layer.as_osm()?.export_xml()?)))
    }

    /// File > Export…: write a layer (see `exportable_layer`) to a
    /// JOSM-style `.osm` file chosen in a save dialog. The file includes
    /// pending edits, so it can be reopened here or in JOSM and uploaded
    /// later.
    fn on_export_osm_file(&mut self, _: &ExportOsmFile, _: &mut Window, cx: &mut Context<Self>) {
        let Some((layer_name, xml)) = self.exportable_layer() else {
            self.set_status(tr!("status.no_data_to_export"));
            cx.notify();
            return;
        };

        cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter(tr!("menu.open_dialog.osm_files").as_ref(), &["osm"])
                .set_title(tr!("menu.export_dialog.title").as_ref())
                .set_file_name(format!("{layer_name}.osm"))
                .save_file()
                .await
            else {
                return;
            };
            let path = file.path().to_path_buf();
            let write_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::write(&write_path, xml) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.set_status(tr!("status.exported", path = path.display())),
                    Err(e) => this.set_status(tr!("status.export_failed", error = e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Apply a `LayerRequest`, adding the corresponding layer. Called
    /// directly by menu handlers (via `with_map_viewer`) and by the custom
    /// imagery dialog's `Submitted` event.
    fn apply_layer_request(&mut self, req: LayerRequest, cx: &mut Context<Self>) {
        match req {
            LayerRequest::OsmCarto => {
                if self
                    .layer_manager
                    .layer_named("OpenStreetMap Carto")
                    .is_none()
                {
                    let layer_id = self.layer_manager.alloc_id();
                    let tile_layer = TileLayer::new(layer_id, self.tile_cache.clone()).with_offset(
                        self.saved_imagery_offset(
                            osm_gpui::layers::tile_layer::OSM_CARTO_IMAGERY_ID,
                        ),
                    );
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
                imagery_id,
            } => {
                let candidate = self.layer_manager.unique_name(&name);
                let layer_id = self.layer_manager.alloc_id();
                let imagery_id = imagery_id.unwrap_or_else(|| format!("custom:{url_template}"));
                let offset = self.saved_imagery_offset(&imagery_id);
                let layer = TileLayer::new_with_template(
                    layer_id,
                    candidate,
                    url_template,
                    self.tile_cache.clone(),
                )
                .with_min_zoom(min_zoom)
                .with_max_zoom(max_zoom)
                .with_attribution(attribution)
                .with_imagery_id(imagery_id)
                .with_offset(offset);
                self.layer_manager.add_layer(Box::new(layer));
            }
        }
        cx.notify();
    }

    fn get_layer_stats(&self) -> (usize, usize, usize) {
        let osm_gpui::layers::LayerStats {
            cached_files,
            osm_nodes,
            osm_ways,
        } = self.layer_manager.layer_stats();

        // Calculate visible tiles
        let zoom_level = self.viewport.zoom_level();
        let tile_zoom = zoom_level
            .round()
            .clamp(0.0, osm_gpui::coordinates::MAX_MAP_ZOOM) as u32;
        let bounds_geo = self.viewport.visible_bounds();
        let visible_tiles = tiles::tiles_covering_geo(&bounds_geo, tile_zoom);
        let total_tiles = visible_tiles.len();

        (total_tiles, cached_files, osm_nodes + osm_ways)
    }

    /// Begin dragging the side panel's resize handle from window x `x`.
    pub(crate) fn start_side_panel_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        self.side_panel_resize = Some((x, self.side_panel_width));
        cx.notify();
    }

    fn drag_side_panel_resize(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((start_x, start_width)) = self.side_panel_resize else {
            return;
        };
        // The button was released somewhere that never delivered the
        // mouse-up (outside the window): stop here.
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            self.finish_side_panel_resize(cx);
            return;
        }
        self.side_panel_width = osm_gpui::ui::style::resized_side_panel_width(
            start_width,
            start_x,
            event.position.x.as_f32(),
        );
        cx.notify();
    }

    /// End a side panel resize drag and remember the new width.
    fn finish_side_panel_resize(&mut self, cx: &mut Context<Self>) {
        if self.side_panel_resize.take().is_none() {
            return;
        }
        let width = Some(self.side_panel_width);
        if settings_store::snapshot().side_panel_width != width {
            settings_store::update(true, |s| s.side_panel_width = width);
        }
        cx.notify();
    }

    fn set_status(&mut self, message: impl Into<SharedString>) {
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
    /// Overlay` menu handler (via `with_map_viewer`).
    pub(crate) fn toggle_debug_overlay(&mut self, cx: &mut Context<Self>) {
        self.show_debug_overlay = !self.show_debug_overlay;
        cx.notify();
    }

    /// Show or hide the downloaded-area wash and refresh the View menu so its
    /// checkmark matches. Window-scoped (see `on_toggle_downloaded_area`).
    pub(crate) fn set_show_downloaded_area(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_downloaded_area == show {
            return;
        }
        self.show_downloaded_area = show;
        let (lat, lon, state) = self.imagery_menu_context();
        rebuild_menus(
            &mut *cx,
            lat,
            lon,
            state,
            show,
            self.has_uploadable_changes(),
        );
        cx.notify();
    }

    /// View > Show Downloaded Area: flip the wash.
    fn on_toggle_downloaded_area(
        &mut self,
        _: &ToggleDownloadedArea,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_show_downloaded_area(!self.show_downloaded_area, cx);
    }

    /// Window-scoped handler for the `AddCustomImagery` action; see
    /// `on_upload_to_osm` for why the global handler alone is not enough.
    fn on_open_custom_imagery_dialog(
        &mut self,
        _: &AddCustomImagery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_custom_imagery_dialog(window, cx);
    }

    /// Open the "Add Custom Imagery…" dialog, if one isn't already open.
    /// Called directly from the menu handler (via `with_map_viewer_in`).
    /// Unlike an inline
    /// tag edit, this dialog has no post-paint focus requirement, so
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
                                imagery_id: None,
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
        use osm_gpui::transfer::{quit_decision, QuitDecision};
        let unsaved = self
            .layer_manager
            .layers()
            .iter()
            .filter_map(|l| l.as_osm())
            .any(|l| l.is_modified());
        match quit_decision(
            osm_gpui::transfer::blocks_edits(self.transfer.as_ref()),
            unsaved,
        ) {
            QuitDecision::Quit => cx.quit(),
            QuitDecision::ConfirmUnsaved => self.show_quit_confirm_dialog(window, cx),
            QuitDecision::UploadInProgress => self.refuse_quit_during_upload(cx),
        }
    }

    /// Quitting mid-upload is refused: between two changesets the layer and
    /// the server would disagree, and the next upload would send the first
    /// changeset's objects again.
    pub(crate) fn refuse_quit_during_upload(&mut self, cx: &mut Context<Self>) {
        self.set_status(tr!("status.upload_blocks_quit"));
        cx.notify();
    }

    /// Open the "unsaved changes" quit-confirmation dialog, if one isn't
    /// already open. Called directly by `on_quit` and by the
    /// `on_window_should_close` hook (which already has a `Window` in
    /// scope). Like
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

    /// Handle the app menu's "About" item. Window-scoped (like `on_quit`)
    /// so the dialog can be built with the `Window` handed to the dispatch.
    fn on_show_about(&mut self, _: &ShowAbout, window: &mut Window, cx: &mut Context<Self>) {
        if self.about_dialog.is_some() {
            return;
        }
        let dialog = cx.new(|cx| osm_gpui::ui::about_dialog::AboutDialog::new(window, cx));
        cx.subscribe(
            &dialog,
            |this, _entity, _event: &osm_gpui::ui::about_dialog::DialogEvent, cx| {
                this.about_dialog = None;
                cx.notify();
            },
        )
        .detach();
        self.about_dialog = Some(dialog);
        cx.notify();
    }

    /// Open the upload-review dialog, if one isn't already open — either
    /// showing the "nothing to upload" status or the dialog itself,
    /// depending on whether any layer has changes. Called directly by
    /// `menu::upload_to_osm` (via `with_map_viewer_in`).
    pub(crate) fn open_upload_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let signed_in_name =
            auth::stored_display_name(&auth::oauth_base_for(&settings_store::api_base_url()));
        self.open_upload_dialog_as(signed_in_name.as_deref(), window, cx);
    }

    /// Whether any layer has changes to upload (enables File > Upload to OSM).
    pub(crate) fn has_uploadable_changes(&self) -> bool {
        self.layer_manager
            .layers()
            .iter()
            .filter_map(|l| l.as_osm())
            .any(|l| l.is_modified())
    }

    /// Keep File > Upload to OSM disabled while no layer has changes. The
    /// menus are rebuilt (deferred, as this runs during a render) only when
    /// the answer differs from the one they were last built for.
    fn sync_upload_menu(&mut self, cx: &mut Context<Self>) {
        let can_upload = self.has_uploadable_changes();
        if can_upload != self.upload_menu_enabled {
            self.upload_menu_enabled = can_upload;
            cx.defer(menu::rebuild_menus_for_live_viewer);
        }
    }

    /// Window-scoped handler for the `UploadToOsm` action (File > Upload to
    /// OSM and its shortcut). Registered on the render tree like `on_quit`:
    /// the global `menu::upload_to_osm` handler runs while this window is
    /// checked out for the dispatch, so its window lookup used to fail
    /// silently and the action never did anything.
    fn on_upload_to_osm(&mut self, _: &UploadToOsm, window: &mut Window, cx: &mut Context<Self>) {
        self.open_upload_dialog(window, cx);
    }

    /// Run the validator on the pending changes of every modified layer.
    /// Only created and modified objects (and what they affect) are checked.
    pub(crate) fn validation_report(&self) -> osm_gpui::validator::Report {
        let mut report = osm_gpui::validator::Report::default();
        for layer in self
            .layer_manager
            .layers()
            .iter()
            .filter_map(|l| l.as_osm())
        {
            if !layer.is_modified() {
                continue;
            }
            let Some(data) = layer.get_osm_data() else {
                continue;
            };
            report.add_layer(layer.id(), &data, &layer.diff_for_upload());
        }
        report
    }

    /// Window-scoped handler for Tools > Validate.
    fn on_validate(&mut self, _: &Validate, _window: &mut Window, cx: &mut Context<Self>) {
        self.show_validation(cx);
    }

    /// Select `features` and zoom to them (a click on a validator finding).
    fn show_features(
        &mut self,
        features: &[osm_gpui::selection::FeatureRef],
        cx: &mut Context<Self>,
    ) {
        self.set_selection(features.to_vec());
        self.fit_to_features(features);
        cx.notify();
    }

    /// `open_upload_dialog` with the signed-in account passed in (so tests
    /// don't depend on the on-disk token store). Always gives visible
    /// feedback: see `upload_dialog::plan_upload`.
    fn open_upload_dialog_as(
        &mut self,
        signed_in_name: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_upload_dialog_with(signed_in_name, false, window, cx);
    }

    /// The upload dialog as a script-only preview: it shows the pending
    /// edits and every control, needs no sign-in, and its Upload button
    /// never does anything (see `UploadDialog::set_preview`).
    pub(crate) fn open_upload_dialog_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_upload_dialog_with(Some("preview"), true, window, cx);
    }

    fn open_upload_dialog_with(
        &mut self,
        signed_in_name: Option<&str>,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.upload_dialog.is_some()
            || self.sign_in_dialog.is_some()
            || self.notice_dialog.is_some()
        {
            return;
        }

        let pending: Vec<(LayerId, osm_gpui::layers::diff::LayerDiff)> = self
            .layer_manager
            .layers()
            .iter()
            .filter_map(|l| l.as_osm())
            .filter(|l| l.is_modified())
            .map(|l| (l.id(), l.diff_for_upload()))
            .collect();
        let summaries: Vec<osm_gpui::ui::upload_dialog::LayerSummary> = self
            .layer_manager
            .layers()
            .iter()
            .filter_map(|l| l.as_osm())
            .filter(|l| l.is_modified())
            .zip(&pending)
            .map(|(l, (_, diff))| {
                let (created, modified, deleted) = diff.counts();
                osm_gpui::ui::upload_dialog::LayerSummary {
                    layer_name: l.name().to_string(),
                    created,
                    modified,
                    deleted,
                }
            })
            .collect();

        // Refuse up front what the server would reject: a way or relation
        // over its size limit. This comes before the sign-in check: it is
        // about the data, and signing in would not help. More changes than
        // one changeset holds are not refused: they are sent as several
        // changesets.
        let limits = osm_gpui::limits::Limits::for_server(&settings_store::api_base_url());
        let diffs: Vec<&osm_gpui::layers::diff::LayerDiff> =
            pending.iter().map(|(_, d)| d).collect();
        if let Err(violation) = osm_gpui::limits::check_upload(&diffs, &limits) {
            self.refuse_upload(
                &violation,
                pending.iter().map(|(id, _)| *id).collect(),
                window,
                cx,
            );
            return;
        }
        use osm_gpui::ui::upload_dialog::{plan_upload, UploadPlan};
        let plan = if preview {
            UploadPlan::ShowDialog
        } else {
            plan_upload(signed_in_name, &summaries)
        };
        match plan {
            UploadPlan::SignIn => {
                // Uploading needs an OSM account; send signed-out users to
                // Settings instead of letting the upload fail for lack of a
                // token.
                self.show_sign_in_dialog(
                    osm_gpui::ui::sign_in_dialog::SignInPurpose::Upload,
                    window,
                    cx,
                );
                return;
            }
            UploadPlan::NothingToUpload => {
                self.set_status(tr!("status.nothing_to_upload"));
                cx.notify();
                return;
            }
            UploadPlan::ShowDialog => {}
        }

        let changesets =
            match osm_gpui::upload_chunks::plan_chunks(&diffs, limits.changeset_elements) {
                Ok(chunks) => chunks.len(),
                Err(e) => {
                    self.set_status(e.to_string());
                    cx.notify();
                    return;
                }
            };

        let dialog =
            cx.new(|cx| osm_gpui::ui::upload_dialog::UploadDialog::new(window, cx, summaries));
        dialog.update(cx, |dialog, cx| dialog.set_changesets(changesets, cx));
        // Layers restored from a run that died mid-upload: the same "may have
        // gone through" stop as after a lost response.
        if let Some(warning) = self.restored_upload_warning() {
            dialog.update(cx, |dialog, cx| {
                dialog.fail_outcome_unknown(warning.into(), cx)
            });
        }
        // Validated once, before the first changeset is opened.
        let report = self.validation_report();
        dialog.update(cx, |dialog, cx| dialog.set_validation(report, cx));
        if !self.upload_comment_draft.is_empty() {
            let draft = self.upload_comment_draft.clone();
            dialog.update(cx, |dialog, cx| dialog.set_draft_comment(draft, window, cx));
        }
        // The sources: what was typed before the dialog was closed, else the
        // last uploaded text.
        let settings = settings_store::snapshot();
        let source = self
            .upload_source_draft
            .clone()
            .unwrap_or(settings.last_source);
        let imagery = self.visible_imagery_names();
        dialog.update(cx, |dialog, cx| {
            if !source.is_empty() {
                dialog.set_draft_source(source, window, cx);
            }
            dialog.set_imagery(imagery, cx);
            dialog.set_recent_comments(settings.recent_comments, cx);
            dialog.set_preview(preview, cx);
        });
        cx.subscribe(
            &dialog,
            |this, _entity, event: &osm_gpui::ui::upload_dialog::DialogEvent, cx| {
                use osm_gpui::ui::upload_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.close_upload_dialog_keeping_draft(cx);
                        cx.notify();
                    }
                    DialogEvent::Upload {
                        comment,
                        source,
                        tags,
                    } => {
                        // The dialog stays open to show the upload's stage;
                        // `start_upload` closes it on success.
                        this.start_upload(comment.clone(), source.clone(), tags.clone(), cx);
                    }
                    DialogEvent::ShowInSidePanel => {
                        this.close_upload_dialog_keeping_draft(cx);
                        this.show_validation_expanded(cx);
                    }
                    DialogEvent::Conflict { action, conflict } => {
                        this.handle_conflict_action(*action, conflict.clone(), cx);
                    }
                }
            },
        )
        .detach();
        self.upload_dialog = Some(dialog);
        cx.notify();
    }

    /// Report why the upload can't go ahead (see `limits::check_upload`) in
    /// a dialog that names the object and the limit (and in the status bar),
    /// and for a way or relation over its limit select it and zoom to it.
    /// `layers` are the layers the violation's `layer` index refers to, in
    /// diff order.
    fn refuse_upload(
        &mut self,
        violation: &osm_gpui::limits::LimitViolation,
        layers: Vec<LayerId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use osm_gpui::limits::LimitViolation;
        use osm_gpui::selection::{FeatureKind, FeatureRef};

        let target = match *violation {
            LimitViolation::WayTooLong { layer, id, .. } => Some((layer, FeatureKind::Way, id)),
            LimitViolation::RelationTooLarge { layer, id, .. } => {
                Some((layer, FeatureKind::Relation, id))
            }
        };
        if let Some((layer, kind, id)) = target {
            if let Some(&layer_id) = layers.get(layer) {
                let feature = FeatureRef { layer_id, kind, id };
                self.set_selection(vec![feature]);
                self.fit_to_features(&[feature]);
            }
        }
        self.set_status(violation.message());
        let dialog = cx.new(|cx| {
            osm_gpui::ui::notice_dialog::NoticeDialog::new(
                tr!("dialog.upload.refused_title"),
                violation.message(),
                window,
                cx,
            )
        });
        cx.subscribe(
            &dialog,
            |this: &mut Self, _entity, _event: &osm_gpui::ui::notice_dialog::DialogEvent, cx| {
                this.notice_dialog = None;
                cx.notify();
            },
        )
        .detach();
        self.notice_dialog = Some(dialog);
        cx.notify();
    }

    /// Show the sign-in prompt, worded for `purpose` (upload or notes).
    /// "Open Settings" closes it and opens the Settings window; Cancel/Esc
    /// just closes it. Nothing continues after sign-in in either flow: the
    /// user repeats the action once signed in.
    pub(crate) fn show_sign_in_dialog(
        &mut self,
        purpose: osm_gpui::ui::sign_in_dialog::SignInPurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog =
            cx.new(|cx| osm_gpui::ui::sign_in_dialog::SignInDialog::new(purpose, window, cx));
        cx.subscribe(
            &dialog,
            move |this, _entity, event: &osm_gpui::ui::sign_in_dialog::DialogEvent, cx| {
                use osm_gpui::ui::sign_in_dialog::DialogEvent;
                this.sign_in_dialog = None;
                if matches!(event, DialogEvent::OpenSettings) {
                    let page = (purpose == osm_gpui::ui::sign_in_dialog::SignInPurpose::Mapillary)
                        .then_some(osm_gpui::ui::settings_window::IMAGERY_PAGE);
                    // Deferred so the window is opened outside this entity update.
                    cx.defer(move |cx| crate::menu::open_settings_page(page, cx));
                }
                cx.notify();
            },
        )
        .detach();
        self.sign_in_dialog = Some(dialog);
        cx.notify();
    }

    /// Run the full upload sequence (create changeset -> build+upload
    /// osmChange -> close changeset -> reconcile local layers) on the
    /// background executor, mirroring `request_download`'s `cx.spawn`/
    /// `background_executor().spawn` structure.
    ///
    /// If the changeset is created but the upload itself fails, the changeset
    /// is closed again before the failure is reported (see
    /// `osm_upload::run_upload`). A refusal by the server (409/410/412) is
    /// handed to the dialog as a conflict the user can act on; see
    /// `handle_conflict_action`.
    fn start_upload(
        &mut self,
        comment: String,
        source: String,
        tags: osm_gpui::changeset_tags::ChangesetTags,
        cx: &mut Context<Self>,
    ) {
        // The script-only preview can never upload.
        if self
            .upload_dialog
            .as_ref()
            .is_some_and(|d| d.read(cx).is_preview())
        {
            return;
        }
        // A scripted run never uploads anywhere but the development server,
        // whatever key or click reached this point.
        if let Some(refusal) =
            script_harness::scripted_upload_refusal(&settings_store::api_base_url())
        {
            if let Some(dialog) = &self.upload_dialog {
                let message = refusal.clone();
                dialog.update(cx, |dialog, cx| dialog.fail(message.into(), cx));
            }
            self.set_status(refusal);
            cx.notify();
            return;
        }
        // Layers are looked up by `LayerId`, not name (names aren't unique
        // identity — see `LayerId`'s doc comment), so both the id and the
        // display name are captured up front: the id for `find_layer`/
        // `find_layer_mut`, the name only for the outgoing changeset XML.
        let modified: Vec<(LayerId, String)> = self
            .layer_manager
            .layers()
            .iter()
            .filter_map(|l| l.as_osm())
            .filter(|l| l.is_modified())
            .map(|l| (l.id(), l.name().to_string()))
            .collect();
        let diffs: Vec<osm_gpui::layers::diff::LayerDiff> = modified
            .iter()
            .filter_map(|(id, _)| {
                self.layer_manager
                    .find_layer(*id)
                    .and_then(|l| l.as_osm())
                    .map(|l| l.diff_for_upload())
            })
            .collect();

        if diffs.iter().all(|d| d.is_empty()) {
            self.set_status(tr!("status.nothing_to_upload"));
            cx.notify();
            return;
        }

        if self.transfer.is_some() {
            self.set_status(tr!("transfer.busy"));
            cx.notify();
            return;
        }

        let progress = TransferHandle::new(TransferKind::Upload, TransferPhase::OpeningChangeset);
        self.status_message = None;
        self.transfer = Some(progress.clone());
        if let Some(dialog) = &self.upload_dialog {
            dialog.update(cx, |dialog, cx| dialog.begin_transfer(progress.clone(), cx));
        }
        self.poll_transfer(progress.clone(), cx);
        cx.notify();

        let base_url = settings_store::api_base_url();
        let oauth_base = auth::oauth_base_for(&base_url);
        let layer_ids: Vec<LayerId> = modified.iter().map(|(id, _)| *id).collect();
        let max_elements = osm_gpui::limits::Limits::for_server(&base_url).changeset_elements;
        let mut driver = {
            let refs: Vec<&osm_gpui::layers::diff::LayerDiff> = diffs.iter().collect();
            match osm_gpui::upload_chunks::UploadDriver::plan(&refs, max_elements, &tags) {
                Ok(driver) => driver,
                Err(e) => {
                    self.transfer = None;
                    if let Some(dialog) = &self.upload_dialog {
                        let message = e.to_string();
                        dialog.update(cx, |dialog, cx| dialog.fail(message.into(), cx));
                    }
                    self.set_status(e.to_string());
                    cx.notify();
                    return;
                }
            }
        };
        drop(diffs);

        // The chunks go one at a time: each is sent on the background
        // executor, and when it is accepted the layers are updated HERE, on
        // the foreground, before the next chunk is handed out. So the id map
        // in `queue` and the layers never disagree, and if the app is lost
        // between two chunks, what the layers still hold as pending is
        // exactly what the server has not got.
        let http = self.http.clone();
        cx.spawn(async move |this, cx| {
            let stop: Option<UploadStop> = loop {
                let Some(request) = driver.next_request() else {
                    break None;
                };
                // Durable before the first request of the chunk leaves: if the
                // app dies from here until the result is applied and
                // autosaved, the next start must know the server may have it.
                let marker_write = this
                    .update(cx, |this, _| {
                        this.upload_marker_prepare(osm_gpui::autosave::UploadMarker::for_request(
                            &base_url,
                            &request,
                            crate::autosave_driver::now_unix(),
                        ))
                    })
                    .ok()
                    .flatten();
                let (oauth_base, base_url, progress, http) = (
                    oauth_base.clone(),
                    base_url.clone(),
                    progress.clone(),
                    http.clone(),
                );
                let outcome = cx
                    .background_executor()
                    .spawn(async move {
                        if let Some(write) = marker_write {
                            write.run();
                        }
                        osm_gpui::upload_chunks::send(
                            request,
                            http.as_ref(),
                            &base_url,
                            // May block on the system keychain's access prompt
                            // the first time; say so while it does (and only
                            // then). A refusal stops the upload with the edits
                            // and the saved login untouched.
                            &mut || {
                                auth::ensure_fresh_token_notify(
                                    http.as_ref(),
                                    &oauth_base,
                                    &mut || {
                                        progress.set_upload_phase(
                                            None,
                                            TransferPhase::WaitingForKeychain,
                                        )
                                    },
                                )
                                .map(|token| token.access_token)
                                .map_err(|e| e.to_string())
                            },
                            &mut |part, phase| progress.set_upload_phase(part, phase),
                        )
                    })
                    .await;
                let step = driver.record(outcome);
                let marker_after = osm_gpui::upload_chunks::marker_after(&step);
                match step {
                    osm_gpui::upload_chunks::Step::Apply(done) => {
                        let layer_ids = layer_ids.clone();
                        let applied = this.update(cx, |this, cx| {
                            for id in &layer_ids {
                                if let Some(layer) = this.layer_manager.find_layer_mut(*id) {
                                    osm_gpui::upload_chunks::apply_to_layer(layer, &done);
                                }
                            }
                            // History that names objects the server has now
                            // created or deleted can't be replayed (it holds
                            // placeholder ids or deleted objects).
                            this.undo_stack
                                .remove_touching(&uploaded_taint(&layer_ids, &done.chunk));
                            this.data_generation += 1;
                            // The autosave must reflect the chunk just
                            // applied, now: a crash from here on must not
                            // offer the accepted objects for upload again.
                            this.autosave_now(cx);
                            this.upload_marker_after(marker_after, cx);
                            cx.notify();
                        });
                        if applied.is_err() {
                            // The view is gone; nothing more to update.
                            return;
                        }
                    }
                    osm_gpui::upload_chunks::Step::Stop(stopped) => {
                        let _ =
                            this.update(cx, |this, cx| this.upload_marker_after(marker_after, cx));
                        break Some(stopped.reason);
                    }
                }
            };
            let (total, accepted, last_changeset) =
                (driver.total(), driver.accepted(), driver.last_changeset());

            let _ = this.update(cx, |this, cx| {
                this.transfer = None;
                this.data_generation += 1;
                match stop {
                    None => {
                        this.upload_dialog = None;
                        this.upload_comment_draft.clear();
                        this.upload_source_draft = None;
                        // Remember the source and comment for next time. Never
                        // written to disk from a script run or a test.
                        settings_store::remember_upload_text(
                            &source,
                            &comment,
                            may_persist_settings(),
                        );
                        if accepted > 1 {
                            this.set_status(tr_count!(
                                "status.uploaded_many",
                                accepted,
                                changeset = last_changeset
                            ));
                        } else {
                            this.set_status(tr!("status.uploaded", changeset = last_changeset));
                        }
                    }
                    Some(why) => {
                        // Every changeset accepted so far is already in the
                        // layers (applied as it landed), so what is pending
                        // is exactly what was not sent.
                        if let Some(dialog) = this.upload_dialog.clone() {
                            if accepted > 0 {
                                this.refresh_upload_dialog(&dialog, cx);
                            }
                            let partial = (total > 1).then_some((accepted, total));
                            dialog.update(cx, |dialog, _| dialog.set_partial(partial));
                        }
                        match why {
                            UploadStop::Refused(conflict) => {
                                // Keep the dialog open: it names the object
                                // and offers the ways forward.
                                let message = conflict.describe();
                                let view = this.conflict_view(conflict);
                                if let Some(dialog) = &this.upload_dialog {
                                    dialog.update(cx, |dialog, cx| {
                                        dialog.fail_with_conflict(view, cx)
                                    });
                                }
                                this.set_status(message);
                            }
                            UploadStop::OutcomeUnknown(message) => {
                                // The dialog says to check the server first
                                // and does not offer a plain retry.
                                if let Some(dialog) = &this.upload_dialog {
                                    dialog.update(cx, |dialog, cx| {
                                        dialog.fail_outcome_unknown(message.clone().into(), cx)
                                    });
                                }
                                this.set_status(message);
                            }
                            UploadStop::Failed(e) => {
                                // Keep the dialog open on the error so it can
                                // be read and the upload retried; the status
                                // bar gets the message too.
                                if let Some(dialog) = &this.upload_dialog {
                                    dialog
                                        .update(cx, |dialog, cx| dialog.fail(e.clone().into(), cx));
                                }
                                this.set_status(e);
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Close the upload dialog without uploading, remembering what was typed
    /// in the comment field so reopening it restores it.
    fn close_upload_dialog_keeping_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.upload_dialog.take() {
            self.upload_comment_draft = dialog.read(cx).draft_comment(cx);
            self.upload_source_draft = Some(dialog.read(cx).draft_source(cx));
        }
    }

    /// Names of the visible imagery layers (tile layers other than the
    /// built-in OSM Carto map, which is the map style rather than imagery),
    /// in layer order, for the changeset's `imagery_used` tag.
    fn visible_imagery_names(&self) -> Vec<String> {
        self.layer_manager
            .layers()
            .iter()
            .filter(|l| {
                l.is_visible()
                    && l.as_imagery().is_some_and(|i| {
                        i.imagery_id() != osm_gpui::layers::tile_layer::OSM_CARTO_IMAGERY_ID
                    })
            })
            .map(|l| l.name().to_string())
            .collect()
    }

    /// Bring an open upload dialog's pending counts and changeset count up to
    /// date with the layers (after part of an upload went through).
    fn refresh_upload_dialog(
        &self,
        dialog: &gpui::Entity<osm_gpui::ui::upload_dialog::UploadDialog>,
        cx: &mut Context<Self>,
    ) {
        let pending: Vec<(String, osm_gpui::layers::diff::LayerDiff)> = self
            .layer_manager
            .layers()
            .iter()
            .filter_map(|l| l.as_osm())
            .filter(|l| l.is_modified())
            .map(|l| (l.name().to_string(), l.diff_for_upload()))
            .collect();
        let summaries: Vec<osm_gpui::ui::upload_dialog::LayerSummary> = pending
            .iter()
            .map(|(name, diff)| {
                let (created, modified, deleted) = diff.counts();
                osm_gpui::ui::upload_dialog::LayerSummary {
                    layer_name: name.clone(),
                    created,
                    modified,
                    deleted,
                }
            })
            .filter(|s| !s.is_empty())
            .collect();
        let limits = osm_gpui::limits::Limits::for_server(&settings_store::api_base_url());
        let diffs: Vec<&osm_gpui::layers::diff::LayerDiff> =
            pending.iter().map(|(_, d)| d).collect();
        let changesets = osm_gpui::upload_chunks::plan_chunks(&diffs, limits.changeset_elements)
            .map(|c| c.len())
            .unwrap_or(1);
        dialog.update(cx, |dialog, cx| {
            dialog.set_pending(summaries, changesets, cx)
        });
    }

    /// The layer that holds `(kind, id)` or has it as a local deletion.
    fn conflict_layer(&self, kind: osm_gpui::selection::FeatureKind, id: i64) -> Option<LayerId> {
        self.layer_manager
            .layers()
            .iter()
            .find(|l| {
                l.as_editable()
                    .is_some_and(|e| e.has_feature(kind, id) || e.is_locally_deleted(kind, id))
            })
            .map(|l| l.id())
    }

    /// A refused upload plus what the loaded data says about its object:
    /// whether it can be shown, and whether the user deleted it.
    fn conflict_view(
        &self,
        conflict: osm_upload::UploadConflict,
    ) -> osm_gpui::ui::upload_dialog::ConflictView {
        let editables = || {
            self.layer_manager
                .layers()
                .iter()
                .filter_map(|l| l.as_editable())
        };
        let (located, deleted_locally) = match conflict.object {
            Some((kind, id)) => (
                editables().any(|e| e.has_feature(kind, id)),
                editables().any(|e| e.is_locally_deleted(kind, id)),
            ),
            None => (false, false),
        };
        osm_gpui::ui::upload_dialog::ConflictView {
            conflict,
            located,
            deleted_locally,
        }
    }

    /// Carry out the way forward the user picked for a refused upload.
    ///
    /// - Show: select the object and zoom to it. The dialog is closed so the
    ///   map is visible (File > Upload reopens it with the comment restored).
    /// - Keep mine / Use theirs: ask the server for the object's current
    ///   state on a worker thread (reading is public: no sign-in needed),
    ///   then `apply_resolution`; the dialog returns to its ready state.
    /// - Drop my change: forget the local delete, no network needed.
    fn handle_conflict_action(
        &mut self,
        action: osm_gpui::ui::upload_dialog::ConflictAction,
        conflict: osm_upload::UploadConflict,
        cx: &mut Context<Self>,
    ) {
        use osm_gpui::osm_api::ServerElement;
        use osm_gpui::ui::upload_dialog::ConflictAction;

        let Some((kind, id)) = conflict.object else {
            return;
        };
        match action {
            ConflictAction::Show => {
                if let Some(layer_id) = self.conflict_layer(kind, id) {
                    let feature = osm_gpui::selection::FeatureRef { layer_id, kind, id };
                    self.set_selection(vec![feature]);
                    self.fit_to_features(&[feature]);
                }
                self.close_upload_dialog_keeping_draft(cx);
                self.set_status(conflict.describe());
                cx.notify();
            }
            ConflictAction::DropMyChange => {
                let result = self.apply_resolution(action, kind, id, ServerElement::Gone);
                self.finish_resolution(conflict, result, cx);
            }
            ConflictAction::KeepMine | ConflictAction::UseTheirs => {
                let label = if action == ConflictAction::KeepMine {
                    tr!("dialog.upload.conflict.checking")
                } else {
                    tr!("dialog.upload.conflict.fetching")
                };
                if let Some(dialog) = &self.upload_dialog {
                    dialog.update(cx, |dialog, cx| dialog.begin_resolving(label, cx));
                }
                cx.notify();
                let base_url = settings_store::api_base_url();
                let full = action == ConflictAction::UseTheirs;
                let http = self.http.clone();
                cx.spawn(async move |this, cx| {
                    let fetched = cx
                        .background_executor()
                        .spawn(async move {
                            osm_gpui::osm_api::fetch_element_with(
                                http.as_ref(),
                                &base_url,
                                None,
                                kind,
                                id,
                                full,
                            )
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        let result = match fetched {
                            Ok(server) => this.apply_resolution(action, kind, id, server),
                            Err(e) => Err(e.to_string().into()),
                        };
                        this.finish_resolution(conflict, result, cx);
                    });
                })
                .detach();
            }
        }
    }

    /// Show how resolving went: back to the ready dialog with a note, or the
    /// conflict again with the reason it didn't work.
    fn finish_resolution(
        &mut self,
        conflict: osm_upload::UploadConflict,
        result: Result<SharedString, SharedString>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(notice) => {
                if let Some(dialog) = &self.upload_dialog {
                    dialog.update(cx, |dialog, cx| dialog.resolved(notice.clone(), cx));
                }
                self.set_status(notice);
            }
            Err(error) => {
                let view = self.conflict_view(conflict);
                if let Some(dialog) = &self.upload_dialog {
                    dialog.update(cx, |dialog, cx| {
                        dialog.resolve_failed(view, error.clone(), cx)
                    });
                }
                self.set_status(error);
            }
        }
        cx.notify();
    }

    /// Change the loaded data to resolve a conflict on `(kind, id)`, given
    /// what the server has (`server`; ignored by "drop my change").
    ///
    /// - Keep mine moves the object's base version to the server's current
    ///   one; the local edit stays and so does the undo history (a version
    ///   isn't part of any history entry's effect, except that undoing a
    ///   delete restores the version it had then).
    /// - Use theirs replaces the object (and, for a way, its nodes) with the
    ///   server's copy, or removes it if the server deleted it. History steps
    ///   that name the object (for a way, also its old and new nodes) refer
    ///   to data that was rewritten (a move of nodes the way now has
    ///   different, a delete snapshot of an object that is back with another
    ///   version) and are removed, whole batches included. Steps that name
    ///   only other features don't depend on the removed ones and stay; the
    ///   note says when steps were removed.
    /// - Drop my change forgets a local delete the server already did; the
    ///   steps naming the object go the same way (undoing the delete would
    ///   bring back an object the server no longer has).
    fn apply_resolution(
        &mut self,
        action: osm_gpui::ui::upload_dialog::ConflictAction,
        kind: osm_gpui::selection::FeatureKind,
        id: i64,
        server: osm_gpui::osm_api::ServerElement,
    ) -> Result<SharedString, SharedString> {
        use osm_gpui::ui::upload_dialog::ConflictAction;

        let name = osm_upload::describe_object(kind, id);
        let not_found = || tr!("upload_conflict.not_found", object = name);
        let layer_id = self.conflict_layer(kind, id).ok_or_else(not_found)?;

        let mut tainted: Vec<osm_gpui::selection::FeatureRef> = Vec::new();
        let changed = match action {
            ConflictAction::KeepMine => {
                let Some(version) = server.version(kind, id) else {
                    return Err(tr!("upload_conflict.keep_gone", object = name));
                };
                let layer = self.layer_manager.find_layer_mut(layer_id);
                let known = layer
                    .and_then(|l| l.as_editable_mut())
                    .is_some_and(|e| e.set_base_version(kind, id, version));
                if !known {
                    return Err(not_found());
                }
                return Ok(tr!("upload_conflict.kept_mine", object = name));
            }
            ConflictAction::UseTheirs => {
                // Which features' history can no longer be trusted: the
                // object, and for a way both its old local nodes and the
                // nodes of the server's copy (they are rewritten, restored
                // or dropped along with it).
                tainted.extend(self.resolution_taint(layer_id, kind, id, &server));
                self.layer_manager
                    .find_layer_mut(layer_id)
                    .and_then(|l| l.as_editable_mut())
                    .is_some_and(|e| e.adopt_server_element(kind, id, server))
            }
            ConflictAction::DropMyChange => {
                tainted.push(osm_gpui::selection::FeatureRef { layer_id, kind, id });
                self.layer_manager
                    .find_layer_mut(layer_id)
                    .and_then(|l| l.as_editable_mut())
                    .is_some_and(|e| e.forget_local_delete(kind, id))
            }
            ConflictAction::Show => return Err(not_found()),
        };
        if !changed {
            return Err(not_found());
        }

        let cleared = self.undo_stack.remove_touching(&tainted);
        self.data_generation += 1;
        self.hovered = None;
        // Drop anything selected that is no longer in the data.
        let still_there: Vec<osm_gpui::selection::FeatureRef> = self
            .selected
            .iter()
            .copied()
            .filter(|f| {
                self.layer_manager
                    .find_layer(f.layer_id)
                    .and_then(|l| l.as_editable())
                    .is_some_and(|e| e.has_feature(f.kind, f.id))
            })
            .collect();
        if still_there.len() != self.selected.len() {
            self.set_selection(still_there);
        }

        let mut note = match action {
            ConflictAction::DropMyChange => tr!("upload_conflict.dropped", object = name),
            _ => tr!("upload_conflict.used_theirs", object = name),
        }
        .to_string();
        if cleared > 0 {
            note.push(' ');
            note.push_str(&tr!("upload_conflict.undo_trimmed"));
        }
        Ok(note.into())
    }

    /// The features whose undo steps stop being sound when "Use theirs"
    /// replaces `(kind, id)` with `server`.
    fn resolution_taint(
        &self,
        layer_id: LayerId,
        kind: osm_gpui::selection::FeatureKind,
        id: i64,
        server: &osm_gpui::osm_api::ServerElement,
    ) -> Vec<osm_gpui::selection::FeatureRef> {
        use osm_gpui::selection::{FeatureKind, FeatureRef};
        let mut out = vec![FeatureRef { layer_id, kind, id }];
        if kind == FeatureKind::Way {
            let mut nodes: Vec<i64> = self
                .layer_manager
                .find_layer(layer_id)
                .and_then(|l| l.as_editable())
                .and_then(|e| e.way_node_ids(id))
                .unwrap_or_default();
            if let osm_gpui::osm_api::ServerElement::Present(data) = server {
                if let Some(way) = data.ways.get(&id) {
                    nodes.extend(way.nodes.iter().copied());
                }
            }
            out.extend(nodes.into_iter().map(|n| FeatureRef {
                layer_id,
                kind: FeatureKind::Node,
                id: n,
            }));
        }
        out
    }

    /// Debug builds only, and only when `OSM_GPUI_DEBUG_TRANSFER` is set:
    /// freeze the UI in a transfer state so progress rendering can be
    /// screenshotted without a network. `upload` opens the upload dialog
    /// mid-upload; `download` / `download-total` show a running download
    /// (without / with a known size). Nothing is ever sent: the handles
    /// have no worker, and the dialog has no event subscription.
    #[cfg(debug_assertions)]
    fn debug_show_transfer_demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(demo) = std::env::var("OSM_GPUI_DEBUG_TRANSFER") else {
            return;
        };
        match demo.as_str() {
            "upload" => {
                let handle = TransferHandle::new(
                    TransferKind::Upload,
                    TransferPhase::UploadingChanges { count: 12 },
                );
                let summaries = vec![osm_gpui::ui::upload_dialog::LayerSummary {
                    layer_name: "OSM data".to_string(),
                    created: 3,
                    modified: 8,
                    deleted: 1,
                }];
                let dialog = cx.new(|cx| {
                    osm_gpui::ui::upload_dialog::UploadDialog::new(window, cx, summaries)
                });
                dialog.update(cx, |dialog, cx| dialog.begin_transfer(handle.clone(), cx));
                self.upload_dialog = Some(dialog);
                self.transfer = Some(handle);
            }
            "conflict" => {
                let summaries = vec![osm_gpui::ui::upload_dialog::LayerSummary {
                    layer_name: "OSM data".to_string(),
                    created: 3,
                    modified: 8,
                    deleted: 1,
                }];
                let view = osm_gpui::ui::upload_dialog::ConflictView {
                    conflict: osm_upload::UploadConflict::parse(
                        409,
                        "Version mismatch: Provided 3, server had: 4 of Node 12345",
                    ),
                    located: true,
                    deleted_locally: false,
                };
                let dialog = cx.new(|cx| {
                    osm_gpui::ui::upload_dialog::UploadDialog::new(window, cx, summaries)
                });
                dialog.update(cx, |dialog, cx| dialog.fail_with_conflict(view, cx));
                self.upload_dialog = Some(dialog);
            }
            "download" | "download-total" => {
                let handle =
                    TransferHandle::new(TransferKind::Download, TransferPhase::Downloading);
                let total = (demo == "download-total").then_some(2_800_000);
                handle.set_bytes(1_400_000, total);
                self.transfer = Some(handle);
            }
            _ => {}
        }
    }

    /// Re-render about every `POLL_INTERVAL` (the status bar and, for an
    /// upload, its dialog) for as long as `progress` is the running
    /// transfer. The worker writes into the shared handle; this is the only
    /// thing that turns those writes into frames.
    fn poll_transfer(&mut self, progress: TransferHandle, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(osm_gpui::transfer::POLL_INTERVAL)
                .await;
            let still_running = this.update(cx, |this, cx| {
                let running = this
                    .transfer
                    .as_ref()
                    .is_some_and(|current| current.is_same(&progress));
                if running {
                    if let Some(dialog) = &this.upload_dialog {
                        dialog.update(cx, |_, cx| cx.notify());
                    }
                    cx.notify();
                }
                running
            });
            if !still_running.unwrap_or(false) {
                break;
            }
        })
        .detach();
    }

    /// Abandon the running download (the status bar's Cancel button). The
    /// worker notices between chunks and drops the connection. Uploads are
    /// never cancellable (see `TransferProgress::is_cancellable`).
    pub(crate) fn cancel_transfer(&mut self, cx: &mut Context<Self>) {
        if let Some(transfer) = &self.transfer {
            if transfer.snapshot().is_cancellable() {
                transfer.cancel();
                cx.notify();
            }
        }
    }

    /// Kick off a background fetch of the OSM data within the current
    /// viewport bounds, applying the result to `self` on completion. Called
    /// directly from the `File > Download from OSM` menu handler (via
    /// `with_map_viewer`).
    /// Restyle every open layer with the active MapCSS style (the built-in
    /// one when no custom style is active) and repaint.
    pub(crate) fn apply_map_style(&mut self, cx: &mut Context<Self>) {
        let sheet = osm_gpui::style::custom::active_stylesheet();
        for layer in self
            .layer_manager
            .layers_mut()
            .iter_mut()
            .filter_map(|l| l.as_osm_mut())
        {
            layer.set_stylesheet(sheet.clone());
        }
        cx.notify();
    }

    pub(crate) fn request_download(&mut self, cx: &mut Context<Self>) {
        let bounds = self.viewport.visible_bounds();

        let base_url = settings_store::api_base_url();
        // Lazily (re)fetch the server's capabilities if we have none or it
        // last reported itself offline; never blocks.
        osm_gpui::capabilities::ensure_fetched(self.http.clone(), &base_url);

        if osm_gpui::capabilities::for_server(&base_url).is_offline() {
            self.set_status(tr!("status.api_offline"));
            cx.notify();
            return;
        }

        if let Err(e) = osm_api::check_area(&bounds, &base_url) {
            self.set_status(e.to_string());
            cx.notify();
            return;
        }

        if self.transfer.is_some() {
            self.set_status(tr!("transfer.busy"));
            cx.notify();
            return;
        }

        let progress = TransferHandle::new(TransferKind::Download, TransferPhase::Requesting);
        self.status_message = None;
        self.transfer = Some(progress.clone());
        self.poll_transfer(progress.clone(), cx);
        cx.notify();

        let http = self.http.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // A map download works anonymously, so it never waits on the
                    // system keychain: `fresh_token_if_loaded` uses the token only
                    // if it is already in memory (it may refresh over the network
                    // if expired, hence this background thread) and otherwise, or
                    // on any error, the request goes out anonymously.
                    let token = auth::fresh_token_if_loaded(
                        http.as_ref(),
                        &auth::oauth_base_for(&base_url),
                    )
                    .map(|t| t.access_token);
                    osm_api::fetch_bbox_tracked_with(
                        http.as_ref(),
                        bounds,
                        &base_url,
                        token.as_deref(),
                        &progress,
                    )
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.transfer = None;
                this.data_generation += 1;
                match result {
                    Ok(data) => {
                        this.apply_download(data);
                        // New data, new downloaded area: refetch the notes.
                        if this.shows_notes() {
                            this.refresh_notes(cx);
                        }
                    }
                    Err(e) => this.set_status(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Put downloaded OSM data on the map. It is merged, by object id, into
    /// the active layer when there is one — so downloading again after a
    /// pan extends the same editable dataset instead of stacking a second
    /// copy of every object in a new layer — keeping any local edits (see
    /// `osm_gpui::layers::merge`). With no active layer, the download
    /// becomes a new layer and is made active.
    fn apply_download(&mut self, data: OsmData) {
        let target = self
            .active_layer
            .and_then(|id| self.layer_manager.find_layer_mut(id))
            .and_then(|layer| layer.as_editable_mut());
        match target {
            Some(editable) => {
                let stats = editable.merge_download(data);
                self.set_status(tr!("status.downloaded", summary = stats.summary()));
            }
            None => {
                let name = self.layer_manager.unique_name(DOWNLOAD_LAYER_NAME);
                let layer_id = self.layer_manager.alloc_id();
                let layer = OsmLayer::new_with_data(layer_id, name, Arc::new(data));
                self.layer_manager.add_layer(Box::new(layer));
                self.note_download_origin(layer_id);
                self.active_layer = Some(layer_id);
                self.status_message = None;
            }
        }
    }
}

/// Name of the layer created by the first download.
const DOWNLOAD_LAYER_NAME: &str = "OSM data";

impl Render for MapViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window_bounds = Some(window.bounds());
        // Times building the element tree (not layout or paint).
        let _render_scope =
            osm_gpui::frame_stats::Scoped::new("view.render", osm_gpui::frame_stats::start());
        // Consume any pending script command first.
        self.process_script_command(window, cx);
        self.sync_upload_menu(cx);

        // Starting an inline tag edit is the one remaining case that must
        // happen inside a render pass (its deferred select-all-on-open
        // depends on the inputs being built in the pass that first paints
        // them — see its doc comment). Every other former polling queue (new
        // OSM data, layer requests, download requests, debug-overlay toggle,
        // the dialogs) is applied directly to `self` by menu handlers /
        // background-task completions via `MAP_VIEWER_HANDLE`, so there's
        // nothing else to drain here.
        self.check_for_pending_tag_edit(window, cx);
        self.check_for_pending_role_edit(window, cx);
        self.check_for_pending_relation_picker(window, cx);
        self.check_for_pending_parallel_dialog(window, cx);
        self.check_for_pending_restore(window, cx);
        self.notes_frame(window, cx);
        self.imagery_frame(cx);
        self.maybe_rebuild_imagery_menu(cx);
        // Re-run the validator (debounced) if the edits changed since the
        // Validation section was last filled.
        self.watch_validation(cx);
        self.sync_history_local(cx);

        // Now it's safe to signal: the effects of this frame's commands
        // and pushes are visible.
        if let Some(bus) = SCRIPT_BUS.get() {
            bus.signal_done_and_frame();
        }

        // Size the viewport to the map area: the window's content area (not
        // `window.bounds()`, which includes the title bar) minus the side
        // panels and the status bar.
        let window_size = window.viewport_size();
        let panel_width = px(self.side_panel_width);
        let left_panel_width = px(Self::MODE_PANEL_WIDTH);
        let map_size = gpui::size(
            window_size.width - panel_width - left_panel_width,
            window_size.height - px(status_bar::STATUS_BAR_HEIGHT) - menu::top_chrome_height(),
        );
        self.viewport.update_size(map_size);

        self.expire_status();
        self.drop_stale_offset_adjust();

        self.keep_dialog_focus(window, cx);

        // Keyboard shortcuts scoped to this view only match while focus is
        // somewhere inside it. With nothing focused at all — at launch, or
        // after a focused text input was removed — give focus to the map so
        // they (and the map's bare-key shortcuts) keep working.
        if window.focused(cx).is_none() {
            window.focus(&self.focus_handle, cx);
        }

        self.sync_selection_to_layers();

        let (center_lat, center_lon) = self.viewport.center();
        let zoom_level = self.viewport.zoom_level();
        let (total_tiles, cached_files, osm_objects) = self.get_layer_stats();
        let fps = self.tick_fps();
        let status_bar = self.render_status_bar(cx);

        // The stylesheet can set the map's background colour and hide the
        // imagery (`canvas { fill-color: ..; show-imagery: no; }`).
        let canvas_style = osm_gpui::style::custom::active_stylesheet().canvas_style();
        let main_row = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .child(self.render_mode_panel(cx))
            .child(
                // Map area
                div()
                    .id("map-area")
                    .flex_1()
                    .relative()
                    .track_focus(&self.focus_handle)
                    .cursor(match &self.offset_adjust {
                        // Imagery is dragged with the left button: a hand.
                        Some(adjust) if !self.viewport.is_dragging && !self.space_held => {
                            if adjust.dragging() {
                                gpui::CursorStyle::ClosedHand
                            } else {
                                gpui::CursorStyle::OpenHand
                            }
                        }
                        _ => map_cursor(
                            // A New Note waiting for its click: a crosshair.
                            if self.notes_armed() {
                                EditMode::Add
                            } else {
                                self.mode
                            },
                            self.viewport.is_dragging,
                            self.space_held,
                            self.hovered.is_some(),
                        ),
                    })
                    // When the pointer leaves the map, drop everything that
                    // tracks it: the hover highlight, the status bar's cursor
                    // coordinates, and the drawing modes' rubber bands.
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !*hovered {
                            this.hovered = None;
                            this.clear_vertex_preview();
                            this.snap_preview = None;
                            this.last_mouse_pos = None;
                            cx.notify();
                        }
                    }))
                    // The right and middle buttons pan the map. A right press
                    // released without moving is a click instead: it opens
                    // the selection-actions context menu.
                    .map(|mut area| {
                        for button in [gpui::MouseButton::Right, gpui::MouseButton::Middle] {
                            let is_right = button == gpui::MouseButton::Right;
                            area = area
                                .on_mouse_down(
                                    button,
                                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                                        if is_right {
                                            this.right_press = Some(ev.position);
                                        }
                                        let position = this.window_to_map(ev.position);
                                        this.start_pan_drag(position, cx);
                                    }),
                                )
                                .on_mouse_up(
                                    button,
                                    cx.listener(move |this, ev: &MouseUpEvent, window, cx| {
                                        this.end_pan_drag(cx);
                                        if is_right {
                                            this.finish_right_press(ev.position, window, cx);
                                        }
                                    }),
                                )
                                .on_mouse_up_out(
                                    button,
                                    cx.listener(move |this, _ev: &MouseUpEvent, _, cx| {
                                        if is_right {
                                            this.right_press = None;
                                        }
                                        this.end_pan_drag(cx);
                                    }),
                                );
                        }
                        area
                    })
                    // Left button: selection, box-select, or move-drag if the
                    // press lands on an already-selected feature — or a pan,
                    // while Space is held.
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                            if this.menu_swallow_press {
                                // This press only closed an open menu.
                                return;
                            }
                            window.focus(&this.focus_handle, cx);
                            if this.rotate_session.is_some() {
                                // The release applies the rotation.
                                return;
                            }
                            let position = this.window_to_map(ev.position);
                            if this.space_held {
                                this.start_pan_drag(position, cx);
                            } else {
                                this.handle_map_mouse_down(position, ev.modifiers);
                            }
                        }),
                    )
                    .on_mouse_up_out(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _ev: &MouseUpEvent, _, cx| {
                            if this.viewport.is_dragging {
                                this.end_pan_drag(cx);
                            }
                        }),
                    )
                    .on_key_up(cx.listener(|this, ev: &gpui::KeyUpEvent, _, cx| {
                        if ev.keystroke.key == "space" && this.space_held {
                            this.space_held = false;
                            cx.notify();
                        }
                    }))
                    .on_pinch(cx.listener(|this, ev: &gpui::PinchEvent, _, cx| {
                        this.handle_pinch(ev, cx);
                    }))
                    .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                        let modifiers = ev.keystroke.modifiers;
                        if this.offset_field_focused(window, cx) {
                            // Typing in the imagery offset panel's fields.
                        } else if modifiers.platform || modifiers.control || modifiers.alt {
                            // Modifier chords belong to the keymap (menu
                            // shortcuts), never to the bare-key handling below.
                        } else if ev.keystroke.key == "space" {
                            // Held Space turns a left-drag into a pan.
                            if !this.space_held {
                                this.space_held = true;
                                cx.notify();
                            }
                        } else if this.offset_adjust_key(
                            &ev.keystroke.key,
                            modifiers.shift,
                            window,
                            cx,
                        ) {
                            // Adjusting imagery offset: arrows nudge the
                            // imagery, Escape cancels, Enter keeps.
                        } else if this.handle_navigation_key(&ev.keystroke.key, cx) {
                            // Arrow keys pan; +/- zoom.
                        } else if ev.keystroke.key == "escape" && this.close_photo_fullscreen() {
                            cx.notify();
                        } else if ev.keystroke.key == "escape" && this.cancel_rotate() {
                            // A Rotate in progress is undone.
                            cx.notify();
                        } else if ev.keystroke.key == "enter" && this.rotate_session.is_some() {
                            this.finish_rotate();
                            cx.notify();
                        } else if ev.keystroke.key == "escape" && this.cancel_armed_note() {
                            // A New Note waiting for its click is cancelled.
                            cx.notify();
                        } else if ev.keystroke.key == "escape" {
                            let cancelled_drag = this.cancel_move_drag(cx);
                            match this.mode {
                                // Escape backs out one step at a time: an
                                // in-progress shape first, then the mode.
                                EditMode::Add => {
                                    if this.add_progress.take().is_none() {
                                        this.mode = EditMode::Select;
                                    }
                                }
                                EditMode::Building => {
                                    this.building_press = None;
                                    if this.building_progress.take().is_none() {
                                        this.mode = EditMode::Select;
                                        this.building_reference = None;
                                    }
                                }
                                EditMode::Extrude => {
                                    if this.extrude_drag.take().is_none() {
                                        this.mode = EditMode::Select;
                                    }
                                }
                                EditMode::Arc => {
                                    if this.arc_progress.take().is_none() {
                                        this.mode = EditMode::Select;
                                    }
                                }
                                EditMode::Improve => {
                                    // First Esc lets go of the way (to pick
                                    // another), the second leaves the mode.
                                    if this.improve_target().is_some() {
                                        this.set_selection(Vec::new());
                                    } else {
                                        this.mode = EditMode::Select;
                                    }
                                }
                                EditMode::Select => {
                                    if !cancelled_drag {
                                        this.set_selection(Vec::new());
                                    }
                                }
                            }
                            cx.notify();
                        } else if ev.keystroke.key == "enter" && this.mode == EditMode::Add {
                            if this.add_progress.take().is_some() {
                                cx.notify();
                            }
                        } else if ev.keystroke.key == "delete" || ev.keystroke.key == "backspace" {
                            this.delete_selected_features(cx);
                        } else {
                            let settings = osm_gpui::settings_store::snapshot();
                            let key = ev.keystroke.key.as_str();
                            // Mode-switch shortcuts are handled here rather
                            // than as global key bindings so they only fire
                            // while the map area has focus (see the comment
                            // by `cx.bind_keys` in `main()`).
                            let key_mode = EditMode::ALL.into_iter().find(|m| {
                                key == osm_gpui::keybindings::effective_spec(
                                    &settings,
                                    m.shortcut_id(),
                                )
                            });
                            if let Some(mode) = key_mode {
                                this.on_set_mode(&SetMode { mode }, window, cx);
                            } else if let Some(action) =
                                selection_actions::find_by_key(&settings, key)
                            {
                                // Like the mode keys above, these only fire
                                // while the map area has focus, so typing in
                                // a text input never triggers them.
                                this.run_selection_action(action.id, cx);
                            }
                        }
                    }))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, ev: &MouseUpEvent, _, cx| {
                            if std::mem::take(&mut this.menu_swallow_press) {
                                return;
                            }
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
                            .when_some(canvas_style.background, |d, color| d.bg(rgb(color)))
                            // Render all layer elements (raster content like tiles)
                            .children(self.layer_manager.render_all_elements(
                                &self.viewport,
                                canvas_style.show_imagery,
                                window,
                                cx,
                            ))
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
                                    // An already-selected feature keeps its
                                    // selection highlight under the cursor.
                                    let hovered =
                                        self.hovered.filter(|h| !self.selected.contains(h));
                                    let fs_frame = osm_gpui::frame_stats::start();
                                    move |bounds, _, window, cx| {
                                        let fs_paint = osm_gpui::frame_stats::start();
                                        let _paint_scope =
                                            osm_gpui::frame_stats::Scoped::new("canvas.paint", fs_paint);
                                        let _frame_scope = osm_gpui::frame_stats::Scoped::new(
                                            "frame.render_start_to_canvas_end",
                                            fs_frame,
                                        );
                                        osm_gpui::frame_stats::interval("frame.interval");
                                        let this = entity.read(cx);
                                        // Downloaded-area wash first: above imagery,
                                        // below every layer's features.
                                        if this.show_downloaded_area {
                                            this.layer_manager.render_all_download_bounds(
                                                &viewport_clone,
                                                bounds,
                                                window,
                                            );
                                        }
                                        let fs_layers = osm_gpui::frame_stats::start();
                                        this.layer_manager.render_all_canvas(
                                            &viewport_clone,
                                            bounds,
                                            window,
                                        );
                                        osm_gpui::frame_stats::lap("layers.canvas", fs_layers);
                                        // A selection of tens of thousands (Edit >
                                        // Find) highlights only its first
                                        // `MAX_SELECTION_HIGHLIGHTS`; features off
                                        // screen cost almost nothing.
                                        for sel in selected.iter().take(MAX_SELECTION_HIGHLIGHTS) {
                                            this.layer_manager.render_highlight(
                                                sel,
                                                HighlightKind::Selected,
                                                &viewport_clone,
                                                bounds,
                                                window,
                                            );
                                        }

                                        // Hover goes after the selection so a hovered
                                        // vertex of a selected way shows above the way.
                                        if let Some(hovered) = &hovered {
                                            this.layer_manager.render_highlight(
                                                hovered,
                                                HighlightKind::Hovered,
                                                &viewport_clone,
                                                bounds,
                                                window,
                                            );
                                        }

                                        this.layer_manager.render_all_labels(
                                            &viewport_clone,
                                            bounds,
                                            &selected,
                                            hovered.as_ref(),
                                            window,
                                            cx,
                                        );

                                        if let Some(reference) = this.building_reference {
                                            // Mark the way the building is aligned to.
                                            this.layer_manager.render_highlight(
                                                &osm_gpui::selection::FeatureRef {
                                                    layer_id: reference.layer_id,
                                                    kind: osm_gpui::selection::FeatureKind::Way,
                                                    id: reference.way_id,
                                                },
                                                HighlightKind::Hovered,
                                                &viewport_clone,
                                                bounds,
                                                window,
                                            );
                                        }

                                        if let Some(progress) = this.building_progress {
                                            let origin_x = bounds.origin.x;
                                            let origin_y = bounds.origin.y;
                                            let a_screen = viewport_clone.geo_to_screen(
                                                progress.corner_a.0,
                                                progress.corner_a.1,
                                            );
                                            match progress.corner_b {
                                                None if this.building_reference.is_some() => {
                                                    // Oriented: the cursor is the opposite
                                                    // corner, so preview the whole rectangle.
                                                    let half = px(4.0);
                                                    let quad_bounds = Bounds {
                                                        origin: point(
                                                            a_screen.x + origin_x - half,
                                                            a_screen.y + origin_y - half,
                                                        ),
                                                        size: size(px(8.0), px(8.0)),
                                                    };
                                                    window.paint_quad(fill(
                                                        quad_bounds,
                                                        rgb(0x3b82f6),
                                                    ));
                                                    let corners = this
                                                        .building_reference
                                                        .zip(this.last_mouse_pos)
                                                        .and_then(|(reference, mouse)| {
                                                            osm_gpui::building::rectangle_from_opposite_corners(
                                                                progress.corner_a,
                                                                viewport_clone.screen_to_geo(mouse),
                                                                reference.orientation,
                                                            )
                                                        });
                                                    if let Some(corners) = corners {
                                                        let mut builder = PathBuilder::stroke(px(2.0));
                                                        for (i, c) in
                                                            corners.iter().chain(Some(&corners[0])).enumerate()
                                                        {
                                                            let p = viewport_clone.geo_to_screen(c.0, c.1);
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
                                                    window.paint_quad(fill(
                                                        quad_bounds,
                                                        rgb(0x3b82f6),
                                                    ));

                                                    if let Some(mouse_pos) = this.last_mouse_pos {
                                                        let p0 = point(
                                                            a_screen.x + origin_x,
                                                            a_screen.y + origin_y,
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
                                                            window.paint_path(path, rgb(0x3b82f6));
                                                        }
                                                    }
                                                }
                                                Some(corner_b) => {
                                                    let cursor_geo = this
                                                        .last_mouse_pos
                                                        .map(|p| viewport_clone.screen_to_geo(p))
                                                        .unwrap_or(corner_b);
                                                    let (far_a, far_b) = this
                                                        .rectangle_from_geo_edge(
                                                            progress.corner_a,
                                                            corner_b,
                                                            cursor_geo,
                                                        );
                                                    let b_screen = viewport_clone
                                                        .geo_to_screen(corner_b.0, corner_b.1);
                                                    let far_a_screen = viewport_clone
                                                        .geo_to_screen(far_a.0, far_a.1);
                                                    let far_b_screen = viewport_clone
                                                        .geo_to_screen(far_b.0, far_b.1);
                                                    let pts = [
                                                        a_screen,
                                                        b_screen,
                                                        far_b_screen,
                                                        far_a_screen,
                                                        a_screen,
                                                    ];
                                                    let mut builder = PathBuilder::stroke(px(2.0));
                                                    for (i, p) in pts.iter().enumerate() {
                                                        let p =
                                                            point(p.x + origin_x, p.y + origin_y);
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

                                        if let Some(progress) = this.arc_progress {
                                            // Screen position of a geo point, in window space.
                                            let to_window = |g: (f64, f64)| {
                                                let p = viewport_clone.geo_to_screen(g.0, g.1);
                                                point(p.x + bounds.origin.x, p.y + bounds.origin.y)
                                            };
                                            let blue = rgb(0x3b82f6);
                                            let half = px(4.0);
                                            let marker = |window: &mut Window, g: (f64, f64)| {
                                                let c = to_window(g);
                                                window.paint_quad(fill(
                                                    Bounds {
                                                        origin: point(c.x - half, c.y - half),
                                                        size: size(px(8.0), px(8.0)),
                                                    },
                                                    blue,
                                                ));
                                            };
                                            marker(window, progress.start.geo);
                                            // The cursor, snapped onto a way as a click would be.
                                            let cursor_geo = this.last_mouse_pos.map(|p| {
                                                match this.active_layer {
                                                    Some(layer_id) => {
                                                        this.arc_point_at(layer_id, p, false).geo
                                                    }
                                                    None => viewport_clone.screen_to_geo(p),
                                                }
                                            });
                                            // The path to draw: the start..cursor
                                            // rubber band before the end is placed,
                                            // then the arc through the cursor (a
                                            // straight chord while it is degenerate).
                                            let path_points: Vec<(f64, f64)> = match (
                                                progress.end,
                                                cursor_geo,
                                            ) {
                                                (None, Some(cursor)) => vec![progress.start.geo, cursor],
                                                (None, None) => Vec::new(),
                                                (Some(end), cursor) => {
                                                    marker(window, end.geo);
                                                    cursor
                                                        .and_then(|c| {
                                                            this.arc_from_geo(
                                                                progress.start.geo,
                                                                end.geo,
                                                                c,
                                                            )
                                                        })
                                                        .unwrap_or_else(|| {
                                                            vec![progress.start.geo, end.geo]
                                                        })
                                                }
                                            };
                                            let mut builder = PathBuilder::stroke(px(2.0));
                                            for (i, g) in path_points.iter().enumerate() {
                                                let p = to_window(*g);
                                                if i == 0 {
                                                    builder.move_to(p);
                                                } else {
                                                    builder.line_to(p);
                                                }
                                            }
                                            if let Ok(path) = builder.build() {
                                                window.paint_path(path, blue);
                                            }
                                        }

                                        // Improve Way preview: the target way as it
                                        // would look after a click at the cursor.
                                        if this.mode == EditMode::Improve {
                                            if let Some(ip) = this
                                                .last_mouse_pos
                                                .and_then(|c| this.improve_plan_at(c))
                                            {
                                                use osm_gpui::improve_way::{marker, preview, Plan};
                                                let color = if matches!(ip.plan, Plan::Delete { .. }) {
                                                    rgb(0xef4444)
                                                } else {
                                                    rgb(0x3b82f6)
                                                };
                                                let to_window = |p: (f64, f64)| {
                                                    point(
                                                        px(p.0 as f32) + bounds.origin.x,
                                                        px(p.1 as f32) + bounds.origin.y,
                                                    )
                                                };
                                                let outline = preview(&ip.points, &ip.ids, ip.plan);
                                                let mut builder = PathBuilder::stroke(px(2.0));
                                                for (i, p) in outline.iter().enumerate() {
                                                    let p = to_window(*p);
                                                    if i == 0 {
                                                        builder.move_to(p);
                                                    } else {
                                                        builder.line_to(p);
                                                    }
                                                }
                                                if let Ok(path) = builder.build() {
                                                    window.paint_path(path, color);
                                                }
                                                let c = to_window(marker(&ip.points, &ip.ids, ip.plan));
                                                let half = px(5.0);
                                                window.paint_quad(fill(
                                                    Bounds {
                                                        origin: point(c.x - half, c.y - half),
                                                        size: size(px(10.0), px(10.0)),
                                                    },
                                                    color,
                                                ));
                                            }
                                        }

                                        // Reshape/Move preview: the way's whole
                                        // outline as it would be after the drop.
                                        if let Some(drag) = this
                                            .extrude_drag
                                            .as_ref()
                                            .filter(|d| d.style != ExtrudeStyle::NewWay)
                                        {
                                            if let Some(mouse_pos) = this.last_mouse_pos {
                                                if let Some(result) =
                                                    this.compute_extrude(drag, mouse_pos)
                                                {
                                                    let mut builder = PathBuilder::stroke(px(2.0));
                                                    for (i, p) in result.outline.iter().enumerate()
                                                    {
                                                        let p = point(
                                                            px(p.0 as f32) + bounds.origin.x,
                                                            px(p.1 as f32) + bounds.origin.y,
                                                        );
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

                                        if let Some(drag) = this
                                            .extrude_drag
                                            .as_ref()
                                            .filter(|d| d.style == ExtrudeStyle::NewWay)
                                        {
                                            if let Some(mouse_pos) = this.last_mouse_pos {
                                                if let Some(layer) =
                                                    this.layer_manager.find_layer(drag.layer)
                                                {
                                                    if let Some(editable) = layer.as_editable() {
                                                        if let (Some(a_geo), Some(b_geo)) = (
                                                            editable.node_lat_lon(drag.node_a),
                                                            editable.node_lat_lon(drag.node_b),
                                                        ) {
                                                            let origin_x = bounds.origin.x;
                                                            let origin_y = bounds.origin.y;
                                                            let cursor_geo = viewport_clone
                                                                .screen_to_geo(mouse_pos);
                                                            let (far_a, far_b) = this
                                                                .rectangle_from_geo_edge(
                                                                    a_geo, b_geo, cursor_geo,
                                                                );
                                                            let a_screen = viewport_clone
                                                                .geo_to_screen(a_geo.0, a_geo.1);
                                                            let b_screen = viewport_clone
                                                                .geo_to_screen(b_geo.0, b_geo.1);
                                                            let far_a_screen = viewport_clone
                                                                .geo_to_screen(far_a.0, far_a.1);
                                                            let far_b_screen = viewport_clone
                                                                .geo_to_screen(far_b.0, far_b.1);
                                                            let pts = [
                                                                a_screen,
                                                                b_screen,
                                                                far_b_screen,
                                                                far_a_screen,
                                                                a_screen,
                                                            ];
                                                            let mut builder =
                                                                PathBuilder::stroke(px(2.0));
                                                            for (i, p) in pts.iter().enumerate() {
                                                                let p = point(
                                                                    p.x + origin_x,
                                                                    p.y + origin_y,
                                                                );
                                                                if i == 0 {
                                                                    builder.move_to(p);
                                                                } else {
                                                                    builder.line_to(p);
                                                                }
                                                            }
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

                                        if let Some(target) = &this.snap_preview {
                                            paint_snap_target(
                                                target,
                                                &viewport_clone,
                                                bounds.origin,
                                                window,
                                            );
                                        }

                                        if let Some(progress) = &this.add_progress {
                                            if let Some(mouse_pos) = this.last_mouse_pos {
                                                if let Some(layer_id) = this.active_layer {
                                                    if let Some(layer) =
                                                        this.layer_manager.find_layer(layer_id)
                                                    {
                                                        if let Some(editable) = layer.as_editable()
                                                        {
                                                            if let Some(last_geo) = editable
                                                                .node_lat_lon(progress.last_node_id)
                                                            {
                                                                let origin_x = bounds.origin.x;
                                                                let origin_y = bounds.origin.y;
                                                                let last_screen = viewport_clone
                                                                    .geo_to_screen(
                                                                        last_geo.0, last_geo.1,
                                                                    );
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
                                .child(format!("Zoom: {:.1}", zoom_level))
                                .child(format!("Center: {:.4}, {:.4}", center_lat, center_lon))
                                .child(format!("Objects: {}", osm_objects))
                                .child(format!("Tiles: {} visible", total_tiles))
                                .child(format!("Cache: {} files", cached_files))
                                .child(format!("FPS: {:.0}", fps))
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
                    .children(self.object_history.view.clone())
                    .children(self.render_imagery_offset_panel(window, cx))
                    .children(self.render_gps_traces_legend(cx))
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
                            layer
                                .as_imagery()?
                                .attribution()
                                .map(|a| (a.text.clone(), a.url.clone()))
                        });
                        let credits = interaction::dedupe_attributions(raw_credits);
                        if credits.is_empty() {
                            div().into_any_element()
                        } else {
                            let n = credits.len();
                            let link_hover = cx.theme().link_hover;
                            // Tucked into the map's bottom-right corner with
                            // no margin and small text, so it covers as
                            // little of the map as it can while staying
                            // readable.
                            div()
                                .absolute()
                                .bottom_0()
                                .right_0()
                                .px_1()
                                .bg(cx.theme().popover)
                                .rounded_tl_sm()
                                .text_color(cx.theme().popover_foreground)
                                .text_size(px(10.0))
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
                    })
                    // Last, so it paints over every other map overlay.
                    .children(self.render_welcome(cx)),
            )
            .child(
                // Right panel with layer controls
                self.render_side_panel(window, cx),
            );

        div()
            .size_full()
            .key_context(MAP_VIEWER_KEY_CONTEXT)
            .bg(cx.theme().background)
            .text_size(osm_gpui::ui::style::current_text_scale().body)
            .flex()
            .flex_col()
            // Seen in the capture phase, before the open menu dismisses
            // itself on this same press.
            .capture_any_mouse_down(cx.listener(|this, ev: &MouseDownEvent, window, _| {
                this.menu_swallow_press =
                    ev.button == gpui::MouseButton::Left && menu::in_window_menu_open(window);
            }))
            // Windows/Linux (or macOS with OSM_GPUI_IN_WINDOW_MENU=1): the
            // menu bar is a row inside the window, above everything else.
            .children(menu::in_window_menu_bar(cx).map(|bar| {
                div()
                    .h(px(menu::IN_WINDOW_MENU_BAR_HEIGHT))
                    .w_full()
                    .flex_none()
                    .px_1()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(bar)
            }))
            .child(main_row)
            .child(status_bar)
            // A side panel resize drag is tracked here, on the window-sized
            // root, so it keeps following the pointer over the map and the
            // panel alike.
            .when(self.side_panel_resize.is_some(), |root| {
                root.cursor_col_resize()
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                        this.drag_side_panel_resize(ev, cx);
                    }))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _ev: &MouseUpEvent, _, cx| {
                            this.finish_side_panel_resize(cx);
                        }),
                    )
            })
            // Alt / Ctrl switch snapping off: update the highlight when one
            // is pressed or released, without a mouse move.
            .on_modifiers_changed(
                cx.listener(|this, ev: &gpui::ModifiersChangedEvent, _, cx| {
                    this.handle_modifiers_changed(ev.modifiers, cx);
                }),
            )
            .on_action(cx.listener(Self::on_export_osm_file))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_deselect_all))
            .on_action(cx.listener(Self::on_find))
            .on_action(cx.listener(Self::on_show_object_history))
            .on_action(cx.listener(Self::on_delete_selection))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_to_data))
            .on_action(cx.listener(Self::on_zoom_to_selection))
            .on_action(cx.listener(Self::on_toggle_downloaded_area))
            .on_action(cx.listener(Self::on_download_gps_traces))
            .on_action(cx.listener(Self::on_toggle_notes))
            .on_action(cx.listener(Self::on_refresh_notes))
            .on_action(cx.listener(Self::on_toggle_mapillary))
            .on_action(cx.listener(Self::on_toggle_fotoshi))
            .on_action(cx.listener(Self::on_toggle_panoramax))
            .on_action(cx.listener(Self::on_refresh_street_imagery))
            .on_action(cx.listener(Self::on_new_note))
            .on_action(cx.listener(Self::on_minimize_window))
            .on_action(cx.listener(Self::on_zoom_window))
            .on_action(cx.listener(Self::on_move_layer))
            .on_action(cx.listener(Self::on_delete_layer))
            .on_action(cx.listener(Self::on_set_active_layer))
            .on_action(cx.listener(Self::on_toggle_layer_visibility))
            .on_action(cx.listener(Self::on_adjust_imagery_offset))
            .on_action(cx.listener(Self::on_adjust_layer_imagery_offset))
            .on_action(cx.listener(Self::on_set_gps_trace_style))
            .on_action(cx.listener(Self::on_reset_imagery_offset))
            .on_action(cx.listener(Self::on_reset_layer_imagery_offset))
            .on_action(cx.listener(Self::on_set_imagery_offset))
            .on_action(cx.listener(Self::on_set_mode))
            .on_action(cx.listener(Self::on_run_selection_action))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_apply_nsi_preset))
            .on_action(cx.listener(Self::on_change_feature_type))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_upload_to_osm))
            .on_action(cx.listener(Self::on_validate))
            .on_action(cx.listener(Self::on_open_custom_imagery_dialog))
            .on_action(cx.listener(Self::on_show_about))
            .children(self.open_modals(cx).into_iter().map(|m| m.view))
            .children(self.render_photo_fullscreen(window, cx))
            .children(self.render_context_menu(window, cx))
    }
}

fn main() {
    eprintln!("🚀 Starting OSM-GPUI Map Viewer with Tile Loading");

    let args = parse_cli_args();
    let window_size_given = args.window_size.is_some();
    let (win_w, win_h) = args.window_size.unwrap_or((1200, 800));
    let cli_language = args.language.clone();
    let cli_theme = args.theme;
    // The one place the persistence policy is raised above its safe default
    // (`Disabled`, which every test binary keeps). A normal run reads and
    // writes the user's saved state; a scripted run reads it but never
    // writes it. The single exception is the OAuth file, which auth narrows
    // to the development server's login so the live-test sign-in persists
    // and no production login can be touched.
    if args.script.is_some() {
        persist::set_policy(persist::Policy::ReadOnly);
        if let Some(oauth_file) = auth::default_path() {
            persist::allow_read_only_writes_to(oauth_file);
        }
    } else {
        persist::set_policy(persist::Policy::Normal);
    }
    // Where autosaves go (and are looked for): the app-data directory, except
    // in a scripted run, which has none unless `--autosave-dir` names one.
    let autosave_root = osm_gpui::autosave::resolve_root(
        args.script.is_some(),
        args.autosave_dir.clone(),
        osm_gpui::autosave::default_root(),
    );
    // `--api-server` applies to this run only: it lives outside the settings
    // store, so nothing here is ever saved to settings.json.
    settings_store::set_run_override(args.api_server.clone());

    osm_gpui::http::install_default_client(Arc::new(osm_gpui::http::UreqClient::new()));

    // Initialize the global idle tracker before the app starts so TileCache
    // picks up the same Arc.
    let idle = IdleTracker::new();
    GLOBAL_IDLE.set(idle.clone()).ok();

    // Initialize script bus
    let bus = ScriptBus::new();
    SCRIPT_BUS.set(bus.clone()).ok();
    KEYSTROKE_QUEUE.set(Arc::new(Mutex::new(Vec::new()))).ok();

    IMAGERY_INDEX.set(Arc::new(Mutex::new(Vec::new()))).unwrap();

    // If there's a script, spawn it on a background OS thread before the app
    // starts. The thread blocks until the window is visible, then drives the
    // live app via ScriptBus.
    if let Some(script_path) = args.script {
        script_harness::SCRIPT_MODE.store(true, std::sync::atomic::Ordering::Relaxed);
        let keep_open = args.keep_open;
        let script_delay = args.script_delay.unwrap_or_default();
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
                step_delay: script_delay,
            };

            let mut live_app = LiveApp {
                _idle: idle.clone(),
                bus: bus_for_runner,
            };

            match runner.run(&mut live_app, &steps) {
                Ok(()) => {
                    SCRIPT_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
                    if !keep_open {
                        // A script that ends normally is a clean exit (one
                        // that ends with `crash` never gets here).
                        autosave_driver::finish_clean();
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

    // Files to open: command-line paths now, and on macOS whatever Finder
    // hands over later (or at launch, before the window exists). They all
    // wait in `open_queue` until the window is up; `open_wake` tells the
    // foreground task that something new arrived.
    let open_queue = Arc::new(Mutex::new(osm_gpui::open_files::OpenQueue::default()));
    let (open_wake_tx, mut open_wake_rx) = futures::channel::mpsc::unbounded::<()>();
    open_queue.lock().unwrap().push(args.files.clone());
    let _ = open_wake_tx.unbounded_send(());

    let app = gpui_platform::application().with_assets(osm_gpui::assets::AppAssets);
    {
        let open_queue = open_queue.clone();
        app.on_open_urls(move |urls| {
            let paths = osm_gpui::open_files::urls_to_paths(&urls);
            if paths.is_empty() {
                return;
            }
            open_queue.lock().unwrap().push(paths);
            let _ = open_wake_tx.unbounded_send(());
        });
    }
    app.run(move |cx: &mut App| {
        gpui_component::init(cx);
        // `--theme` overrides the saved appearance for this run only.
        osm_gpui::appearance::set_run_override(cli_theme);

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
        cx.on_action(validate);
        cx.on_action(quit);
        menu::register_app_actions(cx);
        // Any normal quit (the user confirmed discarding unsaved edits, or
        // there were none) removes this run's autosaves. A crash, a force
        // quit or a power cut never gets here, which is what leaves them for
        // the next start.
        cx.on_app_quit(|_| async {
            autosave_driver::finish_clean();
        })
        .detach();

        // Load persisted custom imagery entries. Scripted runs start with an
        // empty list instead, so captures never show the user's own entries
        // (the list is never written in a scripted run either: see
        // `ScriptSeedCustomImagery`).
        let loaded = if script_harness::script_mode() {
            Vec::new()
        } else {
            custom_imagery_store::load()
        };
        custom_imagery_store::init_store(loaded);
        // Remembered imagery offsets. Scripted runs neither read nor write the
        // user's file: their offsets live for the run only.
        osm_gpui::imagery_offset::init_store(!script_harness::script_mode());

        // Load persisted app settings (OSM API server choice, cache budget) and
        // OAuth login. Seed the tile cache's live budget from the persisted value
        // before the cache does any work.
        let mut app_settings = settings_store::load();
        // Script runs always start with the welcome card showing, whatever
        // the user saved, so captures are deterministic. (In memory only.)
        if script_harness::script_mode() {
            app_settings.welcome_dismissed = false;
        }
        osm_gpui::tile_cache::set_budget_mb(app_settings.cache_budget_mb);
        // Pick the UI language before any menu or window text is built.
        // `--language` overrides the saved setting for this run only.
        osm_gpui::i18n::init(cli_language.as_deref().or(app_settings.language.as_deref()));
        settings_store::init_store(app_settings);
        // Light, dark or the OS appearance, now that the saved choice is known.
        osm_gpui::appearance::apply(cx);
        auth::init_store(auth::load());
        // Learn the server's limits (max download area etc.) in the
        // background; defaults apply until it answers.
        osm_gpui::capabilities::ensure_fetched(
            osm_gpui::http::default_client(),
            &settings_store::api_base_url(),
        );
        load_saved_map_style(cx);

        // Initial menu (before ELI loads). MapViewer's render loop will call
        // rebuild_menus again whenever the load state or viewport changes.
        rebuild_menus(
            cx,
            40.7128,
            -74.0060,
            ImageryLoadState::Loading,
            true,
            false,
        );

        let startup_http = osm_gpui::http::default_client();

        // Kick off background fetch/parse of the Name Suggestion Index.
        osm_gpui::nsi::init_store();
        cx.background_executor()
            .spawn(async move {
                match osm_gpui::nsi::fetch_and_cache(startup_http.as_ref())
                    .and_then(|body| osm_gpui::nsi::parse(&body))
                {
                    Ok(entries) => {
                        eprintln!("nsi: loaded {} brand entries", entries.len());
                        osm_gpui::nsi::set_index(osm_gpui::nsi::NsiIndex::from_entries(entries));
                    }
                    Err(e) => {
                        eprintln!("nsi: failed to load NSI data: {}", e);
                    }
                }
            })
            .detach();

        // Reopen the window where it was left, unless the command line or a
        // script sets the size, or the saved place is no longer on a screen.
        let displays: Vec<(f32, f32, f32, f32)> = cx
            .displays()
            .iter()
            .map(|d| {
                let b = d.bounds();
                (
                    b.origin.x.as_f32(),
                    b.origin.y.as_f32(),
                    b.size.width.as_f32(),
                    b.size.height.as_f32(),
                )
            })
            .collect();
        let saved_rect = if script_harness::script_mode() || window_size_given {
            None
        } else {
            settings_store::snapshot()
                .window_bounds
                .and_then(|r| r.validated(&displays))
        };
        let initial_bounds = match saved_rect {
            Some(r) => Bounds {
                origin: point(px(r.x), px(r.y)),
                size: size(px(r.width), px(r.height)),
            },
            None => Bounds {
                origin: point(px(100.0), px(100.0)),
                size: size(px(win_w as f32), px(win_h as f32)),
            },
        };
        let map_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(initial_bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some(window_title(&settings_store::api_base_url()).into()),
                        appears_transparent: false,
                        traffic_light_position: None,
                    }),
                    focus: true,
                    ..Default::default()
                },
                |window, cx| {
                    // Register keyboard bindings in the window context.
                    // Built from the user's effective shortcut settings
                    // (defaults, overridden by any customizations saved
                    // in Settings > Keyboard Shortcuts).
                    //
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
                    cx.bind_keys(key_bindings_from_settings(
                        &osm_gpui::settings_store::snapshot(),
                        cfg!(target_os = "macos"),
                    ));
                    // Follow live OS light/dark changes while on System.
                    osm_gpui::appearance::observe_system_appearance(window);
                    osm_gpui::appearance::apply_with_system(
                        gpui_component::ThemeMode::from(window.appearance()),
                        cx,
                    );
                    let view = cx.new(|cx| {
                        let mut viewer = MapViewer::new(window, cx);
                        // Start on the standard map, not a blank canvas.
                        viewer.add_default_layers();
                        viewer.init_autosave(autosave_root.clone(), cx);
                        viewer
                    });
                    #[cfg(debug_assertions)]
                    view.update(cx, |v, cx| v.debug_show_transfer_demo(window, cx));
                    // Download and parse the Editor Layer Index in the
                    // background; the viewer is told when it is done.
                    view.update(cx, |v, cx| v.load_imagery_index(cx));

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
                        if let Some(view) = MAP_VIEWER_HANDLE.get().and_then(|h| h.upgrade()) {
                            if osm_gpui::transfer::blocks_edits(view.read(cx).transfer.as_ref()) {
                                view.update(cx, |v, cx| v.refuse_quit_during_upload(cx));
                                return false;
                            }
                        }
                        if has_unsaved_changes(cx) {
                            if let Some(view) = MAP_VIEWER_HANDLE.get().and_then(|h| h.upgrade()) {
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

        // The single window exists now: open everything queued so far
        // (command-line files, a launch-by-Finder document), then keep
        // serving files that arrive while the app runs. They become
        // layers of this window; no second window is ever opened.
        open_queue.lock().unwrap().mark_ready();
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while open_wake_rx.next().await.is_some() {
                let paths = open_queue.lock().unwrap().take_ready();
                if paths.is_empty() {
                    continue;
                }
                cx.update(|cx| {
                    for path in paths {
                        menu::open_osm_path(path, cx);
                    }
                    // Bring an already-running app forward.
                    cx.activate(true);
                });
            }
        })
        .detach();
    });
}

/// Load the saved custom MapCSS style in the background (a URL may be slow)
/// and restyle open layers once it is in. Layers opened before then use the
/// built-in style, as does everything when the style cannot be loaded.
/// Scripted runs skip it so captures stay deterministic.
fn load_saved_map_style(cx: &mut App) {
    if script_harness::script_mode() {
        return;
    }
    let Some(location) = settings_store::snapshot().active_map_style else {
        return;
    };
    let http = osm_gpui::http::default_client();
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { osm_gpui::style::custom::load(&location, http.as_ref()) })
            .await;
        match result {
            Ok(sheet) => {
                cx.update(|cx| {
                    osm_gpui::style::custom::set_active(Some(std::sync::Arc::new(sheet)));
                    with_map_viewer(cx, |v, cx| v.apply_map_style(cx));
                });
            }
            Err(e) => eprintln!("MapCSS style not used, falling back to the built-in one: {e}"),
        }
    })
    .detach();
}

/// The main window's title: the app name, plus the API server's host when it
/// is not the production server, so nobody uploads to the wrong place by
/// accident.
fn window_title(api_base_url: &str) -> String {
    match settings_store::non_primary_host_label(api_base_url) {
        Some(host) => format!("{} \u{2014} {}", menu::APP_NAME, host),
        None => menu::APP_NAME.to_string(),
    }
}

fn append_custom_imagery(entry: CustomImageryEntry) {
    custom_imagery_store::append(entry);
}
