# Custom Imagery Layers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users define, persist, and reuse custom TMS imagery layers (name + URL template + zoom range) via a modal dialog opened from the Imagery menu.

**Architecture:** New `src/custom_imagery_store.rs` persists entries as JSON in the OS config dir. A new `src/ui/` module provides a minimal single-line `TextInput`, a reusable `Modal` shell, and a `CustomImageryDialog` composed of both. `main.rs` gains two new actions (`AddCustomImagery`, `AddSavedCustomImagery`), a `Custom Imagery` submenu prepended to the Imagery menu, and dialog lifecycle state on `MapViewer`. The existing `LayerRequest::Imagery` path is reused to actually add the layer to the map.

**Tech Stack:** Rust, GPUI (git HEAD from zed-industries/zed), serde_json, new `dirs` crate for OS config dir lookup.

Design reference: `docs/superpowers/specs/2026-04-14-custom-imagery-layers-design.md`.

---

## File Structure

**New:**

- `src/custom_imagery_store.rs` — entry type, load/save, in-memory singleton.
- `src/ui/mod.rs` — re-exports.
- `src/ui/text_input.rs` — minimal single-line text input entity.
- `src/ui/modal.rs` — reusable centered modal shell.
- `src/ui/custom_imagery_dialog.rs` — dialog composed of four text inputs + buttons.

**Modified:**

- `Cargo.toml` — add `dirs` dep.
- `src/lib.rs` — declare `custom_imagery_store` and `ui` modules.
- `src/main.rs` — new actions, menu wiring, dialog state on `MapViewer`, startup load.

---

## Task 1: Add `dirs` dependency

**Files:**

- Modify: `Cargo.toml`

- [ ] **Step 1:** Add `dirs = "5"` under `[dependencies]` in `Cargo.toml` (match the position alphabetically near other simple crates).

- [ ] **Step 2:** Run `cargo build` and confirm it compiles.

- [ ] **Step 3:** Commit.

```bash
git add Cargo.toml Cargo.lock
git commit -m "Add dirs dependency for config dir lookup"
```

---

## Task 2: `CustomImageryEntry` type + store module (TDD)

**Files:**

- Create: `src/custom_imagery_store.rs`
- Modify: `src/lib.rs` — add `pub mod custom_imagery_store;`

- [ ] **Step 1: Add module declaration** in `src/lib.rs`:

```rust
pub mod custom_imagery_store;
```

- [ ] **Step 2: Write the failing test file.** Create `src/custom_imagery_store.rs` with *only* the tests module initially:

```rust
//! Persistent storage for user-defined custom imagery layers.
//!
//! Entries are stored as a JSON array in `<config_dir>/osm-gpui/custom-imagery.json`.
//! Missing, unreadable, or malformed files are treated as empty (logged to stderr).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomImageryEntry {
    pub name: String,
    pub url_template: String,
    pub min_zoom: u32,
    pub max_zoom: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("osm-gpui-custom-imagery-tests")
            .join(format!("{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample() -> Vec<CustomImageryEntry> {
        vec![
            CustomImageryEntry {
                name: "Example".into(),
                url_template: "https://tile.example.com/{z}/{x}/{y}.png".into(),
                min_zoom: 0,
                max_zoom: 19,
            },
            CustomImageryEntry {
                name: "Other".into(),
                url_template: "https://other.example.com/{z}/{x}/{-y}.png".into(),
                min_zoom: 4,
                max_zoom: 18,
            },
        ]
    }

    #[test]
    fn round_trip() {
        let dir = tmp_dir("round-trip");
        let path = dir.join("custom-imagery.json");
        save_to(&path, &sample()).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, sample());
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tmp_dir("missing");
        let path = dir.join("custom-imagery.json");
        let loaded = load_from(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn corrupt_file_is_empty() {
        let dir = tmp_dir("corrupt");
        let path = dir.join("custom-imagery.json");
        fs::write(&path, b"not valid json {{").unwrap();
        let loaded = load_from(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn save_overwrites_previous_content() {
        let dir = tmp_dir("overwrite");
        let path = dir.join("custom-imagery.json");
        save_to(&path, &sample()).unwrap();
        save_to(&path, &[]).unwrap();
        let loaded = load_from(&path);
        assert!(loaded.is_empty());
    }
}
```

- [ ] **Step 3: Run tests — they fail with `load_from` / `save_to` undefined.**

Run: `cargo test --lib custom_imagery_store -- --nocapture`
Expected: compile errors — `load_from`/`save_to` not found.

- [ ] **Step 4: Implement `load_from` / `save_to` (path-injectable for tests):**

Append to `src/custom_imagery_store.rs` (above the `#[cfg(test)]` block):

```rust
/// Load entries from the given file path. Returns an empty vec on missing file,
/// unreadable file, or parse error (logged to stderr).
pub fn load_from(path: &Path) -> Vec<CustomImageryEntry> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!("custom_imagery_store: read {:?} failed: {}", path, e);
            return Vec::new();
        }
    };
    match serde_json::from_slice::<Vec<CustomImageryEntry>>(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("custom_imagery_store: parse {:?} failed: {}", path, e);
            Vec::new()
        }
    }
}

/// Atomically write entries to the given path. Writes to a sibling `.tmp` file
/// then renames into place.
pub fn save_to(path: &Path, entries: &[CustomImageryEntry]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(entries)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
```

- [ ] **Step 5: Run tests — they pass.**

Run: `cargo test --lib custom_imagery_store`
Expected: 4 passed.

- [ ] **Step 6: Add the default path + convenience wrappers.** Append to `src/custom_imagery_store.rs`:

