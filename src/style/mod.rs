//! Styling for map features.
//!
//! Currently exposes a minimal subset of MapCSS (JOSM variant) — enough
//! to color ways/nodes and set line weight and node size per tag
//! pattern. See `mapcss` submodule for the parser and evaluator.

pub mod mapcss;

pub use mapcss::{NodeStyle, ParseError, Stylesheet, WayStyle};
