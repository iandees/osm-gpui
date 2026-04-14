# Issue 24: Clean up the UI

## Context
All four items live in `src/main.rs`. The header bar rendering (`"🗺️ OSM-GPUI Map Viewer (Layered)"` plus the hint strip) is the flex-column child at `src/main.rs:910-931`, with a matching `h_12()` (48px) offset baked into four mouse handlers (lines 361, 370, 379, 442) and into the render-frame size math (line 874). The `"🏗️ Layer Controls"` panel label sits at `src/main.rs:1055`. The layer-row div runs `src/main.rs:1071-1192` and currently uses `.p_3()` padding, `.gap_3()` between children, plus internal `.gap_2()` and `.gap_1()` for the checkbox row and reorder buttons, and the list container uses `.p_4()` + `.gap_2()` at `src/main.rs:1061-1064`. The debug overlay (zoom, center, objects, tiles, cache, FPS) is the absolute-positioned floating div at `src/main.rs:989-1008`, always rendered; the app's three menus are built in `rebuild_menus` at `src/main.rs:1554-1622` and new toggle actions follow the `AddCoordinateGrid` pattern (declared via `actions!` at line 22, wired via `cx.on_action` at line 1386, and added as a `MenuItem::action` entry).

## Approach

### 1. Remove the header/title bar
- Delete the child at `src/main.rs:909-931` (the `// Header with menu` div and everything it produces).
- Because the main content area was a `flex_col` with header + map, the remaining `child(...)` (the map area starting at line 932) can stay wrapped in the `flex_col`. Simpler: collapse the outer `div().flex_1().flex().flex_col()` wrapper (lines 905-908) so the map child becomes the sole `flex_1` child of the root row. Either form works; prefer collapsing since it removes now-dead `flex_col()`.
- Remove the `header_height = px(48.0)` offset in all four handlers:
  - `handle_mouse_down` (src/main.rs:361-362) — use `event.position` directly.
  - `handle_mouse_move` (src/main.rs:370-371) — same.
  - `handle_mouse_up` (src/main.rs:379-380) — same.
  - `handle_scroll` (src/main.rs:442-443) — same.
- Remove the `header_height` subtraction in `render`'s viewport size calculation (src/main.rs:874, 877), so `map_size.height = window_size.height`.

### 2. Rename "Layer Controls" → "Layers"
- `src/main.rs:1055`: change `.child("🏗️ Layer Controls")` to `.child("Layers")` (drop the emoji per the tightening-up spirit of the issue; if the emoji is desired keep `"🏗️ Layers"`). Default to `"Layers"` plain.

### 3. Shorten layer rows / reduce padding
- Outer list container at `src/main.rs:1061-1064`: change `.p_4()` → `.p_2()` and `.gap_2()` → `.gap_1()`.
- Each row at `src/main.rs:1071-1082`:
  - `.p_3()` → `.p_2()` (reduces row vertical padding from 12px to 8px).
  - `.gap_3()` → `.gap_2()` (between reorder handle, label group, and index badge).
- Reorder handle buttons at `src/main.rs:1102-1103` and `1127-1128`: keep `w(px(18.0))` but reduce `h(px(14.0))` → `h(px(12.0))`; drop the inter-button `.gap_1()` at line 1098 (or tighten to no-op) so the combined ▲/▼ stack shrinks ~4px.
- Checkbox at `src/main.rs:1158-1159`: shrink from 20×20 to 16×16 so the row's natural height drops; adjust inner `.text_sm()` check to `.text_xs()` at line 1171.
- Target row height: roughly 28-32px (down from ~48px). No specific min-height is set anywhere today, so reducing padding + checkbox size is sufficient.

### 4. Debug overlay default-hidden with menu toggle
- Add a `show_debug_overlay: bool` field to `MapViewer` (`src/main.rs:197-210`); initialize to `false` in `MapViewer::new` (src/main.rs:223-234).
- Gate the overlay render: wrap the child at `src/main.rs:989-1008` in `if self.show_debug_overlay { … .into_any_element() } else { div().into_any_element() }`, mirroring the status-message pattern at lines 1009-1027.
- Add a new action `ToggleDebugOverlay` to the `actions!` list at `src/main.rs:22`.
- Add a handler fn (sibling of `add_coordinate_grid` at `src/main.rs:1543`) that flips the flag. Since action handlers receive `&mut App` and not `MapViewer`, use the same queue pattern as `LAYER_REQUESTS`: declare a new `static TOGGLE_DEBUG_OVERLAY: OnceLock<Arc<Mutex<Vec<()>>>>` (alongside the existing statics around `src/main.rs:59-66`), push from the action fn, drain in a new `check_for_toggle_requests` called from `render` (alongside `check_for_download_requests` at `src/main.rs:862`), and toggle `self.show_debug_overlay` there.
- Register the action with `cx.on_action(toggle_debug_overlay);` near `src/main.rs:1386`.
- Initialize the `OnceLock` where `LAYER_REQUESTS`/`DOWNLOAD_REQUESTS` are initialized (search for the `get_or_init` calls; they are set up during app startup, same block as the other queues — add the new queue there).
- Add a new "View" menu in `rebuild_menus` (src/main.rs:1601-1621) after `Imagery`, containing a single `MenuItem::action("Show Debug Overlay", ToggleDebugOverlay)`. (The menu name is "View" per the issue's "toggle menu item" phrasing; if an existing View-like menu is preferred, the only other candidate is `Imagery`, which is semantically wrong — introduce "View".)
- Add a `KeyBinding` if desired (out of scope unless trivially free; the issue asks for a menu item only).
- Note: menu item labels cannot reflect current state in gpui easily; use a static label `"Show/Hide Debug Overlay"` or just `"Toggle Debug Overlay"`. Prefer `"Toggle Debug Overlay"` for clarity.

## Verification
- `cargo build --release` clean.
- `cargo test --lib` passes.
- Manual:
  - Launch app; the top header strip ("🗺️ OSM-GPUI Map Viewer (Layered)" + hint text) is gone and the map fills from the window chrome down.
  - Clicking/dragging/scrolling on the map responds at the correct coordinates (no 48px vertical offset glitch) — pan to a known tile and confirm the cursor tracks features.
  - Right panel header reads "Layers".
  - Layer rows are visibly shorter (roughly 30px vs ~48px) with tighter padding; reorder arrows and checkbox still clickable.
  - On launch, no zoom/center/FPS overlay is visible.
  - `View → Toggle Debug Overlay` shows the overlay; invoking again hides it.

## Out of scope
- Moving the "Mouse to pan/zoom | 'T' tiles | Click layers to toggle" hint elsewhere (it goes away with the header; no replacement surface is introduced).
- Redesigning the layer row visual language (icons, drag handles, active-layer highlight color) beyond the sizing reductions.
- Persisting the debug overlay toggle across runs.
- Adding keyboard shortcut for the overlay toggle.
- Collapsing the right panel or making it resizable.
- Changing window titlebar text (`"OSM-GPUI Map Viewer"` at src/main.rs:1432) — that's the OS title, not the in-app header.
