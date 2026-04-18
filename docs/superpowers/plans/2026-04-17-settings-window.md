# Settings Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a settings window (opened via menu, Cmd+,) with a custom imagery sources section that supports viewing, adding, editing, and deleting entries using Zed's `ui` crate components.

**Architecture:** A new `SettingsWindow` entity rendered in its own gpui window, separate from the map. It reads/writes the existing `CUSTOM_IMAGERY_STORE` global and persists via `custom_imagery_store::save()`. The Zed `theme` crate is initialized at app startup so `ui` components render with the fallback dark theme. The `on_window_closed` handler is updated to only quit when the *map* window closes, not when the settings window closes.

**Tech Stack:** Rust, gpui, Zed `ui` crate (Button, IconButton, Label, ListItem, ListHeader, Divider, Icon), Zed `theme` crate

**Spec:** `docs/superpowers/specs/2026-04-17-settings-window-design.md`

---

### Task 1: Initialize Zed theme at app startup

**Files:**
- Modify: `src/main.rs:1537` (inside `gpui_platform::application().run` closure)

- [ ] **Step 1: Add theme::init call**

At the top of the `gpui_platform::application().run` closure (line 1537), before `cx.activate(true)`, add theme initialization:

```rust
// Inside the run closure, before cx.activate(true):
theme::init(theme::LoadThemes::JustBase, cx);
```

Also add the import at the top of `main.rs`:

```rust
use theme; // add to existing imports
```

- [ ] **Step 2: Build and verify**

Run: `cargo build 2>&1 | tail -20`
Expected: Compiles with no new errors.

- [ ] **Step 3: Run the app to verify no visual regressions**

Run: `cargo run --release`
Expected: App starts and renders normally. The map window looks the same as before.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs Cargo.toml
git commit -m "Initialize Zed theme system at startup"
```

Note: `Cargo.toml` already has the `ui` dependency added during exploration. Include it in this first commit.

---

### Task 2: Add OpenSettings action and menu item

**Files:**
- Modify: `src/main.rs:23` (actions macro)
- Modify: `src/main.rs:1537-1560` (action registration and key bindings)
- Modify: `src/main.rs:1846-1877` (rebuild_menus function, the "OSM Viewer" menu)

- [ ] **Step 1: Define the action**

Add `OpenSettings` to the existing `actions!` macro on line 23:

```rust
actions!(osm_gpui, [OpenOsmFile, Quit, AddOsmCarto, AddCoordinateGrid, DownloadFromOsm, ToggleDebugOverlay, AddCustomImagery, OpenSettings]);
```

- [ ] **Step 2: Register the action handler (stub)**

In the `run` closure (around line 1542), add:

```rust
cx.on_action(open_settings);
```

Add the stub handler function after the existing `quit` function (around line 1678):

```rust
fn open_settings(_: &OpenSettings, _cx: &mut App) {
    eprintln!("settings: open settings (stub)");
}
```

- [ ] **Step 3: Add Cmd+, key binding**

In the `cx.bind_keys` block (around line 1607), add:

```rust
KeyBinding::new("cmd-,", OpenSettings, None),
```

- [ ] **Step 4: Add Settings menu item to the app menu**

In `rebuild_menus`, modify the "OSM Viewer" menu (around line 1846) to add a Settings item:

```rust
Menu {
    name: "OSM Viewer".into(),
    items: vec![
        MenuItem::action("Settings…", OpenSettings),
        MenuItem::separator(),
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action("Quit", Quit),
    ],
    disabled: false,
},
```

- [ ] **Step 5: Build and verify**

Run: `cargo build 2>&1 | tail -10`
Expected: Compiles without errors.

- [ ] **Step 6: Run and test menu + shortcut**

Run: `cargo run --release`
Expected: "Settings…" appears in the "OSM Viewer" menu. Clicking it or pressing Cmd+, prints "settings: open settings (stub)" to stderr. App continues to work normally.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "Add OpenSettings action with Cmd+, shortcut and menu item"
```

---

### Task 3: Create the SettingsWindow entity with empty layout

**Files:**
- Create: `src/ui/settings_window.rs`
- Modify: `src/ui/mod.rs`
- Modify: `src/main.rs` (open_settings handler)

