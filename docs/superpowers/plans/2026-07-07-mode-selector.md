# Mode Selector (Select / Add / Building / Extrude) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a left-hand mode-selector toolbar (Select / Add / Building / Extrude) that lets a user click nodes and ways into existence, place `building=yes` rectangles, and extrude rectangles off existing way segments, all as reversible undo actions on a designated "active" OSM layer.

**Architecture:** New mutation primitives on `OsmLayer` (mirroring the existing `commit_node_moves`/`set_tag` clone-mutate-patch-caches pattern) back three new `UndoableAction` variants. A new `EditMode` enum drives which interaction handler `MapViewer`'s existing mouse-down/move/up pipeline dispatches to; each mode's in-progress state (`add_progress`/`building_progress`/`extrude_drag`) lives alongside the existing `move_drag`/`box_select` fields. A new left toolbar panel (mirroring `render_side_panel`) switches modes; the existing Layers panel gains a click-to-set "active layer".

**Tech Stack:** Rust, gpui/gpui-component, rstar (R-tree spatial index).

## Global Constraints

- Placeholder (locally-created) node/way ids are negative, starting at `-1` and decrementing, per `docs/superpowers/specs/2026-07-07-mode-selector-design.md`. No id-remapping/upload path exists or is in scope.
- Every new `OsmLayer` mutation method follows the existing clone-`Arc<OsmData>`-mutate-patch-caches-set-modified pattern (see `commit_node_moves`, `src/layers/osm_layer.rs:412-543`).
- No GUI automation is available in this sandbox (documented project limitation) — verify interaction wiring via `cargo build`/`cargo test`/`cargo clippy`, not a driven UI.
- Follow existing repo conventions: `actions!` / `#[action(namespace = ...)]` for dispatch (`src/main.rs:39-72`), `cx.listener(Self::on_x)` + `.on_action(...)` wiring in `render()` (`src/main.rs:1388-1391`), `KeyBinding::new(...)` registered in `main()` (`src/main.rs:1568-1575`).

---

## File Structure

- **Modify** `src/layers/mod.rs` — add 4 new default-no-op `MapLayer` trait methods (`add_node`, `add_way`, `extend_way`, `insert_node_into_way`) plus their inverses (`remove_node`, `remove_way`, `remove_node_from_way`).
- **Modify** `src/layers/osm_layer.rs` — inherent implementations of the above, plus two placeholder-id counters.
- **Modify** `src/undo.rs` — 5 new `UndoableAction` variants + `description()` arms.
- **Modify** `src/selection.rs` — pure geometry helper for the Building/Extrude perpendicular rectangle, shared by both modes.
- **Create** `src/mode_panel.rs` — the new left toolbar panel (`render_mode_panel`), mirroring `src/side_panel.rs`'s structure.
- **Modify** `src/main.rs` — `EditMode` enum, `SetMode` action + keybindings, new `MapViewer` fields (`mode`, `active_layer`, `add_progress`, `building_progress`, `extrude_drag`), left-panel insertion into `render()`, mouse handler dispatch by mode, Escape handling.
- **Modify** `src/side_panel.rs` — click-to-set active layer, active-row highlight.

---

### Task 1: `OsmLayer` mutation primitives + placeholder ids

**Files:**
- Modify: `src/layers/mod.rs` (trait, after `remove_tag` at line 111)
- Modify: `src/layers/osm_layer.rs`
- Test: `src/layers/osm_layer.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces (used by Tasks 2, 6, 7, 8):
  - `OsmLayer::add_node(&mut self, lat: f64, lon: f64) -> i64`
  - `OsmLayer::add_way(&mut self, node_ids: Vec<i64>, tags: Vec<(String, String)>) -> i64`
  - `OsmLayer::extend_way(&mut self, way_id: i64, node_id: i64)`
  - `OsmLayer::insert_node_into_way(&mut self, way_id: i64, index: usize, lat: f64, lon: f64) -> i64`
  - `OsmLayer::remove_node(&mut self, node_id: i64)`
  - `OsmLayer::remove_way(&mut self, way_id: i64)`
  - `OsmLayer::remove_node_from_way(&mut self, way_id: i64, index: usize)`
  - Same 7 methods added to the `MapLayer` trait (default no-op bodies), delegating in `OsmLayer`'s `impl MapLayer for OsmLayer` block exactly like `commit_node_moves`/`set_tag` do today (`src/layers/osm_layer.rs:689-699`).

- [ ] **Step 1: Write failing tests for `add_node`/`remove_node`**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/layers/osm_layer.rs` (after the existing tests, before the closing `}` — find it with the existing `hit_test_rect_empty_when_no_data` test as an anchor):

```rust
    #[test]
    fn add_node_assigns_decrementing_placeholder_ids_and_is_hit_testable() {
        let mut layer = OsmLayer::new();
        let viewport = viewport_centered_on(40.0, -74.0);

        let id1 = layer.add_node(40.0, -74.0);
        let id2 = layer.add_node(40.0, -74.0);
        assert_eq!(id1, -1, "first placeholder id should be -1");
        assert_eq!(id2, -2, "ids should decrement");
        assert!(layer.is_modified());

        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits.iter().any(|h| h.feature.id == id1 && h.kind == FeatureKind::Node));
    }

    #[test]
    fn remove_node_drops_it_from_data_and_index() {
        let mut layer = OsmLayer::new();
        let viewport = viewport_centered_on(40.0, -74.0);
        let id = layer.add_node(40.0, -74.0);

        layer.remove_node(id);

        let data = layer.get_osm_data().expect("data should still exist");
        assert!(!data.nodes.contains_key(&id));
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits.is_empty(), "removed node should not be hit-testable: {:?}", hits);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib osm_layer::tests::add_node_assigns_decrementing_placeholder_ids_and_is_hit_testable`
Expected: FAIL to compile — `add_node`/`remove_node` don't exist yet.

- [ ] **Step 3: Add placeholder-id counters and `add_node`/`remove_node`**

In `src/layers/osm_layer.rs`, add two fields to the `OsmLayer` struct (after `modified: bool,` at line 113):

```rust
    /// Next id to hand out for a locally-created node (JOSM-style negative
    /// placeholder ids; no upload/remap path exists yet).
    next_placeholder_node_id: i64,
    /// Next id to hand out for a locally-created way.
    next_placeholder_way_id: i64,
```

Initialize both to `-1` in `OsmLayer::new()` (line ~344) and `new_with_data` (line ~373), e.g. `next_placeholder_node_id: -1, next_placeholder_way_id: -1,`.

Add the inherent methods after `remove_tag` (after line 582, before `refresh_cached_style`):

```rust
    /// Insert a brand-new node at `(lat, lon)`, assigning the next
    /// placeholder id (negative, decrementing). Patches `node_cache` and
    /// `node_index` incrementally; does not touch any way. Marks the layer
    /// modified. Returns the new node's id.
    pub fn add_node(&mut self, lat: f64, lon: f64) -> i64 {
        let id = self.next_placeholder_node_id;
        self.next_placeholder_node_id -= 1;

        let Some(current) = self.osm_data.clone() else {
            let mut data = OsmData {
                nodes: HashMap::new(),
                ways: Vec::new(),
                relations: Vec::new(),
                bounds: None,
            };
            data.nodes.insert(id, crate::osm::OsmNode {
                id, lat, lon, version: 0, tags: HashMap::new(),
            });
            self.modified = true;
            self.patch_node_cache_insert(id, lat, lon, &data);
            self.osm_data = Some(Arc::new(data));
            return id;
        };
        let mut data = (*current).clone();
        data.nodes.insert(id, crate::osm::OsmNode {
            id, lat, lon, version: 0, tags: HashMap::new(),
        });
        self.modified = true;
        self.patch_node_cache_insert(id, lat, lon, &data);
        self.osm_data = Some(Arc::new(data));
        id
    }

    /// Shared by `add_node`/`insert_node_into_way`: project `(lat, lon)`,
    /// append to `node_cache`, and insert into `node_index`/`layer_bbox`.
    /// No-op (leaves the node absent from position caches) if the
    /// coordinates are invalid, mirroring `compute_node_cache`.
    fn patch_node_cache_insert(&mut self, id: i64, lat: f64, lon: f64, data: &OsmData) {
        let Some((lat, lon)) = validate_coords(lat, lon) else { return };
        let (mx, my) = lat_lon_to_mercator(lat, lon);
        let node = &data.nodes[&id];
        let style = self.stylesheet.node_style(&node.tags);
        let idx = self.node_cache.flat.len();
        self.node_cache.flat.push((id, mx, my));
        self.node_cache.styles.push(style);
        self.node_cache.index_by_id.insert(id, idx);
        self.node_index.insert(GeomWithData::new([mx, my], id));
        match &mut self.layer_bbox {
            Some(lb) => lb.extend(mx, my),
            None => self.layer_bbox = Some(WayBbox { min_x: mx, max_x: mx, min_y: my, max_y: my }),
        }
    }

    /// Remove a node this layer owns from `OsmData` and every derived
    /// cache/index. Does NOT remove it from any way's node list — callers
    /// (undo) are responsible for calling `remove_node_from_way` first if
    /// the node is still referenced. No-op if the node isn't present.
    pub fn remove_node(&mut self, node_id: i64) {
        let Some(current) = self.osm_data.clone() else { return };
        let mut data = (*current).clone();
        if data.nodes.remove(&node_id).is_none() {
            return;
        }
        if let Some(idx) = self.node_cache.index_by_id.remove(&node_id) {
            let (_, mx, my) = self.node_cache.flat[idx];
            self.node_index.remove(&GeomWithData::new([mx, my], node_id));
            self.node_cache.flat.remove(idx);
            self.node_cache.styles.remove(idx);
            for v in self.node_cache.index_by_id.values_mut() {
                if *v > idx { *v -= 1; }
            }
        }
        self.node_to_ways.remove(&node_id);
        self.modified = true;
        self.osm_data = Some(Arc::new(data));
    }
```

