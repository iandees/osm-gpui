//! Modal dialog to review pending edits and provide a changeset comment
//! before uploading to the OSM API. Mirrors `quit_confirm_dialog.rs`'s
//! scrim/card/focus/Escape conventions and `tag_edit_dialog.rs`'s use of
//! `gpui_component::input::{Input, InputState}` for the comment field.

use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _, Disableable as _,
};

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
        format!(
            "{}: {} created, {} modified, {} deleted",
            self.layer_name, self.created, self.modified, self.deleted
        )
    }
}

pub enum DialogEvent {
    /// User clicked "Upload" with a non-empty comment.
    Upload {
        comment: String,
    },
    Cancelled,
}

pub struct UploadDialog {
    summaries: Vec<LayerSummary>,
    comment: Entity<InputState>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for UploadDialog {}

impl UploadDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, summaries: Vec<LayerSummary>) -> Self {
        let comment = cx
            .new(|cx| InputState::new(window, cx).placeholder("Describe your changes (required)"));
        let focus_handle = cx.focus_handle();
        comment.update(cx, |state, cx| state.focus(window, cx));
        Self {
            summaries,
            comment,
            focus_handle,
        }
    }

    fn comment_text(&self, cx: &Context<Self>) -> String {
        self.comment.read(cx).value().trim().to_string()
    }

    fn upload(&mut self, cx: &mut Context<Self>) {
        let comment = self.comment_text(cx);
        if comment.is_empty() {
            return;
        }
        cx.emit(DialogEvent::Upload { comment });
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
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

        let mut summary_list = v_flex().gap_1();
        if self.summaries.is_empty() {
            summary_list = summary_list.child(Label::new("No pending changes.").text_color(muted));
        } else {
            for s in &self.summaries {
                summary_list =
                    summary_list.child(Label::new(s.describe()).text_color(cx.theme().foreground));
            }
        }

        let body = v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new("Pending changes")
                            .text_size(muted_text_size())
                            .text_color(muted),
                    )
                    .child(summary_list),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new("Changeset comment")
                            .text_size(muted_text_size())
                            .text_color(muted),
                    )
                    .child(Input::new(&self.comment)),
            );

        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("upload-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _w, cx| this.cancel(cx))),
            )
            .child(
                Button::new("upload-confirm")
                    .primary()
                    .label("Upload")
                    .disabled(comment_empty)
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
                    .child("Upload to OpenStreetMap"),
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
