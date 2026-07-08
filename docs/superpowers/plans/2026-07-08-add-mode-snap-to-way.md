# Add mode: snap new nodes onto existing ways — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In Add mode, a click near an existing way's line geometry snaps the newly created node onto that way (spliced into the way's node list at the correct position) instead of creating a free-standing node; holding Control bypasses all snapping for that click.

**Architecture:** Two new pure/geometry primitives (`nearest_point_on_segment` in `selection.rs`, `OsmLayer::snap_to_way` in `osm_layer.rs`) reuse the existing `hit_test_segment` rtree query. `handle_add_click` tries `snap_to_way` before creating a node (unless Ctrl is held), routing through `insert_node_into_way` instead of `add_node`. A new compound undo variant, `SnapExtendWay`, covers the 2nd-click case where a single click both splices a node into the snapped-onto way and folds it into the way being drawn.

**Tech Stack:** Rust, GPUI (with the already-enabled `test-support` feature), `rstar` (rtree, via existing `way_index`).

## Global Constraints

- Reuse the existing `6.0` px snap tolerance (the value already used by Extrude mode's `hit_test_segment` call at `src/main.rs:685`) — no new configuration.
- Follow this codebase's existing test conventions exactly: `OsmLayer::new_with_data`/`viewport_centered_on`/`data_with`-style helpers for layer-level tests (see `src/layers/osm_layer.rs:2397-2434`), plain `#[test]` for pure-logic tests, `#[gpui::test]` + `TestAppContext` for the new App-level tests.
- Every undo entry must keep "one click = one undo step" (see `src/undo.rs:48-58`'s documented rationale for `ExtendWay`).
- Do not modify `hit_test_segment`, `insert_node_into_way`, `point_to_segment_distance`, or any other reused building block — only add alongside them.

---

### Task 1: `nearest_point_on_segment` pure helper

**Files:**
- Modify: `src/selection.rs` (add function after `point_to_segment_distance`, ~line 66; add tests in the existing `#[cfg(test)] mod tests` block, ~line 278)

**Interfaces:**
- Produces: `pub fn nearest_point_on_segment(p: Point<Pixels>, a: Point<Pixels>, b: Point<Pixels>) -> Point<Pixels>` — used by Task 2.

- [ ] **Step 1: Write the failing tests**

Add to `src/selection.rs`'s existing `mod tests` block (which already has a `pt(x, y) -> Point<Pixels>` helper at line 283-285), right after the `zero_length_segment_returns_point_distance` test:

```rust
    #[test]
    fn nearest_point_on_segment_orthogonal_projection() {
        let q = nearest_point_on_segment(pt(5.0, 3.0), pt(0.0, 0.0), pt(10.0, 0.0));
        assert!((q.x.as_f32() - 5.0).abs() < 1e-4, "got {:?}", q);
        assert!((q.y.as_f32() - 0.0).abs() < 1e-4, "got {:?}", q);
    }

    #[test]
    fn nearest_point_on_segment_past_endpoint_clamps() {
        let q = nearest_point_on_segment(pt(13.0, 4.0), pt(0.0, 0.0), pt(10.0, 0.0));
        assert!((q.x.as_f32() - 10.0).abs() < 1e-4, "got {:?}", q);
        assert!((q.y.as_f32() - 0.0).abs() < 1e-4, "got {:?}", q);
    }

    #[test]
    fn nearest_point_on_segment_zero_length_returns_endpoint() {
        let q = nearest_point_on_segment(pt(3.0, 4.0), pt(1.0, 1.0), pt(1.0, 1.0));
        assert!((q.x.as_f32() - 1.0).abs() < 1e-4, "got {:?}", q);
        assert!((q.y.as_f32() - 1.0).abs() < 1e-4, "got {:?}", q);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib selection::tests::nearest_point_on_segment`
Expected: FAIL to compile — `cannot find function 'nearest_point_on_segment' in this scope`

- [ ] **Step 3: Implement `nearest_point_on_segment`**

Add to `src/selection.rs` right after `point_to_segment_distance` (after line 66), and add `point` and `px` to the existing `use gpui::{Pixels, Point};` import at line 3 (becomes `use gpui::{point, px, Pixels, Point};`):

```rust
/// The point on segment `a`-`b` nearest to `p`, using the same clamped
/// projection as `point_to_segment_distance`. Handles zero-length segments
/// by returning `a` itself.
pub fn nearest_point_on_segment(p: Point<Pixels>, a: Point<Pixels>, b: Point<Pixels>) -> Point<Pixels> {
    let qx = p.x.as_f32();
    let qy = p.y.as_f32();
    let ax = a.x.as_f32();
    let ay = a.y.as_f32();
    let bx = b.x.as_f32();
    let by = b.y.as_f32();

    let dx = bx - ax;
    let dy = by - ay;
    let len_sq = dx * dx + dy * dy;
    if len_sq <= f32::EPSILON {
        return a;
    }
    let t = (((qx - ax) * dx + (qy - ay) * dy) / len_sq).clamp(0.0, 1.0);
    point(px(ax + t * dx), px(ay + t * dy))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib selection::tests::nearest_point_on_segment`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add src/selection.rs
git commit -m "Add nearest_point_on_segment geometry helper"
```

---

### Task 2: `OsmLayer::snap_to_way`

**Files:**
- Modify: `src/layers/osm_layer.rs` (add method after `hit_test_segment`, ~line 1743; add tests in the existing `#[cfg(test)] mod tests` block, ~after line 2580)

**Interfaces:**
- Consumes: `nearest_point_on_segment` (Task 1), existing `hit_test_segment`'s rtree-query pattern, `Viewport::screen_to_geo(&self, Point<Pixels>) -> (f64, f64)` (`src/viewport.rs:148`).
- Produces: `pub fn snap_to_way(&self, viewport: &Viewport, screen_pt: Point<Pixels>, tol_px: f32) -> Option<(i64, i64, i64, usize, f64, f64)>` — `(way_id, node_id_a, node_id_b, segment_index, lat, lon)`. Used by Task 4.

- [ ] **Step 1: Write the failing tests**

Add to `src/layers/osm_layer.rs`'s existing `mod tests` block, right after `hit_test_segment_none_when_out_of_tolerance` (after line 2580):

```rust
    #[test]
    fn snap_to_way_on_line_returns_exact_click_point() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode {
            id: 1,
            lat: center_lat,
            lon: center_lon - 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat,
            lon: center_lon + 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let (way_id, a, b, idx, lat, lon) = layer
            .snap_to_way(&viewport, point(px(400.0), px(300.0)), 6.0)
            .expect("expected a snap hit");
        assert_eq!((way_id, a, b, idx), (10, 1, 2, 0));
        assert!((lat - center_lat).abs() < 1e-6, "got lat {}", lat);
        assert!((lon - center_lon).abs() < 1e-6, "got lon {}", lon);
    }

    #[test]
    fn snap_to_way_off_line_projects_onto_segment() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode {
            id: 1,
            lat: center_lat,
            lon: center_lon - 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat,
            lon: center_lon + 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        // 3px above the (horizontal) line, within the 6px tolerance.
        let (way_id, a, b, idx, lat, lon) = layer
            .snap_to_way(&viewport, point(px(403.0), px(297.0)), 6.0)
            .expect("expected a snap hit");
        assert_eq!((way_id, a, b, idx), (10, 1, 2, 0));
        // The line is flat (both endpoints share center_lat), so the
        // projected point's lat matches the line exactly regardless of the
        // click's vertical offset.
        assert!((lat - center_lat).abs() < 1e-6, "got lat {}", lat);
        assert!(
            lon > center_lon - 0.001 && lon < center_lon + 0.001,
            "got lon {}",
            lon
        );
    }

    #[test]
    fn snap_to_way_none_when_out_of_tolerance() {
        let n1 = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.001,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(40.0, -74.0);
        let layer = OsmLayer::new_with_data(LayerId(1), "L", data);

        let hit = layer.snap_to_way(&viewport, point(px(0.0), px(0.0)), 6.0);
        assert!(hit.is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib layers::osm_layer::tests::snap_to_way`
Expected: FAIL to compile — `no method named 'snap_to_way' found`

- [ ] **Step 3: Implement `snap_to_way`**

Add to `src/layers/osm_layer.rs` right after `hit_test_segment` (after the closing `}` at line 1742, before `impl MapLayer for OsmLayer` at line 1745). Also add `nearest_point_on_segment` to the existing `use crate::selection::{...}` import at line 10-11 (becomes `point_to_segment_distance, nearest_point_on_segment, DeletedFeatureSnapshot, ...`):

```rust
    /// Like `hit_test_segment`, but also returns the nearest point on the
    /// winning segment as `(lat, lon)` — the coordinates a snapped node
    /// should adopt. Shares the same rtree query and per-segment loop; see
    /// `hit_test_segment`'s doc comment for the shared parameters.
    pub fn snap_to_way(
        &self,
        viewport: &Viewport,
        screen_pt: Point<Pixels>,
        tol_px: f32,
    ) -> Option<(i64, i64, i64, usize, f64, f64)> {
        self.osm_data.as_ref()?;
        let pad = px(tol_px * 4.0);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope =
            AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);

        let mut best: Option<(f32, i64, i64, i64, usize, Point<Pixels>)> = None;
        for item in self.way_index.locate_in_envelope_intersecting(envelope) {
            let way_id = item.data;
            let Some(&way_idx) = self.way_id_to_index.get(&way_id) else {
                continue;
            };
            let verts = &self.way_vertices[way_idx];
            for i in 0..verts.len().saturating_sub(1) {
                let (id_a, ax, ay) = verts[i];
                let (id_b, bx, by) = verts[i + 1];
                let sp_a = viewport.mercator_to_screen(ax, ay);
                let sp_b = viewport.mercator_to_screen(bx, by);
                if !is_point_valid(sp_a) || !is_point_valid(sp_b) {
                    continue;
                }
                let d = point_to_segment_distance(screen_pt, sp_a, sp_b);
                if d <= tol_px && best.as_ref().is_none_or(|&(bd, ..)| d < bd) {
                    let nearest = nearest_point_on_segment(screen_pt, sp_a, sp_b);
                    best = Some((d, way_id, id_a, id_b, i, nearest));
                }
            }
        }
        best.map(|(_, way_id, a, b, idx, nearest)| {
            let (lat, lon) = viewport.screen_to_geo(nearest);
            (way_id, a, b, idx, lat, lon)
        })
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib layers::osm_layer::tests::snap_to_way`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add src/layers/osm_layer.rs
git commit -m "Add OsmLayer::snap_to_way"
```

---

### Task 3: `SnapExtendWay` undo variant

**Files:**
- Modify: `src/undo.rs` (add enum variant after `InsertNodeIntoWay`, ~line 88; add `description()` arm, ~line 118)
- Modify: `src/main.rs` (add `apply_undo_action` match arm after the `InsertNodeIntoWay` arm, ~line 969)

**Interfaces:**
- Consumes: `EditableLayer::{way_node_ids, remove_way, remove_node_from_way, remove_node}` (all pre-existing, `src/layers/mod.rs`).
- Produces: `UndoableAction::SnapExtendWay { layer: LayerId, way_id: i64, way_created: bool, snap_way_id: i64, snap_index: usize, node_id: i64 }`. Used by Task 4.

This task has no isolated automated test: `apply_undo_action` and every `UndoableAction` variant require a live `MapViewer` (which itself requires a `gpui::Context` to construct), so — matching this codebase's existing pattern where `main.rs` orchestration is exercised at a higher level rather than unit-tested in isolation — this variant is exercised end-to-end by Task 5's App-level tests. Verify with a build only.

- [ ] **Step 1: Add the enum variant and description**

In `src/undo.rs`, add after the `InsertNodeIntoWay` variant (after line 88, before the closing `}` of the enum at line 89):

```rust
    /// Add mode's 2nd+ click when it snaps onto an existing way: a node
    /// spliced into `snap_way_id` at `snap_index` (like `InsertNodeIntoWay`),
    /// then that same node folded into the way being drawn — `way_id`,
    /// created fresh if `way_created`, otherwise extended (like `ExtendWay`
    /// with `node_created: true`). Undo reverses both, in the order that
    /// matches how they were applied: detach from the drawn way first
    /// (deleting it if `way_created`, without deleting the node — it is
    /// still referenced by `snap_way_id`), then remove the node from
    /// `snap_way_id` and delete it.
    SnapExtendWay {
        layer: LayerId,
        way_id: i64,
        way_created: bool,
        snap_way_id: i64,
        snap_index: usize,
        node_id: i64,
    },
```

In the same file, add a `description()` match arm right after the `InsertNodeIntoWay` arm (line 118):

```rust
            UndoableAction::SnapExtendWay { .. } => "Snapped a node onto a way".to_string(),
```

- [ ] **Step 2: Add the `apply_undo_action` handling**

In `src/main.rs`, add a new match arm in `apply_undo_action` (`src/main.rs:785`) right after the `InsertNodeIntoWay` arm closes (after line 969, before the `match` block's closing `}`):

```rust
            UndoableAction::SnapExtendWay {
                layer,
                way_id,
                way_created,
                snap_way_id,
                snap_index,
                node_id,
            } => {
                let Some(layer) = self.layer_manager.find_layer_mut(*layer) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                if !forward {
                    // Detach from the drawn way first (deleting it if this
                    // click created it), without deleting the node — it's
                    // still referenced by `snap_way_id` until the next step.
                    if *way_created {
                        editable.remove_way(*way_id);
                    } else {
                        let node_ids = editable.way_node_ids(*way_id).unwrap_or_default();
                        if let Some(idx) = node_ids.iter().rposition(|id| id == node_id) {
                            editable.remove_node_from_way(*way_id, idx);
                        }
                    }
                    // Then remove it from the way it was snapped onto, and
                    // delete the node itself.
                    editable.remove_node_from_way(*snap_way_id, *snap_index);
                    editable.remove_node(*node_id);
                }
                // Redo (forward) is intentionally a no-op, same reasoning as
                // `ExtendWay`/`InsertNodeIntoWay` above.
            }
