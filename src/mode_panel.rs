//! The left-hand mode-selector toolbar: one button per `EditMode`.

use gpui::{div, prelude::*, px, Context, SharedString};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    v_flex, ActiveTheme, Disableable, Icon, Sizable,
};
use osm_gpui::tr;
use osm_gpui::ui::style::muted_text_size;

use crate::{EditMode, MapViewer, SetMode};

impl MapViewer {
    pub(crate) const MODE_PANEL_WIDTH: f32 = 56.0;

    /// The left toolbar: one button per `EditMode`, highlighting the active
    /// one. Modes that write new geometry are disabled (dimmed, no `on_click`)
    /// when `active_layer` is `None` — there's nowhere to write.
    ///
    /// The text label under each icon disambiguates the nearest-fit icons
    /// (the icon set has no exact cursor/extrude glyphs).
    pub(crate) fn render_mode_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .children(EditMode::ALL.into_iter().map(|mode| {
                let enabled = !mode.needs_active_layer() || self.active_layer.is_some();
                self.mode_button(mode, enabled, cx)
            }))
    }

    fn mode_button(
        &self,
        mode: EditMode,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = self.mode() == mode;
        let mut button = Button::new(("mode", mode as usize))
            .w(px(46.0))
            .h(px(46.0))
            .child(
                v_flex()
                    .items_center()
                    .gap_0p5()
                    .child(Icon::new(mode.icon()).small())
                    .child(div().text_size(muted_text_size()).child(mode.label())),
            );
        let shortcut = osm_gpui::keybindings::effective_spec(
            &osm_gpui::settings_store::snapshot(),
            mode.shortcut_id(),
        );
        button = button.tooltip(mode_tooltip(mode, &shortcut, enabled));
        if is_active {
            button = button.primary();
        }
        if enabled {
            button = button.on_click(cx.listener(move |this, _, window, cx| {
                this.on_set_mode(&SetMode { mode }, window, cx);
            }));
        } else {
            button = button.disabled(true);
        }
        button
    }
}

/// Hover text for a mode button: the unabbreviated name, its shortcut key,
/// and — for a disabled button — why it's disabled.
fn mode_tooltip(mode: EditMode, shortcut: &str, enabled: bool) -> SharedString {
    let name = mode.tooltip();
    let shortcut = shortcut.to_uppercase();
    if enabled {
        tr!("mode.tooltip", name = name, shortcut = shortcut)
    } else {
        tr!("mode.tooltip_disabled", name = name, shortcut = shortcut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::IconName;

    #[test]
    fn mode_icon_maps_correctly() {
        assert!(matches!(EditMode::Select.icon(), IconName::Frame));
        assert!(matches!(EditMode::Add.icon(), IconName::Plus));
        assert!(matches!(EditMode::Building.icon(), IconName::Building2));
        assert!(matches!(
            EditMode::Extrude.icon(),
            IconName::LayoutDashboard
        ));
        assert!(matches!(EditMode::Arc.icon(), IconName::Redo2));
        assert!(matches!(EditMode::Improve.icon(), IconName::Replace));
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
    fn every_mode_has_a_label_name_and_tooltip() {
        for mode in EditMode::ALL {
            assert!(!mode_tooltip(mode, "x", true).is_empty());
            assert!(!mode.label().is_empty());
            assert!(!mode.name().is_empty());
        }
    }

    #[test]
    fn mode_labels_are_stable() {
        assert_eq!(EditMode::Select.label(), "Select");
        assert_eq!(EditMode::Add.label(), "Add");
        assert_eq!(EditMode::Building.label(), "Bldg");
        assert_eq!(EditMode::Extrude.label(), "Extr");
        assert_eq!(EditMode::Arc.label(), "Arc");
        assert_eq!(EditMode::Improve.label(), "Impr");
    }
}
