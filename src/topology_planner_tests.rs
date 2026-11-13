//! The unjoin and delete-segment planners and the node tag flip, on node
//! lists alone.

use super::*;

fn ways(list: &[(i64, &[i64])]) -> Vec<(i64, Vec<i64>)> {
    list.iter().map(|(id, n)| (*id, n.to_vec())).collect()
}

fn tag_list(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn change(key: &str, old: Option<&str>, new: Option<&str>) -> TagChange {
    (
        key.to_string(),
        old.map(str::to_string),
        new.map(str::to_string),
    )
}

#[test]
fn a_vertex_is_unjoinable_unless_it_is_an_end_of_an_open_way() {
    let open = [1, 2, 3, 4];
    assert!(is_unjoinable_vertex(&open, 2));
    assert!(is_unjoinable_vertex(&open, 3));
    assert!(!is_unjoinable_vertex(&open, 1), "start");
    assert!(!is_unjoinable_vertex(&open, 4), "end");
    assert!(!is_unjoinable_vertex(&open, 9), "not in the way");
    assert!(!is_unjoinable_vertex(&[], 1), "empty way");
}

#[test]
fn a_two_node_way_has_no_unjoinable_vertex() {
    assert!(!is_unjoinable_vertex(&[1, 2], 1));
    assert!(!is_unjoinable_vertex(&[1, 2], 2));
}

#[test]
fn a_rings_closing_node_is_a_vertex_but_a_short_loop_is_not_a_ring() {
    let ring = [1, 2, 3, 1];
    assert!(is_unjoinable_vertex(&ring, 1), "a ring has no ends");
    assert!(is_unjoinable_vertex(&ring, 2));
    let there_and_back = [1, 2, 1];
    assert!(!is_unjoinable_vertex(&there_and_back, 1), "not closed");
    assert!(is_unjoinable_vertex(&there_and_back, 2));
}

#[test]
fn an_end_node_visited_again_in_the_middle_is_a_vertex() {
    assert!(is_unjoinable_vertex(&[1, 2, 1, 3], 1));
}

#[test]
fn unjoin_pulls_a_shared_vertex_off_one_way() {
    let list = ways(&[(1, &[10, 2, 11]), (2, &[20, 2, 21])]);
    assert!(can_unjoin(2, &list, 1));
    assert!(can_unjoin(2, &list, 2));
    let mut calls = 0;
    let plan = plan_unjoin(2, &list, 1, || {
        calls += 1;
        -7
    })
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(
        plan,
        vec![SeparateEdit {
            way_id: 1,
            before: vec![10, 2, 11],
            after: vec![10, -7, 11],
        }]
    );
}

#[test]
fn unjoin_from_a_node_shared_by_three_ways_edits_only_the_chosen_way() {
    let list = ways(&[(1, &[10, 2, 11]), (2, &[20, 2, 21]), (3, &[30, 2, 31])]);
    for way in [1, 2, 3] {
        assert!(can_unjoin(2, &list, way));
        let plan = plan_unjoin(2, &list, way, || -1).unwrap();
        assert_eq!(plan.len(), 1, "way {way}");
        assert_eq!(plan[0].way_id, way);
        assert_eq!(
            plan[0].after.iter().filter(|&&n| n == -1).count(),
            1,
            "way {way}"
        );
        assert!(!plan[0].after.contains(&2));
    }
}

#[test]
fn nothing_to_unjoin_when_no_other_way_uses_the_node() {
    let list = ways(&[(1, &[10, 2, 11]), (2, &[20, 21])]);
    assert!(!can_unjoin(2, &list, 1));
    let mut called = false;
    assert_eq!(
        plan_unjoin(2, &list, 1, || {
            called = true;
            -1
        }),
        None
    );
    assert!(!called, "no node is allocated for a plan that is refused");
}

#[test]
fn an_end_node_cannot_be_unjoined_even_when_shared() {
    let list = ways(&[(1, &[2, 11, 12]), (2, &[20, 21, 2])]);
    assert!(!can_unjoin(2, &list, 1), "start of way 1");
    assert!(!can_unjoin(2, &list, 2), "end of way 2");
    assert_eq!(plan_unjoin(2, &list, 1, || -1), None);
}

#[test]
fn unjoin_of_a_closed_ways_closing_node_moves_both_ends_of_the_ring() {
    let list = ways(&[(1, &[2, 11, 12, 13, 2]), (2, &[2, 21])]);
    assert!(can_unjoin(2, &list, 1));
    let plan = plan_unjoin(2, &list, 1, || -5).unwrap();
    assert_eq!(plan[0].after, vec![-5, 11, 12, 13, -5]);
    assert!(
        !can_unjoin(2, &list, 2),
        "the other way ends at the node, it is not a vertex there"
    );
}

#[test]
fn unjoin_replaces_every_use_of_the_node_in_the_chosen_way() {
    let list = ways(&[(1, &[10, 2, 11, 2, 12]), (2, &[20, 2, 21])]);
    let plan = plan_unjoin(2, &list, 1, || -3).unwrap();
    assert_eq!(plan[0].after, vec![10, -3, 11, -3, 12]);
}

#[test]
fn unjoin_refuses_an_unknown_way_a_node_not_in_the_way_and_a_node_only_it_uses() {
    let list = ways(&[(1, &[10, 2, 11]), (2, &[20, 2, 21])]);
    assert!(!can_unjoin(2, &list, 99), "unknown way");
    assert!(!can_unjoin(77, &list, 1), "node in no way");
    assert!(!can_unjoin(20, &list, 1), "node not in this way");
    let alone = ways(&[(1, &[10, 2, 11, 2, 12])]);
    assert!(!can_unjoin(2, &alone, 1), "used twice, but by no other way");
}

fn deletion(nodes: &[i64], a: i64, b: i64) -> Result<SegmentDeletion, SegmentError> {
    plan_delete_segment(7, nodes, a, b)
}

#[test]
fn deleting_a_middle_segment_leaves_two_pieces_and_drops_no_node() {
    let d = deletion(&[1, 2, 3, 4], 2, 3).unwrap();
    assert_eq!(d.way_id, 7);
    assert_eq!(d.before, vec![1, 2, 3, 4]);
    assert_eq!(d.segment, [2, 3]);
    assert_eq!(d.pieces, vec![vec![1, 2], vec![3, 4]]);
    assert!(d.dropped_nodes().is_empty());
}

#[test]
fn the_segment_may_be_named_in_either_order() {
    let forward = deletion(&[1, 2, 3, 4], 2, 3).unwrap();
    let backward = deletion(&[1, 2, 3, 4], 3, 2).unwrap();
    assert_eq!(forward, backward);
    assert_eq!(backward.segment, [2, 3], "as the way lists them");
}

#[test]
fn deleting_the_first_or_last_segment_just_shortens_the_way_and_drops_the_end_node() {
    let start = deletion(&[1, 2, 3, 4], 1, 2).unwrap();
    assert_eq!(start.pieces, vec![vec![2, 3, 4]]);
    assert_eq!(start.dropped_nodes(), vec![1]);
    assert!(start.as_split().is_none(), "nothing is split off");

    let end = deletion(&[1, 2, 3, 4], 4, 3).unwrap();
    assert_eq!(end.pieces, vec![vec![1, 2, 3]]);
    assert_eq!(end.dropped_nodes(), vec![4]);
    assert!(end.as_split().is_none());
}

#[test]
fn a_piece_of_one_node_is_dropped_with_its_node() {
    let d = deletion(&[1, 2, 3], 2, 3).unwrap();
    assert_eq!(d.pieces, vec![vec![1, 2]]);
    assert_eq!(d.dropped_nodes(), vec![3]);
    let d = deletion(&[1, 2, 3], 1, 2).unwrap();
    assert_eq!(d.pieces, vec![vec![2, 3]]);
    assert_eq!(d.dropped_nodes(), vec![1]);
}

#[test]
fn a_two_node_way_is_all_segment() {
    assert_eq!(deletion(&[1, 2], 1, 2), Err(SegmentError::WholeWay));
    assert_eq!(deletion(&[1, 2], 2, 1), Err(SegmentError::WholeWay));
}

#[test]
fn nodes_that_are_not_neighbours_are_not_a_segment() {
    assert_eq!(
        deletion(&[1, 2, 3, 4], 1, 3),
        Err(SegmentError::NotAdjacent)
    );
    assert_eq!(
        deletion(&[1, 2, 3, 4], 1, 9),
        Err(SegmentError::NotAdjacent)
    );
    assert_eq!(deletion(&[], 1, 2), Err(SegmentError::NotAdjacent));
    assert_eq!(deletion(&[1], 1, 1), Err(SegmentError::NotAdjacent));
}

#[test]
fn a_way_that_runs_the_same_two_nodes_twice_is_ambiguous() {
    assert_eq!(deletion(&[1, 2, 1, 2], 1, 2), Err(SegmentError::Ambiguous));
    assert_eq!(
        deletion(&[5, 1, 2, 3, 2, 1, 6], 1, 2),
        Err(SegmentError::Ambiguous)
    );
}

#[test]
fn deleting_a_segment_of_a_closed_way_opens_it_there() {
    let ring = [1, 2, 3, 4, 1];
    let d = deletion(&ring, 2, 3).unwrap();
    assert_eq!(d.pieces, vec![vec![3, 4, 1, 2]], "starts after the segment");
    assert!(d.dropped_nodes().is_empty());
    assert!(d.as_split().is_none());

    let first = deletion(&ring, 1, 2).unwrap();
    assert_eq!(first.pieces, vec![vec![2, 3, 4, 1]]);

    let last = deletion(&ring, 4, 1).unwrap();
    assert_eq!(
        last.pieces,
        vec![vec![1, 2, 3, 4]],
        "the closing segment leaves the ring's own order"
    );
    assert_eq!(last.segment, [4, 1]);
}

#[test]
fn the_smallest_ring_opens_into_three_nodes() {
    let d = deletion(&[1, 2, 3, 1], 3, 1).unwrap();
    assert_eq!(d.pieces, vec![vec![1, 2, 3]]);
    let d = deletion(&[1, 2, 3, 1], 1, 2).unwrap();
    assert_eq!(d.pieces, vec![vec![2, 3, 1]]);
}

#[test]
fn as_split_keeps_the_longer_piece_and_the_first_of_equal_ones() {
    let even = deletion(&[1, 2, 3, 4], 2, 3).unwrap().as_split().unwrap();
    assert_eq!(even.way_id, 7);
    assert_eq!(even.before, vec![1, 2, 3, 4]);
    assert_eq!(even.parts, vec![vec![1, 2], vec![2, 3], vec![3, 4]]);
    assert_eq!(even.keeper, 0);
    assert_eq!(even.keep(), [1, 2]);
    assert_eq!(even.new_parts(), vec![&[2, 3][..], &[3, 4][..]]);

    let right_longer = deletion(&[1, 2, 3, 4, 5], 2, 3)
        .unwrap()
        .as_split()
        .unwrap();
    assert_eq!(right_longer.keeper, 2);
    assert_eq!(right_longer.keep(), [3, 4, 5]);

    let left_longer = deletion(&[1, 2, 3, 4, 5], 3, 4)
        .unwrap()
        .as_split()
        .unwrap();
    assert_eq!(left_longer.keeper, 0);
    assert_eq!(left_longer.keep(), [1, 2, 3]);
}

#[test]
fn as_split_cuts_the_original_line_in_two_steps() {
    let plan = deletion(&[1, 2, 3, 4, 5, 6], 3, 4)
        .unwrap()
        .as_split()
        .unwrap();
    let cuts = plan.cuts();
    assert_eq!(cuts.len(), 2);
    assert_eq!(cuts[0].before, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn reversing_a_node_swaps_its_facing_direction() {
    assert_eq!(
        reversed_node_tag_changes(&tag_list(&[("direction", "forward")])),
        vec![change("direction", Some("forward"), Some("backward"))]
    );
    assert_eq!(
        reversed_node_tag_changes(&tag_list(&[("direction", "backward")])),
        vec![change("direction", Some("backward"), Some("forward"))]
    );
    assert_eq!(
        reversed_node_tag_changes(&tag_list(&[("traffic_signals:direction", "forward")])),
        vec![change(
            "traffic_signals:direction",
            Some("forward"),
            Some("backward")
        )]
    );
}

#[test]
fn reversing_a_node_keeps_absolute_directions_and_unrelated_tags() {
    for (key, value) in [
        ("direction", "north"),
        ("direction", "90"),
        ("direction", "both"),
        ("highway", "traffic_signals"),
        ("name", "forward"),
        ("forward", "yes"),
    ] {
        assert!(
            reversed_node_tag_changes(&tag_list(&[(key, value)])).is_empty(),
            "{key}={value}"
        );
    }
    assert!(reversed_node_tag_changes(&[]).is_empty());
}

#[test]
fn reversing_a_node_moves_forward_and_backward_key_parts() {
    assert_eq!(
        reversed_node_tag_changes(&tag_list(&[("traffic_sign:forward", "maxspeed")])),
        vec![
            change("traffic_sign:backward", None, Some("maxspeed")),
            change("traffic_sign:forward", Some("maxspeed"), None),
        ]
    );
}

#[test]
fn reversing_a_node_with_both_sides_swaps_their_values() {
    assert_eq!(
        reversed_node_tag_changes(&tag_list(&[
            ("traffic_sign:forward", "stop"),
            ("traffic_sign:backward", "give_way"),
        ])),
        vec![
            change("traffic_sign:backward", Some("give_way"), Some("stop")),
            change("traffic_sign:forward", Some("stop"), Some("give_way")),
        ]
    );
}

#[test]
fn reversing_a_node_returns_changes_sorted_by_key() {
    let changes = reversed_node_tag_changes(&tag_list(&[
        ("traffic_signals:direction", "forward"),
        ("direction", "backward"),
        ("sign:forward", "x"),
    ]));
    let keys: Vec<&str> = changes.iter().map(|c| c.0.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
    assert_eq!(changes.len(), 4);
}

#[test]
fn reversing_a_node_twice_gives_the_original_tags() {
    let original = tag_list(&[
        ("direction", "forward"),
        ("traffic_sign:forward", "stop"),
        ("traffic_sign:backward", "give_way"),
        ("highway", "stop"),
    ]);
    let apply = |tags: &[(String, String)]| -> Vec<(String, String)> {
        let mut map: BTreeMap<String, String> = tags.iter().cloned().collect();
        for (key, _, new) in reversed_node_tag_changes(tags) {
            match new {
                Some(v) => map.insert(key, v),
                None => map.remove(&key),
            };
        }
        map.into_iter().collect()
    };
    let once = apply(&original);
    assert_ne!(once, {
        let mut o = original.clone();
        o.sort();
        o
    });
    let twice = apply(&once);
    let mut expected = original;
    expected.sort();
    assert_eq!(twice, expected);
}