Add `use crate::osm::OsmData;` alongside the existing `use crate::osm::{OsmData, OsmWay};` import at the top if not already imported (it already is, at line 7 — no change needed there).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib osm_layer::tests::add_node_assigns_decrementing_placeholder_ids_and_is_hit_testable osm_layer::tests::remove_node_drops_it_from_data_and_index`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/layers/osm_layer.rs
git commit -m "Add OsmLayer::add_node/remove_node with placeholder ids"
```

- [ ] **Step 6: Write failing tests for `add_way`/`remove_way`**

```rust
    #[test]
    fn add_way_creates_closed_building_way_and_is_hit_testable() {
        let mut layer = OsmLayer::new();
        let viewport = viewport_centered_on(40.0, -74.0);
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let way_id = layer.add_way(vec![n1, n2, n1], vec![("building".to_string(), "yes".to_string())]);
        assert_eq!(way_id, -1, "first placeholder way id should be -1");

        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(hits.iter().any(|h| h.feature.id == way_id && h.kind == FeatureKind::Way));
        let tags = layer.feature_tags(&crate::selection::FeatureRef {
            layer_name: layer.name().to_string(),
            kind: FeatureKind::Way,
            id: way_id,
        }).expect("way should have tags");
        assert!(tags.contains(&("building".to_string(), "yes".to_string())));
    }

    #[test]
    fn remove_way_drops_it_from_data_and_index_but_keeps_its_nodes() {
        let mut layer = OsmLayer::new();
        let viewport = viewport_centered_on(40.0, -74.0);
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let way_id = layer.add_way(vec![n1, n2], vec![]);

        layer.remove_way(way_id);

        let data = layer.get_osm_data().unwrap();
        assert!(!data.ways.iter().any(|w| w.id == way_id));
        assert!(data.nodes.contains_key(&n1), "removing the way must not remove its nodes");
        let hits = layer.hit_test(&viewport, point(px(400.0), px(300.0)));
        assert!(!hits.iter().any(|h| h.kind == FeatureKind::Way), "way should no longer hit-test: {:?}", hits);
    }
```

- [ ] **Step 7: Run to verify failure**

Run: `cargo test --lib osm_layer::tests::add_way_creates_closed_building_way_and_is_hit_testable`
Expected: FAIL to compile — `add_way`/`remove_way` don't exist yet.

- [ ] **Step 8: Implement `add_way`/`remove_way`**

Add after `add_node`/`remove_node`:

```rust
    /// Insert a brand-new way referencing existing node ids (must already
    /// exist in this layer — callers create nodes with `add_node` first),
    /// assigning the next placeholder way id. Rebuilds this one way's
    /// vertex/bbox/style caches and inserts it into `way_index`/
    /// `node_to_ways`/`way_id_to_index`. Marks the layer modified. Returns
    /// the new way's id.
    pub fn add_way(&mut self, node_ids: Vec<i64>, tags: Vec<(String, String)>) -> i64 {
        let id = self.next_placeholder_way_id;
        self.next_placeholder_way_id -= 1;

        let current = self.osm_data.clone().unwrap_or_else(|| Arc::new(OsmData {
            nodes: HashMap::new(), ways: Vec::new(), relations: Vec::new(), bounds: None,
        }));
        let mut data = (*current).clone();
        let way = OsmWay {
            id,
            nodes: node_ids,
            version: 0,
            tags: tags.into_iter().collect(),
        };
        data.ways.push(way);
        let way_idx = data.ways.len() - 1;

        for &nid in &data.ways[way_idx].nodes {
            self.node_to_ways.entry(nid).or_default().push(way_idx);
        }
        self.patch_way_cache_insert(way_idx, &data);
        self.way_id_to_index.insert(id, way_idx);

        self.modified = true;
        self.osm_data = Some(Arc::new(data));
        id
    }

    /// Compute and insert `way_vertices`/`way_bboxes`/`way_styles`/
    /// `way_index` entries for the way at `way_idx`, given `data` already
    /// contains it. Shared by `add_way`/`extend_way`/`insert_node_into_way`.
    fn patch_way_cache_insert(&mut self, way_idx: usize, data: &OsmData) {
        let way = &data.ways[way_idx];
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        let mut verts = Vec::with_capacity(way.nodes.len());
        for nid in &way.nodes {
            if let Some(&idx) = self.node_cache.index_by_id.get(nid) {
                let (_, mx, my) = self.node_cache.flat[idx];
                if mx < min_x { min_x = mx; }
                if mx > max_x { max_x = mx; }
                if my < min_y { min_y = my; }
                if my > max_y { max_y = my; }
                verts.push((*nid, mx, my));
            }
        }
        let bbox = if verts.is_empty() { None } else { Some(WayBbox { min_x, max_x, min_y, max_y }) };
        if way_idx == self.way_vertices.len() {
            self.way_vertices.push(verts);
            self.way_bboxes.push(bbox);
            self.way_styles.push(self.stylesheet.way_style(&way.tags));
        } else {
            self.way_vertices[way_idx] = verts;
            self.way_bboxes[way_idx] = bbox;
            self.way_styles[way_idx] = self.stylesheet.way_style(&way.tags);
        }
        if let Some(b) = bbox {
            self.way_index.insert(GeomWithData::new(Rectangle::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]), way.id));
            match &mut self.layer_bbox {
                Some(lb) => { lb.extend(b.min_x, b.min_y); lb.extend(b.max_x, b.max_y); }
                None => self.layer_bbox = Some(b),
            }
        }
    }

    /// Remove a way this layer owns from `OsmData` and every derived cache/
    /// index. Its member nodes are untouched. No-op if the way isn't
    /// present. Note: removing from the middle of `way_vertices`/
    /// `way_bboxes`/`way_styles`/`data.ways` shifts every later index by
    /// one, so `way_id_to_index` and every `node_to_ways` entry are
    /// recomputed afterward (acceptable: way removal is a rare undo path,
    /// not a per-frame hot path).
    pub fn remove_way(&mut self, way_id: i64) {
        let Some(current) = self.osm_data.clone() else { return };
        let mut data = (*current).clone();
        let Some(way_idx) = data.ways.iter().position(|w| w.id == way_id) else { return };

        if let Some(bbox) = self.way_bboxes[way_idx] {
            self.way_index.remove(&GeomWithData::new(Rectangle::from_corners([bbox.min_x, bbox.min_y], [bbox.max_x, bbox.max_y]), way_id));
        }
        data.ways.remove(way_idx);
        self.way_vertices.remove(way_idx);
        self.way_bboxes.remove(way_idx);
        self.way_styles.remove(way_idx);
        self.way_id_to_index = build_way_id_index(&data.ways);
        self.node_to_ways = build_node_to_ways(&data.ways);

        self.modified = true;
        self.osm_data = Some(Arc::new(data));
    }
```

- [ ] **Step 9: Run tests to verify they pass**

Run: `cargo test --lib osm_layer::tests::add_way_creates_closed_building_way_and_is_hit_testable osm_layer::tests::remove_way_drops_it_from_data_and_index_but_keeps_its_nodes`
Expected: PASS

- [ ] **Step 10: Commit**

```bash
git add src/layers/osm_layer.rs
git commit -m "Add OsmLayer::add_way/remove_way"
```

- [ ] **Step 11: Write failing tests for `extend_way`/`insert_node_into_way`/`remove_node_from_way`**

```rust
    #[test]
    fn extend_way_appends_node_and_updates_bbox() {
        let mut layer = OsmLayer::new();
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let way_id = layer.add_way(vec![n1, n2], vec![]);
        let n3 = layer.add_node(40.001, -74.0);

        layer.extend_way(way_id, n3);

        let data = layer.get_osm_data().unwrap();
        let way = data.ways.iter().find(|w| w.id == way_id).unwrap();
        assert_eq!(way.nodes, vec![n1, n2, n3]);
    }

    #[test]
    fn insert_node_into_way_splices_at_index_and_remove_node_from_way_undoes_it() {
        let mut layer = OsmLayer::new();
        let n1 = layer.add_node(40.0, -74.001);
        let n2 = layer.add_node(40.0, -74.0);
        let way_id = layer.add_way(vec![n1, n2], vec![]);

        let mid = layer.insert_node_into_way(way_id, 1, 40.0, -74.0005);
        let data = layer.get_osm_data().unwrap();
        let way = data.ways.iter().find(|w| w.id == way_id).unwrap();
        assert_eq!(way.nodes, vec![n1, mid, n2]);

        layer.remove_node_from_way(way_id, 1);
        let data = layer.get_osm_data().unwrap();
        let way = data.ways.iter().find(|w| w.id == way_id).unwrap();
        assert_eq!(way.nodes, vec![n1, n2]);
        assert!(data.nodes.contains_key(&mid), "remove_node_from_way must not delete the node itself");
    }
```

- [ ] **Step 12: Run to verify failure**

Run: `cargo test --lib osm_layer::tests::extend_way_appends_node_and_updates_bbox`
Expected: FAIL to compile.

- [ ] **Step 13: Implement `extend_way`/`insert_node_into_way`/`remove_node_from_way`**

