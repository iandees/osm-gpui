//! The "Revert Changeset" dialog: the number (or page address) of the
//! changeset to undo. It shows a busy state while the changeset is fetched and
//! an error line when that fails, keeping what was typed.

use gpui::{
    prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _, Disableable as _,
};

use crate::changeset_revert::parse_changeset_id;
use crate::tr;
use crate::ui::modal;
use crate::ui::style::muted_text_size;

pub enum DialogEvent {
    /// The changeset to revert.
    Submit(i64),
    Cancelled,
}

pub struct RevertChangesetDialog {
    input: Entity<InputState>,
    focus_handle: FocusHandle,
    busy: bool,
    error: Option<SharedString>,
}

impl EventEmitter<DialogEvent> for RevertChangesetDialog {}

impl RevertChangesetDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("revert.placeholder")));
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

    /// The changeset is being fetched.
    pub fn begin(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        cx.notify();
    }

    /// The revert failed: show why; the text stays.
    pub fn fail(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(message);
        cx.notify();
    }

    pub fn press_revert(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match parse_changeset_id(&self.text(cx)) {
            Some(id) => cx.emit(DialogEvent::Submit(id)),
            None => {
                self.error = Some(tr!("revert.bad_id").to_string().into());
                cx.notify();
            }
        }
    }

    pub fn press_cancel(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(DialogEvent::Cancelled);
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => self.press_cancel(cx),
            modal::ModalKey::Enter => self.press_revert(cx),
            _ => {}
        }
    }
}

impl crate::ui::modal::ModalDialog for RevertChangesetDialog {
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Focusable for RevertChangesetDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RevertChangesetDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let can_revert = !self.busy && !self.text(cx).trim().is_empty();
        let body = v_flex()
            .gap_2()
            .child(
                Label::new(tr!("revert.hint"))
                    .text_size(muted_text_size())
                    .text_color(muted),
            )
            .child(Input::new(&self.input).disabled(self.busy))
            .children(
                self.error
                    .clone()
                    .map(|e| Label::new(e).text_color(cx.theme().danger)),
            );

        let footer = modal::footer_buttons(
            Button::new("revert-changeset-cancel")
                .label(tr!("dialog.cancel"))
                .disabled(self.busy)
                .on_click(cx.listener(|this, _, _w, cx| this.press_cancel(cx))),
            Button::new("revert-changeset-go")
                .primary()
                .label(if self.busy {
                    tr!("revert.working")
                } else {
                    tr!("revert.action")
                })
                .disabled(!can_revert)
                .on_click(cx.listener(|this, _, _w, cx| this.press_revert(cx))),
        );

        modal::scrim(
            &self.focus_handle,
            cx.listener(Self::on_key_down),
            modal::dialog_frame(cx, gpui::px(440.0), tr!("revert.title"), body, footer),
        )
    }
}
