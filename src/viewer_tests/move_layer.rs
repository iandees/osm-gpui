//! Moving a layer up or down the list with the `MoveLayer` action, and the
//! list's order being the map's drawing order.

use super::setup;
use crate::test_support::{add_osm_layer, empty_data};
use crate::{MapViewer, MoveLayer};
use gpui::{Context, TestAppContext, Window};
use osm_gpui::layers::tile_layer::TileLayer;
use osm_gpui::layers::{CanvasRun, DrawStep, LayerId};

/// The stack, bottom first.
fn layer_order(view: &MapViewer) -> Vec<LayerId> {
    view.layer_manager.layers().iter().map(|l| l.id()).collect()
}

/// The Layers list's rows, first row first.
fn list_order(view: &MapViewer) -> Vec<LayerId> {
    view.layer_rows().iter().map(|row| row.1).collect()
}

/// The layers in the order the map draws them, bottom first.
fn draw_order(view: &MapViewer) -> Vec<LayerId> {
    view.layer_manager
        .draw_plan(true)
        .into_iter()
        .flat_map(|step| match step {
            DrawStep::Elements(id) => vec![id],
            DrawStep::Canvas(run) => run.layers,
        })
        .collect()
}

/// A view with three layers: the fixture layer plus two empty ones.
fn add_two_layers(view: &mut MapViewer) {
    add_osm_layer(view, "L2", empty_data());
    add_osm_layer(view, "L3", empty_data());
}

fn add_imagery(view: &mut MapViewer) -> LayerId {
    let id = view.layer_manager.alloc_id();
    view.layer_manager
        .add_layer(Box::new(TileLayer::new(id, view.tile_cache.clone())));
    id
}

fn move_layer(
    view: &mut MapViewer,
    index: usize,
    delta: i32,
    window: &mut Window,
    cx: &mut Context<MapViewer>,
) {
    view.on_move_layer(&MoveLayer { index, delta }, window, cx);
}

/// The index the row showing `id` acts on.
fn row_index(view: &MapViewer, id: LayerId) -> usize {
    view.layer_rows()
        .iter()
        .find(|row| row.1 == id)
        .map(|row| row.0)
        .unwrap()
}

#[gpui::test]
fn moving_a_layer_past_either_end_leaves_the_order_alone(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            add_two_layers(view);
            let order = layer_order(view);
            assert_eq!(order.len(), 3);

            // Up from the top (index 2), down from the bottom (index 0), and
            // further out.
            for (index, delta) in [(2, 1), (0, -1), (2, 5), (0, -5), (1, 2), (1, -2)] {
                move_layer(view, index, delta, window, cx);
                assert_eq!(layer_order(view), order, "index {index}, delta {delta}");
            }
            // An index that is itself past the end is ignored too.
            move_layer(view, 7, -6, window, cx);
            assert_eq!(layer_order(view), order);
        })
        .unwrap();
}

#[gpui::test]
fn moving_a_layer_within_the_list_reorders_it(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            add_two_layers(view);
            let [a, b, c] = layer_order(view).try_into().unwrap();
            // The newest layer is drawn on top, the first row of the list.
            assert_eq!(list_order(view), vec![c, b, a]);

            // Move Up raises a layer: up the list, drawn higher.
            let up = MoveLayer::up(row_index(view, a));
            view.on_move_layer(&up, window, cx);
            assert_eq!(list_order(view), vec![c, a, b]);
            assert_eq!(layer_order(view), vec![b, a, c]);
            // Move Down lowers it again.
            let down = MoveLayer::down(row_index(view, a));
            view.on_move_layer(&down, window, cx);
            assert_eq!(list_order(view), vec![c, b, a]);

            // The bottom layer can move to the top, and the top to the bottom.
            move_layer(view, 0, 2, window, cx);
            assert_eq!(list_order(view), vec![a, c, b]);
            move_layer(view, 2, -2, window, cx);
            assert_eq!(list_order(view), vec![c, b, a]);
        })
        .unwrap();
}

#[gpui::test]
fn the_list_order_is_the_draw_order(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            let data = layer_order(view)[0];
            // Imagery added after the data still goes under it.
            let imagery = add_imagery(view);
            assert_eq!(list_order(view), vec![data, imagery]);
            let mut drawn = draw_order(view);
            drawn.reverse();
            assert_eq!(drawn, list_order(view));
            assert_eq!(
                view.layer_manager.draw_plan(true),
                vec![
                    DrawStep::Elements(imagery),
                    DrawStep::Canvas(CanvasRun {
                        wash: true,
                        layers: vec![data],
                    }),
                ]
            );

            // Move the imagery up, above the data: it is drawn over it.
            view.on_move_layer(&MoveLayer::up(row_index(view, imagery)), window, cx);
            assert_eq!(list_order(view), vec![imagery, data]);
            let mut drawn = draw_order(view);
            drawn.reverse();
            assert_eq!(drawn, list_order(view));
            assert_eq!(
                view.layer_manager.draw_plan(true),
                vec![
                    DrawStep::Canvas(CanvasRun {
                        wash: false,
                        layers: vec![data],
                    }),
                    DrawStep::Elements(imagery),
                    DrawStep::Canvas(CanvasRun {
                        wash: true,
                        layers: vec![],
                    }),
                ]
            );
        })
        .unwrap();
}