- [ ] **Step 1: Create the settings window module**

Create `src/ui/settings_window.rs`:

```rust
//! Settings window with custom imagery management.

use gpui::*;
use ui::prelude::*;

pub struct SettingsWindow {
    focus_handle: FocusHandle,
}

impl SettingsWindow {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().background)
            .p(DynamicSpacing::Base16.rems(cx))
            .child(
                Headline::new("Settings")
                    .size(HeadlineSize::Large),
            )
    }
}
```

- [ ] **Step 2: Export the module**

In `src/ui/mod.rs`, add:

```rust
pub mod settings_window;
```

- [ ] **Step 3: Wire up open_settings to open a real window**

In `src/main.rs`, replace the `open_settings` stub with:

```rust
fn open_settings(_: &OpenSettings, cx: &mut App) {
    cx.open_window(
        WindowOptions {
            window_bounds: Some(gpui::WindowBounds::Windowed(Bounds {
                origin: point(px(200.0), px(200.0)),
                size: size(px(600.0), px(500.0)),
            })),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Settings".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            focus: true,
            ..Default::default()
        },
        |_window, cx| {
            cx.new(|cx| osm_gpui::ui::settings_window::SettingsWindow::new(cx))
        },
    )
    .unwrap();
}
```

- [ ] **Step 4: Fix on_window_closed to only quit on map window close**

The current `on_window_closed` handler quits on *any* window close. We need to track the map window ID and only quit when that window closes. Around line 1591, capture the window handle and use it:

```rust
let map_window = cx.open_window(
    // ... existing WindowOptions ...
)
.unwrap();

let map_window_id = map_window.window_id();
cx.on_window_closed(move |cx, window_id| {
    if window_id == map_window_id {
        cx.quit();
    }
})
.detach();
```

- [ ] **Step 5: Build and verify**

Run: `cargo build 2>&1 | tail -10`
Expected: Compiles without errors.

- [ ] **Step 6: Run and test**

Run: `cargo run --release`
Expected: Cmd+, opens a new window titled "Settings" with a dark background and "Settings" headline text. Closing the settings window does NOT quit the app. Closing the map window does quit.

- [ ] **Step 7: Commit**

```bash
git add src/ui/settings_window.rs src/ui/mod.rs src/main.rs
git commit -m "Add empty settings window with theme-aware rendering"
```

---

### Task 4: Display the custom imagery entries list

**Files:**
- Modify: `src/ui/settings_window.rs`

- [ ] **Step 1: Add state for the entries list**

Update `SettingsWindow` to load entries from the global store:

```rust
use crate::custom_imagery_store::{self, CustomImageryEntry};

pub struct SettingsWindow {
    focus_handle: FocusHandle,
    entries: Vec<CustomImageryEntry>,
}

impl SettingsWindow {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let entries = crate::custom_imagery_snapshot();
        Self {
            focus_handle: cx.focus_handle(),
            entries,
        }
    }
}
```

Note: `custom_imagery_snapshot()` is defined in `main.rs` but not exported. We need to make it accessible. Add this public function to `src/custom_imagery_store.rs`:

```rust
/// Read the current entries from the global store.
/// Returns empty vec if the store is not initialized.
pub fn snapshot() -> Vec<CustomImageryEntry> {
    match default_path() {
        Some(p) => load_from(&p),
        None => Vec::new(),
    }
}
```

Actually, since the settings window should read from the same in-memory store as the rest of the app, and `CUSTOM_IMAGERY_STORE` is private to `main.rs`, the simplest approach is to re-read from disk. The entries are small and this only happens when opening the settings window. Use `custom_imagery_store::load()` instead:

```rust
let entries = custom_imagery_store::load();
```

- [ ] **Step 2: Render the list with ListHeader and ListItems**

Update the `render` method:

```rust
impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex()
            .gap(DynamicSpacing::Base08.rems(cx))
            .child(
                ListHeader::new("Custom Imagery Sources"),
            );

        if self.entries.is_empty() {
            content = content.child(
                Label::new("No custom imagery sources configured.")
                    .color(Color::Muted)
                    .size(LabelSize::Small),
            );
        } else {
            for (idx, entry) in self.entries.iter().enumerate() {
                content = content.child(
                    ListItem::new(("entry", idx))
                        .child(Label::new(entry.name.clone()))
                );
            }
        }

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().background)
            .p(DynamicSpacing::Base16.rems(cx))
            .child(content)
    }
}
```

