//! Settings window with custom imagery management, OSM API server selection, and
//! OpenStreetMap OAuth login. Built on gpui-component's `Settings` widget, which
//! supplies page/group navigation and search chrome; this module only builds the
//! `Vec<SettingPage>` from `SettingsWindow`'s state.

use gpui::*;

use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::InputState,
    label::Label,
    radio::{Radio, RadioGroup},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};

use crate::auth::{self, StoredToken};
use crate::custom_imagery_store::{self, CustomImageryEntry};
use crate::keybindings::{self, ShortcutCategory, SHORTCUTS};
use crate::settings_store::{self, ApiServerChoice, AppSettings, TextSizePreset};
use crate::ui::modal::field_row;

/// Login UI state for the currently-selected API server.
#[derive(Clone)]
enum LoginState {
    LoggedOut,
    LoggingIn,
    LoggedIn(StoredToken),
    Error(SharedString),
}

/// Emitted by `SettingsWindow` when a change needs to propagate outside
/// this window — e.g. rebinding the live `gpui` keymap and refreshing the
/// native menu's shortcut labels, both of which require the concrete
/// `Action` types that only the binary crate (`src/main.rs`/`src/menu.rs`)
/// has access to.
pub enum SettingsEvent {
    KeybindingsChanged,
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
    edit_cache_budget: Entity<InputState>,
    cache_budget_error: Option<SharedString>,
    cache_clear_error: Option<SharedString>,

    login_state: LoginState,

