//! Parsed script operations.

use crate::edit_mode::EditMode;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2 {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseButton {
    Left,
    Right,
}

/// Modifier keys, as given inline on a pointer op (`alt=true`) or held with
/// `mod_down`. An event is sent with both combined.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub cmd: bool,
}

impl Mods {
    pub fn is_empty(&self) -> bool {
        !(self.alt || self.ctrl || self.shift || self.cmd)
    }

    /// Every modifier held in either.
    pub fn union(self, other: Mods) -> Mods {
        Mods {
            alt: self.alt || other.alt,
            ctrl: self.ctrl || other.ctrl,
            shift: self.shift || other.shift,
            cmd: self.cmd || other.cmd,
        }
    }

    /// `self` without the modifiers set in `other`.
    pub fn without(self, other: Mods) -> Mods {
        Mods {
            alt: self.alt && !other.alt,
            ctrl: self.ctrl && !other.ctrl,
            shift: self.shift && !other.shift,
            cmd: self.cmd && !other.cmd,
        }
    }
}

/// Kept for scripts and tests written when only `alt` and `cmd` could be held.
pub type DragMods = Mods;

/// Where a pointer op acts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Where {
    /// Window pixels (`X,Y`), as the other ops use them.
    Window(Point2),
    /// Pixels in the map area, i.e. window pixels minus the toolbar and any
    /// in-window menu bar (`map:X,Y`).
    Map(Point2),
    /// A coordinate, put on screen by the current viewport (`geo:LAT,LON`).
    Geo { lat: f64, lon: f64 },
}

/// What a snap preview is targeting, for `assert_snap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapExpect {
    /// No highlight.
    None,
    /// An existing node.
    Node(i64),
    /// Segment `segment` (0 is the one between a way's first two nodes).
    Way { way: i64, segment: usize },
}

/// A kind of pointer event for the split-up drag and hover ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    /// Move with no button held.
    Hover,
    /// Press the left button.
    Press,
    /// Move with the left button held.
    HeldMove,
    /// Release the left button.
    Release,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chord {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    Node,
    Way,
    Relation,
}

/// How a refused upload is named in `assert_dialog conflict KIND`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKindName {
    VersionMismatch,
    AlreadyDeleted,
    StillInUse,
    MissingDependency,
    Other,
}

/// The way forward to press on a conflict dialog (`resolve_conflict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    KeepMine,
    UseTheirs,
    DropMine,
    Show,
}

/// What `assert_dialog` expects to be on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogExpect {
    /// No dialog at all.
    None,
    /// The upload dialog, ready for a comment and the Upload button.
    Editing,
    /// The upload dialog while an upload is running.
    InFlight,
    /// The upload dialog showing an error (any kind).
    Failed,
    /// The upload dialog showing a refused upload: optionally of this kind
    /// and naming this kind of object.
    Conflict {
        kind: Option<ConflictKindName>,
        object: Option<FeatureKind>,
    },
    /// The "sign in to upload" prompt.
    SignIn,
    /// The "can't do that" notice (e.g. an upload refused for a size limit).
    Notice,
    /// The "delete this layer and its unsaved edits?" confirmation.
    DeleteLayer,
    /// The startup offer to restore autosaved layers.
    Restore,
    /// The Edit > Find dialog.
    Find,
    /// The object history view.
    History,
}

/// What the object history script ops ask the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryCmd {
    /// Open the view for the selected object.
    Show,
    /// Answer the next request from this file.
    LoadFile(std::path::PathBuf),
}

/// What the autosave script ops ask the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutosaveCmd {
    /// Write every layer's autosave now.
    Now,
    /// Press Restore in the restore dialog.
    Restore,
    /// Press Discard in the restore dialog.
    Discard,
    /// Exit the process on the spot, without any clean-up, as a crash would.
    Crash,
}

