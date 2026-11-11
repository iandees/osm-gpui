//! Reverse Way: node order, direction-dependent tags, roles, node tags.

use super::*;

const DOC: &str = "docs/testing/edge-cases-from-josm-and-id.md";

fn one_way(tags: &[(&str, &str)]) -> OsmData {
    Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], tags)
        .done()
}

/// Reverse way 10 once and return its tags afterwards.
fn reversed_tags(
    cx: &mut TestAppContext,
    tags: &'static [(&'static str, &'static str)],
) -> Vec<(String, String)> {
    let out = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = out.clone();
    with_session(cx, one_way(tags), move |s| {
        let before = s.snap();
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert_eq!(s.way_nodes(10), Some(vec![3, 2, 1]));
        *sink.borrow_mut() = s.tags(W, 10);
        // One undo restores nodes and tags together; redo reapplies both.
        let after = s.snap();
        s.undo();
        assert_eq!(s.snap(), before);
        assert_eq!(s.counts(), (0, 0, 0));
        s.redo();
        assert_eq!(s.snap(), after);
    });
    let v = out.borrow().clone();
    v
}

#[gpui::test]
fn oneway_tags_are_kept_when_the_nodes_are_reversed(cx: &mut TestAppContext) {
    // Design difference: JOSM turns oneway=yes into -1 so the traffic
    // direction does not change; iD (and this app) keep the tag, because
    // reversing a oneway is how a user fixes its direction.
    assert_eq!(
        reversed_tags(cx, &[("highway", "residential"), ("oneway", "yes")]),
        pairs(&[("highway", "residential"), ("oneway", "yes")])
    );
    assert_eq!(
        reversed_tags(cx, &[("highway", "residential"), ("oneway", "-1")]),
        pairs(&[("highway", "residential"), ("oneway", "-1")])
    );
}

#[gpui::test]
fn forward_backward_and_left_right_keys_swap(cx: &mut TestAppContext) {
    assert_eq!(
        reversed_tags(
            cx,
            &[
                ("maxspeed:forward", "25"),
                ("maxspeed:backward", "30"),
                ("cycleway:right", "lane"),
                ("cycleway:right:surface", "paved"),
                ("turn:lanes:forward", "left|right"),
            ]
        ),
        pairs(&[
            ("cycleway:left", "lane"),
            ("cycleway:left:surface", "paved"),
            ("maxspeed:backward", "25"),
            ("maxspeed:forward", "30"),
            ("turn:lanes:backward", "left|right"),
        ])
    );
}

#[gpui::test]
fn incline_and_direction_values_flip(cx: &mut TestAppContext) {
    assert_eq!(
        reversed_tags(cx, &[("incline", "up"), ("direction", "forward")]),
        pairs(&[("direction", "backward"), ("incline", "down")])
    );
    assert_eq!(
        reversed_tags(cx, &[("incline", "10%")]),
        pairs(&[("incline", "-10%")])
    );
    assert_eq!(
        reversed_tags(cx, &[("incline", "-.8°")]),
        pairs(&[("incline", ".8°")])
    );
}

#[gpui::test]
fn names_and_notes_that_look_directional_are_left_alone(cx: &mut TestAppContext) {
    assert_eq!(
        reversed_tags(
            cx,
            &[
                ("name", "forward"),
                ("note", "up"),
                ("ref", "left"),
                ("type", "drawdown")
            ]
        ),
        pairs(&[
            ("name", "forward"),
            ("note", "up"),
            ("ref", "left"),
            ("type", "drawdown")
        ])
    );
}

#[gpui::test]
fn conveying_is_kept_like_oneway(cx: &mut TestAppContext) {
    assert_eq!(
        reversed_tags(cx, &[("highway", "steps"), ("conveying", "forward")]),
        pairs(&[("conveying", "forward"), ("highway", "steps")])
    );
}

#[gpui::test]
fn sidewalk_value_left_becomes_right(cx: &mut TestAppContext) {
    let _ = DOC;
    assert_eq!(
        reversed_tags(cx, &[("highway", "residential"), ("sidewalk", "left")]),
        pairs(&[("highway", "residential"), ("sidewalk", "right")])
    );
}

