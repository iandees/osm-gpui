//! Tools > Validate: runs the validator on the pending changes and lists what
//! it found, without starting an upload. An in-window modal like
//! `notice_dialog.rs`. Pressing Show on a finding closes the dialog and
//! selects the objects it concerns.

use gpui::{
    div, prelude::*, px, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    label::Label,
    v_flex, ActiveTheme as _,
};

use crate::selection::FeatureRef;
use crate::tr;
use crate::ui::modal;
use crate::ui::validation_list::{feature_refs, findings_list, summary_text};
use crate::validator::Report;

pub enum DialogEvent {
    /// The user closed the dialog (Close, Esc or Enter).
    Closed,
    /// The user pressed Show on a finding: select these objects and zoom to
    /// them.
    Show(Vec<FeatureRef>),
}

pub struct ValidateDialog {
    report: Report,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for ValidateDialog {}

impl ValidateDialog {
    pub fn new(report: Report, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            report,
            focus_handle,
        }
    }

    pub fn report(&self) -> &Report {
        &self.report
    }

    /// Press Show on finding `index` (what a click does). `false` when there
    /// is no such finding.
    pub fn press_show(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.report.entries.get(index) else {
            return false;
        };
        cx.emit(DialogEvent::Show(feature_refs(entry)));
        true
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

impl Focusable for ValidateDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ValidateDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut body = v_flex().gap_3();
        if !self.report.checked_changes {
            body = body.child(
                Label::new(tr!("validator.nothing_pending")).text_color(theme.muted_foreground),
            );
        } else {
            body = body.child(
                Label::new(summary_text(&self.report))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(if self.report.has_errors() {
                        theme.danger
                    } else if self.report.is_empty() {
                        theme.foreground
                    } else {
                        theme.warning
                    }),
            );
            if !self.report.is_empty() {
                body = body.child(findings_list(
                    &self.report,
                    "validate-findings",
                    px(320.0),
                    cx,
                    |this: &mut Self, index, cx| {
                        this.press_show(index, cx);
                    },
                ));
            }
        }
        let footer = div().flex().flex_row().justify_end().child(
            Button::new("validate-close")
                .primary()
                .label(tr!("validator.close"))
                .on_click(cx.listener(|this, _, _w, cx| this.close(cx))),
        );
        let title: SharedString = tr!("validator.title");
        let frame = modal::dialog_frame(cx, px(560.0), title, body, footer);
        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