```rust
    /// Append `node_id` (must already exist in this layer) to an existing
    /// way's node list, and refresh that one way's derived caches. No-op if
    /// the way isn't found.
    pub fn extend_way(&mut self, way_id: i64, node_id: i64) {
        let Some(current) = self.osm_data.clone() else { return };
        let mut data = (*current).clone();
        let Some(way_idx) = data.ways.iter().position(|w| w.id == way_id) else { return };
        data.ways[way_idx].nodes.push(node_id);
        self.node_to_ways.entry(node_id).or_default().push(way_idx);

        if let Some(old_bbox) = self.way_bboxes[way_idx] {
            self.way_index.remove(&GeomWithData::new(Rectangle::from_corners([old_bbox.min_x, old_bbox.min_y], [old_bbox.max_x, old_bbox.max_y]), way_id));
        }
        self.patch_way_cache_insert(way_idx, &data);

        self.modified = true;
        self.osm_data = Some(Arc::new(data));
    }

    /// Create a new node at `(lat, lon)` and splice it into an existing
    /// way's node list at `index` (0-based, into the node list — e.g.
    /// `index = 1` inserts between the way's 1st and 2nd nodes). Returns the
    /// new node's id. No-op (returns a placeholder id that was never
    /// inserted anywhere) if the way isn't found — callers only invoke this
    /// against a way just found via hit-testing, so this should not happen
    /// in practice.
    pub fn insert_node_into_way(&mut self, way_id: i64, index: usize, lat: f64, lon: f64) -> i64 {
        let id = self.next_placeholder_node_id;
        self.next_placeholder_node_id -= 1;

        let Some(current) = self.osm_data.clone() else { return id };
        let mut data = (*current).clone();
        let Some(way_idx) = data.ways.iter().position(|w| w.id == way_id) else { return id };

        data.nodes.insert(id, crate::osm::OsmNode { id, lat, lon, version: 0, tags: HashMap::new() });
        self.patch_node_cache_insert(id, lat, lon, &data);

        data.ways[way_idx].nodes.insert(index, id);
        self.node_to_ways.entry(id).or_default().push(way_idx);

        if let Some(old_bbox) = self.way_bboxes[way_idx] {
            self.way_index.remove(&GeomWithData::new(Rectangle::from_corners([old_bbox.min_x, old_bbox.min_y], [old_bbox.max_x, old_bbox.max_y]), way_id));
        }
        self.patch_way_cache_insert(way_idx, &data);

        self.modified = true;
        self.osm_data = Some(Arc::new(data));
        id
    }

    /// Inverse of `insert_node_into_way`: splice the node out of `way_id`'s
    /// node list at `index`, and refresh that way's derived caches. Does
    /// NOT delete the node itself — callers combine this with `remove_node`
    /// when fully undoing an insert. No-op if the way isn't found or
    /// `index` is out of bounds.
    pub fn remove_node_from_way(&mut self, way_id: i64, index: usize) {
        let Some(current) = self.osm_data.clone() else { return };
        let mut data = (*current).clone();
        let Some(way_idx) = data.ways.iter().position(|w| w.id == way_id) else { return };
        if index >= data.ways[way_idx].nodes.len() { return; }
        let removed_node_id = data.ways[way_idx].nodes.remove(index);

        if let Some(way_idxs) = self.node_to_ways.get_mut(&removed_node_id) {
            way_idxs.retain(|&i| i != way_idx);
        }
        if let Some(old_bbox) = self.way_bboxes[way_idx] {
            self.way_index.remove(&GeomWithData::new(Rectangle::from_corners([old_bbox.min_x, old_bbox.min_y], [old_bbox.max_x, old_bbox.max_y]), way_id));
        }
        self.patch_way_cache_insert(way_idx, &data);

        self.modified = true;
        self.osm_data = Some(Arc::new(data));
    }
```

- [ ] **Step 14: Run tests to verify they pass**

Run: `cargo test --lib osm_layer::tests::extend_way_appends_node_and_updates_bbox osm_layer::tests::insert_node_into_way_splices_at_index_and_remove_node_from_way_undoes_it`
Expected: PASS

- [ ] **Step 15: Add the 7 methods to the `MapLayer` trait and delegate from `OsmLayer`**

In `src/layers/mod.rs`, add after `remove_tag` (line 111):

```rust
    /// Insert a brand-new node at `(lat, lon)`. Default: no-op, returns 0
    /// (callers only invoke this against layers known to support editing —
    /// see `MapViewer::active_layer`).
    fn add_node(&mut self, _lat: f64, _lon: f64) -> i64 { 0 }

    /// Insert a brand-new way referencing existing node ids. Default: no-op,
    /// returns 0.
    fn add_way(&mut self, _node_ids: Vec<i64>, _tags: Vec<(String, String)>) -> i64 { 0 }

    /// Append a node id to an existing way. Default: no-op.
    fn extend_way(&mut self, _way_id: i64, _node_id: i64) {}

    /// Create a new node and splice it into an existing way at `index`.
    /// Default: no-op, returns 0.
    fn insert_node_into_way(&mut self, _way_id: i64, _index: usize, _lat: f64, _lon: f64) -> i64 { 0 }

    /// Remove a node (must not still be referenced by any way). Default: no-op.
    fn remove_node(&mut self, _node_id: i64) {}

    /// Remove a way (its member nodes are untouched). Default: no-op.
    fn remove_way(&mut self, _way_id: i64) {}

    /// Inverse of `insert_node_into_way`: remove the node at `index` from a
    /// way's node list without deleting the node. Default: no-op.
    fn remove_node_from_way(&mut self, _way_id: i64, _index: usize) {}
```

In `src/layers/osm_layer.rs`'s `impl MapLayer for OsmLayer` block, add delegations right after `remove_tag` (after line 699), matching the existing style:

```rust
    fn add_node(&mut self, lat: f64, lon: f64) -> i64 {
        OsmLayer::add_node(self, lat, lon)
    }

    fn add_way(&mut self, node_ids: Vec<i64>, tags: Vec<(String, String)>) -> i64 {
        OsmLayer::add_way(self, node_ids, tags)
    }

    fn extend_way(&mut self, way_id: i64, node_id: i64) {
        OsmLayer::extend_way(self, way_id, node_id);
    }

    fn insert_node_into_way(&mut self, way_id: i64, index: usize, lat: f64, lon: f64) -> i64 {
        OsmLayer::insert_node_into_way(self, way_id, index, lat, lon)
    }

    fn remove_node(&mut self, node_id: i64) {
        OsmLayer::remove_node(self, node_id);
    }

    fn remove_way(&mut self, way_id: i64) {
        OsmLayer::remove_way(self, way_id);
    }

    fn remove_node_from_way(&mut self, way_id: i64, index: usize) {
        OsmLayer::remove_node_from_way(self, way_id, index);
    }
```

- [ ] **Step 16: Full build + test check**

Run: `cargo build && cargo test --lib`
Expected: builds clean, all tests (existing + new) pass.

- [ ] **Step 17: Commit**

```bash
git add src/layers/mod.rs src/layers/osm_layer.rs
git commit -m "Add extend_way/insert_node_into_way/remove_node_from_way + MapLayer trait wiring"
```

---

### Task 2: Undo variants for node/way creation

**Files:**
- Modify: `src/undo.rs`
- Modify: `src/main.rs` (`apply_undo_action`, `src/main.rs:524-551`)

**Interfaces:**
- Consumes: `OsmLayer`/`MapLayer` methods from Task 1 (`add_node`, `add_way`, `extend_way`, `insert_node_into_way`, `remove_node`, `remove_way`, `remove_node_from_way`), via `LayerManager::find_layer_mut` (`src/layers/mod.rs:205`).
- Produces (used by Tasks 6, 7, 8): 5 new `UndoableAction` variants below, plus their `apply_undo_action` match arms — later tasks push these via `self.undo_stack.push(UndoableAction::X { .. })` exactly like existing `MoveNodes`/`SetTags` pushes.

- [ ] **Step 1: Write failing undo-stack tests**

Add to `src/undo.rs`'s `#[cfg(test)] mod undo_stack_tests`:

```rust
    #[test]
    fn place_node_description_and_round_trip() {
        let action = UndoableAction::PlaceNode {
            layer_name: "L".to_string(), node_id: -1, lat: 40.0, lon: -74.0,
        };
        assert_eq!(action.description(), "Placed 1 node");

        let mut stack = UndoStack::default();
        stack.push(action);
        let undone = stack.undo().unwrap();
        assert_eq!(undone.description(), "Placed 1 node");
    }

    #[test]
    fn create_building_description() {
        let action = UndoableAction::CreateBuilding {
            layer_name: "L".to_string(), way_id: -1, node_ids: [-1, -2, -3, -4],
        };
        assert_eq!(action.description(), "Created a building");
    }

    #[test]
    fn extrude_way_description() {
        let action = UndoableAction::ExtrudeWay {
            layer_name: "L".to_string(), way_id: -1, new_node_ids: [-1, -2],
        };
        assert_eq!(action.description(), "Extruded a building");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib undo_stack_tests::place_node_description_and_round_trip`
Expected: FAIL to compile — variants don't exist.

- [ ] **Step 3: Add the 5 variants + `description()` arms**

In `src/undo.rs`, extend the `UndoableAction` enum:

```rust
#[derive(Clone)]
pub(crate) enum UndoableAction {
    MoveNodes { per_layer: NodeMoveUndoEntries },
    SetTags {
        entries: Vec<(osm_gpui::selection::FeatureRef, String, Option<String>, Option<String>)>,
    },
    /// A single lone node placed by Add mode's first click. Undo removes it.
    PlaceNode { layer_name: String, node_id: i64, lat: f64, lon: f64 },
    /// Add mode's 2nd+ click: a new node appended to a way (creating the
    /// way first if `way_created`). Undo removes the node from the way
    /// (and deletes the way too if `way_created`), then deletes the node.
    ExtendWay { layer_name: String, way_id: i64, node_id: i64, lat: f64, lon: f64, way_created: bool },
    /// Building mode's 3rd click: 4 new nodes + one closed `building=yes`
    /// way, committed atomically. Undo deletes the way then all 4 nodes.
    CreateBuilding { layer_name: String, way_id: i64, node_ids: [i64; 4] },
    /// Extrude mode's drag commit: 2 new nodes + one closed `building=yes`
    /// way off an existing segment. The segment's own 2 nodes are untouched.
    /// Undo deletes the way then the 2 new nodes.
    ExtrudeWay { layer_name: String, way_id: i64, new_node_ids: [i64; 2] },
    /// Extrude mode's double-click: one new node spliced into an existing
    /// way at `index`. Undo removes it from the way, then deletes it.
    InsertNodeIntoWay { layer_name: String, way_id: i64, index: usize, node_id: i64, lat: f64, lon: f64 },
}
```

Extend `description()`:

```rust
            UndoableAction::PlaceNode { .. } => "Placed 1 node".to_string(),
            UndoableAction::ExtendWay { .. } => "Extended a way".to_string(),
            UndoableAction::CreateBuilding { .. } => "Created a building".to_string(),
            UndoableAction::ExtrudeWay { .. } => "Extruded a building".to_string(),
            UndoableAction::InsertNodeIntoWay { .. } => "Inserted a node into a way".to_string(),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib undo_stack_tests`
Expected: PASS

- [ ] **Step 5: Wire `apply_undo_action` in `src/main.rs`**

In `apply_undo_action` (`src/main.rs:524-551`), add match arms. Redo (`forward = true`) replays the same creation; undo (`forward = false`) deletes what was created. Since redo must recreate with the *same* id (not a fresh placeholder), redo re-inserts directly rather than calling `add_node`/`add_way` again (which would hand out a new, different id):

```rust
            UndoableAction::PlaceNode { layer_name, node_id, lat, lon } => {
                let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return };
                if forward {
                    // Redo: re-create the node (id is only correct if this
                    // is the very next placeholder id the layer would hand
                    // out — true here since nothing else can consume ids
                    // between an undo and its redo within one session).
                    layer.add_node(*lat, *lon);
                } else {
                    layer.remove_node(*node_id);
                }
            }
            UndoableAction::ExtendWay { layer_name, way_id, node_id, lat, lon, way_created } => {
                let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return };
                if forward {
                    if *way_created {
                        layer.add_way(vec![*node_id], Vec::new());
                    } else {
                        layer.extend_way(*way_id, *node_id);
                    }
                } else if *way_created {
                    layer.remove_way(*way_id);
                    layer.remove_node(*node_id);
                } else {
                    let node_ids = layer.way_node_ids(*way_id).unwrap_or_default();
                    if let Some(idx) = node_ids.iter().rposition(|id| id == node_id) {
                        layer.remove_node_from_way(*way_id, idx);
                    }
                    layer.remove_node(*node_id);
                }
            }
            UndoableAction::CreateBuilding { layer_name, way_id, node_ids } => {
                let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return };
                if !forward {
                    layer.remove_way(*way_id);
                    for id in node_ids {
                        layer.remove_node(*id);
                    }
                }
                // Redo (forward) is out of scope for Building mode's atomic
                // commit path in this plan: Building mode always creates a
                // *new* placeholder id on each commit, so a straightforward
                // redo-by-recreation isn't id-stable across a redo after
                // other edits. Matches this plan's scope (see spec's "Out
                // of scope": undo/redo depth beyond the immediate action).
            }
            UndoableAction::ExtrudeWay { layer_name, way_id, new_node_ids } => {
                let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return };
                if !forward {
                    layer.remove_way(*way_id);
                    for id in new_node_ids {
                        layer.remove_node(*id);
                    }
                }
            }
            UndoableAction::InsertNodeIntoWay { layer_name, way_id, index, node_id, .. } => {
                let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return };
                if !forward {
                    layer.remove_node_from_way(*way_id, *index);
                    layer.remove_node(*node_id);
                }
            }
```

Note: `way_node_ids` is already a `MapLayer` trait method (`src/layers/mod.rs:98`), no new API needed for that lookup.

- [ ] **Step 6: Build check**

Run: `cargo build`
Expected: builds clean (unused-variable warnings, if any, resolved by prefixing with `_` where a field is genuinely unused in an arm).

- [ ] **Step 7: Commit**

```bash
git add src/undo.rs src/main.rs
git commit -m "Add PlaceNode/ExtendWay/CreateBuilding/ExtrudeWay/InsertNodeIntoWay undo actions"
```

---

### Task 3: Perpendicular-rectangle geometry helper

