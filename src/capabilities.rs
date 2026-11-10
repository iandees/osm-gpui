//! The OSM API's `GET /api/capabilities` document: what the connected server
//! supports (maximum download area, way/relation size limits, service status).
//!
//! The document is fetched in the background once per server and kept in
//! memory. Until a fetch succeeds (or when it fails) every limit has the
//! well-known value of the public OSM server, so behaviour is unchanged
//! without network access. Nothing here blocks the UI: callers read the
//! cached value (or the defaults) and may ask for a refresh, which runs on a
//! plain worker thread.

use crate::coordinates::GeoBounds;
use crate::http::{fetch_with_retries, HttpClient, HttpRequest, RetryPolicy, UreqClient};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Public-server default for `/map`: maximum bbox area in square degrees.
pub const DEFAULT_AREA_MAXIMUM: f64 = 0.25;
pub const DEFAULT_WAYNODES_MAXIMUM: u32 = 2000;
pub const DEFAULT_RELATIONMEMBERS_MAXIMUM: u32 = 32000;
pub const DEFAULT_CHANGESET_MAXIMUM_ELEMENTS: u32 = 10000;
pub const DEFAULT_TIMEOUT_SECONDS: u32 = 300;

/// Minimum time between failed (or "server not online") fetch attempts for
/// one server, so a down server isn't hammered.
const RETRY_COOLDOWN: Duration = Duration::from_secs(30);

/// Availability of a part of the service, from `<status .../>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    Online,
    ReadOnly,
    Offline,
    /// A value this version doesn't know; treated as usable.
    Unknown,
}

impl ServiceStatus {
    fn parse(s: &str) -> Self {
        match s {
            "online" => Self::Online,
            "readonly" => Self::ReadOnly,
            "offline" => Self::Offline,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    /// Maximum bbox area (square degrees) accepted by `/map`.
    pub area_maximum: f64,
    pub waynodes_maximum: u32,
    pub relationmembers_maximum: u32,
    pub changeset_maximum_elements: u32,
    pub timeout_seconds: u32,
    pub api_status: ServiceStatus,
    pub database_status: ServiceStatus,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            area_maximum: DEFAULT_AREA_MAXIMUM,
            waynodes_maximum: DEFAULT_WAYNODES_MAXIMUM,
            relationmembers_maximum: DEFAULT_RELATIONMEMBERS_MAXIMUM,
            changeset_maximum_elements: DEFAULT_CHANGESET_MAXIMUM_ELEMENTS,
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
            api_status: ServiceStatus::Online,
            database_status: ServiceStatus::Online,
        }
    }
}

impl Capabilities {
    /// Parse a capabilities document. Unknown or missing elements and
    /// unparsable values keep their defaults; only XML that is not
    /// well-formed is an error.
    pub fn parse(xml: &str) -> Result<Self, String> {
        let mut caps = Self::default();
        let mut reader = Reader::from_str(xml);
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => caps.apply_element(&e),
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(e) => return Err(format!("capabilities XML: {e}")),
            }
        }
        Ok(caps)
    }

    fn apply_element(&mut self, e: &BytesStart) {
        let attr = |name: &str| -> Option<String> {
            e.attributes()
                .flatten()
                .find(|a| a.key.as_ref() == name.as_bytes())
                .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
        };
        let uint = |v: Option<String>, slot: &mut u32| {
            if let Some(n) = v.and_then(|v| v.trim().parse::<u32>().ok()) {
                *slot = n;
            }
        };
        match e.name().as_ref() {
            b"area" => {
                if let Some(a) = attr("maximum").and_then(|v| v.trim().parse::<f64>().ok()) {
                    if a.is_finite() && a > 0.0 {
                        self.area_maximum = a;
                    }
                }
            }
            b"waynodes" => uint(attr("maximum"), &mut self.waynodes_maximum),
            b"relationmembers" => uint(attr("maximum"), &mut self.relationmembers_maximum),
            b"changesets" => uint(
                attr("maximum_elements"),
                &mut self.changeset_maximum_elements,
            ),
            b"timeout" => uint(attr("seconds"), &mut self.timeout_seconds),
            b"status" => {
                if let Some(s) = attr("api") {
                    self.api_status = ServiceStatus::parse(&s);
                }
                if let Some(s) = attr("database") {
                    self.database_status = ServiceStatus::parse(&s);
                }
            }
            _ => {}
        }
    }

    /// Area of `bounds` in square degrees, computed the way the server does:
    /// (maxlon - minlon) * (maxlat - minlat).
    pub fn bbox_area(bounds: &GeoBounds) -> f64 {
        (bounds.max_lon - bounds.min_lon) * (bounds.max_lat - bounds.min_lat)
    }

    /// Whether the server accepts a `/map` request for `bounds`.
    pub fn allows_download(&self, bounds: &GeoBounds) -> bool {
        Self::bbox_area(bounds) <= self.area_maximum
    }

    /// True when the API or its database is reported offline.
    pub fn is_offline(&self) -> bool {
        self.api_status == ServiceStatus::Offline || self.database_status == ServiceStatus::Offline
    }
}

