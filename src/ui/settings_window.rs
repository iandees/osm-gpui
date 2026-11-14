//! The Settings window (see [`SettingsPage`] for its pages). Built on
//! gpui-component's `Settings` widget, which supplies page/group navigation
//! and search chrome; this module only builds the `Vec<SettingPage>` from
//! `SettingsWindow`'s state.

use gpui::*;
use std::sync::Arc;

use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::InputState,
    label::Label,
    radio::{Radio, RadioGroup},
    setting::{SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};

use crate::auth::{self, StoredToken};
use crate::custom_imagery_store::{self, CustomImageryEntry};
use crate::keybindings::{self, ShortcutCategory, SHORTCUTS};
use crate::settings_store::{self, ApiServerChoice, AppSettings, TextSizePreset, ThemePreference};
use crate::ui::modal::field_row;
use crate::{tr, tr_count};

/// Login UI state for the currently-selected API server.
#[derive(Clone, Debug, PartialEq)]
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
    /// The active MapCSS style changed (`style::custom::set_active` has
    /// already run); open layers must be restyled.
    MapStyleChanged,
}

/// The server sign-in, sign-out and client ids act on.
#[derive(Debug, PartialEq, Eq)]
struct ActingServer {
    api_base_url: String,
    /// Where tokens and client ids are keyed; differs from the API URL for the
    /// production server.
    oauth_base_url: String,
}

/// The run override (`--api-server`) wins over the saved choice; a custom
/// URL is used as saved.
fn acting_server(saved: &AppSettings, run_override: Option<&str>) -> ActingServer {
    let api_base_url = settings_store::effective_base_url(saved, run_override);
    let oauth_base_url = auth::oauth_base_for(&api_base_url);
    ActingServer {
        api_base_url,
        oauth_base_url,
    }
}

/// Signed in when `stored` has a login for the acting server's OAuth base.
/// A login saved for another server does not count.
fn login_state_for(
    server: &ActingServer,
    stored: impl FnOnce(&str) -> Option<StoredToken>,
) -> LoginState {
    match stored(&server.oauth_base_url) {
        Some(token) => LoginState::LoggedIn(token),
        None => LoginState::LoggedOut,
    }
}

/// What was typed into the custom API server field.
#[derive(Debug, PartialEq, Eq)]
enum CustomApiUrl {
    /// An `http(s)://` URL, trimmed, without trailing slashes.
    Valid(String),
    Empty,
    /// Not `http://` or `https://` (case-sensitive, as the API client is), or
    /// a scheme with no host after it.
    NotHttp,
}

fn parse_custom_api_url(raw: &str) -> CustomApiUrl {
    let url = raw.trim();
    if url.is_empty() {
        return CustomApiUrl::Empty;
    }
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return CustomApiUrl::NotHttp;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        CustomApiUrl::NotHttp
    } else {
        CustomApiUrl::Valid(url.trim_end_matches('/').to_string())
    }
}

/// The smallest tile-cache budget the field accepts.
const MIN_CACHE_BUDGET_MB: u64 = 10;

/// What was typed into the tile-cache budget field.
#[derive(Debug, PartialEq, Eq)]
enum CacheBudget {
    Megabytes(u64),
    TooSmall,
    /// Anything but a whole number, including units ("500 MB") and negatives.
    NotANumber,
}

fn parse_cache_budget(raw: &str) -> CacheBudget {
    let digits = raw.trim();
    // `u64::from_str` would also take a leading plus sign.
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return CacheBudget::NotANumber;
    }
    match digits.parse::<u64>() {
        Ok(mb) if mb >= MIN_CACHE_BUDGET_MB => CacheBudget::Megabytes(mb),
        Ok(_) => CacheBudget::TooSmall,
        Err(_) => CacheBudget::NotANumber,
    }
}

/// What saving the OAuth client-id field does for the acting server.
#[derive(Debug, PartialEq, Eq)]
enum ClientIdEdit {
    /// Blank: fall back to the built-in client id.
    Clear,
    Set(String),
}

fn parse_client_id(raw: &str) -> ClientIdEdit {
    match raw.trim() {
        "" => ClientIdEdit::Clear,
        id => ClientIdEdit::Set(id.to_string()),
    }
}

/// What was typed into the add-map-style field.
#[derive(Debug, PartialEq, Eq)]
enum MapStyleLocation {
    /// Nothing to add; the button does nothing.
    Empty,
    /// A file path or URL, trimmed; whether it loads is decided by loading it.
    Location(String),
}

fn parse_map_style_location(raw: &str) -> MapStyleLocation {
    match raw.trim() {
        "" => MapStyleLocation::Empty,
        location => MapStyleLocation::Location(location.to_string()),
    }
}

fn rejection_message(rejection: &keybindings::Rejection) -> SharedString {
    match rejection {
        keybindings::Rejection::Reserved => tr!("settings.shortcuts.error.reserved"),
        keybindings::Rejection::NeedsBareKey => tr!("settings.shortcuts.error.mode_modifier"),
        keybindings::Rejection::InUse(other) => {
            tr!("settings.shortcuts.error.in_use", other = other)
        }
    }
}

/// A page of the Settings window. [`SettingsPage::ALL`] is the one list that
/// orders the pages: `setting_pages` builds them by walking it, and
/// [`SettingsPage::index`] reads positions from it, so a page's index cannot
/// drift from where it is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    Account,
    Appearance,
    Editing,
    /// Custom imagery sources, the street-imagery tokens and the tile cache.
    Imagery,
    Shortcuts,
}

impl SettingsPage {
    pub const ALL: [SettingsPage; 5] = [
        Self::Account,
        Self::Appearance,
        Self::Editing,
        Self::Imagery,
        Self::Shortcuts,
    ];

    /// Position of this page in the window's page list.
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|p| *p == self)
            .expect("every page is in ALL")
    }

    fn title_key(self) -> &'static str {
        match self {
            Self::Account => "settings.account.title",
            Self::Appearance => "settings.appearance.title",
            Self::Editing => "settings.editing.title",
            Self::Imagery => "settings.imagery.title",
            Self::Shortcuts => "settings.shortcuts.title",
        }
    }

    fn title(self) -> SharedString {
        crate::i18n::tr_dynamic(self.title_key())
    }
}

/// Which page the window shows when `Settings` first renders.
///
/// gpui-component's `Settings` keeps its selected page in state keyed by its
/// element id and reads `default_selected_index` only when that state is
/// created. So showing another page later means a new id: each `show` bumps
/// `generation`, which is part of the id, and the fresh state starts on the
/// requested page (the search box and scroll position reset with it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PageSelection {
    start: Option<SettingsPage>,
    generation: u32,
}

impl PageSelection {
    fn new(start: Option<SettingsPage>) -> Self {
        Self {
            start,
            generation: 0,
        }
    }

    fn show(&mut self, page: SettingsPage) {
        self.start = Some(page);
        self.generation = self.generation.wrapping_add(1);
    }

    fn element_id(&self) -> (&'static str, u32) {
        ("app-settings", self.generation)
    }
}

