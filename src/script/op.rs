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

#[derive(Debug, Clone, PartialEq)]
pub struct Chord {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Window { w: u32, h: u32 },
    Viewport { lat: f64, lon: f64, zoom: f32 },
    WaitIdle { timeout: Duration },
    Wait { duration: Duration },
    Drag { from: Point2, to: Point2, duration: Duration },
    Click { at: Point2, button: MouseButton },
    Scroll { at: Point2, dx: f32, dy: f32 },
    Key { chord: Chord },
    Capture { path: String },
    Log { message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub line_no: usize,
    pub op: Op,
}
