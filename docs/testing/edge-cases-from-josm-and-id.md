# Editing edge cases catalogued from JOSM and iD

Working document. It lists situations that JOSM (GPL-2.0-or-later) and iD
(ISC) test for editing operations, described in our own words, and records
what this app does about each. Nothing here is copied from either project:
only the situations and the expected behaviour (facts) are used, with the
JOSM test class and method or Trac ticket number, or the iD spec file and
test title, as a pointer. The tests are in `src/edge_case_tests/` and were
written from scratch against this app's own operations.

Where JOSM and iD differ, the app should follow JOSM unless iD is clearly
safer for data validity or relation integrity.

Status column: `pass` = covered by a new passing test; `FAIL` = covered by an
`#[ignore]`d test that currently fails (a bug or gap in this app, test name
given); `design` = the app deliberately differs, the test documents what it
does; `absent` = the app has no such feature, no test; `existing` = already
covered by tests elsewhere in the repo (not duplicated).

Test names are in `src/edge_case_tests/<file>.rs`. Severity of bugs is given
in the bug list at the end.

## Split way

| # | Situation | JOSM / iD expectation | Reference | Status |
|---|-----------|-----------------------|-----------|--------|
| S1 | Way in a route, halves must stay adjacent, in travel order | Both halves are members next to each other, in route order | JOSM `SplitWayCommandTest.testRouteRelation`, `testTicket18596`, `testTicket17400`; iD `split.js` "member ordering" | pass `route_halves_stay_adjacent_in_travel_order` |
| S2 | Same, but the route drives against the way's node order | New half goes first when the route needs it; role copied | JOSM `testIncompleteMembersOrderedRelationCorrectOrderTest`, `testTicket21856`; iD "splits dual-carriageway route, preserving role (backward way)" | pass `route_driven_against_the_way_direction_puts_the_new_half_first` |
| S3 | Way appears twice in a route (out and back) | Each occurrence gets its half on the right side | JOSM `testTicket21856DoublePoints`, `...DoublePointsRouteMiddle`, `testTicket19432`; iD "splitting out-and-back routes", "hat routes", "spoon routes" | pass `way_used_twice_in_a_route_gets_the_halves_in_the_right_order_each_time` |
| S4 | Turn restriction, `from`/`to` way | The half touching the `via` stays; other half is not a member | JOSM `SplitWayActionTest.testTicket17810`; iD `split.js` restriction cases 1b-3c | pass `restriction_from_way_follows_the_half_touching_the_via_node`, `restriction_from_way_moves_to_the_new_half_when_that_touches_the_via` |
| S5 | Turn restriction, `via` way is split | Both halves are `via` | JOSM `testTicket20163`; iD 6b/6c | pass `splitting_a_via_way_keeps_both_halves_as_via` |
| S6 | Restriction whose via cannot be found | iD/JOSM warn or refuse | iD `parent_incomplete` | existing (`split_refuses_a_restriction_whose_via_is_unknown_and_changes_nothing`) |
| S7 | Multipolygon outer/inner ring split at two nodes | Both halves keep the role | iD "type = multipolygon" | pass `multipolygon_ring_split_at_two_nodes_keeps_the_role_on_both_halves`, `closed_multipolygon_ring_member_is_split_with_both_halves_kept` |
| S8 | Boundary relation with admin_centre and several outer ways | Other members and order untouched | JOSM `testTicket18596` (boundary data) | pass `boundary_outer_ring_keeps_order_and_roles_of_the_other_members` |
| S9 | Way's relation is itself a member of another relation | Parent untouched | iD `split.js` parent relations | pass `a_relation_that_is_a_member_of_another_relation_is_not_disturbed` |
| S10 | Route with members that are not downloaded | JOSM warns/downloads when order is uncertain; iD refuses when too incomplete; unknown members must not be lost or reordered | JOSM `testOneMemberOrderedRelationShowsWarningTest`, `testTicket17400`, `testTicket18863`; iD "returns 'parent_incomplete'" | pass `incomplete_route_keeps_unknown_members_in_place` (unknown members safe; **design**: the app guesses "after" and does not warn, see gaps) |
| S11 | Closed way, one split node | JOSM opens the ring at that node; iD splits at the node and its opposite | iD "splits a closed way at the given point and its antipode" | design `closed_way_needs_two_nodes_and_one_node_changes_nothing` (refuses, no change) |
| S12 | Closed way split at two nodes | Two open ways | iD "splits a closed way at the given points" | pass (`closed_area_split_gives_two_open_ways...`) |
| S13 | Closed area (`building`, `area=yes`) split | JOSM copies tags to both halves; iD converts to a multipolygon | iD "splits an area by converting it to a multipolygon" | design `closed_area_split_gives_two_open_ways_that_both_keep_the_area_tags` (JOSM behaviour) |
| S14 | Roundabout in a route | iD refuses to split; JOSM allows | iD `simple_roundabout` | design `roundabout_in_a_route_splits_and_both_halves_stay_members` (JOSM behaviour, relation stays whole) |
| S15 | Split at an end node | Not possible | iD `not_eligible` | pass `splitting_at_an_end_node_is_refused_and_changes_nothing` |
| S16 | Split at several nodes of an open way at once | Way cut into several parts | iD "splits an open way at multiple given points" | absent (refused, unchanged): `splitting_an_open_way_at_two_nodes_at_once_is_not_supported` |
| S17 | Node the way passes through twice (figure eight, P shape) | Split at the interior occurrence | JOSM `SplitWayActionTest.testTicket11184`; iD "splits self-intersecting ways" | absent (refused, unchanged): `a_node_the_way_passes_through_twice_cannot_be_split_here` |
| S18 | Node shared by several ways | Every way through it is split | JOSM `testTicket11184`, `testTicket18477`; iD "splits multiple ways at an intersection" | pass `split_at_a_shared_node_splits_every_way_through_it` |
| S19 | Which part keeps the id/version | Longer part (iD); JOSM: chosen by strategy, default longest | iD "gives the previous id to the longest way" | pass `the_longer_part_keeps_the_original_id_and_version` (same as iD; ties keep the first part) |
| S20 | Upload order after split | New way created before the relation that names it | upload API rule | pass `split_new_way_is_created_before_the_relation_that_names_it` |
| S21 | Undo / redo of every split above | One undo restores exactly; redo recreates the same new id | JOSM command undo tests | pass (every S test checks it) |
| S22 | Split distributes iD "steps"/ conveying counts | Proportional | iD "distributes the number of steps proportionally" | absent (no `step_count` handling) |

