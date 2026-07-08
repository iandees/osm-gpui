//! The Fields accordion section: typed widgets (text/combo/check/radio/
//! multi-combo) for the selected feature's matched preset, built from the
//! vendored `osm_gpui::fields::FieldIndex`. Only renders when exactly one
//! feature is selected — multi-select keeps using the raw Tags table.
//! See docs/superpowers/specs/2026-07-07-id-preset-labels-design.md.

use gpui::{prelude::*, Context};
use gpui_component::{
    input::{Input, InputEvent, InputState},
    label::Label,
    ActiveTheme,
};

use crate::MapViewer;

impl MapViewer {
    /// Get or create the `InputState` entity for a text field, seeded from
    /// `current_value` only on creation (an existing entity keeps whatever
    /// the user has typed, even if `current_value` hasn't changed — it's
    /// re-read from `self.fields_text_inputs`, not rebuilt every render).
    /// Subscribes to `InputEvent` (committing on Blur/Enter) only the first
    /// time an entity is created for `field_id`, so re-renders never leak
    /// duplicate subscriptions.
    fn text_field_input(
        &mut self,
        field_id: &str,
        current_value: &str,
        placeholder: Option<&str>,
        feature: osm_gpui::selection::FeatureRef,
        field_key: String,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<InputState> {
        if let Some(existing) = self.fields_text_inputs.get(field_id) {
            return existing.clone();
        }
        let placeholder = placeholder.unwrap_or("").to_string();
        let current_value = current_value.to_string();
        let entity = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder(placeholder);
            state.set_value(current_value, window, cx);
            state
        });
        self.fields_text_inputs.insert(field_id.to_string(), entity.clone());
        self.fields_text_subscribed.insert(field_id.to_string());
        cx.subscribe(&entity, move |this: &mut Self, entity, event: &InputEvent, cx| {
            let should_commit =
                matches!(event, InputEvent::Blur) || matches!(event, InputEvent::PressEnter { .. });
            if !should_commit {
                return;
            }
            let value = entity.read(cx).value().to_string();
            this.apply_nsi_preset(&feature, std::collections::HashMap::from([(field_key.clone(), value)]));
            cx.notify();
        })
        .detach();
        entity
    }

    /// The Fields accordion section body.
    pub(crate) fn render_fields_section(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
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
        let Some((preset, tags)) = self.matched_preset_for_field_editing(&feature) else {
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

        // Text fields get a real `InputState`-backed widget; other field
        // types (Check/Radio/Combo/MultiCombo) stay as plain-text labels
        // until later tasks add their widgets.
        let fields =
            osm_gpui::fields::resolve_fields(osm_gpui::fields::field_index(), &preset.fields);
        let field_elements: Vec<gpui::AnyElement> = fields
            .into_iter()
            .map(|field| match field.field_type {
                osm_gpui::fields::FieldType::Text => {
                    let current = tags.get(&field.key).cloned().unwrap_or_default();
                    let input = self.text_field_input(
                        &field.id,
                        &current,
                        field.placeholder.as_deref(),
                        feature,
                        field.key.clone(),
                        window,
                        cx,
                    );
                    gpui::div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(Label::new(field.label.clone()).text_sm())
                        .child(Input::new(&input))
                        .into_any_element()
                }
                osm_gpui::fields::FieldType::Check => {
                    let current = tags.get(&field.key).map(String::as_str) == Some("yes");
                    let field_key = field.key.clone();
                    let element_id: gpui::ElementId =
                        gpui::SharedString::from(format!("field-check-{}", field.id)).into();
                    gpui_component::checkbox::Checkbox::new(element_id)
                        .checked(current)
                        .label(field.label.clone())
                        .on_click(cx.listener(move |this, checked: &bool, _window, cx| {
                            let value = if *checked { "yes" } else { "no" };
                            this.apply_nsi_preset(
                                &feature,
                                std::collections::HashMap::from([(
                                    field_key.clone(),
                                    value.to_string(),
                                )]),
                            );
                            cx.notify();
                        }))
                        .into_any_element()
                }
                osm_gpui::fields::FieldType::Radio => {
                    let current_value = tags.get(&field.key).cloned();
                    let field_key = field.key.clone();
                    let options = field.options.clone();
                    let selected_index = options
                        .iter()
                        .position(|opt| Some(&opt.value) == current_value.as_ref());
                    let group_id: gpui::ElementId =
                        gpui::SharedString::from(format!("field-radio-{}", field.id)).into();

                    let group = gpui_component::radio::RadioGroup::horizontal(group_id)
                        .children(options.iter().map(|opt| opt.label.clone()))
                        .selected_index(selected_index)
                        .on_click(cx.listener(move |this, index: &usize, _window, cx| {
                            let Some(opt) = options.get(*index) else {
                                return;
                            };
                            this.apply_nsi_preset(
                                &feature,
                                std::collections::HashMap::from([(
                                    field_key.clone(),
                                    opt.value.clone(),
                                )]),
                            );
                            cx.notify();
                        }));

                    gpui::div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(Label::new(field.label.clone()).text_sm())
                        .child(group)
                        .into_any_element()
                }
                _ => Label::new(field.label.clone()).text_sm().into_any_element(),
            })
            .collect();

        gpui::div()
            .flex()
            .flex_col()
            .gap_1()
            .children(field_elements)
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
