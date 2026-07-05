# gpui-component Migration, Right-Drag Panning & Dependency Update

**Date:** 2026-07-05
**Status:** Approved (design)

## Summary

Three connected changes to `osm-gpui`:

1. **Update dependencies** to current versions.
2. **Switch map panning** from left-click-drag to **right-click-drag**, freeing the left button for selection only.
3. **Migrate the UI from zed's `ui`/`theme` crates to [gpui-component](https://longbridge.github.io/gpui-component)**, and **rebuild the right (layer/tag) pane** on top of it.

The work is organized into four phases, each of which leaves the app compiling and runnable.

## Background & constraints

- The live app is `src/main.rs` (`MapViewer`) plus the library modules declared in `src/lib.rs`. **`src/map.rs`, `src/data.rs`, `src/background.rs`, and `src/mercator.rs` are dead code** — not declared in `lib.rs` and not part of the binary. They are out of scope and left untouched.
- The live panning path is `src/main.rs` (`handle_mouse_down/move/up`, event wiring in `render`) plus `src/viewport.rs` (button-agnostic drag math). No changes to `map.rs`.
- **zed dependency surface is small:**
  - `src/main.rs` uses `theme::init`, `theme::set_theme_settings_provider`, and a local `DefaultThemeSettings: theme::ThemeSettingsProvider`.
  - `src/ui/settings_window.rs` uses `ui::prelude::*`, `ui::ListHeader`, `ui::ListItem`.
  - `src/ui/text_input.rs`, `src/ui/modal.rs`, `src/ui/custom_imagery_dialog.rs` are **raw gpui with hardcoded colors** (references to `ui::`/`crate::ui::` are to our own `crate::ui` module, not zed's `ui`).
- **gpui-component pins gpui to a specific zed rev** (`1d217ee39d381ac101b7cf49d3d22451ac1093fe`) and itself uses `gpui` + `gpui_platform` from zed. To adopt it we must pin our `gpui`/`gpui_platform` to that same rev. This may move `gpui` *backward* from current unpinned HEAD, so some gpui API usages may need adapting.
- gpui-component requires `gpui_component::init(cx)` at startup and each window's top-level content wrapped in its `Root` view.

## Decisions (from brainstorming)

- **Scope:** Full migration — drop zed `ui`/`theme` entirely; rebuild right pane, settings window, custom imagery dialog, and text input on gpui-component.
- **Left mouse after the change:** left-click selects (as today); left-drag is a **no-op**. Moving selected items is deferred to a separate future task.
- **Right pane reorder:** move layer reorder into the right-click `PopupMenu` (Move up / Move down / Delete). Rows become `Checkbox` + `Label`.
- **Right pane sections:** Layers and Selection/Tags become **collapsible via gpui-component `Accordion`**.

## Phase 1 — Dependencies & gpui-component foundation

**Cargo.toml**
- Pin `gpui` and `gpui_platform` (keep `features = ["font-kit"]`) to gpui-component's rev.
- Add `gpui-component = { git = "https://github.com/longbridge/gpui-component" }` (imported as `gpui_component`).
- **Remove** the zed `ui` and `theme` dependencies.
- Bump remaining crates to latest: `anyhow`, `dirs`, `image`, `quick-xml`, `rfd`, `schemars`, `serde`, `serde_json`, `smallvec`, `ureq`, `core-foundation`, `core-graphics`. Adapt code for any breaking changes (notably `quick-xml` parsing API and `ureq` if it moves to 3.x — prefer staying on the latest 2.x if 3.x churn is large; the deciding factor is a clean compile with minimal call-site changes).

**App initialization (`src/main.rs`)**
- Replace `theme::init(...)` + `theme::set_theme_settings_provider(...)` with `gpui_component::init(cx)`.
- Remove `DefaultThemeSettings` and its `theme::ThemeSettingsProvider` impl.
- Set a dark theme matching the current look.
- Wrap the root content of both windows (map viewer + settings window) in gpui-component's `Root`.

**Exit criterion:** `cargo build` succeeds; both windows open and render (pane may still look unstyled at this point).

## Phase 2 — Right-drag panning

In `src/main.rs` `render`:
- Rebind pan handlers (`on_mouse_down`, `on_mouse_up`, `on_mouse_up_out`) from `MouseButton::Left` to `MouseButton::Right`.
- Keep `on_mouse_move` (button-agnostic; `viewport.handle_mouse_move` only pans while `is_dragging`, which now begins on right-down).
- Keep left-button handling for **click selection only**: left-down records `mouse_down_pos`; left-up performs the click/select test. Left-drag does nothing because it never sets `is_dragging`.
- Update the on-screen hint text from "Drag to pan" to "Right-drag to pan."

`src/viewport.rs` needs no logic change (already button-agnostic).

**Exit criterion:** right-drag pans; scroll zooms; left-click selects a feature; left-drag does nothing.

## Phase 3 — Right (layer/tag) pane rebuilt on gpui-component ⭐

Extract the pane from `main.rs`'s inline `render` into its own module, `src/ui/side_panel.rs`, as a focused unit that takes the data it needs (layer list, selection/tags, callbacks) and renders the panel. This keeps `main.rs` smaller and the pane independently reasoned-about.

**Structure:** a gpui-component `Accordion` with two collapsible sections:

- **Layers section**
  - One row per layer: gpui-component `Checkbox` (visibility toggle) + `Label` (layer name).
  - Right-click opens a gpui-component `PopupMenu` with **Move up**, **Move down**, **Delete** (Move up/down disabled at the ends).
  - Reorder arrow buttons and the `#index` badge are removed.
- **Selection / Tags section**
  - `Label` header + a `Link` to the OSM object.
  - Tag key/value pairs rendered with gpui-component `Table`.
  - Empty states ("Click a feature to see its tags." / "(no tags)") preserved.

**Theming:** replace hardcoded hex colors with gpui-component theme tokens (`cx.theme()`), so the pane matches the active theme.

**Exit criterion:** pane renders via gpui-component; visibility toggle, reorder-via-menu, and delete all work; selecting a feature shows its tags in the table; sections collapse/expand.

## Phase 4 — Port remaining windows

- **`src/ui/settings_window.rs`:** replace zed `ui::ListHeader`/`ui::ListItem`/`ui::prelude` with gpui-component equivalents; theme via `cx.theme()`.
- **Text input:** replace the custom `TextInput` (`src/ui/text_input.rs`) with gpui-component's `Input`; update call sites in `settings_window.rs` and `custom_imagery_dialog.rs`.
- **Modal/dialog:** rebuild `src/ui/custom_imagery_dialog.rs` on gpui-component `Modal`/`Dialog` + `Button`; validation logic (`validate`, `error_message`) is unchanged.
- Remove the now-dead `src/ui/text_input.rs` and `src/ui/modal.rs` once nothing references them.

**Exit criterion:** settings window and custom imagery dialog open, accept input, validate, and save; no remaining references to zed `ui`/`theme`.

## Verification

- `cargo build` and `cargo clippy` clean.
- Run the app and confirm:
  - Right-drag pans; scroll zooms; left-click selects; left-drag is a no-op.
  - Right pane: toggle visibility, reorder via right-click menu, delete via right-click menu; tag table populates on selection; accordion sections collapse/expand.
  - Settings window: list renders, add/edit custom imagery via gpui-component input + dialog, validation errors show, entries persist.
  - Theme is consistent across both windows.

## Risks

1. **gpui rev pin drift** — pinning gpui to gpui-component's rev may require adapting gpui API usages that were written against a newer HEAD. Mitigated by doing the pin + theme/init swap atomically in Phase 1 and letting the compiler guide fixes.
2. **gpui-component API learning curve** — `Input`, `Table`, `PopupMenu`, `Accordion`, and `Root` usage patterns; consult gpui-component examples.
3. **Breaking non-gpui dep bumps** — `quick-xml` and possibly `ureq`; contained to their call sites.

## Out of scope

- Moving/editing selected features (geometry edits, undo, persistence) — deferred to a future task.
- Removing/refactoring the dead `map.rs`/`data.rs`/`background.rs`/`mercator.rs` files.
