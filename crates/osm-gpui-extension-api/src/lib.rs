//! Write osm-gpui extensions in Rust.
//!
//! An extension is a `cdylib` crate built for `wasm32-wasip2`, next to an
//! `extension.toml` manifest. Implement [`Extension`] on a type and name it in
//! [`register_extension!`]:
//!
//! ```ignore
//! use osm_gpui_extension_api::{self as osm, Extension};
//!
//! struct Hello;
//!
//! impl Extension for Hello {
//!     fn new() -> Self { Hello }
//!     fn run_command(&mut self, id: &str) -> osm::Result<()> {
//!         osm::set_status(&format!("ran {id}"));
//!         Ok(())
//!     }
//! }
//!
//! osm::register_extension!(Hello);
//! ```
//!
//! Everything the app offers is a free function here (`http_fetch`,
//! `viewport`, `fly_to`, `set_markers`, `show_panel`, ...). Requests that
//! change the app (markers, panel, map moves) are applied when the current
//! call returns.

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "extension",
    });
}

use std::cell::RefCell;

pub use bindings::osm_gpui::extension::types::{
    Bounds, Button, ButtonStyle, Element, FeatureKind, FeatureRef, HttpMethod, HttpRequest,
    HttpResponse, Image, LatLon, Marker, Panel, TextInput,
};

use bindings::osm_gpui::extension::host;

pub type Result<T, E = String> = std::result::Result<T, E>;

/// What the app calls. Every method but `new` has a default that does
/// nothing, so an extension implements only what it uses.
pub trait Extension: 'static {
    /// Made once, when the app loads the extension.
    fn new() -> Self
    where
        Self: Sized;

    /// A `[[commands]]` entry of the manifest was chosen from the
    /// app's menus.
    fn run_command(&mut self, id: &str) -> Result<()> {
        Err(format!("unknown command {id}"))
    }

    /// One of the markers set with [`set_markers`] was clicked.
    fn marker_clicked(&mut self, _id: &str) -> Result<()> {
        Ok(())
    }

    /// A button in panel `panel` (shown with [`show_panel`]) was clicked.
    fn panel_button(&mut self, _panel: &str, _button: &str, _inputs: &Inputs) -> Result<()> {
        Ok(())
    }

    /// The user closed panel `panel`.
    fn panel_closed(&mut self, _panel: &str) {}

    /// The map moved and settled. Only called when the manifest has
    /// `subscribe = ["viewport"]`.
    fn viewport_changed(&mut self) -> Result<()> {
        Ok(())
    }

    /// The selection changed. Only called when the manifest has
    /// `subscribe = ["selection"]`.
    fn selection_changed(&mut self) -> Result<()> {
        Ok(())
    }

    /// The extension's configuration for its section of Settings >
    /// Extensions. Buttons in it call `panel_button` with panel
    /// [`SETTINGS_PANEL`].
    fn settings(&mut self) -> Option<Panel> {
        None
    }

    /// Tags for the next changeset: `comment` and `hashtags` fill the upload
    /// dialog, anything else is added as a changeset tag.
    fn changeset_tags(&mut self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// An upload succeeded, in these changesets.
    fn upload_finished(&mut self, _changesets: &[i64]) {}
}

/// The text of the panel's inputs when a button was clicked, by input id.
pub struct Inputs(Vec<(String, String)>);

impl Inputs {
    pub fn get(&self, id: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, v)| v.as_str())
    }
}

// --- Host calls ---

/// Send an HTTP request (to a host the manifest allows).
pub fn http_fetch(request: &HttpRequest) -> Result<HttpResponse> {
    host::http_fetch(request)
}

/// Send several requests in parallel; each answer is at its request's
/// position.
pub fn http_fetch_many(requests: &[HttpRequest]) -> Vec<Result<HttpResponse>> {
    host::http_fetch_many(requests)
}

/// A GET request for `url`.
pub fn get(url: &str) -> HttpRequest {
    HttpRequest {
        method: HttpMethod::Get,
        url: url.to_string(),
        headers: Vec::new(),
        body: None,
    }
}

/// GET `url` and parse the 2xx answer as JSON.
pub fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let resp = http_fetch(&HttpRequest {
        method: HttpMethod::Get,
        url: url.to_string(),
        headers: vec![("Accept".into(), "application/json".into())],
        body: None,
    })?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("HTTP {} from {url}", resp.status));
    }
    serde_json::from_slice(&resp.body).map_err(|e| format!("bad JSON from {url}: {e}"))
}

pub fn viewport() -> Bounds {
    host::viewport()
}

pub fn zoom() -> f64 {
    host::zoom()
}

pub fn selection() -> Vec<FeatureRef> {
    host::selection()
}

