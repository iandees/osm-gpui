# gpui-component Migration, Right-Drag Panning & Dependency Update — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Update dependencies, switch map panning to right-click-drag, and migrate all UI from zed's `ui`/`theme` crates to [gpui-component](https://longbridge.github.io/gpui-component), rebuilding the right (layer/tag) pane on top of it.

**Architecture:** Work proceeds in ordered tasks that each keep the app compiling and runnable. The risky step — pinning gpui to gpui-component's rev and introducing `gpui_component::init` + `Root` — is done once, with zed's `ui`/`theme` kept alive (pinned to the same rev) so they can be peeled off incrementally afterward. zed dependencies are removed only in the final task, once nothing references them.

**Tech Stack:** Rust, gpui (zed rev `1d217ee39d381ac101b7cf49d3d22451ac1093fe`), gpui_platform, gpui-component (longbridge, git).

## Global Constraints

- gpui / gpui_platform MUST be pinned to zed rev `1d217ee39d381ac101b7cf49d3d22451ac1093fe` (gpui-component's rev). While zed `ui`/`theme` are still present they MUST be pinned to the same rev.
- `gpui-component` is added as `gpui-component = { git = "https://github.com/longbridge/gpui-component" }` and imported in code as `gpui_component`.
- Each window's top-level content MUST be wrapped in `gpui_component::Root::new(view, window, cx)`; `gpui_component::init(cx)` MUST be called once at startup before any window opens.
- Commit messages: single line, no `Co-Authored-By` trailer.
- Do not touch dead files: `src/map.rs`, `src/data.rs`, `src/background.rs`, `src/mercator.rs` (not in `lib.rs`, not compiled).
- Left mouse = selection only (click selects, drag is a no-op). Right mouse = pan. Moving selected items is OUT OF SCOPE.
- Verification for GUI tasks is `cargo build` + `cargo clippy` + a manual run; existing `cargo test` unit tests (in `coordinates.rs`, `viewport.rs`, `custom_imagery_dialog.rs`, `background.rs`) MUST stay green throughout.

---

### Task 1: Bump non-gpui dependencies

Independent, low-risk. Updates crates unrelated to the gpui pin so the churn is isolated from the migration.

**Files:**
- Modify: `Cargo.toml` (`[dependencies]`, `[target.'cfg(target_os = "macos")'.dependencies]`)

**Interfaces:**
- Produces: an updated, building dependency set. No API surface for later tasks except a clean `Cargo.lock`.

- [ ] **Step 1: Bump versions in `Cargo.toml`**

Update these lines to the latest compatible releases (leave `gpui`, `gpui_platform`, `theme`, `ui` untouched — Task 2 handles them):

```toml
anyhow = "1.0"
dirs = "6"
image = "0.25"
quick-xml = "0.37"
rfd = "0.15"
schemars = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
smallvec = "1.0"
ureq = { version = "2.12", features = ["json"] }
```

macOS target deps:

```toml
core-foundation = "0.10"
core-graphics = "0.24"
```

Keep `ureq` on the latest `2.x` (avoid the `3.x` API rewrite). If `quick-xml 0.37` changes the reader API used in `src/osm.rs`, adapt those call sites (the parser uses `quick_xml::Reader` + `read_event`; method/enum names are stable across recent minors, but verify).

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: compiles. If `quick-xml` breaks, fix `src/osm.rs` event-reading calls until it builds.

- [ ] **Step 3: Run unit tests**

Run: `cargo test`
Expected: all existing tests pass.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src/osm.rs
git commit -m "Bump non-gpui dependencies to latest"
```

---

### Task 2: Pin zed crates, add gpui-component, swap to init + Root (both systems coexist)

The atomic integration step. Pins gpui backward to gpui-component's rev, introduces `gpui_component::init` and `Root`, and keeps zed `ui`/`theme` alive so `settings_window.rs` still compiles.

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/main.rs` (imports at top; `main()` runner around lines 1585–1668; the settings-window opener around lines 1733–1748; `MapViewer::render` return around line 1062; `DefaultThemeSettings` block lines ~50–70)
- Modify: `src/ui/settings_window.rs` (wrap its rendered root in `Root` where the window opens — done in `main.rs`, so this file may be untouched unless a gpui API drift fix is needed)

**Interfaces:**
- Consumes: the building state from Task 1.
- Produces:
  - `gpui_component` available crate-wide.
  - Both windows' content wrapped in `gpui_component::Root`.
  - `gpui_component::init(cx)` called at startup.
  - zed `theme::init` still called (kept for now); `DefaultThemeSettings` kept for now.

- [ ] **Step 1: Pin zed crates and add gpui-component in `Cargo.toml`**

```toml
gpui = { git = "https://github.com/zed-industries/zed", rev = "1d217ee39d381ac101b7cf49d3d22451ac1093fe" }
gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "1d217ee39d381ac101b7cf49d3d22451ac1093fe", features = ["font-kit"] }
theme = { git = "https://github.com/zed-industries/zed", rev = "1d217ee39d381ac101b7cf49d3d22451ac1093fe" }
ui = { git = "https://github.com/zed-industries/zed", rev = "1d217ee39d381ac101b7cf49d3d22451ac1093fe" }
gpui-component = { git = "https://github.com/longbridge/gpui-component" }
```

- [ ] **Step 2: Build to surface gpui API drift**

Run: `cargo build`
Expected: may fail because the code was written against a newer zed HEAD. Fix each error at its call site (renamed/moved gpui items). Re-run until it builds against the pinned rev. Do NOT add features you don't need. This step is done when `cargo build` succeeds with zed `ui`/`theme` still in use.

- [ ] **Step 3: Add `gpui_component::init(cx)` in the app runner**

In `src/main.rs`, inside `gpui_platform::application().run(move |cx: &mut App| { ... })` (around line 1585), keep the existing `theme::init(...)` / `set_theme_settings_provider(...)` lines and add, immediately after them:

```rust
gpui_component::init(cx);
```

- [ ] **Step 4: Wrap the map window content in `Root`**

In the map window opener (around line 1657), change the view constructor closure so the returned entity is a `Root`. The pattern from gpui-component examples:

```rust
|window, cx| {
    cx.bind_keys([
        KeyBinding::new("cmd-o", OpenOsmFile, None),
        KeyBinding::new("cmd-shift-d", DownloadFromOsm, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
    ]);
    let view = cx.new(|cx| MapViewer::new(window, cx));
    cx.new(|cx| gpui_component::Root::new(view.into(), window, cx))
},
```

Confirm the exact `Root::new` signature against `examples/hello_world/src/main.rs`; some versions take `(AnyView, window, cx)` and are themselves the window root element.

- [ ] **Step 5: Wrap the settings window content in `Root`**

In the settings window opener (around line 1733–1748), apply the same `Root::new(...)` wrapping around the `SettingsWindow` view.

- [ ] **Step 6: Build and run**

Run: `cargo build`
Expected: compiles.
Run: `cargo run` and confirm the map window opens, tiles/features render, and the settings window opens (Cmd-,). The right pane may look unchanged/zed-styled — that's expected at this task.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs
git commit -m "Pin zed to gpui-component rev; add gpui-component init and Root wrappers"
```

---

### Task 3: Switch panning to right-click-drag

Localized to `src/main.rs` event wiring. `src/viewport.rs` is button-agnostic and needs no change.

**Files:**
- Modify: `src/main.rs` (`MapViewer::render`, the map-area div around lines 1082–1105; the hint-text child if present)

**Interfaces:**
- Consumes: `MapViewer::handle_mouse_down/handle_mouse_move/handle_mouse_up` (unchanged signatures), `Viewport::handle_mouse_down/move/up`.
- Produces: right-drag pans; left-click selects; left-drag no-op.

- [ ] **Step 1: Rebind pan handlers to the right button**

In the map-area `div()` (around line 1082), change the pan wiring so `handle_mouse_down` (which starts the viewport drag) and the mouse-up handlers fire on the **right** button, while a **new** left-button down handler records only the click-start position for selection.

Replace the current block:

```rust
.on_mouse_down(
    gpui::MouseButton::Left,
    cx.listener(|this, ev: &MouseDownEvent, _, _| {
        this.handle_mouse_down(ev);
    }),
)
.on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
    this.handle_mouse_move(ev, cx);
}))
.on_mouse_up(
    gpui::MouseButton::Left,
    cx.listener(|this, ev: &MouseUpEvent, _, cx| {
        this.handle_mouse_up(ev, cx);
    }),
)
.on_mouse_up_out(
    gpui::MouseButton::Left,
    cx.listener(|this, ev: &MouseUpEvent, _, cx| {
        this.handle_mouse_up(ev, cx);
    }),
)
```

with:

```rust
// Right button drives panning.
.on_mouse_down(
    gpui::MouseButton::Right,
    cx.listener(|this, ev: &MouseDownEvent, _, _| {
        this.handle_mouse_down(ev);
    }),
)
.on_mouse_up(
    gpui::MouseButton::Right,
    cx.listener(|this, ev: &MouseUpEvent, _, cx| {
        this.viewport.handle_mouse_up();
        cx.notify();
    }),
)
.on_mouse_up_out(
    gpui::MouseButton::Right,
    cx.listener(|this, ev: &MouseUpEvent, _, cx| {
        this.viewport.handle_mouse_up();
        cx.notify();
    }),
)
// Left button is selection only: record the press position, decide on release.
.on_mouse_down(
    gpui::MouseButton::Left,
    cx.listener(|this, ev: &MouseDownEvent, _, _| {
        this.mouse_down_pos = Some(ev.position);
    }),
)
.on_mouse_up(
    gpui::MouseButton::Left,
    cx.listener(|this, ev: &MouseUpEvent, _, cx| {
        this.handle_mouse_up(ev, cx);
    }),
)
.on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
    this.handle_mouse_move(ev, cx);
}))
```

Note: `handle_mouse_down` (line 447) both records `mouse_down_pos` and calls `viewport.handle_mouse_down` (which sets `is_dragging`). For the right button we want the full drag start, so we keep calling `handle_mouse_down`. For the left button we only record `mouse_down_pos` (no `is_dragging`), so left-drag never pans. `handle_mouse_up` (line 462) already does the click-vs-drag selection test from `mouse_down_pos` and calls `viewport.handle_mouse_up()`; leaving it on the left button preserves selection. The right-button up handlers only end the viewport drag (they must NOT run the selection test).

- [ ] **Step 2: Update the on-screen hint text**

Search `src/main.rs` for the pan hint string. If a "Drag to pan" hint is rendered in the live `MapViewer` (the dead `map.rs` copy at line 568 is out of scope), change it to `"Right-drag to pan · scroll to zoom"`. If no such hint exists in `MapViewer`, skip.

- [ ] **Step 3: Build and run**

Run: `cargo build && cargo run`
Confirm: right-drag pans the map; scroll zooms; left-click on a feature selects it (tags appear in the right pane); left-drag does nothing.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "Switch map panning to right-click-drag; left button is selection only"
```

