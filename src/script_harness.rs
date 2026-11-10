//! Cross-thread scripting plumbing: the script runner runs on a background
//! thread and drives the live app on the gpui main thread via `ScriptBus`.

use gpui::{
    point, px, Context, Keystroke, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollDelta,
    ScrollWheelEvent, Window,
};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use osm_gpui::idle_tracker::IdleTracker;
use osm_gpui::layers::tile_layer::TileLayer;
use osm_gpui::osm::{OsmData, OsmParser};
use osm_gpui::script::{self, runner::AppHandle};
use osm_gpui::settings_store;
use osm_gpui::ui::upload_dialog::Phase;

use crate::MapViewer;

// Set to true while a script runner thread is active
pub(crate) static SCRIPT_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// True for the whole process when it was launched with `--script`. Set in
/// `main` before the window exists (unlike `SCRIPT_ACTIVE`, which the
/// runner thread flips on a little later and off again when it finishes),
/// so window setup can ask it. Scripted runs lay the window out from
/// defaults rather than the user's saved preferences, so that click
/// coordinates and captures are the same on every machine.
pub(crate) static SCRIPT_MODE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) fn script_mode() -> bool {
    SCRIPT_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

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
pub(crate) enum ScriptCommand {
    /// pan_to + set_zoom + ensure tile layer
    SetViewport { lat: f64, lon: f64, zoom: f64 },
    /// Resize the window
    SetWindowSize { w: u32, h: u32 },
    /// Synthesize a pan-drag of the map (from → to)
    Drag {
        from: (f32, f32),
        to: (f32, f32),
        left: bool,
        mods: script::DragMods,
    },
    /// Synthesize a mouse click
    Click {
        x: f32,
        y: f32,
        right: bool,
        count: u8,
        ctrl: bool,
    },
    /// Synthesize a scroll event
    Scroll { x: f32, y: f32, dx: f32, dy: f32 },
    /// Synthesize a mouse move with no button held
    Hover { x: f32, y: f32 },
    /// Render the current frame in-process and save it as a PNG at `path`
    Capture { path: std::path::PathBuf },
    /// Add a parsed OSM dataset as a new layer (parsing itself happens on
    /// the runner thread, before this is submitted — see `load_osm` below).
    /// The data and the file's recorded pending edits are boxed to keep
    /// this enum small.
    LoadOsm {
        name: String,
        loaded: Box<(OsmData, osm_gpui::osm::FileEdits)>,
    },
    /// Dispatch the named action (bare name in the `osm_gpui` namespace,
    /// e.g. `UploadToOsm`) the way a native menu item does.
    Action { name: String },
    /// Compare `MapViewer.mode` against `want`.
    AssertMode(crate::EditMode),
    /// Compare `MapViewer.selected` against `want`.
    AssertSelected(Option<(osm_gpui::script::FeatureKind, i64)>),
    /// Set a tag on the selection (the side panel's Add tag path).
    SetTag { key: String, value: String },
    /// Remove a tag from the selection.
    RemoveTag { key: String },
    /// Press the upload dialog's Upload / Try again button.
    UploadConfirm,
    /// Replace the upload dialog's changeset comment.
    SetComment(String),
    /// Press the upload dialog's Cancel button.
    UploadCancel,
    /// Press one of the conflict dialog's buttons.
    Resolve(script::ConflictChoice),
    /// Write the active layer to this `.osm` path.
    Export { path: std::path::PathBuf },
    /// Evaluate a read-only condition.
    Check(script::Check),
    /// Show a message in the status bar.
    Note(String),
}

/// Shared state between the script-runner thread and the gpui main thread.
pub(crate) struct ScriptBus {
    /// Pending command for this frame. None when idle.
    pending: Mutex<Option<ScriptCommand>>,
    /// Signalled by the main thread when it has processed a pending command.
    done_cv: Condvar,
    /// Counts how many frames have been rendered (monotonically increasing).
    frame_count: Mutex<u64>,
    /// Signalled each time a frame is rendered.
    frame_cv: Condvar,
    /// Result of the most recently processed `Capture` command.
    capture_result: Mutex<Option<Result<(), String>>>,
    /// Result of the most recently processed `AssertMode`/`AssertSelected`
    /// command. Safe to share a single slot: `submit` blocks until the
    /// pending command is consumed, so only one assertion is ever in flight.
    assert_result: Mutex<Option<Result<(), String>>>,
}

impl ScriptBus {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(None),
            done_cv: Condvar::new(),
            frame_count: Mutex::new(0),
            frame_cv: Condvar::new(),
            capture_result: Mutex::new(None),
            assert_result: Mutex::new(None),
        })
    }

    /// Submit a command and block until the main thread has processed it.
    fn submit(&self, cmd: ScriptCommand) {
        {
            let mut lock = self.pending.lock().unwrap();
            *lock = Some(cmd);
        }
        // Wait until the command is consumed.
        let _guard = self
            .done_cv
            .wait_while(self.pending.lock().unwrap(), |opt| opt.is_some())
            .unwrap();
    }

    /// Wait until at least one more render frame has completed.
    fn wait_frame(&self) {
        let current = *self.frame_count.lock().unwrap();
        let _guard = self
            .frame_cv
            .wait_while(self.frame_count.lock().unwrap(), |fc| *fc <= current)
            .unwrap();
    }

    /// Called by MapViewer::render to drain and process the pending command.
    /// Returns the command if any was pending (caller processes it).
    fn take_pending(&self) -> Option<ScriptCommand> {
        self.pending.lock().unwrap().take()
    }

    /// Called by MapViewer::render after processing a command (or if no command).
    pub(crate) fn signal_done_and_frame(&self) {
        self.done_cv.notify_all();
        let mut fc = self.frame_count.lock().unwrap();
        *fc += 1;
        self.frame_cv.notify_all();
    }

    /// Called by MapViewer::render when handling a `Capture` command, before
    /// `signal_done_and_frame` wakes the waiting runner thread.
    fn set_capture_result(&self, result: Result<(), String>) {
        *self.capture_result.lock().unwrap() = Some(result);
    }

    /// Called by the runner thread after `submit(Capture { .. })` returns.
    fn take_capture_result(&self) -> Result<(), String> {
        self.capture_result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Err("capture: no result recorded".to_string()))
    }

    /// Called by MapViewer::render when handling an `AssertMode`/
    /// `AssertSelected` command, before `signal_done_and_frame` wakes the
    /// waiting runner thread.
    fn set_assert_result(&self, result: Result<(), String>) {
        *self.assert_result.lock().unwrap() = Some(result);
    }

    /// Called by the runner thread after `submit(AssertMode(..))` or
    /// `submit(AssertSelected(..))` returns.
    fn take_assert_result(&self) -> Result<(), String> {
        self.assert_result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Err("assert: no result recorded".to_string()))
    }
}

