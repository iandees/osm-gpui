//! Modal dialog warning the user about unsaved changes before quitting.
//!
//! Mirrors the scrim/dialog conventions in `tag_edit_dialog.rs` /
//! `custom_imagery_dialog.rs`: an `.occlude()`'d full-window scrim behind a
//! centered card, so clicks on the scrim don't pass through to whatever is
//! underneath (see PR #50).

use gpui::{div, prelude::*, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Window};
use gpui_component::ActiveTheme as _;

use crate::ui::modal;

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
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => self.cancel(cx),
            modal::ModalKey::Enter => self.confirm(cx),
            modal::ModalKey::Other => {}
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
        let footer = modal::footer_row(
            "quit-cancel",
            "Cancel",
            cx.listener(|this, _, _w, cx| this.cancel(cx)),
            "quit-confirm",
            "Quit",
            cx.listener(|this, _, _w, cx| this.confirm(cx)),
        );

        let body = div()
            .text_color(cx.theme().foreground)
            .child("You have unsaved changes. Quit anyway?");

        let frame = modal::dialog_frame(cx, gpui::px(360.0), "Unsaved Changes", body, footer);

        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