/// What the upload dialog's changeset-extras script ops ask it to do. None of
/// them can start an upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadDialogCmd {
    /// Open the dialog as a preview: no sign-in needed, Upload never works.
    OpenPreview,
    /// Replace the Sources text.
    SetSource(String),
    /// Press a quick-add source button.
    PressSourceChip(String),
    /// Tick or untick the review request.
    SetReviewRequested(bool),
    /// Leave `imagery_used` out of this upload (`true`) or put it back.
    SetImageryCleared(bool),
    /// Put a comment on the in-memory recent-comments list (never saved).
    AddRecentComment(String),
    /// Open or close the recent-comments list.
    ToggleRecentComments,
    /// Pick the Nth recent comment (0 is the newest).
    PickRecentComment(usize),
}

/// Which validator finding `show_finding` clicks in the side panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingTarget {
    /// The Nth finding in the list (0 is the first row).
    Index(usize),
    /// The first finding of the check with this id (e.g. `crossing_ways`).
    Check(String),
}

/// A collapsible side panel section a script can open or close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelSection {
    Validation,
    History,
}

/// What `assert_selection` expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    None,
    Node,
    Way,
    Relation,
    /// More than one feature.
    Multiple,
}

/// What `assert_native_appearance` expects the native app appearance (title
/// bar and other system chrome) to be forced to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAppearanceWant {
    /// Not forced: following the OS.
    System,
    Light,
    Dark,
}

/// The kind of a layer, for `assert_layer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    Gpx,
    GeoJson,
    /// An OSM data layer.
    Osm,
    Imagery,
    Grid,
}

