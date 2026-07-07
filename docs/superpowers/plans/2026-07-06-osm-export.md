# OSM XML Export Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a **File > Export** (⌘E) command that writes the current OSM layer's data to a JOSM-compatible `.osm` XML file, with `action="modify"`/`action="delete"` markers on touched elements, laying the groundwork for a future changeset upload.

**Architecture:** Add per-element dirty-tracking fields to `OsmLayer` (extending the existing `modified: bool`), a new pure `src/osm_export.rs` module that serializes `OsmData` + those dirty marks to OSM XML via `quick-xml`, a `MapLayer` trait default method `export_xml()` (dyn-dispatch, mirroring the existing `commit_node_moves` pattern) so `main.rs` never has to downcast, and a `File > Export...` menu item wired the same way `File > Open...` already is.

**Tech Stack:** Rust, `quick-xml` (already a dependency via `osm.rs`), `rfd` (already a dependency via the Open dialog), gpui `actions!`/`Menu`/`KeyBinding`.

Design reference: `docs/superpowers/specs/2026-07-06-osm-export-design.md`.

## Global Constraints

- Single-line git commit messages, no `Co-Authored-By` trailer.
- `cargo build`, `cargo clippy`, `cargo test` must stay clean/green after every task.
- Do not touch dead files: `src/map.rs`, `src/data.rs`, `src/background.rs`, `src/mercator.rs`, `src/http_image_loader.rs` (separate housekeeping plan handles these).
- No GUI automation is available in this environment — menu wiring (Task 3) is verified by build + a manual spot-check note for the human reviewer, not a live click-through.
- Root XML element for export: `<osm version="0.6" generator="osm-gpui">`.

---

### Task 1: Per-element dirty-tracking fields on `OsmLayer`

**Files:**
- Modify: `src/layers/osm_layer.rs`

**Interfaces:**
- Produces: new private fields `modified_node_ids: HashSet<i64>`, `modified_way_ids: HashSet<i64>`, `deleted_node_ids: HashSet<i64>`, `deleted_way_ids: HashSet<i64>`, `new_node_ids: HashSet<i64>`, `new_way_ids: HashSet<i64>`, `next_new_id: i64` on `OsmLayer`, plus `pub fn edit_marks(&self) -> osm_export::EditMarks<'_>`. Task 2 defines `EditMarks`; Task 3 calls `edit_marks()`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `src/layers/osm_layer.rs` (near `commit_node_moves_updates_data_and_marks_modified`):

```rust
    #[test]
    fn commit_node_moves_tracks_modified_node_ids() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 41.0, lon: -75.0, tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.commit_node_moves(&[(1, 40.5, -74.5)]);

        let marks = layer.edit_marks();
        assert!(marks.modified_nodes.contains(&1));
        assert!(!marks.modified_nodes.contains(&2));
        assert!(marks.deleted_nodes.is_empty());
        assert!(marks.new_nodes.is_empty());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib commit_node_moves_tracks_modified_node_ids`
Expected: FAIL — `edit_marks` does not exist yet (compile error).

- [ ] **Step 3: Add the fields, initializers, and accessor**

In the `OsmLayer` struct definition, after the existing `modified: bool,` field:

```rust
    modified_node_ids: HashSet<i64>,
    modified_way_ids: HashSet<i64>,
    deleted_node_ids: HashSet<i64>,
    deleted_way_ids: HashSet<i64>,
    new_node_ids: HashSet<i64>,
    new_way_ids: HashSet<i64>,
    next_new_id: i64,
```

In both `OsmLayer::new()` and `OsmLayer::new_with_data()`, after the existing `modified: false,` line in each struct literal:

```rust
            modified_node_ids: HashSet::new(),
            modified_way_ids: HashSet::new(),
            deleted_node_ids: HashSet::new(),
            deleted_way_ids: HashSet::new(),
            new_node_ids: HashSet::new(),
            new_way_ids: HashSet::new(),
            next_new_id: -1,
```

In `commit_node_moves`, inside the `for &(id, lat, lon) in moves` loop, after `node.lat = lat; node.lon = lon;`:

```rust
            self.modified_node_ids.insert(id);
```

Add the accessor method (near `commit_node_moves`):