pub(crate) static SCRIPT_BUS: std::sync::OnceLock<Arc<ScriptBus>> = std::sync::OnceLock::new();

// Keystroke commands need a separate queue since gpui `Keystroke` is not Send-safe
// (it only contains Strings, Modifiers — actually it IS Send). Let's use a simple
// OnceLock queue for keystrokes.
pub(crate) static KEYSTROKE_QUEUE: std::sync::OnceLock<Arc<Mutex<Vec<Keystroke>>>> =
    std::sync::OnceLock::new();

impl MapViewer {
    /// Process any pending script command from the background runner thread.
    /// Called at the start of each render frame.
    pub(crate) fn process_script_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bus) = SCRIPT_BUS.get() else { return };

        let cmd = bus.take_pending();

        if let Some(cmd) = cmd {
            match cmd {
                ScriptCommand::SetViewport { lat, lon, zoom } => {
                    self.viewport.pan_to(lat, lon);
                    self.viewport.set_zoom(zoom);
                    // Ensure tile layer exists
                    if self
                        .layer_manager
                        .layer_named("OpenStreetMap Carto")
                        .is_none()
                    {
                        let layer_id = self.layer_manager.alloc_id();
                        let tile_layer = TileLayer::new(layer_id, self.tile_cache.clone());
                        self.layer_manager.add_layer(Box::new(tile_layer));
                    }
                    cx.notify();
                }
                ScriptCommand::SetWindowSize { w, h } => {
                    window.resize(gpui::size(px(w as f32), px(h as f32)));
                    cx.notify();
                }
                ScriptCommand::Drag {
                    from,
                    to,
                    left: true,
                    mods,
                } => {
                    // A left-button press-drag-release, dispatched as real
                    // events so the current edit mode's handlers see it.
                    window.defer(cx, move |window, cx| {
                        let (from, to) = (point(px(from.0), px(from.1)), point(px(to.0), px(to.1)));
                        let modifiers = gpui::Modifiers {
                            alt: mods.alt,
                            platform: mods.cmd,
                            ..gpui::Modifiers::none()
                        };
                        let button = gpui::MouseButton::Left;
                        let down = MouseDownEvent {
                            button,
                            position: from,
                            modifiers,
                            click_count: 1,
                            first_mouse: false,
                        };
                        window.dispatch_event(gpui::PlatformInput::MouseDown(down), cx);
                        let mv = MouseMoveEvent {
                            position: to,
                            pressed_button: Some(button),
                            modifiers,
                        };
                        window.dispatch_event(gpui::PlatformInput::MouseMove(mv), cx);
                        let up = MouseUpEvent {
                            button,
                            position: to,
                            modifiers,
                            click_count: 1,
                        };
                        window.dispatch_event(gpui::PlatformInput::MouseUp(up), cx);
                    });
                }
                ScriptCommand::Drag { from, to, .. } => {
                    // A pan-drag (as with the right mouse button): down +
                    // single move + up, all in this one frame.
                    let start = self.window_to_map(point(px(from.0), px(from.1)));
                    self.start_pan_drag(start, cx);
                    let ev = MouseMoveEvent {
                        position: point(px(to.0), px(to.1)),
                        pressed_button: Some(gpui::MouseButton::Right),
                        modifiers: gpui::Modifiers::none(),
                    };
                    self.handle_mouse_move(&ev, cx);
                    self.end_pan_drag(cx);
                }
                ScriptCommand::Click {
                    x,
                    y,
                    right,
                    count,
                    ctrl,
                } => {
                    let btn = if right {
                        gpui::MouseButton::Right
                    } else {
                        gpui::MouseButton::Left
                    };
                    let click_count = count as usize;
                    let modifiers = gpui::Modifiers {
                        control: ctrl,
                        ..gpui::Modifiers::none()
                    };
                    // Dispatching synchronously here would double-lease `self`:
                    // we're already inside `MapViewer::render`'s entity update,
                    // and any `.on_mouse_down`/`.on_click` listener belonging to
                    // this same view calls `weak_entity.update(cx, ...)`, which
                    // panics ("cannot update ... while it is already being
                    // updated"). `Window::defer` runs the closure at the end of
                    // the current effect cycle, after this entity is returned to
                    // the app, avoiding the re-entrant lease.
                    window.defer(cx, move |window, cx| {
                        let down = MouseDownEvent {
                            button: btn,
                            position: point(px(x), px(y)),
                            modifiers,
                            click_count,
                            first_mouse: false,
                        };
                        window.dispatch_event(gpui::PlatformInput::MouseDown(down), cx);
                        let up = MouseUpEvent {
                            button: btn,
                            position: point(px(x), px(y)),
                            modifiers,
                            click_count,
                        };
                        window.dispatch_event(gpui::PlatformInput::MouseUp(up), cx);
                    });
                }
                ScriptCommand::Scroll { x, y, dx, dy } => {
                    // Line deltas, i.e. a notched mouse wheel: zooms.
                    let ev = ScrollWheelEvent {
                        position: point(px(x), px(y)),
                        delta: ScrollDelta::Lines(gpui::Point { x: dx, y: dy }),
                        modifiers: gpui::Modifiers::none(),
                        touch_phase: gpui::TouchPhase::Moved,
                    };
                    self.handle_scroll(&ev, cx);
                }
                ScriptCommand::Hover { x, y } => {
                    let ev = MouseMoveEvent {
                        position: point(px(x), px(y)),
                        pressed_button: None,
                        modifiers: gpui::Modifiers::none(),
                    };
                    self.handle_mouse_move(&ev, cx);
                }
                ScriptCommand::Capture { path } => {
                    let result = (|| -> Result<(), String> {
                        let image = window.render_to_image().map_err(|e| e.to_string())?;
                        if let Some(parent) = path.parent() {
                            if !parent.as_os_str().is_empty() {
                                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                            }
                        }
                        image.save(&path).map_err(|e| e.to_string())
                    })();
                    bus.set_capture_result(result);
                }
                ScriptCommand::LoadOsm { name, loaded } => {
                    let (data, edits) = *loaded;
                    self.add_osm_dataset(name, data, &edits, cx);
                }
                ScriptCommand::Action { name } => {
                    // `Name` or `Name {json}`; the JSON fills the action's fields.
                    let (action_name, data) = match name.split_once(char::is_whitespace) {
                        Some((n, json)) => (n, serde_json::from_str(json).ok()),
                        None => (name.as_str(), None),
                    };
                    let qualified = if action_name.contains("::") {
                        action_name.to_string()
                    } else {
                        format!("osm_gpui::{action_name}")
                    };
                    let result = match cx.build_action(&qualified, data) {
                        Ok(action) => {
                            // `App::dispatch_action`, deferred until this
                            // render's window lease is released, is the call
                            // the macOS menu bar makes: it dispatches to the
                            // active window's focused element (and on to the
                            // global handlers).
                            cx.defer(move |cx| cx.dispatch_action(action.as_ref()));
                            Ok(())
                        }
                        Err(e) => Err(format!("unknown action '{name}': {e}")),
                    };
                    bus.set_assert_result(result);
                }
                ScriptCommand::AssertMode(want) => {
                    let result = if self.mode == want {
                        Ok(())
                    } else {
                        Err(format!("expected mode {:?}, got {:?}", want, self.mode))
                    };
                    bus.set_assert_result(result);
                }
                ScriptCommand::AssertSelected(want) => {
                    let got: Option<(osm_gpui::script::FeatureKind, i64)> =
                        match self.selected.as_slice() {
                            [only] => Some((
                                match only.kind {
                                    osm_gpui::selection::FeatureKind::Node => {
                                        osm_gpui::script::FeatureKind::Node
                                    }
                                    osm_gpui::selection::FeatureKind::Way => {
                                        osm_gpui::script::FeatureKind::Way
                                    }
                                    osm_gpui::selection::FeatureKind::Relation => {
                                        osm_gpui::script::FeatureKind::Relation
                                    }
                                },
                                only.id,
                            )),
                            _ => None,
                        };
                    let result = if got == want {
                        Ok(())
                    } else {
                        Err(format!("expected selected {:?}, got {:?}", want, got))
                    };
                    bus.set_assert_result(result);
                }
                ScriptCommand::SetTag { key, value } => {
                    let result = self.script_set_tag(&key, &value, cx);
                    bus.set_assert_result(result);
                }
                ScriptCommand::RemoveTag { key } => {
                    let result = self.script_remove_tag(&key, cx);
                    bus.set_assert_result(result);
                }
                ScriptCommand::UploadConfirm => {
                    let result = self.script_upload_confirm(window, cx);
                    bus.set_assert_result(result);
                }
                ScriptCommand::SetComment(text) => {
                    let result = match self.upload_dialog.clone() {
                        Some(dialog) => {
                            dialog.update(cx, |dialog, cx| {
                                dialog.set_draft_comment(text, window, cx)
                            });
                            Ok(())
                        }
                        None => Err("the upload dialog is not open".to_string()),
                    };
                    bus.set_assert_result(result);
                }
                ScriptCommand::UploadCancel => {
                    let result = match self.upload_dialog.clone() {
                        Some(dialog) if matches!(dialog.read(cx).phase(), Phase::Busy) => {
                            Err("the upload dialog cannot be cancelled while busy".to_string())
                        }
                        Some(dialog) => {
                            window.defer(cx, move |_window, cx| {
                                dialog.update(cx, |dialog, cx| dialog.press_cancel(cx));
                            });
                            Ok(())
                        }
                        None => Err("the upload dialog is not open".to_string()),
                    };
                    bus.set_assert_result(result);
                }
                ScriptCommand::Resolve(choice) => {
                    let result = self.script_resolve_conflict(choice, window, cx);
                    bus.set_assert_result(result);
                }
                ScriptCommand::Export { path } => {
                    let result = self.script_export_layer(&path);
                    bus.set_assert_result(result);
                }
                ScriptCommand::Check(check) => {
                    let result = self.script_check(&check, cx);
                    bus.set_assert_result(result);
                }
                ScriptCommand::Note(message) => {
                    self.set_status(message);
                    cx.notify();
                }
            }
        }

        self.drain_keystrokes_and_animate(window, cx);
    }

    /// Set a tag on every selected feature through the same code the side
    /// panel's "Add tag" row ends in (one undo step; an existing value is
    /// replaced).
    pub(crate) fn script_set_tag(
        &mut self,
        key: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.selected.is_empty() {
            return Err("nothing is selected".into());
        }
        if osm_gpui::transfer::blocks_edits(self.transfer.as_ref()) {
            return Err("an upload is running; edits are refused until it finishes".into());
        }
        let ctx = crate::TagEditContext {
            features: self.selected.clone(),
            original_key: String::new(),
            original_value: String::new(),
            is_add: true,
        };
        self.apply_tag_edit(&ctx, key, value);
        cx.notify();
        let missing = self
            .selected_feature_tag_snapshots()
            .iter()
            .any(|(_, tags)| !tags.iter().any(|(k, v)| k == key && v == value));
        if missing {
            return Err(format!(
                "the selected feature did not take {key}={value} (is the layer editable?)"
            ));
        }
        Ok(())
    }

    pub(crate) fn script_remove_tag(
        &mut self,
        key: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if osm_gpui::transfer::blocks_edits(self.transfer.as_ref()) {
            return Err("an upload is running; edits are refused until it finishes".into());
        }
        let has = self
            .selected_feature_tag_snapshots()
            .iter()
            .any(|(_, tags)| tags.iter().any(|(k, _)| k == key));
        if !has {
            return Err(format!("no selected feature has a '{key}' tag"));
        }
        self.delete_tag(key, cx);
        Ok(())
    }

    /// Press the upload dialog's Upload / Try again button. Refuses (as the
    /// button would) with an empty comment, and, in a scripted run, anywhere
    /// but the development server.
    fn script_upload_confirm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(dialog) = self.upload_dialog.clone() else {
            return Err("the upload dialog is not open (run `action UploadToOsm` first)".into());
        };
        if let Some(refusal) = scripted_upload_refusal(&settings_store::api_base_url()) {
            return Err(refusal);
        }
        if let Some(why) = dialog.read(cx).confirm_blocker(cx) {
            return Err(format!("the Upload button would do nothing: {why}"));
        }
        // Deferred: the dialog's event is handled by this view, which is being
        // rendered right now (see the note on `Click` above).
        window.defer(cx, move |_window, cx| {
            dialog.update(cx, |dialog, cx| dialog.press_confirm(cx));
        });
        Ok(())
    }

    fn script_resolve_conflict(
        &mut self,
        choice: script::ConflictChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        use osm_gpui::ui::upload_dialog::ConflictAction;
        let action = match choice {
            script::ConflictChoice::KeepMine => ConflictAction::KeepMine,
            script::ConflictChoice::UseTheirs => ConflictAction::UseTheirs,
            script::ConflictChoice::DropMine => ConflictAction::DropMyChange,
            script::ConflictChoice::Show => ConflictAction::Show,
        };
        let Some(dialog) = self.upload_dialog.clone() else {
            return Err("the upload dialog is not open".into());
        };
        if let Some(refusal) = scripted_upload_refusal(&settings_store::api_base_url()) {
            return Err(refusal);
        }
        if !dialog.read(cx).offers(action) {
            return Err(format!(
                "the dialog does not offer {action:?} right now (is a conflict showing?)"
            ));
        }
        window.defer(cx, move |_window, cx| {
            dialog.update(cx, |dialog, cx| dialog.press_conflict(action, cx));
        });
        Ok(())
    }

    /// File > Export without the save dialog: the active layer, with its
    /// pending edits, to `path`.
    pub(crate) fn script_export_layer(&mut self, path: &std::path::Path) -> Result<(), String> {
        let (_, xml) = self
            .exportable_layer()
            .ok_or_else(|| "there is no OSM layer to export".to_string())?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        std::fs::write(path, xml).map_err(|e| e.to_string())
    }

    /// Pending (created + modified + deleted) edits in the active layer.
    pub(crate) fn active_layer_pending(&self) -> Option<usize> {
        let layer = self
            .active_layer
            .and_then(|id| self.layer_manager.find_layer(id))?;
        let (created, modified, deleted) = layer.diff_for_upload().counts();
        Some(created + modified + deleted)
    }

    /// Evaluate a read-only condition for `assert_*` and the transfer waits.
    pub(crate) fn script_check(
        &self,
        check: &script::Check,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        use osm_gpui::ui::upload_dialog::Phase;
        use script::{Check, DialogExpect, SelectionKind};

        let phase = self.upload_dialog.as_ref().map(|d| d.read(cx).phase());
        match check {
            Check::Pending(want) => match self.active_layer_pending() {
                Some(got) if got == *want => Ok(()),
                Some(got) => Err(format!("expected {want} pending edits, found {got}")),
                None => Err("there is no active OSM layer".into()),
            },
            Check::StatusContains(text) => {
                let shown = self
                    .status_message
                    .as_ref()
                    .map(|(m, _)| m.to_string())
                    .unwrap_or_default();
                if shown.contains(text.as_str()) {
                    Ok(())
                } else {
                    Err(format!(
                        "status bar says {shown:?}, expected it to contain {text:?}"
                    ))
                }
            }
            Check::TransferIdle => {
                if self.transfer.is_some() || matches!(phase, Some(Phase::Busy)) {
                    Err("a transfer is still running".into())
                } else {
                    Ok(())
                }
            }
            Check::Selection(want) => {
                let got = match self.selected.as_slice() {
                    [] => SelectionKind::None,
                    [only] => match only.kind {
                        osm_gpui::selection::FeatureKind::Node => SelectionKind::Node,
                        osm_gpui::selection::FeatureKind::Way => SelectionKind::Way,
                        osm_gpui::selection::FeatureKind::Relation => SelectionKind::Relation,
                    },
                    _ => SelectionKind::Multiple,
                };
                if got == *want {
                    Ok(())
                } else {
                    Err(format!("expected selection {want:?}, found {got:?}"))
                }
            }
            Check::Dialog(want) => {
                let found = self.describe_dialog(&phase);
                let ok = match (want, &phase) {
                    (DialogExpect::None, _) => found == "no dialog",
                    (DialogExpect::SignIn, _) => self.sign_in_dialog.is_some(),
                    (DialogExpect::Notice, _) => self.notice_dialog.is_some(),
                    (DialogExpect::Editing, Some(Phase::Editing)) => true,
                    (DialogExpect::InFlight, Some(Phase::Busy)) => true,
                    (DialogExpect::Failed, Some(Phase::Failed(_))) => true,
                    (DialogExpect::Conflict { kind, object }, Some(Phase::Failed(Some(view)))) => {
                        kind.is_none_or(|k| conflict_kind_name(&view.conflict.kind) == k)
                            && object.is_none_or(|o| {
                                view.conflict
                                    .object
                                    .is_some_and(|(found, _)| script_kind(found) == o)
                            })
                    }
                    _ => false,
                };
                if ok {
                    Ok(())
                } else {
                    Err(format!("expected {want:?}, found {found}"))
                }
            }
        }
    }

    fn describe_dialog(&self, phase: &Option<osm_gpui::ui::upload_dialog::Phase>) -> String {
        use osm_gpui::ui::upload_dialog::Phase;
        match phase {
            Some(Phase::Editing) => "the upload dialog, ready for a comment".into(),
            Some(Phase::Busy) => "the upload dialog, busy (uploading or resolving)".into(),
            Some(Phase::Failed(None)) => "the upload dialog showing an error".into(),
            Some(Phase::Failed(Some(view))) => format!(
                "the upload dialog showing a conflict: {} ({:?})",
                view.conflict.describe(),
                conflict_kind_name(&view.conflict.kind)
            ),
            None if self.sign_in_dialog.is_some() => "the sign-in prompt".into(),
            None if self.notice_dialog.is_some() => "a notice dialog".into(),
            None => "no dialog".into(),
        }
    }

    /// Drain the keystroke queue and keep the render loop going while a
    /// script runs.
    fn drain_keystrokes_and_animate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Also drain keystroke queue (processed via Window so needs to be here).
        // Dispatched via `window.defer` for the same reason `Click` is (see
        // above): `dispatch_keystroke` can invoke a `cx.listener()` callback
        // on this same `MapViewer` entity (e.g. the mode-switch shortcuts),
        // which would double-lease it if dispatched synchronously from
        // inside `render`.
        if let Some(ks_queue) = KEYSTROKE_QUEUE.get() {
            if let Ok(mut guard) = ks_queue.try_lock() {
                let pending: Vec<_> = guard.drain(..).collect();
                if !pending.is_empty() {
                    window.defer(cx, move |window, cx| {
                        for ks in pending {
                            window.dispatch_keystroke(ks, cx);
                        }
                    });
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
}

// ---------------------------------------------------------------------------
// LiveApp: AppHandle impl backed by ScriptBus (background-thread safe)
// ---------------------------------------------------------------------------

/// Why a scripted run may not upload to `server`, if it may not (the same
/// rule as `script::runner::upload_guard`, applied where the upload starts).
pub(crate) fn scripted_upload_refusal(server: &str) -> Option<String> {
    if !script_mode() {
        return None;
    }
    script::runner::upload_guard(&script::Op::UploadConfirm, server).err()
}

/// The keystroke that types `ch`. A printable ASCII character is its own key
/// (space is "space"); anything else has no key of its own and goes in as
/// text alone. Either way the text is carried explicitly, so it is inserted
/// as written whatever the keyboard layout.
pub(crate) fn keystroke_for_char(ch: char) -> Keystroke {
    let key = match ch {
        ' ' => "space".to_string(),
        c if c.is_ascii_graphic() => c.to_ascii_lowercase().to_string(),
        _ => String::new(),
    };
    Keystroke {
        modifiers: gpui::Modifiers::none(),
        key,
        key_char: Some(ch.to_string()),
    }
}

fn script_kind(kind: osm_gpui::selection::FeatureKind) -> script::FeatureKind {
    match kind {
        osm_gpui::selection::FeatureKind::Node => script::FeatureKind::Node,
        osm_gpui::selection::FeatureKind::Way => script::FeatureKind::Way,
        osm_gpui::selection::FeatureKind::Relation => script::FeatureKind::Relation,
    }
}

fn conflict_kind_name(kind: &osm_gpui::osm_upload::ConflictKind) -> script::ConflictKindName {
    use osm_gpui::osm_upload::ConflictKind;
    match kind {
        ConflictKind::VersionMismatch { .. } => script::ConflictKindName::VersionMismatch,
        ConflictKind::AlreadyDeleted => script::ConflictKindName::AlreadyDeleted,
        ConflictKind::StillInUse { .. } => script::ConflictKindName::StillInUse,
        ConflictKind::MissingDependency => script::ConflictKindName::MissingDependency,
        ConflictKind::Other => script::ConflictKindName::Other,
    }
}

/// The error from the last sign-in the harness started, if it failed.
static SIGN_IN_FAILURE: Mutex<Option<String>> = Mutex::new(None);
/// A sign-in the harness started is still waiting for the browser.
static SIGN_IN_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) struct LiveApp {
    pub(crate) _idle: Arc<IdleTracker>,
    pub(crate) bus: Arc<ScriptBus>,
}

impl AppHandle for LiveApp {
    fn set_window_size(&mut self, w: u32, h: u32) {
        self.bus.submit(ScriptCommand::SetWindowSize { w, h });
    }

    fn set_viewport(&mut self, lat: f64, lon: f64, zoom: f32) {
        self.bus.submit(ScriptCommand::SetViewport {
            lat,
            lon,
            zoom: zoom as f64,
        });
    }

    fn dispatch_drag(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        _duration: Duration,
        left: bool,
        mods: script::DragMods,
    ) {
        // Submit as a single command; the render fn handles the full down/move/up.
        self.bus.submit(ScriptCommand::Drag {
            from,
            to,
            left,
            mods,
        });
    }

    fn dispatch_click(
        &mut self,
        at: (f32, f32),
        button: script::MouseButton,
        count: u8,
        ctrl: bool,
    ) {
        let right = matches!(button, script::MouseButton::Right);
        self.bus.submit(ScriptCommand::Click {
            x: at.0,
            y: at.1,
            right,
            count,
            ctrl,
        });
    }

    fn dispatch_scroll(&mut self, at: (f32, f32), dx: f32, dy: f32) {
        self.bus.submit(ScriptCommand::Scroll {
            x: at.0,
            y: at.1,
            dx,
            dy,
        });
    }

    fn dispatch_hover(&mut self, at: (f32, f32)) {
        self.bus.submit(ScriptCommand::Hover { x: at.0, y: at.1 });
    }

    fn dispatch_key(&mut self, chord: &script::Chord) {
        // `Window::dispatch_keystroke` simulates the text a key would type
        // when `key_char` is unset — which lets `key` type into a focused
        // input one character at a time, but also turns Enter into a
        // literal "\n" inserted into whatever input still has focus after
        // the key-down. A real Return never inserts text into a single-line
        // input (and gpui panics shaping a line containing a newline), so
        // give Enter an explicit empty `key_char` to suppress that.
        let key_char = (chord.key == "enter").then(String::new);
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
            key_char,
        };
        if let Some(q) = KEYSTROKE_QUEUE.get() {
            if let Ok(mut guard) = q.lock() {
                guard.push(ks);
            }
        }
        // The keystroke is queued from this thread, so the frame in flight
        // may already be past the point where the queue is drained: it goes
        // out on the next frame, and its effects show on the one after.
        for _ in 0..3 {
            self.bus.wait_frame();
        }
    }

    fn dispatch_action(&mut self, name: &str) -> Result<(), String> {
        self.bus.submit(ScriptCommand::Action {
            name: name.to_string(),
        });
        let result = self.bus.take_assert_result();
        // Let the deferred dispatch (and the dialog it opens) render.
        self.bus.wait_frame();
        result
    }

    fn wait_frame(&mut self) {
        self.bus.wait_frame();
    }

    fn load_osm(&mut self, path: &std::path::Path) -> Result<(), String> {
        let parser = OsmParser::new();
        let path_str = path.to_string_lossy().to_string();
        let loaded = parser
            .parse_file_with_edits(&path_str)
            .map_err(|e| e.to_string())?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("OSM")
            .to_string();
        // `submit` blocks until the main thread has processed the command
        // (i.e. `MapViewer::add_osm_dataset` has run), so the layer exists
        // as soon as this returns — no separate `wait_frame` needed.
        self.bus.submit(ScriptCommand::LoadOsm {
            name: stem,
            loaded: Box::new(loaded),
        });
        Ok(())
    }

    fn capture(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.bus.submit(ScriptCommand::Capture {
            path: path.to_path_buf(),
        });
        self.bus.take_capture_result()
    }

    fn assert_mode(&mut self, want: script::EditMode) -> Result<(), String> {
        let want = match want {
            script::EditMode::Select => crate::EditMode::Select,
            script::EditMode::Add => crate::EditMode::Add,
            script::EditMode::Building => crate::EditMode::Building,
            script::EditMode::Extrude => crate::EditMode::Extrude,
        };
        self.bus.submit(ScriptCommand::AssertMode(want));
        self.bus.take_assert_result()
    }

    fn assert_selected(&mut self, want: Option<(script::FeatureKind, i64)>) -> Result<(), String> {
        self.bus.submit(ScriptCommand::AssertSelected(want));
        self.bus.take_assert_result()
    }

    fn api_server(&mut self) -> String {
        settings_store::api_base_url()
    }

    fn type_text(&mut self, text: &str) -> Result<(), String> {
        for ch in text.chars() {
            if ch.is_control() {
                return Err(format!("cannot type the control character {ch:?}"));
            }
            let ks = keystroke_for_char(ch);
            if let Some(q) = KEYSTROKE_QUEUE.get() {
                if let Ok(mut guard) = q.lock() {
                    guard.push(ks);
                }
            }
            // One character per frame, like a person typing, so each lands
            // before the next and the input can be watched filling in.
            self.bus.wait_frame();
        }
        // The last character goes out on the next frame and shows on the one
        // after.
        self.bus.wait_frame();
        self.bus.wait_frame();
        Ok(())
    }

    fn set_tag(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.bus.submit(ScriptCommand::SetTag {
            key: key.to_string(),
            value: value.to_string(),
        });
        let result = self.bus.take_assert_result();
        self.bus.wait_frame();
        result
    }

    fn remove_tag(&mut self, key: &str) -> Result<(), String> {
        self.bus.submit(ScriptCommand::RemoveTag {
            key: key.to_string(),
        });
        let result = self.bus.take_assert_result();
        self.bus.wait_frame();
        result
    }

    fn upload_confirm(&mut self) -> Result<(), String> {
        self.bus.submit(ScriptCommand::UploadConfirm);
        let result = self.bus.take_assert_result();
        // Let the press (deferred) happen and the dialog show its new stage.
        self.bus.wait_frame();
        self.bus.wait_frame();
        result
    }

    fn set_comment(&mut self, text: &str) -> Result<(), String> {
        self.bus.submit(ScriptCommand::SetComment(text.to_string()));
        let result = self.bus.take_assert_result();
        self.bus.wait_frame();
        result
    }

    fn upload_cancel(&mut self) -> Result<(), String> {
        self.bus.submit(ScriptCommand::UploadCancel);
        let result = self.bus.take_assert_result();
        self.bus.wait_frame();
        self.bus.wait_frame();
        result
    }

    fn resolve_conflict(&mut self, choice: script::ConflictChoice) -> Result<(), String> {
        self.bus.submit(ScriptCommand::Resolve(choice));
        let result = self.bus.take_assert_result();
        self.bus.wait_frame();
        self.bus.wait_frame();
        result
    }

    fn export_layer(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.bus.submit(ScriptCommand::Export {
            path: path.to_path_buf(),
        });
        self.bus.take_assert_result()
    }

    fn check(&mut self, check: &script::Check) -> Result<(), String> {
        self.bus.submit(ScriptCommand::Check(check.clone()));
        self.bus.take_assert_result()
    }

    fn note(&mut self, message: &str) {
        self.bus.submit(ScriptCommand::Note(message.to_string()));
    }

    fn sign_in(&mut self) -> Result<(), String> {
        if SIGN_IN_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err("a sign-in is already waiting for the browser".into());
        }
        *SIGN_IN_FAILURE.lock().unwrap() = None;
        let base_url = settings_store::api_base_url();
        // `auth::login` opens the browser and blocks until the approval comes
        // back (or three minutes pass), so it runs on its own thread.
        std::thread::spawn(move || {
            if let Err(e) = osm_gpui::auth::login(&base_url) {
                eprintln!("sign_in: {e}");
                *SIGN_IN_FAILURE.lock().unwrap() = Some(e.to_string());
            }
            SIGN_IN_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        });
        Ok(())
    }

    fn signed_in(&mut self) -> bool {
        let base = settings_store::api_base_url();
        osm_gpui::auth::stored_display_name(&osm_gpui::auth::oauth_base_for(&base)).is_some()
    }

    fn sign_in_failure(&mut self) -> Option<String> {
        SIGN_IN_FAILURE.lock().unwrap().clone()
    }
}
