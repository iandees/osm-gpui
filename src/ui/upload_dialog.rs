//! Modal dialog to review pending edits and provide a changeset comment
//! before uploading to the OSM API. Mirrors `quit_confirm_dialog.rs`'s
//! scrim/card/focus/Escape conventions, with a
//! `gpui_component::input::{Input, InputState}` for the comment field.

use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    spinner::Spinner,
    v_flex, ActiveTheme as _, Disableable as _, Sizable as _,
};

use crate::tr;
use crate::transfer::TransferHandle;
use crate::ui::style::muted_text_size;

/// One row of the pending-changes summary: a layer name plus its
/// created/modified/deleted counts (nodes + ways combined).
#[derive(Debug, Clone)]
pub struct LayerSummary {
    pub layer_name: String,
    pub created: usize,
    pub modified: usize,
    pub deleted: usize,
}

impl LayerSummary {
    pub fn is_empty(&self) -> bool {
        self.created == 0 && self.modified == 0 && self.deleted == 0
    }

    pub fn describe(&self) -> String {
        tr!(
            "dialog.upload.layer_summary",
            layer = self.layer_name,
            created = self.created,
            modified = self.modified,
            deleted = self.deleted
        )
        .to_string()
    }
}

/// What File > Upload to OSM (or its shortcut) should do. Every outcome is
/// visible to the user: a dialog or a status-bar message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadPlan {
    /// Not signed in: show the "sign in to upload" dialog.
    SignIn,
    /// Signed in, but no layer has changes (or there is no layer at all):
    /// show the "Nothing to upload" status message.
    NothingToUpload,
    /// Signed in with changes: show the changeset comment dialog.
    ShowDialog,
}

/// Decide what an upload request does, given the signed-in account's display
/// name (if any) and the per-layer summaries of modified layers.
pub fn plan_upload(signed_in_display_name: Option<&str>, summaries: &[LayerSummary]) -> UploadPlan {
    if crate::ui::sign_in_dialog::needs_sign_in(signed_in_display_name) {
        UploadPlan::SignIn
    } else if summaries.iter().all(|s| s.is_empty()) {
        UploadPlan::NothingToUpload
    } else {
        UploadPlan::ShowDialog
    }
}

pub enum DialogEvent {
    /// User clicked "Upload" with a non-empty comment.
    Upload {
        comment: String,
    },
    Cancelled,
}

/// Where the dialog is in the upload: reviewing, sending, or stopped by an error.
enum UploadState {
    Editing,
    /// The upload is running; the stage comes from the shared handle.
    InFlight(TransferHandle),
    /// The upload failed; the message is shown and the user may retry.
    Failed(SharedString),
}

impl UploadState {
    /// Whether the confirm button can be pressed. Disabled while sending so a
    /// second upload can't start.
    fn confirm_enabled(&self, comment_empty: bool) -> bool {
        !comment_empty && !matches!(self, UploadState::InFlight(_))
    }

    /// Whether Cancel/Esc closes the dialog. An upload in flight can't be
    /// abandoned: a half-applied changeset is worse than waiting.
    fn can_dismiss(&self) -> bool {
        !matches!(self, UploadState::InFlight(_))
    }
}

pub struct UploadDialog {
    summaries: Vec<LayerSummary>,
    comment: Entity<InputState>,
    focus_handle: FocusHandle,
    state: UploadState,
}

impl EventEmitter<DialogEvent> for UploadDialog {}

impl UploadDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, summaries: Vec<LayerSummary>) -> Self {
        let comment = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr!("dialog.upload.comment_placeholder"))
        });
        let focus_handle = cx.focus_handle();
        comment.update(cx, |state, cx| state.focus(window, cx));
        Self {
            summaries,
            comment,
            focus_handle,
            state: UploadState::Editing,
        }
    }

    /// Show the running upload's stage (read from `handle` on each render).
    pub fn begin_transfer(&mut self, handle: TransferHandle, cx: &mut Context<Self>) {
        self.state = UploadState::InFlight(handle);
        cx.notify();
    }

    /// Show why the upload stopped and let the user try again.
    pub fn fail(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.state = UploadState::Failed(message);
        cx.notify();
    }

    fn comment_text(&self, cx: &Context<Self>) -> String {
        self.comment.read(cx).value().trim().to_string()
    }

    fn upload(&mut self, cx: &mut Context<Self>) {
        let comment = self.comment_text(cx);
        if !self.state.confirm_enabled(comment.is_empty()) {
            return;
        }
        cx.emit(DialogEvent::Upload { comment });
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.state.can_dismiss() {
            return;
        }
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.cancel(cx),
            "enter" => self.upload(cx),
            _ => {}
        }
    }
}

