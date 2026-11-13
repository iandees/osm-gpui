//! Shared scaffold for the app's hand-rolled modal dialogs (tag edit, custom
//! imagery, quit confirm, and the settings window's field rows): the
//! full-window scrim, the bordered card chrome (title / body / footer), and
//! the standard Cancel + primary-confirm footer button row.

use gpui::{
    div, prelude::*, AnyView, App, ClickEvent, Div, ElementId, FocusHandle, Hsla, KeyDownEvent,
    Pixels, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _,
};

use crate::ui::style::muted_text_size;

/// A modal dialog the host (`MapViewer::render`) keeps focused while it is
/// open. Dialogs focus themselves when built, but something can take the
/// focus back before the next frame: choosing an item of the in-window menu
/// bar restores focus to whatever held it before the menu opened. The host
/// then calls [`ModalDialog::initial_focus`] again, so Escape, Enter, Tab and
/// typing reach the dialog however it was opened.
pub trait ModalDialog: gpui::Focusable {
    /// The handle that holds focus when the dialog opens: the dialog's own
    /// handle by default, or its first text field.
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        gpui::Focusable::focus_handle(self, cx)
    }
}

/// What the host needs of one open dialog: the handle that contains every
/// control of it, the one to focus when none of them is focused, and the view
/// to mount.
pub struct OpenModal {
    pub container: FocusHandle,
    pub initial: FocusHandle,
    pub view: AnyView,
}

/// Describe `dialog` for [`ensure_modal_focus`] (`None` when it is closed).
pub fn open_modal<T: ModalDialog + gpui::Render + 'static>(
    dialog: &Option<gpui::Entity<T>>,
    cx: &App,
) -> Option<OpenModal> {
    let entity = dialog.as_ref()?;
    let dialog = entity.read(cx);
    Some(OpenModal {
        container: gpui::Focusable::focus_handle(dialog, cx),
        initial: dialog.initial_focus(cx),
        view: entity.clone().into(),
    })
}

/// Give the last of `open` (the one drawn on top) the focus unless focus is
/// already inside one of them. Returns whether focus was moved.
pub fn ensure_modal_focus(open: &[OpenModal], window: &mut Window, cx: &mut App) -> bool {
    let Some(top) = open.last() else {
        return false;
    };
    // The first frame after a dialog opens has no dispatch tree for it yet,
    // so a focused first field is checked directly too.
    if open
        .iter()
        .any(|m| m.container.contains_focused(window, cx) || m.initial.is_focused(window))
    {
        return false;
    }
    window.focus(&top.initial, cx);
    true
}

/// Wrap `content` (typically a [`dialog_frame`]) in the shared modal scrim: a
/// full-window, click-occluding, semi-transparent backdrop that centers its
/// child and dispatches key-down events (Escape/Enter) via `on_key_down`.
///
/// This is the `div().track_focus(...).on_key_down(...).absolute().inset_0()
/// .occlude().bg(scrim_color()).flex().justify_center().items_center()`
/// pattern previously duplicated in every dialog's `render`.
pub fn scrim(
    focus_handle: &FocusHandle,
    on_key_down: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
    content: impl IntoElement,
) -> Div {
    div()
        .track_focus(focus_handle)
        .on_key_down(on_key_down)
        .absolute()
        .inset_0()
        .occlude()
        .bg(crate::ui::style::scrim_color())
        .flex()
        .justify_center()
        .items_center()
        .child(content)
}

/// Build the bordered/rounded card chrome shared by every dialog: a bold
/// title header, a padded body, and a footer row (typically built by
/// [`footer_row`]), sized to `width`.
pub fn dialog_frame(
    cx: &App,
    width: Pixels,
    title: impl Into<SharedString>,
    body: impl IntoElement,
    footer: impl IntoElement,
) -> Div {
    v_flex()
        .w(width)
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
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .child(div().p_4().child(body))
        .child(
            div()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(footer),
        )
}

/// The standard dialog footer: a right-aligned "Cancel" button followed by a
/// primary confirm button (e.g. "Save", "Add", "Quit").
pub fn footer_row(
    cancel_id: impl Into<ElementId>,
    cancel_label: impl Into<SharedString>,
    on_cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    confirm_id: impl Into<ElementId>,
    confirm_label: impl Into<SharedString>,
    on_confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .flex_row()
        .justify_end()
        .gap_2()
        .child(
            Button::new(cancel_id)
                .label(cancel_label)
                .on_click(on_cancel),
        )
        .child(
            Button::new(confirm_id)
                .primary()
                .label(confirm_label)
                .on_click(on_confirm),
        )
}

/// A labeled input row: a small muted label above the input widget. Shared
/// by every dialog's form body and by the settings window's inline edit
/// forms.
pub fn field_row(
    label: impl Into<SharedString>,
    input: &gpui::Entity<InputState>,
    muted: Hsla,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            Label::new(label)
                .text_size(muted_text_size())
                .text_color(muted),
        )
        .child(Input::new(input))
}

/// The two keys every modal dialog reacts to on key-down.
pub enum ModalKey {
    Escape,
    Enter,
    Other,
}

/// Classify a key-down event's key as [`ModalKey::Escape`],
/// [`ModalKey::Enter`], or [`ModalKey::Other`].
///
/// Each dialog still defines its own tiny `on_key_down` method (rather than
/// sharing one dispatcher parameterized by cancel/submit callbacks): gpui's
/// `cx.listener` closures each capture `&mut Self` uniquely, so passing both
/// a cancel closure and a confirm closure into one shared function at the
/// same time would require two simultaneous mutable borrows of `self` for a
/// single call site, which the borrow checker rejects. Sharing just the key
/// classification avoids that gymnastics while still removing the
/// duplicated string match.
pub fn classify_key(ev: &KeyDownEvent) -> ModalKey {
    match ev.keystroke.key.as_str() {
        "escape" => ModalKey::Escape,
        "enter" => ModalKey::Enter,
        _ => ModalKey::Other,
    }
}
