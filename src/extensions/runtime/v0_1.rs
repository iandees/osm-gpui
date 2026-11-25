//! Extension API 0.1, kept so extensions built against it go on working.
//!
//! The WIT is the one released as 0.1.0, frozen in
//! `src/extensions/wit/v0_1`. A 0.1 extension gets the same host as a
//! current one: every host function here converts its arguments to the
//! current types, calls the current implementation, and converts the answer
//! back. What 0.1 lacks it simply never sees: no heading on markers, no
//! pictures in panels, no batch fetch, and no viewport or selection events
//! (the caller makes those no-ops for a 0.1 guest).
//!
//! To retire a version: delete its module and its WIT, and drop it from
//! `SUPPORTED_API_VERSIONS`.

use super::bindings::osm_gpui::extension::{host as cur_host, types as cur};
use super::HostState;

wasmtime::component::bindgen!({
    path: "src/extensions/wit/v0_1",
    world: "extension",
});

use self::osm_gpui::extension::{host, types};

// --- 0.1 -> current ---

fn lat_lon(v: types::LatLon) -> cur::LatLon {
    cur::LatLon {
        lat: v.lat,
        lon: v.lon,
    }
}

fn bounds(v: types::Bounds) -> cur::Bounds {
    cur::Bounds {
        min_lat: v.min_lat,
        min_lon: v.min_lon,
        max_lat: v.max_lat,
        max_lon: v.max_lon,
    }
}

fn request(v: types::HttpRequest) -> cur::HttpRequest {
    cur::HttpRequest {
        method: match v.method {
            types::HttpMethod::Get => cur::HttpMethod::Get,
            types::HttpMethod::Post => cur::HttpMethod::Post,
            types::HttpMethod::Put => cur::HttpMethod::Put,
            types::HttpMethod::Delete => cur::HttpMethod::Delete,
        },
        url: v.url,
        headers: v.headers,
        body: v.body,
    }
}

fn marker(v: types::Marker) -> cur::Marker {
    cur::Marker {
        id: v.id,
        at: lat_lon(v.at),
        color: v.color,
        selected: v.selected,
        heading: None,
    }
}

fn button(v: types::Button) -> cur::Button {
    cur::Button {
        id: v.id,
        label: v.label,
        style: match v.style {
            types::ButtonStyle::Primary => cur::ButtonStyle::Primary,
            types::ButtonStyle::Secondary => cur::ButtonStyle::Secondary,
            types::ButtonStyle::Danger => cur::ButtonStyle::Danger,
            types::ButtonStyle::Ghost => cur::ButtonStyle::Ghost,
        },
        enabled: v.enabled,
    }
}

fn element(v: types::Element) -> cur::Element {
    match v {
        types::Element::Heading(t) => cur::Element::Heading(t),
        types::Element::Text(t) => cur::Element::Text(t),
        types::Element::Muted(t) => cur::Element::Muted(t),
        types::Element::Property(p) => cur::Element::Property(p),
        types::Element::Buttons(b) => cur::Element::Buttons(b.into_iter().map(button).collect()),
        types::Element::Input(i) => cur::Element::Input(cur::TextInput {
            id: i.id,
            label: i.label,
            placeholder: i.placeholder,
            value: i.value,
            secret: i.secret,
        }),
        types::Element::Link(l) => cur::Element::Link(l),
        types::Element::Divider => cur::Element::Divider,
    }
}

fn panel(v: types::Panel) -> cur::Panel {
    cur::Panel {
        title: v.title,
        elements: v.elements.into_iter().map(element).collect(),
    }
}

// --- current -> 0.1 ---

fn bounds_back(v: cur::Bounds) -> types::Bounds {
    types::Bounds {
        min_lat: v.min_lat,
        min_lon: v.min_lon,
        max_lat: v.max_lat,
        max_lon: v.max_lon,
    }
}

fn feature_back(v: cur::FeatureRef) -> types::FeatureRef {
    types::FeatureRef {
        kind: match v.kind {
            cur::FeatureKind::Node => types::FeatureKind::Node,
            cur::FeatureKind::Way => types::FeatureKind::Way,
            cur::FeatureKind::Relation => types::FeatureKind::Relation,
        },
        id: v.id,
    }
}

fn response_back(v: cur::HttpResponse) -> types::HttpResponse {
    types::HttpResponse {
        status: v.status,
        body: v.body,
    }
}

impl types::Host for HostState {}

impl host::Host for HostState {
    fn http_fetch(&mut self, r: types::HttpRequest) -> Result<types::HttpResponse, String> {
        cur_host::Host::http_fetch(self, request(r)).map(response_back)
    }

    fn viewport(&mut self) -> types::Bounds {
        bounds_back(cur_host::Host::viewport(self))
    }

    fn zoom(&mut self) -> f64 {
        cur_host::Host::zoom(self)
    }

    fn selection(&mut self) -> Vec<types::FeatureRef> {
        cur_host::Host::selection(self)
            .into_iter()
            .map(feature_back)
            .collect()
    }

    fn fly_to(&mut self, at: types::LatLon, zoom: f64) {
        cur_host::Host::fly_to(self, lat_lon(at), zoom)
    }

    fn download_area(&mut self, area: types::Bounds) {
        cur_host::Host::download_area(self, bounds(area))
    }

    fn set_status(&mut self, message: String) {
        cur_host::Host::set_status(self, message)
    }

    fn open_url(&mut self, url: String) {
        cur_host::Host::open_url(self, url)
    }

    fn set_markers(&mut self, markers: Vec<types::Marker>) {
        cur_host::Host::set_markers(self, markers.into_iter().map(marker).collect())
    }

    fn show_panel(&mut self, id: String, p: Option<types::Panel>) {
        cur_host::Host::show_panel(self, id, p.map(panel))
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