```rust
/// Default on-disk location: `<config_dir>/osm-gpui/custom-imagery.json`.
/// Returns `None` if the OS has no conventional config dir (e.g., exotic platforms).
pub fn default_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("osm-gpui").join("custom-imagery.json"))
}

/// Load from the default path. Empty vec if unavailable.
pub fn load() -> Vec<CustomImageryEntry> {
    match default_path() {
        Some(p) => load_from(&p),
        None => Vec::new(),
    }
}

/// Save to the default path. Silently succeeds (logging only) when there is no config dir.
pub fn save(entries: &[CustomImageryEntry]) {
    let Some(p) = default_path() else {
        eprintln!("custom_imagery_store: no config dir, skipping save");
        return;
    };
    if let Err(e) = save_to(&p, entries) {
        eprintln!("custom_imagery_store: save {:?} failed: {}", p, e);
    }
}
```

- [ ] **Step 7: Run all tests** (make sure nothing else broke):

Run: `cargo test --lib`
Expected: all pass.

- [ ] **Step 8: Commit.**

```bash
git add src/lib.rs src/custom_imagery_store.rs
git commit -m "Add custom imagery store with JSON persistence"
```

---

## Task 3: URL template + zoom validation helper (TDD)

Lives in the dialog module but is factored out for testability. Create the dialog module file in this task *only* to host the helper and its tests; the UI parts land in Task 7.

**Files:**

- Create: `src/ui/mod.rs`, `src/ui/custom_imagery_dialog.rs`
- Modify: `src/lib.rs` — add `pub mod ui;`

- [ ] **Step 1:** Add `pub mod ui;` to `src/lib.rs` (below `pub mod tiles;`, alphabetical).

- [ ] **Step 2:** Create `src/ui/mod.rs`:

```rust
//! UI components shared across the app: text input, modal shell, dialogs.

pub mod custom_imagery_dialog;
pub mod modal;
pub mod text_input;
```

*Note:* `modal` and `text_input` are declared here but created in later tasks. Until those exist, `cargo check` will fail. That's expected — we land them in order.

Actually to avoid a broken intermediate build, only declare what exists. Revise `src/ui/mod.rs` to:

```rust
//! UI components shared across the app: text input, modal shell, dialogs.

pub mod custom_imagery_dialog;
```

We'll add the other `pub mod` lines in their respective tasks.

- [ ] **Step 3:** Create `src/ui/custom_imagery_dialog.rs` with the validator and tests (UI code comes later):

```rust
//! Modal dialog to add a user-defined custom imagery layer, plus the validation
//! helpers the dialog and its tests share.

use crate::custom_imagery_store::CustomImageryEntry;

#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    NameEmpty,
    TemplateEmpty,
    TemplateMissingPlaceholder,
    TemplateYAndMinusY,
    MinZoomInvalid,
    MaxZoomInvalid,
    MinZoomAboveMax,
}

/// Validate raw form fields (already trimmed by the caller) and return a
/// normalised `CustomImageryEntry` on success.
pub fn validate(
    name: &str,
    url_template: &str,
    min_zoom_raw: &str,
    max_zoom_raw: &str,
) -> Result<CustomImageryEntry, ValidationError> {
    if name.trim().is_empty() {
        return Err(ValidationError::NameEmpty);
    }
    let template = url_template.trim();
    if template.is_empty() {
        return Err(ValidationError::TemplateEmpty);
    }
    let has_z = template.contains("{z}");
    let has_x = template.contains("{x}");
    let has_y = template.contains("{y}");
    let has_minus_y = template.contains("{-y}");
    if !has_z || !has_x || (!has_y && !has_minus_y) {
        return Err(ValidationError::TemplateMissingPlaceholder);
    }
    if has_y && has_minus_y {
        return Err(ValidationError::TemplateYAndMinusY);
    }
    let min_zoom = parse_zoom(min_zoom_raw, 0).map_err(|_| ValidationError::MinZoomInvalid)?;
    let max_zoom = parse_zoom(max_zoom_raw, 19).map_err(|_| ValidationError::MaxZoomInvalid)?;
    if min_zoom > max_zoom {
        return Err(ValidationError::MinZoomAboveMax);
    }
    Ok(CustomImageryEntry {
        name: name.trim().to_string(),
        url_template: template.to_string(),
        min_zoom,
        max_zoom,
    })
}

fn parse_zoom(raw: &str, default_if_blank: u32) -> Result<u32, ()> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(default_if_blank);
    }
    let v: u32 = s.parse().map_err(|_| ())?;
    if v > 24 {
        return Err(());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TMPL: &str = "https://tile.example.com/{z}/{x}/{y}.png";

    #[test]
    fn happy_path_defaults() {
        let e = validate("Example", TMPL, "", "").unwrap();
        assert_eq!(e.name, "Example");
        assert_eq!(e.url_template, TMPL);
        assert_eq!(e.min_zoom, 0);
        assert_eq!(e.max_zoom, 19);
    }

    #[test]
    fn happy_path_minus_y() {
        let e = validate(
            "Foo",
            "https://tile.example.com/{z}/{x}/{-y}.png",
            "4",
            "18",
        )
        .unwrap();
        assert_eq!(e.min_zoom, 4);
        assert_eq!(e.max_zoom, 18);
    }

    #[test]
    fn name_must_be_nonempty() {
        assert_eq!(validate("  ", TMPL, "", ""), Err(ValidationError::NameEmpty));
    }

    #[test]
    fn template_required() {
        assert_eq!(
            validate("Example", "  ", "", ""),
            Err(ValidationError::TemplateEmpty)
        );
    }

    #[test]
    fn template_missing_z_x_y() {
        assert_eq!(
            validate("Example", "https://example.com/a/b/c.png", "", ""),
            Err(ValidationError::TemplateMissingPlaceholder)
        );
    }

    #[test]
    fn template_cannot_contain_both_y_variants() {
        assert_eq!(
            validate(
                "Example",
                "https://example.com/{z}/{x}/{y}/{-y}.png",
                "",
                ""
            ),
            Err(ValidationError::TemplateYAndMinusY)
        );
    }

    #[test]
    fn min_above_max_rejected() {
        assert_eq!(
            validate("Example", TMPL, "15", "10"),
            Err(ValidationError::MinZoomAboveMax)
        );
    }

    #[test]
    fn out_of_range_zoom_rejected() {
        assert_eq!(
            validate("Example", TMPL, "25", ""),
            Err(ValidationError::MinZoomInvalid)
        );
        assert_eq!(
            validate("Example", TMPL, "", "99"),
            Err(ValidationError::MaxZoomInvalid)
        );
    }

    #[test]
    fn non_numeric_zoom_rejected() {
        assert_eq!(
            validate("Example", TMPL, "abc", ""),
            Err(ValidationError::MinZoomInvalid)
        );
    }
}
```