```

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build`
Expected: builds cleanly (this variant is not yet constructed anywhere, so no behavior changes yet)

- [ ] **Step 4: Commit**

```bash
git add src/undo.rs src/main.rs
git commit -m "Add SnapExtendWay compound undo action"
```

---

### Task 4: Wire snapping and Ctrl-bypass into Add mode

**Files:**
- Modify: `src/main.rs`:
  - `handle_map_click` (`src/main.rs:1469`)
  - both call sites in `handle_mouse_up` (`src/main.rs:1431`, `:1455`)
  - `handle_add_click` (`src/main.rs:1655`)
  - `add_extend_or_start_way` (`src/main.rs:1759`)

**Interfaces:**
- Consumes: `OsmLayer::snap_to_way` (Task 2, reached via `layer.as_any().downcast_ref::<OsmLayer>()`, mirroring the existing pattern at `src/main.rs:683-696`), `UndoableAction::SnapExtendWay` (Task 3), `EditableLayer::insert_node_into_way` (pre-existing).
- Produces: `handle_add_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>, ctrl_held: bool)` and `add_extend_or_start_way(&mut self, layer_id: LayerId, node_id: i64, node_created: bool, snap: Option<(i64, usize)>) -> i64` — new signatures. Used directly by Task 5's tests.

