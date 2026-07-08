# UI Consistency Pass Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify the right pane's row/action styles, add icons to the left mode panel, and replace hardcoded colors/sizes with theme tokens and shared constants. No behavior changes.

**Architecture:** A new shared style module `src/ui/style.rs` (in the `osm_gpui` lib, alongside `modal.rs`) exports panel-width constants, the modal scrim color, and the blessed `panel_row`/`interactive_row` builders. The bin-side panes (`src/side_panel.rs`, `src/fields_section.rs`, `src/mode_panel.rs`, `src/main.rs`) and the dialogs are rewritten against it. Every clickable action becomes a gpui-component `Button`.

**Tech Stack:** Rust, gpui, gpui-component (vendored source for API reference: `~/.cargo/git/checkouts/gpui-component-95ce574d8a0da8b8/b7e63cc/crates/ui/src/`).

**Spec:** `docs/superpowers/specs/2026-07-07-ui-consistency-design.md`

## Global Constraints

- No behavior changes: every existing `on_mouse_down`/`on_click` handler body carries over verbatim.
- Every clickable action is a gpui-component `Button` (`.primary()` for a section's main action; `.ghost()` + `.xsmall()` for inline/per-row actions). No new `Label`+`on_mouse_down` or bare-div buttons.
- Row interaction states use theme list tokens: hover = `cx.theme().list_hover`, selected/current = `cx.theme().list_active`. Do not use `accent` for row backgrounds.
- Single-line commit messages, no Co-Authored-By trailers, no `&&`/`;` compound shell commands, no `cd`/`git -C` prefixes.
- Verify with `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. If a build fails unexpectedly (shared target dir across worktrees), `touch src/main.rs` and rebuild before trusting the error.
- gpui-component API facts (verified against vendored source): `Button` implements `ParentElement` (`.child(...)`), `Styled` (`.w(...)`, `.h(...)`), `Sizable` (`.xsmall()`, `.small()`), and has `.icon(...)`, `.ghost()`, `.primary()`, `.danger()`. `ThemeColor` has `list_hover`, `list_active`, `link_hover`, `popover`, `popover_foreground`, `background`.
- Do NOT run the app (`cargo run`) in implementation tasks — each launch triggers macOS Keychain prompts. The screenshot verification happens once, in the final task, driven by the session owner.

---

### Task 1: Shared style module `src/ui/style.rs`

**Files:**
- Create: `src/ui/style.rs`
- Modify: `src/ui/mod.rs` (add `pub mod style;`)

**Interfaces:**
- Produces: `osm_gpui::ui::style::{SIDE_PANEL_WIDTH, PANEL_ROW_HEIGHT, scrim_color, panel_row, interactive_row}` — used by Tasks 3–8.
  - `pub const SIDE_PANEL_WIDTH: f32` (= 280.0), `pub const PANEL_ROW_HEIGHT: f32` (= 24.0)
  - `pub fn scrim_color() -> gpui::Rgba` (= `rgba(0x00000099)`)
  - `pub fn panel_row(id: impl Into<ElementId>) -> Stateful<Div>` — layout-only row (no cursor/hover)
  - `pub fn interactive_row(id: impl Into<ElementId>, selected: bool, cx: &App) -> Stateful<Div>` — panel_row + `cursor_pointer` + hover/selected backgrounds

- [ ] **Step 1: Write the module**

```rust
//! Shared styling constants and row builders for the app's side panels.
//!
//! Conventions enforced here (see docs/superpowers/specs/2026-07-07-ui-consistency-design.md):
//! - Panel list rows are built with [`panel_row`] / [`interactive_row`] so
//!   height, padding, rounding, and hover/selected states match across
//!   sections.
//! - Row hover uses `theme().list_hover`; a persistently selected/active row
//!   uses `theme().list_active`. `accent` is not used for row backgrounds.
//! - Every clickable action is a `gpui_component::button::Button`
//!   (`.primary()` for a section's main action, `.ghost().xsmall()` for
//!   inline/per-row actions) — never a bare `div`/`Label` with
//!   `on_mouse_down`.

use gpui::{div, prelude::*, px, App, Div, ElementId, Rgba, Stateful};
use gpui_component::ActiveTheme as _;

/// Width of the right-hand side panel, shared with the map-size math in
/// `main.rs`.
pub const SIDE_PANEL_WIDTH: f32 = 280.0;

/// Fixed height of one list row in the side panel sections.
pub const PANEL_ROW_HEIGHT: f32 = 24.0;

/// The one semi-transparent black used behind every modal dialog.
pub fn scrim_color() -> Rgba {
    gpui::rgba(0x00000099)
}

/// Layout-only panel list row: fixed height, standard padding/rounding/text
/// size. Interaction states come from [`interactive_row`]; passive rows
/// (e.g. History entries) use this directly.
pub fn panel_row(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .h(px(PANEL_ROW_HEIGHT))
        .px_2()
        .gap_1()
        .rounded_md()
        .text_sm()
}

/// A clickable panel list row: [`panel_row`] plus pointer cursor and the
/// standard hover/selected backgrounds (`list_hover` / `list_active`).
pub fn interactive_row(id: impl Into<ElementId>, selected: bool, cx: &App) -> Stateful<Div> {
    let row = panel_row(id).cursor_pointer();
    if selected {
        row.bg(cx.theme().list_active)
    } else {
        let hover_bg = cx.theme().list_hover;
        row.hover(move |this| this.bg(hover_bg))
    }
}
```

Register in `src/ui/mod.rs`: add `pub mod style;` in alphabetical order (after `settings_window`).

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: compiles. (If `Stateful<Div>` import paths differ, fix imports per compiler guidance — `gpui::Stateful` and `gpui::Div` are the current names used elsewhere in this repo, e.g. `src/ui/modal.rs` imports `Div`.)

- [ ] **Step 3: Commit**

```bash
git add src/ui/style.rs src/ui/mod.rs
git commit -m "Add shared ui::style module with panel row builders and constants"
```

*(No unit test for this task: the module is pure element construction with no extractable logic; gpui elements aren't inspectable outside a window. The mapping-style pure functions with tests land in Task 2.)*

---

### Task 2: Mode panel icons (icon above label)

**Files:**
- Modify: `src/mode_panel.rs` (whole file is 81 lines; rewrite of `mode_button` + doc comment + new `mode_icon` fn + tests)

**Interfaces:**
- Consumes: nothing new.
- Produces: `fn mode_icon(mode: EditMode) -> IconName` and existing `fn mode_label(mode: EditMode) -> &'static str` (unchanged), both module-private, both unit-tested.

- [ ] **Step 1: Write the failing tests** (append at end of `src/mode_panel.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_distinct_icon() {
        let icons = [
            mode_icon(EditMode::Select),
            mode_icon(EditMode::Add),
            mode_icon(EditMode::Building),
            mode_icon(EditMode::Extrude),
        ];
        for (i, a) in icons.iter().enumerate() {
            for b in icons.iter().skip(i + 1) {
                assert_ne!(format!("{:?}", a), format!("{:?}", b));
            }
        }
    }

    #[test]
    fn mode_labels_are_stable() {
        assert_eq!(mode_label(EditMode::Select), "Select");
        assert_eq!(mode_label(EditMode::Add), "Add");
        assert_eq!(mode_label(EditMode::Building), "Bldg");
        assert_eq!(mode_label(EditMode::Extrude), "Extr");
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test --bin osm-gpui mode_panel`
Expected: FAIL — `mode_icon` not found.

- [ ] **Step 3: Implement**

Replace the imports, the stale doc comment, `mode_button`, and add `mode_icon`:

Imports become:
```rust
use gpui::{div, prelude::*, px, Context};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    v_flex, ActiveTheme, Disableable, Icon, IconName,
};
```

Doc comment on `render_mode_panel` — replace the final paragraph (lines starting "Uses text labels rather than icons:" through "…mismatched icon.") with:

```rust
    /// Each button shows an icon above a short text label; the label
    /// disambiguates the nearest-fit icons (the icon set has no exact
    /// cursor/extrude glyphs).
```

`mode_button` becomes (same signature; `.small()` and `.label(...)` are replaced by a sized button with a stacked icon+label child):

```rust
    fn mode_button(
        &self,
        id: &'static str,
        mode: EditMode,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = self.mode == mode;
        let action_mode = match mode {
            EditMode::Select => EditModeAction::Select,
            EditMode::Add => EditModeAction::Add,
            EditMode::Building => EditModeAction::Building,
            EditMode::Extrude => EditModeAction::Extrude,
        };
        let mut button = Button::new(id).w(px(46.0)).h(px(46.0)).child(
            v_flex()
                .items_center()
                .gap_0p5()
                .child(Icon::new(mode_icon(mode)).small())
                .child(div().text_xs().child(mode_label(mode))),
        );
        if is_active {
            button = button.primary();
        }
        if enabled {
            button = button.on_click(cx.listener(move |this, _, window, cx| {
                this.on_set_mode(&SetMode { mode: action_mode }, window, cx);
            }));
        } else {
            button = button.disabled(true);
        }
        button
    }
```

Add below `mode_label`:

```rust
fn mode_icon(mode: EditMode) -> IconName {
    match mode {
        EditMode::Select => IconName::Frame,
        EditMode::Add => IconName::Plus,
        EditMode::Building => IconName::Building2,
        EditMode::Extrude => IconName::LayoutDashboard,
    }
}
```

Note: keep `ActiveTheme` in imports only if still used by `render_mode_panel` (it is — `cx.theme().sidebar`/`border`). If the compiler flags the default `Size::Medium` height fighting the explicit `.h(px(46.0))`, the `Styled` refinement wins (applied after base styles in `Button::render`); if the button renders squashed in the final screenshot task, switch to `.compact()` plus explicit size.

- [ ] **Step 4: Run tests**

Run: `cargo test --bin osm-gpui mode_panel`
Expected: PASS (2 tests).

- [ ] **Step 5: Build + clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/mode_panel.rs
git commit -m "Mode panel: icon-above-label buttons with unit-tested icon mapping"
```

---

### Task 3: Right pane — Layers, Selection, History rows on shared row builders

**Files:**
- Modify: `src/side_panel.rs`

**Interfaces:**
- Consumes: `osm_gpui::ui::style::{panel_row, interactive_row, PANEL_ROW_HEIGHT, SIDE_PANEL_WIDTH}` (Task 1).
- Produces: nothing new for later tasks.

- [ ] **Step 1: Import the style module**

At the top of `src/side_panel.rs`, add:

```rust
use osm_gpui::ui::style::{interactive_row, panel_row, PANEL_ROW_HEIGHT, SIDE_PANEL_WIDTH};
```

- [ ] **Step 2: Panel width + selection row height**

- In `render_side_panel`, replace `.w(px(280.0))` with `.w(px(SIDE_PANEL_WIDTH))`.
- Delete the `const SELECTION_ROW_HEIGHT: f32 = 22.0;` associated const; in `render_selection_section`, replace both uses (`visible_rows as f32 * Self::SELECTION_ROW_HEIGHT` and `.h(px(Self::SELECTION_ROW_HEIGHT))`) with `PANEL_ROW_HEIGHT`.

- [ ] **Step 3: Selection rows via `interactive_row`**

In `render_selection_section`, the row construction

```rust
let mut row = div()
    .id(("selection-row", i))
    .flex_shrink_0()
    .h(px(Self::SELECTION_ROW_HEIGHT))
    .px_1()
    .flex()
    .items_center()
    .gap_1()
    .cursor_pointer()
    .text_sm()
    .text_color(cx.theme().foreground)
    .hover(|this| this.bg(cx.theme().accent));
```

becomes

```rust
let mut row = interactive_row(("selection-row", i), false, cx)
    .text_color(cx.theme().foreground);
```

(The svg-icon child, row text child, and `on_mouse_down` handler are unchanged.)

- [ ] **Step 4: Layer rows via `interactive_row`**

In `render_layers_section`, the row construction

```rust
div()
    .id(("layer-row", index))
    .flex()
    .flex_row()
    .items_center()
    .px_1()
    .rounded_md()
    .cursor_pointer()
    .when(is_active, |this| this.bg(cx.theme().accent))
```

becomes

```rust
interactive_row(("layer-row", index), is_active, cx)
```

(Checkbox child, `on_mouse_down`, and `.context_menu(...)` chain unchanged. `interactive_row` sets a fixed 24px height; the Checkbox row fits within it.)

- [ ] **Step 5: History rows via `panel_row`**

In `render_history_section`, replace

```rust
let mut row = div().px_1().py_0p5().text_sm().child(action.description());
if is_current {
    row = row.bg(cx.theme().accent);
} else if is_future {
    row = row.text_color(cx.theme().muted_foreground).italic();
}
row
```

with

```rust
let mut row = panel_row(("history-row", i)).child(action.description());
if is_current {
    row = row.bg(cx.theme().list_active);
} else if is_future {
    row = row.text_color(cx.theme().muted_foreground).italic();
}
row
```

(History rows are passive — `panel_row`, not `interactive_row`.)

- [ ] **Step 6: Build, clippy, test**

Run: `cargo clippy --all-targets -- -D warnings` then `cargo test`
Expected: clean; existing tests pass. Remove any now-unused imports (`px` may still be needed for `PANEL_ROW_HEIGHT` math and the svg size).

- [ ] **Step 7: Commit**

```bash
git add src/side_panel.rs
git commit -m "Side panel: Layers/Selection/History rows use shared style builders"
```

---

### Task 4: Right pane — Tags section rows and buttons

**Files:**
- Modify: `src/side_panel.rs` (`render_tags_section`)

**Interfaces:**
- Consumes: `panel_row` (Task 1), `IconName::{Close, Plus}`.

- [ ] **Step 1: Tag rows via `panel_row`**

In `render_tags_section`, the tag-row construction

```rust
div()
    .id(SharedString::from(format!("tag-row-{k}")))
    .flex()
    .flex_row()
    .items_center()
    .gap_2()
    .px_2()
    .py_1()
    .border_b_1()
    .border_color(cx.theme().border)
```

becomes

```rust
panel_row(SharedString::from(format!("tag-row-{k}")))
    .gap_2()
```

(The two flex_1 key/value cells with their double-click handlers are unchanged. The `border_b_1` separator is dropped — rows now match the other sections; `panel_row`'s `gap_1` is overridden back to `gap_2` to keep key/value spacing.)

The `(no tags)` placeholder row (`div().px_2().py_1().text_sm()...`) is left as-is except drop `.py_1()` → keep it simple: leave unchanged (it's a passive text line, not a row).

- [ ] **Step 2: Tag delete "x" becomes a ghost icon Button**

Replace the delete child:

```rust
.child(
    div()
        .id(SharedString::from(format!("tag-delete-{k}")))
        .cursor_pointer()
        .text_color(cx.theme().muted_foreground)
        .hover(|this| this.text_color(cx.theme().danger))
        .child("x")
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                this.delete_tag(&key_for_delete, cx);
            }),
        ),
)
```

with:

```rust
.child(
    Button::new(SharedString::from(format!("tag-delete-{k}")))
        .icon(IconName::Close)
        .ghost()
        .xsmall()
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.delete_tag(&key_for_delete, cx);
        })),
)
```

- [ ] **Step 3: "Add tag" button gets the Plus icon and small size**

```rust
Button::new("add-tag")
    .label("Add tag")
    .icon(IconName::Plus)
    .primary()
    .small()
    .on_click(/* unchanged handler */)