- [ ] **Step 4:** Run tests.

Run: `cargo test --lib custom_imagery_dialog`
Expected: all 9 tests pass.

- [ ] **Step 5:** Commit.

```bash
git add src/lib.rs src/ui/mod.rs src/ui/custom_imagery_dialog.rs
git commit -m "Add validator for custom imagery form fields"
```

---

## Task 4: `TextInput` entity — typing + cursor movement

The UI tasks (4–7) are not strictly TDD — GPUI views are painful to unit test without a headless harness. We build incrementally and verify by placing a test instance on `MapViewer` at the end of Task 7.

**Files:**

- Create: `src/ui/text_input.rs`
- Modify: `src/ui/mod.rs` — add `pub mod text_input;`

- [ ] **Step 1:** Update `src/ui/mod.rs`:

```rust
//! UI components shared across the app: text input, modal shell, dialogs.

pub mod custom_imagery_dialog;
pub mod text_input;
```

- [ ] **Step 2:** Create `src/ui/text_input.rs` with typing + cursor movement support only. Selection + clipboard land in Task 5.

```rust
//! Minimal single-line text input entity.
//!
//! State:
//! - `content`: current text
//! - `cursor`: byte offset of the insertion caret into `content`
//! - `selection_anchor`: byte offset of the other end of a selection, if any
//!   (introduced in the next task; kept `None` here)
//! - `focus_handle`: GPUI focus handle used to receive key events
//!
//! The widget handles printable characters, Backspace, Delete, Left/Right
//! arrows, and Home/End. Selection, clipboard, and mouse click-to-focus are
//! added in Task 5.

use gpui::{
    div, prelude::*, px, rgb, Context, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    MouseDownEvent, SharedString, Window,
};

pub struct TextInput {
    content: String,
    cursor: usize,
    selection_anchor: Option<usize>,
    placeholder: SharedString,
    focus_handle: FocusHandle,
}

impl TextInput {
    pub fn new(cx: &mut Context<Self>, placeholder: impl Into<SharedString>) -> Self {
        Self {
            content: String::new(),
            cursor: 0,
            selection_anchor: None,
            placeholder: placeholder.into(),
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn set_content(&mut self, value: impl Into<String>, cx: &mut Context<Self>) {
        self.content = value.into();
        self.cursor = self.content.len();
        self.selection_anchor = None;
        cx.notify();
    }

    pub fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        match key {
            "backspace" => self.backspace(),
            "delete" => self.delete_forward(),
            "left" => self.move_left(),
            "right" => self.move_right(),
            "home" => self.cursor = 0,
            "end" => self.cursor = self.content.len(),
            _ => {
                if let Some(s) = printable_from(ev) {
                    self.insert(&s);
                }
            }
        }
        cx.notify();
    }

    fn insert(&mut self, s: &str) {
        self.content.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = prev_char_boundary(&self.content, self.cursor);
        self.content.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    fn delete_forward(&mut self) {
        if self.cursor == self.content.len() {
            return;
        }
        let next = next_char_boundary(&self.content, self.cursor);
        self.content.replace_range(self.cursor..next, "");
    }

    fn move_left(&mut self) {
        self.cursor = prev_char_boundary(&self.content, self.cursor);
    }

    fn move_right(&mut self) {
        self.cursor = next_char_boundary(&self.content, self.cursor);
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(cx.window_context());
        let bg = if focused { rgb(0x1f2937) } else { rgb(0x111827) };
        let border = if focused { rgb(0x60a5fa) } else { rgb(0x374151) };
        let text_color = rgb(0xffffff);

        let content_to_show: SharedString = if self.content.is_empty() {
            self.placeholder.clone()
        } else {
            self.content.clone().into()
        };
        let text_col = if self.content.is_empty() { rgb(0x6b7280) } else { text_color };

        div()
            .track_focus(&self.focus_handle)
            .key_context("TextInput")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle);
                    cx.notify();
                }),
            )
            .w_full()
            .h(px(28.0))
            .px_2()
            .py_1()
            .bg(bg)
            .border_1()
            .border_color(border)
            .rounded_sm()
            .text_color(text_col)
            .text_sm()
            .child(content_to_show)
    }
}

fn printable_from(ev: &KeyDownEvent) -> Option<String> {
    // Filter out modifier-prefixed keystrokes (those are handled as shortcuts).
    let m = &ev.keystroke.modifiers;
    if m.control || m.platform || m.alt {
        return None;
    }
    let key = &ev.keystroke.key;
    // gpui surfaces printable input either on `keystroke.ime_key` or as the raw
    // `key`. Prefer `ime_key` when present (preserves shift-produced capitals
    // and punctuation).
    if let Some(ime) = &ev.keystroke.ime_key {
        if !ime.is_empty() {
            return Some(ime.clone());
        }
    }
    if key.chars().count() == 1 && !key.starts_with('f') {
        return Some(key.clone());
    }
    None
}

fn prev_char_boundary(s: &str, i: usize) -> usize {
    if i == 0 {
        return 0;
    }
    let mut j = i - 1;
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

fn next_char_boundary(s: &str, i: usize) -> usize {
    let n = s.len();
    if i >= n {
        return n;
    }
    let mut j = i + 1;
    while j < n && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}
```

