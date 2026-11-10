//! Selection actions: one-shot operations on the currently selected objects
//! (square, circularize, reverse, delete, ...).
//!
//! `ACTIONS` is the single table that the map's right-click context menu, the
//! "Tools" menu in the menu bar, and the bare-key shortcuts all read from. To
//! add an action, add one entry here (and a `keybindings::SHORTCUTS` entry if
//! it gets a customizable key). Topology-changing actions (join ways, extend
//! way, disconnect node, ...) slot in the same way: `apply` gets the whole
//! `LayerManager` and returns a single `UndoableAction` — use
//! `UndoableAction::Batch` to bundle several steps.

use std::collections::HashMap;

use osm_gpui::geometry::{self, LatLon};
use osm_gpui::layers::{EditableLayer, LayerId, LayerManager};
use osm_gpui::selection::{FeatureKind, FeatureRef};
use osm_gpui::topology::{self, JoinWay};

use crate::undo::{NodeMoveUndoEntry, UndoableAction};

/// Read-only view of what an action's `is_applicable` predicate may look at.
pub(crate) struct ActionEnv<'a> {
    pub selection: &'a [FeatureRef],
    /// The layer geometry actions are allowed to edit.
    pub active_layer: Option<LayerId>,
    pub layers: &'a LayerManager,
}

impl<'a> ActionEnv<'a> {
    fn active_editable(&self) -> Option<&'a dyn EditableLayer> {
        self.layers.find_layer(self.active_layer?)?.as_editable()
    }

    /// The selected ways that live in the active layer, with their node ids
    /// and current node positions (ways with an unresolvable node are
    /// skipped).
    fn selected_ways(&self) -> Vec<WayGeometry> {
        let (Some(layer_id), Some(editable)) = (self.active_layer, self.active_editable()) else {
            return Vec::new();
        };
        selected_ways_in(editable, layer_id, self.selection)
    }
}

impl<'a> ActionEnv<'a> {
    /// The selected ways of the active layer as join candidates.
    fn selected_join_ways(&self) -> Vec<JoinWay> {
        let (Some(layer_id), Some(editable)) = (self.active_layer, self.active_editable()) else {
            return Vec::new();
        };
        join_ways_in(editable, layer_id, self.selection)
    }

    /// Whether the selection is a node that can be separated: one node of
    /// the active layer (plus optionally one way, to detach just that way)
    /// used in more than one place.
    /// Where Extend Way would continue from, for the selection: a single
    /// open way, or a single node that ends exactly one open way.
    fn extend_target(&self) -> Option<topology::Continuation> {
        let (layer_id, editable) = (self.active_layer?, self.active_editable()?);
        extend_target(editable, layer_id, self.selection)
    }

    fn can_separate_selection(&self) -> bool {
        let (Some(layer_id), Some(editable)) = (self.active_layer, self.active_editable()) else {
            return false;
        };
        let Some((node, way)) = separate_target(layer_id, self.selection) else {
            return false;
        };
        let ways = ways_using(editable, node);
        topology::can_separate(node, &ways, way)
    }
}

struct WayGeometry {
    way_id: i64,
    node_ids: Vec<i64>,
    points: Vec<LatLon>,
}

fn selected_ways_in(
    editable: &dyn EditableLayer,
    layer_id: LayerId,
    selection: &[FeatureRef],
) -> Vec<WayGeometry> {
    selection
        .iter()
        .filter(|f| f.layer_id == layer_id && f.kind == FeatureKind::Way)
        .filter_map(|f| {
            let node_ids = editable.way_node_ids(f.id)?;
            let points = node_ids
                .iter()
                .map(|&id| editable.node_lat_lon(id))
                .collect::<Option<Vec<_>>>()?;
            Some(WayGeometry {
                way_id: f.id,
                node_ids,
                points,
            })
        })
        .collect()
}

fn join_ways_in(
    editable: &dyn EditableLayer,
    layer_id: LayerId,
    selection: &[FeatureRef],
) -> Vec<JoinWay> {
    let mut seen = Vec::new();
    let mut ways = Vec::new();
    for f in selection
        .iter()
        .filter(|f| f.layer_id == layer_id && f.kind == FeatureKind::Way)
    {
        if seen.contains(&f.id) {
            continue;
        }
        seen.push(f.id);
        if let Some(nodes) = editable.way_node_ids(f.id) {
            ways.push(JoinWay {
                id: f.id,
                nodes,
                tags: editable.feature_tags(f).unwrap_or_default(),
            });
        }
    }
    ways
}

/// The node to separate, and the way to detach it from if one is selected
/// too. `None` unless the active layer's part of the selection is exactly one
/// node and at most one way.
fn separate_target(layer_id: LayerId, selection: &[FeatureRef]) -> Option<(i64, Option<i64>)> {
    let mut nodes = selection
        .iter()
        .filter(|f| f.layer_id == layer_id && f.kind == FeatureKind::Node);
    let node = nodes.next()?.id;
    if nodes.next().is_some() {
        return None;
    }
    let mut ways = selection
        .iter()
        .filter(|f| f.layer_id == layer_id && f.kind == FeatureKind::Way);
    let way = ways.next().map(|f| f.id);
    if ways.next().is_some() {
        return None;
    }
    Some((node, way))
}