---

### Task 4: Rebuild the right (layer/tag) pane on gpui-component ⭐

Extract the pane into its own module and rebuild it with a collapsible `Accordion`, `Checkbox` rows, a right-click menu for reorder/delete, and a `DescriptionList` for tags.

**Files:**
- Create: `src/ui/side_panel.rs`
- Modify: `src/ui/mod.rs` (add `pub mod side_panel;`)
- Modify: `src/main.rs` (replace the inline right-panel `div` block, lines ~1179–1363, with a call into the new module; the `render_selection_panel` method lines ~865–1010 moves/or is invoked from the new module)

**Interfaces:**
- Consumes from `MapViewer`: `self.layer_manager` layer list (name + visibility, from the existing `layer_info` vec built in `render`), `self.selected: Option<FeatureRef>`, `self.context_menu` state, and the existing mutators `toggle_layer_visibility(&str)`, `reorder_layer(usize, usize)`, layer delete (existing right-click delete path), and `feature_tags(&sel)` used by `render_selection_panel`.
- Produces: `side_panel::render(viewer: &MapViewer, cx: &mut Context<MapViewer>) -> impl IntoElement` (or a method `MapViewer::render_side_panel`). Keep the entry point a single function so `main.rs`'s `render` just calls it.

Design note: the spec said "Table" for tags; use gpui-component `DescriptionList` (label = tag key, value = tag value) — it is the natural key/value component. This is an approved refinement.

