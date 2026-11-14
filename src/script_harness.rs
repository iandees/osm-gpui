//! Cross-thread scripting plumbing: the script runner runs on a background
//! thread and drives the live app on the gpui main thread via `ScriptBus`.

use gpui::{
    point, px, Context, Keystroke, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollDelta,
    ScrollWheelEvent, Window,
};
use std::sync::{Arc, Condvar, Mutex};

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

// Script commands travel from the background runner thread to the gpui main
// thread.
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
    SetViewport {
        lat: f64,
        lon: f64,
        zoom: f64,
    },
    SetWindowSize {
        w: u32,
        h: u32,
    },
    PanDrag {
        from: script::Where,
        to: script::Where,
    },
    Click {
        at: script::Where,
        right: bool,
        count: u8,
        mods: script::Mods,
    },
    Scroll {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
    },
    /// One left-button mouse event: a hover, the press, a move with the button
    /// held, or the release (`at` of `None`: where the pointer is now)
    Pointer {
        kind: script::PointerKind,
        at: Option<script::Where>,
        mods: script::Mods,
    },
    /// Press or release modifier keys by themselves
    Modifiers {
        mods: script::Mods,
        down: bool,
    },
    /// Render the current frame in-process and save it as a PNG at `path`
    Capture {
        path: std::path::PathBuf,
    },
    /// Add a parsed OSM dataset as a new layer (parsing itself happens on
    /// the runner thread, before this is submitted — see `load_osm` below).
    /// The data and the file's recorded pending edits are boxed to keep
    /// this enum small.
    LoadOsm {
        name: String,
        loaded: Box<(OsmData, osm_gpui::osm::FileEdits)>,
    },
    /// Add a parsed GPX / GeoJSON file as a read-only reference layer.
    LoadReference {
        name: String,
        data: Box<osm_gpui::reference::ReferenceData>,
    },
    /// Dispatch the named action the way a native menu item does (see
    /// `script::action::resolve_action` for the accepted names).
    Action {
        name: String,
    },
    /// Compare `MapViewer.mode` against this mode.
    AssertMode(crate::EditMode),
    /// Compare `MapViewer.selected` against this feature (`None`: not exactly
    /// one selected).
    AssertSelected(Option<(osm_gpui::script::FeatureKind, i64)>),
    /// Set a tag on the selection (the side panel's Add tag path).
    SetTag {
        key: String,
        value: String,
    },
    /// Remove a tag from the selection.
    RemoveTag {
        key: String,
    },
    /// Press the upload dialog's Upload / Try again button.
    UploadConfirm,
    /// Replace the upload dialog's changeset comment.
    SetComment(String),
    /// Press the upload dialog's Cancel button.
    UploadCancel,
    /// A changeset-extras op on the upload dialog (never uploads).
    UploadDialog(script::UploadDialogCmd),
    /// Press the delete-layer confirmation's Delete Layer (`true`) or Cancel.
    DeleteLayerDialog {
        confirm: bool,
    },
    /// Press one of the conflict dialog's buttons.
    Resolve(script::ConflictChoice),
    /// Write the active layer to this `.osm` path.
    Export {
        path: std::path::PathBuf,
    },
    /// An autosave op (write now, press Restore or Discard).
    Autosave(script::AutosaveCmd),
    /// An object history op (open the view, feed it a canned response).
    History(script::HistoryCmd),
    /// Evaluate a read-only condition.
    Check(script::Check),
    /// Click a validator finding's row in the side panel.
    ShowFinding(script::FindingTarget),
    /// Expand or collapse a side panel section.
    SetPanelOpen(script::PanelSection, bool),
    /// Show a message in the status bar.
    Note(String),
    /// An OSM notes op (`show_notes`, `note_close`, ...).
    NoteOp(script::NoteOp),
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
    /// Result of the most recently processed command that can fail (every
    /// one but `Capture`, which has its own slot). One slot is enough:
    /// `submit` blocks until the pending command is consumed, so only one
    /// command is ever in flight.
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

    /// Called by MapViewer::render when handling any command but `Capture`,
    /// before `signal_done_and_frame` wakes the waiting runner thread.
    fn set_assert_result(&self, result: Result<(), String>) {
        *self.assert_result.lock().unwrap() = Some(result);
    }

    /// Called by the runner thread after `submit` returns for a command that
    /// reports a result.
    fn take_assert_result(&self) -> Result<(), String> {
        self.assert_result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Err("assert: no result recorded".to_string()))
    }
}

/// Modifier keys a script holds with `mod_down`. Every synthesized pointer
/// event carries these along with the modifiers given on the op itself.
static HELD_MODS: Mutex<script::Mods> = Mutex::new(script::Mods {
    alt: false,
    ctrl: false,
    shift: false,
    cmd: false,
});

