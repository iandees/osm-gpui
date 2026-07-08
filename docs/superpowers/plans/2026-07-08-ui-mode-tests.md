# UI Mode-Switching Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a data-model `assert_mode` op to the osmscript DSL and use it to build the first automated UI test suite, covering mode-selector (Select/Add/Building/Extrude) switching via toolbar clicks, keyboard shortcuts, and the disabled-without-a-layer case.

**Architecture:** Extend the existing three-layer script system (`src/script/op.rs` parses ops → `src/script/runner.rs` dispatches ops through the `AppHandle` trait → `src/script_harness.rs`'s `LiveApp`/`ScriptBus` bridge to the gpui main thread) with one new op, `assert_mode`, that reads `MapViewer.mode` on the main thread and reports a pass/fail `Result` back across the thread boundary — following the exact pattern the existing `capture` op already uses for returning a main-thread-computed result. Two new `.osmscript` test files drive real toolbar clicks and keystrokes and assert on the resulting mode instead of capturing screenshots. A new `tests/ui_scripts.rs` integration test (marked `#[ignore]`, run manually) spawns the real binary against each script and checks its exit code.

**Amendment (discovered during Task 4/original numbering, now Task 5, execution):** the `click` op's `ScriptCommand::Click` handler in `src/script_harness.rs` called `MapViewer::handle_mouse_down`/`handle_mouse_up` directly instead of going through gpui's real event dispatch — so it could never hit-test and trigger a widget's `.on_click` listener (e.g. the mode-panel `Button`s), only the map's own hand-wired interaction handlers. Task 4 below fixes this by routing `click` through `Window::dispatch_event` (gpui's real dispatch, the same mechanism `key` already uses via `dispatch_keystroke`), scoped narrowly to `Click` only — `Drag`/`Scroll` are intentionally left untouched to avoid risking regressions in existing `docs/screenshots/*.osmscript` files that don't need this fix.

**Tech Stack:** Rust, gpui, existing osmscript DSL (no new dependencies).

## Global Constraints

- Run `cargo fmt --check` before every commit (CI enforces it).
- Single-line git commit messages, no Co-Authored-By trailers (see repo conventions).
- Do not wire the new `tests/ui_scripts.rs` suite into `.github/workflows/ci.yml` — launching the app binary touches the macOS Keychain and can hang unattended; this is explicitly out of scope (see `docs/superpowers/specs/2026-07-08-ui-mode-tests-design.md`, Non-Goals).
- `EditMode` in `src/main.rs:111-116` already derives `Clone, Copy, Debug, PartialEq, Eq` — no changes needed there.

---

### Task 1: Add `EditMode` and `Op::AssertMode` to the script DSL

**Files:**
- Modify: `src/script/op.rs`
- Modify: `src/script/parser.rs`
- Modify: `src/script/mod.rs`

**Interfaces:**
- Produces: `pub enum script::EditMode { Select, Add, Building, Extrude }` (derives `Debug, Clone, Copy, PartialEq`), `Op::AssertMode { mode: EditMode }`, parser support for the line `assert_mode <select|add|building|extrude>`. Task 2 consumes both.

- [ ] **Step 1: Write the failing parser tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/script/parser.rs` (after the `load_osm_requires_path` test, before `error_reports_line_number`):

```rust
    #[test]
    fn assert_mode_parses_each_variant() {
        assert_eq!(
            parse("assert_mode select").unwrap()[0].op,
            Op::AssertMode {
                mode: EditMode::Select
            }
        );
        assert_eq!(
            parse("assert_mode add").unwrap()[0].op,
            Op::AssertMode {
                mode: EditMode::Add
            }
        );
        assert_eq!(
            parse("assert_mode building").unwrap()[0].op,
            Op::AssertMode {
                mode: EditMode::Building
            }
        );
        assert_eq!(
            parse("assert_mode extrude").unwrap()[0].op,
            Op::AssertMode {
                mode: EditMode::Extrude
            }
        );
    }

    #[test]
    fn assert_mode_rejects_unknown_mode() {
        let e = parse("assert_mode sideways").unwrap_err();
        assert!(e.message.contains("unknown mode"));
    }

    #[test]
    fn assert_mode_requires_arg() {
        assert!(parse("assert_mode").is_err());
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile (Op::AssertMode / EditMode don't exist yet)**

Run: `cargo test --lib script::parser`
Expected: compile error, `no variant or associated item named 'AssertMode' found for enum 'Op'` (or similar — `EditMode` unresolved).

- [ ] **Step 3: Add `EditMode` and `Op::AssertMode` to `src/script/op.rs`**

In `src/script/op.rs`, add the new enum after `Chord` (after line 24, before `#[derive(Debug, Clone, PartialEq)]\npub enum Op {`):

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EditMode {
    Select,
    Add,
    Building,
    Extrude,
}
```

Then add a variant to `Op` (after the existing `LoadOsm { path: String }` variant, before the closing `}` of the enum at line 69):

```rust
    AssertMode {
        mode: EditMode,
    },
```

- [ ] **Step 4: Add parsing for `assert_mode` in `src/script/parser.rs`**

In the `parse_line` match in `src/script/parser.rs` (after the `"load_osm" => parse_load_osm(line_no, &rest),` arm, before `other => ...`):

```rust
        "assert_mode" => parse_assert_mode(line_no, &rest),
```

Add the parsing function after `parse_load_osm` (after its closing `}`, before the `#[cfg(test)]` module):

```rust
fn parse_assert_mode(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 {
        return Err(err(line_no, "assert_mode: want select|add|building|extrude"));
    }
    let mode = match rest[0] {
        "select" => EditMode::Select,
        "add" => EditMode::Add,
        "building" => EditMode::Building,
        "extrude" => EditMode::Extrude,
        other => return Err(err(line_no, format!("assert_mode: unknown mode '{}'", other))),
    };
    Ok(Op::AssertMode { mode })
}
```

- [ ] **Step 5: Re-export `EditMode` from `src/script/mod.rs`**

Change the `pub use op::{...}` line in `src/script/mod.rs`:

```rust
pub use op::{Chord, EditMode, MouseButton, Op, Point2, Step};
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib script::parser`
Expected: PASS, including the three new tests.

- [ ] **Step 7: Commit**

```bash
git add src/script/op.rs src/script/parser.rs src/script/mod.rs
git commit -m "Add assert_mode op to the osmscript DSL"
```

---

### Task 2: Wire `assert_mode` through `AppHandle` and `Runner`

**Files:**
- Modify: `src/script/runner.rs`

**Interfaces:**
- Consumes: `Op::AssertMode { mode: EditMode }`, `crate::script::EditMode` from Task 1.
- Produces: `AppHandle::assert_mode(&mut self, want: crate::script::EditMode) -> Result<(), String>` trait method. Task 3's `LiveApp` implements it; `Fake` in this file's test module implements it for unit tests.

- [ ] **Step 1: Write the failing runner test**

Add to the `#[cfg(test)] mod tests` block in `src/script/runner.rs`, after the existing `wait_idle_times_out` test:

```rust
    #[test]
    fn assert_mode_passes_when_matching() {
        let idle = IdleTracker::new();
        let mut fake = Fake {
            frames_waited: 0,
            idle_after_frame: u32::MAX,
            idle: idle.clone(),
            mode: crate::script::EditMode::Add,
        };
        let runner = Runner { idle };
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
        };
        let runner = Runner { idle };
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
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test --lib script::runner`
Expected: compile error — `Fake` has no field `mode`, `AppHandle` has no method `assert_mode`, `Op::AssertMode` unhandled in `run_step`/`describe`.

- [ ] **Step 3: Add `assert_mode` to the `AppHandle` trait**

In `src/script/runner.rs`, add to the `AppHandle` trait (after the `capture` method, before the closing `}` at line 26):

```rust
    /// Compare the app's current `EditMode` against `want`; `Err` with a
    /// descriptive message if it doesn't match.
    fn assert_mode(&mut self, want: crate::script::EditMode) -> Result<(), String>;
```

- [ ] **Step 4: Handle `Op::AssertMode` in `run_step` and `describe`**

In `run_step`'s match (after the `Op::LoadOsm { path } => { ... }` arm, before the closing `}` at line 107):

```rust
            Op::AssertMode { mode } => {
                app.assert_mode(*mode).map_err(|e| RunError {
                    line_no: step.line_no,
                    message: format!("assert_mode: {}", e),
                })?;
                Ok(())
            }
```

In `describe` (after the `Op::LoadOsm { path } => ...` arm, before the closing `}` at line 162):

```rust
        Op::AssertMode { mode } => format!("assert_mode {:?}", mode),
```

- [ ] **Step 5: Add `mode` field and `assert_mode` impl to `Fake`**

In the `Fake` struct definition (test module, around line 170-174), add a field:

```rust
    struct Fake {
        pub frames_waited: u32,
        pub idle_after_frame: u32,
        pub idle: Arc<IdleTracker>,
        pub mode: crate::script::EditMode,
    }
```

In `impl AppHandle for Fake` (after the `capture` method, before the closing `}`):

```rust
        fn assert_mode(&mut self, want: crate::script::EditMode) -> Result<(), String> {
            if self.mode == want {
                Ok(())
            } else {
                Err(format!("expected mode {:?}, got {:?}", want, self.mode))
            }
        }
```

Then update the two existing `Fake { .. }` construction sites in this test module (`wait_idle_requires_two_consecutive_idle_frames` and `wait_idle_times_out`) to add `mode: crate::script::EditMode::Select,` to each literal.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib script::runner`
Expected: PASS, including the two new tests and the two pre-existing `wait_idle` tests (still passing with the added `mode` field).

- [ ] **Step 7: Commit**

```bash
git add src/script/runner.rs
git commit -m "Dispatch assert_mode through AppHandle and Runner"
```

---

### Task 3: Bridge `assert_mode` to the live gpui app

**Files:**
- Modify: `src/script_harness.rs`

**Interfaces:**
- Consumes: `AppHandle::assert_mode` from Task 2, `MapViewer.mode: crate::EditMode` (`src/main.rs:358`).
- Produces: working `LiveApp::assert_mode`, so `--script` can execute `assert_mode` lines end to end. Task 4 depends on this compiling and running correctly against the real app.

There's no gpui-free unit test for this file (it requires a live window); correctness is verified by running the real test scripts in Task 4. Focus this task on precise, pattern-matched edits and a clean `cargo build`.

- [ ] **Step 1: Add the `AssertMode` command variant**

In `src/script_harness.rs`, add a variant to `ScriptCommand` (after `LoadOsm { name: String, data: OsmData },`, before the closing `}` at line 54):

```rust
    /// Compare `MapViewer.mode` against `want`.
    AssertMode(crate::EditMode),
```

- [ ] **Step 2: Add the `assert_result` slot to `ScriptBus`**

Add a field to the `ScriptBus` struct (after `capture_result: Mutex<Option<Result<(), String>>>,`, before the closing `}` at line 68):

```rust
    /// Result of the most recently processed `AssertMode` command.
    assert_result: Mutex<Option<Result<(), String>>>,