pub struct SettingsWindow {
    /// Used for sign-in and for fetching a custom map style; the window is
    /// opened from a global action, so it takes the installed default client.
    http: Arc<dyn crate::http::HttpClient>,
    page: PageSelection,
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
    edit_simplify_tolerance: Entity<InputState>,
    simplify_tolerance_error: Option<SharedString>,
    edit_mapillary_token: Entity<InputState>,
    edit_fotoshi_key: Entity<InputState>,
    cache_clear_error: Option<SharedString>,
    map_style_input: Entity<InputState>,
    map_style_error: Option<SharedString>,
    map_style_loading: bool,

    login_state: LoginState,

    recording: Option<&'static str>,
    shortcut_error: Option<(&'static str, SharedString)>,
}

impl SettingsWindow {
    /// A window opened on page `start_page` (the first when `None`).
    pub fn new(
        start_page: Option<SettingsPage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let app_settings = settings_store::snapshot();
        let custom_api_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://example.com")
                .default_value(app_settings.custom_api_url.clone())
        });
        let oauth_base = Self::acting_server(&app_settings).oauth_base_url;
        let client_id_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("settings.account.client_id_placeholder"))
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
        let edit_simplify_tolerance = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("3")
                .default_value(app_settings.simplify_tolerance().to_string())
        });
        let edit_mapillary_token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("MLY|…")
                .default_value(
                    crate::secrets::get(crate::secrets::MAPILLARY_TOKEN).unwrap_or_default(),
                )
        });
        let edit_fotoshi_key = cx.new(|cx| {
            InputState::new(window, cx).default_value(
                crate::secrets::get(crate::secrets::FOTOSHI_API_KEY).unwrap_or_default(),
            )
        });
        let map_style_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr!("settings.map_style.placeholder"))
        });
        let login_state = Self::current_login_state(&app_settings);

        Self {
            http: crate::http::default_client(),
            page: PageSelection::new(start_page),
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
            edit_simplify_tolerance,
            simplify_tolerance_error: None,
            edit_mapillary_token,
            edit_fotoshi_key,
            cache_clear_error: None,
            map_style_input,
            map_style_error: None,
            map_style_loading: false,

            login_state,

            recording: None,
            shortcut_error: None,
        }
    }

    /// Switch the open window to `page`.
    pub fn show_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.page.show(page);
        cx.notify();
    }

    /// The server this window signs in to and keeps client IDs for: the one in
    /// use this run (`--api-server` wins over the saved choice), not the one
    /// the picker shows.
    fn acting_server(saved: &AppSettings) -> ActingServer {
        acting_server(saved, settings_store::run_override().as_deref())
    }

    /// Write the changes `f` makes to the store, then take the store's value
    /// as this window's copy. Only what `f` touches is written, so settings
    /// changed elsewhere while the window was open are never reverted.
    fn commit(&mut self, f: impl FnOnce(&mut AppSettings)) {
        settings_store::update(true, f);
        self.app_settings = settings_store::snapshot();
    }

    fn current_login_state(settings: &AppSettings) -> LoginState {
        // Only the display name is shown, so use the metadata-only lookup: it never
        // touches the keychain (this runs on the UI thread).
        login_state_for(&Self::acting_server(settings), auth::stored_login)
    }

    /// Refresh `client_id_input`'s displayed value to match the currently-selected
    /// server's configured override (or blank, if using the default client_id).
    fn refresh_client_id_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let oauth_base = Self::acting_server(&self.app_settings).oauth_base_url;
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
        self.commit(|s| s.api_server = choice);
        self.login_state = Self::current_login_state(&self.app_settings);
        self.refresh_client_id_input(window, cx);
        cx.notify();
    }

    fn save_custom_api_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.custom_api_url_input.read(cx).value().to_string();
        let url = match parse_custom_api_url(&raw) {
            CustomApiUrl::Valid(url) => url,
            CustomApiUrl::Empty | CustomApiUrl::NotHttp => {
                self.custom_url_error = Some(tr!("settings.account.error.invalid_url"));
                cx.notify();
                return;
            }
        };
        self.custom_url_error = None;
        self.commit(|s| s.custom_api_url = url);
        self.login_state = Self::current_login_state(&self.app_settings);
        self.refresh_client_id_input(window, cx);
        cx.notify();
    }

    /// Turn taginfo tag suggestions on or off. Written straight to the
    /// store, which the main window reads before every lookup.
    fn set_suggest_from_taginfo(&mut self, on: bool, cx: &mut Context<Self>) {
        self.commit(|s| s.suggest_from_taginfo = on);
        cx.notify();
    }

    /// Reverse (or restore) the scroll-zoom direction. Written straight to the
    /// store, which the main window reads on every scroll.
    fn set_invert_scroll_zoom(&mut self, invert: bool, cx: &mut Context<Self>) {
        self.commit(|s| s.invert_scroll_zoom = invert);
        cx.notify();
    }

    /// Load the style at `location` (`None` is the built-in style) off the UI
    /// thread and, if it parses, make it the active one. A style that cannot
    /// be read or parsed is reported and the current style stays in use.
    /// When `add` is set a successfully loaded style is also remembered in
    /// the list.
    fn activate_map_style(&mut self, location: Option<String>, add: bool, cx: &mut Context<Self>) {
        self.map_style_error = None;
        let Some(loc) = location else {
            self.commit(|s| s.active_map_style = None);
            self.finish_map_style(None, cx);
            return;
        };
        self.map_style_loading = true;
        cx.notify();
        let http = self.http.clone();
        cx.spawn(async move |this, cx| {
            let loc2 = loc.clone();
            let result = cx
                .background_executor()
                .spawn(async move { crate::style::custom::load(&loc2, http.as_ref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.map_style_loading = false;
                match result {
                    Ok(sheet) => {
                        this.commit(|s| {
                            if add {
                                crate::style::custom::add_location(&mut s.map_styles, &loc);
                            }
                            s.active_map_style = Some(loc);
                        });
                        this.finish_map_style(Some(Arc::new(sheet)), cx);
                    }
                    Err(e) => {
                        this.map_style_error = Some(tr!("settings.map_style.error", error = e));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Apply the already-saved style choice to the running app.
    fn finish_map_style(
        &mut self,
        sheet: Option<Arc<crate::style::Stylesheet>>,
        cx: &mut Context<Self>,
    ) {
        crate::style::custom::set_active(sheet);
        cx.emit(SettingsEvent::MapStyleChanged);
        cx.notify();
    }

    fn add_map_style(&mut self, cx: &mut Context<Self>) {
        match parse_map_style_location(&self.map_style_input.read(cx).value()) {
            MapStyleLocation::Empty => {}
            MapStyleLocation::Location(location) => {
                self.activate_map_style(Some(location), true, cx);
            }
        }
    }

    /// Forget the active custom style and go back to the built-in one.
    fn remove_active_map_style(&mut self, cx: &mut Context<Self>) {
        self.commit(|s| {
            if let Some(active) = s.active_map_style.take() {
                s.map_styles.retain(|l| *l != active);
            }
        });
        self.map_style_error = None;
        self.finish_map_style(None, cx);
    }

    fn set_theme(&mut self, theme: ThemePreference, cx: &mut Context<Self>) {
        crate::appearance::set_preference(theme, true, cx);
        self.app_settings = settings_store::snapshot();
        cx.notify();
    }

    fn set_autosave(&mut self, on: bool, cx: &mut Context<Self>) {
        self.commit(|s| s.autosave = on);
        cx.notify();
    }

    /// Turn the welcome card on or off. Written straight to the store (which
    /// the main window reads on every render) and the windows are repainted,
    /// so the card reappears at once when no data layer is open.
    fn set_show_welcome(&mut self, show: bool, cx: &mut Context<Self>) {
        self.commit(|s| s.welcome_dismissed = !show);
        cx.refresh_windows();
        cx.notify();
    }

    fn set_text_size_preset(&mut self, preset: TextSizePreset, cx: &mut Context<Self>) {
        self.commit(|s| s.text_size_preset = preset);
        cx.notify();
    }

    fn set_language(&mut self, language: Option<String>, cx: &mut Context<Self>) {
        self.commit(|s| s.language = language);
        cx.notify();
    }

    fn save_client_id(&mut self, cx: &mut Context<Self>) {
        let oauth_base = Self::acting_server(&self.app_settings).oauth_base_url;
        let edit = parse_client_id(&self.client_id_input.read(cx).value());
        self.commit(|s| match edit {
            ClientIdEdit::Clear => {
                s.client_ids.remove(&oauth_base);
            }
            ClientIdEdit::Set(client_id) => {
                s.client_ids.insert(oauth_base, client_id);
            }
        });
        cx.notify();
    }

    /// Store the Fotoshi API key typed into its field.
    fn save_fotoshi_key(&mut self, cx: &mut Context<Self>) {
        let raw = self.edit_fotoshi_key.read(cx).value().to_string();
        crate::secrets::set(crate::secrets::FOTOSHI_API_KEY, &raw);
        cx.notify();
    }

    /// Turn the per-feature Fotoshi lookup on or off.
    fn set_fotoshi_lookup(&mut self, on: bool, cx: &mut Context<Self>) {
        self.commit(|s| s.fotoshi_lookup_selected = on);
        cx.notify();
    }

    /// Store the Mapillary access token typed into its field.
    fn save_mapillary_token(&mut self, cx: &mut Context<Self>) {
        let raw = self.edit_mapillary_token.read(cx).value().to_string();
        crate::secrets::set(crate::secrets::MAPILLARY_TOKEN, &raw);
        cx.notify();
    }

    /// Validate and store the Simplify Way tolerance typed into its field.
    fn save_simplify_tolerance(&mut self, cx: &mut Context<Self>) {
        let raw = self.edit_simplify_tolerance.read(cx).value().to_string();
        match settings_store::parse_simplify_tolerance(&raw) {
            Some(metres) => {
                self.simplify_tolerance_error = None;
                settings_store::set_simplify_tolerance(metres, true);
                self.app_settings = settings_store::snapshot();
            }
            None => {
                self.simplify_tolerance_error =
                    Some(tr!("settings.editing.simplify_tolerance_error"));
            }
        }
        cx.notify();
    }

    fn save_cache_budget(&mut self, cx: &mut Context<Self>) {
        let raw = self.edit_cache_budget.read(cx).value().to_string();
        match parse_cache_budget(&raw) {
            CacheBudget::Megabytes(mb) => {
                self.cache_budget_error = None;
                self.commit(|s| s.cache_budget_mb = mb);
                crate::tile_cache::set_budget_mb(mb);
            }
            CacheBudget::TooSmall => {
                self.cache_budget_error = Some(tr!("settings.cache.error.too_small"));
            }
            CacheBudget::NotANumber => {
                self.cache_budget_error = Some(tr!("settings.cache.error.not_a_number"));
            }
        }
        cx.notify();
    }

    fn clear_cache_source(&mut self, key: String, cx: &mut Context<Self>) {
        if let Err(e) = crate::tile_cache::clear_source(&key) {
            self.cache_clear_error = Some(tr!(
                "settings.cache.error.clear_source",
                source = key,
                error = e
            ));
        } else {
            self.cache_clear_error = None;
        }
        cx.notify();
    }

    fn clear_all_tile_cache(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = crate::tile_cache::clear_all_cache() {
            self.cache_clear_error = Some(tr!("settings.cache.error.clear_all", error = e));
        } else {
            self.cache_clear_error = None;
        }
        cx.notify();
    }

    /// Remove `id`'s override, falling back to its default, and propagate
    /// the change to the live keymap and native menu.
    fn reset_shortcut(&mut self, id: &'static str, cx: &mut Context<Self>) {
        if let Err(rejection) = keybindings::validate_reset(&self.app_settings, id) {
            self.shortcut_error = Some((id, rejection_message(&rejection)));
            cx.notify();
            return;
        }
        self.commit(|s| {
            s.keybindings.remove(id);
        });
        cx.emit(SettingsEvent::KeybindingsChanged);
        cx.notify();
    }

    /// Clear every shortcut override.
    fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.commit(keybindings::reset_all);
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
        if let Err(rejection) = keybindings::validate(&self.app_settings, id, &spec) {
            self.shortcut_error = Some((id, rejection_message(&rejection)));
            cx.notify();
            return;
        }

        self.commit(|s| {
            s.keybindings.insert(id.to_string(), spec);
        });
        self.recording = None;
        self.shortcut_error = None;
        cx.emit(SettingsEvent::KeybindingsChanged);
        cx.notify();
    }

    fn start_login(&mut self, cx: &mut Context<Self>) {
        self.login_state = LoginState::LoggingIn;
        cx.notify();

        let api_base_url = Self::acting_server(&self.app_settings).api_base_url;
        let http = self.http.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { auth::login(http.as_ref(), &api_base_url) })
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
        auth::logout(&Self::acting_server(&self.app_settings).oauth_base_url);
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
                .placeholder(tr!("dialog.custom_imagery.name"))
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
                self.edit_error = Some(crate::ui::custom_imagery_dialog::error_message(&e));
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
        SettingsPage::ALL
            .into_iter()
            .map(|page| match page {
                SettingsPage::Account => self.account_page(view.clone()),
                SettingsPage::Appearance => self.appearance_page(view.clone()),
                SettingsPage::Editing => self.editing_page(view.clone()),
                SettingsPage::Imagery => self.imagery_page(view.clone()),
                SettingsPage::Shortcuts => self.shortcuts_page(view.clone()),
            })
            .collect()
    }

    /// The Language dropdown: "System default" plus every compiled-in
    /// catalog, each named in its own language. Read once at startup, so the
    /// description says a restart is needed.
    fn language_group(&self, view: Entity<Self>) -> SettingGroup {
        const SYSTEM: &str = "system";
        let mut options: Vec<(SharedString, SharedString)> =
            vec![(SYSTEM.into(), tr!("settings.language.system_default"))];
        for locale in crate::i18n::available_locales() {
            let name = crate::i18n::lookup_in(&locale, "meta.language_name")
                .map(SharedString::from)
                .unwrap_or_else(|| locale.clone().into());
            options.push((locale.into(), name));
        }
        let value_view = view.clone();
        let item = SettingItem::new(
            tr!("settings.language.title"),
            SettingField::dropdown(
                options,
                move |cx: &App| {
                    value_view
                        .read(cx)
                        .app_settings
                        .language
                        .clone()
                        .map(SharedString::from)
                        .unwrap_or_else(|| SYSTEM.into())
                },
                move |val: SharedString, cx: &mut App| {
                    let language = (val.as_ref() != SYSTEM).then(|| val.to_string());
                    view.update(cx, |this, cx| this.set_language(language, cx));
                },
            ),
        )
        .description(tr!("settings.language.description"));
        SettingGroup::new()
            .title(tr!("settings.language.group"))
            .item(item)
    }

    /// The "Map style" group: a dropdown of the built-in style and the added
    /// custom ones, then a field to add a file path or URL.
    fn map_style_group(&self, view: Entity<Self>) -> SettingGroup {
        use crate::style::custom;
        // The default style's key is empty (stored as no choice).
        let mut options: Vec<(SharedString, SharedString)> = custom::BUILT_IN_STYLES
            .iter()
            .map(|s| {
                (
                    custom::built_in_location(s.id).unwrap_or_default().into(),
                    tr!(s.label_key),
                )
            })
            .collect();
        for loc in &self.app_settings.map_styles {
            options.push((
                loc.clone().into(),
                crate::style::custom::display_name(loc).into(),
            ));
        }
        let value_view = view.clone();
        let select_view = view.clone();
        let select_item = SettingItem::new(
            tr!("settings.map_style.title"),
            SettingField::dropdown(
                options,
                move |cx: &App| {
                    value_view
                        .read(cx)
                        .app_settings
                        .active_map_style
                        .clone()
                        .unwrap_or_default()
                        .into()
                },
                move |val: SharedString, cx: &mut App| {
                    let location = (!val.is_empty()).then(|| val.to_string());
                    select_view.update(cx, |this, cx| this.activate_map_style(location, false, cx));
                },
            ),
        )
        .description(tr!("settings.map_style.description"));

        let input = self.map_style_input.clone();
        let error = self.map_style_error.clone();
        let loading = self.map_style_loading;
        let can_remove = self
            .app_settings
            .active_map_style
            .as_deref()
            .is_some_and(|l| custom::built_in_for(l).is_none());
        let add_item = SettingItem::new(
            tr!("settings.map_style.add"),
            SettingField::render(move |_options, window, cx| {
                render_map_style_add(
                    view.clone(),
                    input.clone(),
                    error.clone(),
                    loading,
                    can_remove,
                    window,
                    cx,
                )
            }),
        )
        .description(tr!("settings.map_style.add_description"))
        .layout(Axis::Vertical);
        SettingGroup::new()
            .title(tr!("settings.map_style.group"))
            .item(select_item)
            .item(add_item)
    }

    fn appearance_page(&self, view: Entity<Self>) -> SettingPage {
        let map_style_view = view.clone();
        let language_view = view.clone();
        let theme_view = view.clone();
        let value_view = view.clone();
        let welcome_view = view.clone();
        let invert_view = view.clone();
        let set_value_view = view;
        let invert_item = SettingItem::new(
            tr!("settings.appearance.invert_scroll_zoom"),
            SettingField::switch(
                |_cx: &App| settings_store::snapshot().invert_scroll_zoom,
                move |invert: bool, cx: &mut App| {
                    invert_view.update(cx, |this, cx| this.set_invert_scroll_zoom(invert, cx));
                },
            ),
        )
        .description(tr!("settings.appearance.invert_scroll_zoom_description"));
        // The store, not this window's copy: the card's close button changes it.
        let welcome_item = SettingItem::new(
            tr!("settings.appearance.welcome_card"),
            SettingField::switch(
                |_cx: &App| !settings_store::snapshot().welcome_dismissed,
                move |show: bool, cx: &mut App| {
                    welcome_view.update(cx, |this, cx| this.set_show_welcome(show, cx));
                },
            ),
        )
        .description(tr!("settings.appearance.welcome_card_description"));
        let theme_item = SettingItem::new(
            tr!("settings.appearance.theme"),
            SettingField::dropdown(
                ThemePreference::ALL
                    .into_iter()
                    .map(|p| (p.as_key().into(), p.label()))
                    .collect(),
                // The store, not this window's copy: the View menu can change it too.
                |_cx: &App| SharedString::from(settings_store::snapshot().theme.as_key()),
                move |val: SharedString, cx: &mut App| {
                    let Some(theme) = ThemePreference::from_key(&val) else {
                        return;
                    };
                    theme_view.update(cx, |this, cx| this.set_theme(theme, cx));
                },
            ),
        )
        .description(tr!("settings.appearance.theme_description"));
        let item = SettingItem::new(
            tr!("settings.appearance.text_size"),
            SettingField::dropdown(
                TextSizePreset::ALL
                    .into_iter()
                    .map(|p| (p.as_key().into(), p.label()))
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
        .description(tr!("settings.appearance.text_size_description"));

        SettingPage::new(SettingsPage::Appearance.title())
            .icon(Icon::new(IconName::Palette))
            .groups(vec![
                SettingGroup::new()
                    .title(tr!("settings.appearance.theme_group"))
                    .item(theme_item),
                SettingGroup::new()
                    .title(tr!("settings.appearance.text_group"))
                    .item(item),
                self.language_group(language_view),
                self.map_style_group(map_style_view),
                SettingGroup::new()
                    .title(tr!("settings.appearance.scroll_group"))
                    .item(invert_item),
                SettingGroup::new()
                    .title(tr!("settings.appearance.welcome_group"))
                    .item(welcome_item),
            ])
    }

    /// The Editing page: the autosave switch, the tag-suggestion switch and
    /// the Simplify Way tolerance.
    fn editing_page(&self, view: Entity<Self>) -> SettingPage {
        let taginfo_view = view.clone();
        let taginfo_item = SettingItem::new(
            tr!("settings.editing.taginfo"),
            SettingField::switch(
                |_cx: &App| settings_store::snapshot().suggest_from_taginfo,
                move |on: bool, cx: &mut App| {
                    taginfo_view.update(cx, |this, cx| this.set_suggest_from_taginfo(on, cx));
                },
            ),
        )
        .description(tr!("settings.editing.taginfo_description"));
        let simplify_view = view.clone();
        let simplify_input = self.edit_simplify_tolerance.clone();
        let simplify_error = self.simplify_tolerance_error.clone();
        let simplify_item = SettingItem::new(
            tr!("settings.editing.simplify_tolerance"),
            SettingField::render(move |_options, window, cx| {
                render_simplify_tolerance(
                    simplify_view.clone(),
                    simplify_input.clone(),
                    simplify_error.clone(),
                    window,
                    cx,
                )
            }),
        )
        .description(tr!("settings.editing.simplify_tolerance_description"))
        .layout(Axis::Vertical);
        let value_view = view.clone();
        let item = SettingItem::new(
            tr!("settings.editing.autosave"),
            SettingField::switch(
                move |cx: &App| value_view.read(cx).app_settings.autosave,
                move |on: bool, cx: &mut App| {
                    view.update(cx, |this, cx| this.set_autosave(on, cx));
                },
            ),
        )
        .description(tr!("settings.editing.autosave_description"));
        SettingPage::new(SettingsPage::Editing.title())
            .icon(Icon::new(IconName::Settings2))
            .groups(vec![
                SettingGroup::new()
                    .title(tr!("settings.editing.autosave_group"))
                    .item(item),
                SettingGroup::new()
                    .title(tr!("settings.editing.taginfo_group"))
                    .item(taginfo_item),
                SettingGroup::new()
                    .title(tr!("settings.editing.simplify_group"))
                    .item(simplify_item),
            ])
    }

    fn account_page(&self, view: Entity<Self>) -> SettingPage {
        let api_choice = self.app_settings.api_server;

        let server_view = view.clone();
        let mut api_items = vec![SettingItem::new(
            tr!("settings.account.server"),
            SettingField::render(move |_options, window, cx| {
                render_server_picker(api_choice, server_view.clone(), window, cx)
            }),
        )
        .description(tr!("settings.account.server_description"))
        .layout(Axis::Vertical)];

        if matches!(api_choice, ApiServerChoice::Custom) {
            let custom_view = view.clone();
            let input = self.custom_api_url_input.clone();
            let error = self.custom_url_error.clone();
            api_items.push(
                SettingItem::new(
                    tr!("settings.account.custom_url"),
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
                .description(tr!("settings.account.custom_url_description"))
                .layout(Axis::Vertical),
            );
        }

        let client_id_view = view.clone();
        let client_id_input = self.client_id_input.clone();
        api_items.push(
            SettingItem::new(
                tr!("settings.account.client_id"),
                SettingField::render(move |_options, window, cx| {
                    render_client_id(client_id_view.clone(), client_id_input.clone(), window, cx)
                }),
            )
            .description(tr!("settings.account.client_id_description"))
            .layout(Axis::Vertical),
        );

        let login_view = view;
        let login_state = self.login_state.clone();
        let login_item = SettingItem::new(
            tr!("settings.account.account"),
            SettingField::render(move |_options, window, cx| {
                render_login_state(login_view.clone(), login_state.clone(), window, cx)
            }),
        )
        .description(tr!("settings.account.account_description"))
        .layout(Axis::Vertical);

        SettingPage::new(SettingsPage::Account.title())
            .icon(Icon::new(IconName::User))
            .default_open(true)
            .groups(vec![
                SettingGroup::new()
                    .title(tr!("settings.account.api_server_group"))
                    .items(api_items),
                SettingGroup::new()
                    .title(tr!("settings.account.osm_account_group"))
                    .item(login_item),
            ])
    }

    fn imagery_page(&self, view: Entity<Self>) -> SettingPage {
        let mut items = Vec::new();

        if self.entries.is_empty() {
            items.push(SettingItem::render(|_options, _window, _cx| {
                Label::new(tr!("settings.imagery.empty"))
            }));
        }

        for (idx, entry) in self.entries.iter().enumerate() {
            let title = if entry.name.is_empty() {
                tr!("settings.imagery.new_source")
            } else {
                entry.name.clone().into()
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
                let entry_summary = tr!(
                    "settings.imagery.entry_summary",
                    url = entry.url_template,
                    min = entry.min_zoom,
                    max = entry.max_zoom
                );
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
                .label(tr!("settings.imagery.add_source"))
                .ghost()
                .on_click({
                    let add_view = add_view.clone();
                    move |_ev, window, cx| {
                        add_view.update(cx, |this, cx| this.add_new_entry(window, cx));
                    }
                })
        }));

        SettingPage::new(SettingsPage::Imagery.title())
            .icon(Icon::new(IconName::Map))
            .groups(vec![
                SettingGroup::new()
                    .title(tr!("settings.imagery.custom_sources"))
                    .items(items),
                self.street_imagery_group(view.clone()),
                self.cache_group(view),
            ])
    }

    /// The street-imagery group: the Mapillary access token.
    fn street_imagery_group(&self, view: Entity<Self>) -> SettingGroup {
        let token_view = view.clone();
        let token_input = self.edit_mapillary_token.clone();
        let token_item = SettingItem::new(
            tr!("settings.street_imagery.mapillary_token"),
            SettingField::render(move |_options, _window, cx| {
                let muted = cx.theme().muted_foreground;
                let view = token_view.clone();
                v_flex()
                    .gap_2()
                    .child(field_row(
                        tr!("settings.street_imagery.mapillary_token"),
                        &token_input,
                        muted,
                    ))
                    .child(
                        Button::new("save-mapillary-token")
                            .label(tr!("settings.save"))
                            .primary()
                            .compact()
                            .on_click(move |_ev, _window, cx| {
                                view.update(cx, |this, cx| this.save_mapillary_token(cx));
                            }),
                    )
            }),
        )
        .description(tr!("settings.street_imagery.mapillary_token_description"))
        .layout(Axis::Vertical);
        let key_view = view.clone();
        let key_input = self.edit_fotoshi_key.clone();
        let key_item = SettingItem::new(
            tr!("settings.street_imagery.fotoshi_key"),
            SettingField::render(move |_options, _window, cx| {
                let muted = cx.theme().muted_foreground;
                let view = key_view.clone();
                v_flex()
                    .gap_2()
                    .child(field_row(
                        tr!("settings.street_imagery.fotoshi_key"),
                        &key_input,
                        muted,
                    ))
                    .child(
                        Button::new("save-fotoshi-key")
                            .label(tr!("settings.save"))
                            .primary()
                            .compact()
                            .on_click(move |_ev, _window, cx| {
                                view.update(cx, |this, cx| this.save_fotoshi_key(cx));
                            }),
                    )
            }),
        )
        .description(tr!("settings.street_imagery.fotoshi_key_description"))
        .layout(Axis::Vertical);
        let lookup_view = view;
        let lookup_item = SettingItem::new(
            tr!("settings.street_imagery.fotoshi_lookup"),
            SettingField::switch(
                |_cx: &App| settings_store::snapshot().fotoshi_lookup_selected,
                move |on: bool, cx: &mut App| {
                    lookup_view.update(cx, |this, cx| this.set_fotoshi_lookup(on, cx));
                },
            ),
        )
        .description(tr!("settings.street_imagery.fotoshi_lookup_description"));
        SettingGroup::new()
            .title(tr!("settings.street_imagery.group"))
            .item(token_item)
            .item(key_item)
            .item(lookup_item)
    }

    fn cache_group(&self, view: Entity<Self>) -> SettingGroup {
        let summary = crate::tile_cache::cache_summary();

        // The synthetic "(uncategorized)" bucket holds loose files that belong
        // to no imagery source, so it is not counted as one.
        let real_source_count = summary
            .sources
            .iter()
            .filter(|s| s.key != crate::tile_cache::uncategorized_key())
            .count();

        let summary_text = tr!(
            "settings.cache.summary",
            size = crate::tile_cache::format_bytes(summary.total_bytes),
            tiles = tr_count!("settings.cache.tiles", summary.total_files),
            sources = tr_count!("settings.cache.sources", real_source_count)
        );

        let clear_error = self.cache_clear_error.clone();
        let mut items = vec![SettingItem::render(move |_options, _window, cx| {
            render_cache_usage_summary(summary_text.clone(), clear_error.clone(), cx)
        })];

        let budget_view = view.clone();
        let budget_input = self.edit_cache_budget.clone();
        let budget_error = self.cache_budget_error.clone();
        items.push(
            SettingItem::new(
                tr!("settings.cache.budget"),
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
            .description(tr!("settings.cache.budget_description"))
            .layout(Axis::Vertical),
        );

        if summary.sources.is_empty() {
            items.push(SettingItem::render(|_options, _window, _cx| {
                Label::new(tr!("settings.cache.empty"))
            }));
        }

        for source in summary.sources {
            let row_view = view.clone();
            let key = source.key.clone();
            let size_label = tr!(
                "settings.cache.source_size",
                size = crate::tile_cache::format_bytes(source.bytes),
                tiles = tr_count!("settings.cache.tiles", source.file_count)
            );
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
                .label(tr!("settings.cache.clear_all"))
                .ghost()
                .compact()
                .on_click(move |_ev, _window, cx| {
                    clear_all_view.update(cx, |this, cx| this.clear_all_tile_cache(cx));
                })
        }));

        SettingGroup::new()
            .title(tr!("settings.cache.group"))
            .items(items)
    }

    fn shortcuts_page(&self, view: Entity<Self>) -> SettingPage {
        let mut groups = Vec::new();
        for category in ShortcutCategory::ALL {
            let items: Vec<SettingItem> = SHORTCUTS
                .iter()
                .filter(|d| d.category == category)
                .map(|d| {
                    let id = d.id;
                    let label = d.label();
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
            groups.push(SettingGroup::new().title(category.title()).items(items));
        }

        let reset_all_view = view;
        groups.push(SettingGroup::new().item(SettingItem::render(
            move |_options, _window, _cx| {
                Button::new("reset-all-shortcuts")
                    .label(tr!("settings.shortcuts.reset_all"))
                    .ghost()
                    .on_click({
                        let reset_all_view = reset_all_view.clone();
                        move |_ev, _window, cx| {
                            reset_all_view.update(cx, |this, cx| this.reset_all_shortcuts(cx));
                        }
                    })
            },
        )));

        SettingPage::new(SettingsPage::Shortcuts.title()).groups(groups)
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
        .child(Radio::from(tr!("settings.account.server_primary")).small())
        .child(Radio::from(tr!("settings.account.server_dev")).small())
        .child(Radio::from(tr!("settings.account.server_custom")).small())
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

    let mut row = v_flex()
        .gap_2()
        .child(field_row(tr!("settings.account.url"), &input, muted));
    if let Some(err) = error {
        row = row.child(Label::new(err).text_color(danger));
    }
    row.child(
        Button::new("save-custom-api-url")
            .label(tr!("settings.save"))
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
        .child(field_row(
            tr!("settings.account.client_id_field"),
            &input,
            muted,
        ))
        .child(
            Button::new("save-client-id")
                .label(tr!("settings.save"))
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

fn render_map_style_add(
    view: Entity<SettingsWindow>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    loading: bool,
    can_remove: bool,
    _window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    let mut col =
        v_flex()
            .gap_2()
            .child(field_row(tr!("settings.map_style.location"), &input, muted));
    if let Some(err) = error {
        col = col.child(Label::new(err).text_sm().text_color(danger));
    }
    if loading {
        col = col.child(
            Label::new(tr!("settings.map_style.loading"))
                .text_sm()
                .text_color(muted),
        );
    }
    let add_view = view.clone();
    let mut buttons = h_flex().gap_2().child(
        Button::new("add-map-style")
            .label(tr!("settings.map_style.add_button"))
            .primary()
            .compact()
            .disabled(loading)
            .on_click(move |_ev, _window, cx| {
                add_view.update(cx, |this, cx| this.add_map_style(cx));
            }),
    );
    if can_remove {
        buttons = buttons.child(
            Button::new("remove-map-style")
                .label(tr!("settings.map_style.remove_button"))
                .compact()
                .on_click(move |_ev, _window, cx| {
                    view.update(cx, |this, cx| this.remove_active_map_style(cx));
                }),
        );
    }
    col.child(buttons)
}

fn render_simplify_tolerance(
    view: Entity<SettingsWindow>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    _window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    let mut row = v_flex().gap_2().child(field_row(
        tr!("settings.editing.simplify_tolerance"),
        &input,
        muted,
    ));
    if let Some(err) = error {
        row = row.child(Label::new(err).text_sm().text_color(danger));
    }
    row.child(
        Button::new("save-simplify-tolerance")
            .label(tr!("settings.save"))
            .primary()
            .compact()
            .on_click(move |_ev, _window, cx| {
                view.update(cx, |this, cx| this.save_simplify_tolerance(cx));
            }),
    )
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
        .child(field_row(tr!("settings.cache.megabytes"), &input, muted));
    if let Some(err) = error {
        row = row.child(Label::new(err).text_sm().text_color(danger));
    }
    row.child(
        Button::new("save-cache-budget")
            .label(tr!("settings.save"))
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
        .label(tr!("settings.cache.clear_source"))
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
            .label(tr!("settings.account.sign_in"))
            .primary()
            .on_click(move |_ev, _window, cx| {
                view.update(cx, |this, cx| this.start_login(cx));
            })
            .into_any_element(),
        LoginState::LoggingIn => h_flex()
            .gap_2()
            .items_center()
            .child(Label::new(tr!("settings.account.signing_in")).text_color(muted))
            .into_any_element(),
        LoginState::LoggedIn(token) => h_flex()
            .gap_2()
            .items_center()
            .child(
                Label::new(tr!("settings.account.logged_in", name = token.display_name))
                    .text_color(foreground),
            )
            .child(
                Button::new("logout")
                    .label(tr!("settings.account.sign_out"))
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
                    .label(tr!("settings.account.try_again"))
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
                Label::new(tr!("settings.imagery.confirm_delete", name = entry_name))
                    .text_color(danger),
            )
            .child({
                let view = view.clone();
                Button::new(("confirm-delete", idx))
                    .label(tr!("settings.imagery.delete"))
                    .danger()
                    .compact()
                    .on_click(move |_ev, _window, cx| {
                        view.update(cx, |this, cx| this.delete_entry(idx, cx));
                    })
            })
            .child(
                Button::new(("cancel-delete", idx))
                    .label(tr!("dialog.cancel"))
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
                    .label(tr!("settings.imagery.edit"))
                    .ghost()
                    .compact()
                    .on_click(move |_ev, window, cx| {
                        view.update(cx, |this, cx| this.start_edit_at(idx, window, cx));
                    })
            })
            .child(
                Button::new(("trash", idx))
                    .label(tr!("settings.imagery.delete"))
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

        let mut row =
            v_flex().gap_1().child(
                div()
                    .track_focus(&focus_handle)
                    .on_key_down({
                        let capture_view = capture_view.clone();
                        move |ev: &gpui::KeyDownEvent, _window, cx| {
                            match keybindings::classify_recorded(&ev.keystroke) {
                                keybindings::Recorded::ModifierOnly => {}
                                keybindings::Recorded::Cancel => {
                                    capture_view.update(cx, |this, cx| this.cancel_recording(cx));
                                }
                                keybindings::Recorded::Spec(spec) => {
                                    capture_view
                                        .update(cx, |this, cx| this.apply_shortcut(id, spec, cx));
                                }
                            }
                        }
                    })
                    .child(
                        Label::new(tr!("settings.shortcuts.recording"))
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
                .label(tr!("settings.shortcuts.record"))
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
                    .label(tr!("settings.shortcuts.reset"))
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
        .child(field_row(
            tr!("dialog.custom_imagery.name"),
            &edit_name,
            muted,
        ))
        .child(field_row(
            tr!("dialog.custom_imagery.url_template"),
            &edit_url,
            muted,
        ))
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(field_row(
                    tr!("dialog.custom_imagery.min_zoom"),
                    &edit_min_zoom,
                    muted,
                )))
                .child(div().flex_1().child(field_row(
                    tr!("dialog.custom_imagery.max_zoom"),
                    &edit_max_zoom,
                    muted,
                ))),
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
                .child(
                    Button::new(("save", idx))
                        .label(tr!("settings.save"))
                        .primary()
                        .on_click(move |_ev, _window, cx| {
                            save_view.update(cx, |this, cx| this.save_entry(idx, cx));
                        }),
                )
                .child(
                    Button::new(("cancel", idx))
                        .label(tr!("dialog.cancel"))
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
            .child({
                let mut settings =
                    Settings::new(self.page.element_id()).pages(self.setting_pages(cx));
                if let Some(page) = self.page.start {
                    settings = settings.default_selected_index(SelectIndex {
                        page_ix: page.index(),
                        group_ix: None,
                    });
                }
                settings
            })
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it carries `gpui::*`, whose `test` macro shadows `#[test]`.
    use super::{
        acting_server, keybindings, login_state_for, parse_cache_budget, parse_client_id,
        parse_custom_api_url, parse_map_style_location, rejection_message, CacheBudget,
        ClientIdEdit, CustomApiUrl, LoginState, MapStyleLocation, PageSelection, SettingsPage,
        MIN_CACHE_BUDGET_MB,
    };
    use crate::auth::StoredToken;
    use crate::settings_store::{ApiServerChoice, AppSettings, DEV_API_URL, PRIMARY_API_URL};

    fn saved(choice: ApiServerChoice, custom: &str) -> AppSettings {
        AppSettings {
            api_server: choice,
            custom_api_url: custom.to_string(),
            ..Default::default()
        }
    }

    fn token(name: &str) -> StoredToken {
        StoredToken {
            access_token: String::new(),
            display_name: name.to_string(),
            user_id: 1,
            refresh_token: None,
            expires_at: None,
        }
    }

    /// A token store holding one login, keyed by OAuth base URL.
    fn store_with(oauth_base: &'static str) -> impl Fn(&str) -> Option<StoredToken> {
        move |base| (base == oauth_base).then(|| token("mapper"))
    }

    #[test]
    fn saved_choice_is_the_acting_server_without_an_override() {
        let primary = acting_server(&saved(ApiServerChoice::Primary, ""), None);
        assert_eq!(primary.api_base_url, PRIMARY_API_URL);
        // The production API signs in at the website, not the API host.
        assert_eq!(primary.oauth_base_url, "https://www.openstreetmap.org");

        let dev = acting_server(&saved(ApiServerChoice::Dev, ""), None);
        assert_eq!(dev.api_base_url, DEV_API_URL);
        assert_eq!(dev.oauth_base_url, DEV_API_URL);
    }

    #[test]
    fn custom_url_is_the_acting_server_as_saved() {
        let custom = acting_server(
            &saved(ApiServerChoice::Custom, "https://osm.example.org/"),
            None,
        );
        assert_eq!(custom.api_base_url, "https://osm.example.org");
        assert_eq!(custom.oauth_base_url, "https://osm.example.org");
        // The saved custom URL is ignored while another choice is selected.
        let primary = acting_server(
            &saved(ApiServerChoice::Primary, "https://osm.example.org"),
            None,
        );
        assert_eq!(primary.api_base_url, PRIMARY_API_URL);
    }

    #[test]
    fn run_override_wins_over_the_saved_choice() {
        let settings = saved(ApiServerChoice::Primary, "");
        let over = acting_server(&settings, Some("http://localhost:3000/"));
        assert_eq!(over.api_base_url, "http://localhost:3000");
        assert_eq!(over.oauth_base_url, "http://localhost:3000");

        // An override naming the production API still maps to the website.
        let over_primary = acting_server(
            &saved(ApiServerChoice::Dev, ""),
            Some("https://api.openstreetmap.org"),
        );
        assert_eq!(over_primary.oauth_base_url, "https://www.openstreetmap.org");
    }

    #[test]
    fn login_state_follows_the_acting_server_only() {
        let settings = saved(ApiServerChoice::Primary, "");
        let www = "https://www.openstreetmap.org";

        // Signed in on the saved server, no override: signed in.
        let server = acting_server(&settings, None);
        assert_eq!(
            login_state_for(&server, store_with(www)),
            LoginState::LoggedIn(token("mapper"))
        );

        // Same login, but the run override points elsewhere: signed out.
        let server = acting_server(&settings, Some("https://master.apis.dev.openstreetmap.org"));
        assert_eq!(
            login_state_for(&server, store_with(www)),
            LoginState::LoggedOut
        );

        // Signed in on the override server only: signed in.
        assert_eq!(
            login_state_for(&server, store_with(DEV_API_URL)),
            LoginState::LoggedIn(token("mapper"))
        );
    }

    #[test]
    fn login_state_for_a_custom_server_uses_its_url() {
        let settings = saved(ApiServerChoice::Custom, "https://osm.example.org/");
        let server = acting_server(&settings, None);
        assert_eq!(
            login_state_for(&server, store_with("https://osm.example.org")),
            LoginState::LoggedIn(token("mapper"))
        );
        assert_eq!(
            login_state_for(&server, store_with(DEV_API_URL)),
            LoginState::LoggedOut
        );
    }

    #[test]
    fn the_token_lookup_is_given_the_oauth_base_not_the_api_url() {
        let server = acting_server(&saved(ApiServerChoice::Primary, ""), None);
        let mut asked = None;
        let _ = login_state_for(&server, |base| {
            asked = Some(base.to_string());
            None
        });
        assert_eq!(asked.as_deref(), Some("https://www.openstreetmap.org"));
    }

    #[test]
    fn page_indices_are_positions_in_the_page_list() {
        for (position, page) in SettingsPage::ALL.into_iter().enumerate() {
            assert_eq!(page.index(), position, "{page:?}");
        }
    }

    #[test]
    fn every_page_has_its_own_title() {
        let titles: Vec<_> = SettingsPage::ALL.into_iter().map(|p| p.title()).collect();
        for (page, title) in SettingsPage::ALL.into_iter().zip(&titles) {
            assert!(!title.starts_with("en."), "{page:?}: {title}");
        }
        for (i, a) in titles.iter().enumerate() {
            assert!(!titles[i + 1..].contains(a), "duplicate title {a}");
        }
    }

    #[test]
    fn showing_a_page_selects_it_under_a_new_element_id() {
        let mut selection = PageSelection::new(None);
        assert_eq!(selection.start, None);
        let first_id = selection.element_id();

        selection.show(SettingsPage::Imagery);
        assert_eq!(selection.start, Some(SettingsPage::Imagery));
        assert_ne!(selection.element_id(), first_id);

        // Asking for the page already shown still rebuilds the state, so a
        // page the user has since navigated away from is selected again.
        let second_id = selection.element_id();
        selection.show(SettingsPage::Imagery);
        assert_eq!(selection.start, Some(SettingsPage::Imagery));
        assert_ne!(selection.element_id(), second_id);
    }

    #[test]
    fn a_window_opened_on_a_page_starts_there_with_the_original_id() {
        let selection = PageSelection::new(Some(SettingsPage::Shortcuts));
        assert_eq!(selection.start, Some(SettingsPage::Shortcuts));
        assert_eq!(
            selection.element_id(),
            PageSelection::new(None).element_id()
        );
    }

    #[test]
    fn the_imagery_page_is_named_by_title() {
        let page = SettingsPage::Imagery;
        assert_eq!(page.title(), crate::tr!("settings.imagery.title"));
        assert_eq!(SettingsPage::ALL[page.index()], page);
    }

    #[test]
    fn custom_api_url_accepts_http_and_https_and_trims() {
        assert_eq!(
            parse_custom_api_url("https://osm.example.org"),
            CustomApiUrl::Valid("https://osm.example.org".into())
        );
        assert_eq!(
            parse_custom_api_url("http://localhost:3000/api"),
            CustomApiUrl::Valid("http://localhost:3000/api".into())
        );
        assert_eq!(
            parse_custom_api_url("  https://osm.example.org \n"),
            CustomApiUrl::Valid("https://osm.example.org".into())
        );
    }

    #[test]
    fn custom_api_url_drops_every_trailing_slash() {
        assert_eq!(
            parse_custom_api_url("https://osm.example.org/"),
            CustomApiUrl::Valid("https://osm.example.org".into())
        );
        assert_eq!(
            parse_custom_api_url("https://osm.example.org///"),
            CustomApiUrl::Valid("https://osm.example.org".into())
        );
    }

    #[test]
    fn custom_api_url_rejects_blank_input() {
        assert_eq!(parse_custom_api_url(""), CustomApiUrl::Empty);
        assert_eq!(parse_custom_api_url("   \t"), CustomApiUrl::Empty);
    }

    #[test]
    fn custom_api_url_rejects_other_schemes_and_bare_hosts() {
        for raw in [
            "osm.example.org",
            "ftp://osm.example.org",
            "file:///tmp/x",
            "//osm.example.org",
            "HTTPS://osm.example.org",
            "xhttps://osm.example.org",
        ] {
            assert_eq!(parse_custom_api_url(raw), CustomApiUrl::NotHttp, "{raw}");
        }
    }

    #[test]
    fn custom_api_url_rejects_a_scheme_with_no_host() {
        for raw in [
            "https://",
            "http://",
            "https:///",
            "  http://// ",
            "https:///api",
            "https://?x=1",
            "https://#frag",
        ] {
            assert_eq!(parse_custom_api_url(raw), CustomApiUrl::NotHttp, "{raw}");
        }
    }

    #[test]
    fn custom_api_url_keeps_ports_paths_and_queries() {
        for raw in [
            "http://h",
            "https://h:8080",
            "https://h/a/b",
            "https://h?x=1",
        ] {
            assert!(
                matches!(parse_custom_api_url(raw), CustomApiUrl::Valid(_)),
                "{raw}"
            );
        }
    }

    #[test]
    fn cache_budget_accepts_whole_megabytes_from_the_minimum() {
        assert_eq!(MIN_CACHE_BUDGET_MB, 10);
        assert_eq!(parse_cache_budget("10"), CacheBudget::Megabytes(10));
        assert_eq!(parse_cache_budget("500"), CacheBudget::Megabytes(500));
        assert_eq!(
            parse_cache_budget("18446744073709551615"),
            CacheBudget::Megabytes(u64::MAX)
        );
        assert_eq!(parse_cache_budget("  250\n"), CacheBudget::Megabytes(250));
    }

    #[test]
    fn cache_budget_below_the_minimum_is_too_small() {
        assert_eq!(parse_cache_budget("9"), CacheBudget::TooSmall);
        assert_eq!(parse_cache_budget("0"), CacheBudget::TooSmall);
        assert_eq!(parse_cache_budget("007"), CacheBudget::TooSmall);
    }

    #[test]
    fn cache_budget_rejects_anything_but_a_whole_number() {
        for raw in [
            "",
            "   ",
            "abc",
            "-5",
            "1.5",
            "500 MB",
            "500MB",
            "5 00",
            "+50",
            "+",
            "٥٠",
            "18446744073709551616",
        ] {
            assert_eq!(parse_cache_budget(raw), CacheBudget::NotANumber, "{raw:?}");
        }
    }

    #[test]
    fn client_id_blank_clears_and_anything_else_is_trimmed() {
        assert_eq!(parse_client_id(""), ClientIdEdit::Clear);
        assert_eq!(parse_client_id("  \t"), ClientIdEdit::Clear);
        assert_eq!(
            parse_client_id("  abc-123 "),
            ClientIdEdit::Set("abc-123".into())
        );
    }

    #[test]
    fn map_style_location_ignores_blank_and_trims_the_rest() {
        assert_eq!(parse_map_style_location(" "), MapStyleLocation::Empty);
        assert_eq!(
            parse_map_style_location("  /tmp/style.mapcss "),
            MapStyleLocation::Location("/tmp/style.mapcss".into())
        );
        assert_eq!(
            parse_map_style_location("https://example.org/s.mapcss"),
            MapStyleLocation::Location("https://example.org/s.mapcss".into())
        );
    }

    #[test]
    fn every_rejection_has_a_message() {
        for rejection in [
            keybindings::Rejection::Reserved,
            keybindings::Rejection::NeedsBareKey,
            keybindings::Rejection::InUse("Undo".into()),
        ] {
            let message = rejection_message(&rejection);
            assert!(!message.starts_with("en."), "{rejection:?}: {message}");
        }
        assert!(rejection_message(&keybindings::Rejection::InUse("Undo".into())).contains("Undo"));
    }
}
