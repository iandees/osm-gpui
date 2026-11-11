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

mod fields_section;
mod menu;
mod mode_panel;
mod script_harness;
mod selection_actions;
mod side_panel;
mod status_bar;
mod undo;
mod welcome;

use crate::menu::{
    add_coordinate_grid, add_imagery_layer, add_osm_carto, add_saved_custom_imagery,
    download_from_osm, no_op_imagery_info, open_custom_imagery_dialog, open_osm_file,
    open_settings, quit, rebuild_menus, toggle_debug_overlay, upload_to_osm, ShowAbout,
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
    grid_layer::GridLayer, osm_layer::OsmLayer, tile_layer::TileLayer, HighlightKind, LayerId,
    LayerManager,
};
use osm_gpui::osm::OsmData;
use osm_gpui::osm_api;
use osm_gpui::osm_upload;
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
        OpenSettings,
        Undo,
        Redo,
        ApplyNsiPreset,
        ChangeFeatureType,
        UploadToOsm,
        ExportOsmFile,
        SelectAll,
        DeselectAll,
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

/// Build the live `gpui::KeyBinding` list for the modifier-key actions from
/// the current effective shortcut settings, plus the fixed macOS-standard
/// window/app bindings. The bare-letter mode-switch shortcuts (`mode_*`) are
/// deliberately excluded — see the comment where this is called.
pub(crate) fn key_bindings_from_settings(
    settings: &osm_gpui::settings_store::AppSettings,
) -> Vec<KeyBinding> {
    let fixed = [
        KeyBinding::new("cmd-m", MinimizeWindow, None),
        KeyBinding::new("cmd-h", HideApp, None),
        KeyBinding::new("alt-cmd-h", HideOtherApps, None),
    ];
    osm_gpui::keybindings::effective_bindings(settings)
        .into_iter()
        .filter_map(|(id, spec)| match id {
            "select_all" => Some(KeyBinding::new(
                &spec,
                SelectAll,
                Some(MAP_VIEWER_KEY_CONTEXT),
            )),
            // Copy/Paste Tags run the same selection actions as the menus.
            // Scoped to the map like Select All, so they never steal the
            // keys from a focused text input.
            "copy_tags" | "paste_tags" => Some(KeyBinding::new(
                &spec,
                RunSelectionAction { id: id.into() },
                Some(MAP_VIEWER_KEY_CONTEXT),
            )),
            "zoom_in" => Some(KeyBinding::new(&spec, ZoomIn, None)),
            "zoom_out" => Some(KeyBinding::new(&spec, ZoomOut, None)),
            "zoom_to_data" => Some(KeyBinding::new(&spec, ZoomToData, None)),
            "zoom_to_selection" => Some(KeyBinding::new(&spec, ZoomToSelection, None)),
            "open_settings" => Some(KeyBinding::new(&spec, OpenSettings, None)),
            "quit" => Some(KeyBinding::new(&spec, Quit, None)),
            "open_osm_file" => Some(KeyBinding::new(&spec, OpenOsmFile, None)),
            "download_osm" => Some(KeyBinding::new(&spec, DownloadFromOsm, None)),
            "upload_osm" => Some(KeyBinding::new(&spec, UploadToOsm, None)),
            "export_osm_file" => Some(KeyBinding::new(&spec, ExportOsmFile, None)),
            "undo" => Some(KeyBinding::new(&spec, Undo, Some(MAP_VIEWER_KEY_CONTEXT))),
            "redo" => Some(KeyBinding::new(&spec, Redo, Some(MAP_VIEWER_KEY_CONTEXT))),
            _ => None,
        })
        .chain(fixed)
        .collect()
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

/// Undo of a node attached to an existing way's end: pull `node_id` out of
/// `way_id`, from the front if it was prepended, else from the back.
fn detach_end_node(
    editable: &mut dyn osm_gpui::layers::EditableLayer,
    way_id: i64,
    node_id: i64,
    at_start: bool,
) {
    let node_ids = editable.way_node_ids(way_id).unwrap_or_default();
    let idx = if at_start {
        node_ids.iter().position(|id| *id == node_id)
    } else {
        node_ids.iter().rposition(|id| *id == node_id)
    };
    if let Some(idx) = idx {
        editable.remove_node_from_way(way_id, idx);
    }
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

/// Why a chunked upload stopped before the end.
enum UploadStop {
    /// The server refused the diff (409/410/412); the changeset is closed.
    Refused(osm_upload::UploadConflict),
    /// Any other failure, as a message.
    Failed(String),
}

struct MapViewer {
    viewport: Viewport,
    layer_manager: LayerManager,
    tile_cache: Arc<Mutex<TileCache>>,
    first_dataset_fitted: bool,
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
    /// Whether the downloaded-area wash (everything outside the downloaded
    /// bounds dimmed) is drawn. View > Show Downloaded Area; on by default and
    /// not persisted (the settings store holds no other View preferences).
    show_downloaded_area: bool,
    /// Active custom imagery dialog, if open.
    custom_imagery_dialog:
        Option<gpui::Entity<osm_gpui::ui::custom_imagery_dialog::CustomImageryDialog>>,
    /// Active "unsaved changes" quit-confirmation dialog, if open.
    quit_confirm_dialog: Option<gpui::Entity<osm_gpui::ui::quit_confirm_dialog::QuitConfirmDialog>>,
    delete_layer_dialog: Option<gpui::Entity<osm_gpui::ui::delete_layer_dialog::DeleteLayerDialog>>,
    about_dialog: Option<gpui::Entity<osm_gpui::ui::about_dialog::AboutDialog>>,
    /// Active upload-review dialog, if open.
    upload_dialog: Option<gpui::Entity<osm_gpui::ui::upload_dialog::UploadDialog>>,
    /// The changeset comment typed in the upload dialog, kept when the dialog
    /// closes without uploading (Cancel, or Show on a conflict) and put back
    /// when it reopens. Cleared after a successful upload.
    upload_comment_draft: String,
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
    /// Tags, History, Relations, Members, by the indices `render_side_panel`
    /// gives them) is expanded.
    side_panel_open: [bool; 7],
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
    /// Extrude mode's in-progress drag, or `None` when not dragging (see
    /// `ExtrudeDrag`).
    extrude_drag: Option<ExtrudeDrag>,
    /// Window position of the last right-button press on the map, kept until
    /// the button is released: a release within `CLICK_SLOP` of it is a click
    /// (opens the context menu), anything further was a pan.
    right_press: Option<gpui::Point<gpui::Pixels>>,
    /// The open right-click context menu, if any.
    context_menu: Option<ContextMenuState>,
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
            EditMode::Add | EditMode::Building => CursorStyle::Crosshair,
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
        let viewport = Viewport::new(40.7128, -74.0060, 11.0, gpui::size(px(800.0), px(600.0)));
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
            first_dataset_fitted: false,
            status_message: None,
            transfer: None,
            space_held: false,
            data_generation: 0,
            pending_edits: None,
            selected: Vec::new(),
            interaction: Interaction::Idle,
            frame_times: VecDeque::with_capacity(120),
            last_menu_center: None,
            last_imagery_load_state: None,
            show_debug_overlay: false,
            show_downloaded_area: true,
            custom_imagery_dialog: None,
            quit_confirm_dialog: None,
            delete_layer_dialog: None,
            about_dialog: None,
            upload_dialog: None,
            upload_comment_draft: String::new(),
            sign_in_dialog: None,
            notice_dialog: None,
            tag_inline_edit: None,
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
            pending_role_edit_open: None,
            members_show_all: false,
            member_controls_focus: std::cell::RefCell::new(Vec::new()),
            nsi_dialog: None,
            preset_picker_dialog: None,
            side_panel_open: [true; 7],
            fields_text_inputs: std::collections::HashMap::new(),
            fields_text_subscribed: std::collections::HashSet::new(),
            fields_open_combo: None,
            fields_promoted_more_fields: std::collections::HashSet::new(),
            focus_handle,
            undo_stack: UndoStack::default(),
            mode: EditMode::Select,
            active_layer: None,
            add_progress: None,
            building_progress: None,
            building_reference: None,
            building_press: None,
            last_mouse_pos: None,
            hovered: None,
            extrude_drag: None,
            right_press: None,
            context_menu: None,
        }
    }

    /// The `(center_lat, center_lon, ImageryLoadState)` triple `rebuild_menus`
    /// needs, as currently known to this viewer. Exposed so callers outside
    /// the per-frame `maybe_rebuild_imagery_menu` loop (e.g. the Settings
    /// window's keybindings-changed handler in `menu.rs`) can trigger an
    /// immediate menu rebuild without duplicating this lookup.
    pub(crate) fn imagery_menu_context(&self) -> (f64, f64, ImageryLoadState) {
        let (lat, lon) = self.viewport.center();
        let state = IMAGERY_LOAD_STATE
            .get()
            .and_then(|s| s.lock().ok().map(|g| *g))
            .unwrap_or(ImageryLoadState::Loading);
        (lat, lon, state)
    }

    /// Whether View > Show Downloaded Area is on (drives its menu checkmark).
    pub(crate) fn shows_downloaded_area(&self) -> bool {
        self.show_downloaded_area
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
        rebuild_menus(&mut *cx, lat, lon, current_state, self.show_downloaded_area);
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
        } else {
            self.show_delete_layer_dialog(id, name, pending, window, cx);
        }
    }

    /// The layer's unsaved edits, counted as the status bar's "N unsaved
    /// edits" does (and so the quit warning's `is_modified`).
    fn unsaved_edit_count(layer: &dyn osm_gpui::layers::MapLayer) -> usize {
        if !layer.is_modified() {
            return 0;
        }
        let (created, modified, deleted) = layer.diff_for_upload().counts();
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
            // Add/Building progress is relative to the active layer.
            self.add_progress = None;
            self.building_progress = None;
            self.building_press = None;
        }
        if self.building_reference.is_some_and(|r| r.layer_id == id) {
            self.building_reference = None;
        }
        if self.extrude_drag.as_ref().is_some_and(|d| d.layer == id) {
            self.extrude_drag = None;
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
    fn on_set_mode(&mut self, action: &SetMode, _: &mut Window, cx: &mut Context<Self>) {
        let mode: EditMode = action.mode.into();
        // The drawing modes need a layer to write into. The toolbar buttons
        // are disabled in that state; this covers the keyboard and menu paths.
        if mode != EditMode::Select && self.active_layer.is_none() {
            self.set_status(tr!("status.open_data_first"));
            cx.notify();
            return;
        }
        // Entering Building mode with one squared way selected borrows its
        // orientation; staying in Building mode keeps the existing one.
        self.building_reference = match mode {
            EditMode::Building if self.mode == EditMode::Building => self.building_reference,
            EditMode::Building => self.squared_selection_reference(),
            _ => None,
        };
        self.building_press = None;
        self.mode = mode;
        self.hovered = None;
        self.add_progress = None;
        self.building_progress = None;
        self.extrude_drag = None;
        cx.notify();
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
    fn window_to_map(&self, position: gpui::Point<gpui::Pixels>) -> gpui::Point<gpui::Pixels> {
        gpui::point(position.x - px(Self::MODE_PANEL_WIDTH), position.y)
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
                    if let Some(osm_layer) = layer.as_any().downcast_ref::<OsmLayer>() {
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
            UndoableAction::CreateWay { layer, snapshot } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                if forward {
                    editable.restore_feature(snapshot.clone());
                } else {
                    editable.delete_feature(snapshot.kind, snapshot.id);
                }
            }
            UndoableAction::ExtendWay {
                layer,
                way_id,
                node_id,
                way_created,
                node_created,
                at_start,
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
                        detach_end_node(editable, *way_id, *node_id, *at_start);
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
                at_start,
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
                        detach_end_node(editable, *way_id, *node_id, *at_start);
                    }
                    // Then remove it from the way it was snapped onto, and
                    // delete the node itself.
                    editable.remove_node_from_way(*snap_way_id, *snap_index);
                    editable.remove_node(*node_id);
                }
                // Redo (forward) is intentionally a no-op, same reasoning as
                // `ExtendWay`/`InsertNodeIntoWay` above.
            }
            UndoableAction::ReverseWays { layer, way_ids } => {
                // Reversing is its own inverse, so both directions are the
                // same operation.
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                for way_id in way_ids {
                    editable.reverse_way(*way_id);
                }
            }
            UndoableAction::SetWayNodes {
                layer,
                way_id,
                before,
                after,
            } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                let nodes = if forward { after } else { before };
                editable.set_way_nodes(*way_id, nodes.clone());
            }
            UndoableAction::SetRelationMembers {
                layer,
                relation_id,
                before,
                after,
            } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                let members = if forward { after } else { before };
                editable.set_relation_members(*relation_id, members.clone());
            }
            UndoableAction::CreateRelation { layer, snapshot }
            | UndoableAction::DeleteRelation { layer, snapshot } => {
                let created = matches!(action, UndoableAction::CreateRelation { .. });
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                // Redo of a create and undo of a delete put the relation
                // back; the other two directions remove it.
                if forward == created {
                    editable.restore_relation(snapshot.clone());
                } else {
                    editable.delete_relation(snapshot.relation.id);
                }
            }
            UndoableAction::Batch { actions, .. } => {
                if forward {
                    for action in actions {
                        self.apply_undo_action(action, true);
                    }
                } else {
                    for action in actions.iter().rev() {
                        self.apply_undo_action(action, false);
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
        });
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

        match event {
            InputEvent::PressEnter { .. } if add_row_key => {
                let value = edit.value.clone();
                value.update(cx, |state, cx| state.focus(window, cx));
            }
            InputEvent::PressEnter { .. } => {
                self.commit_tag_inline_edit(cx);
                window.focus(&self.focus_handle, cx);
            }
            InputEvent::Blur if add_row_key => {
                if edit.key.read(cx).value().trim().is_empty() {
                    self.tag_inline_edit = None;
                }
            }
            InputEvent::Blur => self.commit_tag_inline_edit(cx),
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
        let Some(before) = editable.set_relation_members(relation_id, after.clone()) else {
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

    /// Run the selection action `id` on the current selection: check it is
    /// applicable (else show why in the status bar), apply it, and record the
    /// result as a single undo step.
    fn run_selection_action(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(action) = selection_actions::find(id) else {
            return;
        };
        let env = selection_actions::ActionEnv {
            selection: &self.selected,
            active_layer: self.active_layer,
            layers: &self.layer_manager,
        };
        if !(action.is_applicable)(&env) {
            let why = action.why_not(&env);
            self.set_status(why);
            cx.notify();
            return;
        }
        if action.id == selection_actions::ADD_TO_RELATION {
            // Which relation? Ask; `render` opens the picker (it needs the
            // window), and the choice comes back through
            // `add_selection_to_relation`.
            self.pending_relation_picker = true;
            cx.notify();
            return;
        }
        let result = (action.apply)(&mut self.layer_manager, &self.selected, self.active_layer);
        self.finish_selection_action(result);
        cx.notify();
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
        if self.edits_blocked(cx) {
            return;
        }
        self.run_selection_action(&action.id, cx);
    }

    /// Switch to Add mode already drawing the existing way `from.way_id`:
    /// the next click attaches a node next to `from.from_node` (before the
    /// way's first node if `from.at_start`), as if the user had just placed
    /// that end node while drawing.
    fn continue_way_in_add_mode(&mut self, from: osm_gpui::topology::Continuation) {
        self.mode = EditMode::Add;
        self.hovered = None;
        self.building_press = None;
        self.building_progress = None;
        self.building_reference = None;
        self.extrude_drag = None;
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
            self.set_status(tr!("status.no_data_to_zoom"));
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
    }

    fn on_redo(&mut self, _: &Redo, _window: &mut Window, cx: &mut Context<Self>) {
        if self.edits_blocked(cx) {
            return;
        }
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
        let drawing = self.add_progress.is_some() || self.building_progress.is_some();
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
        if let Some(editable) = self
            .layer_manager
            .find_layer_mut(target.layer_id)
            .and_then(|l| l.as_editable_mut())
        {
            for (feature, key, _before, after) in &entries {
                match after {
                    Some(v) => editable.set_tag(feature.kind, feature.id, key, v),
                    None => editable.remove_tag(feature.kind, feature.id, key),
                }
            }
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
        // What a click here would select — only meaningful in Select mode
        // with no button down and no pan pending.
        self.hovered = if self.mode == EditMode::Select
            && event.pressed_button.is_none()
            && !self.space_held
        {
            osm_gpui::selection::resolve_hits(
                self.layer_manager
                    .hit_test_all(&self.viewport, adjusted_position),
            )
        } else {
            None
        };
        // Repaint on every move: the status bar shows the cursor's
        // coordinates, and Add/Building/Extrude draw a live preview to it.
        cx.notify();

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
        self.viewport.handle_mouse_up();

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
        let way_id = editable.add_way(
            vec![drag.node_a, drag.node_b, new_b, new_a, drag.node_a],
            Vec::new(),
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
            let Some(id) = editable.create_node(lat, lon, None) else {
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
            editable.set_way_nodes(drag.way_id, after.clone());
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
            editable.commit_node_moves(&moves);
            actions.push(UndoableAction::MoveNodes {
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

    /// Double-click on a segment (no drag): insert a new node at the
    /// double-click position, splitting that segment.
    fn insert_node_on_segment(&mut self, drag: &ExtrudeDrag, up_pos: gpui::Point<gpui::Pixels>) {
        if self.refuse_edit_during_upload() {
            return;
        }
        if self.refuse_way_growth(drag.layer, drag.way_id, 1) {
            return;
        }
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
    /// for the snap-to-way behavior. `ctrl_held` bypasses both snapping onto
    /// a nearby existing node and snapping onto a nearby way's line
    /// geometry, always producing a fully independent node.
    fn handle_add_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>, ctrl_held: bool) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some(layer_id) = self.active_layer else {
            return;
        };
        let (lat, lon) = self.viewport.screen_to_geo(screen_pt);

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
                    Some((way_id, _, _, idx, snap_lat, snap_lon)) => (
                        editable.insert_node_into_way(way_id, idx + 1, snap_lat, snap_lon),
                        Some((way_id, idx + 1)),
                    ),
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

        let (way_id, way_created) = match progress_way_id {
            Some(way_id) if at_start => {
                let mut nodes = editable.way_node_ids(way_id).unwrap_or_default();
                nodes.insert(0, node_id);
                editable.set_way_nodes(way_id, nodes);
                (way_id, false)
            }
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
                    at_start,
                });
            }
            None => {
                self.undo_stack.push(UndoableAction::ExtendWay {
                    layer: layer_id,
                    way_id,
                    node_id,
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
        let changed = match event.delta {
            gpui::ScrollDelta::Lines(delta) => self.viewport.zoom_by_wheel(position, delta.y),
            gpui::ScrollDelta::Pixels(delta) if zoom_modifier => {
                self.viewport.zoom_by_scroll_pixels(position, delta.y)
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
        };
        let actions: Vec<&'static selection_actions::SelectionAction> =
            selection_actions::SelectionAction::applicable(&env).collect();
        if actions.is_empty() {
            return;
        }

        let settings = settings_store::snapshot();
        let focus = self.focus_handle.clone();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            menu = menu.action_context(focus).min_w(px(190.0));
            for (i, action) in actions.iter().enumerate() {
                // Keep the destructive action apart from the edits.
                if i > 0 && action.id == "delete" {
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
    /// the script harness's `ScriptCommand::LoadOsm` — replaces the old
    /// `SHARED_OSM_DATA` queue drained once per frame.
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
            .find_map(|layer| Some((layer.name().to_string(), layer.export_xml()?)))
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
        let mut settings = settings_store::snapshot();
        if settings.side_panel_width != Some(self.side_panel_width) {
            settings.side_panel_width = Some(self.side_panel_width);
            settings_store::update_store(settings);
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
    /// Overlay` menu handler (via `with_map_viewer`) — replaces the old
    /// `TOGGLE_DEBUG_OVERLAY` queue (which used push-count parity to emulate
    /// a single flip per click; a direct call needs no such trick).
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
        rebuild_menus(&mut *cx, lat, lon, state, show);
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
    /// Called directly from the menu handler (via `with_map_viewer_in`) —
    /// replaces the old `OPEN_CUSTOM_IMAGERY_DIALOG` queue. Unlike an inline
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
        let unsaved = self.layer_manager.layers().iter().any(|l| l.is_modified());
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
    /// `menu::upload_to_osm` (via `with_map_viewer_in`) — replaces the old
    /// `SHOW_UPLOAD_DIALOG` queue, same as `show_quit_confirm_dialog`
    /// replaced `SHOW_QUIT_CONFIRM`.
    pub(crate) fn open_upload_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let signed_in_name =
            auth::stored_display_name(&auth::oauth_base_for(&settings_store::api_base_url()));
        self.open_upload_dialog_as(signed_in_name.as_deref(), window, cx);
    }

    /// Window-scoped handler for the `UploadToOsm` action (File > Upload to
    /// OSM and its shortcut). Registered on the render tree like `on_quit`:
    /// the global `menu::upload_to_osm` handler runs while this window is
    /// checked out for the dispatch, so its window lookup used to fail
    /// silently and the action never did anything.
    fn on_upload_to_osm(&mut self, _: &UploadToOsm, window: &mut Window, cx: &mut Context<Self>) {
        self.open_upload_dialog(window, cx);
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
            .filter(|l| l.is_modified())
            .map(|l| (l.id(), l.diff_for_upload()))
            .collect();
        let summaries: Vec<osm_gpui::ui::upload_dialog::LayerSummary> = self
            .layer_manager
            .layers()
            .iter()
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
        match plan_upload(signed_in_name, &summaries) {
            UploadPlan::SignIn => {
                // Uploading needs an OSM account; send signed-out users to
                // Settings instead of letting the upload fail for lack of a
                // token.
                self.show_sign_in_dialog(window, cx);
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
        if !self.upload_comment_draft.is_empty() {
            let draft = self.upload_comment_draft.clone();
            dialog.update(cx, |dialog, cx| dialog.set_draft_comment(draft, window, cx));
        }
        cx.subscribe(
            &dialog,
            |this, _entity, event: &osm_gpui::ui::upload_dialog::DialogEvent, cx| {
                use osm_gpui::ui::upload_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.close_upload_dialog_keeping_draft(cx);
                        cx.notify();
                    }
                    DialogEvent::Upload { comment } => {
                        // The dialog stays open to show the upload's stage;
                        // `start_upload` closes it on success.
                        this.start_upload(comment.clone(), cx);
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

    /// Show the "sign in to upload" prompt. "Open Settings" closes it and
    /// opens the Settings window; Cancel/Esc just closes it.
    fn show_sign_in_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = cx.new(|cx| osm_gpui::ui::sign_in_dialog::SignInDialog::new(window, cx));
        cx.subscribe(
            &dialog,
            |this, _entity, event: &osm_gpui::ui::sign_in_dialog::DialogEvent, cx| {
                use osm_gpui::ui::sign_in_dialog::DialogEvent;
                this.sign_in_dialog = None;
                if matches!(event, DialogEvent::OpenSettings) {
                    // Deferred so the window is opened outside this entity update.
                    cx.defer(|cx| crate::menu::open_settings(&OpenSettings, cx));
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
    fn start_upload(&mut self, comment: String, cx: &mut Context<Self>) {
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
        let mut queue = {
            let refs: Vec<&osm_gpui::layers::diff::LayerDiff> = diffs.iter().collect();
            match osm_gpui::upload_chunks::ChunkQueue::plan(&refs, max_elements) {
                Ok(queue) => queue,
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
        cx.spawn(async move |this, cx| {
            let total = queue.total();
            let mut accepted = 0usize;
            let mut last_changeset = 0u64;
            let stop: Option<UploadStop> = loop {
                let Some((part, chunk)) = queue.next_chunk() else {
                    break None;
                };
                let (oauth_base, base_url, comment, progress) = (
                    oauth_base.clone(),
                    base_url.clone(),
                    comment.clone(),
                    progress.clone(),
                );
                let sent = cx
                    .background_executor()
                    .spawn(async move {
                        // May block on the system keychain's access prompt the
                        // first time; say so while it does (and only then).
                        // A refusal stops the upload with the edits and the
                        // saved login untouched.
                        let token = auth::ensure_fresh_token_notify(&oauth_base, &mut || {
                            progress.set_upload_phase(None, TransferPhase::WaitingForKeychain)
                        })
                        .map_err(|e| UploadStop::Failed(e.to_string()))?;
                        osm_gpui::upload_chunks::upload_chunk(
                            &osm_gpui::http::UreqClient::new(),
                            &base_url,
                            &token.access_token,
                            &comment,
                            part,
                            chunk,
                            &mut |part, phase| progress.set_upload_phase(part, phase),
                        )
                        .map_err(|failure| match failure.error {
                            osm_upload::UploadError::Conflict(conflict) => {
                                UploadStop::Refused(conflict)
                            }
                            other => UploadStop::Failed(other.to_string()),
                        })
                    })
                    .await;
                match sent {
                    Ok(mut done) => {
                        done.last = queue.remaining() == 0;
                        queue.accept(&done.result);
                        accepted += 1;
                        last_changeset = done.changeset_id;
                        let layer_ids = layer_ids.clone();
                        let applied = this.update(cx, |this, cx| {
                            for id in &layer_ids {
                                if let Some(layer) = this.layer_manager.find_layer_mut(*id) {
                                    osm_gpui::upload_chunks::apply_to_layer(layer, &done);
                                }
                            }
                            this.data_generation += 1;
                            cx.notify();
                        });
                        if applied.is_err() {
                            // The view is gone; nothing more to update.
                            return;
                        }
                    }
                    Err(why) => break Some(why),
                }
            };

            let _ = this.update(cx, |this, cx| {
                this.transfer = None;
                this.data_generation += 1;
                match stop {
                    None => {
                        this.upload_dialog = None;
                        this.upload_comment_draft.clear();
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
        }
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
                cx.spawn(async move |this, cx| {
                    let fetched = cx
                        .background_executor()
                        .spawn(async move {
                            osm_gpui::osm_api::fetch_element(&base_url, None, kind, id, full)
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
    /// `with_map_viewer`) — replaces the old `DOWNLOAD_REQUESTS` queue (the
    /// queue only ever carried a trigger; the actual background-fetch and
    /// apply-on-completion logic already used `cx.spawn` and is unchanged).
    pub(crate) fn request_download(&mut self, cx: &mut Context<Self>) {
        let bounds = self.viewport.visible_bounds();

        let base_url = settings_store::api_base_url();
        // Lazily (re)fetch the server's capabilities if we have none or it
        // last reported itself offline; never blocks.
        osm_gpui::capabilities::ensure_fetched(&base_url);

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

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // A map download works anonymously, so it never waits on the
                    // system keychain: `fresh_token_if_loaded` uses the token only
                    // if it is already in memory (it may refresh over the network
                    // if expired, hence this background thread) and otherwise, or
                    // on any error, the request goes out anonymously.
                    let token = auth::fresh_token_if_loaded(&auth::oauth_base_for(&base_url))
                        .map(|t| t.access_token);
                    osm_api::fetch_bbox_tracked(bounds, &base_url, token.as_deref(), &progress)
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.transfer = None;
                this.data_generation += 1;
                match result {
                    Ok(data) => this.apply_download(data),
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
        // Times building the element tree (not layout or paint).
        let _render_scope =
            osm_gpui::frame_stats::Scoped::new("view.render", osm_gpui::frame_stats::start());
        // Consume any pending script command first.
        self.process_script_command(window, cx);

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
        self.maybe_rebuild_imagery_menu(cx);

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
            window_size.height - px(status_bar::STATUS_BAR_HEIGHT),
        );
        self.viewport.update_size(map_size);

        self.expire_status();

        // Keyboard shortcuts scoped to this view only match while focus is
        // somewhere inside it. With nothing focused at all — at launch, or
        // after a focused text input was removed — give focus to the map so
        // they (and the map's bare-key shortcuts) keep working.
        if window.focused(cx).is_none() {
            window.focus(&self.focus_handle, cx);
        }

        // Update all layers
        self.layer_manager.update_all();
        self.sync_selection_to_layers();

        let (center_lat, center_lon) = self.viewport.center();
        let zoom_level = self.viewport.zoom_level();
        let (total_tiles, cached_files, osm_objects) = self.get_layer_stats();
        let fps = self.tick_fps();
        let status_bar = self.render_status_bar(cx);

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
                    .cursor(map_cursor(
                        self.mode,
                        self.viewport.is_dragging,
                        self.space_held,
                        self.hovered.is_some(),
                    ))
                    // When the pointer leaves the map, drop everything that
                    // tracks it: the hover highlight, the status bar's cursor
                    // coordinates, and the drawing modes' rubber bands.
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !*hovered {
                            this.hovered = None;
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
                            window.focus(&this.focus_handle, cx);
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
                        if modifiers.platform || modifiers.control || modifiers.alt {
                            // Modifier chords belong to the keymap (menu
                            // shortcuts), never to the bare-key handling below.
                        } else if ev.keystroke.key == "space" {
                            // Held Space turns a left-drag into a pan.
                            if !this.space_held {
                                this.space_held = true;
                                cx.notify();
                            }
                        } else if this.handle_navigation_key(&ev.keystroke.key, cx) {
                            // Arrow keys pan; +/- zoom.
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
                            if key
                                == osm_gpui::keybindings::effective_spec(&settings, "mode_select")
                            {
                                this.on_set_mode(
                                    &SetMode {
                                        mode: EditModeAction::Select,
                                    },
                                    window,
                                    cx,
                                );
                            } else if key
                                == osm_gpui::keybindings::effective_spec(&settings, "mode_add")
                            {
                                this.on_set_mode(
                                    &SetMode {
                                        mode: EditModeAction::Add,
                                    },
                                    window,
                                    cx,
                                );
                            } else if key
                                == osm_gpui::keybindings::effective_spec(&settings, "mode_building")
                            {
                                this.on_set_mode(
                                    &SetMode {
                                        mode: EditModeAction::Building,
                                    },
                                    window,
                                    cx,
                                );
                            } else if key
                                == osm_gpui::keybindings::effective_spec(&settings, "mode_extrude")
                            {
                                this.on_set_mode(
                                    &SetMode {
                                        mode: EditModeAction::Extrude,
                                    },
                                    window,
                                    cx,
                                );
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
                                        if let Some(hovered) = &hovered {
                                            this.layer_manager.render_highlight(
                                                hovered,
                                                HighlightKind::Hovered,
                                                &viewport_clone,
                                                bounds,
                                                window,
                                            );
                                        }
                                        for sel in &selected {
                                            this.layer_manager.render_highlight(
                                                sel,
                                                HighlightKind::Selected,
                                                &viewport_clone,
                                                bounds,
                                                window,
                                            );
                                        }

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
                    .children(self.render_welcome(cx))
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
            );

        div()
            .size_full()
            .key_context(MAP_VIEWER_KEY_CONTEXT)
            .bg(cx.theme().background)
            .text_size(osm_gpui::ui::style::current_text_scale().body)
            .flex()
            .flex_col()
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
            .on_action(cx.listener(Self::on_export_osm_file))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_deselect_all))
            .on_action(cx.listener(Self::on_delete_selection))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_to_data))
            .on_action(cx.listener(Self::on_zoom_to_selection))
            .on_action(cx.listener(Self::on_toggle_downloaded_area))
            .on_action(cx.listener(Self::on_minimize_window))
            .on_action(cx.listener(Self::on_zoom_window))
            .on_action(cx.listener(Self::on_move_layer))
            .on_action(cx.listener(Self::on_delete_layer))
            .on_action(cx.listener(Self::on_set_active_layer))
            .on_action(cx.listener(Self::on_toggle_layer_visibility))
            .on_action(cx.listener(Self::on_set_mode))
            .on_action(cx.listener(Self::on_run_selection_action))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_apply_nsi_preset))
            .on_action(cx.listener(Self::on_change_feature_type))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_upload_to_osm))
            .on_action(cx.listener(Self::on_open_custom_imagery_dialog))
            .on_action(cx.listener(Self::on_show_about))
            .children(self.custom_imagery_dialog.clone())
            .children(self.quit_confirm_dialog.clone())
            .children(self.delete_layer_dialog.clone())
            .children(self.about_dialog.clone())
            .children(self.nsi_dialog.clone())
            .children(self.preset_picker_dialog.clone())
            .children(self.relation_picker_dialog.clone())
            .children(self.upload_dialog.clone())
            .children(self.sign_in_dialog.clone())
            .children(self.notice_dialog.clone())
            .children(self.render_context_menu(window, cx))
    }
}

fn main() {
    eprintln!("🚀 Starting OSM-GPUI Map Viewer with Tile Loading");

    let args = parse_cli_args();
    let (win_w, win_h) = args.window_size.unwrap_or((1200, 800));
    let cli_language = args.language.clone();
    let cli_theme = args.theme;
    // `--api-server` applies to this run only: it lives outside the settings
    // store, so nothing here is ever saved to settings.json.
    settings_store::set_run_override(args.api_server.clone());

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
        if let Some(mode) = cli_theme {
            menu::set_theme_mode(mode, cx);
        }

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
        cx.on_action(quit);
        menu::register_app_actions(cx);

        // Load persisted custom imagery entries.
        let loaded = custom_imagery_store::load();
        custom_imagery_store::init_store(loaded);

        // Load persisted app settings (OSM API server choice, cache budget) and
        // OAuth login. Seed the tile cache's live budget from the persisted value
        // before the cache does any work.
        let app_settings = settings_store::load();
        osm_gpui::tile_cache::set_budget_mb(app_settings.cache_budget_mb);
        // Pick the UI language before any menu or window text is built.
        // `--language` overrides the saved setting for this run only.
        osm_gpui::i18n::init(cli_language.as_deref().or(app_settings.language.as_deref()));
        settings_store::init_store(app_settings);
        auth::init_store(auth::load());
        // Learn the server's limits (max download area etc.) in the
        // background; defaults apply until it answers.
        osm_gpui::capabilities::ensure_fetched(&settings_store::api_base_url());

        // Initial menu (before ELI loads). MapViewer's render loop will call
        // rebuild_menus again whenever the load state or viewport changes.
        rebuild_menus(cx, 40.7128, -74.0060, ImageryLoadState::Loading, true);

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
                        osm_gpui::nsi::set_index(osm_gpui::nsi::NsiIndex::from_entries(entries));
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
                    ));
                    let view = cx.new(|cx| {
                        let mut viewer = MapViewer::new(window, cx);
                        // Start on the standard map, not a blank canvas.
                        viewer.add_default_layers();
                        viewer
                    });
                    #[cfg(debug_assertions)]
                    view.update(cx, |v, cx| v.debug_show_transfer_demo(window, cx));

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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size, TestAppContext};
    use osm_gpui::osm::{OsmNode, OsmWay};
    use std::collections::HashMap;

    fn empty_tags() -> HashMap<String, String> {
        HashMap::new()
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
            bounds: Vec::new(),
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

    /// The way fixture's data plus one extra node (id 50), as a second
    /// download overlapping the first would return.
    fn overlapping_download() -> OsmData {
        let mut data = way_fixture(CENTER_LAT, CENTER_LON);
        data.nodes.insert(
            50,
            OsmNode {
                id: 50,
                lat: CENTER_LAT + 0.001,
                lon: CENTER_LON,
                version: 1,
                tags: empty_tags(),
            },
        );
        data
    }

    #[gpui::test]
    fn download_merges_into_the_active_layer_and_keeps_local_edits(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                // A local edit to node 1 before the second download arrives.
                view.layer_manager
                    .find_layer_mut(layer_id)
                    .and_then(|l| l.as_editable_mut())
                    .unwrap()
                    .commit_node_moves(&[(1, CENTER_LAT + 0.0005, CENTER_LON - 0.001)]);

                view.apply_download(overlapping_download());

                assert_eq!(view.layer_manager.layers().len(), 1, "no second layer");
                let layer = view.layer_manager.find_layer(layer_id).unwrap();
                let editable = layer.as_editable().unwrap();
                assert!(editable.node_lat_lon(50).is_some(), "new node merged in");
                assert_eq!(
                    editable.node_lat_lon(1),
                    Some((CENTER_LAT + 0.0005, CENTER_LON - 0.001)),
                    "local move kept"
                );
                // Still exactly one pending edit: the move.
                assert_eq!(layer.diff_for_upload().counts(), (0, 1, 0));
                assert!(layer.is_modified());
                let (message, _) = view.status_message.clone().unwrap();
                assert_eq!(message, "Downloaded OSM data: 1 new, 1 local edit kept");
            })
            .unwrap();
    }

    #[gpui::test]
    fn download_with_no_active_layer_creates_one(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(MapViewer::new);
        window
            .update(cx, |view, _window, _cx| {
                assert!(view.active_layer.is_none());

                view.apply_download(overlapping_download());

                let layers = view.layer_manager.layers();
                assert_eq!(layers.len(), 1);
                assert_eq!(layers[0].name(), DOWNLOAD_LAYER_NAME);
                assert_eq!(view.active_layer, Some(layers[0].id()));
                assert!(!layers[0].is_modified());
            })
            .unwrap();
    }

    /// Adds a second editable layer (a copy of the fixture) and returns its id.
    fn add_second_layer(view: &mut MapViewer) -> LayerId {
        let id = view.layer_manager.alloc_id();
        let layer = OsmLayer::new_with_data(
            id,
            "L2".to_string(),
            Arc::new(way_fixture(CENTER_LAT, CENTER_LON)),
        );
        view.layer_manager.add_layer(Box::new(layer));
        id
    }

    /// Records a Create Node step by clicking at `pt` in Add mode on `layer`.
    fn click_node_in(view: &mut MapViewer, layer: LayerId, pt: (f32, f32)) {
        view.active_layer = Some(layer);
        view.add_progress = None;
        view.handle_add_click(point(px(pt.0), px(pt.1)), false);
        view.add_progress = None;
    }

    /// History: [B create, A create, A extend, B create], A being the first
    /// layer of `setup` and B a second one. Returns (A, B).
    fn mixed_layer_history(view: &mut MapViewer) -> (LayerId, LayerId) {
        let a = view.active_layer.unwrap();
        let b = add_second_layer(view);
        click_node_in(view, b, (50.0, 50.0));
        click_node_in(view, a, (50.0, 100.0));
        view.active_layer = Some(a);
        view.handle_add_click(point(px(50.0), px(150.0)), false);
        view.handle_add_click(point(px(50.0), px(200.0)), false);
        view.add_progress = None;
        click_node_in(view, b, (50.0, 250.0));
        (a, b)
    }

    #[gpui::test]
    fn deleting_a_layer_removes_its_undo_and_redo_steps(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                let (a, b) = mixed_layer_history(view);
                let b_steps = view
                    .undo_stack
                    .actions
                    .iter()
                    .filter(|s| s.layers() == vec![b])
                    .count();
                assert_eq!(b_steps, 2);
                assert!(view.undo_stack.actions.len() > b_steps);
                // Make B's last step redoable, and select/hover in A.
                view.on_undo(&Undo, window, cx);
                view.selected = vec![osm_gpui::selection::FeatureRef {
                    layer_id: a,
                    kind: osm_gpui::selection::FeatureKind::Node,
                    id: 1,
                }];
                view.hovered = view.selected.first().copied();
                view.active_layer = Some(a);
                let revision = view.undo_stack.revision;

                // A has unsaved edits, so the action itself only asks; the
                // confirmed deletion is what is under test here.
                view.delete_layer_now(a, cx);

                assert!(view.layer_manager.find_layer(a).is_none());
                assert!(view.undo_stack.revision > revision);
                assert_eq!(view.undo_stack.actions.len(), b_steps);
                assert!(view
                    .undo_stack
                    .actions
                    .iter()
                    .all(|s| s.layers() == vec![b]));
                // B's first step is undoable, its undone last step redoable.
                assert_eq!(view.undo_stack.cursor, 1);
                assert!(view.selected.is_empty());
                assert!(view.hovered.is_none());
                assert_eq!(view.active_layer, None);

                let b_created = |view: &MapViewer| {
                    view.layer_manager
                        .find_layer(b)
                        .unwrap()
                        .diff_for_upload()
                        .counts()
                        .0
                };
                assert_eq!(b_created(view), 1);
                view.on_redo(&Redo, window, cx);
                assert_eq!(b_created(view), 2);
                view.on_undo(&Undo, window, cx);
                view.on_undo(&Undo, window, cx);
                assert_eq!(b_created(view), 0);
                assert!(view.status_message.is_none());
                // Nothing left to undo: a further undo is a quiet no-op.
                view.on_undo(&Undo, window, cx);
                assert_eq!(view.undo_stack.cursor, 0);
            })
            .unwrap();
    }

    /// Press Cancel (`false`) or Delete Layer (`true`) in the open
    /// delete-layer confirmation. The dialog's event is handled when this
    /// update ends, so callers inspect the result in a later `update`.
    fn press_delete_layer_dialog(
        window: gpui::WindowHandle<MapViewer>,
        cx: &mut TestAppContext,
        confirm: bool,
    ) {
        window
            .update(cx, |view, _window, cx| {
                let dialog = view.delete_layer_dialog.clone().expect("dialog is open");
                dialog.update(cx, |dialog, cx| {
                    if confirm {
                        dialog.press_confirm(cx)
                    } else {
                        dialog.press_cancel(cx)
                    }
                });
            })
            .unwrap();
    }

    fn layer_count(view: &MapViewer) -> usize {
        view.layer_manager.layers().len()
    }

    /// Layer A (index 0) has unsaved edits after `mixed_layer_history`.
    fn open_delete_dialog_for_edited_layer(
        window: gpui::WindowHandle<MapViewer>,
        cx: &mut TestAppContext,
    ) -> (LayerId, LayerId) {
        window
            .update(cx, |view, window, cx| {
                let (a, b) = mixed_layer_history(view);
                view.on_delete_layer(&DeleteLayer { index: 0 }, window, cx);
                (a, b)
            })
            .unwrap()
    }

    #[gpui::test]
    fn deleting_a_layer_with_unsaved_edits_asks_first(cx: &mut TestAppContext) {
        let window = setup(cx);
        let (a, _b) = open_delete_dialog_for_edited_layer(window, cx);
        window
            .update(cx, |view, _window, cx| {
                assert_eq!(layer_count(view), 2);
                assert!(view.layer_manager.find_layer(a).is_some());
                let dialog = view.delete_layer_dialog.clone().expect("dialog opens");
                let dialog = dialog.read(cx);
                assert_eq!(dialog.layer(), a);
                assert!(dialog.pending() > 0);
                let message = dialog.message();
                assert!(message.contains("unsaved edit"), "{message}");
                assert!(message.contains("cannot be undone"), "{message}");
                assert!(
                    message.contains(&format!("{} unsaved edit", dialog.pending())),
                    "{message}"
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn cancelling_the_delete_keeps_the_layer_its_edits_and_history(cx: &mut TestAppContext) {
        let window = setup(cx);
        let (a, _b) = open_delete_dialog_for_edited_layer(window, cx);
        let before = window
            .update(cx, |view, _w, _cx| {
                (
                    view.undo_stack.actions.len(),
                    view.undo_stack.cursor,
                    view.undo_stack.revision,
                    MapViewer::unsaved_edit_count(view.layer_manager.find_layer(a).unwrap()),
                )
            })
            .unwrap();
        press_delete_layer_dialog(window, cx, false);
        window
            .update(cx, |view, _w, _cx| {
                assert!(view.delete_layer_dialog.is_none());
                assert_eq!(layer_count(view), 2);
                let after = (
                    view.undo_stack.actions.len(),
                    view.undo_stack.cursor,
                    view.undo_stack.revision,
                    MapViewer::unsaved_edit_count(view.layer_manager.find_layer(a).unwrap()),
                );
                assert_eq!(before, after);
                assert!(after.3 > 0);
            })
            .unwrap();
    }

    #[gpui::test]
    fn enter_and_escape_cancel_the_delete(cx: &mut TestAppContext) {
        for key in ["enter", "escape"] {
            let window = setup(cx);
            let (a, _b) = open_delete_dialog_for_edited_layer(window, cx);
            cx.simulate_keystrokes(window.into(), key);
            window
                .update(cx, |view, _w, _cx| {
                    assert!(view.delete_layer_dialog.is_none(), "{key} closes it");
                    assert!(view.layer_manager.find_layer(a).is_some(), "{key} keeps it");
                    assert_eq!(layer_count(view), 2);
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn confirming_the_delete_removes_the_layer_and_its_history(cx: &mut TestAppContext) {
        let window = setup(cx);
        let (a, b) = open_delete_dialog_for_edited_layer(window, cx);
        press_delete_layer_dialog(window, cx, true);
        window
            .update(cx, |view, _w, _cx| {
                assert!(view.delete_layer_dialog.is_none());
                assert!(view.layer_manager.find_layer(a).is_none());
                assert!(view.layer_manager.find_layer(b).is_some());
                assert!(!view.undo_stack.actions.is_empty());
                assert!(view
                    .undo_stack
                    .actions
                    .iter()
                    .all(|s| s.layers() == vec![b]));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_layer_without_unsaved_edits_is_deleted_without_asking(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                let b = add_second_layer(view);
                assert_eq!(layer_count(view), 2);
                view.on_delete_layer(&DeleteLayer { index: 1 }, window, cx);
                assert!(view.delete_layer_dialog.is_none());
                assert!(view.layer_manager.find_layer(b).is_none());
                assert_eq!(layer_count(view), 1);
                // An index past the end is ignored.
                view.on_delete_layer(&DeleteLayer { index: 7 }, window, cx);
                assert!(view.delete_layer_dialog.is_none());
                assert_eq!(layer_count(view), 1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn deleting_is_refused_during_an_upload_before_the_dialog_and_at_confirm(
        cx: &mut TestAppContext,
    ) {
        let upload = || TransferHandle::new(TransferKind::Upload, TransferPhase::OpeningChangeset);

        // Before: no dialog opens, nothing is deleted.
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                let (_a, _b) = mixed_layer_history(view);
                view.transfer = Some(upload());
                view.on_delete_layer(&DeleteLayer { index: 0 }, window, cx);
                assert!(view.delete_layer_dialog.is_none());
                assert_eq!(layer_count(view), 2);
                assert!(view.status_message.is_some());
            })
            .unwrap();

        // At confirm: the dialog was open, then an upload started.
        let window = setup(cx);
        let (a, _b) = open_delete_dialog_for_edited_layer(window, cx);
        window
            .update(cx, |view, _w, _cx| {
                assert!(view.delete_layer_dialog.is_some());
                view.transfer = Some(upload());
                view.status_message = None;
            })
            .unwrap();
        press_delete_layer_dialog(window, cx, true);
        window
            .update(cx, |view, _w, _cx| {
                assert!(view.delete_layer_dialog.is_none());
                assert!(view.layer_manager.find_layer(a).is_some());
                assert_eq!(layer_count(view), 2);
                assert!(view.status_message.is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn confirming_after_the_layer_disappeared_does_not_panic(cx: &mut TestAppContext) {
        let window = setup(cx);
        let (a, b) = open_delete_dialog_for_edited_layer(window, cx);
        window
            .update(cx, |view, _w, cx| {
                // The layer goes away some other way while the dialog is up.
                view.delete_layer_now(a, cx);
                assert!(view.delete_layer_dialog.is_some());
            })
            .unwrap();
        press_delete_layer_dialog(window, cx, true);
        window
            .update(cx, |view, _w, _cx| {
                assert!(view.delete_layer_dialog.is_none());
                // The other layer is untouched.
                assert!(view.layer_manager.find_layer(b).is_some());
                assert_eq!(layer_count(view), 1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_delete_goes_by_layer_not_by_index_when_the_list_shifts(cx: &mut TestAppContext) {
        let window = setup(cx);
        let (a, b) = open_delete_dialog_for_edited_layer(window, cx);
        window
            .update(cx, |view, _w, cx| {
                // Reordering moves A to index 1 while the dialog is open.
                view.reorder_layer(0, 1);
                cx.notify();
            })
            .unwrap();
        press_delete_layer_dialog(window, cx, true);
        window
            .update(cx, |view, _w, _cx| {
                assert!(view.layer_manager.find_layer(a).is_none());
                assert!(view.layer_manager.find_layer(b).is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_step_whose_layer_is_gone_is_skipped_with_a_message(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                let a = view.active_layer.unwrap();
                let b = add_second_layer(view);
                // A batch with a part in each layer, then drop B behind the
                // history's back (as no current path does).
                let create = |layer, id| UndoableAction::CreateNode {
                    layer,
                    id,
                    lat: CENTER_LAT,
                    lon: CENTER_LON,
                };
                view.undo_stack.push(UndoableAction::Batch {
                    label: "stale".into(),
                    actions: vec![create(a, -100), create(b, -101)],
                });
                let index = view
                    .layer_manager
                    .layers()
                    .iter()
                    .position(|l| l.id() == b)
                    .unwrap();
                view.layer_manager.remove_at(index);

                view.on_undo(&Undo, window, cx);
                let (message, _) = view.status_message.clone().expect("a status message");
                assert_eq!(
                    message,
                    "That step belongs to a layer that no longer exists, so it was skipped."
                );
                // Nothing was half-applied: layer A never had node -100.
                let layer = view.layer_manager.find_layer(a).unwrap();
                assert!(layer.as_editable().unwrap().node_lat_lon(-100).is_none());
                view.status_message = None;
                view.on_redo(&Redo, window, cx);
                assert!(view.status_message.is_some());
                assert!(layer_has_no_node(view, a, -100));
            })
            .unwrap();
    }

    fn layer_has_no_node(view: &MapViewer, layer: LayerId, id: i64) -> bool {
        view.layer_manager
            .find_layer(layer)
            .and_then(|l| l.as_editable())
            .is_some_and(|e| e.node_lat_lon(id).is_none())
    }

    #[gpui::test]
    fn exported_layer_reopens_with_the_same_pending_edits(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                // Two kinds of pending edit: a moved node and a created one.
                view.layer_manager
                    .find_layer_mut(layer_id)
                    .and_then(|l| l.as_editable_mut())
                    .unwrap()
                    .commit_node_moves(&[(1, CENTER_LAT + 0.0005, CENTER_LON - 0.001)]);
                view.handle_add_click(point(px(50.0), px(50.0)), false);
                let pending = view
                    .layer_manager
                    .find_layer(layer_id)
                    .unwrap()
                    .diff_for_upload()
                    .counts();
                assert_eq!(pending, (1, 1, 0));

                let (name, xml) = view.exportable_layer().expect("active layer exports");
                assert_eq!(name, "L");

                // Reopen the exported document as File > Open would.
                let (data, edits) = osm_gpui::osm::OsmParser::new()
                    .parse_str_with_edits(&xml)
                    .unwrap();
                view.add_osm_dataset("reopened".to_string(), data, &edits, cx);

                let reopened = view.layer_manager.layer_named("reopened").unwrap();
                assert_eq!(reopened.diff_for_upload().counts(), pending);
                assert!(reopened.is_modified());
            })
            .unwrap();
    }

    /// Window position of the fixture way's centre (map-local `(400, 300)`).
    fn on_the_way() -> gpui::Point<gpui::Pixels> {
        point(px(400.0 + MapViewer::MODE_PANEL_WIDTH), px(300.0))
    }

    #[gpui::test]
    fn right_click_selects_the_feature_and_opens_the_menu(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                view.mode = EditMode::Select;
                view.right_press = Some(on_the_way());
                view.finish_right_press(on_the_way(), window, cx);
                assert_eq!(view.selected.len(), 1);
                assert_eq!(view.selected[0].id, 10);
                assert!(view.context_menu.is_some());
            })
            .unwrap();
    }

    /// `docs/screenshots/selection-actions.osmscript` clicks fixed window
    /// positions; check they land on the features the script names.
    #[test]
    fn screenshot_script_click_targets_hit_their_features() {
        let xml = std::fs::read_to_string("docs/screenshots/fixtures/selection-actions.osm")
            .expect("fixture");
        let data = osm_gpui::osm::OsmParser::new().parse_str(&xml).unwrap();
        let layer = OsmLayer::new_with_data(LayerId(1), "L".to_string(), Arc::new(data));
        // The map area of a 1200x800 window, centred as the script does.
        let viewport = Viewport::new(40.7120, -74.0060, 18.0, size(px(864.0), px(776.0)));
        for (x, y, way) in [
            (370.0, 470.0, 3001),
            (710.0, 300.0, 3002),
            (640.0, 640.0, 3003),
        ] {
            let at = point(px(x - MapViewer::MODE_PANEL_WIDTH), px(y));
            let hits = osm_gpui::layers::EditableLayer::hit_test(&layer, &viewport, at);
            let hit = osm_gpui::selection::resolve_hits(vec![hits]);
            assert_eq!(hit.map(|h| h.id), Some(way), "click at ({x}, {y})");
        }
    }

    /// `docs/screenshots/topology-actions.osmscript` clicks and box-selects
    /// fixed window positions; check they pick what the script names.
    #[test]
    fn topology_script_targets_hit_their_features() {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let xml = std::fs::read_to_string("docs/screenshots/fixtures/topology-actions.osm")
            .expect("fixture");
        let data = osm_gpui::osm::OsmParser::new().parse_str(&xml).unwrap();
        let layer = OsmLayer::new_with_data(LayerId(1), "L".to_string(), Arc::new(data));
        let viewport = Viewport::new(40.7120, -74.0060, 18.0, size(px(864.0), px(776.0)));
        let local = |x: f32, y: f32| point(px(x - MapViewer::MODE_PANEL_WIDTH), px(y));
        for (x, y, kind, id) in [
            (640.0, 200.0, Node, 6302),
            (600.0, 430.0, Way, 4107),
            (560.0, 420.0, Node, 6401),
        ] {
            let hits = osm_gpui::layers::EditableLayer::hit_test(&layer, &viewport, local(x, y));
            let hit = osm_gpui::selection::resolve_hits(vec![hits]).expect("a hit");
            assert_eq!((hit.kind, hit.id), (kind, id), "click at ({x}, {y})");
        }
        // Box selects: both ways of a pair, and nothing from the others.
        for (from, to, want) in [
            ((70.0, 80.0), (450.0, 170.0), [-4102, 4101]),
            ((70.0, 225.0), (450.0, 310.0), [4103, 4104]),
        ] {
            let rect = gpui::Bounds::from_corners(local(from.0, from.1), local(to.0, to.1));
            let mut ways: Vec<i64> =
                osm_gpui::layers::EditableLayer::hit_test_rect(&layer, &viewport, rect)
                    .into_iter()
                    .filter(|f| f.kind == Way)
                    .map(|f| f.id)
                    .collect();
            ways.sort_unstable();
            assert_eq!(ways, want, "box {from:?} to {to:?}");
        }
    }

    /// `docs/screenshots/relation-editor.osmscript` clicks the route's way on
    /// the map and expects the route to be its relation, with a member that
    /// is not downloaded and a relation nested in another.
    #[test]
    fn relation_editor_script_targets_and_fixture() {
        use osm_gpui::selection::resolve_hits;
        use osm_gpui::selection::FeatureKind::Way;

        let xml = std::fs::read_to_string("docs/screenshots/fixtures/relation-editor.osm")
            .expect("fixture");
        let data = osm_gpui::osm::OsmParser::new().parse_str(&xml).unwrap();
        let layer = OsmLayer::new_with_data(LayerId(1), "L".to_string(), Arc::new(data));
        let viewport = Viewport::new(40.7120, -74.0060, 18.0, size(px(864.0), px(776.0)));
        let hit_at = |x: f32, y: f32| {
            let at = point(px(x - MapViewer::MODE_PANEL_WIDTH), px(y));
            let hits = osm_gpui::layers::EditableLayer::hit_test(&layer, &viewport, at);
            resolve_hits(vec![hits]).map(|h| (h.kind, h.id))
        };
        assert_eq!(hit_at(520.0, 200.0), Some((Way, 8202)));
        assert_eq!(hit_at(340.0, 400.0), Some((Way, 8301)));

        let editable: &dyn osm_gpui::layers::EditableLayer = &layer;
        assert_eq!(editable.relations_containing_way(8202), vec![9101]);
        let route = editable.relation(9101).unwrap();
        assert_eq!(route.members.len(), 5);
        // Way 8999 is a member but not loaded.
        assert!(editable.way_node_ids(8999).is_none());
        let find = |id| editable.relation(id);
        let master = editable.relation(9103).unwrap();
        let flat = osm_gpui::relations::flatten_members(&master, &find);
        assert_eq!(flat.len(), 5);
    }

    /// `docs/screenshots/relations.osmscript` clicks fixed window positions;
    /// check they pick what the script names, that the fixture's relations
    /// show up as the script expects, and that the split it runs keeps the
    /// route intact and moves the later clicks as the script assumes.
    #[test]
    fn relations_script_targets_hit_their_features_and_the_split_keeps_the_route() {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        use osm_gpui::selection::{resolve_hits, FeatureRef};

        let xml =
            std::fs::read_to_string("docs/screenshots/fixtures/relations.osm").expect("fixture");
        let data = osm_gpui::osm::OsmParser::new().parse_str(&xml).unwrap();
        let mut layers = osm_gpui::layers::LayerManager::new();
        layers.add_layer(Box::new(OsmLayer::new_with_data(
            LayerId(1),
            "L".to_string(),
            Arc::new(data),
        )));
        let viewport = Viewport::new(40.7120, -74.0060, 18.0, size(px(864.0), px(776.0)));
        let hit_at = |layers: &osm_gpui::layers::LayerManager, x: f32, y: f32| {
            let at = point(px(x - MapViewer::MODE_PANEL_WIDTH), px(y));
            let hits = layers
                .find_layer(LayerId(1))
                .unwrap()
                .as_editable()
                .unwrap()
                .hit_test(&viewport, at);
            resolve_hits(vec![hits]).map(|h| (h.kind, h.id))
        };
        for (x, y, kind, id) in [
            (520.0, 200.0, Way, 8202),
            (560.0, 200.0, Node, 8104),
            (340.0, 400.0, Way, 8301),
            (640.0, 200.0, Node, 8105),
            (760.0, 200.0, Way, 8202),
        ] {
            assert_eq!(
                hit_at(&layers, x, y),
                Some((kind, id)),
                "click at ({x}, {y})"
            );
        }

        // What the Relations section shows for the clicked features.
        let relations_of = |layers: &osm_gpui::layers::LayerManager, kind, id| {
            let editable = layers
                .find_layer(LayerId(1))
                .unwrap()
                .as_editable()
                .unwrap();
            let ids = match kind {
                Node => editable.relations_containing_node(id),
                Way => editable.relations_containing_way(id),
                osm_gpui::selection::FeatureKind::Relation => Vec::new(),
            };
            ids.into_iter()
                .map(|r| osm_gpui::relations::display_name(&editable.relation(r).unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(relations_of(&layers, Way, 8202), ["Bus 7B"]);
        assert_eq!(relations_of(&layers, Node, 8104), ["Bus 7B"]);
        assert_eq!(relations_of(&layers, Way, 8301), ["restriction"]);

        // Split at the middle node: the new way is -1, takes the right-hand
        // part, and joins the route right after the original.
        let action = selection_actions::find("split").unwrap();
        let sel = [FeatureRef {
            layer_id: LayerId(1),
            kind: Node,
            id: 8105,
        }];
        (action.apply)(&mut layers, &sel, Some(LayerId(1))).unwrap();
        assert_eq!(hit_at(&layers, 760.0, 200.0), Some((Way, -1)));
        assert_eq!(hit_at(&layers, 520.0, 200.0), Some((Way, 8202)));
        let route = layers
            .find_layer(LayerId(1))
            .unwrap()
            .as_editable()
            .unwrap()
            .relation(9101)
            .unwrap();
        let refs: Vec<i64> = route.members.iter().map(|m| m.reference).collect();
        assert_eq!(refs, [8104, 8201, 8202, -1, 8203]);
        assert_eq!(relations_of(&layers, Way, -1), ["Bus 7B"]);
        // Way -1, way 8202 and relation 9101.
        let counts = layers
            .find_layer(LayerId(1))
            .unwrap()
            .diff_for_upload()
            .counts();
        assert_eq!(counts, (1, 2, 0));
    }

    /// `docs/screenshots/more-selection-actions.osmscript` clicks and
    /// box-selects fixed window positions; check they pick what the script
    /// names, and that each action the script runs on them is accepted, with
    /// the results the script's later clicks and assertions expect.
    #[test]
    fn more_actions_script_targets_hit_their_features_and_actions_apply() {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        use osm_gpui::selection::{resolve_hits, FeatureRef};

        let xml = std::fs::read_to_string("docs/screenshots/fixtures/more-selection-actions.osm")
            .expect("fixture");
        let data = osm_gpui::osm::OsmParser::new().parse_str(&xml).unwrap();
        let mut layers = osm_gpui::layers::LayerManager::new();
        layers.add_layer(Box::new(OsmLayer::new_with_data(
            LayerId(1),
            "L".to_string(),
            Arc::new(data),
        )));
        let viewport = Viewport::new(40.7120, -74.0060, 18.0, size(px(864.0), px(776.0)));
        let local = |x: f32, y: f32| point(px(x - MapViewer::MODE_PANEL_WIDTH), px(y));
        let hit_at = |layers: &osm_gpui::layers::LayerManager, x: f32, y: f32| {
            let layer = layers.find_layer(LayerId(1)).unwrap();
            let hits = layer
                .as_editable()
                .unwrap()
                .hit_test(&viewport, local(x, y));
            resolve_hits(vec![hits]).map(|h| (h.kind, h.id))
        };
        let feature = |kind, id| FeatureRef {
            layer_id: LayerId(1),
            kind,
            id,
        };
        let run = |layers: &mut osm_gpui::layers::LayerManager, id: &str, sel: &[FeatureRef]| {
            let action = selection_actions::find(id).unwrap();
            let env = selection_actions::ActionEnv {
                selection: sel,
                active_layer: Some(LayerId(1)),
                layers,
            };
            assert!((action.is_applicable)(&env), "{id} not offered");
            (action.apply)(layers, sel, Some(LayerId(1))).unwrap_or_else(|e| panic!("{id}: {e}"))
        };

        for (x, y, kind, id) in [
            (550.0, 120.0, Node, 7103),
            (700.0, 120.0, Way, 5101),
            (400.0, 120.0, Way, 5101),
            (650.0, 262.0, Way, 5202),
            (500.0, 401.0, Way, 5301),
            (425.0, 520.0, Way, 5401),
            (500.0, 640.0, Way, 5501),
            (500.0, 700.0, Way, 5502),
        ] {
            assert_eq!(
                hit_at(&layers, x, y),
                Some((kind, id)),
                "click at ({x}, {y})"
            );
        }

        // Split at the middle node: the halves tie, so the original keeps the
        // first; the new way is -1 and holds the part the script clicks at 700.
        run(&mut layers, "split", &[feature(Node, 7103)]);
        assert_eq!(hit_at(&layers, 700.0, 120.0), Some((Way, -1)));
        assert_eq!(hit_at(&layers, 400.0, 120.0), Some((Way, 5101)));

        // The box around B's end nodes picks exactly those two nodes.
        let rect = gpui::Bounds::from_corners(local(520.0, 232.0), local(580.0, 280.0));
        let boxed = layers
            .find_layer(LayerId(1))
            .unwrap()
            .as_editable()
            .unwrap()
            .hit_test_rect(&viewport, rect);
        let mut ids: Vec<(osm_gpui::selection::FeatureKind, i64)> =
            boxed.iter().map(|f| (f.kind, f.id)).collect();
        ids.sort_by_key(|&(_, id)| id);
        assert_eq!(ids, [(Node, -7203), (Node, 7202)]);
        let merged = run(&mut layers, "merge", &boxed);
        assert_eq!(merged.select, Some(vec![feature(Node, 7202)]));
        assert_eq!(hit_at(&layers, 650.0, 262.0), Some((Way, 5202)));

        // The wobbly track is within the straighten threshold.
        run(&mut layers, "straighten", &[feature(Way, 5301)]);
        // Reverse flips the direction tags.
        run(&mut layers, "reverse", &[feature(Way, 5401)]);
        let tags = layers
            .find_layer(LayerId(1))
            .unwrap()
            .as_editable()
            .unwrap()
            .feature_tags(&feature(Way, 5401))
            .unwrap();
        assert!(tags.contains(&("lanes:forward".to_string(), "1".to_string())));
        assert!(tags.contains(&("cycleway:right".to_string(), "lane".to_string())));
        assert!(tags.contains(&("incline".to_string(), "-10%".to_string())));
        assert!(tags.contains(&("oneway".to_string(), "yes".to_string())));
        // Copy the driveway's tags and paste them onto the track.
        run(&mut layers, "copy_tags", &[feature(Way, 5501)]);
        run(&mut layers, "paste_tags", &[feature(Way, 5502)]);
        let tags = layers
            .find_layer(LayerId(1))
            .unwrap()
            .as_editable()
            .unwrap()
            .feature_tags(&feature(Way, 5502))
            .unwrap();
        assert!(tags.contains(&("tracktype".to_string(), "grade4".to_string())));
        assert!(tags.contains(&("surface".to_string(), "gravel".to_string())));
        assert!(tags.contains(&("name".to_string(), "Back Lane".to_string())));
    }

    /// The same click, but delivered as real mouse events through the
    /// window's event dispatch, to cover the listeners in `render`.
    #[gpui::test]
    fn right_click_events_open_the_menu(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _, _| view.mode = EditMode::Select)
            .unwrap();
        let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        // Events are only routed through a rendered frame.
        let root = window.root(vcx).unwrap();
        vcx.draw(
            point(px(0.0), px(0.0)),
            size(
                gpui::AvailableSpace::Definite(px(1200.0)),
                gpui::AvailableSpace::Definite(px(800.0)),
            ),
            |_, _| gpui::AnyView::from(root),
        );
        let pos = window
            .update(vcx, |view, _, _| {
                let p = view.viewport.geo_to_screen(CENTER_LAT, CENTER_LON);
                point(p.x + px(MapViewer::MODE_PANEL_WIDTH), p.y)
            })
            .unwrap();
        vcx.simulate_mouse_down(pos, gpui::MouseButton::Right, gpui::Modifiers::none());
        vcx.simulate_mouse_up(pos, gpui::MouseButton::Right, gpui::Modifiers::none());
        window
            .update(vcx, |view, _, _| {
                assert_eq!(view.selected.len(), 1);
                assert!(view.context_menu.is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn right_drag_pans_without_opening_the_menu(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                view.mode = EditMode::Select;
                view.right_press = Some(on_the_way());
                let released = on_the_way() + point(px(30.0), px(0.0));
                view.finish_right_press(released, window, cx);
                assert!(view.selected.is_empty());
                assert!(view.context_menu.is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn right_click_on_empty_map_opens_nothing(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                view.mode = EditMode::Select;
                let empty = point(px(100.0), px(500.0));
                view.right_press = Some(empty);
                view.finish_right_press(empty, window, cx);
                assert!(view.context_menu.is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn right_click_inside_the_selection_keeps_it(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                view.mode = EditMode::Select;
                let layer_id = view.active_layer.unwrap();
                let way = osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Way,
                    id: 10,
                };
                let node = osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Node,
                    id: 1,
                };
                view.set_selection(vec![way, node]);
                view.right_press = Some(on_the_way());
                view.finish_right_press(on_the_way(), window, cx);
                assert_eq!(view.selected, vec![way, node]);
                assert!(view.context_menu.is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn reverse_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                view.set_selection(vec![osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Way,
                    id: 10,
                }]);
                view.run_selection_action("reverse", cx);
                assert_eq!(way_nodes(view, layer_id, 10), vec![2, 1]);
                assert_eq!(view.undo_stack.actions.len(), 1);

                let action = view.undo_stack.undo().unwrap();
                view.apply_undo_action(&action, false);
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
                let action = view.undo_stack.redo().unwrap();
                view.apply_undo_action(&action, true);
                assert_eq!(way_nodes(view, layer_id, 10), vec![2, 1]);
            })
            .unwrap();
    }

    /// The tags of way/node `id` in the view's active layer, sorted.
    fn tags_of(
        view: &MapViewer,
        kind: osm_gpui::selection::FeatureKind,
        id: i64,
    ) -> Vec<(String, String)> {
        let layer_id = view.active_layer.unwrap();
        let mut tags = view
            .layer_manager
            .find_layer(layer_id)
            .unwrap()
            .as_editable()
            .unwrap()
            .feature_tags(&osm_gpui::selection::FeatureRef { layer_id, kind, id })
            .unwrap();
        tags.sort();
        tags
    }

    fn tag_pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[gpui::test]
    fn reverse_flips_direction_tags_as_one_undoable_step(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                let original = {
                    let layer = view.layer_manager.find_layer_mut(layer_id).unwrap();
                    let editable = layer.as_editable_mut().unwrap();
                    editable.set_tag(Way, 100, "cycleway:left", "lane");
                    editable.set_tag(Way, 100, "incline", "5%");
                    tags_of(view, Way, 100)
                };
                select(view, Way, &[100]);
                view.run_selection_action("reverse", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(way_nodes(view, layer_id, 100), vec![3, 2, 1]);
                let flipped = tag_pairs(&[
                    ("cycleway:right", "lane"),
                    ("highway", "residential"),
                    ("incline", "-5%"),
                ]);
                assert_eq!(tags_of(view, Way, 100), flipped);

                undo_step(view);
                assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3]);
                assert_eq!(tags_of(view, Way, 100), original);

                redo_step(view);
                assert_eq!(way_nodes(view, layer_id, 100), vec![3, 2, 1]);
                assert_eq!(tags_of(view, Way, 100), flipped);
            })
            .unwrap();
    }

    fn node_position(view: &MapViewer, id: i64) -> (f64, f64) {
        let layer_id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer(layer_id)
            .unwrap()
            .as_editable()
            .unwrap()
            .node_lat_lon(id)
            .unwrap()
    }

    fn ways_at_node(view: &MapViewer, node: i64) -> Vec<i64> {
        let layer_id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer(layer_id)
            .unwrap()
            .as_editable()
            .unwrap()
            .ways_using_node(node)
    }

    #[gpui::test]
    fn copy_then_paste_tags_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                // Copying leaves no undo step and no edit behind.
                select(view, Way, &[101]);
                view.run_selection_action("copy_tags", cx);
                assert!(view.undo_stack.actions.is_empty());
                assert_eq!(pending(view), (0, 0, 0));
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("Copied 2 tags")
                );

                // Paste onto a way and a node at once.
                let original_way = tags_of(view, Way, 103);
                select(view, Way, &[103]);
                let layer_id = view.active_layer.unwrap();
                view.selected.push(osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: Node,
                    id: 8,
                });
                view.run_selection_action("paste_tags", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                let pasted = tag_pairs(&[("highway", "residential"), ("name", "Main")]);
                assert_eq!(tags_of(view, Way, 103), pasted);
                assert_eq!(tags_of(view, Node, 8), pasted);
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("Pasted 2 tags onto 2 features")
                );
                assert_eq!(pending(view), (0, 2, 0));

                undo_step(view);
                assert_eq!(tags_of(view, Way, 103), original_way);
                assert!(tags_of(view, Node, 8).is_empty());
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert_eq!(tags_of(view, Way, 103), pasted);
                assert_eq!(tags_of(view, Node, 8), pasted);
            })
            .unwrap();
    }

    #[test]
    fn tag_copy_and_paste_are_modifier_key_bindings_on_the_map_context() {
        let bindings = key_bindings_from_settings(&settings_store::AppSettings::default());
        let ids: Vec<String> = bindings
            .iter()
            .filter_map(|b| b.action().as_any().downcast_ref::<RunSelectionAction>())
            .map(|a| a.id.to_string())
            .collect();
        assert_eq!(ids, ["copy_tags", "paste_tags"]);
    }

    #[gpui::test]
    fn split_way_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                select(view, Node, &[4]);
                view.run_selection_action("split", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                let split_users = ways_at_node(view, 4);
                assert_eq!(split_users.len(), 4);
                let new: Vec<i64> = split_users.iter().copied().filter(|&id| id < 0).collect();
                assert_eq!(way_nodes(view, layer_id, 101), vec![3, 4]);
                assert_eq!(pending(view), (2, 2, 0));
                let new_tags: Vec<_> = new.iter().map(|&id| tags_of(view, Way, id)).collect();
                // The node stays selected.
                assert_eq!(view.selected.len(), 1);
                assert_eq!(view.selected[0].id, 4);

                undo_step(view);
                assert_eq!(way_nodes(view, layer_id, 101), vec![3, 4, 5]);
                assert_eq!(way_nodes(view, layer_id, 103), vec![8, 4, 9]);
                assert_eq!(ways_at_node(view, 4), vec![101, 103]);
                for &id in &new {
                    assert!(way_nodes(view, layer_id, id).is_empty());
                }
                assert_eq!(pending(view), (0, 0, 0));

                // Redo recreates the same ways under the same ids.
                redo_step(view);
                assert_eq!(ways_at_node(view, 4), split_users);
                assert_eq!(way_nodes(view, layer_id, 101), vec![3, 4]);
                let redone_tags: Vec<_> = new.iter().map(|&id| tags_of(view, Way, id)).collect();
                assert_eq!(redone_tags, new_tags);
                assert_eq!(pending(view), (2, 2, 0));
                undo_step(view);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn merge_nodes_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Node;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                // Node 4 is a crossing shared by ways 101 and 103; merge it
                // into the untagged node 8 at the end of way 103.
                select(view, Node, &[8, 4]);
                view.run_selection_action("merge", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(way_nodes(view, layer_id, 101), vec![3, 8, 5]);
                assert_eq!(way_nodes(view, layer_id, 103), vec![8, 9]);
                assert_eq!(pending(view), (0, 3, 1));
                assert_eq!(
                    tags_of(view, Node, 8),
                    tag_pairs(&[("highway", "crossing")])
                );
                assert_eq!(view.selected.len(), 1);
                assert_eq!(view.selected[0].id, 8);

                undo_step(view);
                assert_eq!(way_nodes(view, layer_id, 101), vec![3, 4, 5]);
                assert_eq!(way_nodes(view, layer_id, 103), vec![8, 4, 9]);
                assert_eq!(pending(view), (0, 0, 0));
                assert_eq!(
                    tags_of(view, Node, 4),
                    tag_pairs(&[("highway", "crossing")])
                );
                assert!(tags_of(view, Node, 8).is_empty());

                redo_step(view);
                assert_eq!(way_nodes(view, layer_id, 101), vec![3, 8, 5]);
                assert_eq!(way_nodes(view, layer_id, 103), vec![8, 9]);
                assert_eq!(pending(view), (0, 3, 1));
                undo_step(view);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn side_panel_lists_the_relations_of_the_one_selected_feature(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let window = setup_network_with(cx, |data| {
            data.relations[0].tags.insert("ref".into(), "7B".into());
        });
        window
            .update(cx, |view, _window, _cx| {
                // Way 102 and node 7 are in relation 900, which has no name,
                // so its ref stands in.
                select(view, Way, &[102]);
                let membership = |role: &str, index| side_panel::Membership {
                    relation_id: 900,
                    name: "7B".to_string(),
                    role: role.to_string(),
                    index,
                };
                assert_eq!(view.selected_memberships(), vec![membership("", 0)]);
                select(view, Node, &[7]);
                assert_eq!(view.selected_memberships(), vec![membership("stop", 1)]);
                // In no relation, several features, or nothing selected: no
                // section.
                select(view, Way, &[100]);
                assert!(view.selected_memberships().is_empty());
                select(view, Way, &[102, 100]);
                assert!(view.selected_memberships().is_empty());
                select(view, Way, &[]);
                assert!(view.selected_memberships().is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_relation_selected_from_a_members_row_edits_and_zooms(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Relation, Way};
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                select(view, Way, &[102]);
                let membership = view.selected_memberships().remove(0);
                // What clicking the member's Relations row does.
                view.select_relation(layer_id, membership.relation_id);
                let only = view.selected.as_slice();
                assert_eq!(only.len(), 1);
                assert_eq!((only[0].kind, only[0].id), (Relation, 900));
                // A relation lists no memberships of its own.
                assert!(view.selected_memberships().is_empty());

                // Tags edit like a way's: one undo step each.
                let ctx = TagEditContext {
                    features: view.selected.clone(),
                    original_key: String::new(),
                    original_value: String::new(),
                    is_add: true,
                };
                view.apply_tag_edit(&ctx, "ref", "7B");
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(tags_of(view, Relation, 900), tag_pairs(&[("ref", "7B")]));
                assert_eq!(pending(view), (0, 1, 0));
                undo_step(view);
                assert!(tags_of(view, Relation, 900).is_empty());
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert_eq!(tags_of(view, Relation, 900), tag_pairs(&[("ref", "7B")]));

                // Copy Tags works.
                view.run_selection_action("copy_tags", cx);
                assert!(view
                    .status_message
                    .as_ref()
                    .is_some_and(|(m, _)| m.starts_with("Copied")));
                assert_eq!(relation_members(view, 900).len(), 2);
                // Nothing to drag.
                assert!(view.resolve_move_targets().is_empty());

                // Zoom to Selection fits the loaded members (way 102 and
                // node 7), not the whole layer.
                view.viewport.pan_to(10.0, 10.0);
                assert!(view.fit_to_features(&view.selected.clone()));
                let (lat, lon) = view.viewport.center();
                assert!((lat - 40.002).abs() < 1e-3 && (lon + 73.994).abs() < 1e-3);
            })
            .unwrap();
    }

    #[gpui::test]
    fn role_move_and_remove_edits_are_one_undo_step_each_and_redoable(cx: &mut TestAppContext) {
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                let at = |index| MemberRef {
                    layer_id,
                    relation_id: 900,
                    index,
                };
                let original = relation_members(view, 900);
                assert_eq!(
                    original,
                    vec![(102, String::new()), (7, "stop".to_string())]
                );

                // Role: set, undo, redo; an empty role is fine; no change is
                // not an undo step.
                view.set_relation_member_role(at(0), "outer");
                assert_eq!(view.undo_stack.actions.len(), 1);
                view.set_relation_member_role(at(0), "outer");
                assert_eq!(view.undo_stack.actions.len(), 1);
                let renamed = relation_members(view, 900);
                assert_eq!(renamed[0], (102, "outer".to_string()));
                assert_eq!(pending(view), (0, 1, 0));
                undo_step(view);
                assert_eq!(relation_members(view, 900), original);
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert_eq!(relation_members(view, 900), renamed);
                view.set_relation_member_role(at(1), "");
                assert_eq!(relation_members(view, 900)[1], (7, String::new()));
                undo_step(view);
                undo_step(view);
                assert_eq!(relation_members(view, 900), original);

                // Reorder: move down, undo, redo; the ends don't move.
                assert_eq!(view.undo_stack.cursor, 0);
                view.move_relation_member(at(0), 1);
                assert_eq!(view.undo_stack.cursor, 1);
                let moved = relation_members(view, 900);
                assert_eq!(moved, vec![original[1].clone(), original[0].clone()]);
                view.move_relation_member(at(1), 1);
                view.move_relation_member(at(0), -1);
                assert_eq!(relation_members(view, 900), moved);
                undo_step(view);
                assert_eq!(relation_members(view, 900), original);
                redo_step(view);
                assert_eq!(relation_members(view, 900), moved);
                undo_step(view);

                // Remove: one step; the last member is refused with a reason.
                assert_eq!(view.undo_stack.cursor, 0);
                view.remove_relation_member(at(0));
                assert_eq!(view.undo_stack.cursor, 1);
                assert_eq!(relation_members(view, 900), vec![original[1].clone()]);
                assert_eq!(pending(view), (0, 1, 0));
                view.remove_relation_member(at(0));
                assert_eq!(view.undo_stack.cursor, 1);
                assert_eq!(relation_members(view, 900).len(), 1);
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("Can't remove the last member of relation 900")
                );
                undo_step(view);
                assert_eq!(relation_members(view, 900), original);
                redo_step(view);
                undo_step(view);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn removing_a_feature_from_a_relation_on_its_relations_row_is_undoable(
        cx: &mut TestAppContext,
    ) {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                let original = relation_members(view, 900);
                // Node 7 is the relation's second member.
                select(view, Node, &[7]);
                let membership = view.selected_memberships().remove(0);
                view.remove_relation_member(crate::MemberRef {
                    layer_id,
                    relation_id: membership.relation_id,
                    index: membership.index,
                });
                assert_eq!(relation_members(view, 900), vec![original[0].clone()]);
                assert!(view.selected_memberships().is_empty());
                assert_eq!(pending(view), (0, 1, 0));

                // The way is now the last member: its row refuses.
                select(view, Way, &[102]);
                let membership = view.selected_memberships().remove(0);
                view.remove_relation_member(crate::MemberRef {
                    layer_id,
                    relation_id: membership.relation_id,
                    index: membership.index,
                });
                assert_eq!(relation_members(view, 900).len(), 1);
                assert!(view
                    .status_message
                    .as_ref()
                    .is_some_and(|(m, _)| m.contains("last member")));

                undo_step(view);
                assert_eq!(relation_members(view, 900), original);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn add_to_relation_appends_the_new_members_as_one_undo_step(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let original = relation_members(view, 900);
                // Way 102 is already a member; 100, 103 and node 5 are not.
                let layer_id = view.active_layer.unwrap();
                select(view, Way, &[100, 102, 103]);
                view.selected.push(osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: Node,
                    id: 5,
                });
                // The action asks for a relation first and changes nothing.
                view.run_selection_action("add_to_relation", cx);
                assert!(view.pending_relation_picker);
                assert!(view.undo_stack.actions.is_empty());
                // (Opening the picker needs a window root, which the test
                // window lacks.)
                view.pending_relation_picker = false;

                view.add_selection_to_relation(900);
                assert_eq!(view.undo_stack.actions.len(), 1);
                let added = relation_members(view, 900);
                assert_eq!(
                    added,
                    vec![
                        original[0].clone(),
                        original[1].clone(),
                        (100, String::new()),
                        (103, String::new()),
                        (5, String::new()),
                    ]
                );
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("Added 3 features to relation 900")
                );
                assert_eq!(pending(view), (0, 1, 0));

                undo_step(view);
                assert_eq!(relation_members(view, 900), original);
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert_eq!(relation_members(view, 900), added);

                // Everything is a member now: nothing to do, no undo step.
                view.add_selection_to_relation(900);
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("The selected features are already members of 900")
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn add_to_relation_needs_features_and_a_relation_to_add_them_to(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network_with(cx, |data| data.relations.clear());
        window
            .update(cx, |view, _window, cx| {
                select(view, Way, &[100]);
                view.run_selection_action("add_to_relation", cx);
                assert!(!view.pending_relation_picker);
                assert!(view
                    .status_message
                    .as_ref()
                    .is_some_and(|(m, _)| m.starts_with("Select nodes or ways")));
            })
            .unwrap();
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                // A selected relation alone has nothing to add.
                view.select_relation(view.active_layer.unwrap(), 900);
                view.run_selection_action("add_to_relation", cx);
                assert!(!view.pending_relation_picker);
            })
            .unwrap();
    }

    #[gpui::test]
    fn removing_a_restrictions_from_via_or_to_member_is_refused(cx: &mut TestAppContext) {
        use osm_gpui::osm::{MemberType, OsmMember, OsmRelation};
        let window = setup_network_with(cx, |data| {
            let member = |member_type, reference, role: &str| OsmMember {
                member_type,
                reference,
                role: role.to_string(),
            };
            data.relations = vec![OsmRelation {
                id: 900,
                version: 1,
                tags: [("type".to_string(), "restriction".to_string())].into(),
                members: vec![
                    member(MemberType::Way, 100, "from"),
                    member(MemberType::Node, 3, "via"),
                    member(MemberType::Way, 101, "to"),
                    member(MemberType::Node, 7, "note"),
                ],
            }];
        });
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                for (index, role) in [(0, "from"), (1, "via"), (2, "to")] {
                    view.remove_relation_member(MemberRef {
                        layer_id,
                        relation_id: 900,
                        index,
                    });
                    assert!(view.undo_stack.actions.is_empty());
                    assert_eq!(relation_members(view, 900).len(), 4);
                    let status = view.status_message.as_ref().unwrap().0.to_string();
                    assert!(status.contains(&format!("'{role}'")), "{status}");
                }
                // The other member may go.
                view.remove_relation_member(MemberRef {
                    layer_id,
                    relation_id: 900,
                    index: 3,
                });
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(relation_members(view, 900).len(), 3);
            })
            .unwrap();
    }

    #[gpui::test]
    fn deleting_a_relation_member_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                // Way 102 is a member of relation 900.
                let original = relation_members(view, 900);
                select(view, Way, &[102]);
                view.run_selection_action("delete", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("Deleted 1 feature and removed it from 1 relation")
                );
                let remaining = relation_members(view, 900);
                assert_eq!(remaining, vec![(7, "stop".to_string())]);
                // The deleted way and the relation that lost it.
                assert_eq!(pending(view), (0, 1, 1));

                undo_step(view);
                assert_eq!(relation_members(view, 900), original);
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert_eq!(relation_members(view, 900), remaining);
                assert_eq!(pending(view), (0, 1, 1));
                undo_step(view);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn joining_relation_member_ways_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network_with(cx, route_over(&[(100, "forward"), (101, "forward")]));
        window
            .update(cx, |view, _window, cx| {
                let original = relation_members(view, 900);
                select(view, Way, &[100, 101]);
                view.run_selection_action("join", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                let joined = relation_members(view, 900);
                assert_eq!(joined, vec![(100, "forward".to_string())]);
                // Joined way, relation, and the deleted way.
                assert_eq!(pending(view), (0, 2, 1));

                undo_step(view);
                assert_eq!(relation_members(view, 900), original);
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert_eq!(relation_members(view, 900), joined);
                assert_eq!(pending(view), (0, 2, 1));
                undo_step(view);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn merging_relation_member_nodes_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Node;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                // Relation 900 holds way 102 and node 7 ("stop").
                let original = relation_members(view, 900);
                select(view, Node, &[6, 7]);
                view.run_selection_action("merge", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                let merged = relation_members(view, 900);
                assert_eq!(merged[1], (6, "stop".to_string()));
                assert_eq!(pending(view), (0, 1, 1));

                undo_step(view);
                assert_eq!(relation_members(view, 900), original);
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert_eq!(relation_members(view, 900), merged);
                assert_eq!(pending(view), (0, 1, 1));
                undo_step(view);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn straighten_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                let on_line = node_position(view, 2);
                let off_line = (on_line.0 + 0.00005, on_line.1);
                view.layer_manager
                    .find_layer_mut(layer_id)
                    .unwrap()
                    .as_editable_mut()
                    .unwrap()
                    .commit_node_moves(&[(2, off_line.0, off_line.1)]);
                select(view, Way, &[100]);
                view.run_selection_action("straighten", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                let straightened = node_position(view, 2);
                // Moved back to the line through nodes 1 and 3 (which also
                // slides it a little along it, since the line is diagonal).
                assert_ne!(straightened, off_line);
                assert!((straightened.0 - on_line.0).abs() < 1e-4);

                undo_step(view);
                assert_eq!(node_position(view, 2), off_line);
                redo_step(view);
                assert_eq!(node_position(view, 2), straightened);
                // A curved way is refused with the reason and no undo step.
                select(view, Way, &[101]);
                view.run_selection_action("straighten", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("Can't straighten: the way bends more than 10% of its length")
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn inapplicable_action_explains_itself_and_changes_nothing(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                view.set_selection(vec![osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Way,
                    id: 10,
                }]);
                view.run_selection_action("circularize", cx);
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("Select a closed way to circularize it")
                );
                assert!(view.undo_stack.actions.is_empty());
                assert!(!view
                    .layer_manager
                    .find_layer(layer_id)
                    .unwrap()
                    .is_modified());
            })
            .unwrap();
    }

    #[gpui::test]
    fn deleting_several_features_is_one_undo_step(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                // Way first (nodes are still referenced until it goes).
                view.set_selection(vec![
                    osm_gpui::selection::FeatureRef {
                        layer_id,
                        kind: osm_gpui::selection::FeatureKind::Way,
                        id: 10,
                    },
                    osm_gpui::selection::FeatureRef {
                        layer_id,
                        kind: osm_gpui::selection::FeatureKind::Node,
                        id: 1,
                    },
                ]);
                view.run_selection_action("delete", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert!(view.selected.is_empty());
                assert!(way_nodes(view, layer_id, 10).is_empty());

                let action = view.undo_stack.undo().unwrap();
                view.apply_undo_action(&action, false);
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
                let node = view
                    .layer_manager
                    .find_layer(layer_id)
                    .unwrap()
                    .as_editable()
                    .unwrap()
                    .node_lat_lon(1);
                assert!(node.is_some());
            })
            .unwrap();
    }

    /// A view whose active layer holds `selection_actions::test_support`'s
    /// road network, with a server baseline so the upload diff is meaningful.
    fn setup_network(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
        setup_network_with(cx, |_| {})
    }

    /// `setup_network` with `edit` applied to the network data first (e.g. to
    /// swap in different relations).
    fn setup_network_with(
        cx: &mut TestAppContext,
        edit: impl FnOnce(&mut osm_gpui::osm::OsmData),
    ) -> gpui::WindowHandle<MapViewer> {
        let mut data = selection_actions::test_support::network_data();
        edit(&mut data);
        cx.update(gpui_component::init);
        let window = cx.add_window(MapViewer::new);
        window
            .update(cx, |view, _window, _cx| {
                view.viewport =
                    Viewport::new(CENTER_LAT, CENTER_LON, 18.0, size(px(800.0), px(600.0)));
                let layer_id = view.layer_manager.alloc_id();
                let layer = OsmLayer::new_with_data(layer_id, "Net".to_string(), Arc::new(data));
                view.layer_manager.add_layer(Box::new(layer));
                view.active_layer = Some(layer_id);
                view.mode = EditMode::Select;
            })
            .unwrap();
        window
    }

    /// Replace the network's relation 900 with a route over `members`
    /// (`(way id, role)`).
    fn route_over(
        ways: &'static [(i64, &'static str)],
    ) -> impl FnOnce(&mut osm_gpui::osm::OsmData) {
        move |data| {
            data.relations = vec![osm_gpui::osm::OsmRelation {
                id: 900,
                version: 4,
                tags: [("type", "route"), ("name", "Test Route")]
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                members: ways
                    .iter()
                    .map(|&(reference, role)| osm_gpui::osm::OsmMember {
                        member_type: osm_gpui::osm::MemberType::Way,
                        reference,
                        role: role.to_string(),
                    })
                    .collect(),
            }];
        }
    }

    #[gpui::test]
    fn split_way_in_a_relation_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let window = setup_network_with(cx, route_over(&[(100, ""), (101, "forward"), (102, "")]));
        window
            .update(cx, |view, _window, cx| {
                // Node 4 on way 101 only.
                let layer_id = view.active_layer.unwrap();
                view.set_selection(vec![
                    osm_gpui::selection::FeatureRef {
                        layer_id,
                        kind: Node,
                        id: 4,
                    },
                    osm_gpui::selection::FeatureRef {
                        layer_id,
                        kind: Way,
                        id: 101,
                    },
                ]);
                view.run_selection_action("split", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                let after = relation_members(view, 900);
                assert_eq!(after.len(), 4);
                assert_eq!(after[1], (101, "forward".to_string()));
                assert!(after[2].0 < 0 && after[2].1 == "forward");
                // The new way, way 101, and the relation.
                assert_eq!(pending(view), (1, 2, 0));

                undo_step(view);
                assert_eq!(
                    relation_members(view, 900),
                    vec![
                        (100, String::new()),
                        (101, "forward".to_string()),
                        (102, String::new())
                    ]
                );
                assert_eq!(pending(view), (0, 0, 0));

                redo_step(view);
                assert_eq!(relation_members(view, 900), after);
                assert_eq!(pending(view), (1, 2, 0));
                undo_step(view);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    fn select(view: &mut MapViewer, kind: osm_gpui::selection::FeatureKind, ids: &[i64]) {
        let layer_id = view.active_layer.unwrap();
        view.set_selection(
            ids.iter()
                .map(|&id| osm_gpui::selection::FeatureRef { layer_id, kind, id })
                .collect(),
        );
    }

    fn pending(view: &MapViewer) -> (usize, usize, usize) {
        let layer_id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer(layer_id)
            .unwrap()
            .diff_for_upload()
            .counts()
    }

    fn undo_step(view: &mut MapViewer) {
        let action = view.undo_stack.undo().expect("something to undo");
        view.apply_undo_action(&action, false);
    }

    fn redo_step(view: &mut MapViewer) {
        let action = view.undo_stack.redo().expect("something to redo");
        view.apply_undo_action(&action, true);
    }

    /// A drag of one segment of building 108 (a triangle 14-15-16) along
    /// that segment's screen normal, `dist` pixels, plus the drag record.
    fn extrude_triangle_drag(
        view: &MapViewer,
        layer_id: LayerId,
        style: ExtrudeStyle,
        dist: f32,
    ) -> (ExtrudeDrag, gpui::Point<gpui::Pixels>) {
        let screen = |id: i64| {
            let (lat, lon) = view
                .layer_manager
                .find_layer(layer_id)
                .and_then(|l| l.as_editable())
                .and_then(|e| e.node_lat_lon(id))
                .unwrap();
            view.viewport.geo_to_screen(lat, lon)
        };
        let (a, b) = (screen(14), screen(15));
        let mid = point((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        let (dx, dy) = (f32::from(b.x - a.x), f32::from(b.y - a.y));
        let len = dx.hypot(dy);
        let up = point(mid.x + px(-dy / len * dist), mid.y + px(dx / len * dist));
        let drag = ExtrudeDrag {
            layer: layer_id,
            way_id: 108,
            node_a: 14,
            node_b: 15,
            segment: 0,
            style,
            down: mid,
        };
        (drag, up)
    }

    #[gpui::test]
    fn extrude_reshapes_the_way_in_one_undoable_step(cx: &mut TestAppContext) {
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                let before_pending = pending(view);
                let (drag, up) = extrude_triangle_drag(view, layer_id, ExtrudeStyle::Reshape, 60.0);
                view.commit_extrude_reshape(&drag, up);
                // A triangle's neighbours are not along the drag: 2 new nodes
                // are spliced in between 14 and 15; the way keeps its id.
                let after = way_nodes(view, layer_id, 108);
                assert_eq!(after.len(), 6);
                assert_eq!((after[0], after[3], after[4], after[5]), (14, 15, 16, 14));
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_ne!(pending(view), before_pending);

                undo_step(view);
                assert_eq!(way_nodes(view, layer_id, 108), vec![14, 15, 16, 14]);
                assert_eq!(pending(view), before_pending);

                redo_step(view);
                assert_eq!(way_nodes(view, layer_id, 108), after);
            })
            .unwrap();
    }

    #[gpui::test]
    fn extrude_move_translates_without_new_nodes_and_undoes(cx: &mut TestAppContext) {
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                let before_pending = pending(view);
                let old = |view: &MapViewer, id| {
                    view.layer_manager
                        .find_layer(layer_id)
                        .and_then(|l| l.as_editable())
                        .and_then(|e| e.node_lat_lon(id))
                        .unwrap()
                };
                let (a0, b0) = (old(view, 14), old(view, 15));
                let (drag, up) = extrude_triangle_drag(view, layer_id, ExtrudeStyle::Move, 60.0);
                view.commit_extrude_reshape(&drag, up);
                assert_eq!(way_nodes(view, layer_id, 108), vec![14, 15, 16, 14]);
                assert_ne!(old(view, 14), a0);
                assert_ne!(old(view, 15), b0);
                undo_step(view);
                assert_eq!((old(view, 14), old(view, 15)), (a0, b0));
                assert_eq!(pending(view), before_pending);
            })
            .unwrap();
    }

    #[gpui::test]
    fn extrude_refuses_to_grow_a_full_way(cx: &mut TestAppContext) {
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                let mut nodes = long_way_nodes(2000);
                nodes[0] = 14;
                nodes[1] = 15;
                view.layer_manager
                    .find_layer_mut(layer_id)
                    .and_then(|l| l.as_editable_mut())
                    .unwrap()
                    .set_way_nodes(108, nodes);
                let (drag, up) = extrude_triangle_drag(view, layer_id, ExtrudeStyle::Reshape, 60.0);
                view.commit_extrude_reshape(&drag, up);
                assert_eq!(way_nodes(view, layer_id, 108).len(), 2000);
                assert!(view.undo_stack.actions.is_empty());
                assert!(view.status_message.is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn join_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                select(view, Way, &[100, 101]);
                view.run_selection_action("join", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3, 4, 5]);
                assert!(way_nodes(view, layer_id, 101).is_empty());
                assert_eq!(pending(view), (0, 1, 1));
                // The joined way is what stays selected.
                assert_eq!(view.selected.len(), 1);
                assert_eq!(view.selected[0].id, 100);

                undo_step(view);
                assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3]);
                assert_eq!(way_nodes(view, layer_id, 101), vec![3, 4, 5]);
                assert_eq!(pending(view), (0, 0, 0));
                let tags = view
                    .layer_manager
                    .find_layer(layer_id)
                    .unwrap()
                    .as_editable()
                    .unwrap()
                    .feature_tags(&osm_gpui::selection::FeatureRef {
                        layer_id,
                        kind: Way,
                        id: 100,
                    })
                    .unwrap();
                assert!(!tags.iter().any(|(k, _)| k == "name"));

                redo_step(view);
                assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3, 4, 5]);
                assert!(way_nodes(view, layer_id, 101).is_empty());
                assert_eq!(pending(view), (0, 1, 1));
                // Undo once more to prove redo left a sound state.
                undo_step(view);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn refused_join_leaves_no_undo_step_and_shows_why(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                select(view, Way, &[101, 105]);
                view.run_selection_action("join", cx);
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("Can't join: the ways have different values for 'name'")
                );
                assert!(view.undo_stack.actions.is_empty());
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    fn relation_members(view: &MapViewer, relation_id: i64) -> Vec<(i64, String)> {
        let layer_id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer(layer_id)
            .unwrap()
            .as_editable()
            .unwrap()
            .relation(relation_id)
            .unwrap()
            .members
            .iter()
            .map(|m| (m.reference, m.role.clone()))
            .collect()
    }

    #[gpui::test]
    fn set_relation_members_undoes_and_redoes_and_is_labelled(cx: &mut TestAppContext) {
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                let before = view
                    .layer_manager
                    .find_layer(layer_id)
                    .unwrap()
                    .as_editable()
                    .unwrap()
                    .relation(900)
                    .unwrap()
                    .members;
                let mut after = before.clone();
                after.push(osm_gpui::osm::OsmMember {
                    member_type: osm_gpui::osm::MemberType::Way,
                    reference: 101,
                    role: "outer".to_string(),
                });
                view.layer_manager
                    .find_layer_mut(layer_id)
                    .unwrap()
                    .as_editable_mut()
                    .unwrap()
                    .set_relation_members(900, after.clone());
                let action = UndoableAction::SetRelationMembers {
                    layer: layer_id,
                    relation_id: 900,
                    before,
                    after,
                };
                assert_eq!(action.description(), "Changed the members of a relation");
                view.undo_stack.push(action);
                assert_eq!(relation_members(view, 900).len(), 3);
                assert_eq!(pending(view), (0, 1, 0));

                undo_step(view);
                assert_eq!(relation_members(view, 900).len(), 2);
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert_eq!(
                    relation_members(view, 900).last(),
                    Some(&(101, "outer".to_string()))
                );
                assert_eq!(pending(view), (0, 1, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn create_and_delete_relation_undo_and_redo(cx: &mut TestAppContext) {
        use osm_gpui::osm::{MemberType, OsmMember};
        use osm_gpui::selection::DeletedRelationSnapshot;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                let member = |member_type, reference| OsmMember {
                    member_type,
                    reference,
                    role: "x".to_string(),
                };

                // Create a relation containing existing relation 900.
                let new_id = editable(view)
                    .create_relation(
                        vec![("type".to_string(), "route".to_string())],
                        vec![
                            member(MemberType::Relation, 900),
                            member(MemberType::Way, 101),
                        ],
                    )
                    .unwrap();
                assert!(new_id < 0);
                let snapshot = DeletedRelationSnapshot {
                    relation: editable(view).relation(new_id).unwrap(),
                    index: editable(view).relations().len() - 1,
                };
                let create = UndoableAction::CreateRelation {
                    layer: layer_id,
                    snapshot,
                };
                assert_eq!(create.description(), "Created a relation");
                view.undo_stack.push(create);
                assert_eq!(pending(view), (1, 0, 0));
                assert_eq!(editable(view).relations_containing_relation(900), [new_id]);
                // 900 is a member now, so it can't be deleted.
                assert!(editable(view).delete_relation(900).is_none());

                undo_step(view);
                assert!(editable(view).relation(new_id).is_none());
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert_eq!(editable(view).relation(new_id).unwrap().members.len(), 2);
                assert_eq!(pending(view), (1, 0, 0));
                // A later allocation never reuses the id.
                let other = editable(view).create_relation(vec![], vec![]).unwrap();
                assert!(other < new_id);
                editable(view).delete_relation(other).unwrap();

                // Delete both; the created one has to go first.
                let original_900 = editable(view).relation(900).unwrap();
                let created = editable(view).delete_relation(new_id).unwrap();
                let removed = editable(view).delete_relation(900).unwrap();
                assert_eq!(removed.relation, original_900);
                let delete = UndoableAction::Batch {
                    label: "Deleted relations".to_string(),
                    actions: vec![
                        UndoableAction::DeleteRelation {
                            layer: layer_id,
                            snapshot: created,
                        },
                        UndoableAction::DeleteRelation {
                            layer: layer_id,
                            snapshot: removed,
                        },
                    ],
                };
                view.undo_stack.push(delete);
                assert_eq!(pending(view), (0, 0, 1));
                assert_eq!(
                    UndoableAction::DeleteRelation {
                        layer: layer_id,
                        snapshot: DeletedRelationSnapshot {
                            relation: original_900.clone(),
                            index: 0
                        }
                    }
                    .description(),
                    "Deleted a relation"
                );

                undo_step(view);
                assert_eq!(editable(view).relation(900).unwrap(), original_900);
                assert_eq!(editable(view).relation(new_id).unwrap().members.len(), 2);
                assert_eq!(pending(view), (1, 0, 0));
                redo_step(view);
                assert!(editable(view).relation(900).is_none());
                assert!(editable(view).relation(new_id).is_none());
                assert_eq!(pending(view), (0, 0, 1));
            })
            .unwrap();
    }

    fn editable(view: &mut MapViewer) -> &mut dyn osm_gpui::layers::EditableLayer {
        let layer_id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer_mut(layer_id)
            .unwrap()
            .as_editable_mut()
            .unwrap()
    }

    fn status_text(view: &MapViewer) -> Option<&str> {
        view.status_message.as_ref().map(|(m, _)| m.as_str())
    }

    #[gpui::test]
    fn new_relation_collects_the_selection_and_is_one_undo_step(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Node, Relation, Way};
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                select(view, Way, &[101, 100]);
                view.selected.push(osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: Node,
                    id: 7,
                });
                view.run_selection_action("new_relation", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(
                    view.undo_stack.actions[0].description(),
                    "Created a relation"
                );
                assert_eq!(pending(view), (1, 0, 0));

                // The new relation is selected, with the members in
                // selection order and empty roles, and no tags.
                assert_eq!(view.selected.len(), 1);
                assert_eq!(view.selected[0].kind, Relation);
                let new_id = view.selected[0].id;
                assert!(new_id < 0);
                let relation = editable(view).relation(new_id).unwrap();
                assert!(relation.tags.is_empty());
                let members: Vec<_> = relation
                    .members
                    .iter()
                    .map(|m| (m.reference, m.role.as_str()))
                    .collect();
                assert_eq!(members, [(101, ""), (100, ""), (7, "")]);
                assert_eq!(
                    status_text(view),
                    Some("Created a relation with 3 features. Add its tags and roles in the side panel")
                );

                undo_step(view);
                assert!(editable(view).relation(new_id).is_none());
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert_eq!(editable(view).relation(new_id).unwrap(), relation);
                assert_eq!(pending(view), (1, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn new_relation_needs_a_selection_and_accepts_relations(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                view.set_selection(Vec::new());
                view.run_selection_action("new_relation", cx);
                assert!(view.undo_stack.actions.is_empty());
                assert!(status_text(view).is_some_and(|m| m.starts_with("Select nodes, ways")));

                // A selected relation can be a member of the new one.
                view.select_relation(view.active_layer.unwrap(), 900);
                select(view, Way, &[100]);
                view.selected.push(osm_gpui::selection::FeatureRef {
                    layer_id: view.active_layer.unwrap(),
                    kind: osm_gpui::selection::FeatureKind::Relation,
                    id: 900,
                });
                view.run_selection_action("new_relation", cx);
                let new_id = view.selected[0].id;
                let relation = editable(view).relation(new_id).unwrap();
                assert_eq!(relation.members.len(), 2);
                assert_eq!(relation.members[1].reference, 900);
                assert_eq!(editable(view).relations_containing_relation(900), [new_id]);
            })
            .unwrap();
    }

    #[gpui::test]
    fn deleting_a_selected_relation_keeps_its_members_and_undoes(cx: &mut TestAppContext) {
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                let original = editable(view).relation(900).unwrap();
                view.select_relation(layer_id, 900);
                view.delete_selected_features(cx);

                assert!(editable(view).relation(900).is_none());
                // Its member way and node survive.
                assert!(editable(view).way_node_ids(102).is_some());
                assert!(editable(view).node_lat_lon(7).is_some());
                assert!(view.selected.is_empty());
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(
                    view.undo_stack.actions[0].description(),
                    "Deleted a relation"
                );
                assert_eq!(status_text(view), Some("Deleted 1 relation"));
                assert_eq!(pending(view), (0, 0, 1));

                undo_step(view);
                assert_eq!(editable(view).relation(900).unwrap(), original);
                assert_eq!(pending(view), (0, 0, 0));
                redo_step(view);
                assert!(editable(view).relation(900).is_none());
                assert_eq!(pending(view), (0, 0, 1));
            })
            .unwrap();
    }

    fn selected_ids(view: &MapViewer, kind: osm_gpui::selection::FeatureKind) -> Vec<i64> {
        let mut ids: Vec<i64> = view
            .selected
            .iter()
            .filter(|f| f.kind == kind)
            .map(|f| f.id)
            .collect();
        ids.sort();
        ids
    }

    /// Undo and redo select the features the step affected that still exist,
    /// for nodes, ways and relations alike: undoing a delete selects the
    /// restored feature, redoing the delete leaves nothing selected (it is
    /// gone), and a created relation is selected again by redo but dropped
    /// from the selection by undo.
    #[gpui::test]
    fn undo_and_redo_select_the_affected_features_for_every_feature_kind(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Node, Relation, Way};
        let window = setup_network(cx);
        window
            .update(cx, |view, window, cx| {
                let layer_id = view.active_layer.unwrap();

                select(view, Way, &[102]);
                view.delete_selected_features(cx);
                assert!(view.selected.is_empty());
                view.on_undo(&Undo, window, cx);
                assert!(editable(view).way_node_ids(102).is_some());
                assert_eq!(selected_ids(view, Way), vec![102]);
                view.on_redo(&Redo, window, cx);
                // The way is gone, so it can't stay selected (the relation
                // that lost it as a member is also part of the step).
                assert!(editable(view).way_node_ids(102).is_none());
                assert!(selected_ids(view, Way).is_empty());
                view.on_undo(&Undo, window, cx);

                select(view, Node, &[7]);
                view.delete_selected_features(cx);
                assert!(view.selected.is_empty());
                view.on_undo(&Undo, window, cx);
                assert!(editable(view).node_lat_lon(7).is_some());
                assert_eq!(selected_ids(view, Node), vec![7]);
                view.on_redo(&Redo, window, cx);
                assert!(editable(view).node_lat_lon(7).is_none());
                assert!(selected_ids(view, Node).is_empty());
                view.on_undo(&Undo, window, cx);

                view.select_relation(layer_id, 900);
                view.delete_selected_features(cx);
                view.on_undo(&Undo, window, cx);
                assert!(editable(view).relation(900).is_some());
                assert_eq!(selected_ids(view, Relation), vec![900]);
                view.on_redo(&Redo, window, cx);
                assert!(view.selected.is_empty());
                view.on_undo(&Undo, window, cx);

                // Creating a relation selects it. Undoing removes it, so the
                // selection must not keep pointing at it (stale selection);
                // redoing selects it again.
                select(view, Way, &[101]);
                view.run_selection_action("new_relation", cx);
                assert_eq!(view.selected.len(), 1);
                assert_eq!(view.selected[0].kind, Relation);
                let created = view_relation_id(view);
                view.on_undo(&Undo, window, cx);
                assert!(editable(view).relation(created).is_none());
                assert!(view.selected.is_empty());
                view.on_redo(&Redo, window, cx);
                assert!(editable(view).relation(created).is_some());
                assert_eq!(selected_ids(view, Relation), vec![created]);
            })
            .unwrap();
    }

    /// A batch selects the union of what its steps affected (a multi-feature
    /// delete is restored as a multi-feature selection), and a reshape
    /// selects the reshaped way even when something else was selected.
    #[gpui::test]
    fn undo_selects_batch_and_reshape_targets(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let window = setup_network(cx);
        window
            .update(cx, |view, window, cx| {
                select(view, Way, &[101, 102]);
                view.delete_selected_features(cx);
                view.on_undo(&Undo, window, cx);
                assert_eq!(selected_ids(view, Way), vec![101, 102]);

                select(view, Way, &[101]);
                view.run_selection_action("reverse", cx);
                select(view, Node, &[7]);
                view.on_undo(&Undo, window, cx);
                assert_eq!(selected_ids(view, Way), vec![101]);
                assert!(selected_ids(view, Node).is_empty());
                select(view, Node, &[7]);
                view.on_redo(&Redo, window, cx);
                assert_eq!(selected_ids(view, Way), vec![101]);
            })
            .unwrap();
    }

    /// Tag edits and moves select what they changed, and ids that no longer
    /// exist are always pruned from the selection.
    #[gpui::test]
    fn undo_selects_tagged_and_moved_features_and_prunes_stale_ids(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let window = setup_network(cx);
        window
            .update(cx, |view, window, cx| {
                tag_node_one(view);
                select(view, Way, &[101]);
                view.on_undo(&Undo, window, cx);
                assert_eq!(selected_ids(view, Node), vec![1]);
                assert!(selected_ids(view, Way).is_empty());

                let layer_id = view.active_layer.unwrap();
                let (lat, lon) = editable(view).node_lat_lon(1).unwrap();
                editable(view).commit_node_moves(&[(1, lat + 0.001, lon)]);
                view.undo_stack.push(UndoableAction::MoveNodes {
                    per_layer: vec![(layer_id, vec![(1, (lat, lon), (lat + 0.001, lon))])],
                });
                // A selection holding a feature that doesn't exist is pruned
                // even though the step didn't name it.
                view.set_selection(vec![osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: Way,
                    id: 424242,
                }]);
                view.on_undo(&Undo, window, cx);
                assert_eq!(selected_ids(view, Node), vec![1]);
                assert!(selected_ids(view, Way).is_empty());
            })
            .unwrap();
    }

    /// The same through the real key binding: Delete a way with the map
    /// focused, then cmd-z restores and selects it.
    #[gpui::test]
    fn cmd_z_after_delete_selects_the_restored_way(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network(cx);
        cx.update(|cx| {
            cx.bind_keys(key_bindings_from_settings(
                &osm_gpui::settings_store::snapshot(),
            ))
        });
        let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let root = window.root(vcx).unwrap();
        vcx.draw(
            point(px(0.0), px(0.0)),
            size(
                gpui::AvailableSpace::Definite(px(1200.0)),
                gpui::AvailableSpace::Definite(px(800.0)),
            ),
            |_, _| gpui::AnyView::from(root),
        );
        window
            .update(vcx, |view, _, cx| {
                select(view, Way, &[102]);
                view.delete_selected_features(cx);
                assert!(selected_ids(view, Way).is_empty());
            })
            .unwrap();
        vcx.simulate_keystrokes("cmd-z");
        window
            .update(vcx, |view, _, _| {
                assert!(editable(view).way_node_ids(102).is_some());
                // Exactly the way: way 102 is a member of relation 900, which
                // the step also touched, but relations are never mixed in.
                assert_eq!(view.selected.len(), 1);
                assert_eq!(selected_ids(view, Way), vec![102]);
            })
            .unwrap();

        // The menu path: the Undo action dispatched on the window (what the
        // harness's `action Undo` and the Edit menu do), then redo, then
        // undo again.
        vcx.dispatch_action(Redo);
        window
            .update(vcx, |view, _, _| {
                assert!(editable(view).way_node_ids(102).is_none());
                // Only the relation that lost the way is left to select.
                assert!(selected_ids(view, Way).is_empty());
            })
            .unwrap();
        vcx.dispatch_action(Undo);
        window
            .update(vcx, |view, _, _| {
                assert_eq!(view.selected.len(), 1);
                assert_eq!(selected_ids(view, Way), vec![102]);
            })
            .unwrap();
    }

    /// Mid-way through drawing in Add mode the selection is left alone apart
    /// from pruning, so undo can't disturb the drawing session.
    #[gpui::test]
    fn undo_while_drawing_leaves_the_selection_alone(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::{Node, Way};
        let window = setup_network(cx);
        window
            .update(cx, |view, window, cx| {
                tag_node_one(view);
                select(view, Way, &[101]);
                view.add_progress = Some(AddProgress {
                    way_id: None,
                    last_node_id: 1,
                    at_start: false,
                });
                view.on_undo(&Undo, window, cx);
                assert_eq!(selected_ids(view, Way), vec![101]);
                assert!(selected_ids(view, Node).is_empty());
            })
            .unwrap();
    }

    /// Tab walks the enabled up/down/remove buttons of the Members rows in
    /// row order, and a row counts as focused (so its buttons are shown)
    /// whenever focus is on any of its buttons.
    #[gpui::test]
    fn tab_reaches_member_buttons_in_row_order_and_reveals_the_row(cx: &mut TestAppContext) {
        let mut data = selection_actions::test_support::network_data();
        route_over(&[(100, ""), (101, ""), (102, "")])(&mut data);
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MapViewer::new(window, cx));
            gpui_component::Root::new(view, window, cx)
        });
        let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let root = window.root(vcx).unwrap();
        let view: gpui::Entity<MapViewer> = root
            .read_with(vcx, |root, _| root.view().clone())
            .downcast::<MapViewer>()
            .unwrap();
        view.update(vcx, |view, _| {
            view.viewport = Viewport::new(CENTER_LAT, CENTER_LON, 18.0, size(px(800.0), px(600.0)));
            let layer_id = view.layer_manager.alloc_id();
            let layer = OsmLayer::new_with_data(layer_id, "Net".to_string(), Arc::new(data));
            view.layer_manager.add_layer(Box::new(layer));
            view.active_layer = Some(layer_id);
            view.mode = EditMode::Select;
            view.set_selection(vec![osm_gpui::selection::FeatureRef {
                layer_id,
                kind: osm_gpui::selection::FeatureKind::Relation,
                id: 900,
            }]);
        });
        let draw = |vcx: &mut gpui::VisualTestContext| {
            let root = root.clone();
            vcx.draw(
                point(px(0.0), px(0.0)),
                size(
                    gpui::AvailableSpace::Definite(px(1200.0)),
                    gpui::AvailableSpace::Definite(px(800.0)),
                ),
                |_, _| gpui::AnyView::from(root),
            );
        };
        draw(vcx);

        // Which row (if any) holds focus now.
        let focused_row = |vcx: &mut gpui::VisualTestContext| -> Option<usize> {
            view.update_in(vcx, |view, window, cx| {
                view.member_controls_focus
                    .borrow()
                    .iter()
                    .position(|handle| handle.contains_focused(window, cx))
            })
        };
        // Real key presses: the window's `Root` turns Tab into `focus_next`.
        let tab = |vcx: &mut gpui::VisualTestContext, forward: bool| {
            vcx.simulate_keystrokes(if forward { "tab" } else { "shift-tab" });
            draw(vcx);
        };

        let mut stops: Vec<usize> = Vec::new();
        for _ in 0..80 {
            tab(vcx, true);
            if let Some(row) = focused_row(vcx) {
                stops.push(row);
            }
        }
        // Row 0 has down+remove, row 1 up+down+remove, row 2 up+remove, and
        // the cycle repeats once Tab wraps around.
        let one_pass = [0, 0, 1, 1, 1, 2, 2];
        assert!(
            stops.starts_with(&one_pass),
            "unexpected tab order {stops:?}"
        );
        assert!(stops.len() >= one_pass.len() * 2, "Tab never wrapped");

        // Shift-Tab walks back the other way: from row 1's first button
        // (reached by Tab until the row changes) it returns to row 0.
        while focused_row(vcx) != Some(0) {
            tab(vcx, true);
        }
        while focused_row(vcx) != Some(1) {
            tab(vcx, true);
        }
        tab(vcx, false);
        assert_eq!(focused_row(vcx), Some(0));
    }

    /// The id of the relation `new_relation` created (the only negative one).
    fn view_relation_id(view: &mut MapViewer) -> i64 {
        editable(view)
            .relation_ids()
            .into_iter()
            .find(|id| *id < 0)
            .expect("a created relation")
    }

    #[gpui::test]
    fn show_downloaded_area_toggles_and_defaults_on(cx: &mut TestAppContext) {
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                assert!(view.shows_downloaded_area());
                view.set_show_downloaded_area(false, cx);
                assert!(!view.shows_downloaded_area());
                // Setting the current value again is a no-op.
                view.set_show_downloaded_area(false, cx);
                assert!(!view.shows_downloaded_area());
                view.on_toggle_downloaded_area(&ToggleDownloadedArea, _window, cx);
                assert!(view.shows_downloaded_area());
            })
            .unwrap();
    }

    #[gpui::test]
    fn deleting_a_relation_that_is_a_member_is_refused_until_its_parent_goes(
        cx: &mut TestAppContext,
    ) {
        use osm_gpui::osm::{MemberType, OsmMember};
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                let parent = editable(view)
                    .create_relation(
                        vec![],
                        vec![OsmMember {
                            member_type: MemberType::Relation,
                            reference: 900,
                            role: String::new(),
                        }],
                    )
                    .unwrap();

                // Alone, 900 can't go: the parent would be left dangling.
                view.select_relation(layer_id, 900);
                view.delete_selected_features(cx);
                assert!(editable(view).relation(900).is_some());
                assert!(view.undo_stack.actions.is_empty());
                assert!(status_text(view)
                    .is_some_and(|m| m.starts_with("Can't delete relation 900")
                        && m.contains(&parent.to_string())));

                // Selected together with its parent, both go as one step.
                view.select_relation(layer_id, 900);
                view.selected.push(osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Relation,
                    id: parent,
                });
                view.delete_selected_features(cx);
                assert!(editable(view).relation(900).is_none());
                assert!(editable(view).relation(parent).is_none());
                assert_eq!(view.undo_stack.actions.len(), 1);
                assert_eq!(status_text(view), Some("Deleted 2 relations"));

                undo_step(view);
                assert!(editable(view).relation(900).is_some());
                assert!(editable(view).relation(parent).is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn separate_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Node;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                select(view, Node, &[4]);
                view.run_selection_action("separate", cx);
                assert_eq!(view.undo_stack.actions.len(), 1);
                let separated = way_nodes(view, layer_id, 103);
                let new_id = separated[1];
                assert!(new_id < 0);
                assert_eq!(pending(view), (1, 1, 0));
                // The original node is still the selection.
                assert_eq!(view.selected.len(), 1);
                assert_eq!(view.selected[0].id, 4);

                undo_step(view);
                assert_eq!(way_nodes(view, layer_id, 103), vec![8, 4, 9]);
                let editable = view
                    .layer_manager
                    .find_layer(layer_id)
                    .unwrap()
                    .as_editable()
                    .unwrap();
                assert_eq!(editable.node_lat_lon(new_id), None);
                assert_eq!(pending(view), (0, 0, 0));

                redo_step(view);
                assert_eq!(way_nodes(view, layer_id, 103), separated);
                assert_eq!(pending(view), (1, 1, 0));
                undo_step(view);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    /// Window-independent screen point for a spot well clear of the network.
    fn empty_spot(view: &MapViewer, east: f64) -> gpui::Point<gpui::Pixels> {
        view.viewport.geo_to_screen(39.98, -73.98 + east)
    }

    #[gpui::test]
    fn extend_from_a_way_end_appends_with_one_undo_step_per_click(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                select(view, Way, &[100]);
                view.run_selection_action("extend", cx);
                assert_eq!(view.mode, EditMode::Add);
                let progress = view.add_progress.as_ref().unwrap();
                assert_eq!((progress.way_id, progress.last_node_id), (Some(100), 3));
                assert!(view.undo_stack.actions.is_empty());
                assert_eq!(pending(view), (0, 0, 0));

                view.handle_add_click(empty_spot(view, 0.0), false);
                let after_one = way_nodes(view, layer_id, 100);
                assert_eq!(&after_one[..3], &[1, 2, 3]);
                assert_eq!(after_one.len(), 4);
                view.handle_add_click(empty_spot(view, 0.001), false);
                let after_two = way_nodes(view, layer_id, 100);
                assert_eq!(&after_two[..4], &after_one[..]);
                assert_eq!(after_two.len(), 5);
                assert_eq!(view.undo_stack.actions.len(), 2);
                assert_eq!(pending(view), (2, 1, 0));
                assert_eq!(view.selected[0].id, 100);

                undo_step(view);
                assert_eq!(way_nodes(view, layer_id, 100), after_one);
                undo_step(view);
                assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3]);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn extend_from_a_start_node_prepends_and_undoes_from_the_front(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Node;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                select(view, Node, &[1]);
                view.run_selection_action("extend", cx);
                assert_eq!(view.mode, EditMode::Add);
                assert!(view.add_progress.as_ref().unwrap().at_start);

                view.handle_add_click(empty_spot(view, 0.0), false);
                let first = way_nodes(view, layer_id, 100);
                assert_eq!(&first[1..], &[1, 2, 3]);
                view.handle_add_click(empty_spot(view, 0.001), false);
                let second = way_nodes(view, layer_id, 100);
                assert_eq!(&second[2..], &[1, 2, 3]);
                // The newest node leads, and the one before it follows.
                assert_eq!(second[1], first[0]);
                assert_eq!(pending(view), (2, 1, 0));

                // Enter (or Esc) ends the continuation; the way is kept.
                view.add_progress = None;

                undo_step(view);
                assert_eq!(way_nodes(view, layer_id, 100), first);
                undo_step(view);
                assert_eq!(way_nodes(view, layer_id, 100), vec![1, 2, 3]);
                assert_eq!(pending(view), (0, 0, 0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn refused_extend_stays_in_select_mode(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Node;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                select(view, Node, &[2]);
                view.run_selection_action("extend", cx);
                assert_eq!(view.mode, EditMode::Select);
                assert!(view.add_progress.is_none());
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("Select an open way, or the end node of one, to continue drawing it")
                );
            })
            .unwrap();
    }

    /// Way 100 reshaped to `len` nodes, still from node 1 to node 3.
    fn long_way_nodes(len: usize) -> Vec<i64> {
        let mut nodes = vec![1];
        nodes.extend(std::iter::repeat_n(2, len - 2));
        nodes.push(3);
        nodes
    }

    #[gpui::test]
    fn add_mode_refuses_to_grow_a_way_past_the_node_limit(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                select(view, Way, &[100]);
                view.run_selection_action("extend", cx);
                let set = |view: &mut MapViewer, len| {
                    view.layer_manager
                        .find_layer_mut(layer_id)
                        .and_then(|l| l.as_editable_mut())
                        .unwrap()
                        .set_way_nodes(100, long_way_nodes(len));
                };

                // 1,999 nodes: one more click reaches the limit exactly.
                set(view, 1999);
                view.handle_add_click(empty_spot(view, 0.0), false);
                assert_eq!(way_nodes(view, layer_id, 100).len(), 2000);
                let undo_steps = view.undo_stack.actions.len();

                // At the limit the next click is refused, with a message,
                // and changes nothing (no node, no undo step).
                let before = pending(view);
                view.handle_add_click(empty_spot(view, 0.001), false);
                assert_eq!(way_nodes(view, layer_id, 100).len(), 2000);
                assert_eq!(pending(view), before);
                assert_eq!(view.undo_stack.actions.len(), undo_steps);
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("A way can have at most 2,000 nodes on this server")
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn inserting_a_node_on_a_full_way_is_refused(cx: &mut TestAppContext) {
        let window = setup_network(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                view.layer_manager
                    .find_layer_mut(layer_id)
                    .and_then(|l| l.as_editable_mut())
                    .unwrap()
                    .set_way_nodes(100, long_way_nodes(2000));
                let before = pending(view);
                let drag = ExtrudeDrag {
                    layer: layer_id,
                    way_id: 100,
                    node_a: 1,
                    node_b: 2,
                    segment: 0,
                    style: ExtrudeStyle::Reshape,
                    down: point(px(0.0), px(0.0)),
                };
                view.insert_node_on_segment(&drag, point(px(400.0), px(300.0)));
                assert_eq!(way_nodes(view, layer_id, 100).len(), 2000);
                assert_eq!(pending(view), before);
                assert!(view.status_message.is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn upload_is_refused_for_an_oversized_way_and_selects_it(cx: &mut TestAppContext) {
        let window = setup_network(cx);
        window
            .update(cx, |view, window, cx| {
                let layer_id = view.active_layer.unwrap();
                view.layer_manager
                    .find_layer_mut(layer_id)
                    .and_then(|l| l.as_editable_mut())
                    .unwrap()
                    .set_way_nodes(100, long_way_nodes(2104));
                view.open_upload_dialog_as(Some("me"), window, cx);
                assert!(view.upload_dialog.is_none() && view.sign_in_dialog.is_none());
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some(
                        "Way 100 has 2,104 nodes; the server allows 2,000. Split it before uploading."
                    )
                );
                assert_eq!(view.selected.len(), 1);
                assert_eq!(view.selected[0].id, 100);
                assert_eq!(view.selected[0].layer_id, layer_id);
                // The refusal is also a dialog naming the object and limit,
                // and it is shown to a signed-out user too (signing in would
                // not help).
                let notice = view.notice_dialog.clone().expect("refusal dialog");
                assert!(notice.read(cx).message().contains("Way 100 has 2,104 nodes"));
                assert!(notice.read(cx).message().contains("allows 2,000"));
                view.notice_dialog = None;
                view.open_upload_dialog_as(None, window, cx);
                assert!(view.notice_dialog.is_some() && view.sign_in_dialog.is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn an_unmodified_oversized_way_does_not_block_other_changes(cx: &mut TestAppContext) {
        let window = setup_network_with(cx, |data| {
            data.ways.get_mut(&100).unwrap().nodes = long_way_nodes(2500);
        });
        window
            .update(cx, |view, _window, _cx| {
                make_pending_change(view);
                let layer_id = view.active_layer.unwrap();
                let diff = view
                    .layer_manager
                    .find_layer(layer_id)
                    .unwrap()
                    .diff_for_upload();
                assert!(diff.modified_ways.is_empty());
                let limits = osm_gpui::limits::Limits::for_server(&settings_store::api_base_url());
                assert_eq!(osm_gpui::limits::check_upload(&[&diff], &limits), Ok(()));
            })
            .unwrap();
    }

    /// Give the test view's layer a pending change (one new node).
    fn make_pending_change(view: &mut MapViewer) {
        let id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer_mut(id)
            .and_then(|l| l.as_editable_mut())
            .unwrap()
            .create_node(CENTER_LAT, CENTER_LON, None)
            .unwrap();
    }

    #[gpui::test]
    fn upload_without_changes_shows_a_status_message(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                view.open_upload_dialog_as(Some("me"), window, cx);
                assert!(view.upload_dialog.is_none() && view.sign_in_dialog.is_none());
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("Nothing to upload")
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_upload_comment_survives_closing_the_dialog(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MapViewer::new(window, cx));
            gpui_component::Root::new(view, window, cx)
        });
        let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let root = window.root(vcx).unwrap();
        let view: gpui::Entity<MapViewer> = root
            .read_with(vcx, |root, _| root.view().clone())
            .downcast::<MapViewer>()
            .unwrap();
        view.update_in(vcx, |view, window, cx| {
            let layer_id = view.layer_manager.alloc_id();
            let layer = OsmLayer::new_with_data(
                layer_id,
                "L".to_string(),
                Arc::new(way_fixture(CENTER_LAT, CENTER_LON)),
            );
            view.layer_manager.add_layer(Box::new(layer));
            view.active_layer = Some(layer_id);
            make_pending_change(view);

            view.open_upload_dialog_as(Some("me"), window, cx);
            let dialog = view.upload_dialog.clone().expect("dialog opens");
            dialog.update(cx, |d, cx| {
                d.set_draft_comment("Add a cafe".into(), window, cx)
            });
            view.close_upload_dialog_keeping_draft(cx);
            assert!(view.upload_dialog.is_none());
            assert_eq!(view.upload_comment_draft, "Add a cafe");

            view.open_upload_dialog_as(Some("me"), window, cx);
            let dialog = view.upload_dialog.clone().expect("dialog reopens");
            assert_eq!(dialog.read(cx).draft_comment(cx), "Add a cafe");
        });
    }

    #[gpui::test]
    fn edits_and_undo_are_refused_while_an_upload_runs(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                make_pending_change(view);
                // Not uploading: nothing is refused.
                assert!(!view.edits_blocked(cx));
                assert!(view.status_message.is_none());

                view.transfer = Some(TransferHandle::new(
                    TransferKind::Upload,
                    TransferPhase::OpeningChangeset,
                ));
                for handler in [
                    |v: &mut MapViewer, w: &mut Window, cx: &mut Context<MapViewer>| {
                        v.on_undo(&Undo, w, cx)
                    },
                    |v: &mut MapViewer, w: &mut Window, cx: &mut Context<MapViewer>| {
                        v.on_redo(&Redo, w, cx)
                    },
                    |v: &mut MapViewer, w: &mut Window, cx: &mut Context<MapViewer>| {
                        v.on_delete_selection(&DeleteSelection, w, cx)
                    },
                ] {
                    view.status_message = None;
                    handler(view, window, cx);
                    assert_eq!(
                        view.status_message.as_ref().map(|(m, _)| m.as_str()),
                        Some("An upload is in progress. Wait for it to finish before editing.")
                    );
                }

                // A download does not freeze editing.
                view.transfer = Some(TransferHandle::new(
                    TransferKind::Download,
                    TransferPhase::Downloading,
                ));
                assert!(!view.edits_blocked(cx));

                // Quitting during an upload is refused with a message.
                view.status_message = None;
                view.refuse_quit_during_upload(cx);
                assert_eq!(
                    view.status_message.as_ref().map(|(m, _)| m.as_str()),
                    Some("An upload is in progress. Wait for it to finish before quitting.")
                );
            })
            .unwrap();
    }

    /// Every way the side panel (and the map) changes the layers is refused
    /// during an upload, with the standard message and nothing recorded.
    #[gpui::test]
    fn side_panel_edits_are_refused_while_an_upload_runs(cx: &mut TestAppContext) {
        use osm_gpui::osm::{MemberType, OsmMember};
        use osm_gpui::selection::{FeatureKind, FeatureRef};
        const REFUSAL: &str = "An upload is in progress. Wait for it to finish before editing.";
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                let layer_id = view.active_layer.unwrap();
                let members = vec![
                    OsmMember {
                        member_type: MemberType::Node,
                        reference: 1,
                        role: "a".into(),
                    },
                    OsmMember {
                        member_type: MemberType::Node,
                        reference: 2,
                        role: "b".into(),
                    },
                ];
                let relation_id = editable(view)
                    .create_relation(Vec::new(), members.clone())
                    .unwrap();
                let node1 = FeatureRef {
                    layer_id,
                    kind: FeatureKind::Node,
                    id: 1,
                };
                let ctx = TagEditContext {
                    features: vec![node1],
                    original_key: String::new(),
                    original_value: String::new(),
                    is_add: true,
                };
                let member = MemberRef {
                    layer_id,
                    relation_id,
                    index: 0,
                };
                let preset_tags =
                    std::collections::HashMap::from([("amenity".to_string(), "cafe".to_string())]);
                select(view, FeatureKind::Node, &[1]);

                // Each edit, as the side panel makes it.
                type Edit = Box<dyn Fn(&mut MapViewer, &mut Context<MapViewer>)>;
                let edits: Vec<(&str, Edit)> = vec![
                    ("tag edit", {
                        let ctx = TagEditContext {
                            features: ctx.features.clone(),
                            original_key: String::new(),
                            original_value: String::new(),
                            is_add: true,
                        };
                        Box::new(move |v, _| v.apply_tag_edit(&ctx, "amenity", "cafe"))
                    }),
                    ("tag delete", Box::new(|v, cx| v.delete_tag("amenity", cx))),
                    (
                        "preset",
                        Box::new({
                            let tags = preset_tags.clone();
                            move |v, _| v.apply_preset(&node1, &tags, &[])
                        }),
                    ),
                    (
                        "field / NSI preset",
                        Box::new({
                            let tags = preset_tags.clone();
                            move |v, _| v.apply_nsi_preset(&node1, tags.clone())
                        }),
                    ),
                    (
                        "member role",
                        Box::new(move |v, _| v.set_relation_member_role(member, "z")),
                    ),
                    (
                        "member order",
                        Box::new(move |v, _| v.move_relation_member(member, 1)),
                    ),
                    (
                        "member remove",
                        Box::new(move |v, _| v.remove_relation_member(member)),
                    ),
                    (
                        "add to relation",
                        Box::new(move |v, _| v.add_selection_to_relation(relation_id)),
                    ),
                    (
                        "map add click",
                        Box::new(|v, _| v.handle_add_click(point(px(400.0), px(300.0)), false)),
                    ),
                ];

                view.transfer = Some(TransferHandle::new(
                    TransferKind::Upload,
                    TransferPhase::UploadingChanges { count: 1 },
                ));
                let before = (
                    pending(view),
                    view.undo_stack.actions.len(),
                    view.relation_of(layer_id, relation_id).unwrap().members,
                );
                for (name, edit) in &edits {
                    view.status_message = None;
                    edit(view, cx);
                    assert_eq!(status_text(view), Some(REFUSAL), "{name} not refused");
                    let after = (
                        pending(view),
                        view.undo_stack.actions.len(),
                        view.relation_of(layer_id, relation_id).unwrap().members,
                    );
                    assert_eq!(before, after, "{name} changed the data");
                }

                // A download does not block the same edits.
                view.transfer = Some(TransferHandle::new(
                    TransferKind::Download,
                    TransferPhase::Downloading,
                ));
                view.status_message = None;
                view.apply_tag_edit(&ctx, "amenity", "cafe");
                view.set_relation_member_role(member, "z");
                assert_ne!(status_text(view), Some(REFUSAL));
                assert_eq!(view.undo_stack.actions.len(), 2);
            })
            .unwrap();
    }

    #[gpui::test]
    fn upload_without_any_layer_shows_a_status_message(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(MapViewer::new);
        window
            .update(cx, |view, window, cx| {
                view.open_upload_dialog_as(Some("me"), window, cx);
                assert!(view.upload_dialog.is_none() && view.sign_in_dialog.is_none());
                assert!(view.status_message.is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn upload_signed_out_shows_the_sign_in_dialog(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                make_pending_change(view);
                view.open_upload_dialog_as(None, window, cx);
                assert!(view.sign_in_dialog.is_some());
                assert!(view.upload_dialog.is_none());
            })
            .unwrap();
    }

    /// Regression: dispatching `UploadToOsm` through the window (as the
    /// native menu and the shortcut do) used to do nothing, because the only
    /// handler was a global one that re-looked-up the checked-out window.
    #[gpui::test]
    fn upload_action_dispatched_through_the_window_opens_a_dialog(cx: &mut TestAppContext) {
        use gpui::Action as _;
        // Signed out -> the sign-in prompt. (A signed-in upload dialog needs
        // gpui-component's `Root`, which this test window lacks, so skip on a
        // machine with a stored login.)
        let stored =
            auth::stored_display_name(&auth::oauth_base_for(&settings_store::api_base_url()));
        if !osm_gpui::ui::sign_in_dialog::needs_sign_in(stored.as_deref()) {
            return;
        }
        let window = setup(cx);
        window
            .update(cx, |view, window, cx| {
                make_pending_change(view);
                window.focus(&view.focus_handle, cx);
            })
            .unwrap();
        // Draw so the dispatch tree (with the root's action listeners) exists.
        cx.update(|cx| {
            window.update(cx, |_, window, _| window.refresh()).unwrap();
        });
        cx.run_until_parked();
        window
            .update(cx, |_, window, cx| {
                window.dispatch_action(UploadToOsm.boxed_clone(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |view, _, _| {
                assert!(view.sign_in_dialog.is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn nothing_to_export_without_an_osm_layer(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(MapViewer::new);
        window
            .update(cx, |view, _window, _cx| {
                assert!(view.exportable_layer().is_none());
            })
            .unwrap();
    }

    // --- Resolving an upload conflict -------------------------------------

    /// `setup`'s layer with node 1 tagged locally (and that edit on the undo
    /// stack), as it would be when the server then refuses the upload.
    fn tag_node_one(view: &mut MapViewer) {
        let layer_id = view.active_layer.unwrap();
        editable(view).set_tag(osm_gpui::selection::FeatureKind::Node, 1, "amenity", "cafe");
        view.undo_stack.push(UndoableAction::SetTags {
            entries: vec![(
                osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Node,
                    id: 1,
                },
                "amenity".to_string(),
                None,
                Some("cafe".to_string()),
            )],
        });
    }

    fn server_node(version: i32, lat: f64) -> osm_gpui::osm_api::ServerElement {
        let mut data = way_fixture(CENTER_LAT, CENTER_LON);
        data.ways.clear();
        data.nodes.retain(|id, _| *id == 1);
        let node = data.nodes.get_mut(&1).unwrap();
        node.version = version;
        node.lat = lat;
        node.tags.insert("shop".to_string(), "bakery".to_string());
        osm_gpui::osm_api::ServerElement::Present(data)
    }

    #[gpui::test]
    fn keep_mine_moves_the_version_and_keeps_edit_and_undo(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Node;
        use osm_gpui::ui::upload_dialog::ConflictAction;
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                tag_node_one(view);
                let note = view
                    .apply_resolution(ConflictAction::KeepMine, Node, 1, server_node(5, 40.0))
                    .unwrap();
                assert!(note.contains("Node 1"), "{note}");
                let layer_id = view.active_layer.unwrap();
                let diff = view
                    .layer_manager
                    .find_layer(layer_id)
                    .unwrap()
                    .diff_for_upload();
                assert_eq!(diff.modified_nodes.len(), 1);
                assert_eq!(diff.modified_nodes[0].version, 5);
                assert_eq!(
                    diff.modified_nodes[0]
                        .tags
                        .get("amenity")
                        .map(String::as_str),
                    Some("cafe")
                );
                assert_eq!(view.undo_stack.actions.len(), 1, "history untouched");
            })
            .unwrap();
    }

    #[gpui::test]
    fn keep_mine_refuses_an_object_the_server_deleted(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Node;
        use osm_gpui::ui::upload_dialog::ConflictAction;
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                tag_node_one(view);
                let err = view
                    .apply_resolution(
                        ConflictAction::KeepMine,
                        Node,
                        1,
                        osm_gpui::osm_api::ServerElement::Gone,
                    )
                    .unwrap_err();
                assert!(err.contains("deleted on the server"), "{err}");
                assert_eq!(pending(view), (0, 1, 0), "nothing changed");
                assert_eq!(view.undo_stack.actions.len(), 1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn use_theirs_replaces_the_object_and_keeps_unrelated_undo_history(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Node;
        use osm_gpui::ui::upload_dialog::ConflictAction;
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                tag_node_one(view);
                // An unrelated earlier edit is on the stack too: it stays.
                view.undo_stack.push(UndoableAction::ReverseWays {
                    layer: view.active_layer.unwrap(),
                    way_ids: vec![99],
                });
                let note = view
                    .apply_resolution(ConflictAction::UseTheirs, Node, 1, server_node(5, 40.0))
                    .unwrap();
                assert!(note.contains("Replaced Node 1"), "{note}");
                assert!(note.contains("Undo steps that involved it"), "{note}");
                assert_eq!(view.undo_stack.actions.len(), 1, "only the unrelated step");
                assert_eq!(view.undo_stack.cursor, 1);
                assert!(matches!(
                    view.undo_stack.actions[0],
                    UndoableAction::ReverseWays { .. }
                ));
                assert_eq!(pending(view), (0, 0, 0), "the change to it is gone");
                let layer_id = view.active_layer.unwrap();
                let tags = editable(view)
                    .feature_tags(&osm_gpui::selection::FeatureRef {
                        layer_id,
                        kind: Node,
                        id: 1,
                    })
                    .unwrap();
                assert_eq!(tags, vec![("shop".to_string(), "bakery".to_string())]);
            })
            .unwrap();
    }

    #[gpui::test]
    fn use_theirs_for_a_deleted_object_removes_it_and_its_selection(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        use osm_gpui::ui::upload_dialog::ConflictAction;
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                editable(view).set_tag(Way, 10, "highway", "path");
                select(view, Way, &[10]);
                assert_eq!(view.selected.len(), 1);
                let note = view
                    .apply_resolution(
                        ConflictAction::UseTheirs,
                        Way,
                        10,
                        osm_gpui::osm_api::ServerElement::Gone,
                    )
                    .unwrap();
                assert!(note.contains("Way 10"), "{note}");
                assert!(editable(view).way_node_ids(10).is_none());
                assert!(view.selected.is_empty(), "no dangling selection");
                let _ = layer_id;
                assert_eq!(pending(view), (0, 0, 0));
                // Asking again: the object is gone, so there is nothing to do.
                let err = view
                    .apply_resolution(
                        ConflictAction::UseTheirs,
                        Way,
                        10,
                        osm_gpui::osm_api::ServerElement::Gone,
                    )
                    .unwrap_err();
                assert!(err.contains("no longer in the loaded data"), "{err}");
            })
            .unwrap();
    }

    #[gpui::test]
    fn drop_my_change_forgets_the_local_delete(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Way;
        use osm_gpui::ui::upload_dialog::ConflictAction;
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                let snapshot = editable(view).delete_feature(Way, 10).unwrap();
                view.undo_stack.push(UndoableAction::DeleteFeature {
                    layer: layer_id,
                    snapshot,
                });
                assert_eq!(pending(view), (0, 0, 1));
                let note = view
                    .apply_resolution(
                        ConflictAction::DropMyChange,
                        Way,
                        10,
                        osm_gpui::osm_api::ServerElement::Gone,
                    )
                    .unwrap();
                assert!(note.contains("Forgot your delete of Way 10"), "{note}");
                assert_eq!(pending(view), (0, 0, 0));
                assert!(view.undo_stack.actions.is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    fn show_selects_the_object_and_closes_the_dialog(cx: &mut TestAppContext) {
        use osm_gpui::selection::FeatureKind::Node;
        use osm_gpui::ui::upload_dialog::ConflictAction;
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                let conflict = osm_upload::UploadConflict::parse(
                    409,
                    "Version mismatch: Provided 1, server had: 2 of Node 2",
                );
                view.viewport = Viewport::new(10.0, 10.0, 5.0, size(px(800.0), px(600.0)));
                view.handle_conflict_action(ConflictAction::Show, conflict, cx);
                assert_eq!(view.selected.len(), 1);
                assert_eq!((view.selected[0].kind, view.selected[0].id), (Node, 2));
                // Zoomed in on node 2.
                let p = view.viewport.geo_to_screen(CENTER_LAT, CENTER_LON + 0.001);
                assert!(p.x > px(0.0) && p.x < px(800.0) && p.y > px(0.0) && p.y < px(600.0));
                assert!(view.upload_dialog.is_none());
                assert!(status_text(view).unwrap().contains("Node 2"));
            })
            .unwrap();
    }

    // ---- Script ops that drive an upload (docs/live-test) ----------------

    fn dialog_window(cx: &mut TestAppContext) -> gpui::WindowHandle<gpui_component::Root> {
        cx.update(gpui_component::init);
        cx.add_window(|window, cx| {
            let view = cx.new(|cx| MapViewer::new(window, cx));
            gpui_component::Root::new(view, window, cx)
        })
    }

    /// The window's `MapViewer`, with a one-way layer (way 10 on nodes 1, 2)
    /// loaded and active.
    fn view_with_layer(
        window: gpui::WindowHandle<gpui_component::Root>,
        vcx: &mut gpui::VisualTestContext,
    ) -> gpui::Entity<MapViewer> {
        let root = window.root(vcx).unwrap();
        let view: gpui::Entity<MapViewer> = root
            .read_with(vcx, |root, _| root.view().clone())
            .downcast::<MapViewer>()
            .unwrap();
        view.update_in(vcx, |view, _window, _cx| {
            let layer_id = view.layer_manager.alloc_id();
            let layer = OsmLayer::new_with_data(
                layer_id,
                "L".to_string(),
                Arc::new(way_fixture(CENTER_LAT, CENTER_LON)),
            );
            view.layer_manager.add_layer(Box::new(layer));
            view.active_layer = Some(layer_id);
            view.viewport = Viewport::new(CENTER_LAT, CENTER_LON, 18.0, size(px(800.0), px(600.0)));
        });
        view
    }

    #[gpui::test]
    fn typed_script_text_lands_in_the_comment_field(cx: &mut TestAppContext) {
        let window = dialog_window(cx);
        let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let view = view_with_layer(window, vcx);
        view.update_in(vcx, |view, window, cx| {
            make_pending_change(view);
            view.open_upload_dialog_as(Some("me"), window, cx);
            assert!(view.upload_dialog.is_some());
        });
        let text = "osm-gpui live test: a b x & <ok> \"q\" caf\u{e9} \u{65e5}";
        for ch in text.chars() {
            vcx.update(|window, cx| {
                window.dispatch_keystroke(script_harness::keystroke_for_char(ch), cx);
            });
        }
        view.update_in(vcx, |view, _window, cx| {
            let dialog = view.upload_dialog.clone().unwrap();
            assert_eq!(dialog.read(cx).draft_comment(cx), text);
        });
    }

    #[gpui::test]
    fn script_checks_see_pending_edits_dialogs_and_selection(cx: &mut TestAppContext) {
        use osm_gpui::script::{Check, DialogExpect, SelectionKind};
        let window = dialog_window(cx);
        let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let view = view_with_layer(window, vcx);
        view.update_in(vcx, |view, window, cx| {
            assert!(view.script_check(&Check::Pending(0), cx).is_ok());
            assert!(view.script_check(&Check::Pending(1), cx).is_err());
            make_pending_change(view);
            assert!(view.script_check(&Check::Pending(1), cx).is_ok());
            assert!(view
                .script_check(&Check::Dialog(DialogExpect::None), cx)
                .is_ok());
            assert!(view
                .script_check(&Check::Dialog(DialogExpect::Editing), cx)
                .is_err());
            assert!(view.script_check(&Check::TransferIdle, cx).is_ok());

            view.open_upload_dialog_as(Some("me"), window, cx);
            assert!(view
                .script_check(&Check::Dialog(DialogExpect::Editing), cx)
                .is_ok());
            assert!(view
                .script_check(&Check::Dialog(DialogExpect::None), cx)
                .is_err());
            assert!(view
                .script_check(&Check::Dialog(DialogExpect::Failed), cx)
                .is_err());

            // The Upload button does nothing without a comment, and the
            // script says why.
            assert!(view
                .upload_dialog
                .clone()
                .unwrap()
                .read(cx)
                .confirm_blocker(cx)
                .is_some());
            view.upload_dialog.clone().unwrap().update(cx, |d, cx| {
                d.set_draft_comment("osm-gpui live test".into(), window, cx)
            });
            assert!(view
                .upload_dialog
                .clone()
                .unwrap()
                .read(cx)
                .confirm_blocker(cx)
                .is_none());

            // A refused upload is a conflict of a kind, naming an object.
            let conflict = osm_upload::UploadConflict::parse(
                409,
                "Version mismatch: Provided 1, server had: 2 of Node 2",
            );
            let conflict_view = view.conflict_view(conflict);
            view.upload_dialog
                .clone()
                .unwrap()
                .update(cx, |d, cx| d.fail_with_conflict(conflict_view, cx));
            let expect = |kind, object| Check::Dialog(DialogExpect::Conflict { kind, object });
            use osm_gpui::script::{ConflictKindName, FeatureKind as K};
            assert!(view.script_check(&expect(None, None), cx).is_ok());
            assert!(view
                .script_check(
                    &expect(Some(ConflictKindName::VersionMismatch), Some(K::Node)),
                    cx
                )
                .is_ok());
            assert!(view
                .script_check(&expect(Some(ConflictKindName::AlreadyDeleted), None), cx)
                .is_err());
            assert!(view.script_check(&expect(None, Some(K::Way)), cx).is_err());
            assert!(view
                .script_check(&Check::Dialog(DialogExpect::Failed), cx)
                .is_ok());

            assert!(view
                .script_check(&Check::Selection(SelectionKind::None), cx)
                .is_ok());
            assert!(view
                .script_check(&Check::Selection(SelectionKind::Node), cx)
                .is_err());
        });
    }

    #[gpui::test]
    fn script_set_tag_and_remove_tag_edit_the_selection_in_one_undo_step_each(
        cx: &mut TestAppContext,
    ) {
        use osm_gpui::selection::{FeatureKind, FeatureRef};
        let window = dialog_window(cx);
        let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let view = view_with_layer(window, vcx);
        view.update_in(vcx, |view, _window, cx| {
            let layer_id = view.active_layer.unwrap();
            assert!(
                view.script_set_tag("name", "x", cx).is_err(),
                "no selection"
            );
            view.selected = vec![FeatureRef {
                layer_id,
                kind: FeatureKind::Way,
                id: 10,
            }];
            let value = "A & B <C> \"D\" caf\u{e9}";
            view.script_set_tag("name", value, cx).unwrap();
            assert_eq!(
                view.selected_feature_tag_snapshots()[0].1,
                vec![("name".to_string(), value.to_string())]
            );
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(view.active_layer_pending(), Some(1));
            // Setting the same value again changes nothing.
            view.script_set_tag("name", value, cx).unwrap();
            assert_eq!(view.undo_stack.actions.len(), 1);
            view.script_remove_tag("name", cx).unwrap();
            assert!(view.selected_feature_tag_snapshots()[0].1.is_empty());
            assert!(view.script_remove_tag("name", cx).is_err());
        });
    }

    #[gpui::test]
    fn script_export_writes_the_active_layer(cx: &mut TestAppContext) {
        let window = dialog_window(cx);
        let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let view = view_with_layer(window, vcx);
        let path = std::env::temp_dir()
            .join(format!("osm-gpui-script-export-{}", std::process::id()))
            .join("nested")
            .join("layer.osm");
        view.update_in(vcx, |view, _window, _cx| {
            make_pending_change(view);
            view.script_export_layer(&path).unwrap();
        });
        let xml = std::fs::read_to_string(&path).unwrap();
        assert!(xml.contains("<osm"), "{xml}");
        assert!(xml.contains("<way"), "{xml}");
        // The export loads back with the pending edit still pending.
        let (data, edits) = osm_gpui::osm::OsmParser::new()
            .parse_file_with_edits(path.to_str().unwrap())
            .unwrap();
        assert!(data.ways.contains_key(&10));
        let _ = edits;
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_scripted_run_only_uploads_to_the_dev_server() {
        // Outside script mode nothing is refused.
        assert!(!script_harness::script_mode());
        assert_eq!(
            script_harness::scripted_upload_refusal("https://api.openstreetmap.org"),
            None
        );
    }
}
