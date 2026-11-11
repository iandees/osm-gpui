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
use osm_gpui::tr;
use osm_gpui::ui::style::muted_text_size;

/// Height of every Fields control (text input, select box, checkbox row), so
/// the column reads as one rhythm; the Tags table rows are about the same.
const FIELD_CONTROL_HEIGHT: f32 = 28.0;

/// A field's name: small muted text above its control.
fn field_label(text: impl Into<gpui::SharedString>, cx: &gpui::App) -> gpui::Div {
    gpui::div()
        .text_size(muted_text_size())
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

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
                this.apply_field_values(
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
        // Labels, placeholders and option labels follow the UI language;
        // tag keys and values never do.
        let tr = osm_gpui::preset_translations::active();
        let body = osm_gpui::ui::style::current_text_scale().body;
        use osm_gpui::fields::Widget;
        match field.widget() {
            Widget::Text => {
                let current = tags.get(&field.key).cloned().unwrap_or_default();
                let input = self.text_field_input(
                    &field.id,
                    &current,
                    field.display_placeholder(tr),
                    feature,
                    field.key.clone(),
                    window,
                    cx,
                );
                gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label(field.display_label(tr).to_string(), cx))
                    .child(
                        Input::new(&input)
                            .with_size(gpui_component::Size::Size(body / 0.875))
                            .h(gpui::px(FIELD_CONTROL_HEIGHT)),
                    )
                    .into_any_element()
            }
            Widget::Check => {
                let current = tags.get(&field.key).map(String::as_str) == Some("yes");
                let field_key = field.key.clone();
                let element_id: gpui::ElementId =
                    gpui::SharedString::from(format!("field-check-{}", field.id)).into();
                // Same row height as the other controls; the label is body
                // text like a value (it is the control's own text).
                gpui::div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(gpui::px(FIELD_CONTROL_HEIGHT))
                    .child(
                        gpui_component::checkbox::Checkbox::new(element_id)
                            .checked(current)
                            .label(field.display_label(tr).to_string())
                            .text_size(body)
                            .on_click(cx.listener(move |this, checked: &bool, _window, cx| {
                                let value = if *checked { "yes" } else { "no" };
                                this.apply_field_values(
                                    &feature,
                                    std::collections::HashMap::from([(
                                        field_key.clone(),
                                        value.to_string(),
                                    )]),
                                );
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
            Widget::Radio => {
                let current_value = tags.get(&field.key).cloned();
                let field_key = field.key.clone();
                let options = field.options.clone();
                let selected_index = options
                    .iter()
                    .position(|opt| Some(&opt.value) == current_value.as_ref());
                let group_id: gpui::ElementId =
                    gpui::SharedString::from(format!("field-radio-{}", field.id)).into();

                let group = gpui_component::radio::RadioGroup::horizontal(group_id)
                    .children(
                        options
                            .iter()
                            .map(|opt| field.display_option_label(opt, tr).to_string()),
                    )
                    .selected_index(selected_index)
                    .on_click(cx.listener(move |this, index: &usize, _window, cx| {
                        let Some(opt) = options.get(*index) else {
                            return;
                        };
                        this.apply_field_values(
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
                    .child(field_label(field.display_label(tr).to_string(), cx))
                    .child(group)
                    .into_any_element()
            }
            Widget::Combo => {
                let current_value = tags.get(&field.key).cloned();
                let is_open = self.fields_open_combo.as_deref() == Some(field.id.as_str());
                // A value the options don't list (a custom one) is shown as
                // it is rather than as "None".
                let current_label = current_value
                    .as_ref()
                    .map(|v| {
                        field
                            .options
                            .iter()
                            .find(|o| &o.value == v)
                            .map(|o| field.display_option_label(o, tr).to_string())
                            .unwrap_or_else(|| v.clone())
                    })
                    .unwrap_or_else(|| tr!("panel.fields.none").to_string());

                let field_id_for_toggle = field.id.clone();
                let header = gpui::div()
                    .id(format!("field-combo-header-{}", field.id))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .h(gpui::px(FIELD_CONTROL_HEIGHT))
                    .px_2()
                    // A select box: bordered like the text inputs, with a
                    // chevron, so a read-only-looking value reads as a control.
                    .border_1()
                    .border_color(cx.theme().input)
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().background)
                    .hover(|el| el.bg(cx.theme().accent))
                    .cursor_pointer()
                    .child(
                        gpui::div()
                            .flex_1()
                            .min_w(gpui::px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .when(current_value.is_none(), |this| {
                                this.text_color(cx.theme().muted_foreground)
                            })
                            .child(current_label),
                    )
                    .child(
                        gpui_component::Icon::new(if is_open {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .xsmall()
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
                    );

                let mut column = gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label(field.display_label(tr).to_string(), cx))
                    .child(header);

                if is_open {
                    let field_key = field.key.clone();
                    // The first row clears the field (removes the tag).
                    let clear_key = field.key.clone();
                    let clear_row = gpui::div()
                        .id(format!("field-combo-clear-{}", field.id))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|el| el.bg(cx.theme().accent))
                        .child(
                            Label::new(tr!("panel.fields.none"))
                                .text_color(cx.theme().muted_foreground),
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.apply_field_values(
                                    &feature,
                                    std::collections::HashMap::from([(
                                        clear_key.clone(),
                                        String::new(),
                                    )]),
                                );
                                this.fields_open_combo = None;
                                cx.notify();
                            }),
                        );
                    column = column.child(
                        gpui::div()
                            .id(format!("field-combo-options-{}", field.id))
                            .flex()
                            .flex_col()
                            .max_h(gpui::px(160.0))
                            .overflow_y_scroll()
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(clear_row)
                            .children(field.options.iter().enumerate().map(|(i, opt)| {
                                let value = opt.value.clone();
                                let field_key = field_key.clone();
                                gpui::div()
                                    .id(("field-combo-option", i))
                                    .px_2()
                                    .py_1()
                                    .cursor_pointer()
                                    .hover(|el| el.bg(cx.theme().accent))
                                    .child(Label::new(
                                        field.display_option_label(opt, tr).to_string(),
                                    ))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |this, _ev, _window, cx| {
                                            this.apply_field_values(
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
            Widget::MultiKey => {
                // Several keys: one simple row per key under the field's name.
                let mut column = gpui::div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(field_label(field.display_label(tr).to_string(), cx));
                for sub in field.sub_fields(tags, tr) {
                    column = column.child(self.render_one_field(&sub, tags, feature, window, cx));
                }
                column.into_any_element()
            }
            Widget::PrefixMulti => {
                // `payment:` + `cash` -> payment:cash=yes. Chips for the set
                // options; picking one sets its key, a chip's x clears it.
                let set: Vec<&osm_gpui::fields::FieldOption> = field
                    .options
                    .iter()
                    .filter(|o| tags.get(&format!("{}{}", field.key, o.value)).is_some())
                    .collect();
                let is_open = self.fields_open_combo.as_deref() == Some(field.id.as_str());
                let mut column = gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label(field.display_label(tr).to_string(), cx));
                column = column.child(gpui::div().flex().flex_row().flex_wrap().gap_1().children(
                    set.iter().map(|opt| {
                        let tag_key = format!("{}{}", field.key, opt.value);
                        gpui::div()
                            .id(format!("field-prefixmulti-chip-{}", tag_key))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .bg(cx.theme().accent)
                            .cursor_pointer()
                            .child(
                                Label::new(format!("{} ×", field.display_option_label(opt, tr)))
                                    .text_size(muted_text_size()),
                            )
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.apply_field_values(
                                        &feature,
                                        std::collections::HashMap::from([(
                                            tag_key.clone(),
                                            String::new(),
                                        )]),
                                    );
                                    cx.notify();
                                }),
                            )
                    }),
                ));
                let field_id_for_toggle = field.id.clone();
                column = column.child(
                    gpui::div()
                        .id(format!("field-prefixmulti-add-{}", field.id))
                        .cursor_pointer()
                        .child(
                            Label::new(tr!("panel.fields.multi_add"))
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
                    column = column.child(
                        gpui::div()
                            .id(format!("field-prefixmulti-options-{}", field.id))
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
                                    .filter(|o| !set.iter().any(|s| s.value == o.value))
                                    .enumerate()
                                    .map(|(i, opt)| {
                                        let tag_key = format!("{}{}", field.key, opt.value);
                                        gpui::div()
                                            .id(("field-prefixmulti-option", i))
                                            .px_2()
                                            .py_1()
                                            .cursor_pointer()
                                            .hover(|el| el.bg(cx.theme().accent))
                                            .child(Label::new(
                                                field.display_option_label(opt, tr).to_string(),
                                            ))
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(move |this, _ev, _window, cx| {
                                                    this.apply_field_values(
                                                        &feature,
                                                        std::collections::HashMap::from([(
                                                            tag_key.clone(),
                                                            "yes".to_string(),
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
            Widget::SemiCombo => {
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
                    .child(field_label(field.display_label(tr).to_string(), cx));

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
                            .map(|o| field.display_option_label(o, tr).to_string())
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
                                    this.apply_field_values(
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
                            Label::new(tr!("panel.fields.multi_add"))
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
                                            .child(Label::new(
                                                field.display_option_label(opt, tr).to_string(),
                                            ))
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(move |this, _ev, _window, cx| {
                                                    let mut updated = base_values.clone();
                                                    updated.push(value.clone());
                                                    this.apply_field_values(
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
        // The side panel only shows this section for a single selected
        // feature; there is nothing to say otherwise.
        if self.selected.len() != 1 {
            return gpui::div().into_any_element();
        }

        let feature = self.selected[0];

        // (The feature's name and the change-type control are in the
        // selection header above the sections.)
        let Some((preset, tags, geometry)) = self.matched_preset_for_field_editing(&feature) else {
            return Label::new(tr!("panel.fields.no_preset"))
                .text_color(cx.theme().muted_foreground)
                .into_any_element();
        };

        // Which fields to show was settled when the index loaded (references
        // and inheritance); here only the feature's geometry and tags decide
        // what applies, plus what the user has added.
        let tr = osm_gpui::preset_translations::active();
        let resolved = osm_gpui::fields::field_index().for_feature(
            preset,
            geometry,
            &tags,
            &self.fields_promoted_more_fields,
        );
        // Only a feature with nothing to show *and* nothing to add is empty;
        // universal fields (name, note, ...) are always on offer.
        if resolved.shown.is_empty() && resolved.addable.is_empty() {
            return Label::new(tr!("panel.fields.no_fields"))
                .text_color(cx.theme().muted_foreground)
                .into_any_element();
        }

        let field_elements: Vec<gpui::AnyElement> = resolved
            .shown
            .into_iter()
            .map(|field| self.render_one_field(field, &tags, feature, window, cx))
            .collect();

        let mut column = gpui::div().flex().flex_col().gap_3();
        column = column.children(field_elements);

        // "Add field" control: the optional fields not already shown.
        let addable = resolved.addable;

        if !addable.is_empty() {
            // One "Add field" control instead of a row per optional field
            // (a cafe has fifteen), so the Tags and History sections below
            // stay on screen. Its list opens on demand and closes again
            // once a field is picked.
            let more_open = self.fields_more_open_for == Some(feature);
            column = column.child(
                gpui::div().flex().flex_row().child(
                    Button::new("field-add-more-toggle")
                        .label(tr!("panel.fields.add_field", count = addable.len()))
                        .icon(if more_open {
                            IconName::ChevronDown
                        } else {
                            IconName::Plus
                        })
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.fields_more_open_for =
                                if more_open { None } else { Some(feature) };
                            cx.notify();
                        })),
                ),
            );
            if more_open {
                column = column.child(
                    gpui::div()
                        .id("field-add-more-list")
                        .flex()
                        .flex_col()
                        .items_start()
                        .max_h(gpui::px(200.0))
                        .overflow_y_scroll()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_md()
                        .p_1()
                        .children(addable.into_iter().map(|f| {
                            let field_id = f.id.clone();
                            Button::new(gpui::SharedString::from(format!(
                                "field-add-more-{}",
                                field_id
                            )))
                            .label(f.display_label(tr).to_string())
                            .ghost()
                            .small()
                            .on_click(cx.listener(
                                move |this, _ev, _window, cx| {
                                    this.fields_promoted_more_fields.insert(field_id.clone());
                                    this.fields_more_open_for = None;
                                    cx.notify();
                                },
                            ))
                        })),
                );
            }
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
        osm_gpui::presets::Geometry,
    )> {
        let layer = self.layer_manager.find_layer(feat.layer_id)?;
        let editable = layer.as_editable()?;
        let tags: std::collections::HashMap<String, String> =
            editable.feature_tags(feat)?.into_iter().collect();
        let geometry = editable.feature_geometry(feat, osm_gpui::presets::area_keys())?;
        let preset = osm_gpui::presets::preset_index().match_feature(&tags, geometry)?;
        Some((preset, tags, geometry))
    }
}
