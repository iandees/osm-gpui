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

Today `OsmLayer` has no notion of "this node was touched." Introduce an `EditState` (new, in `src/osm_layer_edit.rs` or inline in `layers/osm_layer.rs` — implementer's call, keep it a small standalone struct so the editing-primitives work can extend it):

```rust
#[derive(Default)]
struct EditState {
    modified_nodes: HashSet<i64>,
    modified_ways: HashSet<i64>,
    deleted_nodes: HashSet<i64>,
    deleted_ways: HashSet<i64>,
    new_nodes: HashSet<i64>,   // negative synthetic ids
    new_ways: HashSet<i64>,
    next_new_id: i64,          // starts at -1, decrements
}
```

One `EditState` per `OsmLayer` (matches undo already being per-layer via `NodeMoveUndoEntries`'s `(layer_name, ...)` pairing). `next_new_id` is only consumed by the editing-primitives plan (add/draw ops); this plan just needs the struct to exist and be threaded through so future ops have somewhere to record state.

The existing node-move commit path (`apply_undo_action` / wherever the live drag commits, in `main.rs`) gets one line added: mark the moved node's id in `modified_nodes` for its layer. This is the only behavioral hook this plan adds to existing editing.

### Serialization

New module `src/osm_export.rs`:

```rust
pub fn to_osm_xml(data: &OsmData, edit_state: &EditState) -> String
```

Iterates `data.nodes` and `data.ways` (and passes `data.relations` through untouched), writing standard OSM XML via `quick-xml`'s `Writer` (mirrors the reader in `osm.rs`):

- Untouched element: written as-is (all existing tags/attrs preserved).
- In `modified_nodes`/`modified_ways`: same, plus `action="modify"`.
- In `deleted_nodes`/`deleted_ways`: written with `action="delete"` and `visible="false"` (JOSM convention), tags omitted (deleted elements don't need their tags).
- In `new_nodes`/`new_ways`: negative `id`, no `version` attribute (or `version="0"`), `action="modify"`.

Root element: `<osm version="0.6" generator="osm-gpui">`.

### Wiring

- New `actions!` entry `ExportOsmFile`, bound to ⌘E (mirrors `OpenOsmFile` / ⌘O).
- Menu item **File > Export...**.
- Handler: `rfd` save dialog (worker thread, same pattern as Open) defaulting to `.osm` extension, then `to_osm_xml(&layer.data, &layer.edit_state)` written to the chosen path.
- If there are multiple `OsmLayer`s, export the one currently selected in the layer panel (fall back to the first `OsmLayer` if none selected — there's no existing "active layer" concept beyond the panel selection, so this plan introduces the minimal notion needed: whichever layer's tags most recently populated the side panel).

### Testing

- Pure unit tests on `to_osm_xml` given hand-built `OsmData` + `EditState`: assert exact XML output (string assertions) for each case — untouched, modified, deleted, new. No round-trip through `OsmParser` is required (JOSM is the intended consumer), but a round-trip test (export then re-parse with the existing `OsmParser`, assert node/way counts match) is worth adding as a sanity check.
- No GUI test — menu wiring is verified by build + a manual spot-check note for the human reviewer (existing project convention per the box-selection plan).

## Open questions resolved during brainstorming

- Format: full-fidelity JOSM-style `.osm` with action attributes, not osmChange. (User choice.)
