use crate::coordinates::GeoBounds;
use crate::osm::{OsmData, OsmParser, OsmParseError};

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
        bounds.min_lon, bounds.min_lat, bounds.max_lon, bounds.max_lat
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
    check_area(&bounds)?;

    let url = build_url(base_url, &bounds);
    let body = fetch_with_retries(&url, token)?;

    OsmParser::new()
        .parse_str(&body)
        .map_err(OsmApiError::Parse)
}

/// Max number of attempts for a single bbox fetch, including the first try.
const MAX_ATTEMPTS: u32 = 3;
/// Delay before each retry, indexed by (attempt number - 1).
const RETRY_DELAYS: [std::time::Duration; MAX_ATTEMPTS as usize - 1] = [
    std::time::Duration::from_millis(200),
    std::time::Duration::from_millis(500),
];

/// `GET url` with a small bounded retry on transport errors and retryable
/// HTTP status codes (see `crate::is_retryable_status`). Other 4xx responses
/// are returned immediately since retrying won't help.
fn fetch_with_retries(url: &str, token: Option<&str>) -> Result<String, OsmApiError> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;

        let mut request = ureq::get(url)
            .set("User-Agent", crate::USER_AGENT)
            .timeout(std::time::Duration::from_secs(30));
        if let Some(token) = token {
            request = request.set("Authorization", &format!("Bearer {}", token));
        }

        match request.call() {
            Ok(resp) => {
                return resp
                    .into_string()
                    .map_err(|e| OsmApiError::Network(e.to_string()));
            }
            Err(ureq::Error::Status(status, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                if attempt < MAX_ATTEMPTS && crate::is_retryable_status(status) {
                    std::thread::sleep(RETRY_DELAYS[(attempt - 1) as usize]);
                    continue;
                }
                return Err(OsmApiError::Http { status, body });
            }
            Err(e) => {
                if attempt < MAX_ATTEMPTS {
                    std::thread::sleep(RETRY_DELAYS[(attempt - 1) as usize]);
                    continue;
                }
                return Err(OsmApiError::Network(e.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_check_rejects_large_bbox() {
        let b = GeoBounds::new(40.0, 41.0, -75.0, -74.0);
        assert!(matches!(check_area(&b), Err(OsmApiError::AreaTooLarge { .. })));
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
        assert_eq!(e.to_string(), "Area too large for OSM API (zoom in and try again)");
    }

    #[test]
    fn display_http_400_mentions_smaller_area() {
        let e = OsmApiError::Http { status: 400, body: "too many nodes".into() };
        assert_eq!(e.to_string(), "OSM API rejected request (400) — try a smaller area");
    }

    #[test]
    fn display_http_509_mentions_rate_limit() {
        let e = OsmApiError::Http { status: 509, body: String::new() };
        assert_eq!(e.to_string(), "OSM API rate-limited (509) — try again later");
    }

    #[test]
    fn display_http_other_uses_first_body_line() {
        let e = OsmApiError::Http { status: 503, body: "Service down\nretry later".into() };
        assert_eq!(e.to_string(), "OSM API error 503: Service down");
    }
}
