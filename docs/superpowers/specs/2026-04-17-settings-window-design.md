# Settings Window Design

**Date:** 2026-04-17

## Overview

A settings window for osm-gpui, opened via menu bar (osm-gpui > Settings..., Cmd+,). Uses Zed's `ui` crate for polished, theme-aware components. Initial scope: managing custom imagery sources. Structured to support additional settings sections later.

## Dependencies

- `ui` crate from Zed (already added to Cargo.toml via git dependency)
- Transitive: `theme`, `component`, `icons`, `menu`, `ui_macros`, etc.
- Theme initialized with `theme::init(LoadThemes::JustBase, cx)` for fallback dark theme

## Opening the Settings Window

- Menu bar item: **osm-gpui > Settings...** with **Cmd+,** keyboard shortcut
- Opens as a separate gpui window (not a modal over the map)
- Action: `OpenSettings`

## Window Layout

Single-window settings panel. No sidebar navigation (only one section initially), but structured for future sections.

```
┌─ Settings ──────────────────────────────────┐
│                                             │
│  Custom Imagery Sources                     │
│  ─────────────────────────────────────────  │
│  ┌─ My Ortho Layer ──────────── [▼] [🗑] ─┐ │
│  │  URL: https://tiles.example/{z}/{x}/{y} │ │
│  │  Zoom: 0–19                             │ │
│  └─────────────────────────────────────────┘ │
│  ┌─ Sentinel-2 ──────────────── [▼] [🗑] ─┐ │
│  │  (collapsed — just the name row)        │ │
│  └─────────────────────────────────────────┘ │
│                                             │
│  [+ Add Source]                              │
│                                             │
└─────────────────────────────────────────────┘
```

## Custom Imagery Section

### Section Header

`ListHeader` with title "Custom Imagery Sources".

### Collapsed Entry Row

Each saved entry renders as a `ListItem`:
- **Label**: entry name
- **Disclosure chevron**: expand/collapse toggle
- **Delete button**: trash `IconButton`, visible on hover via `end_slot`

### Expanded Entry (Accordion)

Clicking the chevron or row expands to show editable fields below the row:
- **Name** — text input, pre-populated with current value
- **URL template** — text input, pre-populated
- **Min zoom / Max zoom** — text inputs side by side, pre-populated
- **Save / Cancel** buttons at the bottom of the expanded area

Constraints:
- Only one entry expanded at a time (expanding another collapses the current)
- Validation uses existing `validate()` function from `custom_imagery_dialog.rs` on save
- Validation errors displayed inline below the fields

### Empty State

When no entries exist: "No custom imagery sources configured." message with the Add Source button.

### Add Source

`Button` with plus icon at the bottom of the list. Clicking it appends a new blank entry in expanded/edit mode.

### Delete Confirmation

Clicking the trash icon replaces that row's content with an inline prompt: "Delete [name]?" with **Delete** (destructive style) and **Cancel** buttons.

## Persistence

- On save: updates `CUSTOM_IMAGERY_STORE` in-memory and writes to disk via `custom_imagery_store::save()`
- On cancel: reverts fields to stored values, collapses the row
- Closing the window discards any unsaved expanded edits
- The map window's imagery menu should reflect changes (existing dirty-flag mechanism)

## Components from `ui` Crate

| Component | Usage |
|-----------|-------|
| `Button` | Save, Cancel, Add Source, Delete confirmation |
| `IconButton` | Delete (trash), expand/collapse |
| `Label` | Entry names, field labels, empty state message |
| `ListItem` | Entry rows |
| `ListHeader` | "Custom Imagery Sources" section header |
| `Divider` | Section separator |
| `Icon` | Chevrons, trash, plus |

## Theme

- Initialize with `theme::init(LoadThemes::JustBase, cx)` during app startup
- All components use semantic colors via the `ui` crate's `Color` enum
- Fallback dark theme (Zed's built-in default)

## Files to Create/Modify

- **New**: `src/ui/settings_window.rs` — the settings window entity
- **Modify**: `src/main.rs` — add `OpenSettings` action, menu item, Cmd+, shortcut, theme init
- **Modify**: `src/ui/mod.rs` — export new module
- **Modify**: `Cargo.toml` — `ui` dependency (already added)
- **Reuse**: `src/custom_imagery_store.rs` — persistence (no changes)
- **Reuse**: `src/ui/custom_imagery_dialog.rs` — `validate()` function (no changes to module, may refactor validation out)
- **Reuse/replace**: `src/ui/text_input.rs` — continue using for text fields within the settings window

## Out of Scope

- Sidebar navigation (single section for now)
- Other settings sections (map defaults, cache, UI preferences)
- Drag-to-reorder entries
- Import/export of custom imagery configs