```

Initialize it in `ScriptBus::new()` (after `capture_result: Mutex::new(None),`, before the closing `})` at line 78):

```rust
            assert_result: Mutex::new(None),
```

Add getter/setter methods (after `take_capture_result`, before the closing `}` of `impl ScriptBus` at line 131):

```rust
    /// Called by MapViewer::render when handling an `AssertMode` command,
    /// before `signal_done_and_frame` wakes the waiting runner thread.
    fn set_assert_result(&self, result: Result<(), String>) {
        *self.assert_result.lock().unwrap() = Some(result);
    }

    /// Called by the runner thread after `submit(AssertMode(..))` returns.
    fn take_assert_result(&self) -> Result<(), String> {
        self.assert_result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Err("assert_mode: no result recorded".to_string()))
    }
```

- [ ] **Step 3: Handle `ScriptCommand::AssertMode` in `process_script_command`**

In the `match cmd` block inside `process_script_command` (after the `ScriptCommand::LoadOsm { name, data } => { ... }` arm, before the closing `}` at line 246):

```rust
                ScriptCommand::AssertMode(want) => {
                    let result = if self.mode == want {
                        Ok(())
                    } else {
                        Err(format!("expected mode {:?}, got {:?}", want, self.mode))
                    };
                    bus.set_assert_result(result);
                }
