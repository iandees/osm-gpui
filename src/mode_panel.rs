//! The left-hand mode-selector toolbar: Select / Add / Building / Extrude / Arc.

use gpui::{div, prelude::*, px, Context, SharedString};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable,
};
use osm_gpui::tr;
use osm_gpui::ui::style::muted_text_size;

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
            .child(self.mode_button("mode-arc", EditMode::Arc, has_active_layer, cx))
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
            EditMode::Arc => EditModeAction::Arc,
        };
        let mut button = Button::new(id).w(px(46.0)).h(px(46.0)).child(
            v_flex()
                .items_center()
                .gap_0p5()
                .child(Icon::new(mode_icon(mode)).small())
                .child(div().text_size(muted_text_size()).child(mode_label(mode))),
        );
        let shortcut = osm_gpui::keybindings::effective_spec(
            &osm_gpui::settings_store::snapshot(),
            mode_shortcut_id(mode),
        );
        button = button.tooltip(mode_tooltip(mode, &shortcut, enabled));
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

fn mode_label(mode: EditMode) -> SharedString {
    match mode {
        EditMode::Select => tr!("mode.select.label"),
        EditMode::Add => tr!("mode.add.label"),
        EditMode::Building => tr!("mode.building.label"),
        EditMode::Extrude => tr!("mode.extrude.label"),
        EditMode::Arc => tr!("mode.arc.label"),
    }
}

/// The `keybindings::SHORTCUTS` id of the shortcut that switches to `mode`.
fn mode_shortcut_id(mode: EditMode) -> &'static str {
    match mode {
        EditMode::Select => "mode_select",
        EditMode::Add => "mode_add",
        EditMode::Building => "mode_building",
        EditMode::Extrude => "mode_extrude",
        EditMode::Arc => "mode_arc",
    }
}

/// Hover text for a mode button: the unabbreviated name, its shortcut key,
/// and — for a disabled button — why it's disabled.
fn mode_tooltip(mode: EditMode, shortcut: &str, enabled: bool) -> SharedString {
    let name = match mode {
        EditMode::Select => tr!("mode.select.tooltip"),
        EditMode::Add => tr!("mode.add.tooltip"),
        EditMode::Building => tr!("mode.building.tooltip"),
        EditMode::Extrude => tr!("mode.extrude.tooltip"),
        EditMode::Arc => tr!("mode.arc.tooltip"),
    };
    let shortcut = shortcut.to_uppercase();
    if enabled {
        tr!("mode.tooltip", name = name, shortcut = shortcut)
    } else {
        tr!("mode.tooltip_disabled", name = name, shortcut = shortcut)
    }
}

fn mode_icon(mode: EditMode) -> IconName {
    match mode {
        EditMode::Select => IconName::Frame,
        EditMode::Add => IconName::Plus,
        EditMode::Building => IconName::Building2,
        EditMode::Extrude => IconName::LayoutDashboard,
        EditMode::Arc => IconName::Redo2,
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
            mode_icon(EditMode::Arc),
        ];
        // Assert the exact mode -> icon mapping so a change to any one
        // mode's icon is caught here.
        assert!(matches!(icons[0], IconName::Frame));
        assert!(matches!(icons[1], IconName::Plus));
        assert!(matches!(icons[2], IconName::Building2));
        assert!(matches!(icons[3], IconName::LayoutDashboard));
        assert!(matches!(icons[4], IconName::Redo2));
    }

    #[test]
    fn mode_tooltip_shows_shortcut_and_disabled_reason() {
        assert_eq!(
            mode_tooltip(EditMode::Add, "a", true),
            "Add nodes and ways (A)"
        );
        assert!(mode_tooltip(EditMode::Add, "a", false).ends_with("OSM data first"));
    }

    #[test]
    fn every_mode_has_a_registered_shortcut() {
        for mode in [
            EditMode::Select,
            EditMode::Add,
            EditMode::Building,
            EditMode::Extrude,
            EditMode::Arc,
        ] {
            // `def` panics on an unknown id.
            osm_gpui::keybindings::def(mode_shortcut_id(mode));
        }
    }

    #[test]
    fn mode_labels_are_stable() {
        assert_eq!(mode_label(EditMode::Select), "Select");
        assert_eq!(mode_label(EditMode::Add), "Add");
        assert_eq!(mode_label(EditMode::Building), "Bldg");
        assert_eq!(mode_label(EditMode::Extrude), "Extr");
        assert_eq!(mode_label(EditMode::Arc), "Arc");
    }
}
