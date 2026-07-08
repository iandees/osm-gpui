//! The left-hand mode-selector toolbar: Select / Add / Building / Extrude.

use gpui::{div, prelude::*, px, Context};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable,
};

use crate::{EditMode, EditModeAction, MapViewer, SetMode};

impl MapViewer {
    pub(crate) const MODE_PANEL_WIDTH: f32 = 56.0;

    /// The left toolbar: one button per `EditMode` showing an icon above a
    /// short text label, highlighting the active one. Add/Building/Extrude are
    /// disabled (dimmed, no `on_click`) when `active_layer` is `None` — there's
    /// nowhere to write new geometry.
    ///
    /// Each button shows an icon above a short text label; the label
    /// disambiguates the nearest-fit icons (the icon set has no exact
    /// cursor/extrude glyphs).
    pub(crate) fn render_mode_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_active_layer = self.active_layer.is_some();

        div()
            .w(px(Self::MODE_PANEL_WIDTH))
            .h_full()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_2()
            .child(self.mode_button("mode-select", EditMode::Select, true, cx))
            .child(self.mode_button("mode-add", EditMode::Add, has_active_layer, cx))
            .child(self.mode_button("mode-building", EditMode::Building, has_active_layer, cx))
            .child(self.mode_button("mode-extrude", EditMode::Extrude, has_active_layer, cx))
    }

    fn mode_button(
        &self,
        id: &'static str,
        mode: EditMode,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = self.mode == mode;
        let action_mode = match mode {
            EditMode::Select => EditModeAction::Select,
            EditMode::Add => EditModeAction::Add,
            EditMode::Building => EditModeAction::Building,
            EditMode::Extrude => EditModeAction::Extrude,
        };
        let mut button = Button::new(id).w(px(46.0)).h(px(46.0)).child(
            v_flex()
                .items_center()
                .gap_0p5()
                .child(Icon::new(mode_icon(mode)).small())
                .child(div().text_xs().child(mode_label(mode))),
        );
        if is_active {
            button = button.primary();
        }
        if enabled {
            button = button.on_click(cx.listener(move |this, _, window, cx| {
                this.on_set_mode(&SetMode { mode: action_mode }, window, cx);
            }));
        } else {
            button = button.disabled(true);
        }
        button
    }
}

fn mode_label(mode: EditMode) -> &'static str {
    match mode {
        EditMode::Select => "Select",
        EditMode::Add => "Add",
        EditMode::Building => "Bldg",
        EditMode::Extrude => "Extr",
    }
}

fn mode_icon(mode: EditMode) -> IconName {
    match mode {
        EditMode::Select => IconName::Frame,
        EditMode::Add => IconName::Plus,
        EditMode::Building => IconName::Building2,
        EditMode::Extrude => IconName::LayoutDashboard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_icon_maps_correctly() {
        let icons = [
            mode_icon(EditMode::Select),
            mode_icon(EditMode::Add),
            mode_icon(EditMode::Building),
            mode_icon(EditMode::Extrude),
        ];
        // Assert the exact mode -> icon mapping so a change to any one
        // mode's icon is caught here.
        assert!(matches!(icons[0], IconName::Frame));
        assert!(matches!(icons[1], IconName::Plus));
        assert!(matches!(icons[2], IconName::Building2));
        assert!(matches!(icons[3], IconName::LayoutDashboard));
    }

    #[test]
    fn mode_labels_are_stable() {
        assert_eq!(mode_label(EditMode::Select), "Select");
        assert_eq!(mode_label(EditMode::Add), "Add");
        assert_eq!(mode_label(EditMode::Building), "Bldg");
        assert_eq!(mode_label(EditMode::Extrude), "Extr");
    }
}