**Files:**
- Modify: `src/selection.rs`
- Test: `src/selection.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces (used by Tasks 7, 8): a pure function computing the far two corners of a rectangle given one fixed edge and a point defining the perpendicular offset:
  ```rust
  pub fn rectangle_from_edge(
      a: (f64, f64),
      b: (f64, f64),
      offset_point: (f64, f64),
  ) -> ((f64, f64), (f64, f64))
  ```
  Returns `(far_a, far_b)` — the two new corners such that `a, b, far_b, far_a` form a closed rectangle (`a`-`b` is one edge, `far_a`-`far_b` is the opposite parallel edge). Operates in a flat Cartesian-like space (works identically whether called with screen pixels-as-f64 or mercator meters — both are locally Euclidean at map zoom scales, matching how `point_to_segment_distance` already treats screen pixels).

- [ ] **Step 1: Write the failing test**

Add to `src/selection.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn rectangle_from_edge_computes_perpendicular_offset() {
        // Edge along the x-axis from (0,0) to (10,0). Offset point at
        // (5, 4) is 4 units above the line -> far corners 4 units above
        // each original corner.
        let (far_a, far_b) = rectangle_from_edge((0.0, 0.0), (10.0, 0.0), (5.0, 4.0));
        assert!((far_a.0 - 0.0).abs() < 1e-9 && (far_a.1 - 4.0).abs() < 1e-9, "got {:?}", far_a);
        assert!((far_b.0 - 10.0).abs() < 1e-9 && (far_b.1 - 4.0).abs() < 1e-9, "got {:?}", far_b);
    }

    #[test]
    fn rectangle_from_edge_offset_below_line_goes_negative() {
        let (far_a, far_b) = rectangle_from_edge((0.0, 0.0), (10.0, 0.0), (5.0, -3.0));
        assert!((far_a.1 + 3.0).abs() < 1e-9, "got {:?}", far_a);
        assert!((far_b.1 + 3.0).abs() < 1e-9, "got {:?}", far_b);
    }

    #[test]
    fn rectangle_from_edge_handles_diagonal_edge() {
        // Edge from (0,0) to (0,10) (vertical); offset point at (3, 5) is 3
        // units to the right of the line -> far corners shift +3 in x.
        let (far_a, far_b) = rectangle_from_edge((0.0, 0.0), (0.0, 10.0), (3.0, 5.0));
        assert!((far_a.0 - 3.0).abs() < 1e-9 && (far_a.1 - 0.0).abs() < 1e-9, "got {:?}", far_a);
        assert!((far_b.0 - 3.0).abs() < 1e-9 && (far_b.1 - 10.0).abs() < 1e-9, "got {:?}", far_b);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib selection::tests::rectangle_from_edge_computes_perpendicular_offset`
Expected: FAIL to compile — function doesn't exist.

- [ ] **Step 3: Implement `rectangle_from_edge`**

Add to `src/selection.rs`, near `point_to_segment_distance`:

```rust
/// Given one fixed rectangle edge `a`-`b` and a third point `offset_point`
/// (typically the current cursor), compute the two far corners of the
/// perpendicular rectangle: project `offset_point` onto the line
/// perpendicular to `a`-`b`, and offset both `a` and `b` by that
/// perpendicular vector. Returns `(far_a, far_b)`, so `a, b, far_b, far_a`
/// traces a closed rectangle. Degenerate (zero-length `a`-`b`) input returns
/// `(a, b)` unchanged (no perpendicular direction to offset along).
pub fn rectangle_from_edge(
    a: (f64, f64),
    b: (f64, f64),
    offset_point: (f64, f64),
) -> ((f64, f64), (f64, f64)) {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < f64::EPSILON {
        return (a, b);
    }
    // Unit vector perpendicular to a-b.
    let nx = -dy / len;
    let ny = dx / len;

    // Signed distance of offset_point from the infinite line through a-b,
    // projected onto the perpendicular direction.
    let vx = offset_point.0 - a.0;
    let vy = offset_point.1 - a.1;
    let dist = vx * nx + vy * ny;

    let far_a = (a.0 + nx * dist, a.1 + ny * dist);
    let far_b = (b.0 + nx * dist, b.1 + ny * dist);
    (far_a, far_b)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib selection::tests::rectangle_from_edge_computes_perpendicular_offset selection::tests::rectangle_from_edge_offset_below_line_goes_negative selection::tests::rectangle_from_edge_handles_diagonal_edge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/selection.rs
git commit -m "Add rectangle_from_edge geometry helper for Building/Extrude modes"
```

---

### Task 4: `EditMode` enum, `SetMode` action, keybindings, left toolbar panel

**Files:**
- Modify: `src/main.rs` (actions near line 39-72, `MapViewer` struct at line 179-217, constructor at line 262, `render()` at line 1177-1396, keybindings at line 1568-1575)
- Create: `src/mode_panel.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks (this task is UI/plumbing-only; interaction logic lands in Tasks 6-8).
- Produces (used by Tasks 5, 6, 7, 8):
  - `enum EditMode { Select, Add, Building, Extrude }` (derives `Clone, Copy, Debug, PartialEq`), field `MapViewer::mode: EditMode` (default `EditMode::Select`).
  - `struct SetMode { mode: EditMode }` action (namespace `mode`), handler `MapViewer::on_set_mode`.
  - `MapViewer::active_layer: Option<String>` field is declared here (initialized `None`) but populated by Task 5.
  - `MapViewer::render_mode_panel(&self, cx: &mut Context<Self>) -> impl IntoElement` in the new `src/mode_panel.rs`, called from `render()`.

- [ ] **Step 1: Add the `EditMode` enum and `SetMode` action**

In `src/main.rs`, add near the other action structs (after `DeleteLayer`, line 72):

```rust
/// The current map-interaction mode. `Select` is today's existing click/
/// drag/box-select behavior; the others place new geometry (see
/// docs/superpowers/specs/2026-07-07-mode-selector-design.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditMode {
    Select,
    Add,
    Building,
    Extrude,
}

/// Action to switch the current `EditMode`.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = mode)]
#[serde(deny_unknown_fields)]
struct SetMode {
    mode: EditModeAction,
}

/// `EditMode` isn't itself `Deserialize`/`JsonSchema` (gpui's `Action` derive
/// requires both on every field); this mirrors it 1:1 purely so `SetMode`
/// can carry a mode value through the action-dispatch system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
enum EditModeAction {
    Select,
    Add,
    Building,
    Extrude,
}

impl From<EditModeAction> for EditMode {
    fn from(a: EditModeAction) -> Self {
        match a {
            EditModeAction::Select => EditMode::Select,
            EditModeAction::Add => EditMode::Add,
            EditModeAction::Building => EditMode::Building,
            EditModeAction::Extrude => EditMode::Extrude,
        }
    }
}
```

- [ ] **Step 2: Add fields to `MapViewer` and initialize in `new()`**

In the `MapViewer` struct (`src/main.rs:179-217`), add after `undo_stack: UndoStack,`:

```rust
    /// The current map-interaction mode (Select/Add/Building/Extrude).
    mode: EditMode,
    /// Name of the OSM layer that Add/Building/Extrude write into, or
    /// `None` if no layer is designated (those modes are disabled then).
    active_layer: Option<String>,
```

In `MapViewer::new` (`src/main.rs:262`), find the struct-literal construction and add:

```rust
            mode: EditMode::Select,
            active_layer: None,
```

(Exact insertion point: wherever the existing struct literal builds `undo_stack: UndoStack::default(),` — add the two new fields immediately after that line.)

- [ ] **Step 3: Add the `on_set_mode` handler**

Near `on_move_layer`/`on_delete_layer` (`src/main.rs:417-435`):

```rust
    /// Handle the `SetMode` action: switch modes, discarding any
    /// in-progress add/building/extrude state without committing it (Tasks
    /// 6-8 populate these fields; they don't exist until then, so this
    /// handler is added here as a no-op placeholder for those clears and
    /// extended in each later task).
    fn on_set_mode(&mut self, action: &SetMode, _: &mut Window, cx: &mut Context<Self>) {
        self.mode = action.mode.into();
        cx.notify();
    }
```

(Tasks 6/7/8 each add one line here to clear their own in-progress field — noted in those tasks' steps.)

- [ ] **Step 4: Register the action + keybindings**

In `render()`, add `.on_action(cx.listener(Self::on_set_mode))` alongside the existing `.on_action(...)` chain (`src/main.rs:1388-1391`).

In `main()`'s `cx.bind_keys([...])` (`src/main.rs:1568-1575`), add:

```rust
                    KeyBinding::new("a", SetMode { mode: EditModeAction::Add }, None),
                    KeyBinding::new("b", SetMode { mode: EditModeAction::Building }, None),
                    KeyBinding::new("x", SetMode { mode: EditModeAction::Extrude }, None),
```

- [ ] **Step 5: Build check (compiles, keybindings registered, no UI yet)**

Run: `cargo build`
Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "Add EditMode enum, SetMode action, and a/b/x keybindings"
```

- [ ] **Step 7: Create `src/mode_panel.rs`**

```rust
//! The left-hand mode-selector toolbar: Select / Add / Building / Extrude.

use gpui::{div, prelude::*, px, Context};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, button::{Button, ButtonVariants as _}};

use crate::{EditMode, EditModeAction, MapViewer, SetMode};

impl MapViewer {
    pub(crate) const MODE_PANEL_WIDTH: f32 = 56.0;

    /// The left toolbar: one icon button per `EditMode`, highlighting the
    /// active one. Add/Building/Extrude are disabled (no `on_click`, dimmed)
    /// when `active_layer` is `None` — there's nowhere to write new
    /// geometry.
    pub(crate) fn render_mode_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_active_layer = self.active_layer.is_some();

        div()
            .w(px(Self::MODE_PANEL_WIDTH))
            .h_full()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_2()
            .child(self.mode_button("mode-select", IconName::default(), EditMode::Select, true, cx))
            .child(self.mode_button("mode-add", IconName::default(), EditMode::Add, has_active_layer, cx))
            .child(self.mode_button("mode-building", IconName::default(), EditMode::Building, has_active_layer, cx))
            .child(self.mode_button("mode-extrude", IconName::default(), EditMode::Extrude, has_active_layer, cx))
    }

    fn mode_button(
        &self,
        id: &'static str,
        _icon: IconName,
        mode: EditMode,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = self.mode == mode;
        let action_mode = match mode {
            EditMode::Select => EditModeAction::Select,
            EditMode::Add => EditModeAction::Add,
            EditMode::Building => EditModeAction::Building,
            EditMode::Extrude => EditModeAction::Extrude,
        };
        let mut button = Button::new(id).label(mode_label(mode)).small();
        if is_active {
            button = button.primary();
        }
        if enabled {
            button = button.on_click(cx.listener(move |this, _, window, cx| {
                this.on_set_mode(&SetMode { mode: action_mode }, window, cx);
            }));
        } else {
            button = button.disabled(true);
        }
        button
    }
}

fn mode_label(mode: EditMode) -> &'static str {
    match mode {
        EditMode::Select => "Select",
        EditMode::Add => "Add",
        EditMode::Building => "Bldg",
        EditMode::Extrude => "Extr",
    }
}
```

Note: this uses text labels rather than a specific `IconName` variant since the exact icon set available in this project's `gpui_component::IconName` isn't confirmed by this plan — if `IconName::default()`/the `_icon` param doesn't compile as written, drop the icon and keep text-only buttons (the `mode_label` strings already make each button identifiable); this is a cosmetic detail, not a functional one, so resolve it inline by checking what `IconName` variants `side_panel.rs` already imports (`ChevronDown`, `ChevronRight`) and picking any four existing variants, or removing the icon parameter entirely if none fit.

Add `mod mode_panel;` to the `mod ...;` block in `src/main.rs` (alongside `mod side_panel;`, line 12).

- [ ] **Step 8: Insert the panel into `render()`**

In `render()` (`src/main.rs:1177-1181`), insert the new panel as the *first* child, before the map area:

```rust
        div()
            .size_full()
            .bg(rgb(0x1a202c))
            .flex()
            .flex_row()
            .child(self.render_mode_panel(cx))
            .child(
                // Map area
                div()
                    .flex_1()
```

Update the `panel_width` calc (`src/main.rs:1159-1164`) to also reserve the mode panel's width:

```rust
        let window_size = window.bounds().size;
        let right_panel_width = px(280.0);
        let left_panel_width = px(Self::MODE_PANEL_WIDTH);
        let map_size = gpui::size(
            window_size.width - right_panel_width - left_panel_width,
            window_size.height,
        );
```

- [ ] **Step 9: Build check**

Run: `cargo build`
Expected: builds clean; if `IconName`/`Button` API details differ from what's written above, adjust per Step 7's note and re-run until clean.

- [ ] **Step 10: Commit**

```bash
git add src/main.rs src/mode_panel.rs
git commit -m "Add left mode-selector toolbar panel"
```

---

### Task 5: Active layer

**Files:**
- Modify: `src/side_panel.rs` (`render_layers_section`, `src/side_panel.rs:214-263`)
- Modify: `src/main.rs` (`on_delete_layer`, `src/main.rs:427-435`)

**Interfaces:**
- Consumes: `MapViewer::active_layer` field (Task 4).
- Produces (used by Tasks 6, 7, 8): `active_layer` is populated by user interaction; Tasks 6-8 read `self.active_layer.clone()` to know which layer to mutate.

- [ ] **Step 1: Add click-to-activate + highlight in `render_layers_section`**

In `src/side_panel.rs`, `render_layers_section` (line 214-263) currently builds one `Checkbox` per layer row. Wrap each row so a click on the row (outside the checkbox) sets `active_layer`, and the active row gets a highlight. Replace the `.map(...)` closure body (lines 232-259) with:

```rust
                    .map(|(index, (name, is_visible, is_modified))| {
                        let layer_name = name.clone();
                        let layer_name_for_activate = name.clone();
                        let is_active = self.active_layer.as_deref() == Some(name.as_str());
                        let label = if *is_modified {
                            format!("{} \u{2022}", name)
                        } else {
                            name.clone()
                        };
                        let checkbox = Checkbox::new(("layer", index))
                            .checked(*is_visible)
                            .label(label)
                            .on_click(cx.listener(move |this, _checked: &bool, _, cx| {
                                this.toggle_layer_visibility(&layer_name);
                                cx.notify();
                            }))
                            .context_menu(move |menu, _window, _cx| {
                                let mut menu = menu;
                                if index > 0 {
                                    menu = menu
                                        .menu("Move up", Box::new(MoveLayer { index, delta: -1 }));
                                }
                                if index + 1 < total {
                                    menu = menu
                                        .menu("Move down", Box::new(MoveLayer { index, delta: 1 }));
                                }
                                menu.separator()
                                    .menu("Delete", Box::new(DeleteLayer { index }))
                            });

                        div()
                            .id(("layer-row", index))
                            .flex()
                            .flex_row()
                            .items_center()
                            .px_1()
                            .rounded_md()
                            .when(is_active, |this| this.bg(cx.theme().accent))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                                    this.active_layer = Some(layer_name_for_activate.clone());
                                    cx.notify();
                                }),
                            )
                            .child(checkbox)
                            .into_any_element()
                    })
