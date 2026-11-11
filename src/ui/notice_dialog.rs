//! A plain "this can't be done" dialog: a title, a message and one OK button.
//! Used where a refusal must not be missed in the status bar (an upload the
//! server would reject for a way or relation over its size limit). An
//! in-window modal like `about_dialog.rs`; no second native window.

use gpui::{
    div, prelude::*, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    label::Label,
    ActiveTheme as _,
};

use crate::tr;
use crate::ui::modal;

pub enum DialogEvent {
    /// The user dismissed the dialog (OK, Esc or Enter).
    Closed,
}

pub struct NoticeDialog {
    title: SharedString,
    message: SharedString,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for NoticeDialog {}

impl NoticeDialog {
    pub fn new(
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            title: title.into(),
            message: message.into(),
            focus_handle,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Closed);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape | modal::ModalKey::Enter => self.close(cx),
            modal::ModalKey::Other => {}
        }
    }
}

impl crate::ui::modal::ModalDialog for NoticeDialog {}

impl Focusable for NoticeDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NoticeDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let footer = div().flex().flex_row().justify_end().child(
            Button::new("notice-ok")
                .primary()
                .label(tr!("dialog.notice.ok"))
                .on_click(cx.listener(|this, _, _w, cx| this.close(cx))),
        );
        let body = Label::new(self.message.clone()).text_color(cx.theme().foreground);
        let frame = modal::dialog_frame(cx, gpui::px(420.0), self.title.clone(), body, footer);
        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
