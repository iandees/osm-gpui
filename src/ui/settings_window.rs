//! Settings window with custom imagery management.

use gpui::*;
use ui::prelude::*;
use ui::{ListHeader, ListItem};

use crate::custom_imagery_store::{self, CustomImageryEntry};
use crate::ui::text_input::TextInput;

pub struct SettingsWindow {
    focus_handle: FocusHandle,
    entries: Vec<CustomImageryEntry>,
    expanded_index: Option<usize>,
    confirm_delete_index: Option<usize>,
    edit_name: Option<Entity<TextInput>>,
    edit_url: Option<Entity<TextInput>>,
    edit_min_zoom: Option<Entity<TextInput>>,
    edit_max_zoom: Option<Entity<TextInput>>,
    edit_error: Option<SharedString>,
}

impl SettingsWindow {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            entries: custom_imagery_store::load(),
            expanded_index: None,
            confirm_delete_index: None,
            edit_name: None,
            edit_url: None,
            edit_min_zoom: None,
            edit_max_zoom: None,
            edit_error: None,
        }
    }

    fn start_editing(&mut self, entry: &CustomImageryEntry, cx: &mut Context<Self>) {
        let name = cx.new(|cx| TextInput::new(cx, "Name"));
        let url = cx.new(|cx| TextInput::new(cx, "https://…/{z}/{x}/{y}.png"));
        let min_zoom = cx.new(|cx| TextInput::new(cx, "0"));
        let max_zoom = cx.new(|cx| TextInput::new(cx, "19"));

        name.update(cx, |ti, cx| ti.set_content(entry.name.clone(), cx));
        url.update(cx, |ti, cx| ti.set_content(entry.url_template.clone(), cx));
        min_zoom.update(cx, |ti, cx| ti.set_content(entry.min_zoom.to_string(), cx));
        max_zoom.update(cx, |ti, cx| ti.set_content(entry.max_zoom.to_string(), cx));

        self.edit_name = Some(name);
        self.edit_url = Some(url);
        self.edit_min_zoom = Some(min_zoom);
        self.edit_max_zoom = Some(max_zoom);
        self.edit_error = None;
    }

    fn clear_editing(&mut self) {
        self.edit_name = None;
        self.edit_url = None;
        self.edit_min_zoom = None;
        self.edit_max_zoom = None;
        self.edit_error = None;
    }

    fn save_entry(&mut self, idx: usize, cx: &mut Context<Self>) {
        let (Some(name), Some(url), Some(min_z), Some(max_z)) = (
            self.edit_name.as_ref(),
            self.edit_url.as_ref(),
            self.edit_min_zoom.as_ref(),
            self.edit_max_zoom.as_ref(),
        ) else {
            return;
        };

        let name_val = name.read(cx).content().to_string();
        let url_val = url.read(cx).content().to_string();
        let min_val = min_z.read(cx).content().to_string();
        let max_val = max_z.read(cx).content().to_string();

        match crate::ui::custom_imagery_dialog::validate(&name_val, &url_val, &min_val, &max_val) {
            Ok(entry) => {
                self.entries[idx] = entry;
                self.persist();
                self.expanded_index = None;
                self.clear_editing();
                cx.notify();
            }
            Err(e) => {
                self.edit_error = Some(
                    crate::ui::custom_imagery_dialog::error_message(&e).into(),
                );
                cx.notify();
            }
        }
    }

    fn delete_entry(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx < self.entries.len() {
            self.entries.remove(idx);
            self.persist();
        }
        self.expanded_index = None;
        self.clear_editing();
        self.confirm_delete_index = None;
        cx.notify();
    }

    fn add_new_entry(&mut self, cx: &mut Context<Self>) {
        let blank = CustomImageryEntry {
            name: String::new(),
            url_template: String::new(),
            min_zoom: 0,
            max_zoom: 19,
        };
        self.entries.push(blank.clone());
        let new_idx = self.entries.len() - 1;
        self.expanded_index = Some(new_idx);
        self.confirm_delete_index = None;
        self.start_editing(&blank, cx);
        cx.notify();
    }

    fn persist(&self) {
        custom_imagery_store::update_store(self.entries.clone());
    }
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn field_row(label: &'static str, input: Entity<TextInput>) -> impl IntoElement {
    v_flex()
        .gap(px(2.0))
        .child(Label::new(label).size(LabelSize::XSmall).color(Color::Muted))
        .child(input)
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex()
            .gap(DynamicSpacing::Base08.rems(cx))
            .child(ListHeader::new("Custom Imagery Sources"));

        if self.entries.is_empty() {
            content = content.child(
                Label::new("No custom imagery sources configured.")
                    .color(Color::Muted)
                    .size(LabelSize::Small),
            );
        } else {
            for (idx, entry) in self.entries.iter().enumerate() {
                let is_expanded = self.expanded_index == Some(idx);
                let entry_name = entry.name.clone();

                let end_slot: AnyElement = if self.confirm_delete_index == Some(idx) {
                    let name_for_label = entry_name.clone();
                    h_flex()
                        .gap(DynamicSpacing::Base04.rems(cx))
                        .child(
                            Label::new(format!("Delete {}?", name_for_label))
                                .size(LabelSize::Small)
                                .color(Color::Error),
                        )
                        .child(
                            Button::new(("confirm-delete", idx), "Delete")
                                .style(ButtonStyle::Filled)
                                .size(ButtonSize::Compact)
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    this.delete_entry(idx, cx);
                                })),
                        )
                        .child(
                            Button::new(("cancel-delete", idx), "Cancel")
                                .style(ButtonStyle::Subtle)
                                .size(ButtonSize::Compact)
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    this.confirm_delete_index = None;
                                    cx.notify();
                                })),
                        )
                        .into_any_element()
                } else {
                    IconButton::new(("trash", idx), IconName::Trash)
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Muted)
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.confirm_delete_index = Some(idx);
                            cx.notify();
                        }))
                        .into_any_element()
                };

                let list_item = ListItem::new(("entry", idx))
                    .child(Label::new(entry_name))
                    .toggle(Some(is_expanded))
                    .on_toggle(cx.listener(move |this, _ev, _window, cx| {
                        if this.expanded_index == Some(idx) {
                            this.expanded_index = None;
                            this.clear_editing();
                        } else {
                            let entry = this.entries[idx].clone();
                            this.expanded_index = Some(idx);
                            this.confirm_delete_index = None;
                            this.start_editing(&entry, cx);
                        }
                        cx.notify();
                    }))
                    .end_slot(end_slot);

                content = content.child(list_item);

                if is_expanded {
                    if let (Some(edit_name), Some(edit_url), Some(edit_min_zoom), Some(edit_max_zoom)) = (
                        self.edit_name.clone(),
                        self.edit_url.clone(),
                        self.edit_min_zoom.clone(),
                        self.edit_max_zoom.clone(),
                    ) {
                        let mut expanded_content = v_flex()
                            .pl(DynamicSpacing::Base24.rems(cx))
                            .gap(DynamicSpacing::Base08.rems(cx))
                            .child(field_row("Name", edit_name))
                            .child(field_row("URL template", edit_url))
                            .child(
                                h_flex()
                                    .gap(DynamicSpacing::Base08.rems(cx))
                                    .child(
                                        div().flex_1().child(field_row("Min zoom", edit_min_zoom)),
                                    )
                                    .child(
                                        div().flex_1().child(field_row("Max zoom", edit_max_zoom)),
                                    ),
                            );

                        if let Some(err) = &self.edit_error {
                            expanded_content = expanded_content.child(
                                Label::new(err.clone())
                                    .color(Color::Error)
                                    .size(LabelSize::Small),
                            );
                        }

                        let save_btn = Button::new(("save", idx), "Save")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                this.save_entry(idx, cx);
                            }));

                        let cancel_btn = Button::new(("cancel", idx), "Cancel")
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                if let Some(entry) = this.entries.get(idx) {
                                    if entry.name.is_empty() && entry.url_template.is_empty() {
                                        this.entries.remove(idx);
                                    }
                                }
                                this.expanded_index = None;
                                this.clear_editing();
                                cx.notify();
                            }));

                        expanded_content = expanded_content.child(
                            h_flex()
                                .gap(DynamicSpacing::Base08.rems(cx))
                                .child(save_btn)
                                .child(cancel_btn),
                        );

                        content = content.child(expanded_content);
                    }
                }
            }
        }

        content = content.child(
            Button::new("add-source", "Add Source")
                .style(ButtonStyle::Subtle)
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.add_new_entry(cx);
                })),
        );

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().background)
            .p(DynamicSpacing::Base16.rems(cx))
            .child(content)
    }
}
