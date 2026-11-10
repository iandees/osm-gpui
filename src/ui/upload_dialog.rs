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

use crate::osm_upload::{ConflictKind, UploadConflict};
use crate::transfer::TransferHandle;
use crate::ui::style::muted_text_size;
use crate::{tr, tr_count};

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
    /// User picked a way forward from a refused upload.
    Conflict {
        action: ConflictAction,
        conflict: UploadConflict,
    },
}

/// What the user can do about a refused upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    /// Overwrite the server's newer version with the local object.
    KeepMine,
    /// Replace the local object with the server's, dropping the local change.
    UseTheirs,
    /// The server already deleted what the user deleted: forget that delete.
    DropMyChange,
    /// Select the object on the map and zoom to it.
    Show,
}

impl ConflictAction {
    fn label(self) -> SharedString {
        match self {
            ConflictAction::KeepMine => tr!("dialog.upload.conflict.keep_mine"),
            ConflictAction::UseTheirs => tr!("dialog.upload.conflict.use_theirs"),
            ConflictAction::DropMyChange => tr!("dialog.upload.conflict.drop_mine"),
            ConflictAction::Show => tr!("dialog.upload.conflict.show"),
        }
    }
}

/// A refused upload together with what the editor knows about the object it
/// names, which decides the offered actions.
#[derive(Debug, Clone)]
pub struct ConflictView {
    pub conflict: UploadConflict,
    /// The object is in the loaded data (so Show can find it).
    pub located: bool,
    /// The user deleted the object locally.
    pub deleted_locally: bool,
}

impl ConflictView {
    /// The actions to offer, in button order.
    ///
    /// - A version mismatch on a known object: keep mine / use theirs.
    /// - The server already deleted an object the user also deleted: drop my
    ///   change (nothing is left to show).
    /// - Everything else is explained, with Show when the object is loaded.
    pub fn actions(&self) -> Vec<ConflictAction> {
        let mut actions = Vec::new();
        let named = self.conflict.object.is_some();
        match &self.conflict.kind {
            ConflictKind::VersionMismatch { .. } if named => {
                actions.push(ConflictAction::KeepMine);
                actions.push(ConflictAction::UseTheirs);
            }
            ConflictKind::AlreadyDeleted if named && self.deleted_locally => {
                actions.push(ConflictAction::DropMyChange);
            }
            _ => {}
        }
        if self.located {
            actions.push(ConflictAction::Show);
        }
        actions
    }

    /// One line on what the choices mean, if there is a choice.
    fn hint(&self) -> Option<SharedString> {
        match &self.conflict.kind {
            ConflictKind::VersionMismatch { .. } if self.conflict.object.is_some() => {
                Some(tr!("dialog.upload.conflict.hint_version"))
            }
            ConflictKind::AlreadyDeleted if self.deleted_locally => {
                Some(tr!("dialog.upload.conflict.hint_deleted_both"))
            }
            ConflictKind::AlreadyDeleted => Some(tr!("dialog.upload.conflict.hint_deleted")),
            ConflictKind::StillInUse { .. } | ConflictKind::MissingDependency => {
                Some(tr!("dialog.upload.conflict.hint_related"))
            }
            _ => None,
        }
    }
}

/// What the upload dialog is showing right now, for the script harness and
/// tests.
#[derive(Debug, Clone)]
pub enum Phase {
    /// Reviewing the changes, ready for a comment and the Upload button.
    Editing,
    /// An upload is running, or a conflict is being resolved.
    Busy,
    /// The upload stopped: with the server's refusal when it was one.
    Failed(Option<ConflictView>),
}

/// An upload that stopped: the message, and for a refusal by the server the
/// conflict with its actions.
struct Failure {
    message: SharedString,
    conflict: Option<ConflictView>,
    /// Why the last attempt to resolve the conflict didn't work, if it didn't.
    resolve_error: Option<SharedString>,
}

/// Where the dialog is in the upload: reviewing, sending, or stopped by an error.
enum UploadState {
    Editing,
    /// The upload is running; the stage comes from the shared handle.
    InFlight(TransferHandle),
    /// The upload failed; the message is shown and the user may retry.
    Failed(Failure),
    /// A conflict is being resolved (asking the server); the label says how.
    Resolving(SharedString),
}

impl UploadState {
    /// Whether the confirm button can be pressed. Disabled while sending so a
    /// second upload can't start.
    fn confirm_enabled(&self, comment_empty: bool) -> bool {
        !comment_empty && !self.is_busy()
    }

    /// Whether Cancel/Esc closes the dialog. An upload in flight can't be
    /// abandoned: a half-applied changeset is worse than waiting.
    fn can_dismiss(&self) -> bool {
        !self.is_busy()
    }

    fn is_busy(&self) -> bool {
        matches!(self, UploadState::InFlight(_) | UploadState::Resolving(_))
    }
}