```

Also wrap it so it doesn't stretch full-width oddly: keep as-is if it already renders inline; `Button` is `flex_shrink_0` by default. Leave the existing `list.child(...)` structure.

- [ ] **Step 4: Build, clippy, test**

Run: `cargo clippy --all-targets -- -D warnings` then `cargo test`
Expected: clean. `Sizable` is already imported in side_panel.rs; `IconName` too.

- [ ] **Step 5: Commit**

```bash
git add src/side_panel.rs
git commit -m "Tags section: shared row style, ghost icon delete button, iconified Add tag"
```

---

### Task 5: Fields section — real Buttons for "Change feature type…" and "Add field"

**Files:**
- Modify: `src/fields_section.rs` (`render_fields_section`, ~lines 396–405 and 472–489)

**Interfaces:**
- Consumes: `Button`, `ButtonVariants`, `Sizable`, `IconName` from gpui_component (check the file's existing imports; add what's missing).

- [ ] **Step 1: "Change feature type…"**

Replace:

```rust
let change_type_button = gpui::div()
    .id("change-feature-type")
    .cursor_pointer()
    .child(Label::new("Change feature type…").text_xs())
    .on_mouse_down(
        gpui::MouseButton::Left,
        cx.listener(move |_this, _ev, window, cx| {
            window.dispatch_action(Box::new(crate::ChangeFeatureType), cx);
        }),
    );
