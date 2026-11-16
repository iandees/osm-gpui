//! Lane edits from the Lanes section.

use super::{select, tags_of, way_fixture, CENTER_LAT, CENTER_LON};
use crate::test_support::{osm_window, redo_step, undo_step};
use crate::EditMode;
use gpui::TestAppContext;
use osm_gpui::ids::WayId;
use osm_gpui::lanes::{Arrow, LaneEdit};
use osm_gpui::selection::FeatureKind;

fn lane_tags(view: &crate::MapViewer) -> Vec<(String, String)> {
    tags_of(view, FeatureKind::Way, 10)
        .into_iter()
        .filter(|(k, _)| k.contains("lanes"))
        .collect()
}

fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
    p.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[gpui::test]
fn each_lane_edit_is_one_undo_step(cx: &mut TestAppContext) {
    let mut data = way_fixture(CENTER_LAT, CENTER_LON);
    data.ways.get_mut(&WayId(10)).unwrap().tags = [("highway", "primary"), ("lanes", "2")]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let window = osm_window(cx, "L", data, EditMode::Select);
    window
        .update(cx, |view, _window, _cx| {
            select(view, FeatureKind::Way, &[10]);
            view.apply_lane_edit(LaneEdit::Count {
                forward: true,
                add: true,
            });
            assert_eq!(
                lane_tags(view),
                pairs(&[
                    ("lanes", "3"),
                    ("lanes:backward", "1"),
                    ("lanes:forward", "2")
                ])
            );
            view.apply_lane_edit(LaneEdit::Arrow {
                forward: true,
                lane: 0,
                arrow: Arrow::Left,
            });
            assert_eq!(
                lane_tags(view),
                pairs(&[
                    ("lanes", "3"),
                    ("lanes:backward", "1"),
                    ("lanes:forward", "2"),
                    ("turn:lanes:forward", "left|none")
                ])
            );
            assert_eq!(view.undo_stack.actions.len(), 2);

            undo_step(view);
            undo_step(view);
            assert_eq!(lane_tags(view), pairs(&[("lanes", "2")]));
            redo_step(view);
            assert_eq!(lane_tags(view).len(), 3);

            // Removing the last lane of a direction changes nothing.
            let steps = view.undo_stack.actions.len();
            let before = lane_tags(view);
            view.apply_lane_edit(LaneEdit::Count {
                forward: false,
                add: false,
            });
            assert_eq!(view.undo_stack.actions.len(), steps);
            assert_eq!(lane_tags(view), before);
        })
        .unwrap();
}