```

- [ ] **Step 4: Implement `AppHandle::assert_mode` for `LiveApp`**

In `impl AppHandle for LiveApp` (after the `capture` method, before the closing `}` at line 360):

```rust
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
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: builds cleanly. If `crate::EditMode` isn't visible in `script_harness.rs` (it's a private `enum` in `main.rs`), the compiler will report a privacy error — `EditMode` is declared in the same crate root (`main.rs`) as `script_harness.rs` is a `mod` of, so it should already be visible via `crate::EditMode` without changes. Confirm no error; if one appears, it will name the exact missing visibility and the fix is adding `pub(crate)` to the `enum EditMode` declaration in `src/main.rs:111` — do that only if the build actually fails here.

- [ ] **Step 6: Run the full test suite to check nothing else broke**

Run: `cargo test`
Expected: PASS (all existing tests plus Tasks 1-2's new tests).

- [ ] **Step 7: Run `cargo fmt --check` and `cargo clippy --all-targets -D warnings`**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: both clean. Fix any formatting/lint issues (`cargo fmt` to auto-fix formatting).

- [ ] **Step 8: Commit**

```bash
git add src/script_harness.rs
git commit -m "Bridge assert_mode to the live gpui app via ScriptBus"
```

---

### Task 4: Route `ScriptCommand::Click` through gpui's real event dispatch

**Files:**
- Modify: `src/script_harness.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks (independent of the `assert_mode` DSL work in Tasks 1-3).
- Produces: `click` op now capable of triggering any gpui widget's `.on_click`/mouse listener in the render tree (not just the map's own hand-wired handlers). Task 5's `mode_switching.osmscript` depends on this to make toolbar-button clicks work.

There is no gpui-free unit test for this file — correctness is verified by re-running an existing, already-manually-verified `.osmscript` script to confirm no regression, plus Task 5's own verification. Focus this task on a precise, minimal edit.

- [ ] **Step 1: Replace the `ScriptCommand::Click` arm's direct method calls with real gpui dispatch**

In `src/script_harness.rs`, inside `process_script_command`'s `match cmd` block, replace the `ScriptCommand::Click { x, y, right } => { ... }` arm (currently at approximately line 216-238) with:

```rust
                ScriptCommand::Click { x, y, right } => {
                    let btn = if right {
                        gpui::MouseButton::Right
                    } else {
                        gpui::MouseButton::Left
                    };
                    let down = MouseDownEvent {
                        button: btn,
                        position: point(px(x), px(y)),
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                        first_mouse: false,
                    };
                    window.dispatch_event(gpui::PlatformInput::MouseDown(down), cx);
                    let up = MouseUpEvent {
                        button: btn,
                        position: point(px(x), px(y)),
                        modifiers: gpui::Modifiers::none(),
                        click_count: 1,
                    };
                    window.dispatch_event(gpui::PlatformInput::MouseUp(up), cx);
                }
```

This removes the direct `self.handle_mouse_down(&ev)` / `self.handle_mouse_up(&ev, cx)` calls and the manual `cx.notify()` — real dispatch invokes gpui's own registered listeners (`.on_mouse_down`/`.on_mouse_up` on the map div, `.on_click` on toolbar buttons), and those listeners already call `cx.notify()` themselves where needed (see the existing `.on_mouse_up(Right, ...)` closure a few lines above, which calls `cx.notify()` internally). `Window::dispatch_event` takes `(PlatformInput, &mut App)`; passing `cx: &mut Context<Self>` here works via deref coercion — this file already does exactly this for `window.dispatch_keystroke(ks, cx)` a few lines below (see the `KEYSTROKE_QUEUE` draining block), which has the same `&mut App` requirement.

Leave `ScriptCommand::Drag` and `ScriptCommand::Scroll` completely unchanged — they keep calling `self.handle_mouse_down`/`handle_mouse_move`/`handle_mouse_up`/`handle_scroll` directly, exactly as today.

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: builds cleanly. If `gpui::PlatformInput` isn't in scope, add it to the `use gpui::{...}` list at the top of the file (currently `point, px, Context, Keystroke, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollDelta, ScrollWheelEvent, Window`) — add `PlatformInput` to that list, or reference it fully-qualified as `gpui::PlatformInput` (already used that way in the code above, so no import change should be strictly necessary, but if `cargo build` reports it's ambiguous or unresolved, add it to the `use` list).

- [ ] **Step 3: Run `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: both clean. Fix any formatting/lint issues (`cargo fmt` to auto-fix).

- [ ] **Step 4: Regression-check against an existing, already-verified script**

Run: `cargo run -- --script docs/screenshots/select.osmscript`
Expected: exits 0, no `script error` line. This script exercises left-`click`-based node/way/empty-space selection end to end — the exact code path this task changes. It writes PNGs to `docs/screenshots/out/select-node.png`, `select-empty.png`, `select-way.png`. Use the Read tool to view each of the three PNGs and visually confirm they still show the expected state per the script's own header comment (POI node selected/highlighted in the first, nothing selected in the second, the way selected/highlighted in the third). This is a human-in-the-loop launch — the app binary touches the macOS Keychain at startup and may prompt; a human is available to click through it (same as the rest of this session's launches).

