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
use crate::http::{fetch_with_retries, HttpClient, HttpError, HttpRequest, RetryPolicy};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Why the capabilities of a server are not known.
#[derive(Debug, Clone)]
pub enum CapabilitiesError {
    /// The request failed or the answer was not text.
    Fetch(HttpError),
    /// The answer is not well-formed XML.
    Parse(String),
    /// The background worker could not be started.
    Spawn,
}

impl From<HttpError> for CapabilitiesError {
    fn from(e: HttpError) -> Self {
        CapabilitiesError::Fetch(e)
    }
}

impl std::fmt::Display for CapabilitiesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapabilitiesError::Fetch(e) => write!(f, "{e}"),
            CapabilitiesError::Parse(detail) => write!(f, "capabilities XML: {detail}"),
            CapabilitiesError::Spawn => write!(f, "could not spawn worker thread"),
        }
    }
}

impl std::error::Error for CapabilitiesError {}

/// Public-server default for `/map`: maximum bbox area in square degrees.
pub const DEFAULT_AREA_MAXIMUM: f64 = 0.25;
/// Public-server default for the notes bbox (`<note_area maximum=.../>`).
pub const DEFAULT_NOTE_AREA_MAXIMUM: f64 = 25.0;
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
    /// Maximum bbox area (square degrees) accepted by `/notes`.
    pub note_area_maximum: f64,
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
            note_area_maximum: DEFAULT_NOTE_AREA_MAXIMUM,
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
    pub fn parse(xml: &str) -> Result<Self, CapabilitiesError> {
        let mut caps = Self::default();
        let mut reader = Reader::from_str(xml);
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => caps.apply_element(&e),
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(e) => return Err(CapabilitiesError::Parse(e.to_string())),
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
            b"note_area" => {
                if let Some(a) = attr("maximum").and_then(|v| v.trim().parse::<f64>().ok()) {
                    if a.is_finite() && a > 0.0 {
                        self.note_area_maximum = a;
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
    /// Why the latest attempt failed; cleared by a success.
    last_error: Option<CapabilitiesError>,
}

fn key(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

/// What is known about each server's capabilities, with the in-flight and
/// cooldown state of its fetches. Owned: tests and callers that want their
/// own state build one with `new`; the app reaches the shared one through the
/// free functions below.
#[derive(Default)]
pub struct CapabilitiesCache {
    servers: Mutex<HashMap<String, Entry>>,
}

impl CapabilitiesCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The capabilities known for `base_url`, or the defaults if none have
    /// been fetched (yet).
    pub fn for_server(&self, base_url: &str) -> Capabilities {
        self.servers
            .lock()
            .ok()
            .and_then(|g| g.get(&key(base_url)).and_then(|e| e.caps.clone()))
            .unwrap_or_default()
    }

    /// Why the latest fetch for `base_url` failed, if it did.
    pub fn last_error(&self, base_url: &str) -> Option<CapabilitiesError> {
        self.servers
            .lock()
            .ok()
            .and_then(|g| g.get(&key(base_url)).and_then(|e| e.last_error.clone()))
    }

    /// Whether the server at `base_url` accepts a download of `bounds`.
    pub fn allows_download(&self, base_url: &str, bounds: &GeoBounds) -> bool {
        self.for_server(base_url).allows_download(bounds)
    }

    /// Decide whether a fetch for `base_url` should start now, marking it in
    /// flight if so. A server with no capabilities, or whose last known status
    /// was offline (possibly stale), is retried at most once per cooldown; a
    /// healthy cached entry is never refetched.
    fn begin_attempt(&self, base_url: &str, now: Instant) -> bool {
        let Ok(mut g) = self.servers.lock() else {
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

    fn finish_attempt(&self, base_url: &str, result: Result<Capabilities, CapabilitiesError>) {
        let Ok(mut g) = self.servers.lock() else {
            return;
        };
        let e = g.entry(key(base_url)).or_default();
        e.in_flight = false;
        match result {
            Ok(caps) => {
                e.caps = Some(caps);
                e.last_error = None;
            }
            // Keep whatever we had (or the defaults) and retry on a later attempt.
            Err(err) => {
                log_debug(&format!("fetch for {base_url} failed: {err}"));
                e.last_error = Some(err);
            }
        }
    }

    /// Fetch synchronously if `begin_attempt` allows it. Returns whether a fetch ran.
    pub fn refresh_blocking(&self, client: &dyn HttpClient, base_url: &str, now: Instant) -> bool {
        if !self.begin_attempt(base_url, now) {
            return false;
        }
        self.finish_attempt(base_url, fetch_with(client, base_url));
        true
    }

    /// Start a background fetch for `base_url` if one is due (see
    /// `begin_attempt`). Never blocks; safe to call on every download attempt,
    /// at startup and after the server setting changes.
    pub fn ensure_fetched(self: &Arc<Self>, client: Arc<dyn HttpClient>, base_url: &str) {
        if base_url.is_empty() || !self.begin_attempt(base_url, Instant::now()) {
            return;
        }
        let base = base_url.to_string();
        let cache = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("capabilities".into())
            .spawn({
                let base = base.clone();
                move || cache.finish_attempt(&base, fetch_with(client.as_ref(), &base))
            });
        if spawned.is_err() {
            self.finish_attempt(&base, Err(CapabilitiesError::Spawn));
        }
    }
}

/// The app's one cache. A global because the size limits are consulted from
/// pure edit-time checks (`limits::way_over_limit`, `Limits::for_server`) and
/// the download gate in about twenty places that have no viewer to ask, and
/// from the settings store's server-changed hook.
static SHARED: OnceLock<Arc<CapabilitiesCache>> = OnceLock::new();

/// The app's shared cache.
pub fn shared() -> &'static Arc<CapabilitiesCache> {
    SHARED.get_or_init(Default::default)
}

/// The capabilities known for `base_url` in the shared cache.
pub fn for_server(base_url: &str) -> Capabilities {
    shared().for_server(base_url)
}

/// Why the latest fetch for `base_url` failed, if it did.
pub fn last_error(base_url: &str) -> Option<CapabilitiesError> {
    shared().last_error(base_url)
}

/// Whether the server at `base_url` accepts a download of `bounds`. The one
/// shared download gate (status bar hint, Download action, `osm_api::check_area`).
pub fn allows_download(base_url: &str, bounds: &GeoBounds) -> bool {
    shared().allows_download(base_url, bounds)
}

/// Largest bbox area (square degrees) the server at `base_url` answers a
/// notes request for.
pub fn max_note_area(base_url: &str) -> f64 {
    for_server(base_url).note_area_maximum
}

/// Most nodes a way may have on the server at `base_url`.
pub fn max_way_nodes(base_url: &str) -> usize {
    for_server(base_url).waynodes_maximum as usize
}

/// Most members a relation may have on the server at `base_url`.
pub fn max_relation_members(base_url: &str) -> usize {
    for_server(base_url).relationmembers_maximum as usize
}

/// Most elements (created + modified + deleted) one changeset may hold on the
/// server at `base_url`.
pub fn max_changeset_elements(base_url: &str) -> usize {
    for_server(base_url).changeset_maximum_elements as usize
}

fn log_debug(msg: &str) {
    if cfg!(debug_assertions) {
        eprintln!("capabilities: {msg}");
    }
}

/// Single-attempt blocking fetch of `<base>/api/capabilities`.
pub fn fetch_with(
    client: &dyn HttpClient,
    base_url: &str,
) -> Result<Capabilities, CapabilitiesError> {
    let mut req = HttpRequest::get(format!("{}/api/capabilities", key(base_url)));
    req.timeout = Duration::from_secs(10);
    let body = fetch_with_retries(client, &req, &RetryPolicy::none())?.into_string()?;
    Capabilities::parse(&body)
}

/// Start a background fetch into the shared cache if one is due.
pub fn ensure_fetched(client: Arc<dyn HttpClient>, base_url: &str) {
    shared().ensure_fetched(client, base_url);
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
    fn note_area_is_read_separately_from_the_map_area() {
        let c = Capabilities::parse(
            r#"<osm><api><area maximum="0.5"/><note_area maximum="9"/></api></osm>"#,
        )
        .unwrap();
        assert_eq!(c.area_maximum, 0.5);
        assert_eq!(c.note_area_maximum, 9.0);
        let c = Capabilities::parse(r#"<osm><api><area maximum="0.5"/></api></osm>"#).unwrap();
        assert_eq!(c.note_area_maximum, DEFAULT_NOTE_AREA_MAXIMUM);
        let c = Capabilities::parse(r#"<osm><api><note_area maximum="-1"/></api></osm>"#).unwrap();
        assert_eq!(c.note_area_maximum, DEFAULT_NOTE_AREA_MAXIMUM);
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
        let err = Capabilities::parse("<osm><api></osm>").unwrap_err();
        assert!(matches!(err, CapabilitiesError::Parse(_)), "{err:?}");
        assert!(err.to_string().starts_with("capabilities XML: "));
    }

    #[test]
    fn fetch_errors_are_typed_and_remembered() {
        let base = "https://caps-errors.test";
        let client = FakeClient::new(vec![status_err(503, "down")]);
        let err = fetch_with(&client, base).unwrap_err();
        assert!(matches!(
            err,
            CapabilitiesError::Fetch(HttpError::Status { status: 503, .. })
        ));
        assert_eq!(err.to_string(), "HTTP 503");

        let client = FakeClient::new(vec![ok(200, vec![0xff, 0xfe])]);
        assert!(matches!(
            fetch_with(&client, base),
            Err(CapabilitiesError::Fetch(HttpError::Transport(_)))
        ));

        let client = FakeClient::new(vec![ok(200, "<osm><api></osm>")]);
        assert!(matches!(
            fetch_with(&client, base),
            Err(CapabilitiesError::Parse(_))
        ));

        assert_eq!(
            CapabilitiesError::Spawn.to_string(),
            "could not spawn worker thread"
        );

        // The failure is kept for the server until a fetch succeeds.
        let failing = FakeClient::new(vec![status_err(500, "oops")]);
        let cache = CapabilitiesCache::new();
        assert!(cache.refresh_blocking(&failing, base, Instant::now()));
        assert!(matches!(
            cache.last_error(base),
            Some(CapabilitiesError::Fetch(_))
        ));
        let later = Instant::now() + RETRY_COOLDOWN + Duration::from_secs(1);
        let healthy = FakeClient::new(vec![ok(200, "<osm/>")]);
        assert!(cache.refresh_blocking(&healthy, base, later));
        assert!(cache.last_error(base).is_none());
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
        let cache = CapabilitiesCache::new();
        assert!(!cache.allows_download(base, &b(40.0, 41.0, -75.0, -74.0)));
        assert!(cache.refresh_blocking(&client, base, Instant::now()));
        assert_eq!(
            client.requests.lock().unwrap()[0].url,
            format!("{base}/api/capabilities")
        );
        assert!(cache.allows_download(base, &b(40.0, 41.0, -75.0, -74.0)));
        // A healthy cached entry is not refetched.
        assert!(!cache.refresh_blocking(&client, base, Instant::now()));
        assert_eq!(client.request_count(), 1);
    }

    #[test]
    fn size_limit_accessors_follow_the_fetched_server() {
        let base = "https://caps-limits.test";
        let cache = CapabilitiesCache::new();
        let caps = |c: &CapabilitiesCache| c.for_server(base);
        assert_eq!(caps(&cache).waynodes_maximum, 2000);
        assert_eq!(caps(&cache).relationmembers_maximum, 32000);
        assert_eq!(caps(&cache).changeset_maximum_elements, 10000);
        let xml = r#"<osm><api><waynodes maximum="10"/><relationmembers maximum="20"/>
            <changesets maximum_elements="30"/></api></osm>"#;
        let client = FakeClient::new(vec![ok(200, xml)]);
        assert!(cache.refresh_blocking(&client, base, Instant::now()));
        assert_eq!(caps(&cache).waynodes_maximum, 10);
        assert_eq!(caps(&cache).relationmembers_maximum, 20);
        assert_eq!(caps(&cache).changeset_maximum_elements, 30);
    }

    #[test]
    fn two_caches_do_not_share_what_they_know() {
        let base = "https://caps-independent.test";
        let xml = r#"<osm><api><area maximum="1"/></api></osm>"#;
        let (a, b_cache) = (CapabilitiesCache::new(), CapabilitiesCache::new());
        assert!(a.refresh_blocking(&FakeClient::new(vec![ok(200, xml)]), base, Instant::now()));
        assert_eq!(a.for_server(base).area_maximum, 1.0);
        assert_eq!(b_cache.for_server(base), Capabilities::default());
        // The other cache is not in cooldown or in flight either.
        let client = FakeClient::new(vec![ok(200, xml)]);
        assert!(b_cache.refresh_blocking(&client, base, Instant::now()));
    }

    #[test]
    fn failure_keeps_defaults_and_retries_only_after_cooldown() {
        let base = "https://caps-failure.test/";
        let client = FakeClient::new(vec![status_err(503, "down"), ok(200, "<osm/>")]);
        let t0 = Instant::now();
        let cache = CapabilitiesCache::new();
        assert!(cache.refresh_blocking(&client, base, t0));
        assert_eq!(cache.for_server(base), Capabilities::default());
        // Inside the cooldown: no request.
        assert!(!cache.refresh_blocking(&client, base, t0 + Duration::from_secs(5)));
        assert_eq!(client.request_count(), 1);
        // After it: retried, and the success sticks.
        assert!(cache.refresh_blocking(&client, base, t0 + RETRY_COOLDOWN));
        assert_eq!(client.request_count(), 2);
    }

    #[test]
    fn offline_status_is_rechecked_after_cooldown() {
        let base = "https://caps-offline.test";
        let offline = r#"<osm><api><status api="offline" database="online"/></api></osm>"#;
        let client = FakeClient::new(vec![ok(200, offline), ok(200, LIVE)]);
        let t0 = Instant::now();
        let cache = CapabilitiesCache::new();
        assert!(cache.refresh_blocking(&client, base, t0));
        assert!(cache.for_server(base).is_offline());
        assert!(cache.refresh_blocking(&client, base, t0 + RETRY_COOLDOWN));
        assert!(!cache.for_server(base).is_offline());
    }
}