/// A read-only condition about the running app, evaluated on its main thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// A layer of this kind and exact name exists.
    Layer {
        kind: LayerKind,
        name: String,
    },
    /// Exactly one node is selected, within `tolerance_m` metres of the point.
    SelectedAt {
        lat: f64,
        lon: f64,
        tolerance_m: f64,
    },
    /// Pending (created + modified + deleted) edits in the active layer.
    Pending(usize),
    /// The status bar's message contains the text.
    StatusContains(String),
    /// What the snap highlight is targeting.
    Snap(SnapExpect),
    /// A way's nodes, in order.
    WayNodes {
        way: i64,
        nodes: Vec<i64>,
    },
    /// The ways that use a node are exactly these (in any order).
    NodeShared {
        node: i64,
        ways: Vec<i64>,
    },
    Dialog(DialogExpect),
    /// The side panel's Validation section is present (`true`), expanded or
    /// collapsed, or not.
    ValidationPanel(bool),
    /// A side panel section is expanded (`true`) or collapsed (`false`).
    PanelOpen(PanelSection, bool),
    /// What the validator finds in the pending changes: the number of errors
    /// and warnings (when given) and of findings per check id. Read from the
    /// Validation section while it is showing (so this also tests that it
    /// refreshes by itself), otherwise from a fresh run.
    Validation {
        errors: Option<usize>,
        warnings: Option<usize>,
        checks: Vec<(String, usize)>,
    },
    Selection(SelectionKind),
    NativeAppearance(NativeAppearanceWant),
    /// Exactly this many features are selected.
    SelectedCount(usize),
    /// Exactly this many layers are on the map (imagery and data).
    LayerCount(usize),
    /// The open Find dialog's match-count line contains the text.
    FindPreview(String),
    /// No download or upload is running and the upload dialog is not busy.
    TransferIdle,
    /// Every layer's autosave is up to date and no autosave write or
    /// restore is running.
    AutosaveIdle,
    /// This run's autosave directory holds a file for at least one layer
    /// (`None`) or for exactly this many.
    AutosaveFiles(Option<usize>),
    /// The map zoom level is `level`, within `tolerance`.
    Zoom {
        level: f64,
        tolerance: f64,
    },
    /// The imagery offset (of the layer being adjusted, else the topmost
    /// visible imagery layer) is `east`/`north` metres, within `tolerance`.
    ImageryOffset {
        east: f64,
        north: f64,
        tolerance: f64,
    },
    /// Whether the Adjust imagery offset mode is active.
    AdjustingOffset(bool),
    /// The object history view is open and lists exactly this many versions.
    HistoryVersions(usize),
    /// The object history view is open and no longer loading (loaded or
    /// failed).
    HistorySettled,
    /// A condition about OSM notes (see `note_ops`).
    Note(super::note_ops::NoteCheck),
    /// The tags the upload dialog would write now include `key` with
    /// `value` (`Some`) or do not include `key` at all (`None`).
    ChangesetTag {
        key: String,
        value: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Window {
        w: u32,
        h: u32,
    },
    Viewport {
        lat: f64,
        lon: f64,
        zoom: f32,
    },
    WaitIdle {
        timeout: Duration,
    },
    Wait {
        duration: Duration,
    },
    Drag {
        from: Where,
        to: Where,
        duration: Duration,
        /// Drag with the left button (a press-drag-release gesture handled by
        /// the current edit mode) instead of the default map pan.
        left: bool,
        /// Modifier keys held for the whole left-button drag.
        mods: Mods,
    },
    Click {
        at: Where,
        button: MouseButton,
        /// Number of clicks in the sequence (2 = double-click). Mirrors
        /// `MouseUpEvent::click_count`, which `handle_select_click` reads to
        /// decide whether to fall back to interior hit-testing.
        count: u8,
        mods: Mods,
    },
    /// One pointer event: `hover`, `drag_begin`, `drag_move`, `drag_end`.
    /// `at` is `None` for `drag_end` without a target (release where the
    /// pointer is).
    Pointer {
        kind: PointerKind,
        at: Option<Where>,
        mods: Mods,
    },
    /// `mod_down` / `mod_up`: press or release modifier keys by themselves,
    /// like holding Alt without touching the mouse. They stay held, for the
    /// pointer ops that follow, until released.
    Modifiers {
        mods: Mods,
        down: bool,
    },
    Scroll {
        at: Point2,
        dx: f32,
        dy: f32,
    },
    Key {
        chord: Chord,
    },
    /// Dispatch a named action (e.g. `UploadToOsm`) to the window the way a
    /// native menu item does, which the harness cannot click.
    Action {
        name: String,
    },
    Capture {
        path: String,
    },
    Log {
        message: String,
    },
    /// Print and clear the per-phase timing statistics collected since the
    /// last report (only when `OSM_GPUI_FRAME_STATS=1`), tagged with `label`.
    FrameStats {
        label: String,
    },
    LoadOsm {
        path: String,
    },
    /// `load_file PATH`: open a file the way File > Open does, routed by type
    /// (`.osm`, `.gpx`, `.geojson`/`.json`).
    LoadFile {
        path: String,
    },
    /// `assert_layer KIND NAME`.
    AssertLayer {
        kind: LayerKind,
        name: String,
    },
    /// `assert_selected_at LAT LON [tol=METRES]`.
    AssertSelectedAt {
        lat: f64,
        lon: f64,
        tolerance_m: f64,
    },
    AssertMode {
        mode: EditMode,
    },
    /// Assert the current selection: `Some((kind, id))` for exactly that
    /// single feature selected, `None` for an empty selection.
    AssertSelected {
        feature: Option<(FeatureKind, i64)>,
    },
    /// Type text into the focused input, one character at a time.
    Type {
        text: String,
    },
    /// Set a tag on the selected feature(s) the way the side panel's Add tag
    /// row does (one undo step; replaces an existing value).
    SetTag {
        key: String,
        value: String,
    },
    /// Remove a tag from the selected feature(s) (the side panel's delete).
    RemoveTag {
        key: String,
    },
    /// Press the upload dialog's Upload (or Try again) button. Only runs
    /// against the OSM development server.
    UploadConfirm,
    /// Replace the upload dialog's changeset comment with `text` (unlike
    /// `type`, which appends to whatever is in the field).
    SetComment {
        text: String,
    },
    /// One of the upload dialog's changeset-extras ops (see `UploadDialogCmd`).
    UploadDialog(UploadDialogCmd),
    /// Assert a changeset tag the upload dialog would write now
    /// (`assert_changeset_tag KEY VALUE`), or that it would not write `key`
    /// (`assert_no_changeset_tag KEY`, `value` is `None`). Nothing is sent.
    AssertChangesetTag {
        key: String,
        value: Option<String>,
    },
    /// Press the upload dialog's Cancel button.
    UploadCancel,
    /// Press the delete-layer confirmation's destructive "Delete Layer" button.
    ConfirmDeleteLayer,
    /// Press the delete-layer confirmation's Cancel button.
    CancelDeleteLayer,
    /// Press one of the conflict dialog's buttons.
    ResolveConflict {
        choice: ConflictChoice,
    },
    /// Wait until no transfer is running (success or failure).
    WaitTransfer {
        timeout: Duration,
    },
    /// While a transfer runs, capture `PREFIX-NN.png` every `every`.
    CaptureWhileTransferring {
        prefix: String,
        every: Duration,
        timeout: Duration,
    },
    AssertPending {
        count: usize,
    },
    AssertStatusContains {
        text: String,
    },
    /// `assert_snap none|node ID|way ID segment N`: what the snap highlight
    /// is targeting now.
    AssertSnap {
        expect: SnapExpect,
    },
    /// `assert_way_nodes WAY ID...`: the way's nodes, in order.
    AssertWayNodes {
        way: i64,
        nodes: Vec<i64>,
    },
    /// `assert_node_shared NODE [WAY...]`: exactly these ways use the node.
    AssertNodeShared {
        node: i64,
        ways: Vec<i64>,
    },
    /// `assert_zoom LEVEL [tol=N]`.
    AssertZoom {
        level: f64,
        tolerance: f64,
    },
    /// `assert_imagery_offset EAST NORTH [tol=METRES]`.
    AssertImageryOffset {
        east: f64,
        north: f64,
        tolerance: f64,
    },
    /// `assert_adjusting_offset on|off`.
    AssertAdjustingOffset {
        on: bool,
    },
    AssertDialog {
        expect: DialogExpect,
    },
    AssertSelection {
        kind: SelectionKind,
    },
    /// Run the validator and compare its findings with what is expected
    /// (see `Check::Validation`).
    AssertValidation {
        errors: Option<usize>,
        warnings: Option<usize>,
        checks: Vec<(String, usize)>,
    },
    /// Click a finding's row in the side panel's Validation section.
    ShowFinding {
        target: FindingTarget,
    },
    /// Assert the Validation section is present (expanded or collapsed) or
    /// hidden.
    AssertValidationPanel {
        shown: bool,
    },
    /// Expand or collapse a side panel section (what a click on its header
    /// does).
    SetPanelOpen {
        section: PanelSection,
        open: bool,
    },
    /// Assert a side panel section is expanded or collapsed.
    AssertPanelOpen {
        section: PanelSection,
        open: bool,
    },
    /// Assert the native app appearance (as read back from the OS after the
    /// app last set it) is forced to dark or light, or not forced (system).
    AssertNativeAppearance {
        expect: NativeAppearanceWant,
    },
    /// Assert exactly `count` features are selected.
    AssertSelectedCount {
        count: usize,
    },
    /// Assert exactly `count` layers are on the map.
    AssertLayerCount {
        count: usize,
    },
    /// Assert the Find dialog's match-count line contains `text`.
    AssertFindPreview {
        text: String,
    },
    /// Write the active layer (with its pending edits) to a `.osm` file, as
    /// File > Export does, without the native save dialog.
    ExportLayer {
        path: String,
    },
    /// Write every layer's autosave now and wait until it is on disk. Needs
    /// an autosave directory (`--autosave-dir`).
    AutosaveNow,
    /// Assert this run's autosave directory holds a layer file (any, or
    /// exactly `count`).
    AssertAutosaveExists {
        count: Option<usize>,
    },
    /// Press Restore in the restore dialog and wait for the layers to load.
    RestoreAutosave,
    /// Press Discard in the restore dialog.
    DiscardAutosave,
    /// Exit at once without cleaning up, as a crash would (so the next run
    /// finds an unclean session). Scripted runs only.
    Crash,
    /// Open the object history view for the selected object and wait for
    /// its versions to arrive (or fail).
    ShowHistory,
    /// Script-only: answer the next object history request from this local
    /// `/history` document instead of the server.
    LoadHistoryFile {
        path: String,
    },
    /// Assert the open object history view lists exactly `count` versions.
    AssertHistoryVersions {
        count: usize,
    },
    /// Start the browser sign-in for the active API server.
    SignIn,
    /// Wait for the sign-in to finish (someone approves it in the browser).
    WaitSignedIn {
        timeout: Duration,
    },
    AssertSignedIn,
    /// Assert the active API server is the OSM development server (exact
    /// host match). Scripts start with this so a forgotten `--api-server dev`
    /// stops them before they draw anything.
    AssertServerDev,
    /// Show `message` in the status bar and wait, so a person can follow.
    Pause {
        duration: Duration,
        message: String,
    },
    /// An OSM notes op (`show_notes`, `open_note`, `note_close`, ...).
    Note(super::note_ops::NoteOp),
    /// An assertion about OSM notes (`assert_notes`, ...).
    AssertNote(super::note_ops::NoteCheck),
}