- [ ] **Step 3: Build and verify**

Run: `cargo build 2>&1 | tail -10`
Expected: Compiles without errors.

- [ ] **Step 4: Run and test with entries**

Before running, ensure you have at least one custom imagery entry saved (add one via the existing "Custom Imagery > Add…" menu if needed).

Run: `cargo run --release`
Expected: Settings window shows "Custom Imagery Sources" header. Below it, each saved entry name appears as a list item. If no entries exist, the "No custom imagery sources configured." message appears.

- [ ] **Step 5: Commit**

```bash
git add src/ui/settings_window.rs
git commit -m "Display custom imagery entries list in settings window"
```

---

### Task 5: Add expand/collapse accordion behavior

**Files:**
- Modify: `src/ui/settings_window.rs`

- [ ] **Step 1: Add expanded state and disclosure toggle**

Add an `expanded_index` field to `SettingsWindow`:

```rust
pub struct SettingsWindow {
    focus_handle: FocusHandle,
    entries: Vec<CustomImageryEntry>,
    expanded_index: Option<usize>,
}
```

Initialize it as `None` in `new()`.

- [ ] **Step 2: Add disclosure icon and click-to-expand on ListItems**

Update the entry rendering in the `render` method. Replace the simple `ListItem` with one that has a disclosure toggle and shows detail when expanded:

```rust
for (idx, entry) in self.entries.iter().enumerate() {
    let is_expanded = self.expanded_index == Some(idx);

    let item = ListItem::new(("entry", idx))
        .child(Label::new(entry.name.clone()))
        .toggle(Some(is_expanded))
        .on_toggle(cx.listener(move |this, _toggled, _window, cx| {
            if this.expanded_index == Some(idx) {
                this.expanded_index = None;
            } else {
                this.expanded_index = Some(idx);
            }
            cx.notify();
        }))
        .end_slot(
            IconButton::new(("delete", idx), IconName::Trash)
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
        );

    content = content.child(item);

    if is_expanded {
        content = content.child(
            v_flex()
                .pl(DynamicSpacing::Base24.rems(cx))
                .gap(DynamicSpacing::Base04.rems(cx))
                .child(
                    Label::new(format!("URL: {}", entry.url_template))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    Label::new(format!("Zoom: {}–{}", entry.min_zoom, entry.max_zoom))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        );
    }
}
```

- [ ] **Step 3: Build and verify**

Run: `cargo build 2>&1 | tail -10`
Expected: Compiles without errors.

- [ ] **Step 4: Run and test accordion**

Run: `cargo run --release`
Expected: Each entry row has a disclosure chevron. Clicking it expands the row to show URL and zoom details. Clicking again collapses. Expanding one entry collapses any previously expanded entry.

- [ ] **Step 5: Commit**

```bash
git add src/ui/settings_window.rs
git commit -m "Add expand/collapse accordion for imagery entries"
```

---

### Task 6: Add editable fields in expanded view

**Files:**
- Modify: `src/ui/settings_window.rs`

- [ ] **Step 1: Add editing state with TextInput entities**

Add fields for tracking the edit state:

```rust
use crate::ui::text_input::TextInput;

pub struct SettingsWindow {
    focus_handle: FocusHandle,
    entries: Vec<CustomImageryEntry>,
    expanded_index: Option<usize>,
    // Edit fields — created when an entry is expanded
    edit_name: Option<Entity<TextInput>>,
    edit_url: Option<Entity<TextInput>>,
    edit_min_zoom: Option<Entity<TextInput>>,
    edit_max_zoom: Option<Entity<TextInput>>,
    edit_error: Option<SharedString>,
}
```

Initialize all edit fields as `None` in `new()`.

- [ ] **Step 2: Create TextInput entities when expanding**

Add a method to populate edit fields from an entry:

```rust
impl SettingsWindow {
    fn start_editing(&mut self, entry: &CustomImageryEntry, cx: &mut Context<Self>) {
        let name = cx.new(|cx| {
            let mut input = TextInput::new(cx, "Name");
            input.set_content(&entry.name, cx);
            input
        });
        let url = cx.new(|cx| {
            let mut input = TextInput::new(cx, "https://…/{z}/{x}/{y}.png");
            input.set_content(&entry.url_template, cx);
            input
        });
        let min_zoom = cx.new(|cx| {
            let mut input = TextInput::new(cx, "0");
            input.set_content(&entry.min_zoom.to_string(), cx);
            input
        });
        let max_zoom = cx.new(|cx| {
            let mut input = TextInput::new(cx, "19");
            input.set_content(&entry.max_zoom.to_string(), cx);
            input
        });
        self.edit_name = Some(name);
        self.edit_url = Some(url);
        self.edit_min_zoom = Some(min_zoom);
        self.edit_max_zoom = Some(max_zoom);
        self.edit_error = None;
    }

    fn clear_editing(&mut self) {
        self.edit_name = None;
        self.edit_url = None;
        self.edit_min_zoom = None;
        self.edit_max_zoom = None;
        self.edit_error = None;
    }
}
```

- [ ] **Step 3: Call start_editing on expand, clear on collapse**

Update the toggle handler:

```rust
.on_toggle(cx.listener(move |this, _toggled, _window, cx| {
    if this.expanded_index == Some(idx) {
        this.expanded_index = None;
        this.clear_editing();
    } else {
        this.expanded_index = Some(idx);
        let entry = this.entries[idx].clone();
        this.start_editing(&entry, cx);
    }
    cx.notify();
}))
```

- [ ] **Step 4: Render edit fields instead of read-only labels when expanded**

Replace the expanded detail view with editable fields:

```rust
if is_expanded {
    if let (Some(name), Some(url), Some(min_z), Some(max_z)) = (
        self.edit_name.clone(),
        self.edit_url.clone(),
        self.edit_min_zoom.clone(),
        self.edit_max_zoom.clone(),
    ) {
        let save = cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
            this.save_entry(idx, cx);
        });
        let cancel = cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
            this.expanded_index = None;
            this.clear_editing();
            cx.notify();
        });

        content = content.child(
            v_flex()
                .pl(DynamicSpacing::Base24.rems(cx))
                .pr(DynamicSpacing::Base08.rems(cx))
                .gap(DynamicSpacing::Base06.rems(cx))
                .child(field_row("Name", name))
                .child(field_row("URL template", url))
                .child(
                    h_flex()
                        .gap(DynamicSpacing::Base08.rems(cx))
                        .child(div().flex_1().child(field_row("Min zoom", min_z)))
                        .child(div().flex_1().child(field_row("Max zoom", max_z))),
                )
                .children(self.edit_error.clone().map(|msg| {
                    Label::new(msg).color(Color::Error).size(LabelSize::Small)
                }))
                .child(
                    h_flex()
                        .gap(DynamicSpacing::Base04.rems(cx))
                        .child(
                            Button::new(("cancel", idx), "Cancel")
                                .style(ButtonStyle::Subtle)
                                .on_click(move |_ev, window, cx| {
                                    // Re-dispatch as MouseDownEvent isn't used here.
                                    // We'll handle this differently below.
                                })
                        )
                        .child(
                            Button::new(("save", idx), "Save")
                                .style(ButtonStyle::Filled)
                                .on_click(move |_ev, window, cx| {
                                    // Same — handle below.
                                })
                        ),
                ),
        );
    }
}
```

Actually, `Button::on_click` uses `ClickEvent`, not `MouseDownEvent`. Let's use the proper signatures. Replace the buttons section with:

```rust
.child(
    h_flex()
        .gap(DynamicSpacing::Base04.rems(cx))
        .child(
            Button::new(("cancel", idx), "Cancel")
                .style(ButtonStyle::Subtle)
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.expanded_index = None;
                    this.clear_editing();
                    cx.notify();
                }))
        )
        .child(
            Button::new(("save", idx), "Save")
                .style(ButtonStyle::Filled)
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.save_entry(idx, cx);
                }))
        ),
)
```

- [ ] **Step 5: Add the field_row helper**

Add this function in the module:

```rust
fn field_row(label: &'static str, input: Entity<TextInput>) -> impl IntoElement {
    v_flex()
        .gap(DynamicSpacing::Base02.px())
        .child(Label::new(label).size(LabelSize::XSmall).color(Color::Muted))
        .child(input)
}
```

Note: `DynamicSpacing::Base02.px()` may not exist. Use `gpui::px(2.0)` as a fallback, or `rems(0.125)`. Check at build time and adjust.

- [ ] **Step 6: Add save_entry method (stub)**

```rust
impl SettingsWindow {
    fn save_entry(&mut self, idx: usize, cx: &mut Context<Self>) {
        // Will be implemented in the next task
        eprintln!("settings: save entry {} (stub)", idx);
    }
}
```

- [ ] **Step 7: Build and verify**

Run: `cargo build 2>&1 | tail -20`
Expected: Compiles. Fix any type mismatches (e.g., spacing API differences).

- [ ] **Step 8: Run and test**

Run: `cargo run --release`
Expected: Expanding an entry shows four text inputs pre-populated with the entry's current values, plus Save and Cancel buttons. Cancel collapses the row. Save prints a stub message.

- [ ] **Step 9: Commit**

```bash
git add src/ui/settings_window.rs
git commit -m "Add editable text fields in expanded accordion view"
```

---

### Task 7: Implement save with validation

**Files:**
- Modify: `src/ui/settings_window.rs`

- [ ] **Step 1: Implement save_entry**

Replace the stub with real validation and persistence:

```rust
use crate::ui::custom_imagery_dialog::validate;

impl SettingsWindow {
    fn save_entry(&mut self, idx: usize, cx: &mut Context<Self>) {
        let (Some(name), Some(url), Some(min_z), Some(max_z)) = (
            self.edit_name.as_ref(),
            self.edit_url.as_ref(),
            self.edit_min_zoom.as_ref(),
            self.edit_max_zoom.as_ref(),
        ) else {
            return;
        };

        let name_val = name.read(cx).content().to_string();
        let url_val = url.read(cx).content().to_string();
        let min_val = min_z.read(cx).content().to_string();
        let max_val = max_z.read(cx).content().to_string();

        match validate(&name_val, &url_val, &min_val, &max_val) {
            Ok(entry) => {
                self.entries[idx] = entry;
                self.persist(cx);
                self.expanded_index = None;
                self.clear_editing();
                cx.notify();
            }
            Err(e) => {
                self.edit_error = Some(
                    crate::ui::custom_imagery_dialog::error_message(&e).into(),
                );
                cx.notify();
            }
        }
    }

    fn persist(&self, _cx: &mut Context<Self>) {
        custom_imagery_store::save(&self.entries);
        // Also update the in-memory global store so menus reflect changes.
        if let Some(store) = crate::CUSTOM_IMAGERY_STORE.get() {
            if let Ok(mut g) = store.lock() {
                *g = self.entries.clone();
            }
        }
    }
}
```

Note: `CUSTOM_IMAGERY_STORE` is private to `main.rs`. To avoid making it public, we can instead just save to disk and let the menu rebuild read from the store. But since the in-memory store is what the menu reads, we need a way to sync. The simplest approach: make `CUSTOM_IMAGERY_STORE` `pub(crate)` in `main.rs`, or add a helper function. Add this public function to `main.rs`:

```rust
/// Replace the in-memory custom imagery store contents and persist to disk.
pub fn update_custom_imagery_store(entries: Vec<CustomImageryEntry>) {
    if let Some(store) = CUSTOM_IMAGERY_STORE.get() {
        if let Ok(mut g) = store.lock() {
            *g = entries.clone();
        }
    }
    custom_imagery_store::save(&entries);
}
```

Then in `settings_window.rs`, the `persist` method becomes:

```rust
fn persist(&self, _cx: &mut Context<Self>) {
    crate::update_custom_imagery_store(self.entries.clone());
}
```

