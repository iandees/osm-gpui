//! The Fields accordion section: typed widgets (text/combo/check/radio/
//! multi-combo) for the selected feature's matched preset, built from the
//! vendored `osm_gpui::fields::FieldIndex`. Only renders when exactly one
//! feature is selected — multi-select keeps using the raw Tags table.
//! See docs/superpowers/specs/2026-07-07-id-preset-labels-design.md.

use gpui::{prelude::*, Context};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    label::Label,
    ActiveTheme, IconName, Sizable,
};

use crate::MapViewer;
use osm_gpui::ui::style::muted_text_size;

impl MapViewer {
    /// Get or create the `InputState` entity for a text field, seeded from
    /// `current_value` only on creation (an existing entity keeps whatever
    /// the user has typed, even if `current_value` hasn't changed — it's
    /// re-read from `self.fields_text_inputs`, not rebuilt every render).
    /// Subscribes to `InputEvent` (committing on Blur/Enter) only the first
    /// time an entity is created for `field_id`, so re-renders never leak
    /// duplicate subscriptions.
    #[allow(clippy::too_many_arguments)]
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
        self.fields_text_inputs
            .insert(field_id.to_string(), entity.clone());
        self.fields_text_subscribed.insert(field_id.to_string());
        cx.subscribe(
            &entity,
            move |this: &mut Self, entity, event: &InputEvent, cx| {
                let should_commit = matches!(event, InputEvent::Blur)
                    || matches!(event, InputEvent::PressEnter { .. });
                if !should_commit {
                    return;
                }
                let value = entity.read(cx).value().to_string();
                this.apply_nsi_preset(
                    &feature,
                    std::collections::HashMap::from([(field_key.clone(), value)]),
                );
                cx.notify();
            },
        )
        .detach();
        entity
    }

    /// Render a single `Field` with the widget matching its `FieldType`.
    /// Shared by both the preset's default `fields` and any promoted
    /// `more_fields`, so there is exactly one per-type dispatch in this
    /// module.
    fn render_one_field(
        &mut self,
        field: &osm_gpui::fields::Field,
        tags: &std::collections::HashMap<String, String>,
        feature: osm_gpui::selection::FeatureRef,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match field.field_type {
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
                    .child(Label::new(field.label.clone()))
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
                    .child(Label::new(field.label.clone()))
                    .child(group)
                    .into_any_element()
            }
            osm_gpui::fields::FieldType::Combo => {
                let current_value = tags.get(&field.key).cloned();
                let is_open = self.fields_open_combo.as_deref() == Some(field.id.as_str());
                let current_label = current_value
                    .as_ref()
                    .and_then(|v| field.options.iter().find(|o| &o.value == v))
                    .map(|o| o.label.clone())
                    .unwrap_or_else(|| "(none)".to_string());

                let field_id_for_toggle = field.id.clone();
                let header = gpui::div()
                    .id(format!("field-combo-header-{}", field.id))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .child(Label::new(current_label))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.fields_open_combo = if this.fields_open_combo.as_deref()
                                == Some(field_id_for_toggle.as_str())
                            {
                                None
                            } else {
                                Some(field_id_for_toggle.clone())
                            };
                            cx.notify();
                        }),
                    );

                let mut column = gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new(field.label.clone()))
                    .child(header);

                if is_open {
                    let field_key = field.key.clone();
                    column = column.child(
                        gpui::div()
                            .id(format!("field-combo-options-{}", field.id))
                            .flex()
                            .flex_col()
                            .max_h(gpui::px(160.0))
                            .overflow_y_scroll()
                            .border_1()
                            .border_color(cx.theme().border)
                            .children(field.options.iter().enumerate().map(|(i, opt)| {
                                let value = opt.value.clone();
                                let field_key = field_key.clone();
                                gpui::div()
                                    .id(("field-combo-option", i))
                                    .px_2()
                                    .py_1()
                                    .cursor_pointer()
                                    .hover(|el| el.bg(cx.theme().accent))
                                    .child(Label::new(opt.label.clone()))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |this, _ev, _window, cx| {
                                            this.apply_nsi_preset(
                                                &feature,
                                                std::collections::HashMap::from([(
                                                    field_key.clone(),
                                                    value.clone(),
                                                )]),
                                            );
                                            this.fields_open_combo = None;
                                            cx.notify();
                                        }),
                                    )
                            })),
                    );
                }

                column.into_any_element()
            }
            osm_gpui::fields::FieldType::MultiCombo => {
                let current_values: Vec<String> = tags
                    .get(&field.key)
                    .map(|v| {
                        v.split(';')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let is_open = self.fields_open_combo.as_deref() == Some(field.id.as_str());

                let mut column = gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new(field.label.clone()));

                // Chips for already-selected values, each removable.
                let chips = gpui::div().flex().flex_row().flex_wrap().gap_1().children(
                    current_values.iter().map(|value| {
                        let field_key = field.key.clone();
                        let value_to_remove = value.clone();
                        let remaining: Vec<String> = current_values
                            .iter()
                            .filter(|v| *v != &value_to_remove)
                            .cloned()
                            .collect();
                        let label = field
                            .options
                            .iter()
                            .find(|o| &o.value == value)
                            .map(|o| o.label.clone())
                            .unwrap_or_else(|| value.clone());
                        gpui::div()
                            .id(format!("field-multicombo-chip-{}", value))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .bg(cx.theme().accent)
                            .cursor_pointer()
                            .child(Label::new(format!("{} ×", label)).text_size(muted_text_size()))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.apply_nsi_preset(
                                        &feature,
                                        std::collections::HashMap::from([(
                                            field_key.clone(),
                                            remaining.join(";"),
                                        )]),
                                    );
                                    cx.notify();
                                }),
                            )
                    }),
                );
                column = column.child(chips);

                let field_id_for_toggle = field.id.clone();
                column = column.child(
                    gpui::div()
                        .id(format!("field-multicombo-add-{}", field.id))
                        .cursor_pointer()
                        .child(
                            Label::new("+ Add")
                                .text_size(muted_text_size())
                                .text_color(cx.theme().muted_foreground),
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.fields_open_combo = if this.fields_open_combo.as_deref()
                                    == Some(field_id_for_toggle.as_str())
                                {
                                    None
                                } else {
                                    Some(field_id_for_toggle.clone())
                                };
                                cx.notify();
                            }),
                        ),
                );

                if is_open {
                    let field_key = field.key.clone();
                    let current_values_for_options = current_values.clone();
                    column = column.child(
                        gpui::div()
                            .id(format!("field-multicombo-options-{}", field.id))
                            .flex()
                            .flex_col()
                            .max_h(gpui::px(160.0))
                            .overflow_y_scroll()
                            .border_1()
                            .border_color(cx.theme().border)
                            .children(
                                field
                                    .options
                                    .iter()
                                    .filter(|opt| !current_values_for_options.contains(&opt.value))
                                    .enumerate()
                                    .map(|(i, opt)| {
                                        let value = opt.value.clone();
                                        let field_key = field_key.clone();
                                        let base_values = current_values_for_options.clone();
                                        gpui::div()
                                            .id(("field-multicombo-option", i))
                                            .px_2()
                                            .py_1()
                                            .cursor_pointer()
                                            .hover(|el| el.bg(cx.theme().accent))
                                            .child(Label::new(opt.label.clone()))
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(move |this, _ev, _window, cx| {
                                                    let mut updated = base_values.clone();
                                                    updated.push(value.clone());
                                                    this.apply_nsi_preset(
                                                        &feature,
                                                        std::collections::HashMap::from([(
                                                            field_key.clone(),
                                                            updated.join(";"),
                                                        )]),
                                                    );
                                                    this.fields_open_combo = None;
                                                    cx.notify();
                                                }),
                                            )
                                    }),
                            ),
                    );
                }

                column.into_any_element()
            }
        }
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
                .into_any_element();
        }

        let feature = self.selected[0];

        let change_type_button = Button::new("change-feature-type")
            .label("Change feature type…")
            .ghost()
            .xsmall()
            .on_click(cx.listener(move |_this, _ev, window, cx| {
                window.dispatch_action(Box::new(crate::ChangeFeatureType), cx);
            }));

        // The same friendly name shown in the Selection section (preset name,
        // with a geometry-based fallback like "Point"/"Line"/"Area").
        let name_header = self
            .describe_selected_feature(&feature)
            .map(|(name, _icon_path)| Label::new(name).font_weight(gpui::FontWeight::SEMIBOLD));

        let Some((preset, tags)) = self.matched_preset_for_field_editing(&feature) else {
            let mut column = gpui::div().flex().flex_col().gap_2();
            if let Some(header) = name_header {
                column = column.child(header);
            }
            return column
                .child(change_type_button)
                .child(Label::new("No matched preset.").text_color(cx.theme().muted_foreground))
                .into_any_element();
        };

        if preset.fields.is_empty() {
            let mut column = gpui::div().flex().flex_col().gap_2();
            if let Some(header) = name_header {
                column = column.child(header);
            }
            return column
                .child(change_type_button)
                .child(
                    Label::new("This feature type has no editable fields.")
                        .text_color(cx.theme().muted_foreground),
                )
                .into_any_element();
        }

        let field_index = osm_gpui::fields::field_index();
        let fields = osm_gpui::fields::resolve_fields(field_index, &preset.fields);
        let mut field_elements: Vec<gpui::AnyElement> = fields
            .into_iter()
            .map(|field| {
                let field = field.clone();
                self.render_one_field(&field, &tags, feature, window, cx)
            })
            .collect();

        // Promoted `more_fields` render through the exact same per-type
        // dispatch as default fields.
        let promoted_ids: Vec<String> = self.fields_promoted_more_fields.iter().cloned().collect();
        let promoted_fields = osm_gpui::fields::resolve_fields(field_index, &promoted_ids);
        for field in promoted_fields {
            let field = field.clone();
            field_elements.push(self.render_one_field(&field, &tags, feature, window, cx));
        }

        let mut column = gpui::div().flex().flex_col().gap_2();
        if let Some(header) = name_header {
            column = column.child(header);
        }
        column = column.child(change_type_button).children(field_elements);

        // "Add field" control: list `preset.more_fields` not already shown
        // (default fields or already-promoted more_fields).
        let already_shown: Vec<String> = preset
            .fields
            .iter()
            .cloned()
            .chain(self.fields_promoted_more_fields.iter().cloned())
            .collect();
        let addable =
            osm_gpui::fields::resolve_more_fields(field_index, &preset.more_fields, &already_shown);

        if !addable.is_empty() {
            column = column.child(
                gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .items_start()
                    .children(addable.into_iter().map(|f| {
                        let field_id = f.id.clone();
                        Button::new(gpui::SharedString::from(format!(
                            "field-add-more-{}",
                            field_id
                        )))
                        .label(f.label.clone())
                        .icon(IconName::Plus)
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(
                            move |this, _ev, _window, cx| {
                                this.fields_promoted_more_fields.insert(field_id.clone());
                                cx.notify();
                            },
                        ))
                    })),
            );
        }

        column.into_any_element()
    }

    /// Resolve the matched `Preset` and current tags for the single
    /// selected feature, or `None` if the feature/layer/tags/geometry
    /// can't be resolved (mirrors `describe_selected_feature`'s existing
    /// graceful-`None` pattern in `src/side_panel.rs`).
    fn matched_preset_for_field_editing(
        &self,
        feat: &osm_gpui::selection::FeatureRef,
    ) -> Option<(
        &'static osm_gpui::presets::Preset,
        std::collections::HashMap<String, String>,
    )> {
        let layer = self.layer_manager.find_layer(feat.layer_id)?;
        let editable = layer.as_editable()?;
        let tags: std::collections::HashMap<String, String> =
            editable.feature_tags(feat)?.into_iter().collect();
        let geometry = editable.feature_geometry(feat, osm_gpui::presets::area_keys())?;
        let preset = osm_gpui::presets::preset_index().match_feature(&tags, geometry)?;
        Some((preset, tags))
    }
}
