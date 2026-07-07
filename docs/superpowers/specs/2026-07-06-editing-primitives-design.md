# Editing Primitives — Design

## Problem

The only edit operation today is "drag a node to move it" (with undo/redo). A real editor needs the next tier of JOSM-standard operations: delete, insert/remove a way vertex, draw a new way, split/join ways, and square corners.

## Depends on

`EditState` and `next_new_id` from the OSM XML Export plan — every op here either sets a dirty/deleted flag or allocates a new negative id through that struct, so Export should land first.

## Design

### Shared plumbing

Extend `UndoableAction` (in `main.rs`) with one variant per op below. Each variant stores enough before/after state to reverse itself, following the existing `MoveNodes` pattern (store old and new full node/way records, not deltas). Each op:

1. Mutates the layer's `OsmData` in place.
2. Updates `EditState` (marks modified/deleted, or registers a new id).
3. Pushes an `UndoableAction` onto the existing `UndoStack`.
4. Triggers the same re-render/cache-rebuild path node-move already uses (`OsmLayer` cache invalidation).

No new undo infrastructure — this plan is additive variants on what #45 already built.

### Ops

**1. Delete node / delete way**
- Trigger: `Delete`/`Backspace` key with a selection.
- Node: if referenced by any way, refuse with a status-line message ("node is part of N ways — remove from way first") rather than silently detaching it. Unreferenced node: mark deleted, remove from `OsmData.nodes`.
- Way: mark deleted, remove from `OsmData.ways`; member nodes are left alone (JOSM behavior — deleting a way never deletes its nodes).
- Multi-select (from #43's box select) deletes every selected node/way in one undo step.

**2. Insert node into way**
- Trigger: right-click on a way segment (between two existing vertices) → context menu "Insert node here".
- Hit-test: reuse `point_to_segment_distance` from `selection.rs` (already used for way picking) to find the nearest segment and the fractional position along it.
- Creates a new node at the clicked point (registered via `EditState::next_new_id`), inserts its id into the way's node list at the correct index.

**3. Remove node from way**
- Trigger: right-click on a way-vertex node → context menu "Remove from way".
- Removes the node's id from the way's node list. If the node is now unreferenced by any way, it is *not* auto-deleted (JOSM leaves orphaned nodes for the user to explicitly delete) — just marks the way modified.

**4. Draw new way**
- Trigger: menu action "Draw Way" (no default keybinding yet — add one under a `Draw` submenu next to `Undo`/`Redo`).
- Enters a modal draw state on `MapViewer` (`drawing: Option<Vec<(f64,f64)>>` of geo points placed so far). Each click appends a point (snaps to an existing node within a small pixel radius, reusing existing hit-test, otherwise creates a new node). `Enter`/double-click finalizes: creates a new way from the accumulated node ids. `Esc` cancels and discards all points placed so far (no partial commit, no undo entry for a cancelled draw).
- Rendered live as an in-progress polyline overlay (separate from `OsmLayer`'s canvas — a `MapViewer`-level overlay, since it isn't committed data yet).

**5. Split way**
- Trigger: select a node that is an interior vertex (not an endpoint) of exactly one selected way → context menu "Split way here".
- Splits the way's node list at that index into two ways: original way keeps nodes `[0..=idx]` (retains original id, tags), new way gets nodes `[idx..]` (new id via `EditState`, tags copied from the original — JOSM duplicates tags onto both halves).

**6. Join ways**
- Trigger: select exactly two ways sharing an endpoint node → context menu "Join ways" (available from either way's context menu when the other is also selected).
- Requires matching tags or user confirmation if tags differ — **for this plan, refuse the join with a status-line message if tag sets differ** (no merge-conflict UI; that's out of scope). If tags match (or one/both have no tags), concatenate node lists (reversing one side if needed so the shared node is adjacent), keep the id of whichever way was selected first, mark the other deleted.

**7. Square corners**
- Trigger: select a closed way (first node id == last node id) → `Q` key (JOSM convention).
- For each vertex, compute the angle formed by its two neighbors; any vertex within a threshold (JOSM uses ~15°) of 90° or 180° gets adjusted so the polygon's corners become exact right angles, using JOSM's iterative least-squares orthogonalization approach (or a simpler single-pass projection if the geometry is simple rectangles — implementer's call on algorithm complexity, but must handle the common "almost-rectangular building" case correctly, verified with a unit test using a slightly-skewed rectangle fixture).
- Single undo entry covering all moved nodes in the way.

### Testing

- Ops 1, 2, 3, 5, 6 are pure data-structure mutations on `OsmData` + `EditState` — fully unit-testable without touching GPUI (TDD, following the `aggregate_tags`-style pure-function tests already in this codebase).
- Op 7 (square corners) is pure geometry — unit test against known input/output coordinate fixtures.
- Op 4 (draw way) has a pure "finalize" step (list of points → new way) that's unit-testable; the modal interaction (click-to-place, Esc-to-cancel) is GUI-only and verified by build + a manual spot-check list, per this project's existing convention (the gpui window can't be driven or screenshotted here).
- Each op's undo/redo round-trip gets a test mirroring the existing `undo_redo_at_empty_stack_is_none` / `push_then_undo_then_redo_round_trips` style tests in `main.rs`.

## Explicitly out of scope

- Tag-conflict resolution UI for way-join (refuse instead).
- Multi-way split/join beyond the pairwise cases above.
- Relation membership updates when nodes/ways are deleted or split (relations remain unedited elsewhere in the app; this is consistent with that).
