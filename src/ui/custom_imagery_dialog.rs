//! Modal dialog to add a user-defined custom imagery layer, plus the validation
//! helpers the dialog and its tests share.

use crate::custom_imagery_store::CustomImageryEntry;
use crate::ui::modal::Modal;
use crate::ui::text_input::TextInput;
use gpui::{
    div, prelude::*, rgb, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, MouseDownEvent, SharedString, Window,
};

#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    NameEmpty,
    TemplateEmpty,
    TemplateMissingPlaceholder,
    TemplateYAndMinusY,
    MinZoomInvalid,
    MaxZoomInvalid,
    MinZoomAboveMax,
}

/// Validate raw form fields (already trimmed by the caller) and return a
/// normalised `CustomImageryEntry` on success.
pub fn validate(
    name: &str,
    url_template: &str,
    min_zoom_raw: &str,
    max_zoom_raw: &str,
) -> Result<CustomImageryEntry, ValidationError> {
    if name.trim().is_empty() {
        return Err(ValidationError::NameEmpty);
    }
    let template = url_template.trim();
    if template.is_empty() {
        return Err(ValidationError::TemplateEmpty);
    }
    let has_z = template.contains("{z}");
    let has_x = template.contains("{x}");
    let has_y = template.contains("{y}");
    let has_minus_y = template.contains("{-y}");
    if !has_z || !has_x || (!has_y && !has_minus_y) {
        return Err(ValidationError::TemplateMissingPlaceholder);
    }
    if has_y && has_minus_y {
        return Err(ValidationError::TemplateYAndMinusY);
    }
    let min_zoom = parse_zoom(min_zoom_raw, 0).map_err(|_| ValidationError::MinZoomInvalid)?;
    let max_zoom = parse_zoom(max_zoom_raw, 19).map_err(|_| ValidationError::MaxZoomInvalid)?;
    if min_zoom > max_zoom {
        return Err(ValidationError::MinZoomAboveMax);
    }
    Ok(CustomImageryEntry {
        name: name.trim().to_string(),
        url_template: template.to_string(),
        min_zoom,
        max_zoom,
    })
}

fn parse_zoom(raw: &str, default_if_blank: u32) -> Result<u32, ()> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(default_if_blank);
    }
    let v: u32 = s.parse().map_err(|_| ())?;
    if v > 24 {
        return Err(());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TMPL: &str = "https://tile.example.com/{z}/{x}/{y}.png";

    #[test]
    fn happy_path_defaults() {
        let e = validate("Example", TMPL, "", "").unwrap();
        assert_eq!(e.name, "Example");
        assert_eq!(e.url_template, TMPL);
        assert_eq!(e.min_zoom, 0);
        assert_eq!(e.max_zoom, 19);
    }

    #[test]
    fn happy_path_minus_y() {
        let e = validate(
            "Foo",
            "https://tile.example.com/{z}/{x}/{-y}.png",
            "4",
            "18",
        )
        .unwrap();
        assert_eq!(e.min_zoom, 4);
        assert_eq!(e.max_zoom, 18);
    }

    #[test]
    fn name_must_be_nonempty() {
        assert_eq!(validate("  ", TMPL, "", ""), Err(ValidationError::NameEmpty));
    }

    #[test]
    fn template_required() {
        assert_eq!(
            validate("Example", "  ", "", ""),
            Err(ValidationError::TemplateEmpty)
        );
    }

    #[test]
    fn template_missing_z_x_y() {
        assert_eq!(
            validate("Example", "https://example.com/a/b/c.png", "", ""),
            Err(ValidationError::TemplateMissingPlaceholder)
        );
    }

    #[test]
    fn template_cannot_contain_both_y_variants() {
        assert_eq!(
            validate(
                "Example",
                "https://example.com/{z}/{x}/{y}/{-y}.png",
                "",
                ""
            ),
            Err(ValidationError::TemplateYAndMinusY)
        );
    }

    #[test]
    fn min_above_max_rejected() {
        assert_eq!(
            validate("Example", TMPL, "15", "10"),
            Err(ValidationError::MinZoomAboveMax)
        );
    }

    #[test]
    fn out_of_range_zoom_rejected() {
        assert_eq!(
            validate("Example", TMPL, "25", ""),
            Err(ValidationError::MinZoomInvalid)
        );
        assert_eq!(
            validate("Example", TMPL, "", "99"),
            Err(ValidationError::MaxZoomInvalid)
        );
    }

    #[test]
    fn non_numeric_zoom_rejected() {
        assert_eq!(
            validate("Example", TMPL, "abc", ""),
            Err(ValidationError::MinZoomInvalid)
        );
    }
}

// ---------------------------------------------------------------------------
// Dialog entity
// ---------------------------------------------------------------------------

pub enum DialogEvent {
    Submitted(CustomImageryEntry),
    Cancelled,
}

pub struct CustomImageryDialog {
    name: Entity<TextInput>,
    url_template: Entity<TextInput>,
    min_zoom: Entity<TextInput>,
    max_zoom: Entity<TextInput>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    /// True on the first render pass — the name field is focused then cleared.
    needs_focus: bool,
}

impl EventEmitter<DialogEvent> for CustomImageryDialog {}