- [ ] **Step 1: Thread `ctrl_held` through `handle_map_click`**

In `src/main.rs`, change the `handle_map_click` signature (line 1469) and its `Add` arm:

```rust
    fn handle_map_click(
        &mut self,
        screen_pt: gpui::Point<gpui::Pixels>,
        shift_held: bool,
        ctrl_held: bool,
    ) {
        match self.mode {
            EditMode::Select => self.handle_select_click(screen_pt, shift_held),
            EditMode::Add => self.handle_add_click(screen_pt, ctrl_held),
            EditMode::Building => self.handle_building_click(screen_pt),
            EditMode::Extrude => {
                // Extrude doesn't use the plain-click path (Task 8 hooks
                // mouse-down/mouse-move/mouse-up directly); a stray click
                // here (e.g. a zero-movement mouse-up while extruding) is a
                // no-op.
            }
        }
    }
```

Update both call sites in `handle_mouse_up`:

At line 1431 (inside `interaction::Gesture::MoveCancelledAsClick`):
```rust
                self.handle_map_click(from_pt(at), event.modifiers.shift, event.modifiers.control);
```

At line 1455 (inside `interaction::Gesture::Click`):
```rust
                self.handle_map_click(from_pt(at), event.modifiers.shift, event.modifiers.control);
```

