//! The Fields accordion section: typed widgets (text/combo/check/radio/
//! multi-combo) for the selected feature's matched preset, built from the
//! vendored `osm_gpui::fields::FieldIndex`. Only renders when exactly one
//! feature is selected — multi-select keeps using the raw Tags table.
//! See docs/superpowers/specs/2026-07-07-id-preset-labels-design.md.

use gpui::{prelude::*, Context};
use gpui_component::{label::Label, ActiveTheme};

use crate::MapViewer;

impl MapViewer {
    /// The Fields accordion section body.
    pub(crate) fn render_fields_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.selected.len() != 1 {
            let message = if self.selected.is_empty() {
                "No selection."
            } else {
                "Select a single feature to edit fields."
            };
            return Label::new(message)
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let feature = self.selected[0];
        let Some((preset, _tags)) = self.matched_preset_for_field_editing(&feature) else {
            return Label::new("No matched preset.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        };

        if preset.fields.is_empty() {
            return Label::new("This feature type has no editable fields.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        // Real widget rendering lands in Tasks 8-10; for now, just list the
        // resolved field labels as plain text so the shell is independently
        // verifiable.
        let fields =
            osm_gpui::fields::resolve_fields(osm_gpui::fields::field_index(), &preset.fields);
        gpui::div()
            .flex()
            .flex_col()
            .gap_1()
            .children(fields.into_iter().map(|f| Label::new(f.label.clone()).text_sm()))
            .into_any_element()
    }

    /// Resolve the matched `Preset` and current tags for the single
    /// selected feature, or `None` if the feature/layer/tags/geometry
    /// can't be resolved (mirrors `describe_selected_feature`'s existing
    /// graceful-`None` pattern in `src/side_panel.rs`).
    fn matched_preset_for_field_editing(
        &self,
        feat: &osm_gpui::selection::FeatureRef,
    ) -> Option<(&'static osm_gpui::presets::Preset, std::collections::HashMap<String, String>)> {
        let layer = self.layer_manager.find_layer(feat.layer_id)?;
        let editable = layer.as_editable()?;
        let tags: std::collections::HashMap<String, String> =
            editable.feature_tags(feat)?.into_iter().collect();
        let geometry = editable.feature_geometry(feat, osm_gpui::presets::area_keys())?;
        let preset = osm_gpui::presets::preset_index().match_feature(&tags, geometry)?;
        Some((preset, tags))
    }
}
