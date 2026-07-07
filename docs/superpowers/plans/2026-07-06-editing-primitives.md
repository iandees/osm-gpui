# Editing Primitives Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the next tier of JOSM-standard editing operations — delete, remove-from-way, insert-into-way, split way, join ways, square corners, draw new way — each undoable, each triggered from the Edit menu (plus a couple of dedicated keys), building on the per-element dirty-tracking fields added to `OsmLayer` by the OSM XML Export plan.

**Architecture:** Each op is an `OsmLayer` inherent method (`commit_*`) that mutates `self.osm_data` in place and updates the dirty-tracking fields, mirroring the existing `commit_node_moves`. `main.rs` captures pre-mutation state, calls the `commit_*` method, and pushes a new `UndoableAction` variant carrying enough before/after data to reverse itself via `apply_undo_action`. All ops are reached through the existing **Edit** menu (next to Undo/Redo) rather than new right-click context menus, avoiding an unverified GPUI interaction pattern on the canvas.

**Tech Stack:** Rust, existing `OsmLayer`/`UndoStack`/`selection.rs` infrastructure — no new dependencies.

Design reference: `docs/superpowers/specs/2026-07-06-editing-primitives-design.md`. Depends on: `docs/superpowers/plans/2026-07-06-osm-export.md` (must land first — this plan uses the `modified_node_ids`/`deleted_node_ids`/`new_node_ids`/`next_new_id`/`_way_ids` fields it adds to `OsmLayer`).

## Global Constraints

- Single-line git commit messages, no `Co-Authored-By` trailer.
- `cargo build`, `cargo clippy`, `cargo test` must stay clean/green after every task.
- Do not touch dead files: `src/map.rs`, `src/data.rs`, `src/background.rs`, `src/mercator.rs`, `src/http_image_loader.rs`.
- Every op refuses with `self.set_status("...")` rather than panicking or silently no-opping when preconditions aren't met (empty selection, wrong feature kind, etc.) — never `unwrap()` on a selection in a menu-action handler.
- No GUI automation available — menu/key wiring is verified by build + tests + a manual spot-check list per task, not a live click-through.
- Deleting a way never deletes its member nodes (JOSM convention). Removing a node from a way never deletes the node.
- Multi-select deletion (Task 1) is the only op here that acts on more than one feature at once; all other ops act on exactly the selection shape stated in their trigger.

---

### Task 1: Delete node / delete way

**Files:**
- Modify: `src/layers/osm_layer.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `OsmLayer::commit_delete_features(&mut self, node_ids: &[i64], way_ids: &[i64]) -> Result<(Vec<OsmNode>, Vec<OsmWay>), String>` (returns the removed records on success, for undo; `Err(msg)` if any requested node is still referenced by a way not also being deleted). `OsmLayer::restore_deleted_features(&mut self, nodes: Vec<OsmNode>, ways: Vec<OsmWay>)`. `UndoableAction::DeleteFeatures { layer_name: String, nodes: Vec<OsmNode>, ways: Vec<OsmWay> }`.
- Consumes: `MapViewer::selected: Vec<FeatureRef>` (existing), `MapViewer::set_status` (existing).

- [ ] **Step 1: Write the failing tests**

Add to `src/layers/osm_layer.rs`'s test module:

```rust
    #[test]
    fn commit_delete_features_removes_unreferenced_node() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let (nodes, ways) = layer.commit_delete_features(&[1], &[]).unwrap();
        assert_eq!(nodes.len(), 1);
        assert!(ways.is_empty());
        assert!(layer.get_osm_data().unwrap().nodes.get(&1).is_none());
        assert!(layer.edit_marks().deleted_nodes.contains(&1));
    }

    #[test]
    fn commit_delete_features_refuses_node_still_in_way() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 41.0, lon: -75.0, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let err = layer.commit_delete_features(&[1], &[]).unwrap_err();
        assert!(err.contains("1 way"));
        assert!(layer.get_osm_data().unwrap().nodes.get(&1).is_some());
    }

    #[test]
    fn commit_delete_features_deletes_way_without_touching_nodes() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 41.0, lon: -75.0, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let (nodes, ways) = layer.commit_delete_features(&[], &[10]).unwrap();
        assert!(nodes.is_empty());
        assert_eq!(ways.len(), 1);
        assert!(layer.get_osm_data().unwrap().ways.is_empty());
        assert!(layer.get_osm_data().unwrap().nodes.get(&1).is_some());
    }

    #[test]
    fn commit_delete_features_allows_node_and_its_only_way_together() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 41.0, lon: -75.0, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let (nodes, ways) = layer.commit_delete_features(&[1], &[10]).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(ways.len(), 1);
    }

    #[test]
    fn restore_deleted_features_puts_data_back() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let (nodes, ways) = layer.commit_delete_features(&[1], &[]).unwrap();
        layer.restore_deleted_features(nodes, ways);

        assert!(layer.get_osm_data().unwrap().nodes.get(&1).is_some());
        assert!(!layer.edit_marks().deleted_nodes.contains(&1));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib commit_delete_features`
Expected: FAIL — methods don't exist yet.

- [ ] **Step 3: Implement `commit_delete_features` and `restore_deleted_features`**

Add to `impl OsmLayer` in `src/layers/osm_layer.rs`, near `commit_node_moves`:

```rust
    /// Delete the given nodes and ways. Refuses (returning `Err`) if any
    /// node in `node_ids` is still referenced by a way that isn't also
    /// being deleted in the same call. On success, returns the removed
    /// `OsmNode`/`OsmWay` records (for undo) and rebuilds derived caches.
    pub fn commit_delete_features(
        &mut self,
        node_ids: &[i64],
        way_ids: &[i64],
    ) -> Result<(Vec<OsmNode>, Vec<OsmWay>), String> {
        let Some(current) = self.osm_data.clone() else {
            return Err("no data loaded".to_string());
        };
        let deleting_ways: HashSet<i64> = way_ids.iter().copied().collect();
        for &node_id in node_ids {
            let referencing_count = current
                .ways
                .iter()
                .filter(|w| !deleting_ways.contains(&w.id) && w.nodes.contains(&node_id))
                .count();
            if referencing_count > 0 {
                return Err(format!(
                    "node {} is part of {} way(s) — remove from way first",
                    node_id, referencing_count
                ));
            }
        }

        let mut data = (*current).clone();
        let mut removed_nodes = Vec::new();
        for &id in node_ids {
            if let Some(node) = data.nodes.remove(&id) {
                removed_nodes.push(node);
                self.deleted_node_ids.insert(id);
            }
        }
        let mut removed_ways = Vec::new();
        let deleting: HashSet<i64> = way_ids.iter().copied().collect();
        let mut kept_ways = Vec::with_capacity(data.ways.len());
        for way in data.ways.into_iter() {
            if deleting.contains(&way.id) {
                self.deleted_way_ids.insert(way.id);
                removed_ways.push(way);
            } else {
                kept_ways.push(way);
            }
        }
        data.ways = kept_ways;

        self.modified = true;
        self.set_osm_data(Arc::new(data));
        Ok((removed_nodes, removed_ways))
    }

    /// Undo counterpart of `commit_delete_features`: re-inserts the given
    /// nodes/ways and clears their deleted-marks.
    pub fn restore_deleted_features(&mut self, nodes: Vec<OsmNode>, ways: Vec<OsmWay>) {
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        for node in nodes {
            self.deleted_node_ids.remove(&node.id);
            data.nodes.insert(node.id, node);
        }
        for way in ways {
            self.deleted_way_ids.remove(&way.id);
            data.ways.push(way);
        }
        self.set_osm_data(Arc::new(data));
    }
```

Add `use std::collections::HashSet;` at the top of the file if not already imported (check first — `node_index`/`way_index` fields use `RTree`, and `HashSet` is likely already imported for `drag_preview`; grep the file's `use` block before adding a duplicate).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib commit_delete_features restore_deleted_features`
Expected: PASS (5 tests)

- [ ] **Step 5: Wire the `Delete` key and `UndoableAction::DeleteFeatures`**

In `src/main.rs`, add a variant to `UndoableAction`:

```rust
enum UndoableAction {
    MoveNodes { per_layer: NodeMoveUndoEntries },
    DeleteFeatures { layer_name: String, nodes: Vec<osm_gpui::osm::OsmNode>, ways: Vec<osm_gpui::osm::OsmWay> },
}
```

Update `description()`:

```rust
            UndoableAction::DeleteFeatures { nodes, ways, .. } => {
                format!("Deleted {} node(s), {} way(s)", nodes.len(), ways.len())
            }
```

Update `apply_undo_action`:

```rust
            UndoableAction::DeleteFeatures { layer_name, nodes, ways } => {
                if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                    if forward {
                        let node_ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
                        let way_ids: Vec<i64> = ways.iter().map(|w| w.id).collect();
                        let _ = layer.commit_delete_features(&node_ids, &way_ids);
                    } else {
                        layer.restore_deleted_features(nodes.clone(), ways.clone());
                    }
                }
            }
```

