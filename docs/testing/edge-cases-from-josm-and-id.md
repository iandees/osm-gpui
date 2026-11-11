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
| S11 | Closed way, one split node | JOSM opens the ring at that node; iD splits at the node and its opposite | iD "splits a closed way at the given point and its antipode" | design `closed_way_with_one_node_is_turned_to_start_and_end_there` (JOSM behaviour: the way is rewritten to start and end at the node, same id/version/tags/relations; refused if the node already is the start). See batch I |
| S12 | Closed way split at two nodes | Two open ways | iD "splits a closed way at the given points" | pass (`closed_area_split_gives_two_open_ways...`) |
| S13 | Closed area (`building`, `area=yes`) split | JOSM copies tags to both halves; iD converts to a multipolygon | iD "splits an area by converting it to a multipolygon" | design `closed_area_split_gives_two_open_ways_that_both_keep_the_area_tags` (JOSM behaviour) |
| S14 | Roundabout in a route | iD refuses to split; JOSM allows | iD `simple_roundabout` | design `roundabout_in_a_route_splits_and_both_halves_stay_members` (JOSM behaviour, relation stays whole) |
| S15 | Split at an end node | Not possible | iD `not_eligible` | pass `splitting_at_an_end_node_is_refused_and_changes_nothing` |
| S16 | Split at several nodes of an open way at once | Way cut into several parts | iD "splits an open way at multiple given points" | pass `splitting_an_open_way_at_two_nodes_at_once_gives_three_parts` and the batch I tests (one undo step, longest part keeps the id) |
| S17 | Node the way passes through twice (figure eight, P shape) | Split at the interior occurrence | JOSM `SplitWayActionTest.testTicket11184`; iD "splits self-intersecting ways" | pass `a_node_the_way_passes_through_twice_is_split_at_every_interior_pass`, `a_p_shaped_way_is_split_at_the_interior_pass_only` (JOSM: every interior pass is a cut). See batch I |
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
| J9 | Both in a route but not adjacent | iD `conflicting_relations`; JOSM asks | iD join.js, JOSM combine dialog | pass `join_of_ways_that_are_not_adjacent_in_a_route_is_refused` (refused with a status message) |
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
| M6 | Triangle collapses to two distinct nodes | iD deletes the degenerate way | iD connect (degenerate ways) | pass `merge_must_not_leave_a_degenerate_closed_way` (refused rather than deleted) |
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
| X5 | Untagged, unshared nodes of a deleted way | Deleted with it | iD "deletes member nodes not referenced by another parent"; JOSM delete action | pass `deleting_a_way_deletes_its_untagged_unshared_nodes` |
| X6 | Delete a vertex of a way | Removed from the way | iD `delete_node.js` "removes the node from parent ways"; JOSM `DeleteCommandTest.testDeleteNodesInWay` | pass `deleting_a_vertex_removes_it_from_its_way` |
| X7 | Vertex delete leaves a way with fewer than two nodes | Way deleted too | iD "deletes parent ways that would otherwise have less than two nodes" | pass `deleting_a_vertex_that_leaves_a_way_too_short_deletes_that_way` |
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
| O2 | Multipolygon building from loose ways (MultipolygonBuilder tests, CreateMultipolygonActionTest #17767 ...) | JOSM | implemented as the Create Multipolygon action, see batch J (rendering is covered by `src/multipolygon.rs` tests) |
| O3 | Validator tests (DuplicateNode, CrossingWays, MultipolygonTest, TurnrestrictionTest, RelationChecker) | JOSM `data/validation/tests` | existing in `src/validator/` (not repeated) |
| G1 | Orthogonalize / circularize / straighten / align / distribute | JOSM `OrthogonalizeActionTest`, `CreateCircleActionTest`, `AlignIn*ActionTest`; iD `orthogonalize.js`, `circularize.js`, `straighten_*.js` | existing in `src/geometry.rs`, `src/align.rs` and `selection_actions` tests |
| G2 | Simplify way, join node-way, extrude, parallel way | JOSM `SimplifyWayActionTest`, `JoinNodeWayActionTest`, `ExtrudeActionTest`, `ParallelWayActionTest` | extrude/parallel existing; join-node-to-way implemented (batch H: action plus snapping); simplify implemented (batch K) |
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
| A1 | Reverse a way whose node (stop sign `direction=forward`) is shared with a second way that is not reversed | The node still serves the other way, so its direction should not flip | pass `reversing_one_way_leaves_a_direction_tag_on_a_node_shared_with_another_way` |
| A1b | Same node shared by two ways that are both reversed | Flipped once, not twice | pass `a_node_shared_by_two_reversed_ways_is_flipped_exactly_once` |
| A2 | Reverse a roundabout (`junction=roundabout`) | Ring reversed, closing node kept, tags unchanged | pass `reversing_a_roundabout_reverses_the_ring_and_keeps_the_closing_node` |
| A2b | Reverse a closed `area=yes` + `oneway=yes` way; closed building | Stays closed, oneway/area kept, side keys swapped | pass `reversing_a_oneway_area_keeps_the_area_and_oneway_tags_and_swaps_side_keys`, `reversing_a_closed_building_keeps_it_closed` |
| A3 | Split a way that is a restriction `via` and a route member | Both halves `via` in travel order; route halves adjacent with the role; one undo | pass `splitting_a_way_that_is_a_restriction_via_and_a_route_member_updates_both` |
| A4 | Split a way that is both `from` and `to` of a U-turn | Both roles follow the half touching the `via` | pass `splitting_the_way_that_is_both_from_and_to_of_a_u_turn_keeps_a_valid_restriction`, `splitting_a_u_turn_way_at_the_far_side_of_the_via_keeps_both_roles_on_the_original` |
| A5 | Duplicate consecutive node `[1,1,2,3]`: reverse | Literal reverse | pass `reversing_a_way_with_a_duplicated_node_reverses_it_literally` |
| A5b | Split at the doubled node `[1,2,2,3]` | Refused, unchanged (a one-node part would be left) | design `a_node_listed_twice_in_a_row_cannot_be_split_at` |
| A5c | Split elsewhere in a way with a doubled node; join with it at the end | Duplicate kept, no dangling refs | pass `splitting_elsewhere_in_a_way_with_a_duplicated_node_keeps_the_duplicate`, `joining_a_way_with_a_duplicated_end_node_keeps_the_duplicate`, `joining_ways_that_both_end_on_the_duplicated_node_chains_them` |
| A6 | One way listed under two roles (`forward`, `backward`) in a route: split | Each occurrence gets its own halves with its own role, in the right order | pass `split_gives_each_occurrence_of_a_member_its_own_role` |
| A6b | Same, delete | Both memberships removed | pass `deleting_a_member_with_two_roles_removes_both_memberships` |
| A6c | Same, join | Refused with "different role" and nothing changes | design `join_is_refused_when_the_way_has_different_roles_in_one_relation` |

Bug found in this batch (fixed): A1, LOW-MEDIUM. Reversing only one of two
ways that share a tagged node used to flip the node's `direction`, which then
faced the wrong way for the other way. The node's tags are now left alone when
a way that is not being reversed also uses it.

## Further situations (batch C)

Tests in `src/edge_case_tests/limits_and_data.rs`: size limits, geometry near
the antimeridian and the poles, and Reverse on unusual tag keys and values.
Server limits are the defaults (2,000 nodes per way, 32,000 members per
relation). Big networks are built in code.

| # | Situation | Expectation | Status |
|---|-----------|-------------|--------|
| L1 | Merge nodes of two ways that both have 2,000 nodes | No way grows, nothing refused | pass `merge_between_two_full_ways_keeps_both_at_the_limit` |
| L2 | Merge two nodes of one 2,000 node way (far apart, neighbours, the two ends) | Length stays 2,000 (1,999 for neighbours) | pass `merge_inside_a_full_way_never_grows_it`, `merging_the_ends_of_a_full_open_way_closes_it_without_growing` |
| L3 | Delete a vertex of a 2,000 node way | Node removed (X6) | design `a_vertex_of_a_full_way_cannot_be_deleted_today` (refused, nothing changes) |
| L4 | Delete a vertex to bring a 2,001 node way back to the limit | Way shortened | pass `deleting_a_vertex_of_a_way_over_the_limit_shortens_it` (X6 is fixed) |
| L5 | Join to exactly 2,000 nodes (one way reversed), undo, redo; then one more node | One undo splits it back, redo repeats, 2,001 refused with no undo step recorded | pass `join_right_at_the_limit_then_undo_redo_and_one_more_node_is_refused` |
| L6 | Split a way in a relation of 32,000 members | Refused with "32,001 ... 32,000", nothing changed | design `split_that_would_push_a_relation_past_32000_is_refused_and_changes_nothing` |
| L7 | Same at 31,999 members | Goes through, exactly 32,000, undo/redo | pass `split_that_lands_exactly_on_32000_members_goes_through` |
| L8 | Way listed twice in a 31,999 member relation (split adds two) | Refused | design `a_way_listed_twice_needs_room_for_both_new_halves` |
| L9 | Way in a full relation and a small one | Refusal leaves both relations and the ways untouched | pass `one_full_relation_blocks_the_split_for_every_relation_the_way_is_in` |
| L10 | Relation already over the limit (32,001), edit that adds no members (Reverse) | Allowed | pass `a_relation_already_over_the_limit_still_allows_edits_that_do_not_grow_it` |
| L11 | Join two adjacent members of a 32,000 member relation | One member fewer | pass `join_inside_a_full_relation_keeps_it_within_the_limit` |
| L12 | Merge two nodes that are members with different roles in a 32,000 member relation | Both kept, count stays 32,000 | pass `merging_two_members_of_a_full_relation_does_not_grow_it` |
| G3 | Square / circularize at 179.9995 E, 179.9995 W, centred on 180, at 85 N / 85 S, 85 N on 180 | Finite, inside -90..90 / -180..180, nodes move a few metres | pass `square_and_circularize_stay_sane_*` |
| G4 | Align / distribute near the antimeridian (not crossing) and near the poles; align across it | Same | pass `align_and_distribute_stay_sane_*`, `align_stays_sane_for_a_line_that_straddles_the_antimeridian` |
| G5 | Distribute nodes on both sides of the antimeridian | Nodes stay within the span of the line | pass `distribute_stays_sane_for_a_line_that_straddles_the_antimeridian` |
| G6 | Extrude (reshape, move, new way) near the antimeridian, near the poles, and across 180 | Finite, valid range, near the ring | pass `extrude_stays_sane_near_the_antimeridian_and_the_poles`, `extrude_stays_sane_across_the_antimeridian` |
| K1 | Reverse keys that only look directional: `lane:forward_x`, `:forwards`, trailing space, `:Forward`, `:BACKWARD`, Cyrillic look-alike letter, leading `forward` key | Left alone | pass `keys_that_only_look_like_forward_or_backward_are_not_swapped` |
| K2 | Exact `forward` / `backward` parts in any position after the first, both halves of a pair, a look-alike next to them | Swapped, no tag lost or merged | pass `exact_forward_and_backward_parts_swap_in_any_position_after_the_first`, `swapping_keys_never_loses_or_merges_tags` |
| K3 | Values `Forward`, `forward `, `forwards`, `LEFT`, `right `, `backwards` (R13) | Only exact lower case flips; `forwards` is not read as legacy iD does | design `values_with_other_case_spaces_or_an_s_are_not_taken_for_directions`, `directional_values_of_directional_keys_follow_the_same_exact_rule` |
| K4 | Unicode keys and values | Untouched except real `:forward` parts | pass `unicode_keys_and_values_survive_reverse_untouched` |
| K5 | Node tags with the same odd keys / values on a reversed way | Same exact rule | pass `node_direction_tags_with_odd_keys_and_values_follow_the_exact_rule` |
| K6 | `direction=90` (compass degrees) on a way | Unchanged | pass `numeric_compass_direction_on_a_way_is_not_negated` |

Bugs from this batch: none open (G5 and K6 are fixed).

## Further situations (batch B)

Interactions between merge, disconnect, join and delete. Tests are in
`src/edge_case_tests/interactions_topology.rs`. No bugs found in this batch.

| # | Situation | Expectation | Status |
|---|-----------|-------------|--------|
| B1 | Merge two nodes of one way that are not neighbours (`[1,2,3,4]`, merge 2 and 4) | Allowed: `[1,2,3,2]`, node 4 deleted, one undo / redo | design `merge_of_non_neighbours_in_one_way_makes_the_way_revisit_the_survivor`; first selected survives (`merge_of_non_neighbours_with_the_other_node_selected_first_keeps_that_node`) |
| B2 | Disconnect: upload order | New node in `<create>`, the way that names it only in `<modify>` after it; undo / redo | pass `disconnect_creates_the_new_node_before_the_way_that_names_it` |
| B3 | Two disconnects in a row | Both nodes created before the modified ways; one undo per step | pass `disconnect_of_a_ring_and_a_second_disconnect_create_nodes_before_both_ways` |
| B4 | Join with one way selected | Refused, nothing recorded | pass `join_of_a_single_way_is_refused` |
| B5 | Join a closed way and an open way touching it | Refused (a ring has no free end; iD refuses too) | design `join_of_a_closed_way_and_an_open_way_touching_it_is_refused` |
| B6 | Join two closed ways sharing a node | Refused | pass `join_of_two_closed_ways_sharing_a_node_is_refused` |
| B7 | Join a way with itself (same way selected twice) | Refused, nothing recorded | pass `join_of_a_way_with_itself_changes_nothing` |
| B8 | Delete a way a relation lists twice (different roles, other members between), then undo | Both slots return at their original positions with their roles; redo removes both again | pass `deleting_a_way_listed_twice_then_undo_restores_both_slots_in_place` |
| B9 | Delete a way that is every member of a relation (listed twice) | Refused, like X3 | design `deleting_a_way_that_is_the_only_two_entries_of_a_relation_is_refused` |

## Further situations (batch D)

Undo / redo against refusals, autosave and several layers
(`src/edge_case_tests/undo_interactions.rs`).

| # | Situation | Status |
|---|-----------|--------|
| D1 | Join refused (T junction) between two edits, with a live redo branch: stack, cursor and revision unchanged, redo still works | pass `a_refused_join_between_two_edits_keeps_the_stack_and_the_redo_branch` |
| D2 | Merge refused (would collapse a two-node way) in the same position | pass `a_refused_merge_between_two_edits_keeps_the_stack_and_the_redo_branch` |
| D3 | Delete refused by the last-relation-member check in the same position | pass `a_delete_refused_by_a_relation_check_between_two_edits_keeps_the_stack_and_the_redo_branch` |
| D4 | After restore from autosave, pending edits are back but undo history is not persisted: nothing to undo; new edits undo only back to the restored state | design `a_restored_layer_keeps_its_pending_edits_but_has_no_undo_history` |
| D5 | Restoring a layer into a running view leaves existing steps and the redo branch untouched, and no step names the restored layer | pass `restoring_a_layer_into_a_running_view_leaves_the_existing_history_alone` |
| D6 | Several layers: the undo stack is one global list; undo reverts the layer a step was recorded on, not the active layer, and the active layer then follows the undone feature | design `undo_reverts_the_layer_the_step_was_recorded_on_not_the_active_layer` |
| D7 | Edits interleaved across two layers undo and redo in one shared order, restoring both layers exactly | pass `edits_interleaved_across_layers_undo_in_one_shared_order_and_redo_back` |
| D8 | A new edit in one layer discards the redo branch of another layer (global history) | design `a_new_edit_in_one_layer_drops_the_redo_branch_of_the_other_layer` |
| D9 | Per-layer undo stacks | absent (one global stack; deleting a layer prunes its steps, covered in `main.rs` tests) |
| D10 | Persisting undo history across a restart | absent |

No bugs found in this batch.

## Further situations (batch F)

Upload failures (`src/edge_case_tests/upload_failures.rs`). The tests run
`upload_chunks::run_chunked_upload` and `apply_to_layer` (what
`MapViewer::upload_to_osm` does on a background task) against `Server`, an
in-memory stand-in for the OSM API that applies a diff atomically and checks
versions, references, objects in use, negative placeholder ids and the
10,000 element cap, and can fail an upload before or after applying it.

| # | Situation | Expectation | Status |
|---|-----------|-------------|--------|
| F1 | One element of a diff is rejected (409 on one of three nodes) | Whole diff rolled back, changeset closed, all three edits pending | pass `one_rejected_element_rolls_back_the_whole_diff_and_every_edit_stays_pending` |
| F2 | Connection drops after the server applied the diff | `OutcomeUnknown` with the changeset url, diff sent once, changeset closed, edits still pending | pass `a_lost_response_after_the_server_applied_the_diff_is_reported_as_unknown_and_not_resent` |
| F2b | User presses Upload again after F2 | Creates exist twice; the editor cannot tell, the message sends the user to the changeset first | design `sending_again_after_an_unknown_outcome_duplicates_creates_and_conflicts_on_modifies` |
| F3 | Changeset 2 of 3 refused (429); retry | Chunk 1 kept in the layer, rest pending, retry sends only the rest, nothing created twice | pass `a_refusal_in_a_later_changeset_keeps_the_accepted_ones_and_retry_sends_only_the_rest` |
| F3b | Response lost for changeset 2 of 3 | Chunk 3 never sent, chunk 2 stays pending (same hazard as F2b) | pass `a_lost_response_in_a_later_changeset_leaves_that_chunk_pending_and_the_rest_unsent` |
| F3c | New way and its new nodes on either side of a changeset boundary | Way names server ids | pass `a_way_split_from_its_new_nodes_by_a_changeset_boundary_names_server_ids` |
| F4 | 409 version mismatch | Names the node and both versions; retry fails the same way; "keep mine" then succeeds, undo history kept | pass `a_409_names_the_node_and_both_versions_and_a_retry_with_keep_mine_goes_through` |
| F5 | 412 referenced node gone | `MissingDependency` naming the way; edits kept; retry safe | pass `a_412_for_a_node_that_is_gone_names_the_way_that_needs_it` |
| F6 | 412 delete of an object another way now uses | `StillInUse` naming that way; delete still pending and undoable | pass `a_412_for_deleting_something_still_in_use_lists_the_user` |
| F7 | 429 | Plain `Http` error with status and body shown, not retried by itself, retry works | pass `a_429_is_a_definite_refusal_with_the_status_shown_and_a_retry_sends_the_edit_once` |
| F8 | 500/502/503/504 / 509 | Unknown outcome / definite refusal | pass `server_errors_are_unknown_outcomes_except_509_which_is_refused_before_any_work` |
| F9 | Changeset closed under us (409) | Reported, edits kept | pass `a_changeset_closed_under_us_is_reported_as_a_refusal_with_the_edits_kept` |
| F10 | Exactly 10,000 changes | One changeset | pass `exactly_10000_changes_go_in_one_changeset` |
| F11 | 10,001 changes | Split 10,000 + 1 (the server refuses the single document) | pass `one_change_over_the_cap_is_split_into_two_changesets_and_the_whole_diff_would_be_refused` |
| F12 | Failure in changeset 2 of an oversized upload | Exactly the rest pending; retry sends only it | pass `a_failure_in_the_second_changeset_of_an_oversized_upload_leaves_exactly_the_rest_pending` |
| F13 | New relations that contain each other, more than a changeset | Refused before anything is sent (checked with a limit of 1) | design `a_cycle_of_new_relations_bigger_than_a_changeset_is_refused_before_anything_is_sent` |
| F14 | Undo of a tag edit after upload, redo | A new edit on the new version; uploads cleanly | pass `undoing_a_tag_edit_after_it_was_uploaded_is_a_new_edit_on_the_new_version` |
| F15 | Undo of a split after upload | Used to restore way 10 whole next to the uploaded half (two ways on the same nodes); now the step is dropped | pass (fixed) `undoing_a_split_after_it_was_uploaded_does_not_leave_the_uploaded_half_behind` |
| F16 | Undo of a delete after upload | Used to re-create the objects with their old positive ids in `<create>`, which the server refuses; now the step is dropped | pass (fixed) `undoing_a_delete_after_it_was_uploaded_does_not_try_to_modify_a_deleted_object` |
| F17 | Server changed an element we did not edit | Does not block the upload | pass `an_unrelated_change_on_the_server_does_not_block_uploading_other_edits` |
| F18 | Edit then undo of an element the server changed | Nothing to upload, no conflict | pass `editing_then_undoing_an_element_the_server_changed_uploads_nothing_and_cannot_conflict` |

Bugs found in this batch (both fixed): F15 and F16, MEDIUM. After an accepted
changeset the undo history still named placeholder ids of objects the server
had created and objects it had deleted. Undoing such a step corrupted the
layer or produced an upload the server refuses. `uploaded_taint` in
`main.rs` now removes the steps that name created or deleted objects after
each accepted changeset, like conflict resolution already does.

## Further situations (batch G)

Nodes and ways shared by several relations, copy / paste of members, and
255-character tag limits (`src/edge_case_tests/shared_members.rs`).

| # | Situation | Expectation | Status |
|---|-----------|-------------|--------|
| G1 | Merge two nodes that sit in 7 relations (via of two restrictions, route stops with different roles, same role far apart and adjacent, multipolygon label, one node under two roles) | Every membership moves to the survivor with its role and position; only an equal neighbour collapses; relations are edited before the node is deleted; one undo | pass `merging_nodes_in_many_relations_moves_every_membership_to_the_survivor`, `merging_in_the_other_order_keeps_the_same_memberships_on_the_other_node` |
| G2 | Merge where only the removed node has memberships | They move to the survivor | pass `merging_into_a_survivor_that_is_in_no_relation_still_carries_the_other_nodes_memberships` |
| G3 | Delete a node that is a stop and an admin centre | Removed from ways and both relations, one undo | pass `deleting_a_node_that_is_a_stop_and_an_admin_centre_cleans_up_both_relations` |
| G4 | Delete the via node of a restriction that is also a stop and an admin centre | Refused, nothing half-done | design `deleting_a_restriction_via_node_is_refused_and_leaves_the_other_relations_alone` |
| G5 | Delete a node that is the only member of one relation and a stop of another | Refused, the other relation keeps its stop | design `deleting_the_only_member_of_one_relation_is_refused_even_if_the_others_would_be_fine` |
| G6 | Delete a vertex that shortens one way and would delete a restriction's `to` way | Refused before the first way is shortened | pass `deleting_a_vertex_whose_short_way_is_a_restriction_member_changes_nothing` |
| G7 | Delete a refused node and an allowed node together | Only the allowed one goes, one step | design `deleting_a_refused_and_an_allowed_node_together_deletes_only_the_allowed_one_in_one_step` |
| G8 | Disconnect a node that is a stop / admin centre / label | The original node keeps every membership, the copy is in none, no relation modified | design `disconnecting_a_stop_node_leaves_every_membership_on_the_original_node`, `the_membership_stays_with_the_original_node_even_when_its_own_way_is_the_one_detached` |
| G9 | Disconnect the via node of a restriction so the from / to way loses it | Used to succeed and leave the restriction invalid; now refused naming the way and relation | pass (was FAIL, fixed) `disconnecting_a_restriction_via_node_must_not_silently_break_the_restriction`, `disconnecting_an_unrelated_way_from_a_restriction_via_node_is_allowed` |
| G10 | Copy a way that is in restrictions and routes | The copy is in no relation; relations unchanged; Duplicate of a stop node likewise | pass `a_pasted_way_is_not_a_member_of_the_relations_the_original_was_in`, `duplicating_a_stop_node_does_not_copy_its_memberships` |
| G11 | Copy a relation with only some members | Copied members point at the copies, others at the originals | pass `a_pasted_relation_keeps_pointing_at_originals_that_were_not_copied` |
| G12 | Paste a relation into another layer | Server ids stay, another layer's unsaved members are dropped | design `pasting_a_relation_into_another_layer_drops_unsaved_members_that_layer_cannot_have` |
| G13 | Reverse swaps `lanes:forward` / `lanes:backward` (255-char values) | Both keys kept, values exchanged, nothing truncated | pass `reverse_swaps_a_pair_of_opposite_keys_without_losing_either_or_truncating` |
| G14 | Reverse renames a 255-char key ending `:forward`, or negates a 255-char incline | Used to produce 256 characters; now refused, nothing changed | pass (was FAIL, fixed) `reverse_must_not_grow_a_key_past_255_characters`, `reverse_must_not_grow_a_value_past_255_characters` |
| G15 | Merge nodes / join ways with equal or different 255-char values for one key | Equal: kept once, untruncated. Different: refused naming the key | pass `merge_nodes_unions_tags_with_255_character_keys_and_values_untouched`, `merge_nodes_with_different_255_character_values_for_one_key_is_refused`, `join_unions_tags_with_255_character_values_untouched`, `join_with_different_255_character_values_for_one_key_is_refused` |

Bugs found in this batch (fixed): G9 and G14.

## Further situations (batch H)

Join node to way: the `Join Node to Way` selection action and snapping while
placing or dragging a node (`src/snap.rs`, `src/edge_case_tests/snap_join_node.rs`).
Snapping is off while Alt (Option) or Ctrl is held; what may snap to what is the
`CLASS_RULES` / `COMPAT` table in `src/snap.rs` (unit tested there).

| # | Situation | Expectation | Status |
|---|-----------|-------------|--------|
| H1 | Node joined to a way: which segment, which index | Nearest segment; inserted after its start; node moved onto the line; one undo step; undo / redo exact | pass `node_is_inserted_on_the_nearest_segment_and_moved_onto_the_line`, `insertion_index_follows_the_segment_for_every_position` |
| H2 | Way passes the node's position twice (out and back over one line) | Deterministic: the lowest-indexed equally near segment | pass `a_way_that_passes_the_node_twice_takes_the_first_pass` |
| H3 | Closed way, node nearest the closing segment | Inserted before the repeated first node; still closed | pass `a_closed_way_can_be_joined_on_its_closing_segment` |
| H4 | Way already at 2,000 nodes | Refused with the limit message, nothing changed | pass `a_full_way_refuses_one_more_node` |
| H5 | Node already in the way | Refused (no duplicate, no consecutive duplicate), nothing changed | pass `a_node_already_in_the_way_is_refused_without_a_duplicate` |
| H6 | Node is a relation member; target way is in route and restriction relations | Memberships and roles untouched in both | pass `a_node_that_is_a_relation_member_stays_one_and_the_way_relations_are_unchanged` |
| H7 | Upload of a node created this session joined to a way | Node created before the modified way, once, at its final position | pass `a_new_node_goes_up_before_the_modified_way` |
| H8 | Selection is not exactly one node and one way | Action not applicable, nothing changed | pass `it_needs_exactly_one_node_and_one_way` |
| H9 | Drag a node onto a way | Inserted on release, one undo step | pass `dragging_a_node_onto_a_way_inserts_it_as_one_undo_step` |
| H10 | Same, with Alt or Ctrl held | Plain move, no snap | pass `the_disable_modifier_leaves_a_dragged_node_free` |
| H11 | Drag a road node onto a building | No snap, no highlight | pass `a_node_does_not_snap_onto_a_way_of_an_incompatible_kind` |
| H12 | Highlight while dragging | Target shown while it applies; Alt clears it | pass `the_snap_target_is_highlighted_while_dragging_and_cleared_after` |
| H13 | Drag a node onto another node | Merged by the Merge Nodes planner, one undo step | pass `dragging_a_node_onto_another_node_merges_them` |
| H14 | Add mode: way being drawn is tagged / untagged | Tagged: only compatible ways; untagged: any; the modifier always disables | pass `add_mode_snaps_a_way_being_drawn_only_onto_compatible_ways`, `add_mode_with_an_untagged_way_snaps_and_the_modifier_prevents_it` |

## Further situations (batch E)

Multipolygon-aware editing, in `src/edge_case_tests/multipolygon_edits.rs`.
Outer ring `[1,2,3,4,1]` and inner triangle `[5,6,7,5]` unless noted.

| # | Situation | Expectation | Status |
|---|-----------|-------------|--------|
| E1 | Merge an outer-ring node with an inner-ring node (rings touch) | Inner ring now uses the first selected node, relation untouched, one undo / redo | pass `merging_a_node_of_the_outer_ring_with_one_of_the_inner_ring_touches_the_rings`, `merging_the_inner_node_first_keeps_the_inner_ring_node` |
| E2 | Merge nodes of two different multipolygons | Rings touch, both relations untouched | pass `merging_nodes_of_two_different_multipolygons_joins_their_rings` |
| E3 | Reverse the outer ring, the inner ring, both at once, twice | Roles stay, only the way(s) uploaded (no relation modify), one undo step | pass `reversing_the_outer_ring_keeps_its_role_and_leaves_the_relation_alone`, `reversing_the_inner_ring_keeps_its_role`, `reversing_both_rings_at_once_is_one_undo_step`, `reversing_a_ring_twice_returns_to_the_original_and_uploads_nothing` |
| E4 | Reverse a ring with `fence_type:left` and a node with `direction=forward` | Side key swapped, node flipped, relation tags and members untouched | pass `reversing_a_ring_flips_direction_tags_but_not_the_relation` |
| E5 | Reverse a ring that is outer of one multipolygon and inner of another | Both relations untouched | pass `reversing_a_ring_shared_by_two_multipolygons_leaves_both_alone` |
| E6 | Merge two nodes of the inner triangle (neighbours, or the closing node with its neighbour) | Refused, nothing recorded (M6) | design `merging_two_nodes_of_a_triangle_ring_is_refused`, `merging_the_closing_node_with_its_neighbour_in_a_triangle_is_refused` |
| E7 | Merge two nodes of the square (leaves a triangle); merge across rings | Ring stays closed with 3 distinct points; across rings no ring shrinks | pass `merging_two_nodes_of_a_square_ring_leaves_a_valid_triangle`, `merging_across_the_rings_cannot_shrink_either_ring` |
| E8 | Delete a node of the inner triangle | Degenerate ring is deleted and dropped from the relation, outer stays; one undo restores | pass `deleting_a_node_of_a_triangle_ring_deletes_the_ring_and_drops_it_from_the_relation` |
| E9 | Same, but the triangle is the relation's only member | Refused (X3), nothing recorded | design `deleting_a_node_that_would_remove_the_only_ring_of_a_multipolygon_is_refused` |
| E10 | Delete an ordinary or the closing node of the square | Ring stays closed, role and membership kept | pass `deleting_a_node_of_a_square_ring_keeps_it_a_member_with_its_role`, `deleting_the_closing_node_of_a_ring_keeps_it_closed` |
| E11 | Split a closed ring that is outer of one multipolygon and inner of another | New half added to both relations with each one's own role; new way created before the relations that name it; undo / redo | pass `splitting_a_ring_shared_by_two_multipolygons_gives_each_its_own_role_on_both_halves`, `split_ring_upload_creates_the_new_way_before_both_relations_that_name_it` |
| E12 | Join the two halves again | Both relations back to one member each (roles kept); two undos restore the start | pass `joining_the_halves_of_a_shared_ring_restores_both_multipolygons` |
| E13 | Split an open shore way listed by two multipolygons | Both get the new half next to the original | pass `splitting_a_shared_shore_way_updates_both_multipolygons` |
| E14 | Join two shore ways that both multipolygons list | Each relation ends with one member | pass `joining_two_shores_that_both_multipolygons_list_merges_each_relation` |
| E15 | Join a shore listed by two multipolygons with a way only one of them lists | Should be refused: the other multipolygon would not be able to keep its shore | pass `joining_a_shore_with_a_way_only_one_multipolygon_has_is_refused` (fixed) |

Bug found in this batch (fixed): E15, MEDIUM. Joining way 10 (only in lake
900) with shore 11 (in lakes 900 and 901) removed 11 and, because the survivor
10 was not in lake 901, `relations::join_members` renamed 11 to 10 there. Lake
901 then listed the whole ring `[1,2,3,4,1]` plus its other shore, silently
changing its geometry. The join is now refused (`JoinError::SurvivorNotInArea`)
when a removed way is in a multipolygon the survivor is not in.

## Further situations (batch I)

Split Way at several nodes, at a node visited twice, and a closed way at one
node (S11, S16, S17), in `src/edge_case_tests/split.rs` and the planner tests
in `src/topology.rs`. Rules, as in JOSM:

- Every selected node is a cut at each of its interior occurrences in an open
  way; an occurrence at the first or last position is not a cut, and a
  selected node that is only an end node (or not on the way) is ignored. No
  cut at all is refused.
- The longest part (most nodes; the first of equals) keeps the original id,
  version and (so) the modify; every other part is a new way with a copy of
  the tags. Relations are updated one cut at a time with the same rules as a
  single split, so each new part gets the route order / role, and a
  restriction `from`/`to` follows the part touching `via`.
- A closed way with two or more selected nodes (each once in the ring) is cut
  into that many arcs. With exactly one selected node it is rewritten to start
  and end at that node, but only when it is the only way using the node or
  was selected too, so splitting a street at a junction does not also turn a
  building. Nothing new is created, so id, version, tags and relations stay.
- The selection is not changed by a split.

| # | Situation | Expectation | Status |
|---|-----------|-------------|--------|
| I1 | Open way `[1..6]`, nodes 2 and 5 | Three parts, middle keeps id 10 and version 3, ends are created with copied tags, one undo step | pass `splitting_an_open_way_at_two_nodes_at_once_gives_three_parts` |
| I2 | Which part keeps the id with three parts | Longest; first of equals | pass `open_way_several_cuts_keep_the_longest_part_and_the_first_of_equals` |
| I3 | Selected end node or node of another way among the cuts | Ignored; only end nodes: refused, unchanged | pass `open_way_cut_ignores_a_selected_end_node_and_a_node_not_on_the_way` |
| I4 | Two nodes shared by several ways | Every way through them is cut at the nodes it has | pass `two_selected_nodes_split_every_way_through_them` |
| I5 | Route, several cuts, with the way running with / against the route | Parts in travel order next to the neighbours | pass `route_stays_in_travel_order_after_a_multi_node_split`, `route_driven_against_the_way_direction_stays_ordered_after_a_multi_node_split` |
| I6 | Way listed twice in a route (out and back), two cuts | Parts in each pass in the right order | pass `way_used_twice_in_a_route_is_split_into_every_part_each_time` |
| I7 | Restriction `from` way cut twice | Role goes to the part touching `via`; unknown `via` refuses the whole split | pass `restriction_from_way_follows_the_part_touching_the_via_after_a_multi_node_split`, `restriction_with_an_unknown_via_refuses_a_multi_node_split_whole` |
| I8 | Multipolygon ring member cut at several nodes | Every part has the role | pass `multipolygon_ring_member_cut_at_several_nodes_keeps_the_role_on_every_part`, `closed_way_cut_at_three_nodes_gives_three_arcs` |
| I9 | Upload order | Every new part is in `<create>` before the relation that names it | pass `every_new_part_is_created_before_the_relation_that_names_it_in_the_upload` |
| I10 | Node limit | Parts are never longer than the way; an overlong offline way is valid again | pass `split_parts_stay_within_the_way_node_limit_of_the_server` |
| I11 | Several cuts would push a relation past the member maximum | Refused whole, nothing changed | pass `split_that_adds_parts_to_a_full_relation_is_refused_whole` |
| I12 | Closed way that visits a node twice | Refused, unchanged | pass `a_ring_node_visited_twice_still_cannot_be_split` |
| I13 | Figure eight through node 3 (S17) | `[1,2,3]`, `[3,4,5,3]` (keeps id), `[3,6]`; in a route all three in order | pass `a_node_the_way_passes_through_twice_is_split_at_every_interior_pass`, `figure_eight_in_a_route_gets_all_three_parts_in_order` |
| I14 | P shape, ending or starting on the node | Only the interior pass is a cut | pass `a_p_shaped_way_is_split_at_the_interior_pass_only` |
| I15 | Closed way, one node (S11) | Rewritten to start and end there; same id/version/tags/relations; start node refused | pass `closed_way_with_one_node_is_turned_to_start_and_end_there`, `closed_way_one_node_without_selecting_the_way_and_at_the_start_node` |
| I16 | Junction of a street and a building ring | Only the street is split unless the ring is selected | design `closed_way_one_node_leaves_a_ring_alone_when_a_street_is_split_at_the_junction` |

Where the arcs of a multipolygon ring go when no neighbour decides (I8) is
not specified: they follow the original, which is harmless for rings.

## Further situations (batch J)

Create Multipolygon (`src/edge_case_tests/create_multipolygon.rs`, planner in
`src/multipolygon_create.rs`), modelled on JOSM `CreateMultipolygonAction`.
Design: one `type=multipolygon` relation holds every ring (several outers are
allowed in one relation, as in JOSM); open ways are chained into rings; a ring
nested inside an even number of other rings is `outer`, odd is `inner`; members
are listed outers first, then inners, each group in the order the selection
first names its ways. Tags move only in the conservative case: exactly one
outer ring made of one way that carries an area key (`landuse`, `natural`,
`building`, `water`, ...); then those keys and `name` move to the relation and
everything else (`source`, `boundary`, `fixme`, ...) stays on the way. With
several outers, a ring of several ways, or no area key, nothing moves.
Relations the ways are already in are untouched (a way already in another
multipolygon is allowed and mentioned in the status). One undo step; the
relation is selected afterwards. Differences from JOSM: no dialog, no reuse of
an existing multipolygon, no shortcut, and rings that merely touch at a node are
accepted while rings that cross, overlap along an edge, or have a node on the
other's edge are refused.

| # | Situation | Expectation | Status |
|---|-----------|-------------|--------|
| J1 | Lake with an island | One relation, outer then inner, `natural=water` moved, `source` stays; relation selected; one step | pass `a_lake_with_an_island_becomes_one_relation_with_the_area_tags_moved` |
| J2 | Undo / redo | Relation gone and tags back; redo identical; nothing left to upload after undo | pass `undo_removes_the_relation_and_puts_the_tags_back_and_redo_repeats_it` |
| J3 | Two outers, an inner each | One relation, four members, tags stay on the ways | pass `two_outers_with_an_inner_each_share_one_relation_and_keep_their_tags` |
| J4 | Ring from two open ways | Both `outer`, no tag move | pass `a_ring_made_of_two_open_ways_gets_both_as_outer_and_keeps_their_tags` |
| J5 | Inner ring from two open ways | Both `inner` | pass `an_inner_ring_may_be_built_from_open_ways_too` |
| J6 | Nested three deep | outer, inner, outer | pass `rings_nested_three_deep_alternate_outer_inner_outer` |
| J7 | Inner touching the outer at a node | Accepted | pass `an_inner_touching_the_outer_at_a_shared_node_is_accepted` |
| J8 | Inner running along the outer's edge from a shared node | Refused (overlap) | pass `an_inner_running_along_the_outer_from_a_shared_node_is_refused` |
| J9 | Two outers meeting at a corner | Both `outer` | pass `two_outers_touching_at_one_corner_are_both_outer` |
| J10 | Crossing rings; rings sharing an edge | Refused, nothing changes | pass `crossing_rings_are_refused_and_nothing_changes`, `rings_sharing_an_edge_are_refused` |
| J11 | Chain that does not close; three open ways at one node; ring through a node twice | Refused with the node or way named | pass `a_chain_that_does_not_close_is_refused`, `three_open_ways_ending_at_one_node_are_refused_as_ambiguous`, `a_ring_that_touches_itself_is_refused` |
| J12 | Nothing selected, or only nodes | Hint, no step | pass `nothing_selected_or_only_nodes_is_refused_with_the_hint` |
| J13 | Tag move | Only area keys and `name` move; `source`, `boundary`, `fixme` stay; a `name` alone does not move; untagged ways stay untagged | pass `only_area_tags_and_the_name_move_everything_else_stays_on_the_way`, `a_name_alone_does_not_move_and_untagged_ways_stay_untagged`, `a_single_closed_way_makes_a_one_member_multipolygon` |
| J14 | Ring with 3,000 nodes | Fine: the way node limit applies to editing ways, and no way changes here | pass `a_ring_with_thousands_of_nodes_is_fine_the_node_limit_belongs_to_editing_ways` |
| J15 | Ways already in another multipolygon and a route | Their relations untouched, also after undo; the shared multipolygon is noted | pass `relations_the_ways_are_already_in_are_untouched_and_a_shared_multipolygon_is_noted` |
| J16 | Upload order with new ways | Relation after the ways that it names | pass `new_ways_go_up_before_the_relation_that_names_them` |

Member limit: the new relation is checked with `limits::relation_over_limit`
like New Relation (covered by `new_relation_is_refused_past_the_member_limit`);
no separate test.

No bugs found in this batch.

## Further situations (batch K)

Simplify Way (`src/edge_case_tests/simplify_way.rs`, geometry in
`geometry::simplify_keep`). Design:
Douglas-Peucker in the local metric plane (antimeridian-safe) with a
tolerance, a persisted setting (Settings > Editing, `simplify_tolerance_m`,
default 3 m, valid 0.1 to 100 m, anything else reads as the default) that is
read on every run and named in the status. Kept: both ends, any vertex
used by another way or visited twice by the same way, tagged vertices, and
relation members. A closed way that would drop under 4 entries is left alone.
Dropped vertices are by construction used by nothing else, so they are
deleted; the way edit is recorded before the node deletions, as one undo step.
The selection stays; the status says how many nodes went, or "Nothing to
simplify" (no undo step). Tools menu and context menu, no shortcut.

| # | Situation | Expectation | Status |
|---|-----------|-------------|--------|
| K1 | Straight line with midpoints | Only the ends stay; midpoints deleted; one step; selection kept | pass `a_straight_line_with_midpoints_loses_them` |
| K2 | Gentle curve beyond tolerance | Nothing changes, no step | pass `a_gentle_curve_is_kept` |
| K3 | One kink among near-straight vertices | Only the in-tolerance vertices go | pass `only_the_vertices_within_tolerance_go` |
| K4 | Junction, tagged, relation-member vertex | Kept | pass `a_junction_vertex_is_kept`, `a_tagged_vertex_is_kept`, `a_relation_member_vertex_is_kept` |
| K5 | Two-node way; way whose middle is a junction | Nothing to simplify | pass `ends_and_a_way_that_cannot_shrink_are_left_alone` |
| K6 | Closed way with midpoints | Corners stay, ring stays closed with 4+ entries | pass `a_closed_way_stays_a_valid_ring` |
| K7 | Tiny closed way within tolerance | Untouched (would collapse) | pass `a_tiny_closed_way_that_would_collapse_is_not_touched` |
| K8 | Node visited twice by one way | Kept | pass `a_node_visited_twice_by_one_way_is_kept` |
| K9 | Several ways sharing vertices | Shared vertices stay in every way; only selected ways change | pass `several_ways_sharing_vertices_each_keep_the_shared_ones`, `a_vertex_in_the_middle_of_two_selected_ways_stays_in_both`, `a_way_that_is_not_selected_is_not_simplified` |
| K10 | Way across the antimeridian | Simplified by ground distance, kink kept | pass `ways_across_the_antimeridian_simplify_by_ground_distance` |
| K12 | Tolerance setting | Larger tolerance removes more on the same input; bad or out-of-range values fall back to 3 m; round-trips through `settings.json` | pass `a_larger_tolerance_removes_more_vertices_on_the_same_input`, `simplify_tolerance_defaults_when_missing_and_validates`, `round_trip` |
| K11 | Undo / redo and upload | One step each way; way modified before nodes deleted; undo leaves nothing to upload | pass `upload_modifies_the_way_before_deleting_the_nodes` (undo/redo in K1, K4, K6, K9, K10) |

No bugs found in this batch.

## Gaps that are not bugs (feature absent or deliberate difference)

- A closed way with one split node is turned to start there (JOSM); iD splits
  at the node and its opposite (S11). A closed way that visits a node twice is
  still not split (batch I, I12).
- Split of an incomplete route guesses "after" instead of warning (S10).
- iD converts a split area to a multipolygon; this app, like JOSM, leaves two
  open ways with the area tags (S13).
- Deleting the last member of a relation is refused, not cascaded (X3).
- Two server nodes / ways merged or joined keep the first selected, not the
  oldest (J6, M2).
- Disconnect leaves the new node untagged (D1).
- No relation sorting, multipolygon creation, simplify, purge.
- Join node to way never targets a way that already contains the node (JOSM can
  put a node into a closed way a second time); snapping also refuses it (H5).

## Bugs found (ignored tests)

Ordered by risk of corrupting data on upload.

1-3. FIXED. Reverse now swaps `forward`/`backward` route roles (R9), flips
   direction tags on the way's nodes (R11) and flips `left`/`right`/
   `forward`/`backward` values and side-specific oneways (R6-R8).
4. FIXED (join is now refused). J9 join of two ways that are not adjacent in a shared route drops the
   second membership without asking, which can leave the route disconnected.
   `join_of_ways_that_are_not_adjacent_in_a_route_is_refused`.
5. FIXED. X6/X7 a vertex can now be deleted: it is removed from its ways, and
   a way left too short is deleted. `deleting_a_vertex_removes_it_from_its_way`,
   `deleting_a_vertex_that_leaves_a_way_too_short_deletes_that_way`.
6. FIXED. X5 deleting a way now also deletes its untagged unshared nodes.
   `deleting_a_way_deletes_its_untagged_unshared_nodes`.
7. FIXED (merge is now refused). M6 merging two nodes of a triangle leaves a closed way with two
   distinct nodes. `merge_must_not_leave_a_degenerate_closed_way`.
8. FIXED. G9 disconnecting a restriction's via node could leave its from or to
   way off the via node. Separate is now refused in that case.
9. FIXED. G14 Reverse could push a key or value past 255 characters
   (`:forward` to `:backward`, a negated incline); it is now refused.
