# Scriptable Screenshot Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `--script <path>` mode to the `osm-gpui` binary that runs a line-oriented script of viewport/input/capture operations against the live app and produces PNG screenshots.

**Architecture:** In-process script runner dispatches synthesized gpui events into the real `Window`. An `IdleTracker` with atomic counters in `tile_cache` and `http_image_loader` powers `wait_idle`. Captures shell out to macOS `screencapture -l <windowid>` using a CGWindow-id lookup by PID.

**Tech Stack:** Rust, gpui, macOS `screencapture`, `core-foundation` / `core-graphics` (already transitively present).

**Spec:** `docs/superpowers/specs/2026-04-12-scriptable-screenshot-harness-design.md`

---

## File Structure

**New files:**
- `src/idle_tracker.rs` — atomic counter struct + unit tests
- `src/script/mod.rs` — public module root, re-exports
- `src/script/op.rs` — `Op` enum and related types (`Chord`, `Point2`, `Duration`)
- `src/script/parser.rs` — line DSL parser + unit tests
- `src/script/runner.rs` — executes parsed ops against the running app
- `src/capture.rs` — window-id lookup + `screencapture` subprocess
- `docs/screenshots/smoke.osmscript` — harness smoke test
- `docs/screenshots/.gitignore` — ignore generated PNGs

**Modified files:**
- `src/lib.rs` — add `pub mod idle_tracker; pub mod script; pub mod capture;`
- `src/tile_cache.rs` — accept `Arc<IdleTracker>`; increment on fetch start, decrement on fetch complete
- `src/http_image_loader.rs` — accept `Arc<IdleTracker>`; increment on HTTP start + decode submit, decrement on complete
- `src/main.rs` — CLI flag parsing (`--script`, `--window-size`, `--keep-open`); construct `IdleTracker`, thread it into `TileCache`/loader, spawn script runner after window creation
- `Cargo.toml` — confirm `core-foundation` and `core-graphics` are available as direct deps (add if transitive-only)

---

## Task 1: IdleTracker

**Files:**
- Create: `src/idle_tracker.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/idle_tracker.rs`:

```rust
//! Tracks outstanding async work so `wait_idle` can know when the map has settled.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct IdleTracker {
    pending_tile_fetches: AtomicUsize,
    pending_image_decodes: AtomicUsize,
}

impl IdleTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn tile_fetch_started(&self) {
        self.pending_tile_fetches.fetch_add(1, Ordering::SeqCst);
    }

    pub fn tile_fetch_finished(&self) {
        let prev = self.pending_tile_fetches.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev > 0, "tile_fetch_finished underflow");
    }

    pub fn image_decode_started(&self) {
        self.pending_image_decodes.fetch_add(1, Ordering::SeqCst);
    }

    pub fn image_decode_finished(&self) {
        let prev = self.pending_image_decodes.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev > 0, "image_decode_finished underflow");
    }

    pub fn is_idle(&self) -> bool {
        self.pending_tile_fetches.load(Ordering::SeqCst) == 0
            && self.pending_image_decodes.load(Ordering::SeqCst) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_is_idle() {
        assert!(IdleTracker::new().is_idle());
    }

    #[test]
    fn tile_fetch_toggles_idle() {
        let t = IdleTracker::new();
        t.tile_fetch_started();
        assert!(!t.is_idle());
        t.tile_fetch_finished();
        assert!(t.is_idle());
    }

    #[test]
    fn image_decode_toggles_idle() {
        let t = IdleTracker::new();
        t.image_decode_started();
        assert!(!t.is_idle());
        t.image_decode_finished();
        assert!(t.is_idle());
    }

    #[test]
    fn both_counters_must_be_zero() {
        let t = IdleTracker::new();
        t.tile_fetch_started();
        t.image_decode_started();
        t.tile_fetch_finished();
        assert!(!t.is_idle());
        t.image_decode_finished();
        assert!(t.is_idle());
    }

    #[test]
    fn concurrent_increments_balance() {
        use std::thread;
        let t = IdleTracker::new();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let t = t.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    t.tile_fetch_started();
                    t.tile_fetch_finished();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(t.is_idle());
    }
}
```

- [ ] **Step 2: Expose from lib.rs**

Add to `src/lib.rs` alphabetically near other `pub mod` declarations:

```rust
pub mod idle_tracker;
```

- [ ] **Step 3: Run tests**

