//! Copying, pasting and duplicating tags and features, and their key bindings.

use super::{
    node_position, pairs, pending, redo_step, select, setup_network, status, tags_of, undo_step,
    way_nodes, ways_at_node,
};
use crate::{
    key_binding_for, key_bindings_from_settings, HideApp, HideOtherApps, MapViewer, MinimizeWindow,
    OpenSettings, Quit, RunSelectionAction, MAP_VIEWER_KEY_CONTEXT,
};
use gpui::{point, px, Context, KeyBinding, TestAppContext, Window};
use osm_gpui::coordinates::LatLon;
use osm_gpui::coordinates::Mercator;
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::settings_store;
use osm_gpui::transfer::{TransferHandle, TransferKind, TransferPhase};
use std::sync::Arc;

#[gpui::test]
fn copy_then_paste_tags_is_one_undo_step_and_redoable(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            // Copying leaves no undo step and no edit behind.
            select(view, Way, &[101]);
            view.run_selection_action("copy_tags", cx);
            assert!(view.undo_stack.actions.is_empty());
            assert_eq!(pending(view), (0, 0, 0));
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("Copied 2 tags")
            );

            // Paste onto a way and a node at once.
            let original_way = tags_of(view, Way, 103);
            select(view, Way, &[103]);
            let layer_id = view.active_layer.unwrap();
            view.selected.push(osm_gpui::selection::FeatureRef {
                layer_id,
                kind: Node,
                id: 8,
            });
            view.run_selection_action("paste_tags", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            let pasted = pairs(&[("highway", "residential"), ("name", "Main")]);
            assert_eq!(tags_of(view, Way, 103), pasted);
            assert_eq!(tags_of(view, Node, 8), pasted);
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("Pasted 2 tags onto 2 features")
            );
            assert_eq!(pending(view), (0, 2, 0));

            undo_step(view);
            assert_eq!(tags_of(view, Way, 103), original_way);
            assert!(tags_of(view, Node, 8).is_empty());
            assert_eq!(pending(view), (0, 0, 0));
            redo_step(view);
            assert_eq!(tags_of(view, Way, 103), pasted);
            assert_eq!(tags_of(view, Node, 8), pasted);
        })
        .unwrap();
}

fn ids_of(
    view: &MapViewer,
    kind: osm_gpui::selection::FeatureKind,
) -> std::collections::BTreeSet<i64> {
    let layer_id = view.active_layer.unwrap();
    view.layer_manager
        .find_layer(layer_id)
        .unwrap()
        .as_editable()
        .unwrap()
        .all_features()
        .into_iter()
        .filter(|f| f.kind == kind)
        .map(|f| f.id)
        .collect()
}

/// Run a history step the way Undo/Redo do, selection included.
fn history_step(view: &mut MapViewer, forward: bool) {
    let action = if forward {
        view.undo_stack.redo()
    } else {
        view.undo_stack.undo()
    }
    .expect("a step");
    view.run_history_step(&action, forward);
}

