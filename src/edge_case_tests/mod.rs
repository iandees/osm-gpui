//! Edge-case tests for the editing operations, written from a catalogue of
//! situations other editors are known to get wrong (see
//! `docs/testing/edge-cases-from-josm-and-id.md`). Every test builds a small
//! network in code, runs the operation through the same entry point the UI
//! uses (`MapViewer::run_selection_action`, which records undo), and checks
//! the data, the pending upload diff, and one undo / redo.

mod create_multipolygon;
mod delete_and_disconnect;
mod interactions_reverse_split;
mod interactions_topology;
mod join_and_merge;
mod limits_and_data;
mod multipolygon_edits;
mod parse_and_export;
mod purge;
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

use crate::{EditMode, MapViewer};
use gpui::{px, size, Context, TestAppContext};
use osm_gpui::layers::diff::LayerDiff;
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::layers::{EditableLayer, LayerId};
use osm_gpui::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
use osm_gpui::selection::{FeatureKind, FeatureRef};
use osm_gpui::viewport::Viewport;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub(crate) use osm_gpui::selection::FeatureKind::{Node as N, Relation as R, Way as W};

/// Builds a server-side data set (everything has a positive id and version 3
/// unless the id is negative).
#[derive(Default)]
pub(crate) struct Build {
    data: Option<OsmData>,
}

fn tag_map(tags: &[(&str, &str)]) -> HashMap<String, String> {
    tags.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

impl Build {
    pub(crate) fn new() -> Self {
        Build {
            data: Some(OsmData {
                nodes: HashMap::new(),
                ways: HashMap::new(),
                relations: Vec::new(),
                bounds: Vec::new(),
            }),
        }
    }
    fn d(&mut self) -> &mut OsmData {
        self.data.as_mut().unwrap()
    }
    /// A node on a rough grid derived from its id (distinct, near 40N 74W).
    pub(crate) fn node(mut self, id: i64, tags: &[(&str, &str)]) -> Self {
        let a = id.unsigned_abs() as f64;
        self.d().nodes.insert(
            id,
            OsmNode {
                id,
                lat: 40.0 + (a % 7.0) * 0.0004,
                lon: -74.0 + a * 0.0003,
                version: 3,
                tags: tag_map(tags),
            },
        );
        self
    }
    pub(crate) fn nodes(mut self, ids: &[i64]) -> Self {
        for &id in ids {
            self = self.node(id, &[]);
        }
        self
    }
    pub(crate) fn way(mut self, id: i64, nodes: &[i64], tags: &[(&str, &str)]) -> Self {
        self.d().ways.insert(
            id,
            OsmWay {
                id,
                nodes: nodes.to_vec(),
                version: 3,
                tags: tag_map(tags),
            },
        );
        self
    }
    pub(crate) fn relation(
        mut self,
        id: i64,
        tags: &[(&str, &str)],
        members: &[(MemberType, i64, &str)],
    ) -> Self {
        self.d().relations.push(OsmRelation {
            id,
            version: 2,
            tags: tag_map(tags),
            members: members
                .iter()
                .map(|&(member_type, reference, role)| OsmMember {
                    member_type,
                    reference,
                    role: role.to_string(),
                })
                .collect(),
        });
        self
    }
    pub(crate) fn done(self) -> OsmData {
        self.data.unwrap()
    }
}

pub(crate) fn wm(id: i64, role: &str) -> (MemberType, i64, &str) {
    (MemberType::Way, id, role)
}
pub(crate) fn nm(id: i64, role: &str) -> (MemberType, i64, &str) {
    (MemberType::Node, id, role)
}
pub(crate) fn rm(id: i64, role: &str) -> (MemberType, i64, &str) {
    (MemberType::Relation, id, role)
}

/// Everything about a layer's data that an undo must restore exactly.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Snap {
    nodes: BTreeMap<i64, ((f64, f64), TagList)>,
    ways: BTreeMap<i64, (Vec<i64>, TagList)>,
    relations: BTreeMap<i64, (TagList, Vec<MemberRow>)>,
}

type MemberRow = (String, i64, String);

type TagList = Vec<(String, String)>;

pub(crate) struct Session<'a, 'c> {
    pub view: &'a mut MapViewer,
    pub cx: &'a mut Context<'c, MapViewer>,
    pub layer: LayerId,
}

/// Run `f` against a view whose active layer holds `data` as its server
/// baseline.
pub(crate) fn with_session(
    cx: &mut TestAppContext,
    data: OsmData,
    f: impl FnOnce(&mut Session<'_, '_>) + 'static,
) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, _window, cx| {
            view.viewport = Viewport::new(40.0, -74.0, 18.0, size(px(800.0), px(600.0)));
            let layer = view.layer_manager.alloc_id();
            view.layer_manager
                .add_layer(Box::new(OsmLayer::new_with_data(
                    layer,
                    "Net".to_string(),
                    Arc::new(data),
                )));
            view.active_layer = Some(layer);
            view.mode = EditMode::Select;
            f(&mut Session { view, cx, layer });
        })
        .unwrap();
}

