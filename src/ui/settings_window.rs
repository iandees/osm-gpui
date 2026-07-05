//! Settings window with custom imagery management.

use gpui::*;

use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _, StyledExt as _,
};

use crate::custom_imagery_store::{self, CustomImageryEntry};

pub struct SettingsWindow {
    focus_handle: FocusHandle,
    entries: Vec<CustomImageryEntry>,
    expanded_index: Option<usize>,
    confirm_delete_index: Option<usize>,
    edit_name: Option<Entity<InputState>>,
    edit_url: Option<Entity<InputState>>,
    edit_min_zoom: Option<Entity<InputState>>,
    edit_max_zoom: Option<Entity<InputState>>,
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

    fn start_editing(
        &mut self,
        entry: &CustomImageryEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Name")
                .default_value(entry.name.clone())
        });
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://…/{z}/{x}/{y}.png")
                .default_value(entry.url_template.clone())
        });
        let min_zoom = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("0")
                .default_value(entry.min_zoom.to_string())
        });
        let max_zoom = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("19")
                .default_value(entry.max_zoom.to_string())
        });

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

        let name_val = name.read(cx).value().to_string();
        let url_val = url.read(cx).value().to_string();
        let min_val = min_z.read(cx).value().to_string();
        let max_val = max_z.read(cx).value().to_string();

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

    fn add_new_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.start_editing(&blank, window, cx);
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

fn field_row(label: &'static str, input: &Entity<InputState>, muted: Hsla) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(Label::new(label).text_xs().text_color(muted))
        .child(Input::new(input))
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let border = cx.theme().border;

        let mut content = v_flex().gap_2().child(
            Label::new("Custom Imagery Sources")
                .text_sm()
                .font_semibold()
                .text_color(cx.theme().foreground),
        );

        if self.entries.is_empty() {
            content = content.child(
                Label::new("No custom imagery sources configured.")
                    .text_sm()
                    .text_color(muted),
            );
        } else {
            for (idx, entry) in self.entries.iter().enumerate() {
                let is_expanded = self.expanded_index == Some(idx);
                let entry_name = entry.name.clone();

                let end_slot: AnyElement = if self.confirm_delete_index == Some(idx) {
                    let name_for_label = entry_name.clone();
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Label::new(format!("Delete {}?", name_for_label))
                                .text_sm()
                                .text_color(danger),
                        )
                        .child(
                            Button::new(("confirm-delete", idx))
                                .label("Delete")
                                .danger()
                                .compact()
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    this.delete_entry(idx, cx);
                                })),
                        )
                        .child(
                            Button::new(("cancel-delete", idx))
                                .label("Cancel")
                                .ghost()
                                .compact()
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    this.confirm_delete_index = None;
                                    cx.notify();
                                })),
                        )
                        .into_any_element()
                } else {
                    Button::new(("trash", idx))
                        .label("Delete")
                        .ghost()
                        .compact()
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.confirm_delete_index = Some(idx);
                            cx.notify();
                        }))
                        .into_any_element()
                };

                let row_toggle_idx = idx;
                let row = h_flex()
                    .id(("entry", idx))
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _ev, window, cx| {
                        let idx = row_toggle_idx;
                        if this.expanded_index == Some(idx) {
                            this.expanded_index = None;
                            this.clear_editing();
                        } else {
                            let entry = this.entries[idx].clone();
                            this.expanded_index = Some(idx);
                            this.confirm_delete_index = None;
                            this.start_editing(&entry, window, cx);
                        }
                        cx.notify();
                    }))
                    .child(Label::new(entry_name))
                    .child(end_slot);

                content = content.child(row);

                if is_expanded {
                    if let (Some(edit_name), Some(edit_url), Some(edit_min_zoom), Some(edit_max_zoom)) = (
                        self.edit_name.clone(),
                        self.edit_url.clone(),
                        self.edit_min_zoom.clone(),
                        self.edit_max_zoom.clone(),
                    ) {
                        let mut expanded_content = v_flex()
                            .pl_6()
                            .gap_2()
                            .child(field_row("Name", &edit_name, muted))
                            .child(field_row("URL template", &edit_url, muted))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .child(field_row("Min zoom", &edit_min_zoom, muted)),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .child(field_row("Max zoom", &edit_max_zoom, muted)),
                                    ),
                            );

                        if let Some(err) = &self.edit_error {
                            expanded_content = expanded_content.child(
                                Label::new(err.clone()).text_sm().text_color(danger),
                            );
                        }

                        let save_btn = Button::new(("save", idx))
                            .label("Save")
                            .primary()
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                this.save_entry(idx, cx);
                            }));

                        let cancel_btn = Button::new(("cancel", idx))
                            .label("Cancel")
                            .ghost()
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

                        expanded_content = expanded_content
                            .child(h_flex().gap_2().child(save_btn).child(cancel_btn));

                        content = content.child(expanded_content);
                    }
                }
            }
        }

        content = content.child(
            Button::new("add-source")
                .label("Add Source")
                .ghost()
                .on_click(cx.listener(|this, _ev, window, cx| {
                    this.add_new_entry(window, cx);
                })),
        );

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .p_4()
            .child(content)
    }
}