Run: `cargo test idle_tracker -- --nocapture`
Expected: all 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/idle_tracker.rs src/lib.rs
git commit -m "Add IdleTracker with atomic counters for pending async work"
```

---

## Task 2: Wire IdleTracker into TileCache

**Files:**
- Modify: `src/tile_cache.rs`

- [ ] **Step 1: Read current tile_cache.rs to find fetch request/completion sites**

Run: `grep -n "pending\|fetch\|spawn\|executor" src/tile_cache.rs` and read the file.

Identify:
- The constructor (likely `TileCache::new(executor)`).
- Where a tile fetch is dispatched (spawn on executor or similar).
- Where the fetch future resolves (success and failure paths).

Write down the exact line numbers in your scratch notes.

- [ ] **Step 2: Change constructor signature to accept an IdleTracker**

Change `TileCache::new` to:

```rust
pub fn new(executor: BackgroundExecutor, idle: Arc<crate::idle_tracker::IdleTracker>) -> Self {
    // ...existing body, store `idle` on the struct...
}
```

Add an `idle: Arc<IdleTracker>` field on the `TileCache` struct. Update imports:

```rust
use std::sync::Arc;
use crate::idle_tracker::IdleTracker;
```

Because `main.rs` is the only current caller (verify with `grep -rn "TileCache::new" src`), updating it here will break compilation until Task 7 — that's expected; we'll leave `main.rs` for Task 7 but temporarily pass a throwaway tracker from `main.rs` in this task to keep the build green.

- [ ] **Step 3: Temporarily fix main.rs call site**

In `src/main.rs`, change the `TileCache::new(executor)` call to:

```rust
let idle = osm_gpui::idle_tracker::IdleTracker::new();
let tile_cache = Arc::new(Mutex::new(TileCache::new(executor, idle.clone())));
```

(Store `idle` in a local for now; Task 7 will hoist it to a proper location.)

- [ ] **Step 4: Instrument fetch start and completion**

At the line where the tile fetch future is spawned, add before the spawn:

```rust
self.idle.tile_fetch_started();
```

At both the success and error resolution paths inside the spawned future, add:

```rust
idle.tile_fetch_finished();
```

(Clone `self.idle` into the spawned future via `let idle = self.idle.clone();` before the `executor.spawn(async move { ... })` block.)

Make sure every path that the `started` counter opens is balanced by exactly one `finished`, including error/early-return branches.

- [ ] **Step 5: Compile check**

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 6: Run existing tests**

Run: `cargo test`
Expected: all previously passing tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/tile_cache.rs src/main.rs
git commit -m "Thread IdleTracker through TileCache fetch lifecycle"
```

---

## Task 3: Wire IdleTracker into http_image_loader

**Files:**
- Modify: `src/http_image_loader.rs`
- Modify: callers of the loader (find via grep)

- [ ] **Step 1: Locate the loader's public entry point and all call sites**

Run: `grep -rn "http_image_loader\|HttpImageLoader" src` and read `src/http_image_loader.rs`.

Identify:
- The public function or struct that kicks off an HTTP fetch.
- Where the response is received (HTTP start/end).
- Where image decoding happens (before decode / after decode).

- [ ] **Step 2: Change the loader entry point to accept `Arc<IdleTracker>`**

If the loader is a struct, add an `idle: Arc<IdleTracker>` field and update its constructor. If it's a free function, add an `idle: &Arc<IdleTracker>` parameter. Update every call site discovered in Step 1.

- [ ] **Step 3: Instrument HTTP start/end and decode start/end**

Example shape (adapt to actual code):

```rust
idle.tile_fetch_started();     // at HTTP kickoff (reusing the tile counter if this loader IS the tile fetcher)
// ...await HTTP response...
idle.tile_fetch_finished();

idle.image_decode_started();   // before spawning decode task
// ...decode...
idle.image_decode_finished();
```

If the loader is in fact the same path instrumented in Task 2, do NOT double-count — pick the single location closest to the actual network I/O and decode, and remove the Task 2 hooks if they become redundant. The invariant is: every logical "started" has exactly one matching "finished" on every path (success, error, cancellation).

- [ ] **Step 4: Build and test**

Run: `cargo build && cargo test`
Expected: clean build, existing tests pass.

- [ ] **Step 5: Manual smoke check**

Run: `cargo run` and watch eprintln output as tiles load. Verify the app still renders the map normally. (No new assertion — just confirm we didn't break the loader.)

- [ ] **Step 6: Commit**

```bash
git add src/http_image_loader.rs src/main.rs
git commit -m "Instrument http_image_loader with IdleTracker counters"
```

---

## Task 4: Script op types and parser

**Files:**
- Create: `src/script/mod.rs`
- Create: `src/script/op.rs`
- Create: `src/script/parser.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Create op types**

Create `src/script/op.rs`:

```rust
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
```

- [ ] **Step 2: Create parser with tests**

Create `src/script/parser.rs`:

```rust
//! Line-oriented DSL parser for the screenshot script format.

use std::time::Duration;
use super::op::*;

#[derive(Debug)]
pub struct ParseError {
    pub line_no: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line_no, self.message)
    }
}

impl std::error::Error for ParseError {}

pub fn parse(source: &str) -> Result<Vec<Step>, ParseError> {
    let mut steps = Vec::new();
    for (i, raw) in source.lines().enumerate() {
        let line_no = i + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let op = parse_line(line_no, line)?;
        steps.push(Step { line_no, op });
    }
    Ok(steps)
}

fn strip_comment(s: &str) -> &str {
    match s.find('#') {
        Some(i) => &s[..i],
        None => s,
    }
}

fn err(line_no: usize, msg: impl Into<String>) -> ParseError {
    ParseError { line_no, message: msg.into() }
}

