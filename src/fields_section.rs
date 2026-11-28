//! The Fields accordion section: typed widgets (see `fields::Widget`) for the
//! selected feature's matched preset, built from the vendored
//! `osm_gpui::fields::FieldIndex`. Only renders when exactly one feature is
//! selected — multi-select keeps using the raw Tags table.

use gpui::{prelude::*, Context};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    label::Label,
    ActiveTheme, IconName, Sizable,
};
use osm_gpui::tags::Tags;

use crate::side_panel_state::OptionDescriptions;
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

/// One dropdown row: the value's icon (when iD has one), its label, and its
/// short description underneath (when iD has one).
fn combo_option_row(
    label: String,
    description: Option<String>,
    opt: &osm_gpui::fields::FieldOption,
    reserve_icon: bool,
    cx: &gpui::App,
) -> gpui::Div {
    let icon = opt
        .icon
        .as_deref()
        .and_then(osm_gpui::presets::icon_path)
        .map(|path| {
            gpui::div()
                .flex_shrink_0()
                .child(osm_gpui::presets::icon_element(
                    path,
                    20.0,
                    cx.theme().foreground,
                    cx.theme().is_dark(),
                ))
        })
        .or_else(|| reserve_icon.then(|| gpui::div().flex_shrink_0().size(gpui::px(20.0))));
    let mut text = gpui::div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(gpui::px(0.))
        .child(Label::new(label));
    if let Some(desc) = description {
        text = text.child(
            gpui::div()
                .text_size(muted_text_size())
                .text_color(cx.theme().muted_foreground)
                .child(desc),
        );
    }
    gpui::div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .children(icon)
        .child(text)
}

/// Whether `key` holds an `opening_hours`-syntax value the hours editor can
/// open (`opening_hours`, `opening_hours:drive_through`, …).
fn is_opening_hours_key(key: &str) -> bool {
    key == "opening_hours" || key.starts_with("opening_hours:")
}

/// The language taginfo descriptions are asked for in: the UI language
/// without its region (`de-AT` -> `de`).
fn description_language() -> String {
    let locale = osm_gpui::i18n::current_locale();
    locale
        .split(['-', '_'])
        .next()
        .unwrap_or("en")
        .to_lowercase()
}

