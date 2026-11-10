use crate::capabilities;
use crate::coordinates::GeoBounds;
use std::time::Instant;

use crate::http::{
    fetch_with_retries_progress, HttpClient, HttpError, HttpRequest, RetryPolicy, UreqClient,
};
use crate::osm::{OsmData, OsmParseError, OsmParser};
use crate::selection::FeatureKind;
use crate::tr;
use crate::transfer::{Throttle, TransferHandle, TransferPhase, REPORT_INTERVAL};

#[derive(Debug)]
pub enum OsmApiError {
    AreaTooLarge {
        area_sq_deg: f64,
    },
    Http {
        status: u16,
        body: String,
    },
    Network(String),
    Parse(OsmParseError),
    /// The user cancelled the download.
    Cancelled,
}

impl std::fmt::Display for OsmApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OsmApiError::Cancelled => write!(f, "{}", tr!("transfer.download.cancelled")),
            OsmApiError::AreaTooLarge { .. } => write!(f, "{}", tr!("error.api.area_too_large")),
            OsmApiError::Http { status: 400, .. } => {
                write!(f, "{}", tr!("error.api.rejected"))
            }
            OsmApiError::Http { status: 509, .. } => {
                write!(f, "{}", tr!("error.api.rate_limited"))
            }
            OsmApiError::Http { status, body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(
                    f,
                    "{}",
                    tr!("error.api.http", status = status, detail = first_line)
                )
            }
            OsmApiError::Network(msg) => write!(f, "{}", tr!("error.network", detail = msg)),
            OsmApiError::Parse(e) => write!(f, "{}", tr!("error.api.parse", detail = e)),
        }
    }
}

/// Refuse `bounds` if the server at `base_url` would (its advertised maximum
/// area, or the public server's 0.25 sq. degrees until that is known).
pub fn check_area(bounds: &GeoBounds, base_url: &str) -> Result<(), OsmApiError> {
    if capabilities::allows_download(base_url, bounds) {
        Ok(())
    } else {
        Err(OsmApiError::AreaTooLarge {
            area_sq_deg: capabilities::Capabilities::bbox_area(bounds),
        })
    }
}

pub(crate) fn build_url(base_url: &str, bounds: &GeoBounds) -> String {
    format!(
        "{}/api/0.6/map?bbox={:.7},{:.7},{:.7},{:.7}",
        base_url.trim_end_matches('/'),
        bounds.min_lon,
        bounds.min_lat,
        bounds.max_lon,
        bounds.max_lat
    )
}

/// Synchronous fetch — call from a worker thread, not the UI thread. `token`, if
/// present, is sent as an OAuth2 bearer token so the request is attributed to a
/// logged-in user.
pub fn fetch_bbox(
    bounds: GeoBounds,
    base_url: &str,
    token: Option<&str>,
) -> Result<OsmData, OsmApiError> {
    fetch_bbox_with(&UreqClient::new(), bounds, base_url, token)
}

/// Same as `fetch_bbox`, but against an injected `HttpClient` so it's testable
/// without a real network. Kept `pub(crate)` since tests are the only other caller.
pub(crate) fn fetch_bbox_with(
    client: &dyn HttpClient,
    bounds: GeoBounds,
    base_url: &str,
    token: Option<&str>,
) -> Result<OsmData, OsmApiError> {
    check_area(&bounds, base_url)?;

    let url = build_url(base_url, &bounds);
    let body = fetch_map_body(client, &url, token, None)?;

    parse_download(&body, &bounds)
}

/// Parse a `/map` response. The API's response carries a `<bounds>` element;
/// if one is missing, the area we asked for is what was downloaded.
fn parse_download(body: &str, requested: &GeoBounds) -> Result<OsmData, OsmApiError> {
    let mut data = OsmParser::new()
        .parse_str(body)
        .map_err(OsmApiError::Parse)?;
    if data.bounds.is_empty() {
        data.bounds.push(crate::osm::OsmBounds {
            min_lat: requested.min_lat,
            max_lat: requested.max_lat,
            min_lon: requested.min_lon,
            max_lon: requested.max_lon,
        });
    }
    Ok(data)
}