fn parse_line(line_no: usize, line: &str) -> Result<Op, ParseError> {
    let mut tokens = line.split_whitespace();
    let head = tokens.next().ok_or_else(|| err(line_no, "empty line reached parser"))?;
    let rest: Vec<&str> = tokens.collect();
    match head {
        "window" => parse_window(line_no, &rest),
        "viewport" => parse_viewport(line_no, &rest),
        "wait_idle" => parse_wait_idle(line_no, &rest),
        "wait" => parse_wait(line_no, &rest),
        "drag" => parse_drag(line_no, &rest),
        "click" => parse_click(line_no, &rest),
        "scroll" => parse_scroll(line_no, &rest),
        "key" => parse_key(line_no, &rest),
        "capture" => parse_capture(line_no, &rest),
        "log" => Ok(Op::Log { message: rest.join(" ") }),
        other => Err(err(line_no, format!("unknown op '{}'", other))),
    }
}

fn parse_u32(line_no: usize, s: &str, field: &str) -> Result<u32, ParseError> {
    s.parse::<u32>().map_err(|e| err(line_no, format!("{}: {}", field, e)))
}
fn parse_f32(line_no: usize, s: &str, field: &str) -> Result<f32, ParseError> {
    s.parse::<f32>().map_err(|e| err(line_no, format!("{}: {}", field, e)))
}
fn parse_f64(line_no: usize, s: &str, field: &str) -> Result<f64, ParseError> {
    s.parse::<f64>().map_err(|e| err(line_no, format!("{}: {}", field, e)))
}

fn parse_point(line_no: usize, s: &str) -> Result<Point2, ParseError> {
    let (x, y) = s.split_once(',').ok_or_else(|| err(line_no, format!("bad point '{}': want X,Y", s)))?;
    Ok(Point2 {
        x: parse_f32(line_no, x, "point.x")?,
        y: parse_f32(line_no, y, "point.y")?,
    })
}

fn parse_duration(line_no: usize, s: &str) -> Result<Duration, ParseError> {
    if let Some(ms) = s.strip_suffix("ms") {
        let n: u64 = ms.parse().map_err(|e| err(line_no, format!("bad ms: {}", e)))?;
        Ok(Duration::from_millis(n))
    } else if let Some(sec) = s.strip_suffix('s') {
        let n: f64 = sec.parse().map_err(|e| err(line_no, format!("bad seconds: {}", e)))?;
        Ok(Duration::from_secs_f64(n))
    } else {
        Err(err(line_no, format!("bad duration '{}': want Nms or Ns", s)))
    }
}

fn parse_window(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 2 { return Err(err(line_no, "window: want W H")); }
    Ok(Op::Window {
        w: parse_u32(line_no, rest[0], "window.w")?,
        h: parse_u32(line_no, rest[1], "window.h")?,
    })
}

fn parse_viewport(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 3 { return Err(err(line_no, "viewport: want LAT LON ZOOM")); }
    Ok(Op::Viewport {
        lat: parse_f64(line_no, rest[0], "viewport.lat")?,
        lon: parse_f64(line_no, rest[1], "viewport.lon")?,
        zoom: parse_f32(line_no, rest[2], "viewport.zoom")?,
    })
}

fn parse_wait_idle(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    let timeout = match rest.len() {
        0 => Duration::from_secs(10),
        1 => parse_duration(line_no, rest[0])?,
        _ => return Err(err(line_no, "wait_idle: at most one TIMEOUT arg")),
    };
    Ok(Op::WaitIdle { timeout })
}

fn parse_wait(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 { return Err(err(line_no, "wait: want DURATION")); }
    Ok(Op::Wait { duration: parse_duration(line_no, rest[0])? })
}

fn parse_drag(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() < 2 { return Err(err(line_no, "drag: want X1,Y1 X2,Y2 [duration=Nms]")); }
    let from = parse_point(line_no, rest[0])?;
    let to = parse_point(line_no, rest[1])?;
    let mut duration = Duration::from_millis(200);
    for kv in &rest[2..] {
        let (k, v) = kv.split_once('=').ok_or_else(|| err(line_no, format!("drag: bad kv '{}'", kv)))?;
        match k {
            "duration" => duration = parse_duration(line_no, v)?,
            _ => return Err(err(line_no, format!("drag: unknown key '{}'", k))),
        }
    }
    Ok(Op::Drag { from, to, duration })
}

fn parse_click(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.is_empty() { return Err(err(line_no, "click: want X,Y [button=left|right]")); }
    let at = parse_point(line_no, rest[0])?;
    let mut button = MouseButton::Left;
    for kv in &rest[1..] {
        let (k, v) = kv.split_once('=').ok_or_else(|| err(line_no, format!("click: bad kv '{}'", kv)))?;
        match (k, v) {
            ("button", "left") => button = MouseButton::Left,
            ("button", "right") => button = MouseButton::Right,
            _ => return Err(err(line_no, format!("click: unknown {}={}", k, v))),
        }
    }
    Ok(Op::Click { at, button })
}

fn parse_scroll(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.is_empty() { return Err(err(line_no, "scroll: want X,Y [dx=N] [dy=N]")); }
    let at = parse_point(line_no, rest[0])?;
    let mut dx = 0.0f32;
    let mut dy = 0.0f32;
    for kv in &rest[1..] {
        let (k, v) = kv.split_once('=').ok_or_else(|| err(line_no, format!("scroll: bad kv '{}'", kv)))?;
        match k {
            "dx" => dx = parse_f32(line_no, v, "scroll.dx")?,
            "dy" => dy = parse_f32(line_no, v, "scroll.dy")?,
            _ => return Err(err(line_no, format!("scroll: unknown key '{}'", k))),
        }
    }
    Ok(Op::Scroll { at, dx, dy })
}

