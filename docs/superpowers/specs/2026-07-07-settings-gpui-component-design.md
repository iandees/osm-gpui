# Settings Window: Migrate to gpui-component `Settings`

**Date:** 2026-07-07

## Overview

Replace the hand-built `v_flex` layout in `src/ui/settings_window.rs` with gpui-component's
`Settings` widget (`gpui_component::setting::{Settings, SettingPage, SettingGroup, SettingItem,
SettingField}`), pinned at the version already in `Cargo.lock` (0.5.2,
`b7e63cc29018c27bb24788ddb212f4cbdd861334`). This gives the settings window built-in sidebar page
navigation, search/filter, and consistent group/item styling, replacing the single-column layout
and the `RadioGroup`-based server picker.

This supersedes `docs/superpowers/specs/2026-04-17-settings-window-design.md`, which predates both
the OAuth/API-server sections and the migration to `gpui-component`.

## Reference API (from the pinned gpui-component source)

```
Settings                         <- top-level widget, RenderOnce, owns its own sidebar/search state
  SettingPage (title, icon, resettable, default_open)
    SettingGroup (optional title, GroupBoxVariant)
      SettingItem (title, description, disabled, keywords, layout: Axis)
        SettingField::switch/checkbox/dropdown/input/number_input/element/render
```

- `Settings::new(id).with_size(Size).pages(Vec<SettingPage>)` — pages are rebuilt on every render
  call, same as any other `Render` impl; conditionally including/excluding items or pages in the
  `Vec` is the normal way to express dynamic visibility (see the story's `resettable`/`disabled`
  fields).
- `SettingField::render(|options, window, cx| -> impl IntoElement)` and
  `SettingItem::render(|options, window, cx| -> AnyElement)` allow fully custom widgets when no
  built-in field type fits (used in the reference story for buttons, multi-button "density"
  picker, and free-form About content).
- `SettingField::element(impl SettingFieldElement)` allows a reusable custom field type
  implementing `SettingFieldElement::render_field`.
- The widget owns its own sidebar (page + group list, with per-group search-filter keywords) and a
  search box, via `window.use_keyed_state`, so no window-level layout code is needed beyond
  supplying `Settings::new(...).pages(...)`.

## Opening the window

Unchanged: `OpenSettings` action (`src/main.rs`), `cmd-,` keybinding, `Settings…` menu item
(`src/menu.rs`), `SETTINGS_WINDOW_OPEN` atomic guard, `Root::new(SettingsWindow::new(...))` in its
own `WindowOptions`-configured OS window (`src/menu.rs::open_settings`).

Window bounds grow from 600×500 to roughly 850×600 to accommodate the widget's default ~250px
sidebar without cramping the content pane.

## Pages

### Page 1 — "Account" (icon: `IconName::User` or closest match)

**Group "API Server"**
- `SettingItem` "Server" → `SettingField::dropdown` with options Primary
  (`api.openstreetmap.org`), Dev/testing, Custom — replaces the current `RadioGroup`.
- When the selected server is Custom, a second `SettingItem` "Custom API URL" is included in the
  group's `items` Vec (simply omitted when not Custom — no visibility API needed since pages
  rebuild every render). Its field is a custom `SettingField::render` combining the existing
  `Input`/`InputState` with a "Save" button and inline validation error, preserving today's
  explicit-save-with-validation behavior (`save_custom_api_url`) rather than saving on every
  keystroke.

**Group "OpenStreetMap Account"**
- One `SettingItem::render` custom item reproducing the existing `LoginState` state machine
  (`LoggedOut`/`LoggingIn`/`LoggedIn(StoredToken)`/`Error`) with Sign in/Sign out buttons. Logic in
  `start_login`/`logout` is unchanged; only the surrounding chrome moves from a hand-built section
  into one custom-rendered setting item.

### Page 2 — "Imagery Sources" (icon: `IconName::Image` or closest match)

**Group "Custom Imagery Sources"**
- Each `CustomImageryEntry` becomes its own `SettingItem`:
  - title = source name
  - description = URL template + zoom range summary (e.g. `https://tiles.example/{z}/{x}/{y} ·
    zoom 0–19`)
  - field = Edit/Delete buttons via `SettingField::render`
- Clicking Edit swaps that item's field into an inline form (Input fields for name/URL/min/max
  zoom + Save/Cancel), driven by an `editing_ix: Option<usize>` field on `SettingsWindow` (replaces
  the current per-entry `expanded` bool) — only one entry editable at a time, same as today.
- Delete confirmation stays as today's inline confirm state, now scoped per item via the field
  closure rather than a manual row.
- A trailing `SettingItem::render` "Add Source" row is appended after the entry items in the same
  group.

## State

`SettingsWindow` remains an `Entity<SettingsWindow>` (not converted to globals). Its `Render` impl
becomes:

```rust
Settings::new("app-settings")
    .with_size(Size::Medium)
    .pages(self.setting_pages(window, cx))
```

`setting_pages(&self, window, cx) -> Vec<SettingPage>` builds the two pages from `self`'s existing
fields (API server choice, custom URL input state + validation error, login state, imagery entries,
`editing_ix`, delete-confirm state), following the reference story's pattern of capturing
`let view = cx.entity();` and reading/writing via `view.read(cx)` / `view.update(cx, ...)` inside
field closures.

Persistence is unchanged: `settings_store::update_store` and `custom_imagery_store::update_store`
remain the on-save/on-change write paths.

## Dependencies

No `Cargo.toml` change — `gpui-component` is already a dependency; this only adds imports of its
`setting` module. The `radio::RadioGroup` import in `src/ui/settings_window.rs` is dropped since
the server picker becomes a dropdown.

## Files to modify

- `src/ui/settings_window.rs` — replace layout/render code as described; state field changes
  (`editing_ix` replacing per-entry `expanded`)
- `src/menu.rs` — window bounds (850×600)
- No changes to `src/custom_imagery_store.rs`, `src/settings_store.rs`, OAuth/login logic, or the
  `OpenSettings` action wiring

## Out of scope

- "Keyboard Shortcuts" and "Caches" pages — noted as likely future `SettingPage` additions but not
  part of this change; the page-list structure supports appending them later with no rework.
- Changing the underlying custom-imagery validation logic or OAuth flow.
- Search/filter behavior tuning beyond what `Settings` provides out of the box.
