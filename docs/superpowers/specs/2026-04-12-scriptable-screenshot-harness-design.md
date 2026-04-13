# Scriptable Screenshot Harness — Design

## Purpose

Give Claude (and anyone else) a reliable way to exercise the app end-to-end and capture PNG screenshots from scripted interactions. The goal is to make visual changes and interaction changes testable without a human in the loop: run a script, read back PNGs, verify behavior.

Covers both rendering regressions ("do tiles still render right after a refactor?") and interaction regressions ("does drag-to-pan still work?").

## Shape & Entry Point

Add a `--script <path>` flag to the existing `osm-gpui` binary. No separate test binary — the harness drives the real app end-to-end.

When `--script` is passed:

1. The app boots normally and opens its window.
2. A **script runner** task reads the script file and executes steps sequentially against the live `Map` view.
3. Input events (`drag`, `click`, `key`, `scroll`) are dispatched *through gpui's own event system* from inside the process — not via `CGEvent` — so they're deterministic and need no accessibility permissions.
4. `capture` steps shell out to `screencapture -l <windowid> <path>`. The window id is resolved once at startup via `CGWindowListCopyWindowInfo` filtered by our PID.
5. When the script ends, the process exits `0`. With `--keep-open`, the app stays up for manual inspection.

Additional flags:

- `--window-size WxH` — set the window size before the script runs, so captures are reproducible across machines.
- `--keep-open` — do not exit after the final step.

When neither `--script` nor related flags are passed, the app behaves exactly as today.

## Script Language

Line-oriented, `#` comments, whitespace-separated tokens. Coordinates are window pixels, origin top-left.

Example:

```
# setup
window 1200 800
viewport 47.6062 -122.3321 12

wait_idle 5s
capture docs/screenshots/seattle-initial.png

drag 600,400 300,400
wait_idle
capture docs/screenshots/seattle-panned.png

scroll 600,400 dy=-5
wait_idle
capture docs/screenshots/seattle-zoomed.png

key cmd+0
wait 500ms
capture docs/screenshots/seattle-reset.png
```

### Ops

| Op | Arguments | Behavior |
|---|---|---|
| `window` | `W H` | Resize window. Intended to appear once near the top. |
| `viewport` | `LAT LON ZOOM` | Jump the map to a viewport. |
| `wait_idle` | `[TIMEOUT]` | Block until the idle signal fires. Default timeout 10s. On timeout, script fails. |
| `wait` | `DURATION` | Hard sleep. Accepts `500ms`, `2s`, etc. |
| `drag` | `X1,Y1 X2,Y2 [duration=200ms]` | Mouse-down, ~12 interpolated moves over `duration` (frame-driven), mouse-up. |
| `click` | `X,Y [button=left]` | Mouse-down + mouse-up at the same point, ~16ms apart. |
| `scroll` | `X,Y dx=N dy=N` | One `ScrollWheel` event at `(X,Y)` with the given deltas. |
| `key` | `CHORD` | Parse `cmd+shift+x`-style chords; emit `KeyDown` then `KeyUp`. |
| `capture` | `PATH` | Shell out to `screencapture -l <windowid> -o -x PATH`. Creates parent dirs. |
| `log` | `MSG...` | Print to stdout for diagnostics. |

Parser: per-line `split_whitespace` + per-op argument parsing. Errors include file path and line number.

## Idle Signal

`wait_idle` returns when the map has finished reacting to the previous step.

**Idle = all of the following hold for 2 consecutive frames:**

1. `tile_cache`: zero tiles in `Pending` state for the current viewport's visible set.
2. `http_image_loader`: zero in-flight HTTP requests, zero images awaiting decode.
3. No viewport animation in progress (applies once smooth zoom/pan exists; today this is vacuously true).

### Implementation

Add an `IdleTracker` held in the app's global state:

```rust
pub struct IdleTracker {
    pending_tile_fetches: AtomicUsize,
    pending_image_decodes: AtomicUsize,
    // future: animation in-progress flag
}

impl IdleTracker {
    pub fn is_idle(&self) -> bool { /* all counters == 0 */ }
}
```

- `tile_cache` increments on fetch request, decrements on fetch completion (success or failure).
- `http_image_loader` increments on HTTP request start and image-decode submission, decrements on completion.

The script runner polls `is_idle()` once per frame. It requires **two consecutive `true` reads** before returning, to avoid the "tile A finishes → app queues tile B" race that would otherwise capture a half-loaded map.