fn parse_key(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 { return Err(err(line_no, "key: want CHORD like cmd+shift+a")); }
    let mut chord = Chord { cmd: false, shift: false, alt: false, ctrl: false, key: String::new() };
    for part in rest[0].split('+') {
        match part {
            "cmd" => chord.cmd = true,
            "shift" => chord.shift = true,
            "alt" => chord.alt = true,
            "ctrl" => chord.ctrl = true,
            k if !k.is_empty() && chord.key.is_empty() => chord.key = k.to_string(),
            k => return Err(err(line_no, format!("key: unexpected '{}'", k))),
        }
    }
    if chord.key.is_empty() {
        return Err(err(line_no, "key: missing base key after modifiers"));
    }
    Ok(Op::Key { chord })
}

fn parse_capture(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 { return Err(err(line_no, "capture: want PATH")); }
    Ok(Op::Capture { path: rest[0].to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_blanks_and_comments() {
        let s = "\n# hi\nwindow 10 20\n";
        let steps = parse(s).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].op, Op::Window { w: 10, h: 20 });
        assert_eq!(steps[0].line_no, 3);
    }

    #[test]
    fn window_parses() {
        assert_eq!(parse("window 1200 800").unwrap()[0].op, Op::Window { w: 1200, h: 800 });
    }

    #[test]
    fn window_rejects_bad_args() {
        assert!(parse("window 1200").is_err());
        assert!(parse("window x y").is_err());
    }

    #[test]
    fn viewport_parses() {
        let s = parse("viewport 47.6 -122.3 12").unwrap();
        assert_eq!(s[0].op, Op::Viewport { lat: 47.6, lon: -122.3, zoom: 12.0 });
    }

    #[test]
    fn wait_idle_default_and_custom() {
        assert_eq!(parse("wait_idle").unwrap()[0].op, Op::WaitIdle { timeout: Duration::from_secs(10) });
        assert_eq!(parse("wait_idle 3s").unwrap()[0].op, Op::WaitIdle { timeout: Duration::from_secs(3) });
        assert_eq!(parse("wait_idle 500ms").unwrap()[0].op, Op::WaitIdle { timeout: Duration::from_millis(500) });
    }

    #[test]
    fn wait_requires_duration() {
        assert!(parse("wait").is_err());
        assert_eq!(parse("wait 2s").unwrap()[0].op, Op::Wait { duration: Duration::from_secs(2) });
    }

    #[test]
    fn drag_parses_with_default_duration() {
        let s = parse("drag 10,20 30,40").unwrap();
        assert_eq!(s[0].op, Op::Drag {
            from: Point2 { x: 10.0, y: 20.0 },
            to: Point2 { x: 30.0, y: 40.0 },
            duration: Duration::from_millis(200),
        });
    }

    #[test]
    fn drag_parses_with_duration_override() {
        let s = parse("drag 0,0 100,0 duration=500ms").unwrap();
        if let Op::Drag { duration, .. } = &s[0].op {
            assert_eq!(*duration, Duration::from_millis(500));
        } else { panic!("expected Drag"); }
    }

    #[test]
    fn click_defaults_left() {
        let s = parse("click 5,6").unwrap();
        assert_eq!(s[0].op, Op::Click { at: Point2 { x: 5.0, y: 6.0 }, button: MouseButton::Left });
    }

    #[test]
    fn click_button_right() {
        let s = parse("click 5,6 button=right").unwrap();
        assert_eq!(s[0].op, Op::Click { at: Point2 { x: 5.0, y: 6.0 }, button: MouseButton::Right });
    }

    #[test]
    fn scroll_parses_dx_dy() {
        let s = parse("scroll 100,200 dx=1 dy=-5").unwrap();
        assert_eq!(s[0].op, Op::Scroll { at: Point2 { x: 100.0, y: 200.0 }, dx: 1.0, dy: -5.0 });
    }

    #[test]
    fn key_parses_chord() {
        let s = parse("key cmd+shift+a").unwrap();
        if let Op::Key { chord } = &s[0].op {
            assert!(chord.cmd && chord.shift && !chord.alt && !chord.ctrl);
            assert_eq!(chord.key, "a");
        } else { panic!("expected Key"); }
    }

    #[test]
    fn key_requires_base_key() {
        assert!(parse("key cmd+shift").is_err());
    }

    #[test]
    fn unknown_op_errors() {
        let e = parse("wiggle 1 2").unwrap_err();
        assert!(e.message.contains("unknown op"));
    }

    #[test]
    fn capture_captures_path() {
        assert_eq!(parse("capture out.png").unwrap()[0].op, Op::Capture { path: "out.png".into() });
    }

    #[test]
    fn log_joins_tokens() {
        assert_eq!(parse("log hello world").unwrap()[0].op, Op::Log { message: "hello world".into() });
    }

    #[test]
    fn error_reports_line_number() {
        let e = parse("\nwindow 1200\n").unwrap_err();
        assert_eq!(e.line_no, 2);
    }
}
```

- [ ] **Step 3: Create module root**

Create `src/script/mod.rs`:

```rust
//! Scriptable screenshot harness.