- [ ] **Step 3:** Run `cargo build` to verify it compiles. If `key_context`, `track_focus`, or `is_focused` signatures differ from current zed gpui, fix them — GPUI's API moves; trust the compiler error over this snippet.

Expected: compiles clean (no warnings about unused items is fine; `selection_anchor` is unused until Task 5 — silence with `#[allow(dead_code)]` if needed).

- [ ] **Step 4:** Commit.

```bash
git add src/ui/mod.rs src/ui/text_input.rs
git commit -m "Add minimal single-line TextInput entity"
```

---

## Task 5: Selection + clipboard support on `TextInput`

**Files:**

- Modify: `src/ui/text_input.rs`

- [ ] **Step 1:** In `on_key_down`, branch on modifiers before the plain-key match. Replace the `on_key_down` body with:

```rust
fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
    let key = ev.keystroke.key.as_str();
    let m = &ev.keystroke.modifiers;
    let cmd = m.platform || m.control; // Cmd on macOS, Ctrl elsewhere

    if cmd {
        match key {
            "a" => {
                self.selection_anchor = Some(0);
                self.cursor = self.content.len();
                cx.notify();
                return;
            }
            "c" => {
                if let Some(text) = self.selected_text() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
                return;
            }
            "x" => {
                if let Some(text) = self.selected_text() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    self.delete_selection();
                    cx.notify();
                }
                return;
            }
            "v" => {
                if let Some(item) = cx.read_from_clipboard() {
                    if let Some(text) = item.text() {
                        let clean: String = text.replace(['\r', '\n'], "");
                        self.replace_selection_with(&clean);
                        cx.notify();
                    }
                }
                return;
            }
            _ => {}
        }
    }

    match key {
        "backspace" => {
            if self.has_selection() {
                self.delete_selection();
            } else {
                self.backspace();
            }
        }
        "delete" => {
            if self.has_selection() {
                self.delete_selection();
            } else {
                self.delete_forward();
            }
        }
        "left" => self.move_horizontal(-1, m.shift),
        "right" => self.move_horizontal(1, m.shift),
        "home" => self.move_to(0, m.shift),
        "end" => self.move_to(self.content.len(), m.shift),
        _ => {
            if let Some(s) = printable_from(ev) {
                self.replace_selection_with(&s);
            }
        }
    }
    cx.notify();
}
```

- [ ] **Step 2:** Add selection helpers. Inside `impl TextInput`, add:

```rust
fn has_selection(&self) -> bool {
    matches!(self.selection_anchor, Some(a) if a != self.cursor)
}

fn selection_range(&self) -> Option<(usize, usize)> {
    let a = self.selection_anchor?;
    if a == self.cursor {
        return None;
    }
    Some((a.min(self.cursor), a.max(self.cursor)))
}

fn selected_text(&self) -> Option<String> {
    let (lo, hi) = self.selection_range()?;
    Some(self.content[lo..hi].to_string())
}

fn delete_selection(&mut self) {
    if let Some((lo, hi)) = self.selection_range() {
        self.content.replace_range(lo..hi, "");
        self.cursor = lo;
        self.selection_anchor = None;
    }
}

fn replace_selection_with(&mut self, s: &str) {
    if self.has_selection() {
        self.delete_selection();
    }
    self.insert(s);
    self.selection_anchor = None;
}

fn move_horizontal(&mut self, dir: i32, extend: bool) {
    let next = if dir < 0 {
        prev_char_boundary(&self.content, self.cursor)
    } else {
        next_char_boundary(&self.content, self.cursor)
    };
    if extend {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
    } else {
        self.selection_anchor = None;
    }
    self.cursor = next;
}

fn move_to(&mut self, target: usize, extend: bool) {
    if extend {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
    } else {
        self.selection_anchor = None;
    }
    self.cursor = target.min(self.content.len());
}
```

- [ ] **Step 3:** Run `cargo build` and fix any GPUI clipboard API mismatches (method names `write_to_clipboard` / `read_from_clipboard`, type `ClipboardItem::new_string`, and `.text()` returning `Option<String>` or `Option<&str>` — adapt to current signatures).

- [ ] **Step 4:** Commit.

```bash
git add src/ui/text_input.rs
git commit -m "Add selection and clipboard support to TextInput"
```

*Note on selection rendering:* drawing a highlighted selection range cleanly in GPUI requires text measurement that's non-trivial. For this issue we accept that the selection model exists internally (so cut/copy/paste work correctly) but the visible highlight is a stretch goal. If rendering the highlight proves quick using gpui text layout APIs, add it; otherwise document this in the module doc-comment and move on.

---

## Task 6: Reusable `Modal` shell

**Files:**

- Create: `src/ui/modal.rs`
- Modify: `src/ui/mod.rs` — add `pub mod modal;`

- [ ] **Step 1:** Update `src/ui/mod.rs`:

```rust
//! UI components shared across the app: text input, modal shell, dialogs.

pub mod custom_imagery_dialog;
pub mod modal;
pub mod text_input;
```

