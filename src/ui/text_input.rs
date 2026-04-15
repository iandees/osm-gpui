//! Minimal single-line text input entity.

use gpui::{
    div, prelude::*, px, rgb, App, ClipboardItem, Context, FocusHandle, Focusable, KeyDownEvent,
    MouseButton, MouseDownEvent, SharedString, Window,
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
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    return;
                }
                "x" => {
                    if let Some(text) = self.selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
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
}

impl Focusable for TextInput {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let bg = if focused { rgb(0x1f2937) } else { rgb(0x111827) };
        let border = if focused { rgb(0x60a5fa) } else { rgb(0x374151) };

        let content_to_show: SharedString = if self.content.is_empty() {
            self.placeholder.clone()
        } else {
            self.content.clone().into()
        };
        let text_col = if self.content.is_empty() {
            rgb(0x6b7280)
        } else {
            rgb(0xffffff)
        };

        div()
            .track_focus(&self.focus_handle)
            .key_context("TextInput")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseDownEvent, window, _cx| {
                    this.focus_handle.focus(window, _cx);
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
    let m = &ev.keystroke.modifiers;
    if m.control || m.platform || m.alt {
        return None;
    }
    // Prefer key_char (preserves shift-produced chars).
    if let Some(ch) = &ev.keystroke.key_char {
        if !ch.is_empty() {
            return Some(ch.clone());
        }
    }
    let key = &ev.keystroke.key;
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
