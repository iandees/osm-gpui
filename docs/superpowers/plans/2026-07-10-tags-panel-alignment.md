# Tags Panel Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the Tags panel's key/value alignment — currently each tag row independently flexes its key and value columns to 50/50, so keys and values don't line up across rows, there's no divider between rows, and long text overflows the panel instead of wrapping.

**Architecture:** Replace the hand-rolled `div().flex().flex_col()` loop in `MapViewer::render_tags_section` (`src/side_panel.rs`) with `gpui_component::description_list::DescriptionList` — a library widget already vendored via the `gpui-component` git dependency that provides a shared fixed-width label column, row dividers, and text wrapping out of the box. The existing double-click-to-edit and per-row delete-button behavior is preserved by placing the same interactive elements inside `DescriptionItem`'s `AnyElement` label/value slots.

**Tech Stack:** Rust, GPUI, gpui-component (`description_list` module).

## Global Constraints

- Run all 4 CI steps locally before considering this done: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo build`, `cargo test`. Missing any one has caused a CI failure before.
- Do not modify `src/ui/style.rs`'s `panel_row`/`interactive_row` — other panel sections (Layers, Selection, History) keep their current fixed-height row style; this change is scoped to the Tags section only.
- Preserve existing behavior exactly: double-click on key or value opens the tag-edit dialog with that field pre-selected; clicking the trailing "x" deletes the tag immediately; the "Add tag" button below the list is unchanged.
- GUI changes must be verified visually via the osmscript capture harness (per project convention) — cargo test alone doesn't exercise rendered layout.

---

### Task 1: Rewrite the Tags section to use `DescriptionList`

**Files:**
- Modify: `src/side_panel.rs:1-18` (imports), `src/side_panel.rs:360-504` (`render_tags_section`)

**Interfaces:**
- Consumes: `osm_gpui::selection::{aggregate_tags, TagValue}` (unchanged), `osm_gpui::ui::tag_edit_dialog::TagEditField` (unchanged), `crate::PendingTagEditOpen` (unchanged struct, unchanged fields: `features`, `original_key`, `original_value`, `select`, `is_add`), `self.selected`, `self.layer_manager`, `self.pending_tag_edit_open` (unchanged field on `MapViewer`), `self.delete_tag(&str, &mut Context<Self>)` (unchanged method signature).
- Produces: `render_tags_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement` — same signature as before, called from `src/side_panel.rs:50`. No other file depends on the internals of this function.

- [ ] **Step 1: Add the `description_list` import**

In `src/side_panel.rs`, change the `gpui_component` use block (currently lines 4-10):

```rust
use gpui_component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    description_list::{DescriptionItem, DescriptionList},
    label::Label,
    menu::ContextMenuExt,
    ActiveTheme, Icon, IconName, Sizable,
};
```

- [ ] **Step 2: Rewrite `render_tags_section`**

Replace the entire function body from `let mut list = div().flex().flex_col();` (line 383) through the closing `.into_any_element()` (line 503) — i.e. everything after the `aggregated`/`selection` setup and before the function's closing `}` — with:

```rust
    fn render_tags_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui_component::Size;
        use osm_gpui::ui::tag_edit_dialog::TagEditField;

        if self.selected.is_empty() {
            return Label::new("No selection.")
                .text_color(cx.theme().muted_foreground)
                .into_any_element();
        }

        let per_feature: Vec<Vec<(String, String)>> = self
            .selected
            .iter()
            .filter_map(|sel| {
                self.layer_manager
                    .find_layer(sel.layer_id)
                    .and_then(|layer| layer.as_editable())
                    .and_then(|editable| editable.feature_tags(sel))
            })
            .collect();

        let aggregated = osm_gpui::selection::aggregate_tags(&per_feature);
        let selection = self.selected.clone();

        let mut list = div().flex().flex_col().gap_2();

        if aggregated.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(cx.theme().muted_foreground)
                    .child("(no tags)"),
            );
        } else {
            let mut description_list = DescriptionList::new()
                .columns(1)
                .label_width(px(90.0))
                .bordered(true)
                .with_size(Size::Small);

            for (k, v) in aggregated.into_iter() {
                let value_text = match v {
                    osm_gpui::selection::TagValue::Single(s) => s,
                    osm_gpui::selection::TagValue::Multiple(n) => format!("<{} values>", n),
                };

                let key_for_key_click = k.clone();
                let value_for_key_click = value_text.clone();
                let selection_for_key_click = selection.clone();

                let key_for_value_click = k.clone();
                let value_for_value_click = value_text.clone();
                let selection_for_value_click = selection.clone();

                let key_for_delete = k.clone();

                let key_element = div()
                    .id(SharedString::from(format!("tag-key-{k}")))
                    .cursor_pointer()
                    .child(k.clone())
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                            if ev.click_count == 2 {
                                this.pending_tag_edit_open = Some(PendingTagEditOpen {
                                    features: selection_for_key_click.clone(),
                                    original_key: key_for_key_click.clone(),
                                    original_value: value_for_key_click.clone(),
                                    select: TagEditField::Key,
                                    is_add: false,
                                });
                                cx.notify();
                            }
                        }),
                    )
                    .into_any_element();

                let value_element = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .w_full()
                    .child(
                        div()
                            .id(SharedString::from(format!("tag-value-{k}")))
                            .flex_1()
                            .cursor_pointer()
                            .child(value_text.clone())
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                                    if ev.click_count == 2 {
                                        this.pending_tag_edit_open = Some(PendingTagEditOpen {
                                            features: selection_for_value_click.clone(),
                                            original_key: key_for_value_click.clone(),
                                            original_value: value_for_value_click.clone(),
                                            select: TagEditField::Value,
                                            is_add: false,
                                        });
                                        cx.notify();
                                    }
                                }),
                            ),
                    )
                    .child(
                        // Spec deviation: the design spec calls for a danger
                        // hover treatment on this delete icon, but
                        // gpui-component's Button doesn't support it cleanly
                        // in combination with `.ghost()`. `ButtonVariant::Custom`
                        // (via `ButtonCustomVariant`) fixes the foreground color
                        // across all states rather than only on hover, and its
                        // `.hover()` background color is actually unused by
                        // `ButtonVariant::hovered()` for the non-outline case
                        // (it re-derives the hover background from `color`
                        // instead). Layering a second `.hover(...)` directly on
                        // the `Button` via `Styled`/`InteractiveElement` would
                        // also conflict with the `.hover()` call `Button::render`
                        // already makes internally on the same `Interactivity`,
                        // which panics in debug builds (`hover style already
                        // set`). Keeping `.ghost()` until upstream exposes a
                        // composable per-state foreground/hover API.
                        Button::new(SharedString::from(format!("tag-delete-{k}")))
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                this.delete_tag(&key_for_delete, cx);
                            })),
                    )
                    .into_any_element();

                description_list = description_list
                    .child(DescriptionItem::new(key_element).value(value_element).span(1));
            }

            list = list.child(description_list);
        }

        let add_selection = selection.clone();
        list.child(
            Button::new("add-tag")
                .label("Add tag")
                .icon(IconName::Plus)
                .primary()
                .small()
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.pending_tag_edit_open = Some(PendingTagEditOpen {
                        features: add_selection.clone(),
                        original_key: String::new(),
                        original_value: String::new(),
                        select: TagEditField::None,
                        is_add: true,
                    });
                    cx.notify();
                })),
        )
        .into_any_element()
    }