```rust
    /// Borrowing view of this layer's per-element dirty-tracking, for
    /// `osm_export::to_osm_xml`.
    pub fn edit_marks(&self) -> crate::osm_export::EditMarks<'_> {
        crate::osm_export::EditMarks {
            modified_nodes: &self.modified_node_ids,
            modified_ways: &self.modified_way_ids,
            deleted_nodes: &self.deleted_node_ids,
            deleted_ways: &self.deleted_way_ids,
            new_nodes: &self.new_node_ids,
            new_ways: &self.new_way_ids,
        }
    }
```

This creates a forward reference to `crate::osm_export`, which Task 2 creates. Task 1's own test only needs the struct fields, so it will compile once Task 2 lands; do Task 1 and Task 2 as one commit if your toolchain won't compile with the dangling module reference (see Step 4).

- [ ] **Step 4: Run test to verify it passes**

This step depends on `src/osm_export.rs` existing (Task 2). If doing these tasks in strict order, stub `src/osm_export.rs` now with just the `EditMarks` struct (no `to_osm_xml` yet) so this compiles:

```rust
use std::collections::HashSet;

#[derive(Default)]
pub struct EditMarks<'a> {
    pub modified_nodes: &'a HashSet<i64>,
    pub modified_ways: &'a HashSet<i64>,
    pub deleted_nodes: &'a HashSet<i64>,
    pub deleted_ways: &'a HashSet<i64>,
    pub new_nodes: &'a HashSet<i64>,
    pub new_ways: &'a HashSet<i64>,
}
```

Add `pub mod osm_export;` to `src/lib.rs` alongside the other `pub mod` declarations.

Run: `cargo test --lib commit_node_moves_tracks_modified_node_ids`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/layers/osm_layer.rs src/osm_export.rs src/lib.rs
git commit -m "Track per-element modified ids on OsmLayer"
```

---

### Task 2: `to_osm_xml` serializer (TDD, pure)

**Files:**
- Modify: `src/osm_export.rs` (created as a stub in Task 1)
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `osm_gpui::osm::{OsmData, OsmNode, OsmWay, OsmRelation, OsmMember}` (all fields public, see `src/osm.rs`), `EditMarks` from Task 1.
- Produces: `pub fn to_osm_xml(data: &OsmData, marks: &EditMarks) -> String`. Task 3 calls this via `OsmLayer::export_xml`.

- [ ] **Step 1: Write the failing tests**

Replace the stub content of `src/osm_export.rs` with:

```rust
//! Serializes `OsmData` back to OSM XML, marking dirty elements for a
//! future changeset upload. Produces a JOSM-compatible `.osm` save file:
//! the full dataset, not a diff, with `action="modify"`/`action="delete"`
//! attributes on touched elements.

use crate::osm::OsmData;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Writer;
use std::collections::HashSet;

#[derive(Default)]
pub struct EditMarks<'a> {
    pub modified_nodes: &'a HashSet<i64>,
    pub modified_ways: &'a HashSet<i64>,
    pub deleted_nodes: &'a HashSet<i64>,
    pub deleted_ways: &'a HashSet<i64>,
    pub new_nodes: &'a HashSet<i64>,
    pub new_ways: &'a HashSet<i64>,
}