This requires `commit_delete_features`/`restore_deleted_features` on the `MapLayer` trait too (mirroring `commit_node_moves`), since `apply_undo_action` calls through `&mut Box<dyn MapLayer>`. Add to `src/layers/mod.rs`'s `MapLayer` trait:

```rust
    fn commit_delete_features(&mut self, _node_ids: &[i64], _way_ids: &[i64]) -> Result<(Vec<crate::osm::OsmNode>, Vec<crate::osm::OsmWay>), String> {
        Err("layer does not support delete".to_string())
    }
    fn restore_deleted_features(&mut self, _nodes: Vec<crate::osm::OsmNode>, _ways: Vec<crate::osm::OsmWay>) {}
```

And forward them in `impl MapLayer for OsmLayer` (`src/layers/osm_layer.rs`), next to the existing `fn commit_node_moves` forwarder:

```rust
    fn commit_delete_features(&mut self, node_ids: &[i64], way_ids: &[i64]) -> Result<(Vec<OsmNode>, Vec<OsmWay>), String> {
        OsmLayer::commit_delete_features(self, node_ids, way_ids)
    }
    fn restore_deleted_features(&mut self, nodes: Vec<OsmNode>, ways: Vec<OsmWay>) {
        OsmLayer::restore_deleted_features(self, nodes, ways)
    }
```

Add a `MapViewer` method (near `on_undo`):

```rust
    fn on_delete_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            return;
        }
        let layer_name = self.selected[0].layer_name.clone();
        let node_ids: Vec<i64> = self.selected.iter()
            .filter(|f| f.kind == osm_gpui::selection::FeatureKind::Node)
            .map(|f| f.id).collect();
        let way_ids: Vec<i64> = self.selected.iter()
            .filter(|f| f.kind == osm_gpui::selection::FeatureKind::Way)
            .map(|f| f.id).collect();
        let Some(layer) = self.layer_manager.find_layer_mut(&layer_name) else { return; };
        match layer.commit_delete_features(&node_ids, &way_ids) {
            Ok((nodes, ways)) => {
                self.selected.clear();
                self.undo_stack.push(UndoableAction::DeleteFeatures { layer_name, nodes, ways });
                cx.notify();
            }
            Err(msg) => self.set_status(msg),
        }
    }
```

Wire it to the `Delete`/`Backspace` key: find the existing `.on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| { if ev.keystroke.key == "escape" { this.cancel_move_drag(cx); } }))` block on the map area and add a branch:

```rust
                                if ev.keystroke.key == "backspace" || ev.keystroke.key == "delete" {
                                    this.on_delete_selection(cx);
                                }
```

Add an **Edit > Delete** menu entry for discoverability. This needs an action; add `DeleteSelection` to the `actions!` macro, register `cx.on_action` is not right here since this needs `&mut self` — instead register it the same way as `Undo`/`Redo` via `cx.listener(Self::on_delete_selection_action)`, where `on_delete_selection_action` is a thin wrapper matching the `fn on_undo(&mut self, _: &Undo, _window: &mut Window, cx: &mut Context<Self>)` signature and calling `self.on_delete_selection(cx)`. Add `MenuItem::action("Delete", DeleteSelection)` to the `"Edit"` `Menu` block, and `KeyBinding::new("backspace", DeleteSelection, None)` alongside the other bindings (drop the manual key-down branch above if the action-based binding covers it — prefer one mechanism; use the `KeyBinding`/action route since it's what `Undo`/`Redo` already use and keeps `Delete` visible in the Edit menu).

- [ ] **Step 6: Build, test, clippy**

Run: `cargo build && cargo test && cargo clippy`
Expected: clean.

- [ ] **Step 7: Manual spot-check note**

Add to PR description: "Manual check — select a node not in any way, press Delete, confirm it disappears; Undo brings it back. Select a way, Delete, Undo. Select a node that's part of a way, Delete, confirm the status line explains why it refused."

- [ ] **Step 8: Commit**

```bash
git add src/layers/osm_layer.rs src/layers/mod.rs src/main.rs
git commit -m "Add delete node/way editing primitive with undo"
```

---

### Task 2: Remove node from way

**Files:**
- Modify: `src/layers/osm_layer.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `OsmLayer::commit_remove_node_from_way(&mut self, way_id: i64, node_id: i64) -> Result<usize, String>` (returns the removed index, for undo re-insertion). `OsmLayer::commit_insert_existing_node_into_way(&mut self, way_id: i64, node_id: i64, index: usize)` (undo counterpart — re-inserts a node id that's already in `OsmData.nodes` at a specific index, distinct from Task 3's "create a new node" insert). `UndoableAction::RemoveNodeFromWay { layer_name: String, way_id: i64, node_id: i64, index: usize }`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn commit_remove_node_from_way_removes_by_id() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 41.0, lon: -75.0, tags: empty_tags() };
        let n3 = OsmNode { id: 3, lat: 42.0, lon: -76.0, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2, 3], tags: empty_tags() };
        let data = data_with(vec![n1, n2, n3], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let index = layer.commit_remove_node_from_way(10, 2).unwrap();
        assert_eq!(index, 1);
        assert_eq!(layer.way_node_ids(10), Some(vec![1, 3]));
        assert!(layer.edit_marks().modified_ways.contains(&10));
        // Node itself is untouched.
        assert!(layer.get_osm_data().unwrap().nodes.get(&2).is_some());
    }

    #[test]
    fn commit_remove_node_from_way_errors_if_not_a_member() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1], tags: empty_tags() };
        let data = data_with(vec![n1], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        assert!(layer.commit_remove_node_from_way(10, 999).is_err());
    }

    #[test]
    fn commit_insert_existing_node_into_way_reverses_removal() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 41.0, lon: -75.0, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let index = layer.commit_remove_node_from_way(10, 2).unwrap();
        layer.commit_insert_existing_node_into_way(10, 2, index);

        assert_eq!(layer.way_node_ids(10), Some(vec![1, 2]));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib commit_remove_node_from_way commit_insert_existing_node_into_way`
Expected: FAIL — methods don't exist.

- [ ] **Step 3: Implement**

```rust
    /// Remove `node_id` from `way_id`'s node list. Returns the index it was
    /// removed from (for undo). Errors if the way doesn't exist or doesn't
    /// contain the node. Does not touch `OsmData.nodes` — the node may still
    /// be referenced by other ways, or simply left orphaned (JOSM behavior).
    pub fn commit_remove_node_from_way(&mut self, way_id: i64, node_id: i64) -> Result<usize, String> {
        let Some(current) = self.osm_data.clone() else {
            return Err("no data loaded".to_string());
        };
        let mut data = (*current).clone();
        let Some(way) = data.ways.iter_mut().find(|w| w.id == way_id) else {
            return Err(format!("way {} not found", way_id));
        };
        let Some(index) = way.nodes.iter().position(|&id| id == node_id) else {
            return Err(format!("node {} is not a member of way {}", node_id, way_id));
        };
        way.nodes.remove(index);
        self.modified_way_ids.insert(way_id);
        self.modified = true;
        self.set_osm_data(Arc::new(data));
        Ok(index)
    }

    /// Undo counterpart of `commit_remove_node_from_way`: re-inserts
    /// `node_id` into `way_id`'s node list at `index`. No-op if the way
    /// no longer exists (shouldn't happen in normal undo/redo flow).
    pub fn commit_insert_existing_node_into_way(&mut self, way_id: i64, node_id: i64, index: usize) {
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        if let Some(way) = data.ways.iter_mut().find(|w| w.id == way_id) {
            let index = index.min(way.nodes.len());
            way.nodes.insert(index, node_id);
        }
        self.set_osm_data(Arc::new(data));
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib commit_remove_node_from_way commit_insert_existing_node_into_way`
Expected: PASS (3 tests)

- [ ] **Step 5: Wire `UndoableAction::RemoveNodeFromWay` and the Edit menu action**

Add to `UndoableAction` in `main.rs`:

```rust
    RemoveNodeFromWay { layer_name: String, way_id: i64, node_id: i64, index: usize },
```

`description()`:

```rust
            UndoableAction::RemoveNodeFromWay { .. } => "Removed node from way".to_string(),
```

`apply_undo_action`:

```rust
            UndoableAction::RemoveNodeFromWay { layer_name, way_id, node_id, index } => {
                if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                    if forward {
                        let _ = layer.commit_remove_node_from_way(*way_id, *node_id);
                    } else {
                        layer.commit_insert_existing_node_into_way(*way_id, *node_id, *index);
                    }
                }
            }
```

Add the two methods to the `MapLayer` trait (`src/layers/mod.rs`) with error-returning/no-op defaults, and forward them on `OsmLayer`, following exactly the same shape as Task 1's `commit_delete_features`/`restore_deleted_features` additions.

Add `actions!` entry `RemoveFromWay`, a `MapViewer::on_remove_from_way` method requiring `self.selected` to be exactly one `Node` feature that is a member of exactly one way in its layer's data (look up via `way_node_ids`/iterate `layer.get_osm_data()` — refuse with `set_status` otherwise, e.g. "select a node that belongs to exactly one way"), pushing `UndoableAction::RemoveNodeFromWay`. Register via `cx.listener(Self::on_remove_from_way)` like `on_undo`. Add `MenuItem::action("Remove From Way", RemoveFromWay)` to the `"Edit"` menu. No default key binding (menu-only, like the original JOSM "Unglue"/"Remove from way" which has no universal default key either).

- [ ] **Step 6: Build, test, clippy**

Run: `cargo build && cargo test && cargo clippy`
Expected: clean.

- [ ] **Step 7: Manual spot-check note**

"Select a shared-boundary node that's part of one way, Edit > Remove From Way, confirm the way's shape updates and the node itself still renders (unattached). Undo restores the way."

- [ ] **Step 8: Commit**

```bash
git add src/layers/osm_layer.rs src/layers/mod.rs src/main.rs
git commit -m "Add remove-node-from-way editing primitive with undo"
```

---

### Task 3: Insert node into way

**Files:**
- Modify: `src/layers/osm_layer.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `OsmLayer::commit_insert_new_node_into_way(&mut self, way_id: i64, index: usize, lat: f64, lon: f64) -> i64` (creates the node via `next_new_id`, returns its new id). `OsmLayer::commit_delete_features` (Task 1, reused for undo — a fresh node created here is deleted the same way a normal node is on undo, since it's a real entry in `OsmData.nodes` once committed) plus `commit_remove_node_from_way` (Task 2, reused to pull it back out of the way on undo without deleting the way's other structure — actually simpler: undo of an insert is "remove this node from the way, then delete the node," both already-built primitives).
- Consumes: `point_to_segment_distance` (`src/selection.rs`, existing), `OsmLayer::way_vertices`-equivalent public accessor — check whether one already exists; if the only way to get a way's projected vertex list is the private `way_vertices` field, add `pub fn way_geo_nodes(&self, way_id: i64) -> Option<Vec<(i64, f64, f64)>>` returning `(node_id, lat, lon)` triples for hit-testing in `main.rs` without exposing mercator internals.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn commit_insert_new_node_into_way_allocates_negative_id() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 41.0, lon: -75.0, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let new_id = layer.commit_insert_new_node_into_way(10, 1, 40.5, -74.5);
        assert!(new_id < 0);
        assert_eq!(layer.way_node_ids(10), Some(vec![1, new_id, 2]));
        assert_eq!(layer.node_lat_lon(new_id), Some((40.5, -74.5)));
        assert!(layer.edit_marks().new_nodes.contains(&new_id));
        assert!(layer.edit_marks().modified_ways.contains(&10));
    }

    #[test]
    fn commit_insert_new_node_into_way_allocates_distinct_ids_each_call() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1], tags: empty_tags() };
        let data = data_with(vec![n1], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let a = layer.commit_insert_new_node_into_way(10, 0, 40.1, -74.1);
        let b = layer.commit_insert_new_node_into_way(10, 0, 40.2, -74.2);
        assert_ne!(a, b);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib commit_insert_new_node_into_way`
Expected: FAIL — method doesn't exist.

- [ ] **Step 3: Implement**

```rust
    /// Create a new node at `(lat, lon)` with a synthetic negative id, and
    /// insert it into `way_id`'s node list at `index`. Returns the new
    /// node's id.
    pub fn commit_insert_new_node_into_way(&mut self, way_id: i64, index: usize, lat: f64, lon: f64) -> i64 {
        let new_id = self.next_new_id;
        self.next_new_id -= 1;

        let Some(current) = self.osm_data.clone() else { return new_id; };
        let mut data = (*current).clone();
        data.nodes.insert(new_id, OsmNode { id: new_id, lat, lon, tags: HashMap::new() });
        if let Some(way) = data.ways.iter_mut().find(|w| w.id == way_id) {
            let index = index.min(way.nodes.len());
            way.nodes.insert(index, new_id);
        }
        self.new_node_ids.insert(new_id);
        self.modified_way_ids.insert(way_id);
        self.modified = true;
        self.set_osm_data(Arc::new(data));
        new_id
    }

    /// Read-only view of a way's member nodes as `(id, lat, lon)` triples,
    /// for hit-testing against way segments outside this module (e.g. to
    /// find where to insert a new vertex).
    pub fn way_geo_nodes(&self, way_id: i64) -> Option<Vec<(i64, f64, f64)>> {
        let data = self.osm_data.as_ref()?;
        let way = data.ways.iter().find(|w| w.id == way_id)?;
        Some(
            way.nodes
                .iter()
                .filter_map(|id| data.nodes.get(id).map(|n| (*id, n.lat, n.lon)))
                .collect(),
        )
    }
```

`HashMap` should already be imported in this file (used elsewhere for tags); if not, add `use std::collections::HashMap;`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib commit_insert_new_node_into_way`
Expected: PASS (2 tests)

- [ ] **Step 5: Wire "Insert Node Mode" and undo**

Add to `UndoableAction`:

```rust
    InsertNodeIntoWay { layer_name: String, way_id: i64, index: usize, node_id: i64 },
```

`description()`: `"Inserted node into way".to_string()`.

`apply_undo_action`:

```rust
            UndoableAction::InsertNodeIntoWay { layer_name, way_id, index, node_id } => {
                if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                    if forward {
                        layer.commit_insert_existing_node_into_way(*way_id, *node_id, *index);
                        // The node itself was deleted on undo (see below); recreate it
                        // by re-inserting from the same recorded lat/lon is unnecessary
                        // here because redo re-inserts the *same* node id back into
                        // OsmData.nodes via restore_deleted_features, called first.
                    } else {
                        let _ = layer.commit_remove_node_from_way(*way_id, *node_id);
                        let _ = layer.commit_delete_features(&[*node_id], &[]);
                    }
                }
            }
