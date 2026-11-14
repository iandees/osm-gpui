//! Fixed window positions that the documentation screenshot scripts click.

use crate::{selection_actions, MapViewer};
use gpui::{point, px, size};
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::layers::LayerId;
use osm_gpui::viewport::Viewport;
use std::sync::Arc;

/// `docs/screenshots/selection-actions.osmscript` clicks fixed window
/// positions; check they land on the features the script names.
#[test]
fn screenshot_script_click_targets_hit_their_features() {
    let xml = std::fs::read_to_string("docs/screenshots/fixtures/selection-actions.osm")
        .expect("fixture");
    let data = osm_gpui::osm::OsmParser::new().parse_str(&xml).unwrap();
    let layer = OsmLayer::new_with_data(LayerId(1), "L".to_string(), Arc::new(data));
    // The map area of a 1200x800 window, centred as the script does.
    let viewport = Viewport::new(40.7120, -74.0060, 18.0, size(px(864.0), px(776.0)));
    for (x, y, way) in [
        (370.0, 470.0, 3001),
        (710.0, 300.0, 3002),
        (640.0, 640.0, 3003),
    ] {
        let at = point(px(x - MapViewer::MODE_PANEL_WIDTH), px(y));
        let hits = osm_gpui::layers::EditableLayer::hit_test(&layer, &viewport, at);
        let hit = osm_gpui::selection::resolve_hits(vec![hits]);
        assert_eq!(hit.map(|h| h.id), Some(way), "click at ({x}, {y})");
    }
}

/// `docs/screenshots/topology-actions.osmscript` clicks and box-selects
/// fixed window positions; check they pick what the script names.
#[test]
fn topology_script_targets_hit_their_features() {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let xml =
        std::fs::read_to_string("docs/screenshots/fixtures/topology-actions.osm").expect("fixture");
    let data = osm_gpui::osm::OsmParser::new().parse_str(&xml).unwrap();
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L".to_string(), Arc::new(data));
    // The script selects the open path before clicking its first node:
    // plain vertices are drawn (and clickable) only on a selected way.
    osm_gpui::layers::EditableLayer::set_highlight(
        &mut layer,
        &[osm_gpui::selection::FeatureRef {
            layer_id: LayerId(1),
            kind: Way,
            id: 4107,
        }],
    );
    let viewport = Viewport::new(40.7120, -74.0060, 18.0, size(px(864.0), px(776.0)));
    let local = |x: f32, y: f32| point(px(x - MapViewer::MODE_PANEL_WIDTH), px(y));
    for (x, y, kind, id) in [
        (640.0, 200.0, Node, 6302),
        (600.0, 430.0, Way, 4107),
        (560.0, 420.0, Node, 6401),
    ] {
        let hits = osm_gpui::layers::EditableLayer::hit_test(&layer, &viewport, local(x, y));
        let hit = osm_gpui::selection::resolve_hits(vec![hits]).expect("a hit");
        assert_eq!((hit.kind, hit.id), (kind, id), "click at ({x}, {y})");
    }
    // Box selects: both ways of a pair, and nothing from the others.
    for (from, to, want) in [
        ((70.0, 80.0), (450.0, 170.0), [-4102, 4101]),
        ((70.0, 225.0), (450.0, 310.0), [4103, 4104]),
    ] {
        let rect = gpui::Bounds::from_corners(local(from.0, from.1), local(to.0, to.1));
        let mut ways: Vec<i64> =
            osm_gpui::layers::EditableLayer::hit_test_rect(&layer, &viewport, rect)
                .into_iter()
                .filter(|f| f.kind == Way)
                .map(|f| f.id)
                .collect();
        ways.sort_unstable();
        assert_eq!(ways, want, "box {from:?} to {to:?}");
    }
}