impl CustomImageryDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextInput::new(cx, "My imagery"));
        let url_template = cx.new(|cx| TextInput::new(cx, "https://…/{z}/{x}/{y}.png"));
        let min_zoom = cx.new(|cx| TextInput::new(cx, "0"));
        let max_zoom = cx.new(|cx| TextInput::new(cx, "19"));
        let focus_handle = cx.focus_handle();
        // Focus the name field on open.
        let name_handle = name.read(cx).focus_handle(cx);
        name_handle.focus(window, cx);
        Self {
            name,
            url_template,
            min_zoom,
            max_zoom,
            error: None,
            focus_handle,
            needs_focus: false,
        }
    }

    /// Constructor that defers the initial focus to the first render pass.
    /// Use this when a `Window` reference is not available at creation time.
    pub fn new_deferred(cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextInput::new(cx, "My imagery"));
        let url_template = cx.new(|cx| TextInput::new(cx, "https://…/{z}/{x}/{y}.png"));
        let min_zoom = cx.new(|cx| TextInput::new(cx, "0"));
        let max_zoom = cx.new(|cx| TextInput::new(cx, "19"));
        let focus_handle = cx.focus_handle();
        Self {
            name,
            url_template,
            min_zoom,
            max_zoom,
            error: None,
            focus_handle,
            needs_focus: true,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).content().to_string();
        let tmpl = self.url_template.read(cx).content().to_string();
        let minz = self.min_zoom.read(cx).content().to_string();
        let maxz = self.max_zoom.read(cx).content().to_string();
        match validate(&name, &tmpl, &minz, &maxz) {
            Ok(entry) => {
                self.error = None;
                cx.emit(DialogEvent::Submitted(entry));
            }
            Err(e) => {
                self.error = Some(error_message(&e).into());
                cx.notify();
            }
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        let m = &ev.keystroke.modifiers;
        match key {
            "escape" => self.cancel(cx),
            "enter" => self.submit(cx),
            "tab" => {
                let order = [
                    self.name.read(cx).focus_handle(cx),
                    self.url_template.read(cx).focus_handle(cx),
                    self.min_zoom.read(cx).focus_handle(cx),
                    self.max_zoom.read(cx).focus_handle(cx),
                ];
                cycle_focus(&order, m.shift, window, cx);
            }
            _ => {}
        }
    }
}

pub fn error_message(e: &ValidationError) -> &'static str {
    match e {
        ValidationError::NameEmpty => "Name is required.",
        ValidationError::TemplateEmpty => "URL template is required.",
        ValidationError::TemplateMissingPlaceholder => {
            "URL template must contain {z}, {x}, and {y} (or {-y})."
        }
        ValidationError::TemplateYAndMinusY => {
            "URL template must use {y} or {-y}, not both."
        }
        ValidationError::MinZoomInvalid => "Min zoom must be a whole number from 0 to 24.",
        ValidationError::MaxZoomInvalid => "Max zoom must be a whole number from 0 to 24.",
        ValidationError::MinZoomAboveMax => "Min zoom must be ≤ max zoom.",
    }
}

fn cycle_focus(order: &[FocusHandle], reverse: bool, window: &mut Window, cx: &mut App) {
    let focused_idx = order
        .iter()
        .position(|h| h.is_focused(window))
        .unwrap_or(0);
    let next = if reverse {
        (focused_idx + order.len() - 1) % order.len()
    } else {
        (focused_idx + 1) % order.len()
    };
    order[next].focus(window, cx);
}

impl Focusable for CustomImageryDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CustomImageryDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Deferred focus: focus the name field on the first render pass when
        // the dialog was created without a Window reference.
        if self.needs_focus {
            self.needs_focus = false;
            let name_handle = self.name.read(cx).focus_handle(cx);
            name_handle.focus(window, cx);
        }

        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(field_row("Name", self.name.clone()))
            .child(field_row("URL template", self.url_template.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .child(div().flex_1().child(field_row("Min zoom", self.min_zoom.clone())))
                    .child(div().flex_1().child(field_row("Max zoom", self.max_zoom.clone()))),
            )
            .children(self.error.clone().map(|msg| {
                div().text_color(rgb(0xf87171)).text_sm().child(msg)
            }));

        let add = cx.listener(|this, _: &MouseDownEvent, _w, cx| this.submit(cx));
        let cancel = cx.listener(|this, _: &MouseDownEvent, _w, cx| this.cancel(cx));
        let footer = div()
            .flex()
            .flex_row()
            .gap_2()
            .child(button("Cancel").on_mouse_down(MouseButton::Left, cancel))
            .child(button_primary("Add").on_mouse_down(MouseButton::Left, add));

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .absolute()
            .inset_0()
            .child(Modal::new("Custom Imagery", body, footer))
    }
}

fn field_row(label: &'static str, input: Entity<TextInput>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_color(rgb(0x9ca3af)).text_xs().child(label))
        .child(input)
}

fn button(label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .px_3()
        .py_1()
        .bg(rgb(0x1f2937))
        .border_1()
        .border_color(rgb(0x374151))
        .rounded_sm()
        .text_color(rgb(0xffffff))
        .text_sm()
        .cursor_pointer()
        .child(label)
}

fn button_primary(label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .px_3()
        .py_1()
        .bg(rgb(0x2563eb))
        .border_1()
        .border_color(rgb(0x1d4ed8))
        .rounded_sm()
        .text_color(rgb(0xffffff))
        .text_sm()
        .cursor_pointer()
        .child(label)
}