```

This redo path has an ordering subtlety: undoing an insert deletes the new node entirely (via `commit_delete_features`), so redoing it must recreate the node before re-inserting it into the way, not just call `commit_insert_existing_node_into_way` (which assumes the node id already exists in `OsmData.nodes`). Store the node's `(lat, lon)` in the `UndoableAction` variant too so redo can recreate it deterministically with the *same* id:

```rust
    InsertNodeIntoWay { layer_name: String, way_id: i64, index: usize, node_id: i64, lat: f64, lon: f64 },
```

And the forward branch becomes:

```rust
                    if forward {
                        if layer.get_osm_data().and_then(|d| d.nodes.get(node_id).cloned()).is_none() {
                            // Recreate the exact node (same id) the undo step removed.
                            layer.restore_deleted_features(
                                vec![osm_gpui::osm::OsmNode { id: *node_id, lat: *lat, lon: *lon, tags: Default::default() }],
                                vec![],
                            );
                        }
                        layer.commit_insert_existing_node_into_way(*way_id, *node_id, *index);
                    }
```

Add `MapViewer` fields for the mode:

```rust
    /// Way id the user is inserting a node into via "Insert Node Mode", or
    /// `None` when the mode is inactive.
    insert_node_mode: Option<String>,  // layer_name; next left-click on a way in this layer inserts
