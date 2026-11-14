//! Escape backs out one step at a time in every mode: the unfinished gesture
//! first, then the mode itself.

use super::state_tests::{has_gesture, start_gesture, GESTURE_MODES};
use crate::test_support::*;
use gpui::TestAppContext;
use osm_gpui::edit_mode::EditMode;
use osm_gpui::selection::FeatureKind;

#[gpui::test]
fn escape_drops_the_unfinished_gesture_and_stays_in_the_mode(cx: &mut TestAppContext) {
    for mode in GESTURE_MODES {
        with_osm(cx, empty_data(), mode, move |view, _w, cx, layer| {
            start_gesture(view, layer);
            assert!(has_gesture(view), "{mode:?}");
            view.escape(cx);
            assert!(!has_gesture(view), "{mode:?}");
            assert_eq!(view.mode(), mode, "the first Escape keeps the mode");
        });
    }
}

#[gpui::test]
fn escape_without_a_gesture_leaves_the_mode(cx: &mut TestAppContext) {
    for mode in GESTURE_MODES.into_iter().chain([EditMode::Improve]) {
        with_osm(cx, empty_data(), mode, move |view, _w, cx, _layer| {
            assert!(!has_gesture(view));
            view.escape(cx);
            assert_eq!(view.mode(), EditMode::Select, "{mode:?}");
        });
    }
}

#[gpui::test]
fn leaving_building_mode_forgets_its_reference(cx: &mut TestAppContext) {
    with_osm(
        cx,
        empty_data(),
        EditMode::Building,
        |view, _w, cx, layer| {
            view.set_building_reference(Some(crate::BuildingReference {
                layer_id: layer,
                way_id: 1,
                orientation: 0.5,
            }));
            start_gesture(view, layer);
            view.escape(cx);
            assert_eq!(view.mode(), EditMode::Building);
            assert!(
                view.building_reference().is_some(),
                "kept while still in the mode"
            );
            view.escape(cx);
            assert_eq!(view.mode(), EditMode::Select);
            assert!(view.building_reference().is_none());
        },
    );
}

#[gpui::test]
fn improve_lets_go_of_its_way_before_leaving(cx: &mut TestAppContext) {
    with_osm(
        cx,
        empty_data(),
        EditMode::Improve,
        |view, _w, cx, layer| {
            view.set_selection(vec![osm_gpui::selection::FeatureRef {
                layer_id: layer,
                kind: FeatureKind::Way,
                id: 1,
            }]);
            view.escape(cx);
            assert_eq!(view.mode(), EditMode::Improve);
            assert!(view.selected.is_empty());
            view.escape(cx);
            assert_eq!(view.mode(), EditMode::Select);
        },
    );
}

#[gpui::test]
fn select_mode_escape_clears_the_selection(cx: &mut TestAppContext) {
    with_osm(cx, empty_data(), EditMode::Select, |view, _w, cx, layer| {
        view.set_selection(vec![osm_gpui::selection::FeatureRef {
            layer_id: layer,
            kind: FeatureKind::Node,
            id: 1,
        }]);
        view.escape(cx);
        assert!(view.selected.is_empty());
        assert_eq!(view.mode(), EditMode::Select);
    });
}

#[gpui::test]
fn an_armed_note_is_cancelled_before_the_mode_steps_back(cx: &mut TestAppContext) {
    with_osm(
        cx,
        empty_data(),
        EditMode::Select,
        |view, _w, cx, _layer| {
            view.arm_new_note();
            assert!(view.notes_armed());
            view.escape(cx);
            assert!(!view.notes_armed());
        },
    );
}