impl Focusable for UploadDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for UploadDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let comment_empty = self.comment.read(cx).value().trim().is_empty();
        let in_flight = matches!(self.state, UploadState::InFlight(_));

        let mut summary_list = v_flex().gap_1();
        if self.summaries.is_empty() {
            summary_list =
                summary_list.child(Label::new(tr!("dialog.upload.no_changes")).text_color(muted));
        } else {
            for s in &self.summaries {
                summary_list =
                    summary_list.child(Label::new(s.describe()).text_color(cx.theme().foreground));
            }
        }

        let mut body = v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(tr!("dialog.upload.pending_changes"))
                            .text_size(muted_text_size())
                            .text_color(muted),
                    )
                    .child(summary_list),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(tr!("dialog.upload.comment_label"))
                            .text_size(muted_text_size())
                            .text_color(muted),
                    )
                    .child(Input::new(&self.comment)),
            );

        match &self.state {
            UploadState::Editing => {}
            UploadState::InFlight(handle) => {
                body = body.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .text_color(cx.theme().foreground)
                        .child(Spinner::new().small())
                        .child(Label::new(handle.snapshot().label())),
                );
            }
            UploadState::Failed(message) => {
                body = body.child(
                    v_flex()
                        .gap_1()
                        .child(
                            Label::new(tr!("dialog.upload.failed"))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(cx.theme().danger),
                        )
                        .child(Label::new(message.clone()).text_color(cx.theme().danger)),
                );
            }
        }

        let confirm_label = if matches!(self.state, UploadState::Failed(_)) {
            tr!("dialog.upload.retry")
        } else {
            tr!("dialog.upload.confirm")
        };
        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("upload-cancel")
                    .label(tr!("dialog.cancel"))
                    .disabled(in_flight)
                    .on_click(cx.listener(|this, _, _w, cx| this.cancel(cx))),
            )
            .child(
                Button::new("upload-confirm")
                    .primary()
                    .label(confirm_label)
                    .disabled(!self.state.confirm_enabled(comment_empty))
                    .on_click(cx.listener(|this, _, _w, cx| this.upload(cx))),
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
                    .child(tr!("dialog.upload.title")),
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
    fn layer_summary_describe_format() {
        let s = LayerSummary {
            layer_name: "Downtown".to_string(),
            created: 2,
            modified: 1,
            deleted: 0,
        };
        assert_eq!(s.describe(), "Downtown: 2 created, 1 modified, 0 deleted");
    }

    fn summary(created: usize) -> LayerSummary {
        LayerSummary {
            layer_name: "L".to_string(),
            created,
            modified: 0,
            deleted: 0,
        }
    }

    #[test]
    fn plan_upload_covers_every_state() {
        // Signed out always gets the sign-in prompt, changes or not.
        assert_eq!(plan_upload(None, &[summary(1)]), UploadPlan::SignIn);
        assert_eq!(plan_upload(None, &[]), UploadPlan::SignIn);
        // Signed in: no layers / no changes -> message; changes -> dialog.
        assert_eq!(plan_upload(Some("me"), &[]), UploadPlan::NothingToUpload);
        assert_eq!(
            plan_upload(Some("me"), &[summary(0)]),
            UploadPlan::NothingToUpload
        );
        assert_eq!(
            plan_upload(Some("me"), &[summary(0), summary(2)]),
            UploadPlan::ShowDialog
        );
    }

    #[test]
    fn confirm_is_disabled_while_the_upload_is_in_flight() {
        use crate::transfer::{TransferHandle, TransferKind, TransferPhase};
        let handle = TransferHandle::new(TransferKind::Upload, TransferPhase::OpeningChangeset);
        let in_flight = UploadState::InFlight(handle);
        assert!(!in_flight.confirm_enabled(false));
        assert!(!in_flight.can_dismiss());

        assert!(UploadState::Editing.confirm_enabled(false));
        assert!(!UploadState::Editing.confirm_enabled(true));
        assert!(UploadState::Editing.can_dismiss());

        let failed = UploadState::Failed("nope".into());
        assert!(
            failed.confirm_enabled(false),
            "a failed upload can be retried"
        );
        assert!(failed.can_dismiss());
    }

    #[test]
    fn layer_summary_is_empty_when_all_zero() {
        let s = LayerSummary {
            layer_name: "L".to_string(),
            created: 0,
            modified: 0,
            deleted: 0,
        };
        assert!(s.is_empty());
        let s2 = LayerSummary {
            layer_name: "L".to_string(),
            created: 1,
            modified: 0,
            deleted: 0,
        };
        assert!(!s2.is_empty());
    }
}