pub fn fly_to(at: LatLon, zoom: f64) {
    host::fly_to(at, zoom)
}

pub fn download_area(area: Bounds) {
    host::download_area(area)
}

pub fn set_status(message: &str) {
    host::set_status(message)
}

pub fn open_url(url: &str) {
    host::open_url(url)
}

pub fn set_markers(markers: &[Marker]) {
    host::set_markers(markers)
}

/// Show (or replace) panel `id` in the side panel; `None` closes it.
pub fn show_panel(id: &str, panel: Option<&Panel>) {
    host::show_panel(id, panel)
}

pub fn storage_get(key: &str) -> Option<String> {
    host::storage_get(key)
}

pub fn storage_set(key: &str, value: Option<&str>) {
    host::storage_set(key, value)
}

/// A secret from the system keychain, private to this extension.
pub fn secret_get(key: &str) -> Option<String> {
    host::secret_get(key)
}

/// Store a secret in the system keychain; `None` deletes it.
pub fn secret_set(key: &str, value: Option<&str>) {
    host::secret_set(key, value)
}

/// The app's UI language, such as `de` or `fr-CA`.
pub fn locale() -> String {
    host::locale()
}

/// `key` from this extension's `locales/<tag>.toml` in the app's language.
pub fn t(key: &str) -> String {
    host::translate(key, &[])
}

/// [`t`] with each `%{name}` replaced from `args`.
pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    let args: Vec<(String, String)> = args
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    host::translate(key, &args)
}

/// The panel id `panel_button` gets for a button in the extension's
/// section of Settings.
pub const SETTINGS_PANEL: &str = "settings";

/// Show or clear the checkmark of a menu command declared with
/// `toggle = true`.
pub fn set_command_checked(command: &str, checked: bool) {
    host::set_command_checked(command, checked)
}

pub fn log(message: &str) {
    host::log(message)
}

// --- Panel building helpers ---

pub fn button(id: &str, label: &str, style: ButtonStyle) -> Button {
    Button {
        id: id.into(),
        label: label.into(),
        style,
        enabled: true,
    }
}

// --- Dispatch from the WIT exports to the registered `Extension` ---

thread_local! {
    static EXTENSION: RefCell<Option<Box<dyn Extension>>> = const { RefCell::new(None) };
}

#[doc(hidden)]
pub fn __set_extension(ext: Box<dyn Extension>) {
    EXTENSION.with(|e| *e.borrow_mut() = Some(ext));
}

fn with_extension<T>(f: impl FnOnce(&mut dyn Extension) -> T) -> T {
    EXTENSION.with(|e| {
        let mut e = e.borrow_mut();
        let ext = e
            .as_mut()
            .expect("register_extension! was not used, or init was not called");
        f(ext.as_mut())
    })
}

extern "Rust" {
    /// Defined by `register_extension!` in the extension's crate.
    fn __osm_gpui_extension_new() -> Box<dyn Extension>;
}

struct Component;

impl bindings::Guest for Component {
    fn init() -> Result<(), String> {
        // SAFETY: `register_extension!` defines this symbol with exactly this
        // signature; an extension without it fails to link.
        let ext = unsafe { __osm_gpui_extension_new() };
        __set_extension(ext);
        Ok(())
    }

    fn run_command(id: String) -> Result<(), String> {
        with_extension(|e| e.run_command(&id))
    }

    fn marker_clicked(id: String) -> Result<(), String> {
        with_extension(|e| e.marker_clicked(&id))
    }

    fn panel_button(
        panel: String,
        button: String,
        inputs: Vec<(String, String)>,
    ) -> Result<(), String> {
        with_extension(|e| e.panel_button(&panel, &button, &Inputs(inputs)))
    }

    fn panel_closed(panel: String) {
        with_extension(|e| e.panel_closed(&panel))
    }

    fn viewport_changed() -> Result<(), String> {
        with_extension(|e| e.viewport_changed())
    }

    fn selection_changed() -> Result<(), String> {
        with_extension(|e| e.selection_changed())
    }

    fn settings() -> Option<Panel> {
        with_extension(|e| e.settings())
    }

    fn changeset_tags() -> Vec<(String, String)> {
        with_extension(|e| e.changeset_tags())
    }

    fn upload_finished(changesets: Vec<i64>) {
        with_extension(|e| e.upload_finished(&changesets))
    }
}

bindings::export!(Component with_types_in bindings);

/// Name the type that implements [`Extension`]. Use once, in the
/// extension's crate root.
#[macro_export]
macro_rules! register_extension {
    ($ty:ty) => {
        #[no_mangle]
        fn __osm_gpui_extension_new() -> Box<dyn $crate::Extension> {
            Box::new(<$ty as $crate::Extension>::new())
        }
    };
}
