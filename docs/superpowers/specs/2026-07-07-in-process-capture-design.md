
# In-Process Screenshot Capture — Design

## Purpose

Replace the `capture` op's `screencapture -l <windowid>` shell-out with gpui's
built-in in-process texture readback, so scripted UI tests no longer depend on
macOS Screen Recording permission, on-screen window visibility, or
`CGWindowListCopyWindowInfo` window-id lookup.

## Background

The forked `gpui` crate (pinned rev in `Cargo.toml`) exposes
`Window::render_to_image() -> anyhow::Result<image::RgbaImage>`, gated behind
the `test-support` cargo feature. It renders the current frame's scene to a
Metal texture and reads the pixels back directly — gpui's own visual-test
harness (`VisualTestAppContext::capture_screenshot`) uses exactly this path.
No OS screenshot APIs, no permission prompts, and the window does not need to
be on-screen or unoccluded.

## Changes

1. **`Cargo.toml`** — add `features = ["test-support"]` to the `gpui`
   dependency. Drop `core-foundation` / `core-graphics` (only used for the
   window-id lookup being removed).
2. **`src/script_harness.rs`** — add `ScriptCommand::Capture { path: PathBuf }`.
   Handled in `process_script_command` on the gpui main thread: call
   `window.render_to_image()`, create parent dirs, save via
   `RgbaImage::save`. Result (`Result<(), String>`) is stashed on `ScriptBus`
   (new `capture_result: Mutex<Option<Result<(), String>>>` field) for the
   runner thread to pick up after `submit()` returns, same pattern already
   used for command completion signaling.
3. **`src/script/runner.rs`** — add `fn capture(&mut self, path: &Path) -> Result<(), String>` to
   the `AppHandle` trait. `Op::Capture` calls `app.capture(&pb)` instead of
   `capture::capture(self.window_id, &pb)`.
4. **`src/capture.rs`** — delete. No longer needed.
5. **`src/main.rs`** — remove `find_own_window_id()` call and the associated
   fixed startup sleep used only to let the window become visible before
   OS-level lookup; `Runner`/`LiveApp` drop the now-unused `window_id` field.
6. **`src/idle_tracker.rs`, script parser, DSL** — unchanged. `capture PATH`
   keeps the same syntax and semantics from the script author's perspective.

## Error Handling

Same as today: a failed capture (e.g. bad path, encode error) becomes a
`RunError` at the line number of the `capture` op, script exits 1.

## Testing

- Existing parser/runner unit tests unaffected (they use a `Fake` `AppHandle`;
  add a no-op `capture` impl to the fake).
- Manual smoke test: run `docs/screenshots/smoke.osmscript` and confirm PNGs
  are produced without a Screen Recording permission prompt, including with
  the app window occluded by another window.

## Non-Goals

- Cross-platform capture (Linux/Windows `PlatformWindow::render_to_image` are
  not implemented upstream as of this writing; capture remains effectively
  macOS-only in practice, same as before).
- Pixel-exact diffing — unchanged from the original design.