fn extend_target(
    editable: &dyn EditableLayer,
    layer_id: LayerId,
    selection: &[FeatureRef],
) -> Option<topology::Continuation> {
    let [feature] = selection else {
        return None;
    };
    if feature.layer_id != layer_id {
        return None;
    }
    match feature.kind {
        FeatureKind::Way => topology::continue_way(feature.id, &editable.way_node_ids(feature.id)?),
        FeatureKind::Node => {
            editable.node_lat_lon(feature.id)?;
            topology::continue_from_node(feature.id, &ways_using(editable, feature.id))
        }
    }
}

/// `(way id, node ids)` for every way that uses `node`.
fn ways_using(editable: &dyn EditableLayer, node: i64) -> Vec<(i64, Vec<i64>)> {
    editable
        .ways_using_node(node)
        .into_iter()
        .filter_map(|id| Some((id, editable.way_node_ids(id)?)))
        .collect()
}

/// What running an action produced; the caller records `undo` as one step.
pub(crate) struct ActionOutcome {
    /// The undo step to record; `None` if the action changed no data (Extend
    /// Way only switches mode).
    pub undo: Option<UndoableAction>,
    /// Short status-bar message, e.g. "Squared 1 way".
    pub status: String,
    /// Whether the selection no longer makes sense (e.g. after a delete).
    pub clear_selection: bool,
    /// Replace the selection with these features (e.g. the surviving way
    /// after a join). Takes effect after `clear_selection`.
    pub select: Option<Vec<FeatureRef>>,
    /// Hand the user a way to keep drawing: switch to Add mode, continuing
    /// this way from this end.
    pub continue_way: Option<topology::Continuation>,
}

pub(crate) struct SelectionAction {
    pub id: &'static str,
    pub label: &'static str,
    /// Id of this action's customizable shortcut in `keybindings::SHORTCUTS`.
    pub shortcut_id: Option<&'static str>,
    /// Display text of a fixed (non-customizable) shortcut, if any.
    pub fixed_shortcut: Option<&'static str>,
    /// Whether the action needs an active editable layer to do anything.
    pub needs_active_layer: bool,
    /// Whether the Tools menu lists it (Delete lives in Edit instead).
    pub in_tools_menu: bool,
    /// Status-bar message when run with an unsuitable selection.
    pub hint: &'static str,
    pub is_applicable: fn(&ActionEnv) -> bool,
    pub apply: ApplyFn,
}

/// Performs an action on the selection in the active layer (if any). `Err`
/// carries a short message for the status bar and means nothing changed.
pub(crate) type ApplyFn =
    fn(&mut LayerManager, &[FeatureRef], Option<LayerId>) -> Result<ActionOutcome, String>;