/// `docs/screenshots/relation-editor.osmscript` clicks the route's way on
/// the map and expects the route to be its relation, with a member that
/// is not downloaded and a relation nested in another.
#[test]
fn relation_editor_script_targets_and_fixture() {
    use osm_gpui::selection::resolve_hits;
    use osm_gpui::selection::FeatureKind::Way;

    let xml =
        std::fs::read_to_string("docs/screenshots/fixtures/relation-editor.osm").expect("fixture");
    let data = osm_gpui::osm::OsmParser::new().parse_str(&xml).unwrap();
    let layer = OsmLayer::new_with_data(LayerId(1), "L".to_string(), Arc::new(data));
    let viewport = Viewport::new(40.7120, -74.0060, 18.0, size(px(864.0), px(776.0)));
    let hit_at = |x: f32, y: f32| {
        let at = point(px(x - MapViewer::MODE_PANEL_WIDTH), px(y));
        let hits = osm_gpui::layers::EditableLayer::hit_test(&layer, &viewport, at);
        resolve_hits(vec![hits]).map(|h| (h.kind, h.id))
    };
    assert_eq!(hit_at(520.0, 200.0), Some((Way, 8202)));
    assert_eq!(hit_at(340.0, 400.0), Some((Way, 8301)));

    let editable: &dyn osm_gpui::layers::EditableLayer = &layer;
    assert_eq!(editable.relations_containing_way(8202), vec![9101]);
    let route = editable.relation(9101).unwrap();
    assert_eq!(route.members.len(), 5);
    // Way 8999 is a member but not loaded.
    assert!(editable.way_node_ids(8999).is_none());
    let find = |id| editable.relation(id);
    let master = editable.relation(9103).unwrap();
    let flat = osm_gpui::relations::flatten_members(&master, &find);
    assert_eq!(flat.len(), 5);
}

/// `docs/screenshots/relations.osmscript` clicks fixed window positions;
/// check they pick what the script names, that the fixture's relations
/// show up as the script expects, and that the split it runs keeps the
/// route intact and moves the later clicks as the script assumes.
#[test]
fn relations_script_targets_hit_their_features_and_the_split_keeps_the_route() {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    use osm_gpui::selection::{resolve_hits, FeatureRef};

    let xml = std::fs::read_to_string("docs/screenshots/fixtures/relations.osm").expect("fixture");
    let data = osm_gpui::osm::OsmParser::new().parse_str(&xml).unwrap();
    let mut layers = osm_gpui::layers::LayerManager::new();
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L".to_string(), Arc::new(data));
    // The script selects the middle route way before clicking its middle
    // node: plain vertices are drawn (and clickable) only on a selected way.
    osm_gpui::layers::EditableLayer::set_highlight(
        &mut layer,
        &[FeatureRef {
            layer_id: LayerId(1),
            kind: Way,
            id: 8202,
        }],
    );
    layers.add_layer(Box::new(layer));
    let viewport = Viewport::new(40.7120, -74.0060, 18.0, size(px(864.0), px(776.0)));
    let hit_at = |layers: &osm_gpui::layers::LayerManager, x: f32, y: f32| {
        let at = point(px(x - MapViewer::MODE_PANEL_WIDTH), px(y));
        let hits = layers
            .find_layer(LayerId(1))
            .unwrap()
            .as_editable()
            .unwrap()
            .hit_test(&viewport, at);
        resolve_hits(vec![hits]).map(|h| (h.kind, h.id))
    };
    for (x, y, kind, id) in [
        (520.0, 200.0, Way, 8202),
        (560.0, 200.0, Node, 8104),
        (340.0, 400.0, Way, 8301),
        (640.0, 200.0, Node, 8105),
        (760.0, 200.0, Way, 8202),
    ] {
        assert_eq!(
            hit_at(&layers, x, y),
            Some((kind, id)),
            "click at ({x}, {y})"
        );
    }

    // What the Relations section shows for the clicked features.
    let relations_of = |layers: &osm_gpui::layers::LayerManager, kind, id| {
        let editable = layers
            .find_layer(LayerId(1))
            .unwrap()
            .as_editable()
            .unwrap();
        let ids = match kind {
            Node => editable.relations_containing_node(id),
            Way => editable.relations_containing_way(id),
            osm_gpui::selection::FeatureKind::Relation => Vec::new(),
        };
        ids.into_iter()
            .map(|r| osm_gpui::relations::display_name(&editable.relation(r).unwrap()))
            .collect::<Vec<_>>()
    };
    assert_eq!(relations_of(&layers, Way, 8202), ["Bus 7B"]);
    assert_eq!(relations_of(&layers, Node, 8104), ["Bus 7B"]);
    assert_eq!(relations_of(&layers, Way, 8301), ["restriction"]);

    // Split at the middle node: the new way is -1, takes the right-hand
    // part, and joins the route right after the original.
    let action = selection_actions::find("split").unwrap();
    let sel = [FeatureRef {
        layer_id: LayerId(1),
        kind: Node,
        id: 8105,
    }];
    (action.apply())(&mut layers, &sel, Some(LayerId(1))).unwrap();
    assert_eq!(hit_at(&layers, 760.0, 200.0), Some((Way, -1)));
    assert_eq!(hit_at(&layers, 520.0, 200.0), Some((Way, 8202)));
    let route = layers
        .find_layer(LayerId(1))
        .unwrap()
        .as_editable()
        .unwrap()
        .relation(9101)
        .unwrap();
    let refs: Vec<i64> = route.members.iter().map(|m| m.reference).collect();
    assert_eq!(refs, [8104, 8201, 8202, -1, 8203]);
    assert_eq!(relations_of(&layers, Way, -1), ["Bus 7B"]);
    // Way -1, way 8202 and relation 9101.
    let counts = layers
        .find_layer(LayerId(1))
        .unwrap()
        .as_osm()
        .unwrap()
        .diff_for_upload()
        .counts();
    assert_eq!(counts, (1, 2, 0));
}