/// `fetch_bbox` that reports its progress through `progress` (phase, bytes
/// received, `Content-Length` when sent) and stops early, with
/// `OsmApiError::Cancelled`, once `progress.is_cancelled()`.
pub fn fetch_bbox_tracked(
    bounds: GeoBounds,
    base_url: &str,
    token: Option<&str>,
    progress: &TransferHandle,
) -> Result<OsmData, OsmApiError> {
    fetch_bbox_tracked_with(&UreqClient::new(), bounds, base_url, token, progress)
}

pub(crate) fn fetch_bbox_tracked_with(
    client: &dyn HttpClient,
    bounds: GeoBounds,
    base_url: &str,
    token: Option<&str>,
    progress: &TransferHandle,
) -> Result<OsmData, OsmApiError> {
    check_area(&bounds, base_url)?;

    let url = build_url(base_url, &bounds);
    let body = fetch_map_body(client, &url, token, Some(progress))?;

    if progress.is_cancelled() {
        return Err(OsmApiError::Cancelled);
    }
    progress.set_phase(TransferPhase::Reading);
    parse_download(&body, &bounds)
}

/// `GET url` with a small bounded retry on transport errors and retryable HTTP
/// status codes (see `crate::is_retryable_status`). Other 4xx responses are
/// returned immediately since retrying won't help. With `progress`, the body
/// is read in chunks and counted as it arrives.
fn fetch_map_body(
    client: &dyn HttpClient,
    url: &str,
    token: Option<&str>,
    progress: Option<&TransferHandle>,
) -> Result<String, OsmApiError> {
    let mut req = HttpRequest::get(url);
    if let Some(token) = token {
        req = req.bearer(token);
    }

    let mut throttle = Throttle::new(REPORT_INTERVAL);
    let result = fetch_with_retries_progress(
        client,
        &req,
        &RetryPolicy::standard(),
        &mut |bytes, total| match progress {
            None => true,
            Some(p) => {
                if p.is_cancelled() {
                    return false;
                }
                if throttle.ready(Instant::now()) {
                    p.set_phase(TransferPhase::Downloading);
                    p.set_bytes(bytes, total);
                }
                true
            }
        },
    );
    let resp = result.map_err(|e| match e {
        HttpError::Status { status, body } => OsmApiError::Http {
            status,
            body: String::from_utf8_lossy(&body).into_owned(),
        },
        HttpError::Transport(msg) => OsmApiError::Network(msg),
        HttpError::Cancelled => OsmApiError::Cancelled,
    })?;

    if let Some(p) = progress {
        // The throttle may have swallowed the last few reports.
        p.set_phase(TransferPhase::Downloading);
        p.set_bytes(resp.body.len() as u64, None);
    }

    resp.into_string()
        .map_err(|e| OsmApiError::Network(e.to_string()))
}

/// One object as the server has it right now.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerElement {
    /// The object (for a way fetched in full, with its nodes).
    Present(OsmData),
    /// The server answered 410: the object has been deleted.
    Gone,
}

impl ServerElement {
    /// The current server version of `(kind, id)`; `None` for a deleted or
    /// absent object.
    pub fn version(&self, kind: FeatureKind, id: i64) -> Option<i32> {
        let ServerElement::Present(data) = self else {
            return None;
        };
        match kind {
            FeatureKind::Node => data.nodes.get(&id).map(|n| n.version),
            FeatureKind::Way => data.ways.get(&id).map(|w| w.version),
            FeatureKind::Relation => data
                .relations
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.version),
        }
    }
}

pub(crate) fn element_url(base_url: &str, kind: FeatureKind, id: i64, full: bool) -> String {
    let noun = match kind {
        FeatureKind::Node => "node",
        FeatureKind::Way => "way",
        FeatureKind::Relation => "relation",
    };
    format!(
        "{}/api/0.6/{}/{}{}",
        base_url.trim_end_matches('/'),
        noun,
        id,
        // A way's nodes come along with `/full`; a relation is wanted alone.
        if full && kind == FeatureKind::Way {
            "/full"
        } else {
            ""
        }
    )
}