```

Note: `DescriptionItem::new`/`.value` accept anything implementing `Into<DescriptionText>`, and `AnyElement` has a `From<AnyElement> for DescriptionText` impl, so passing the `AnyElement` results directly works without an explicit `.into()`.

- [ ] **Step 3: Build and fix any type errors**

Run: `cargo build`
Expected: compiles with no errors. If `DescriptionItem::new`/`.value` don't accept `AnyElement` directly due to type inference, wrap with `.into()` explicitly, e.g. `DescriptionItem::new(key_element).value(value_element)`.

- [ ] **Step 4: Run the existing test suite**

Run: `cargo test`
Expected: PASS — `preset_label_tests::cafe_node_resolves_to_cafe_label` and all other existing tests are unaffected since none exercise `render_tags_section`'s rendered tree directly.

- [ ] **Step 5: Run formatting and lint checks**

Run: `cargo fmt`
Run: `cargo clippy -- -D warnings`
Expected: `cargo fmt` makes no further changes after being applied once; `cargo clippy` reports no warnings.

- [ ] **Step 6: Commit**

```bash
git add src/side_panel.rs
git commit -m "Align Tags panel key/value columns using DescriptionList"
```

---

### Task 2: Add a visual verification fixture and confirm the fix renders correctly

**Files:**
- Create: `docs/screenshots/fixtures/tags_panel.osm`
- Create: `docs/screenshots/tags_panel.osmscript`

**Interfaces:**
- Consumes: the app's existing `--script <path>.osmscript` CLI flag and `capture <path>.png` script command (see `docs/screenshots/select.osmscript` for the established pattern: `window`, `load_osm`, `viewport`, `wait_idle`, `click`, `capture`).
- Produces: nothing consumed by later tasks — this is the terminal verification step.

- [ ] **Step 1: Create a fixture with a busy, mixed-length tag set**

Write `docs/screenshots/fixtures/tags_panel.osm`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6" generator="handwritten">
  <bounds minlat="40.7080" minlon="-74.0100" maxlat="40.7160" maxlon="-74.0000"/>

  <!-- A tagged POI node with short, long, and very long key/value pairs,
       to exercise the Tags panel's column alignment and text wrapping. -->
  <node id="2001" lat="40.7120" lon="-74.0060">
    <tag k="shop" v="bakery"/>
    <tag k="name" v="Hot Hands Pie &amp; Biscuit"/>
    <tag k="addr:housenumber" v="272"/>
    <tag k="addr:street" v="Snelling Avenue South"/>
    <tag k="addr:city" v="Saint Paul"/>
    <tag k="addr:state" v="MN"/>
    <tag k="addr:postcode" v="55105"/>
    <tag k="check_date:opening_hours" v="2026-03-14"/>
    <tag k="opening_hours" v="Tu-Sa 08:00-15:00; Su 08:00-13:00"/>
    <tag k="phone" v="+1-651-300-1503"/>
    <tag k="website" v="https://www.hothandspie.com"/>
  </node>
</osm>
```

