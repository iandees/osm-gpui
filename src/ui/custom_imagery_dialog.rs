//! Modal dialog to add or edit a user-defined custom imagery source, plus the
//! validation helpers the dialog and its tests share. The Imagery menu and the
//! Settings window both use it.

use crate::custom_imagery_store::CustomImageryEntry;
use crate::tr;
use crate::ui::modal;
use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::{input::InputState, label::Label, v_flex, ActiveTheme as _};

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
    // A box-rendering server (WMS, ArcGIS export) needs no tile indices.
    let has_bbox = template.contains(crate::tiles::BBOX_3857_PLACEHOLDER);
    if !has_bbox && (!has_z || !has_x || (!has_y && !has_minus_y)) {
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
        attribution: None,
        tile_size: crate::layers::tile_layer::STANDARD_TILE_SIZE,
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

pub enum DialogEvent {
    Submitted(CustomImageryEntry),
    Cancelled,
}

pub struct CustomImageryDialog {
    name: Entity<InputState>,
    url_template: Entity<InputState>,
    min_zoom: Entity<InputState>,
    max_zoom: Entity<InputState>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    /// The entry being edited, whose attribution and tile size the form does
    /// not show and so keeps; `None` when adding.
    editing: Option<CustomImageryEntry>,
}

impl EventEmitter<DialogEvent> for CustomImageryDialog {}

impl CustomImageryDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr!("dialog.custom_imagery.name_placeholder"))
        });
        let url_template = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(3, 8)
                .placeholder("https://…/{z}/{x}/{y}.png")
        });
        let min_zoom = cx.new(|cx| InputState::new(window, cx).placeholder("0"));
        let max_zoom = cx.new(|cx| InputState::new(window, cx).placeholder("19"));
        let focus_handle = cx.focus_handle();
        name.update(cx, |state, cx| state.focus(window, cx));
        Self {
            name,
            url_template,
            min_zoom,
            max_zoom,
            error: None,
            focus_handle,
            editing: None,
        }
    }

    /// The dialog filled in with `entry`, to change it.
    pub fn edit(entry: &CustomImageryEntry, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut dialog = Self::new(window, cx);
        dialog.name.update(cx, |state, cx| {
            state.set_value(entry.name.clone(), window, cx)
        });
        dialog.url_template.update(cx, |state, cx| {
            state.set_value(entry.url_template.clone(), window, cx)
        });
        dialog.min_zoom.update(cx, |state, cx| {
            state.set_value(entry.min_zoom.to_string(), window, cx)
        });
        dialog.max_zoom.update(cx, |state, cx| {
            state.set_value(entry.max_zoom.to_string(), window, cx)
        });
        dialog.editing = Some(entry.clone());
        dialog
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().to_string();
        // A URL has no line breaks; the box wraps long ones for editing.
        let tmpl: String = self
            .url_template
            .read(cx)
            .value()
            .chars()
            .filter(|c| !matches!(c, '\n' | '\r'))
            .collect();
        let minz = self.min_zoom.read(cx).value().to_string();
        let maxz = self.max_zoom.read(cx).value().to_string();
        match validate(&name, &tmpl, &minz, &maxz) {
            Ok(mut entry) => {
                if let Some(old) = &self.editing {
                    entry.attribution = old.attribution.clone();
                    entry.tile_size = old.tile_size;
                }
                self.error = None;
                cx.emit(DialogEvent::Submitted(entry));
            }
            Err(e) => {
                self.error = Some(error_message(&e));
                cx.notify();
            }
        }
    }

    /// The name box, so the host can put the keyboard focus in it.
    pub fn name_input(&self) -> Entity<InputState> {
        self.name.clone()
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => self.cancel(cx),
            // Enter belongs to the multi-line URL box.
            modal::ModalKey::Enter => {
                if !self.url_template.read(cx).focus_handle(cx).is_focused(window) {
                    self.submit(cx)
                }
            }
            modal::ModalKey::Other => {}
        }
    }
}

pub fn error_message(e: &ValidationError) -> SharedString {
    match e {
        ValidationError::NameEmpty => tr!("dialog.custom_imagery.error.name_empty"),
        ValidationError::TemplateEmpty => tr!("dialog.custom_imagery.error.template_empty"),
        ValidationError::TemplateMissingPlaceholder => {
            tr!("dialog.custom_imagery.error.template_missing_placeholder")
        }
        ValidationError::TemplateYAndMinusY => {
            tr!("dialog.custom_imagery.error.template_y_and_minus_y")
        }
        ValidationError::MinZoomInvalid => tr!("dialog.custom_imagery.error.min_zoom_invalid"),
        ValidationError::MaxZoomInvalid => tr!("dialog.custom_imagery.error.max_zoom_invalid"),
        ValidationError::MinZoomAboveMax => tr!("dialog.custom_imagery.error.min_zoom_above_max"),
    }
}

impl crate::ui::modal::ModalDialog for CustomImageryDialog {
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        self.name.read(cx).focus_handle(cx)
    }
}

impl Focusable for CustomImageryDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CustomImageryDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let mut body = v_flex()
            .gap_3()
            .child(modal::field_row(
                tr!("dialog.custom_imagery.name"),
                &self.name,
                muted,
            ))
            .child(modal::field_row(
                tr!("dialog.custom_imagery.url_template"),
                &self.url_template,
                muted,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .child(div().flex_1().child(modal::field_row(
                        tr!("dialog.custom_imagery.min_zoom"),
                        &self.min_zoom,
                        muted,
                    )))
                    .child(div().flex_1().child(modal::field_row(
                        tr!("dialog.custom_imagery.max_zoom"),
                        &self.max_zoom,
                        muted,
                    ))),
            );

        if let Some(msg) = self.error.clone() {
            body = body.child(Label::new(msg).text_color(cx.theme().danger));
        }

        let footer = modal::footer_row(
            "cancel",
            tr!("dialog.cancel"),
            cx.listener(|this, _, _w, cx| this.cancel(cx)),
            "add",
            if self.editing.is_some() {
                tr!("dialog.custom_imagery.confirm_edit")
            } else {
                tr!("dialog.custom_imagery.confirm")
            },
            cx.listener(|this, _, _w, cx| this.submit(cx)),
        );

        let frame = modal::dialog_frame(
            cx,
            gpui::px(560.0),
            if self.editing.is_some() {
                tr!("dialog.custom_imagery.title_edit")
            } else {
                tr!("dialog.custom_imagery.title")
            },
            body,
            footer,
        );

        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
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
        assert_eq!(
            validate("  ", TMPL, "", ""),
            Err(ValidationError::NameEmpty)
        );
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
    fn a_bbox_template_needs_no_tile_indices() {
        let e = validate(
            "Export",
            "https://gis.example.com/arcgis/rest/services/A/MapServer/export?bbox={bbox-epsg-3857}&f=image",
            "",
            "22",
        )
        .unwrap();
        assert_eq!(e.max_zoom, 22);
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