## Reverse way

| # | Situation | Expectation | Reference | Status |
|---|-----------|-------------|-----------|--------|
| R1 | `oneway=yes` / `-1` | JOSM flips to the opposite value; iD keeps the tag unless asked | JOSM `ReverseWayTagCorrectorTest.testTagSwitch`; iD `reverse.js` "preserves oneway tags" | design `oneway_tags_are_kept_when_the_nodes_are_reversed` (iD behaviour: reversing is how the user turns traffic around) |
| R2 | `*:forward` / `*:backward`, `*:left` / `*:right` keys, `lanes:forward`, `turn:lanes:forward` | Keys swap | JOSM same test; iD "directional keys on ways" | pass `forward_backward_and_left_right_keys_swap` |
| R3 | `incline=up/down`, numeric incline sign, `direction=forward/backward` | Values flip | JOSM `testTagSwitch` (#19508); iD "reverses incline" | pass `incline_and_direction_values_flip` |
| R4 | `name=forward`, `note=up`, `ref=left`, `type=drawdown` | Left alone | JOSM #8499; iD "skips ... for ignored tags" | pass `names_and_notes_that_look_directional_are_left_alone` |
| R5 | `conveying=forward` | iD keeps it by default | iD "preserves conveying tags" | pass `conveying_is_kept_like_oneway` |
| R6 | `sidewalk=left` / `right` value | iD swaps | iD "transforms *=right <=> *=left" | pass `sidewalk_value_left_becomes_right` |
| R7 | Any key with `forward`/`backward` value (`priority`, `trolley_wire`, `railway:preferred_direction`) | Values swap | JOSM generic value rule; iD "transforms *=forward <=> *=backward" | pass `forward_backward_values_flip_for_any_key_except_free_text` |
| R8 | `cycleway:left:oneway=-1` | Key side swaps and value flips (relative to the way) | JOSM #10977 | pass `side_specific_oneway_value_flips_with_the_side` |
| R9 | Route role `forward` / `backward` of the reversed way | Swapped | JOSM and iD "reverses relation roles" | pass `route_roles_forward_and_backward_swap` |
| R10 | Restriction `from` / `to` | Not swapped | iD | pass `restriction_from_and_to_roles_are_not_swapped` |
| R11 | Direction tags on the way's nodes (`direction=forward`, `traffic_signals:direction`, `traffic_sign:forward`) | Flipped; compass values stay (JOSM) / are mirrored (iD) | JOSM `testSwitchingWayNodes`, `testNotSwitchingWayNodes`, `testIsReversible` (#20013); iD "directional values on childnodes" | pass `direction_tags_on_the_ways_nodes_flip` |
| R12 | Undo / redo | Nodes and tags in one step | | pass (all R tests) and `reversing_two_ways_is_one_undo_step...` |
| R13 | `direction=clockwise/anticlockwise`, `forwards` (with s) | iD drops the s | iD "drops 's' from forwards/backwards" | not verified (not tested) |
| R14 | Adding a `direction` to direction-less stop signs when reversing | iD field-dependent | iD "directionless nodes" | absent |

## Join / combine ways

| # | Situation | Expectation | Reference | Status |
|---|-----------|-------------|-----------|--------|
| J1 | Second way has to be reversed to chain | Reverse it, union tags | JOSM `CombineWayActionTest.testTicket11957`, `testDetectReversedWays`, `testTicket18367`; iD "joins a <-- b ==> c" | pass `join_reverses_a_way_to_make_the_chain` |
| J2 | Direction-dependent tags decide which way is reversed | Never reverse a way whose tags depend on direction if the other can be | JOSM `testTicket18387`; iD "takes tag reversals into account" | pass `join_keeps_the_direction_of_a_way_that_cannot_be_reversed` |
| J3 | Two arcs close into a ring | Closed way | iD join | pass `join_closes_two_arcs_into_a_ring` |
| J4 | T junction / three ways at one end | Refused | iD `not_adjacent`, "returns falsy for more than two ways" | pass `join_refuses_a_t_junction_and_three_ways_meeting_at_an_end` |
| J5 | Conflicting tags | Refused, key named (JOSM dialog; iD `conflicting_tags`) | iD join.js | pass `join_with_conflicting_tags_names_the_key_and_changes_nothing` |
| J6 | Which way survives | iD / JOSM keep the oldest / the one on the server | iD "prefers to keep the oldest way" | design `join_of_two_server_ways_keeps_the_first_selected_id` (server way wins over a new one, else first selected) |
| J7 | Both ways in the same route, adjacent | One membership remains | iD "merges relations", "preserves duplicate route segments" | pass `join_into_a_member_of_a_route_keeps_the_route_in_order` |
| J8 | Only the removed way is a member | Membership moves to the survivor | iD "merges relations" | pass `join_where_only_the_removed_way_is_a_member_points_the_member_at_the_survivor` |
| J9 | Both in a route but not adjacent | iD `conflicting_relations`; JOSM asks | iD join.js, JOSM combine dialog | FAIL `join_of_ways_that_are_not_adjacent_in_a_route_is_refused` |
| J10 | Restriction member / role clash | Refused | iD "relation would be damaged" | pass `join_refuses_a_turn_restriction_member_and_a_role_clash` |
| J11 | Result beyond the node limit | Refused | OSM API limit | existing (`join_is_refused_when_the_result_would_pass_the_way_node_limit`) |
| J12 | Join collapses a single-member multipolygon into an area | iD converts | iD join.js 710-781 | absent |
| J13 | Overlapping segments (#18385, #18367) | Combine anyway / split first | JOSM CombineWayActionTest | not verified (join of ways that share a segment is not offered) |

## Merge nodes

| # | Situation | Expectation | Reference | Status |
|---|-----------|-------------|-----------|--------|
| M1 | Tags unioned, ways and relation roles move to the survivor | | iD merge_nodes.js, JOSM `MergeNodesActionTest` | pass `merge_unions_tags_and_moves_ways_and_relation_roles_to_the_survivor` |
| M2 | Survivor: server node over new node | iD keeps the oldest; JOSM prefers a node with an id | iD "keeps the id of the existing node" | pass `merge_prefers_the_server_node_over_a_node_made_in_this_session`; design: two server nodes keep the first selected |
| M3 | Neighbours in one way | No repeated node | iD "merges two nodes along a single way" | pass `merge_of_neighbours_in_one_way_does_not_repeat_the_node` |
| M4 | Ends of an open way merged | Closed way | | pass `merge_of_the_two_ends_of_an_open_way_closes_it` |
| M5 | Way would collapse to one node | Refused (JOSM) | | pass `merge_refuses_to_collapse_a_two_node_way_to_one_node` |
| M6 | Triangle collapses to two distinct nodes | iD deletes the degenerate way | iD connect (degenerate ways) | FAIL `merge_must_not_leave_a_degenerate_closed_way` |
| M7 | Same relation, different roles | Both memberships kept | | pass `merge_keeps_both_memberships_of_one_relation_when_the_roles_differ` |
| M8 | Tag conflict | Refused, key named | iD / JOSM conflict dialog | pass `merge_with_conflicting_tags_names_the_key_and_changes_nothing` |

## Disconnect / unglue

| # | Situation | Expectation | Reference | Status |
|---|-----------|-------------|-----------|--------|
| D1 | Node shared by two ways, tagged, in a relation | JOSM keeps tags on one copy; iD copies tags and position to the new node | JOSM `UnGlueActionTest.testSingleNodeInTwoWays`; iD "copies location and tags" | design `disconnect_gives_the_later_way_a_copy_and_the_original_keeps_tags_and_memberships` (copy is bare, original keeps tags and memberships) |
| D2 | Same way visits the node twice | Later visit gets the new node | iD "replaces later occurrences in a self-intersecting way" | pass `disconnect_replaces_a_later_visit_of_the_same_way` |
| D3 | Closing node of a ring shared with another way | Ring stays closed, one new node | iD "disconnects the closing node of an area without breaking the area" | pass `disconnecting_a_ring_from_another_way_keeps_the_ring_closed` |
| D4 | Node on one way only | Nothing to do | JOSM `testSingleNodeInSingleWay`; iD `not_connected` | pass `disconnect_of_a_node_used_once_is_refused` |
| D5 | JOSM "UnJoinNodeWay" (pull a node off a way, ticket 10396) | | JOSM `UnJoinNodeWayActionTest` | absent |

## Delete

| # | Situation | Expectation | Reference | Status |
|---|-----------|-------------|-----------|--------|
| X1 | Way in a route | Removed from the relation, rest in order; relation edited before the way is deleted in the upload | iD `delete_way.js` "removes a way from parent relations"; JOSM `DeleteCommandTest.testReferredDelete` | pass `deleting_a_way_removes_it_from_the_route_and_keeps_the_rest_in_order` |
| X2 | Way listed twice in a relation | Both memberships go | JOSM #19432 flavour | pass `a_way_listed_twice_in_a_route_loses_both_memberships` |
| X3 | Last member of a relation | iD and JOSM delete the relation | iD "deletes parent relations that become empty" | design `deleting_the_last_member_of_a_relation_is_refused` |
| X4 | Tagged or shared nodes of a deleted way | Kept | iD "does not delete member nodes with interesting tags / referenced by another parent" | pass `deleting_a_way_keeps_nodes_that_are_tagged_or_shared` |
| X5 | Untagged, unshared nodes of a deleted way | Deleted with it | iD "deletes member nodes not referenced by another parent"; JOSM delete action | FAIL `deleting_a_way_deletes_its_untagged_unshared_nodes` |
| X6 | Delete a vertex of a way | Removed from the way | iD `delete_node.js` "removes the node from parent ways"; JOSM `DeleteCommandTest.testDeleteNodesInWay` | FAIL `deleting_a_vertex_removes_it_from_its_way` |
| X7 | Vertex delete leaves a way with fewer than two nodes | Way deleted too | iD "deletes parent ways that would otherwise have less than two nodes" | FAIL `deleting_a_vertex_that_leaves_a_way_too_short_deletes_that_way`; current behaviour pinned by `deleting_a_node_used_by_a_way_changes_nothing_today` |
| X8 | Way and its nodes selected together | One step, ways before nodes in the upload | | pass `deleting_a_way_with_its_nodes_in_one_selection_is_one_step` |
| X9 | Delete a relation | Members stay | iD `delete_relation.js` (iD also deletes untagged member ways; JOSM does not) | pass `deleting_a_relation_keeps_its_members` (JOSM behaviour) |
| X10 | Delete an inner ring of a multipolygon | Outer stays a member | | pass `deleting_an_inner_ring_leaves_the_outer_member` |
| X11 | Node member not on any way | Removed from the relation | | pass `deleting_a_node_member_that_is_not_on_a_way_edits_the_relation` |
| X12 | Members that are not downloaded | Kept, in place | JOSM #18863 flavour | pass `unknown_members_survive_edits_of_their_relation` |
| X13 | Restriction role removal, relation member of relation | Refused | | existing |
| X14 | Delete way segment (JOSM `DeleteCommandTest.testDeleteWaySegment*`) | | | absent |

## Relation ordering, multipolygons, restrictions, geometry, copy/paste

| # | Situation | Reference | Status |
|---|-----------|-----------|--------|
| O1 | Relation sorting (RelationSorterTest, WayConnectionTypeCalculatorTest: roundabouts, loops, oneway/forward/backward) | JOSM `gui/dialogs/relation/sort` | absent (no member sorting; app only edits member order manually) |
| O2 | Multipolygon building from loose ways (MultipolygonBuilder tests, CreateMultipolygonActionTest #17767 ...) | JOSM | absent (no "create multipolygon" action; multipolygon rendering is covered by `src/multipolygon.rs` tests) |
| O3 | Validator tests (DuplicateNode, CrossingWays, MultipolygonTest, TurnrestrictionTest, RelationChecker) | JOSM `data/validation/tests` | existing in `src/validator/` (not repeated) |
| G1 | Orthogonalize / circularize / straighten / align / distribute | JOSM `OrthogonalizeActionTest`, `CreateCircleActionTest`, `AlignIn*ActionTest`; iD `orthogonalize.js`, `circularize.js`, `straighten_*.js` | existing in `src/geometry.rs`, `src/align.rs` and `selection_actions` tests |
| G2 | Simplify way, join node-way, extrude, parallel way | JOSM `SimplifyWayActionTest`, `JoinNodeWayActionTest`, `ExtrudeActionTest`, `ParallelWayActionTest` | extrude/parallel existing; simplify and join-node-to-way absent |
| C1 | Copy / paste / duplicate (JOSM `CopyActionTest`, `PasteAction`; PurgeAction #12038) | | existing in `feature_clipboard` and `main.rs` tests |
| C2 | Purge | | absent |
| C3 | Conflict/merge of downloaded data (`DataSetMergerTest`, #7481, #12599, #12616) | | existing in `src/layers/merge.rs` |

## Upload diff and ordering

| # | Situation | Expectation | Reference | Status |
|---|-----------|-------------|-----------|--------|
| U1 | New relation contains another new relation | Inner created first, even when the id order is the other way | JOSM `APIDataSetTest.testNewParentChildPair`, `testOneExistingAndThreNewInAChain` | pass `a_new_relation_that_contains_a_new_relation_comes_after_it` |
| U2 | Cycle of new relations | JOSM reports a cyclic dependency | JOSM `testOneCycle` | pass `relation_reference_cycles_still_produce_a_document` (no hang; the server rejects it) |
| U3 | Deletions: relations, then ways, then nodes; after all modifications | | JOSM #9624 `testDeleteOneParentTwoNewChildren`, #9656 `testDeleteWay` | pass `deletions_go_relations_then_ways_then_nodes_after_all_modifications` |
| U4 | Node moved, its way deleted | Both uploaded, modify before delete | | pass `moving_a_node_and_deleting_its_way_uploads_both` |
| U5 | Created then deleted | Nothing to upload | | pass `created_then_deleted_in_one_session_uploads_nothing` |
| U6 | Modified then reverted | Nothing to upload | | pass `modified_then_reverted_uploads_nothing` |
| U7 | Creates nodes, ways, relations; modifies nodes, ways, relations | | | pass `modifications_are_ordered_nodes_ways_relations_and_creates_nodes_ways_relations` |

## Parsing and export

| # | Situation | Reference | Status |
|---|-----------|-----------|--------|
| P1 | Negative ids and references to them | JOSM `OsmReaderTest` | pass `negative_ids_are_read_as_given_and_references_resolve` |
| P2 | `action="delete"` / `modify` kept apart from data | JOSM file format | pass `deleted_elements_are_kept_apart_from_the_data_and_modified_ones_noted` |
| P3 | No tags, no members, no version | `testMissingVersion` | pass `objects_without_tags_or_members_and_missing_versions_are_accepted` |
| P4 | Missing id / ref / key, unknown member type, bad number | `testMissingId`, `testMissingRef`, `testIllegalType`, `testMissingKeyValue` | pass `malformed_input_is_an_error_not_a_guess` |
| P5 | Empty value, duplicate key | | pass `an_empty_tag_value_and_duplicate_keys_are_handled` |
| P6 | `& < > " '`, newline, CR, tab, padding, unicode in tags and roles | OSM XML escaping | pass `awkward_characters_survive_a_write_and_read` |
| P7 | Pending edits written and read back | JOSM action attributes | pass `pending_edits_round_trip_through_the_action_attributes` |
| P8 | `upload="false"`, `visible="false"`, incomplete objects | JOSM | absent |

## Undo / redo interplay

| # | Situation | Status |
|---|-----------|--------|
| Z1 | Split then join back: net nothing to upload; two undos; redo keeps ids | pass `split_then_join_again_leaves_nothing_to_upload_and_undoes_in_two_steps` |
| Z2 | Split, disconnect, merge, join, undo all four, redo all four | pass `split_separate_merge_join_then_undo_all_and_redo_all` |
| Z3 | Delete way in a route, undo restores way, version and slot | pass `delete_then_undo_restores_the_way_its_version_and_its_route_slot` |
| Z4 | New edit after undo drops the redo branch | pass `a_new_edit_after_undo_discards_the_redo_branch` |

## Further situations (batch A)

Tests in `src/edge_case_tests/interactions_reverse_split.rs`.

| # | Situation | Expectation | Status |
|---|-----------|-------------|--------|
| A1 | Reverse a way whose node (stop sign `direction=forward`) is shared with a second way that is not reversed | The node still serves the other way, so its direction should not flip | FAIL `reversing_one_way_leaves_a_direction_tag_on_a_node_shared_with_another_way` (the tag is flipped to `backward`) |
| A1b | Same node shared by two ways that are both reversed | Flipped once, not twice | pass `a_node_shared_by_two_reversed_ways_is_flipped_exactly_once` |
| A2 | Reverse a roundabout (`junction=roundabout`) | Ring reversed, closing node kept, tags unchanged | pass `reversing_a_roundabout_reverses_the_ring_and_keeps_the_closing_node` |
| A2b | Reverse a closed `area=yes` + `oneway=yes` way; closed building | Stays closed, oneway/area kept, side keys swapped | pass `reversing_a_oneway_area_keeps_the_area_and_oneway_tags_and_swaps_side_keys`, `reversing_a_closed_building_keeps_it_closed` |
| A3 | Split a way that is a restriction `via` and a route member | Both halves `via` in travel order; route halves adjacent with the role; one undo | pass `splitting_a_way_that_is_a_restriction_via_and_a_route_member_updates_both` |
| A4 | Split a way that is both `from` and `to` of a U-turn | Both roles follow the half touching the `via` | pass `splitting_the_way_that_is_both_from_and_to_of_a_u_turn_keeps_a_valid_restriction`, `splitting_a_u_turn_way_at_the_far_side_of_the_via_keeps_both_roles_on_the_original` |
| A5 | Duplicate consecutive node `[1,1,2,3]`: reverse | Literal reverse | pass `reversing_a_way_with_a_duplicated_node_reverses_it_literally` |
| A5b | Split at the doubled node `[1,2,2,3]` | Refused, unchanged (like S17) | design `a_node_listed_twice_in_a_row_cannot_be_split_at` |
| A5c | Split elsewhere in a way with a doubled node; join with it at the end | Duplicate kept, no dangling refs | pass `splitting_elsewhere_in_a_way_with_a_duplicated_node_keeps_the_duplicate`, `joining_a_way_with_a_duplicated_end_node_keeps_the_duplicate`, `joining_ways_that_both_end_on_the_duplicated_node_chains_them` |
| A6 | One way listed under two roles (`forward`, `backward`) in a route: split | Each occurrence gets its own halves with its own role, in the right order | pass `split_gives_each_occurrence_of_a_member_its_own_role` |
| A6b | Same, delete | Both memberships removed | pass `deleting_a_member_with_two_roles_removes_both_memberships` |
| A6c | Same, join | Refused with "different role" and nothing changes | design `join_is_refused_when_the_way_has_different_roles_in_one_relation` |

Bug found in this batch: A1, LOW-MEDIUM. Reversing only one of two ways that
share a tagged node flips the node's `direction`, which then faces the wrong
way for the other way.

## Gaps that are not bugs (feature absent or deliberate difference)

- Split at several nodes at once; split at a node the way visits twice (S16,
  S17); closed way with one node (S11).
- Split of an incomplete route guesses "after" instead of warning (S10).
- iD converts a split area to a multipolygon; this app, like JOSM, leaves two
  open ways with the area tags (S13).
- Deleting the last member of a relation is refused, not cascaded (X3).
- Two server nodes / ways merged or joined keep the first selected, not the
  oldest (J6, M2).
- Disconnect leaves the new node untagged (D1).
- No relation sorting, multipolygon creation, simplify, purge, join-node-to-way.

## Bugs found (ignored tests)

Ordered by risk of corrupting data on upload.

1-3. FIXED. Reverse now swaps `forward`/`backward` route roles (R9), flips
   direction tags on the way's nodes (R11) and flips `left`/`right`/
   `forward`/`backward` values and side-specific oneways (R6-R8).
4. MEDIUM. J9 join of two ways that are not adjacent in a shared route drops the
   second membership without asking, which can leave the route disconnected.
   `join_of_ways_that_are_not_adjacent_in_a_route_is_refused`.
5. MEDIUM. X6/X7 a vertex cannot be deleted (the action refuses any node
   used by a way), so the user cannot take a node out of a way.
   `deleting_a_vertex_removes_it_from_its_way`,
   `deleting_a_vertex_that_leaves_a_way_too_short_deletes_that_way`.
6. LOW-MEDIUM. X5 deleting a way leaves its untagged unshared nodes as orphans
   that stay on the server. `deleting_a_way_deletes_its_untagged_unshared_nodes`.
7. LOW. M6 merging two nodes of a triangle leaves a closed way with two
   distinct nodes. `merge_must_not_leave_a_degenerate_closed_way`.