/// `docs/screenshots/more-selection-actions.osmscript` clicks and
/// box-selects fixed window positions; check they pick what the script
/// names, and that each action the script runs on them is accepted, with
/// the results the script's later clicks and assertions expect.
#[test]
fn more_actions_script_targets_hit_their_features_and_actions_apply() {
    use osm_gpui::selection::FeatureKind::{Node, Way};
    use osm_gpui::selection::{resolve_hits, FeatureRef};

    let xml = std::fs::read_to_string("docs/screenshots/fixtures/more-selection-actions.osm")
        .expect("fixture");
    let data = osm_gpui::osm::OsmParser::new().parse_str(&xml).unwrap();
    let mut layers = osm_gpui::layers::LayerManager::new();
    layers.add_layer(Box::new(OsmLayer::new_with_data(
        LayerId(1),
        "L".to_string(),
        Arc::new(data),
    )));
    let viewport = Viewport::new(40.7120, -74.0060, 18.0, size(px(864.0), px(776.0)));
    let local = |x: f32, y: f32| point(px(x - MapViewer::MODE_PANEL_WIDTH), px(y));
    let hit_at = |layers: &osm_gpui::layers::LayerManager, x: f32, y: f32| {
        let layer = layers.find_layer(LayerId(1)).unwrap();
        let hits = layer
            .as_editable()
            .unwrap()
            .hit_test(&viewport, local(x, y));
        resolve_hits(vec![hits]).map(|h| (h.kind, h.id))
    };
    let feature = |kind, id| FeatureRef {
        layer_id: LayerId(1),
        kind,
        id,
    };
    // The script selects a way first; its plain vertices are then drawn
    // and so clickable (untagged vertices never are otherwise).
    let select_way = |layers: &mut osm_gpui::layers::LayerManager, id: i64| {
        layers
            .find_layer_mut(LayerId(1))
            .unwrap()
            .as_editable_mut()
            .unwrap()
            .set_highlight(&[FeatureRef {
                layer_id: LayerId(1),
                kind: Way,
                id,
            }]);
    };
    let mut clipboards = clipboard_actions::Clipboards::default();
    let mut run = |layers: &mut osm_gpui::layers::LayerManager, id: &str, sel: &[FeatureRef]| {
        let action = selection_actions::find(id).unwrap();
        let env = selection_actions::ActionEnv {
            selection: sel,
            active_layer: Some(LayerId(1)),
            layers,
            clipboards: &clipboards,
        };
        assert!((action.is_applicable)(&env), "{id} not offered");
        action
            .run_direct(layers, sel, Some(LayerId(1)), &mut clipboards)
            .unwrap_or_else(|e| panic!("{id}: {e}"))
    };

    select_way(&mut layers, 5101);
    for (x, y, kind, id) in [
        (550.0, 120.0, Node, 7103),
        (700.0, 120.0, Way, 5101),
        (400.0, 120.0, Way, 5101),
        (650.0, 262.0, Way, 5202),
        (450.0, 250.0, Way, 5201),
        (500.0, 401.0, Way, 5301),
        (425.0, 520.0, Way, 5401),
        (500.0, 640.0, Way, 5501),
        (500.0, 700.0, Way, 5502),
    ] {
        assert_eq!(
            hit_at(&layers, x, y),
            Some((kind, id)),
            "click at ({x}, {y})"
        );
    }

    // Split at the middle node: the halves tie, so the original keeps the
    // first; the new way is -1 and holds the part the script clicks at 700.
    run(&mut layers, "split", &[feature(Node, 7103)]);
    assert_eq!(hit_at(&layers, 700.0, 120.0), Some((Way, -1)));
    assert_eq!(hit_at(&layers, 400.0, 120.0), Some((Way, 5101)));

    // The box around B's end nodes picks exactly those two nodes (with
    // the west road selected, as the script has it).
    select_way(&mut layers, 5201);
    let rect = gpui::Bounds::from_corners(local(520.0, 232.0), local(580.0, 280.0));
    let boxed = layers
        .find_layer(LayerId(1))
        .unwrap()
        .as_editable()
        .unwrap()
        .hit_test_rect(&viewport, rect);
    let mut ids: Vec<(osm_gpui::selection::FeatureKind, i64)> =
        boxed.iter().map(|f| (f.kind, f.id)).collect();
    ids.sort_by_key(|&(_, id)| id);
    assert_eq!(ids, [(Node, -7203), (Node, 7202)]);
    let merged = run(&mut layers, "merge", &boxed);
    assert_eq!(merged.select, Some(vec![feature(Node, 7202)]));
    assert_eq!(hit_at(&layers, 650.0, 262.0), Some((Way, 5202)));

    // The wobbly track is within the straighten threshold.
    run(&mut layers, "straighten", &[feature(Way, 5301)]);
    // Reverse flips the direction tags.
    run(&mut layers, "reverse", &[feature(Way, 5401)]);
    let tags = layers
        .find_layer(LayerId(1))
        .unwrap()
        .as_editable()
        .unwrap()
        .feature_tags(&feature(Way, 5401))
        .unwrap();
    assert!(tags.contains(&("lanes:forward".to_string(), "1".to_string())));
    assert!(tags.contains(&("cycleway:right".to_string(), "lane".to_string())));
    assert!(tags.contains(&("incline".to_string(), "-10%".to_string())));
    assert!(tags.contains(&("oneway".to_string(), "yes".to_string())));
    // Copy the driveway's tags and paste them onto the track.
    run(&mut layers, "copy_tags", &[feature(Way, 5501)]);
    run(&mut layers, "paste_tags", &[feature(Way, 5502)]);
    let tags = layers
        .find_layer(LayerId(1))
        .unwrap()
        .as_editable()
        .unwrap()
        .feature_tags(&feature(Way, 5502))
        .unwrap();
    assert!(tags.contains(&("tracktype".to_string(), "grade4".to_string())));
    assert!(tags.contains(&("surface".to_string(), "gravel".to_string())));
    assert!(tags.contains(&("name".to_string(), "Back Lane".to_string())));
}
