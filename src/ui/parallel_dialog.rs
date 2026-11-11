//! Modal dialog for Create Parallel Way: the offset in metres, and whether to
//! copy the tags and reverse the copy's direction.

use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::{
    checkbox::Checkbox,
    input::{InputEvent, InputState},
    label::Label,
    v_flex, ActiveTheme as _,
};

use crate::parallel::{self, ParallelParams};
use crate::tr;
use crate::ui::modal;
use crate::ui::style::muted_text_size;

pub enum DialogEvent {
    Submitted(ParallelParams),
    Cancelled,
}

pub struct ParallelDialog {
    offset: Entity<InputState>,
    copy_tags: bool,
    reverse: bool,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    _subscription: gpui::Subscription,
}

impl EventEmitter<DialogEvent> for ParallelDialog {}

impl ParallelDialog {
    /// A dialog starting from `initial` (the values last used this session).
    pub fn new(initial: ParallelParams, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = format!("{}", initial.offset_m);
        let offset = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(text, window, cx);
            state
        });
        offset.update(cx, |state, cx| state.focus(window, cx));
        let subscription = cx.subscribe(&offset, |this, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && this.error.take().is_some() {
                cx.notify();
            }
        });
        Self {
            offset,
            copy_tags: initial.copy_tags,
            reverse: initial.reverse,
            error: None,
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.offset.read(cx).value().to_string();
        match parallel::parse_offset(&text) {
            Some(offset_m) => cx.emit(DialogEvent::Submitted(ParallelParams {
                offset_m,
                copy_tags: self.copy_tags,
                reverse: self.reverse,
            })),
            None => {
                self.error = Some(tr!(
                    "dialog.parallel.error.offset",
                    max = parallel::MAX_OFFSET_M
                ));
                cx.notify();
            }
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => cx.emit(DialogEvent::Cancelled),
            modal::ModalKey::Enter => self.submit(cx),
            modal::ModalKey::Other => {}
        }
    }
}

impl Focusable for ParallelDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ParallelDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut body = v_flex()
            .gap_3()
            .child(modal::field_row(
                tr!("dialog.parallel.offset"),
                &self.offset,
                muted,
            ))
            .child(
                Label::new(tr!("dialog.parallel.offset_hint"))
                    .text_size(muted_text_size())
                    .text_color(muted),
            )
            .child(
                Checkbox::new("parallel-copy-tags")
                    .checked(self.copy_tags)
                    .label(tr!("dialog.parallel.copy_tags"))
                    .text_size(crate::ui::style::body_text_size())
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.copy_tags = *checked;
                        cx.notify();
                    })),
            )
            .child(
                Checkbox::new("parallel-reverse")
                    .checked(self.reverse)
                    .label(tr!("dialog.parallel.reverse"))
                    .text_size(crate::ui::style::body_text_size())
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.reverse = *checked;
                        cx.notify();
                    })),
            );
        if let Some(message) = self.error.clone() {
            body = body.child(div().child(Label::new(message).text_color(cx.theme().danger)));
        }

        let footer = modal::footer_row(
            "cancel",
            tr!("dialog.cancel"),
            cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Cancelled)),
            "create",
            tr!("dialog.parallel.confirm"),
            cx.listener(|this, _, _, cx| this.submit(cx)),
        );
        let frame = modal::dialog_frame(
            cx,
            gpui::px(380.0),
            tr!("dialog.parallel.title"),
            body,
            footer,
        );
        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
