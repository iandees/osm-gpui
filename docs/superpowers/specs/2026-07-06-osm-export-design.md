# OSM XML Export — Design

## Problem

Editing (node move, and soon delete/add/split/join/square) only ever mutates in-memory `OsmData`. Nothing survives closing the app, and there's no way to hand edits to JOSM or a future upload flow. We need a **File > Export** path that writes the current layer's data back out as OSM XML.

## Goal

Export a JOSM-compatible `.osm` file: the full dataset (not just a diff), with `action="modify"` / `action="delete"` attributes on touched elements and negative synthetic ids on newly-created elements. This format is reopenable in JOSM or this app, and is the natural basis for a future osmChange-based upload (#13).

## Non-goals

- No changeset upload / OAuth (#13 is separate and blocked).
- No osmChange (`<create>/<modify>/<delete>` sectioned) format — that's an upload-time concern, not a save-file concern.
- No dirty-tracking for relations (relations aren't editable yet; they're carried through unchanged if present).

## Design

### Edit-state tracking

`OsmLayer` (`src/layers/osm_layer.rs`) already has a coarse `modified: bool` flag, set by `commit_node_moves`. That's not fine-grained enough to know *which* elements to mark `action="modify"` vs. leave untouched on export, and there's no tracking at all for deletes or new elements (needed by the editing-primitives plan, not this one, but the fields should exist now so that plan only adds behavior, not schema).

Add fields directly to `OsmLayer` (alongside the existing `modified: bool`, which stays as-is — it's still the cheap "is there anything to save" check used elsewhere):

```rust
modified_node_ids: HashSet<i64>,
modified_way_ids: HashSet<i64>,
deleted_node_ids: HashSet<i64>,
deleted_way_ids: HashSet<i64>,
new_node_ids: HashSet<i64>,    // negative synthetic ids
new_way_ids: HashSet<i64>,
next_new_id: i64,              // starts at -1, decrements on each allocation
```

Initialized empty / `-1` in both `OsmLayer::new()` and `OsmLayer::new_with_data()`. `deleted_*`, `new_*`, and `next_new_id` are only consumed by the editing-primitives plan; this plan only populates and reads `modified_node_ids`/`modified_way_ids`.

`commit_node_moves` gets one line added: insert each moved node's id into `modified_node_ids` (in addition to setting `self.modified = true`, which it already does).

### Serialization

New module `src/osm_export.rs`, with a plain data-only struct describing which ids are dirty (keeps the function testable without depending on `OsmLayer`/gpui):

```rust
#[derive(Default)]
pub struct EditMarks<'a> {
    pub modified_nodes: &'a HashSet<i64>,
    pub modified_ways: &'a HashSet<i64>,
    pub deleted_nodes: &'a HashSet<i64>,
    pub deleted_ways: &'a HashSet<i64>,
    pub new_nodes: &'a HashSet<i64>,
    pub new_ways: &'a HashSet<i64>,
}

pub fn to_osm_xml(data: &OsmData, marks: &EditMarks) -> String
```

Callers in `main.rs` construct `EditMarks` from the exporting `OsmLayer`'s fields. Iterates `data.nodes` and `data.ways` (and passes `data.relations` through untouched), writing standard OSM XML via `quick-xml`'s `Writer` (mirrors the reader in `osm.rs`):

- Untouched element: written as-is (all existing tags/attrs preserved).
- In `modified_nodes`/`modified_ways`: same, plus `action="modify"`.
- In `deleted_nodes`/`deleted_ways`: written with `action="delete"` and `visible="false"` (JOSM convention), tags omitted (deleted elements don't need their tags).
- In `new_nodes`/`new_ways`: negative `id`, no `version` attribute (or `version="0"`), `action="modify"`.

Root element: `<osm version="0.6" generator="osm-gpui">`.

### Wiring

- New `actions!` entry `ExportOsmFile`, bound to ⌘E (mirrors `OpenOsmFile` / ⌘O).
- Menu item **File > Export...** in the existing `Menu { name: "File", ... }` block.
- `MapLayer` trait (`src/layers/mod.rs`) gains a default no-op method `fn export_xml(&self) -> Option<String> { None }`, matching the existing `commit_node_moves` default-no-op pattern (dyn-dispatch instead of downcasting). `OsmLayer` overrides it: builds an `EditMarks` from its own id sets and calls `osm_export::to_osm_xml`.
- Handler finds the first layer in `layer_manager.layers()` whose `export_xml()` returns `Some(..)` (i.e. the first `OsmLayer` with data) and writes that string to the chosen path via an `rfd` save dialog (worker thread, same pattern as Open), defaulting to a `.osm` extension. Multi-`OsmLayer` export (picking a specific one) is out of scope for this plan — there's no existing "active layer" concept to hang it off of.

### Testing

- Pure unit tests on `to_osm_xml` given hand-built `OsmData` + `EditState`: assert exact XML output (string assertions) for each case — untouched, modified, deleted, new. No round-trip through `OsmParser` is required (JOSM is the intended consumer), but a round-trip test (export then re-parse with the existing `OsmParser`, assert node/way counts match) is worth adding as a sanity check.
- No GUI test — menu wiring is verified by build + a manual spot-check note for the human reviewer (existing project convention per the box-selection plan).

## Open questions resolved during brainstorming

- Format: full-fidelity JOSM-style `.osm` with action attributes, not osmChange. (User choice.)