/// Serialize `data` to OSM XML, annotating elements present in `marks`
/// with `action="modify"` / `action="delete"`. Untouched elements are
/// written as plain `<node>`/`<way>` with no `action` attribute. Deleted
/// elements are written with `action="delete" visible="false"` and no
/// tags. Relations are passed through unchanged (not yet editable).
pub fn to_osm_xml(data: &OsmData, marks: &EditMarks) -> String {
    let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);

    let mut osm_start = BytesStart::new("osm");
    osm_start.push_attribute(("version", "0.6"));
    osm_start.push_attribute(("generator", "osm-gpui"));
    writer.write_event(Event::Start(osm_start)).unwrap();

    let mut node_ids: Vec<&i64> = data.nodes.keys().collect();
    node_ids.sort();
    for id in node_ids {
        let node = &data.nodes[id];
        let deleted = marks.deleted_nodes.contains(id);
        let modified = marks.modified_nodes.contains(id);
        let is_new = marks.new_nodes.contains(id);

        let mut start = BytesStart::new("node");
        start.push_attribute(("id", id.to_string().as_str()));
        if !deleted {
            start.push_attribute(("lat", node.lat.to_string().as_str()));
            start.push_attribute(("lon", node.lon.to_string().as_str()));
        }
        if deleted {
            start.push_attribute(("visible", "false"));
            start.push_attribute(("action", "delete"));
            writer.write_event(Event::Empty(start)).unwrap();
            continue;
        }
        if modified || is_new {
            start.push_attribute(("action", "modify"));
        }

        if node.tags.is_empty() {
            writer.write_event(Event::Empty(start)).unwrap();
        } else {
            writer.write_event(Event::Start(start)).unwrap();
            write_tags(&mut writer, &node.tags);
            writer.write_event(Event::End(BytesEnd::new("node"))).unwrap();
        }
    }

    for way in &data.ways {
        let deleted = marks.deleted_ways.contains(&way.id);
        let modified = marks.modified_ways.contains(&way.id);
        let is_new = marks.new_ways.contains(&way.id);

        let mut start = BytesStart::new("way");
        start.push_attribute(("id", way.id.to_string().as_str()));
        if deleted {
            start.push_attribute(("visible", "false"));
            start.push_attribute(("action", "delete"));
            writer.write_event(Event::Empty(start)).unwrap();
            continue;
        }
        if modified || is_new {
            start.push_attribute(("action", "modify"));
        }
        writer.write_event(Event::Start(start)).unwrap();
        for nd_ref in &way.nodes {
            let mut nd = BytesStart::new("nd");
            nd.push_attribute(("ref", nd_ref.to_string().as_str()));
            writer.write_event(Event::Empty(nd)).unwrap();
        }
        write_tags(&mut writer, &way.tags);
        writer.write_event(Event::End(BytesEnd::new("way"))).unwrap();
    }

    for relation in &data.relations {
        let mut start = BytesStart::new("relation");
        start.push_attribute(("id", relation.id.to_string().as_str()));
        writer.write_event(Event::Start(start)).unwrap();
        for member in &relation.members {
            let mut m = BytesStart::new("member");
            m.push_attribute(("type", member.member_type.as_str()));
            m.push_attribute(("ref", member.reference.to_string().as_str()));
            m.push_attribute(("role", member.role.as_str()));
            writer.write_event(Event::Empty(m)).unwrap();
        }
        write_tags(&mut writer, &relation.tags);
        writer.write_event(Event::End(BytesEnd::new("relation"))).unwrap();
    }

    writer.write_event(Event::End(BytesEnd::new("osm"))).unwrap();

    String::from_utf8(writer.into_inner()).unwrap()
}

