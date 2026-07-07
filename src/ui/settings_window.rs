//! Settings window with custom imagery management, OSM API server selection, and
//! OpenStreetMap OAuth login. Built on gpui-component's `Settings` widget, which
//! supplies page/group navigation and search chrome; this module only builds the
//! `Vec<SettingPage>` from `SettingsWindow`'s state.

use gpui::*;

use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    label::Label,
    radio::RadioGroup,
    setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    v_flex, ActiveTheme as _, Icon, IconName,
};

use crate::auth::{self, StoredToken};
use crate::custom_imagery_store::{self, CustomImageryEntry};
use crate::settings_store::{self, ApiServerChoice, AppSettings};

/// Login UI state for the currently-selected API server.
#[derive(Clone)]
enum LoginState {
    LoggedOut,
    LoggingIn,
    LoggedIn(StoredToken),
    Error(SharedString),
}

pub struct SettingsWindow {
    focus_handle: FocusHandle,
    entries: Vec<CustomImageryEntry>,
    editing_index: Option<usize>,
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
            editing_index: None,
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

    fn start_edit_at(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let entry = self.entries[idx].clone();
        self.editing_index = Some(idx);
        self.confirm_delete_index = None;
        self.start_editing(&entry, window, cx);
        cx.notify();
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
                self.editing_index = None;
                self.clear_editing();
                cx.notify();
            }
            Err(e) => {
                self.edit_error = Some(crate::ui::custom_imagery_dialog::error_message(&e).into());
                cx.notify();
            }
        }
    }

    fn cancel_edit(&mut self, idx: usize, cx: &mut Context<Self>) {
        if let Some(entry) = self.entries.get(idx) {
            if entry.name.is_empty() && entry.url_template.is_empty() {
                self.entries.remove(idx);
            }
        }
        self.editing_index = None;
        self.clear_editing();
        cx.notify();
    }

    fn delete_entry(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx < self.entries.len() {
            self.entries.remove(idx);
            self.persist();
        }
        self.editing_index = None;
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
        self.editing_index = Some(new_idx);
        self.confirm_delete_index = None;
        self.start_editing(&blank, window, cx);
        cx.notify();
    }

    fn persist(&self) {
        custom_imagery_store::update_store(self.entries.clone());
    }

    fn setting_pages(&self, cx: &mut Context<Self>) -> Vec<SettingPage> {
        let view = cx.entity();
        vec![self.account_page(view.clone()), self.imagery_page(view)]
    }

    fn account_page(&self, view: Entity<Self>) -> SettingPage {
        let api_choice = self.app_settings.api_server;

        let server_view = view.clone();
        let mut api_items = vec![SettingItem::new(
            "Server",
            SettingField::render(move |_options, window, cx| {
                render_server_picker(api_choice, server_view.clone(), window, cx)
            }),
        )
        .description("Choose which OpenStreetMap API server to use.")];

        if matches!(api_choice, ApiServerChoice::Custom) {
            let custom_view = view.clone();
            let input = self.custom_api_url_input.clone();
            let error = self.custom_url_error.clone();
            api_items.push(
                SettingItem::new(
                    "Custom API URL",
                    SettingField::render(move |_options, window, cx| {
                        render_custom_api_url(custom_view.clone(), input.clone(), error.clone(), window, cx)
                    }),
                )
                .description("The base URL of a self-hosted or alternate OSM API server."),
            );
        }

        let client_id_view = view.clone();
        let client_id_input = self.client_id_input.clone();
        api_items.push(
            SettingItem::new(
                "OAuth Client ID",
                SettingField::render(move |_options, window, cx| {
                    render_client_id(client_id_view.clone(), client_id_input.clone(), window, cx)
                }),
            )
            .description("Override the OAuth client_id used for this server (leave blank for default)."),
        );

        let login_view = view;
        let login_state = self.login_state.clone();
        let login_item = SettingItem::new(
            "Account",
            SettingField::render(move |_options, window, cx| {
                render_login_state(login_view.clone(), login_state.clone(), window, cx)
            }),
        )
        .description("Sign in with your OpenStreetMap account to edit data.");

        SettingPage::new("Account")
            .icon(Icon::new(IconName::User))
            .default_open(true)
            .groups(vec![
                SettingGroup::new().title("API Server").items(api_items),
                SettingGroup::new()
                    .title("OpenStreetMap Account")
                    .item(login_item),
            ])
    }

    fn imagery_page(&self, view: Entity<Self>) -> SettingPage {
        let mut items = Vec::new();

        if self.entries.is_empty() {
            items.push(SettingItem::render(|_options, _window, _cx| {
                Label::new("No custom imagery sources configured.")
            }));
        }

        for (idx, entry) in self.entries.iter().enumerate() {
            let title = if entry.name.is_empty() {
                "New Source".to_string()
            } else {
                entry.name.clone()
            };
            let is_editing = self.editing_index == Some(idx);
            let is_confirming_delete = self.confirm_delete_index == Some(idx);

            let mut item = if is_editing {
                let entry_view = view.clone();
                let edit_name = self.edit_name.clone();
                let edit_url = self.edit_url.clone();
                let edit_min_zoom = self.edit_min_zoom.clone();
                let edit_max_zoom = self.edit_max_zoom.clone();
                let edit_error = self.edit_error.clone();
                SettingItem::new(
                    title,
                    SettingField::render(move |_options, window, cx| {
                        render_entry_edit_form(
                            entry_view.clone(),
                            idx,
                            edit_name.clone(),
                            edit_url.clone(),
                            edit_min_zoom.clone(),
                            edit_max_zoom.clone(),
                            edit_error.clone(),
                            window,
                            cx,
                        )
                    }),
                )
            } else {
                let entry_view = view.clone();
                let entry_summary: SharedString =
                    format!("{} · zoom {}–{}", entry.url_template, entry.min_zoom, entry.max_zoom)
                        .into();
                let entry_name = entry.name.clone();
                SettingItem::new(
                    title,
                    SettingField::render(move |_options, window, cx| {
                        render_entry_row(
                            entry_view.clone(),
                            idx,
                            entry_name.clone(),
                            is_confirming_delete,
                            window,
                            cx,
                        )
                    }),
                )
                .description(entry_summary)
            };

            item = item.keywords([entry.name.clone(), entry.url_template.clone()]);
            items.push(item);
        }

        let add_view = view;
        items.push(SettingItem::render(move |_options, _window, _cx| {
            Button::new("add-source")
                .label("Add Source")
                .ghost()
                .on_click({
                    let add_view = add_view.clone();
                    move |_ev, window, cx| {
                        add_view.update(cx, |this, cx| this.add_new_entry(window, cx));
                    }
                })
        }));

        SettingPage::new("Imagery Sources")
            .icon(Icon::new(IconName::Map))
            .group(
                SettingGroup::new()
                    .title("Custom Imagery Sources")
                    .items(items),
            )
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

fn render_server_picker(
    api_choice: ApiServerChoice,
    view: Entity<SettingsWindow>,
    _window: &mut Window,
    _cx: &mut App,
) -> impl IntoElement {
    let selected_index = match api_choice {
        ApiServerChoice::Primary => 0,
        ApiServerChoice::Dev => 1,
        ApiServerChoice::Custom => 2,
    };

    RadioGroup::vertical("api-server")
        .selected_index(Some(selected_index))
        .on_click(move |idx: &usize, window, cx| {
            let choice = match idx {
                0 => ApiServerChoice::Primary,
                1 => ApiServerChoice::Dev,
                _ => ApiServerChoice::Custom,
            };
            view.update(cx, |this, cx| this.set_api_server(choice, window, cx));
        })
        .child("Primary (api.openstreetmap.org)")
        .child("Dev / testing (master.apis.dev.openstreetmap.org)")
        .child("Custom")
}

fn render_custom_api_url(
    view: Entity<SettingsWindow>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    _window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    let mut row = v_flex().gap_2().child(field_row("URL", &input, muted));
    if let Some(err) = error {
        row = row.child(Label::new(err).text_sm().text_color(danger));
    }
    row.child(
        Button::new("save-custom-api-url")
            .label("Save")
            .primary()
            .compact()
            .on_click(move |_ev, window, cx| {
                view.update(cx, |this, cx| this.save_custom_api_url(window, cx));
            }),
    )
}

fn render_client_id(
    view: Entity<SettingsWindow>,
    input: Entity<InputState>,
    _window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;

    v_flex()
        .gap_2()
        .child(field_row("Client ID", &input, muted))
        .child(
            Button::new("save-client-id")
                .label("Save")
                .primary()
                .compact()
                .on_click(move |_ev, _window, cx| {
                    view.update(cx, |this, cx| this.save_client_id(cx));
                }),
        )
}

fn render_login_state(
    view: Entity<SettingsWindow>,
    login_state: LoginState,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;
    let foreground = cx.theme().foreground;

    match login_state {
        LoginState::LoggedOut => Button::new("login")
            .label("Sign in with OpenStreetMap")
            .primary()
            .on_click(move |_ev, _window, cx| {
                view.update(cx, |this, cx| this.start_login(cx));
            })
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
                    .on_click(move |_ev, _window, cx| {
                        view.update(cx, |this, cx| this.logout(cx));
                    }),
            )
            .into_any_element(),
        LoginState::Error(msg) => v_flex()
            .gap_2()
            .child(Label::new(msg).text_sm().text_color(danger))
            .child(
                Button::new("login-retry")
                    .label("Try again")
                    .primary()
                    .compact()
                    .on_click(move |_ev, _window, cx| {
                        view.update(cx, |this, cx| this.start_login(cx));
                    }),
            )
            .into_any_element(),
    }
}

