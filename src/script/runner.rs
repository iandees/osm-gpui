//! Executes parsed script steps against the live app.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::idle_tracker::IdleTracker;
use crate::script::{AutosaveCmd, Check, Op, Step};

/// Interface to the live app. Implemented by `LiveApp` in `script_harness.rs`, which holds the
/// gpui `WindowHandle` and the `MapViewer` entity. Keeps the runner testable
/// and free of gpui imports.
pub trait AppHandle {
    fn set_window_size(&mut self, w: u32, h: u32);
    fn set_viewport(&mut self, lat: f64, lon: f64, zoom: f32);
    /// Pan the map by dragging with the right button from `from` to `to`.
    fn dispatch_pan_drag(&mut self, from: crate::script::Where, to: crate::script::Where);
    /// A press and release of one button. `mods` are added to any modifiers
    /// held with `set_held_modifiers`.
    fn dispatch_click(
        &mut self,
        at: crate::script::Where,
        button: crate::script::MouseButton,
        count: u8,
        mods: crate::script::Mods,
    );
    fn dispatch_scroll(&mut self, at: (f32, f32), dx: f32, dy: f32);
    /// One mouse event with the left button (a hover, the press, a move with
    /// the button held, the release), the way the OS delivers it. `at` of
    /// `None` is where the pointer last was.
    fn dispatch_pointer(
        &mut self,
        kind: crate::script::PointerKind,
        at: Option<crate::script::Where>,
        mods: crate::script::Mods,
    );
    /// Press (`down`) or release modifier keys with no other input. They
    /// count as held for every pointer event until released.
    fn set_held_modifiers(&mut self, mods: crate::script::Mods, down: bool);
    fn dispatch_key(&mut self, chord: &crate::script::Chord);
    /// Dispatch the named action (as a native menu item would). Errors if
    /// the name is not a registered action.
    fn dispatch_action(&mut self, name: &str) -> Result<(), String>;
    /// Yield until the next frame has been rendered.
    fn wait_frame(&mut self);
    /// Parse and load an OSM file, making it a new layer on the map.
    fn load_osm(&mut self, path: &std::path::Path) -> Result<(), String>;
    /// Render the current frame in-process and save it as a PNG.
    fn capture(&mut self, path: &std::path::Path) -> Result<(), String>;
    /// Compare the app's current `EditMode` against `want`; `Err` with a
    /// descriptive message if it doesn't match.
    fn assert_mode(&mut self, want: crate::script::EditMode) -> Result<(), String>;
    /// Compare the app's current selection against `want`: `Some((kind,
    /// id))` requires exactly that single feature selected, `None` requires
    /// an empty selection. `Err` with a descriptive message if it doesn't
    /// match.
    fn assert_selected(
        &mut self,
        want: Option<(crate::script::FeatureKind, i64)>,
    ) -> Result<(), String>;

    /// The OSM API base URL in effect for this run (what an upload would go to).
    fn api_server(&mut self) -> String;
    /// Type `text` into the focused input, one character at a time.
    fn type_text(&mut self, text: &str) -> Result<(), String>;
    /// Set a tag on the selected feature(s), as the side panel's Add tag does.
    fn set_tag(&mut self, key: &str, value: &str) -> Result<(), String>;
    /// Remove a tag from the selected feature(s).
    fn remove_tag(&mut self, key: &str) -> Result<(), String>;
    /// Press the upload dialog's Upload / Try again button.
    fn upload_confirm(&mut self) -> Result<(), String>;
    /// Replace the upload dialog's changeset comment.
    fn set_comment(&mut self, text: &str) -> Result<(), String>;
    /// Press the upload dialog's Cancel button.
    fn upload_cancel(&mut self) -> Result<(), String>;
    /// One of the upload dialog's changeset-extras ops. None of them can
    /// start an upload.
    fn upload_dialog(&mut self, cmd: &crate::script::UploadDialogCmd) -> Result<(), String>;
    /// Press the delete-layer confirmation's "Delete Layer" button.
    fn confirm_delete_layer(&mut self) -> Result<(), String>;
    /// Press the delete-layer confirmation's Cancel button.
    fn cancel_delete_layer(&mut self) -> Result<(), String>;
    /// Press one of the conflict dialog's buttons.
    fn resolve_conflict(&mut self, choice: crate::script::ConflictChoice) -> Result<(), String>;
    /// Write the active layer to a `.osm` file.
    fn export_layer(&mut self, path: &std::path::Path) -> Result<(), String>;
    /// Evaluate a read-only condition on the app; `Err` says what it found.
    fn check(&mut self, check: &Check) -> Result<(), String>;
    /// Do one of the autosave script ops (see `AutosaveCmd`).
    fn autosave(&mut self, cmd: crate::script::AutosaveCmd) -> Result<(), String>;
    /// Do one of the object history script ops (see `HistoryCmd`).
    fn history(&mut self, cmd: crate::script::HistoryCmd) -> Result<(), String>;
    /// Click a finding's row in the side panel's Validation section.
    fn show_finding(&mut self, target: &crate::script::FindingTarget) -> Result<(), String>;
    /// Expand or collapse a side panel section.
    fn set_panel_open(
        &mut self,
        section: crate::script::PanelSection,
        open: bool,
    ) -> Result<(), String>;
    /// Show `message` in the status bar.
    fn note(&mut self, message: &str);
    /// Start the browser sign-in for the active API server.
    fn sign_in(&mut self) -> Result<(), String>;
    /// Whether an account is signed in for the active API server.
    fn signed_in(&mut self) -> bool;
    /// Why the sign-in started by `sign_in` failed, if it did.
    fn sign_in_failure(&mut self) -> Option<String>;
    /// Do an OSM notes op (see `note_ops`). A write starts in the background;
    /// the runner then waits for `NoteCheck::Idle`.
    fn note_op(&mut self, op: &crate::script::NoteOp) -> Result<(), String>;
}

/// Why `op` may not run against `server`, if it may not. An op that starts or
/// confirms an upload, or signs in to upload, only runs against the OSM
/// development server (exact host match), so a script can never write to the
/// production server.
pub fn upload_guard(op: &Op, server: &str) -> Result<(), String> {
    if !op.writes_to_server() {
        return Ok(());
    }
    let described = describe(op);
    let name = described.split_whitespace().next().unwrap_or_default();
    if crate::settings_store::is_dev_server(server) {
        Ok(())
    } else {
        Err(format!(
            "{name}: refusing to run: the active API server is {server}, not the OSM development \
             server ({}). Scripted uploads and note writes only run against the development \
             server; start the app with --api-server dev",
            crate::settings_store::DEV_API_URL
        ))
    }
}

#[derive(Debug)]
pub struct RunError {
    pub line_no: usize,
    pub message: String,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "script error at line {}: {}", self.line_no, self.message)
    }
}

pub struct Runner {
    pub idle: Arc<IdleTracker>,
    /// Pause after every op (`--script-delay-ms`), so a person can follow.
    pub step_delay: Duration,
}

