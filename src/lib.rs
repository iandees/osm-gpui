//! # OSM-GPUI Map Rendering Library
//!
//! A high-performance map rendering system built with Rust and the GPUI framework.

pub mod appearance;
pub mod assets;
pub mod auth;
pub mod building;
pub mod capabilities;
pub mod coordinates;
pub mod custom_imagery_store;
pub mod download_bounds;
pub mod extrude;
pub mod fields;
pub mod frame_stats;
pub mod geometry;
pub mod http;
pub mod i18n;
pub mod idle_tracker;
pub mod imagery;
pub mod imagery_offset;
pub mod interaction;
pub mod keybindings;
pub mod layers;
pub mod limits;
pub mod multipolygon;
pub mod nsi;
pub mod open_files;
pub mod osm;
pub mod osm_api;
pub mod osm_export;
pub mod osm_upload;
pub mod persist;
pub mod preset_recents;
pub mod preset_translations;
pub mod presets;
pub mod relation_overlay;
pub mod relations;
pub mod script;
pub mod search;
pub mod selection;
pub mod settings_store;
pub mod style;
pub mod svg_normalize;
pub mod tile_cache;
pub mod tiles;
pub mod topology;
pub mod transfer;
pub mod ui;
pub mod upload_chunks;
pub mod version;
pub mod viewport;

// Message catalogs: every `locales/*.yml`, compiled in. See `i18n`.
rust_i18n::i18n!("locales", fallback = "en", backend = i18n::extra_backend());

pub use gpui;
pub use osm::{MemberType, OsmBounds, OsmData, OsmMember, OsmNode, OsmParser, OsmRelation, OsmWay};

pub use version::{CREATED_BY, USER_AGENT, VERSION};

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
