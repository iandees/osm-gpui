# Custom Keyboard Shortcuts — Design

## Context

Keyboard shortcuts are currently hardcoded in two places in `src/main.rs`:

1. Seven actions registered via gpui's `Action` system and bound with `cx.bind_keys([...])` at window creation: `OpenOsmFile` (cmd-o), `DownloadFromOsm` (cmd-shift-d), `UploadToOsm` (cmd-shift-u), `Quit` (cmd-q), `OpenSettings` (cmd-,), `Undo` (cmd-z), `Redo` (cmd-shift-z). These also drive the shortcut labels shown in the native macOS menu (built in `src/menu.rs`).
2. Three unmodified single-letter mode-switch shortcuts (`a` = Add, `b` = Building, `x` = Extrude), matched literally against `ev.keystroke.key` inside the map canvas's `on_key_down` handler. These are deliberately *not* registered as gpui `KeyBinding`s because an unmodified letter binding would fire even while a text input elsewhere in the app (e.g. the tag-edit dialog) has focus; instead they only fire when the map canvas itself has focus.

This design adds a "Keyboard Shortcuts" page to the Settings window where users can view and override all ten of these shortcuts, with changes taking effect immediately.

**Out of scope:** Escape (cancel), Enter (confirm), and Delete/Backspace (delete selected features) stay hardcoded. They're core interaction primitives baked into canvas gesture handling rather than discrete commands, and Delete already has a hardcoded two-key fallback (Delete *and* Backspace both trigger it) that doesn't fit the one-action-one-shortcut model below.

## Storage

`AppSettings` (in `src/settings_store.rs`) gains a new field:

```rust
#[serde(default)]
pub keybindings: HashMap<String, String>,
```

Keyed by a stable action id (e.g. `"open_osm_file"`, `"mode_add"`), valued by a gpui keystroke spec string (e.g. `"cmd-o"`, `"a"`) as accepted by `KeyBinding::new` / produced by `Keystroke::unparse()`. This map is **sparse**: it holds only the actions the user has overridden. Actions absent from the map use their built-in default. This means a future release can change a default binding without silently freezing it for users who never touched settings.

## Default table

A new module, `src/keybindings.rs`, defines the single source of truth for all ten customizable shortcuts:

```rust
pub struct ShortcutDef {
    pub id: &'static str,
    pub default_spec: &'static str,
    pub label: &'static str,
    pub category: ShortcutCategory,
}

pub enum ShortcutCategory { General, File, Edit, Modes }

pub const SHORTCUTS: &[ShortcutDef] = &[
    ShortcutDef { id: "open_settings",  default_spec: "cmd-,",       label: "Settings…",         category: ShortcutCategory::General },
    ShortcutDef { id: "quit",           default_spec: "cmd-q",       label: "Quit",              category: ShortcutCategory::General },
    ShortcutDef { id: "open_osm_file",  default_spec: "cmd-o",       label: "Open…",             category: ShortcutCategory::File },
    ShortcutDef { id: "download_osm",   default_spec: "cmd-shift-d", label: "Download from OSM", category: ShortcutCategory::File },
    ShortcutDef { id: "upload_osm",     default_spec: "cmd-shift-u", label: "Upload to OSM…",    category: ShortcutCategory::File },
    ShortcutDef { id: "undo",           default_spec: "cmd-z",       label: "Undo",              category: ShortcutCategory::Edit },
    ShortcutDef { id: "redo",           default_spec: "cmd-shift-z", label: "Redo",              category: ShortcutCategory::Edit },
    ShortcutDef { id: "mode_add",       default_spec: "a",           label: "Switch to Add mode",      category: ShortcutCategory::Modes },
    ShortcutDef { id: "mode_building",  default_spec: "b",           label: "Switch to Building mode", category: ShortcutCategory::Modes },
    ShortcutDef { id: "mode_extrude",   default_spec: "x",           label: "Switch to Extrude mode",  category: ShortcutCategory::Modes },
];

/// Effective spec for `id`: the user's override if present, else the default.
pub fn effective_spec(settings: &AppSettings, id: &str) -> &str { ... }

/// All ten (id, effective_spec) pairs, in table order.
pub fn effective_bindings(settings: &AppSettings) -> Vec<(&'static str, String)> { ... }
```