```

with:

```rust
let change_type_button = Button::new("change-feature-type")
    .label("Change feature type…")
    .ghost()
    .xsmall()
    .on_click(cx.listener(move |_this, _ev, window, cx| {
        window.dispatch_action(Box::new(crate::ChangeFeatureType), cx);
    }));
```

`change_type_button` is used as a `.child(...)` in three branches; `Button` is `IntoElement`, so the usages compile unchanged. If a branch requires `AnyElement`, add `.into_any_element()` at the usages the compiler flags.

- [ ] **Step 2: "Add field" entries**

Replace:

```rust
gpui::div()
    .id(format!("field-add-more-{}", field_id))
    .cursor_pointer()
    .child(Label::new(format!("+ {}", f.label)).text_xs())
    .on_mouse_down(
        gpui::MouseButton::Left,
        cx.listener(move |this, _ev, _window, cx| {
            this.fields_promoted_more_fields.insert(field_id.clone());
            cx.notify();
        }),
    )
```

with:

```rust
Button::new(gpui::SharedString::from(format!("field-add-more-{}", field_id)))
    .label(f.label.clone())
    .icon(IconName::Plus)
    .ghost()
    .xsmall()
    .on_click(cx.listener(move |this, _ev, _window, cx| {
        this.fields_promoted_more_fields.insert(field_id.clone());
        cx.notify();
    }))
