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

/// What running an action produced; the caller records `undo` as one step.
pub(crate) struct ActionOutcome {
    pub undo: UndoableAction,
    /// Short status-bar message, e.g. "Squared 1 way".
    pub status: String,
    /// Whether the selection no longer makes sense (e.g. after a delete).
    pub clear_selection: bool,
}

pub(crate) struct SelectionAction {
    pub id: &'static str,
    pub label: &'static str,
    /// Id of this action's customizable shortcut in `keybindings::SHORTCUTS`.
    pub shortcut_id: Option<&'static str>,
    /// Display text of a fixed (non-customizable) shortcut, if any.
    #[allow(dead_code)] // TEMP: read by the context menu (next commit)
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
    #[allow(dead_code)] // TEMP: used by the context menu (next commit)
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
    #[allow(dead_code)] // TEMP: used by the context menu (next commit)
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
#[allow(dead_code)] // TEMP: used by the context menu (next commit)
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
        undo: UndoableAction::MoveNodes {
            per_layer: vec![(layer_id, entries)],
        },
        status: format!("{verb} {}", plural(changed_ways, "way", "ways")),
        clear_selection: false,
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
        undo: UndoableAction::ReverseWays {
            layer: layer_id,
            way_ids: reversed,
        },
        status,
        clear_selection: false,
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
        undo,
        status,
        clear_selection: true,
    })
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
            ["reverse", "delete"]
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
        let UndoableAction::MoveNodes { per_layer } = &out.undo else {
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
        let UndoableAction::MoveNodes { per_layer } = &out.undo else {
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
        let UndoableAction::ReverseWays { way_ids, .. } = &out.undo else {
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
        assert!(matches!(&out.undo, UndoableAction::Batch { actions, .. } if actions.len() == 2));
        assert_eq!(out.undo.description(), "Deleted 2 features");
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
}