fn render_entry_row(
    view: Entity<SettingsWindow>,
    idx: usize,
    entry_name: String,
    is_confirming_delete: bool,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let danger = cx.theme().danger;

    if is_confirming_delete {
        h_flex()
            .gap_2()
            .items_center()
            .child(
                Label::new(format!("Delete {}?", entry_name))
                    .text_sm()
                    .text_color(danger),
            )
            .child({
                let view = view.clone();
                Button::new(("confirm-delete", idx))
                    .label("Delete")
                    .danger()
                    .compact()
                    .on_click(move |_ev, _window, cx| {
                        view.update(cx, |this, cx| this.delete_entry(idx, cx));
                    })
            })
            .child(
                Button::new(("cancel-delete", idx))
                    .label("Cancel")
                    .ghost()
                    .compact()
                    .on_click(move |_ev, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.confirm_delete_index = None;
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    } else {
        h_flex()
            .gap_2()
            .child({
                let view = view.clone();
                Button::new(("edit", idx))
                    .label("Edit")
                    .ghost()
                    .compact()
                    .on_click(move |_ev, window, cx| {
                        view.update(cx, |this, cx| this.start_edit_at(idx, window, cx));
                    })
            })
            .child(
                Button::new(("trash", idx))
                    .label("Delete")
                    .ghost()
                    .compact()
                    .on_click(move |_ev, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.confirm_delete_index = Some(idx);
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn render_entry_edit_form(
    view: Entity<SettingsWindow>,
    idx: usize,
    edit_name: Option<Entity<InputState>>,
    edit_url: Option<Entity<InputState>>,
    edit_min_zoom: Option<Entity<InputState>>,
    edit_max_zoom: Option<Entity<InputState>>,
    edit_error: Option<SharedString>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    let (Some(edit_name), Some(edit_url), Some(edit_min_zoom), Some(edit_max_zoom)) =
        (edit_name, edit_url, edit_min_zoom, edit_max_zoom)
    else {
        return div().into_any_element();
    };

    let mut content = v_flex()
        .gap_2()
        .child(field_row("Name", &edit_name, muted))
        .child(field_row("URL template", &edit_url, muted))
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(field_row("Min zoom", &edit_min_zoom, muted)))
                .child(div().flex_1().child(field_row("Max zoom", &edit_max_zoom, muted))),
        );

    if let Some(err) = edit_error {
        content = content.child(Label::new(err).text_sm().text_color(danger));
    }

    let save_view = view.clone();
    let cancel_view = view;
    content
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new(("save", idx))
                        .label("Save")
                        .primary()
                        .on_click(move |_ev, _window, cx| {
                            save_view.update(cx, |this, cx| this.save_entry(idx, cx));
                        }),
                )
                .child(
                    Button::new(("cancel", idx))
                        .label("Cancel")
                        .ghost()
                        .on_click(move |_ev, _window, cx| {
                            cancel_view.update(cx, |this, cx| this.cancel_edit(idx, cx));
                        }),
                ),
        )
        .into_any_element()
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .child(Settings::new("app-settings").pages(self.setting_pages(cx)))
    }
}