/// Where the last scripted pointer event was, in window pixels.
static LAST_POINTER: Mutex<Option<gpui::Point<gpui::Pixels>>> = Mutex::new(None);

/// The modifiers an event is sent with: those held plus those given inline.
fn script_modifiers(inline: script::Mods) -> gpui::Modifiers {
    let m = HELD_MODS.lock().unwrap().union(inline);
    gpui::Modifiers {
        control: m.ctrl,
        alt: m.alt,
        shift: m.shift,
        platform: m.cmd,
        function: false,
    }
}

pub(crate) static SCRIPT_BUS: std::sync::OnceLock<Arc<ScriptBus>> = std::sync::OnceLock::new();

/// Keystrokes queued by the runner thread, drained each frame by the main thread.
pub(crate) static KEYSTROKE_QUEUE: std::sync::OnceLock<Arc<Mutex<Vec<Keystroke>>>> =
    std::sync::OnceLock::new();

impl MapViewer {
    /// Window pixels for a script position (see `script::Where`).
    pub(crate) fn script_window_point(&self, at: script::Where) -> gpui::Point<gpui::Pixels> {
        let to_window = |p: gpui::Point<gpui::Pixels>| {
            point(
                p.x + px(MapViewer::MODE_PANEL_WIDTH),
                p.y + crate::menu::top_chrome_height(),
            )
        };
        match at {
            script::Where::Window(p) => point(px(p.x), px(p.y)),
            script::Where::Map(p) => to_window(point(px(p.x), px(p.y))),
            script::Where::Geo { lat, lon } => to_window(self.viewport.geo_to_screen(lat, lon)),
        }
    }

    /// `assert_snap`: what the snap highlight targets now.
    pub(crate) fn script_check_snap(&self, want: script::SnapExpect) -> Result<(), String> {
        use crate::SnapTarget;
        use script::SnapExpect;
        let got = match &self.snap_preview {
            None => SnapExpect::None,
            Some(SnapTarget::Node { id, .. }) => SnapExpect::Node(*id),
            Some(SnapTarget::Way { way_id, index, .. }) => SnapExpect::Way {
                way: *way_id,
                segment: *index,
            },
        };
        if got == want {
            Ok(())
        } else {
            Err(format!("expected snap {want:?}, the highlight is {got:?}"))
        }
    }

    fn script_active_editable(&self) -> Result<&dyn osm_gpui::layers::EditableLayer, String> {
        self.active_layer
            .and_then(|id| self.layer_manager.find_layer(id))
            .and_then(|l| l.as_editable())
            .ok_or_else(|| "no editable layer is active".to_string())
    }

    /// `assert_way_nodes`: the way's nodes, in order, in the active layer.
    pub(crate) fn script_check_way_nodes(&self, way: i64, want: &[i64]) -> Result<(), String> {
        let got = self
            .script_active_editable()?
            .way_node_ids(way)
            .ok_or_else(|| format!("way {way} does not exist"))?;
        if got == want {
            Ok(())
        } else {
            Err(format!("way {way} has nodes {got:?}, expected {want:?}"))
        }
    }

