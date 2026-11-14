//! Edge-case tests for the editing operations, written from a catalogue of
//! situations other editors are known to get wrong (see
//! `docs/testing/edge-cases-from-josm-and-id.md`). Every test builds a small
//! network in code, runs the operation through the same entry point the UI
//! uses (`MapViewer::run_selection_action`, which records undo), and checks
//! the data, the pending upload diff, and one undo / redo.

mod action_round_trip;
mod create_multipolygon;
mod delete_and_disconnect;
mod edit_primitives;
mod interactions_reverse_split;
mod interactions_topology;
mod join_and_merge;
mod limits_and_data;
mod multipolygon_edits;
mod parse_and_export;
mod relation_sort;

mod purge;
mod redo_drawing;
mod reverse;
mod shared_members;
mod simplify_way;
mod snap_join_node;
mod split;
mod undo_interactions;
mod undo_sequences;
mod unjoin_and_segment;
mod upload_failures;
mod upload_order;

// The submodules reach all of this through `use super::*`.
pub(crate) use crate::test_support::*;
pub(crate) use crate::{EditMode, MapViewer};
pub(crate) use gpui::{px, size, TestAppContext};
pub(crate) use osm_gpui::layers::diff::LayerDiff;
pub(crate) use osm_gpui::layers::osm_layer::OsmLayer;
pub(crate) use osm_gpui::layers::LayerId;
pub(crate) use osm_gpui::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
pub(crate) use osm_gpui::selection::FeatureKind;
pub(crate) use osm_gpui::viewport::Viewport;
pub(crate) use std::collections::{BTreeMap, HashMap};
pub(crate) use std::sync::Arc;
