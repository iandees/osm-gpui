//! Modal dialog asking for confirmation before deleting a layer that has
//! unsaved edits. An in-window modal like `quit_confirm_dialog.rs` (no second
//! native window).
//!
//! Unlike the quit dialog, Cancel is the default: Enter and Escape both
//! cancel, and the destructive button only fires on a click.

use gpui::{
    div, prelude::*, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    ActiveTheme as _,
};

use crate::layers::LayerId;
use crate::ui::modal;
use crate::{tr, tr_count};

pub enum DialogEvent {
    /// The user chose "Delete Layer" for this layer.
    Confirmed(LayerId),
    /// The user chose "Cancel" (or pressed Escape or Enter).
    Cancelled,
}

pub struct DeleteLayerDialog {
    layer: LayerId,
    layer_name: String,
    pending: usize,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for DeleteLayerDialog {}

impl DeleteLayerDialog {
    pub fn new(
        layer: LayerId,
        layer_name: impl Into<String>,
        pending: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            layer,
            layer_name: layer_name.into(),
            pending,
            focus_handle,
        }
    }

    /// The layer this dialog is asking about.
    pub fn layer(&self) -> LayerId {
        self.layer
    }

    /// The unsaved-edit count shown in the message.
    pub fn pending(&self) -> usize {
        self.pending
    }

    /// The message text.
    pub fn message(&self) -> String {
        tr_count!(
            "dialog.delete_layer.body",
            self.pending,
            layer = self.layer_name
        )
        .to_string()
    }

    /// Press the destructive "Delete Layer" button.
    pub fn press_confirm(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Confirmed(self.layer));
    }

    /// Press Cancel.
    pub fn press_cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape | modal::ModalKey::Enter => self.press_cancel(cx),
            modal::ModalKey::Other => {}
        }
    }
}

impl Focusable for DeleteLayerDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DeleteLayerDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("delete-layer-cancel")
                    .primary()
                    .label(tr!("dialog.cancel"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_cancel(cx))),
            )
            .child(
                Button::new("delete-layer-confirm")
                    .danger()
                    .label(tr!("dialog.delete_layer.confirm"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_confirm(cx))),
            );

        let body = div()
            .text_color(cx.theme().foreground)
            .child(self.message());

        let frame = modal::dialog_frame(
            cx,
            gpui::px(420.0),
            tr!("dialog.delete_layer.title"),
            body,
            footer,
        );

        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