impl Op {
    /// Whether running this op can write to the OSM server (an upload, a
    /// conflict resolution, signing in to upload, a note write). The runner
    /// refuses these unless the active server is the development server. The
    /// match has no wildcard so a new op must be classified here.
    pub fn writes_to_server(&self) -> bool {
        match self {
            Op::UploadConfirm | Op::ResolveConflict { .. } | Op::SignIn => true,
            Op::Note(note) => note.is_write(),
            Op::Window { .. }
            | Op::Viewport { .. }
            | Op::WaitIdle { .. }
            | Op::Wait { .. }
            | Op::Drag { .. }
            | Op::Click { .. }
            | Op::Pointer { .. }
            | Op::Modifiers { .. }
            | Op::Scroll { .. }
            | Op::Key { .. }
            | Op::Action { .. }
            | Op::Capture { .. }
            | Op::Log { .. }
            | Op::FrameStats { .. }
            | Op::LoadOsm { .. }
            | Op::LoadFile { .. }
            | Op::AssertLayer { .. }
            | Op::AssertSelectedAt { .. }
            | Op::AssertMode { .. }
            | Op::AssertSelected { .. }
            | Op::Type { .. }
            | Op::SetTag { .. }
            | Op::RemoveTag { .. }
            | Op::SetComment { .. }
            | Op::UploadDialog(..)
            | Op::AssertChangesetTag { .. }
            | Op::UploadCancel
            | Op::ConfirmDeleteLayer
            | Op::CancelDeleteLayer
            | Op::WaitTransfer { .. }
            | Op::CaptureWhileTransferring { .. }
            | Op::AssertPending { .. }
            | Op::AssertStatusContains { .. }
            | Op::AssertSnap { .. }
            | Op::AssertWayNodes { .. }
            | Op::AssertNodeShared { .. }
            | Op::AssertZoom { .. }
            | Op::AssertImageryOffset { .. }
            | Op::AssertAdjustingOffset { .. }
            | Op::AssertDialog { .. }
            | Op::AssertSelection { .. }
            | Op::AssertValidation { .. }
            | Op::ShowFinding { .. }
            | Op::AssertValidationPanel { .. }
            | Op::SetPanelOpen { .. }
            | Op::AssertPanelOpen { .. }
            | Op::AssertNativeAppearance { .. }
            | Op::AssertSelectedCount { .. }
            | Op::AssertLayerCount { .. }
            | Op::AssertFindPreview { .. }
            | Op::ExportLayer { .. }
            | Op::AutosaveNow
            | Op::AssertAutosaveExists { .. }
            | Op::RestoreAutosave
            | Op::DiscardAutosave
            | Op::Crash
            | Op::ShowHistory
            | Op::LoadHistoryFile { .. }
            | Op::AssertHistoryVersions { .. }
            | Op::WaitSignedIn { .. }
            | Op::AssertSignedIn
            | Op::AssertServerDev
            | Op::Pause { .. }
            | Op::AssertNote(..) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub line_no: usize,
    pub op: Op,
}