fn write_tags(writer: &mut Writer<Vec<u8>>, tags: &std::collections::HashMap<String, String>) {
    let mut keys: Vec<&String> = tags.keys().collect();
    keys.sort();
    for key in keys {
        let mut tag = BytesStart::new("tag");
        tag.push_attribute(("k", key.as_str()));
        tag.push_attribute(("v", tags[key].as_str()));
        writer.write_event(Event::Empty(tag)).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::{OsmNode, OsmWay};
    use std::collections::HashMap;

    fn node(id: i64, lat: f64, lon: f64) -> OsmNode {
        OsmNode { id, lat, lon, tags: HashMap::new() }
    }

    fn data_with(nodes: Vec<OsmNode>, ways: Vec<OsmWay>) -> OsmData {
        let mut map = HashMap::new();
        for n in nodes {
            map.insert(n.id, n);
        }
        OsmData { nodes: map, ways, relations: Vec::new(), bounds: None }
    }

    #[test]
    fn untouched_node_has_no_action_attribute() {
        let data = data_with(vec![node(1, 40.0, -74.0)], vec![]);
        let marks = EditMarks::default();
        let xml = to_osm_xml(&data, &marks);
        assert!(xml.contains(r#"<node id="1" lat="40" lon="-74"/>"#));
        assert!(!xml.contains("action"));
    }

    #[test]
    fn modified_node_gets_modify_action() {
        let data = data_with(vec![node(1, 40.5, -74.5)], vec![]);
        let mut modified = HashSet::new();
        modified.insert(1i64);
        let marks = EditMarks { modified_nodes: &modified, ..Default::default() };
        let xml = to_osm_xml(&data, &marks);
        assert!(xml.contains(r#"action="modify""#));
        assert!(xml.contains(r#"lat="40.5""#));
    }

    #[test]
    fn deleted_node_omits_coordinates_and_tags() {
        let mut n = node(1, 40.0, -74.0);
        n.tags.insert("amenity".to_string(), "cafe".to_string());
        let data = data_with(vec![n], vec![]);
        let mut deleted = HashSet::new();
        deleted.insert(1i64);
        let marks = EditMarks { deleted_nodes: &deleted, ..Default::default() };
        let xml = to_osm_xml(&data, &marks);
        assert!(xml.contains(r#"action="delete""#));
        assert!(xml.contains(r#"visible="false""#));
        assert!(!xml.contains("lat="));
        assert!(!xml.contains("amenity"));
    }

    #[test]
    fn way_writes_nd_refs_in_order() {
        let n1 = node(1, 40.0, -74.0);
        let n2 = node(2, 41.0, -75.0);
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: HashMap::new() };
        let data = data_with(vec![n1, n2], vec![way]);
        let marks = EditMarks::default();
        let xml = to_osm_xml(&data, &marks);
        let way_idx = xml.find("<way").unwrap();
        let first_nd = xml[way_idx..].find(r#"ref="1""#).unwrap();
        let second_nd = xml[way_idx..].find(r#"ref="2""#).unwrap();
        assert!(first_nd < second_nd);
    }

    #[test]
    fn round_trips_through_osm_parser() {
        use crate::osm::OsmParser;
        let n1 = node(1, 40.0, -74.0);
        let n2 = node(2, 41.0, -75.0);
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: HashMap::new() };
        let data = data_with(vec![n1, n2], vec![way]);
        let marks = EditMarks::default();
        let xml = to_osm_xml(&data, &marks);

        let parsed = OsmParser::new().parse_str(&xml).expect("export should re-parse");
        assert_eq!(parsed.nodes.len(), 2);
        assert_eq!(parsed.ways.len(), 1);
    }
}
```

Note: `round_trips_through_osm_parser` uses `OsmParser::parse_str(&self, xml_str: &str) -> Result<OsmData, OsmParseError>`, which already exists in `src/osm.rs` (`parse_file` reads the file into a string and is a thin wrapper around it) — no changes needed to `osm.rs` for this test.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib osm_export::`
Expected: FAIL to compile (or fail assertions) until Step 1's code above is in place — if you pasted the full implementation already, skip straight to Step 4. If TDD'ing incrementally, comment out the function bodies first, confirm compile failure, then implement.

- [ ] **Step 3: N/A — implementation given above**

The full implementation is included in Step 1 since it's one cohesive function; there's no meaningful smaller increment to TDD within a single serializer function.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib osm_export::`
Expected: PASS (5 tests)

- [ ] **Step 5: Commit**

```bash
git add src/osm_export.rs
git commit -m "Add OSM XML export serializer"
```

---

### Task 3: Wire File > Export menu action

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `OsmLayer::edit_marks()` (Task 1), `osm_export::to_osm_xml` (Task 2), `layer.get_osm_data()` (existing).
- Produces: `ExportOsmFile` action, `export_osm_file` handler, `MapLayer::export_xml` default method.

- [ ] **Step 1: Add the trait default method**

In `src/layers/mod.rs`, in `pub trait MapLayer`, alongside the existing `fn commit_node_moves(&mut self, _moves: &[(i64, f64, f64)]) {}` default:

```rust
    /// Serialize this layer to OSM XML for export, or `None` if this layer
    /// type has no exportable OSM data (e.g. tile/grid layers).
    fn export_xml(&self) -> Option<String> {
        None
    }
```

- [ ] **Step 2: Override it on `OsmLayer`**

In `src/layers/osm_layer.rs`, in `impl MapLayer for OsmLayer`, alongside `fn commit_node_moves`:

```rust
    fn export_xml(&self) -> Option<String> {
        let data = self.osm_data.as_ref()?;
        Some(crate::osm_export::to_osm_xml(data, &self.edit_marks()))
    }
```

- [ ] **Step 3: Write the failing test**

Add to `src/layers/osm_layer.rs`'s test module:

```rust
    #[test]
    fn export_xml_none_without_data() {
        let layer = OsmLayer::new();
        assert_eq!(layer.export_xml(), None);
    }

    #[test]
    fn export_xml_some_with_data() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let layer = OsmLayer::new_with_data("L", data);
        let xml = layer.export_xml().expect("layer has data");
        assert!(xml.contains(r#"<node id="1""#));
    }
```

- [ ] **Step 4: Run tests to verify they fail then pass**

Run: `cargo test --lib export_xml`
Expected: FAIL (method doesn't exist) before Steps 1-2, PASS after.

- [ ] **Step 5: Add the action, key binding, menu item, and handler**

In `src/main.rs`, add `ExportOsmFile` to the existing `actions!` macro call:

```rust
actions!(osm_gpui, [OpenOsmFile, ExportOsmFile, Quit, AddOsmCarto, AddCoordinateGrid, DownloadFromOsm, ToggleDebugOverlay, AddCustomImagery, OpenSettings, Undo, Redo]);
```

Add the key binding next to `KeyBinding::new("cmd-o", OpenOsmFile, None),`:

```rust
                    KeyBinding::new("cmd-e", ExportOsmFile, None),
```

Add the menu item in the `"File"` `Menu` block, after `MenuItem::action("Download from OSM", DownloadFromOsm),`:

```rust
                MenuItem::action("Export…", ExportOsmFile),
```

Add the handler function next to `fn open_osm_file`:

```rust
// Handle the File > Export menu action: writes the first OsmLayer with
// data to a user-chosen .osm file.
fn export_osm_file(_: &ExportOsmFile, layer_manager: &LayerManager, cx: &mut App) {
    let Some(xml) = layer_manager.layers().iter().find_map(|l| l.export_xml()) else {
        return;
    };
    let executor = cx.background_executor().clone();
    executor
        .spawn(async move {
            if let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter("OSM files", &["osm"])
                .set_file_name("export.osm")
                .set_title("Export OSM data")
                .save_file()
                .await
            {
                if let Err(e) = std::fs::write(file.path(), xml) {
                    eprintln!("Failed to write export file: {}", e);
                }
            }
        })
        .detach();
}
```

Check how `open_osm_file` is registered as an action handler (search for `.on_action(open_osm_file` or similar `cx.on_action` / `window.on_action` call near where `OpenOsmFile` is wired) and register `export_osm_file` the same way. If `open_osm_file` takes `&mut App` only (no `&LayerManager` parameter, as shown in the existing code) and layer access requires going through the `MapViewer` entity, adapt `export_osm_file` to whatever signature `on_undo`/`on_redo` use instead (those are `MapViewer` methods with `&mut self`) — mirror the exact registration mechanism already used for `Undo`/`Redo` rather than `OpenOsmFile`'s free-function/global-queue mechanism, since export needs direct read access to `self.layer_manager`, not a cross-thread queue. Concretely: make `export_osm_file` a method `fn on_export(&mut self, _: &ExportOsmFile, _window: &mut Window, cx: &mut Context<Self>)` on `MapViewer`, following the exact shape of `fn on_undo` shown earlier in this file, and register it wherever `on_undo` is registered.

- [ ] **Step 6: Build and run full test suite**

Run: `cargo build`
Expected: builds clean.

Run: `cargo test`
Expected: all tests pass (existing suite + new tests from Tasks 1-3), no regressions.

Run: `cargo clippy`
Expected: no new warnings.

- [ ] **Step 7: Manual spot-check note for reviewer**

Add to the PR description (not to the codebase): "Manual check needed — load an .osm file, move a node, File > Export (⌘E), confirm the saved file opens in a text editor/JOSM and shows `action=\"modify\"` on the moved node only."

- [ ] **Step 8: Commit**

```bash
git add src/main.rs src/layers/mod.rs src/layers/osm_layer.rs
git commit -m "Wire File > Export to write OSM XML"
```