#[gpui::test]
fn copy_and_paste_features_is_one_undo_step_with_the_same_ids_on_redo(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            // Copy changes nothing: no undo step, no pending edit.
            select(view, Way, &[100, 101]);
            view.run_selection_action("copy_features", cx);
            assert!(view.undo_stack.actions.is_empty());
            assert_eq!(pending(view), (0, 0, 0));
            assert_eq!(status(view), Some("Copied 7 features"));
            // The system clipboard got the text form.
            let text = cx.read_from_clipboard().and_then(|item| item.text());
            assert!(text.is_some_and(|t| t.contains("<way id=\"-")));

            // Paste with the pointer at a spot on the map.
            let nodes_before = ids_of(view, Node);
            let ways_before = ids_of(view, Way);
            let pointer = point(px(500.0), px(200.0));
            view.pointer.last_mouse_pos = Some(pointer);
            view.run_selection_action("paste_features", cx);
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(status(view), Some("Pasted 7 features"));
            assert_eq!(ids_of(view, Node).len(), nodes_before.len() + 5);
            assert_eq!(ids_of(view, Way).len(), ways_before.len() + 2);
            let new_ways: Vec<i64> = ids_of(view, Way)
                .difference(&ways_before)
                .copied()
                .collect();
            assert!(new_ways.iter().all(|id| *id < 0));
            // New data, and the pasted ways are what is selected.
            assert_eq!(pending(view), (5 + 2, 0, 0));
            let mut selected: Vec<i64> = view.selected.iter().map(|f| f.id).collect();
            selected.sort();
            let mut want = new_ways.clone();
            want.sort();
            assert_eq!(selected, want);

            // The copy's centre is where the pointer was.
            let layer_id = view.active_layer.unwrap();
            let pasted_nodes: Vec<i64> = ids_of(view, Node)
                .difference(&nodes_before)
                .copied()
                .collect();
            let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
            for id in &pasted_nodes {
                let (lat, lon) = node_position(view, *id);
                let Mercator { x, y } =
                    osm_gpui::coordinates::lat_lon_to_mercator(LatLon::new(lat, lon));
                lo = (lo.0.min(x), lo.1.min(y));
                hi = (hi.0.max(x), hi.1.max(y));
            }
            let LatLon {
                lat: want_lat,
                lon: want_lon,
            } = view.viewport.screen_to_geo(pointer);
            let (cx_m, cy_m) = ((lo.0 + hi.0) / 2.0, (lo.1 + hi.1) / 2.0);
            let Mercator {
                x: want_x,
                y: want_y,
            } = osm_gpui::coordinates::lat_lon_to_mercator(LatLon::new(want_lat, want_lon));
            assert!((cx_m - want_x).abs() < 0.01 && (cy_m - want_y).abs() < 0.01);
            // Two ways that met at node 3 still meet at one new node.
            let meets: Vec<_> = pasted_nodes
                .iter()
                .filter(|n| ways_at_node(view, **n).len() == 2)
                .collect();
            assert_eq!(meets.len(), 1);

            // One undo removes it all and restores the old selection.
            history_step(view, false);
            assert_eq!(ids_of(view, Node), nodes_before);
            assert_eq!(ids_of(view, Way), ways_before);
            assert_eq!(pending(view), (0, 0, 0));
            assert_eq!(
                view.selected,
                vec![
                    osm_gpui::selection::FeatureRef {
                        layer_id,
                        kind: Way,
                        id: 100
                    },
                    osm_gpui::selection::FeatureRef {
                        layer_id,
                        kind: Way,
                        id: 101
                    },
                ]
            );

            // Redo brings back exactly the same ids and selects them.
            history_step(view, true);
            assert_eq!(
                ids_of(view, Way)
                    .difference(&ways_before)
                    .copied()
                    .collect::<Vec<_>>(),
                new_ways
            );
            let mut selected: Vec<i64> = view.selected.iter().map(|f| f.id).collect();
            selected.sort();
            assert_eq!(selected, want);
        })
        .unwrap();
}

#[gpui::test]
fn duplicate_pastes_at_the_source_and_leaves_the_clipboard_alone(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            select(view, Way, &[102]);
            view.run_selection_action("copy_features", cx);
            let copied = view.clipboards.features.clone();

            select(view, Way, &[103]);
            let ways_before = ids_of(view, Way);
            view.run_selection_action("duplicate_features", cx);
            assert_eq!(status(view), Some("Duplicated 4 features"));
            assert_eq!(view.undo_stack.actions.len(), 1);
            assert_eq!(
                view.undo_stack.actions[0].description(),
                "Duplicated features"
            );
            // The duplicate is selected, in the same place as the way.
            let [dup] = view.selected.as_slice() else {
                panic!("one feature selected");
            };
            assert!(dup.id < 0 && dup.kind == Way);
            assert!(!ways_before.contains(&dup.id));
            let layer_id = view.active_layer.unwrap();
            let editable = view
                .layer_manager
                .find_layer(layer_id)
                .unwrap()
                .as_editable()
                .unwrap();
            let at = |way: i64| -> Vec<(f64, f64)> {
                editable
                    .way_node_ids(way)
                    .unwrap()
                    .iter()
                    .map(|n| editable.node_lat_lon(*n).unwrap())
                    .collect()
            };
            assert_eq!(at(dup.id), at(103));
            // Duplicating does not touch what Copy put on the clipboard.
            assert_eq!(view.clipboards.features.clone(), copied);
            // Drag the duplicate away: the original stays where it was.
            let first = editable.way_node_ids(dup.id).unwrap()[0];
            assert!(first < 0 && editable.node_lat_lon(8).is_some());
            assert_ne!(first, 8);
            let _ = Node;
        })
        .unwrap();
}