- [ ] **Step 5: Run the full non-ignored test suite once more**

Run: `cargo test`
Expected: PASS (this task doesn't add or change any `#[test]`, just confirms nothing else broke).

- [ ] **Step 6: Commit**

```bash
git add src/script_harness.rs
git commit -m "Route script click op through gpui's real event dispatch"
```

---

### Task 5: Mode-switching test scripts and integration test runner

**Files:**
- Create: `tests/ui/mode_switching.osmscript`
- Create: `tests/ui/mode_requires_layer.osmscript`
- Create: `tests/ui_scripts.rs`

**Interfaces:**
- Consumes: `--script` CLI flag and `assert_mode` op (Tasks 1-3), the real-dispatch `click` fix (Task 4), existing fixture `docs/screenshots/fixtures/select.osm`.
- Produces: nothing consumed by later tasks — this is the final deliverable.

Button coordinates used below: the mode-panel toolbar (`src/mode_panel.rs`) is `MODE_PANEL_WIDTH = 56px` wide, a `v_flex` with `items_center()` (centers each 46×46 button horizontally: center x = 28), `gap_1` (4px, gpui's spacing scale: `"1"` = `0.25rem` = 4px at the default 16px rem size) between buttons, and `py_2` (8px top/bottom padding, `"2"` = `0.5rem` = 8px). Button `i`'s center y = `8 + i*(46+4) + 23 = 31 + i*50`: Select=(28,31), Add=(28,81), Building=(28,131), Extrude=(28,181). These are computed, not yet empirically confirmed — Step 1 below has you verify them for real and adjust if `assert_mode` reports a mismatch (a wrong coordinate will fail loudly with `expected mode X, got Y`, which is the fast feedback loop for calibrating them — much better than eyeballing a screenshot).

- [ ] **Step 1: Write `tests/ui/mode_switching.osmscript`**

```
# Exercise all EditMode transitions via toolbar clicks and keyboard
# shortcuts, asserting the resulting mode via `assert_mode` (no screenshots).
#
# Mode-panel button coordinates: MODE_PANEL_WIDTH=56px, buttons 46x46 in a
# v_flex with items_center/gap_1(4px)/py_2(8px), stacked Select/Add/
# Building/Extrude. Button i center = (28, 31 + i*50).
#   mode-select:   28,31
#   mode-add:      28,81
#   mode-building: 28,131
#   mode-extrude:  28,181

window 1200 800
load_osm docs/screenshots/fixtures/select.osm
wait_idle 5s
assert_mode select

# Toolbar: Select -> Add -> Building -> Extrude -> back to Select.
click 28,81
wait_idle 2s
assert_mode add

click 28,131
wait_idle 2s
assert_mode building

click 28,181
wait_idle 2s
assert_mode extrude

click 28,31
wait_idle 2s
assert_mode select

# Keyboard shortcuts (map area must have focus first, gained by clicking
# into it away from any fixture feature).
click 900,600
wait_idle 2s
assert_mode select

key a
wait_idle 2s
assert_mode add

key b
wait_idle 2s
assert_mode building

key x
wait_idle 2s
assert_mode extrude

# escape from Add mode with no in-progress way falls back to Select.
key a
wait_idle 2s
assert_mode add
key escape
wait_idle 2s
assert_mode select

log mode_switching: all transitions verified
```

- [ ] **Step 2: Write `tests/ui/mode_requires_layer.osmscript`**

```
# Add/Building/Extrude toolbar buttons are disabled without an active
# layer (src/mode_panel.rs render_mode_panel, has_active_layer gate);
# clicking them must leave the mode at Select.
#
# Same button coordinates as mode_switching.osmscript.

window 1200 800
wait_idle 5s
assert_mode select

click 28,81
wait_idle 1s
assert_mode select

click 28,131
wait_idle 1s
assert_mode select

click 28,181
wait_idle 1s
assert_mode select

log mode_requires_layer: disabled buttons correctly inert
```

- [ ] **Step 3: Run both scripts manually to verify the coordinates and assertions are correct**

Run: `cargo run -- --script tests/ui/mode_switching.osmscript`
Expected: exits 0, prints `mode_switching: all transitions verified` near the end, no `script error` line.

Run: `cargo run -- --script tests/ui/mode_requires_layer.osmscript`
Expected: exits 0, prints `mode_requires_layer: disabled buttons correctly inert`.

If either fails with an `assert_mode: expected mode X, got Y` error, the reported "got Y" tells you which mode the click actually landed on (or that it landed on nothing, mode unchanged) — adjust the coordinate in the `.osmscript` file (shift the failing click's x/y) and re-run until both pass. Note in this task's summary if you had to change the computed coordinates, and update the coordinate comments at the top of both files to match reality.

- [ ] **Step 4: Write `tests/ui_scripts.rs`**

```rust
//! Runs the real `osm-gpui` binary against each `.osmscript` file under
//! `tests/ui/`, asserting exit status 0 — `assert_mode` failures, crashes,
//! and `wait_idle` timeouts all surface as non-zero exit.
//!
//! Ignored by default: launching the app touches the macOS Keychain and can
//! hang unattended (see docs/superpowers/specs/2026-07-08-ui-mode-tests-design.md).
//! Run explicitly: `cargo test --test ui_scripts -- --ignored`

use std::path::Path;
use std::process::Command;

fn run_script(name: &str) {
    let script_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/ui")
        .join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_osm-gpui"))
        .arg("--script")
        .arg(&script_path)
        .output()
        .unwrap_or_else(|e| panic!("failed to launch osm-gpui: {e}"));

    assert!(
        output.status.success(),
        "{name} failed (status {:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
#[ignore]
fn mode_switching() {
    run_script("mode_switching.osmscript");
}

#[test]
#[ignore]
fn mode_requires_layer() {
    run_script("mode_requires_layer.osmscript");
}
```

- [ ] **Step 5: Run the integration test to confirm it passes end to end**

Run: `cargo test --test ui_scripts -- --ignored`
Expected: `test mode_switching ... ok`, `test mode_requires_layer ... ok`.

- [ ] **Step 6: Run `cargo fmt --check` and full non-ignored test suite one more time**

Run: `cargo fmt --check && cargo test`
Expected: both clean/passing (the new `#[ignore]`d tests are skipped by plain `cargo test`, matching the constraint that this suite isn't part of the default/CI run).

- [ ] **Step 7: Commit**

```bash
git add tests/ui/mode_switching.osmscript tests/ui/mode_requires_layer.osmscript tests/ui_scripts.rs
git commit -m "Add osmscript-based mode-switching UI test suite"
```

---

---

### Task 6: Route the keystroke-queue drain through the same re-entrancy-safe dispatch as Click

**Files:**
- Modify: `src/script_harness.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `key` op no longer panics when a keystroke's handler is itself a `cx.listener()` callback on `MapViewer` (e.g. the mode-switch shortcuts `a`/`b`/`x`/`escape` at `src/main.rs:2345-2373`). Task 5's `mode_switching.osmscript` depends on this for its keyboard-shortcut assertions to run at all.

**Context:** Task 5's implementer found that `mode_switching.osmscript` panics at its first `key a` step with the identical panic Task 4 fixed for `Click` ("cannot update osm_gpui::MapViewer while it is already being updated") — same root cause, different call site: the keystroke-queue drain loop in `process_script_command` still calls `window.dispatch_keystroke(ks, cx)` synchronously, which re-enters `MapViewer`'s own entity lease via the map div's `.on_key_down(cx.listener(...))` handler, exactly as the old synchronous `Click` dispatch did. Apply the same `window.defer` fix Task 4 already established and had reviewed/approved.

- [ ] **Step 1: Wrap the keystroke drain in `window.defer`**

In `src/script_harness.rs`, replace the keystroke-queue-draining block in `process_script_command` (currently, after Task 4's changes, approximately):

```rust
        // Also drain keystroke queue (processed via Window so needs to be here)
        if let Some(ks_queue) = KEYSTROKE_QUEUE.get() {
            if let Ok(mut guard) = ks_queue.try_lock() {
                for ks in guard.drain(..) {
                    window.dispatch_keystroke(ks, cx);
                }
            }
        }
```

with:

```rust
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
```

The lock guard is drained into a plain `Vec` and dropped (via the `if let` scope) before `window.defer`'s `'static` closure is constructed, so nothing tries to move a `MutexGuard` into it.

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 3: Run `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: both clean.

- [ ] **Step 4: Run `tests/ui/mode_switching.osmscript` to confirm the keyboard-shortcut section now passes**

Run: `cargo run -- --script tests/ui/mode_switching.osmscript`
Expected: exits 0, prints `mode_switching: all transitions verified`, no `script error` line, no panic.

- [ ] **Step 5: Regression-check an existing script that uses `key`**

Run: `cargo run -- --script docs/screenshots/smoke.osmscript`
Expected: exits 0 (this script exercises `key cmd+0` among other ops — confirms the defer wrapping doesn't break simple keystroke dispatch outside the mode-switch-listener case).

- [ ] **Step 6: Run the full non-ignored test suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/script_harness.rs
git commit -m "Route script keystroke dispatch through the same defer fix as click"
```

---

## Self-Review Notes

- **Spec coverage:** `assert_mode` op (Tasks 1-3) ✓, real-dispatch `click` fix so toolbar buttons are reachable (Task 4) ✓, mode-switching + disabled-without-layer scripts (Task 5 Steps 1-2) ✓, `tests/ui/` directory separate from `docs/screenshots/` ✓, ignored integration runner not wired into CI (Task 5 Step 4 doc comment + Global Constraints) ✓.
- **Placeholder scan:** none — every step has literal code/commands.
- **Type consistency:** `EditMode` (script-DSL, `script::EditMode`) flows Task 1 → Task 2 (`AppHandle::assert_mode(want: crate::script::EditMode)`) → Task 3 (`LiveApp::assert_mode` converts to bin-crate `crate::EditMode` before building `ScriptCommand::AssertMode`). Checked the enum names and match arms are identical at each boundary.
- **Crate boundary:** `EditMode` in `src/main.rs` is bin-crate; `script::EditMode` in `src/script/op.rs` is lib-crate (`osm_gpui`). They're distinct types by design (mirrors the existing `EditModeAction`/`EditMode` split used for the same reason) — `script_harness.rs` is compiled as part of the bin crate (it's a `mod` of `main.rs`) so it can reference both `crate::EditMode` and `osm_gpui::script::EditMode` and convert between them, which is exactly what Task 3 Step 4 does.
- **Task 4 addendum (post Tasks 1-3 discovery):** the original plan assumed `click` already reached arbitrary widgets via real gpui dispatch; Task 4's implementer found this false (`click` called `MapViewer::handle_mouse_down`/`handle_mouse_up` directly, bypassing gpui's hit-testing entirely) and was correctly BLOCKED rather than guessing a fix outside its scope. Task 4 (this amendment) fixes it narrowly — `Click` only, `Drag`/`Scroll` untouched — confirmed via research into gpui's `Window::dispatch_event`/`dispatch_mouse_event` (same mechanism `dispatch_keystroke` already uses) that this requires no additional paint-ordering ceremony and is safe to call from the existing call site.
- **Task 6 addendum (post Task 5 discovery):** Task 4's own brief claimed dispatching real events synchronously from inside `render` was safe, but its implementer found it panics (re-entrant entity lease) and fixed it with `window.defer` for `Click`. Task 5's implementer then hit the identical panic for `key` — the keystroke-queue drain was never updated to match. Task 6 applies the same, already-reviewed `window.defer` pattern to the keystroke drain. Ordering/staleness safety argument is identical to Task 4's (verified there by the task reviewer): `ScriptBus` drains and signals completion for at most one script command per `render()` call, so any later script step can only be processed by a subsequent `render()` call, which cannot happen until the current frame's deferred effects (including the deferred keystroke dispatch) have flushed.
