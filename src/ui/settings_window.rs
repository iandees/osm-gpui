//! Settings window with custom imagery management, OSM API server selection, and
//! OpenStreetMap OAuth login.

use gpui::*;

use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    label::Label,
    radio::RadioGroup,
    v_flex, ActiveTheme as _, StyledExt as _,
};

use crate::auth::{self, StoredToken};
use crate::custom_imagery_store::{self, CustomImageryEntry};
use crate::settings_store::{self, ApiServerChoice, AppSettings};

/// Login UI state for the currently-selected API server.
enum LoginState {
    LoggedOut,
    LoggingIn,
    LoggedIn(StoredToken),
    Error(SharedString),
}

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

    app_settings: AppSettings,
    custom_api_url_input: Entity<InputState>,
    custom_url_error: Option<SharedString>,
    client_id_input: Entity<InputState>,

    login_state: LoginState,
}

impl SettingsWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app_settings = settings_store::snapshot();
        let custom_api_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://example.com")
                .default_value(app_settings.custom_api_url.clone())
        });
        let oauth_base = auth::oauth_base_for(&app_settings.api_base_url());
        let client_id_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Default")
                .default_value(
                    app_settings
                        .client_ids
                        .get(&oauth_base)
                        .cloned()
                        .unwrap_or_default(),
                )
        });
        let login_state = Self::current_login_state(&app_settings);

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

            app_settings,
            custom_api_url_input,
            custom_url_error: None,
            client_id_input,

            login_state,
        }
    }

    fn current_login_state(settings: &AppSettings) -> LoginState {
        let oauth_base = auth::oauth_base_for(&settings.api_base_url());
        match auth::current_token(&oauth_base) {
            Some(token) => LoginState::LoggedIn(token),
            None => LoginState::LoggedOut,
        }
    }

    /// Refresh `client_id_input`'s displayed value to match the currently-selected
    /// server's configured override (or blank, if using the default client_id).
    fn refresh_client_id_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let oauth_base = auth::oauth_base_for(&self.app_settings.api_base_url());
        let value = self
            .app_settings
            .client_ids
            .get(&oauth_base)
            .cloned()
            .unwrap_or_default();
        self.client_id_input
            .update(cx, |state, cx| state.set_value(value, window, cx));
    }

    fn set_api_server(&mut self, choice: ApiServerChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.app_settings.api_server = choice;
        settings_store::update_store(self.app_settings.clone());
        self.login_state = Self::current_login_state(&self.app_settings);
        self.refresh_client_id_input(window, cx);
        cx.notify();
    }

    fn save_custom_api_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.custom_api_url_input.read(cx).value().to_string();
        let url = url.trim();
        if url.is_empty() || (!url.starts_with("http://") && !url.starts_with("https://")) {
            self.custom_url_error = Some("Enter a valid http(s) URL".into());
            cx.notify();
            return;
        }
        self.custom_url_error = None;
        self.app_settings.custom_api_url = url.trim_end_matches('/').to_string();
        settings_store::update_store(self.app_settings.clone());
        self.login_state = Self::current_login_state(&self.app_settings);
        self.refresh_client_id_input(window, cx);
        cx.notify();
    }

    fn save_client_id(&mut self, cx: &mut Context<Self>) {
        let oauth_base = auth::oauth_base_for(&self.app_settings.api_base_url());
        let client_id = self.client_id_input.read(cx).value().trim().to_string();
        if client_id.is_empty() {
            self.app_settings.client_ids.remove(&oauth_base);
        } else {
            self.app_settings.client_ids.insert(oauth_base, client_id);
        }
        settings_store::update_store(self.app_settings.clone());
        cx.notify();
    }

    fn start_login(&mut self, cx: &mut Context<Self>) {
        self.login_state = LoginState::LoggingIn;
        cx.notify();

        let api_base_url = self.app_settings.api_base_url();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { auth::login(&api_base_url) })
                .await;

            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(login) => {
                        this.login_state = LoginState::LoggedIn(login.token);
                    }
                    Err(e) => {
                        this.login_state = LoginState::Error(e.to_string().into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn logout(&mut self, cx: &mut Context<Self>) {
        let oauth_base = auth::oauth_base_for(&self.app_settings.api_base_url());
        auth::logout(&oauth_base);
        self.login_state = LoginState::LoggedOut;
        cx.notify();
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
        let foreground = cx.theme().foreground;

        let api_choice = self.app_settings.api_server;
        let selected_index = match api_choice {
            ApiServerChoice::Primary => 0,
            ApiServerChoice::Dev => 1,
            ApiServerChoice::Custom => 2,
        };

        let mut api_section = v_flex()
            .gap_2()
            .child(
                Label::new("OSM API Server")
                    .text_sm()
                    .font_semibold()
                    .text_color(foreground),
            )
            .child(
                RadioGroup::vertical("api-server")
                    .selected_index(Some(selected_index))
                    .on_click(cx.listener(|this, idx: &usize, window, cx| {
                        let choice = match idx {
                            0 => ApiServerChoice::Primary,
                            1 => ApiServerChoice::Dev,
                            _ => ApiServerChoice::Custom,
                        };
                        this.set_api_server(choice, window, cx);
                    }))
                    .child("Primary (api.openstreetmap.org)")
                    .child("Dev / testing (master.apis.dev.openstreetmap.org)")
                    .child("Custom"),
            );

        if matches!(api_choice, ApiServerChoice::Custom) {
            let mut custom_row = v_flex()
                .gap_1()
                .pl_6()
                .child(field_row("Custom API URL", &self.custom_api_url_input, muted));
            if let Some(err) = &self.custom_url_error {
                custom_row = custom_row.child(Label::new(err.clone()).text_sm().text_color(danger));
            }
            custom_row = custom_row.child(
                Button::new("save-custom-api-url")
                    .label("Save")
                    .primary()
                    .compact()
                    .on_click(cx.listener(|this, _ev, window, cx| this.save_custom_api_url(window, cx))),
            );
            api_section = api_section.child(custom_row);
        }

        let mut client_id_row = v_flex()
            .gap_1()
            .pl_6()
            .child(field_row("OAuth Client ID (leave blank for default)", &self.client_id_input, muted));
        client_id_row = client_id_row.child(
            Button::new("save-client-id")
                .label("Save")
                .primary()
                .compact()
                .on_click(cx.listener(|this, _ev, _window, cx| this.save_client_id(cx))),
        );
        api_section = api_section.child(client_id_row);

        let login_section = v_flex()
            .gap_2()
            .child(
                Label::new("OpenStreetMap Account")
                    .text_sm()
                    .font_semibold()
                    .text_color(foreground),
            )
            .child(match &self.login_state {
                LoginState::LoggedOut => Button::new("login")
                    .label("Sign in with OpenStreetMap")
                    .primary()
                    .on_click(cx.listener(|this, _ev, _window, cx| this.start_login(cx)))
                    .into_any_element(),
                LoginState::LoggingIn => h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Label::new("Signing in… complete login in your browser.")
                            .text_sm()
                            .text_color(muted),
                    )
                    .into_any_element(),
                LoginState::LoggedIn(token) => h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Label::new(format!("✅ Logged in as {}", token.display_name))
                            .text_sm()
                            .text_color(foreground),
                    )
                    .child(
                        Button::new("logout")
                            .label("Sign out")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _ev, _window, cx| this.logout(cx))),
                    )
                    .into_any_element(),
                LoginState::Error(msg) => v_flex()
                    .gap_2()
                    .child(Label::new(msg.clone()).text_sm().text_color(danger))
                    .child(
                        Button::new("login-retry")
                            .label("Try again")
                            .primary()
                            .compact()
                            .on_click(cx.listener(|this, _ev, _window, cx| this.start_login(cx))),
                    )
                    .into_any_element(),
            });

        let mut content = v_flex()
            .gap_4()
            .child(api_section)
            .child(login_section)
            .child(
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
