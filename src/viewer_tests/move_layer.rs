//! Moving a layer up or down the list with the `MoveLayer` action.

use super::setup;
use crate::test_support::{add_osm_layer, empty_data};
use crate::{MapViewer, MoveLayer};
use gpui::{Context, TestAppContext, Window};
use osm_gpui::layers::LayerId;

fn layer_order(view: &MapViewer) -> Vec<LayerId> {
    view.layer_manager.layers().iter().map(|l| l.id()).collect()
}

/// A view with three layers: the fixture layer plus two empty ones.
fn add_two_layers(view: &mut MapViewer) {
    add_osm_layer(view, "L2", empty_data());
    add_osm_layer(view, "L3", empty_data());
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

#[gpui::test]
fn moving_a_layer_past_either_end_leaves_the_order_alone(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            add_two_layers(view);
            let order = layer_order(view);
            assert_eq!(order.len(), 3);

            // Up from the top, down from the bottom, and further out.
            for (index, delta) in [(0, -1), (2, 1), (0, -5), (2, 5), (1, 2), (1, -2)] {
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

            move_layer(view, 0, 1, window, cx);
            assert_eq!(layer_order(view), vec![b, a, c]);
            // The bottom layer can move to the top, and the top to the bottom.
            move_layer(view, 2, -2, window, cx);
            assert_eq!(layer_order(view), vec![c, b, a]);
            move_layer(view, 0, 2, window, cx);
            assert_eq!(layer_order(view), vec![b, a, c]);
        })
        .unwrap();
}
