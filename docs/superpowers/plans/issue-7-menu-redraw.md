# Issue 7: Trigger redraw on menu interactions

## Context
Menu actions `DownloadFromOsm` and `AddOsmCarto` push to static queues (`DOWNLOAD_REQUESTS`, `LAYER_REQUESTS`) but never notify any entity, so gpui does not re-render the `MapViewer`. The drains live inside `MapViewer::render()` — they only fire when render is otherwise invoked (e.g. by a mouse drag). Users see the app sit idle after picking a menu item until they nudge it.

The idiomatic gpui fix from inside a free action handler (`cx: &mut App`) is to queue a refresh for all open windows. `App::refresh_windows(&mut self)` does exactly that — it enqueues `Effect::RefreshWindows`, which causes gpui to re-render each window on the next effect cycle.

## Approach
- `src/main.rs`:
  - In `download_from_osm(_: &DownloadFromOsm, cx: &mut App)` (currently takes `_cx`): take `cx` by name and, after pushing to `DOWNLOAD_REQUESTS`, call `cx.refresh_windows();`.
  - In `add_osm_carto(_: &AddOsmCarto, cx: &mut App)` (currently takes `_cx`): same — take `cx` by name and call `cx.refresh_windows();` after queuing.
  - Leave `open_osm_file` alone — it already spawns an async task which wakes the loop.
  - Leave `quit` alone.

## Verification
- `cargo build --release` clean.
- `cargo test --lib` passes.
- Manually (will document in the PR test plan for the human to run): launch the app, pick `File > Download from OSM` — the "Downloading OSM data…" status banner should appear immediately without any additional mouse movement; pick `Imagery > OpenStreetMap Carto` on a fresh launch — tiles should begin fetching immediately.

## Out of scope
- Any change to how the queues are drained.
- Adding `refresh_windows` to `AddCoordinateGrid` (added in open PR #10 for issue #6). A follow-up commit on that branch — or conflict-resolution when the two PRs merge — will apply the same fix there. Call this out in the PR body.
- Replacing the static-queue pattern with entity-scoped channels.