pub mod op;
pub mod parser;
pub mod runner;

pub use op::{Chord, MouseButton, Op, Point2, Step};
pub use parser::{parse, ParseError};
```

Create an empty `src/script/runner.rs` for now:

```rust
//! Executes parsed script steps against the live app. Implemented in Task 6.
```

- [ ] **Step 4: Expose from lib.rs**

Add to `src/lib.rs`:

```rust
pub mod script;
```

- [ ] **Step 5: Run tests**

Run: `cargo test script::parser`
Expected: all ~15 parser tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/script src/lib.rs
git commit -m "Add script op types and line-DSL parser with tests"
```

---

## Task 5: Capture module

**Files:**
- Create: `src/capture.rs`
- Modify: `src/lib.rs`
- Modify: `Cargo.toml` (only if core-graphics isn't resolvable)

- [ ] **Step 1: Confirm core-graphics availability**

Run: `cargo tree -p osm-gpui -e normal | grep -E "core-(foundation|graphics)"`
If both crates show up transitively, fine. If not, add to `Cargo.toml`:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
core-foundation = "0.9"
core-graphics = "0.23"
```

- [ ] **Step 2: Create capture module**

Create `src/capture.rs`:

```rust
//! Window-id lookup + screencapture subprocess for test PNGs. macOS only.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub enum CaptureError {
    WindowNotFound,
    Io(std::io::Error),
    ScreencaptureFailed { status: Option<i32>, stderr: String },
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WindowNotFound => write!(f, "no on-screen window found for this PID"),
            Self::Io(e) => write!(f, "io error: {}", e),
            Self::ScreencaptureFailed { status, stderr } => {
                write!(f, "screencapture exited with {:?}: {}", status, stderr)
            }
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}

#[cfg(target_os = "macos")]
pub fn find_own_window_id() -> Result<u32, CaptureError> {
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::window::{
        kCGNullWindowID, kCGWindowListOptionOnScreenOnly, CGWindowListCopyWindowInfo,
    };

    let pid = std::process::id() as i64;
    let info: CFArray<CFDictionary<CFString, CFType>> = unsafe {
        let raw = CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly, kCGNullWindowID);
        if raw.is_null() { return Err(CaptureError::WindowNotFound); }
        CFArray::wrap_under_create_rule(raw)
    };

    let owner_key = CFString::from_static_string("kCGWindowOwnerPID");
    let number_key = CFString::from_static_string("kCGWindowNumber");

    for dict in info.iter() {
        let Some(owner) = dict.find(&owner_key) else { continue };
        let Some(owner_num) = owner.downcast::<CFNumber>() else { continue };
        if owner_num.to_i64() != Some(pid) { continue; }
        let Some(num) = dict.find(&number_key) else { continue };
        let Some(num) = num.downcast::<CFNumber>() else { continue };
        if let Some(id) = num.to_i64() {
            return Ok(id as u32);
        }
    }
    Err(CaptureError::WindowNotFound)
}

#[cfg(not(target_os = "macos"))]
pub fn find_own_window_id() -> Result<u32, CaptureError> {
    Err(CaptureError::WindowNotFound)
}

