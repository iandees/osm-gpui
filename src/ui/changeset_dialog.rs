//! The "Show Changeset" dialog: asks for a changeset number (or a link to one)
//! and shows a busy state while it loads, or why that failed.

use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _, Disableable as _,
};

use crate::changeset_view::parse_changeset_id;
use crate::tr;
use crate::ui::modal;
use crate::ui::style::muted_text_size;

pub enum DialogEvent {
    Submit(i64),
    Cancelled,
}

pub struct ChangesetDialog {
    input: Entity<InputState>,
    focus_handle: FocusHandle,
    busy: bool,
    error: Option<SharedString>,
}

impl EventEmitter<DialogEvent> for ChangesetDialog {}

impl ChangesetDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder(tr!("changeset.dialog.placeholder")));
        input.update(cx, |state, cx| state.focus(window, cx));
        Self {
            input,
            focus_handle: cx.focus_handle(),
            busy: false,
            error: None,
        }
    }

    pub fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| {
            state.set_value(text.to_string(), window, cx)
        });
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// The changeset is loading.
    pub fn begin(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        cx.notify();
    }

    /// Loading failed: show why; the text stays.
    pub fn fail(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(message);
        cx.notify();
    }

    pub fn press_show(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match parse_changeset_id(&self.text(cx)) {
            Some(id) => cx.emit(DialogEvent::Submit(id)),
            None => {
                self.error = Some(tr!("changeset.dialog.invalid"));
                cx.notify();
            }
        }
    }

    pub fn press_cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.press_cancel(cx),
            "enter" => self.press_show(cx),
            _ => {}
        }
    }
}

impl crate::ui::modal::ModalDialog for ChangesetDialog {
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Focusable for ChangesetDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChangesetDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let can_show = !self.busy && !self.text(cx).trim().is_empty();
        let body = v_flex()
            .gap_2()
            .child(
                Label::new(tr!("changeset.dialog.hint"))
                    .text_size(muted_text_size())
                    .text_color(muted),
            )
            .child(Input::new(&self.input).disabled(self.busy))
            .children(
                self.error
                    .clone()
                    .map(|e| Label::new(e).text_color(cx.theme().danger)),
            );

        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("changeset-cancel")
                    .label(tr!("dialog.cancel"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_cancel(cx))),
            )
            .child(
                Button::new("changeset-show")
                    .primary()
                    .label(if self.busy {
                        tr!("changeset.dialog.loading")
                    } else {
                        tr!("changeset.dialog.show")
                    })
                    .disabled(!can_show)
                    .on_click(cx.listener(|this, _, _w, cx| this.press_show(cx))),
            );

        modal::scrim(
            &self.focus_handle,
            cx.listener(Self::on_key_down),
            modal::dialog_frame(
                cx,
                gpui::px(440.0),
                tr!("changeset.dialog.title"),
                body,
                footer,
            ),
        )
    }
}