```

Wrap the buttons' column so they left-align instead of stretching: change the containing `gpui::div().flex().flex_col().gap_1()` to `gpui::div().flex().flex_col().gap_1().items_start()`.

- [ ] **Step 3: Imports**

Ensure `src/fields_section.rs` imports include:

```rust
use gpui_component::{
    button::{Button, ButtonVariants as _},
    IconName, Sizable,
};
```

(merge with whatever the file already imports from `gpui_component` — do not duplicate paths).

- [ ] **Step 4: Build, clippy, test**

Run: `cargo clippy --all-targets -- -D warnings` then `cargo test`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/fields_section.rs
git commit -m "Fields section: replace div-buttons with ghost Buttons"
```

---

### Task 6: main.rs drift — theme background, overlay tokens, shared width constant

**Files:**
- Modify: `src/main.rs` (root render ~2246–2272; debug overlay ~2478–2490; status overlay ~2505–2519; attribution ~2559–2580)

**Interfaces:**
- Consumes: `osm_gpui::ui::style::SIDE_PANEL_WIDTH` (Task 1); theme tokens `background`, `popover`, `popover_foreground`, `link_hover`.

- [ ] **Step 1: Width constant + root background**

Around line 2248: `let panel_width = px(280.0);` → `let panel_width = px(osm_gpui::ui::style::SIDE_PANEL_WIDTH);`