- [ ] **Step 2:** Create `src/ui/modal.rs`. `Modal` is a *rendering helper*, not an entity — it's cheaper and simpler as a plain builder that callers compose inside their own `Render` impl.

```rust
//! Reusable modal dialog chrome: backdrop, centered frame, title, body, footer.
//!
//! `Modal` is a builder used inside a caller's `Render` impl rather than its own
//! entity. The caller owns the body/footer state and decides when to show the
//! modal (typically by keeping `Option<Entity<MyDialog>>` and rendering the
//! dialog's entity only when `Some`).

use gpui::{
    div, prelude::*, px, rgb, AnyElement, IntoElement, ParentElement, SharedString, Styled,
    WindowContext,
};

pub struct Modal {
    title: SharedString,
    body: AnyElement,
    footer: AnyElement,
}

impl Modal {
    pub fn new(
        title: impl Into<SharedString>,
        body: impl IntoElement,
        footer: impl IntoElement,
    ) -> Self {
        Self {
            title: title.into(),
            body: body.into_any_element(),
            footer: footer.into_any_element(),
        }
    }
}

impl IntoElement for Modal {
    type Element = gpui::Div;

    fn into_element(self) -> Self::Element {
        let title = self.title;
        let body = self.body;
        let footer = self.footer;

        let frame = div()
            .w(px(420.0))
            .bg(rgb(0x0f172a))
            .border_1()
            .border_color(rgb(0x374151))
            .rounded_md()
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(0x374151))
                    .text_color(rgb(0xffffff))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(title),
            )
            .child(div().p_4().flex().flex_col().gap_3().child(body))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(0x374151))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(footer),
            );

        div()
            .absolute()
            .inset_0()
            .bg(rgba_u32(0x00000099))
            .flex()
            .justify_center()
            .items_center()
            .child(frame)
    }
}

fn rgba_u32(v: u32) -> gpui::Rgba {
    let r = ((v >> 24) & 0xff) as f32 / 255.0;
    let g = ((v >> 16) & 0xff) as f32 / 255.0;
    let b = ((v >> 8) & 0xff) as f32 / 255.0;
    let a = (v & 0xff) as f32 / 255.0;
    gpui::Rgba { r, g, b, a }
}
```

*Note:* GPUI's exact `IntoElement` requirements may force this to be a function that returns a `Div` rather than a struct implementing `IntoElement`. If the struct form fights the compiler, collapse `Modal` into a free function `pub fn modal(title, body, footer) -> impl IntoElement`. The interface (title + body + footer) stays the same.

Esc handling and focus cycling: these are wired up by the dialog entity (Task 7) since they need access to the caller's cancel callback and focus handles. `Modal` is purely chrome.

- [ ] **Step 3:** Run `cargo build` and adjust for current GPUI APIs (imports, color helpers, `.shadow_lg()` availability).

- [ ] **Step 4:** Commit.

```bash
git add src/ui/mod.rs src/ui/modal.rs
git commit -m "Add reusable Modal rendering helper"
```

---

## Task 7: `CustomImageryDialog` entity

**Files:**

- Modify: `src/ui/custom_imagery_dialog.rs` (append UI code to the validator already there)

- [ ] **Step 1:** At the top of `src/ui/custom_imagery_dialog.rs`, add imports:

```rust
use crate::ui::modal::Modal;
use crate::ui::text_input::TextInput;
use gpui::{
    div, prelude::*, px, rgb, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, MouseDownEvent, SharedString, Window,
};
```

- [ ] **Step 2:** Append the dialog entity and its events:

```rust
pub enum DialogEvent {
    Submitted(CustomImageryEntry),
    Cancelled,
}

pub struct CustomImageryDialog {
    name: Entity<TextInput>,
    url_template: Entity<TextInput>,
    min_zoom: Entity<TextInput>,
    max_zoom: Entity<TextInput>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for CustomImageryDialog {}

impl CustomImageryDialog {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextInput::new(cx, "My imagery"));
        let url_template = cx.new(|cx| TextInput::new(cx, "https://…/{z}/{x}/{y}.png"));
        let min_zoom = cx.new(|cx| TextInput::new(cx, "0"));
        let max_zoom = cx.new(|cx| TextInput::new(cx, "19"));
        let focus_handle = cx.focus_handle();
        // Focus the name field on open.
        cx.focus(&name.read(cx).focus_handle(cx));
        Self {
            name,
            url_template,
            min_zoom,
            max_zoom,
            error: None,
            focus_handle,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).content().to_string();
        let tmpl = self.url_template.read(cx).content().to_string();
        let minz = self.min_zoom.read(cx).content().to_string();
        let maxz = self.max_zoom.read(cx).content().to_string();
        match validate(&name, &tmpl, &minz, &maxz) {
            Ok(entry) => {
                self.error = None;
                cx.emit(DialogEvent::Submitted(entry));
            }
            Err(e) => {
                self.error = Some(error_message(&e).into());
                cx.notify();
            }
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        let m = &ev.keystroke.modifiers;
        match key {
            "escape" => self.cancel(cx),
            "enter" => self.submit(cx),
            "tab" => {
                // Tab cycling: let GPUI's focus traversal handle it if available;
                // otherwise cycle through an explicit list of focus handles.
                let order = [
                    self.name.read(cx).focus_handle(cx),
                    self.url_template.read(cx).focus_handle(cx),
                    self.min_zoom.read(cx).focus_handle(cx),
                    self.max_zoom.read(cx).focus_handle(cx),
                ];
                cycle_focus(&order, m.shift, cx);
            }
            _ => {}
        }
    }
}

fn error_message(e: &ValidationError) -> &'static str {
    match e {
        ValidationError::NameEmpty => "Name is required.",
        ValidationError::TemplateEmpty => "URL template is required.",
        ValidationError::TemplateMissingPlaceholder => {
            "URL template must contain {z}, {x}, and {y} (or {-y})."
        }
        ValidationError::TemplateYAndMinusY => {
            "URL template must use {y} or {-y}, not both."
        }
        ValidationError::MinZoomInvalid => "Min zoom must be a whole number from 0 to 24.",
        ValidationError::MaxZoomInvalid => "Max zoom must be a whole number from 0 to 24.",
        ValidationError::MinZoomAboveMax => "Min zoom must be ≤ max zoom.",
    }
}

fn cycle_focus(order: &[FocusHandle], reverse: bool, cx: &mut gpui::WindowContext) {
    let focused_idx = order
        .iter()
        .position(|h| h.is_focused(cx))
        .unwrap_or(0);
    let next = if reverse {
        (focused_idx + order.len() - 1) % order.len()
    } else {
        (focused_idx + 1) % order.len()
    };
    cx.focus(&order[next]);
}

impl Focusable for CustomImageryDialog {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CustomImageryDialog {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(field_row("Name", self.name.clone()))
            .child(field_row("URL template", self.url_template.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .child(div().flex_1().child(field_row("Min zoom", self.min_zoom.clone())))
                    .child(div().flex_1().child(field_row("Max zoom", self.max_zoom.clone()))),
            )
            .children(self.error.clone().map(|msg| {
                div()
                    .text_color(rgb(0xf87171))
                    .text_sm()
                    .child(msg)
            }));

        let add = cx.listener(|this, _: &MouseDownEvent, _w, cx| this.submit(cx));
        let cancel = cx.listener(|this, _: &MouseDownEvent, _w, cx| this.cancel(cx));
        let footer = div()
            .flex()
            .flex_row()
            .gap_2()
            .child(button("Cancel").on_mouse_down(MouseButton::Left, cancel))
            .child(button_primary("Add").on_mouse_down(MouseButton::Left, add));

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .absolute()
            .inset_0()
            .child(Modal::new("Custom Imagery", body, footer))
    }
}

fn field_row(label: &'static str, input: Entity<TextInput>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_color(rgb(0x9ca3af)).text_xs().child(label))
        .child(input)
}

fn button(label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(("btn", label))
        .px_3()
        .py_1()
        .bg(rgb(0x1f2937))
        .border_1()
        .border_color(rgb(0x374151))
        .rounded_sm()
        .text_color(rgb(0xffffff))
        .text_sm()
        .cursor_pointer()
        .child(label)
}

fn button_primary(label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(("btn-primary", label))
        .px_3()
        .py_1()
        .bg(rgb(0x2563eb))
        .border_1()
        .border_color(rgb(0x1d4ed8))
        .rounded_sm()
        .text_color(rgb(0xffffff))
        .text_sm()
        .cursor_pointer()
        .child(label)
}
```

- [ ] **Step 3:** `cargo build`. Expect API mismatches — adapt:
  - `cx.focus(...)` vs `window.focus(...)` — current zed HEAD uses `Window::focus`. If `cx.focus` doesn't exist, thread `Window` through.
  - `cx.new(...)` vs `cx.new_model(...)` — recent gpui dropped the `_model` suffix but check your tree.
  - `cx.emit(...)` from a `Context<Self>` — correct pattern.
  - `WindowContext` may no longer exist; use `(Window, App)` pair per zed HEAD.

Expected: compiles after adaptation.

- [ ] **Step 4:** Commit.

```bash
git add src/ui/custom_imagery_dialog.rs
git commit -m "Add custom imagery dialog UI"
```

---

## Task 8: Wire the in-memory store + startup load in `main.rs`

**Files:**

- Modify: `src/main.rs`

- [ ] **Step 1:** Add the import near the top of `src/main.rs` (alongside the existing `imagery` import):

```rust
use osm_gpui::custom_imagery_store::{self, CustomImageryEntry};
```

- [ ] **Step 2:** Add the singleton near the existing `IMAGERY_INDEX` declaration (~line 55):

```rust
/// User-defined custom imagery entries, loaded from disk at startup and kept
/// in sync with the on-disk JSON file.
static CUSTOM_IMAGERY_STORE: OnceLock<Arc<Mutex<Vec<CustomImageryEntry>>>> = OnceLock::new();
```

- [ ] **Step 3:** In `main()` or wherever `IMAGERY_INDEX.set(...)` happens (~line 1401), initialise the custom-imagery store synchronously before `rebuild_menus` is called for the first time (~line 1485):

```rust
let loaded = custom_imagery_store::load();
let _ = CUSTOM_IMAGERY_STORE.set(Arc::new(Mutex::new(loaded)));
```

- [ ] **Step 4:** Add a helper beside the other queue helpers:

```rust
fn custom_imagery_snapshot() -> Vec<CustomImageryEntry> {
    CUSTOM_IMAGERY_STORE
        .get()
        .and_then(|s| s.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

fn append_custom_imagery(entry: CustomImageryEntry) {
    let Some(store) = CUSTOM_IMAGERY_STORE.get() else { return };
    let snapshot = {
        let Ok(mut g) = store.lock() else { return };
        g.push(entry);
        g.clone()
    };
    custom_imagery_store::save(&snapshot);
}
```

- [ ] **Step 5:** `cargo build`. Expected: compiles (no callers yet).

- [ ] **Step 6:** Commit.

```bash
git add src/main.rs
git commit -m "Load custom imagery store at startup"
```

---

