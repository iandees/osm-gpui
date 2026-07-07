//! Modal dialog warning the user about unsaved changes before quitting.
//!
//! Mirrors the scrim/dialog conventions in `tag_edit_dialog.rs` /
//! `custom_imagery_dialog.rs`: an `.occlude()`'d full-window scrim behind a
//! centered card, so clicks on the scrim don't pass through to whatever is
//! underneath (see PR #50).

use gpui::{
    div, prelude::*, rgba, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    v_flex, ActiveTheme as _,
};

pub enum DialogEvent {
    /// The user chose "Quit" — caller should actually call `cx.quit()`.
    ConfirmQuit,
    /// The user chose "Cancel" (or pressed Escape) — do nothing further.
    Cancelled,
}

pub struct QuitConfirmDialog {
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for QuitConfirmDialog {}

impl QuitConfirmDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self { focus_handle }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::ConfirmQuit);
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.cancel(cx),
            "enter" => self.confirm(cx),
            _ => {}
        }
    }
}

impl Focusable for QuitConfirmDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for QuitConfirmDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("quit-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _w, cx| this.cancel(cx))),
            )
            .child(
                Button::new("quit-confirm")
                    .primary()
                    .label("Quit")
                    .on_click(cx.listener(|this, _, _w, cx| this.confirm(cx))),
            );

        let frame = v_flex()
            .w(gpui::px(360.0))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_color(cx.theme().foreground)
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Unsaved Changes"),
            )
            .child(
                div()
                    .p_4()
                    .text_color(cx.theme().foreground)
                    .child("You have unsaved changes. Quit anyway?"),
            )
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(footer),
            );

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .absolute()
            .inset_0()
            .occlude()
            .bg(rgba(0x00000099))
            .flex()
            .justify_center()
            .items_center()
            .child(frame)
    }
}