## Input Injection

Events go through gpui, not the OS. The script runner constructs `PlatformInput` variants (`MouseDown`, `MouseMove`, `MouseUp`, `ScrollWheel`, `KeyDown`, `KeyUp`) and dispatches them into the active `Window` on the main thread.

Per-op mapping:

- **drag** — one `MouseDown` at `(X1,Y1)`; ~12 interpolated `MouseMove`s spread across the duration, driven by the frame clock (not `thread::sleep`); one `MouseUp` at `(X2,Y2)`. Frame-driven timing matters for interactions like drag-end-on-mouse-up-outside (`cb35709`).
- **click** — `MouseDown` + `MouseUp` at the same point, ~16ms apart.
- **scroll** — single `ScrollWheel` with the provided deltas.
- **key** — parse modifier tokens + key; emit `KeyDown` then `KeyUp`.

**Open implementation detail:** the exact gpui API for synthesizing events from application code. Confirm during implementation by reading the `gpui` crate (already in `Cargo.lock`). If gpui doesn't expose a clean dispatch path, fallback is an app-level `TestInputBus` that `map.rs` drains alongside real events. Uglier but fully within our control.

## Capture Mechanics

**Window id lookup.** After the window is shown, call `CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly, kCGNullWindowID)`, filter by `kCGWindowOwnerPID == getpid()`, and cache `kCGWindowNumber`. Uses `core-foundation` / `core-graphics` crates (already transitively present via gpui on macOS).

**Capture subprocess.** `screencapture -l <windowid> -o -x <path>`, blocking. Flags:

- `-l` — capture the specified window id (works even when occluded/offscreen, so no focus-stealing).
- `-o` — omit window shadow.
- `-x` — no sound.

Parent directories are created if missing. Non-zero exit fails the script with a line number.

**Retina.** A 1200×800 logical window produces 2400×1600 PNGs on retina displays. Captures are for visual inspection, not pixel-exact diffs. If pixel-exact diffs become a need, add downscaling or switch to in-process capture.

## Error Handling & Exit

- **Script errors** (parse error, unknown op, `wait_idle` timeout, capture non-zero): stderr `script error at line N: <message>`, process exit `1`. Partial captures from earlier steps remain on disk.
- **Normal completion:** exit `0`, unless `--keep-open`.

**Logging.** `--script` implies verbose stdout:

```
step 1: window 1200 800
  ok (2ms)
step 2: viewport 47.6062 -122.3321 12
  ok (1ms)
step 3: wait_idle 5s
  ok (1420ms)
step 4: capture docs/screenshots/seattle-initial.png
  ok (84ms) -> docs/screenshots/seattle-initial.png
```

## Testing the Harness Itself

1. **Unit tests** for the script parser — one test per op covering valid syntax, invalid syntax, and edge cases (missing args, bad coordinates, unknown modifier). Parser tests live alongside the parser.
2. **`IdleTracker` unit test** — increment/decrement counters, verify `is_idle()` transitions.
3. **Smoke script** at `docs/screenshots/smoke.osmscript` exercising every op against a known viewport (Seattle, zoom 12). Produces a fixed set of PNGs. Not asserted pixel-wise; its job is to fail loudly when gpui API drift or an idle-tracker regression breaks the wiring. Run manually, not in CI.

## Non-Goals

- Pixel-exact visual diffing. Captures are for human/LLM inspection.
- Cross-platform support. macOS only; `screencapture` and `CGWindowList` are Apple-specific.
- Recording real user input into a script. Scripts are hand-authored.
- JSON/TOML script formats. Line DSL only; add other formats only if a tool needs to emit scripts.
- Headless offscreen rendering. A visible window is required because capture goes through the window server.

## Files Touched (anticipated)

- `src/main.rs` — CLI flags, script-runner wiring.
- `src/lib.rs` — expose `IdleTracker` and script runner modules.
- `src/tile_cache.rs`, `src/http_image_loader.rs` — increment/decrement idle counters.
- New `src/script/mod.rs` — parser, runner, op types.
- New `src/script/parser.rs` — line DSL parser + tests.
- New `src/idle_tracker.rs` — counters + tests.
- New `src/capture.rs` — window-id lookup + `screencapture` subprocess.
- New `docs/screenshots/smoke.osmscript` — harness smoke test.