pub fn capture(window_id: u32, path: &Path) -> Result<PathBuf, CaptureError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let out = Command::new("screencapture")
        .arg("-l").arg(window_id.to_string())
        .arg("-o")
        .arg("-x")
        .arg(path)
        .output()?;
    if !out.status.success() {
        return Err(CaptureError::ScreencaptureFailed {
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(path.to_path_buf())
}
```

- [ ] **Step 3: Expose from lib.rs**

Add:

```rust
pub mod capture;
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: clean build. If `downcast` / API names have drifted in the installed `core-foundation` version, adjust per compiler errors — keep the intent identical.

- [ ] **Step 5: Commit**

```bash
git add src/capture.rs src/lib.rs Cargo.toml Cargo.lock
git commit -m "Add macOS window-id lookup and screencapture wrapper"
```

---

## Task 6: Script runner (non-input ops first)

**Files:**
- Modify: `src/script/runner.rs`

This task implements the ops that don't require gpui input injection: `window`, `viewport`, `wait_idle`, `wait`, `log`, `capture`. Input ops come in Task 7.

- [ ] **Step 1: Define the runner interface**

Replace `src/script/runner.rs` with:

```rust
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
    /// Dispatched on the main thread between frames. Task 7 extends this.
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
        }
    }

    fn wait_idle<A: AppHandle>(&self, app: &mut A, timeout: Duration, line_no: usize) -> Result<(), RunError> {
        let deadline = Instant::now() + timeout;
        let mut consecutive_idle = 0;
        loop {
            app.wait_frame();
            if self.idle.is_idle() {
                consecutive_idle += 1;
                if consecutive_idle >= 2 { return Ok(()); }
            } else {
                consecutive_idle = 0;
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
```

- [ ] **Step 2: Run tests**

Run: `cargo test script::runner`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/script/runner.rs
git commit -m "Add script runner with wait_idle, viewport, wait, capture, log"
```

---

## Task 7: Input injection and main.rs wiring

**Files:**
- Modify: `src/main.rs`
- Possibly modify: `src/script/runner.rs` (if the trait needs to grow)

This task is the highest-risk: it depends on gpui's event-dispatch surface, which the spec explicitly calls an implementation-time detail. Do the research first, then adapt.

- [ ] **Step 1: Research gpui event-dispatch API**

Run:

```bash
grep -rn "dispatch_event\|simulate_\|TestAppContext\|send_input\|PlatformInput" ~/.cargo/registry/src | head -n 80
```

(or locate the gpui source via `cargo metadata`). You are looking for:
- A `Window` method that accepts synthesized `PlatformInput` (or similar).
- A `TestAppContext`-style helper that can deliver mouse/keyboard events.
- If neither exists: a way to call our own handlers (`handle_mouse_down`, `handle_mouse_move`, `handle_mouse_up`, `handle_scroll`) directly on the `MapViewer` entity.

Write findings into scratch notes and pick ONE of these strategies:
- **A) gpui dispatch:** construct `PlatformInput` variants and call the discovered dispatch method.
- **B) Entity-direct:** hold an `Entity<MapViewer>` and call its handler methods with synthetic events.

Both are acceptable. Strategy B is more robust against gpui API drift but bypasses gpui's hit-testing and keyboard-chord resolution. If keyboard chords like `cmd+0` need to route through gpui's binding system, strategy A is required.

- [ ] **Step 2: Implement the `AppHandle` trait in `main.rs`**

Add a struct that wraps whatever handles are needed:

```rust
// At the top of main.rs
use osm_gpui::idle_tracker::IdleTracker;
use osm_gpui::script::{self, runner::{AppHandle, Runner}, Chord, MouseButton, Step};
use osm_gpui::capture;
use std::path::PathBuf;
use std::time::Duration;
use gpui::{WindowHandle, Entity, AsyncApp};

struct LiveApp {
    // Fill in based on Step 1 research. At minimum:
    window: WindowHandle<MapViewer>,
    viewer: Entity<MapViewer>,
    cx: AsyncApp,
}

impl AppHandle for LiveApp {
    fn set_window_size(&mut self, w: u32, h: u32) {
        let sz = gpui::size(gpui::px(w as f32), gpui::px(h as f32));
        let _ = self.window.update(&mut self.cx, |_, window, _| {
            // Adjust to the actual gpui resize API discovered in Step 1.
            // If no direct resize exists, use window.set_window_bounds with Windowed(Bounds{...}).
            let origin = window.bounds().origin;
            window.set_window_bounds(gpui::WindowBounds::Windowed(gpui::Bounds { origin, size: sz }));
        });
    }

    fn set_viewport(&mut self, lat: f64, lon: f64, zoom: f32) {
        self.viewer.update(&mut self.cx, |viewer, cx| {
            viewer.viewport.pan_to(lat, lon);
            viewer.viewport.set_zoom(zoom as f64);
            cx.notify();
        }).ok();
    }

    fn dispatch_drag(&mut self, from: (f32, f32), to: (f32, f32), duration: Duration) {
        let steps = 12u32;
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let per_step = duration / steps.max(1);
        self.dispatch_mouse_down(from);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            self.dispatch_mouse_move((from.0 + dx * t, from.1 + dy * t));
            std::thread::sleep(per_step);
        }
        self.dispatch_mouse_up(to);
    }

    fn dispatch_click(&mut self, at: (f32, f32), _button: MouseButton) {
        self.dispatch_mouse_down(at);
        std::thread::sleep(Duration::from_millis(16));
        self.dispatch_mouse_up(at);
    }

    fn dispatch_scroll(&mut self, at: (f32, f32), dx: f32, dy: f32) {
        let pos = gpui::point(gpui::px(at.0), gpui::px(at.1));
        let ev = ScrollWheelEvent {
            position: pos,
            delta: gpui::ScrollDelta::Lines(gpui::Point { x: dx, y: dy }),
            modifiers: Default::default(),
            touch_phase: Default::default(),
        };
        self.viewer.update(&mut self.cx, |v, cx| { v.handle_scroll(&ev, cx); }).ok();
    }

    fn dispatch_key(&mut self, chord: &Chord) {
        // Preferred path: build a gpui `Keystroke` and call `window.dispatch_keystroke`,
        // which routes through the binding system so `KeyBinding::new("cmd-o", ...)` fires.
        let keystroke = gpui::Keystroke {
            modifiers: gpui::Modifiers {
                control: chord.ctrl,
                alt: chord.alt,
                shift: chord.shift,
                platform: chord.cmd, // cmd on macOS = "platform" modifier in gpui
                function: false,
            },
            key: chord.key.clone(),
            key_char: None,
        };
        let _ = self.window.update(&mut self.cx, |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        });
    }

    fn wait_frame(&mut self) {
        // Simplest: spin until cx advances one frame. If gpui exposes an async
        // frame-await, prefer that. Otherwise, sleep for 16ms as an approximation.
        std::thread::sleep(Duration::from_millis(16));
    }
}

