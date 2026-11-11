//! The "New Note" dialog: the text of a note about to be created at a place
//! the user clicked. It shows a busy state while the note is being sent and an
//! error line when that fails, keeping the typed text.

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

use crate::tr;
use crate::ui::modal;
use crate::ui::style::muted_text_size;

pub enum DialogEvent {
    /// The text to create the note with.
    Submit(String),
    Cancelled,
}

pub struct NewNoteDialog {
    input: Entity<InputState>,
    focus_handle: FocusHandle,
    lat: f64,
    lon: f64,
    busy: bool,
    error: Option<SharedString>,
}

impl EventEmitter<DialogEvent> for NewNoteDialog {}

impl NewNoteDialog {
    pub fn new(lat: f64, lon: f64, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(4, 10)
                .placeholder(tr!("notes.new.placeholder"))
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        Self {
            input,
            focus_handle: cx.focus_handle(),
            lat,
            lon,
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

    /// The note is being sent.
    pub fn begin(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        cx.notify();
    }

    /// Sending failed: show why; the text stays.
    pub fn fail(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(message);
        cx.notify();
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    pub fn press_create(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let text = self.text(cx);
        if text.trim().is_empty() {
            return;
        }
        cx.emit(DialogEvent::Submit(text));
    }

    pub fn press_cancel(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(DialogEvent::Cancelled);
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "escape" {
            self.press_cancel(cx);
        }
    }
}

impl crate::ui::modal::ModalDialog for NewNoteDialog {
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Focusable for NewNoteDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NewNoteDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let can_create = !self.busy && !self.text(cx).trim().is_empty();
        let body = v_flex()
            .gap_2()
            .child(
                Label::new(tr!("notes.new.hint"))
                    .text_size(muted_text_size())
                    .text_color(muted),
            )
            .child(
                Label::new(format!("{:.5}, {:.5}", self.lat, self.lon))
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
                Button::new("new-note-cancel")
                    .label(tr!("dialog.cancel"))
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _w, cx| this.press_cancel(cx))),
            )
            .child(
                Button::new("new-note-create")
                    .primary()
                    .label(if self.busy {
                        tr!("notes.panel.working")
                    } else {
                        tr!("notes.new.create")
                    })
                    .disabled(!can_create)
                    .on_click(cx.listener(|this, _, _w, cx| this.press_create(cx))),
            );

        modal::scrim(
            &self.focus_handle,
            cx.listener(Self::on_key_down),
            modal::dialog_frame(cx, gpui::px(440.0), tr!("notes.new.title"), body, footer),
        )
    }
}