pub struct UploadDialog {
    summaries: Vec<LayerSummary>,
    comment: Entity<InputState>,
    focus_handle: FocusHandle,
    state: UploadState,
    /// Shown while editing: what was just done about a conflict.
    notice: Option<SharedString>,
    /// How many changesets the pending changes will be sent as; the dialog
    /// says so before uploading when it is more than one.
    changesets: usize,
    /// After a stop part way through a multi-changeset upload: how many of
    /// how many changesets the server accepted.
    partial: Option<(usize, usize)>,
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
            notice: None,
            changesets: 1,
            partial: None,
        }
    }

    /// Say how many changesets the pending changes will be sent as (shown
    /// when more than one).
    pub fn set_changesets(&mut self, count: usize, cx: &mut Context<Self>) {
        self.changesets = count;
        cx.notify();
    }

    /// The edits still pending after part of an upload went through: the new
    /// per-layer counts and how many changesets they will take.
    pub fn set_pending(
        &mut self,
        summaries: Vec<LayerSummary>,
        changesets: usize,
        cx: &mut Context<Self>,
    ) {
        self.summaries = summaries;
        self.set_changesets(changesets, cx);
    }

    /// Record that an upload stopped after `done` of `total` changesets (call
    /// before `fail` / `fail_with_conflict`); `None` clears it.
    pub fn set_partial(&mut self, progress: Option<(usize, usize)>) {
        self.partial = progress;
    }

    /// Show the running upload's stage (read from `handle` on each render).
    pub fn begin_transfer(&mut self, handle: TransferHandle, cx: &mut Context<Self>) {
        self.state = UploadState::InFlight(handle);
        self.notice = None;
        self.partial = None;
        cx.notify();
    }

    /// Show why the upload stopped and let the user try again.
    pub fn fail(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.state = UploadState::Failed(Failure {
            message,
            conflict: None,
            resolve_error: None,
        });
        cx.notify();
    }

    /// Show a refused upload: what happened in plain words, and the ways
    /// forward `view` allows.
    pub fn fail_with_conflict(&mut self, view: ConflictView, cx: &mut Context<Self>) {
        self.state = UploadState::Failed(Failure {
            message: view.conflict.describe().into(),
            conflict: Some(view),
            resolve_error: None,
        });
        cx.notify();
    }

    /// The chosen way forward is being carried out (`label` says how).
    pub fn begin_resolving(&mut self, label: SharedString, cx: &mut Context<Self>) {
        self.state = UploadState::Resolving(label);
        cx.notify();
    }

    /// The conflict is dealt with: back to the review, ready for another
    /// Upload, with `notice` saying what was done.
    pub fn resolved(&mut self, notice: SharedString, cx: &mut Context<Self>) {
        self.state = UploadState::Editing;
        self.notice = Some(notice);
        cx.notify();
    }

    /// Resolving didn't work (`error` says why): show the conflict again.
    pub fn resolve_failed(
        &mut self,
        view: ConflictView,
        error: SharedString,
        cx: &mut Context<Self>,
    ) {
        self.state = UploadState::Failed(Failure {
            message: view.conflict.describe().into(),
            conflict: Some(view),
            resolve_error: Some(error),
        });
        cx.notify();
    }

    /// The comment as typed (not trimmed), so the app can keep it when the
    /// dialog closes without uploading.
    pub fn draft_comment(&self, cx: &App) -> String {
        self.comment.read(cx).value().to_string()
    }

    /// Put a previously kept comment back in the field.
    pub fn set_draft_comment(&self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.comment
            .update(cx, |state, cx| state.set_value(text, window, cx));
    }

    /// What the dialog is showing (see `Phase`).
    pub fn phase(&self) -> Phase {
        match &self.state {
            UploadState::Editing => Phase::Editing,
            UploadState::InFlight(_) | UploadState::Resolving(_) => Phase::Busy,
            UploadState::Failed(failure) => Phase::Failed(failure.conflict.clone()),
        }
    }

    /// Why pressing the Upload / Try again button would do nothing right now,
    /// or `None` when it would go ahead.
    pub fn confirm_blocker(&self, cx: &App) -> Option<&'static str> {
        let empty = self.comment.read(cx).value().trim().is_empty();
        if self.state.is_busy() {
            Some("an upload is already running")
        } else if empty {
            Some("the changeset comment is empty (type one first)")
        } else {
            None
        }
    }

    /// Press the Upload / Try again button (what a click does; see
    /// `confirm_blocker` for when that is a no-op).
    pub fn press_confirm(&mut self, cx: &mut Context<Self>) {
        self.upload(cx);
    }

    /// Press the Cancel button (what a click does; a no-op while busy).
    pub fn press_cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel(cx);
    }

    /// Whether the conflict dialog currently offers `action` as a button.
    pub fn offers(&self, action: ConflictAction) -> bool {
        match &self.state {
            UploadState::Failed(Failure {
                conflict: Some(view),
                ..
            }) => view.actions().contains(&action),
            _ => false,
        }
    }

    /// Press one of the conflict buttons (what a click does; see `offers`).
    pub fn press_conflict(&mut self, action: ConflictAction, cx: &mut Context<Self>) {
        self.resolve(action, cx);
    }

    fn comment_text(&self, cx: &Context<Self>) -> String {
        self.comment.read(cx).value().trim().to_string()
    }

    fn upload(&mut self, cx: &mut Context<Self>) {
        let comment = self.comment_text(cx);
        if !self.state.confirm_enabled(comment.is_empty()) {
            return;
        }
        self.notice = None;
        cx.emit(DialogEvent::Upload { comment });
    }

    fn resolve(&mut self, action: ConflictAction, cx: &mut Context<Self>) {
        let UploadState::Failed(Failure {
            conflict: Some(view),
            ..
        }) = &self.state
        else {
            return;
        };
        cx.emit(DialogEvent::Conflict {
            action,
            conflict: view.conflict.clone(),
        });
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
        let in_flight = self.state.is_busy();

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

        let busy_row = |label: SharedString| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_color(cx.theme().foreground)
                .child(Spinner::new().small())
                .child(Label::new(label))
        };
        match &self.state {
            UploadState::Editing => {
                if self.changesets > 1 {
                    body = body.child(
                        Label::new(tr_count!("dialog.upload.large", self.changesets))
                            .text_color(cx.theme().foreground),
                    );
                }
                if let Some(notice) = &self.notice {
                    body = body.child(Label::new(notice.clone()).text_color(cx.theme().foreground));
                }
            }
            UploadState::InFlight(handle) => {
                body = body.child(busy_row(handle.snapshot().label()));
            }
            UploadState::Resolving(label) => {
                body = body.child(busy_row(label.clone()));
            }
            UploadState::Failed(failure) => {
                let mut failed = v_flex()
                    .gap_2()
                    .child(
                        Label::new(tr!("dialog.upload.failed"))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(cx.theme().danger),
                    )
                    .child(Label::new(failure.message.clone()).text_color(cx.theme().danger));
                if let Some((done, total)) = self.partial.filter(|&(done, _)| done > 0) {
                    failed = failed.child(
                        Label::new(tr!("dialog.upload.partial", done = done, total = total))
                            .text_color(cx.theme().foreground),
                    );
                }
                if let Some(view) = &failure.conflict {
                    if let Some(hint) = view.hint() {
                        failed = failed.child(
                            Label::new(hint)
                                .text_size(muted_text_size())
                                .text_color(muted),
                        );
                    }
                    let actions = view.actions();
                    if !actions.is_empty() {
                        let mut row = div().flex().flex_row().flex_wrap().gap_2();
                        for action in actions {
                            row = row.child(
                                Button::new(SharedString::from(format!(
                                    "upload-conflict-{action:?}"
                                )))
                                .label(action.label())
                                .small()
                                .on_click(
                                    cx.listener(move |this, _, _w, cx| this.resolve(action, cx)),
                                ),
                            );
                        }
                        failed = failed.child(row);
                    }
                }
                if let Some(error) = &failure.resolve_error {
                    failed = failed.child(Label::new(error.clone()).text_color(cx.theme().danger));
                }
                body = body.child(failed);
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
    use crate::selection::FeatureKind;

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

        let failed = UploadState::Failed(Failure {
            message: "nope".into(),
            conflict: None,
            resolve_error: None,
        });
        assert!(
            failed.confirm_enabled(false),
            "a failed upload can be retried"
        );
        assert!(failed.can_dismiss());

        let resolving = UploadState::Resolving("asking".into());
        assert!(!resolving.confirm_enabled(false));
        assert!(!resolving.can_dismiss());
    }

    fn view(
        kind: ConflictKind,
        object: Option<(FeatureKind, i64)>,
        located: bool,
        deleted_locally: bool,
    ) -> ConflictView {
        ConflictView {
            conflict: UploadConflict {
                kind,
                object,
                status: 409,
                message: String::new(),
            },
            located,
            deleted_locally,
        }
    }

    #[test]
    fn conflict_actions_follow_the_kind_of_conflict() {
        use ConflictAction::*;
        let node = Some((FeatureKind::Node, 12345));
        let mismatch = ConflictKind::VersionMismatch {
            provided: 3,
            server: 4,
        };
        assert_eq!(
            view(mismatch.clone(), node, true, false).actions(),
            vec![KeepMine, UseTheirs, Show]
        );
        // Deleted here, so there is nothing on the map to show.
        assert_eq!(
            view(mismatch.clone(), node, false, true).actions(),
            vec![KeepMine, UseTheirs]
        );
        assert!(view(mismatch, None, false, false).actions().is_empty());

        assert_eq!(
            view(ConflictKind::AlreadyDeleted, node, false, true).actions(),
            vec![DropMyChange]
        );
        // Deleted on the server but edited here: explained, shown, no fix.
        assert_eq!(
            view(ConflictKind::AlreadyDeleted, node, true, false).actions(),
            vec![Show]
        );
        assert_eq!(
            view(ConflictKind::StillInUse { by: vec![] }, node, true, false).actions(),
            vec![Show]
        );
        assert_eq!(
            view(ConflictKind::MissingDependency, node, true, false).actions(),
            vec![Show]
        );
        assert!(view(ConflictKind::Other, None, false, false)
            .actions()
            .is_empty());
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
