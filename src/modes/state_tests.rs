//! `ModeState`: entering a mode yields that mode's fresh state, and no other
//! mode's state can be reached from it.

use super::ModeState;
use crate::test_support::*;
use crate::{
    AddProgress, ArcPoint, ArcProgress, BuildingProgress, BuildingReference, ExtrudeDrag,
    ExtrudeStyle, MapViewer, SetMode,
};
use gpui::{point, px, TestAppContext};
use osm_gpui::edit_mode::EditMode;
use osm_gpui::layers::LayerId;

/// The modes that have a gesture to abandon.
pub(crate) const GESTURE_MODES: [EditMode; 4] = [
    EditMode::Add,
    EditMode::Building,
    EditMode::Arc,
    EditMode::Extrude,
];

/// Put `view`'s current mode part-way through its gesture.
pub(crate) fn start_gesture(view: &mut MapViewer, layer: LayerId) {
    match view.mode() {
        EditMode::Add => view.set_add_progress(Some(AddProgress {
            way_id: None,
            last_node_id: -1,
            at_start: false,
        })),
        EditMode::Building => view.set_building_progress(Some(BuildingProgress {
            corner_a: (LAT, LON),
            corner_b: None,
        })),
        EditMode::Arc => view.set_arc_progress(Some(ArcProgress {
            start: ArcPoint {
                geo: (LAT, LON),
                snap: None,
            },
            end: None,
        })),
        EditMode::Extrude => view.set_extrude_drag(Some(ExtrudeDrag {
            layer,
            way_id: 1,
            node_a: 1,
            node_b: 2,
            segment: 0,
            style: ExtrudeStyle::Reshape,
            down: point(px(0.0), px(0.0)),
        })),
        EditMode::Select | EditMode::Improve => {}
    }
}

/// Whether any mode's gesture slot is filled.
pub(crate) fn has_gesture(view: &MapViewer) -> bool {
    view.add_progress().is_some()
        || view.building_progress().is_some()
        || matches!(&view.mode_state, ModeState::Building(s) if s.press.is_some())
        || view.arc_progress().is_some()
        || view.extrude_drag().is_some()
}

#[test]
fn a_fresh_state_reports_its_mode_and_holds_nothing() {
    for mode in EditMode::ALL {
        let state = ModeState::fresh(mode);
        assert_eq!(state.mode(), mode);
        assert!(!state.is_drawing(), "{mode:?}");
    }
}

#[gpui::test]
fn entering_a_mode_gives_that_modes_fresh_state(cx: &mut TestAppContext) {
    for from in EditMode::ALL {
        for to in EditMode::ALL {
            with_osm(cx, empty_data(), from, move |view, w, cx, layer| {
                start_gesture(view, layer);
                view.on_set_mode(&SetMode { mode: to }, w, cx);
                assert_eq!(view.mode(), to, "{from:?} -> {to:?}");
                assert!(!has_gesture(view), "{from:?} -> {to:?}");
                assert!(!view.mode_state.is_drawing(), "{from:?} -> {to:?}");
                assert!(
                    view.building_reference().is_none(),
                    "{from:?} -> {to:?}: nothing selected to align to"
                );
            });
        }
    }
}

#[gpui::test]
fn another_modes_slots_are_not_reachable(cx: &mut TestAppContext) {
    with_osm(cx, empty_data(), EditMode::Arc, |view, _w, _cx, layer| {
        start_gesture(view, layer);
        assert!(view.arc_progress().is_some());
        // Writing into a slot the current mode does not have does nothing.
        view.set_add_progress(Some(AddProgress {
            way_id: None,
            last_node_id: -1,
            at_start: false,
        }));
        view.set_building_reference(Some(BuildingReference {
            layer_id: layer,
            way_id: 1,
            orientation: 0.0,
        }));
        view.set_extrude_drag(None);
        assert!(view.add_progress().is_none());
        assert!(view.building_reference().is_none());
        assert!(view.arc_progress().is_some());
        assert_eq!(view.mode(), EditMode::Arc);
    });
}

#[gpui::test]
fn is_drawing_follows_the_placing_modes_progress(cx: &mut TestAppContext) {
    for mode in EditMode::ALL {
        with_osm(cx, empty_data(), mode, move |view, _w, _cx, layer| {
            assert!(!view.mode_state.is_drawing(), "{mode:?} before");
            start_gesture(view, layer);
            let placing = matches!(mode, EditMode::Add | EditMode::Building | EditMode::Arc);
            assert_eq!(view.mode_state.is_drawing(), placing, "{mode:?} after");
        });
    }
}

#[gpui::test]
fn resetting_progress_keeps_the_mode_and_the_building_reference(cx: &mut TestAppContext) {
    for mode in EditMode::ALL {
        with_osm(cx, empty_data(), mode, move |view, _w, _cx, layer| {
            view.set_building_reference(Some(BuildingReference {
                layer_id: layer,
                way_id: 1,
                orientation: 0.0,
            }));
            start_gesture(view, layer);
            view.reset_mode_progress();
            assert_eq!(view.mode(), mode);
            assert!(!has_gesture(view), "{mode:?}");
            assert_eq!(
                view.building_reference().is_some(),
                mode == EditMode::Building,
                "{mode:?}"
            );
        });
    }
}

#[gpui::test]
fn staying_in_building_mode_keeps_the_reference(cx: &mut TestAppContext) {
    with_osm(
        cx,
        empty_data(),
        EditMode::Building,
        |view, w, cx, layer| {
            view.set_building_reference(Some(BuildingReference {
                layer_id: layer,
                way_id: 1,
                orientation: 0.0,
            }));
            start_gesture(view, layer);
            view.on_set_mode(
                &SetMode {
                    mode: EditMode::Building,
                },
                w,
                cx,
            );
            assert!(view.building_reference().is_some());
            assert!(!has_gesture(view));
        },
    );
}

#[gpui::test]
fn forgetting_the_active_layer_drops_the_gesture(cx: &mut TestAppContext) {
    for mode in GESTURE_MODES {
        with_osm(cx, empty_data(), mode, move |view, _w, _cx, layer| {
            start_gesture(view, layer);
            view.forget_layer(layer);
            assert_eq!(view.mode(), mode);
            assert!(!has_gesture(view), "{mode:?}");
        });
    }
}
