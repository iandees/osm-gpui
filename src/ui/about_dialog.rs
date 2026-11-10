//! "About" modal dialog: app name, version, a one-line description and the
//! project link. GPUI has no standard About panel, and a second native window
//! is avoided, so this is an in-window modal like `quit_confirm_dialog.rs`.

use gpui::{
    div, prelude::*, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    v_flex, ActiveTheme as _,
};

use crate::tr;
use crate::ui::modal;
use crate::version::VERSION;

/// The project page shown (and opened) from the dialog.
pub const PROJECT_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// App name shown in the dialog title and body.
const APP_NAME: &str = "osm-gpui";

pub enum DialogEvent {
    /// The user closed the dialog (Close button, Esc or Enter).
    Closed,
}

pub struct AboutDialog {
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for AboutDialog {}

impl AboutDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self { focus_handle }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Closed);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape | modal::ModalKey::Enter => self.close(cx),
            modal::ModalKey::Other => {}
        }
    }
}

impl Focusable for AboutDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AboutDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let footer = div().flex().flex_row().justify_end().child(
            Button::new("about-close")
                .primary()
                .label(tr!("dialog.about.close"))
                .on_click(cx.listener(|this, _, _w, cx| this.close(cx))),
        );

        let link_color = cx.theme().link;
        let link_hover = cx.theme().link_hover;
        let body = v_flex()
            .gap_2()
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(APP_NAME),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("dialog.about.version", version = VERSION)),
            )
            .child(tr!("dialog.about.description"))
            .child(
                div()
                    .id("about-project-link")
                    .cursor_pointer()
                    .text_color(link_color)
                    .underline()
                    .hover(move |this| this.text_color(link_hover))
                    .on_click(|_, _, _| {
                        let _ = open::that(PROJECT_URL);
                    })
                    .child(PROJECT_URL),
            );

        let frame = modal::dialog_frame(
            cx,
            gpui::px(360.0),
            tr!("dialog.about.title", app = APP_NAME),
            body,
            footer,
        );

        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