#[gpui::test]
fn paste_at_source_does_not_move_and_works_across_layers(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let first = view.active_layer.unwrap();
            select(view, Way, &[101]);
            view.run_selection_action("copy_features", cx);
            let original: Vec<(f64, f64)> =
                [3, 4, 5].iter().map(|n| node_position(view, *n)).collect();

            // A second, empty data layer becomes the active one.
            let second = view.layer_manager.alloc_id();
            view.layer_manager
                .add_layer(Box::new(OsmLayer::new_with_data(
                    second,
                    "Second".to_string(),
                    Arc::new(osm_gpui::osm::OsmData {
                        nodes: Default::default(),
                        ways: Default::default(),
                        relations: Vec::new(),
                        bounds: Vec::new(),
                    }),
                )));
            view.active_layer = Some(second);
            view.run_selection_action("paste_features_source", cx);
            assert_eq!(status(view), Some("Pasted 4 features"));
            assert_eq!(ids_of(view, Way).len(), 1);
            assert_eq!(ids_of(view, Node).len(), 3);
            let [pasted] = view.selected.as_slice() else {
                panic!("one feature selected");
            };
            assert_eq!(pasted.layer_id, second);
            let nodes = way_nodes(view, second, pasted.id);
            let positions: Vec<(f64, f64)> =
                nodes.iter().map(|n| node_position(view, *n)).collect();
            assert_eq!(positions, original);
            // The step belongs to the second layer only.
            assert_eq!(view.undo_stack.actions[0].layers(), vec![second]);
            assert_eq!(pending(view), (4, 0, 0));

            // Undo in the second layer; the first never changed.
            history_step(view, false);
            assert!(ids_of(view, Way).is_empty());
            view.active_layer = Some(first);
            assert_eq!(pending(view), (0, 0, 0));
            assert_eq!(way_nodes(view, first, 101), vec![3, 4, 5]);
        })
        .unwrap();
}

#[gpui::test]
fn the_feature_clipboard_survives_deleting_its_source_layer(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            let first = view.active_layer.unwrap();
            select(view, Way, &[101]);
            view.run_selection_action("copy_features", cx);
            let index = view
                .layer_manager
                .layers()
                .iter()
                .position(|l| l.id() == first)
                .unwrap();
            view.layer_manager.remove_at(index);
            let second = view.layer_manager.alloc_id();
            view.layer_manager
                .add_layer(Box::new(OsmLayer::new_with_data(
                    second,
                    "Second".to_string(),
                    Arc::new(osm_gpui::osm::OsmData {
                        nodes: Default::default(),
                        ways: Default::default(),
                        relations: Vec::new(),
                        bounds: Vec::new(),
                    }),
                )));
            view.active_layer = Some(second);
            view.run_selection_action("paste_features_source", cx);
            assert_eq!(ids_of(view, Way).len(), 1);
        })
        .unwrap();
}

#[gpui::test]
fn pasting_is_refused_during_an_upload_but_copying_is_not(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, window, cx| {
            select(view, Way, &[101]);
            view.transfer = Some(TransferHandle::new(
                TransferKind::Upload,
                TransferPhase::OpeningChangeset,
            ));
            let blocked = "An upload is in progress. Wait for it to finish before editing.";
            let run = |view: &mut MapViewer,
                       window: &mut Window,
                       cx: &mut Context<MapViewer>,
                       id: &str| {
                view.on_run_selection_action(&RunSelectionAction { id: id.into() }, window, cx);
            };
            run(view, window, cx, "copy_features");
            assert_eq!(status(view), Some("Copied 4 features"));
            for id in [
                "paste_features",
                "paste_features_source",
                "duplicate_features",
            ] {
                view.status_message = None;
                run(view, window, cx, id);
                assert_eq!(status(view), Some(blocked), "{id}");
                assert!(view.undo_stack.actions.is_empty());
                assert_eq!(pending(view), (0, 0, 0));
            }
            // Even called straight (a script's `action`), the paste checks.
            view.status_message = None;
            view.run_selection_action("paste_features", cx);
            assert_eq!(status(view), Some(blocked));
            assert_eq!(pending(view), (0, 0, 0));
        })
        .unwrap();
}

