//! Parsed script operations.

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

/// Modifier keys held during a left-button `drag`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DragMods {
    pub alt: bool,
    pub cmd: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chord {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EditMode {
    Select,
    Add,
    Building,
    Extrude,
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
    /// The Tools > Validate dialog.
    Validate,
    /// The Edit > Find dialog.
    Find,
}

/// Which validator finding `show_finding` presses Show on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingTarget {
    /// The Nth finding in the list (0 is the first row).
    Index(usize),
    /// The first finding of the check with this id (e.g. `crossing_ways`).
    Check(String),
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

/// A read-only condition about the running app, evaluated on its main thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// Pending (created + modified + deleted) edits in the active layer.
    Pending(usize),
    /// The status bar's message contains the text.
    StatusContains(String),
    Dialog(DialogExpect),
    /// What the validator finds in the pending changes right now: the number
    /// of errors and warnings (when given) and of findings per check id.
    Validation {
        errors: Option<usize>,
        warnings: Option<usize>,
        checks: Vec<(String, usize)>,
    },
    Selection(SelectionKind),
    /// Exactly this many features are selected.
    SelectedCount(usize),
    /// The open Find dialog's match-count line contains the text.
    FindPreview(String),
    /// No download or upload is running and the upload dialog is not busy.
    TransferIdle,
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
        from: Point2,
        to: Point2,
        duration: Duration,
        /// Drag with the left button (a press-drag-release gesture handled by
        /// the current edit mode) instead of the default map pan.
        left: bool,
        /// Modifier keys held for the whole left-button drag.
        mods: DragMods,
    },
    Click {
        at: Point2,
        button: MouseButton,
        /// Number of clicks in the sequence (2 = double-click). Mirrors
        /// `MouseUpEvent::click_count`, which `handle_select_click` reads to
        /// decide whether to fall back to interior hit-testing.
        count: u8,
        ctrl: bool,
    },
    Scroll {
        at: Point2,
        dx: f32,
        dy: f32,
    },
    /// Move the mouse to `at` with no button held.
    Hover {
        at: Point2,
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
    /// Press Show on a finding of the open Validate (or upload) dialog.
    ShowFinding {
        target: FindingTarget,
    },
    /// Assert exactly `count` features are selected.
    AssertSelectedCount {
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub line_no: usize,
    pub op: Op,
}
