//! Modal dialog offered at startup when the previous run left autosaved
//! layers behind (it crashed, was force-quit, or lost power). An in-window
//! modal like `quit_confirm_dialog.rs`.
//!
//! Restore is the default (Enter). Escape is deliberately NOT "discard": it
//! means "ask me next time" and leaves the files where they are. Discarding
//! has to be chosen with its own button.

use gpui::{
    div, prelude::*, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    v_flex, ActiveTheme as _,
};

use crate::ui::modal;
use crate::ui::style::muted_text_size;
use crate::{tr, tr_count};

pub enum DialogEvent {
    /// Load every listed layer back, with its edits pending.
    Restore,
    /// Delete the autosaved files for good.
    Discard,
    /// Keep the files and ask again at the next start (also Escape).
    Later,
}

/// One autosaved layer, as the dialog lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreItem {
    pub layer_name: String,
    pub edits: usize,
    /// When it was saved, seconds since the Unix epoch.
    pub saved_at: u64,
    /// Set when the layer came from a different API server than the one in
    /// use now: the server it came from and the current one.
    pub other_server: Option<(String, String)>,
}

/// "just now", "5 minutes ago", "2 hours ago", "3 days ago".
pub fn age_phrase(now: u64, saved_at: u64) -> String {
    let secs = now.saturating_sub(saved_at);
    match secs {
        0..=59 => tr!("dialog.age.just_now").to_string(),
        60..=3599 => tr_count!("dialog.age.minutes", secs / 60).to_string(),
        3600..=86_399 => tr_count!("dialog.age.hours", secs / 3600).to_string(),
        86_400..=2_591_999 => tr_count!("dialog.age.days", secs / 86_400).to_string(),
        2_592_000..=31_535_999 => tr_count!("dialog.age.months", secs / 2_592_000).to_string(),
        _ => tr_count!("dialog.age.years", secs / 31_536_000).to_string(),
    }
}

impl RestoreItem {
    /// The list line: `Name: 12 edits, 5 minutes ago`.
    pub fn line(&self, now: u64) -> String {
        tr!(
            "dialog.restore.layer",
            layer = self.layer_name,
            edits = tr_count!("dialog.restore.edits", self.edits),
            when = age_phrase(now, self.saved_at)
        )
        .to_string()
    }

    /// The warning shown under the line when the server differs.
    pub fn server_note(&self) -> Option<String> {
        self.other_server.as_ref().map(|(from, current)| {
            tr!(
                "dialog.restore.other_server",
                server = from,
                current = current
            )
            .to_string()
        })
    }
}

pub struct RestoreDialog {
    items: Vec<RestoreItem>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for RestoreDialog {}

impl RestoreDialog {
    pub fn new(items: Vec<RestoreItem>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            items,
            focus_handle,
        }
    }

    /// The layers the dialog lists.
    pub fn items(&self) -> &[RestoreItem] {
        &self.items
    }

    /// Press Restore.
    pub fn press_restore(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Restore);
    }

    /// Press Discard.
    pub fn press_discard(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Discard);
    }

    /// Press "Ask Me Next Time" (or Escape).
    pub fn press_later(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Later);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => self.press_later(cx),
            modal::ModalKey::Enter => self.press_restore(cx),
            modal::ModalKey::Other => {}
        }
    }
}

impl crate::ui::modal::ModalDialog for RestoreDialog {}

impl Focusable for RestoreDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RestoreDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());

        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("restore-later")
                    .label(tr!("dialog.restore.later"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_later(cx))),
            )
            .child(
                Button::new("restore-discard")
                    .danger()
                    .label(tr!("dialog.restore.discard"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_discard(cx))),
            )
            .child(
                Button::new("restore-confirm")
                    .primary()
                    .label(tr!("dialog.restore.restore"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_restore(cx))),
            );

        let mut list = v_flex().gap_2().pl_2();
        for item in &self.items {
            let mut entry = v_flex().gap_0p5().child(
                div()
                    .text_color(cx.theme().foreground)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(item.line(now)),
            );
            if let Some(note) = item.server_note() {
                entry = entry.child(
                    div()
                        .text_size(muted_text_size())
                        .text_color(cx.theme().warning)
                        .child(note),
                );
            }
            list = list.child(entry);
        }

        let body = v_flex()
            .gap_3()
            .child(
                div()
                    .text_color(cx.theme().foreground)
                    .child(tr!("dialog.restore.intro")),
            )
            .child(list)
            .child(
                div()
                    .text_size(muted_text_size())
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("dialog.restore.footnote")),
            );

        let frame = modal::dialog_frame(
            cx,
            gpui::px(520.0),
            tr!("dialog.restore.title"),
            body,
            footer,
        );

        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_naturally() {
        let now = 1_000_000;
        assert_eq!(age_phrase(now, now), "just now");
        assert_eq!(age_phrase(now, now - 59), "just now");
        assert_eq!(age_phrase(now, now - 60), "1 minute ago");
        assert_eq!(age_phrase(now, now - 300), "5 minutes ago");
        assert_eq!(age_phrase(now, now - 3600), "1 hour ago");
        assert_eq!(age_phrase(now, now - 7300), "2 hours ago");
        assert_eq!(age_phrase(now, now - 86_400 * 3), "3 days ago");
        assert_eq!(age_phrase(now + 86_400 * 100, now), "3 months ago");
        assert_eq!(age_phrase(now + 86_400 * 400, now), "1 year ago");
        // A clock that went backwards is not an error.
        assert_eq!(age_phrase(now, now + 500), "just now");
    }

    #[test]
    fn a_line_names_the_layer_edits_and_age() {
        let item = RestoreItem {
            layer_name: "Downtown".into(),
            edits: 12,
            saved_at: 1_000_000 - 300,
            other_server: None,
        };
        assert_eq!(item.line(1_000_000), "Downtown: 12 edits, 5 minutes ago");
        assert_eq!(item.server_note(), None);
        let one = RestoreItem { edits: 1, ..item };
        assert_eq!(one.line(1_000_000), "Downtown: 1 edit, 5 minutes ago");
    }

    #[test]
    fn a_layer_from_another_server_says_so() {
        let item = RestoreItem {
            layer_name: "Dev test".into(),
            edits: 2,
            saved_at: 0,
            other_server: Some((
                "https://master.apis.dev.openstreetmap.org/api/0.6".into(),
                "https://api.openstreetmap.org/api/0.6".into(),
            )),
        };
        let note = item.server_note().unwrap();
        assert!(note.contains("master.apis.dev.openstreetmap.org"), "{note}");
        assert!(note.contains("https://api.openstreetmap.org"), "{note}");
    }
}