#[derive(Default)]
struct Entry {
    caps: Option<Capabilities>,
    in_flight: bool,
    last_attempt: Option<Instant>,
}

fn store() -> &'static Mutex<HashMap<String, Entry>> {
    static STORE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    STORE.get_or_init(Default::default)
}

fn key(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

/// The capabilities known for `base_url`, or the defaults if none have been
/// fetched (yet).
pub fn for_server(base_url: &str) -> Capabilities {
    store()
        .lock()
        .ok()
        .and_then(|g| g.get(&key(base_url)).and_then(|e| e.caps.clone()))
        .unwrap_or_default()
}

/// Whether the server at `base_url` accepts a download of `bounds`. The one
/// shared download gate (status bar hint, Download action, `fetch_bbox`).
pub fn allows_download(base_url: &str, bounds: &GeoBounds) -> bool {
    for_server(base_url).allows_download(bounds)
}

fn log_debug(msg: &str) {
    if cfg!(debug_assertions) {
        eprintln!("capabilities: {msg}");
    }
}

/// Single-attempt blocking fetch of `<base>/api/capabilities`.
pub fn fetch_with(client: &dyn HttpClient, base_url: &str) -> Result<Capabilities, String> {
    let mut req = HttpRequest::get(format!("{}/api/capabilities", key(base_url)));
    req.timeout = Duration::from_secs(10);
    let resp = fetch_with_retries(client, &req, &RetryPolicy::none()).map_err(|e| e.to_string())?;
    let body = resp.into_string().map_err(|e| e.to_string())?;
    Capabilities::parse(&body)
}

/// Decide whether a fetch for `base_url` should start now, marking it in
/// flight if so. A server with no capabilities, or whose last known status
/// was offline (possibly stale), is retried at most once per cooldown; a
/// healthy cached entry is never refetched.
fn begin_attempt(base_url: &str, now: Instant) -> bool {
    let Ok(mut g) = store().lock() else {
        return false;
    };
    let e = g.entry(key(base_url)).or_default();
    if e.in_flight {
        return false;
    }
    let healthy = e.caps.as_ref().is_some_and(|c| !c.is_offline());
    if healthy {
        return false;
    }
    if e.last_attempt
        .is_some_and(|t| now.duration_since(t) < RETRY_COOLDOWN)
    {
        return false;
    }
    e.in_flight = true;
    e.last_attempt = Some(now);
    true
}

fn finish_attempt(base_url: &str, result: Result<Capabilities, String>) {
    let Ok(mut g) = store().lock() else {
        return;
    };
    let e = g.entry(key(base_url)).or_default();
    e.in_flight = false;
    match result {
        Ok(caps) => e.caps = Some(caps),
        // Keep whatever we had (or the defaults) and retry on a later attempt.
        Err(err) => log_debug(&format!("fetch for {base_url} failed: {err}")),
    }
}

/// Fetch synchronously if `begin_attempt` allows it. Returns whether a fetch ran.
pub fn refresh_blocking(client: &dyn HttpClient, base_url: &str, now: Instant) -> bool {
    if !begin_attempt(base_url, now) {
        return false;
    }
    finish_attempt(base_url, fetch_with(client, base_url));
    true
}

/// Start a background fetch for `base_url` if one is due (see
/// `begin_attempt`). Never blocks; safe to call on every download attempt,
/// at startup and after the server setting changes.
pub fn ensure_fetched(base_url: &str) {
    if base_url.is_empty() || !begin_attempt(base_url, Instant::now()) {
        return;
    }
    let base = base_url.to_string();
    let spawned = std::thread::Builder::new()
        .name("capabilities".into())
        .spawn({
            let base = base.clone();
            move || finish_attempt(&base, fetch_with(&UreqClient::new(), &base))
        });
    if spawned.is_err() {
        finish_attempt(&base, Err("could not spawn worker thread".into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, status_err, FakeClient};

    const LIVE: &str = include_str!("../tests/fixtures/capabilities.xml");

    fn b(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> GeoBounds {
        GeoBounds::new(min_lat, max_lat, min_lon, max_lon)
    }

    #[test]
    fn parses_live_fixture() {
        let c = Capabilities::parse(LIVE).unwrap();
        assert_eq!(c, Capabilities::default());
        assert_eq!(c.api_status, ServiceStatus::Online);
    }

    #[test]
    fn parses_non_default_values_and_status() {
        let xml = r#"<osm><api>
            <area maximum="0.5"/><waynodes maximum="1000"/>
            <relationmembers maximum="100"/>
            <changesets maximum_elements="5000"/>
            <timeout seconds="60"/>
            <status database="readonly" api="offline" gpx="online"/>
        </api></osm>"#;
        let c = Capabilities::parse(xml).unwrap();
        assert_eq!(c.area_maximum, 0.5);
        assert_eq!(c.waynodes_maximum, 1000);
        assert_eq!(c.relationmembers_maximum, 100);
        assert_eq!(c.changeset_maximum_elements, 5000);
        assert_eq!(c.timeout_seconds, 60);
        assert_eq!(c.database_status, ServiceStatus::ReadOnly);
        assert_eq!(c.api_status, ServiceStatus::Offline);
        assert!(c.is_offline());
    }

    #[test]
    fn partial_and_odd_documents_keep_defaults() {
        let c = Capabilities::parse(r#"<osm><api><area maximum="2"/><mystery x="1"/></api></osm>"#)
            .unwrap();
        assert_eq!(c.area_maximum, 2.0);
        assert_eq!(c.waynodes_maximum, DEFAULT_WAYNODES_MAXIMUM);
        assert_eq!(Capabilities::parse("").unwrap(), Capabilities::default());
        for bad in ["abc", "-1", "0", "NaN", ""] {
            let xml = format!(r#"<osm><api><area maximum="{bad}"/></api></osm>"#);
            assert_eq!(
                Capabilities::parse(&xml).unwrap().area_maximum,
                DEFAULT_AREA_MAXIMUM,
                "{bad}"
            );
        }
        let c = Capabilities::parse(r#"<status api="weird"/>"#).unwrap();
        assert_eq!(c.api_status, ServiceStatus::Unknown);
        assert!(!c.is_offline());
    }

    #[test]
    fn malformed_xml_is_an_error() {
        assert!(Capabilities::parse("<osm><api></osm>").is_err());
    }

    #[test]
    fn download_gate_at_and_around_default_limit() {
        let c = Capabilities::default();
        // 0.5 x 0.5 = 0.25 exactly: allowed.
        assert!(c.allows_download(&b(40.0, 40.5, -74.0, -73.5)));
        assert!(!c.allows_download(&b(40.0, 40.5, -74.0, -73.499)));
        assert!(c.allows_download(&b(40.0, 40.5, -74.0, -73.501)));
        assert!(!c.allows_download(&b(40.0, 41.0, -75.0, -74.0)));
    }

    #[test]
    fn download_gate_uses_server_maximum() {
        let c = Capabilities {
            area_maximum: 1.0,
            ..Default::default()
        };
        assert!(c.allows_download(&b(40.0, 41.0, -75.0, -74.0)));
        assert!(!c.allows_download(&b(40.0, 41.0, -75.0, -73.9)));
        let tight = Capabilities {
            area_maximum: 0.01,
            ..Default::default()
        };
        assert!(!tight.allows_download(&b(40.0, 40.2, -74.0, -73.9)));
    }

    #[test]
    fn fetch_success_is_stored_and_used_by_gate() {
        let base = "https://caps-success.test";
        let xml = r#"<osm><api><area maximum="1"/></api></osm>"#;
        let client = FakeClient::new(vec![ok(200, xml)]);
        assert!(!allows_download(base, &b(40.0, 41.0, -75.0, -74.0)));
        assert!(refresh_blocking(&client, base, Instant::now()));
        assert_eq!(
            client.requests.lock().unwrap()[0].url,
            format!("{base}/api/capabilities")
        );
        assert!(allows_download(base, &b(40.0, 41.0, -75.0, -74.0)));
        // A healthy cached entry is not refetched.
        assert!(!refresh_blocking(&client, base, Instant::now()));
        assert_eq!(client.request_count(), 1);
    }

    #[test]
    fn failure_keeps_defaults_and_retries_only_after_cooldown() {
        let base = "https://caps-failure.test/";
        let client = FakeClient::new(vec![status_err(503, "down"), ok(200, "<osm/>")]);
        let t0 = Instant::now();
        assert!(refresh_blocking(&client, base, t0));
        assert_eq!(for_server(base), Capabilities::default());
        // Inside the cooldown: no request.
        assert!(!refresh_blocking(
            &client,
            base,
            t0 + Duration::from_secs(5)
        ));
        assert_eq!(client.request_count(), 1);
        // After it: retried, and the success sticks.
        assert!(refresh_blocking(&client, base, t0 + RETRY_COOLDOWN));
        assert_eq!(client.request_count(), 2);
    }

    #[test]
    fn offline_status_is_rechecked_after_cooldown() {
        let base = "https://caps-offline.test";
        let offline = r#"<osm><api><status api="offline" database="online"/></api></osm>"#;
        let client = FakeClient::new(vec![ok(200, offline), ok(200, LIVE)]);
        let t0 = Instant::now();
        assert!(refresh_blocking(&client, base, t0));
        assert!(for_server(base).is_offline());
        assert!(refresh_blocking(&client, base, t0 + RETRY_COOLDOWN));
        assert!(!for_server(base).is_offline());
    }
}