impl Runner {
    pub fn run<A: AppHandle>(&self, app: &mut A, steps: &[Step]) -> Result<(), RunError> {
        for (i, step) in steps.iter().enumerate() {
            let started = Instant::now();
            println!("step {}: {}", i + 1, describe(&step.op));
            self.run_step(app, step)?;
            println!("  ok ({}ms)", started.elapsed().as_millis());
            if !self.step_delay.is_zero() {
                std::thread::sleep(self.step_delay);
            }
        }
        Ok(())
    }

    fn run_step<A: AppHandle>(&self, app: &mut A, step: &Step) -> Result<(), RunError> {
        let fail = |message: String| RunError {
            line_no: step.line_no,
            message,
        };
        // Checked before anything else, whatever the op: see `upload_guard`.
        if step.op.writes_to_server() {
            let server = app.api_server();
            upload_guard(&step.op, &server).map_err(fail)?;
        }
        match &step.op {
            Op::Window { w, h } => {
                app.set_window_size(*w, *h);
                Ok(())
            }
            Op::Viewport { lat, lon, zoom } => {
                app.set_viewport(*lat, *lon, *zoom);
                Ok(())
            }
            Op::Wait { duration } => {
                std::thread::sleep(*duration);
                Ok(())
            }
            Op::WaitIdle { timeout } => self.wait_idle(app, *timeout, step.line_no),
            Op::Drag {
                from,
                to,
                left: false,
                ..
            } => {
                app.dispatch_pan_drag(*from, *to);
                Ok(())
            }
            Op::Drag {
                from,
                to,
                duration,
                mods,
                ..
            } => {
                // What a real mouse does: arrive (so hover state, such as the
                // vertices a hovered way offers for grabbing, is up to date
                // when the button goes down), press, move, release. Each event
                // gets its own frame.
                use crate::script::PointerKind::*;
                let pause = (*duration / 2).min(Duration::from_millis(250));
                let frame = |app: &mut A| {
                    app.wait_frame();
                    app.wait_frame();
                };
                app.dispatch_pointer(Hover, Some(*from), *mods);
                frame(app);
                app.dispatch_pointer(Press, Some(*from), *mods);
                frame(app);
                app.dispatch_pointer(HeldMove, Some(*to), *mods);
                frame(app);
                std::thread::sleep(pause);
                app.dispatch_pointer(Release, Some(*to), *mods);
                frame(app);
                Ok(())
            }
            Op::Pointer { kind, at, mods } => {
                // A person's pointer arrives before the button goes down, and
                // arriving is what makes a way's vertices grabbable (hover
                // state), so `drag_begin` does the same.
                if let (crate::script::PointerKind::Press, Some(at)) = (kind, at) {
                    app.dispatch_pointer(crate::script::PointerKind::Hover, Some(*at), *mods);
                    app.wait_frame();
                    app.wait_frame();
                }
                app.dispatch_pointer(*kind, *at, *mods);
                app.wait_frame();
                app.wait_frame();
                Ok(())
            }
            Op::Modifiers { mods, down } => {
                app.set_held_modifiers(*mods, *down);
                app.wait_frame();
                app.wait_frame();
                Ok(())
            }
            Op::Click {
                at,
                button,
                count,
                mods,
            } => {
                app.dispatch_click(*at, *button, *count, *mods);
                Ok(())
            }
            Op::AssertSnap { expect } => app
                .check(&Check::Snap(*expect))
                .map_err(|e| fail(format!("assert_snap: {e}"))),
            Op::AssertWayNodes { way, nodes } => app
                .check(&Check::WayNodes {
                    way: *way,
                    nodes: nodes.clone(),
                })
                .map_err(|e| fail(format!("assert_way_nodes: {e}"))),
            Op::AssertNodeShared { node, ways } => app
                .check(&Check::NodeShared {
                    node: *node,
                    ways: ways.clone(),
                })
                .map_err(|e| fail(format!("assert_node_shared: {e}"))),
            Op::Scroll { at, dx, dy } => {
                app.dispatch_scroll((at.x, at.y), *dx, *dy);
                Ok(())
            }
            Op::Key { chord } => {
                app.dispatch_key(chord);
                Ok(())
            }
            Op::Action { name } => {
                app.dispatch_action(name).map_err(|e| RunError {
                    line_no: step.line_no,
                    message: format!("action: {}", e),
                })?;
                Ok(())
            }
            Op::Capture { path } => {
                let pb = PathBuf::from(path);
                app.capture(&pb).map_err(|e| RunError {
                    line_no: step.line_no,
                    message: format!("capture: {}", e),
                })?;
                println!("  -> {}", path);
                Ok(())
            }
            Op::Log { message } => {
                println!("{}", message);
                Ok(())
            }
            Op::FrameStats { label } => {
                crate::frame_stats::report(label);
                Ok(())
            }
            Op::LoadOsm { path } => {
                let pb = std::path::PathBuf::from(path);
                app.load_osm(&pb).map_err(|e| RunError {
                    line_no: step.line_no,
                    message: format!("load_osm: {}", e),
                })?;
                Ok(())
            }
            // The live app routes by file type (see `LiveApp::load_osm`).
            Op::LoadFile { path } => app
                .load_osm(std::path::Path::new(path))
                .map_err(|e| fail(format!("load_file: {e}"))),
            Op::AssertLayer { kind, name } => app
                .check(&Check::Layer {
                    kind: *kind,
                    name: name.clone(),
                })
                .map_err(|e| fail(format!("assert_layer: {e}"))),
            Op::AssertSelectedAt {
                lat,
                lon,
                tolerance_m,
            } => app
                .check(&Check::SelectedAt {
                    lat: *lat,
                    lon: *lon,
                    tolerance_m: *tolerance_m,
                })
                .map_err(|e| fail(format!("assert_selected_at: {e}"))),
            Op::AssertMode { mode } => {
                app.assert_mode(*mode).map_err(|e| RunError {
                    line_no: step.line_no,
                    message: format!("assert_mode: {}", e),
                })?;
                Ok(())
            }
            Op::AssertSelected { feature } => {
                app.assert_selected(*feature).map_err(|e| RunError {
                    line_no: step.line_no,
                    message: format!("assert_selected: {}", e),
                })?;
                Ok(())
            }
            Op::Type { text } => app.type_text(text).map_err(|e| fail(format!("type: {e}"))),
            Op::SetTag { key, value } => app
                .set_tag(key, value)
                .map_err(|e| fail(format!("set_tag: {e}"))),
            Op::RemoveTag { key } => app
                .remove_tag(key)
                .map_err(|e| fail(format!("remove_tag: {e}"))),
            Op::UploadConfirm => app
                .upload_confirm()
                .map_err(|e| fail(format!("upload_confirm: {e}"))),
            Op::SetComment { text } => app
                .set_comment(text)
                .map_err(|e| fail(format!("set_comment: {e}"))),
            Op::UploadCancel => app
                .upload_cancel()
                .map_err(|e| fail(format!("upload_cancel: {e}"))),
            Op::UploadDialog(cmd) => app
                .upload_dialog(cmd)
                .map_err(|e| fail(format!("{}: {e}", describe(&step.op)))),
            Op::AssertChangesetTag { key, value } => app
                .check(&Check::ChangesetTag {
                    key: key.clone(),
                    value: value.clone(),
                })
                .map_err(|e| fail(format!("assert_changeset_tag: {e}"))),
            Op::ConfirmDeleteLayer => app
                .confirm_delete_layer()
                .map_err(|e| fail(format!("confirm_delete_layer: {e}"))),
            Op::CancelDeleteLayer => app
                .cancel_delete_layer()
                .map_err(|e| fail(format!("cancel_delete_layer: {e}"))),
            Op::ResolveConflict { choice } => app
                .resolve_conflict(*choice)
                .map_err(|e| fail(format!("resolve_conflict: {e}"))),
            Op::WaitTransfer { timeout } => self.wait_transfer(app, *timeout, step.line_no),
            Op::CaptureWhileTransferring {
                prefix,
                every,
                timeout,
            } => self.capture_while_transferring(app, prefix, *every, *timeout, step.line_no),
            Op::AssertPending { count } => app
                .check(&Check::Pending(*count))
                .map_err(|e| fail(format!("assert_pending: {e}"))),
            Op::AssertStatusContains { text } => app
                .check(&Check::StatusContains(text.clone()))
                .map_err(|e| fail(format!("assert_status_contains: {e}"))),
            Op::AssertZoom { level, tolerance } => app
                .check(&Check::Zoom {
                    level: *level,
                    tolerance: *tolerance,
                })
                .map_err(|e| fail(format!("assert_zoom: {e}"))),
            Op::AssertImageryOffset {
                east,
                north,
                tolerance,
            } => app
                .check(&Check::ImageryOffset {
                    east: *east,
                    north: *north,
                    tolerance: *tolerance,
                })
                .map_err(|e| fail(format!("assert_imagery_offset: {e}"))),
            Op::AssertAdjustingOffset { on } => app
                .check(&Check::AdjustingOffset(*on))
                .map_err(|e| fail(format!("assert_adjusting_offset: {e}"))),
            Op::AssertDialog { expect } => app
                .check(&Check::Dialog(*expect))
                .map_err(|e| fail(format!("assert_dialog: {e}"))),
            Op::AssertSelection { kind } => app
                .check(&Check::Selection(*kind))
                .map_err(|e| fail(format!("assert_selection: {e}"))),
            Op::AssertValidation {
                errors,
                warnings,
                checks,
            } => app
                .check(&Check::Validation {
                    errors: *errors,
                    warnings: *warnings,
                    checks: checks.clone(),
                })
                .map_err(|e| fail(format!("assert_validation: {e}"))),
            Op::ShowFinding { target } => app
                .show_finding(target)
                .map_err(|e| fail(format!("show_finding: {e}"))),
            Op::SetPanelOpen { section, open } => app
                .set_panel_open(*section, *open)
                .map_err(|e| fail(format!("panel: {e}"))),
            Op::AssertPanelOpen { section, open } => app
                .check(&Check::PanelOpen(*section, *open))
                .map_err(|e| fail(format!("assert_panel: {e}"))),
            Op::AssertValidationPanel { shown } => app
                .check(&Check::ValidationPanel(*shown))
                .map_err(|e| fail(format!("assert_validation_panel: {e}"))),
            Op::AssertNativeAppearance { expect } => app
                .check(&Check::NativeAppearance(*expect))
                .map_err(|e| fail(format!("assert_native_appearance: {e}"))),
            Op::AssertLayerCount { count } => app
                .check(&Check::LayerCount(*count))
                .map_err(|e| fail(format!("assert_layer_count: {e}"))),
            Op::AssertSelectedCount { count } => app
                .check(&Check::SelectedCount(*count))
                .map_err(|e| fail(format!("assert_selected_count: {e}"))),
            Op::AssertFindPreview { text } => app
                .check(&Check::FindPreview(text.clone()))
                .map_err(|e| fail(format!("assert_find_preview: {e}"))),
            Op::ExportLayer { path } => {
                app.export_layer(std::path::Path::new(path))
                    .map_err(|e| fail(format!("export_layer: {e}")))?;
                println!("  -> {}", path);
                Ok(())
            }
            Op::AutosaveNow => {
                app.autosave(AutosaveCmd::Now)
                    .map_err(|e| fail(format!("autosave_now: {e}")))?;
                self.wait_autosave_idle(app, step.line_no, "autosave_now")
            }
            Op::RestoreAutosave => {
                app.autosave(AutosaveCmd::Restore)
                    .map_err(|e| fail(format!("restore_autosave: {e}")))?;
                self.wait_autosave_idle(app, step.line_no, "restore_autosave")
            }
            Op::DiscardAutosave => {
                app.autosave(AutosaveCmd::Discard)
                    .map_err(|e| fail(format!("discard_autosave: {e}")))?;
                self.wait_autosave_idle(app, step.line_no, "discard_autosave")
            }
            Op::Crash => {
                println!("  crashing now: exiting without any clean-up");
                app.autosave(AutosaveCmd::Crash)
                    .map_err(|e| fail(format!("crash: {e}")))
            }
            Op::AssertAutosaveExists { count } => app
                .check(&Check::AutosaveFiles(*count))
                .map_err(|e| fail(format!("assert_autosave_exists: {e}"))),
            Op::ShowHistory => {
                app.history(crate::script::HistoryCmd::Show)
                    .map_err(|e| fail(format!("show_history: {e}")))?;
                let deadline = Instant::now() + Duration::from_secs(30);
                // Let the request start, then wait for it to settle.
                for _ in 0..3 {
                    app.wait_frame();
                }
                loop {
                    match app.check(&Check::HistorySettled) {
                        Ok(()) => {
                            app.wait_frame();
                            return Ok(());
                        }
                        Err(why) if Instant::now() >= deadline => {
                            return Err(fail(format!(
                                "show_history: still loading after 30s: {why}"
                            )));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
            }
            Op::LoadHistoryFile { path } => app
                .history(crate::script::HistoryCmd::LoadFile(path.into()))
                .map_err(|e| fail(format!("load_history_file: {e}"))),
            Op::AssertHistoryVersions { count } => app
                .check(&Check::HistoryVersions(*count))
                .map_err(|e| fail(format!("assert_history_versions: {e}"))),
            Op::SignIn => app.sign_in().map_err(|e| fail(format!("sign_in: {e}"))),
            Op::WaitSignedIn { timeout } => {
                let deadline = Instant::now() + *timeout;
                while !app.signed_in() {
                    if let Some(why) = app.sign_in_failure() {
                        return Err(fail(format!("wait_signed_in: sign-in failed: {why}")));
                    }
                    if Instant::now() >= deadline {
                        return Err(fail(format!(
                            "wait_signed_in timed out after {:?}: nobody approved the sign-in in the browser",
                            timeout
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                Ok(())
            }
            Op::AssertSignedIn => {
                if app.signed_in() {
                    Ok(())
                } else {
                    Err(fail(format!(
                        "assert_signed_in: not signed in to {}; run docs/live-test/00-sign-in.osmscript first",
                        app.api_server()
                    )))
                }
            }
            Op::AssertServerDev => {
                let server = app.api_server();
                if crate::settings_store::is_dev_server(&server) {
                    Ok(())
                } else {
                    Err(fail(format!(
                        "assert_server: the active API server is {server}, not {}; start the app with --api-server dev",
                        crate::settings_store::DEV_API_URL
                    )))
                }
            }
            Op::Pause { duration, message } => {
                println!("{}", message);
                app.note(message);
                std::thread::sleep(*duration);
                Ok(())
            }
            Op::Note(n) => {
                let name = n.name();
                app.note_op(n).map_err(|e| fail(format!("{name}: {e}")))?;
                if n.waits() {
                    self.wait_notes_idle(app, step.line_no, name)?;
                }
                Ok(())
            }
            Op::AssertNote(c) => app
                .check(&Check::Note(c.clone()))
                .map_err(|e| fail(format!("assert_note: {e}"))),
        }
    }

    /// Wait until the note fetch or write just started has finished (the
    /// outcome is then in the note panel and the status bar).
    fn wait_notes_idle<A: AppHandle>(
        &self,
        app: &mut A,
        line_no: usize,
        op: &str,
    ) -> Result<(), RunError> {
        let deadline = Instant::now() + Duration::from_secs(60);
        for _ in 0..3 {
            app.wait_frame();
        }
        loop {
            if app
                .check(&Check::Note(crate::script::NoteCheck::Idle))
                .is_ok()
            {
                app.wait_frame();
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(RunError {
                    line_no,
                    message: format!("{op}: still running after 60s"),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Wait until every autosave write and any restore has finished.
    fn wait_autosave_idle<A: AppHandle>(
        &self,
        app: &mut A,
        line_no: usize,
        op: &str,
    ) -> Result<(), RunError> {
        let deadline = Instant::now() + Duration::from_secs(30);
        // Let the command take effect (a restore starts on the next frame).
        for _ in 0..3 {
            app.wait_frame();
        }
        loop {
            match app.check(&Check::AutosaveIdle) {
                Ok(()) => {
                    // One more frame so the result is on screen.
                    app.wait_frame();
                    return Ok(());
                }
                Err(why) if Instant::now() >= deadline => {
                    return Err(RunError {
                        line_no,
                        message: format!("{op}: still not finished after 30s: {why}"),
                    });
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    /// Wait until no transfer is running (it finished, or failed).
    fn wait_transfer<A: AppHandle>(
        &self,
        app: &mut A,
        timeout: Duration,
        line_no: usize,
    ) -> Result<(), RunError> {
        let deadline = Instant::now() + timeout;
        // Give a just-started transfer a few frames to register as running.
        for _ in 0..5 {
            app.wait_frame();
        }
        loop {
            if app.check(&Check::TransferIdle).is_ok() {
                // One more frame so the result (status, dialog) is on screen.
                app.wait_frame();
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(RunError {
                    line_no,
                    message: format!("wait_transfer timed out after {:?}", timeout),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Capture `prefix-NN.png` every `every` for as long as a transfer runs,
    /// so an upload's stages (including "Changeset 2 of 3") are on record.
    fn capture_while_transferring<A: AppHandle>(
        &self,
        app: &mut A,
        prefix: &str,
        every: Duration,
        timeout: Duration,
        line_no: usize,
    ) -> Result<(), RunError> {
        let deadline = Instant::now() + timeout;
        for _ in 0..5 {
            app.wait_frame();
        }
        let mut n = 0;
        while app.check(&Check::TransferIdle).is_err() {
            n += 1;
            let path = format!("{prefix}-{n:02}.png");
            app.capture(std::path::Path::new(&path))
                .map_err(|e| RunError {
                    line_no,
                    message: format!("capture_while_transferring: {e}"),
                })?;
            println!("  -> {}", path);
            if Instant::now() >= deadline {
                return Err(RunError {
                    line_no,
                    message: format!("capture_while_transferring timed out after {:?}", timeout),
                });
            }
            std::thread::sleep(every);
        }
        app.wait_frame();
        Ok(())
    }

    fn wait_idle<A: AppHandle>(
        &self,
        app: &mut A,
        timeout: Duration,
        line_no: usize,
    ) -> Result<(), RunError> {
        // Number of frames to wait unconditionally before we start checking
        // for idle. This gives gpui time to run at least one render cycle so
        // tile-fetch work has been submitted to the background executor.
        const PRIMING_FRAMES: u32 = 10;

        let deadline = Instant::now() + timeout;
        let mut frame = 0u32;
        let mut consecutive_idle = 0;
        loop {
            app.wait_frame();
            frame += 1;

            if frame > PRIMING_FRAMES {
                if self.idle.is_idle() {
                    consecutive_idle += 1;
                    if consecutive_idle >= 2 {
                        return Ok(());
                    }
                } else {
                    consecutive_idle = 0;
                }
            }

            if Instant::now() >= deadline {
                return Err(RunError {
                    line_no,
                    message: format!("wait_idle timed out after {:?}", timeout),
                });
            }
        }
    }
}

fn describe(op: &Op) -> String {
    match op {
        Op::Window { w, h } => format!("window {} {}", w, h),
        Op::Viewport { lat, lon, zoom } => format!("viewport {} {} {}", lat, lon, zoom),
        Op::WaitIdle { timeout } => format!("wait_idle {:?}", timeout),
        Op::Wait { duration } => format!("wait {:?}", duration),
        Op::Drag {
            from, to, duration, ..
        } => format!("drag {:?} -> {:?} ({:?})", from, to, duration),
        Op::Click {
            at,
            button,
            count,
            mods,
        } => format!(
            "click {:?} {:?} count={} mods={:?}",
            at, button, count, mods
        ),
        Op::Pointer { kind, at, mods } => format!("pointer {:?} {:?} mods={:?}", kind, at, mods),
        Op::Modifiers { mods, down } => {
            format!("{} {:?}", if *down { "mod_down" } else { "mod_up" }, mods)
        }
        Op::AssertSnap { expect } => format!("assert_snap {:?}", expect),
        Op::AssertWayNodes { way, nodes } => format!("assert_way_nodes {way} {nodes:?}"),
        Op::AssertNodeShared { node, ways } => format!("assert_node_shared {node} {ways:?}"),
        Op::Scroll { at, dx, dy } => format!("scroll {:?} dx={} dy={}", at, dx, dy),
        Op::Key { chord } => format!("key {:?}", chord),
        Op::Action { name } => format!("action {}", name),
        Op::Capture { path } => format!("capture {}", path),
        Op::Log { message } => format!("log {}", message),
        Op::FrameStats { label } => format!("frame_stats {}", label),
        Op::LoadOsm { path } => format!("load_osm {}", path),
        Op::LoadFile { path } => format!("load_file {}", path),
        Op::AssertLayer { kind, name } => format!("assert_layer {:?} {}", kind, name),
        Op::AssertSelectedAt { lat, lon, .. } => format!("assert_selected_at {lat} {lon}"),
        Op::AssertMode { mode } => format!("assert_mode {:?}", mode),
        Op::AssertSelected { feature } => format!("assert_selected {:?}", feature),
        Op::Type { text } => format!("type {}", text),
        Op::SetTag { key, value } => format!("set_tag {}={}", key, value),
        Op::RemoveTag { key } => format!("remove_tag {}", key),
        Op::UploadConfirm => "upload_confirm".to_string(),
        Op::UploadCancel => "upload_cancel".to_string(),
        Op::ConfirmDeleteLayer => "confirm_delete_layer".to_string(),
        Op::CancelDeleteLayer => "cancel_delete_layer".to_string(),
        Op::SetComment { text } => format!("set_comment {}", text),
        Op::UploadDialog(cmd) => format!("upload dialog {:?}", cmd),
        Op::AssertChangesetTag { key, value } => {
            format!("assert_changeset_tag {} {:?}", key, value)
        }
        Op::ResolveConflict { choice } => format!("resolve_conflict {:?}", choice),
        Op::WaitTransfer { timeout } => format!("wait_transfer {:?}", timeout),
        Op::CaptureWhileTransferring { prefix, .. } => {
            format!("capture_while_transferring {}", prefix)
        }
        Op::AssertPending { count } => format!("assert_pending {}", count),
        Op::AssertStatusContains { text } => format!("assert_status_contains {}", text),
        Op::AssertZoom { level, .. } => format!("assert_zoom {level}"),
        Op::AssertImageryOffset { east, north, .. } => {
            format!("assert_imagery_offset {east} {north}")
        }
        Op::AssertAdjustingOffset { on } => format!("assert_adjusting_offset {on}"),
        Op::AssertDialog { expect } => format!("assert_dialog {:?}", expect),
        Op::AssertSelection { kind } => format!("assert_selection {:?}", kind),
        Op::AssertValidation {
            errors,
            warnings,
            checks,
        } => format!(
            "assert_validation errors={:?} warnings={:?} {:?}",
            errors, warnings, checks
        ),
        Op::ShowFinding { target } => format!("show_finding {:?}", target),
        Op::AssertValidationPanel { shown } => format!("assert_validation_panel {}", shown),
        Op::SetPanelOpen { section, open } => format!("panel {:?} open={}", section, open),
        Op::AssertPanelOpen { section, open } => {
            format!("assert_panel {:?} open={}", section, open)
        }
        Op::AssertNativeAppearance { expect } => format!("assert_native_appearance {:?}", expect),
        Op::AssertSelectedCount { count } => format!("assert_selected_count {}", count),
        Op::AssertLayerCount { count } => format!("assert_layer_count {}", count),
        Op::AssertFindPreview { text } => format!("assert_find_preview {}", text),
        Op::ExportLayer { path } => format!("export_layer {}", path),
        Op::AutosaveNow => "autosave_now".to_string(),
        Op::AssertAutosaveExists { count } => format!("assert_autosave_exists {:?}", count),
        Op::RestoreAutosave => "restore_autosave".to_string(),
        Op::DiscardAutosave => "discard_autosave".to_string(),
        Op::Crash => "crash".to_string(),
        Op::ShowHistory => "show_history".to_string(),
        Op::LoadHistoryFile { path } => format!("load_history_file {}", path),
        Op::AssertHistoryVersions { count } => format!("assert_history_versions {}", count),
        Op::SignIn => "sign_in".to_string(),
        Op::WaitSignedIn { timeout } => format!("wait_signed_in {:?}", timeout),
        Op::AssertSignedIn => "assert_signed_in".to_string(),
        Op::AssertServerDev => "assert_server dev".to_string(),
        Op::Pause { message, .. } => format!("pause {}", message),
        Op::Note(n) => format!("{} {:?}", n.name(), n),
        Op::AssertNote(c) => format!("assert_note {:?}", c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::{Chord, MouseButton};

    struct Fake {
        pub frames_waited: u32,
        pub idle_after_frame: u32,
        pub idle: Arc<IdleTracker>,
        pub mode: crate::script::EditMode,
        pub selected: Option<(crate::script::FeatureKind, i64)>,
        pub server: String,
        pub calls: Vec<String>,
        pub busy_checks: u32,
        pub check_result: Result<(), String>,
        pub signed_in: bool,
        pub sign_in_failure: Option<String>,
    }

    impl AppHandle for Fake {
        fn set_window_size(&mut self, _w: u32, _h: u32) {}
        fn set_viewport(&mut self, _lat: f64, _lon: f64, _zoom: f32) {}
        fn dispatch_pan_drag(&mut self, _: crate::script::Where, _: crate::script::Where) {
            self.calls.push("pan".to_string());
        }
        fn dispatch_click(
            &mut self,
            _: crate::script::Where,
            _: MouseButton,
            _: u8,
            mods: crate::script::Mods,
        ) {
            self.calls.push(format!("click {mods:?}"));
        }
        fn dispatch_scroll(&mut self, _: (f32, f32), _: f32, _: f32) {}
        fn dispatch_pointer(
            &mut self,
            kind: crate::script::PointerKind,
            at: Option<crate::script::Where>,
            mods: crate::script::Mods,
        ) {
            self.calls
                .push(format!("pointer {kind:?} {} {mods:?}", at.is_some()));
        }
        fn set_held_modifiers(&mut self, mods: crate::script::Mods, down: bool) {
            self.calls.push(format!("held {down} {mods:?}"));
        }
        fn dispatch_key(&mut self, _: &Chord) {}
        fn dispatch_action(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn wait_frame(&mut self) {
            self.frames_waited += 1;
            if self.frames_waited == 1 {
                self.idle.tile_fetch_started();
            }
            if self.frames_waited == self.idle_after_frame {
                self.idle.tile_fetch_finished();
            }
        }
        fn load_osm(&mut self, _p: &std::path::Path) -> Result<(), String> {
            Ok(())
        }
        fn capture(&mut self, _p: &std::path::Path) -> Result<(), String> {
            Ok(())
        }
        fn assert_mode(&mut self, want: crate::script::EditMode) -> Result<(), String> {
            if self.mode == want {
                Ok(())
            } else {
                Err(format!("expected mode {:?}, got {:?}", want, self.mode))
            }
        }
        fn assert_selected(
            &mut self,
            want: Option<(crate::script::FeatureKind, i64)>,
        ) -> Result<(), String> {
            if self.selected == want {
                Ok(())
            } else {
                Err(format!(
                    "expected selected {:?}, got {:?}",
                    want, self.selected
                ))
            }
        }
        fn api_server(&mut self) -> String {
            self.server.clone()
        }
        fn type_text(&mut self, text: &str) -> Result<(), String> {
            self.calls.push(format!("type {text}"));
            Ok(())
        }
        fn set_tag(&mut self, key: &str, value: &str) -> Result<(), String> {
            self.calls.push(format!("set_tag {key}={value}"));
            Ok(())
        }
        fn remove_tag(&mut self, key: &str) -> Result<(), String> {
            self.calls.push(format!("remove_tag {key}"));
            Ok(())
        }
        fn upload_confirm(&mut self) -> Result<(), String> {
            self.calls.push("upload_confirm".into());
            Ok(())
        }
        fn set_comment(&mut self, text: &str) -> Result<(), String> {
            self.calls.push(format!("set_comment {text}"));
            Ok(())
        }
        fn upload_cancel(&mut self) -> Result<(), String> {
            self.calls.push("upload_cancel".into());
            Ok(())
        }
        fn upload_dialog(&mut self, cmd: &crate::script::UploadDialogCmd) -> Result<(), String> {
            self.calls.push(format!("upload_dialog {cmd:?}"));
            Ok(())
        }
        fn confirm_delete_layer(&mut self) -> Result<(), String> {
            self.calls.push("confirm_delete_layer".into());
            Ok(())
        }
        fn cancel_delete_layer(&mut self) -> Result<(), String> {
            self.calls.push("cancel_delete_layer".into());
            Ok(())
        }
        fn resolve_conflict(
            &mut self,
            choice: crate::script::ConflictChoice,
        ) -> Result<(), String> {
            self.calls.push(format!("resolve {choice:?}"));
            Ok(())
        }
        fn export_layer(&mut self, path: &std::path::Path) -> Result<(), String> {
            self.calls.push(format!("export {}", path.display()));
            Ok(())
        }
        fn check(&mut self, check: &Check) -> Result<(), String> {
            self.calls.push(format!("check {check:?}"));
            match check {
                Check::TransferIdle => {
                    if self.busy_checks > 0 {
                        self.busy_checks -= 1;
                        Err("busy".into())
                    } else {
                        Ok(())
                    }
                }
                _ => self.check_result.clone(),
            }
        }
        fn autosave(&mut self, cmd: crate::script::AutosaveCmd) -> Result<(), String> {
            self.calls.push(format!("autosave {cmd:?}"));
            Ok(())
        }
        fn history(&mut self, cmd: crate::script::HistoryCmd) -> Result<(), String> {
            self.calls.push(format!("history {cmd:?}"));
            Ok(())
        }
        fn show_finding(&mut self, target: &crate::script::FindingTarget) -> Result<(), String> {
            self.calls.push(format!("show_finding {target:?}"));
            Ok(())
        }
        fn set_panel_open(
            &mut self,
            section: crate::script::PanelSection,
            open: bool,
        ) -> Result<(), String> {
            self.calls.push(format!("panel {section:?} {open}"));
            Ok(())
        }
        fn note(&mut self, message: &str) {
            self.calls.push(format!("note {message}"));
        }
        fn sign_in(&mut self) -> Result<(), String> {
            self.calls.push("sign_in".into());
            Ok(())
        }
        fn signed_in(&mut self) -> bool {
            self.signed_in
        }
        fn sign_in_failure(&mut self) -> Option<String> {
            self.sign_in_failure.clone()
        }
        fn note_op(&mut self, op: &crate::script::NoteOp) -> Result<(), String> {
            self.calls.push(format!("note_op {}", op.name()));
            Ok(())
        }
    }

    const DEV: &str = "https://master.apis.dev.openstreetmap.org";

    fn fake_on(server: &str) -> (Fake, Runner) {
        let idle = IdleTracker::new();
        (
            Fake {
                frames_waited: 0,
                idle_after_frame: u32::MAX,
                idle: idle.clone(),
                mode: crate::script::EditMode::Select,
                selected: None,
                server: server.to_string(),
                calls: Vec::new(),
                busy_checks: 0,
                check_result: Ok(()),
                signed_in: true,
                sign_in_failure: None,
            },
            Runner {
                idle,
                step_delay: Duration::ZERO,
            },
        )
    }

    fn steps(src: &str) -> Vec<Step> {
        crate::script::parse(src).unwrap()
    }

    #[test]
    fn upload_ops_refuse_to_run_unless_the_server_is_the_dev_server() {
        for server in [
            "https://api.openstreetmap.org",
            "https://www.openstreetmap.org",
            "http://127.0.0.1:9",
            // Look-alikes of the dev host.
            "https://master.apis.dev.openstreetmap.org.evil.example",
            "https://evil.example/master.apis.dev.openstreetmap.org",
            "https://master.apis.dev.openstreetmap.org@evil.example",
            "https://xmaster.apis.dev.openstreetmap.org",
        ] {
            for line in [
                "upload_confirm",
                "resolve_conflict use_theirs",
                "resolve_conflict keep_mine",
                "sign_in",
            ] {
                let (mut fake, runner) = fake_on(server);
                let e = runner.run(&mut fake, &steps(line)).unwrap_err();
                assert!(
                    e.message.contains("refusing to run"),
                    "{line} on {server}: {}",
                    e.message
                );
                assert!(e.message.contains(server), "{}", e.message);
                // Nothing reached the app.
                assert!(
                    fake.calls.is_empty(),
                    "{line} on {server} reached the app: {:?}",
                    fake.calls
                );
            }
        }
    }

    #[test]
    fn scripted_note_writes_are_refused_unless_the_server_is_the_dev_server() {
        for server in [
            "https://api.openstreetmap.org",
            "https://www.openstreetmap.org",
            "http://127.0.0.1:9",
            "https://master.apis.dev.openstreetmap.org.evil.example",
            "https://master.apis.dev.openstreetmap.org@evil.example",
        ] {
            for line in [
                "note_comment hello",
                "note_close",
                "note_close done",
                "note_reopen",
                "new_note -48 -123 test",
            ] {
                let (mut fake, runner) = fake_on(server);
                let e = runner.run(&mut fake, &steps(line)).unwrap_err();
                assert!(
                    e.message.contains("refusing to run") && e.message.contains(server),
                    "{line} on {server}: {}",
                    e.message
                );
                assert!(
                    fake.calls.is_empty(),
                    "{line} on {server} reached the app: {:?}",
                    fake.calls
                );
            }
        }
    }

    #[test]
    fn a_left_drag_arrives_presses_moves_and_releases_in_that_order() {
        let (mut fake, runner) = fake_on(DEV);
        runner
            .run(
                &mut fake,
                &steps("drag 1,2 geo:40,-74 button=left alt=true"),
            )
            .unwrap();
        let alt = "Mods { alt: true, ctrl: false, shift: false, cmd: false }";
        assert_eq!(
            fake.calls,
            vec![
                format!("pointer Hover true {alt}"),
                format!("pointer Press true {alt}"),
                format!("pointer HeldMove true {alt}"),
                format!("pointer Release true {alt}"),
            ]
        );
        // Every event had frames to take effect in.
        assert!(fake.frames_waited >= 8, "{}", fake.frames_waited);
    }

    #[test]
    fn a_pan_drag_is_one_call() {
        let (mut fake, runner) = fake_on(DEV);
        runner.run(&mut fake, &steps("drag 1,2 3,4")).unwrap();
        assert_eq!(fake.calls, vec!["pan".to_string()]);
    }

    #[test]
    fn a_split_drag_can_be_checked_between_its_steps() {
        let (mut fake, runner) = fake_on(DEV);
        runner
            .run(
                &mut fake,
                &steps(
                    "drag_begin 1,2\ndrag_move 3,4\nassert_snap way 10 segment 0\n\
                     capture x.png\ndrag_end alt=true",
                ),
            )
            .unwrap();
        let kinds: Vec<&str> = fake
            .calls
            .iter()
            .map(|c| c.split_whitespace().next().unwrap())
            .collect();
        // The pointer arrives (hover), then the press, the move, the check,
        // and the release.
        assert_eq!(kinds, ["pointer", "pointer", "pointer", "check", "pointer"]);
        assert!(fake.calls[0].starts_with("pointer Hover true"));
        assert!(fake.calls[1].starts_with("pointer Press true"));
        assert!(fake.calls[3].contains("Snap(Way { way: 10, segment: 0 })"));
        // `drag_end` with no position releases where the pointer is.
        assert!(fake.calls[4].starts_with("pointer Release false"));
    }

    #[test]
    fn modifier_ops_and_inline_modifiers_reach_the_app() {
        let (mut fake, runner) = fake_on(DEV);
        runner
            .run(
                &mut fake,
                &steps("mod_down alt\nhover 1,2\nclick 3,4 shift=true\nmod_up alt"),
            )
            .unwrap();
        assert!(fake.calls[0].starts_with("held true Mods { alt: true"));
        assert!(fake.calls[1].starts_with("pointer Hover true"));
        assert!(fake.calls[2].contains("shift: true"));
        assert!(fake.calls[3].starts_with("held false Mods { alt: true"));
    }

    #[test]
    fn the_new_asserts_reach_the_app_and_a_failure_names_its_line() {
        let (mut fake, runner) = fake_on(DEV);
        runner
            .run(
                &mut fake,
                &steps(
                    "assert_snap none\nassert_snap node 5\nassert_way_nodes 10 1 2\n\
                     assert_node_shared 2 10",
                ),
            )
            .unwrap();
        assert_eq!(fake.calls.len(), 4);
        assert!(fake.calls[3].contains("NodeShared { node: 2, ways: [10] }"));
        fake.check_result = Err("nope".into());
        let e = runner
            .run(&mut fake, &steps("log hi\nassert_way_nodes 10 1"))
            .unwrap_err();
        assert_eq!(e.line_no, 2);
        assert!(e.message.starts_with("assert_way_nodes:"), "{}", e.message);
    }

    #[test]
    fn note_writes_run_on_the_dev_server_and_wait_for_the_write() {
        let (mut fake, runner) = fake_on(DEV);
        runner
            .run(
                &mut fake,
                &steps("note_comment hi\nassert_note_status open"),
            )
            .unwrap();
        assert!(fake.calls.contains(&"note_op note_comment".to_string()));
        assert!(fake.calls.contains(&"check Note(Idle)".to_string()));
    }

    #[test]
    fn reading_notes_is_allowed_on_any_server() {
        let (mut fake, runner) = fake_on("https://api.openstreetmap.org");
        runner
            .run(
                &mut fake,
                &steps("show_notes on\nload_notes_file x.xml\nopen_note 5\nassert_notes 2"),
            )
            .unwrap();
        assert_eq!(
            fake.calls
                .iter()
                .filter(|c| c.starts_with("note_op "))
                .count(),
            3
        );
        // Turning notes on waits for the fetch; loading a file and opening a
        // note are immediate.
        assert_eq!(
            fake.calls
                .iter()
                .filter(|c| *c == "check Note(Idle)")
                .count(),
            1
        );
    }

    #[test]
    fn upload_ops_run_on_the_dev_server() {
        for server in [DEV, "https://master.apis.dev.openstreetmap.org/"] {
            let (mut fake, runner) = fake_on(server);
            runner
                .run(
                    &mut fake,
                    &steps("upload_confirm\nresolve_conflict use_theirs\nsign_in"),
                )
                .unwrap();
            assert_eq!(
                fake.calls,
                vec!["upload_confirm", "resolve UseTheirs", "sign_in"]
            );
        }
    }

    #[test]
    fn autosave_ops_reach_the_app_and_wait_for_it_to_settle() {
        let (mut fake, runner) = fake_on("https://api.openstreetmap.org");
        runner
            .run(
                &mut fake,
                &steps(
                    "autosave_now\nassert_autosave_exists 2\nrestore_autosave\ndiscard_autosave\ncrash",
                ),
            )
            .unwrap();
        let autosave: Vec<&String> = fake
            .calls
            .iter()
            .filter(|c| c.starts_with("autosave "))
            .collect();
        assert_eq!(
            autosave,
            [
                "autosave Now",
                "autosave Restore",
                "autosave Discard",
                "autosave Crash"
            ]
        );
        assert!(fake
            .calls
            .contains(&"check AutosaveFiles(Some(2))".to_string()));
        // Each waiting op looked at the settled state before moving on.
        assert_eq!(
            fake.calls
                .iter()
                .filter(|c| *c == "check AutosaveIdle")
                .count(),
            3
        );
    }

    #[test]
    fn ops_that_do_not_upload_are_not_guarded() {
        let (mut fake, runner) = fake_on("https://api.openstreetmap.org");
        runner
            .run(
                &mut fake,
                &steps("set_tag name x\nassert_pending 0\nassert_dialog none\naction UploadToOsm"),
            )
            .unwrap();
    }

    #[test]
    fn delete_layer_dialog_ops_reach_the_app_on_any_server() {
        let (mut fake, runner) = fake_on("https://api.openstreetmap.org");
        runner
            .run(
                &mut fake,
                &steps("assert_dialog delete_layer\ncancel_delete_layer\nconfirm_delete_layer"),
            )
            .unwrap();
        assert_eq!(
            fake.calls[1..],
            ["cancel_delete_layer", "confirm_delete_layer"]
        );
    }

    #[test]
    fn the_guard_is_a_plain_function_of_op_and_server() {
        assert!(upload_guard(&Op::UploadConfirm, DEV).is_ok());
        assert!(upload_guard(&Op::UploadConfirm, "https://api.openstreetmap.org").is_err());
        assert!(upload_guard(&Op::AssertSignedIn, "https://api.openstreetmap.org").is_ok());
    }

    #[test]
    fn text_ops_reach_the_app() {
        let (mut fake, runner) = fake_on(DEV);
        runner
            .run(
                &mut fake,
                &steps(
                    "type osm-gpui live test: x\nset_comment c d\nupload_cancel\nset_tag name A & B\nremove_tag name\nexport_layer out/a.osm\npause 1ms hello",
                ),
            )
            .unwrap();
        assert_eq!(
            fake.calls,
            vec![
                "type osm-gpui live test: x",
                "set_comment c d",
                "upload_cancel",
                "set_tag name=A & B",
                "remove_tag name",
                "export out/a.osm",
                "note hello",
            ]
        );
    }

    #[test]
    fn assertions_pass_their_checks_to_the_app_and_report_failures() {
        let (mut fake, runner) = fake_on(DEV);
        runner
            .run(
                &mut fake,
                &steps("assert_pending 3\nassert_status_contains Uploaded\nassert_selection way\nassert_dialog conflict version_mismatch node"),
            )
            .unwrap();
        assert_eq!(fake.calls.len(), 4);
        assert!(fake.calls[0].contains("Pending(3)"));
        assert!(fake.calls[3].contains("VersionMismatch"));

        let (mut fake, runner) = fake_on(DEV);
        fake.check_result = Err("found 2".into());
        let e = runner
            .run(&mut fake, &steps("\nassert_pending 0"))
            .unwrap_err();
        assert_eq!(e.line_no, 2);
        assert!(e.message.contains("assert_pending"));
        assert!(e.message.contains("found 2"));
    }

    #[test]
    fn wait_transfer_waits_for_idle_and_times_out() {
        let (mut fake, runner) = fake_on(DEV);
        fake.busy_checks = 3;
        runner.run(&mut fake, &steps("wait_transfer 5s")).unwrap();
        assert_eq!(fake.busy_checks, 0);

        let (mut fake, runner) = fake_on(DEV);
        fake.busy_checks = u32::MAX;
        let e = runner
            .run(&mut fake, &steps("wait_transfer 150ms"))
            .unwrap_err();
        assert!(e.message.contains("wait_transfer timed out"));
    }

    #[test]
    fn capture_while_transferring_numbers_its_files() {
        let (mut fake, runner) = fake_on(DEV);
        fake.busy_checks = 2;
        runner
            .run(
                &mut fake,
                &steps("capture_while_transferring out/p every=1ms timeout=5s"),
            )
            .unwrap();
        // Two busy checks -> two captures (the Fake's capture is a no-op).
        assert_eq!(fake.busy_checks, 0);
    }

    #[test]
    fn assert_server_dev_checks_the_host_exactly() {
        let (mut fake, runner) = fake_on(DEV);
        runner.run(&mut fake, &steps("assert_server dev")).unwrap();
        for server in [
            "https://api.openstreetmap.org",
            "https://master.apis.dev.openstreetmap.org.evil.example",
        ] {
            let (mut fake, runner) = fake_on(server);
            let e = runner
                .run(&mut fake, &steps("assert_server dev"))
                .unwrap_err();
            assert!(e.message.contains("--api-server dev"), "{}", e.message);
        }
    }

    #[test]
    fn assert_signed_in_and_wait_signed_in() {
        let (mut fake, runner) = fake_on(DEV);
        fake.signed_in = false;
        let e = runner
            .run(&mut fake, &steps("assert_signed_in"))
            .unwrap_err();
        assert!(e.message.contains("not signed in"));
        assert!(e.message.contains("00-sign-in"));
        let e = runner
            .run(&mut fake, &steps("wait_signed_in 300ms"))
            .unwrap_err();
        assert!(e.message.contains("timed out"));
        fake.signed_in = true;
        runner
            .run(&mut fake, &steps("assert_signed_in\nwait_signed_in 1s"))
            .unwrap();
    }

    #[test]
    fn the_step_delay_applies_after_every_op() {
        let (mut fake, mut runner) = fake_on(DEV);
        runner.step_delay = Duration::from_millis(40);
        let started = Instant::now();
        runner
            .run(&mut fake, &steps("log a\nlog b\nlog c"))
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(120));
    }

    #[test]
    fn wait_idle_requires_two_consecutive_idle_frames() {
        let idle = IdleTracker::new();
        let mut fake = Fake {
            frames_waited: 0,
            idle_after_frame: 3,
            idle: idle.clone(),
            mode: crate::script::EditMode::Select,
            selected: None,
            server: DEV.to_string(),
            calls: Vec::new(),
            busy_checks: 0,
            check_result: Ok(()),
            signed_in: true,
            sign_in_failure: None,
        };
        let runner = Runner {
            idle,
            step_delay: Duration::ZERO,
        };
        runner
            .wait_idle(&mut fake, Duration::from_secs(5), 1)
            .unwrap();
        assert!(
            fake.frames_waited >= 4,
            "should wait at least one extra frame after idle"
        );
    }

    #[test]
    fn wait_idle_times_out() {
        let idle = IdleTracker::new();
        idle.tile_fetch_started();
        let mut fake = Fake {
            frames_waited: 0,
            idle_after_frame: u32::MAX,
            idle: idle.clone(),
            mode: crate::script::EditMode::Select,
            selected: None,
            server: DEV.to_string(),
            calls: Vec::new(),
            busy_checks: 0,
            check_result: Ok(()),
            signed_in: true,
            sign_in_failure: None,
        };
        let runner = Runner {
            idle,
            step_delay: Duration::ZERO,
        };
        let e = runner
            .wait_idle(&mut fake, Duration::from_millis(50), 7)
            .unwrap_err();
        assert_eq!(e.line_no, 7);
        assert!(e.message.contains("timed out"));
    }

    #[test]
    fn assert_mode_passes_when_matching() {
        let idle = IdleTracker::new();
        let mut fake = Fake {
            frames_waited: 0,
            idle_after_frame: u32::MAX,
            idle: idle.clone(),
            mode: crate::script::EditMode::Add,
            selected: None,
            server: DEV.to_string(),
            calls: Vec::new(),
            busy_checks: 0,
            check_result: Ok(()),
            signed_in: true,
            sign_in_failure: None,
        };
        let runner = Runner {
            idle,
            step_delay: Duration::ZERO,
        };
        let steps = vec![Step {
            line_no: 1,
            op: Op::AssertMode {
                mode: crate::script::EditMode::Add,
            },
        }];
        runner.run(&mut fake, &steps).unwrap();
    }

    #[test]
    fn assert_mode_fails_with_message_when_not_matching() {
        let idle = IdleTracker::new();
        let mut fake = Fake {
            frames_waited: 0,
            idle_after_frame: u32::MAX,
            idle: idle.clone(),
            mode: crate::script::EditMode::Select,
            selected: None,
            server: DEV.to_string(),
            calls: Vec::new(),
            busy_checks: 0,
            check_result: Ok(()),
            signed_in: true,
            sign_in_failure: None,
        };
        let runner = Runner {
            idle,
            step_delay: Duration::ZERO,
        };
        let steps = vec![Step {
            line_no: 4,
            op: Op::AssertMode {
                mode: crate::script::EditMode::Building,
            },
        }];
        let e = runner.run(&mut fake, &steps).unwrap_err();
        assert_eq!(e.line_no, 4);
        assert!(e.message.contains("assert_mode"));
    }

    #[test]
    fn assert_selected_passes_when_matching() {
        let idle = IdleTracker::new();
        let mut fake = Fake {
            frames_waited: 0,
            idle_after_frame: u32::MAX,
            idle: idle.clone(),
            mode: crate::script::EditMode::Select,
            selected: Some((crate::script::FeatureKind::Way, 10)),
            server: DEV.to_string(),
            calls: Vec::new(),
            busy_checks: 0,
            check_result: Ok(()),
            signed_in: true,
            sign_in_failure: None,
        };
        let runner = Runner {
            idle,
            step_delay: Duration::ZERO,
        };
        let steps = vec![Step {
            line_no: 1,
            op: Op::AssertSelected {
                feature: Some((crate::script::FeatureKind::Way, 10)),
            },
        }];
        runner.run(&mut fake, &steps).unwrap();
    }

    #[test]
    fn assert_selected_fails_with_message_when_not_matching() {
        let idle = IdleTracker::new();
        let mut fake = Fake {
            frames_waited: 0,
            idle_after_frame: u32::MAX,
            idle: idle.clone(),
            mode: crate::script::EditMode::Select,
            selected: None,
            server: DEV.to_string(),
            calls: Vec::new(),
            busy_checks: 0,
            check_result: Ok(()),
            signed_in: true,
            sign_in_failure: None,
        };
        let runner = Runner {
            idle,
            step_delay: Duration::ZERO,
        };
        let steps = vec![Step {
            line_no: 9,
            op: Op::AssertSelected {
                feature: Some((crate::script::FeatureKind::Way, 10)),
            },
        }];
        let e = runner.run(&mut fake, &steps).unwrap_err();
        assert_eq!(e.line_no, 9);
        assert!(e.message.contains("assert_selected"));
    }
}
