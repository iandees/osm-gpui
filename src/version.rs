//! App version and the identifying strings derived from it.
//!
//! The single source of truth is `version` in `Cargo.toml`. These are protocol
//! strings (HTTP `User-Agent`, changeset `created_by`) and are deliberately not
//! localized.

/// The crate version from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shared User-Agent string sent on all outgoing HTTP requests, so the OSM API,
/// tile servers, and other services see a single consistent, identifying
/// string regardless of which module made the request.
pub const USER_AGENT: &str = concat!(
    "osm-gpui/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/iandees/osm-gpui)"
);

/// Value of the `created_by` changeset tag (and the `generator` attribute of
/// exported/uploaded OSM XML).
pub const CREATED_BY: &str = concat!("osm-gpui ", env!("CARGO_PKG_VERSION"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_contains_cargo_version_and_project_url() {
        assert!(USER_AGENT.starts_with(&format!("osm-gpui/{}", env!("CARGO_PKG_VERSION"))));
        assert!(USER_AGENT.contains(VERSION));
        assert!(USER_AGENT.contains("https://github.com/iandees/osm-gpui"));
    }

    #[test]
    fn created_by_is_name_and_version() {
        assert_eq!(
            CREATED_BY,
            format!("osm-gpui {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