- [ ] **Step 1: Create the module skeleton**

Create `src/ui/side_panel.rs` with a single entry function that returns the pane element. Move the body of the current inline right panel here. Imports:

```rust
use gpui::{div, prelude::*, px, Context, IntoElement};
use gpui_component::{
    ActiveTheme,
    accordion::Accordion,
    checkbox::Checkbox,
    description_list::{DescriptionItem, DescriptionList},
    label::Label,
    menu::ContextMenuExt,
};
use crate::MapViewer; // adjust to the actual path/visibility of MapViewer
```

If `MapViewer` is defined in `main.rs` (a binary), it is not importable from the library `src/ui/`. In that case keep the pane code as a `impl MapViewer` method inside `main.rs` split into a small `mod side_panel { ... }` submodule of `main.rs`, OR move the render logic inline but factored into private methods. Prefer: add `render_side_panel(&self, cx) -> impl IntoElement` and `render_layers_section` / `render_tags_section` methods on `MapViewer` in `main.rs`, since `MapViewer` lives in the binary. (Skip creating `side_panel.rs` if this path is chosen; note that in the commit.)

- [ ] **Step 2: Build the Layers accordion section**

Replace the hand-drawn layer rows with `Checkbox` rows inside an `Accordion` section. Each row toggles visibility; right-click opens a context menu with Move up / Move down / Delete.