## Task 9: `AddCustomImagery` action + dialog lifecycle

**Files:**

- Modify: `src/main.rs`

- [ ] **Step 1:** Add the action. Near the existing `actions!(osm_gpui, [OpenOsmFile, Quit, ...])` line (~line 22), append `AddCustomImagery`:

```rust
actions!(osm_gpui, [OpenOsmFile, Quit, AddOsmCarto, AddCoordinateGrid, DownloadFromOsm, ToggleDebugOverlay, AddCustomImagery]);
```

- [ ] **Step 2:** Add an open-dialog queue alongside the other `OnceLock` queues (~line 73):

```rust
static OPEN_CUSTOM_IMAGERY_DIALOG: OnceLock<Arc<Mutex<Vec<()>>>> = OnceLock::new();
```

- [ ] **Step 3:** Add the handler (near `add_osm_carto` / `add_coordinate_grid`, ~line 1646):

```rust
fn open_custom_imagery_dialog(_: &AddCustomImagery, cx: &mut App) {
    if let Some(queue) = OPEN_CUSTOM_IMAGERY_DIALOG.get() {
        if let Ok(mut g) = queue.lock() {
            g.push(());
        }
    }
    cx.refresh_windows();
}
```

- [ ] **Step 4:** In `main()` where other `OnceLock::set` / `cx.on_action` calls happen (~line 1480), initialise the queue and register the handler:

```rust
let _ = OPEN_CUSTOM_IMAGERY_DIALOG.set(Arc::new(Mutex::new(Vec::new())));
cx.on_action(open_custom_imagery_dialog);
```

- [ ] **Step 5:** Add dialog state to `MapViewer` (struct at ~line 214) and in `MapViewer::new` (~line 234):

```rust
// In the struct:
custom_imagery_dialog: Option<gpui::Entity<osm_gpui::ui::custom_imagery_dialog::CustomImageryDialog>>,

// In new():
custom_imagery_dialog: None,
```

- [ ] **Step 6:** In `MapViewer`'s per-frame hook (the method that runs each render — look for where `check_for_layer_requests` or the download queue is drained, ~line 500–750), add a block that opens the dialog if the queue has an entry and one isn't already open:

```rust
if let Some(queue) = OPEN_CUSTOM_IMAGERY_DIALOG.get() {
    if let Ok(mut g) = queue.lock() {
        if !g.is_empty() && self.custom_imagery_dialog.is_none() {
            g.clear();
            let dialog = cx.new(|cx| {
                use osm_gpui::ui::custom_imagery_dialog::{CustomImageryDialog, DialogEvent};
                CustomImageryDialog::new(cx)
            });
            // Subscribe to close/submit events.
            cx.subscribe(&dialog, |this, _entity, event, cx| {
                use osm_gpui::ui::custom_imagery_dialog::DialogEvent;
                match event {
                    DialogEvent::Cancelled => {
                        this.custom_imagery_dialog = None;
                        cx.notify();
                    }
                    DialogEvent::Submitted(entry) => {
                        append_custom_imagery(entry.clone());
                        if let Some(requests) = LAYER_REQUESTS.get() {
                            if let Ok(mut q) = requests.lock() {
                                q.push(LayerRequest::Imagery {
                                    name: entry.name.clone(),
                                    url_template: entry.url_template.clone(),
                                    min_zoom: Some(entry.min_zoom),
                                    max_zoom: Some(entry.max_zoom),
                                });
                            }
                        }
                        this.custom_imagery_dialog = None;
                        this.last_menu_center = None; // force menu rebuild
                        cx.notify();
                    }
                }
            })
            .detach();
            self.custom_imagery_dialog = Some(dialog);
            cx.notify();
        }
    }
}
```

- [ ] **Step 7:** In `MapViewer::render` (wherever the root `div` is composed), add the dialog as an overlay *after* the map content so it paints above:

```rust
.children(self.custom_imagery_dialog.clone().map(|d| d))
```