pub(crate) const ACTIONS: &[SelectionAction] = &[
    SelectionAction {
        id: "square",
        label: "Square",
        shortcut_id: Some("action_square"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        hint: "Select a closed way (or a way with 3+ nodes) to square it",
        is_applicable: |env| {
            env.selected_ways()
                .iter()
                .any(|w| geometry::can_square(&w.points))
        },
        apply: apply_square,
    },
    SelectionAction {
        id: "circularize",
        label: "Circularize",
        shortcut_id: Some("action_circularize"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        hint: "Select a closed way to circularize it",
        is_applicable: |env| {
            env.selected_ways()
                .iter()
                .any(|w| geometry::can_circularize(&w.points))
        },
        apply: apply_circularize,
    },
    SelectionAction {
        id: "reverse",
        label: "Reverse Way",
        shortcut_id: Some("action_reverse"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        hint: "Select a way to reverse it",
        is_applicable: |env| env.selected_ways().iter().any(|w| w.node_ids.len() >= 2),
        apply: apply_reverse,
    },
    SelectionAction {
        id: "join",
        label: "Join Ways",
        shortcut_id: Some("action_join"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        hint: "Select two or more ways that connect end to end to join them",
        is_applicable: |env| topology::can_chain(&env.selected_join_ways()),
        apply: apply_join,
    },
    SelectionAction {
        id: "separate",
        label: "Separate Node",
        shortcut_id: Some("action_separate"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        hint: "Select a node shared by two or more ways to separate it",
        is_applicable: |env| env.can_separate_selection(),
        apply: apply_separate,
    },
    SelectionAction {
        id: "extend",
        label: "Extend Way",
        shortcut_id: Some("action_extend"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        hint: "Select an open way, or the end node of one, to continue drawing it",
        is_applicable: |env| env.extend_target().is_some(),
        apply: apply_extend,
    },
    SelectionAction {
        id: "delete",
        label: "Delete",
        shortcut_id: None,
        fixed_shortcut: Some("⌫"),
        needs_active_layer: false,
        in_tools_menu: false,
        hint: "Select something to delete",
        is_applicable: |env| {
            env.selection.iter().any(|f| {
                env.layers
                    .find_layer(f.layer_id)
                    .is_some_and(|l| l.as_editable().is_some())
            })
        },
        apply: apply_delete,
    },
];

pub(crate) fn find(id: &str) -> Option<&'static SelectionAction> {
    ACTIONS.iter().find(|a| a.id == id)
}

/// The action whose customizable shortcut currently resolves to the bare key
/// `key`, if any.
pub(crate) fn find_by_key(
    settings: &osm_gpui::settings_store::AppSettings,
    key: &str,
) -> Option<&'static SelectionAction> {
    ACTIONS.iter().find(|a| {
        a.shortcut_id
            .is_some_and(|sid| osm_gpui::keybindings::effective_spec(settings, sid) == key)
    })
}

impl SelectionAction {
    /// The actions applicable to `env`, in table order.
    pub(crate) fn applicable<'a>(
        env: &'a ActionEnv<'a>,
    ) -> impl Iterator<Item = &'static Self> + 'a {
        ACTIONS.iter().filter(|a| (a.is_applicable)(env))
    }

    /// Why this action can't run with `env`, for the status bar.
    pub(crate) fn why_not(&self, env: &ActionEnv) -> &'static str {
        if self.needs_active_layer && env.active_editable().is_none() {
            "Open or download OSM data first"
        } else {
            self.hint
        }
    }

    /// The shortcut to show next to this action, under the user's settings.
    pub(crate) fn shortcut_label(
        &self,
        settings: &osm_gpui::settings_store::AppSettings,
    ) -> Option<String> {
        if let Some(sid) = self.shortcut_id {
            return Some(display_shortcut(&osm_gpui::keybindings::effective_spec(
                settings, sid,
            )));
        }
        self.fixed_shortcut.map(str::to_string)
    }
}

/// Human-readable form of a keystroke spec: a bare letter shows uppercase,
/// as in menus ("q" -> "Q").
pub(crate) fn display_shortcut(spec: &str) -> String {
    match spec {
        "backspace" | "delete" => "⌫".to_string(),
        s if s.chars().count() == 1 => s.to_uppercase(),
        s => s.to_string(),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn active_editable_mut(
    layers: &mut LayerManager,
    active: Option<LayerId>,
) -> Result<(LayerId, &mut dyn EditableLayer), String> {
    let id = active.ok_or("Open or download OSM data first")?;
    let editable = layers
        .find_layer_mut(id)
        .and_then(|l| l.as_editable_mut())
        .ok_or("The active layer is not editable")?;
    Ok((id, editable))
}

/// Move nodes by running `reshape` over each selected way's positions.
/// `reshape` returns new positions for every point of the way, or `None` to
/// leave the way alone. Nodes shared between selected ways see each other's
/// moves, and the whole thing is recorded as one `MoveNodes` step.
fn reshape_ways(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
    reshape: fn(&[LatLon]) -> Option<Vec<LatLon>>,
    none_changed: &str,
    verb: &str,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let ways = selected_ways_in(editable, layer_id, selection);

    let mut original: HashMap<i64, LatLon> = HashMap::new();
    let mut current: HashMap<i64, LatLon> = HashMap::new();
    let mut changed_ways = 0usize;
    for way in &ways {
        let points: Vec<LatLon> = way
            .node_ids
            .iter()
            .zip(&way.points)
            .map(|(id, p)| *current.get(id).unwrap_or(p))
            .collect();
        let Some(new_points) = reshape(&points) else {
            continue;
        };
        for ((&id, &old), new) in way.node_ids.iter().zip(&way.points).zip(new_points) {
            original.entry(id).or_insert(old);
            current.insert(id, new);
        }
        changed_ways += 1;
    }
    if changed_ways == 0 {
        return Err(none_changed.to_string());
    }

    let mut entries: Vec<NodeMoveUndoEntry> = current
        .iter()
        .filter(|(id, after)| original[id] != **after)
        .map(|(&id, &after)| (id, original[&id], after))
        .collect();
    entries.sort_by_key(|e| e.0);
    if entries.is_empty() {
        return Err(none_changed.to_string());
    }
    let moves: Vec<(i64, f64, f64)> = entries
        .iter()
        .map(|&(id, _, (lat, lon))| (id, lat, lon))
        .collect();
    editable.commit_node_moves(&moves);

    Ok(ActionOutcome {
        undo: Some(UndoableAction::MoveNodes {
            per_layer: vec![(layer_id, entries)],
        }),
        status: format!("{verb} {}", plural(changed_ways, "way", "ways")),
        clear_selection: false,
        select: None,
        continue_way: None,
    })
}

fn apply_square(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    reshape_ways(
        layers,
        selection,
        active,
        geometry::orthogonalize,
        "No corners are close enough to 90° to square",
        "Squared",
    )
}

fn apply_circularize(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    reshape_ways(
        layers,
        selection,
        active,
        geometry::circularize,
        "Select a closed way to circularize it",
        "Circularized",
    )
}

fn apply_reverse(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let ways = selected_ways_in(editable, layer_id, selection);
    let mut reversed = Vec::new();
    for way in ways.iter().filter(|w| w.node_ids.len() >= 2) {
        if editable.reverse_way(way.way_id) {
            reversed.push(way.way_id);
        }
    }
    if reversed.is_empty() {
        return Err("Select a way to reverse it".to_string());
    }
    let status = format!("Reversed {}", plural(reversed.len(), "way", "ways"));
    Ok(ActionOutcome {
        undo: Some(UndoableAction::ReverseWays {
            layer: layer_id,
            way_ids: reversed,
        }),
        status,
        clear_selection: false,
        select: None,
        continue_way: None,
    })
}

/// Join the selected ways into one (JOSM "Combine Way"). See
/// `topology::plan_join` for the chaining, survivor and tag rules; ways that
/// belong to a relation can't be removed, so the join is refused. One undo
/// step: the survivor's new nodes and tags, then the deletion of the rest.
fn apply_join(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let ways = join_ways_in(editable, layer_id, selection);
    let plan = topology::plan_join(&ways).map_err(|e| e.message())?;
    for id in &plan.remove {
        if !editable.relations_containing_way(*id).is_empty() {
            return Err(format!(
                "Can't join: way {id} is a member of a relation and would be deleted"
            ));
        }
    }
    let kept = ways
        .iter()
        .find(|w| w.id == plan.keep)
        .expect("the plan keeps one of the given ways");
    let way_ref = |id| FeatureRef {
        layer_id,
        kind: FeatureKind::Way,
        id,
    };

    let mut actions = vec![UndoableAction::SetWayNodes {
        layer: layer_id,
        way_id: plan.keep,
        before: kept.nodes.clone(),
        after: plan.nodes.clone(),
    }];
    editable.set_way_nodes(plan.keep, plan.nodes);

    let mut tag_entries = Vec::new();
    for (key, value) in &plan.tags {
        if !kept.tags.iter().any(|(k, _)| k == key) {
            editable.set_tag(FeatureKind::Way, plan.keep, key, value);
            tag_entries.push((way_ref(plan.keep), key.clone(), None, Some(value.clone())));
        }
    }
    if !tag_entries.is_empty() {
        actions.push(UndoableAction::SetTags {
            entries: tag_entries,
        });
    }
    for id in &plan.remove {
        if let Some(snapshot) = editable.delete_feature(FeatureKind::Way, *id) {
            actions.push(UndoableAction::DeleteFeature {
                layer: layer_id,
                snapshot,
            });
        }
    }

    let label = format!("Joined {}", plural(ways.len(), "way", "ways"));
    Ok(ActionOutcome {
        undo: Some(UndoableAction::Batch {
            label: label.clone(),
            actions,
        }),
        status: label,
        clear_selection: false,
        select: Some(vec![way_ref(plan.keep)]),
        continue_way: None,
    })
}

/// Give each way but the first that uses the selected node its own copy of
/// it (iD "Disconnect", JOSM "Unglue"). With a way selected as well, only
/// that way is moved off the node. The copies are untagged, at the same
/// position, with fresh negative ids. One undo step.
fn apply_separate(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    const HINT: &str = "Select a node shared by two or more ways to separate it";
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let (node, detach) = separate_target(layer_id, selection).ok_or(HINT)?;
    let (lat, lon) = editable
        .node_lat_lon(node)
        .ok_or("The selected node is not in the active layer")?;
    let ways = ways_using(editable, node);

    let mut created = Vec::new();
    let edits = topology::plan_separate(node, &ways, detach, || {
        let id = editable
            .create_node(lat, lon, None)
            .expect("the layer has data, since the node exists");
        created.push(id);
        id
    })
    .ok_or(HINT)?;

    let mut actions: Vec<UndoableAction> = created
        .iter()
        .map(|&id| UndoableAction::CreateNode {
            layer: layer_id,
            id,
            lat,
            lon,
        })
        .collect();
    for edit in edits {
        editable.set_way_nodes(edit.way_id, edit.after.clone());
        actions.push(UndoableAction::SetWayNodes {
            layer: layer_id,
            way_id: edit.way_id,
            before: edit.before,
            after: edit.after,
        });
    }

    let label = format!(
        "Separated a node into {}",
        plural(created.len() + 1, "copy", "copies")
    );
    Ok(ActionOutcome {
        undo: Some(UndoableAction::Batch {
            label: label.clone(),
            actions,
        }),
        status: label,
        clear_selection: false,
        select: None,
        continue_way: None,
    })
}

/// Continue drawing the selected open way (iD "Continue this line"): from
/// its last node, or from the selected end node. Changes no data itself; it
/// puts Add mode into the state of having just placed that end node, so each
/// following click is its own undo step and Enter / Esc finish as usual.
fn apply_extend(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let from = extend_target(editable, layer_id, selection)
        .ok_or("Select an open way, or the end node of one, to continue drawing it")?;
    Ok(ActionOutcome {
        undo: None,
        status: "Click to add nodes to the way; Enter or Esc to finish".to_string(),
        clear_selection: false,
        select: Some(vec![FeatureRef {
            layer_id,
            kind: FeatureKind::Way,
            id: from.way_id,
        }]),
        continue_way: Some(from),
    })
}

/// Delete every selected feature. Node deletion is refused per feature if the
/// node is still referenced by a way (see `OsmLayer::delete_feature`); refused
/// features are skipped. The deletions are one undo step.
fn apply_delete(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    _active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let mut actions = Vec::new();
    for feature in selection {
        let Some(editable) = layers
            .find_layer_mut(feature.layer_id)
            .and_then(|l| l.as_editable_mut())
        else {
            continue;
        };
        if let Some(snapshot) = editable.delete_feature(feature.kind, feature.id) {
            actions.push(UndoableAction::DeleteFeature {
                layer: feature.layer_id,
                snapshot,
            });
        }
    }
    let deleted = actions.len();
    let status = match deleted {
        0 => return Err("Nothing to delete".to_string()),
        1 => "Deleted 1 feature".to_string(),
        n => format!("Deleted {n} features"),
    };
    let undo = if deleted == 1 {
        actions.remove(0)
    } else {
        UndoableAction::Batch {
            label: format!("Deleted {deleted} features"),
            actions,
        }
    };
    Ok(ActionOutcome {
        undo: Some(undo),
        status,
        clear_selection: true,
        select: None,
        continue_way: None,
    })
}

/// A small road network with server (positive) ids, shared by the tests here
/// and in `main.rs`:
///
/// - way 100 `[1,2,3]` and way 101 `[3,4,5]` chain end to end (101 also has
///   `name=Main`);
/// - way 102 `[5,6]` is a member of relation 900;
/// - way 103 `[8,4,9]` crosses way 101 at node 4;
/// - way 105 `[5,10]` has `name=Elm`, which conflicts with way 101;
/// - ways 106 `[11,12]` and 107 `[13,12]` are one-ways that run head to head;
/// - way 108 `[14,15,16,14]` is a closed ring.
#[cfg(test)]
pub(crate) mod test_support {
    use osm_gpui::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
    use std::collections::HashMap;

    pub(crate) fn network_data() -> OsmData {
        let nodes = (1..=16)
            .map(|id| {
                let node = OsmNode {
                    id,
                    lat: 40.0 + (id % 4) as f64 * 0.001,
                    lon: -74.0 + id as f64 * 0.001,
                    version: 1,
                    tags: if id == 4 {
                        HashMap::from([("highway".to_string(), "crossing".to_string())])
                    } else {
                        HashMap::new()
                    },
                };
                (id, node)
            })
            .collect();
        let way = |id: i64, nodes: &[i64], tags: &[(&str, &str)]| {
            (
                id,
                OsmWay {
                    id,
                    nodes: nodes.to_vec(),
                    version: 3,
                    tags: tags
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                },
            )
        };
        let road = ("highway", "residential");
        OsmData {
            nodes,
            ways: HashMap::from([
                way(100, &[1, 2, 3], &[road]),
                way(101, &[3, 4, 5], &[road, ("name", "Main")]),
                way(102, &[5, 6], &[road]),
                way(103, &[8, 4, 9], &[("highway", "footway")]),
                way(105, &[5, 10], &[road, ("name", "Elm")]),
                way(106, &[11, 12], &[road, ("oneway", "yes")]),
                way(107, &[13, 12], &[road, ("oneway", "yes")]),
                way(108, &[14, 15, 16, 14], &[("building", "yes")]),
            ]),
            relations: vec![OsmRelation {
                id: 900,
                members: vec![OsmMember {
                    member_type: MemberType::Way,
                    reference: 102,
                    role: String::new(),
                }],
                version: 1,
                tags: HashMap::new(),
            }],
            bounds: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use osm_gpui::layers::osm_layer::OsmLayer;

    /// A layer holding one skewed closed rectangle ("way") and one open
    /// two-node way, plus the ids needed to select them.
    struct Fixture {
        layers: LayerManager,
        layer_id: LayerId,
        rect: i64,
        rect_nodes: Vec<i64>,
        line: i64,
    }

    fn fixture() -> Fixture {
        let layer_id = LayerId(1);
        let mut layer = OsmLayer::new(layer_id);
        // ~40 m x 20 m, top edge sheared by ~8 degrees, near Boston.
        let nodes: Vec<i64> = [
            (42.36000, -71.06000),
            (42.36000, -71.05940),
            (42.36018, -71.05937),
            (42.36018, -71.05997),
        ]
        .iter()
        .map(|&(lat, lon)| layer.add_node(lat, lon))
        .collect();
        let mut ring = nodes.clone();
        ring.push(nodes[0]);
        let rect = layer.add_way(ring, vec![("building".into(), "yes".into())]);
        let a = layer.add_node(42.37, -71.07);
        let b = layer.add_node(42.371, -71.07);
        let line = layer.add_way(vec![a, b], vec![]);
        let mut layers = LayerManager::new();
        layers.add_layer(Box::new(layer));
        Fixture {
            layers,
            layer_id,
            rect,
            rect_nodes: nodes,
            line,
        }
    }

    fn way_ref(f: &Fixture, id: i64) -> FeatureRef {
        FeatureRef {
            layer_id: f.layer_id,
            kind: FeatureKind::Way,
            id,
        }
    }

    fn env<'a>(f: &'a Fixture, sel: &'a [FeatureRef]) -> ActionEnv<'a> {
        ActionEnv {
            selection: sel,
            active_layer: Some(f.layer_id),
            layers: &f.layers,
        }
    }

    fn applicable_ids(f: &Fixture, sel: &[FeatureRef]) -> Vec<&'static str> {
        let e = env(f, sel);
        SelectionAction::applicable(&e).map(|a| a.id).collect()
    }

    fn node_positions(f: &Fixture) -> Vec<LatLon> {
        let layer = f.layers.find_layer(f.layer_id).unwrap();
        let editable = layer.as_editable().unwrap();
        f.rect_nodes
            .iter()
            .map(|&id| editable.node_lat_lon(id).unwrap())
            .collect()
    }

    #[test]
    fn applicability_follows_the_selection() {
        let f = fixture();
        assert!(applicable_ids(&f, &[]).is_empty());
        assert_eq!(
            applicable_ids(&f, &[way_ref(&f, f.rect)]),
            ["square", "circularize", "reverse", "delete"]
        );
        // An open two-node way can only be reversed (or deleted).
        assert_eq!(
            applicable_ids(&f, &[way_ref(&f, f.line)]),
            ["reverse", "extend", "delete"]
        );
    }

    #[test]
    fn geometry_actions_ignore_other_layers_and_need_an_active_layer() {
        let f = fixture();
        let sel = [way_ref(&f, f.rect)];
        let other = ActionEnv {
            selection: &sel,
            active_layer: Some(LayerId(99)),
            layers: &f.layers,
        };
        assert!(!(find("square").unwrap().is_applicable)(&other));
        let none = ActionEnv {
            selection: &sel,
            active_layer: None,
            layers: &f.layers,
        };
        assert_eq!(
            find("square").unwrap().why_not(&none),
            "Open or download OSM data first"
        );
        assert_eq!(
            find("circularize").unwrap().why_not(&env(&f, &[])),
            "Select a closed way to circularize it"
        );
    }

    #[test]
    fn square_moves_nodes_marks_modified_and_records_one_move_step() {
        let mut f = fixture();
        let before = node_positions(&f);
        let sel = [way_ref(&f, f.rect)];
        let out = (find("square").unwrap().apply)(&mut f.layers, &sel, Some(f.layer_id)).unwrap();
        assert_eq!(out.status, "Squared 1 way");
        assert!(!out.clear_selection);
        let UndoableAction::MoveNodes { per_layer } = out.undo.as_ref().unwrap() else {
            panic!("expected one MoveNodes step");
        };
        assert_eq!(per_layer.len(), 1);
        assert_eq!(per_layer[0].1.len(), 4);
        assert_ne!(node_positions(&f), before);
        assert!(f.layers.find_layer(f.layer_id).unwrap().is_modified());

        // Squaring again finds nothing left to do.
        let again = (find("square").unwrap().apply)(&mut f.layers, &sel, Some(f.layer_id));
        assert!(again.is_err());
    }

    #[test]
    fn circularize_then_undo_entries_restore_positions() {
        let mut f = fixture();
        let before = node_positions(&f);
        let sel = [way_ref(&f, f.rect)];
        let out =
            (find("circularize").unwrap().apply)(&mut f.layers, &sel, Some(f.layer_id)).unwrap();
        let UndoableAction::MoveNodes { per_layer } = out.undo.as_ref().unwrap() else {
            panic!("expected MoveNodes");
        };
        // Each entry's `before` is the original position.
        for (id, b, _) in &per_layer[0].1 {
            let idx = f.rect_nodes.iter().position(|n| n == id).unwrap();
            assert_eq!(*b, before[idx]);
        }
        assert_ne!(node_positions(&f), before);
    }

    #[test]
    fn reverse_flips_the_way_and_reports_it_for_undo() {
        let mut f = fixture();
        let sel = [way_ref(&f, f.rect), way_ref(&f, f.line)];
        let out = (find("reverse").unwrap().apply)(&mut f.layers, &sel, Some(f.layer_id)).unwrap();
        assert_eq!(out.status, "Reversed 2 ways");
        let UndoableAction::ReverseWays { way_ids, .. } = out.undo.as_ref().unwrap() else {
            panic!("expected ReverseWays");
        };
        assert_eq!(way_ids.len(), 2);
        let layer = f.layers.find_layer(f.layer_id).unwrap();
        let nodes = layer.as_editable().unwrap().way_node_ids(f.rect).unwrap();
        assert_eq!(nodes.first(), Some(&f.rect_nodes[0]));
        assert_eq!(nodes[1], f.rect_nodes[3]);
    }

    #[test]
    fn delete_is_one_undo_step_and_clears_the_selection() {
        let mut f = fixture();
        let sel = [way_ref(&f, f.rect), way_ref(&f, f.line)];
        let out = (find("delete").unwrap().apply)(&mut f.layers, &sel, Some(f.layer_id)).unwrap();
        assert!(out.clear_selection);
        assert_eq!(out.status, "Deleted 2 features");
        assert!(
            matches!(out.undo.as_ref().unwrap(), UndoableAction::Batch { actions, .. } if actions.len() == 2)
        );
        assert_eq!(
            out.undo.as_ref().unwrap().description(),
            "Deleted 2 features"
        );
    }

    #[test]
    fn shortcuts_resolve_through_settings() {
        let settings = osm_gpui::settings_store::AppSettings::default();
        assert_eq!(find_by_key(&settings, "q").map(|a| a.id), Some("square"));
        assert_eq!(
            find_by_key(&settings, "o").map(|a| a.id),
            Some("circularize")
        );
        assert_eq!(find_by_key(&settings, "r").map(|a| a.id), Some("reverse"));
        assert_eq!(find_by_key(&settings, "c").map(|a| a.id), Some("join"));
        assert_eq!(find_by_key(&settings, "g").map(|a| a.id), Some("separate"));
        assert_eq!(find_by_key(&settings, "e").map(|a| a.id), Some("extend"));
        assert!(find_by_key(&settings, "z").is_none());
        assert_eq!(
            find("square").unwrap().shortcut_label(&settings).unwrap(),
            "Q"
        );
        assert_eq!(
            find("delete").unwrap().shortcut_label(&settings).unwrap(),
            "⌫"
        );
    }

    #[test]
    fn every_action_shortcut_id_exists_in_the_keybinding_table() {
        for a in ACTIONS {
            if let Some(sid) = a.shortcut_id {
                assert!(osm_gpui::keybindings::SHORTCUTS.iter().any(|d| d.id == sid));
            }
        }
    }

    /// The `test_support` network, in a layer with a server baseline.
    struct Net {
        layers: LayerManager,
        layer_id: LayerId,
    }

    fn net() -> Net {
        let layer_id = LayerId(1);
        let layer = OsmLayer::new_with_data(
            layer_id,
            "Net".to_string(),
            std::sync::Arc::new(test_support::network_data()),
        );
        let mut layers = LayerManager::new();
        layers.add_layer(Box::new(layer));
        Net { layers, layer_id }
    }

    impl Net {
        fn feature(&self, kind: FeatureKind, id: i64) -> FeatureRef {
            FeatureRef {
                layer_id: self.layer_id,
                kind,
                id,
            }
        }
        fn ways(&self, ids: &[i64]) -> Vec<FeatureRef> {
            ids.iter()
                .map(|&id| self.feature(FeatureKind::Way, id))
                .collect()
        }
        fn run(&mut self, action: &str, sel: &[FeatureRef]) -> Result<ActionOutcome, String> {
            (find(action).unwrap().apply)(&mut self.layers, sel, Some(self.layer_id))
        }
        fn editable(&self) -> &dyn EditableLayer {
            self.layers
                .find_layer(self.layer_id)
                .unwrap()
                .as_editable()
                .unwrap()
        }
        fn layer(&self) -> &dyn osm_gpui::layers::MapLayer {
            self.layers.find_layer(self.layer_id).unwrap()
        }
        fn applicable(&self, sel: &[FeatureRef]) -> Vec<&'static str> {
            let e = ActionEnv {
                selection: sel,
                active_layer: Some(self.layer_id),
                layers: &self.layers,
            };
            SelectionAction::applicable(&e).map(|a| a.id).collect()
        }
    }

    #[test]
    fn join_and_separate_are_offered_for_the_right_selections() {
        let n = net();
        let node4 = n.feature(FeatureKind::Node, 4);
        assert!(n.applicable(&n.ways(&[100, 101])).contains(&"join"));
        // One way, ways that don't connect, or a ring: nothing to join.
        assert!(!n.applicable(&n.ways(&[100])).contains(&"join"));
        assert!(!n.applicable(&n.ways(&[100, 103])).contains(&"join"));
        assert!(!n.applicable(&n.ways(&[108, 100])).contains(&"join"));
        // A conflicting join is still offered, so running it can explain.
        assert!(n.applicable(&n.ways(&[101, 105])).contains(&"join"));

        assert!(n.applicable(&[node4]).contains(&"separate"));
        // Node 4 with a way that uses it, or one that doesn't.
        let with_way = [node4, n.feature(FeatureKind::Way, 103)];
        assert!(n.applicable(&with_way).contains(&"separate"));
        let unrelated = [node4, n.feature(FeatureKind::Way, 100)];
        assert!(!n.applicable(&unrelated).contains(&"separate"));
        // Used by one way only; and two nodes at once.
        assert!(!n
            .applicable(&[n.feature(FeatureKind::Node, 1)])
            .contains(&"separate"));
        let two = [node4, n.feature(FeatureKind::Node, 3)];
        assert!(!n.applicable(&two).contains(&"separate"));
        assert!(!n.applicable(&[]).contains(&"separate"));
    }

    #[test]
    fn join_chains_the_ways_unions_tags_and_leaves_the_result_selected() {
        let mut n = net();
        let out = n.run("join", &n.ways(&[100, 101])).unwrap();
        assert_eq!(out.status, "Joined 2 ways");
        assert_eq!(out.select, Some(n.ways(&[100])));
        assert!(!out.clear_selection);
        assert_eq!(n.editable().way_node_ids(100), Some(vec![1, 2, 3, 4, 5]));
        assert_eq!(n.editable().way_node_ids(101), None);
        let tags = n
            .editable()
            .feature_tags(&n.feature(FeatureKind::Way, 100))
            .unwrap();
        assert!(tags.contains(&("name".to_string(), "Main".to_string())));
        assert!(tags.contains(&("highway".to_string(), "residential".to_string())));

        // Marked modified, and the upload diff sees the survivor as changed
        // and the other way as deleted (nodes are untouched).
        assert!(n.layer().is_modified());
        assert_eq!(n.layer().diff_for_upload().counts(), (0, 1, 1));
        let UndoableAction::Batch { label, actions } = out.undo.as_ref().unwrap() else {
            panic!("expected one Batch step");
        };
        assert_eq!(label, "Joined 2 ways");
        assert!(matches!(
            actions.as_slice(),
            [
                UndoableAction::SetWayNodes { .. },
                UndoableAction::SetTags { .. },
                UndoableAction::DeleteFeature { .. }
            ]
        ));
    }

    #[test]
    fn join_keeps_the_first_selected_when_several_are_server_ways() {
        let mut n = net();
        n.run("join", &n.ways(&[101, 100])).unwrap();
        assert_eq!(n.editable().way_node_ids(101), Some(vec![1, 2, 3, 4, 5]));
        assert_eq!(n.editable().way_node_ids(100), None);
    }

    #[test]
    fn join_refuses_conflicting_tags_naming_the_key() {
        let mut n = net();
        let Err(message) = n.run("join", &n.ways(&[101, 105])) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Can't join: the ways have different values for 'name'"
        );
        assert!(!n.layer().is_modified());
    }

    #[test]
    fn join_refuses_to_delete_a_relation_member() {
        let mut n = net();
        // 102 would be deleted (101 is first, both are server ways).
        let Err(message) = n.run("join", &n.ways(&[101, 102])) else {
            panic!("expected a refusal");
        };
        assert!(message.contains("member of a relation"), "{message}");
        assert!(!n.layer().is_modified());
        // Selected first, the member way survives and the join works.
        assert!(n.run("join", &n.ways(&[102, 101])).is_ok());
        assert_eq!(n.editable().way_node_ids(102), Some(vec![3, 4, 5, 6]));
    }

    #[test]
    fn join_refuses_to_reverse_a_oneway() {
        let mut n = net();
        let Err(message) = n.run("join", &n.ways(&[106, 107])) else {
            panic!("expected a refusal");
        };
        assert!(message.contains("depend on its direction"), "{message}");
        assert!(!n.layer().is_modified());
    }

    #[test]
    fn separate_gives_the_other_way_a_new_untagged_node_at_the_same_spot() {
        let mut n = net();
        let node4 = n.feature(FeatureKind::Node, 4);
        let out = n.run("separate", &[node4]).unwrap();
        assert_eq!(out.status, "Separated a node into 2 copies");
        // The node stays selected: no selection change requested.
        assert!(out.select.is_none() && !out.clear_selection);

        // Way 101 keeps node 4; 103 got a new negative node.
        assert_eq!(n.editable().way_node_ids(101), Some(vec![3, 4, 5]));
        let moved = n.editable().way_node_ids(103).unwrap();
        assert_eq!((moved[0], moved[2]), (8, 9));
        let new_id = moved[1];
        assert!(new_id < 0);
        assert_eq!(
            n.editable().node_lat_lon(new_id),
            n.editable().node_lat_lon(4)
        );
        assert!(n
            .editable()
            .feature_tags(&n.feature(FeatureKind::Node, new_id))
            .unwrap()
            .is_empty());
        assert_eq!(
            n.editable().feature_tags(&node4).unwrap(),
            vec![("highway".to_string(), "crossing".to_string())]
        );

        // One new node and one changed way show up in the upload diff.
        assert!(n.layer().is_modified());
        assert_eq!(n.layer().diff_for_upload().counts(), (1, 1, 0));
        let UndoableAction::Batch { actions, .. } = out.undo.as_ref().unwrap() else {
            panic!("expected one Batch step");
        };
        assert!(matches!(
            actions.as_slice(),
            [
                UndoableAction::CreateNode { .. },
                UndoableAction::SetWayNodes { .. }
            ]
        ));

        // Nothing left to separate.
        assert!(n.run("separate", &[node4]).is_err());
        assert!(!n.applicable(&[node4]).contains(&"separate"));
    }

    #[test]
    fn separate_with_a_way_selected_detaches_only_that_way() {
        let mut n = net();
        let sel = [
            n.feature(FeatureKind::Node, 4),
            n.feature(FeatureKind::Way, 101),
        ];
        n.run("separate", &sel).unwrap();
        // 103 keeps the original this time; 101 got the new node.
        assert_eq!(n.editable().way_node_ids(103), Some(vec![8, 4, 9]));
        let nodes = n.editable().way_node_ids(101).unwrap();
        assert!(nodes[1] < 0 && nodes[0] == 3 && nodes[2] == 5);
    }

    #[test]
    fn join_and_separate_explain_when_inapplicable() {
        let n = net();
        let none = ActionEnv {
            selection: &[],
            active_layer: Some(n.layer_id),
            layers: &n.layers,
        };
        assert_eq!(
            find("join").unwrap().why_not(&none),
            "Select two or more ways that connect end to end to join them"
        );
        assert_eq!(
            find("separate").unwrap().why_not(&none),
            "Select a node shared by two or more ways to separate it"
        );
        let no_layer = ActionEnv {
            selection: &[],
            active_layer: None,
            layers: &n.layers,
        };
        assert_eq!(
            find("join").unwrap().why_not(&no_layer),
            "Open or download OSM data first"
        );
    }

    #[test]
    fn extend_is_offered_for_one_open_way_or_one_open_end() {
        let n = net();
        let node = |id| n.feature(FeatureKind::Node, id);
        assert!(n.applicable(&n.ways(&[100])).contains(&"extend"));
        // A ring has no end; two ways are ambiguous.
        assert!(!n.applicable(&n.ways(&[108])).contains(&"extend"));
        assert!(!n.applicable(&n.ways(&[100, 101])).contains(&"extend"));
        // Node 1 starts way 100 and node 6 ends way 102.
        assert!(n.applicable(&[node(1)]).contains(&"extend"));
        assert!(n.applicable(&[node(6)]).contains(&"extend"));
        // Middle nodes, nodes that end two ways, and ring nodes: no.
        assert!(!n.applicable(&[node(2)]).contains(&"extend"));
        assert!(!n.applicable(&[node(3)]).contains(&"extend"));
        assert!(!n.applicable(&[node(5)]).contains(&"extend"));
        assert!(!n.applicable(&[node(14)]).contains(&"extend"));
        assert!(!n.applicable(&[]).contains(&"extend"));
    }

    #[test]
    fn extend_changes_no_data_and_points_add_mode_at_the_right_end() {
        let mut n = net();
        let out = n.run("extend", &n.ways(&[100])).unwrap();
        assert!(out.undo.is_none());
        assert_eq!(
            out.continue_way,
            Some(topology::Continuation {
                way_id: 100,
                from_node: 3,
                at_start: false
            })
        );
        assert_eq!(out.select, Some(n.ways(&[100])));

        let first = n.feature(FeatureKind::Node, 1);
        let out = n.run("extend", &[first]).unwrap();
        assert_eq!(
            out.continue_way,
            Some(topology::Continuation {
                way_id: 100,
                from_node: 1,
                at_start: true
            })
        );
        assert!(!n.layer().is_modified());

        let none = ActionEnv {
            selection: &n.ways(&[108]),
            active_layer: Some(n.layer_id),
            layers: &n.layers,
        };
        assert_eq!(
            find("extend").unwrap().why_not(&none),
            "Select an open way, or the end node of one, to continue drawing it"
        );
        assert!(n.run("extend", &n.ways(&[108])).is_err());
    }
}