- [ ] **Step 2: Create the verification script**

Write `docs/screenshots/tags_panel.osmscript`:

```
# Load a busy-tagged POI and screenshot the Tags panel to verify column
# alignment: fixed-width key column, row dividers, wrapped long text.
#
# Coordinate model: `click X Y` uses raw window coords. MapViewer subtracts
# 48px (header) from y before hit-testing. With window 1200x800 and the
# 280px right panel, macOS renders the window slightly taller than requested
# (actual content height ~828px), so the map area is 920x780 and the
# viewport center projects to map-area (460, 390) = window (460, 438).

window 1200 800
load_osm docs/screenshots/fixtures/tags_panel.osm
viewport 40.7120 -74.0060 18
wait_idle 5s

# Click the tagged POI node to select it and populate the Tags panel.
click 460,438
wait_idle 2s
capture out/tags-panel.png
```

- [ ] **Step 3: Run the harness and capture the screenshot**

Run: `cargo run -- --script docs/screenshots/tags_panel.osmscript`
Expected: exits cleanly, `out/tags-panel.png` is written.

- [ ] **Step 4: Inspect the screenshot**

Read `out/tags-panel.png` and confirm:
- All keys start at the same x-position and all values start at the same x-position (aligned columns), regardless of key length.
- A visible divider line separates each tag row.
- The long `check_date:opening_hours` key and the long `opening_hours` value wrap onto additional lines instead of being cut off at the panel edge.
- The delete "x" button is still present and right-aligned on each row.
- Double-click-to-edit and delete are unchanged in behavior (already covered by existing app behavior — this step is a visual check only, not a re-test of click handling).

If anything looks wrong, fix `render_tags_section` (Task 1) and re-run this step. Do not proceed to Step 5 until the screenshot looks correct.

- [ ] **Step 5: Commit the fixture and script**

```bash
git add docs/screenshots/fixtures/tags_panel.osm docs/screenshots/tags_panel.osmscript
git commit -m "Add Tags panel alignment verification fixture and script"
```

---

## Self-Review Notes

- **Spec coverage:** fixed-width key column (label_width) ✓, row dividers (bordered(true)) ✓, wrapping of long keys and values (DescriptionList's unconstrained-height cells) ✓, delete button preserved ✓, double-click-to-edit preserved ✓, "Add tag" button unchanged ✓, no changes to other panel sections ✓.
- **No placeholders:** both tasks contain complete, runnable code and exact commands.
- **Type consistency:** `render_tags_section` keeps its existing signature (`&self, cx: &mut Context<Self>) -> gpui::AnyElement`) and existing collaborators (`PendingTagEditOpen`, `TagEditField`, `delete_tag`) unchanged, so no other call site needs updating.