Around line 2269: `.bg(rgb(0x1a202c))` → `.bg(cx.theme().background)` (the render fn already has `cx`; `ActiveTheme` is already in scope in main.rs — verify with the compiler, adding `use gpui_component::ActiveTheme as _;` if not).

- [ ] **Step 2: Debug overlay (≈2479–2489) and status overlay (≈2508–2517)**

In both, replace `.bg(gpui::black())` → `.bg(cx.theme().popover)` and `.text_color(rgb(0xffffff))` → `.text_color(cx.theme().popover_foreground)`. Add `.border_1().border_color(cx.theme().border)` right after the `.bg(...)` in both overlays so they read like the app's popovers instead of floating unbordered. Keep `.opacity(0.9)`.

- [ ] **Step 3: Attribution overlay (≈2559–2580)**

Same bg/text substitution (`popover`/`popover_foreground`, keep `.opacity(0.75)`), and the link hover `.hover(|this| this.text_color(rgb(0xaad4ff)))` → `.hover(|this| this.text_color(cx.theme().link_hover))`. NOTE: this closure captures from the surrounding render — `cx.theme()` returns a value borrowed from `cx`; hoist first: `let link_hover = cx.theme().link_hover;` before the `credits.into_iter().enumerate().map(...)` and use `.hover(move |this| this.text_color(link_hover))`.

- [ ] **Step 4: Check for other `rgb(0xffffff)`/`gpui::black()` remnants**

