# App-wide text size setting

## Problem

Text sizing is set ad hoc, call-by-call, across the app (`.text_sm()` / `.text_xs()`
scattered in `side_panel.rs`, `fields_section.rs`, `mode_panel.rs`, `main.rs`, and
several dialogs). There's no single place that defines "the app's default text
size," and no way for a user to change it.

## Design

- `AppSettings` (in `settings_store.rs`) gains a `text_size_preset: TextSizePreset`
  field (`Small` / `Medium` / `Large`, default `Medium`), persisted like the
  existing settings fields (`#[serde(default)]` for forward compatibility with
  existing `settings.json` files).
- A `TextScale { body: Pixels, muted: Pixels }` mapping:
  | Preset | body | muted |
  |--------|------|-------|
  | Small  | 12px | 10px  |
  | Medium | 14px | 12px  |
  | Large  | 16px | 14px  |

  Medium matches the app's current look (most panels already use `.text_sm()`/
  `.text_xs()`, i.e. 14px/12px).
- `TextScale` is registered as a gpui global, initialized from
  `settings_store::snapshot()` at startup and updated (via `cx.set_global`)
  whenever the setting changes, so open windows re-render at the new size
  without a restart.
- The main window's root render container and the Settings window's root
  render container each call `.text_size(scale.body)` once. Because gpui's
  `TextStyleRefinement` cascades down the element tree, every descendant that
  doesn't set its own explicit size inherits this as the new default — no need
  to touch every leaf.
- Existing `.text_sm()` calls used purely to get "normal" body text are
  removed (they now inherit the cascaded default).
- Existing `.text_xs()` calls used for secondary/muted text (field labels,
  dialog captions, mode labels) are replaced with a `text_muted_size(cx)`
  helper that reads `TextScale.muted` from the global, so muted text scales
  proportionally with the body size.
- New "Appearance" `SettingGroup` in `settings_window.rs`, one dropdown
  (`SettingField::dropdown`) with Small/Medium/Large, wired to
  `settings_store::update_store(...)` + `cx.set_global(...)`.

## Testing

- Unit test: preset → pixel mapping, and settings round-trip / serde-default
  behavior when `text_size_preset` is absent from an old `settings.json`.
- Manual: osmscript harness screenshots of the app at each of the 3 presets to
  confirm visible size change and no layout breakage.

## Out of scope

- Per-panel or per-element size overrides beyond the existing muted/body
  distinction.
- A custom/free-form numeric size (presets only, per user decision).