```

Add `actions!` entry `InsertNodeMode`, a `MapViewer::on_insert_node_mode` handler that sets `self.insert_node_mode = Some(active_layer_name)` (reuse whichever "current layer" resolution Task 1 of the Export plan settled on — the first `OsmLayer` with data, via the same `export_xml`-style iteration but calling a new trait-default `fn layer_name_if_osm(&self) -> Option<String>` **or**, simpler, reuse `self.selected.first().map(|f| f.layer_name.clone())` if there's a selection, falling back to the first layer in `layer_manager.layers()` whose `name()` isn't a known non-OSM layer name like `"Grid"`/tile layer names — implementer's call, but must not crash when there's no OSM layer loaded: `set_status("load an OSM file first")` and return early instead).

In the existing left-click handler (`handle_map_mouse_down` / wherever clicks resolve a hit before assigning `self.selected`), add a branch checked first: if `self.insert_node_mode` is `Some(layer_name)`, resolve the click position to a way + segment index using `way_geo_nodes` and the viewport's `geo_to_screen` transform plus `point_to_segment_distance` (find the way under the cursor via the existing way hit-test, then walk its `way_geo_nodes` pairs to find the closest segment and its index), call `commit_insert_new_node_into_way`, push the undo action, set `self.insert_node_mode = None`, and return early (skip normal selection handling for this click). Esc (existing key handler) also clears `self.insert_node_mode` if set, before its existing `cancel_move_drag` behavior.

Add `MenuItem::action("Insert Node Mode", InsertNodeMode)` to the `"Edit"` menu.

- [ ] **Step 6: Build, test, clippy**

Run: `cargo build && cargo test && cargo clippy`
Expected: clean.

- [ ] **Step 7: Manual spot-check note**

"Edit > Insert Node Mode, click the middle of a way segment, confirm a new vertex appears there and the mode auto-exits. Undo removes it; Redo brings it back at the same position. Esc while in the mode cancels without inserting."

- [ ] **Step 8: Commit**

```bash
git add src/layers/osm_layer.rs src/main.rs
git commit -m "Add insert-node-into-way editing primitive with undo"
```

---

### Task 4: Split way

**Files:**
- Modify: `src/layers/osm_layer.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `OsmLayer::commit_split_way(&mut self, way_id: i64, node_id: i64) -> Result<i64, String>` (splits at the interior vertex matching `node_id`; returns the new way's id). `OsmLayer::commit_undo_split_way(&mut self, original_way_id: i64, new_way_id: i64, original_nodes_before_split: Vec<i64>)` (undo counterpart — restores the original's full node list and deletes the new way).
- `UndoableAction::SplitWay { layer_name: String, way_id: i64, new_way_id: i64, original_nodes: Vec<i64> }`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn commit_split_way_divides_at_interior_vertex() {
        let nodes: Vec<OsmNode> = (1..=4).map(|id| OsmNode { id, lat: id as f64, lon: -id as f64, tags: empty_tags() }).collect();
        let mut tags = empty_tags();
        tags.insert("highway".to_string(), "residential".to_string());
        let way = OsmWay { id: 10, nodes: vec![1, 2, 3, 4], tags };
        let data = data_with(nodes, vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let new_way_id = layer.commit_split_way(10, 2).unwrap();
        assert_eq!(layer.way_node_ids(10), Some(vec![1, 2]));
        assert_eq!(layer.way_node_ids(new_way_id), Some(vec![2, 3, 4]));
        assert!(layer.edit_marks().new_ways.contains(&new_way_id));
        assert!(layer.edit_marks().modified_ways.contains(&10));
    }

    #[test]
    fn commit_split_way_copies_tags_to_new_way() {
        let nodes: Vec<OsmNode> = (1..=3).map(|id| OsmNode { id, lat: id as f64, lon: -id as f64, tags: empty_tags() }).collect();
        let mut tags = empty_tags();
        tags.insert("highway".to_string(), "residential".to_string());
        let way = OsmWay { id: 10, nodes: vec![1, 2, 3], tags: tags.clone() };
        let data = data_with(nodes, vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let new_way_id = layer.commit_split_way(10, 2).unwrap();
        let data = layer.get_osm_data().unwrap();
        let new_way = data.ways.iter().find(|w| w.id == new_way_id).unwrap();
        assert_eq!(new_way.tags, tags);
    }

    #[test]
    fn commit_split_way_refuses_at_endpoint() {
        let nodes: Vec<OsmNode> = (1..=3).map(|id| OsmNode { id, lat: id as f64, lon: -id as f64, tags: empty_tags() }).collect();
        let way = OsmWay { id: 10, nodes: vec![1, 2, 3], tags: empty_tags() };
        let data = data_with(nodes, vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        assert!(layer.commit_split_way(10, 1).is_err());
        assert!(layer.commit_split_way(10, 3).is_err());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib commit_split_way`
Expected: FAIL — method doesn't exist.

- [ ] **Step 3: Implement**

```rust
    /// Split `way_id` into two ways at the interior vertex `node_id`
    /// (must be neither the first nor last node). The original way keeps
    /// nodes `[0..=idx]`; a new way (new negative id, tags copied from the
    /// original) gets nodes `[idx..]`. Returns the new way's id.
    pub fn commit_split_way(&mut self, way_id: i64, node_id: i64) -> Result<i64, String> {
        let Some(current) = self.osm_data.clone() else {
            return Err("no data loaded".to_string());
        };
        let mut data = (*current).clone();
        let way_idx = data.ways.iter().position(|w| w.id == way_id)
            .ok_or_else(|| format!("way {} not found", way_id))?;
        let node_pos = data.ways[way_idx].nodes.iter().position(|&id| id == node_id)
            .ok_or_else(|| format!("node {} is not a member of way {}", node_id, way_id))?;
        if node_pos == 0 || node_pos == data.ways[way_idx].nodes.len() - 1 {
            return Err("cannot split at a way endpoint".to_string());
        }

        let tail: Vec<i64> = data.ways[way_idx].nodes[node_pos..].to_vec();
        data.ways[way_idx].nodes.truncate(node_pos + 1);

        let new_way_id = self.next_new_id;
        self.next_new_id -= 1;
        let new_way = OsmWay { id: new_way_id, nodes: tail, tags: data.ways[way_idx].tags.clone() };
        data.ways.push(new_way);

        self.modified_way_ids.insert(way_id);
        self.new_way_ids.insert(new_way_id);
        self.modified = true;
        self.set_osm_data(Arc::new(data));
        Ok(new_way_id)
    }

    /// Undo counterpart of `commit_split_way`: restores `way_id`'s full
    /// node list and removes the way created by the split.
    pub fn commit_undo_split_way(&mut self, way_id: i64, new_way_id: i64, original_nodes: Vec<i64>) {
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        if let Some(way) = data.ways.iter_mut().find(|w| w.id == way_id) {
            way.nodes = original_nodes;
        }
        data.ways.retain(|w| w.id != new_way_id);
        self.new_way_ids.remove(&new_way_id);
        self.set_osm_data(Arc::new(data));
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib commit_split_way`
Expected: PASS (3 tests)

- [ ] **Step 5: Wire `UndoableAction::SplitWay` and Edit menu action**

Add to `UndoableAction`:

```rust
    SplitWay { layer_name: String, way_id: i64, new_way_id: i64, original_nodes: Vec<i64> },
```

`description()`: `"Split way".to_string()`.

`apply_undo_action`:

```rust
            UndoableAction::SplitWay { layer_name, way_id, new_way_id, original_nodes } => {
                if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                    if forward {
                        let _ = layer.commit_split_way(*way_id, original_nodes[
                            original_nodes.iter().position(|_| true).unwrap()
                        ]);
                    } else {
                        layer.commit_undo_split_way(*way_id, *new_way_id, original_nodes.clone());
                    }
                }
            }
```

The redo branch above needs the split vertex's node id, not just the original node list — store it explicitly instead of trying to recompute it. Change the variant to:

```rust
    SplitWay { layer_name: String, way_id: i64, split_node_id: i64, new_way_id: i64, original_nodes: Vec<i64> },
```

and the forward branch to:

```rust
                    if forward {
                        let _ = layer.commit_split_way(*way_id, *split_node_id);
                    } else {
```

Add `MapLayer` trait defaults + `OsmLayer` forwarders for `commit_split_way`/`commit_undo_split_way`, mirroring Task 1's pattern.

Add `actions!` entry `SplitWayAction`, a `MapViewer::on_split_way` handler: requires `self.selected` to be exactly one `Node` feature; find the way containing it by scanning the layers for a way whose `way_node_ids` contains that node id at an interior position (refuse via `set_status` if zero or more than one such way exists — "select a node that's an interior vertex of exactly one way"); capture `original_nodes` via `way_node_ids` *before* calling `commit_split_way`; push `UndoableAction::SplitWay`. Register via `cx.listener(Self::on_split_way)`. Add `MenuItem::action("Split Way", SplitWayAction)` to `"Edit"`.

- [ ] **Step 6: Build, test, clippy**

Run: `cargo build && cargo test && cargo clippy`
Expected: clean.

- [ ] **Step 7: Manual spot-check note**

"Select an interior vertex of a way, Edit > Split Way, confirm two ways now exist sharing that node, both with the original tags. Undo merges them back into one way with the original id."

- [ ] **Step 8: Commit**

```bash
git add src/layers/osm_layer.rs src/layers/mod.rs src/main.rs
git commit -m "Add split-way editing primitive with undo"
```

---

### Task 5: Join ways

**Files:**
- Modify: `src/layers/osm_layer.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `OsmLayer::commit_join_ways(&mut self, keep_way_id: i64, remove_way_id: i64) -> Result<Vec<i64>, String>` (returns `remove_way_id`'s original node list, for undo; errors if the ways don't share an endpoint or have differing non-empty tag sets). `OsmLayer::commit_undo_join_ways(&mut self, keep_way_id: i64, removed_way: OsmWay, kept_way_original_nodes: Vec<i64>)`.
- `UndoableAction::JoinWays { layer_name: String, keep_way_id: i64, kept_way_original_nodes: Vec<i64>, removed_way: OsmWay }`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn commit_join_ways_concatenates_sharing_endpoint() {
        let nodes: Vec<OsmNode> = (1..=4).map(|id| OsmNode { id, lat: id as f64, lon: -id as f64, tags: empty_tags() }).collect();
        let way_a = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let way_b = OsmWay { id: 11, nodes: vec![2, 3, 4], tags: empty_tags() };
        let data = data_with(nodes, vec![way_a, way_b]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.commit_join_ways(10, 11).unwrap();
        assert_eq!(layer.way_node_ids(10), Some(vec![1, 2, 3, 4]));
        assert!(layer.get_osm_data().unwrap().ways.iter().all(|w| w.id != 11));
        assert!(layer.edit_marks().deleted_way_ids.contains(&11));
    }

    #[test]
    fn commit_join_ways_reverses_second_way_if_needed() {
        // way_b's shared node (2) is at its *end*, not its start; must reverse
        // way_b before concatenating so node 2 isn't duplicated out of order.
        let nodes: Vec<OsmNode> = (1..=4).map(|id| OsmNode { id, lat: id as f64, lon: -id as f64, tags: empty_tags() }).collect();
        let way_a = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let way_b = OsmWay { id: 11, nodes: vec![4, 3, 2], tags: empty_tags() };
        let data = data_with(nodes, vec![way_a, way_b]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.commit_join_ways(10, 11).unwrap();
        assert_eq!(layer.way_node_ids(10), Some(vec![1, 2, 3, 4]));
    }

    #[test]
    fn commit_join_ways_refuses_without_shared_endpoint() {
        let nodes: Vec<OsmNode> = (1..=4).map(|id| OsmNode { id, lat: id as f64, lon: -id as f64, tags: empty_tags() }).collect();
        let way_a = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let way_b = OsmWay { id: 11, nodes: vec![3, 4], tags: empty_tags() };
        let data = data_with(nodes, vec![way_a, way_b]);
        let mut layer = OsmLayer::new_with_data("L", data);

        assert!(layer.commit_join_ways(10, 11).is_err());
    }

    #[test]
    fn commit_join_ways_refuses_conflicting_tags() {
        let nodes: Vec<OsmNode> = (1..=3).map(|id| OsmNode { id, lat: id as f64, lon: -id as f64, tags: empty_tags() }).collect();
        let mut tags_a = empty_tags();
        tags_a.insert("highway".to_string(), "residential".to_string());
        let mut tags_b = empty_tags();
        tags_b.insert("highway".to_string(), "footway".to_string());
        let way_a = OsmWay { id: 10, nodes: vec![1, 2], tags: tags_a };
        let way_b = OsmWay { id: 11, nodes: vec![2, 3], tags: tags_b };
        let data = data_with(nodes, vec![way_a, way_b]);
        let mut layer = OsmLayer::new_with_data("L", data);

        assert!(layer.commit_join_ways(10, 11).is_err());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib commit_join_ways`
Expected: FAIL — method doesn't exist.

- [ ] **Step 3: Implement**

```rust
    /// Join `remove_way_id` onto the end of `keep_way_id` if they share an
    /// endpoint node, keeping `keep_way_id`'s id and tags. Refuses if they
    /// don't share an endpoint, or if both have non-empty, differing tag
    /// sets. Returns `remove_way_id`'s original node list (for undo).
    pub fn commit_join_ways(&mut self, keep_way_id: i64, remove_way_id: i64) -> Result<Vec<i64>, String> {
        let Some(current) = self.osm_data.clone() else {
            return Err("no data loaded".to_string());
        };
        let mut data = (*current).clone();
        let keep_idx = data.ways.iter().position(|w| w.id == keep_way_id)
            .ok_or_else(|| format!("way {} not found", keep_way_id))?;
        let remove_idx = data.ways.iter().position(|w| w.id == remove_way_id)
            .ok_or_else(|| format!("way {} not found", remove_way_id))?;

        let keep_tags = data.ways[keep_idx].tags.clone();
        let remove_tags = data.ways[remove_idx].tags.clone();
        if !keep_tags.is_empty() && !remove_tags.is_empty() && keep_tags != remove_tags {
            return Err("ways have conflicting tags".to_string());
        }

        let keep_nodes = data.ways[keep_idx].nodes.clone();
        let mut remove_nodes = data.ways[remove_idx].nodes.clone();
        let original_remove_nodes = remove_nodes.clone();

        let keep_last = *keep_nodes.last().ok_or("way has no nodes")?;
        let keep_first = *keep_nodes.first().ok_or("way has no nodes")?;
        let remove_first = *remove_nodes.first().ok_or("way has no nodes")?;
        let remove_last = *remove_nodes.last().ok_or("way has no nodes")?;

        let mut new_nodes = keep_nodes.clone();
        if keep_last == remove_first {
            new_nodes.extend(remove_nodes.into_iter().skip(1));
        } else if keep_last == remove_last {
            remove_nodes.reverse();
            new_nodes.extend(remove_nodes.into_iter().skip(1));
        } else if keep_first == remove_last {
            remove_nodes.pop();
            remove_nodes.extend(new_nodes);
            new_nodes = remove_nodes;
        } else if keep_first == remove_first {
            remove_nodes.reverse();
            remove_nodes.pop();
            remove_nodes.extend(new_nodes);
            new_nodes = remove_nodes;
        } else {
            return Err("ways do not share an endpoint".to_string());
        }

        data.ways[keep_idx].nodes = new_nodes;
        if keep_tags.is_empty() {
            data.ways[keep_idx].tags = remove_tags;
        }
        data.ways.remove(remove_idx);

        self.modified_way_ids.insert(keep_way_id);
        self.deleted_way_ids.insert(remove_way_id);
        self.modified = true;
        self.set_osm_data(Arc::new(data));
        Ok(original_remove_nodes)
    }

    /// Undo counterpart of `commit_join_ways`: restores `keep_way_id`'s
    /// original node list and re-adds the removed way.
    pub fn commit_undo_join_ways(&mut self, keep_way_id: i64, removed_way: OsmWay, kept_way_original_nodes: Vec<i64>) {
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        if let Some(way) = data.ways.iter_mut().find(|w| w.id == keep_way_id) {
            way.nodes = kept_way_original_nodes;
        }
        self.deleted_way_ids.remove(&removed_way.id);
        data.ways.push(removed_way);
        self.set_osm_data(Arc::new(data));
    }
```

Note: `commit_undo_join_ways` needs the *original tags* of `keep_way_id` too if the "adopt remove_way's tags when keep_way_id had none" branch fired — to keep this correct, capture `keep_way_id`'s original tags alongside its node list before calling `commit_join_ways`, and restore both in the undo action (the `UndoableAction::JoinWays` variant below carries a full pre-join snapshot rather than just node ids, which sidesteps this).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib commit_join_ways`
Expected: PASS (4 tests)

- [ ] **Step 5: Wire `UndoableAction::JoinWays` and Edit menu action**

```rust
    JoinWays { layer_name: String, keep_way_id: i64, kept_way_before: OsmWay, removed_way: OsmWay },
```

`description()`: `"Joined ways".to_string()`.

`apply_undo_action`:

```rust
            UndoableAction::JoinWays { layer_name, keep_way_id, kept_way_before, removed_way } => {
                if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                    if forward {
                        let _ = layer.commit_join_ways(*keep_way_id, removed_way.id);
                    } else {
                        layer.commit_undo_join_ways(*keep_way_id, removed_way.clone(), kept_way_before.nodes.clone());
                        // Also restore original tags if the join adopted removed_way's tags.
                    }
                }
            }
```

Given the tag-adoption edge case noted in Step 3, prefer capturing and restoring the *entire* pre-join `OsmWay` for `keep_way_id` (`kept_way_before: OsmWay`) and have `commit_undo_join_ways` take that whole struct rather than just its node list, replacing the way entirely on undo:

```rust
    pub fn commit_undo_join_ways(&mut self, kept_way_before: OsmWay, removed_way: OsmWay) {
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        if let Some(way) = data.ways.iter_mut().find(|w| w.id == kept_way_before.id) {
            *way = kept_way_before;
        }
        self.deleted_way_ids.remove(&removed_way.id);
        data.ways.push(removed_way);
        self.set_osm_data(Arc::new(data));
    }
```

Update the Step 1-4 tests and implementation signature accordingly before moving on (this is a signature change from the initial draft above — apply it now, re-run `cargo test --lib commit_join_ways commit_undo_join_ways`, confirm still green).

Add `MapLayer` trait defaults + `OsmLayer` forwarders for `commit_join_ways`/`commit_undo_join_ways`, mirroring Task 1.

Add `actions!` entry `JoinWaysAction`, a `MapViewer::on_join_ways` handler: requires `self.selected` to contain exactly two `Way` features in the same layer; capture `keep_way_id = selected ways[0].id`, snapshot that way's current `OsmWay` via `layer.get_osm_data()` before calling `commit_join_ways`, and the other way's full `OsmWay` snapshot too (for `removed_way` in the undo variant — note `commit_join_ways` only returns node ids in the current signature; change its return type to `Result<(), String>` and have the caller in `on_join_ways` snapshot both ways' full `OsmWay` structs *before* calling it, since `main.rs` already has read access via `layer.get_osm_data()`). Refuse via `set_status` if selection isn't exactly two ways, or forward the error message from `commit_join_ways` if it refuses. Register via `cx.listener(Self::on_join_ways)`. Add `MenuItem::action("Join Ways", JoinWaysAction)` to `"Edit"`.

- [ ] **Step 6: Build, test, clippy**

Run: `cargo build && cargo test && cargo clippy`
Expected: clean.

- [ ] **Step 7: Manual spot-check note**

"Box-select two ways sharing an endpoint, Edit > Join Ways, confirm one way remains with the combined shape. Undo restores both original ways including their original tags. Try two ways with conflicting `highway` tags — confirm the join refuses with a status message."

- [ ] **Step 8: Commit**

```bash
git add src/layers/osm_layer.rs src/layers/mod.rs src/main.rs
git commit -m "Add join-ways editing primitive with undo"
```

---

### Task 6: Square corners

**Files:**
- Create: `src/geometry.rs`
- Modify: `src/layers/osm_layer.rs`
- Modify: `src/coordinates.rs` (make `mercator_to_lat_lon` public)
- Modify: `src/lib.rs` (add `pub mod geometry;`)
- Modify: `src/main.rs`

**Interfaces:**
- Produces: pure function `pub fn square_way_corners(vertices: &[(f64, f64)]) -> Vec<(f64, f64)>` in a new small module `src/geometry.rs` (mercator-meters in, mercator-meters out — kept unit-free/pure so it's trivially testable; the caller projects lat/lon to mercator before calling it and back after). `OsmLayer::commit_square_way(&mut self, way_id: i64) -> Result<Vec<(i64, f64, f64)>, String>` (returns the moved nodes' pre-square `(id, lat, lon)`, for undo — same shape as `NodeMoveUndoEntries`'s inner `Vec`, so `UndoableAction::MoveNodes` can be reused directly instead of a new variant).

- [ ] **Step 1: Write the failing tests**

Create `src/geometry.rs`:

```rust
//! Pure geometry helpers with no OSM/GPUI dependencies, so they're testable
//! as plain math.

/// Snap a closed polygon's near-90°/near-180° corners to exact right
/// angles, JOSM's "Square" (`Q`) operation. Operates in a flat metric
/// space (e.g. Web Mercator meters) — the caller is responsible for
/// projecting to/from lat/lon. `vertices` must be closed (first == last);
/// the returned vec has the same length and the same closure property.
///
/// Algorithm: compute the average bearing of edges closest to 0°/90° to
/// find the dominant axis pair, then for each vertex, replace it with the
/// intersection of the two lines (through its neighbors, parallel to the
/// nearest dominant axis) that best preserves its original position.
/// Vertices whose corner angle isn't within 15° of 90°/180° are left
/// unchanged.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squares_a_slightly_skewed_rectangle() {
        // A rectangle whose corners are each nudged ~2 degrees off 90°.
        let vertices = vec![
            (0.0, 0.0),
            (10.2, 0.1),
            (10.0, 5.3),
            (-0.3, 5.0),
            (0.0, 0.0),
        ];
        let squared = square_way_corners(&vertices);
        assert_eq!(squared.len(), vertices.len());
        assert_eq!(squared.first(), squared.last());

        // Check each interior angle is now within 0.5 degrees of 90.
        for i in 0..squared.len() - 1 {
            let prev = squared[(i + squared.len() - 2) % (squared.len() - 1)];
            let curr = squared[i];
            let next = squared[(i + 1) % (squared.len() - 1)];
            let angle = corner_angle_degrees(prev, curr, next);
            assert!((angle - 90.0).abs() < 0.5, "corner {} angle was {}", i, angle);
        }
    }

    fn corner_angle_degrees(prev: (f64, f64), curr: (f64, f64), next: (f64, f64)) -> f64 {
        let v1 = (prev.0 - curr.0, prev.1 - curr.1);
        let v2 = (next.0 - curr.0, next.1 - curr.1);
        let dot = v1.0 * v2.0 + v1.1 * v2.1;
        let mag1 = (v1.0 * v1.0 + v1.1 * v1.1).sqrt();
        let mag2 = (v2.0 * v2.0 + v2.1 * v2.1).sqrt();
        (dot / (mag1 * mag2)).acos().to_degrees()
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib squares_a_slightly_skewed_rectangle`
Expected: FAIL — `square_way_corners` doesn't exist yet.

- [ ] **Step 3: Implement `square_way_corners`**

Add above the `#[cfg(test)]` block in `src/geometry.rs`:

```rust
pub fn square_way_corners(vertices: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if vertices.len() < 4 {
        return vertices.to_vec();
    }
    let n = vertices.len() - 1; // last point duplicates the first (closed ring)
    let ring = &vertices[..n];

    // 1. Find the dominant axis: average bearing (mod 90°) of all edges,
    //    weighted toward edges already close to axis-aligned.
    let mut sin_sum = 0.0;
    let mut cos_sum = 0.0;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let angle = (b.1 - a.1).atan2(b.0 - a.0);
        let folded = angle * 4.0; // period 90° folded into a full turn for averaging
        sin_sum += folded.sin();
        cos_sum += folded.cos();
    }
    let dominant = sin_sum.atan2(cos_sum) / 4.0;
    let axis_u = (dominant.cos(), dominant.sin());
    let axis_v = (-dominant.sin(), dominant.cos());

    // 2. Project every vertex onto the (u, v) axis pair.
    let project = |p: (f64, f64)| (p.0 * axis_u.0 + p.1 * axis_u.1, p.0 * axis_v.0 + p.1 * axis_v.1);
    let unproject = |u: f64, v: f64| (u * axis_u.0 + v * axis_v.0, u * axis_u.1 + v * axis_v.1);

    // 3. For each vertex within 15 degrees of a right angle to its
    //    neighbors, snap it to share the appropriate neighbor's u or v
    //    coordinate (whichever the edge is closer to), alternating so
    //    consecutive edges stay perpendicular.
    let mut squared: Vec<(f64, f64)> = ring.to_vec();
    for i in 0..n {
        let prev = ring[(i + n - 1) % n];
        let curr = ring[i];
        let next = ring[(i + 1) % n];
        let angle = corner_angle_degrees(prev, curr, next);
        if (angle - 90.0).abs() > 15.0 {
            continue;
        }
        let (pu, pv) = project(prev);
        let (_cu, _cv) = project(curr);
        let (nu, nv) = project(next);
        // Snap curr's u to prev's u if the prev edge is more u-aligned than
        // v-aligned, else snap curr's v to prev's v (and symmetrically for next).
        let prev_edge_is_u_aligned = (project(curr).0 - pu).abs() < (project(curr).1 - pv).abs();
        let new_u = if prev_edge_is_u_aligned { pu } else { project(curr).0 };
        let new_v = if prev_edge_is_u_aligned { project(curr).1 } else { pv };
        let _ = (nu, nv);
        squared[i] = unproject(new_u, new_v);
    }

    let mut result = squared;
    result.push(result[0]);
    result
}

fn corner_angle_degrees(prev: (f64, f64), curr: (f64, f64), next: (f64, f64)) -> f64 {
    let v1 = (prev.0 - curr.0, prev.1 - curr.1);
    let v2 = (next.0 - curr.0, next.1 - curr.1);
    let dot = v1.0 * v2.0 + v1.1 * v2.1;
    let mag1 = (v1.0 * v1.0 + v1.1 * v1.1).sqrt();
    let mag2 = (v2.0 * v2.0 + v2.1 * v2.1).sqrt();
    (dot / (mag1 * mag2)).clamp(-1.0, 1.0).acos().to_degrees()
}
```

Remove the duplicate `corner_angle_degrees` from the test module (use `super::corner_angle_degrees` instead) to avoid a name clash — update the test's helper call site accordingly (delete the test module's private copy, keep only the `use super::*;` import which now brings in the module-level one).

Add `pub mod geometry;` to `src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib squares_a_slightly_skewed_rectangle`
Expected: PASS. If the angle assertion fails, the projection/snap logic above needs adjusting — this is genuinely fiddly geometry; treat the test's 0.5° tolerance as the actual acceptance bar and iterate on the implementation (not the test) until it passes. Add a second fixture test with a rectangle rotated ~30° to confirm the dominant-axis detection isn't just accidentally aligned to world axes:

```rust
    #[test]
    fn squares_a_rotated_rectangle() {
        // Same rectangle as above, rotated ~30 degrees and re-skewed slightly.
        let angle: f64 = 30.0_f64.to_radians();
        let base = [(0.0, 0.0), (10.2, 0.1), (10.0, 5.3), (-0.3, 5.0)];
        let rotated: Vec<(f64, f64)> = base.iter()
            .map(|(x, y)| (x * angle.cos() - y * angle.sin(), x * angle.sin() + y * angle.cos()))
            .collect();
        let mut vertices = rotated.clone();
        vertices.push(rotated[0]);

        let squared = square_way_corners(&vertices);
        for i in 0..squared.len() - 1 {
            let prev = squared[(i + squared.len() - 2) % (squared.len() - 1)];
            let curr = squared[i];
            let next = squared[(i + 1) % (squared.len() - 1)];
            let angle = corner_angle_degrees(prev, curr, next);
            assert!((angle - 90.0).abs() < 0.5, "corner {} angle was {}", i, angle);
        }
    }
```

Run: `cargo test --lib geometry::`
Expected: PASS (2 tests)

- [ ] **Step 5: `OsmLayer::commit_square_way`**

Add to `impl OsmLayer`:

```rust
    /// Square the corners of a closed way (`Q` in JOSM). Errors if the way
    /// isn't closed (first node id != last) or doesn't exist. Returns the
    /// moved nodes' pre-square `(id, lat, lon)` for undo, in the same shape
    /// `commit_node_moves` consumes — square is implemented as a batch of
    /// node moves, reusing `UndoableAction::MoveNodes` rather than a new variant.
    pub fn commit_square_way(&mut self, way_id: i64) -> Result<Vec<(i64, f64, f64)>, String> {
        let Some(current) = self.osm_data.clone() else {
            return Err("no data loaded".to_string());
        };
        let way = current.ways.iter().find(|w| w.id == way_id)
            .ok_or_else(|| format!("way {} not found", way_id))?;
        if way.nodes.len() < 4 || way.nodes.first() != way.nodes.last() {
            return Err("way is not closed".to_string());
        }

        let before: Vec<(i64, f64, f64)> = way.nodes.iter()
            .filter_map(|id| current.nodes.get(id).map(|n| (*id, n.lat, n.lon)))
            .collect();

        // Project to Web Mercator meters (flat enough for this operation at
        // building scale), square, then project back.
        let mercator: Vec<(f64, f64)> = before.iter()
            .map(|&(_, lat, lon)| crate::coordinates::lat_lon_to_mercator(lat, lon))
            .collect();
        let squared_mercator = crate::geometry::square_way_corners(&mercator);

        let moves: Vec<(i64, f64, f64)> = before.iter().zip(squared_mercator.iter())
            .map(|(&(id, _, _), &(mx, my))| {
                let (lat, lon) = crate::coordinates::mercator_to_lat_lon(mx, my);
                (id, lat, lon)
            })
            .collect();
        self.commit_node_moves(&moves);
        Ok(before)
    }
```

`src/coordinates.rs` already has `fn mercator_to_lat_lon(x: f64, y: f64) -> (f64, f64)` (the exact inverse of `lat_lon_to_mercator`), but it's private. Change its signature to `pub fn mercator_to_lat_lon(...)` so `osm_layer.rs` can call it — this is the only change needed to `coordinates.rs` for this task.

- [ ] **Step 6: Write and run the `commit_square_way` test**

```rust
    #[test]
    fn commit_square_way_moves_nodes_and_returns_before_state() {
        let nodes: Vec<OsmNode> = vec![
            OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() },
            OsmNode { id: 2, lat: 40.0001, lon: -74.0, tags: empty_tags() },
            OsmNode { id: 3, lat: 40.0001, lon: -73.9999, tags: empty_tags() },
            OsmNode { id: 4, lat: 40.0, lon: -73.9999, tags: empty_tags() },
        ];
        let way = OsmWay { id: 10, nodes: vec![1, 2, 3, 4, 1], tags: empty_tags() };
        let data = data_with(nodes, vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let before = layer.commit_square_way(10).unwrap();
        assert_eq!(before.len(), 4);
        assert!(layer.is_modified());
    }

    #[test]
    fn commit_square_way_refuses_open_way() {
        let nodes: Vec<OsmNode> = vec![
            OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() },
            OsmNode { id: 2, lat: 40.0001, lon: -74.0, tags: empty_tags() },
        ];
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(nodes, vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        assert!(layer.commit_square_way(10).is_err());
    }
```

Run: `cargo test --lib commit_square_way`
Expected: PASS (2 tests)

- [ ] **Step 7: Wire the `Q` key / Edit menu action**

`commit_square_way` returns `Vec<(i64, f64, f64)>` in the exact shape `UndoableAction::MoveNodes`'s `per_layer` entries need (`(layer_name, Vec<(id, before, after)>)`) once paired with the post-square lat/lon (read back via `node_lat_lon` after committing). Add a `MapViewer::on_square_way` method:

```rust
    fn on_square_way(&mut self, cx: &mut Context<Self>) {
        let Some(feature) = self.selected.first().cloned() else {
            self.set_status("select a closed way first");
            return;
        };
        if feature.kind != osm_gpui::selection::FeatureKind::Way {
            self.set_status("select a closed way first");
            return;
        }
        let layer_name = feature.layer_name.clone();
        let Some(layer) = self.layer_manager.find_layer_mut(&layer_name) else { return; };
        match layer.commit_square_way(feature.id) {
            Ok(before) => {
                let entries: Vec<(i64, (f64, f64), (f64, f64))> = before.iter()
                    .filter_map(|&(id, blat, blon)| {
                        layer.node_lat_lon(id).map(|(alat, alon)| (id, (blat, blon), (alat, alon)))
                    })
                    .collect();
                self.undo_stack.push(UndoableAction::MoveNodes { per_layer: vec![(layer_name, entries)] });
                cx.notify();
            }
            Err(msg) => self.set_status(msg),
        }
    }
```

This requires `OsmLayer::commit_square_way`/`node_lat_lon` to be reachable through `&mut Box<dyn MapLayer>` — `node_lat_lon` is already on the trait (used elsewhere per the earlier grep of this file); add `commit_square_way` to the `MapLayer` trait with an `Err("layer does not support square".to_string())` default, forwarded on `OsmLayer`, mirroring Task 1's pattern.

Add `actions!` entry `SquareWay`, register `cx.listener(Self::on_square_way_action)` (thin wrapper calling `self.on_square_way(cx)`, matching the `on_undo` signature), `KeyBinding::new("q", SquareWay, None)`, and `MenuItem::action("Square", SquareWay)` in `"Edit"`.

- [ ] **Step 8: Build, test, clippy**

Run: `cargo build && cargo test && cargo clippy`
Expected: clean.

- [ ] **Step 9: Manual spot-check note**

"Select a slightly-skewed rectangular building way, press Q, confirm its corners snap to right angles. Undo restores the original shape exactly."

- [ ] **Step 10: Commit**

```bash
git add src/geometry.rs src/lib.rs src/coordinates.rs src/layers/osm_layer.rs src/layers/mod.rs src/main.rs
git commit -m "Add square-corners editing primitive with undo"
```

---

### Task 7: Draw new way

**Files:**
- Modify: `src/layers/osm_layer.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `OsmLayer::commit_create_way(&mut self, points: &[(f64, f64)]) -> i64` (creates a new node per point plus a new way linking them, all via `next_new_id`; returns the new way's id). `OsmLayer::commit_undo_create_way(&mut self, way_id: i64, node_ids: Vec<i64>)` (deletes the way and its nodes — safe because nodes created by drawing are never shared with pre-existing ways).
- `UndoableAction::CreateWay { layer_name: String, way_id: i64, node_ids: Vec<i64> }`.
- Consumes: nothing new from prior tasks (independent of Tasks 1-6 other than the shared `next_new_id` field from the Export plan).

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn commit_create_way_creates_nodes_and_way() {
        let data = data_with(vec![], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let way_id = layer.commit_create_way(&[(40.0, -74.0), (40.1, -74.1), (40.2, -74.2)]);
        assert!(way_id < 0);
        let node_ids = layer.way_node_ids(way_id).unwrap();
        assert_eq!(node_ids.len(), 3);
        for id in &node_ids {
            assert!(*id < 0);
            assert!(layer.edit_marks().new_nodes.contains(id));
        }
        assert!(layer.edit_marks().new_ways.contains(&way_id));
    }

    #[test]
    fn commit_undo_create_way_removes_way_and_its_nodes() {
        let data = data_with(vec![], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        let way_id = layer.commit_create_way(&[(40.0, -74.0), (40.1, -74.1)]);
        let node_ids = layer.way_node_ids(way_id).unwrap();
        layer.commit_undo_create_way(way_id, node_ids.clone());

        assert!(layer.way_node_ids(way_id).is_none());
        for id in &node_ids {
            assert_eq!(layer.node_lat_lon(*id), None);
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib commit_create_way`
Expected: FAIL — methods don't exist.

- [ ] **Step 3: Implement**

```rust
    /// Create a new way from a sequence of `(lat, lon)` points: one new
    /// node per point plus a new way linking them in order. Returns the
    /// new way's id.
    pub fn commit_create_way(&mut self, points: &[(f64, f64)]) -> i64 {
        let Some(current) = self.osm_data.clone() else {
            // No data loaded yet (e.g. drawing the very first feature into
            // an empty layer) — start from an empty dataset.
            let mut data = OsmData { nodes: HashMap::new(), ways: Vec::new(), relations: Vec::new(), bounds: None };
            let way_id = self.create_way_in(&mut data, points);
            self.set_osm_data(Arc::new(data));
            return way_id;
        };
        let mut data = (*current).clone();
        let way_id = self.create_way_in(&mut data, points);
        self.set_osm_data(Arc::new(data));
        way_id
    }

    fn create_way_in(&mut self, data: &mut OsmData, points: &[(f64, f64)]) -> i64 {
        let mut node_ids = Vec::with_capacity(points.len());
        for &(lat, lon) in points {
            let id = self.next_new_id;
            self.next_new_id -= 1;
            data.nodes.insert(id, OsmNode { id, lat, lon, tags: HashMap::new() });
            self.new_node_ids.insert(id);
            node_ids.push(id);
        }
        let way_id = self.next_new_id;
        self.next_new_id -= 1;
        data.ways.push(OsmWay { id: way_id, nodes: node_ids, tags: HashMap::new() });
        self.new_way_ids.insert(way_id);
        self.modified = true;
        way_id
    }

    /// Undo counterpart of `commit_create_way`: removes the way and every
    /// node it created. Safe only because drawn nodes are never shared
    /// with other ways at creation time.
    pub fn commit_undo_create_way(&mut self, way_id: i64, node_ids: Vec<i64>) {
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        data.ways.retain(|w| w.id != way_id);
        for id in &node_ids {
            data.nodes.remove(id);
            self.new_node_ids.remove(id);
        }
        self.new_way_ids.remove(&way_id);
        self.set_osm_data(Arc::new(data));
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib commit_create_way commit_undo_create_way`
Expected: PASS (2 tests)

- [ ] **Step 5: Wire "Draw Way" mode**

Add to `UndoableAction`:

```rust
    CreateWay { layer_name: String, way_id: i64, node_ids: Vec<i64> },
```

`description()`: `"Drew new way".to_string()`.

`apply_undo_action`:

```rust
            UndoableAction::CreateWay { layer_name, way_id, node_ids } => {
                if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                    if forward {
                        // Redo: node_ids/way_id are already fixed (assigned once
                        // at creation time), so recreate deterministically by id
                        // rather than calling commit_create_way again (which
                        // would allocate *new* ids). Reuse restore-style insertion:
                        // this requires the original points, so CreateWay must
                        // also carry them.
                    } else {
                        layer.commit_undo_create_way(*way_id, node_ids.clone());
                    }
                }
            }
```

The redo path needs the original `(lat, lon)` points to recreate the exact same node ids (`commit_create_way` always allocates fresh ids from `next_new_id`, which would drift on redo). Store them in the variant:

```rust
    CreateWay { layer_name: String, way_id: i64, node_ids: Vec<i64>, points: Vec<(f64, f64)> },
```

Add an `OsmLayer` method for redo that recreates at fixed ids (mirrors the `InsertNodeIntoWay` redo fix in Task 3):

```rust
    /// Redo counterpart of `commit_create_way`: recreates the way and its
    /// nodes at the exact ids they had before undo removed them.
    pub fn commit_redo_create_way(&mut self, way_id: i64, node_ids: &[i64], points: &[(f64, f64)]) {
        let Some(current) = self.osm_data.clone() else { return; };
        let mut data = (*current).clone();
        for (&id, &(lat, lon)) in node_ids.iter().zip(points.iter()) {
            data.nodes.insert(id, OsmNode { id, lat, lon, tags: HashMap::new() });
            self.new_node_ids.insert(id);
        }
        data.ways.push(OsmWay { id: way_id, nodes: node_ids.to_vec(), tags: HashMap::new() });
        self.new_way_ids.insert(way_id);
        self.set_osm_data(Arc::new(data));
    }
```

And the forward branch:

```rust
                    if forward {
                        layer.commit_redo_create_way(*way_id, node_ids, points);
                    } else {
```

Add `MapLayer` trait defaults + `OsmLayer` forwarders for `commit_create_way`/`commit_undo_create_way`/`commit_redo_create_way`, mirroring Task 1's pattern (note `commit_create_way`'s trait default takes `&[(f64,f64)]` and returns `i64` — pick `0` as the non-OSM-layer default return, since callers only invoke it on a layer already known to be an `OsmLayer` via the same "first OsmLayer" resolution used elsewhere in this plan).

Add `MapViewer` fields:

```rust
    /// In-progress "Draw Way" points (geo lat/lon), or `None` when not drawing.
    drawing_way: Option<(String, Vec<(f64, f64)>)>, // (layer_name, points so far)
```

Add `actions!` entry `DrawWayMode`, a handler `on_draw_way_mode` that sets `self.drawing_way = Some((active_layer_name, Vec::new()))` (same "first OsmLayer with data, or the layer of the current selection" resolution used in Task 3's Insert Node Mode — if there is no `OsmLayer` at all, `commit_create_way` still works against an empty dataset per Step 3's `None` branch, so allow starting a draw even with nothing loaded, defaulting to a layer named `"Drawn"` created on the fly if `layer_manager` has no `OsmLayer` yet: `layer_manager.add_layer(Box::new(OsmLayer::new_with_data("Drawn", Arc::new(OsmData { nodes: HashMap::new(), ways: Vec::new(), relations: Vec::new(), bounds: None }))))`).

In the left-click handler, add a branch checked before normal selection handling (and before the Task 3 insert-node-mode branch — order the checks so only one mode can be active, refusing to enter Draw Way mode while Insert Node mode is active and vice versa, via early-return `set_status` in each `on_*_mode` handler if the other is `Some`): if `self.drawing_way` is `Some((layer_name, points))`, append the clicked position's `(lat, lon)` (via the viewport's screen-to-geo conversion) to `points`.

Add a key-down branch (alongside the existing Escape handling) for `Enter`: if `self.drawing_way` is `Some((layer_name, points))` and `points.len() >= 2`, call `layer.commit_create_way(&points)`, push `UndoableAction::CreateWay { layer_name, way_id, node_ids, points: points.clone() }` (fetch `node_ids` via `layer.way_node_ids(way_id)` right after committing), clear `self.drawing_way`. If `points.len() < 2`, `set_status("draw at least 2 points first")` instead of committing.

Extend the existing Escape branch: if `self.drawing_way.is_some()`, clear it (discard points, no undo entry) before falling through to the existing `cancel_move_drag` call.

Render the in-progress points as a simple polyline overlay: in `MapViewer`'s canvas render closure (find where `render_all_canvas` / the layer canvas painting happens), after the layer rendering, if `self.drawing_way` is `Some((_, points))` and has ≥2 points, build a `PathBuilder` from the points (projected to screen via the existing viewport transform) and `window.paint_path` it in a distinct color (e.g. bright green) — follow the exact `PathBuilder`/`paint_path` calling convention already used in `OsmLayer::render_canvas` (same crate, same gpui version, so the API is identical; copy the stroke-path construction pattern from there, not the fill pattern from `paint_quad`).

Add `MenuItem::action("Draw Way", DrawWayMode)` to `"Edit"`.

- [ ] **Step 6: Build, test, clippy**

Run: `cargo build && cargo test && cargo clippy`
Expected: clean.

- [ ] **Step 7: Manual spot-check note**

"Edit > Draw Way, click 3 points on the map, confirm a green in-progress line follows; press Enter, confirm a new way with 3 new nodes now renders normally. Undo removes it entirely (way and all 3 nodes); Redo brings back the identical way/nodes. Esc mid-draw discards the in-progress points with no undo entry."

- [ ] **Step 8: Commit**

```bash
git add src/layers/osm_layer.rs src/layers/mod.rs src/main.rs
git commit -m "Add draw-new-way editing primitive with undo"
```

---

## Self-review notes for the implementer

- Tasks 4 and 5 (`SplitWay`/`JoinWays` undo variants) each went through a signature revision mid-task once the redo/undo asymmetry became clear (needing the split node id explicitly; needing the full pre-join `OsmWay` rather than just node ids). Apply the *final* signature shown at the end of each task's Step 5, not the first draft shown earlier in the same section — re-run that task's tests after the revision before moving on.
- Every op that creates new elements (`InsertNodeIntoWay`, `SplitWay`, `CreateWay`) needs its **redo** path to recreate elements at their original ids rather than re-invoking the `commit_*` method naively (which would allocate fresh ids from `next_new_id` and drift). Double-check this in code review for each task — it's the single easiest mistake to make across this whole plan.
- `commit_join_ways`'s tag-conflict check treats "one side has no tags" as compatible (adopts the other side's tags). Confirm this matches the spec's intent (`docs/superpowers/specs/2026-07-06-editing-primitives-design.md`) during Task 5's review — the design doc's wording ("refuse ... if tag sets differ") is slightly ambiguous about the no-tags case; the implementation in this plan resolves it in the more permissive direction.
