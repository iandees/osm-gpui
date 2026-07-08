# UI Mode-Switching Tests — Design

## Purpose

The osmscript harness (`docs/superpowers/specs/2026-04-12-scriptable-screenshot-harness-design.md`) can drive real input into the app and capture screenshots, but it has no way to assert on application state — every existing `.osmscript` is verified by eyeballing PNGs. The mode selector (`EditMode`: Select/Add/Building/Extrude, added in PR #68) has zero automated coverage.

This adds a data-model assertion primitive to the DSL and a first test suite that drives mode switching through both the toolbar and keyboard shortcuts, asserting the resulting `EditMode` directly — no screenshots, no pixel diffing.

## New DSL Op: `assert_mode`

```
assert_mode <select|add|building|extrude>
```

Fails the script (same as any other op failure: `RunError` → stderr message → process exit 1) if the app's current `EditMode` doesn't match.

### Wiring

Follows the existing `capture` pattern exactly, since that's the only current op that returns a value computed on the gpui main thread back to the script-runner thread:

- `AppHandle` trait (`src/script/runner.rs`) gains `fn assert_mode(&mut self, want: EditMode) -> Result<(), String>`.
- `LiveApp::assert_mode` (`src/script_harness.rs`) submits `ScriptCommand::AssertMode(EditMode)` on `ScriptBus` and blocks on `submit`'s condvar, then reads a new `assert_result: Mutex<Option<Result<(), String>>>` field on `ScriptBus` (mirrors `capture_result`).
- `MapViewer::process_script_command` gets a new match arm: compare `self.mode` to the wanted `EditMode`, build `Ok(())` or `Err(format!("expected mode {want:?}, got {actual:?}"))`, call `bus.set_assert_result(...)` **before** `signal_done_and_frame()` (same ordering requirement as `capture_result`, so the runner thread is guaranteed to observe it once `submit()` unblocks).
- `Runner::run_step` (`src/script/runner.rs`) maps the `Err(String)` into `RunError { line_no, message }`, same as `Capture`/`LoadOsm`.
- Parser (`src/script/parser.rs`) gains an `assert_mode` line case parsing one of the four mode names into `Op::AssertMode(EditMode)`; unknown mode name is a parse error.
- `Fake` `AppHandle` (`src/script/runner.rs` test module) gains a settable `mode: EditMode` field; `assert_mode` compares against it, so the dispatch/error-message logic is unit-tested without gpui.

`EditMode` (`src/main.rs:111-116`) needs `PartialEq`/`Debug` derives if not already present — used both for the comparison and the error message.

## Test Scripts — new `tests/ui/` directory

Kept separate from `docs/screenshots/` because these scripts assert instead of just capturing — `docs/screenshots/` stays the home for pure visual-inspection scripts.

**`tests/ui/mode_switching.osmscript`**
1. `load_osm docs/screenshots/fixtures/select.osm` (existing fixture, gives an active layer)
2. Click each toolbar button (Select/Add/Building/Extrude) in turn, `assert_mode` after each. Coordinates computed from `MODE_PANEL_WIDTH=56`, 46×46 buttons with `gap_1`/`py_2` stacking (same math as existing scripts in `docs/screenshots/`).
3. Click into the map (for focus), then `key a` / `key b` / `key x`, `assert_mode` after each.
4. From Add mode, `key escape`, `assert_mode select`.

**`tests/ui/mode_requires_layer.osmscript`**
1. No `load_osm` — no active layer.
2. Click the Add/Building/Extrude toolbar positions; `assert_mode select` after each (buttons are disabled without an active layer, so mode must not change).

Both scripts end with `log` lines noting what passed, for readable stdout when run manually.

## Test Runner — `tests/ui_scripts.rs`

A `#[test]`-per-script integration test using `env!("CARGO_BIN_EXE_osm-gpui")` to spawn the real binary with `--script tests/ui/<name>.osmscript`, asserting exit status 0 and printing stdout/stderr on failure for diagnosis.

Marked `#[ignore]`. Rationale: per prior experience in this repo, launching the app binary touches the macOS Keychain (OAuth) and can hang or prompt in unattended contexts (see project memory on keyring access in tests). Until there's a mocked/no-auth launch path, these tests must stay opt-in — run explicitly with `cargo test --test ui_scripts -- --ignored`.

**Not wired into CI in this pass.** `.github/workflows/ci.yml` continues to run only `cargo test` (which skips `#[ignore]`d tests). Wiring this suite into CI is follow-up work gated on solving the keyring/headless-launch problem.

## Non-Goals

- Screenshot/pixel-diff verification — this suite is data-model assertions only. Visual regression testing stays with the existing `docs/screenshots/*.osmscript` + human/LLM review.
- Testing Add/Building/Extrude's actual editing output (node/way creation) — needs additional assert ops (e.g. way/node counts) not built here. Follow-up.
- CI wiring — blocked on a headless/no-keyring launch path.
- A generic "assert any field" mechanism — `assert_mode` is purpose-built; broaden the pattern only when a second concrete assertion need shows up.

## Files Touched (anticipated)

- `src/script/op.rs` — `Op::AssertMode(EditMode)` variant.
- `src/script/parser.rs` — `assert_mode` line parsing + unit tests.
- `src/script/runner.rs` — `AppHandle::assert_mode`, `run_step` match arm, `Fake::mode` field + unit tests.
- `src/script_harness.rs` — `ScriptCommand::AssertMode`, `ScriptBus::assert_result` + getter/setter, `LiveApp::assert_mode`, `process_script_command` match arm.
- `src/main.rs` — derive `PartialEq`/`Debug` on `EditMode` if missing.
- New `tests/ui/mode_switching.osmscript`
- New `tests/ui/mode_requires_layer.osmscript`
- New `tests/ui_scripts.rs` — ignored integration test runner.
