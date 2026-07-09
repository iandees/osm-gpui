# Custom Keyboard Shortcuts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users view and override the app's ten customizable keyboard shortcuts (7 cmd-modified actions + 3 mode-switch letters) from a new "Keyboard Shortcuts" page in the Settings window, with changes applied immediately.

**Architecture:** A sparse `HashMap<String, String>` override map lives in `AppSettings` (lib crate). A new `osm_gpui::keybindings` module (lib crate) holds the canonical default table and pure resolution/validation helpers, usable by both the binary (to build real `gpui::KeyBinding`s, since the `Action` structs are defined there) and the lib crate's `SettingsWindow` (to render the UI). Because `SettingsWindow` lives in the lib crate and can't reference the binary's `Action` types, it doesn't call `cx.bind_keys` directly — it persists the change and emits a `SettingsEvent::KeybindingsChanged` event; `menu.rs::open_settings` (in the binary, which already owns window/menu setup) subscribes to that event and does the actual rebind + native-menu refresh.

**Tech Stack:** Rust, gpui (`zed-industries/zed` git dep), gpui-component (`longbridge/gpui-component` git dep, `Settings`/`SettingPage`/`SettingItem`/`Kbd` widgets).

## Global Constraints

- Run `cargo fmt --check` before every commit (CI enforces formatting).
- Escape, Enter, and Delete/Backspace are NOT part of this feature — they stay hardcoded (see spec's "Out of scope").
- Ten customizable shortcuts total, ids exactly: `open_settings`, `quit`, `open_osm_file`, `download_osm`, `upload_osm`, `undo`, `redo`, `mode_add`, `mode_building`, `mode_extrude`.
- Spec doc: `docs/superpowers/specs/2026-07-08-custom-keyboard-shortcuts-design.md` — consult it for anything this plan doesn't spell out.

---

### Task 1: `keybindings` override storage in `AppSettings`

**Files:**
- Modify: `src/settings_store.rs`

**Interfaces:**
- Produces: `AppSettings.keybindings: HashMap<String, String>` (public field, `#[serde(default)]`, empty by default).

- [ ] **Step 1: Add the field**

In `src/settings_store.rs`, add to the `AppSettings` struct (after `client_ids`):

```rust
    /// User overrides for customizable keyboard shortcuts, keyed by the
    /// shortcut id (see `crate::keybindings::SHORTCUTS`), valued by a gpui
    /// keystroke spec (e.g. `"cmd-o"`). Sparse: only overridden shortcuts
    /// appear here — everything else uses its built-in default, so a future
    /// default change isn't silently frozen for users who never touched
    /// this setting.
    #[serde(default)]
    pub keybindings: HashMap<String, String>,
```

Update `impl Default for AppSettings` to add `keybindings: HashMap::new(),`.

- [ ] **Step 2: Fix the three existing test struct literals**

In the `#[cfg(test)] mod tests` block, three `AppSettings { ... }` literals (in `round_trip` and twice in `base_url_matches_choice`) construct the struct without the new field. Add `keybindings: HashMap::new(),` to each.

- [ ] **Step 3: Add a round-trip test covering `keybindings`**

Add this test to the `tests` module:

```rust
    #[test]
    fn round_trip_with_keybindings() {
        let dir = tmp_dir("round-trip-keybindings");
        let path = dir.join("settings.json");
        let mut keybindings = HashMap::new();
        keybindings.insert("undo".to_string(), "cmd-alt-z".to_string());
        let settings = AppSettings {
            api_server: ApiServerChoice::Primary,
            custom_api_url: String::new(),
            client_ids: HashMap::new(),
            keybindings,
        };
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, settings);
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib settings_store -- --nocapture`
Expected: all `settings_store::tests::*` tests pass, including the new `round_trip_with_keybindings`.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
git add src/settings_store.rs
git commit -m "Add keybindings override map to AppSettings"
```

---

### Task 2: `keybindings` module — default table and resolution helpers

**Files:**
- Create: `src/keybindings.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `crate::settings_store::AppSettings` (from Task 1).
- Produces:
  - `pub enum ShortcutCategory { General, File, Edit, Modes }` (derives `Clone, Copy, Debug, PartialEq, Eq`)
  - `pub struct ShortcutDef { pub id: &'static str, pub default_spec: &'static str, pub label: &'static str, pub category: ShortcutCategory }` (derives `Clone, Copy, Debug`)
  - `pub const SHORTCUTS: &[ShortcutDef]` — all 10 entries, in display order.
  - `pub fn def(id: &str) -> &'static ShortcutDef`
  - `pub fn effective_spec(settings: &AppSettings, id: &str) -> String`
  - `pub fn effective_bindings(settings: &AppSettings) -> Vec<(&'static str, String)>`
  - `pub fn is_reserved(spec: &str) -> bool`
  - `pub fn is_bare_key(spec: &str) -> bool` — true if `spec` has no modifier prefix (required for `Modes`-category shortcuts, which are matched as unmodified literal keys — see `src/main.rs`'s `on_key_down`).
  - `pub fn conflict(settings: &AppSettings, id: &str, spec: &str) -> Option<&'static str>` — `Some(other_label)` if `spec` collides with a different shortcut's effective binding.

- [ ] **Step 1: Write the module with its unit tests**

Create `src/keybindings.rs`:

```rust
//! Canonical table of user-customizable keyboard shortcuts, and helpers to
//! resolve a shortcut's effective (possibly user-overridden) key spec.
//!
//! The ten entries here split into two groups, both driven by the same
//! `AppSettings.keybindings` override map, but consumed differently:
//! - The seven non-`Modes` entries correspond to gpui `Action` structs
//!   defined in the binary crate (`src/main.rs`) and are turned into real
//!   `gpui::KeyBinding`s there (this module can't reference those `Action`
//!   types — they live in a different crate).
//! - The three `Modes` entries are unmodified single letters, matched
//!   directly against `KeyDownEvent.keystroke.key` in the map canvas's
//!   `on_key_down` handler (`src/main.rs`), never registered as
//!   `KeyBinding`s (an unmodified-letter `KeyBinding` would fire even while
//!   a text input elsewhere in the app has focus).

use crate::settings_store::AppSettings;

/// Grouping used to organize the Settings > Keyboard Shortcuts page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutCategory {
    General,
    File,
    Edit,
    Modes,
}

/// One customizable shortcut: a stable id (used as the
/// `AppSettings.keybindings` key), its built-in default keystroke spec, a
/// human-readable label, and which category it's grouped under in the
/// Settings UI.
#[derive(Clone, Copy, Debug)]
pub struct ShortcutDef {
    pub id: &'static str,
    pub default_spec: &'static str,
    pub label: &'static str,
    pub category: ShortcutCategory,
}

/// The full set of customizable shortcuts, in the order they should be
/// displayed within their category.
pub const SHORTCUTS: &[ShortcutDef] = &[
    ShortcutDef {
        id: "open_settings",
        default_spec: "cmd-,",
        label: "Settings…",
        category: ShortcutCategory::General,
    },
    ShortcutDef {
        id: "quit",
        default_spec: "cmd-q",
        label: "Quit",
        category: ShortcutCategory::General,
    },
    ShortcutDef {
        id: "open_osm_file",
        default_spec: "cmd-o",
        label: "Open…",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "download_osm",
        default_spec: "cmd-shift-d",
        label: "Download from OSM",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "upload_osm",
        default_spec: "cmd-shift-u",
        label: "Upload to OSM…",
        category: ShortcutCategory::File,
    },
    ShortcutDef {
        id: "undo",
        default_spec: "cmd-z",
        label: "Undo",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "redo",
        default_spec: "cmd-shift-z",
        label: "Redo",
        category: ShortcutCategory::Edit,
    },
    ShortcutDef {
        id: "mode_add",
        default_spec: "a",
        label: "Switch to Add mode",
        category: ShortcutCategory::Modes,
    },
    ShortcutDef {
        id: "mode_building",
        default_spec: "b",
        label: "Switch to Building mode",
        category: ShortcutCategory::Modes,
    },
    ShortcutDef {
        id: "mode_extrude",
        default_spec: "x",
        label: "Switch to Extrude mode",
        category: ShortcutCategory::Modes,
    },
];

/// Look up a shortcut's definition by id. Panics if `id` isn't in
/// `SHORTCUTS` — a programmer error, since callers only ever pass ids
/// sourced from `SHORTCUTS` itself.
pub fn def(id: &str) -> &'static ShortcutDef {
    SHORTCUTS
        .iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("unknown shortcut id: {id}"))
}

/// The effective keystroke spec for `id`: the user's override if present in
/// `settings.keybindings`, else the built-in default.
pub fn effective_spec(settings: &AppSettings, id: &str) -> String {
    settings
        .keybindings
        .get(id)
        .cloned()
        .unwrap_or_else(|| def(id).default_spec.to_string())
}

/// All shortcuts' effective `(id, spec)` pairs, in `SHORTCUTS` order.
pub fn effective_bindings(settings: &AppSettings) -> Vec<(&'static str, String)> {
    SHORTCUTS
        .iter()
        .map(|d| (d.id, effective_spec(settings, d.id)))
        .collect()
}

/// Keys that can never be assigned to a customizable shortcut, because
/// they're hardcoded interaction primitives elsewhere (cancel, confirm,
/// delete-selection) — see the design doc's "Out of scope" section.
const RESERVED_KEYS: &[&str] = &["escape", "enter", "return", "delete", "backspace"];

/// True if `spec`'s key portion (ignoring any modifier prefix) is reserved
/// and can't be assigned to a customizable shortcut.
pub fn is_reserved(spec: &str) -> bool {
    let key = spec.rsplit('-').next().unwrap_or(spec);
    RESERVED_KEYS.iter().any(|r| r.eq_ignore_ascii_case(key))
}

/// True if `spec` has no modifier prefix (e.g. `"a"`, not `"cmd-a"`).
/// `Modes`-category shortcuts must satisfy this, since they're matched as
/// bare unmodified keys (see the module doc comment).
pub fn is_bare_key(spec: &str) -> bool {
    !spec.contains('-')
}

/// If `spec` is already used by a *different* shortcut's effective
/// binding, return that shortcut's label. Used to block duplicate
/// assignments when recording a new shortcut.
pub fn conflict(settings: &AppSettings, id: &str, spec: &str) -> Option<&'static str> {
    effective_bindings(settings)
        .into_iter()
        .find(|(other_id, other_spec)| *other_id != id && other_spec == spec)
        .map(|(other_id, _)| def(other_id).label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn settings_with(overrides: &[(&str, &str)]) -> AppSettings {
        let mut s = AppSettings::default();
        s.keybindings = overrides
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        s
    }

    #[test]
    fn effective_spec_falls_back_to_default() {
        let s = AppSettings::default();
        assert_eq!(effective_spec(&s, "undo"), "cmd-z");
    }

    #[test]
    fn effective_spec_uses_override() {
        let s = settings_with(&[("undo", "cmd-alt-z")]);
        assert_eq!(effective_spec(&s, "undo"), "cmd-alt-z");
    }

    #[test]
    fn effective_bindings_covers_all_ids_in_order() {
        let s = AppSettings::default();
        let ids: Vec<_> = effective_bindings(&s)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let expected: Vec<_> = SHORTCUTS.iter().map(|d| d.id).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn no_two_defaults_collide() {
        for (i, a) in SHORTCUTS.iter().enumerate() {
            for b in &SHORTCUTS[i + 1..] {
                assert_ne!(
                    a.default_spec, b.default_spec,
                    "{} and {} share a default spec",
                    a.id, b.id
                );
            }
        }
    }

    #[test]
    fn reserved_keys_are_rejected() {
        assert!(is_reserved("escape"));
        assert!(is_reserved("cmd-delete"));
        assert!(is_reserved("backspace"));
        assert!(!is_reserved("cmd-o"));
    }

    #[test]
    fn bare_key_check() {
        assert!(is_bare_key("a"));
        assert!(!is_bare_key("cmd-a"));
        assert!(!is_bare_key("shift-a"));
    }

    #[test]
    fn conflict_detects_duplicate_and_ignores_self() {
        let s = AppSettings::default();
        // "cmd-z" is undo's default spec; assigning it to redo conflicts.
        assert_eq!(conflict(&s, "redo", "cmd-z"), Some("Undo"));
        // Assigning undo's own current spec to undo itself is not a conflict.
        assert_eq!(conflict(&s, "undo", "cmd-z"), None);
    }

    #[test]
    fn conflict_none_when_spec_unused() {
        let s = AppSettings::default();
        assert_eq!(conflict(&s, "undo", "cmd-alt-shift-z"), None);
    }
}
```

- [ ] **Step 2: Register the module**

In `src/lib.rs`, add `pub mod keybindings;` alphabetically (after `pub mod interaction;`, before `pub mod layers;`).

- [ ] **Step 3: Run the tests**

Run: `cargo test --lib keybindings:: -- --nocapture`
Expected: all 8 tests in `keybindings::tests` pass.

- [ ] **Step 4: Format and commit**

```bash
cargo fmt
git add src/keybindings.rs src/lib.rs
git commit -m "Add keybindings module: default shortcut table and resolution helpers"
```

---

### Task 3: Wire effective bindings into the app's real key dispatch

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `osm_gpui::keybindings::{effective_bindings, effective_spec}` (Task 2), `osm_gpui::settings_store::snapshot()` (existing).
- Produces: `pub(crate) fn key_bindings_from_settings(settings: &osm_gpui::settings_store::AppSettings) -> Vec<KeyBinding>` — used here and reused by Task 5's rebind-on-change handler in `menu.rs`.

- [ ] **Step 1: Add `key_bindings_from_settings`**

In `src/main.rs`, add this free function near the top level (after the `actions!` macro block, before the `AddImageryLayer` struct is fine):

```rust
/// Build the live `gpui::KeyBinding` list for the 7 cmd-modified actions
/// from the current effective shortcut settings. The 3 mode-switch letters
/// (`mode_add`/`mode_building`/`mode_extrude`) are deliberately excluded —
/// see the comment where this is called.
pub(crate) fn key_bindings_from_settings(
    settings: &osm_gpui::settings_store::AppSettings,
) -> Vec<KeyBinding> {
    osm_gpui::keybindings::effective_bindings(settings)
        .into_iter()
        .filter_map(|(id, spec)| match id {
            "open_settings" => Some(KeyBinding::new(&spec, OpenSettings, None)),
            "quit" => Some(KeyBinding::new(&spec, Quit, None)),
            "open_osm_file" => Some(KeyBinding::new(&spec, OpenOsmFile, None)),
            "download_osm" => Some(KeyBinding::new(&spec, DownloadFromOsm, None)),
            "upload_osm" => Some(KeyBinding::new(&spec, UploadToOsm, None)),
            "undo" => Some(KeyBinding::new(&spec, Undo, None)),
            "redo" => Some(KeyBinding::new(&spec, Redo, None)),
            _ => None,
        })
        .collect()
}
```

- [ ] **Step 2: Use it at window creation**

Replace the hardcoded block at `src/main.rs:3058` (the `cx.bind_keys([...])` call with the 7 literal `KeyBinding::new` entries and the explanatory comment about mode letters):

```rust
                        // Register keyboard bindings in the window context.
                        // Built from the user's effective shortcut settings
                        // (defaults, overridden by any customizations saved
                        // in Settings > Keyboard Shortcuts).
                        //
                        // Note: the "a"/"b"/"x" mode-switch shortcuts are
                        // deliberately NOT registered here as global key
                        // bindings. Unlike the cmd-modified bindings above,
                        // these are plain unmodified letter keys, which would
                        // otherwise risk firing while the user is typing into a
                        // text input elsewhere in the app (e.g. the tag-edit
                        // dialog's key/value fields). Instead they're handled in
                        // the map area's local `on_key_down` handler below,
                        // which only fires when the map area itself has focus —
                        // see the `on_key_down` closure in `render()`.
                        cx.bind_keys(key_bindings_from_settings(
                            &osm_gpui::settings_store::snapshot(),
                        ));
```

- [ ] **Step 3: Make the mode-letter handler settings-driven**

Replace the three `else if ev.keystroke.key == "a" | "b" | "x"` branches at `src/main.rs:2525-2535` with:

```rust
                        } else {
                            let settings = osm_gpui::settings_store::snapshot();
                            let key = ev.keystroke.key.as_str();
                            // Mode-switch shortcuts are handled here rather
                            // than as global key bindings so they only fire
                            // while the map area has focus (see the comment
                            // by `cx.bind_keys` in `main()`).
                            if key == osm_gpui::keybindings::effective_spec(&settings, "mode_add") {
                                this.on_set_mode(
                                    &SetMode { mode: EditModeAction::Add },
                                    window,
                                    cx,
                                );
                            } else if key
                                == osm_gpui::keybindings::effective_spec(&settings, "mode_building")
                            {
                                this.on_set_mode(
                                    &SetMode { mode: EditModeAction::Building },
                                    window,
                                    cx,
                                );
                            } else if key
                                == osm_gpui::keybindings::effective_spec(&settings, "mode_extrude")
                            {
                                this.on_set_mode(
                                    &SetMode { mode: EditModeAction::Extrude },
                                    window,
                                    cx,
                                );
                            }
                        }
```

(This sits as the final `else` branch after the existing `escape`/`enter`/`delete`/`backspace` checks — only the three mode-letter branches are being replaced, not the whole `on_key_down` body.)

- [ ] **Step 4: Build and run the existing test suite**

Run: `cargo build --release 2>&1 | tail -40`
Expected: builds cleanly (no errors). If `key_bindings_from_settings` or `KeyBinding` name resolution fails, fix imports (both are already in scope per the existing `use gpui::{..., KeyBinding, ...}` and local `actions!` macro).

Run: `cargo test --lib`
Expected: all existing tests still pass (this task doesn't change test-covered logic, only wiring).

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
git add src/main.rs
git commit -m "Drive real key bindings and mode-switch letters from keybindings settings"
```

---

### Task 4: "Keyboard Shortcuts" settings page — read-only list + Reset

**Files:**
- Modify: `src/ui/settings_window.rs`

**Interfaces:**
- Consumes: `osm_gpui::keybindings::{self, ShortcutCategory, SHORTCUTS}` (Task 2), `crate::settings_store::{self, AppSettings}` (existing import, already in this file).
- Produces:
  - `SettingsWindow` gains `EventEmitter<SettingsEvent>` where `pub enum SettingsEvent { KeybindingsChanged }`.
  - `SettingsWindow::shortcuts_page(&self, view: Entity<Self>) -> SettingPage` — third page, wired into `setting_pages()`.
  - `SettingsWindow::reset_shortcut(&mut self, id: &'static str, cx: &mut Context<Self>)` and `reset_all_shortcuts(&mut self, cx: &mut Context<Self>)`, both persisting via `settings_store::update_store` and emitting `SettingsEvent::KeybindingsChanged`.

This task covers everything except the interactive "Record" capture (Task 5 adds that on top) so Reset can be verified independently first.

- [ ] **Step 1: Add the event type and derive it on `SettingsWindow`**

Near the top of `src/ui/settings_window.rs`, after the `LoginState` enum, add:

```rust
/// Emitted by `SettingsWindow` when a change needs to propagate outside
/// this window — e.g. rebinding the live `gpui` keymap and refreshing the
/// native menu's shortcut labels, both of which require the concrete
/// `Action` types that only the binary crate (`src/main.rs`/`src/menu.rs`)
/// has access to.
pub enum SettingsEvent {
    KeybindingsChanged,
}
```

After the `impl Focusable for SettingsWindow` block (or anywhere at module scope), add:

```rust
impl EventEmitter<SettingsEvent> for SettingsWindow {}
```

(`EventEmitter` is already reachable via the file's `use gpui::*;` at the top.)

- [ ] **Step 2: Add reset methods**

Add these methods inside `impl SettingsWindow` (near `save_client_id`, which follows the same persist-then-notify shape):

```rust
    /// Remove `id`'s override, falling back to its default, and propagate
    /// the change to the live keymap and native menu.
    fn reset_shortcut(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.app_settings.keybindings.remove(id);
        settings_store::update_store(self.app_settings.clone());
        cx.emit(SettingsEvent::KeybindingsChanged);
        cx.notify();
    }

    /// Clear every shortcut override, restoring all ten defaults.
    fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.app_settings.keybindings.clear();
        settings_store::update_store(self.app_settings.clone());
        cx.emit(SettingsEvent::KeybindingsChanged);
        cx.notify();
    }
```

- [ ] **Step 3: Add the page**

Add `use crate::keybindings::{self, ShortcutCategory, SHORTCUTS};` to the file's imports, next to the existing `use crate::settings_store::{self, ApiServerChoice, AppSettings};` line (`crate::` here is the `osm_gpui` lib crate root, since `settings_window.rs` lives at `osm_gpui::ui::settings_window` — same reasoning as the existing `settings_store` import).

Add this method inside `impl SettingsWindow`:

```rust
    fn shortcuts_page(&self, view: Entity<Self>) -> SettingPage {
        let category_title = |c: ShortcutCategory| match c {
            ShortcutCategory::General => "General",
            ShortcutCategory::File => "File",
            ShortcutCategory::Edit => "Edit",
            ShortcutCategory::Modes => "Modes",
        };

        let mut groups = Vec::new();
        for category in [
            ShortcutCategory::General,
            ShortcutCategory::File,
            ShortcutCategory::Edit,
            ShortcutCategory::Modes,
        ] {
            let items: Vec<SettingItem> = SHORTCUTS
                .iter()
                .filter(|d| d.category == category)
                .map(|d| {
                    let id = d.id;
                    let label = d.label;
                    let has_override = self.app_settings.keybindings.contains_key(id);
                    let spec = keybindings::effective_spec(&self.app_settings, id);
                    let row_view = view.clone();
                    SettingItem::new(
                        label,
                        SettingField::render(move |_options, _window, cx| {
                            render_shortcut_row(row_view.clone(), id, spec.clone(), has_override, cx)
                        }),
                    )
                })
                .collect();
            groups.push(SettingGroup::new().title(category_title(category)).items(items));
        }

        let reset_all_view = view;
        groups.push(
            SettingGroup::new().item(SettingItem::render(move |_options, _window, _cx| {
                Button::new("reset-all-shortcuts")
                    .label("Reset All to Defaults")
                    .ghost()
                    .on_click({
                        let reset_all_view = reset_all_view.clone();
                        move |_ev, _window, cx| {
                            reset_all_view.update(cx, |this, cx| this.reset_all_shortcuts(cx));
                        }
                    })
            })),
        );

        SettingPage::new("Keyboard Shortcuts").groups(groups)
    }
```

Update `setting_pages()`:

```rust
    fn setting_pages(&self, cx: &mut Context<Self>) -> Vec<SettingPage> {
        let view = cx.entity();
        vec![
            self.account_page(view.clone()),
            self.imagery_page(view.clone()),
            self.shortcuts_page(view),
        ]
    }
```

- [ ] **Step 4: Add the row-render free function**

Add near the other `render_*` free functions (e.g. after `render_entry_row`):

```rust
fn render_shortcut_row(
    view: Entity<SettingsWindow>,
    id: &'static str,
    spec: String,
    has_override: bool,
    cx: &mut App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;

    let mut row = h_flex().gap_2().items_center();

    if let Ok(stroke) = gpui::Keystroke::parse(&spec) {
        row = row.child(gpui_component::kbd::Kbd::new(stroke));
    } else {
        row = row.child(Label::new(spec).text_sm().text_color(muted));
    }

    row = row.child(
        Button::new(("record-shortcut", id))
            .label("Record")
            .ghost()
            .compact()
            .on_click({
                let view = view.clone();
                move |_ev, _window, cx| {
                    view.update(cx, |this, cx| this.start_recording(id, cx));
                }
            }),
    );

    if has_override {
        row = row.child(
            Button::new(("reset-shortcut", id))
                .label("Reset")
                .ghost()
                .compact()
                .on_click(move |_ev, _window, cx| {
                    view.update(cx, |this, cx| this.reset_shortcut(id, cx));
                }),
        );
    }

    row.into_any_element()
}
```

This references `this.start_recording`, which Task 5 adds — Task 4 alone won't compile standalone, so do Step 5 (a temporary stub) now and let Task 5 replace it.

- [ ] **Step 5: Add a temporary `start_recording` stub so this task compiles on its own**

Add to `impl SettingsWindow` (Task 5 will replace the body):

```rust
    fn start_recording(&mut self, _id: &'static str, _cx: &mut Context<Self>) {
        // Filled in by the shortcut-recording task.
    }
```

- [ ] **Step 6: Build**

Run: `cargo build --release 2>&1 | tail -60`
Expected: builds cleanly. Common issues to check: `SettingGroup`/`SettingItem`/`SettingField`/`SettingPage` are already imported at the top of the file (they are, per the existing `use gpui_component::{... setting::{...}, ...}` line); `h_flex`/`v_flex`/`ActiveTheme` likewise already imported.

- [ ] **Step 7: Format and commit**

```bash
cargo fmt
git add src/ui/settings_window.rs
git commit -m "Add read-only Keyboard Shortcuts settings page with Reset/Reset All"
```

---

### Task 5: Interactive shortcut recording

**Files:**
- Modify: `src/ui/settings_window.rs`

**Interfaces:**
- Consumes: `keybindings::{is_reserved, is_bare_key, conflict, def}` (Task 2).
- Produces: `SettingsWindow` fields `recording: Option<&'static str>`, `shortcut_error: Option<(&'static str, SharedString)>`; replaces the Task 4 stub `start_recording` with the real implementation; adds `apply_shortcut(&mut self, id: &'static str, spec: String, cx: &mut Context<Self>)` and `cancel_recording(&mut self, cx: &mut Context<Self>)`.

- [ ] **Step 1: Add the new state fields**

In the `SettingsWindow` struct, add (near `login_state`):

```rust
    recording: Option<&'static str>,
    shortcut_error: Option<(&'static str, SharedString)>,
```

In `SettingsWindow::new`, initialize both to `None` in the returned struct literal.

- [ ] **Step 2: Implement `start_recording`, `cancel_recording`, `apply_shortcut`**

Replace the Task 4 stub with:

Reuse `self.focus_handle` (the window-wide handle already on `SettingsWindow`) for the key-capture div, rather than allocating a fresh `FocusHandle` on every render — only one row is ever recording at a time, and allocating one per render would fight `window.focus()` calls across frames.

```rust
    fn start_recording(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = Some(id);
        self.shortcut_error = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn cancel_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        self.shortcut_error = None;
        cx.notify();
    }

    /// Validate and, if valid, save `spec` as `id`'s override. On failure,
    /// sets `shortcut_error` and leaves `recording` active so the user can
    /// try another combo.
    fn apply_shortcut(&mut self, id: &'static str, spec: String, cx: &mut Context<Self>) {
        if keybindings::is_reserved(&spec) {
            self.shortcut_error = Some((id, "That key is reserved.".into()));
            cx.notify();
            return;
        }
        if keybindings::def(id).category == keybindings::ShortcutCategory::Modes
            && !keybindings::is_bare_key(&spec)
        {
            self.shortcut_error =
                Some((id, "Mode shortcuts can't use modifier keys.".into()));
            cx.notify();
            return;
        }
        if let Some(other_label) = keybindings::conflict(&self.app_settings, id, &spec) {
            self.shortcut_error =
                Some((id, format!("Already used by {other_label}.").into()));
            cx.notify();
            return;
        }

        self.app_settings.keybindings.insert(id.to_string(), spec);
        settings_store::update_store(self.app_settings.clone());
        self.recording = None;
        self.shortcut_error = None;
        cx.emit(SettingsEvent::KeybindingsChanged);
        cx.notify();
    }
```

- [ ] **Step 3: Wire key capture into the row renderer**

Replace `render_shortcut_row`'s body (Task 4, Step 4) with a version that renders a key-capturing state when this row's `id` is being recorded. Change the function signature to also take `recording: bool` and `error: Option<SharedString>`:

```rust
fn render_shortcut_row(
    view: Entity<SettingsWindow>,
    id: &'static str,
    spec: String,
    has_override: bool,
    recording: bool,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    cx: &mut App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    if recording {
        let capture_view = view.clone();

        let mut row = v_flex().gap_1().child(
            div()
                .track_focus(&focus_handle)
                .on_key_down({
                    let capture_view = capture_view.clone();
                    move |ev: &gpui::KeyDownEvent, _window, cx| {
                        if ev.keystroke.key.is_empty() {
                            // Modifier-only keydown (e.g. bare Cmd) — keep
                            // waiting for a real key.
                            return;
                        }
                        if ev.keystroke.key == "escape" {
                            capture_view.update(cx, |this, cx| this.cancel_recording(cx));
                            return;
                        }
                        let spec = ev.keystroke.unparse();
                        capture_view.update(cx, |this, cx| this.apply_shortcut(id, spec, cx));
                    }
                })
                .child(Label::new("Press keys… (Esc to cancel)").text_sm().text_color(muted)),
        );
        if let Some(msg) = error {
            row = row.child(Label::new(msg).text_xs().text_color(danger));
        }
        row.into_any_element()
    } else {
        let mut row = h_flex().gap_2().items_center();

        if let Ok(stroke) = gpui::Keystroke::parse(&spec) {
            row = row.child(gpui_component::kbd::Kbd::new(stroke));
        } else {
            row = row.child(Label::new(spec).text_sm().text_color(muted));
        }

        row = row.child(
            Button::new(("record-shortcut", id))
                .label("Record")
                .ghost()
                .compact()
                .on_click({
                    let view = view.clone();
                    move |_ev, window, cx| {
                        view.update(cx, |this, cx| this.start_recording(id, window, cx));
                    }
                }),
        );

        if has_override {
            row = row.child(
                Button::new(("reset-shortcut", id))
                    .label("Reset")
                    .ghost()
                    .compact()
                    .on_click(move |_ev, _window, cx| {
                        view.update(cx, |this, cx| this.reset_shortcut(id, cx));
                    }),
            );
        }

        row.into_any_element()
    }
}
```

Note: `Keystroke::unparse()` and `Keystroke::parse()` come from `gpui` (already imported wholesale via `use gpui::*;` at the top of this file).

- [ ] **Step 4: Update the call site in `shortcuts_page`**

`render_shortcut_row` now takes `focus_handle: FocusHandle` instead of a `window` reference (Step 3 reuses `SettingsWindow`'s own handle, not a fresh per-render one — see Step 2's note). Update the closure in `shortcuts_page`:

```rust
                    let recording = self.recording == Some(id);
                    let error = self
                        .shortcut_error
                        .as_ref()
                        .filter(|(err_id, _)| *err_id == id)
                        .map(|(_, msg)| msg.clone());
                    let focus_handle = self.focus_handle.clone();
                    SettingItem::new(
                        label,
                        SettingField::render(move |_options, _window, cx| {
                            render_shortcut_row(
                                row_view.clone(),
                                id,
                                spec.clone(),
                                has_override,
                                recording,
                                error.clone(),
                                focus_handle.clone(),
                                cx,
                            )
                        }),
                    )
```

(This replaces the `SettingItem::new(label, SettingField::render(...))` block from Task 4 Step 3 — capture `recording`, `error`, and `focus_handle` into the closure alongside the existing `id`/`label`/`has_override`/`spec`/`row_view`.)

- [ ] **Step 5: Build**

Run: `cargo build --release 2>&1 | tail -60`
Expected: builds cleanly.

- [ ] **Step 6: Format and commit**

```bash
cargo fmt
git add src/ui/settings_window.rs
git commit -m "Add press-to-record shortcut capture with reserved-key and conflict validation"
```

---

### Task 6: Apply changes live — rebind keymap and refresh the native menu

**Files:**
- Modify: `src/menu.rs`

**Interfaces:**
- Consumes: `crate::key_bindings_from_settings` (Task 3, `pub(crate)` in `main.rs`), `osm_gpui::ui::settings_window::SettingsEvent` (Task 4/5), `crate::rebuild_menus` (existing, same file).

- [ ] **Step 1: Subscribe to `SettingsEvent` in `open_settings`**

In `src/menu.rs`, `open_settings`'s window-creation closure currently does:

```rust
            |window, cx| {
                let view =
                    cx.new(|cx| osm_gpui::ui::settings_window::SettingsWindow::new(window, cx));
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
```

Change it to keep a handle to `view` and subscribe before wrapping it in `Root`:

```rust
            |window, cx| {
                let view =
                    cx.new(|cx| osm_gpui::ui::settings_window::SettingsWindow::new(window, cx));
                cx.subscribe(
                    &view,
                    |_entity, event: &osm_gpui::ui::settings_window::SettingsEvent, cx| {
                        match event {
                            osm_gpui::ui::settings_window::SettingsEvent::KeybindingsChanged => {
                                apply_keybindings_change(cx);
                            }
                        }
                    },
                )
                .detach();
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
```

- [ ] **Step 2: Add `apply_keybindings_change`**

Add this free function to `src/menu.rs` (near `rebuild_menus`):

```rust
/// Re-apply the live `gpui` keymap from current settings and refresh the
/// native menu's shortcut labels, in response to a
/// `SettingsEvent::KeybindingsChanged` from the Settings window. Menu
/// shortcut labels are baked in at `cx.set_menus` time (they don't update
/// on their own), so this always re-triggers a full `rebuild_menus` too.
fn apply_keybindings_change(cx: &mut App) {
    let settings = osm_gpui::settings_store::snapshot();
    cx.clear_key_bindings();
    cx.bind_keys(crate::key_bindings_from_settings(&settings));

    // rebuild_menus needs a map center and imagery-load state; pull the
    // live map viewer's if it exists yet, else fall back to the same
    // placeholder used before the map viewer/imagery index are ready (see
    // the startup call to `rebuild_menus` in `main.rs`).
    let (lat, lon, state) = crate::MAP_VIEWER_HANDLE
        .get()
        .and_then(|h| h.upgrade())
        .map(|view| {
            view.read(cx)
                .imagery_menu_context()
        })
        .unwrap_or((40.7128, -74.0060, crate::ImageryLoadState::Loading));
    rebuild_menus(cx, lat, lon, state);
}
```

- [ ] **Step 3: Add the small accessor this needs on `MapViewer`**

`apply_keybindings_change` calls `view.read(cx).imagery_menu_context()`, a small getter that doesn't exist yet. In `src/main.rs`, add this method to `impl MapViewer` (near `maybe_rebuild_imagery_menu`, which computes the same three values inline):

```rust
    /// The `(center_lat, center_lon, ImageryLoadState)` triple `rebuild_menus`
    /// needs, as currently known to this viewer. Exposed so callers outside
    /// the per-frame `maybe_rebuild_imagery_menu` loop (e.g. the Settings
    /// window's keybindings-changed handler in `menu.rs`) can trigger an
    /// immediate menu rebuild without duplicating this lookup.
    pub(crate) fn imagery_menu_context(&self) -> (f64, f64, ImageryLoadState) {
        let (lat, lon) = self.viewport.center();
        let state = IMAGERY_LOAD_STATE
            .get()
            .and_then(|s| s.lock().ok().map(|g| *g))
            .unwrap_or(ImageryLoadState::Loading);
        (lat, lon, state)
    }
```

- [ ] **Step 4: Build**

Run: `cargo build --release 2>&1 | tail -60`
Expected: builds cleanly. If `MAP_VIEWER_HANDLE`, `ImageryLoadState`, or `key_bindings_from_settings` aren't visible from `menu.rs`, check their visibility in `main.rs` — `MAP_VIEWER_HANDLE` and `ImageryLoadState` are already used from `menu.rs` today (see `open_osm_file`), and `key_bindings_from_settings` was declared `pub(crate)` in Task 3.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
git add src/menu.rs src/main.rs
git commit -m "Apply keybinding changes live: rebind keymap and refresh native menu"
```

---

### Task 7: Full verification pass

**Files:** none (verification only).

- [ ] **Step 1: Full test suite**

Run: `cargo test --lib`
Expected: all tests pass, including the new `settings_store::tests::round_trip_with_keybindings` and all `keybindings::tests::*`.

- [ ] **Step 2: Format check (matches CI)**

Run: `cargo fmt --check`
Expected: no output (clean).

- [ ] **Step 3: Release build**

Run: `cargo build --release 2>&1 | tail -40`
Expected: builds cleanly, no warnings introduced by this feature.

- [ ] **Step 4: Manual verification note**

The Settings window is a second native OS window; this sandboxed dev environment is known to segfault when opening a second `gpui` window even on unmodified `main` (see `project_second_window_sandbox_crash` in project memory — pre-existing environment limitation, not something this feature introduces). Because of this, the "Keyboard Shortcuts" page's recording/reset/conflict UI can't be reliably driven end-to-end via this sandbox's osmscript harness. Note this explicitly rather than claiming UI verification that didn't happen: the manual check (open Settings → Keyboard Shortcuts, record a new shortcut, confirm a conflicting one is rejected, confirm Reset restores the default, confirm the native menu label updates) should be run by the user (or in an unsandboxed environment) before merging, and called out as such in the PR description.

- [ ] **Step 5: Final commit check**

```bash
git log --oneline main..HEAD
git status
```

Expected: seven feature commits (Tasks 1–6, one each) on top of the design-doc commit, clean working tree.
