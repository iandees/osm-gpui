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
    div, prelude::*, rgba, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState, SelectAll},
    label::Label,
    v_flex, ActiveTheme as _,
};

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

        match select {
            TagEditField::Key => {
                key.update(cx, |state, cx| state.focus(window, cx));
                window.dispatch_action(Box::new(SelectAll), cx);
            }
            TagEditField::Value => {
                value.update(cx, |state, cx| state.focus(window, cx));
                window.dispatch_action(Box::new(SelectAll), cx);
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
        match ev.keystroke.key.as_str() {
            "escape" => self.cancel(cx),
            "enter" => self.submit(cx),
            _ => {}
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

        let field_row = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap_1()
                .child(Label::new(label).text_xs().text_color(muted))
                .child(Input::new(input))
        };

        let mut body = v_flex()
            .gap_3()
            .child(field_row("Key", &self.key))
            .child(field_row("Value", &self.value));

        if let Some(msg) = self.error.clone() {
            body = body.child(Label::new(msg).text_sm().text_color(cx.theme().danger));
        }

        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _w, cx| this.cancel(cx))),
            )
            .child(
                Button::new("save")
                    .primary()
                    .label("Save")
                    .on_click(cx.listener(|this, _, _w, cx| this.submit(cx))),
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
                    .child(self.title.clone()),
            )
            .child(div().p_4().child(body))
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
            .bg(rgba(0x00000099))
            .flex()
            .justify_center()
            .items_center()
            .child(frame)
    }
}
