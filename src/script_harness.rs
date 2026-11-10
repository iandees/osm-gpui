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

use crate::MapViewer;

// Set to true while a script runner thread is active
pub(crate) static SCRIPT_ACTIVE: std::sync::atomic::AtomicBool =
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
    /// Compare `MapViewer.mode` against `want`.
    AssertMode(crate::EditMode),
    /// Compare `MapViewer.selected` against `want`.
    AssertSelected(Option<(osm_gpui::script::FeatureKind, i64)>),
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
                } => {
                    // A left-button press-drag-release, dispatched as real
                    // events so the current edit mode's handlers see it.
                    window.defer(cx, move |window, cx| {
                        let (from, to) = (point(px(from.0), px(from.1)), point(px(to.0), px(to.1)));
                        let modifiers = gpui::Modifiers::none();
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
            }
        }

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

    fn dispatch_drag(&mut self, from: (f32, f32), to: (f32, f32), _duration: Duration, left: bool) {
        // Submit as a single command; the render fn handles the full down/move/up.
        self.bus.submit(ScriptCommand::Drag { from, to, left });
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
        // Wait for next frame so gpui processes the keystroke.
        self.bus.wait_frame();
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
}
