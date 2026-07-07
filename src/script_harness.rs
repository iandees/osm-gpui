//! Cross-thread scripting plumbing: the script runner runs on a background
//! thread and drives the live app on the gpui main thread via `ScriptBus`.

use gpui::{point, px, Context, Keystroke, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollDelta, ScrollWheelEvent, Window};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use osm_gpui::idle_tracker::IdleTracker;
use osm_gpui::layers::tile_layer::TileLayer;
use osm_gpui::osm::OsmParser;
use osm_gpui::script::{self, runner::AppHandle};

use crate::{MapViewer, SHARED_OSM_DATA};

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
    /// Synthesize a left-button drag (from → to with sleep between steps)
    Drag { from: (f32, f32), to: (f32, f32) },
    /// Synthesize a mouse click
    Click { x: f32, y: f32, right: bool },
    /// Synthesize a scroll event
    Scroll { x: f32, y: f32, dx: f32, dy: f32 },
    /// Render the current frame in-process and save it as a PNG at `path`
    Capture { path: std::path::PathBuf },
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
}

impl ScriptBus {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(None),
            done_cv: Condvar::new(),
            frame_count: Mutex::new(0),
            frame_cv: Condvar::new(),
            capture_result: Mutex::new(None),
        })
    }

    /// Submit a command and block until the main thread has processed it.
    fn submit(&self, cmd: ScriptCommand) {
        {
            let mut lock = self.pending.lock().unwrap();
            *lock = Some(cmd);
        }
        // Wait until the command is consumed.
        let _guard = self.done_cv.wait_while(
            self.pending.lock().unwrap(),
            |opt| opt.is_some(),
        ).unwrap();
    }

    /// Wait until at least one more render frame has completed.
    fn wait_frame(&self) {
        let current = *self.frame_count.lock().unwrap();
        let _guard = self.frame_cv.wait_while(
            self.frame_count.lock().unwrap(),
            |fc| *fc <= current,
        ).unwrap();
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
                    if self.layer_manager.layer_named("OpenStreetMap Carto").is_none() {
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
                ScriptCommand::Drag { from, to } => {
                    // For drag: just do down + single move + up; the sleep between steps
                    // happens in the runner thread, so here we do single events.
                    let ev = MouseDownEvent {
                        button: gpui::MouseButton::Left,
                        position: point(px(from.0), px(from.1)),
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                        first_mouse: false,
                    };
                    self.handle_mouse_down(&ev);
                    let ev = MouseMoveEvent {
                        position: point(px(to.0), px(to.1)),
                        pressed_button: Some(gpui::MouseButton::Left),
                        modifiers: gpui::Modifiers::none(),
                    };
                    self.handle_mouse_move(&ev, cx);
                    let ev = MouseUpEvent {
                        button: gpui::MouseButton::Left,
                        position: point(px(to.0), px(to.1)),
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                    };
                    self.handle_mouse_up(&ev, cx);
                    cx.notify();
                }
                ScriptCommand::Click { x, y, right } => {
                    let btn = if right { gpui::MouseButton::Right } else { gpui::MouseButton::Left };
                    let ev = MouseDownEvent {
                        button: btn,
                        position: point(px(x), px(y)),
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                        first_mouse: false,
                    };
                    self.handle_mouse_down(&ev);
                    let ev = MouseUpEvent {
                        button: btn,
                        position: point(px(x), px(y)),
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                    };
                    self.handle_mouse_up(&ev, cx);
                    cx.notify();
                }
                ScriptCommand::Scroll { x, y, dx, dy } => {
                    let ev = ScrollWheelEvent {
                        position: point(px(x), px(y)),
                        delta: ScrollDelta::Pixels(gpui::Point { x: px(dx), y: px(dy) }),
                        modifiers: gpui::Modifiers::none(),
                        touch_phase: gpui::TouchPhase::Moved,
                    };
                    self.handle_scroll(&ev, cx);
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
            }
        }

        // Also drain keystroke queue (processed via Window so needs to be here)
        if let Some(ks_queue) = KEYSTROKE_QUEUE.get() {
            if let Ok(mut guard) = ks_queue.try_lock() {
                for ks in guard.drain(..) {
                    window.dispatch_keystroke(ks, &mut **cx);
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
        self.bus.submit(ScriptCommand::SetViewport { lat, lon, zoom: zoom as f64 });
    }

    fn dispatch_drag(&mut self, from: (f32, f32), to: (f32, f32), _duration: Duration) {
        // Submit as a single command; the render fn handles the full down/move/up.
        self.bus.submit(ScriptCommand::Drag { from, to });
    }

    fn dispatch_click(&mut self, at: (f32, f32), button: script::MouseButton) {
        let right = matches!(button, script::MouseButton::Right);
        self.bus.submit(ScriptCommand::Click { x: at.0, y: at.1, right });
    }

    fn dispatch_scroll(&mut self, at: (f32, f32), dx: f32, dy: f32) {
        self.bus.submit(ScriptCommand::Scroll { x: at.0, y: at.1, dx, dy });
    }

    fn dispatch_key(&mut self, chord: &script::Chord) {
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
            key_char: None,
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
        let data = parser.parse_file(&path_str).map_err(|e| e.to_string())?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("OSM").to_string();
        if let Some(q) = SHARED_OSM_DATA.get() {
            if let Ok(mut guard) = q.lock() {
                guard.push((stem, data));
            } else {
                return Err("SHARED_OSM_DATA mutex poisoned".into());
            }
        } else {
            return Err("SHARED_OSM_DATA not initialized".into());
        }
        // Thanks to the reorder in render(), the next frame drains the queue
        // before signalling — so after wait_frame the layer exists.
        self.bus.wait_frame();
        Ok(())
    }

    fn capture(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.bus.submit(ScriptCommand::Capture { path: path.to_path_buf() });
        self.bus.take_capture_result()
    }
}