/// `GET /api/0.6/[node|way|relation]/#id` (a way with `/full` when `full`),
/// for resolving an upload conflict. Synchronous — call from a worker thread.
pub fn fetch_element(
    base_url: &str,
    token: Option<&str>,
    kind: FeatureKind,
    id: i64,
    full: bool,
) -> Result<ServerElement, OsmApiError> {
    fetch_element_with(&UreqClient::new(), base_url, token, kind, id, full)
}

pub(crate) fn fetch_element_with(
    client: &dyn HttpClient,
    base_url: &str,
    token: Option<&str>,
    kind: FeatureKind,
    id: i64,
    full: bool,
) -> Result<ServerElement, OsmApiError> {
    let url = element_url(base_url, kind, id, full);
    match fetch_map_body(client, &url, token, None) {
        Ok(body) => OsmParser::new()
            .parse_str(&body)
            .map(ServerElement::Present)
            .map_err(OsmApiError::Parse),
        Err(OsmApiError::Http { status: 410, .. }) => Ok(ServerElement::Gone),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, status_err, FakeClient};

    #[test]
    fn area_check_rejects_large_bbox() {
        let b = GeoBounds::new(40.0, 41.0, -75.0, -74.0);
        assert!(matches!(
            check_area(&b, "https://api.openstreetmap.org"),
            Err(OsmApiError::AreaTooLarge { .. })
        ));
    }

    #[test]
    fn area_check_accepts_small_bbox() {
        let b = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        assert!(check_area(&b, "https://api.openstreetmap.org").is_ok());
    }

    #[test]
    fn area_check_accepts_exact_limit() {
        let b = GeoBounds::new(40.0, 40.5, -74.0, -73.5);
        assert!(check_area(&b, "https://api.openstreetmap.org").is_ok());
    }

    #[test]
    fn url_is_min_lon_min_lat_max_lon_max_lat() {
        let b = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let url = build_url("https://api.openstreetmap.org", &b);
        assert_eq!(
            url,
            "https://api.openstreetmap.org/api/0.6/map?bbox=-74.0200000,40.7000000,-73.9800000,40.7500000"
        );
    }

    #[test]
    fn url_trims_trailing_slash_from_base() {
        let b = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let url = build_url("https://api.openstreetmap.org/", &b);
        assert_eq!(
            url,
            "https://api.openstreetmap.org/api/0.6/map?bbox=-74.0200000,40.7000000,-73.9800000,40.7500000"
        );
    }

    #[test]
    fn element_urls_ask_for_a_ways_nodes_only_when_wanted() {
        let base = "https://osm.test/";
        assert_eq!(
            element_url(base, FeatureKind::Node, 1, true),
            "https://osm.test/api/0.6/node/1"
        );
        assert_eq!(
            element_url(base, FeatureKind::Way, 2, false),
            "https://osm.test/api/0.6/way/2"
        );
        assert_eq!(
            element_url(base, FeatureKind::Way, 2, true),
            "https://osm.test/api/0.6/way/2/full"
        );
        assert_eq!(
            element_url(base, FeatureKind::Relation, 3, true),
            "https://osm.test/api/0.6/relation/3"
        );
    }

    #[test]
    fn fetch_element_reads_the_current_version() {
        let body = r#"<osm version="0.6"><node id="12345" visible="true" version="4" lat="1.5" lon="2.5"><tag k="a" v="b"/></node></osm>"#;
        let client = FakeClient::new(vec![ok(200, body)]);
        let server = fetch_element_with(
            &client,
            "https://osm.test",
            None,
            FeatureKind::Node,
            12345,
            false,
        )
        .unwrap();
        assert_eq!(server.version(FeatureKind::Node, 12345), Some(4));
        assert_eq!(server.version(FeatureKind::Node, 1), None);
        assert_eq!(
            client.requests.lock().unwrap()[0].url,
            "https://osm.test/api/0.6/node/12345"
        );
    }

    #[test]
    fn fetch_element_reports_a_deleted_object_as_gone() {
        let client = FakeClient::new(vec![status_err(410, "")]);
        let server =
            fetch_element_with(&client, "https://osm.test", None, FeatureKind::Way, 5, true)
                .unwrap();
        assert_eq!(server, ServerElement::Gone);
        assert_eq!(server.version(FeatureKind::Way, 5), None);
        assert_eq!(client.request_count(), 1, "410 is not retried");
    }

    #[test]
    fn fetch_element_passes_other_errors_through() {
        let client = FakeClient::new(vec![status_err(404, "")]);
        let err = fetch_element_with(
            &client,
            "https://osm.test",
            None,
            FeatureKind::Node,
            5,
            false,
        )
        .unwrap_err();
        assert!(matches!(err, OsmApiError::Http { status: 404, .. }));
    }

    #[test]
    fn display_area_too_large_is_user_readable() {
        let e = OsmApiError::AreaTooLarge { area_sq_deg: 1.0 };
        assert_eq!(
            e.to_string(),
            "Area too large for OSM API (zoom in and try again)"
        );
    }

    #[test]
    fn display_http_400_mentions_smaller_area() {
        let e = OsmApiError::Http {
            status: 400,
            body: "too many nodes".into(),
        };
        assert_eq!(
            e.to_string(),
            "OSM API rejected request (400) — try a smaller area"
        );
    }

    #[test]
    fn display_http_509_mentions_rate_limit() {
        let e = OsmApiError::Http {
            status: 509,
            body: String::new(),
        };
        assert_eq!(
            e.to_string(),
            "OSM API rate-limited (509) — try again later"
        );
    }

    #[test]
    fn display_http_other_uses_first_body_line() {
        let e = OsmApiError::Http {
            status: 503,
            body: "Service down\nretry later".into(),
        };
        assert_eq!(e.to_string(), "OSM API error 503: Service down");
    }

    const MINIMAL_OSM_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6">
  <node id="1" lat="40.0" lon="-74.0"/>
</osm>"#;

    #[test]
    fn fetch_bbox_with_happy_path_parses_body() {
        let client = FakeClient::new(vec![ok(200, MINIMAL_OSM_XML)]);
        let bounds = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let data = fetch_bbox_with(&client, bounds, "https://api.openstreetmap.org", None).unwrap();
        assert!(data.nodes.contains_key(&1));
    }

    #[test]
    fn fetch_bbox_with_falls_back_to_the_requested_bbox() {
        let client = FakeClient::new(vec![ok(200, MINIMAL_OSM_XML)]);
        let bounds = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let data = fetch_bbox_with(&client, bounds, "https://api.openstreetmap.org", None).unwrap();
        assert_eq!(data.bounds.len(), 1);
        assert_eq!(data.bounds[0].min_lat, 40.70);
        assert_eq!(data.bounds[0].max_lat, 40.75);
        assert_eq!(data.bounds[0].min_lon, -74.02);
        assert_eq!(data.bounds[0].max_lon, -73.98);
    }

    #[test]
    fn fetch_bbox_with_prefers_the_response_bounds() {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6">
  <bounds minlat="40.71" minlon="-74.01" maxlat="40.74" maxlon="-73.99"/>
  <node id="1" lat="40.72" lon="-74.0"/>
</osm>"#;
        let client = FakeClient::new(vec![ok(200, body)]);
        let bounds = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let data = fetch_bbox_with(&client, bounds, "https://api.openstreetmap.org", None).unwrap();
        assert_eq!(data.bounds.len(), 1);
        assert_eq!(data.bounds[0].min_lat, 40.71);
    }

    #[test]
    fn tracked_fetch_falls_back_to_the_requested_bbox() {
        use crate::transfer::{TransferKind, TransferPhase};
        let client = FakeClient::new(vec![ok(200, MINIMAL_OSM_XML)]);
        let bounds = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let handle = TransferHandle::new(TransferKind::Download, TransferPhase::Requesting);
        let data = fetch_bbox_tracked_with(
            &client,
            bounds,
            "https://api.openstreetmap.org",
            None,
            &handle,
        )
        .unwrap();
        assert_eq!(data.bounds.len(), 1);
        assert_eq!(data.bounds[0].min_lat, 40.70);
        assert_eq!(data.bounds[0].max_lon, -73.98);
    }

    #[test]
    fn tracked_fetch_prefers_the_response_bounds() {
        use crate::transfer::{TransferKind, TransferPhase};
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6">
  <bounds minlat="40.71" minlon="-74.01" maxlat="40.74" maxlon="-73.99"/>
  <node id="1" lat="40.72" lon="-74.0"/>
</osm>"#;
        let client = FakeClient::new(vec![ok(200, body)]);
        let bounds = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let handle = TransferHandle::new(TransferKind::Download, TransferPhase::Requesting);
        let data = fetch_bbox_tracked_with(
            &client,
            bounds,
            "https://api.openstreetmap.org",
            None,
            &handle,
        )
        .unwrap();
        assert_eq!(data.bounds.len(), 1);
        assert_eq!(data.bounds[0].min_lat, 40.71);
    }

    #[test]
    fn tracked_fetch_reports_bytes_then_reading() {
        use crate::transfer::{TransferKind, TransferPhase};
        let client = FakeClient::new(vec![ok(200, MINIMAL_OSM_XML)]);
        let bounds = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let handle = TransferHandle::new(TransferKind::Download, TransferPhase::Requesting);
        let data = fetch_bbox_tracked_with(
            &client,
            bounds,
            "https://api.openstreetmap.org",
            None,
            &handle,
        )
        .unwrap();
        assert!(data.nodes.contains_key(&1));
        let snap = handle.snapshot();
        assert_eq!(snap.phase, TransferPhase::Reading);
        assert_eq!(snap.bytes, MINIMAL_OSM_XML.len() as u64);
    }

    #[test]
    fn cancelled_tracked_fetch_stops_without_retrying() {
        use crate::transfer::{TransferKind, TransferPhase};
        let client = FakeClient::new(vec![ok(200, MINIMAL_OSM_XML), ok(200, MINIMAL_OSM_XML)]);
        let bounds = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let handle = TransferHandle::new(TransferKind::Download, TransferPhase::Requesting);
        handle.cancel();
        let err = fetch_bbox_tracked_with(
            &client,
            bounds,
            "https://api.openstreetmap.org",
            None,
            &handle,
        )
        .unwrap_err();
        assert!(matches!(err, OsmApiError::Cancelled));
        assert_eq!(client.request_count(), 1);
        assert_eq!(err.to_string(), "Download cancelled");
    }

    #[test]
    fn fetch_bbox_with_maps_http_error() {
        let client = FakeClient::new(vec![
            status_err(509, "rate limited"),
            status_err(509, "rate limited"),
            status_err(509, "rate limited"),
        ]);
        let bounds = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let err =
            fetch_bbox_with(&client, bounds, "https://api.openstreetmap.org", None).unwrap_err();
        assert!(matches!(err, OsmApiError::Http { status: 509, .. }));
    }

    #[test]
    fn fetch_bbox_with_area_too_large_never_makes_a_request() {
        let client = FakeClient::new(vec![]);
        let bounds = GeoBounds::new(40.0, 41.0, -75.0, -74.0);
        let err =
            fetch_bbox_with(&client, bounds, "https://api.openstreetmap.org", None).unwrap_err();
        assert!(matches!(err, OsmApiError::AreaTooLarge { .. }));
    }

    #[test]
    fn fetch_bbox_with_sends_bearer_token() {
        let client = FakeClient::new(vec![ok(200, MINIMAL_OSM_XML)]);
        let bounds = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        fetch_bbox_with(
            &client,
            bounds,
            "https://api.openstreetmap.org",
            Some("tok123"),
        )
        .unwrap();
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0]
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer tok123"));
    }
}
