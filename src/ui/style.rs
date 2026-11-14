//! Shared styling constants and row builders for the app's side panels.
//!
//! The type scale (sizes follow the user's text-size setting, see
//! [`TextScale`]; the window root sets the body size, so plain text inherits it):
//! - dialog title: body, semibold ([`crate::ui::modal::dialog_frame`])
//! - dialog and side-panel body text, control text (checkbox labels, links):
//!   body ([`body_text_size`])
//! - field labels, helper lines, counters, dates, captions: muted
//!   ([`muted_text_size`]) in `muted_foreground`
//! - side-panel section headers: body, semibold
//! - buttons: dialog footers use the default size; inline buttons in dialog
//!   and side-panel bodies are `.small()` (`.ghost()` for secondary actions,
//!   left-aligned in a `div().flex().flex_row()`); per-row icon buttons are
//!   `.xsmall()`
//! - links: [`link`]
//!
//! Conventions enforced here:
//! - Panel list rows are built with [`panel_row`] / [`interactive_row`] so
//!   height, padding, rounding, and hover/selected states match across
//!   sections.
//! - Row hover uses `theme().list_hover`; a persistently selected/active row
//!   uses `theme().list_active`. `accent` is not used for these side-panel
//!   list-row backgrounds.
//! - In the side panel's list rows and action buttons, every clickable
//!   action is a `gpui_component::button::Button` (`.primary()` for a
//!   section's main action, `.ghost().xsmall()` for inline/per-row actions)
//!   — never a bare `div`/`Label` with `on_mouse_down`. Known debt: the
//!   Fields section's combo/multicombo widgets (`src/fields_section.rs`)
//!   predate this convention and still use `theme().accent` hover fills and
//!   `on_mouse_down` divs; they have not yet been converted.

use crate::settings_store::{self, TextSizePreset};
use gpui::{div, prelude::*, px, App, Div, ElementId, Pixels, Rgba, SharedString, Stateful};
use gpui_component::ActiveTheme as _;

/// Default width of the right-hand side panel. The live width is
/// `MapViewer::side_panel_width`, which the user can drag between
/// [`SIDE_PANEL_MIN_WIDTH`] and [`SIDE_PANEL_MAX_WIDTH`].
pub const SIDE_PANEL_WIDTH: f32 = 280.0;
pub const SIDE_PANEL_MIN_WIDTH: f32 = 220.0;
pub const SIDE_PANEL_MAX_WIDTH: f32 = 560.0;

/// The side panel width for a resize drag that started at window x
/// `start_x` with the panel `start_width` wide and is now at `x`. The
/// panel's left edge follows the pointer, so moving left widens it.
pub fn resized_side_panel_width(start_width: f32, start_x: f32, x: f32) -> f32 {
    (start_width + (start_x - x)).clamp(SIDE_PANEL_MIN_WIDTH, SIDE_PANEL_MAX_WIDTH)
}

/// The side panel width to start with, given the persisted setting.
pub fn initial_side_panel_width(saved: Option<f32>) -> f32 {
    saved
        .filter(|w| w.is_finite())
        .map(|w| w.clamp(SIDE_PANEL_MIN_WIDTH, SIDE_PANEL_MAX_WIDTH))
        .unwrap_or(SIDE_PANEL_WIDTH)
}

/// Fixed height of one list row in the side panel sections.
pub const PANEL_ROW_HEIGHT: f32 = 24.0;

/// Pixel sizes for the app's two text roles (normal body text and secondary/
/// muted text), derived from the user's [`TextSizePreset`] setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextScale {
    pub body: Pixels,
    pub muted: Pixels,
}

impl TextScale {
    pub fn for_preset(preset: TextSizePreset) -> Self {
        match preset {
            TextSizePreset::Small => Self {
                body: px(12.0),
                muted: px(10.0),
            },
            TextSizePreset::Medium => Self {
                body: px(14.0),
                muted: px(12.0),
            },
            TextSizePreset::Large => Self {
                body: px(16.0),
                muted: px(14.0),
            },
        }
    }
}

/// The text scale implied by the current app settings. Call this at each
/// window's root render to apply `.text_size(current_text_scale().body)` so
/// it cascades to the whole tree as the default.
pub fn current_text_scale() -> TextScale {
    TextScale::for_preset(settings_store::snapshot().text_size_preset)
}

/// Size for secondary/muted text (field labels, captions), scaled
/// proportionally with the current text size setting.
pub fn muted_text_size() -> Pixels {
    current_text_scale().muted
}

/// Size for normal body text: the window default, and what a control's own
/// text (a checkbox label, a link) must use. `gpui-component` widgets pick
/// their own fixed sizes (a default `Button` and `Checkbox` are 16px, `.small()`
/// 14px, `.xsmall()` 12px), so they have to be pinned to it.
pub fn body_text_size() -> Pixels {
    current_text_scale().body
}

/// A text link that opens something on the website: link colour, underlined
/// on hover, inline (so it sits left-aligned with the text around it and
/// inherits that text's size). The caller adds `.on_click(...)`. Use this for
/// every "open on openstreetmap.org", changeset and user link.
pub fn link(id: impl Into<ElementId>, text: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    let color = cx.theme().link;
    let hover = cx.theme().link_hover;
    div()
        .id(id)
        .cursor_pointer()
        .text_color(color)
        .hover(move |this| this.text_color(hover).underline())
        .child(text.into())
}

/// The one semi-transparent black used behind every modal dialog.
pub fn scrim_color() -> Rgba {
    gpui::rgba(0x00000099)
}

/// Layout-only panel list row: fixed height, standard padding/rounding/text
/// size. Interaction states come from [`interactive_row`]; passive rows
/// (e.g. History entries) use this directly.
pub fn panel_row(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .h(px(PANEL_ROW_HEIGHT))
        .px_2()
        .gap_1()
        .rounded_md()
}

/// A clickable panel list row: [`panel_row`] plus pointer cursor and the
/// standard hover/selected backgrounds (`list_hover` / `list_active`).
pub fn interactive_row(id: impl Into<ElementId>, selected: bool, cx: &App) -> Stateful<Div> {
    let row = panel_row(id).cursor_pointer();
    if selected {
        row.bg(cx.theme().list_active)
    } else {
        let hover_bg = cx.theme().list_hover;
        row.hover(move |this| this.bg(hover_bg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_left_widens_the_side_panel_within_limits() {
        assert_eq!(resized_side_panel_width(280.0, 900.0, 860.0), 320.0);
        assert_eq!(resized_side_panel_width(280.0, 900.0, 930.0), 250.0);
        assert_eq!(
            resized_side_panel_width(280.0, 900.0, 0.0),
            SIDE_PANEL_MAX_WIDTH
        );
        assert_eq!(
            resized_side_panel_width(280.0, 900.0, 2000.0),
            SIDE_PANEL_MIN_WIDTH
        );
    }

    #[test]
    fn initial_side_panel_width_defaults_and_clamps() {
        assert_eq!(initial_side_panel_width(None), SIDE_PANEL_WIDTH);
        assert_eq!(initial_side_panel_width(Some(340.0)), 340.0);
        assert_eq!(initial_side_panel_width(Some(10.0)), SIDE_PANEL_MIN_WIDTH);
        assert_eq!(initial_side_panel_width(Some(f32::NAN)), SIDE_PANEL_WIDTH);
    }
}