Also make `error_message` in `custom_imagery_dialog.rs` public (it's currently private):

Change `fn error_message` to `pub fn error_message` in `src/ui/custom_imagery_dialog.rs`.

- [ ] **Step 2: Build and verify**

Run: `cargo build 2>&1 | tail -10`
Expected: Compiles without errors.

- [ ] **Step 3: Run and test validation**

Run: `cargo run --release`
Expected: Editing an entry and clicking Save validates the input. If validation fails, error message appears in red below the fields. If validation passes, the entry updates, the row collapses, and the entry list shows the new name.

- [ ] **Step 4: Verify persistence**

After saving an edit, close and reopen the settings window. The edited values should persist.

- [ ] **Step 5: Commit**

```bash
git add src/ui/settings_window.rs src/ui/custom_imagery_dialog.rs src/main.rs
git commit -m "Implement save with validation for imagery entry editing"
```

---

### Task 8: Implement delete with inline confirmation

**Files:**
- Modify: `src/ui/settings_window.rs`

- [ ] **Step 1: Add delete confirmation state**

Add a field to track which entry is pending deletion:

```rust
pub struct SettingsWindow {
    // ... existing fields ...
    confirm_delete_index: Option<usize>,
}
```

Initialize as `None` in `new()`.

- [ ] **Step 2: Wire up delete button to show confirmation**

Update the trash `IconButton` on each row to set the confirmation state:

```rust
.end_slot(
    if self.confirm_delete_index == Some(idx) {
        h_flex()
            .gap(DynamicSpacing::Base04.rems(cx))
            .child(
                Label::new(format!("Delete {}?", entry.name))
                    .size(LabelSize::Small)
                    .color(Color::Error),
            )
            .child(
                Button::new(("confirm-delete", idx), "Delete")
                    .style(ButtonStyle::Tinted(TintColor::Error))
                    .size(ButtonSize::Compact)
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.delete_entry(idx, cx);
                    }))
            )
            .child(
                Button::new(("cancel-delete", idx), "Cancel")
                    .style(ButtonStyle::Subtle)
                    .size(ButtonSize::Compact)
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.confirm_delete_index = None;
                        cx.notify();
                    }))
            )
            .into_any_element()
    } else {
        IconButton::new(("delete", idx), IconName::Trash)
            .icon_size(IconSize::Small)
            .icon_color(Color::Muted)
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.confirm_delete_index = Some(idx);
                cx.notify();
            }))
            .into_any_element()
    }
)
```

Note: `end_slot` expects `impl IntoElement`. Since the two branches return different types, we use `.into_any_element()` on each branch to unify the type.

- [ ] **Step 3: Implement delete_entry**

```rust
impl SettingsWindow {
    fn delete_entry(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx < self.entries.len() {
            self.entries.remove(idx);
            self.persist(cx);
        }
        // Clear any expanded/editing state that may now have stale indices
        self.expanded_index = None;
        self.clear_editing();
        self.confirm_delete_index = None;
        cx.notify();
    }
}
```

- [ ] **Step 4: Build and verify**

Run: `cargo build 2>&1 | tail -10`
Expected: Compiles without errors.

- [ ] **Step 5: Run and test delete flow**

Run: `cargo run --release`
Expected: Hovering over a row shows the trash icon. Clicking it replaces the icon with "Delete [name]? [Delete] [Cancel]". Clicking Delete removes the entry. Clicking Cancel restores the trash icon. After deletion, the entry is gone from the list and from disk.

- [ ] **Step 6: Commit**

```bash
git add src/ui/settings_window.rs
git commit -m "Add delete with inline confirmation for imagery entries"
```

---

### Task 9: Add "Add Source" button for new entries

**Files:**
- Modify: `src/ui/settings_window.rs`

- [ ] **Step 1: Add the "Add Source" button below the list**

After the entries loop in the `render` method, add:

```rust
content = content.child(
    Button::new("add-source", "Add Source")
        .style(ButtonStyle::Subtle)
        .start_icon(Icon::new(IconName::Plus))
        .on_click(cx.listener(|this, _ev, _window, cx| {
            this.add_new_entry(cx);
        })),
);
```

- [ ] **Step 2: Implement add_new_entry**

```rust
impl SettingsWindow {
    fn add_new_entry(&mut self, cx: &mut Context<Self>) {
        // Append a blank entry
        let blank = CustomImageryEntry {
            name: String::new(),
            url_template: String::new(),
            min_zoom: 0,
            max_zoom: 19,
        };
        self.entries.push(blank.clone());
        let new_idx = self.entries.len() - 1;
        self.expanded_index = Some(new_idx);
        self.confirm_delete_index = None;
        self.start_editing(&blank, cx);
        cx.notify();
    }
}
```

- [ ] **Step 3: Handle cancel on a new (unsaved) entry**

When cancelling an add, we should remove the blank entry. Update the cancel button handler in the expanded view to detect new entries:

In the cancel button `on_click`, check if the entry is blank (name is empty and url_template is empty) and remove it:

```rust
.on_click(cx.listener(move |this, _ev, _window, cx| {
    // If this was a new blank entry that hasn't been saved, remove it
    if let Some(entry) = this.entries.get(idx) {
        if entry.name.is_empty() && entry.url_template.is_empty() {
            this.entries.remove(idx);
        }
    }
    this.expanded_index = None;
    this.clear_editing();
    cx.notify();
}))
```

And similarly, update `save_entry` — when saving a new entry, it should validate and persist just like editing an existing one. The existing `save_entry` code already handles this correctly since it assigns to `self.entries[idx]`.

- [ ] **Step 4: Build and verify**

Run: `cargo build 2>&1 | tail -10`
Expected: Compiles without errors.

- [ ] **Step 5: Run and test add flow**

Run: `cargo run --release`
Expected: "Add Source" button appears below the list. Clicking it adds a new row in expanded edit mode with empty fields. Filling in valid data and clicking Save persists the entry. Clicking Cancel removes the blank row.

- [ ] **Step 6: Test the empty state**

Delete all entries. The "No custom imagery sources configured." message should appear. "Add Source" button should still be visible.

- [ ] **Step 7: Commit**

```bash
git add src/ui/settings_window.rs
git commit -m "Add 'Add Source' button for creating new imagery entries"
```

---

### Task 10: Polish and integration testing

**Files:**
- Modify: `src/ui/settings_window.rs` (minor adjustments)

- [ ] **Step 1: Prevent opening multiple settings windows**

In `main.rs`, track whether a settings window is already open. Add a global:

```rust
static SETTINGS_WINDOW_OPEN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
```

Update `open_settings`:

```rust
fn open_settings(_: &OpenSettings, cx: &mut App) {
    if SETTINGS_WINDOW_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    SETTINGS_WINDOW_OPEN.store(true, std::sync::atomic::Ordering::Relaxed);

    cx.open_window(
        // ... existing window options ...
    )
    .unwrap();
}
```

Reset the flag when the settings window closes. Add inside the `open_settings` function, after `cx.open_window`:

```rust
cx.on_window_closed(move |_cx, window_id| {
    // Only reset if this was the settings window
    // We can track the settings window ID, or just always reset since
    // we already guard with the flag.
    // For simplicity, we check the map window separately.
}).detach();
```

Actually, we need to be more precise. Update the existing `on_window_closed` handler to differentiate. The simplest approach: track the settings window ID.

```rust
fn open_settings(_: &OpenSettings, cx: &mut App) {
    if SETTINGS_WINDOW_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    SETTINGS_WINDOW_OPEN.store(true, std::sync::atomic::Ordering::Relaxed);

    let settings_window = cx.open_window(
        // ... existing window options ...
    )
    .unwrap();

    let settings_window_id = settings_window.window_id();
    cx.on_window_closed(move |_cx, window_id| {
        if window_id == settings_window_id {
            SETTINGS_WINDOW_OPEN.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    })
    .detach();
}
```

- [ ] **Step 2: Build and run full integration test**

Run: `cargo build 2>&1 | tail -10`

Then: `cargo run --release`

Test the full workflow:
1. Open settings via Cmd+,
2. See the list of custom imagery sources (or empty state)
3. Add a new source — fill in name, URL, zoom, click Save
4. Verify it appears in the list
5. Expand the entry, edit the name, click Save
6. Verify the name updated
7. Delete the entry — click trash, confirm
8. Verify it's gone
9. Close settings window, verify app doesn't quit
10. Reopen settings, verify persisted changes are reflected
11. Press Cmd+, again while settings is open — should not open a second window
12. Check the Imagery > Custom Imagery menu reflects changes made in settings

- [ ] **Step 3: Commit**

```bash
git add src/main.rs src/ui/settings_window.rs
git commit -m "Prevent duplicate settings windows"
```
