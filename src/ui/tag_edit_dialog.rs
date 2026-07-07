//! Modal dialog to add, edit, or rename a single OSM tag key/value on the
//! current selection.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagEditField {
    Key,
    Value,
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_variants_are_distinct() {
        assert_ne!(TagEditField::Key, TagEditField::Value);
        assert_ne!(TagEditField::Value, TagEditField::None);
    }
}

use gpui::{
    prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::{
    input::{InputState, SelectAll},
    label::Label,
    v_flex, ActiveTheme as _,
};

use crate::ui::modal;

pub enum DialogEvent {
    Submitted { key: String, value: String },
    Cancelled,
}

pub struct TagEditDialog {
    title: SharedString,
    key: Entity<InputState>,
    value: Entity<InputState>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for TagEditDialog {}

impl TagEditDialog {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        title: impl Into<SharedString>,
        initial_key: String,
        initial_value: String,
        select: TagEditField,
    ) -> Self {
        let key = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("key");
            state.set_value(initial_key, window, cx);
            state
        });
        let value = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("value");
            state.set_value(initial_value, window, cx);
            state
        });
        let focus_handle = cx.focus_handle();

        // `Window::dispatch_action` resolves the target element by looking it up in
        // `self.rendered_frame` (the last *painted* frame). At construction time this
        // dialog has never been rendered, so the input's focus handle can't be found
        // there yet and the action would be dispatched against the wrong node (or
        // dropped). Deferring the dispatch to `on_next_frame` runs it right after the
        // dialog's first paint, by which point the focused input is present in
        // `rendered_frame` and `SelectAll` reaches the right `InputState`.
        match select {
            TagEditField::Key => {
                key.update(cx, |state, cx| state.focus(window, cx));
                window.on_next_frame(|window, cx| {
                    window.dispatch_action(Box::new(SelectAll), cx);
                });
            }
            TagEditField::Value => {
                value.update(cx, |state, cx| state.focus(window, cx));
                window.on_next_frame(|window, cx| {
                    window.dispatch_action(Box::new(SelectAll), cx);
                });
            }
            TagEditField::None => {
                key.update(cx, |state, cx| state.focus(window, cx));
            }
        }

        Self {
            title: title.into(),
            key,
            value,
            error: None,
            focus_handle,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let key = self.key.read(cx).value().trim().to_string();
        let value = self.value.read(cx).value().to_string();
        if key.is_empty() {
            self.error = Some("Tag key is required.".into());
            cx.notify();
            return;
        }
        self.error = None;
        cx.emit(DialogEvent::Submitted { key, value });
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => self.cancel(cx),
            modal::ModalKey::Enter => self.submit(cx),
            modal::ModalKey::Other => {}
        }
    }
}

impl Focusable for TagEditDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TagEditDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let mut body = v_flex()
            .gap_3()
            .child(modal::field_row("Key", &self.key, muted))
            .child(modal::field_row("Value", &self.value, muted));

        if let Some(msg) = self.error.clone() {
            body = body.child(Label::new(msg).text_sm().text_color(cx.theme().danger));
        }

        let footer = modal::footer_row(
            "cancel",
            "Cancel",
            cx.listener(|this, _, _w, cx| this.cancel(cx)),
            "save",
            "Save",
            cx.listener(|this, _, _w, cx| this.submit(cx)),
        );

        let frame = modal::dialog_frame(cx, gpui::px(360.0), self.title.clone(), body, footer);

        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