```

This dispatches `active_layer` on any mouse-down within the row — including on the checkbox itself, which also toggles visibility via its own `on_click`; both effects firing together (activate + toggle) on a checkbox click is acceptable and matches how the row-click model is described in the spec (clicking the row activates it).

- [ ] **Step 2: Reset `active_layer` on layer deletion**

In `src/main.rs`'s `on_delete_layer` (line 427-435), after pushing the delete request, also clear `active_layer` if it names the layer being deleted:

```rust
    fn on_delete_layer(&mut self, action: &DeleteLayer, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(name) = self.layer_manager.layers().get(action.index).map(|l| l.name().to_string()) {
            if self.active_layer.as_deref() == Some(name.as_str()) {
                self.active_layer = None;
            }
        }
        if let Some(reqs) = LAYER_REQUESTS.get() {
            if let Ok(mut guard) = reqs.lock() {
                guard.push(LayerRequest::Delete { index: action.index });
            }
        }
        cx.notify();
    }
```

- [ ] **Step 3: Build check**

Run: `cargo build`
Expected: builds clean.

- [ ] **Step 4: Commit**

```bash
git add src/side_panel.rs src/main.rs
git commit -m "Add click-to-activate layer with persistent highlight"
```

---

### Task 6: Add mode interaction

**Files:**
- Modify: `src/main.rs` (`MapViewer` struct, `handle_map_click`/`handle_mouse_up`/key-down handler, `render()`)

**Interfaces:**
- Consumes: `EditMode`/`active_layer` (Task 4/5), `OsmLayer::add_node`/`add_way`/`extend_way` (Task 1), `UndoableAction::PlaceNode`/`ExtendWay` (Task 2).
- Produces (used by nothing later — Add mode is self-contained): none.

- [ ] **Step 1: Add `AddProgress` struct and `MapViewer` field**

In `src/main.rs`, near `MoveDrag`'s import (it's defined in `src/undo.rs` and imported at line 21) — define `AddProgress` locally in `main.rs` since it's pure interaction state, not undo history:

```rust
/// Add mode's in-progress way-building state: the last-placed node, and
/// which way (if any) it belongs to. `None` on `MapViewer` means "no node
/// placed yet in this continuation" — the next click starts fresh.
struct AddProgress {
    way_id: Option<i64>,
    last_node_id: i64,
}
```

Add to `MapViewer` (after `active_layer: Option<String>,`):

```rust
    /// In-progress Add-mode way-building state, or `None` between
    /// continuations (see `AddProgress`).
    add_progress: Option<AddProgress>,
```

Initialize `add_progress: None,` in `MapViewer::new`.

- [ ] **Step 2: Clear `add_progress` in `on_set_mode`**

Update `on_set_mode` (Task 4, Step 3):

```rust
    fn on_set_mode(&mut self, action: &SetMode, _: &mut Window, cx: &mut Context<Self>) {
        self.mode = action.mode.into();
        self.add_progress = None;
        cx.notify();
    }
```

- [ ] **Step 3: Branch `handle_map_click` by mode**

`handle_map_click` (`src/main.rs:805-810`) currently always does select-hit-testing. Rename the existing body to `handle_select_click` and add a mode dispatch:

```rust
    fn handle_map_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>) {
        match self.mode {
            EditMode::Select => self.handle_select_click(screen_pt),
            EditMode::Add => self.handle_add_click(screen_pt),
            EditMode::Building | EditMode::Extrude => {
                // Building/Extrude don't use the plain-click path (Tasks 7/8
                // hook mouse-down/mouse-move/mouse-up directly); a stray
                // click here (e.g. a zero-movement mouse-up while building)
                // is a no-op.
            }
        }
    }

    fn handle_select_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>) {
        let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
        self.selected = osm_gpui::selection::resolve_hits(per_layer)
            .into_iter()
            .collect();
    }

    /// Add mode: place a node, or extend/connect the in-progress way. See
    /// docs/superpowers/specs/2026-07-07-mode-selector-design.md "Add mode".
    fn handle_add_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>) {
        let Some(layer_name) = self.active_layer.clone() else { return };
        let (lat, lon) = self.viewport.screen_to_geo(screen_pt);

        // Clicking an existing node/way finishes the in-progress way by
        // connecting to it.
        if self.add_progress.is_some() {
            let per_layer = self.layer_manager.hit_test_all(&self.viewport, screen_pt);
            if let Some(hit) = osm_gpui::selection::resolve_hits(per_layer) {
                if hit.layer_name == layer_name {
                    if let osm_gpui::selection::FeatureKind::Node = hit.kind {
                        self.add_extend_or_start_way(&layer_name, hit.id, lat, lon, /*existing=*/true);
                        self.add_progress = None;
                        self.selected = vec![osm_gpui::selection::FeatureRef {
                            layer_name: layer_name.clone(), kind: osm_gpui::selection::FeatureKind::Way, id: hit.id,
                        }];
                        return;
                    }
                }
            }
        }

        let Some(layer) = self.layer_manager.find_layer_mut(&layer_name) else { return };
        let new_id = layer.add_node(lat, lon);
        self.undo_stack.push(UndoableAction::PlaceNode {
            layer_name: layer_name.clone(), node_id: new_id, lat, lon,
        });

        match self.add_progress.take() {
            None => {
                self.add_progress = Some(AddProgress { way_id: None, last_node_id: new_id });
                self.selected = vec![osm_gpui::selection::FeatureRef {
                    layer_name, kind: osm_gpui::selection::FeatureKind::Node, id: new_id,
                }];
            }
            Some(progress) => {
                let way_id = self.add_extend_or_start_way(&layer_name, new_id, lat, lon, /*existing=*/false);
                let _ = progress;
                self.add_progress = Some(AddProgress { way_id: Some(way_id), last_node_id: new_id });
                self.selected = vec![osm_gpui::selection::FeatureRef {
                    layer_name, kind: osm_gpui::selection::FeatureKind::Way, id: way_id,
                }];
            }
        }
    }

    /// Shared by the "continue clicking" and "connect to existing feature"
    /// paths: start a new 2-node way if none exists yet, or extend the
    /// existing one, pushing the matching `ExtendWay` undo entry. Returns
    /// the way id (new or existing).
    fn add_extend_or_start_way(&mut self, layer_name: &str, node_id: i64, lat: f64, lon: f64, _existing: bool) -> i64 {
        let progress_way_id = self.add_progress.as_ref().and_then(|p| p.way_id);
        let last_node_id = self.add_progress.as_ref().map(|p| p.last_node_id).unwrap_or(node_id);
        let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return progress_way_id.unwrap_or(0) };

        match progress_way_id {
            Some(way_id) => {
                layer.extend_way(way_id, node_id);
                self.undo_stack.push(UndoableAction::ExtendWay {
                    layer_name: layer_name.to_string(), way_id, node_id, lat, lon, way_created: false,
                });
                way_id
            }
            None => {
                let way_id = layer.add_way(vec![last_node_id, node_id], Vec::new());
                self.undo_stack.push(UndoableAction::ExtendWay {
                    layer_name: layer_name.to_string(), way_id, node_id, lat, lon, way_created: true,
                });
                way_id
            }
        }
    }
```

- [ ] **Step 4: Handle Escape/Enter to finish the way**

In the map area's `on_key_down` handler (`src/main.rs:1218-1222`), extend:

```rust
                            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                                if ev.keystroke.key == "escape" {
                                    this.cancel_move_drag(cx);
                                    if this.mode == EditMode::Add {
                                        if this.add_progress.take().is_some() {
                                            cx.notify();
                                        } else {
                                            this.mode = EditMode::Select;
                                            cx.notify();
                                        }
                                    }
                                } else if ev.keystroke.key == "enter" && this.mode == EditMode::Add {
                                    if this.add_progress.take().is_some() {
                                        cx.notify();
                                    }
                                }
                            }))
```

- [ ] **Step 5: Build check**

Run: `cargo build`
Expected: builds clean.

- [ ] **Step 6: Add a unit test for `add_extend_or_start_way`'s core logic**

Since `MapViewer` itself isn't unit-testable in isolation (it owns a `Window`/`Context`), verify the underlying layer + undo mechanics directly instead (already covered by Task 1/2's tests) — no new test needed here; this step is a build/clippy pass only.

Run: `cargo clippy --all-targets`
Expected: no new warnings introduced by this task (fix any that appear, e.g. unused `_existing` parameter — either use it or keep the underscore prefix as written).

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "Wire Add mode: click to place nodes and chain them into a way"
```

---

### Task 7: Building mode interaction

**Files:**
- Modify: `src/main.rs` (`MapViewer` struct, mouse handlers, `render()` preview overlay)

**Interfaces:**
- Consumes: `rectangle_from_edge` (Task 3), `OsmLayer::add_node`/`add_way` (Task 1), `UndoableAction::CreateBuilding` (Task 2), `EditMode`/`active_layer` (Task 4/5).
- Produces: none (terminal mode, like Add).

