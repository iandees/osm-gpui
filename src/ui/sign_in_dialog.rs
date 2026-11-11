//! Modal dialog shown when the user tries to upload while signed out. Mirrors
//! `upload_dialog.rs`'s scrim/card/focus/Escape conventions. The primary
//! button asks the host to open Settings (where sign-in lives).

use crate::tr;
use gpui::{
    div, prelude::*, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    label::Label,
    v_flex, ActiveTheme as _,
};

/// Whether an upload attempt must be intercepted with the sign-in prompt,
/// given the display name of the signed-in account (if any).
pub fn needs_sign_in(signed_in_display_name: Option<&str>) -> bool {
    signed_in_display_name.is_none()
}

pub enum DialogEvent {
    /// User chose to open Settings to sign in.
    OpenSettings,
    Cancelled,
}

/// What the user was trying to do when the prompt appeared; selects the
/// wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInPurpose {
    Upload,
    Notes,
}

impl SignInPurpose {
    pub fn title(self) -> String {
        match self {
            SignInPurpose::Upload => tr!("dialog.sign_in.title").to_string(),
            SignInPurpose::Notes => tr!("dialog.sign_in.notes_title").to_string(),
        }
    }

    pub fn body(self) -> String {
        match self {
            SignInPurpose::Upload => tr!("dialog.sign_in.body").to_string(),
            SignInPurpose::Notes => tr!("dialog.sign_in.notes_body").to_string(),
        }
    }
}

pub struct SignInDialog {
    focus_handle: FocusHandle,
    purpose: SignInPurpose,
}

impl EventEmitter<DialogEvent> for SignInDialog {}

impl SignInDialog {
    pub fn new(purpose: SignInPurpose, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            focus_handle,
            purpose,
        }
    }

    pub fn purpose(&self) -> SignInPurpose {
        self.purpose
    }

    fn open_settings(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::OpenSettings);
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.cancel(cx),
            "enter" => self.open_settings(cx),
            _ => {}
        }
    }
}

impl crate::ui::modal::ModalDialog for SignInDialog {}

impl Focusable for SignInDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SignInDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body =
            v_flex().child(Label::new(self.purpose.body()).text_color(cx.theme().foreground));

        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("sign-in-cancel")
                    .label(tr!("dialog.cancel"))
                    .on_click(cx.listener(|this, _, _w, cx| this.cancel(cx))),
            )
            .child(
                Button::new("sign-in-open-settings")
                    .primary()
                    .label(tr!("dialog.sign_in.open_settings"))
                    .on_click(cx.listener(|this, _, _w, cx| this.open_settings(cx))),
            );

        let frame = v_flex()
            .w(gpui::px(420.0))
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
                    .child(self.purpose.title()),
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
            .occlude()
            .bg(crate::ui::style::scrim_color())
            .flex()
            .justify_center()
            .items_center()
            .child(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_needed_when_signed_out() {
        assert!(needs_sign_in(None));
    }

    #[test]
    fn sign_in_not_needed_when_signed_in() {
        assert!(!needs_sign_in(Some("someone")));
    }

    #[test]
    fn wording_follows_the_purpose() {
        assert!(SignInPurpose::Upload.title().contains("upload"));
        assert!(SignInPurpose::Upload.body().contains("upload"));
        assert!(SignInPurpose::Notes.title().contains("notes"));
        assert!(SignInPurpose::Notes.body().contains("notes"));
        assert!(!SignInPurpose::Notes.body().contains("upload"));
        assert!(!SignInPurpose::Notes.title().contains("upload"));
    }
}
