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

pub(crate) fn build_url(bounds: &GeoBounds) -> String {
    format!(
        "https://api.openstreetmap.org/api/0.6/map?bbox={:.7},{:.7},{:.7},{:.7}",
        bounds.min_lon, bounds.min_lat, bounds.max_lon, bounds.max_lat
    )
}

const USER_AGENT: &str = concat!("osm-gpui/", env!("CARGO_PKG_VERSION"));

/// Synchronous fetch — call from a worker thread, not the UI thread.
pub fn fetch_bbox(bounds: GeoBounds) -> Result<OsmData, OsmApiError> {
    check_area(&bounds)?;

    let url = build_url(&bounds);
    let response = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .call();

    let body = match response {
        Ok(resp) => resp
            .into_string()
            .map_err(|e| OsmApiError::Network(e.to_string()))?,
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            return Err(OsmApiError::Http { status, body });
        }
        Err(e) => return Err(OsmApiError::Network(e.to_string())),
    };

    OsmParser::new()
        .parse_str(&body)
        .map_err(OsmApiError::Parse)
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
        let url = build_url(&b);
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