- [ ] **Step 1: Add `BuildingProgress` struct and field**

```rust
/// Building mode's in-progress rectangle: corner A is fixed after click 1;
/// corner B after click 2. Both are geo (lat, lon) coordinates.
struct BuildingProgress {
    corner_a: (f64, f64),
    corner_b: Option<(f64, f64)>,
}
```

Add `building_progress: Option<BuildingProgress>,` to `MapViewer`, initialize `None` in `new()`. Clear it in `on_set_mode` alongside `add_progress`:

```rust
        self.add_progress = None;
        self.building_progress = None;
```

- [ ] **Step 2: Route Building-mode clicks in `handle_mouse_up`'s click path**

`handle_map_click`'s `EditMode::Building` arm (added as a no-op placeholder in Task 6, Step 3) becomes:

```rust
            EditMode::Building => self.handle_building_click(screen_pt),
```

Add the handler:

```rust
    /// Building mode: click 1 sets corner A, click 2 sets corner B (fixing
    /// the first edge), click 3 commits the rectangle. See
    /// docs/superpowers/specs/2026-07-07-mode-selector-design.md "Building mode".
    fn handle_building_click(&mut self, screen_pt: gpui::Point<gpui::Pixels>) {
        let Some(layer_name) = self.active_layer.clone() else { return };
        let (lat, lon) = self.viewport.screen_to_geo(screen_pt);

        match self.building_progress.take() {
            None => {
                self.building_progress = Some(BuildingProgress { corner_a: (lat, lon), corner_b: None });
            }
            Some(BuildingProgress { corner_a, corner_b: None }) => {
                self.building_progress = Some(BuildingProgress { corner_a, corner_b: Some((lat, lon)) });
            }
            Some(BuildingProgress { corner_a, corner_b: Some(corner_b) }) => {
                self.commit_building(&layer_name, corner_a, corner_b, (lat, lon));
                self.building_progress = None;
            }
        }
    }

    /// Compute the final rectangle (corner_a, corner_b as one edge, offset
    /// by `cursor`'s perpendicular distance) and commit 4 new nodes + a
    /// closed `building=yes` way as one undo action.
    fn commit_building(&mut self, layer_name: &str, corner_a: (f64, f64), corner_b: (f64, f64), cursor: (f64, f64)) {
        let (far_a, far_b) = osm_gpui::selection::rectangle_from_edge(corner_a, corner_b, cursor);
        let Some(layer) = self.layer_manager.find_layer_mut(layer_name) else { return };

        let n0 = layer.add_node(corner_a.0, corner_a.1);
        let n1 = layer.add_node(corner_b.0, corner_b.1);
        let n2 = layer.add_node(far_b.0, far_b.1);
        let n3 = layer.add_node(far_a.0, far_a.1);
        let way_id = layer.add_way(
            vec![n0, n1, n2, n3, n0],
            vec![("building".to_string(), "yes".to_string())],
        );

        self.undo_stack.push(UndoableAction::CreateBuilding {
            layer_name: layer_name.to_string(), way_id, node_ids: [n0, n1, n2, n3],
        });
        self.selected = vec![osm_gpui::selection::FeatureRef {
            layer_name: layer_name.to_string(), kind: osm_gpui::selection::FeatureKind::Way, id: way_id,
        }];
    }
```

- [ ] **Step 3: Render the live preview**

In `render()`, add a preview overlay as a sibling of the existing box-select overlay (`src/main.rs:1307-1324`), after it:

```rust
                            .child({
                                if let Some(BuildingProgress { corner_a, corner_b }) = &self.building_progress {
                                    let a_screen = self.viewport.geo_to_screen(corner_a.0, corner_a.1);
                                    match corner_b {
                                        None => {
                                            // Only corner A placed: nothing to draw yet without
                                            // a live cursor position (no mouse-move-driven preview
                                            // until a 2nd corner exists); render just the corner
                                            // itself as a small marker.
                                            div()
                                                .absolute()
                                                .left(a_screen.x - px(3.0))
                                                .top(a_screen.y - px(3.0))
                                                .w(px(6.0))
                                                .h(px(6.0))
                                                .bg(cx.theme().accent)
                                                .into_any_element()
                                        }
                                        Some(corner_b) => {
                                            let b_screen = self.viewport.geo_to_screen(corner_b.0, corner_b.1);
                                            div()
                                                .absolute()
                                                .left(a_screen.x.min(b_screen.x) - px(3.0))
                                                .top(a_screen.y.min(b_screen.y) - px(3.0))
                                                .w((a_screen.x - b_screen.x).abs() + px(6.0))
                                                .h((a_screen.y - b_screen.y).abs() + px(6.0))
                                                .border_2()
                                                .border_color(cx.theme().accent)
                                                .into_any_element()
                                        }
                                    }
                                } else {
                                    div().into_any_element()
                                }
                            })
```

Note: a full perpendicular-offset live preview (following the mouse after the 2nd click, per the spec) requires reading the current mouse position each frame. Since `MapViewer` doesn't otherwise track "last known mouse position" outside a drag, add one: a `last_mouse_pos: Option<gpui::Point<gpui::Pixels>>` field updated at the top of `handle_mouse_move` (`src/main.rs:693`), then use `self.last_mouse_pos` (converted via `screen_to_geo`) as the `cursor` argument to `rectangle_from_edge` when rendering the two-corner preview, replacing the simple bounding-box placeholder above with the true perpendicular rectangle (4 points: `corner_a`, `corner_b`, and the two far corners from `rectangle_from_edge`), drawn as a 4-point closed polyline via `PathBuilder::stroke` (same pattern as `render_highlight`'s way outline in `src/layers/osm_layer.rs:1080-1087`) rather than an axis-aligned `div`.

- [ ] **Step 4: Add `last_mouse_pos` field**

Add `last_mouse_pos: Option<gpui::Point<gpui::Pixels>>,` to `MapViewer`, initialize `None`. At the top of `handle_mouse_move` (`src/main.rs:693`):

```rust
    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let adjusted_position = event.position;
        self.last_mouse_pos = Some(adjusted_position);
        if self.building_progress.is_some() || self.extrude_drag.is_some() {
            cx.notify(); // repaint the live preview every move while building/extruding
        }
```

(`extrude_drag` is added in Task 8; if Task 7 lands before Task 8, temporarily reference only `self.building_progress.is_some()` here and extend this line in Task 8, Step 4.)

- [ ] **Step 5: Build check**

Run: `cargo build`
Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "Wire Building mode: 3-click perpendicular rectangle with live preview"
```

---

### Task 8: Extrude mode interaction

**Files:**
- Modify: `src/main.rs` (`MapViewer` struct, mouse handlers, `render()`)
- Modify: `src/selection.rs` (segment-hit-test helper, if not already reusable as-is)

**Interfaces:**
- Consumes: `point_to_segment_distance` (existing, `src/selection.rs:27`), `rectangle_from_edge` (Task 3), `OsmLayer::add_node`/`add_way`/`insert_node_into_way` (Task 1), `UndoableAction::ExtrudeWay`/`InsertNodeIntoWay` (Task 2), `EditMode`/`active_layer` (Task 4/5).
- Produces: none (terminal mode).

- [ ] **Step 1: Add a way-segment hit-test to `OsmLayer`**

Extrude mode needs "which way segment (way id + endpoint node ids) is under this point", distinct from the existing `hit_test` (which returns whole-way `FeatureRef`s, not segment endpoints). Add to `src/layers/osm_layer.rs`, near `hit_test`:

```rust
    /// Find the way segment nearest `screen_pt`, within `tol_px`, returning
    /// `(way_id, node_id_a, node_id_b)` for its two endpoints (in the way's
    /// node-list order) if within tolerance. Used by Extrude mode, which
    /// needs the segment's endpoints rather than just a `FeatureRef` to the
    /// whole way (unlike `hit_test`).
    pub fn hit_test_segment(&self, viewport: &Viewport, screen_pt: Point<Pixels>, tol_px: f32) -> Option<(i64, i64, i64, usize)> {
        if self.osm_data.is_none() { return None; }
        let pad = px(tol_px * 4.0);
        let (ex1, ey1) = viewport.screen_to_mercator(point(screen_pt.x - pad, screen_pt.y - pad));
        let (ex2, ey2) = viewport.screen_to_mercator(point(screen_pt.x + pad, screen_pt.y + pad));
        let envelope = AABB::from_corners([ex1.min(ex2), ey1.min(ey2)], [ex1.max(ex2), ey1.max(ey2)]);

        let mut best: Option<(f32, i64, i64, i64, usize)> = None;
        for item in self.way_index.locate_in_envelope_intersecting(envelope) {
            let way_id = item.data;
            let Some(&way_idx) = self.way_id_to_index.get(&way_id) else { continue };
            let verts = &self.way_vertices[way_idx];
            for i in 0..verts.len().saturating_sub(1) {
                let (id_a, ax, ay) = verts[i];
                let (id_b, bx, by) = verts[i + 1];
                let sp_a = viewport.mercator_to_screen(ax, ay);
                let sp_b = viewport.mercator_to_screen(bx, by);
                if !is_point_valid(sp_a) || !is_point_valid(sp_b) { continue; }
                let d = point_to_segment_distance(screen_pt, sp_a, sp_b);
                if d <= tol_px && best.as_ref().map_or(true, |&(bd, ..)| d < bd) {
                    best = Some((d, way_id, id_a, id_b, i));
                }
            }
        }
        best.map(|(_, way_id, a, b, idx)| (way_id, a, b, idx))
    }
