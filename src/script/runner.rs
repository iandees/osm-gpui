//! Executes parsed script steps against the live app.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::capture;
use crate::idle_tracker::IdleTracker;
use crate::script::{Op, Step};

/// Interface to the live app. Implemented by a shim in main.rs that holds the
/// gpui `WindowHandle` and the `MapViewer` entity. Keeps the runner testable
/// and free of gpui imports.
pub trait AppHandle {
    fn set_window_size(&mut self, w: u32, h: u32);
    fn set_viewport(&mut self, lat: f64, lon: f64, zoom: f32);
    fn dispatch_drag(&mut self, from: (f32, f32), to: (f32, f32), duration: Duration);
    fn dispatch_click(&mut self, at: (f32, f32), button: crate::script::MouseButton);
    fn dispatch_scroll(&mut self, at: (f32, f32), dx: f32, dy: f32);
    fn dispatch_key(&mut self, chord: &crate::script::Chord);
    /// Yield until the next frame has been rendered.
    fn wait_frame(&mut self);
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
    pub window_id: u32,
}

impl Runner {
    pub fn run<A: AppHandle>(&self, app: &mut A, steps: &[Step]) -> Result<(), RunError> {
        for (i, step) in steps.iter().enumerate() {
            let started = Instant::now();
            println!("step {}: {}", i + 1, describe(&step.op));
            self.run_step(app, step)?;
            println!("  ok ({}ms)", started.elapsed().as_millis());
        }
        Ok(())
    }

    fn run_step<A: AppHandle>(&self, app: &mut A, step: &Step) -> Result<(), RunError> {
        match &step.op {
            Op::Window { w, h } => { app.set_window_size(*w, *h); Ok(()) }
            Op::Viewport { lat, lon, zoom } => { app.set_viewport(*lat, *lon, *zoom); Ok(()) }
            Op::Wait { duration } => { std::thread::sleep(*duration); Ok(()) }
            Op::WaitIdle { timeout } => self.wait_idle(app, *timeout, step.line_no),
            Op::Drag { from, to, duration } => {
                app.dispatch_drag((from.x, from.y), (to.x, to.y), *duration);
                Ok(())
            }
            Op::Click { at, button } => { app.dispatch_click((at.x, at.y), *button); Ok(()) }
            Op::Scroll { at, dx, dy } => { app.dispatch_scroll((at.x, at.y), *dx, *dy); Ok(()) }
            Op::Key { chord } => { app.dispatch_key(chord); Ok(()) }
            Op::Capture { path } => {
                let pb = PathBuf::from(path);
                capture::capture(self.window_id, &pb)
                    .map_err(|e| RunError { line_no: step.line_no, message: format!("capture: {}", e) })?;
                println!("  -> {}", path);
                Ok(())
            }
            Op::Log { message } => { println!("{}", message); Ok(()) }
            Op::LoadOsm { .. } => panic!("load_osm: not yet wired (Task 10)"),
        }
    }

    fn wait_idle<A: AppHandle>(&self, app: &mut A, timeout: Duration, line_no: usize) -> Result<(), RunError> {
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
                    if consecutive_idle >= 2 { return Ok(()); }
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
        Op::Drag { from, to, duration } => format!("drag {:?} -> {:?} ({:?})", from, to, duration),
        Op::Click { at, button } => format!("click {:?} {:?}", at, button),
        Op::Scroll { at, dx, dy } => format!("scroll {:?} dx={} dy={}", at, dx, dy),
        Op::Key { chord } => format!("key {:?}", chord),
        Op::Capture { path } => format!("capture {}", path),
        Op::Log { message } => format!("log {}", message),
        Op::LoadOsm { .. } => panic!("load_osm: not yet wired (Task 10)"),
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
    }

    impl AppHandle for Fake {
        fn set_window_size(&mut self, _w: u32, _h: u32) {}
        fn set_viewport(&mut self, _lat: f64, _lon: f64, _zoom: f32) {}
        fn dispatch_drag(&mut self, _: (f32, f32), _: (f32, f32), _: Duration) {}
        fn dispatch_click(&mut self, _: (f32, f32), _: MouseButton) {}
        fn dispatch_scroll(&mut self, _: (f32, f32), _: f32, _: f32) {}
        fn dispatch_key(&mut self, _: &Chord) {}
        fn wait_frame(&mut self) {
            self.frames_waited += 1;
            if self.frames_waited == 1 {
                self.idle.tile_fetch_started();
            }
            if self.frames_waited == self.idle_after_frame {
                self.idle.tile_fetch_finished();
            }
        }
    }

    #[test]
    fn wait_idle_requires_two_consecutive_idle_frames() {
        let idle = IdleTracker::new();
        let mut fake = Fake { frames_waited: 0, idle_after_frame: 3, idle: idle.clone() };
        let runner = Runner { idle, window_id: 0 };
        runner.wait_idle(&mut fake, Duration::from_secs(5), 1).unwrap();
        assert!(fake.frames_waited >= 4, "should wait at least one extra frame after idle");
    }

    #[test]
    fn wait_idle_times_out() {
        let idle = IdleTracker::new();
        idle.tile_fetch_started();
        let mut fake = Fake { frames_waited: 0, idle_after_frame: u32::MAX, idle: idle.clone() };
        let runner = Runner { idle, window_id: 0 };
        let e = runner.wait_idle(&mut fake, Duration::from_millis(50), 7).unwrap_err();
        assert_eq!(e.line_no, 7);
        assert!(e.message.contains("timed out"));
    }
}
