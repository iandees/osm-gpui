use crate::coordinates::GeoBounds;
use crate::http::{
    fetch_with_retries, HttpClient, HttpError, HttpRequest, RetryPolicy, UreqClient,
};
use crate::osm::{OsmData, OsmParseError, OsmParser};

const MAX_AREA_SQ_DEG: f64 = 0.25;

#[derive(Debug)]
pub enum OsmApiError {
    AreaTooLarge { area_sq_deg: f64 },
    Http { status: u16, body: String },
    Network(String),
    Parse(OsmParseError),
}

impl std::fmt::Display for OsmApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OsmApiError::AreaTooLarge { .. } => {
                write!(f, "Area too large for OSM API (zoom in and try again)")
            }
            OsmApiError::Http { status: 400, .. } => {
                write!(f, "OSM API rejected request (400) — try a smaller area")
            }
            OsmApiError::Http { status: 509, .. } => {
                write!(f, "OSM API rate-limited (509) — try again later")
            }
            OsmApiError::Http { status, body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(f, "OSM API error {}: {}", status, first_line)
            }
            OsmApiError::Network(msg) => write!(f, "Network error: {}", msg),
            OsmApiError::Parse(e) => write!(f, "Failed to parse OSM response: {}", e),
        }
    }
}

pub fn check_area(bounds: &GeoBounds) -> Result<(), OsmApiError> {
    let area = (bounds.max_lon - bounds.min_lon) * (bounds.max_lat - bounds.min_lat);
    if area > MAX_AREA_SQ_DEG {
        Err(OsmApiError::AreaTooLarge { area_sq_deg: area })
    } else {
        Ok(())
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
    check_area(&bounds)?;

    let url = build_url(base_url, &bounds);
    let body = fetch_map_body(client, &url, token)?;

    OsmParser::new()
        .parse_str(&body)
        .map_err(OsmApiError::Parse)
}

/// `GET url` with a small bounded retry on transport errors and retryable HTTP
/// status codes (see `crate::is_retryable_status`). Other 4xx responses are
/// returned immediately since retrying won't help.
fn fetch_map_body(
    client: &dyn HttpClient,
    url: &str,
    token: Option<&str>,
) -> Result<String, OsmApiError> {
    let mut req = HttpRequest::get(url);
    if let Some(token) = token {
        req = req.bearer(token);
    }

    let resp = fetch_with_retries(client, &req, &RetryPolicy::standard()).map_err(|e| match e {
        HttpError::Status { status, body } => OsmApiError::Http {
            status,
            body: String::from_utf8_lossy(&body).into_owned(),
        },
        HttpError::Transport(msg) => OsmApiError::Network(msg),
    })?;

    resp.into_string()
        .map_err(|e| OsmApiError::Network(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, status_err, FakeClient};

    #[test]
    fn area_check_rejects_large_bbox() {
        let b = GeoBounds::new(40.0, 41.0, -75.0, -74.0);
        assert!(matches!(
            check_area(&b),
            Err(OsmApiError::AreaTooLarge { .. })
        ));
    }

    #[test]
    fn area_check_accepts_small_bbox() {
        let b = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        assert!(check_area(&b).is_ok());
    }

    #[test]
    fn area_check_accepts_exact_limit() {
        let b = GeoBounds::new(40.0, 40.5, -74.0, -73.5);
        assert!(check_area(&b).is_ok());
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