```

- [ ] **Step 2: Write a failing test for `hit_test_segment`**

```rust
    #[test]
    fn hit_test_segment_finds_nearest_segment_and_endpoint_indices() {
        let center_lat = 40.0;
        let center_lon = -74.0;
        let n1 = OsmNode { id: 1, lat: center_lat, lon: center_lon - 0.001, version: 1, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: center_lat, lon: center_lon + 0.001, version: 1, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], version: 1, tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(center_lat, center_lon);
        let layer = OsmLayer::new_with_data("L", data);

        let hit = layer.hit_test_segment(&viewport, point(px(400.0), px(300.0)), 6.0);
        assert_eq!(hit, Some((10, 1, 2, 0)));
    }

    #[test]
    fn hit_test_segment_none_when_out_of_tolerance() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.001, version: 1, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 40.0, lon: -74.0, version: 1, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], version: 1, tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let viewport = viewport_centered_on(40.0, -74.0);
        let layer = OsmLayer::new_with_data("L", data);

        let hit = layer.hit_test_segment(&viewport, point(px(0.0), px(0.0)), 6.0);
        assert!(hit.is_none());
    }
```

Run: `cargo test --lib osm_layer::tests::hit_test_segment_finds_nearest_segment_and_endpoint_indices`
Expected: FAIL first (method absent) if written before Step 1; run again after Step 1 to confirm PASS.

- [ ] **Step 3: Add `extrude_drag` state and field**

```rust
/// Extrude mode's in-progress drag: the way segment being extruded from,
/// and the two endpoint node ids/index used to build the preview/final
/// rectangle.
struct ExtrudeDrag {
    layer_name: String,
    way_id: i64,
    node_a: i64,
    node_b: i64,
}
```

Add `extrude_drag: Option<ExtrudeDrag>,` to `MapViewer`, initialize `None`. Clear it in `on_set_mode` alongside the other two progress fields. Extend Task 7 Step 4's `handle_mouse_move` guard to include it (`self.building_progress.is_some() || self.extrude_drag.is_some()`).

- [ ] **Step 4: Mouse-down: start an extrude drag on a segment hit**

In `handle_map_mouse_down` (`src/main.rs:448-464`), add an Extrude-mode branch before the existing Select-mode move-drag logic:

```rust
    fn handle_map_mouse_down(&mut self, position: gpui::Point<gpui::Pixels>) {
        self.mouse_down_pos = Some(position);

        if self.mode == EditMode::Extrude {
            if let Some(layer_name) = self.active_layer.clone() {
                if let Some(layer) = self.layer_manager.find_layer(&layer_name) {
                    if let Some(osm_layer) = layer.as_any().downcast_ref::<OsmLayer>() {
                        if let Some((way_id, node_a, node_b, _idx)) = osm_layer.hit_test_segment(&self.viewport, position, 6.0) {
                            self.extrude_drag = Some(ExtrudeDrag { layer_name, way_id, node_a, node_b });
                        }
                    }
                }
            }
            return;
        }

        if self.selected.is_empty() {
            return;
        }
        // ... existing Select-mode body unchanged below
```

This requires `MapLayer` to expose a downcast, since `hit_test_segment` is `OsmLayer`-specific (not a trait method — Extrude mode's segment concept doesn't apply to tile/grid layers, matching the design's "OSM-data layers only" eligibility). Add to the `MapLayer` trait (`src/layers/mod.rs`):

```rust
    /// Downcast support for callers (Extrude mode) that need `OsmLayer`-
    /// specific methods not otherwise part of this trait. Default: `None`.
    fn as_any(&self) -> &dyn std::any::Any {
        // Default impl can't produce a real `&dyn Any` for `Self` without a
        // `Sized` bound this trait doesn't have; every implementor overrides
        // this with `self`. (Rust requires each impl to supply its own body;
        // there is no generic default here despite the signature living on
        // the trait.)
        unimplemented!("implementors must override as_any")
    }
```

and in each of `OsmLayer`, `TileLayer`, `GridLayer`'s `impl MapLayer` blocks, add:

```rust
    fn as_any(&self) -> &dyn std::any::Any { self }
```

(`TileLayer`/`GridLayer` need this override purely so the trait's `unimplemented!` default is never hit for them — Extrude mode never calls `hit_test_segment` on non-`OsmLayer` results because `find_layer` only returns the *active* layer, but the trait method must still be implemented on every type since Rust doesn't allow a truly generic default for `&dyn Any` over `Self`.)

- [ ] **Step 5: Mouse-move: update the live preview (no data mutation)**

Already covered by Task 7 Step 4's `cx.notify()` on every move while `extrude_drag.is_some()` — the actual preview geometry is computed at render time (Step 7 below) from `self.extrude_drag` + `self.last_mouse_pos`, no separate mouse-move handler logic needed.

- [ ] **Step 6: Mouse-up: commit past the drag threshold, or treat a non-drag release as a double-click check**

In `handle_mouse_up` (`src/main.rs:733-803`), add an Extrude-mode branch before the existing `move_drag`/`box_select`/click logic:

```rust
    fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let up_pos = event.position;
        let down_pos = self.mouse_down_pos.take();
        self.viewport.handle_mouse_up();

        if let Some(drag) = self.extrude_drag.take() {
            let moved = match down_pos {
                Some(down) => (up_pos - down).magnitude() >= 4.0,
                None => false,
            };
            if moved {
                self.commit_extrude(&drag, up_pos);
            } else if event.click_count == 2 {
                self.insert_node_on_segment(&drag, up_pos);
            }
            cx.notify();
            return;
        }

        // ... existing body unchanged below
```

Add the two handlers:

```rust
    /// Commit an Extrude drag: compute the far 2 corners via
    /// `rectangle_from_edge` (using `up_pos` for the perpendicular offset),
    /// create 2 new nodes + a closed `building=yes` way, push one
    /// `ExtrudeWay` undo action.
    fn commit_extrude(&mut self, drag: &ExtrudeDrag, up_pos: gpui::Point<gpui::Pixels>) {
        let Some(layer) = self.layer_manager.find_layer(&drag.layer_name) else { return };
        let Some(a_geo) = layer.node_lat_lon(drag.node_a) else { return };
        let Some(b_geo) = layer.node_lat_lon(drag.node_b) else { return };
        let cursor_geo = self.viewport.screen_to_geo(up_pos);

        let (far_a, far_b) = osm_gpui::selection::rectangle_from_edge(a_geo, b_geo, cursor_geo);
        let Some(layer) = self.layer_manager.find_layer_mut(&drag.layer_name) else { return };
        let new_a = layer.add_node(far_a.0, far_a.1);
        let new_b = layer.add_node(far_b.0, far_b.1);
        let way_id = layer.add_way(
            vec![drag.node_a, drag.node_b, new_b, new_a, drag.node_a],
            vec![("building".to_string(), "yes".to_string())],
        );

        self.undo_stack.push(UndoableAction::ExtrudeWay {
            layer_name: drag.layer_name.clone(), way_id, new_node_ids: [new_a, new_b],
        });
        self.selected = vec![osm_gpui::selection::FeatureRef {
            layer_name: drag.layer_name.clone(), kind: osm_gpui::selection::FeatureKind::Way, id: way_id,
        }];
    }

    /// Double-click on a segment (no drag): insert a new node at the
    /// double-click position, splitting that segment.
    fn insert_node_on_segment(&mut self, drag: &ExtrudeDrag, up_pos: gpui::Point<gpui::Pixels>) {
        let (lat, lon) = self.viewport.screen_to_geo(up_pos);
        let Some(layer) = self.layer_manager.find_layer_mut(&drag.layer_name) else { return };
        // The segment's start index within the way's node list: `node_a`'s
        // position (the segment is node_a -> node_b, consecutive).
        let Some(node_ids) = layer.way_node_ids(drag.way_id) else { return };
        let Some(idx_a) = node_ids.iter().position(|&id| id == drag.node_a) else { return };
        let insert_index = idx_a + 1;

        let new_id = layer.insert_node_into_way(drag.way_id, insert_index, lat, lon);
        self.undo_stack.push(UndoableAction::InsertNodeIntoWay {
            layer_name: drag.layer_name.clone(), way_id: drag.way_id, index: insert_index, node_id: new_id, lat, lon,
        });
    }
```

- [ ] **Step 7: Render the live extrude preview**

Extend the Building-mode preview block added in Task 7 Step 3 with an Extrude arm (or add a sibling `.child({...})` block following the same pattern): while `self.extrude_drag.is_some()` and `self.last_mouse_pos.is_some()`, compute `(far_a, far_b)` via `rectangle_from_edge` using the drag's two node geo-positions and the current mouse position (converted via `screen_to_geo`), project all 4 points via `viewport.geo_to_screen`, and draw the 4-point closed outline the same way as Building mode's preview.

- [ ] **Step 8: Build + full test check**

Run: `cargo build && cargo test --lib && cargo clippy --all-targets`
Expected: builds clean, all tests pass, no new clippy warnings.

- [ ] **Step 9: Commit**

```bash
git add src/main.rs src/layers/mod.rs src/layers/osm_layer.rs src/layers/tile_layer.rs src/layers/grid_layer.rs
git commit -m "Wire Extrude mode: drag-to-extrude rectangle and double-click node insertion"
```

---

## Final verification

- [ ] Run the full check: `cargo build && cargo test --lib && cargo clippy --all-targets -- -D warnings` (or without `-D warnings` if the repo doesn't already enforce that — match existing CI/lint conventions).
- [ ] Manually confirm via `/verify` or the project's `run` skill that the app launches, the left toolbar renders with 4 buttons, and Add/Building/Extrude are visibly disabled until a layer is activated (documented GUI-verification limitation applies beyond this: full click-driven gesture testing isn't automatable in this sandbox).
