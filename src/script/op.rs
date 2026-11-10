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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EditMode {
    Select,
    Add,
    Building,
    Extrude,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FeatureKind {
    Node,
    Way,
    Relation,
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
    Capture {
        path: String,
    },
    Log {
        message: String,
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub line_no: usize,
    pub op: Op,
}