#[gpui::test]
fn forward_backward_values_flip_for_any_key_except_free_text(cx: &mut TestAppContext) {
    assert_eq!(
        reversed_tags(
            cx,
            &[
                ("priority", "forward"),
                ("railway:preferred_direction", "backward"),
                ("name", "forward")
            ]
        ),
        pairs(&[
            ("name", "forward"),
            ("priority", "backward"),
            ("railway:preferred_direction", "forward"),
        ])
    );
}

#[gpui::test]
fn side_specific_oneway_value_flips_with_the_side(cx: &mut TestAppContext) {
    assert_eq!(
        reversed_tags(cx, &[("cycleway:left:oneway", "-1")]),
        pairs(&[("cycleway:right:oneway", "yes")])
    );
}

#[gpui::test]
fn route_roles_forward_and_backward_swap(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2], &[("highway", "primary")])
        .way(11, &[3, 4], &[("highway", "primary")])
        .way(12, &[2, 3], &[("highway", "primary")])
        .relation(
            900,
            &[("type", "route"), ("route", "bicycle")],
            &[wm(10, "forward"), wm(11, "backward"), wm(12, "")],
        )
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11), (W, 12)]);
        assert!(s.act("reverse"));
        assert_eq!(
            s.members(900),
            mem(&[(10, "backward"), (11, "forward"), (12, "")])
        );
        // The relation change belongs in the same undo step.
        assert_eq!(s.steps(), 1);
        s.undo();
        assert_eq!(s.snap(), before);
    });
}

#[gpui::test]
fn restriction_from_and_to_roles_are_not_swapped(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &[("highway", "primary")])
        .way(11, &[2, 3], &[("highway", "primary")])
        .relation(
            900,
            &[("type", "restriction"), ("restriction", "no_left_turn")],
            &[wm(10, "from"), nm(2, "via"), wm(11, "to")],
        )
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("reverse"));
        assert_eq!(s.members(900), mem(&[(10, "from"), (2, "via"), (11, "to")]));
        // The relation is not part of the upload; only the ways are.
        assert_eq!(s.counts(), (0, 2, 0));
    });
}

#[gpui::test]
fn direction_tags_on_the_ways_nodes_flip(cx: &mut TestAppContext) {
    let data = Build::new()
        .node(1, &[])
        .node(2, &[("highway", "stop"), ("direction", "forward")])
        .node(
            3,
            &[
                ("highway", "traffic_signals"),
                ("traffic_signals:direction", "backward"),
            ],
        )
        .node(4, &[("highway", "stop"), ("direction", "SSW")])
        .node(5, &[("traffic_sign:forward", "stop")])
        .way(10, &[1, 2, 3, 4, 5], &[("highway", "residential")])
        .done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert_eq!(
            s.tags(N, 2),
            pairs(&[("direction", "backward"), ("highway", "stop")])
        );
        assert_eq!(
            s.tags(N, 3),
            pairs(&[
                ("highway", "traffic_signals"),
                ("traffic_signals:direction", "forward")
            ])
        );
        // Compass directions are absolute and stay.
        assert_eq!(
            s.tags(N, 4),
            pairs(&[("direction", "SSW"), ("highway", "stop")])
        );
        assert_eq!(s.tags(N, 5), pairs(&[("traffic_sign:backward", "stop")]));
    });
}

#[gpui::test]
fn reversing_two_ways_is_one_undo_step_and_touches_only_those_ways(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2], &[("highway", "path")])
        .way(11, &[3, 4], &[("highway", "path")])
        .way(12, &[2, 3], &[("highway", "path")])
        .done();
    with_session(cx, data, |s| {
        let before = s.snap();
        s.select(&[(W, 10), (W, 11)]);
        assert!(s.act("reverse"));
        assert_eq!(s.steps(), 1);
        assert_eq!(s.way_nodes(12), Some(vec![2, 3]));
        assert_eq!(s.counts(), (0, 2, 0));
        s.undo();
        assert_eq!(s.snap(), before);
    });
}
