//! Modal dialog to add a PMTiles vector-tile overlay from a web address or a
//! file on this computer.

use crate::pmtiles::SourceSpec;
use crate::tr;
use crate::ui::modal;
use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::{input::InputState, label::Label, v_flex, ActiveTheme as _};

pub enum DialogEvent {
    /// The archive to open and the layer's name (blank: name it after the file).
    Submitted {
        source: SourceSpec,
        name: String,
    },
    Cancelled,
}

pub struct PmtilesDialog {
    source: Entity<InputState>,
    name: Entity<InputState>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for PmtilesDialog {}

impl PmtilesDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let source = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr!("dialog.pmtiles.source_placeholder"))
        });
        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr!("dialog.pmtiles.name_placeholder"))
        });
        let focus_handle = cx.focus_handle();
        source.update(cx, |state, cx| state.focus(window, cx));
        Self {
            source,
            name,
            error: None,
            focus_handle,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let raw = self.source.read(cx).value().to_string();
        let Some(source) = SourceSpec::parse(&raw) else {
            self.error = Some(tr!("dialog.pmtiles.error.source_empty"));
            cx.notify();
            return;
        };
        let name = self.name.read(cx).value().trim().to_string();
        self.error = None;
        cx.emit(DialogEvent::Submitted { source, name });
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

impl crate::ui::modal::ModalDialog for PmtilesDialog {
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        self.source.read(cx).focus_handle(cx)
    }
}

impl Focusable for PmtilesDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PmtilesDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut body = v_flex()
            .gap_3()
            .child(modal::field_row(
                tr!("dialog.pmtiles.source"),
                &self.source,
                muted,
            ))
            .child(modal::field_row(
                tr!("dialog.pmtiles.name"),
                &self.name,
                muted,
            ))
            .child(
                div()
                    .text_size(crate::ui::style::muted_text_size())
                    .text_color(muted)
                    .child(tr!("dialog.pmtiles.hint")),
            );
        if let Some(msg) = self.error.clone() {
            body = body.child(Label::new(msg).text_color(cx.theme().danger));
        }
        let footer = modal::footer_row(
            "cancel",
            tr!("dialog.cancel"),
            cx.listener(|this, _, _w, cx| this.cancel(cx)),
            "add",
            tr!("dialog.pmtiles.confirm"),
            cx.listener(|this, _, _w, cx| this.submit(cx)),
        );
        let frame = modal::dialog_frame(
            cx,
            gpui::px(480.0),
            tr!("dialog.pmtiles.title"),
            body,
            footer,
        );
        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