    recording: Option<&'static str>,
    shortcut_error: Option<(&'static str, SharedString)>,
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
        let edit_cache_budget = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("500")
                .default_value(app_settings.cache_budget_mb.to_string())
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
            edit_cache_budget,
            cache_budget_error: None,
            cache_clear_error: None,

            login_state,

            recording: None,
            shortcut_error: None,
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

    fn set_api_server(
        &mut self,
        choice: ApiServerChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn set_text_size_preset(&mut self, preset: TextSizePreset, cx: &mut Context<Self>) {
        self.app_settings.text_size_preset = preset;
        settings_store::update_store(self.app_settings.clone());
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

    fn save_cache_budget(&mut self, cx: &mut Context<Self>) {
        let raw = self.edit_cache_budget.read(cx).value().trim().to_string();
        match raw.parse::<u64>() {
            Ok(mb) if mb >= 10 => {
                self.cache_budget_error = None;
                self.app_settings.cache_budget_mb = mb;
                settings_store::update_store(self.app_settings.clone());
                crate::tile_cache::set_budget_mb(mb);
            }
            Ok(_) => {
                self.cache_budget_error = Some("Budget must be at least 10 MB".into());
            }
            Err(_) => {
                self.cache_budget_error = Some("Enter a whole number of megabytes".into());
            }
        }
        cx.notify();
    }

    fn clear_cache_source(&mut self, key: String, cx: &mut Context<Self>) {
        if let Err(e) = crate::tile_cache::clear_source(&key) {
            self.cache_clear_error = Some(format!("Failed to clear {}: {}", key, e).into());
        } else {
            self.cache_clear_error = None;
        }
        cx.notify();
    }

    fn clear_all_tile_cache(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = crate::tile_cache::clear_all_cache() {
            self.cache_clear_error = Some(format!("Failed to clear cache: {}", e).into());
        } else {
            self.cache_clear_error = None;
        }
        cx.notify();
    }

    /// Remove `id`'s override, falling back to its default, and propagate
    /// the change to the live keymap and native menu.
    fn reset_shortcut(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.app_settings.keybindings.remove(id);
        settings_store::update_store(self.app_settings.clone());
        cx.emit(SettingsEvent::KeybindingsChanged);
        cx.notify();
    }

    /// Clear every shortcut override, restoring all ten defaults.
    fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.app_settings.keybindings.clear();
        settings_store::update_store(self.app_settings.clone());
        cx.emit(SettingsEvent::KeybindingsChanged);
        cx.notify();
    }

    fn start_recording(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = Some(id);
        self.shortcut_error = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn cancel_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        self.shortcut_error = None;
        cx.notify();
    }

    /// Validate and, if valid, save `spec` as `id`'s override. On failure,
    /// sets `shortcut_error` and leaves `recording` active so the user can
    /// try another combo.
    fn apply_shortcut(&mut self, id: &'static str, spec: String, cx: &mut Context<Self>) {
        if keybindings::is_reserved(&spec) {
            self.shortcut_error = Some((id, "That key is reserved.".into()));
            cx.notify();
            return;
        }
        if keybindings::def(id).category == keybindings::ShortcutCategory::Modes
            && !keybindings::is_bare_key(&spec)
        {
            self.shortcut_error = Some((id, "Mode shortcuts can't use modifier keys.".into()));
            cx.notify();
            return;
        }
        if let Some(other_label) = keybindings::conflict(&self.app_settings, id, &spec) {
            self.shortcut_error = Some((id, format!("Already used by {other_label}.").into()));
            cx.notify();
            return;
        }

        self.app_settings.keybindings.insert(id.to_string(), spec);
        settings_store::update_store(self.app_settings.clone());
        self.recording = None;
        self.shortcut_error = None;
        cx.emit(SettingsEvent::KeybindingsChanged);
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
        vec![
            self.account_page(view.clone()),
            self.appearance_page(view.clone()),
            self.imagery_page(view.clone()),
            self.shortcuts_page(view),
        ]
    }

    fn appearance_page(&self, view: Entity<Self>) -> SettingPage {
        let value_view = view.clone();
        let set_value_view = view;
        let item = SettingItem::new(
            "Text Size",
            SettingField::dropdown(
                TextSizePreset::ALL
                    .into_iter()
                    .map(|p| (p.as_key().into(), p.label().into()))
                    .collect(),
                move |cx: &App| {
                    SharedString::from(value_view.read(cx).app_settings.text_size_preset.as_key())
                },
                move |val: SharedString, cx: &mut App| {
                    let Some(preset) = TextSizePreset::from_key(&val) else {
                        return;
                    };
                    set_value_view.update(cx, |this, cx| this.set_text_size_preset(preset, cx));
                },
            ),
        )
        .description("Size of text throughout the app.");

        SettingPage::new("Appearance")
            .icon(Icon::new(IconName::Palette))
            .groups(vec![SettingGroup::new().title("Text").item(item)])
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
        .description("Choose which OpenStreetMap API server to use.")
        .layout(Axis::Vertical)];

        if matches!(api_choice, ApiServerChoice::Custom) {
            let custom_view = view.clone();
            let input = self.custom_api_url_input.clone();
            let error = self.custom_url_error.clone();
            api_items.push(
                SettingItem::new(
                    "Custom API URL",
                    SettingField::render(move |_options, window, cx| {
                        render_custom_api_url(
                            custom_view.clone(),
                            input.clone(),
                            error.clone(),
                            window,
                            cx,
                        )
                    }),
                )
                .description("The base URL of a self-hosted or alternate OSM API server.")
                .layout(Axis::Vertical),
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
            .description(
                "Override the OAuth client_id used for this server (leave blank for default).",
            )
            .layout(Axis::Vertical),
        );

        let login_view = view;
        let login_state = self.login_state.clone();
        let login_item = SettingItem::new(
            "Account",
            SettingField::render(move |_options, window, cx| {
                render_login_state(login_view.clone(), login_state.clone(), window, cx)
            }),
        )
        .description("Sign in with your OpenStreetMap account to edit data.")
        .layout(Axis::Vertical);

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
                .layout(Axis::Vertical)
            } else {
                let entry_view = view.clone();
                let entry_summary: SharedString = format!(
                    "{} · zoom {}–{}",
                    entry.url_template, entry.min_zoom, entry.max_zoom
                )
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

        let add_view = view.clone();
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

        SettingPage::new("Imagery")
            .icon(Icon::new(IconName::Map))
            .groups(vec![
                SettingGroup::new().title("Custom Sources").items(items),
                self.cache_group(view),
            ])
    }

    fn cache_group(&self, view: Entity<Self>) -> SettingGroup {
        let summary = crate::tile_cache::cache_summary();

        // Exclude the synthetic "(uncategorized)" bucket from the reported
        // source count: it's leftover loose files from before per-source
        // directories existed, not a real configured imagery source, so
        // including it would inflate the count a user sees relative to how
        // many layers they actually configured.
        let real_source_count = summary
            .sources
            .iter()
            .filter(|s| s.key != crate::tile_cache::uncategorized_key())
            .count();

        let summary_text: SharedString = format!(
            "{} across {} tile{} in {} source{}",
            crate::tile_cache::format_bytes(summary.total_bytes),
            summary.total_files,
            if summary.total_files == 1 { "" } else { "s" },
            real_source_count,
            if real_source_count == 1 { "" } else { "s" },
        )
        .into();

        let clear_error = self.cache_clear_error.clone();
        let mut items = vec![SettingItem::render(move |_options, _window, cx| {
            render_cache_usage_summary(summary_text.clone(), clear_error.clone(), cx)
        })];

        let budget_view = view.clone();
        let budget_input = self.edit_cache_budget.clone();
        let budget_error = self.cache_budget_error.clone();
        items.push(
            SettingItem::new(
                "Cache budget (MB)",
                SettingField::render(move |_options, window, cx| {
                    render_cache_budget(
                        budget_view.clone(),
                        budget_input.clone(),
                        budget_error.clone(),
                        window,
                        cx,
                    )
                }),
            )
            .description(
                "Maximum on-disk tile cache size. Lowering this doesn't delete anything \
                 immediately — the cache shrinks to the new limit the next time it's \
                 written to.",
            )
            .layout(Axis::Vertical),
        );

        if summary.sources.is_empty() {
            items.push(SettingItem::render(|_options, _window, _cx| {
                Label::new("No cached tiles yet.")
            }));
        }

        for source in summary.sources {
            let row_view = view.clone();
            let key = source.key.clone();
            let size_label: SharedString = format!(
                "{} · {} tile{}",
                crate::tile_cache::format_bytes(source.bytes),
                source.file_count,
                if source.file_count == 1 { "" } else { "s" },
            )
            .into();
            items.push(
                SettingItem::new(
                    source.key.clone(),
                    SettingField::render(move |_options, _window, _cx| {
                        render_cache_source_row(row_view.clone(), key.clone())
                    }),
                )
                .description(size_label),
            );
        }

        let clear_all_view = view;
        items.push(SettingItem::render(move |_options, _window, _cx| {
            let clear_all_view = clear_all_view.clone();
            Button::new("clear-all-cache")
                .label("Clear All")
                .ghost()
                .compact()
                .on_click(move |_ev, _window, cx| {
                    clear_all_view.update(cx, |this, cx| this.clear_all_tile_cache(cx));
                })
        }));

        SettingGroup::new().title("Tile Cache").items(items)
    }

    fn shortcuts_page(&self, view: Entity<Self>) -> SettingPage {
        let category_title = |c: ShortcutCategory| match c {
            ShortcutCategory::General => "General",
            ShortcutCategory::File => "File",
            ShortcutCategory::Edit => "Edit",
            ShortcutCategory::Modes => "Modes",
        };

        let mut groups = Vec::new();
        for category in [
            ShortcutCategory::General,
            ShortcutCategory::File,
            ShortcutCategory::Edit,
            ShortcutCategory::Modes,
        ] {
            let items: Vec<SettingItem> = SHORTCUTS
                .iter()
                .filter(|d| d.category == category)
                .map(|d| {
                    let id = d.id;
                    let label = d.label;
                    let has_override = self.app_settings.keybindings.contains_key(id);
                    let spec = keybindings::effective_spec(&self.app_settings, id);
                    let row_view = view.clone();
                    let recording = self.recording == Some(id);
                    let error = self
                        .shortcut_error
                        .as_ref()
                        .filter(|(err_id, _)| *err_id == id)
                        .map(|(_, msg)| msg.clone());
                    let focus_handle = self.focus_handle.clone();
                    SettingItem::new(
                        label,
                        SettingField::render(move |_options, _window, cx| {
                            render_shortcut_row(
                                row_view.clone(),
                                id,
                                spec.clone(),
                                has_override,
                                recording,
                                error.clone(),
                                focus_handle.clone(),
                                cx,
                            )
                        }),
                    )
                })
                .collect();
            groups.push(
                SettingGroup::new()
                    .title(category_title(category))
                    .items(items),
            );
        }

        let reset_all_view = view;
        groups.push(SettingGroup::new().item(SettingItem::render(
            move |_options, _window, _cx| {
                Button::new("reset-all-shortcuts")
                    .label("Reset All to Defaults")
                    .ghost()
                    .on_click({
                        let reset_all_view = reset_all_view.clone();
                        move |_ev, _window, cx| {
                            reset_all_view.update(cx, |this, cx| this.reset_all_shortcuts(cx));
                        }
                    })
            },
        )));

        SettingPage::new("Keyboard Shortcuts").groups(groups)
    }
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<SettingsEvent> for SettingsWindow {}

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
        .child(Radio::from("Primary (api.openstreetmap.org)").small())
        .child(Radio::from("Dev / testing (master.apis.dev.openstreetmap.org)").small())
        .child(Radio::from("Custom").small())
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
        row = row.child(Label::new(err).text_color(danger));
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

fn render_cache_usage_summary(
    summary_text: SharedString,
    clear_error: Option<SharedString>,
    cx: &mut App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;
    let mut col = v_flex()
        .gap_1()
        .child(Label::new(summary_text).text_sm().text_color(muted));
    if let Some(err) = clear_error {
        col = col.child(Label::new(err).text_sm().text_color(danger));
    }
    col.into_any_element()
}

fn render_cache_budget(
    view: Entity<SettingsWindow>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    _window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    let mut row = v_flex()
        .gap_2()
        .child(field_row("Megabytes", &input, muted));
    if let Some(err) = error {
        row = row.child(Label::new(err).text_sm().text_color(danger));
    }
    row.child(
        Button::new("save-cache-budget")
            .label("Save")
            .primary()
            .compact()
            .on_click(move |_ev, _window, cx| {
                view.update(cx, |this, cx| this.save_cache_budget(cx));
            }),
    )
}

fn render_cache_source_row(view: Entity<SettingsWindow>, key: String) -> AnyElement {
    let button_id = format!("clear-cache-source-{key}");
    Button::new(button_id)
        .label("Clear")
        .ghost()
        .compact()
        .on_click(move |_ev, _window, cx| {
            view.update(cx, |this, cx| this.clear_cache_source(key.clone(), cx));
        })
        .into_any_element()
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
            .child(Label::new("Signing in… complete login in your browser.").text_color(muted))
            .into_any_element(),
        LoginState::LoggedIn(token) => h_flex()
            .gap_2()
            .items_center()
            .child(
                Label::new(format!("✅ Logged in as {}", token.display_name))
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
            .child(Label::new(msg).text_color(danger))
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
            .child(Label::new(format!("Delete {}?", entry_name)).text_color(danger))
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
fn render_shortcut_row(
    view: Entity<SettingsWindow>,
    id: &'static str,
    spec: String,
    has_override: bool,
    recording: bool,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    cx: &mut App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    if recording {
        let capture_view = view.clone();

        let mut row = v_flex().gap_1().child(
            div()
                .track_focus(&focus_handle)
                .on_key_down({
                    let capture_view = capture_view.clone();
                    move |ev: &gpui::KeyDownEvent, _window, cx| {
                        if ev.keystroke.key.is_empty() {
                            // Modifier-only keydown (e.g. bare Cmd) — keep
                            // waiting for a real key.
                            return;
                        }
                        if ev.keystroke.key == "escape" {
                            capture_view.update(cx, |this, cx| this.cancel_recording(cx));
                            return;
                        }
                        let spec = ev.keystroke.unparse();
                        capture_view.update(cx, |this, cx| this.apply_shortcut(id, spec, cx));
                    }
                })
                .child(
                    Label::new("Press keys… (Esc to cancel)")
                        .text_sm()
                        .text_color(muted),
                ),
        );
        if let Some(msg) = error {
            row = row.child(Label::new(msg).text_xs().text_color(danger));
        }
        row.into_any_element()
    } else {
        let mut row = h_flex().gap_2().items_center();

        if let Ok(stroke) = gpui::Keystroke::parse(&spec) {
            row = row.child(gpui_component::kbd::Kbd::new(stroke));
        } else {
            row = row.child(Label::new(spec).text_sm().text_color(muted));
        }

        row = row.child(
            Button::new(SharedString::from(format!("record-shortcut-{id}")))
                .label("Record")
                .ghost()
                .compact()
                .on_click({
                    let view = view.clone();
                    move |_ev, window, cx| {
                        view.update(cx, |this, cx| this.start_recording(id, window, cx));
                    }
                }),
        );

        if has_override {
            row = row.child(
                Button::new(SharedString::from(format!("reset-shortcut-{id}")))
                    .label("Reset")
                    .ghost()
                    .compact()
                    .on_click(move |_ev, _window, cx| {
                        view.update(cx, |this, cx| this.reset_shortcut(id, cx));
                    }),
            );
        }

        row.into_any_element()
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

    if let Some(err) = edit_error {
        content = content.child(Label::new(err).text_color(danger));
    }

    let save_view = view.clone();
    let cancel_view = view;
    content
        .child(
            h_flex()
                .gap_2()
                .child(Button::new(("save", idx)).label("Save").primary().on_click(
                    move |_ev, _window, cx| {
                        save_view.update(cx, |this, cx| this.save_entry(idx, cx));
                    },
                ))
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
            .text_size(crate::ui::style::current_text_scale().body)
            .child(Settings::new("app-settings").pages(self.setting_pages(cx)))
    }
}