(Where `d: Entity<CustomImageryDialog>` — GPUI renders entities directly via `.children`. If that doesn't compile, wrap with `div().absolute().inset_0().child(d)` to ensure it overlays.)

- [ ] **Step 8:** `cargo build`. Adapt signatures as needed (e.g., `cx.subscribe` may require a closure shape specific to current gpui).

- [ ] **Step 9:** Commit.

```bash
git add src/main.rs
git commit -m "Wire AddCustomImagery action and dialog lifecycle"
```

---

## Task 10: `AddSavedCustomImagery` action + Custom Imagery submenu

**Files:**

- Modify: `src/main.rs`

- [ ] **Step 1:** Add a new parameterised action, matching the existing `AddImageryLayer` pattern (~line 28):

```rust
/// Action for adding a previously-saved custom imagery layer by index.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
struct AddSavedCustomImagery {
    index: usize,
}
```

- [ ] **Step 2:** Add its handler near `add_imagery_layer` (~line 1613):

```rust
fn add_saved_custom_imagery(action: &AddSavedCustomImagery, cx: &mut App) {
    let entries = custom_imagery_snapshot();
    let Some(entry) = entries.get(action.index).cloned() else {
        eprintln!("add_saved_custom_imagery: stale index {}", action.index);
        return;
    };
    if let Some(requests) = LAYER_REQUESTS.get() {
        if let Ok(mut q) = requests.lock() {
            q.push(LayerRequest::Imagery {
                name: entry.name,
                url_template: entry.url_template,
                min_zoom: Some(entry.min_zoom),
                max_zoom: Some(entry.max_zoom),
            });
        }
    }
    cx.refresh_windows();
}
```

- [ ] **Step 3:** Register the handler in `main()` (beside `cx.on_action(add_imagery_layer);`, ~line 1480):

```rust
cx.on_action(add_saved_custom_imagery);
```

- [ ] **Step 4:** In `rebuild_menus` (~line 1658), prepend the `Custom Imagery` submenu to `imagery_items`. Replace the initial `let mut imagery_items: Vec<MenuItem> = vec![ ... OsmCarto, separator, CoordinateGrid ]` block with:

```rust
let custom = custom_imagery_snapshot();
let mut custom_items: Vec<MenuItem> = vec![
    MenuItem::action("Add…", AddCustomImagery),
];
if !custom.is_empty() {
    custom_items.push(MenuItem::separator());
    for (idx, entry) in custom.iter().enumerate() {
        custom_items.push(MenuItem::action(
            entry.name.clone(),
            AddSavedCustomImagery { index: idx },
        ));
    }
}

let mut imagery_items: Vec<MenuItem> = vec![
    MenuItem::submenu(Menu {
        name: "Custom Imagery".into(),
        items: custom_items,
        disabled: false,
    }),
    MenuItem::separator(),
    MenuItem::action("OpenStreetMap Carto", AddOsmCarto),
    MenuItem::separator(),
    MenuItem::action("Coordinate Grid", AddCoordinateGrid),
];
```

(If GPUI's `MenuItem::submenu` differs, e.g. takes a `(name, Vec<MenuItem>)` pair, adjust — the existing codebase uses `MenuItem::os_submenu` for OS-provided submenus so there's precedent for submenu support.)

- [ ] **Step 5:** `cargo build`.

- [ ] **Step 6:** Commit.

```bash
git add src/main.rs
git commit -m "Add Custom Imagery submenu with saved entries"
```

---

## Task 11: Menu refresh after saving a new entry

The submit path in Task 9 already sets `self.last_menu_center = None` to force `maybe_rebuild_imagery_menu` to rebuild on the next frame. Verify this works in practice; if the center-moved check short-circuits before the `None` check, adjust the short-circuit.

**Files:**

- Modify: `src/main.rs` (if needed)

- [ ] **Step 1:** Re-read `maybe_rebuild_imagery_menu` (~line 261). Confirm that `last_menu_center = None` triggers `center_moved = true` via the first match arm. It does today — good, nothing to change.

- [ ] **Step 2:** No commit for this task unless a fix was actually needed.

---

## Task 12: Build, test, manual verification

- [ ] **Step 1:** Full build in release mode (per user preference — debug builds are ~4× slower):

Run: `cargo build --release`
Expected: clean build.

- [ ] **Step 2:** Run all tests:

Run: `cargo test --lib`
Expected: all pass, including the new `custom_imagery_store` and `custom_imagery_dialog` tests.

- [ ] **Step 3:** Manual smoke test (screenshots optional):

1. Launch `cargo run --release`.
2. Open **Imagery → Custom Imagery → Add…** — dialog appears; Name field focused.
3. Click **Add** with all fields blank → red error: "Name is required." Dialog stays open.
4. Type `Test`. Click **URL template** field, paste `https://tile.openstreetmap.org/{z}/{x}/{y}.png` (Cmd/Ctrl+V).
5. Leave zoom fields blank. Click **Add** → dialog closes, layer appears on map, `Test` appears under **Imagery → Custom Imagery**.
6. Click **Imagery → Custom Imagery → Test** → a second copy of the layer is added (no dialog).
7. Quit and relaunch. Confirm `Test` still appears in the submenu and still works.
8. Open `~/Library/Application Support/osm-gpui/custom-imagery.json` (macOS path) — contents match what was entered.
9. Corrupt the file by hand (replace contents with `garbage`). Relaunch — app starts with no saved entries; stderr shows a parse-error log line.

- [ ] **Step 4:** If everything passes, push and open a PR referencing issue #36.

```bash
git push -u origin <branch>
gh pr create --title "Custom imagery layers (#36)" --body "$(cat <<'EOF'
## Summary
- Adds a `Custom Imagery` submenu to the Imagery menu with an `Add…` item that opens a modal dialog.
- Dialog prompts for Name, URL template, Min zoom, Max zoom; validates placeholders and zoom range.
- Saved entries persist to `<config dir>/osm-gpui/custom-imagery.json` and appear as clickable submenu items across restarts.
- Introduces a minimal in-app `TextInput` + reusable `Modal` shell under `src/ui/`.

Closes #36.

## Test plan
- [x] `cargo test --lib` passes, including new store and validator tests.
- [x] Add a custom imagery layer; it renders on the map.
- [x] Relaunch; saved entry persists; click adds the layer.
- [x] Corrupt JSON on disk → app logs and starts with empty list.
EOF
)"
```

---

## Self-review notes

- **Spec coverage:** User flow (Task 9, 10), validation (Task 3), persistence (Task 2, 8), dialog UI (Task 4–7), menu integration (Task 10), file touch list (all tasks), testing (Task 2, 3, 12). All sections covered.
- **Placeholder scan:** No TBDs. GPUI API caveats are explicit and give the engineer enough to adapt.
- **Type consistency:** `CustomImageryEntry` defined in Task 2 with fields `name`, `url_template`, `min_zoom: u32`, `max_zoom: u32`; same names used in Tasks 3, 9, 10. `DialogEvent::Submitted(CustomImageryEntry)` / `DialogEvent::Cancelled` consistent between Task 7 (emitter) and Task 9 (subscriber). `MapViewer` field name `custom_imagery_dialog` consistent across Tasks 9, 11. `CUSTOM_IMAGERY_STORE` and helpers `custom_imagery_snapshot` / `append_custom_imagery` used as declared in Task 8 from Tasks 9 and 10.
