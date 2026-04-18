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

    fn save_entry(&mut self, idx: usize, _cx: &mut Context<Self>) {
        eprintln!("settings: save entry {} (stub)", idx);
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

                let trash_button = IconButton::new(("trash", idx), IconName::Trash)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted);

                let list_item = ListItem::new(("entry", idx))
                    .child(Label::new(entry.name.clone()))
                    .toggle(Some(is_expanded))
                    .on_toggle(cx.listener(move |this, _ev, _window, cx| {
                        if this.expanded_index == Some(idx) {
                            this.expanded_index = None;
                            this.clear_editing();
                        } else {
                            let entry = this.entries[idx].clone();
                            this.expanded_index = Some(idx);
                            this.start_editing(&entry, cx);
                        }
                        cx.notify();
                    }))
                    .end_slot(trash_button);

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

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().background)
            .p(DynamicSpacing::Base16.rems(cx))
            .child(content)
    }
}