- [ ] **Step 2: Update `handle_add_click` to accept `ctrl_held` and try snapping**

Replace the full body of `handle_add_click` (`src/main.rs:1655-1750`) with:

```rust
    /// Add mode: place a node, or extend/connect the in-progress way. See
    /// docs/superpowers/specs/2026-07-07-mode-selector-design.md "Add mode"
    /// and docs/superpowers/specs/2026-07-08-add-mode-snap-to-way-design.md
    /// for the snap-to-way behavior. `ctrl_held` bypasses both snapping onto
    /// a nearby existing node and snapping onto a nearby way's line
    /// geometry, always producing a fully independent node.
    fn handle_add_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>, ctrl_held: bool) {
        let Some(layer_id) = self.active_layer else {
            return;
        };
        let (lat, lon) = self.viewport.screen_to_geo(screen_pt);

        // Clicking an existing node/way finishes the in-progress way by
        // connecting to it. Skipped entirely when Ctrl is held.
        if !ctrl_held && self.add_progress.is_some() {
            let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
            if let Some(hit) = osm_gpui::selection::resolve_hits(per_layer) {
                if hit.layer_id == layer_id {
                    if let osm_gpui::selection::FeatureKind::Node = hit.kind {
                        let way_id = self.add_extend_or_start_way(layer_id, hit.id, false, None);
                        self.add_progress = None;
                        self.selected = vec![osm_gpui::selection::FeatureRef {
                            layer_id,
                            kind: osm_gpui::selection::FeatureKind::Way,
                            id: way_id,
                        }];
                        self.fields_text_inputs.clear();
                        self.fields_text_subscribed.clear();
                        self.fields_open_combo = None;
                        self.fields_promoted_more_fields.clear();
                        return;
                    }
                }
            }
        }

        // Try to snap onto a nearby way's line geometry, unless Ctrl is
        // held. `snap` carries the snapped-onto way's id and splice index,
        // if any, threaded through to `add_extend_or_start_way` for the
        // 2nd+ click case.
        let snap_hit = if ctrl_held {
            None
        } else {
            self.layer_manager
                .find_layer(layer_id)
                .and_then(|layer| layer.as_any().downcast_ref::<OsmLayer>())
                .and_then(|osm_layer| osm_layer.snap_to_way(&self.viewport, screen_pt, 6.0))
        };

        // Note: `find_layer_mut` is re-called in each arm below (rather than
        // binding `layer` once above the match) so its mutable borrow ends
        // before the arm needs `&mut self` again for `self.add_progress`/
        // `self.add_extend_or_start_way`/`self.undo_stack` — binding it once
        // outside the match would keep the borrow alive across those calls
        // and fail to compile.
        match self.add_progress.take() {
            None => {
                // First click of a fresh continuation: a lone node (or, if
                // snapped, a node spliced into the snapped-onto way), no way
                // of its own yet — the next click always starts a *new* way
                // from this node, whether or not this one landed on top of
                // an existing way.
                let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                let new_id = match snap_hit {
                    Some((way_id, _, _, idx, snap_lat, snap_lon)) => {
                        let new_id = editable.insert_node_into_way(way_id, idx + 1, snap_lat, snap_lon);
                        self.undo_stack.push(UndoableAction::InsertNodeIntoWay {
                            layer: layer_id,
                            way_id,
                            index: idx + 1,
                            node_id: new_id,
                        });
                        new_id
                    }
                    None => {
                        // Reuses the pre-existing `CreateNode` undo action
                        // (same one the retired Cmd+Click gesture used to
                        // use) — this is the same underlying mutation, just
                        // triggered by Add mode instead.
                        let new_id = editable.add_node(lat, lon);
                        self.undo_stack.push(UndoableAction::CreateNode {
                            layer: layer_id,
                            id: new_id,
                            lat,
                            lon,
                        });
                        new_id
                    }
                };
                self.add_progress = Some(AddProgress {
                    way_id: None,
                    last_node_id: new_id,
                });
                self.selected = vec![osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Node,
                    id: new_id,
                }];
            }
            Some(progress) => {
                // 2nd+ click: create the node (or snap it onto a way) and
                // fold it into the way being drawn in one step.
                // `add_extend_or_start_way` pushes the matching undo entry
                // that covers both the node creation and the way
                // mutation(s) (one click = one undo step).
                let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
                    return;
                };
                let Some(editable) = layer.as_editable_mut() else {
                    return;
                };
                let (new_id, snap) = match snap_hit {
                    Some((way_id, _, _, idx, snap_lat, snap_lon)) => (
                        editable.insert_node_into_way(way_id, idx + 1, snap_lat, snap_lon),
                        Some((way_id, idx + 1)),
                    ),
                    None => (editable.add_node(lat, lon), None),
                };
                self.add_progress = Some(progress);
                let way_id = self.add_extend_or_start_way(layer_id, new_id, true, snap);
                self.add_progress = Some(AddProgress {
                    way_id: Some(way_id),
                    last_node_id: new_id,
                });
                self.selected = vec![osm_gpui::selection::FeatureRef {
                    layer_id,
                    kind: osm_gpui::selection::FeatureKind::Way,
                    id: way_id,
                }];
            }
        }
        self.fields_text_inputs.clear();
        self.fields_text_subscribed.clear();
        self.fields_open_combo = None;
        self.fields_promoted_more_fields.clear();
    }
```

