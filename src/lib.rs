//! # OSM-GPUI Map Rendering Library
//!
//! A high-performance map rendering system built with Rust and the GPUI framework.

pub mod auth;
pub mod coordinates;
pub mod custom_imagery_store;
pub mod idle_tracker;
pub mod imagery;
pub mod layers;
pub mod nsi;
pub mod osm;
pub mod osm_api;
pub mod osm_upload;
pub mod script;
pub mod selection;
pub mod settings_store;
pub mod style;
pub mod tile_cache;
pub mod tiles;
pub mod ui;
pub mod viewport;

pub use osm::{OsmBounds, OsmData, OsmNode, OsmParser, OsmRelation, OsmWay};
pub use gpui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shared User-Agent string sent on all outgoing HTTP requests, so the OSM API,
/// tile servers, and other services see a single consistent identifier for
/// this app regardless of which module made the request.
pub const USER_AGENT: &str = concat!("osm-gpui/", env!("CARGO_PKG_VERSION"));

/// Predicate for whether an HTTP status code from an idempotent GET is worth
/// retrying: 429 (rate limited), 509 (bandwidth limit exceeded, used by the
/// OSM API), and any 5xx server error. Other 4xx errors are not retried since
/// they indicate a request that won't succeed no matter how many times it's
/// repeated.
pub fn is_retryable_status(status: u16) -> bool {
    status == 429 || status == 509 || (500..600).contains(&status)
}

#[cfg(test)]
mod lib_tests {
    use super::*;

    #[test]
    fn user_agent_matches_expected_format() {
        assert_eq!(USER_AGENT, concat!("osm-gpui/", env!("CARGO_PKG_VERSION")));
        assert!(USER_AGENT.starts_with("osm-gpui/"));
    }

    #[test]
    fn retryable_status_covers_429_509_and_5xx() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(509));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(599));
    }

    #[test]
    fn retryable_status_excludes_other_4xx_and_2xx() {
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(600));
    }
}
