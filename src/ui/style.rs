//! Shared styling constants and row builders for the app's side panels.
//!
//! Conventions enforced here (see docs/superpowers/specs/2026-07-07-ui-consistency-design.md):
//! - Panel list rows are built with [`panel_row`] / [`interactive_row`] so
//!   height, padding, rounding, and hover/selected states match across
//!   sections.
//! - Row hover uses `theme().list_hover`; a persistently selected/active row
//!   uses `theme().list_active`. `accent` is not used for row backgrounds.
//! - Every clickable action is a `gpui_component::button::Button`
//!   (`.primary()` for a section's main action, `.ghost().xsmall()` for
//!   inline/per-row actions) — never a bare `div`/`Label` with
//!   `on_mouse_down`.

use gpui::{div, prelude::*, px, App, Div, ElementId, Rgba, Stateful};
use gpui_component::ActiveTheme as _;

/// Width of the right-hand side panel, shared with the map-size math in
/// `main.rs`.
pub const SIDE_PANEL_WIDTH: f32 = 280.0;

/// Fixed height of one list row in the side panel sections.
pub const PANEL_ROW_HEIGHT: f32 = 24.0;

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
        .text_sm()
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