- [ ] **Step 3: Update `add_extend_or_start_way` to accept and act on `snap`**

Replace the full body of `add_extend_or_start_way` (`src/main.rs:1759-1802`) with:

```rust
    /// Shared by the "continue clicking" and "connect to existing feature"
    /// paths: start a new 2-node way if none exists yet, or extend the
    /// existing one, pushing the matching undo entry. Returns the way id
    /// (new or existing). `node_created` must reflect whether `node_id` was
    /// just created by this click (vs. an existing node the user clicked to
    /// connect) — it's recorded on the undo entry so undo never deletes a
    /// node it didn't create. `snap`, when `Some((snap_way_id,
    /// snap_index))`, means `node_id` was just spliced into `snap_way_id` at
    /// `snap_index` by the caller (via `snap_to_way`/`insert_node_into_way`)
    /// — this click is a compound mutation, so it pushes `SnapExtendWay`
    /// instead of `ExtendWay` to undo both steps together.
    fn add_extend_or_start_way(
        &mut self,
        layer_id: LayerId,
        node_id: i64,
        node_created: bool,
        snap: Option<(i64, usize)>,
    ) -> i64 {
        let progress_way_id = self.add_progress.as_ref().and_then(|p| p.way_id);
        let last_node_id = self
            .add_progress
            .as_ref()
            .map(|p| p.last_node_id)
            .unwrap_or(node_id);
        let Some(layer) = self.layer_manager.find_layer_mut(layer_id) else {
            return progress_way_id.unwrap_or(0);
        };
        let Some(editable) = layer.as_editable_mut() else {
            return progress_way_id.unwrap_or(0);
        };

        let (way_id, way_created) = match progress_way_id {
            Some(way_id) => {
                editable.extend_way(way_id, node_id);
                (way_id, false)
            }
            None => {
                let way_id = editable.add_way(vec![last_node_id, node_id], Vec::new());
                (way_id, true)
            }
        };

        match snap {
            Some((snap_way_id, snap_index)) => {
                self.undo_stack.push(UndoableAction::SnapExtendWay {
                    layer: layer_id,
                    way_id,
                    way_created,
                    snap_way_id,
                    snap_index,
                    node_id,
                });
            }
            None => {
                self.undo_stack.push(UndoableAction::ExtendWay {
                    layer: layer_id,
                    way_id,
                    node_id,
                    way_created,
                    node_created,
                });
            }
        }
        way_id
    }
```

- [ ] **Step 4: Build to verify it compiles**

Run: `cargo build`
Expected: builds cleanly. (`snap_to_way` is now reachable through `handle_add_click`; `add_extend_or_start_way`'s new `snap` parameter is `None` at its only other call site, the "connect to existing node" path in `handle_add_click`, which is already updated in Step 2 above.)

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "Wire snap-to-way and Ctrl-bypass into Add mode"
```

---

### Task 5: App-level tests

**Files:**
- Modify: `src/main.rs` (add `#[cfg(test)] mod tests` block at the end of the file — first of its kind in this file)

**Interfaces:**
- Consumes: `MapViewer::new` (`src/main.rs:442`), `MapViewer::handle_add_click` (Task 4), `MapViewer::apply_undo_action` (`src/main.rs:785`), `OsmLayer::new_with_data`, `LayerManager::alloc_id`/`add_layer`, `EditableLayer::{way_node_ids, node_lat_lon}`.

This is the primary automated coverage for the feature: it exercises the real `MapViewer` struct end-to-end (real `layer_manager`, real `undo_stack`, real `OsmLayer` mutations), calling `handle_add_click` directly rather than simulating the OS-level mouse-down/up/gesture-resolution pipeline (which is unrelated, pre-existing machinery — `handle_map_click`'s only new responsibility, `ctrl_held`/`shift_held` extraction from `event.modifiers`, is a one-line pass-through with nothing to meaningfully test in isolation).

- [ ] **Step 1: Write the failing tests**

Add at the end of `src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size, TestAppContext};
    use osm_gpui::osm::{OsmNode, OsmWay};
    use std::collections::HashMap;

    fn empty_tags() -> HashMap<String, String> {
        HashMap::new()
    }

    /// A single 2-node way `10 = [1, 2]`, both nodes at `center_lat`, lon
    /// `center_lon - 0.001` / `+ 0.001` — a flat horizontal line through the
    /// viewport's center at zoom 18 (matches the convention used by
    /// `OsmLayer`'s own tests in `src/layers/osm_layer.rs`).
    fn way_fixture(center_lat: f64, center_lon: f64) -> OsmData {
        let n1 = OsmNode {
            id: 1,
            lat: center_lat,
            lon: center_lon - 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let n2 = OsmNode {
            id: 2,
            lat: center_lat,
            lon: center_lon + 0.001,
            version: 1,
            tags: empty_tags(),
        };
        let way = OsmWay {
            id: 10,
            nodes: vec![1, 2],
            version: 1,
            tags: empty_tags(),
        };
        let mut nodes = HashMap::new();
        nodes.insert(1, n1);
        nodes.insert(2, n2);
        let mut ways = HashMap::new();
        ways.insert(10, way);
        OsmData {
            nodes,
            ways,
            relations: Vec::new(),
            bounds: None,
        }
    }

    const CENTER_LAT: f64 = 40.0;
    const CENTER_LON: f64 = -74.0;

    /// Builds a `MapViewer` in a headless test window, viewport centered on
    /// `way_fixture`'s line (so screen `(400, 300)` lands exactly on it, per
    /// the same convention as `OsmLayer`'s own tests), with one active OSM
    /// layer containing that fixture, in Add mode.
    fn setup(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
        let window = cx.add_window(|window, cx| MapViewer::new(window, cx));
        window
            .update(cx, |view, _window, _cx| {
                view.viewport =
                    Viewport::new(CENTER_LAT, CENTER_LON, 18.0, size(px(800.0), px(600.0)));
                let layer_id = view.layer_manager.alloc_id();
                let layer = OsmLayer::new_with_data(
                    layer_id,
                    "L".to_string(),
                    Arc::new(way_fixture(CENTER_LAT, CENTER_LON)),
                );
                view.layer_manager.add_layer(Box::new(layer));
                view.active_layer = Some(layer_id);
                view.mode = EditMode::Add;
            })
            .unwrap();
        window
    }

    fn way_nodes(view: &MapViewer, layer_id: LayerId, way_id: i64) -> Vec<i64> {
        view.layer_manager
            .find_layer(layer_id)
            .and_then(|l| l.as_editable())
            .and_then(|e| e.way_node_ids(way_id))
            .unwrap_or_default()
    }

    #[gpui::test]
    fn first_click_in_add_mode_snaps_onto_existing_way(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);

                // On the line, exactly at the viewport's projected center.
                view.handle_add_click(point(px(400.0), px(300.0)), false);

                let nodes = way_nodes(view, layer_id, 10);
                assert_eq!(nodes.len(), 3, "expected a node spliced in: {:?}", nodes);
                assert_eq!((nodes[0], nodes[2]), (1, 2));
                let new_id = nodes[1];

                // Add mode's own progress continues from the snapped node,
                // not from an implicit continuation of way 10.
                assert!(view.add_progress.is_some());
                assert_eq!(view.add_progress.as_ref().unwrap().way_id, None);
                assert_eq!(view.add_progress.as_ref().unwrap().last_node_id, new_id);

                // Undo removes the splice and deletes the node.
                let action = view.undo_stack.undo().expect("expected an undo entry");
                view.apply_undo_action(&action, false);
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
                let layer = view.layer_manager.find_layer(layer_id).unwrap();
                assert_eq!(layer.as_editable().unwrap().node_lat_lon(new_id), None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn second_click_snaps_and_folds_with_compound_undo(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();

                // First click: a free node A, far from way 10.
                view.handle_add_click(point(px(50.0), px(50.0)), false);
                let node_a = view.add_progress.as_ref().unwrap().last_node_id;
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);

                // Second click: on way 10's line — snaps a new node B into
                // way 10, and folds B into a brand-new way (A -> B).
                view.handle_add_click(point(px(400.0), px(300.0)), false);

                let nodes10 = way_nodes(view, layer_id, 10);
                assert_eq!(nodes10.len(), 3, "expected a node spliced in: {:?}", nodes10);
                assert_eq!((nodes10[0], nodes10[2]), (1, 2));
                let node_b = nodes10[1];

                let drawn_way_id = view.add_progress.as_ref().unwrap().way_id.unwrap();
                assert_eq!(way_nodes(view, layer_id, drawn_way_id), vec![node_a, node_b]);

                // Undo reverses both mutations from that single click: the
                // drawn way goes away entirely (it was created by this
                // click), way 10 reverts, and node B is deleted — but node A
                // (created by the *first* click, a separate undo entry) is
                // untouched.
                let action = view.undo_stack.undo().expect("expected an undo entry");
                view.apply_undo_action(&action, false);
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2]);
                let layer = view.layer_manager.find_layer(layer_id).unwrap();
                let editable = layer.as_editable().unwrap();
                assert_eq!(editable.node_lat_lon(node_b), None);
                assert!(editable.node_lat_lon(node_a).is_some());
                assert_eq!(editable.way_node_ids(drawn_way_id), None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn ctrl_held_bypasses_both_node_and_way_snap(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();
                let n1_screen = view.viewport.geo_to_screen(CENTER_LAT, CENTER_LON - 0.001);

                // First click: a free node A, far from way 10 (Ctrl
                // irrelevant here — nothing nearby to snap to).
                view.handle_add_click(point(px(50.0), px(50.0)), false);
                let node_a = view.add_progress.as_ref().unwrap().last_node_id;

                // Second click, Ctrl held, exactly on top of existing node 1:
                // without Ctrl this would connect to node 1 (see the "click
                // hits an existing node" path); with Ctrl it must create a
                // brand-new independent node instead.
                view.handle_add_click(n1_screen, true);
                let node_b = view.add_progress.as_ref().unwrap().last_node_id;
                assert_ne!(node_b, 1, "Ctrl should not have connected to node 1");
                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2], "way 10 untouched");

                // Third click, Ctrl held, on way 10's line: without Ctrl this
                // would snap into way 10 (as in the sibling tests above);
                // with Ctrl it must stay a free node.
                view.handle_add_click(point(px(400.0), px(300.0)), true);
                let node_c = view.add_progress.as_ref().unwrap().last_node_id;
                assert_eq!(
                    way_nodes(view, layer_id, 10),
                    vec![1, 2],
                    "Ctrl should not have snapped onto way 10"
                );

                let drawn_way_id = view.add_progress.as_ref().unwrap().way_id.unwrap();
                assert_eq!(
                    way_nodes(view, layer_id, drawn_way_id),
                    vec![node_a, node_b, node_c]
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn click_with_no_way_nearby_falls_back_to_free_node(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, _cx| {
                let layer_id = view.active_layer.unwrap();

                // Far from way 10's line — outside the 6px snap tolerance.
                view.handle_add_click(point(px(50.0), px(50.0)), false);

                assert_eq!(way_nodes(view, layer_id, 10), vec![1, 2], "way 10 untouched");
                let new_id = view.add_progress.as_ref().unwrap().last_node_id;
                let layer = view.layer_manager.find_layer(layer_id).unwrap();
                assert!(layer.as_editable().unwrap().node_lat_lon(new_id).is_some());
            })
            .unwrap();
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin osm-gpui tests::`
Expected: FAIL to compile — `handle_add_click`/`add_extend_or_start_way` signature mismatches if Task 4 weren't already done, or (since Task 4 is already implemented at this point) the tests should compile; run them to confirm they currently PASS, not fail — Tasks 3-4 already provide the implementation. If any test fails, that's a real bug in Task 3/4's wiring — fix it there (not by weakening the test) before proceeding.

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --bin osm-gpui tests::`
Expected: PASS (4 tests)

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: PASS (no regressions in any existing test)

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "Add App-level tests for Add-mode snap-to-way"
```

---

### Task 6: `.osmscript` Ctrl-modifier support for `click` (secondary)

**Files:**
- Modify: `src/script/op.rs` (`Op::Click`, ~line 48-51)
- Modify: `src/script/parser.rs` (`parse_click`, ~line 172-189; add a test)
- Modify: `src/script/runner.rs` (`AppHandle::dispatch_click`, ~line 17; `Op::Click` dispatch, ~line 74-77; `describe`, ~line 156; the test `Fake`'s `dispatch_click`, ~line 180)
- Modify: `src/script_harness.rs` (`ScriptCommand::Click`, ~line 46; `LiveApp::dispatch_click`, ~line 294-301; the `Click` arm in `process_script_command`, ~line 196-218)

**Interfaces:**
- Consumes: nothing from earlier tasks — independent of Tasks 1-5.
- Produces: `.osmscript` syntax `click X,Y ctrl=true` (in addition to the existing `button=left|right`).

- [ ] **Step 1: Write the failing parser test**

Add to `src/script/parser.rs`. First check whether a `#[cfg(test)] mod tests` block already exists in this file:

Run: `grep -n "mod tests" src/script/parser.rs`

If it exists, add the test inside it; if not, add this block at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_parses_ctrl_modifier() {
        let steps = parse("click 10,20 ctrl=true\n").unwrap();
        assert_eq!(steps.len(), 1);
        match &steps[0].op {
            Op::Click { at, ctrl, .. } => {
                assert_eq!(*at, Point2 { x: 10.0, y: 20.0 });
                assert!(*ctrl);
            }
            other => panic!("expected Op::Click, got {:?}", other),
        }
    }

    #[test]
    fn click_defaults_ctrl_to_false() {
        let steps = parse("click 10,20\n").unwrap();
        match &steps[0].op {
            Op::Click { ctrl, .. } => assert!(!*ctrl),
            other => panic!("expected Op::Click, got {:?}", other),
        }
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib script::parser::tests::click_parses_ctrl_modifier`
Expected: FAIL to compile — `Op::Click` has no field `ctrl`

- [ ] **Step 3: Add the `ctrl` field and parsing**

In `src/script/op.rs`, change `Op::Click` (line 48-51):

```rust
    Click {
        at: Point2,
        button: MouseButton,
        ctrl: bool,
    },
```

In `src/script/parser.rs`, replace `parse_click` (line 172-189):

```rust
fn parse_click(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.is_empty() {
        return Err(err(line_no, "click: want X,Y [button=left|right] [ctrl=true]"));
    }
    let at = parse_point(line_no, rest[0])?;
    let mut button = MouseButton::Left;
    let mut ctrl = false;
    for kv in &rest[1..] {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| err(line_no, format!("click: bad kv '{}'", kv)))?;
        match (k, v) {
            ("button", "left") => button = MouseButton::Left,
            ("button", "right") => button = MouseButton::Right,
            ("ctrl", "true") => ctrl = true,
            ("ctrl", "false") => ctrl = false,
            _ => return Err(err(line_no, format!("click: unknown {}={}", k, v))),
        }
    }
    Ok(Op::Click { at, button, ctrl })
}
```

- [ ] **Step 4: Fix every other `Op::Click` construction/match site to compile**

In `src/script/runner.rs`:
- Change `AppHandle::dispatch_click` (line 17): `fn dispatch_click(&mut self, at: (f32, f32), button: crate::script::MouseButton, ctrl: bool);`
- Change the `Op::Click` dispatch (line 74-77):
  ```rust
              Op::Click { at, button, ctrl } => {
                  app.dispatch_click((at.x, at.y), *button, *ctrl);
                  Ok(())
              }
  ```
- Change `describe`'s `Op::Click` arm (line 156):
  ```rust
          Op::Click { at, button, ctrl } => format!("click {:?} {:?} ctrl={}", at, button, ctrl),
  ```
- Change the test `Fake`'s `dispatch_click` (line 180): `fn dispatch_click(&mut self, _: (f32, f32), _: MouseButton, _: bool) {}`

In `src/script_harness.rs`:
- Change `ScriptCommand::Click` (line 46): add a `ctrl: bool` field, e.g. `Click { x: f32, y: f32, right: bool, ctrl: bool },`
- Change the `ScriptCommand::Click` arm in `process_script_command` (line 196-218) to build the modifiers from `ctrl` instead of hardcoding `gpui::Modifiers::none()`:
  ```rust
                  ScriptCommand::Click { x, y, right, ctrl } => {
                      let btn = if right {
                          gpui::MouseButton::Right
                      } else {
                          gpui::MouseButton::Left
                      };
                      let modifiers = gpui::Modifiers {
                          control: ctrl,
                          ..gpui::Modifiers::none()
                      };
                      let ev = MouseDownEvent {
                          button: btn,
                          position: point(px(x), px(y)),
                          modifiers,
                          click_count: 1,
                          first_mouse: false,
                      };
                      self.handle_mouse_down(&ev);
                      let ev = MouseUpEvent {
                          button: btn,
                          position: point(px(x), px(y)),
                          modifiers,
                          click_count: 1,
                      };
                      self.handle_mouse_up(&ev, cx);
                      cx.notify();
                  }
  ```
- Change `LiveApp::dispatch_click` (line 294-301):
  ```rust
      fn dispatch_click(&mut self, at: (f32, f32), button: script::MouseButton, ctrl: bool) {
          let right = matches!(button, script::MouseButton::Right);
          self.bus.submit(ScriptCommand::Click {
              x: at.0,
              y: at.1,
              right,
              ctrl,
          });
      }
  ```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib script::`
Expected: PASS

- [ ] **Step 6: Run the full test suite and build the binary**

Run: `cargo test && cargo build`
Expected: both succeed (this confirms every `Op::Click`/`dispatch_click` call site was updated)

- [ ] **Step 7: Commit**

```bash
git add src/script/op.rs src/script/parser.rs src/script/runner.rs src/script_harness.rs
git commit -m "Add ctrl modifier support to .osmscript click op"
```

---

## Self-Review Notes

- **Spec coverage:** every "Scope" item in the design spec maps to a task — reused building blocks (Tasks 2/4), new pure helpers (Tasks 1/2), `handle_add_click`/Ctrl threading (Task 4), `SnapExtendWay` (Task 3), out-of-scope items are deliberately not implemented anywhere, pure-geometry tests (Tasks 1/2), App-level tests (Task 5), `.osmscript` fix (Task 6).
- **Type consistency checked:** `handle_add_click(&mut self, screen_pt, ctrl_held: bool)` (Task 4) matches its two call sites in `handle_map_click` (Task 4) and its four call sites in Task 5's tests. `add_extend_or_start_way`'s new `snap: Option<(i64, usize)>` parameter is threaded consistently at both its call sites (the "connect to existing node" path passes `None`; the 2nd+ click path passes `snap`). `SnapExtendWay`'s field names match between its `undo.rs` definition (Task 3) and its `main.rs` construction (Task 4) and `apply_undo_action` handling (Task 3).
- **No placeholders**, all code blocks are complete and copy-pasteable.
