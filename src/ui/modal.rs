//! Shared scaffold for the app's hand-rolled modal dialogs (tag edit, custom
//! imagery, quit confirm, and the settings window's field rows): the
//! full-window scrim, the bordered card chrome (title / body / footer), and
//! the standard Cancel + primary-confirm footer button row.

use gpui::{div, prelude::*, rgba, App, ClickEvent, Div, ElementId, FocusHandle, Hsla, KeyDownEvent, Pixels, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _,
};

/// Wrap `content` (typically a [`dialog_frame`]) in the shared modal scrim: a
/// full-window, click-occluding, semi-transparent backdrop that centers its
/// child and dispatches key-down events (Escape/Enter) via `on_key_down`.
///
/// This is the `div().track_focus(...).on_key_down(...).absolute().inset_0()
/// .occlude().bg(rgba(0x00000099)).flex().justify_center().items_center()`
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
        .bg(rgba(0x00000099))
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
                .font_weight(gpui::FontWeight::BOLD)
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
        .child(Label::new(label).text_xs().text_color(muted))
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