```rust
// Inside the layers section child list:
layer_info.iter().enumerate().map(|(index, (name, is_visible))| {
    let layer_name = name.clone();
    let total = layer_info.len();
    Checkbox::new(("layer", index))
        .checked(*is_visible)
        .label(name.clone())
        .on_click(cx.listener(move |this, _checked: &bool, _, cx| {
            this.toggle_layer_visibility(&layer_name);
            cx.notify();
        }))
        .context_menu({
            let name_for_menu = name.clone();
            move |menu, _window, _cx| {
                let mut menu = menu;
                if index > 0 {
                    menu = menu.menu("Move up", Box::new(MoveLayer { index, delta: -1 }));
                }
                if index + 1 < total {
                    menu = menu.menu("Move down", Box::new(MoveLayer { index, delta: 1 }));
                }
                menu.separator()
                    .menu("Delete", Box::new(DeleteLayer { index }))
            }
        })
}).collect::<Vec<_>>()
```

Define the actions near the top of `main.rs` (data-carrying gpui actions):

```rust
#[derive(Clone, PartialEq, gpui::Action, serde::Deserialize)]
#[action(namespace = layers)]
struct MoveLayer { index: usize, delta: i32 }

#[derive(Clone, PartialEq, gpui::Action, serde::Deserialize)]
#[action(namespace = layers)]
struct DeleteLayer { index: usize }
```

Register handlers on `MapViewer` in `render` (or via `cx.on_action` when the view is created) that call `reorder_layer(index, (index as i32 + delta) as usize)` and the existing delete path, then `cx.notify()`.

**Fallback if action-with-data dispatch is awkward with the pinned gpui-component version:** keep the existing manual `self.context_menu = Some(LayerContextMenu { .. })` mechanism (right-click sets it; it renders as an absolute overlay dismissed on outside click), but (a) add Move up / Move down entries alongside the existing Delete, and (b) restyle its items and the layer rows with gpui-component `Checkbox`/`Label`/`Button` + `cx.theme()` tokens. Verify which path compiles against the pinned version before committing; either satisfies the spec.

- [ ] **Step 3: Build the Selection/Tags accordion section**

Port `render_selection_panel` to emit a `Label` header, an OSM link, and a `DescriptionList` of tags. Preserve the empty states.

```rust
// When nothing selected:
Label::new("Click a feature to see its tags.")

// When selected, tags_vec: Vec<(String, String)>:
if tags_vec.is_empty() {
    DescriptionList::new().child(DescriptionItem::new("").value(Label::new("(no tags)").into_any_element()))
} else {
    DescriptionList::new()
        .columns(1)
        .bordered(true)
        .children(tags_vec.into_iter().map(|(k, v)| {
            DescriptionItem::new(k).value(Label::new(v).into_any_element())
        }))
}
```

Keep the existing OSM link (`render_selection_panel` builds a `link` child around lines 1010); render it as a gpui-component element or a themed `div` above the list.

- [ ] **Step 4: Wrap both sections in a collapsible Accordion and replace the inline panel**

```rust
Accordion::new("side-panel")
    .multiple(true)
    .item(|item| item.title("Layers").child(layers_section))
    .item(|item| item.title("Selection").child(tags_section))
```

