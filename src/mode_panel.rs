//! The left-hand mode-selector toolbar: Select / Add / Building / Extrude.

use gpui::{div, prelude::*, px, Context};
use gpui_component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants as _},
};

use crate::{EditMode, EditModeAction, MapViewer, SetMode};

impl MapViewer {
    pub(crate) const MODE_PANEL_WIDTH: f32 = 56.0;

    /// The left toolbar: one text-labeled button per `EditMode`, highlighting
    /// the active one. Add/Building/Extrude are disabled (dimmed, no
    /// `on_click`) when `active_layer` is `None` — there's nowhere to write
    /// new geometry.
    ///
    /// Uses text labels rather than icons: this project's `IconName` set
    /// (see `gpui-component-assets`) doesn't have a coherent four-icon set
    /// covering Select/Add/Building/Extrude (e.g. no cursor/pointer or
    /// extrude/3D icon), so per the plan's documented fallback we keep
    /// text-only buttons here instead of guessing at a mismatched icon.
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
        let mut button = Button::new(id).label(mode_label(mode)).small();
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
