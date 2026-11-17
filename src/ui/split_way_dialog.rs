//! Modal dialog asking which part of a way split at a point keeps the
//! existing way (its id, version and history). The other part becomes a new
//! way. An in-window modal like `delete_layer_dialog.rs`.

use gpui::{
    div, prelude::*, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    ActiveTheme as _,
};

use crate::tr;
use crate::ui::modal;

/// Which part of the split way stays in the original way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepPart {
    /// From the way's first node to the split point.
    First,
    /// From the split point to the way's last node.
    Second,
}

pub enum DialogEvent {
    /// The user chose the part that keeps the history.
    Chosen(KeepPart),
    /// The user cancelled (Cancel or Escape).
    Cancelled,
}

pub struct SplitWayDialog {
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for SplitWayDialog {}

impl SplitWayDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self { focus_handle }
    }

    /// Choose the part that keeps the existing way.
    pub fn press_keep(&mut self, part: KeepPart, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Chosen(part));
    }

    /// Press Cancel.
    pub fn press_cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let modal::ModalKey::Escape = modal::classify_key(ev) {
            self.press_cancel(cx);
        }
    }
}

impl modal::ModalDialog for SplitWayDialog {}

impl Focusable for SplitWayDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SplitWayDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("split-way-cancel")
                    .label(tr!("dialog.cancel"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_cancel(cx))),
            )
            .child(
                Button::new("split-way-first")
                    .primary()
                    .label(tr!("dialog.split_way.first"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_keep(KeepPart::First, cx))),
            )
            .child(
                Button::new("split-way-second")
                    .primary()
                    .label(tr!("dialog.split_way.second"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_keep(KeepPart::Second, cx))),
            );

        let body = div()
            .text_color(cx.theme().foreground)
            .child(tr!("dialog.split_way.body"));

        let frame = modal::dialog_frame(
            cx,
            gpui::px(560.0),
            tr!("dialog.split_way.title"),
            body,
            footer,
        );

        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
