//! Shared scaffold for the app's hand-rolled modal dialogs (and the settings
//! window's field rows): the full-window scrim, the bordered card chrome
//! (title / body / footer), and the standard Cancel + primary-confirm footer
//! button row.

use gpui::{
    div, prelude::*, AnyElement, AnyView, App, ClickEvent, Div, ElementId, FocusHandle, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, Pixels, SharedString, Window,
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

/// Like [`scrim`], and a press on the backdrop outside the card also calls
/// `on_backdrop_click` (a press on the card itself does not).
pub fn scrim_dismissing(
    focus_handle: &FocusHandle,
    on_key_down: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
    on_backdrop_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    content: impl IntoElement,
) -> Div {
    scrim(
        focus_handle,
        on_key_down,
        div()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(content),
    )
    .on_mouse_down(MouseButton::Left, on_backdrop_click)
}

/// The card chrome every dialog shares: a title header, a padded body and an
/// optional footer, sized to `width`. [`dialog_frame`] is the common case (a
/// footer and nothing else); build one with [`card`] when a dialog needs a
/// header control, a scrolling body or no footer.
pub struct Card {
    width: Pixels,
    title: SharedString,
    body: AnyElement,
    header_end: Option<AnyElement>,
    footer: Option<AnyElement>,
    scroll: Option<(ElementId, Pixels)>,
    background: Hsla,
    border: Hsla,
    foreground: Hsla,
}

pub fn card(
    cx: &App,
    width: Pixels,
    title: impl Into<SharedString>,
    body: impl IntoElement,
) -> Card {
    Card {
        width,
        title: title.into(),
        body: body.into_any_element(),
        header_end: None,
        footer: None,
        scroll: None,
        background: cx.theme().popover,
        border: cx.theme().border,
        foreground: cx.theme().foreground,
    }
}

impl Card {
    /// The footer row (typically built by [`footer_row`]).
    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    /// A control at the right end of the title row (e.g. a close button).
    pub fn header_end(mut self, element: impl IntoElement) -> Self {
        self.header_end = Some(element.into_any_element());
        self
    }

    /// Let the body scroll once it is taller than `max_height`, so the title
    /// and footer stay on screen.
    pub fn scroll_body(mut self, id: impl Into<ElementId>, max_height: Pixels) -> Self {
        self.scroll = Some((id.into(), max_height));
        self
    }

    pub fn build(self) -> Div {
        let border = self.border;
        let body = match self.scroll {
            Some((id, max_height)) => div()
                .id(id)
                .p_4()
                .max_h(max_height)
                .overflow_y_scroll()
                .child(self.body)
                .into_any_element(),
            None => div().p_4().child(self.body).into_any_element(),
        };
        let has_header_end = self.header_end.is_some();
        v_flex()
            .w(self.width)
            .bg(self.background)
            .border_1()
            .border_color(border)
            .rounded_lg()
            .shadow_lg()
            .child(
                div()
                    .when(has_header_end, |el| {
                        el.flex().items_center().justify_between()
                    })
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(border)
                    .text_color(self.foreground)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(self.title)
                    .children(self.header_end),
            )
            .child(body)
            .children(self.footer.map(|footer| {
                div()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(border)
                    .child(footer)
            }))
    }
}

/// The bordered/rounded card shared by most dialogs: title header, padded
/// body and a footer row, sized to `width`.
pub fn dialog_frame(
    cx: &App,
    width: Pixels,
    title: impl Into<SharedString>,
    body: impl IntoElement,
    footer: impl IntoElement,
) -> Div {
    card(cx, width, title, body).footer(footer).build()
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
    footer_buttons(
        Button::new(cancel_id)
            .label(cancel_label)
            .on_click(on_cancel),
        Button::new(confirm_id)
            .primary()
            .label(confirm_label)
            .on_click(on_confirm),
    )
}

/// The footer row around two buttons the caller built itself (for a button
/// that is sometimes disabled).
pub fn footer_buttons(cancel: Button, confirm: Button) -> Div {
    div()
        .flex()
        .flex_row()
        .justify_end()
        .gap_2()
        .child(cancel)
        .child(confirm)
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalKey {
    Escape,
    Enter,
    Other,
}

/// Where the highlighted row of a result list goes when `key` ("up" or
/// "down") is pressed with `count` rows: `None` when the key does not move
/// it (or there is nothing to move over).
pub fn move_selection(selected: usize, count: usize, key: &str) -> Option<usize> {
    match key {
        "down" if count > 0 => Some((selected + 1).min(count - 1)),
        "up" => Some(selected.saturating_sub(1)),
        _ => None,
    }
}

/// Classify a key-down event's key as [`ModalKey::Escape`],
/// [`ModalKey::Enter`], or [`ModalKey::Other`].
///
/// Each dialog defines its own tiny `on_key_down` rather than sharing one
/// dispatcher parameterized by cancel/submit callbacks: `cx.listener`
/// closures each capture `&mut Self` uniquely, so passing both a cancel and
/// a confirm closure to one shared function would need two simultaneous
/// mutable borrows of `self`. Only the key classification is shared.
pub fn classify_key(ev: &KeyDownEvent) -> ModalKey {
    match ev.keystroke.key.as_str() {
        "escape" => ModalKey::Escape,
        "enter" => ModalKey::Enter,
        _ => ModalKey::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_moves_within_the_list() {
        assert_eq!(move_selection(0, 3, "down"), Some(1));
        assert_eq!(move_selection(2, 3, "down"), Some(2));
        assert_eq!(move_selection(2, 3, "up"), Some(1));
        assert_eq!(move_selection(0, 3, "up"), Some(0));
    }

    #[test]
    fn selection_stays_put_without_rows_or_for_other_keys() {
        assert_eq!(move_selection(0, 0, "down"), None);
        assert_eq!(move_selection(1, 3, "left"), None);
    }
}
