//! # OSM-GPUI Map Rendering Library
//!
//! A high-performance map rendering system built with Rust and the GPUI framework.

pub mod capture;
pub mod coordinates;
pub mod idle_tracker;
pub mod imagery;
pub mod layers;
pub mod osm;
pub mod osm_api;
pub mod script;
pub mod selection;
pub mod style;
pub mod tile_cache;
pub mod tiles;
pub mod viewport;

pub use osm::{OsmBounds, OsmData, OsmNode, OsmParser, OsmRelation, OsmWay};
pub use gpui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
