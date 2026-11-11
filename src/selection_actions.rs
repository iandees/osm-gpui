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

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::SharedString;
use osm_gpui::geometry::{self, LatLon};
use osm_gpui::i18n::tr_dynamic;
use osm_gpui::layers::{EditableLayer, LayerId, LayerManager};
use osm_gpui::osm::MemberType;
use osm_gpui::selection::{FeatureKind, FeatureRef};
use osm_gpui::topology::{self, JoinWay};
use osm_gpui::{tr, tr_count};

use crate::undo::{NodeMoveUndoEntry, UndoableAction};

thread_local! {
    /// The tags set aside by Copy Tags, for Paste Tags: the app's own tag
    /// clipboard, kept apart from the system pasteboard. Actions only ever
    /// run on the UI thread, which is why this can be a thread-local (it also
    /// keeps parallel tests from sharing one).
    static TAG_CLIPBOARD: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// The tags currently on the tag clipboard, sorted by key.
pub(crate) fn tag_clipboard() -> Vec<(String, String)> {
    TAG_CLIPBOARD.with(|c| c.borrow().clone())
}

fn set_tag_clipboard(mut tags: Vec<(String, String)>) {
    tags.sort();
    TAG_CLIPBOARD.with(|c| *c.borrow_mut() = tags);
}

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

    /// How many distinct nodes of the active layer are selected.
    fn selected_node_count(&self) -> usize {
        let (Some(layer_id), Some(editable)) = (self.active_layer, self.active_editable()) else {
            return 0;
        };
        merge_nodes_in(editable, layer_id, self.selection).len()
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
        FeatureKind::Relation => None,
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
    /// Stable id; also the catalog key of its texts (`action.<id>.label`,
    /// `action.<id>.hint`).
    pub id: &'static str,
    /// Id of this action's customizable shortcut in `keybindings::SHORTCUTS`.
    pub shortcut_id: Option<&'static str>,
    /// Display text of a fixed (non-customizable) shortcut, if any.
    pub fixed_shortcut: Option<&'static str>,
    /// Whether the action needs an active editable layer to do anything.
    pub needs_active_layer: bool,
    /// Whether the Tools menu lists it (Delete lives in Edit instead).
    pub in_tools_menu: bool,
    pub is_applicable: fn(&ActionEnv) -> bool,
    pub apply: ApplyFn,
}

/// Performs an action on the selection in the active layer (if any). `Err`
/// carries a short message for the status bar and means nothing changed.
pub(crate) type ApplyFn =
    fn(&mut LayerManager, &[FeatureRef], Option<LayerId>) -> Result<ActionOutcome, String>;

/// Id of the "Add to Relation…" action, which opens a picker rather than
/// applying right away.
pub(crate) const ADD_TO_RELATION: &str = "add_to_relation";

/// Id of the "Create Parallel Way…" action, which asks for the offset in a
/// dialog first (see `create_parallel_ways`).
pub(crate) const PARALLEL: &str = "parallel";

pub(crate) const ACTIONS: &[SelectionAction] = &[
    SelectionAction {
        id: "square",
        shortcut_id: Some("action_square"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| {
            env.selected_ways()
                .iter()
                .any(|w| geometry::can_square(&w.points))
        },
        apply: apply_square,
    },
    SelectionAction {
        id: "circularize",
        shortcut_id: Some("action_circularize"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| {
            env.selected_ways()
                .iter()
                .any(|w| geometry::can_circularize(&w.points))
        },
        apply: apply_circularize,
    },
    SelectionAction {
        id: "reverse",
        shortcut_id: Some("action_reverse"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| env.selected_ways().iter().any(|w| w.node_ids.len() >= 2),
        apply: apply_reverse,
    },
    SelectionAction {
        id: "join",
        shortcut_id: Some("action_join"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| topology::can_chain(&env.selected_join_ways()),
        apply: apply_join,
    },
    SelectionAction {
        id: "separate",
        shortcut_id: Some("action_separate"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| env.can_separate_selection(),
        apply: apply_separate,
    },
    SelectionAction {
        id: "extend",
        shortcut_id: Some("action_extend"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| env.extend_target().is_some(),
        apply: apply_extend,
    },
    SelectionAction {
        id: "split",
        shortcut_id: Some("action_split"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| {
            let (Some(layer_id), Some(editable)) = (env.active_layer, env.active_editable()) else {
                return false;
            };
            !split_plans(editable, layer_id, env.selection).is_empty()
        },
        apply: apply_split,
    },
    SelectionAction {
        id: "merge",
        shortcut_id: Some("action_merge"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| env.selected_node_count() >= 2,
        apply: apply_merge,
    },
    SelectionAction {
        id: "straighten",
        shortcut_id: Some("action_straighten"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| {
            env.selected_ways()
                .iter()
                .any(|w| geometry::can_straighten(&w.points))
        },
        apply: apply_straighten,
    },
    // Copy, Paste, Paste at Source Position and Duplicate need the pointer
    // position and the window's clipboard, so `MapViewer::run_selection_action`
    // hands them to `clipboard_actions` instead of calling `apply`.
    SelectionAction {
        id: "copy_features",
        shortcut_id: Some("copy_features"),
        fixed_shortcut: None,
        needs_active_layer: false,
        in_tools_menu: false,
        is_applicable: crate::clipboard_actions::can_copy,
        apply: crate::clipboard_actions::apply_elsewhere,
    },
    SelectionAction {
        id: "paste_features",
        shortcut_id: Some("paste_features"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: false,
        is_applicable: crate::clipboard_actions::can_paste,
        apply: crate::clipboard_actions::apply_elsewhere,
    },
    SelectionAction {
        id: "paste_features_source",
        shortcut_id: None,
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: false,
        is_applicable: crate::clipboard_actions::can_paste,
        apply: crate::clipboard_actions::apply_elsewhere,
    },
    SelectionAction {
        id: "duplicate_features",
        shortcut_id: Some("duplicate_features"),
        fixed_shortcut: None,
        needs_active_layer: false,
        in_tools_menu: false,
        is_applicable: crate::clipboard_actions::can_copy,
        apply: crate::clipboard_actions::apply_elsewhere,
    },
    SelectionAction {
        id: "align",
        shortcut_id: Some("action_align"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| env.selected_node_count() >= 3,
        apply: apply_align,
    },
    SelectionAction {
        id: "join_node_way",
        shortcut_id: None,
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| {
            let (Some(layer_id), Some(editable)) = (env.active_layer, env.active_editable()) else {
                return false;
            };
            join_node_way_target(editable, layer_id, env.selection).is_some()
        },
        apply: apply_join_node_way,
    },
    SelectionAction {
        id: "distribute",
        shortcut_id: None,
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| env.selected_node_count() >= 3,
        apply: apply_distribute,
    },
    SelectionAction {
        id: PARALLEL,
        shortcut_id: None,
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| env.selected_ways().iter().any(|w| w.node_ids.len() >= 2),
        // Needs the offset first, which takes a dialog: see
        // `MapViewer::run_selection_action`, which opens it instead of
        // applying, and `create_parallel_ways` below, which applies the answer.
        apply: |_, _, _| Err(tr!("action.parallel.hint").to_string()),
    },
    SelectionAction {
        id: "copy_tags",
        shortcut_id: Some("copy_tags"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: false,
        is_applicable: |env| {
            let (Some(layer_id), Some(editable)) = (env.active_layer, env.active_editable()) else {
                return false;
            };
            copy_source(editable, layer_id, env.selection).is_some()
        },
        apply: apply_copy_tags,
    },
    SelectionAction {
        id: "paste_tags",
        shortcut_id: Some("paste_tags"),
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: false,
        is_applicable: |env| {
            env.active_layer.is_some_and(|layer_id| {
                env.active_editable().is_some()
                    && !tag_clipboard().is_empty()
                    && !paste_targets(layer_id, env.selection).is_empty()
            })
        },
        apply: apply_paste_tags,
    },
    SelectionAction {
        id: ADD_TO_RELATION,
        shortcut_id: None,
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| {
            let (Some(layer_id), Some(editable)) = (env.active_layer, env.active_editable()) else {
                return false;
            };
            !member_candidates(editable, layer_id, env.selection).is_empty()
                && !editable.relation_ids().is_empty()
        },
        // Needs a relation chosen first, which takes a dialog: see
        // `MapViewer::run_selection_action`, which opens the picker instead
        // of applying, and `add_to_relation` below, which applies the choice.
        apply: |_, _, _| Err(tr!("action.add_to_relation.hint").to_string()),
    },
    SelectionAction {
        id: "new_relation",
        shortcut_id: None,
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        is_applicable: |env| {
            let (Some(layer_id), Some(editable)) = (env.active_layer, env.active_editable()) else {
                return false;
            };
            !relation_member_candidates(editable, layer_id, env.selection).is_empty()
        },
        apply: apply_new_relation,
    },
    SelectionAction {
        id: "create_multipolygon",
        shortcut_id: None,
        fixed_shortcut: None,
        needs_active_layer: true,
        in_tools_menu: true,
        // A lone open way can never close into a ring.
        is_applicable: |env| {
            let ways = env.selected_ways();
            ways.len() >= 2
                || ways
                    .iter()
                    .any(|w| w.node_ids.len() >= 4 && w.node_ids.first() == w.node_ids.last())
        },
        apply: apply_create_multipolygon,
    },
    SelectionAction {
        id: "purge",
        shortcut_id: None,
        fixed_shortcut: None,
        needs_active_layer: false,
        in_tools_menu: true,
        is_applicable: |env| {
            env.selection.iter().any(|f| {
                env.layers
                    .find_layer(f.layer_id)
                    .is_some_and(|l| l.as_editable().is_some())
            })
        },
        apply: apply_purge,
    },
    SelectionAction {
        id: "delete",
        shortcut_id: None,
        fixed_shortcut: Some("⌫"),
        needs_active_layer: false,
        in_tools_menu: false,
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
        a.shortcut_id.is_some_and(|sid| {
            // Only the bare-key shortcuts; Copy/Paste Tags are key bindings.
            osm_gpui::keybindings::def(sid)
                .category
                .is_bare_key_category()
                && osm_gpui::keybindings::effective_spec(settings, sid) == key
        })
    })
}

impl SelectionAction {
    /// The actions applicable to `env`, in table order.
    pub(crate) fn applicable<'a>(
        env: &'a ActionEnv<'a>,
    ) -> impl Iterator<Item = &'static Self> + 'a {
        ACTIONS.iter().filter(|a| (a.is_applicable)(env))
    }

    /// Localized menu label, e.g. "Square".
    pub(crate) fn label(&self) -> SharedString {
        tr_dynamic(&format!("action.{}.label", self.id))
    }

    /// Localized status-bar message when run with an unsuitable selection.
    pub(crate) fn hint(&self) -> SharedString {
        tr_dynamic(&format!("action.{}.hint", self.id))
    }

    /// Why this action can't run with `env`, for the status bar.
    pub(crate) fn why_not(&self, env: &ActionEnv) -> SharedString {
        if self.needs_active_layer && env.active_editable().is_none() {
            tr!("action.no_data")
        } else {
            self.hint()
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
/// as in menus ("q" -> "Q"). Modifiers follow the running platform's
/// conventions (see [`display_shortcut_for`]).
pub(crate) fn display_shortcut(spec: &str) -> String {
    display_shortcut_for(spec, cfg!(target_os = "macos"))
}

/// [`display_shortcut`] for an explicit platform. On macOS the modifiers show
/// as symbols in the order macOS menus use ("cmd-shift-c" -> "⇧⌘C") and
/// `secondary` is Cmd. Elsewhere they are spelled out in the usual
/// Windows/Linux order ("secondary-shift-c" -> "Ctrl+Shift+C"), `secondary`
/// is Ctrl, and `cmd` is the Windows/Super key.
pub(crate) fn display_shortcut_for(spec: &str, mac: bool) -> String {
    // Slots: ctrl, alt, shift, platform (Cmd / Win key).
    const PREFIXES: [(&str, Option<usize>); 7] = [
        ("ctrl-", Some(0)),
        ("alt-", Some(1)),
        ("shift-", Some(2)),
        ("cmd-", Some(3)),
        ("super-", Some(3)),
        ("win-", Some(3)),
        ("secondary-", None),
    ];
    let mut held = [false; 4];
    let mut key = spec;
    'strip: loop {
        for (prefix, slot) in PREFIXES {
            if let Some(rest) = key.strip_prefix(prefix) {
                let slot = slot.unwrap_or(if mac { 3 } else { 0 });
                held[slot] = true;
                key = rest;
                continue 'strip;
            }
        }
        break;
    }
    let key = match key {
        "backspace" | "delete" => "⌫".to_string(),
        k if k.chars().count() == 1 => k.to_uppercase(),
        k => k.to_string(),
    };
    let labels: [&str; 4] = if mac {
        ["⌃", "⌥", "⇧", "⌘"]
    } else {
        ["Ctrl+", "Alt+", "Shift+", "Win+"]
    };
    let modifiers: String = labels
        .iter()
        .zip(held)
        .filter(|(_, held)| *held)
        .map(|(label, _)| *label)
        .collect();
    format!("{modifiers}{key}")
}

fn active_editable_mut(
    layers: &mut LayerManager,
    active: Option<LayerId>,
) -> Result<(LayerId, &mut dyn EditableLayer), String> {
    let id = active.ok_or_else(|| tr!("action.no_data").to_string())?;
    let editable = layers
        .find_layer_mut(id)
        .and_then(|l| l.as_editable_mut())
        .ok_or_else(|| tr!("action.not_editable").to_string())?;
    Ok((id, editable))
}

/// Computes new positions for a way's points; see `reshape_ways`.
type ReshapeFn = fn(&[LatLon]) -> Result<Option<Vec<LatLon>>, String>;

/// Move nodes by running `reshape` over each selected way's positions.
/// `reshape` returns new positions for every point of the way, `Ok(None)` to
/// leave the way alone, or `Err` with a message to refuse the whole action.
/// Nodes shared between selected ways see each other's moves, and the whole
/// thing is recorded as one `MoveNodes` step.
fn reshape_ways(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
    reshape: ReshapeFn,
    none_changed: fn() -> SharedString,
    done: fn(usize) -> SharedString,
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
        let Some(new_points) = reshape(&points)? else {
            continue;
        };
        for ((&id, &old), new) in way.node_ids.iter().zip(&way.points).zip(new_points) {
            original.entry(id).or_insert(old);
            current.insert(id, new);
        }
        changed_ways += 1;
    }
    if changed_ways == 0 {
        return Err(none_changed().to_string());
    }

    let mut entries: Vec<NodeMoveUndoEntry> = current
        .iter()
        .filter(|(id, after)| original[id] != **after)
        .map(|(&id, &after)| (id, original[&id], after))
        .collect();
    entries.sort_by_key(|e| e.0);
    if entries.is_empty() {
        return Err(none_changed().to_string());
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
        status: done(changed_ways).to_string(),
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
        |points| Ok(geometry::orthogonalize(points)),
        || tr!("action.square.none"),
        |n| tr_count!("action.square.done", n),
    )
}

/// Straighten the selected open ways (iD "Straighten"): see
/// `geometry::straighten`. A way that bends more than
/// `geometry::STRAIGHTEN_MAX_DEVIATION` of its length refuses the whole
/// action, so a curved road is never flattened by accident.
fn apply_straighten(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    reshape_ways(
        layers,
        selection,
        active,
        |points| match geometry::straighten(points) {
            Ok(straight) => Ok(Some(straight)),
            Err(geometry::StraightenError::NotApplicable) => Ok(None),
            Err(geometry::StraightenError::NoLine) => {
                Err(tr!("action.straighten.error.no_line").to_string())
            }
            Err(geometry::StraightenError::TooCurved) => Err(tr!(
                "action.straighten.error.too_curved",
                percent = (geometry::STRAIGHTEN_MAX_DEVIATION * 100.0).round()
            )
            .to_string()),
        },
        || tr!("action.straighten.none"),
        |n| tr_count!("action.straighten.done", n),
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
        |points| Ok(geometry::circularize(points)),
        || tr!("action.circularize.hint"),
        |n| tr_count!("action.circularize.done", n),
    )
}

/// The most characters the server allows in a tag key or value.
const MAX_TAG_CHARS: usize = 255;

fn apply_reverse(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let ways = selected_ways_in(editable, layer_id, selection);
    // Renaming `:forward` to `:backward` or negating an incline can push a
    // key or value past the server's limit; refuse before changing anything.
    let too_long = |changes: Vec<topology::TagChange>| {
        changes.iter().any(|(key, _, after)| {
            after.is_some()
                && (key.chars().count() > MAX_TAG_CHARS
                    || after
                        .as_ref()
                        .is_some_and(|v| v.chars().count() > MAX_TAG_CHARS))
        })
    };
    for way in ways.iter().filter(|w| w.node_ids.len() >= 2) {
        let way_ref = FeatureRef {
            layer_id,
            kind: FeatureKind::Way,
            id: way.way_id,
        };
        let mut over = too_long(topology::reversed_tag_changes(
            &editable.feature_tags(&way_ref).unwrap_or_default(),
        ));
        for &node_id in &way.node_ids {
            let node_ref = FeatureRef {
                layer_id,
                kind: FeatureKind::Node,
                id: node_id,
            };
            over |= too_long(topology::reversed_node_tag_changes(
                &editable.feature_tags(&node_ref).unwrap_or_default(),
            ));
        }
        if over {
            return Err(tr!("action.reverse.error.tag_too_long", way = way.way_id).to_string());
        }
    }
    let mut reversed = Vec::new();
    let mut tag_entries = Vec::new();
    for way in ways.iter().filter(|w| w.node_ids.len() >= 2) {
        if !editable.reverse_way(way.way_id) {
            continue;
        }
        reversed.push(way.way_id);
        // Keep direction-dependent tags meaning the same thing.
        let way_ref = FeatureRef {
            layer_id,
            kind: FeatureKind::Way,
            id: way.way_id,
        };
        let tags = editable.feature_tags(&way_ref).unwrap_or_default();
        for (key, before, after) in topology::reversed_tag_changes(&tags) {
            match &after {
                Some(value) => editable.set_tag(FeatureKind::Way, way.way_id, &key, value),
                None => editable.remove_tag(FeatureKind::Way, way.way_id, &key),
            }
            tag_entries.push((way_ref, key, before, after));
        }
    }
    if reversed.is_empty() {
        return Err(tr!("action.reverse.hint").to_string());
    }
    // Facing tags on the ways' nodes (a stop sign's `direction=forward`,
    // `traffic_sign:forward`) flip too. A node shared by several reversed ways
    // is flipped once; a node that is also used by a way that is not being
    // reversed keeps its tags, since it still faces the right way for that way.
    let mut seen_nodes = std::collections::HashSet::new();
    for way in ways.iter().filter(|w| reversed.contains(&w.way_id)) {
        for &node_id in &way.node_ids {
            if !seen_nodes.insert(node_id) {
                continue;
            }
            if editable
                .ways_using_node(node_id)
                .iter()
                .any(|w| !reversed.contains(w))
            {
                continue;
            }
            let node_ref = FeatureRef {
                layer_id,
                kind: FeatureKind::Node,
                id: node_id,
            };
            let tags = editable.feature_tags(&node_ref).unwrap_or_default();
            for (key, before, after) in topology::reversed_node_tag_changes(&tags) {
                match &after {
                    Some(value) => editable.set_tag(FeatureKind::Node, node_id, &key, value),
                    None => editable.remove_tag(FeatureKind::Node, node_id, &key),
                }
                tag_entries.push((node_ref, key, before, after));
            }
        }
    }
    // Route relations: a member's `forward`/`backward` role swaps with its way.
    let mut relation_edits = Vec::new();
    let mut relation_ids: Vec<i64> = reversed
        .iter()
        .flat_map(|id| editable.relations_containing_way(*id))
        .collect();
    relation_ids.sort_unstable();
    relation_ids.dedup();
    for relation_id in relation_ids {
        let Some(relation) = editable.relation(relation_id) else {
            continue;
        };
        let mut after = relation.members.clone();
        let mut changed = false;
        for member in after.iter_mut() {
            if member.member_type == osm_gpui::osm::MemberType::Way
                && reversed.contains(&member.reference)
            {
                let flipped = match member.role.as_str() {
                    "forward" => "backward",
                    "backward" => "forward",
                    _ => continue,
                };
                member.role = flipped.to_string();
                changed = true;
            }
        }
        if changed {
            editable.set_relation_members(relation_id, after.clone());
            relation_edits.push(UndoableAction::SetRelationMembers {
                layer: layer_id,
                relation_id,
                before: relation.members,
                after,
            });
        }
    }
    let status = tr_count!("action.reverse.done", reversed.len()).to_string();
    let reverse = UndoableAction::ReverseWays {
        layer: layer_id,
        way_ids: reversed,
    };
    // The node order, the tag flips and the role swaps are one undo step.
    let undo = if tag_entries.is_empty() && relation_edits.is_empty() {
        reverse
    } else {
        let mut actions = vec![reverse];
        if !tag_entries.is_empty() {
            actions.push(UndoableAction::SetTags {
                entries: tag_entries,
            });
        }
        actions.extend(relation_edits);
        UndoableAction::Batch {
            label: status.clone(),
            actions,
        }
    };
    Ok(ActionOutcome {
        undo: Some(undo),
        status,
        clear_selection: false,
        select: None,
        continue_way: None,
    })
}

/// Join the selected ways into one (JOSM "Combine Way"). See
/// `topology::plan_join` for the chaining, survivor and tag rules. A removed
/// way that is a relation member is folded into the survivor's membership (see
/// `relations::join_members`; roles that differ, and turn restrictions, refuse
/// the join). One undo step: the survivor's new nodes and tags, the relations'
/// new member lists, then the deletion of the rest.
fn apply_join(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let ways = join_ways_in(editable, layer_id, selection);
    let plan = topology::plan_join(&ways).map_err(|e| e.message())?;
    if let Some(m) =
        osm_gpui::limits::way_over_limit(plan.nodes.len(), osm_gpui::limits::join_message)
    {
        return Err(m);
    }
    // Work out the relations' new member lists before changing anything, so
    // a refusal leaves no trace.
    let mut relation_ids: Vec<i64> = plan
        .remove
        .iter()
        .flat_map(|&id| editable.relations_containing_way(id))
        .collect();
    relation_ids.sort_unstable();
    relation_ids.dedup();
    let mut relation_edits = Vec::new();
    for relation_id in relation_ids {
        let Some(relation) = editable.relation(relation_id) else {
            continue;
        };
        let after = osm_gpui::relations::join_members(&relation, plan.keep, &plan.remove)
            .map_err(|e| e.message())?;
        if let Some(after) = after {
            relation_edits.push((relation_id, relation.members, after));
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
    for (relation_id, before, after) in relation_edits {
        editable.set_relation_members(relation_id, after.clone());
        actions.push(UndoableAction::SetRelationMembers {
            layer: layer_id,
            relation_id,
            before,
            after,
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

    let label = tr_count!("action.join.done", ways.len()).to_string();
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
    let hint = || tr!("action.separate.hint").to_string();
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let (node, detach) = separate_target(layer_id, selection).ok_or_else(hint)?;
    let (lat, lon) = editable
        .node_lat_lon(node)
        .ok_or_else(|| tr!("action.separate.node_missing").to_string())?;
    let ways = ways_using(editable, node);

    // A turn restriction needs its `from` and `to` ways to touch the `via`
    // node. Dry-run the plan with throwaway ids, and refuse before anything
    // is created if a way it detaches is such a member.
    let mut fake = i64::MIN;
    let dry = topology::plan_separate(node, &ways, detach, || {
        fake += 1;
        fake
    })
    .ok_or_else(hint)?;
    for relation_id in editable.relations_containing_node(node) {
        let Some(relation) = editable.relation(relation_id) else {
            continue;
        };
        if !osm_gpui::relations::is_restriction(&relation)
            || !relation.members.iter().any(|m| {
                m.member_type == MemberType::Node && m.reference == node && m.role == "via"
            })
        {
            continue;
        }
        for edit in &dry {
            let detached = !edit.after.contains(&node);
            let is_end = relation.members.iter().any(|m| {
                m.member_type == MemberType::Way
                    && m.reference == edit.way_id
                    && (m.role == "from" || m.role == "to")
            });
            if detached && is_end {
                return Err(tr!(
                    "action.separate.error.restriction",
                    way = edit.way_id,
                    relation = relation_id
                )
                .to_string());
            }
        }
    }

    let mut created = Vec::new();
    let edits = topology::plan_separate(node, &ways, detach, || {
        let id = editable
            .create_node(lat, lon, None)
            .expect("the layer has data, since the node exists");
        created.push(id);
        id
    })
    .ok_or_else(hint)?;

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

    let label = tr_count!("action.separate.done", created.len() + 1).to_string();
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

/// The tags of the one selected feature, if the selection is exactly one
/// feature of the active layer and it has tags.
fn copy_source(
    editable: &dyn EditableLayer,
    layer_id: LayerId,
    selection: &[FeatureRef],
) -> Option<Vec<(String, String)>> {
    let [feature] = selection else {
        return None;
    };
    if feature.layer_id != layer_id {
        return None;
    }
    editable
        .feature_tags(feature)
        .filter(|tags| !tags.is_empty())
}

/// The distinct selected features that live in the active layer.
fn paste_targets(layer_id: LayerId, selection: &[FeatureRef]) -> Vec<FeatureRef> {
    let mut targets: Vec<FeatureRef> = Vec::new();
    for f in selection.iter().filter(|f| f.layer_id == layer_id) {
        if !targets.contains(f) {
            targets.push(*f);
        }
    }
    targets
}

/// Remember the tags of the one selected feature on the tag clipboard.
fn apply_copy_tags(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let tags = copy_source(editable, layer_id, selection)
        .ok_or_else(|| tr!("action.copy_tags.hint").to_string())?;
    let status = tr!(
        "action.copy_tags.done",
        tags = tr_count!("action.tag_count", tags.len())
    )
    .to_string();
    set_tag_clipboard(tags);
    Ok(ActionOutcome {
        undo: None,
        status,
        clear_selection: false,
        select: None,
        continue_way: None,
    })
}

/// Apply the tag clipboard to every selected feature of the active layer:
/// the copied keys overwrite existing values, other tags stay. One `SetTags`
/// undo step covering every change.
fn apply_paste_tags(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let clipboard = tag_clipboard();
    if clipboard.is_empty() {
        return Err(tr!("action.paste_tags.hint").to_string());
    }
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let targets = paste_targets(layer_id, selection);
    if targets.is_empty() {
        return Err(tr!("action.paste_tags.hint").to_string());
    }

    let mut entries = Vec::new();
    let mut changed_features = 0usize;
    for feature in &targets {
        let Some(existing) = editable.feature_tags(feature) else {
            continue;
        };
        let before = entries.len();
        for (key, value) in &clipboard {
            let old = existing.iter().find(|(k, _)| k == key).map(|(_, v)| v);
            if old == Some(value) {
                continue;
            }
            editable.set_tag(feature.kind, feature.id, key, value);
            entries.push((*feature, key.clone(), old.cloned(), Some(value.clone())));
        }
        changed_features += usize::from(entries.len() > before);
    }
    if entries.is_empty() {
        return Err(tr!("action.paste_tags.none").to_string());
    }
    let status = tr!(
        "action.paste_tags.done",
        tags = tr_count!("action.tag_count", clipboard.len()),
        features = tr_count!("action.feature_count", changed_features)
    )
    .to_string();
    Ok(ActionOutcome {
        undo: Some(UndoableAction::SetTags { entries }),
        status,
        clear_selection: false,
        select: None,
        continue_way: None,
    })
}

/// What Split Way would do for the selection: one or more nodes of the active
/// layer, plus optionally one way to restrict it to.
/// Empty unless the selection has that shape and something can be split.
fn split_plans(
    editable: &dyn EditableLayer,
    layer_id: LayerId,
    selection: &[FeatureRef],
) -> Vec<topology::SplitPlan> {
    let in_layer = |kind| {
        let mut ids: Vec<i64> = Vec::new();
        for f in selection
            .iter()
            .filter(|f| f.layer_id == layer_id && f.kind == kind)
        {
            if !ids.contains(&f.id) {
                ids.push(f.id);
            }
        }
        ids
    };
    let nodes = in_layer(FeatureKind::Node);
    let selected_ways = in_layer(FeatureKind::Way);
    if nodes.is_empty() || selected_ways.len() > 1 {
        return Vec::new();
    }
    let mut ways: Vec<(i64, Vec<i64>)> = Vec::new();
    for &node in &nodes {
        for way in ways_using(editable, node) {
            if !ways.iter().any(|(id, _)| *id == way.0) {
                ways.push(way);
            }
        }
    }
    topology::plan_split(&nodes, selected_ways.first().copied(), &ways)
}

/// Split the ways at the selected node(s) (JOSM "Split Way"): the original
/// keeps its id and the longest part, every other part is a new way with a
/// copy of the tags, and neighbouring parts share the split node. A closed
/// way with one node is only rewritten to start there. Relations that hold the way keep
/// working: see `relations::split_members` for where the new way goes (and
/// the turn restriction exception, which can refuse the split). One undo
/// step: per way, the new way's creation, then the original's new nodes, and
/// finally the relations' new member lists.
fn apply_split(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let plans = split_plans(editable, layer_id, selection);
    if plans.is_empty() {
        return Err(tr!("action.split.hint").to_string());
    }
    // Work out every relation's new member list before changing anything, so
    // a refusal leaves no trace. The new ways don't exist yet; each stands in
    // under a placeholder id that is swapped for the real one below.
    let mut relations: Vec<osm_gpui::osm::OsmRelation> = Vec::new();
    let mut way_nodes: HashMap<i64, Vec<i64>> = HashMap::new();
    // Per plan: the id each of its ways stands under (the original, then a
    // placeholder per new way, in the order the cuts create them).
    let mut placeholders: Vec<i64> = Vec::new();
    let mut ids_by_plan: Vec<Vec<i64>> = Vec::new();
    for plan in plans.iter() {
        let cuts = plan.cuts();
        let mut ids = vec![plan.way_id];
        for _ in &cuts {
            let id = i64::MIN + placeholders.len() as i64;
            placeholders.push(id);
            ids.push(id);
        }
        // A part cut off more than once is several steps of one split: each
        // is worked out as if the user had split at one node at a time.
        for (i, cut) in cuts.iter().enumerate() {
            let step = osm_gpui::relations::SplitStep {
                way_id: ids[cut.holder],
                new_id: ids[i + 1],
                before: &cut.before,
                keep: &cut.keep,
                new: &cut.new,
            };
            for relation_id in editable.relations_containing_way(plan.way_id) {
                let at = match relations.iter().position(|r| r.id == relation_id) {
                    Some(at) => at,
                    None => {
                        let Some(relation) = editable.relation(relation_id) else {
                            continue;
                        };
                        relations.push(relation);
                        relations.len() - 1
                    }
                };
                let lookup = |id: i64| {
                    way_nodes
                        .get(&id)
                        .cloned()
                        .or_else(|| editable.way_node_ids(id))
                };
                let members = osm_gpui::relations::split_members(&relations[at], &step, &lookup)
                    .map_err(|e| e.message())?;
                if let Some(members) = members {
                    relations[at].members = members;
                }
            }
            way_nodes.insert(step.way_id, cut.keep.clone());
            way_nodes.insert(step.new_id, cut.new.clone());
        }
        ids_by_plan.push(ids);
    }

    // The new halves are added to the relations the way is in; refuse if that
    // would push one past the server's member maximum (nothing is changed yet).
    for relation in &relations {
        let grew = editable
            .relation(relation.id)
            .is_some_and(|r| relation.members.len() > r.members.len());
        if grew {
            if let Some(m) = osm_gpui::limits::relation_over_limit(
                relation.members.len(),
                osm_gpui::limits::relation_add_message,
            ) {
                return Err(m);
            }
        }
    }

    let mut actions = Vec::new();
    let mut new_ids = Vec::new();
    for (plan, ids) in plans.iter().zip(&ids_by_plan) {
        let way_ref = FeatureRef {
            layer_id,
            kind: FeatureKind::Way,
            id: plan.way_id,
        };
        let tags = editable.feature_tags(&way_ref).unwrap_or_default();
        // Where every way of this plan ends up, replaying its cuts.
        let mut finals = vec![plan.before.clone()];
        for cut in plan.cuts() {
            finals[cut.holder] = cut.keep;
            finals.push(cut.new);
        }
        for (k, nodes) in finals.iter().enumerate().skip(1) {
            let new_id = editable.add_way(nodes.clone(), tags.clone());
            new_ids.push((ids[k], new_id));
            actions.push(UndoableAction::CreateWay {
                layer: layer_id,
                snapshot: osm_gpui::selection::DeletedFeatureSnapshot {
                    kind: FeatureKind::Way,
                    id: new_id,
                    tags: tags.clone(),
                    way_nodes: nodes.clone(),
                    node_lat_lon: None,
                    version: 0,
                },
            });
        }
        if plan.cuts().is_empty() {
            // A closed way only turned to start at the node.
            finals[0] = plan.keep().to_vec();
        }
        editable.set_way_nodes(plan.way_id, finals[0].clone());
        actions.push(UndoableAction::SetWayNodes {
            layer: layer_id,
            way_id: plan.way_id,
            before: plan.before.clone(),
            after: finals[0].clone(),
        });
    }
    for relation in relations {
        let Some(before) = editable.relation(relation.id).map(|r| r.members) else {
            continue;
        };
        let mut after = relation.members;
        for member in &mut after {
            if let Some(&(_, real)) = new_ids.iter().find(|(p, _)| *p == member.reference) {
                member.reference = real;
            }
        }
        if after != before {
            editable.set_relation_members(relation.id, after.clone());
            actions.push(UndoableAction::SetRelationMembers {
                layer: layer_id,
                relation_id: relation.id,
                before,
                after,
            });
        }
    }

    let label = tr_count!("action.split.done", plans.len()).to_string();
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

/// The distinct nodes of the active layer that are selected, in selection
/// order, with what the merge planner needs to know about each.
fn merge_nodes_in(
    editable: &dyn EditableLayer,
    layer_id: LayerId,
    selection: &[FeatureRef],
) -> Vec<topology::MergeNode> {
    let mut nodes: Vec<topology::MergeNode> = Vec::new();
    for f in selection
        .iter()
        .filter(|f| f.layer_id == layer_id && f.kind == FeatureKind::Node)
    {
        if nodes.iter().any(|n| n.id == f.id) || editable.node_lat_lon(f.id).is_none() {
            continue;
        }
        nodes.push(topology::MergeNode {
            id: f.id,
            tags: editable.feature_tags(f).unwrap_or_default(),
        });
    }
    nodes
}

/// Merge the selected nodes into one (JOSM "Merge Nodes"). See
/// `topology::plan_merge_nodes` for the survivor, tag, way-rewrite and refusal
/// rules. Relations that have a removed node as a member get the survivor
/// instead (see `relations::merge_node_members`). One undo step: the
/// survivor's added tags, the rewritten ways, the relations' new member lists,
/// then the deletion of the other nodes.
pub(crate) fn apply_merge(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let nodes = merge_nodes_in(editable, layer_id, selection);
    let mut ways: Vec<(i64, Vec<i64>)> = Vec::new();
    for node in &nodes {
        for way in ways_using(editable, node.id) {
            if !ways.iter().any(|(id, _)| *id == way.0) {
                ways.push(way);
            }
        }
    }
    let plan = topology::plan_merge_nodes(&nodes, &ways).map_err(|e| e.message())?;
    let node_ref = |id| FeatureRef {
        layer_id,
        kind: FeatureKind::Node,
        id,
    };

    let mut actions = Vec::new();
    let kept = nodes
        .iter()
        .find(|n| n.id == plan.keep)
        .expect("the plan keeps one of the given nodes");
    let mut tag_entries = Vec::new();
    for (key, value) in &plan.tags {
        if !kept.tags.iter().any(|(k, _)| k == key) {
            editable.set_tag(FeatureKind::Node, plan.keep, key, value);
            tag_entries.push((node_ref(plan.keep), key.clone(), None, Some(value.clone())));
        }
    }
    if !tag_entries.is_empty() {
        actions.push(UndoableAction::SetTags {
            entries: tag_entries,
        });
    }
    for edit in plan.way_edits {
        editable.set_way_nodes(edit.way_id, edit.after.clone());
        actions.push(UndoableAction::SetWayNodes {
            layer: layer_id,
            way_id: edit.way_id,
            before: edit.before,
            after: edit.after,
        });
    }
    let mut relation_ids: Vec<i64> = plan
        .remove
        .iter()
        .flat_map(|&id| editable.relations_containing_node(id))
        .collect();
    relation_ids.sort_unstable();
    relation_ids.dedup();
    for relation_id in relation_ids {
        let Some(relation) = editable.relation(relation_id) else {
            continue;
        };
        if let Some(after) =
            osm_gpui::relations::merge_node_members(&relation.members, &plan.remove, plan.keep)
        {
            editable.set_relation_members(relation_id, after.clone());
            actions.push(UndoableAction::SetRelationMembers {
                layer: layer_id,
                relation_id,
                before: relation.members,
                after,
            });
        }
    }
    for id in &plan.remove {
        if let Some(snapshot) = editable.delete_feature(FeatureKind::Node, *id) {
            actions.push(UndoableAction::DeleteFeature {
                layer: layer_id,
                snapshot,
            });
        }
    }

    let label = tr_count!("action.merge.done", nodes.len()).to_string();
    Ok(ActionOutcome {
        undo: Some(UndoableAction::Batch {
            label: label.clone(),
            actions,
        }),
        status: label,
        clear_selection: false,
        select: Some(vec![node_ref(plan.keep)]),
        continue_way: None,
    })
}

/// The node and the way to join: exactly one node and one way of the active
/// layer selected, both present and the way having a segment. (A way that
/// already contains the node is refused with its own message when run.)
fn join_node_way_target(
    editable: &dyn EditableLayer,
    layer_id: LayerId,
    selection: &[FeatureRef],
) -> Option<(i64, i64)> {
    let (node, way) = separate_target(layer_id, selection)?;
    let way = way?;
    editable.node_lat_lon(node)?;
    let nodes = editable.way_node_ids(way)?;
    (nodes.len() >= 2).then_some((node, way))
}

/// Insert `node` into `way` as a new vertex, moving it onto the way's line.
/// `segment` picks the segment (the one starting at that index); `None`
/// takes the nearest. Used by Join Node to Way and by snapping on a drag.
/// Checks the way-size limit and that the node is not already in the way, and
/// changes nothing on refusal. Returns the undo steps (a move, then the way's
/// new node list); relation memberships are not touched, so a node that is a
/// relation member stays one and relations on the way are unchanged.
pub(crate) fn join_node_into_way(
    editable: &mut dyn EditableLayer,
    layer_id: LayerId,
    node: i64,
    way: i64,
    segment: Option<usize>,
) -> Result<Vec<UndoableAction>, String> {
    use osm_gpui::snap::{plan_join_at_segment, plan_join_nearest, JoinError};
    let position = editable
        .node_lat_lon(node)
        .ok_or_else(|| tr!("action.join_node_way.hint").to_string())?;
    let before = editable
        .way_node_ids(way)
        .ok_or_else(|| tr!("action.join_node_way.hint").to_string())?;
    let points: Vec<LatLon> = before
        .iter()
        .map(|&id| editable.node_lat_lon(id))
        .collect::<Option<_>>()
        .ok_or_else(|| tr!("action.join_node_way.hint").to_string())?;
    let plan = match segment {
        Some(i) => plan_join_at_segment(node, position, &before, &points, i),
        None => plan_join_nearest(node, position, &before, &points),
    }
    .map_err(|e| match e {
        JoinError::AlreadyInWay => tr!("action.join_node_way.error.in_way").to_string(),
        JoinError::NoSegment | JoinError::BadSegment => {
            tr!("action.join_node_way.hint").to_string()
        }
    })?;
    if let Some(message) =
        osm_gpui::limits::way_over_limit(before.len() + 1, osm_gpui::limits::way_full_message)
    {
        return Err(message);
    }
    let mut actions = Vec::new();
    if plan.point != position {
        editable.commit_node_moves(&[(node, plan.point.0, plan.point.1)]);
        actions.push(UndoableAction::MoveNodes {
            per_layer: vec![(layer_id, vec![(node, position, plan.point)])],
        });
    }
    let mut after = before.clone();
    after.insert(plan.index, node);
    editable.set_way_nodes(way, after.clone());
    actions.push(UndoableAction::SetWayNodes {
        layer: layer_id,
        way_id: way,
        before,
        after,
    });
    Ok(actions)
}

/// Join Node to Way (JOSM "Join Node to Way"): put the selected node onto
/// the nearest point of the selected way and make it a vertex of that way.
/// Deliberate, so unlike snapping it ignores tag compatibility. One undo step.
fn apply_join_node_way(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let (node, way) = join_node_way_target(editable, layer_id, selection)
        .ok_or_else(|| tr!("action.join_node_way.hint").to_string())?;
    let actions = join_node_into_way(editable, layer_id, node, way, None)?;
    let label = tr!("action.join_node_way.done").to_string();
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
        .ok_or_else(|| tr!("action.extend.hint").to_string())?;
    Ok(ActionOutcome {
        undo: None,
        status: tr!("action.extend.started").to_string(),
        clear_selection: false,
        select: Some(vec![FeatureRef {
            layer_id,
            kind: FeatureKind::Way,
            id: from.way_id,
        }]),
        continue_way: Some(from),
    })
}

/// The selected nodes and ways of `layer_id` that exist in it, each once, in
/// selection order: what Add to Relation appends.
fn member_candidates(
    editable: &dyn EditableLayer,
    layer_id: LayerId,
    selection: &[FeatureRef],
) -> Vec<(MemberType, i64)> {
    let mut out: Vec<(MemberType, i64)> = Vec::new();
    for f in selection.iter().filter(|f| f.layer_id == layer_id) {
        let (member_type, exists) = match f.kind {
            FeatureKind::Node => (MemberType::Node, editable.node_lat_lon(f.id).is_some()),
            FeatureKind::Way => (MemberType::Way, editable.way_node_ids(f.id).is_some()),
            FeatureKind::Relation => continue,
        };
        if exists && !out.contains(&(member_type, f.id)) {
            out.push((member_type, f.id));
        }
    }
    out
}

/// The selected nodes, ways and relations of `layer_id` that exist in it,
/// each once, in selection order: what New Relation makes the members of the
/// relation it creates.
fn relation_member_candidates(
    editable: &dyn EditableLayer,
    layer_id: LayerId,
    selection: &[FeatureRef],
) -> Vec<(MemberType, i64)> {
    let mut out = Vec::new();
    for f in selection.iter().filter(|f| f.layer_id == layer_id) {
        let (member_type, exists) = match f.kind {
            FeatureKind::Node => (MemberType::Node, editable.node_lat_lon(f.id).is_some()),
            FeatureKind::Way => (MemberType::Way, editable.way_node_ids(f.id).is_some()),
            FeatureKind::Relation => (MemberType::Relation, editable.relation(f.id).is_some()),
        };
        if exists && !out.contains(&(member_type, f.id)) {
            out.push((member_type, f.id));
        }
    }
    out
}

/// Create a tagless relation whose members are the selected objects of the
/// active layer, in selection order with empty roles, and select it so the
/// side panel offers its tags and members. One undo step.
fn apply_new_relation(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let members: Vec<osm_gpui::osm::OsmMember> =
        relation_member_candidates(editable, layer_id, selection)
            .into_iter()
            .map(|(member_type, reference)| osm_gpui::osm::OsmMember {
                member_type,
                reference,
                role: String::new(),
            })
            .collect();
    if members.is_empty() {
        return Err(tr!("action.new_relation.hint").to_string());
    }
    if let Some(m) =
        osm_gpui::limits::relation_over_limit(members.len(), osm_gpui::limits::relation_new_message)
    {
        return Err(m);
    }
    let count = members.len();
    let id = editable
        .create_relation(Vec::new(), members)
        .ok_or_else(|| tr!("action.no_data").to_string())?;
    let relation = editable
        .relation(id)
        .ok_or_else(|| tr!("action.new_relation.hint").to_string())?;
    let index = editable.relation_ids().len().saturating_sub(1);
    Ok(ActionOutcome {
        undo: Some(UndoableAction::CreateRelation {
            layer: layer_id,
            snapshot: osm_gpui::selection::DeletedRelationSnapshot { relation, index },
        }),
        status: tr!(
            "action.new_relation.done",
            members = tr_count!("action.feature_count", count)
        )
        .to_string(),
        clear_selection: false,
        select: Some(vec![FeatureRef {
            layer_id,
            kind: FeatureKind::Relation,
            id,
        }]),
        continue_way: None,
    })
}

/// Tag keys that describe what an area is. Create Multipolygon moves these
/// (and `name`) from a lone outer way to the relation; everything else, such
/// as `source`, `boundary` or `highway`, stays on the way.
const AREA_TAG_KEYS: &[&str] = &[
    "landuse",
    "natural",
    "leisure",
    "amenity",
    "building",
    "water",
    "wetland",
    "place",
    "tourism",
    "shop",
    "historic",
    "military",
    "aeroway",
    "man_made",
    "office",
    "craft",
    "healthcare",
];

/// Create a `type=multipolygon` relation from the selected ways (JOSM
/// "Create Multipolygon"). See `multipolygon_create::plan` for how rings are
/// chained and given roles. One relation holds every outer and inner ring.
/// When there is exactly one outer ring and it is a single way, that way's
/// area tags (`AREA_TAG_KEYS`, plus `name` if any of them moved) move to the
/// relation; with several outers or a ring of several ways nothing moves.
/// Relations the ways are already in are left alone. One undo step: the
/// relation's creation and the tag moves. The new relation is selected.
fn apply_create_multipolygon(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    use osm_gpui::multipolygon_create::{plan, Problem, Role};
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let mut ways: Vec<(i64, Vec<i64>)> = Vec::new();
    for f in selection
        .iter()
        .filter(|f| f.layer_id == layer_id && f.kind == FeatureKind::Way)
    {
        if ways.iter().any(|(id, _)| *id == f.id) {
            continue;
        }
        if let Some(nodes) = editable.way_node_ids(f.id) {
            ways.push((f.id, nodes));
        }
    }
    // Planar coordinates: longitude scaled by the cosine of the first
    // node's latitude, which is plenty for containment and crossing tests.
    let scale = ways
        .iter()
        .find_map(|(_, nodes)| editable.node_lat_lon(*nodes.first()?))
        .map_or(1.0, |(lat, _)| lat.to_radians().cos());
    let coord = |id: i64| {
        editable
            .node_lat_lon(id)
            .map(|(lat, lon)| (lon * scale, lat))
    };
    let plan = plan(&ways, &coord).map_err(|p| match p {
        Problem::Nothing => tr!("action.create_multipolygon.hint").to_string(),
        Problem::DegenerateWay(id) => {
            tr!("action.create_multipolygon.degenerate", way = id).to_string()
        }
        Problem::MissingNode(id) => {
            tr!("action.create_multipolygon.missing_node", node = id).to_string()
        }
        Problem::Branching(id) => {
            tr!("action.create_multipolygon.branching", node = id).to_string()
        }
        Problem::Unclosed(id) => tr!("action.create_multipolygon.unclosed", way = id).to_string(),
        Problem::SelfTouching(id) => {
            tr!("action.create_multipolygon.self_touching", node = id).to_string()
        }
        Problem::ZeroArea => tr!("action.create_multipolygon.zero_area").to_string(),
        Problem::Crossing(a, b) => {
            tr!("action.create_multipolygon.crossing", first = a, second = b).to_string()
        }
    })?;

    let members: Vec<osm_gpui::osm::OsmMember> = plan
        .rings
        .iter()
        .flat_map(|ring| {
            ring.ways.iter().map(move |&way| osm_gpui::osm::OsmMember {
                member_type: MemberType::Way,
                reference: way,
                role: ring.role.as_str().to_string(),
            })
        })
        .collect();
    if let Some(m) =
        osm_gpui::limits::relation_over_limit(members.len(), osm_gpui::limits::relation_new_message)
    {
        return Err(m);
    }

    let outers: Vec<_> = plan
        .rings
        .iter()
        .filter(|r| r.role == Role::Outer)
        .collect();
    let mut relation_tags = vec![("type".to_string(), "multipolygon".to_string())];
    let mut tag_entries = Vec::new();
    if let [outer] = outers.as_slice() {
        if let [way] = outer.ways.as_slice() {
            let feature = FeatureRef {
                layer_id,
                kind: FeatureKind::Way,
                id: *way,
            };
            let tags = editable.feature_tags(&feature).unwrap_or_default();
            if tags
                .iter()
                .any(|(k, _)| AREA_TAG_KEYS.contains(&k.as_str()))
            {
                for (k, v) in tags {
                    if AREA_TAG_KEYS.contains(&k.as_str()) || k == "name" {
                        tag_entries.push((feature, k.clone(), Some(v.clone()), None));
                        relation_tags.push((k, v));
                    }
                }
            }
        }
    }
    // Ways that already belong to a multipolygon: allowed, but mentioned.
    let shared = plan
        .rings
        .iter()
        .flat_map(|r| r.ways.iter())
        .filter(|&&way| {
            editable.relations_containing_way(way).into_iter().any(|r| {
                editable
                    .relation(r)
                    .is_some_and(|r| r.tags.get("type").is_some_and(|t| t == "multipolygon"))
            })
        })
        .count();

    for (f, key, _, _) in &tag_entries {
        editable.remove_tag(f.kind, f.id, key);
    }
    let count = members.len();
    let (n_outer, n_inner) = (outers.len(), plan.rings.len() - outers.len());
    let id = editable
        .create_relation(relation_tags, members)
        .ok_or_else(|| tr!("action.no_data").to_string())?;
    let relation = editable
        .relation(id)
        .ok_or_else(|| tr!("action.create_multipolygon.hint").to_string())?;
    let index = editable.relation_ids().len().saturating_sub(1);
    let create = UndoableAction::CreateRelation {
        layer: layer_id,
        snapshot: osm_gpui::selection::DeletedRelationSnapshot { relation, index },
    };
    let mut status = tr!(
        "action.create_multipolygon.done",
        members = tr_count!("action.feature_count", count),
        outers = n_outer,
        inners = n_inner
    )
    .to_string();
    if shared > 0 {
        status = format!(
            "{status}. {}",
            tr!("action.create_multipolygon.shared", count = shared)
        );
    }
    let undo = if tag_entries.is_empty() {
        create
    } else {
        UndoableAction::Batch {
            label: status.clone(),
            actions: vec![
                create,
                UndoableAction::SetTags {
                    entries: tag_entries,
                },
            ],
        }
    };
    Ok(ActionOutcome {
        undo: Some(undo),
        status,
        clear_selection: false,
        select: Some(vec![FeatureRef {
            layer_id,
            kind: FeatureKind::Relation,
            id,
        }]),
        continue_way: None,
    })
}

/// Append the selected nodes and ways of the active layer to relation
/// `relation_id` (the picker's choice), each with an empty role; those that
/// are already members are skipped. One undo step.
pub(crate) fn add_to_relation(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
    relation_id: i64,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let relation = editable
        .relation(relation_id)
        .ok_or_else(|| tr!("action.add_to_relation.hint").to_string())?;
    let name = osm_gpui::relations::display_name(&relation);
    let candidates = member_candidates(editable, layer_id, selection);
    let after = osm_gpui::relations::add_members(&relation.members, &candidates)
        .ok_or_else(|| tr!("action.add_to_relation.already", relation = name).to_string())?;
    if let Some(m) =
        osm_gpui::limits::relation_over_limit(after.len(), osm_gpui::limits::relation_add_message)
    {
        return Err(m);
    }
    let added = after.len() - relation.members.len();
    let before = editable
        .set_relation_members(relation_id, after.clone())
        .ok_or_else(|| tr!("action.add_to_relation.hint").to_string())?;
    Ok(ActionOutcome {
        undo: Some(UndoableAction::SetRelationMembers {
            layer: layer_id,
            relation_id,
            before,
            after,
        }),
        status: tr!(
            "action.add_to_relation.done",
            features = tr_count!("action.feature_count", added),
            relation = name
        )
        .to_string(),
        clear_selection: false,
        select: None,
        continue_way: None,
    })
}

/// One relation's member list before and after a feature leaves it.
type MemberEdit = (
    i64,
    Vec<osm_gpui::osm::OsmMember>,
    Vec<osm_gpui::osm::OsmMember>,
);

/// The relation edits that deleting a node or way needs, or the reason it
/// must be refused (it would empty a relation or break a restriction).
fn plan_member_edits(
    editable: &dyn EditableLayer,
    kind: FeatureKind,
    id: i64,
) -> Result<Vec<MemberEdit>, String> {
    let (member_type, relation_ids, what) = match kind {
        FeatureKind::Node => (
            MemberType::Node,
            editable.relations_containing_node(id),
            tr!("action.delete.kind.node"),
        ),
        FeatureKind::Way => (
            MemberType::Way,
            editable.relations_containing_way(id),
            tr!("action.delete.kind.way"),
        ),
        FeatureKind::Relation => return Ok(Vec::new()),
    };
    let mut edits = Vec::new();
    for relation_id in relation_ids {
        let Some(relation) = editable.relation(relation_id) else {
            continue;
        };
        match osm_gpui::relations::delete_member(&relation, member_type, id) {
            Ok(Some(after)) => edits.push((relation_id, relation.members, after)),
            Ok(None) => {}
            Err(e) => return Err(e.message(&what, id)),
        }
    }
    Ok(edits)
}

/// Apply planned relation edits and record them as undo entries.
fn apply_member_edits(
    editable: &mut dyn EditableLayer,
    layer_id: LayerId,
    edits: Vec<MemberEdit>,
    actions: &mut Vec<UndoableAction>,
    changed_relations: &mut Vec<(LayerId, i64)>,
) {
    for (relation_id, before, after) in edits {
        editable.set_relation_members(relation_id, after.clone());
        actions.push(UndoableAction::SetRelationMembers {
            layer: layer_id,
            relation_id,
            before,
            after,
        });
        if !changed_relations.contains(&(layer_id, relation_id)) {
            changed_relations.push((layer_id, relation_id));
        }
    }
}

/// Delete the way, then every node it used that is now an orphan: untagged,
/// in no other way and no relation, and not selected itself (a selected node
/// is handled on its own turn) nor `keep`. `Ok(false)` if the way is gone.
fn delete_way_and_orphans(
    editable: &mut dyn EditableLayer,
    layer_id: LayerId,
    way_id: i64,
    keep: Option<i64>,
    selection: &[FeatureRef],
    actions: &mut Vec<UndoableAction>,
    changed_relations: &mut Vec<(LayerId, i64)>,
) -> Result<bool, String> {
    let edits = plan_member_edits(editable, FeatureKind::Way, way_id)?;
    // The layer may still refuse; then nothing about the relations changes.
    let Some(snapshot) = editable.delete_feature(FeatureKind::Way, way_id) else {
        return Ok(false);
    };
    let way_nodes = snapshot.way_nodes.clone();
    actions.push(UndoableAction::DeleteFeature {
        layer: layer_id,
        snapshot,
    });
    apply_member_edits(editable, layer_id, edits, actions, changed_relations);
    let mut seen = Vec::new();
    for node_id in way_nodes {
        if seen.contains(&node_id) {
            continue;
        }
        seen.push(node_id);
        let node = FeatureRef {
            layer_id,
            kind: FeatureKind::Node,
            id: node_id,
        };
        if Some(node_id) == keep
            || selection.contains(&node)
            || !editable.ways_using_node(node_id).is_empty()
            || !editable.relations_containing_node(node_id).is_empty()
            || editable
                .feature_tags(&node)
                .is_some_and(|tags| !tags.is_empty())
        {
            continue;
        }
        if let Some(snapshot) = editable.delete_feature(FeatureKind::Node, node_id) {
            actions.push(UndoableAction::DeleteFeature {
                layer: layer_id,
                snapshot,
            });
        }
    }
    Ok(true)
}

/// What deleting a vertex does to one way that uses it.
enum WayFate {
    /// The way keeps going without the node.
    Shorten(Vec<i64>),
    /// Too few nodes would be left, so the way goes too.
    Delete,
}

/// `way_nodes` with every occurrence of `node` taken out (a closed way stays
/// closed), or `Delete` if what is left can no longer be a way.
fn way_without_node(way_nodes: &[i64], node: i64) -> WayFate {
    let closed = way_nodes.len() > 1 && way_nodes.first() == way_nodes.last();
    let mut rest: Vec<i64> = way_nodes.iter().copied().filter(|&n| n != node).collect();
    rest.dedup();
    if closed {
        // The ring's start and end are the same node; losing it moves both.
        if way_nodes.first() == Some(&node) {
            if let Some(&first) = rest.first() {
                rest.push(first);
            }
        }
        if rest.len() < 4 {
            return WayFate::Delete;
        }
    } else if rest.len() < 2 {
        return WayFate::Delete;
    }
    WayFate::Shorten(rest)
}

/// Delete a node. One used by ways is taken out of each of them first; a way
/// left with too few nodes is deleted (with its orphaned nodes). Everything
/// is checked before anything changes, so a refusal leaves no trace.
fn delete_vertex(
    editable: &mut dyn EditableLayer,
    layer_id: LayerId,
    node_id: i64,
    selection: &[FeatureRef],
    actions: &mut Vec<UndoableAction>,
    changed_relations: &mut Vec<(LayerId, i64)>,
) -> Result<bool, String> {
    if editable.node_lat_lon(node_id).is_none() {
        return Ok(false);
    }
    let node_edits = plan_member_edits(editable, FeatureKind::Node, node_id)?;
    let mut fates = Vec::new();
    let mut way_ids = editable.ways_using_node(node_id);
    way_ids.sort_unstable();
    for way_id in way_ids {
        let Some(way_nodes) = editable.way_node_ids(way_id) else {
            continue;
        };
        let fate = way_without_node(&way_nodes, node_id);
        if matches!(fate, WayFate::Delete) {
            plan_member_edits(editable, FeatureKind::Way, way_id)?;
        }
        fates.push((way_id, way_nodes, fate));
    }
    for (way_id, before, fate) in fates {
        match fate {
            WayFate::Shorten(after) => {
                if editable.set_way_nodes(way_id, after.clone()) {
                    actions.push(UndoableAction::SetWayNodes {
                        layer: layer_id,
                        way_id,
                        before,
                        after,
                    });
                }
            }
            WayFate::Delete => {
                delete_way_and_orphans(
                    editable,
                    layer_id,
                    way_id,
                    Some(node_id),
                    selection,
                    actions,
                    changed_relations,
                )?;
            }
        }
    }
    let Some(snapshot) = editable.delete_feature(FeatureKind::Node, node_id) else {
        return Ok(false);
    };
    actions.push(UndoableAction::DeleteFeature {
        layer: layer_id,
        snapshot,
    });
    apply_member_edits(editable, layer_id, node_edits, actions, changed_relations);
    Ok(true)
}

/// Delete every selected feature. A node used by ways is removed from them
/// (a way left too short is deleted); deleting a way also deletes its
/// untagged nodes nothing else uses. Refused features are skipped. A deleted node or way is also removed from the
/// relations it is a member of (see `relations::delete_member`); a feature
/// whose removal would empty a relation or break a turn restriction is refused
/// instead, with the reason in the status. The deletions and membership
/// changes are one undo step.
fn apply_delete(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    _active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    let mut actions = Vec::new();
    let mut deleted = 0usize;
    let mut changed_relations: Vec<(LayerId, i64)> = Vec::new();
    let mut refusal: Option<String> = None;
    // Relations go first, so a node or way that is a member of a relation
    // being deleted along with it needs no membership edit.
    let mut deleted_relations = 0usize;
    let mut pending: Vec<&FeatureRef> = selection
        .iter()
        .filter(|f| f.kind == FeatureKind::Relation)
        .collect();
    // A relation that is a member of another selected relation is refused
    // until that parent is gone, so retry while anything still goes through.
    loop {
        let before = pending.len();
        pending.retain(|feature| {
            let Some(editable) = layers
                .find_layer_mut(feature.layer_id)
                .and_then(|l| l.as_editable_mut())
            else {
                return false;
            };
            match editable.delete_relation(feature.id) {
                Some(snapshot) => {
                    deleted_relations += 1;
                    actions.push(UndoableAction::DeleteRelation {
                        layer: feature.layer_id,
                        snapshot,
                    });
                    false
                }
                // Gone already: nothing to do. Otherwise another relation
                // still has it as a member.
                None => editable.relation(feature.id).is_some(),
            }
        });
        if pending.len() == before {
            break;
        }
    }
    for feature in pending {
        let Some(editable) = layers
            .find_layer(feature.layer_id)
            .and_then(|l| l.as_editable())
        else {
            continue;
        };
        let parent = editable
            .relations_containing_relation(feature.id)
            .first()
            .and_then(|&id| editable.relation(id));
        let relation = editable.relation(feature.id);
        if let (Some(parent), Some(relation)) = (parent, relation) {
            refusal.get_or_insert_with(|| {
                tr!(
                    "action.delete.error.relation_in_relation",
                    relation = osm_gpui::relations::display_name(&relation),
                    parent = osm_gpui::relations::display_name(&parent)
                )
                .to_string()
            });
        }
    }
    deleted += deleted_relations;
    // Ways go before nodes, so a node selected together with the ways that
    // use it is free to go by the time its turn comes.
    let ways = selection.iter().filter(|f| f.kind == FeatureKind::Way);
    let nodes = selection.iter().filter(|f| f.kind == FeatureKind::Node);
    for feature in ways.chain(nodes) {
        let Some(editable) = layers
            .find_layer_mut(feature.layer_id)
            .and_then(|l| l.as_editable_mut())
        else {
            continue;
        };
        let layer_id = feature.layer_id;
        let result = match feature.kind {
            FeatureKind::Way => delete_way_and_orphans(
                editable,
                layer_id,
                feature.id,
                None,
                selection,
                &mut actions,
                &mut changed_relations,
            ),
            FeatureKind::Node => delete_vertex(
                editable,
                layer_id,
                feature.id,
                selection,
                &mut actions,
                &mut changed_relations,
            ),
            FeatureKind::Relation => continue,
        };
        match result {
            Ok(true) => deleted += 1,
            Ok(false) => {}
            Err(message) => {
                refusal.get_or_insert(message);
            }
        }
    }
    if deleted == 0 {
        return Err(refusal.unwrap_or_else(|| tr!("status.nothing_to_delete").to_string()));
    }
    let mut status = if deleted == deleted_relations && changed_relations.is_empty() {
        tr_count!("status.deleted_relations", deleted).to_string()
    } else if changed_relations.is_empty() {
        tr_count!("status.deleted_features", deleted).to_string()
    } else {
        tr_count!(
            "status.deleted_features_in_relations",
            deleted,
            relations = tr_count!("action.relation_count", changed_relations.len())
        )
        .to_string()
    };
    let undo = if actions.len() == 1 {
        actions.remove(0)
    } else {
        UndoableAction::Batch {
            label: status.clone(),
            actions,
        }
    };
    if let Some(reason) = refusal {
        status = tr!("status.delete_skipped", done = status, reason = reason).to_string();
    }
    Ok(ActionOutcome {
        undo: Some(undo),
        status,
        clear_selection: true,
        select: None,
        continue_way: None,
    })
}

/// Purge: drop the selected objects from the layer's
/// downloaded data without deleting them. Nothing is uploaded for them; they
/// are simply no longer loaded.
///
/// Conservative rules, checked before anything changes (a single refusal
/// refuses the whole action, with the reason in the status):
/// - an object with a pending local edit (created here or changed) is refused;
///   the user uploads or undoes it first;
/// - a selected node that a way outside the selection still uses is refused;
/// - ways and relations named by a remaining relation are purged anyway: the
///   relation keeps the member, which is then "not downloaded" (never deleted);
/// - the nodes of a purged way go with it only when no remaining way or
///   relation uses them and they have no pending edit; otherwise they stay.
///
/// Selected objects of several layers are purged per layer. One undo step.
fn apply_purge(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    _active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    use std::collections::BTreeSet;
    let mut layer_ids: Vec<LayerId> = Vec::new();
    for f in selection {
        if !layer_ids.contains(&f.layer_id) {
            layer_ids.push(f.layer_id);
        }
    }
    // (layer, nodes, ways, relations) to purge, and how many child nodes stay.
    type Plan = (LayerId, Vec<i64>, Vec<i64>, Vec<i64>);
    let mut plans: Vec<Plan> = Vec::new();
    let mut kept = 0usize;
    for layer_id in layer_ids {
        let Some(editable) = layers.find_layer(layer_id).and_then(|l| l.as_editable()) else {
            continue;
        };
        let ids_of = |kind| -> BTreeSet<i64> {
            selection
                .iter()
                .filter(|f| f.layer_id == layer_id && f.kind == kind)
                .map(|f| f.id)
                .collect()
        };
        let nodes = ids_of(FeatureKind::Node);
        let ways = ids_of(FeatureKind::Way);
        let relations = ids_of(FeatureKind::Relation);
        let named = [
            (FeatureKind::Node, &nodes),
            (FeatureKind::Way, &ways),
            (FeatureKind::Relation, &relations),
        ];
        for (kind, ids) in &named {
            for &id in ids.iter() {
                if editable.has_pending_edit(*kind, id) {
                    let what = match kind {
                        FeatureKind::Node => tr!("action.purge.kind.node"),
                        FeatureKind::Way => tr!("action.purge.kind.way"),
                        FeatureKind::Relation => tr!("action.purge.kind.relation"),
                    };
                    return Err(tr!(
                        "action.purge.error.modified",
                        what = what,
                        id = id.to_string()
                    )
                    .to_string());
                }
            }
        }
        for &id in &nodes {
            if let Some(way) = editable
                .ways_using_node(id)
                .into_iter()
                .find(|w| !ways.contains(w))
            {
                return Err(tr!(
                    "action.purge.error.in_way",
                    id = id.to_string(),
                    way = way.to_string()
                )
                .to_string());
            }
        }
        // Children of purged ways that nothing else needs.
        let mut all_nodes = nodes.clone();
        for &way in &ways {
            for child in editable.way_node_ids(way).unwrap_or_default() {
                if all_nodes.contains(&child) {
                    continue;
                }
                let in_other_way = editable
                    .ways_using_node(child)
                    .iter()
                    .any(|w| !ways.contains(w));
                let in_relation = editable
                    .relations_containing_node(child)
                    .iter()
                    .any(|r| !relations.contains(r));
                if in_other_way
                    || in_relation
                    || editable.has_pending_edit(FeatureKind::Node, child)
                {
                    kept += 1;
                } else {
                    all_nodes.insert(child);
                }
            }
        }
        plans.push((
            layer_id,
            all_nodes.into_iter().collect(),
            ways.into_iter().collect(),
            relations.into_iter().collect(),
        ));
    }
    let mut actions = Vec::new();
    let mut total = 0usize;
    for (layer_id, nodes, ways, relations) in plans {
        let Some(editable) = layers
            .find_layer_mut(layer_id)
            .and_then(|l| l.as_editable_mut())
        else {
            continue;
        };
        let purged = editable.purge_features(&nodes, &ways, &relations);
        if purged.is_empty() {
            continue;
        }
        total += purged.nodes.len() + purged.ways.len() + purged.relations.len();
        actions.push(UndoableAction::Purge {
            layer: layer_id,
            purged,
        });
    }
    if actions.is_empty() {
        return Err(tr!("action.purge.hint").to_string());
    }
    let label = tr_count!("action.purge.done", total).to_string();
    let mut status = label.clone();
    if kept > 0 {
        status = tr!("action.purge.kept", done = status, kept = kept.to_string()).to_string();
    }
    Ok(ActionOutcome {
        undo: Some(UndoableAction::Batch { label, actions }),
        status,
        clear_selection: true,
        select: None,
        continue_way: None,
    })
}

/// Computes new positions for a list of nodes; see `reposition_nodes`.
type RepositionFn = fn(&[LatLon]) -> Option<Vec<LatLon>>;

/// Move the selected nodes of the active layer (at least three) by running
/// `reposition` over their positions in selection order. Nodes shared with
/// other ways move those ways too; tags and relation memberships are not
/// touched. One `MoveNodes` undo step.
fn reposition_nodes(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
    reposition: RepositionFn,
    done: fn(usize) -> SharedString,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let nodes = merge_nodes_in(editable, layer_id, selection);
    if nodes.len() < 3 {
        return Err(tr!("action.align.hint").to_string());
    }
    let ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
    let before: Vec<LatLon> = ids
        .iter()
        .filter_map(|&id| editable.node_lat_lon(id))
        .collect();
    if before.len() != ids.len() {
        return Err(tr!("action.align.hint").to_string());
    }
    let after =
        reposition(&before).ok_or_else(|| tr!("action.align.error.same_place").to_string())?;
    let entries: Vec<NodeMoveUndoEntry> = ids
        .iter()
        .zip(before.iter().zip(&after))
        .filter(|(_, (b, a))| b != a)
        .map(|(&id, (&b, &a))| (id, b, a))
        .collect();
    if entries.is_empty() {
        return Err(tr!("action.align.none").to_string());
    }
    let moves: Vec<(i64, f64, f64)> = entries
        .iter()
        .map(|&(id, _, (lat, lon))| (id, lat, lon))
        .collect();
    editable.commit_node_moves(&moves);
    Ok(ActionOutcome {
        undo: Some(UndoableAction::MoveNodes {
            per_layer: vec![(layer_id, entries.clone())],
        }),
        status: done(entries.len()).to_string(),
        clear_selection: false,
        select: None,
        continue_way: None,
    })
}

/// Align Nodes in Line: see `osm_gpui::align::align_in_line`. For a way
/// rather than loose nodes, Straighten does the same job.
fn apply_align(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    reposition_nodes(
        layers,
        selection,
        active,
        osm_gpui::align::align_in_line,
        |n| tr_count!("action.align.done", n),
    )
}

/// Distribute Nodes: see `osm_gpui::align::distribute`.
fn apply_distribute(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    reposition_nodes(
        layers,
        selection,
        active,
        osm_gpui::align::distribute,
        |n| tr_count!("action.distribute.done", n),
    )
}

/// Create an offset copy of every selected way of the active layer (the
/// dialog's answer; see `osm_gpui::parallel`). Each copy gets its own new
/// nodes and is not connected to the original; relation memberships of the
/// original are not copied. The new ways become the selection. One undo step.
pub(crate) fn create_parallel_ways(
    layers: &mut LayerManager,
    selection: &[FeatureRef],
    active: Option<LayerId>,
    params: osm_gpui::parallel::ParallelParams,
) -> Result<ActionOutcome, String> {
    let (layer_id, editable) = active_editable_mut(layers, active)?;
    let ways = selected_ways_in(editable, layer_id, selection);

    // Plan everything first, so a refusal leaves no trace.
    type Plan = (Vec<LatLon>, Vec<(String, String)>);
    let mut plans: Vec<Plan> = Vec::new();
    let mut seen: Vec<i64> = Vec::new();
    for way in &ways {
        if seen.contains(&way.way_id) {
            continue;
        }
        seen.push(way.way_id);
        let Some(mut line) = osm_gpui::parallel::offset_way(&way.points, params.offset_m) else {
            continue;
        };
        if params.reverse {
            line.reverse();
        }
        if let Some(m) =
            osm_gpui::limits::way_over_limit(line.len(), osm_gpui::limits::way_full_message)
        {
            return Err(m);
        }
        let tags = if params.copy_tags {
            let way_ref = FeatureRef {
                layer_id,
                kind: FeatureKind::Way,
                id: way.way_id,
            };
            editable.feature_tags(&way_ref).unwrap_or_default()
        } else {
            Vec::new()
        };
        plans.push((line, tags));
    }
    if plans.is_empty() {
        return Err(tr!("action.parallel.hint").to_string());
    }

    let mut actions = Vec::new();
    let mut new_ways = Vec::new();
    for (line, tags) in plans {
        // A ring's last point is its first node again.
        let closed = line.len() >= 4 && line.first() == line.last();
        let distinct = if closed { line.len() - 1 } else { line.len() };
        let mut node_ids = Vec::with_capacity(line.len());
        for &(lat, lon) in &line[..distinct] {
            let id = editable.add_node(lat, lon);
            actions.push(UndoableAction::CreateNode {
                layer: layer_id,
                id,
                lat,
                lon,
            });
            node_ids.push(id);
        }
        if closed {
            node_ids.push(node_ids[0]);
        }
        let way_id = editable.add_way(node_ids.clone(), tags.clone());
        actions.push(UndoableAction::CreateWay {
            layer: layer_id,
            snapshot: osm_gpui::selection::DeletedFeatureSnapshot {
                kind: FeatureKind::Way,
                id: way_id,
                tags,
                way_nodes: node_ids,
                node_lat_lon: None,
                version: 0,
            },
        });
        new_ways.push(FeatureRef {
            layer_id,
            kind: FeatureKind::Way,
            id: way_id,
        });
    }

    let label = tr_count!("action.parallel.done", new_ways.len()).to_string();
    Ok(ActionOutcome {
        undo: Some(UndoableAction::Batch {
            label: label.clone(),
            actions,
        }),
        status: label,
        clear_selection: false,
        select: Some(new_ways),
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
/// - way 108 `[14,15,16,14]` is a closed ring;
/// - node 7 is on no way, and is a member of relation 900 (as is way 102).
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
                members: vec![
                    OsmMember {
                        member_type: MemberType::Way,
                        reference: 102,
                        role: String::new(),
                    },
                    OsmMember {
                        member_type: MemberType::Node,
                        reference: 7,
                        role: "stop".to_string(),
                    },
                ],
                version: 1,
                tags: HashMap::new(),
            }],
            bounds: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use osm_gpui::layers::osm_layer::OsmLayer;
    use osm_gpui::osm::{OsmMember, OsmRelation};

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
            [
                "square",
                "circularize",
                "reverse",
                "copy_features",
                "duplicate_features",
                "parallel",
                "copy_tags",
                "new_relation",
                "create_multipolygon",
                "purge",
                "delete"
            ]
        );
        // An open two-node way can only be reversed or copied alongside (or
        // extended, copied, put in a relation, or deleted).
        assert_eq!(
            applicable_ids(&f, &[way_ref(&f, f.line)]),
            [
                "reverse",
                "extend",
                "copy_features",
                "duplicate_features",
                "parallel",
                "new_relation",
                "purge",
                "delete"
            ]
        );
    }

    #[test]
    fn new_relation_collects_the_selection_and_undoes_via_its_snapshot() {
        let mut f = fixture();
        let sel = [
            way_ref(&f, f.line),
            way_ref(&f, f.rect),
            way_ref(&f, f.line),
        ];
        let out =
            (find("new_relation").unwrap().apply)(&mut f.layers, &sel, Some(f.layer_id)).unwrap();
        let Some(UndoableAction::CreateRelation { layer, snapshot }) = out.undo else {
            panic!("expected CreateRelation");
        };
        assert_eq!(layer, f.layer_id);
        assert!(snapshot.relation.id < 0);
        assert!(snapshot.relation.tags.is_empty());
        // Selection order, once each, empty roles.
        let members: Vec<(MemberType, i64, &str)> = snapshot
            .relation
            .members
            .iter()
            .map(|m| (m.member_type, m.reference, m.role.as_str()))
            .collect();
        assert_eq!(
            members,
            [(MemberType::Way, f.line, ""), (MemberType::Way, f.rect, "")]
        );
        assert_eq!(
            out.status.as_str(),
            "Created a relation with 2 features. Add its tags and roles in the side panel"
        );
        assert_eq!(
            out.select,
            Some(vec![FeatureRef {
                layer_id: f.layer_id,
                kind: FeatureKind::Relation,
                id: snapshot.relation.id
            }])
        );

        let editable = f
            .layers
            .find_layer_mut(f.layer_id)
            .unwrap()
            .as_editable_mut()
            .unwrap();
        assert_eq!(
            editable.relation(snapshot.relation.id),
            Some(snapshot.relation.clone())
        );
        // Undo removes it, redo puts the same relation back.
        assert!(editable.delete_relation(snapshot.relation.id).is_some());
        assert!(editable.relation(snapshot.relation.id).is_none());
        assert!(editable.restore_relation(snapshot.clone()));
        assert_eq!(
            editable.relation(snapshot.relation.id),
            Some(snapshot.relation)
        );
    }

    #[test]
    fn new_relation_needs_something_to_put_in_it() {
        let mut f = fixture();
        assert!(!applicable_ids(&f, &[]).contains(&"new_relation"));
        let err = (find("new_relation").unwrap().apply)(&mut f.layers, &[], Some(f.layer_id))
            .err()
            .unwrap();
        assert!(err.starts_with("Select nodes, ways"));
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
    fn reverse_without_direction_tags_is_a_plain_reverse_step() {
        let mut f = fixture();
        let sel = [way_ref(&f, f.rect)];
        let out = (find("reverse").unwrap().apply)(&mut f.layers, &sel, Some(f.layer_id)).unwrap();
        assert!(matches!(
            out.undo.as_ref().unwrap(),
            UndoableAction::ReverseWays { .. }
        ));
    }

    #[test]
    fn delete_is_one_undo_step_and_clears_the_selection() {
        let mut f = fixture();
        let sel = [way_ref(&f, f.rect), way_ref(&f, f.line)];
        let out = (find("delete").unwrap().apply)(&mut f.layers, &sel, Some(f.layer_id)).unwrap();
        assert!(out.clear_selection);
        assert_eq!(out.status, "Deleted 2 features");
        assert!(
            matches!(out.undo.as_ref().unwrap(), UndoableAction::Batch { actions, .. } if actions.len() >= 2)
        );
        assert_eq!(
            out.undo.as_ref().unwrap().description(),
            "Deleted 2 features"
        );
    }

    #[test]
    fn delete_removes_the_feature_from_its_relations_in_the_same_step() {
        use MemberType::{Node, Way};
        // Way 102 and node 7 are members of relation 900.
        let mut n = net();
        let out = n.run("delete", &n.ways(&[102])).unwrap();
        assert_eq!(
            out.status,
            "Deleted 1 feature and removed it from 1 relation"
        );
        assert_eq!(n.members(), members_of(&[(7, "stop")]));
        // The way and its one untagged node nothing else uses, plus the relation.
        assert_eq!(n.layer().diff_for_upload().counts(), (0, 1, 2));
        let undo = out.undo.as_ref().unwrap();
        assert_eq!(undo.description(), out.status);
        let UndoableAction::Batch { actions, .. } = undo else {
            panic!("expected one Batch step");
        };
        assert!(matches!(
            actions.as_slice(),
            [
                UndoableAction::DeleteFeature { .. },
                UndoableAction::SetRelationMembers {
                    relation_id: 900,
                    ..
                },
                UndoableAction::DeleteFeature { .. }
            ]
        ));

        // A node that is on no way works the same.
        let mut n = net();
        let out = n.run("delete", &n.nodes(&[7])).unwrap();
        assert_eq!(
            out.status,
            "Deleted 1 feature and removed it from 1 relation"
        );
        assert_eq!(n.members(), members_of(&[(102, "")]));

        // Several features, several relations, properly pluralised.
        let mut other = relation_900("route", &[(Way, 102, ""), (Way, 106, "")]);
        other.id = 901;
        let mut n = net_with_relations(vec![
            relation_900("route", &[(Way, 102, ""), (Node, 7, "stop")]),
            other,
        ]);
        let out = n.run("delete", &n.ways(&[102])).unwrap();
        assert_eq!(
            out.status,
            "Deleted 1 feature and removed it from 2 relations"
        );
        assert_eq!(n.members(), members_of(&[(7, "stop")]));

        // Two features in two relations: plural on both counts.
        let mut second = relation_900("route", &[(Way, 106, ""), (Way, 107, "")]);
        second.id = 901;
        let mut n = net_with_relations(vec![
            relation_900("route", &[(Way, 102, ""), (Node, 7, "stop")]),
            second,
        ]);
        let out = n.run("delete", &n.ways(&[102, 106])).unwrap();
        assert_eq!(
            out.status,
            "Deleted 2 features and removed them from 2 relations"
        );
        assert_eq!(n.layer().diff_for_upload().modified_relations.len(), 2);
    }

    #[test]
    fn select_all_then_delete_removes_everything() {
        use MemberType::Way;
        // Way 102 is the only member of relation 900, and every node is used
        // by a way: with the relation and the ways in the same selection,
        // nothing is refused, whatever order the selection lists them in.
        let mut n = net_with(relation_900("route", &[(Way, 102, "")]));
        let all = n.editable().all_features();
        assert!(all.iter().any(|f| f.kind == FeatureKind::Relation));
        assert_eq!(all.first().map(|f| f.kind), Some(FeatureKind::Node));
        let out = n.run("delete", &all).unwrap();
        assert!(out.status.starts_with("Deleted "), "{}", out.status);
        assert!(!out.status.contains("Can't"), "{}", out.status);
        assert!(n.editable().all_features().is_empty());
    }

    #[test]
    fn delete_refuses_to_empty_a_relation_or_break_a_restriction() {
        use MemberType::{Node, Way};
        // Way 102 is all of relation 900.
        let mut n = net_with(relation_900("route", &[(Way, 102, "")]));
        let Err(message) = n.run("delete", &n.ways(&[102])) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Can't delete way 102: it is the only member of relation 900"
        );
        assert!(!n.layer().is_modified());
        assert!(n.editable().way_node_ids(102).is_some());

        // Node 7 is the restriction's via member.
        let mut n = net_with(relation_900(
            "restriction",
            &[(Way, 100, "from"), (Node, 7, "via"), (Way, 102, "to")],
        ));
        let Err(message) = n.run("delete", &n.nodes(&[7])) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Can't delete node 7: turn restriction 900 would be left without a 'via' member"
        );
        assert!(!n.layer().is_modified());
        assert!(n.editable().node_lat_lon(7).is_some());

        // Part of a selection refused: the rest is still deleted, and the
        // status says why the other wasn't.
        let mut n = net_with(relation_900("route", &[(Way, 102, "")]));
        let out = n.run("delete", &n.ways(&[102, 106])).unwrap();
        assert_eq!(
            out.status,
            "Deleted 1 feature; Can't delete way 102: it is the only member of relation 900"
        );
        assert!(n.editable().way_node_ids(102).is_some());
        assert!(n.editable().way_node_ids(106).is_none());
        // The undo step is just way 106 and its orphaned node.
        let UndoableAction::Batch { actions, .. } = out.undo.as_ref().unwrap() else {
            panic!("expected one Batch step");
        };
        assert!(actions
            .iter()
            .all(|a| matches!(a, UndoableAction::DeleteFeature { .. })));
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
        assert_eq!(find_by_key(&settings, "m").map(|a| a.id), Some("merge"));
        assert_eq!(
            find_by_key(&settings, "t").map(|a| a.id),
            Some("straighten")
        );
        assert!(find_by_key(&settings, "z").is_none());
        // Copy/Paste Tags are modifier key bindings, never bare keys.
        assert!(find_by_key(&settings, "cmd-shift-c").is_none());
        assert_eq!(
            find("copy_tags")
                .unwrap()
                .shortcut_label(&settings)
                .unwrap(),
            "⇧⌘C"
        );
        assert_eq!(
            find("paste_tags")
                .unwrap()
                .shortcut_label(&settings)
                .unwrap(),
            "⇧⌘V"
        );
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
    fn every_action_has_a_label_and_a_hint_in_the_catalog() {
        // A missing key renders as "<locale>.<key>".
        for a in ACTIONS {
            assert!(!a.label().starts_with("en."), "{}: {}", a.id, a.label());
            assert!(!a.hint().starts_with("en."), "{}: {}", a.id, a.hint());
        }
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
        // Each test starts with an empty tag clipboard, even when the test
        // harness runs several tests on one thread.
        set_tag_clipboard(Vec::new());
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
    fn join_points_a_removed_members_relation_at_the_survivor() {
        let mut n = net();
        // 102 (a member of relation 900) would be deleted: 101 is first and
        // both are server ways. 101 isn't in the relation, so 102's
        // membership is re-pointed at it.
        let out = n.run("join", &n.ways(&[101, 102])).unwrap();
        assert_eq!(n.members(), members_of(&[(101, ""), (7, "stop")]));
        assert_eq!(n.editable().way_node_ids(101), Some(vec![3, 4, 5, 6]));
        assert_eq!(n.layer().diff_for_upload().counts(), (0, 2, 1));
        let UndoableAction::Batch { actions, .. } = out.undo.as_ref().unwrap() else {
            panic!("expected one Batch step");
        };
        assert!(matches!(
            actions.as_slice(),
            [
                UndoableAction::SetWayNodes { .. },
                UndoableAction::SetRelationMembers {
                    relation_id: 900,
                    ..
                },
                UndoableAction::DeleteFeature { .. }
            ]
        ));
        // Selected the other way round, the member way survives untouched.
        let mut n = net();
        assert!(n.run("join", &n.ways(&[102, 101])).is_ok());
        assert_eq!(n.editable().way_node_ids(102), Some(vec![3, 4, 5, 6]));
        assert_eq!(n.relation_edits(), 0);
    }

    #[test]
    fn join_drops_the_removed_way_when_the_survivor_is_a_member_with_the_same_role() {
        use MemberType::Way;
        let mut n = net_with(relation_900(
            "route",
            &[(Way, 100, "forward"), (Way, 101, "forward"), (Way, 102, "")],
        ));
        n.run("join", &n.ways(&[100, 101])).unwrap();
        assert_eq!(n.members(), members_of(&[(100, "forward"), (102, "")]));
    }

    #[test]
    fn join_refuses_different_roles_and_restrictions_and_changes_nothing() {
        use MemberType::{Node, Way};
        let mut n = net_with(relation_900(
            "route",
            &[(Way, 100, "forward"), (Way, 101, "backward")],
        ));
        let Err(message) = n.run("join", &n.ways(&[100, 101])) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Can't join: way 101 has a different role in relation 900 than the way it would be joined into"
        );
        assert!(!n.layer().is_modified());
        assert_eq!(n.editable().way_node_ids(101), Some(vec![3, 4, 5]));

        let mut n = net_with(relation_900(
            "restriction",
            &[(Way, 100, "from"), (Node, 3, "via"), (Way, 101, "to")],
        ));
        let Err(message) = n.run("join", &n.ways(&[100, 101])) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Can't join: way 101 is part of turn restriction 900"
        );
        assert!(!n.layer().is_modified());
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

    impl Net {
        fn tag(&mut self, kind: FeatureKind, id: i64, key: &str, value: &str) {
            self.layers
                .find_layer_mut(self.layer_id)
                .unwrap()
                .as_editable_mut()
                .unwrap()
                .set_tag(kind, id, key, value);
        }
        fn tags_of(&self, kind: FeatureKind, id: i64) -> Vec<(String, String)> {
            let mut tags = self
                .editable()
                .feature_tags(&self.feature(kind, id))
                .unwrap();
            tags.sort();
            tags
        }
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn reverse_flips_direction_tags_in_the_same_undo_step() {
        let mut n = net();
        n.tag(FeatureKind::Way, 106, "lanes:forward", "2");
        n.tag(FeatureKind::Way, 106, "incline", "8%");
        let out = n.run("reverse", &n.ways(&[106])).unwrap();
        assert_eq!(n.editable().way_node_ids(106), Some(vec![12, 11]));
        // oneway=yes stays; the direction-relative tags flip.
        assert_eq!(
            n.tags_of(FeatureKind::Way, 106),
            pairs(&[
                ("highway", "residential"),
                ("incline", "-8%"),
                ("lanes:backward", "2"),
                ("oneway", "yes"),
            ])
        );
        let UndoableAction::Batch { actions, .. } = out.undo.as_ref().unwrap() else {
            panic!("expected one Batch step");
        };
        assert!(matches!(
            actions.as_slice(),
            [
                UndoableAction::ReverseWays { .. },
                UndoableAction::SetTags { entries }
            ] if entries.len() == 3
        ));
        assert_eq!(n.layer().diff_for_upload().counts(), (0, 1, 0));
    }

    impl Net {
        fn nudge(&mut self, node: i64, lat: f64, lon: f64) {
            self.layers
                .find_layer_mut(self.layer_id)
                .unwrap()
                .as_editable_mut()
                .unwrap()
                .commit_node_moves(&[(node, lat, lon)]);
        }
    }

    impl Net {
        fn editable_mut(&mut self) -> &mut dyn EditableLayer {
            self.layers
                .find_layer_mut(self.layer_id)
                .unwrap()
                .as_editable_mut()
                .unwrap()
        }
        fn nodes(&self, ids: &[i64]) -> Vec<FeatureRef> {
            ids.iter()
                .map(|&id| self.feature(FeatureKind::Node, id))
                .collect()
        }
    }

    fn feature_list(n: &Net, list: &[(FeatureKind, i64)]) -> Vec<FeatureRef> {
        list.iter().map(|&(k, id)| n.feature(k, id)).collect()
    }

    #[test]
    fn copy_remembers_the_tags_of_exactly_one_tagged_feature() {
        let mut n = net();
        assert_eq!(tag_clipboard(), vec![]);
        let out = n.run("copy_tags", &n.ways(&[101])).unwrap();
        assert_eq!(out.status, "Copied 2 tags");
        assert!(out.undo.is_none() && out.select.is_none() && !out.clear_selection);
        assert_eq!(
            tag_clipboard(),
            pairs(&[("highway", "residential"), ("name", "Main")])
        );
        // Copying changes no data.
        assert!(!n.layer().is_modified());

        // One tag reads "1 tag"; more than one feature, or one without
        // tags, copy nothing and leave the clipboard alone.
        let single = n
            .run("copy_tags", &[n.feature(FeatureKind::Way, 103)])
            .unwrap();
        assert_eq!(single.status, "Copied 1 tag");
        assert!(n.run("copy_tags", &n.ways(&[100, 101])).is_err());
        assert!(n.run("copy_tags", &n.nodes(&[1])).is_err());
        assert!(n.run("copy_tags", &[]).is_err());
        assert_eq!(tag_clipboard(), pairs(&[("highway", "footway")]));
        assert!(n.applicable(&n.ways(&[101])).contains(&"copy_tags"));
        assert!(!n.applicable(&n.ways(&[100, 101])).contains(&"copy_tags"));
        assert!(!n.applicable(&n.nodes(&[1])).contains(&"copy_tags"));
        assert!(n.applicable(&n.nodes(&[4])).contains(&"copy_tags"));
    }

    #[test]
    fn paste_overwrites_copied_keys_keeps_others_and_is_one_set_tags_step() {
        let mut n = net();
        n.run("copy_tags", &n.ways(&[101])).unwrap();
        // Way 103 is a footway; node 8 has no tags; node 4 is a crossing.
        let targets = feature_list(
            &n,
            &[
                (FeatureKind::Way, 103),
                (FeatureKind::Node, 8),
                (FeatureKind::Node, 4),
            ],
        );
        let out = n.run("paste_tags", &targets).unwrap();
        assert_eq!(out.status, "Pasted 2 tags onto 3 features");
        assert_eq!(
            n.tags_of(FeatureKind::Way, 103),
            pairs(&[("highway", "residential"), ("name", "Main")])
        );
        assert_eq!(
            n.tags_of(FeatureKind::Node, 8),
            pairs(&[("highway", "residential"), ("name", "Main")])
        );
        // The crossing's own `highway` is overwritten; nothing else it had
        // is lost (it had nothing else).
        assert_eq!(
            n.tags_of(FeatureKind::Node, 4),
            pairs(&[("highway", "residential"), ("name", "Main")])
        );
        // Untouched features stay as they were.
        assert_eq!(
            n.tags_of(FeatureKind::Way, 100),
            pairs(&[("highway", "residential")])
        );
        let UndoableAction::SetTags { entries } = out.undo.as_ref().unwrap() else {
            panic!("expected one SetTags step");
        };
        // Way 103: 2 (highway overwritten, name added); node 8: 2; node 4: 2.
        assert_eq!(entries.len(), 6);
        let overwrite = entries
            .iter()
            .find(|e| e.0 == n.feature(FeatureKind::Way, 103) && e.1 == "highway")
            .unwrap();
        assert_eq!(overwrite.2.as_deref(), Some("footway"));
        assert_eq!(overwrite.3.as_deref(), Some("residential"));
        // Ways modified: 103 (tags); nodes: 8 and 4 (tags).
        assert_eq!(n.layer().diff_for_upload().counts(), (0, 3, 0));

        // Nothing left to change.
        assert_eq!(
            n.run("paste_tags", &targets).err().as_deref(),
            Some("The selected features already have the copied tags")
        );
    }

    #[test]
    fn paste_counts_only_features_that_changed() {
        let mut n = net();
        n.run("copy_tags", &n.ways(&[101])).unwrap();
        // Way 100 already has highway=residential: only `name` is new.
        // Way 101 is the source and has both.
        let out = n.run("paste_tags", &n.ways(&[100, 101])).unwrap();
        assert_eq!(out.status, "Pasted 2 tags onto 1 feature");
        let UndoableAction::SetTags { entries } = out.undo.as_ref().unwrap() else {
            panic!("expected one SetTags step");
        };
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn paste_needs_a_clipboard_and_something_selected() {
        let mut n = net();
        let sel = n.ways(&[100]);
        assert!(!n.applicable(&sel).contains(&"paste_tags"));
        assert_eq!(
            n.run("paste_tags", &sel).err().as_deref(),
            Some("Copy a feature's tags first, then select what to paste them onto")
        );
        n.run("copy_tags", &n.ways(&[101])).unwrap();
        assert!(n.applicable(&sel).contains(&"paste_tags"));
        assert!(n.applicable(&n.nodes(&[1, 2])).contains(&"paste_tags"));
        assert!(!n.applicable(&[]).contains(&"paste_tags"));
        // Features of another layer are not pasted onto.
        let elsewhere = FeatureRef {
            layer_id: LayerId(99),
            kind: FeatureKind::Way,
            id: 100,
        };
        assert!(!n.applicable(&[elsewhere]).contains(&"paste_tags"));
    }

    #[test]
    fn shortcut_labels_show_modifier_symbols() {
        assert_eq!(display_shortcut_for("cmd-shift-c", true), "⇧⌘C");
        assert_eq!(display_shortcut_for("cmd-alt-ctrl-z", true), "⌃⌥⌘Z");
        assert_eq!(display_shortcut_for("cmd--", true), "⌘-");
        assert_eq!(display_shortcut_for("secondary-shift-c", true), "⇧⌘C");
        assert_eq!(display_shortcut_for("q", true), "Q");
        assert_eq!(display_shortcut_for("backspace", true), "⌫");
    }

    #[test]
    fn shortcut_labels_are_spelled_out_off_macos() {
        assert_eq!(
            display_shortcut_for("secondary-shift-c", false),
            "Ctrl+Shift+C"
        );
        assert_eq!(display_shortcut_for("secondary--", false), "Ctrl+-");
        assert_eq!(display_shortcut_for("ctrl-alt-z", false), "Ctrl+Alt+Z");
        assert_eq!(display_shortcut_for("cmd-j", false), "Win+J");
        assert_eq!(display_shortcut_for("q", false), "Q");
    }

    #[test]
    fn split_is_offered_for_an_interior_node_or_two_ring_nodes() {
        let n = net();
        assert!(n.applicable(&n.nodes(&[4])).contains(&"split"));
        assert!(n.applicable(&n.nodes(&[2])).contains(&"split"));
        // Node 4 with a way that uses it, or one that doesn't.
        let with_way = [
            n.feature(FeatureKind::Node, 4),
            n.feature(FeatureKind::Way, 103),
        ];
        assert!(n.applicable(&with_way).contains(&"split"));
        let unrelated = [
            n.feature(FeatureKind::Node, 4),
            n.feature(FeatureKind::Way, 100),
        ];
        assert!(!n.applicable(&unrelated).contains(&"split"));
        // End nodes, a node on no way, nothing at all.
        assert!(!n.applicable(&n.nodes(&[1])).contains(&"split"));
        assert!(!n.applicable(&n.nodes(&[7])).contains(&"split"));
        assert!(!n.applicable(&[]).contains(&"split"));
        // Several nodes of an open way split it into several parts (an end
        // node among them is ignored); of a ring they are fine, and one
        // ring node alone only turns the ring so it starts there.
        assert!(n.applicable(&n.nodes(&[2, 3])).contains(&"split"));
        assert!(n.applicable(&n.nodes(&[14, 16])).contains(&"split"));
        assert!(!n.applicable(&n.nodes(&[14])).contains(&"split"));
        assert!(!n.applicable(&n.ways(&[100])).contains(&"split"));
    }

    #[test]
    fn split_at_a_shared_node_splits_every_way_and_copies_tags() {
        let mut n = net();
        let out = n.run("split", &n.nodes(&[4])).unwrap();
        assert_eq!(out.status, "Split 2 ways");
        assert!(out.select.is_none() && !out.clear_selection);
        // Equal halves: the originals keep their first part.
        assert_eq!(n.editable().way_node_ids(101), Some(vec![3, 4]));
        assert_eq!(n.editable().way_node_ids(103), Some(vec![8, 4]));
        // The new ways (negative ids) take the rest, and everything still
        // meets at node 4.
        let users = n.editable().ways_using_node(4);
        let new: Vec<i64> = users.iter().copied().filter(|&id| id < 0).collect();
        assert_eq!(new.len(), 2);
        assert_eq!(users.len(), 4);
        let mut rests: Vec<Vec<i64>> = new
            .iter()
            .map(|&id| n.editable().way_node_ids(id).unwrap())
            .collect();
        rests.sort();
        assert_eq!(rests, vec![vec![4, 5], vec![4, 9]]);
        // Each new way carries a copy of its original's tags.
        let tags_of_new: Vec<Vec<(String, String)>> = new
            .iter()
            .map(|&id| n.tags_of(FeatureKind::Way, id))
            .collect();
        assert!(tags_of_new.contains(&pairs(&[("highway", "residential"), ("name", "Main")])));
        assert!(tags_of_new.contains(&pairs(&[("highway", "footway")])));
        // The node itself is untouched.
        assert_eq!(
            n.tags_of(FeatureKind::Node, 4),
            pairs(&[("highway", "crossing")])
        );

        // Two new ways, two modified ones.
        assert_eq!(n.layer().diff_for_upload().counts(), (2, 2, 0));
        let UndoableAction::Batch { label, actions } = out.undo.as_ref().unwrap() else {
            panic!("expected one Batch step");
        };
        assert_eq!(label, "Split 2 ways");
        assert!(matches!(
            actions.as_slice(),
            [
                UndoableAction::CreateWay { .. },
                UndoableAction::SetWayNodes { .. },
                UndoableAction::CreateWay { .. },
                UndoableAction::SetWayNodes { .. }
            ]
        ));
    }

    #[test]
    fn split_with_a_way_selected_splits_only_that_way() {
        let mut n = net();
        let sel = [
            n.feature(FeatureKind::Node, 4),
            n.feature(FeatureKind::Way, 103),
        ];
        let out = n.run("split", &sel).unwrap();
        assert_eq!(out.status, "Split 1 way");
        assert_eq!(n.editable().way_node_ids(101), Some(vec![3, 4, 5]));
        assert_eq!(n.editable().way_node_ids(103), Some(vec![8, 4]));
        assert_eq!(n.layer().diff_for_upload().counts(), (1, 1, 0));
    }

    #[test]
    fn split_keeps_the_longer_part_in_the_original() {
        let mut n = net();
        // Make way 100 longer: [1,2,3,4,5]-ish by pointing it at more nodes.
        n.editable_mut().set_way_nodes(100, vec![1, 2, 3, 9, 10]);
        n.run("split", &n.nodes(&[2])).unwrap();
        assert_eq!(n.editable().way_node_ids(100), Some(vec![2, 3, 9, 10]));
        let new = n.editable().ways_using_node(1)[0];
        assert!(new < 0);
        assert_eq!(n.editable().way_node_ids(new), Some(vec![1, 2]));
    }

    #[test]
    fn split_a_ring_at_two_nodes_makes_two_open_ways() {
        let mut n = net();
        let out = n.run("split", &n.nodes(&[16, 14])).unwrap();
        assert_eq!(out.status, "Split 1 way");
        // Ring [14,15,16,14]: arcs 14-15-16 (3 nodes) and 16-14 (2).
        assert_eq!(n.editable().way_node_ids(108), Some(vec![14, 15, 16]));
        let new = n.editable().ways_using_node(14);
        assert_eq!(new.len(), 2);
        assert_eq!(n.editable().way_node_ids(new[0]), Some(vec![16, 14]));
        assert_eq!(
            n.tags_of(FeatureKind::Way, new[0]),
            pairs(&[("building", "yes")])
        );
        assert_eq!(n.layer().diff_for_upload().counts(), (1, 1, 0));
    }

    /// A relation with id 900 and the given `type`, over the test network.
    fn relation_900(kind: &str, members: &[(MemberType, i64, &str)]) -> OsmRelation {
        OsmRelation {
            id: 900,
            version: 1,
            tags: HashMap::from([("type".to_string(), kind.to_string())]),
            members: members
                .iter()
                .map(|&(member_type, reference, role)| OsmMember {
                    member_type,
                    reference,
                    role: role.to_string(),
                })
                .collect(),
        }
    }

    /// The network with relation 900 replaced by `relation`.
    fn net_with(relation: OsmRelation) -> Net {
        net_with_relations(vec![relation])
    }

    fn net_with_relations(relations: Vec<OsmRelation>) -> Net {
        let mut data = test_support::network_data();
        data.relations = relations;
        let layer_id = LayerId(1);
        let layer = OsmLayer::new_with_data(layer_id, "Net".to_string(), std::sync::Arc::new(data));
        let mut layers = LayerManager::new();
        layers.add_layer(Box::new(layer));
        Net { layers, layer_id }
    }

    impl Net {
        /// Relation 900's members as `(ref, role)`.
        fn members(&self) -> Vec<(i64, String)> {
            self.editable()
                .relation(900)
                .unwrap()
                .members
                .iter()
                .map(|m| (m.reference, m.role.clone()))
                .collect()
        }
        /// Node 4 plus a way through it: split just that way there.
        fn at_4_in(&self, way: i64) -> Vec<FeatureRef> {
            vec![
                self.feature(FeatureKind::Node, 4),
                self.feature(FeatureKind::Way, way),
            ]
        }
        fn relation_edits(&self) -> usize {
            self.layer().diff_for_upload().modified_relations.len()
        }
    }

    fn members_of(list: &[(i64, &str)]) -> Vec<(i64, String)> {
        list.iter().map(|&(r, s)| (r, s.to_string())).collect()
    }

    /// The id of the one way created by a split (the only negative id).
    fn the_new_way(n: &Net) -> i64 {
        let ids: Vec<i64> = n
            .layer()
            .diff_for_upload()
            .created_ways
            .iter()
            .map(|w| w.id)
            .collect();
        assert_eq!(ids.len(), 1, "{ids:?}");
        ids[0]
    }

    #[test]
    fn split_adds_the_new_way_to_the_relation_next_to_the_original() {
        use MemberType::Way;
        // Way 101 [3,4,5] splits at 4 into [3,4] (kept) and [4,5] (new). Way
        // 100 ends at 3, way 102 starts at 5.
        let mut n = net_with(relation_900(
            "route",
            &[(Way, 100, ""), (Way, 101, "forward"), (Way, 102, "")],
        ));
        let out = n.run("split", &n.at_4_in(101)).unwrap();
        let new = the_new_way(&n);
        // The new half follows the original: it is the one that meets 102.
        assert_eq!(
            n.members(),
            members_of(&[(100, ""), (101, "forward"), (new, "forward"), (102, "")])
        );
        assert_eq!(n.relation_edits(), 1);

        // One undo step, with the relation change last.
        let UndoableAction::Batch { actions, .. } = out.undo.as_ref().unwrap() else {
            panic!("expected one Batch step");
        };
        assert!(matches!(
            actions.as_slice(),
            [
                UndoableAction::CreateWay { .. },
                UndoableAction::SetWayNodes { .. },
                UndoableAction::SetRelationMembers { relation_id: 900, before, after, .. }
            ] if before.len() == 3 && after.len() == 4
        ));
    }

    #[test]
    fn split_puts_the_new_way_before_the_original_when_the_route_runs_backwards() {
        use MemberType::Way;
        let mut n = net_with(relation_900(
            "route",
            &[(Way, 102, ""), (Way, 101, ""), (Way, 100, "")],
        ));
        n.run("split", &n.at_4_in(101)).unwrap();
        let new = the_new_way(&n);
        assert_eq!(
            n.members(),
            members_of(&[(102, ""), (new, ""), (101, ""), (100, "")])
        );
    }

    #[test]
    fn split_inserts_after_when_the_neighbours_do_not_say() {
        use MemberType::{Node, Way};
        let mut n = net_with(relation_900("route", &[(Node, 7, "stop"), (Way, 101, "")]));
        n.run("split", &n.at_4_in(101)).unwrap();
        let new = the_new_way(&n);
        assert_eq!(
            n.members(),
            members_of(&[(7, "stop"), (101, ""), (new, "")])
        );
        // Neighbours that touch neither half count for nothing either.
        let mut n = net_with(relation_900("route", &[(Way, 106, ""), (Way, 101, "")]));
        n.run("split", &n.at_4_in(101)).unwrap();
        let new = the_new_way(&n);
        assert_eq!(n.members(), members_of(&[(106, ""), (101, ""), (new, "")]));
    }

    #[test]
    fn splitting_two_ways_of_one_relation_adds_both_halves() {
        use MemberType::Way;
        let mut n = net_with(relation_900("route", &[(Way, 103, ""), (Way, 101, "")]));
        // Node 4 is shared by ways 101 [3,4,5] and 103 [8,4,9].
        n.run("split", &n.nodes(&[4])).unwrap();
        let members = n.members();
        assert_eq!(members.len(), 4);
        assert_eq!(members[0].0, 103);
        assert_eq!(members[2].0, 101);
        assert!(members[1].0 < 0 && members[3].0 < 0 && members[1].0 != members[3].0);
        assert_eq!(n.relation_edits(), 1);
    }

    #[test]
    fn split_of_a_restriction_keeps_only_the_half_touching_the_via_node() {
        use MemberType::{Node, Way};
        // Way 101 [3,4,5]; the restriction's via node 5 is on the new half.
        let mut n = net_with(relation_900(
            "restriction",
            &[(Way, 101, "from"), (Node, 5, "via"), (Way, 102, "to")],
        ));
        n.run("split", &n.at_4_in(101)).unwrap();
        let new = the_new_way(&n);
        assert_eq!(
            n.members(),
            members_of(&[(new, "from"), (5, "via"), (102, "to")])
        );

        // Via node 3 is on the original's half: the relation is untouched.
        let mut n = net_with(relation_900(
            "restriction",
            &[(Way, 101, "to"), (Node, 3, "via"), (Way, 100, "from")],
        ));
        let out = n.run("split", &n.at_4_in(101)).unwrap();
        assert_eq!(n.relation_edits(), 0);
        let UndoableAction::Batch { actions, .. } = out.undo.as_ref().unwrap() else {
            panic!("expected one Batch step");
        };
        assert_eq!(actions.len(), 2, "no relation step");
    }

    #[test]
    fn split_of_a_restriction_finds_the_via_way_end() {
        use MemberType::Way;
        // Via way 102 [5,6] meets way 101 at node 5.
        let mut n = net_with(relation_900(
            "restriction:hgv",
            &[(Way, 101, "from"), (Way, 102, "via"), (Way, 105, "to")],
        ));
        n.run("split", &n.at_4_in(101)).unwrap();
        let new = the_new_way(&n);
        assert_eq!(n.members()[0], (new, "from".to_string()));
        assert_eq!(n.members().len(), 3);
    }

    #[test]
    fn split_refuses_a_restriction_whose_via_is_unknown_and_changes_nothing() {
        use MemberType::{Node, Way};
        for members in [
            // No via member.
            vec![(Way, 101, "from"), (Way, 102, "to")],
            // A via node that isn't on the way.
            vec![(Way, 101, "from"), (Node, 7, "via"), (Way, 102, "to")],
        ] {
            let mut n = net_with(relation_900("restriction", &members));
            let before = n.layer().diff_for_upload().counts();
            let Err(message) = n.run("split", &n.at_4_in(101)) else {
                panic!("expected a refusal");
            };
            assert_eq!(
                message,
                "Can't split: can't tell which part of way 101 belongs in turn restriction 900"
            );
            assert_eq!(n.layer().diff_for_upload().counts(), before);
            assert_eq!(n.editable().way_node_ids(101), Some(vec![3, 4, 5]));
            assert!(!n.layer().is_modified());
        }
    }

    #[test]
    fn splitting_a_closed_multipolygon_member_keeps_both_halves_with_the_role() {
        use MemberType::Way;
        // Ring 108 [14,15,16,14] split at 16 and 14.
        let mut n = net_with(relation_900(
            "multipolygon",
            &[(Way, 100, "inner"), (Way, 108, "outer")],
        ));
        n.run("split", &n.nodes(&[16, 14])).unwrap();
        let new = the_new_way(&n);
        assert_eq!(
            n.members(),
            members_of(&[(100, "inner"), (108, "outer"), (new, "outer")])
        );
    }

    #[test]
    fn split_explains_when_inapplicable() {
        let mut n = net();
        let Err(message) = n.run("split", &n.nodes(&[1])) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Select one or more nodes inside a way (two or more nodes of a closed way) to split it"
        );
        assert!(!n.layer().is_modified());
    }

    #[test]
    fn merge_is_offered_for_two_or_more_nodes() {
        let n = net();
        assert!(n.applicable(&n.nodes(&[1, 2])).contains(&"merge"));
        assert!(n.applicable(&n.nodes(&[1, 2, 3])).contains(&"merge"));
        assert!(!n.applicable(&n.nodes(&[1])).contains(&"merge"));
        assert!(!n.applicable(&n.nodes(&[1, 1])).contains(&"merge"));
        assert!(!n.applicable(&n.ways(&[100, 101])).contains(&"merge"));
        assert!(!n.applicable(&[]).contains(&"merge"));
    }

    #[test]
    fn merge_rewrites_ways_unions_tags_and_deletes_the_other_nodes() {
        let mut n = net();
        // Node 4 (a crossing) merged into the untagged node 8, selected first.
        let out = n.run("merge", &n.nodes(&[8, 4])).unwrap();
        assert_eq!(out.status, "Merged 2 nodes");
        assert_eq!(out.select, Some(n.nodes(&[8])));
        // 101 now passes through 8; 103 would have had 8 twice in a row.
        assert_eq!(n.editable().way_node_ids(101), Some(vec![3, 8, 5]));
        assert_eq!(n.editable().way_node_ids(103), Some(vec![8, 9]));
        assert_eq!(n.editable().node_lat_lon(4), None);
        // The survivor keeps its place and picks up the crossing tag.
        assert_eq!(
            n.tags_of(FeatureKind::Node, 8),
            pairs(&[("highway", "crossing")])
        );
        assert_eq!(
            n.editable().node_lat_lon(8),
            Some((40.0 + 0.0 * 0.001, -74.0 + 8.0 * 0.001))
        );

        // Node 4 deleted; node 8 (tag) and ways 101 and 103 modified.
        assert_eq!(n.layer().diff_for_upload().counts(), (0, 3, 1));
        let UndoableAction::Batch { label, actions } = out.undo.as_ref().unwrap() else {
            panic!("expected one Batch step");
        };
        assert_eq!(label, "Merged 2 nodes");
        assert!(matches!(
            actions.as_slice(),
            [
                UndoableAction::SetTags { .. },
                UndoableAction::SetWayNodes { .. },
                UndoableAction::SetWayNodes { .. },
                UndoableAction::DeleteFeature { .. }
            ]
        ));
    }

    #[test]
    fn merge_keeps_the_server_node_over_a_new_one() {
        let mut n = net();
        let fresh = n
            .layers
            .find_layer_mut(n.layer_id)
            .unwrap()
            .as_editable_mut()
            .unwrap()
            .create_node(40.5, -74.5, None)
            .unwrap();
        assert!(fresh < 0);
        // The new node is selected first but node 6 is on the server.
        let out = n.run("merge", &n.nodes(&[fresh, 6])).unwrap();
        assert_eq!(out.select, Some(n.nodes(&[6])));
        assert_eq!(n.editable().node_lat_lon(fresh), None);
        // Merging into an existing node leaves no trace of the new one.
        assert_eq!(n.layer().diff_for_upload().counts(), (0, 0, 0));
    }

    #[test]
    fn merge_survivor_that_is_shared_joins_ways() {
        let mut n = net();
        // Ways 102 [5,6] and 105 [5,10]: merge their far ends, 6 and 10.
        n.run("merge", &n.nodes(&[6, 10])).unwrap();
        assert_eq!(n.editable().way_node_ids(105), Some(vec![5, 6]));
        assert_eq!(n.editable().way_node_ids(102), Some(vec![5, 6]));
        assert_eq!(n.layer().diff_for_upload().counts(), (0, 1, 1));
    }

    #[test]
    fn merge_refuses_conflicting_tags_naming_the_key() {
        let mut n = net();
        n.tag(FeatureKind::Node, 8, "highway", "bus_stop");
        let Err(message) = n.run("merge", &n.nodes(&[8, 4])) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Can't merge: the nodes have different values for 'highway'"
        );
        assert_eq!(n.editable().way_node_ids(103), Some(vec![8, 4, 9]));
    }

    #[test]
    fn merge_refuses_to_leave_a_way_with_one_node() {
        let mut n = net();
        let Err(message) = n.run("merge", &n.nodes(&[5, 10])) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Can't merge: way 105 would be left with fewer than two distinct nodes"
        );
        assert!(!n.layer().is_modified());
    }

    #[test]
    fn merge_moves_a_relation_membership_to_the_survivor() {
        let mut n = net();
        // Node 7 is a "stop" in relation 900 (with way 102); with 6 selected
        // first, 7 is the one removed.
        let out = n.run("merge", &n.nodes(&[6, 7])).unwrap();
        assert_eq!(n.members(), members_of(&[(102, ""), (6, "stop")]));
        assert_eq!(n.editable().node_lat_lon(7), None);
        // Relation 900, the way untouched, the node gone.
        assert_eq!(n.layer().diff_for_upload().counts(), (0, 1, 1));
        let UndoableAction::Batch { actions, .. } = out.undo.as_ref().unwrap() else {
            panic!("expected one Batch step");
        };
        assert!(matches!(
            actions.as_slice(),
            [
                UndoableAction::SetRelationMembers { relation_id: 900, before, after, .. },
                UndoableAction::DeleteFeature { .. }
            ] if before.len() == 2 && after.len() == 2
        ));
        // Selected the other way round, the member survives unchanged.
        let mut n = net();
        n.run("merge", &n.nodes(&[7, 6])).unwrap();
        assert_eq!(n.members(), members_of(&[(102, ""), (7, "stop")]));
        assert_eq!(n.editable().way_node_ids(102), Some(vec![5, 7]));
    }

    #[test]
    fn merge_drops_an_adjacent_duplicate_membership_it_creates() {
        use MemberType::{Node, Way};
        let mut n = net_with(relation_900(
            "route",
            &[
                (Node, 6, "stop"),
                (Node, 7, "stop"),
                (Way, 102, ""),
                (Node, 7, "platform"),
            ],
        ));
        n.run("merge", &n.nodes(&[6, 7])).unwrap();
        // 6/7 collapse where role and neighbours match; the platform keeps
        // its own role.
        assert_eq!(
            n.members(),
            members_of(&[(6, "stop"), (102, ""), (6, "platform")])
        );
    }

    #[test]
    fn merge_leaves_unrelated_relations_alone() {
        use MemberType::Way;
        let mut n = net_with(relation_900("route", &[(Way, 100, "")]));
        n.run("merge", &n.nodes(&[6, 7])).unwrap();
        assert_eq!(n.relation_edits(), 0);
    }

    #[test]
    fn straighten_is_offered_for_open_ways_with_three_nodes() {
        let n = net();
        assert!(n.applicable(&n.ways(&[100])).contains(&"straighten"));
        assert!(n.applicable(&n.ways(&[100, 102])).contains(&"straighten"));
        // Two nodes, a ring, or no way at all: nothing to straighten.
        assert!(!n.applicable(&n.ways(&[102])).contains(&"straighten"));
        assert!(!n.applicable(&n.ways(&[108])).contains(&"straighten"));
        assert!(!n.applicable(&[]).contains(&"straighten"));
        assert!(!n
            .applicable(&[n.feature(FeatureKind::Node, 2)])
            .contains(&"straighten"));
    }

    #[test]
    fn straighten_puts_the_wobbly_node_back_on_the_line_in_one_move_step() {
        let mut n = net();
        // Way 100 is a straight line; push its middle node ~5 m off it.
        let straight = n.editable().node_lat_lon(2).unwrap();
        n.nudge(2, straight.0 + 0.00005, straight.1);
        let out = n.run("straighten", &n.ways(&[100])).unwrap();
        assert_eq!(out.status, "Straightened 1 way");
        let UndoableAction::MoveNodes { per_layer } = out.undo.as_ref().unwrap() else {
            panic!("expected one MoveNodes step");
        };
        // Only the interior node moved: from the nudge back onto the line.
        assert_eq!(per_layer[0].1.len(), 1);
        let (id, before, after) = per_layer[0].1[0];
        assert_eq!(id, 2);
        assert_eq!(before, (straight.0 + 0.00005, straight.1));
        // Projected onto the line through nodes 1 and 3 (the nudge was
        // sideways to a diagonal line, so it also slides along it a little).
        let (a, c) = (
            n.editable().node_lat_lon(1).unwrap(),
            n.editable().node_lat_lon(3).unwrap(),
        );
        let k = ((a.0 + c.0) / 2.0).to_radians().cos();
        let cross = (after.1 - a.1) * k * (c.0 - a.0) - (after.0 - a.0) * (c.1 - a.1) * k;
        assert!(cross.abs() < 1e-11, "not on the line: {cross}");
        assert!((after.0 - straight.0).abs() < 1e-4);
        assert_eq!(n.editable().node_lat_lon(2), Some(after));
        // The way itself is untouched.
        assert_eq!(n.editable().way_node_ids(100), Some(vec![1, 2, 3]));
        // Straight now: running again has nothing to do.
        assert_eq!(
            n.run("straighten", &n.ways(&[100])).err().as_deref(),
            Some("The selected ways are already straight")
        );
    }

    #[test]
    fn straighten_refuses_a_zigzag_and_changes_nothing() {
        let mut n = net();
        let Err(message) = n.run("straighten", &n.ways(&[101])) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Can't straighten: the way bends more than 10% of its length"
        );
        assert!(!n.layer().is_modified());
        // One curved way among several refuses all of them.
        assert!(n.run("straighten", &n.ways(&[100, 101])).is_err());
        assert!(!n.layer().is_modified());
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

    // -- server size limits (default limits: 2,000 way nodes, 32,000 members) --

    /// Way 100 reshaped to `len` nodes that start at node 1 and end at node 3
    /// (the node it shares with way 101), the middle being node 2 repeated.
    fn long_way_100(n: &mut Net, len: usize) {
        let mut nodes = vec![1];
        nodes.extend(std::iter::repeat_n(2, len - 2));
        nodes.push(3);
        assert!(n.editable_mut().set_way_nodes(100, nodes));
    }

    #[test]
    fn join_is_refused_when_the_result_would_pass_the_way_node_limit() {
        // 100 + 101 [3,4,5] share node 3, so the join is len + 2 nodes.
        let mut n = net();
        long_way_100(&mut n, 1999);
        let Err(message) = n.run("join", &n.ways(&[100, 101])) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Can't join: the combined way would have 2,001 nodes; the server allows 2,000"
        );
        // Nothing changed: both ways are still there.
        assert_eq!(n.editable().way_node_ids(100).unwrap().len(), 1999);
        assert!(n.editable().way_node_ids(101).is_some());

        // Exactly at the limit is fine.
        let mut n = net();
        long_way_100(&mut n, 1998);
        n.run("join", &n.ways(&[100, 101])).unwrap();
        assert_eq!(n.editable().way_node_ids(100).unwrap().len(), 2000);
    }

    #[test]
    fn an_already_oversized_way_can_still_be_edited_otherwise() {
        let mut n = net();
        long_way_100(&mut n, 2001);
        n.run("reverse", &n.ways(&[100])).unwrap();
        assert_eq!(n.editable().way_node_ids(100).unwrap()[0], 3);
    }

    fn members_of_kind(count: usize) -> Vec<osm_gpui::osm::OsmMember> {
        (0..count)
            .map(|i| osm_gpui::osm::OsmMember {
                member_type: MemberType::Node,
                reference: 1000 + i as i64,
                role: String::new(),
            })
            .collect()
    }

    #[test]
    fn add_to_relation_is_refused_past_the_member_limit() {
        // Relation 900 with 31,999 members: one more is the limit, two is over.
        let mut n = net_with(relation_900("route", &[]));
        n.editable_mut()
            .set_relation_members(900, members_of_kind(31_999));
        let node1 = n.feature(FeatureKind::Node, 1);
        let node2 = n.feature(FeatureKind::Node, 2);

        let result = add_to_relation(&mut n.layers, &[node1, node2], Some(n.layer_id), 900);
        assert_eq!(
            result.err().as_deref(),
            Some(
                "Can't add to the relation: it would have 32,001 members; the server allows 32,000"
            )
        );
        assert_eq!(n.editable().relation(900).unwrap().members.len(), 31_999);

        add_to_relation(&mut n.layers, &[node1], Some(n.layer_id), 900).unwrap();
        assert_eq!(n.editable().relation(900).unwrap().members.len(), 32_000);
    }

    #[test]
    fn new_relation_is_refused_past_the_member_limit() {
        let mut n = net();
        let existing = n.editable().relation_ids().len();
        let mut selection = Vec::new();
        for i in 0..32_001 {
            let id = n.editable_mut().add_node(1.0 + i as f64 * 1e-6, 2.0);
            selection.push(n.feature(FeatureKind::Node, id));
        }
        let Err(message) = n.run("new_relation", &selection) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "Can't create the relation: 32,001 members is more than the server allows (32,000)"
        );
        assert_eq!(n.editable().relation_ids().len(), existing);

        selection.pop();
        n.run("new_relation", &selection).unwrap();
        assert_eq!(n.editable().relation_ids().len(), existing + 1);
    }

    #[test]
    fn split_is_refused_when_a_full_relation_could_not_take_the_new_half() {
        use MemberType::Way;
        let mut members = vec![(Way, 101, "")];
        members.extend(std::iter::repeat_n((Way, 100, ""), 31_999));
        let mut n = net_with(relation_900("route", &members));
        let Err(message) = n.run("split", &n.at_4_in(101)) else {
            panic!("expected a refusal");
        };
        assert!(
            message.contains("32,001") && message.contains("32,000"),
            "{message}"
        );
        // Changed nothing: no new way, way 101 intact.
        assert_eq!(n.editable().way_node_ids(101), Some(vec![3, 4, 5]));
        assert!(!n.layer().is_modified());
    }

    fn parallel(
        n: &mut Net,
        ids: &[i64],
        params: osm_gpui::parallel::ParallelParams,
    ) -> Result<ActionOutcome, String> {
        let selection = n.ways(ids);
        create_parallel_ways(&mut n.layers, &selection, Some(n.layer_id), params)
    }

    #[test]
    fn parallel_way_is_new_unconnected_tagged_and_selected() {
        let mut n = net();
        let out = parallel(&mut n, &[100], Default::default()).unwrap();
        assert_eq!(out.status, "Created 1 parallel way");
        let select = out.select.clone().unwrap();
        assert_eq!(select.len(), 1);
        let new_id = select[0].id;
        assert!(new_id < 0);
        let nodes = n.editable().way_node_ids(new_id).unwrap();
        assert_eq!(nodes.len(), 3);
        assert!(nodes.iter().all(|&id| id < 0));
        // The original is untouched, and the copy shares no node with it.
        assert_eq!(n.editable().way_node_ids(100), Some(vec![1, 2, 3]));
        let tags = n.editable().feature_tags(&select[0]).unwrap();
        assert!(tags.contains(&("highway".to_string(), "residential".to_string())));
        // One undo step: nodes first, then the way.
        let Some(UndoableAction::Batch { actions, .. }) = &out.undo else {
            panic!("expected one Batch step");
        };
        assert!(matches!(
            actions.as_slice(),
            [
                UndoableAction::CreateNode { .. },
                UndoableAction::CreateNode { .. },
                UndoableAction::CreateNode { .. },
                UndoableAction::CreateWay { .. },
            ]
        ));
        // 5 m to the right of the first segment, in metres on the ground.
        let (a, b) = (
            n.editable().node_lat_lon(1).unwrap(),
            n.editable().node_lat_lon(2).unwrap(),
        );
        let (c, d) = (
            n.editable().node_lat_lon(nodes[0]).unwrap(),
            n.editable().node_lat_lon(nodes[1]).unwrap(),
        );
        let p = geometry::LocalProjection::new(a);
        let (a, b, c, d) = (p.project(a), p.project(b), p.project(c), p.project(d));
        assert!(((c.0 - a.0).hypot(c.1 - a.1) - 5.0).abs() < 0.05);
        let along = ((b.0 - a.0), (b.1 - a.1));
        let right = (along.1, -along.0);
        assert!((c.0 - a.0) * right.0 + (c.1 - a.1) * right.1 > 0.0);
        assert!(
            ((d.0 - c.0) * along.1 - (d.1 - c.1) * along.0).abs() < 0.05 * along.0.hypot(along.1)
        );
    }

    #[test]
    fn parallel_way_can_skip_tags_reverse_and_go_left() {
        let mut n = net();
        let params = osm_gpui::parallel::ParallelParams {
            offset_m: -8.0,
            copy_tags: false,
            reverse: true,
        };
        let out = parallel(&mut n, &[101], params).unwrap();
        let new_way = out.select.unwrap()[0];
        assert!(n.editable().feature_tags(&new_way).unwrap().is_empty());
        let nodes = n.editable().way_node_ids(new_way.id).unwrap();
        // Reversed: the copy starts beside original node 5, not node 3.
        let first = n.editable().node_lat_lon(nodes[0]).unwrap();
        let near = |id: i64| {
            let o = n.editable().node_lat_lon(id).unwrap();
            ((first.0 - o.0) * 111_000.0).hypot((first.1 - o.1) * 85_000.0)
        };
        assert!(near(5) < near(3));
    }

    #[test]
    fn parallel_of_a_ring_is_a_ring_and_leaves_relations_alone() {
        let mut n = net();
        let before = n.editable().relation(900).unwrap();
        let out = parallel(&mut n, &[108, 102], Default::default()).unwrap();
        assert_eq!(out.status, "Created 2 parallel ways");
        let new_ways = out.select.unwrap();
        let ring = n.editable().way_node_ids(new_ways[0].id).unwrap();
        assert_eq!(ring.first(), ring.last());
        assert_eq!(ring.len(), 4);
        assert_eq!(n.editable().relation(900).unwrap(), before);
        assert!(n
            .editable()
            .relations_containing_way(new_ways[1].id)
            .is_empty());
    }

    #[test]
    fn parallel_needs_a_way_with_two_different_nodes() {
        let n = net();
        assert!(!n
            .applicable(&[n.feature(FeatureKind::Node, 1)])
            .contains(&"parallel"));
        assert!(n.applicable(&n.ways(&[100])).contains(&"parallel"));
        let mut n = net();
        // Collapse way 102's two nodes onto one point: nothing to offset.
        let (lat, lon) = n.editable().node_lat_lon(5).unwrap();
        n.layers
            .find_layer_mut(n.layer_id)
            .unwrap()
            .as_editable_mut()
            .unwrap()
            .commit_node_moves(&[(6, lat, lon)]);
        let result = parallel(&mut n, &[102], Default::default());
        assert!(result.is_err());
        assert_eq!(n.editable().way_node_ids(102), Some(vec![5, 6]));
    }

    fn nodes(n: &Net, ids: &[i64]) -> Vec<FeatureRef> {
        ids.iter()
            .map(|&id| n.feature(FeatureKind::Node, id))
            .collect()
    }

    fn positions(n: &Net, ids: &[i64]) -> Vec<LatLon> {
        ids.iter()
            .map(|&id| n.editable().node_lat_lon(id).unwrap())
            .collect()
    }

    #[test]
    fn align_and_distribute_are_offered_for_three_or_more_nodes() {
        let n = net();
        assert!(!n.applicable(&nodes(&n, &[1, 2])).contains(&"align"));
        assert!(n.applicable(&nodes(&n, &[1, 2, 4])).contains(&"align"));
        assert!(n.applicable(&nodes(&n, &[1, 2, 4])).contains(&"distribute"));
        // A way alone is Straighten's job.
        assert!(!n.applicable(&n.ways(&[100])).contains(&"align"));
        assert!(n.applicable(&n.ways(&[100])).contains(&"straighten"));
    }

    #[test]
    fn align_keeps_the_anchors_moves_the_rest_and_leaves_relations_alone() {
        let mut n = net();
        let ids = [7, 2, 6, 11, 5];
        let relation = n.editable().relation(900).unwrap();
        let before = positions(&n, &ids);
        let out = n.run("align", &nodes(&n, &ids)).unwrap();
        assert!(matches!(out.undo, Some(UndoableAction::MoveNodes { .. })));
        let after = positions(&n, &ids);
        assert_ne!(after, before);
        // Collinear within a millimetre.
        let proj = geometry::LocalProjection::new(after[0]);
        let pts: Vec<(f64, f64)> = after.iter().map(|&p| proj.project(p)).collect();
        let (a, b) = pts
            .iter()
            .flat_map(|p| pts.iter().map(move |q| (*p, *q)))
            .max_by(|x, y| {
                let d = |(p, q): &((f64, f64), (f64, f64))| (p.0 - q.0).hypot(p.1 - q.1);
                d(x).total_cmp(&d(y))
            })
            .unwrap();
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        for p in &pts {
            let off = ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / dx.hypot(dy);
            assert!(off < 1e-3, "{off}");
        }
        // Exactly two nodes (the anchors) did not move.
        let unmoved = before.iter().zip(&after).filter(|(b, a)| b == a).count();
        assert_eq!(unmoved, 2);
        assert_eq!(n.editable().relation(900).unwrap(), relation);
        assert!(n.layer().is_modified());
    }

    #[test]
    fn distribute_refuses_nodes_in_one_place_and_aligned_input_is_a_no_op() {
        let mut n = net();
        // Nodes 1, 2, 3 are already evenly spaced on a line.
        let Err(message) = n.run("distribute", &nodes(&n, &[1, 2, 3])) else {
            panic!("expected a refusal");
        };
        assert_eq!(message, "The selected nodes are already in place");
        assert!(!n.layer().is_modified());
        // Collapse three nodes onto one point.
        let p = n.editable().node_lat_lon(1).unwrap();
        n.layers
            .find_layer_mut(n.layer_id)
            .unwrap()
            .as_editable_mut()
            .unwrap()
            .commit_node_moves(&[(2, p.0, p.1), (3, p.0, p.1)]);
        let Err(message) = n.run("align", &nodes(&n, &[1, 2, 3])) else {
            panic!("expected a refusal");
        };
        assert!(message.starts_with("Can't align"), "{message}");
    }
}