The seven `ShortcutCategory::General | File | Edit` entries map to the existing gpui `Action` structs (`open_settings` → `OpenSettings`, etc.) via a small match in `main.rs`; the three `Modes` entries map to `EditModeAction` variants and are consumed directly by the canvas's `on_key_down` handler rather than through `KeyBinding`.

## Applying bindings

**At window creation** (`src/main.rs`, where `cx.bind_keys([...])` currently hardcodes the seven specs): build the list from `keybindings::effective_bindings(&settings_store::snapshot())` instead, mapping each of the seven action ids to its `KeyBinding::new(spec, ActionStruct, None)`. The three mode-letter entries are not passed to `bind_keys` (same reasoning as today — unmodified letters must stay scoped to the focused canvas).

**In the canvas `on_key_down` handler**: replace the hardcoded `"a"` / `"b"` / `"x"` literals with a lookup against `keybindings::effective_spec(&settings_store::snapshot(), "mode_add"|"mode_building"|"mode_extrude")`. Settings are re-read on every keypress (cheap in-memory snapshot, same pattern used elsewhere in the codebase, e.g. `auth::current_token`) rather than cached on `MapViewer`, so no new cross-window propagation channel is needed for this half.

**When a shortcut is changed or reset in Settings**: `SettingsWindow` (which runs in its own OS window but shares the same `App`) calls, in order:
1. `settings_store::update_store(...)` with the updated `keybindings` map.
2. `cx.clear_key_bindings()` then `cx.bind_keys([...])` rebuilt from the new `effective_bindings()` (App::bind_keys/clear_key_bindings are app-global, not per-window, and internally trigger `Effect::RefreshWindows`, so this is sufficient to update the live map window too — no explicit event needed).
3. `menu::rebuild_menus(...)` so the native menu's displayed shortcut labels refresh immediately.

Mode-letter changes need no explicit push — the canvas handler already re-reads settings on every keypress.

## Settings UI

A third page in `src/ui/settings_window.rs`'s `setting_pages()`, `SettingPage::new("Keyboard Shortcuts")`, alongside the existing "Account" and "Imagery Sources" pages. Grouped into four `SettingGroup`s matching `ShortcutCategory`: General, File, Edit, Modes.

Each shortcut is one `SettingItem` row showing:
- The action label.
- The current effective shortcut, rendered as a `Kbd`-style badge (gpui-component's `crates/ui/src/kbd.rs` display widget).
- A **Record** button.
- A **Reset** button, enabled only when that action currently has an override.

**Recording flow** — new `SettingsWindow` fields `recording: Option<&'static str>` (the action id being recorded) and `shortcut_error: Option<(&'static str, SharedString)>` (action id + message):
1. Click Record → `recording` is set; the row shows "Press keys…" and gets a focused, key-capturing div.
2. Its `on_key_down` handler ignores pure-modifier keystrokes (no `key` yet) and acts on the first real keystroke:
   - `escape` → cancel recording, clear `recording`, no change made.
   - Otherwise format via `Keystroke::unparse()` and validate:
     - Reserved keys (`escape`, `enter`, `return`, `delete`, `backspace`, case-insensitive) → set `shortcut_error`, stay in recording mode.
     - Conflicts with another action's *effective* binding (any of the other nine) → set `shortcut_error` to `"Already used by {label}"`, stay in recording mode.
     - Otherwise: insert the override into `AppSettings.keybindings`, persist + rebind + rebuild menus (per "Applying bindings" above), clear `recording` and `shortcut_error`.
3. **Reset** button: remove that action's id from `AppSettings.keybindings`, persist + rebind + rebuild menus.
4. A **"Reset All to Defaults"** button at the bottom of the page clears the entire `keybindings` map in one step.

## Testing

- `settings_store` round-trip test extended to cover a non-empty `keybindings` map (mirrors the existing `client_ids` coverage).
- `keybindings` module unit tests: `effective_spec` falls back to default when unset and returns the override when set; `effective_bindings` returns all ten ids in table order; no two default specs collide with each other (a static sanity check on `SHORTCUTS` itself).
- Existing osmscript-based UI tests (see `project_mode_selector_editing_modes` precedent) extended, if practical, to cover: recording a new shortcut for one action, confirming a conflicting shortcut is rejected, and confirming Reset restores the default — otherwise this is called out explicitly as unverified via osmscript and confirmed manually instead.
