//! Shared styling constants and row builders for the app's side panels.
//!
//! Conventions enforced here (see docs/superpowers/specs/2026-07-07-ui-consistency-design.md):
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
use gpui::{div, prelude::*, px, App, Div, ElementId, Pixels, Rgba, Stateful};
use gpui_component::ActiveTheme as _;

/// Width of the right-hand side panel, shared with the map-size math in
/// `main.rs`.
pub const SIDE_PANEL_WIDTH: f32 = 280.0;

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