impl MapViewer {
    /// Fetch taginfo's wiki descriptions of the values of `key`, once per
    /// session and language, so a combo for it can show them. Does nothing
    /// when taginfo is off (the setting, a unit test, a scripted run without
    /// canned answers) or `key` is not a plain schema key.
    fn request_option_descriptions(&mut self, key: &str, cx: &mut Context<Self>) {
        if !crate::tag_suggest_ui::taginfo_enabled() || !osm_gpui::taginfo::may_describe_values(key)
        {
            return;
        }
        let lang = description_language();
        let id = (key.to_string(), lang.clone());
        if self.fields_panel.option_descriptions.contains_key(&id) {
            return;
        }
        self.fields_panel
            .option_descriptions
            .insert(id.clone(), OptionDescriptions::Loading);
        let http = self.http.clone();
        let key = key.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    osm_gpui::taginfo::fetch_value_descriptions(http.as_ref(), &key, &lang)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let state = match result {
                    Ok(map) => OptionDescriptions::Ready(map),
                    Err(_) => OptionDescriptions::Failed,
                };
                this.fields_panel.option_descriptions.insert(id, state);
                cx.notify();
            });
        })
        .detach();
    }

    /// Open the hours editor for `field_key` of `feature`, starting from what
    /// the field's `input` shows (which may not be committed yet). Applying
    /// writes the tag and the field's input.
    fn open_opening_hours_dialog(
        &mut self,
        feature: osm_gpui::selection::FeatureRef,
        field_key: String,
        title: String,
        input: gpui::Entity<InputState>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        use osm_gpui::ui::opening_hours_dialog::{DialogEvent, OpeningHoursDialog};
        if self.dialogs.opening_hours_dialog.is_some() {
            return;
        }
        let current = input.read(cx).value().to_string();
        let dialog = cx.new(|cx| OpeningHoursDialog::new(title, &current, window, cx));
        cx.subscribe_in(
            &dialog,
            window,
            move |this: &mut Self, _dialog, event: &DialogEvent, window, cx| {
                if let DialogEvent::Submitted(value) = event {
                    input.update(cx, |state, cx| state.set_value(value.clone(), window, cx));
                    this.apply_field_values(
                        &feature,
                        std::collections::HashMap::from([(field_key.clone(), value.clone())]),
                    );
                }
                this.dialogs.opening_hours_dialog = None;
                cx.notify();
            },
        )
        .detach();
        self.dialogs.opening_hours_dialog = Some(dialog);
        cx.notify();
    }

    /// Get or create the `InputState` entity for a text field, seeded from
    /// `current_value` only on creation (an existing entity keeps whatever
    /// the user has typed, even if `current_value` hasn't changed — it's
    /// re-read from `self.fields_panel.fields_text_inputs`, not rebuilt every render).
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
        if let Some(existing) = self.fields_panel.fields_text_inputs.get(field_id) {
            return existing.clone();
        }
        let placeholder = placeholder.unwrap_or("").to_string();
        let current_value = current_value.to_string();
        let entity = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder(placeholder);
            state.set_value(current_value, window, cx);
            state
        });
        self.fields_panel
            .fields_text_inputs
            .insert(field_id.to_string(), entity.clone());
        self.fields_panel
            .fields_text_subscribed
            .insert(field_id.to_string());
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
        tags: &Tags,
        feature: osm_gpui::selection::FeatureRef,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // Labels, placeholders and option labels follow the UI language;
        // tag keys and values never do.
        let cat = osm_gpui::catalogue::Catalogue::bundled();
        let body = osm_gpui::ui::style::current_text_scale().body;
        use osm_gpui::fields::Widget;
        match field.widget() {
            Widget::Text => {
                let current = tags.get(&field.key).map(str::to_string).unwrap_or_default();
                let input = self.text_field_input(
                    &field.id,
                    &current,
                    cat.field_placeholder(field),
                    feature,
                    field.key.clone(),
                    window,
                    cx,
                );
                // Roads near the feature, offered as one-click street names.
                let nearby: Vec<String> = if field.key == "addr:street" {
                    self.layer_manager
                        .find_layer(feature.layer_id)
                        .and_then(|l| l.as_editable())
                        .map(|e| {
                            e.nearby_road_names(
                                &feature,
                                osm_gpui::tag_suggest::NEARBY_ROAD_RADIUS_M,
                            )
                        })
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|n| *n != current)
                        .take(5)
                        .collect()
                } else {
                    Vec::new()
                };
                let chips = (!nearby.is_empty()).then(|| {
                    let field_key = field.key.clone();
                    gpui::div().flex().flex_row().flex_wrap().gap_1().children(
                        nearby.into_iter().enumerate().map(|(i, name)| {
                            let (input, field_key, value) =
                                (input.clone(), field_key.clone(), name.clone());
                            Button::new(("street-suggestion", i))
                                .label(name)
                                .xsmall()
                                .outline()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    input.update(cx, |state, cx| {
                                        state.set_value(value.clone(), window, cx)
                                    });
                                    this.apply_field_values(
                                        &feature,
                                        std::collections::HashMap::from([(
                                            field_key.clone(),
                                            value.clone(),
                                        )]),
                                    );
                                    cx.notify();
                                }))
                        }),
                    )
                });
                let label = cat.field_label(field).to_string();
                let text_input = Input::new(&input)
                    .with_size(gpui_component::Size::Size(body / 0.875))
                    .h(gpui::px(FIELD_CONTROL_HEIGHT));
                // Hours fields get a button that opens the hours editor.
                let control = if is_opening_hours_key(&field.key) {
                    let (input, field_key, title) =
                        (input.clone(), field.key.clone(), label.clone());
                    gpui::div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(gpui::div().flex_1().min_w(gpui::px(0.)).child(text_input))
                        .child(
                            Button::new(gpui::SharedString::from(format!(
                                "field-hours-edit-{}",
                                field.id
                            )))
                            .icon(IconName::Calendar)
                            .ghost()
                            .small()
                            .tooltip(tr!("dialog.opening_hours.open"))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_opening_hours_dialog(
                                        feature,
                                        field_key.clone(),
                                        title.clone(),
                                        input.clone(),
                                        window,
                                        cx,
                                    );
                                },
                            )),
                        )
                        .into_any_element()
                } else {
                    text_input.into_any_element()
                };
                gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label(label, cx))
                    .child(control)
                    .children(chips)
                    .into_any_element()
            }
            Widget::Check => {
                let current = tags.get(&field.key) == Some("yes");
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
                            .label(cat.field_label(field).to_string())
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
                let current_value = tags.get(&field.key).map(str::to_string);
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
                            .map(|opt| cat.option_label(field, opt).to_string()),
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
                    .child(field_label(cat.field_label(field).to_string(), cx))
                    .child(group)
                    .into_any_element()
            }
            Widget::Combo => {
                let current_value = tags.get(&field.key).map(str::to_string);
                let is_open =
                    self.fields_panel.fields_open_combo.as_deref() == Some(field.id.as_str());
                // A value the options don't list (a custom one) is shown as
                // it is rather than as "None".
                let current_label = current_value
                    .as_ref()
                    .map(|v| {
                        field
                            .options
                            .iter()
                            .find(|o| &o.value == v)
                            .map(|o| cat.option_label(field, o).to_string())
                            .unwrap_or_else(|| v.clone())
                    })
                    .unwrap_or_else(|| tr!("panel.fields.none").to_string());

                // Typing while open filters the options (substring match on
                // label or value); Enter picks the first match.
                let query = if is_open {
                    self.fields_panel.fields_combo_query.to_lowercase()
                } else {
                    String::new()
                };
                let matches: Vec<(usize, String)> = field
                    .options
                    .iter()
                    .enumerate()
                    .filter(|(_, o)| {
                        query.is_empty()
                            || o.value.to_lowercase().contains(&query)
                            || cat.option_label(field, o).to_lowercase().contains(&query)
                    })
                    .map(|(i, o)| (i, o.value.clone()))
                    .collect();
                let current_label = if is_open && !query.is_empty() {
                    self.fields_panel.fields_combo_query.clone()
                } else {
                    current_label
                };
                let focus = self
                    .fields_panel
                    .fields_combo_focus
                    .get_or_insert_with(|| cx.focus_handle())
                    .clone();

                let field_id_for_toggle = field.id.clone();
                let key_for_toggle = field.key.clone();
                // Wiki descriptions from taginfo, for values the schema has
                // no description for (loaded when the list is first opened).
                let taginfo_descriptions = match self
                    .fields_panel
                    .option_descriptions
                    .get(&(field.key.clone(), description_language()))
                {
                    Some(OptionDescriptions::Ready(m)) if is_open => m.clone(),
                    _ => Default::default(),
                };
                let focus_for_toggle = focus.clone();
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
                        cx.listener(move |this, _ev, window, cx| {
                            this.fields_panel.fields_combo_query.clear();
                            this.fields_panel.fields_open_combo =
                                if this.fields_panel.fields_open_combo.as_deref()
                                    == Some(field_id_for_toggle.as_str())
                                {
                                    None
                                } else {
                                    window.focus(&focus_for_toggle, cx);
                                    Some(field_id_for_toggle.clone())
                                };
                            if this.fields_panel.fields_open_combo.is_none() {
                                window.focus(&this.focus_handle, cx);
                            } else {
                                this.request_option_descriptions(&key_for_toggle, cx);
                            }
                            cx.notify();
                        }),
                    );

                let mut column = gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label(cat.field_label(field).to_string(), cx))
                    .child(header);

                if is_open {
                    let key_field_key = field.key.clone();
                    let first_match = matches.first().map(|(_, v)| v.clone());
                    column = column.track_focus(&focus).on_key_down(cx.listener(
                        move |this, ev: &gpui::KeyDownEvent, window, cx| {
                            let ks = &ev.keystroke;
                            match ks.key.as_str() {
                                "escape" => {
                                    this.fields_panel.fields_open_combo = None;
                                    window.focus(&this.focus_handle, cx);
                                }
                                "enter" => {
                                    if let Some(value) = first_match.clone() {
                                        this.apply_field_values(
                                            &feature,
                                            std::collections::HashMap::from([(
                                                key_field_key.clone(),
                                                value,
                                            )]),
                                        );
                                        this.fields_panel.fields_open_combo = None;
                                        window.focus(&this.focus_handle, cx);
                                    }
                                }
                                "backspace" => {
                                    this.fields_panel.fields_combo_query.pop();
                                }
                                _ => match &ks.key_char {
                                    Some(ch)
                                        if !ch.is_empty()
                                            && !ks.modifiers.control
                                            && !ks.modifiers.platform =>
                                    {
                                        this.fields_panel.fields_combo_query.push_str(ch);
                                    }
                                    _ => return,
                                },
                            }
                            cx.stop_propagation();
                            cx.notify();
                        },
                    ));
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
                            cx.listener(move |this, _ev, window, cx| {
                                this.apply_field_values(
                                    &feature,
                                    std::collections::HashMap::from([(
                                        clear_key.clone(),
                                        String::new(),
                                    )]),
                                );
                                this.fields_panel.fields_open_combo = None;
                                window.focus(&this.focus_handle, cx);
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
                            // gpui's scroll listener never stops propagation, so without
                            // this the inspector behind the list scrolls along with it.
                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                            .border_1()
                            .border_color(cx.theme().border)
                            .when(query.is_empty(), |d| d.child(clear_row))
                            .children(matches.iter().map(|(i, _)| {
                                let i = *i;
                                let opt = &field.options[i];
                                let value = opt.value.clone();
                                let field_key = field_key.clone();
                                gpui::div()
                                    .id(("field-combo-option", i))
                                    .px_2()
                                    .py_1()
                                    .cursor_pointer()
                                    .hover(|el| el.bg(cx.theme().accent))
                                    .child(combo_option_row(
                                        cat.option_label(field, opt).to_string(),
                                        cat.option_description(field, opt)
                                            .map(str::to_string)
                                            .or_else(|| {
                                                taginfo_descriptions.get(&opt.value).cloned()
                                            }),
                                        opt,
                                        field.options.iter().any(|o| o.icon.is_some()),
                                        cx,
                                    ))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |this, _ev, window, cx| {
                                            this.apply_field_values(
                                                &feature,
                                                std::collections::HashMap::from([(
                                                    field_key.clone(),
                                                    value.clone(),
                                                )]),
                                            );
                                            this.fields_panel.fields_open_combo = None;
                                            window.focus(&this.focus_handle, cx);
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
                    .child(field_label(cat.field_label(field).to_string(), cx));
                for sub in cat.sub_fields(field, tags) {
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
                let is_open =
                    self.fields_panel.fields_open_combo.as_deref() == Some(field.id.as_str());
                let mut column = gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label(cat.field_label(field).to_string(), cx));
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
                                Label::new(format!("{} ×", cat.option_label(field, opt)))
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
                                this.fields_panel.fields_open_combo =
                                    if this.fields_panel.fields_open_combo.as_deref()
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
                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
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
                                                cat.option_label(field, opt).to_string(),
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
                                                    this.fields_panel.fields_open_combo = None;
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
                let is_open =
                    self.fields_panel.fields_open_combo.as_deref() == Some(field.id.as_str());

                let mut column = gpui::div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label(cat.field_label(field).to_string(), cx));

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
                            .map(|o| cat.option_label(field, o).to_string())
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
                                this.fields_panel.fields_open_combo =
                                    if this.fields_panel.fields_open_combo.as_deref()
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
                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
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
                                                cat.option_label(field, opt).to_string(),
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
                                                    this.fields_panel.fields_open_combo = None;
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
        let cat = osm_gpui::catalogue::Catalogue::bundled();
        let resolved = cat.fields_for(
            preset,
            geometry,
            &tags,
            &self.fields_panel.fields_promoted_more_fields,
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
            .map(|view| self.render_one_field(view.field, &tags, feature, window, cx))
            .collect();

        let mut column = gpui::div().flex().flex_col().gap_3();
        column = column.children(field_elements);

        let addable = resolved.addable;

        if !addable.is_empty() {
            // One "Add field" control instead of a row per optional field
            // (a cafe has fifteen), so the Tags and History sections below
            // stay on screen. Its list opens on demand and closes again
            // once a field is picked.
            let more_open = self.fields_panel.fields_more_open_for == Some(feature);
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
                            this.fields_panel.fields_more_open_for =
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
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_md()
                        .p_1()
                        .children(addable.into_iter().map(|view| {
                            let field_id = view.field.id.clone();
                            Button::new(gpui::SharedString::from(format!(
                                "field-add-more-{}",
                                field_id
                            )))
                            .label(view.label.to_string())
                            .ghost()
                            .small()
                            .on_click(cx.listener(
                                move |this, _ev, _window, cx| {
                                    this.fields_panel
                                        .fields_promoted_more_fields
                                        .insert(field_id.clone());
                                    this.fields_panel.fields_more_open_for = None;
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
    /// can't be resolved (as `describe_selected_feature` does).
    fn matched_preset_for_field_editing(
        &self,
        feat: &osm_gpui::selection::FeatureRef,
    ) -> Option<(
        &'static osm_gpui::presets::Preset,
        Tags,
        osm_gpui::presets::Geometry,
    )> {
        let layer = self.layer_manager.find_layer(feat.layer_id)?;
        let editable = layer.as_editable()?;
        let tags = editable.feature_tags(feat)?;
        let geometry = editable.feature_geometry(feat, osm_gpui::presets::area_keys())?;
        let preset = osm_gpui::catalogue::Catalogue::bundled().match_preset(&tags, geometry)?;
        Some((preset, tags, geometry))
    }
}