Confirm `Accordion` open-state handling against `crates/story/src/stories/accordion_story.rs`: it uses `.item(|this| this.open(bool)...)` plus `.on_toggle_click(...)`. Store the open indices on `MapViewer` (e.g. `side_panel_open: Vec<usize>`, default `vec![0, 1]`) and wire `on_toggle_click` to update it and `cx.notify()`.

Replace the inline right-panel `div` (lines ~1179–1363) in `MapViewer::render` with a call to the new entry function. Theme the panel container with `cx.theme().sidebar` / `cx.theme().border` instead of `rgb(0x111827)` / `rgb(0x374151)`.

- [ ] **Step 5: Build and run**

Run: `cargo build && cargo run`
Confirm: right pane renders via gpui-component; toggling a checkbox shows/hides the layer; right-click offers Move up / Move down / Delete and each works; clicking a feature fills the tag list; both accordion sections collapse and expand.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/ui/mod.rs src/ui/side_panel.rs
git commit -m "Rebuild right layer/tag pane on gpui-component (accordion, checkbox rows, context menu, description list)"
```

---

### Task 5: Replace custom TextInput with gpui-component Input; rebuild the imagery dialog

**Files:**
- Modify: `src/ui/custom_imagery_dialog.rs` (rebuild on gpui-component `Modal`/`Dialog` + `Button` + `Input`; keep `validate` / `error_message` and their tests unchanged)
- Modify: `src/ui/settings_window.rs` (swap `TextInput` fields for gpui-component `InputState`/`Input`)
- Delete: `src/ui/text_input.rs`, `src/ui/modal.rs`
- Modify: `src/ui/mod.rs` (remove `pub mod text_input;` and `pub mod modal;`)

**Interfaces:**
- Consumes: `validate(name, url, min, max)` and `error_message(&e)` in `custom_imagery_dialog.rs` (unchanged), `CustomImageryEntry`.
- Produces: dialogs/forms whose text fields are gpui-component `InputState` entities read via `input_state.read(cx).value()`.

- [ ] **Step 1: Introduce `InputState` fields**

For each text field, create an `InputState` entity in the owner's `new()`:

```rust
let name_input = cx.new(|cx| gpui_component::input::InputState::new(window, cx).placeholder("Name"));
```

Store the entities on the struct; render with `gpui_component::input::Input::new(&self.name_input)`. Read values with `self.name_input.read(cx).value()` in the submit/validate path. Reference `examples/input/src/main.rs` for the exact `InputState::new` signature and `InputEvent::Change` subscription if live validation is wanted (optional — validate on submit is fine).

- [ ] **Step 2: Rebuild the dialog chrome on gpui-component**

Replace the custom `Modal` usage in `custom_imagery_dialog.rs` with gpui-component's modal/dialog and `Button`s (Save / Cancel). Keep calling `validate(...)`/`error_message(...)`; render the error string in a themed `Label`. Reference `crates/story/src/stories/dialog_story.rs` / `modal` API.

- [ ] **Step 3: Delete dead custom widgets**

Remove `src/ui/text_input.rs` and `src/ui/modal.rs` and their `pub mod` lines in `src/ui/mod.rs`. Fix any remaining references.

- [ ] **Step 4: Build, test, run**

Run: `cargo build && cargo test`
Expected: `custom_imagery_dialog` validation tests still pass.
Run: `cargo run`, open Settings, add a custom imagery entry via the dialog; confirm input works, validation errors show for bad input, and a valid entry saves.

- [ ] **Step 5: Commit**

```bash
git add src/ui/ src/main.rs
git commit -m "Replace custom TextInput/Modal with gpui-component Input and Dialog"
```

---

### Task 6: Port settings_window off zed `ui`

**Files:**
- Modify: `src/ui/settings_window.rs` (replace `ui::prelude::*`, `ui::ListHeader`, `ui::ListItem` with gpui-component equivalents)

**Interfaces:**
- Consumes: `custom_imagery_store` (list/load/save), gpui-component components.
- Produces: a settings window with no zed `ui` dependency.

- [ ] **Step 1: Replace the list rendering**

Swap `ListHeader`/`ListItem` for gpui-component list primitives (see `crates/story/src/stories/list_story.rs`) or a themed `div` list using `Label` + `Button` rows. Replace `ui::prelude::*` (which provided zed's `ActiveTheme`) with `gpui_component::ActiveTheme` and use `cx.theme()` tokens. Remove `use ui::...` lines.

- [ ] **Step 2: Build and run**

Run: `cargo build`
Expected: compiles with no reference to zed `ui` anywhere (`grep -rn "use ui\|ui::ListHeader\|ui::ListItem\|ui::prelude" src/` returns nothing).
Run: `cargo run`, open Settings; confirm the imagery list renders and add/remove still work.

- [ ] **Step 3: Commit**

```bash
git add src/ui/settings_window.rs
git commit -m "Port settings window off zed ui to gpui-component"
```

---

### Task 7: Remove zed `ui`/`theme` and finalize

**Files:**
- Modify: `Cargo.toml` (drop `ui` and `theme`)
- Modify: `src/main.rs` (remove `theme::init`, `set_theme_settings_provider`, `use theme`, and the `DefaultThemeSettings` struct + its `theme::ThemeSettingsProvider` impl, lines ~22 and ~50–70 and ~1586–1587)

**Interfaces:**
- Consumes: gpui-component theming already active from Task 2.
- Produces: a build with zero zed `ui`/`theme` usage.

- [ ] **Step 1: Remove zed theme init and provider**

Delete the `theme::init(...)` and `theme::set_theme_settings_provider(...)` calls, the `use theme;` import, and the `DefaultThemeSettings` struct with its `impl theme::ThemeSettingsProvider`. Leave `gpui_component::init(cx)` in place.

- [ ] **Step 2: Drop the deps**

Remove the `theme = { ... }` and `ui = { ... }` lines from `Cargo.toml`.

- [ ] **Step 3: Build clean**

Run: `cargo build`
Expected: compiles. If anything still references `theme::` or `ui::`, fix it (there should be nothing left).
Run: `cargo clippy`
Expected: no new warnings introduced by the migration (fix any that are).

- [ ] **Step 4: Full manual verification**

Run: `cargo run` and confirm the whole acceptance list:
- Right-drag pans; scroll zooms; left-click selects; left-drag no-op.
- Right pane: toggle visibility, reorder via right-click menu, delete via right-click menu; tag list populates on selection; accordion sections collapse/expand.
- Settings window: list renders, add/edit custom imagery via gpui-component input + dialog, validation errors show, entries persist across restart.
- Theme is consistent across both windows.

- [ ] **Step 5: Run tests**

Run: `cargo test`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs
git commit -m "Remove zed ui/theme; gpui-component is the sole UI toolkit"
```