#[gpui::test]
fn paste_and_duplicate_explain_themselves_when_they_cannot_run(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _window, cx| {
            // Nothing copied: Paste says what to do first.
            select(view, Way, &[101]);
            view.run_selection_action("paste_features", cx);
            assert_eq!(
                status(view),
                Some("Copy some features first, then paste them into a data layer")
            );
            assert!(view.undo_stack.actions.is_empty());
            // Nothing selected: Copy and Duplicate say so.
            view.set_selection(Vec::new());
            view.run_selection_action("copy_features", cx);
            assert_eq!(
                status(view),
                Some("Select nodes, ways or relations to copy")
            );
            view.run_selection_action("duplicate_features", cx);
            assert_eq!(
                status(view),
                Some("Select nodes, ways or relations to duplicate")
            );
            // Something copied but no data layer to paste into.
            select(view, Way, &[101]);
            view.run_selection_action("copy_features", cx);
            view.active_layer = None;
            view.run_selection_action("paste_features", cx);
            assert_eq!(status(view), Some("Open or download OSM data first"));
            assert!(view.undo_stack.actions.is_empty());
        })
        .unwrap();
}

#[test]
fn feature_copy_and_paste_use_the_standard_shortcuts() {
    let settings = settings_store::AppSettings::default();
    for (id, spec) in [
        ("copy_features", "secondary-c"),
        ("paste_features", "secondary-v"),
        ("duplicate_features", "secondary-d"),
        ("copy_tags", "secondary-shift-c"),
        ("paste_tags", "secondary-shift-v"),
    ] {
        assert_eq!(osm_gpui::keybindings::effective_spec(&settings, id), spec);
    }
    // Download keeps Shift+Cmd/Ctrl+D; Duplicate does not clash with it.
    assert_eq!(
        osm_gpui::keybindings::effective_spec(&settings, "download_osm"),
        "secondary-shift-d"
    );
    // Bound on the map's key context, so a focused text field keeps its
    // own copy and paste.
    let bindings = key_bindings_from_settings(&settings, true);
    for id in ["copy_features", "paste_features", "duplicate_features"] {
        let binding = bindings
            .iter()
            .find(|b| {
                b.action()
                    .as_any()
                    .downcast_ref::<RunSelectionAction>()
                    .is_some_and(|a| a.id == id)
            })
            .expect("bound");
        assert!(
            format!("{binding:?}").contains(MAP_VIEWER_KEY_CONTEXT),
            "{id} is not scoped to the map"
        );
    }
}

#[test]
fn tag_copy_and_paste_are_modifier_key_bindings_on_the_map_context() {
    let bindings = key_bindings_from_settings(&settings_store::AppSettings::default(), true);
    let ids: Vec<String> = bindings
        .iter()
        .filter_map(|b| b.action().as_any().downcast_ref::<RunSelectionAction>())
        .map(|a| a.id.to_string())
        .collect();
    assert_eq!(
        ids,
        [
            "copy_features",
            "paste_features",
            "duplicate_features",
            "copy_tags",
            "paste_tags"
        ]
    );
}

#[test]
fn macos_only_fixed_bindings_are_registered_only_on_macos() {
    let has = |bindings: &[KeyBinding], f: fn(&dyn gpui::Action) -> bool| {
        bindings.iter().any(|b| f(b.action()))
    };
    let settings = settings_store::AppSettings::default();
    let mac = key_bindings_from_settings(&settings, true);
    let other = key_bindings_from_settings(&settings, false);
    assert!(has(&mac, |a| a.as_any().is::<MinimizeWindow>()));
    assert!(has(&mac, |a| a.as_any().is::<HideApp>()));
    assert!(has(&mac, |a| a.as_any().is::<HideOtherApps>()));
    assert!(!has(&other, |a| a.as_any().is::<MinimizeWindow>()));
    assert!(!has(&other, |a| a.as_any().is::<HideApp>()));
    assert!(!has(&other, |a| a.as_any().is::<HideOtherApps>()));
    // The functional shortcuts are unaffected.
    assert!(has(&other, |a| a.as_any().is::<Quit>()));
    assert!(has(&other, |a| a.as_any().is::<OpenSettings>()));
}

/// A shortcut listed in Settings must do something: modifier shortcuts get
/// a `KeyBinding`; only the bare-key categories are matched by the map's
/// key handler instead.
#[test]
fn every_listed_shortcut_is_bound_or_handled_as_a_bare_key() {
    for def in osm_gpui::keybindings::SHORTCUTS {
        let bound = key_binding_for(def.id, def.default_spec).is_some();
        if def.category.is_bare_key_category() {
            assert!(
                !bound,
                "{} is a bare key handled by the map but also has a KeyBinding",
                def.id
            );
            assert!(
                osm_gpui::keybindings::is_bare_key(def.default_spec),
                "{} is in a bare-key category but its default has a modifier",
                def.id
            );
        } else {
            assert!(
                bound,
                "shortcut `{}` is listed in Settings but key_binding_for binds nothing to it",
                def.id
            );
        }
    }
}