Run: `grep -n '0x1a202c\|gpui::black()\|rgb(0xffffff)\|0xaad4ff\|px(280.0)' src/main.rs src/side_panel.rs`
Expected: no hits (canvas paint colors `0x3b82f6` are out of scope and remain).

- [ ] **Step 5: Build, clippy, test**

Run: `cargo clippy --all-targets -- -D warnings` then `cargo test`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "Use theme tokens for root background and map overlays, share panel width constant"
```

---

### Task 7: Scrim consolidation + dialog header weight

**Files:**
- Modify: `src/ui/modal.rs` (scrim bg + header weight), `src/ui/nsi_dialog.rs:197`, `src/ui/upload_dialog.rs:188`, `src/ui/preset_picker_dialog.rs:195`

**Interfaces:**
- Consumes: `crate::ui::style::scrim_color` (these files are inside the `osm_gpui` lib, so the path is `crate::ui::style::scrim_color`).

- [ ] **Step 1: modal.rs**

- `.bg(rgba(0x00000099))` → `.bg(crate::ui::style::scrim_color())`; update the doc comment on `scrim` that spells out the old pattern (replace `rgba(0x00000099)` in the comment with `scrim_color()`). Remove the now-unused `rgba` import if nothing else uses it.
- In `dialog_frame`, `.font_weight(gpui::FontWeight::BOLD)` → `.font_weight(gpui::FontWeight::SEMIBOLD)` (aligns dialog titles with the side panel's section headers).

- [ ] **Step 2: The three dialogs**

In `nsi_dialog.rs`, `upload_dialog.rs`, `preset_picker_dialog.rs`: replace `.bg(rgba(0x00000099))` with `.bg(crate::ui::style::scrim_color())`; drop unused `rgba` imports where the compiler flags them.

- [ ] **Step 3: Build, clippy, test**

Run: `cargo clippy --all-targets -- -D warnings` then `cargo test`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add src/ui/modal.rs src/ui/nsi_dialog.rs src/ui/upload_dialog.rs src/ui/preset_picker_dialog.rs
git commit -m "Share modal scrim color and align dialog header weight with section headers"
```

---

### Task 8: Screenshot session script + full verification

**Files:**
- Create: `docs/screenshots/ui-consistency.osmscript`
- Create (generated): `docs/screenshots/ui-consistency-01-panes.png`, `docs/screenshots/ui-consistency-02-selection.png`

**Interfaces:**
- Consumes: the `--script` harness ops (`window`, `viewport`, `load_osm`, `click`, `wait_idle`, `capture` — see `docs/screenshots/smoke.osmscript` and `src/script/parser.rs`).

- [ ] **Step 1: Write the script**

Check `docs/screenshots/fixtures/` and `docs/screenshots/select.osmscript` first for an existing fixture + known click coordinates that select a feature; mirror those. Template (adjust fixture path/coords to what `select.osmscript` actually uses):

```
# UI consistency regression captures: both panes, populated right pane.
window 1200 800
viewport 47.6062 -122.3321 12
wait_idle 10s
capture docs/screenshots/ui-consistency-01-panes.png

click 600,400
wait 250ms
capture docs/screenshots/ui-consistency-02-selection.png

log done
```

If `select.osmscript` loads a local fixture via `load_osm` to make the click deterministic, copy that approach verbatim.

- [ ] **Step 2: Full check suite**

Run: `cargo build --release` then `cargo clippy --all-targets -- -D warnings` then `cargo test`
Expected: all clean/pass.

- [ ] **Step 3: Run the screenshot session (ONE launch — coordinate with session owner; Keychain prompts twice)**

Run: `cargo run --release -- --script docs/screenshots/ui-consistency.osmscript --window-size 1200x800`
Expected: exits cleanly, writes both PNGs.

- [ ] **Step 4: Inspect the PNGs** (Read them as images)

Checklist: mode panel shows 4 icon-above-label buttons; active mode highlighted; right pane rows uniform height/rounding; tag rows have icon delete buttons; overlays use theme popover colors (not pure black); attribution readable.

- [ ] **Step 5: Commit**

```bash
git add docs/screenshots/ui-consistency.osmscript docs/screenshots/ui-consistency-01-panes.png docs/screenshots/ui-consistency-02-selection.png
git commit -m "Add UI consistency screenshot session and captures"
```