    /// `assert_node_shared`: exactly these ways use the node.
    pub(crate) fn script_check_node_shared(&self, node: i64, want: &[i64]) -> Result<(), String> {
        let editable = self.script_active_editable()?;
        if editable.node_lat_lon(node).is_none() {
            return Err(format!("node {node} does not exist"));
        }
        let mut got = editable.ways_using_node(node);
        got.sort_unstable();
        got.dedup();
        let mut want = want.to_vec();
        want.sort_unstable();
        want.dedup();
        if got == want {
            Ok(())
        } else {
            Err(format!(
                "node {node} is used by ways {got:?}, expected {want:?}"
            ))
        }
    }

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
                    // Ensure tile layer exists (benchmarks set
                    // OSM_GPUI_FRAME_STATS_NO_TILES to measure vector drawing
                    // alone and keep the tile servers out of it).
                    if std::env::var_os("OSM_GPUI_FRAME_STATS_NO_TILES").is_none()
                        && self
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
                ScriptCommand::PanDrag { from, to } => {
                    // A pan-drag (as with the right mouse button): down +
                    // single move + up, all in this one frame.
                    let (from, to) = (self.script_window_point(from), self.script_window_point(to));
                    let start = self.window_to_map(from);
                    self.start_pan_drag(start, cx);
                    let ev = MouseMoveEvent {
                        position: to,
                        pressed_button: Some(gpui::MouseButton::Right),
                        modifiers: gpui::Modifiers::none(),
                    };
                    self.handle_mouse_move(&ev, cx);
                    self.end_pan_drag(cx);
                }
                ScriptCommand::Click {
                    at,
                    right,
                    count,
                    mods,
                } => {
                    let btn = if right {
                        gpui::MouseButton::Right
                    } else {
                        gpui::MouseButton::Left
                    };
                    let click_count = count as usize;
                    let modifiers = script_modifiers(mods);
                    let position = self.script_window_point(at);
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
                            position,
                            modifiers,
                            click_count,
                            first_mouse: false,
                        };
                        window.dispatch_event(gpui::PlatformInput::MouseDown(down), cx);
                        let up = MouseUpEvent {
                            button: btn,
                            position,
                            modifiers,
                            click_count,
                        };
                        window.dispatch_event(gpui::PlatformInput::MouseUp(up), cx);
                    });
                }
                ScriptCommand::Pointer { kind, at, mods } => {
                    use script::PointerKind;
                    // Where the pointer last was, as this script left it (the
                    // window's own record also moves with real mouse input).
                    let position = match at {
                        Some(at) => self.script_window_point(at),
                        None => LAST_POINTER
                            .lock()
                            .unwrap()
                            .unwrap_or_else(|| window.mouse_position()),
                    };
                    *LAST_POINTER.lock().unwrap() = Some(position);
                    let modifiers = script_modifiers(mods);
                    let left = gpui::MouseButton::Left;
                    let event = match kind {
                        PointerKind::Hover => gpui::PlatformInput::MouseMove(MouseMoveEvent {
                            position,
                            pressed_button: None,
                            modifiers,
                        }),
                        PointerKind::HeldMove => gpui::PlatformInput::MouseMove(MouseMoveEvent {
                            position,
                            pressed_button: Some(left),
                            modifiers,
                        }),
                        PointerKind::Press => gpui::PlatformInput::MouseDown(MouseDownEvent {
                            button: left,
                            position,
                            modifiers,
                            click_count: 1,
                            first_mouse: false,
                        }),
                        PointerKind::Release => gpui::PlatformInput::MouseUp(MouseUpEvent {
                            button: left,
                            position,
                            modifiers,
                            click_count: 1,
                        }),
                    };
                    if kind == PointerKind::Hover {
                        // Applied straight away as well, so the map's hover
                        // state is current even if the window-level dispatch
                        // finds something else under the pointer.
                        let direct = MouseMoveEvent {
                            position,
                            pressed_button: None,
                            modifiers,
                        };
                        self.handle_mouse_move(&direct, cx);
                    }
                    // Dispatched as a real window event (deferred for the
                    // same reason as `Click`), so a drag takes exactly the
                    // path a person's mouse takes and widgets that open on
                    // hover (menu bar dropdowns, tooltips) see it.
                    window.defer(cx, move |window, cx| {
                        window.dispatch_event(event, cx);
                    });
                }
                ScriptCommand::Modifiers { mods, down } => {
                    let held = {
                        let mut held = HELD_MODS.lock().unwrap();
                        *held = if down {
                            held.union(mods)
                        } else {
                            held.without(mods)
                        };
                        *held
                    };
                    window.defer(cx, move |window, cx| {
                        window.dispatch_event(
                            gpui::PlatformInput::ModifiersChanged(gpui::ModifiersChangedEvent {
                                modifiers: script_modifiers(held),
                                capslock: gpui::Capslock::default(),
                            }),
                            cx,
                        );
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
                    let over_side_panel =
                        x >= window.viewport_size().width.as_f32() - self.side_panel_width;
                    if over_side_panel {
                        // Over the side panel: a real wheel event, so its
                        // scroll areas scroll instead of the map zooming.
                        window.defer(cx, move |window, cx| {
                            window.dispatch_event(gpui::PlatformInput::ScrollWheel(ev), cx);
                        });
                    } else {
                        self.handle_scroll(&ev, cx);
                    }
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
                ScriptCommand::LoadReference { name, data } => {
                    self.add_reference_dataset(name, *data, cx);
                }
                ScriptCommand::Action { name } => {
                    use script::action::{resolve_action, unhandled_message, ActionTarget};
                    let result = match resolve_action(&name, cx.all_action_names()) {
                        // Benchmark knob for the street-name labels, e.g.
                        // `action LabelBench:on,halo=none,repeat=4` (see
                        // `line_labels::parse_bench`).
                        Ok(ActionTarget::LabelBench) => {
                            // The script parser wants one token: `LabelBench:on,halo=none`.
                            let spec = name
                                .trim_start_matches("LabelBench")
                                .replace([':', ','], " ");
                            let result = osm_gpui::layers::line_labels::set_bench(&spec);
                            cx.notify();
                            result
                        }
                        // Switch the MapCSS style, e.g. `action MapStyle:wireframe`
                        // (`default`, `wireframe` or `id`), in memory only.
                        Ok(ActionTarget::MapStyle(id)) => {
                            let sheet = osm_gpui::style::custom::BUILT_IN_STYLES
                                .iter()
                                .find(|s| s.id == id)
                                .and_then(|s| osm_gpui::style::Stylesheet::parse(s.source).ok());
                            match sheet {
                                Some(sheet) => {
                                    osm_gpui::style::custom::set_active(Some(std::sync::Arc::new(
                                        sheet,
                                    )));
                                    self.apply_map_style(cx);
                                    Ok(())
                                }
                                None => Err(format!("unknown built-in map style '{id}'")),
                            }
                        }
                        Ok(ActionTarget::Registered { qualified, json }) => {
                            let data = json.and_then(|j| serde_json::from_str(&j).ok());
                            match cx.build_action(&qualified, data) {
                                Ok(action) => {
                                    // Asked now, in the frame the runner is
                                    // waiting on, so a missing handler can
                                    // fail the op; the dispatch itself must
                                    // wait. Window handlers are looked up on
                                    // the last rendered frame; the app-level
                                    // check adds global handlers (it cannot
                                    // see this window while it is leased).
                                    if !window.is_action_available(action.as_ref(), cx)
                                        && !cx.is_action_available(action.as_ref())
                                    {
                                        Err(unhandled_message(&qualified))
                                    } else {
                                        // `App::dispatch_action`, deferred until this
                                        // render's window lease is released, is the call
                                        // the macOS menu bar makes: it dispatches to the
                                        // active window's focused element (and on to the
                                        // global handlers).
                                        //
                                        // That needs an active window. A scripted run
                                        // can lose it (a Keychain prompt or another
                                        // app comes to the front), and then
                                        // window-scoped handlers such as Select All
                                        // would never see the action, so in that case
                                        // it is sent to this window directly.
                                        let handle = window.window_handle();
                                        cx.defer(move |cx| {
                                            if cx.active_window() == Some(handle) {
                                                cx.dispatch_action(action.as_ref());
                                            } else {
                                                let _ = handle.update(cx, |_, window, cx| {
                                                    window.dispatch_action(action, cx);
                                                });
                                            }
                                        });
                                        Ok(())
                                    }
                                }
                                Err(e) => Err(format!("unknown action '{name}': {e}")),
                            }
                        }
                        Err(e) => Err(e),
                    };
                    bus.set_assert_result(result);
                }
                ScriptCommand::AssertMode(want) => {
                    let result = if self.mode() == want {
                        Ok(())
                    } else {
                        Err(format!("expected mode {:?}, got {:?}", want, self.mode()))
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
                ScriptCommand::UploadDialog(cmd) => {
                    let result = self.script_upload_dialog(&cmd, window, cx);
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
                ScriptCommand::DeleteLayerDialog { confirm } => {
                    let result = match self.delete_layer_dialog.clone() {
                        Some(dialog) => {
                            // Deferred like the upload dialog's presses: the
                            // dialog's event is handled by this view, which
                            // is being rendered right now.
                            window.defer(cx, move |_window, cx| {
                                dialog.update(cx, |dialog, cx| {
                                    if confirm {
                                        dialog.press_confirm(cx)
                                    } else {
                                        dialog.press_cancel(cx)
                                    }
                                });
                            });
                            Ok(())
                        }
                        None => Err("the delete-layer confirmation is not open".to_string()),
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
                ScriptCommand::Autosave(cmd) => {
                    let result = self.script_autosave(cmd, window, cx);
                    bus.set_assert_result(result);
                }
                ScriptCommand::History(cmd) => {
                    let result = self.script_history(cmd, window, cx);
                    bus.set_assert_result(result);
                }
                ScriptCommand::Check(check) => {
                    let result = self.script_check(&check, cx);
                    bus.set_assert_result(result);
                }
                ScriptCommand::ShowFinding(target) => {
                    let result = self.script_show_finding(&target, window, cx);
                    bus.set_assert_result(result);
                }
                ScriptCommand::SetPanelOpen(section, open) => {
                    self.side_panel_open[panel_section_index(section)] = open;
                    cx.notify();
                    bus.set_assert_result(Ok(()));
                }
                ScriptCommand::Note(message) => {
                    self.set_status(message);
                    cx.notify();
                }
                ScriptCommand::NoteOp(op) => {
                    let result = self.script_note_op(&op, window, cx);
                    bus.set_assert_result(result);
                }
            }
        }

        self.drain_keystrokes_and_animate(window, cx);
    }

    /// Run one of the upload dialog's changeset-extras ops. Nothing here
    /// can upload; recent comments added here stay in memory only.
    fn script_upload_dialog(
        &mut self,
        cmd: &script::UploadDialogCmd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        use script::UploadDialogCmd as Cmd;
        if matches!(cmd, Cmd::OpenPreview) {
            if self.upload_dialog.is_some() {
                return Err("the upload dialog is already open".to_string());
            }
            self.open_upload_dialog_preview(window, cx);
            return if self.upload_dialog.is_some() {
                Ok(())
            } else {
                Err("the preview did not open (a dialog is already showing)".to_string())
            };
        }
        if let Cmd::AddRecentComment(text) = cmd {
            // In memory only: this is a script run, never the settings file.
            osm_gpui::settings_store::update(false, |s| {
                osm_gpui::changeset_tags::push_recent_comment(&mut s.recent_comments, text)
            });
            let recent_comments = osm_gpui::settings_store::snapshot().recent_comments;
            if let Some(dialog) = self.upload_dialog.clone() {
                dialog.update(cx, |d, cx| d.set_recent_comments(recent_comments, cx));
            }
            return Ok(());
        }
        let Some(dialog) = self.upload_dialog.clone() else {
            return Err("the upload dialog is not open".to_string());
        };
        dialog.update(cx, |d, cx| match cmd {
            Cmd::SetSource(text) => {
                d.set_draft_source(text.clone(), window, cx);
                Ok(())
            }
            Cmd::PressSourceChip(value) => {
                d.press_source_chip(value, window, cx);
                Ok(())
            }
            Cmd::SetReviewRequested(on) => {
                d.set_review_requested(*on, cx);
                Ok(())
            }
            Cmd::SetImageryCleared(cleared) => {
                d.set_imagery_cleared(*cleared, cx);
                Ok(())
            }
            Cmd::ToggleRecentComments => {
                d.toggle_recent_comments(cx);
                Ok(())
            }
            Cmd::PickRecentComment(index) => {
                if d.pick_recent_comment(*index, window, cx) {
                    Ok(())
                } else {
                    Err(format!("there is no recent comment number {index}"))
                }
            }
            Cmd::OpenPreview | Cmd::AddRecentComment(_) => Ok(()),
        })
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

    /// Click a finding's row in the side panel's Validation section (what a
    /// click on it does: select the objects and zoom to them).
    fn script_show_finding(
        &mut self,
        target: &script::FindingTarget,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(report) = self.shown_validation() else {
            return Err("the Validation section is not showing (run `action Validate`)".into());
        };
        let index = match target {
            script::FindingTarget::Index(n) if *n < report.entries.len() => *n,
            script::FindingTarget::Index(n) => {
                return Err(format!(
                    "there is no finding {n} (the list has {})",
                    report.entries.len()
                ))
            }
            script::FindingTarget::Check(id) => report
                .entries
                .iter()
                .position(|e| e.finding.check.id() == id)
                .ok_or_else(|| format!("no finding of check {id}"))?,
        };
        // Run from `render`, on this view itself: no deferral needed.
        self.select_finding(index, cx);
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
        let Some(osm) = layer.as_osm() else {
            return Some(0);
        };
        let (created, modified, deleted) = osm.diff_for_upload().counts();
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
            Check::Snap(want) => self.script_check_snap(*want),
            Check::WayNodes { way, nodes } => self.script_check_way_nodes(*way, nodes),
            Check::NodeShared { node, ways } => self.script_check_node_shared(*node, ways),
            Check::Layer { kind, name } => {
                let kind_of = |layer: &dyn osm_gpui::layers::MapLayer| {
                    use osm_gpui::layers::LayerKind as Kind;
                    use osm_gpui::reference::Format;
                    match layer.kind() {
                        Kind::Reference => match layer.as_reference().map(|r| r.format()) {
                            Some(Format::GeoJson) => Some(script::LayerKind::GeoJson),
                            _ => Some(script::LayerKind::Gpx),
                        },
                        Kind::Osm => Some(script::LayerKind::Osm),
                        Kind::Tile => Some(script::LayerKind::Imagery),
                        Kind::Grid => Some(script::LayerKind::Grid),
                        // The script language has no word for these.
                        Kind::Notes | Kind::StreetImagery => None,
                    }
                };
                let layers = self.layer_manager.layers();
                if layers
                    .iter()
                    .any(|l| l.name() == name && kind_of(l.as_ref()) == Some(*kind))
                {
                    Ok(())
                } else {
                    let found: Vec<String> = layers
                        .iter()
                        .map(|l| format!("{:?} {:?}", l.kind(), l.name()))
                        .collect();
                    Err(format!(
                        "no {kind:?} layer named {name:?}; layers: {}",
                        found.join(", ")
                    ))
                }
            }
            Check::SelectedAt {
                lat,
                lon,
                tolerance_m,
            } => {
                let [only] = self.selected.as_slice() else {
                    return Err(format!(
                        "expected one selected node, found {} selected",
                        self.selected.len()
                    ));
                };
                let pos = (only.kind == osm_gpui::selection::FeatureKind::Node)
                    .then(|| {
                        self.layer_manager
                            .find_layer(only.layer_id)
                            .and_then(|l| l.as_editable())
                            .and_then(|e| e.node_lat_lon(only.id))
                    })
                    .flatten()
                    .ok_or_else(|| "the selected feature is not a node".to_string())?;
                let (ax, ay) = osm_gpui::coordinates::lat_lon_to_mercator(pos.0, pos.1);
                let (bx, by) = osm_gpui::coordinates::lat_lon_to_mercator(*lat, *lon);
                // Mercator metres stretch by 1/cos(lat); undo that.
                let scale = pos.0.to_radians().cos();
                let metres = ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt() * scale;
                if metres <= *tolerance_m {
                    Ok(())
                } else {
                    Err(format!(
                        "the selected node is at {:.7}, {:.7}, {metres:.2} m from {lat}, {lon} \
                         (tolerance {tolerance_m} m)",
                        pos.0, pos.1
                    ))
                }
            }
            Check::ChangesetTag { key, value } => {
                // The tags the dialog would send now; nothing is sent.
                let Some(dialog) = &self.upload_dialog else {
                    return Err("the upload dialog is not open".to_string());
                };
                let tags = dialog.read(cx).changeset_tags(cx);
                match (tags.get(key), value) {
                    (Some(got), Some(want)) if got == want => Ok(()),
                    (Some(got), Some(want)) => {
                        Err(format!("changeset tag {key} is {got:?}, expected {want:?}"))
                    }
                    (None, Some(want)) => Err(format!(
                        "changeset tag {key} is not written, expected {want:?}"
                    )),
                    (Some(got), None) => Err(format!(
                        "changeset tag {key} is written ({got:?}), expected none"
                    )),
                    (None, None) => Ok(()),
                }
            }
            Check::Pending(want) => match self.active_layer_pending() {
                Some(got) if got == *want => Ok(()),
                Some(got) => Err(format!("expected {want} pending edits, found {got}")),
                None => Err("there is no active OSM layer".into()),
            },
            Check::StatusContains(text) => {
                let shown = self
                    .rotate_status()
                    .or_else(|| self.status_message.as_ref().map(|(m, _)| m.clone()))
                    .map(|m| m.to_string())
                    .unwrap_or_default();
                if shown.contains(text.as_str()) {
                    Ok(())
                } else {
                    Err(format!(
                        "status bar says {shown:?}, expected it to contain {text:?}"
                    ))
                }
            }
            Check::ValidationPanel(want) => {
                if self.validation_shown() == *want {
                    Ok(())
                } else if *want {
                    Err("the Validation section is not showing".into())
                } else {
                    Err("the Validation section is showing".into())
                }
            }
            Check::PanelOpen(section, want) => {
                let open = self.side_panel_open[panel_section_index(*section)];
                if open == *want {
                    Ok(())
                } else if *want {
                    Err(format!("the {section:?} section is collapsed"))
                } else {
                    Err(format!("the {section:?} section is expanded"))
                }
            }
            Check::Validation {
                errors,
                warnings,
                checks,
            } => {
                // The section's own list while it is showing, so that waiting
                // for it to refresh after an edit is what is tested.
                let report = self
                    .shown_validation()
                    .cloned()
                    .unwrap_or_else(|| self.validation_report());
                let mut wrong = Vec::new();
                let mut compare = |what: &str, want: usize, got: usize| {
                    if want != got {
                        wrong.push(format!("{what}: expected {want}, found {got}"));
                    }
                };
                if let Some(want) = errors {
                    compare("errors", *want, report.errors());
                }
                if let Some(want) = warnings {
                    compare("warnings", *want, report.warnings());
                }
                for (id, want) in checks {
                    let got = osm_gpui::validator::Check::from_id(id)
                        .map(|c| report.count_of(c))
                        .unwrap_or(0);
                    compare(id, *want, got);
                }
                if wrong.is_empty() {
                    Ok(())
                } else {
                    let found: Vec<String> = report
                        .entries
                        .iter()
                        .map(|e| format!("{} ({})", e.finding.check.id(), e.label))
                        .collect();
                    Err(format!(
                        "{}; findings: {}",
                        wrong.join("; "),
                        found.join(", ")
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
            Check::AutosaveIdle => self.script_autosave_idle(),
            Check::AutosaveFiles(want) => self.script_autosave_files(*want),
            Check::Zoom { level, tolerance } => {
                let got = self.viewport.zoom_level();
                if (got - level).abs() <= *tolerance {
                    Ok(())
                } else {
                    Err(format!(
                        "expected zoom {level} (within {tolerance}), found {got:.4}"
                    ))
                }
            }
            Check::ImageryOffset {
                east,
                north,
                tolerance,
            } => {
                let got = self
                    .current_imagery_offset()
                    .ok_or_else(|| "there is no visible imagery layer".to_string())?;
                if (got.east_m - east).abs() <= *tolerance
                    && (got.north_m - north).abs() <= *tolerance
                {
                    Ok(())
                } else {
                    Err(format!(
                        "expected imagery offset E {east} N {north} (within {tolerance} m), \
                         found E {:.3} N {:.3}",
                        got.east_m, got.north_m
                    ))
                }
            }
            Check::AdjustingOffset(want) => {
                let got = self.offset_adjust.is_some();
                if got == *want {
                    Ok(())
                } else {
                    Err(format!(
                        "expected imagery offset adjusting to be {}, it is {}",
                        if *want { "on" } else { "off" },
                        if got { "on" } else { "off" },
                    ))
                }
            }
            Check::Note(c) => self.script_note_check(c),
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
            Check::NativeAppearance(want) => {
                use gpui_component::ThemeMode;
                let got = match osm_gpui::appearance::native_readback() {
                    None => script::NativeAppearanceWant::System,
                    Some(ThemeMode::Light) => script::NativeAppearanceWant::Light,
                    Some(ThemeMode::Dark) => script::NativeAppearanceWant::Dark,
                };
                if got == *want {
                    Ok(())
                } else {
                    Err(format!(
                        "expected native appearance {want:?}, found {got:?}"
                    ))
                }
            }
            Check::LayerCount(want) => {
                let have = self.layer_manager.layers().len();
                if have == *want {
                    Ok(())
                } else {
                    Err(format!("expected {want} layers, found {have}"))
                }
            }
            Check::SelectedCount(want) => {
                if self.selected.len() == *want {
                    Ok(())
                } else {
                    Err(format!(
                        "expected {want} selected features, found {}",
                        self.selected.len()
                    ))
                }
            }
            Check::FindPreview(text) => match &self.find_dialog {
                None => Err("the Find dialog is not open".into()),
                Some(dialog) => {
                    let shown = dialog.read(cx).preview_text().to_string();
                    if shown.contains(text.as_str()) {
                        Ok(())
                    } else {
                        Err(format!(
                            "the Find dialog says {shown:?}, expected it to contain {text:?}"
                        ))
                    }
                }
            },
            Check::HistoryVersions(want) => self.script_history_versions(*want, cx),
            Check::HistorySettled => self.script_history_settled(cx),
            Check::Dialog(want) => {
                let found = self.describe_dialog(&phase);
                let ok = match (want, &phase) {
                    (DialogExpect::None, _) => found == "no dialog",
                    (DialogExpect::SignIn, _) => self.sign_in_dialog.is_some(),
                    (DialogExpect::Notice, _) => self.notice_dialog.is_some(),
                    (DialogExpect::DeleteLayer, _) => self.delete_layer_dialog.is_some(),
                    (DialogExpect::Restore, _) => self.restore_dialog.is_some(),
                    (DialogExpect::Find, _) => self.find_dialog.is_some(),
                    (DialogExpect::History, _) => self.object_history.view.is_some(),
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
            None if self.delete_layer_dialog.is_some() => "the delete-layer confirmation".into(),
            None if self.restore_dialog.is_some() => "the restore-autosave offer".into(),
            None if self.find_dialog.is_some() => "the Find dialog".into(),
            None if self.object_history.view.is_some() => "the object history view".into(),
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

/// Why a scripted run may not upload to `server`, if it may not (the same
/// rule as `script::runner::upload_guard`, applied where the upload starts).
pub(crate) fn scripted_upload_refusal(server: &str) -> Option<String> {
    if !script_mode() {
        return None;
    }
    script::runner::upload_guard(&script::Op::UploadConfirm, server).err()
}

/// Why a scripted run may not write a note to `server`, if it may not (the
/// same rule as for uploads, applied where the write starts).
pub(crate) fn scripted_note_write_refusal(server: &str) -> Option<String> {
    if !script_mode() {
        return None;
    }
    script::runner::upload_guard(&script::Op::Note(script::NoteOp::Reopen(None)), server).err()
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

/// The `side_panel_open` slot of a section a script can open or close.
fn panel_section_index(section: script::PanelSection) -> usize {
    match section {
        script::PanelSection::Validation => crate::validation_panel::SECTION,
        script::PanelSection::History => crate::side_panel::HISTORY_SECTION,
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

    fn dispatch_pan_drag(&mut self, from: script::Where, to: script::Where) {
        self.bus.submit(ScriptCommand::PanDrag { from, to });
    }

    fn dispatch_click(
        &mut self,
        at: script::Where,
        button: script::MouseButton,
        count: u8,
        mods: script::Mods,
    ) {
        let right = matches!(button, script::MouseButton::Right);
        self.bus.submit(ScriptCommand::Click {
            at,
            right,
            count,
            mods,
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

    fn dispatch_pointer(
        &mut self,
        kind: script::PointerKind,
        at: Option<script::Where>,
        mods: script::Mods,
    ) {
        self.bus.submit(ScriptCommand::Pointer { kind, at, mods });
    }

    fn set_held_modifiers(&mut self, mods: script::Mods, down: bool) {
        self.bus.submit(ScriptCommand::Modifiers { mods, down });
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
        // GPX and GeoJSON become read-only reference layers.
        if let Some(format) = osm_gpui::reference::sniff_path(path) {
            let data = osm_gpui::reference::load_file(path, format).map_err(|e| e.to_string())?;
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Reference")
                .to_string();
            self.bus.submit(ScriptCommand::LoadReference {
                name,
                data: Box::new(data),
            });
            return Ok(());
        }
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

    fn upload_dialog(&mut self, cmd: &script::UploadDialogCmd) -> Result<(), String> {
        self.bus.submit(ScriptCommand::UploadDialog(cmd.clone()));
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

    fn confirm_delete_layer(&mut self) -> Result<(), String> {
        self.bus
            .submit(ScriptCommand::DeleteLayerDialog { confirm: true });
        let result = self.bus.take_assert_result();
        self.bus.wait_frame();
        self.bus.wait_frame();
        result
    }

    fn cancel_delete_layer(&mut self) -> Result<(), String> {
        self.bus
            .submit(ScriptCommand::DeleteLayerDialog { confirm: false });
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

    fn autosave(&mut self, cmd: script::AutosaveCmd) -> Result<(), String> {
        if cmd == script::AutosaveCmd::Crash {
            // As a crash would: no destructors, no quit hooks, no clean-up.
            // The OS drops the session's file lock, which is what marks the
            // session as crashed for the next run.
            eprintln!("crash: exiting without clean-up");
            std::process::exit(3);
        }
        self.bus.submit(ScriptCommand::Autosave(cmd));
        let result = self.bus.take_assert_result();
        self.bus.wait_frame();
        result
    }

    fn history(&mut self, cmd: script::HistoryCmd) -> Result<(), String> {
        self.bus.submit(ScriptCommand::History(cmd));
        let result = self.bus.take_assert_result();
        self.bus.wait_frame();
        result
    }

    fn show_finding(&mut self, target: &script::FindingTarget) -> Result<(), String> {
        self.bus.submit(ScriptCommand::ShowFinding(target.clone()));
        let result = self.bus.take_assert_result();
        self.bus.wait_frame();
        self.bus.wait_frame();
        result
    }

    fn set_panel_open(&mut self, section: script::PanelSection, open: bool) -> Result<(), String> {
        self.bus.submit(ScriptCommand::SetPanelOpen(section, open));
        let result = self.bus.take_assert_result();
        self.bus.wait_frame();
        result
    }

    fn note(&mut self, message: &str) {
        self.bus.submit(ScriptCommand::Note(message.to_string()));
    }

    fn note_op(&mut self, op: &script::NoteOp) -> Result<(), String> {
        self.bus.submit(ScriptCommand::NoteOp(op.clone()));
        let result = self.bus.take_assert_result();
        // Let the panel (opened or updated on the next frame) show.
        self.bus.wait_frame();
        self.bus.wait_frame();
        result
    }

    fn sign_in(&mut self) -> Result<(), String> {
        if SIGN_IN_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err("a sign-in is already waiting for the browser".into());
        }
        *SIGN_IN_FAILURE.lock().unwrap() = None;
        let base_url = settings_store::api_base_url();
        // `auth::login` opens the browser and blocks until the approval comes
        // back (or three minutes pass), so it runs on its own thread.
        let http = osm_gpui::http::default_client();
        std::thread::spawn(move || {
            if let Err(e) = osm_gpui::auth::login(http.as_ref(), &base_url) {
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