---

## Self-Review

**Spec coverage:**
- Update dependencies → Task 1 (non-gpui) + Task 2 (gpui pin, which the spec notes moves backward to gpui-component's rev). ✓
- Right-drag panning, left-click select, left-drag no-op → Task 3. ✓
- Full migration to gpui-component, drop zed ui/theme → Tasks 2 (introduce), 5–6 (port consumers), 7 (remove). ✓
- Rebuild right pane: Checkbox rows, reorder+delete in right-click menu, collapsible Accordion, tags list → Task 4. ✓
- Move-items deferred / dead files untouched → Global Constraints. ✓

**Placeholder scan:** No "TBD"/"handle edge cases"/"similar to". Uncertain external-lib specifics (exact `Root::new`, `Accordion` toggle, `Input` state, context-menu action dispatch) are each pinned to a named gpui-component reference file the implementer opens, with a concrete fallback given for the one genuinely risky item (context-menu-with-index in Task 4). This is intentional: the API belongs to an external crate at a pinned rev, so the reference-file pointer is the accurate instruction, not a placeholder.

**Type consistency:** `toggle_layer_visibility(&str)`, `reorder_layer(usize, usize)`, `handle_mouse_down/up`, `mouse_down_pos`, `feature_tags` are used consistently with their current definitions in `main.rs`. New actions `MoveLayer { index, delta }` / `DeleteLayer { index }` are defined once and referenced with matching fields.

**Note on TDD:** This is a GUI/dependency migration; the meaningful automated checks are `cargo build`, `cargo clippy`, and keeping the existing `cargo test` suite green. Each task's verification reflects that rather than inventing GUI unit tests. Behavioral correctness is verified by the manual run checklist in Tasks 3, 4, 5, 7.
