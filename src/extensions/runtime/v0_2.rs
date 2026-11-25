//! Extension API 0.2, kept so extensions built against it go on working.
//!
//! Its data types are the same as the current version's, so the bindings
//! reuse them (`with`) and every host function passes straight through to
//! the current implementation. What 0.2 lacks it never sees: it cannot set
//! a command's checkmark, and it has no settings section (the caller treats
//! `settings` as `none` for it).

use super::bindings::osm_gpui::extension::{host as cur_host, types as cur};
use super::HostState;

wasmtime::component::bindgen!({
    path: "src/extensions/wit/v0_2",
    world: "extension",
    with: {
        "osm-gpui:extension/types": super::bindings::osm_gpui::extension::types,
    },
});

use self::osm_gpui::extension::host;

impl host::Host for HostState {
    fn http_fetch(&mut self, r: cur::HttpRequest) -> Result<cur::HttpResponse, String> {
        cur_host::Host::http_fetch(self, r)
    }

    fn http_fetch_many(
        &mut self,
        requests: Vec<cur::HttpRequest>,
    ) -> Vec<Result<cur::HttpResponse, String>> {
        cur_host::Host::http_fetch_many(self, requests)
    }

    fn viewport(&mut self) -> cur::Bounds {
        cur_host::Host::viewport(self)
    }

    fn zoom(&mut self) -> f64 {
        cur_host::Host::zoom(self)
    }

    fn selection(&mut self) -> Vec<cur::FeatureRef> {
        cur_host::Host::selection(self)
    }

    fn fly_to(&mut self, at: cur::LatLon, zoom: f64) {
        cur_host::Host::fly_to(self, at, zoom)
    }

    fn download_area(&mut self, area: cur::Bounds) {
        cur_host::Host::download_area(self, area)
    }

    fn set_status(&mut self, message: String) {
        cur_host::Host::set_status(self, message)
    }

    fn open_url(&mut self, url: String) {
        cur_host::Host::open_url(self, url)
    }

    fn set_markers(&mut self, markers: Vec<cur::Marker>) {
        cur_host::Host::set_markers(self, markers)
    }

    fn show_panel(&mut self, id: String, panel: Option<cur::Panel>) {
        cur_host::Host::show_panel(self, id, panel)
    }

    fn storage_get(&mut self, key: String) -> Option<String> {
        cur_host::Host::storage_get(self, key)
    }

    fn storage_set(&mut self, key: String, value: Option<String>) {
        cur_host::Host::storage_set(self, key, value)
    }

    fn secret_get(&mut self, key: String) -> Option<String> {
        cur_host::Host::secret_get(self, key)
    }

    fn secret_set(&mut self, key: String, value: Option<String>) {
        cur_host::Host::secret_set(self, key, value)
    }

    fn locale(&mut self) -> String {
        cur_host::Host::locale(self)
    }

    fn translate(&mut self, key: String, args: Vec<(String, String)>) -> String {
        cur_host::Host::translate(self, key, args)
    }

    fn log(&mut self, message: String) {
        cur_host::Host::log(self, message)
    }
}