impl LiveApp {
    fn dispatch_mouse_down(&mut self, at: (f32, f32)) {
        let pos = gpui::point(gpui::px(at.0), gpui::px(at.1));
        let ev = MouseDownEvent {
            position: pos,
            button: gpui::MouseButton::Left,
            click_count: 1,
            modifiers: Default::default(),
            first_mouse: false,
        };
        self.viewer.update(&mut self.cx, |v, cx| { v.handle_mouse_down(&ev); cx.notify(); }).ok();
    }
    fn dispatch_mouse_move(&mut self, at: (f32, f32)) {
        let pos = gpui::point(gpui::px(at.0), gpui::px(at.1));
        let ev = MouseMoveEvent {
            position: pos,
            pressed_button: Some(gpui::MouseButton::Left),
            modifiers: Default::default(),
        };
        self.viewer.update(&mut self.cx, |v, cx| { v.handle_mouse_move(&ev, cx); }).ok();
    }
    fn dispatch_mouse_up(&mut self, at: (f32, f32)) {
        let pos = gpui::point(gpui::px(at.0), gpui::px(at.1));
        let ev = MouseUpEvent {
            position: pos,
            button: gpui::MouseButton::Left,
            click_count: 1,
            modifiers: Default::default(),
        };
        self.viewer.update(&mut self.cx, |v, cx| { v.handle_mouse_up(&ev); cx.notify(); }).ok();
    }
}
```

The `MouseDownEvent` / `MouseMoveEvent` / `MouseUpEvent` literal field names above are best-effort guesses based on `main.rs` imports. Fix them against the actual gpui struct definitions during implementation.

- [ ] **Step 3: Add CLI flag parsing to main**

Near the top of `fn main()`, replace the current body with:

```rust
fn main() {
    eprintln!("🚀 Starting OSM-GPUI Map Viewer with Tile Loading");

    let args = parse_cli_args();

    SHARED_OSM_DATA.set(Arc::new(Mutex::new(Vec::new()))).unwrap();
    LAYER_REQUESTS.set(Arc::new(Mutex::new(Vec::new()))).unwrap();

    let idle = IdleTracker::new();
    let window_size = args.window_size.unwrap_or((1200, 800));

    Application::new().run({
        let idle = idle.clone();
        move |cx: &mut App| {
            // ... existing setup: menus, actions, bindings ...

            // Modify open_window to use window_size.
            let window_handle = cx.open_window(
                WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(Bounds {
                        origin: point(px(100.0), px(100.0)),
                        size: size(px(window_size.0 as f32), px(window_size.1 as f32)),
                    })),
                    // ... rest unchanged ...
                    ..Default::default()
                },
                |window, cx| {
                    cx.bind_keys([
                        KeyBinding::new("cmd-o", OpenOsmFile, None),
                        KeyBinding::new("cmd-q", Quit, None),
                    ]);
                    cx.new(|cx| MapViewer::new_with_idle(window, cx, idle.clone()))
                },
            ).unwrap();

            if let Some(script_path) = args.script.clone() {
                let keep_open = args.keep_open;
                let idle = idle.clone();
                cx.spawn(move |cx| async move {
                    if let Err(e) = run_script_session(window_handle, cx, idle, &script_path, keep_open).await {
                        eprintln!("{}", e);
                        std::process::exit(1);
                    }
                    if !keep_open { std::process::exit(0); }
                }).detach();
            }

            cx.on_window_closed(|cx| { cx.quit(); }).detach();
        }
    });
}

#[derive(Default)]
struct CliArgs {
    script: Option<PathBuf>,
    window_size: Option<(u32, u32)>,
    keep_open: bool,
}

fn parse_cli_args() -> CliArgs {
    let mut out = CliArgs::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--script" => out.script = Some(PathBuf::from(args.next().expect("--script needs a path"))),
            "--window-size" => {
                let v = args.next().expect("--window-size needs WxH");
                let (w, h) = v.split_once('x').expect("--window-size format WxH");
                out.window_size = Some((w.parse().expect("W"), h.parse().expect("H")));
            }
            "--keep-open" => out.keep_open = true,
            other => eprintln!("ignoring unknown arg: {}", other),
        }
    }
    out
}

