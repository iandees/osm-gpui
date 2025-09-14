//! # OSM-GPUI Map Rendering Library
//!
//! A high-performance map rendering system built with Rust and the GPUI framework.
//! This library provides components for displaying OpenStreetMap data with
//! interactive pan and zoom capabilities.
//!
//! ## Features
//!
//! - Interactive map view with pan and zoom
//! - OpenStreetMap XML parsing and rendering
//! - Simple visualization: nodes as yellow rectangles, ways as blue lines
//! - Built on GPUI for smooth, GPU-accelerated rendering
//! - Native OS menu integration for file selection
//!
//! ## Quick Start
//!
//! ```rust
//! use osm_gpui::{OsmParser, OsmData};
//!
//! // Parse OSM data from file
//! let parser = OsmParser::new();
//! let osm_data = parser.parse_file("map.osm").unwrap();
//!
//! println!("Loaded {} nodes and {} ways",
//!          osm_data.nodes.len(),
//!          osm_data.ways.len());
//! ```

pub mod osm;

// Re-export main types for convenience
pub use osm::{OsmBounds, OsmData, OsmNode, OsmParser, OsmRelation, OsmWay};

// Re-export common dependencies
pub use gpui;

/// Current version of the library
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Library metadata
pub mod meta {
    /// Library name
    pub const NAME: &str = env!("CARGO_PKG_NAME");

    /// Library description
    pub const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

    /// Library authors
    pub const AUTHORS: &str = env!("CARGO_PKG_AUTHORS");

    /// Library repository URL
    pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
}