impl Session<'_, '_> {
    fn editable(&self) -> &dyn EditableLayer {
        self.view
            .layer_manager
            .find_layer(self.layer)
            .unwrap()
            .as_editable()
            .unwrap()
    }
    fn editable_mut(&mut self) -> &mut dyn EditableLayer {
        self.view
            .layer_manager
            .find_layer_mut(self.layer)
            .unwrap()
            .as_editable_mut()
            .unwrap()
    }
    pub(crate) fn feature(&self, kind: FeatureKind, id: i64) -> FeatureRef {
        FeatureRef {
            layer_id: self.layer,
            kind,
            id,
        }
    }
    pub(crate) fn select(&mut self, items: &[(FeatureKind, i64)]) {
        let list = items.iter().map(|&(k, id)| self.feature(k, id)).collect();
        self.view.set_selection(list);
    }
    /// Run a selection action the way the toolbar / shortcut does. Returns
    /// whether it recorded an undo step.
    pub(crate) fn act(&mut self, action: &str) -> bool {
        let before = self.view.undo_stack.revision;
        self.view.run_selection_action(action, self.cx);
        self.view.undo_stack.revision != before
    }
    pub(crate) fn status(&self) -> String {
        self.view
            .status_message
            .as_ref()
            .map(|(m, _)| m.to_string())
            .unwrap_or_default()
    }
    pub(crate) fn undo(&mut self) {
        let action = self.view.undo_stack.undo().expect("something to undo");
        self.view.apply_undo_action(&action, false);
    }
    pub(crate) fn redo(&mut self) {
        let action = self.view.undo_stack.redo().expect("something to redo");
        self.view.apply_undo_action(&action, true);
    }
    pub(crate) fn steps(&self) -> usize {
        self.view.undo_stack.actions.len()
    }
    pub(crate) fn snap(&self) -> Snap {
        let e = self.editable();
        let tags = |kind, id| {
            let mut t = e.feature_tags(&self.feature(kind, id)).unwrap_or_default();
            t.sort();
            t
        };
        let mut s = Snap {
            nodes: BTreeMap::new(),
            ways: BTreeMap::new(),
            relations: BTreeMap::new(),
        };
        for f in e.all_features() {
            match f.kind {
                FeatureKind::Node => {
                    s.nodes
                        .insert(f.id, (e.node_lat_lon(f.id).unwrap(), tags(f.kind, f.id)));
                }
                FeatureKind::Way => {
                    s.ways
                        .insert(f.id, (e.way_node_ids(f.id).unwrap(), tags(f.kind, f.id)));
                }
                FeatureKind::Relation => {}
            }
        }
        for r in e.relations() {
            let mut t: Vec<(String, String)> = r.tags.into_iter().collect();
            t.sort();
            s.relations.insert(
                r.id,
                (
                    t,
                    r.members
                        .iter()
                        .map(|m| (m.member_type.to_string(), m.reference, m.role.clone()))
                        .collect(),
                ),
            );
        }
        s
    }
    pub(crate) fn way_nodes(&self, id: i64) -> Option<Vec<i64>> {
        self.editable().way_node_ids(id)
    }
    pub(crate) fn tags(&self, kind: FeatureKind, id: i64) -> Vec<(String, String)> {
        let mut t = self
            .editable()
            .feature_tags(&self.feature(kind, id))
            .unwrap_or_default();
        t.sort();
        t
    }
    pub(crate) fn has_way(&self, id: i64) -> bool {
        self.editable().way_node_ids(id).is_some()
    }
    pub(crate) fn has_node(&self, id: i64) -> bool {
        self.editable().node_lat_lon(id).is_some()
    }
    pub(crate) fn members(&self, relation: i64) -> Vec<(i64, String)> {
        self.editable()
            .relation(relation)
            .unwrap()
            .members
            .iter()
            .map(|m| (m.reference, m.role.clone()))
            .collect()
    }
    pub(crate) fn typed_members(&self, relation: i64) -> Vec<(MemberType, i64, String)> {
        self.editable()
            .relation(relation)
            .unwrap()
            .members
            .iter()
            .map(|m| (m.member_type, m.reference, m.role.clone()))
            .collect()
    }
    pub(crate) fn diff(&self) -> LayerDiff {
        self.view
            .layer_manager
            .find_layer(self.layer)
            .unwrap()
            .diff_for_upload()
    }
    pub(crate) fn counts(&self) -> (usize, usize, usize) {
        self.diff().counts()
    }
    pub(crate) fn xml(&self) -> String {
        osm_gpui::osm_upload::build_osm_change_xml(1, &[("Net", self.diff())])
    }
    pub(crate) fn set_tag(&mut self, kind: FeatureKind, id: i64, k: &str, v: &str) {
        self.editable_mut().set_tag(kind, id, k, v);
    }
    /// The one way that exists now but did not before (ids below zero).
    pub(crate) fn new_ways(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .snap()
            .ways
            .keys()
            .copied()
            .filter(|&i| i < 0)
            .collect();
        ids.sort();
        ids
    }
    /// Every relation member and way node refers to something the layer has.
    pub(crate) fn assert_no_dangling(&self) {
        let s = self.snap();
        for (w, (nodes, _)) in &s.ways {
            for n in nodes {
                assert!(s.nodes.contains_key(n), "way {w} names missing node {n}");
            }
        }
        for (r, (_, members)) in &s.relations {
            for (t, id, _) in members {
                let ok = match t.as_str() {
                    "node" => s.nodes.contains_key(id),
                    "way" => s.ways.contains_key(id),
                    _ => s.relations.contains_key(id),
                };
                // Members that were never loaded are allowed to be absent
                // only if the test says so by using ids above 9000.
                assert!(ok || *id > 9000, "relation {r} names missing {t} {id}");
            }
        }
    }
}

/// Position of `needle` in `hay`, panicking with context if absent.
pub(crate) fn at(hay: &str, needle: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("`{needle}` not found in {hay}"))
}

pub(crate) fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub(crate) fn mem(list: &[(i64, &str)]) -> Vec<(i64, String)> {
    list.iter().map(|&(i, r)| (i, r.to_string())).collect()
}