async fn run_script_session(
    window: WindowHandle<MapViewer>,
    mut cx: AsyncApp,
    idle: Arc<IdleTracker>,
    path: &std::path::Path,
    _keep_open: bool,
) -> Result<(), String> {
    // Give the window time to be on-screen before looking up its window id.
    cx.background_executor().timer(Duration::from_millis(300)).await;

    let window_id = capture::find_own_window_id().map_err(|e| format!("window id lookup: {}", e))?;
    let source = std::fs::read_to_string(path).map_err(|e| format!("read script: {}", e))?;
    let steps = script::parse(&source).map_err(|e| format!("parse: {}", e))?;

    let viewer = window.read_with(&cx, |viewer, _| viewer.clone()).map_err(|e| format!("viewer handle: {:?}", e))?;
    let mut app = LiveApp { window, viewer, cx: cx.clone() };
    let runner = Runner { idle, window_id };
    runner.run(&mut app, &steps).map_err(|e| e.to_string())
}
```

`MapViewer::new_with_idle` is a new constructor that accepts an `Arc<IdleTracker>` and passes it to `TileCache::new`. Update `MapViewer::new` to accept the idle tracker as well (simpler: just change the one constructor and its call site).

- [ ] **Step 4: Build and fix compilation errors iteratively**

Run: `cargo build` repeatedly, fixing each gpui API mismatch as it arises. Common fix-ups:
- `WindowHandle::read_with` / `update` argument shapes may differ in this gpui version.
- `Entity<MapViewer>` may need to be obtained via a different path.
- `async move` closure arg count for `cx.spawn` may differ.

Keep the shape of the code the same; only adjust to match the actual gpui signatures.

- [ ] **Step 5: Manual smoke test — `capture` op only**

Create a temporary script:

```bash
cat > /tmp/smoke1.osmscript <<'EOF'
window 1200 800
viewport 47.6062 -122.3321 12
wait_idle 8s
capture /tmp/seattle.png
EOF
```

Run: `cargo run -- --script /tmp/smoke1.osmscript`
Expected: app launches, map renders Seattle, `/tmp/seattle.png` is written, process exits 0.

If `wait_idle` times out, the idle counters in Tasks 2/3 aren't balanced — debug via the log line "step 3: wait_idle ..." and inspect counter values with temporary `eprintln!` in `IdleTracker`.

- [ ] **Step 6: Manual smoke test — drag**

Extend the script:

```
drag 600,400 300,400
wait_idle
capture /tmp/seattle-panned.png
```

Run again and verify the second PNG shows a panned view.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/script/runner.rs
git commit -m "Wire --script CLI flag and gpui event injection into main"
```

---

## Task 8: Smoke script and gitignore

**Files:**
- Create: `docs/screenshots/smoke.osmscript`
- Create: `docs/screenshots/.gitignore`

- [ ] **Step 1: Write the smoke script**

Create `docs/screenshots/smoke.osmscript`:

```
# Exercises every op. Run with:
#   cargo run -- --script docs/screenshots/smoke.osmscript --window-size 1200x800

window 1200 800
viewport 47.6062 -122.3321 12
log loading seattle
wait_idle 10s
capture docs/screenshots/smoke-01-initial.png

drag 600,400 300,400
wait_idle
capture docs/screenshots/smoke-02-panned.png

scroll 600,400 dy=-5
wait_idle
capture docs/screenshots/smoke-03-zoomed.png

click 600,400
wait 250ms
capture docs/screenshots/smoke-04-clicked.png

key cmd+o
wait 500ms
# No capture — the file dialog is modal; this just proves key dispatch didn't panic.
log done
```

- [ ] **Step 2: Ignore generated PNGs**

Create `docs/screenshots/.gitignore`:

```
*.png
```

- [ ] **Step 3: Run the smoke script**

Run: `cargo run -- --script docs/screenshots/smoke.osmscript --window-size 1200x800`
Expected: process exits 0; `docs/screenshots/smoke-0{1,2,3,4}-*.png` exist; each PNG visibly differs from the previous one.

- [ ] **Step 4: Visually inspect the PNGs**

Open the four PNGs. Confirm:
- 01: Seattle map rendered at zoom 12.
- 02: visibly panned east relative to 01.
- 03: visibly zoomed in relative to 02.
- 04: approximately the same framing as 03 (click alone doesn't change viewport).

If any PNG is blank/black, the window id or window-visibility timing is wrong — revisit Task 7 Step 5.

- [ ] **Step 5: Commit**

```bash
git add docs/screenshots/smoke.osmscript docs/screenshots/.gitignore
git commit -m "Add screenshot harness smoke script"
```

---

## Task 9: Final verification

- [ ] **Step 1: Full test run**

Run: `cargo test`
Expected: all tests pass (idle tracker, parser, runner).

- [ ] **Step 2: Full lint / warning sweep**

Run: `cargo build --all-targets 2>&1 | tee /tmp/build.log`
Expected: no new warnings introduced by this feature. Fix any that appear.

- [ ] **Step 3: Confirm default mode unchanged**

Run: `cargo run` (no flags)
Expected: app behaves identically to before — opens window, renders map, responds to real mouse input. No script runner threads spawned.

- [ ] **Step 4: Confirm error paths**

```bash
echo "wiggle" > /tmp/bad.osmscript
cargo run -- --script /tmp/bad.osmscript
echo "exit: $?"
```

Expected: stderr includes `script error at line 1: ...unknown op...`; exit code `1`.

- [ ] **Step 5: Final commit if any cleanup landed**

```bash
git status
# commit anything outstanding with a descriptive message
```

---

## Notes

- **gpui drift is the main risk.** Task 7 is deliberately written to be flexible about the exact event-dispatch path. If the "best-effort" struct literals don't compile, fix them against actual definitions — the *shape* of the approach is sound even if field names change.
- **macOS-only.** Everything guarded by `cfg(target_os = "macos")` is expected to no-op or error on other platforms. That's per spec non-goals.
- **Pixel diffs are out of scope.** Don't add image-compare crates or snapshot frameworks. Captures are for human/LLM visual inspection.
