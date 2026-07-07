# NSI-style presets — design

## Context

osm-gpui currently has no tag-editing capability at all — tags are read-only,
aggregated across the current selection in the side panel (`render_tags_section`,
`src/main.rs:1484`). This feature adds a minimal way to *set* tags on a single
selected feature by searching the [Name Suggestion Index](https://github.com/osmlab/name-suggestion-index)
(NSI) — a large dataset of brand-name → tag-set mappings (e.g. typing
"starbucks" surfaces `amenity=cafe`, `name=Starbucks`, `brand=Starbucks`,
`brand:wikidata=Q37158`, ...) — and applying the matched tags in one action.

This is intentionally narrow: it is *not* a general tag editor, and not the
broader iD-style generic preset system (amenity/shop/etc. category pickers).
It only covers NSI's brand-matching use case, for a single selected node or
way at a time.

## 1. Data & background fetch (`src/nsi.rs`, new file)

```rust
pub struct NsiEntry {
    pub display_name: String,
    pub tags: HashMap<String, String>,
    pub match_names: Vec<String>, // pre-normalized aliases from upstream
}

pub struct NsiIndex {
    entries: Vec<NsiEntry>,
}

impl NsiIndex {
    pub fn search(&self, query: &str, limit: usize) -> Vec<&NsiEntry> { ... }
}
```

- Parse via `serde_json::Value` (not strict structs, to tolerate upstream
  schema drift) against the shape of `dist/nsi.json` from
  `osmlab/name-suggestion-index`: top-level `"nsi"` object maps category path
  → `{"items": [...]}`. Each item may have `displayName`, `tags` (string map),
  `matchNames` (string array). Skip items missing `displayName` or `tags`.
- **No `locationSet` geo-filtering in v1** — every brand entry is offered
  regardless of country. Documented limitation / follow-up candidate.
- No data is bundled with the app. On startup:
  - If a cached copy exists at `dirs::cache_dir()/osm-gpui/nsi.json`, parse it
    synchronously into the in-memory index immediately, so search works on
    the very next frame.
  - Regardless, check the cache file's mtime. If missing or older than 7
    days, spawn a background thread that `ureq::get`s the raw file from
    GitHub, writes it to a `.tmp` file, and atomically renames it into place
    (same pattern as `settings_store::save_to`). On success, reparse and hand
    the new `Arc<NsiIndex>` back through a
    `OnceLock<Mutex<Option<Arc<NsiIndex>>>>`, drained once per frame the same
    way `SHARED_OSM_DATA` is today.
  - On first-ever run (no cache, fetch in flight), the index is empty and the
    dialog shows a "Downloading NSI data…" placeholder instead of results.

## 2. Search / matching

- Normalize query and candidate strings: lowercase + trim non-alphanumeric
  characters (NSI's own `matchNames` are already normalized this way upstream).
- An entry matches if the normalized query is a substring of `display_name`
  or any of its `match_names`.
- Ranking: prefix matches before other substring matches; ties broken by
  shorter `display_name` first. Cap at 30 results.
- Empty query shows no results (never dump the whole index).

## 3. UI: dialog + menu

- New file `src/ui/nsi_dialog.rs`, modeled on `src/ui/custom_imagery_dialog.rs`.
- Menu item **Edit > Apply NSI Preset…**, enabled only when
  `self.selected.len() == 1` (mirrors how Undo/Redo already reflect stack
  state). Opens the dialog, held in an `Option<Entity<NsiPresetDialog>>`
  field on `MapViewer`, same pattern as `custom_imagery_dialog`.
- Dialog: text input at top, scrollable result list below, each row showing
  `display_name` plus a compact tag preview (e.g.
  "Starbucks — amenity=cafe, brand=Starbucks"). While the index is
  empty/loading, the list area shows "Downloading NSI data…" instead.
- Selecting a result (click, or arrow-keys + Enter) applies tags to the one
  selected feature and closes the dialog. Escape closes without applying.

## 4. Apply + undo integration

- New `OsmLayer::commit_tag_change(&mut self, kind: FeatureKind, id: i64, new_tags: HashMap<String, String>)`,
  mirroring `commit_node_moves` (`src/layers/osm_layer.rs:291`): clone the
  current `OsmData`, set `node.tags` or `way.tags` on the matching id, mark
  `modified`, call `set_osm_data` to rebuild derived caches.
- New `UndoableAction::SetTags { layer_name: String, kind: FeatureKind, id: i64, before: Vec<(String,String)>, after: Vec<(String,String)> }`.
  `apply_undo_action` gets a matching arm that calls `commit_tag_change` with
  `before` (undo) or `after` (redo). `description()` returns
  `"Applied preset to 1 feature"`.
- Apply logic: read the single selected `FeatureRef`, look up its current
  tags, merge (`existing.clone()` overwritten key-by-key by the preset's
  tags — preset wins on conflicts, all other existing tags like `addr:*` or
  `opening_hours` are preserved), call `commit_tag_change`, push the undo
  entry, close the dialog.

## 5. Testing

- `src/nsi.rs`: unit tests for JSON parsing (small inline fixture matching
  NSI's shape), normalization, search ranking (prefix-before-substring, cap
  at 30), and the staleness check (mtime > 7 days ⇒ refresh).
- `osm_layer.rs`/`main.rs`: unit tests for `commit_tag_change` (tags updated,
  `modified` set, other fields untouched) and `UndoableAction::SetTags`
  round-tripping through `UndoStack` (undo restores `before`, redo restores
  `after`), following the existing `MoveNodes` test pattern.
- Manual (documented in the PR test plan): load an OSM file, select a single
  node, open the dialog, search a known brand (e.g. "starbucks"), apply,
  confirm tags appear in the side panel, confirm Undo reverts and Redo
  reapplies. Also manually verify first-run behavior (no cache) shows the
  loading placeholder, and that a populated cache survives an app restart.

## Out of scope (v1)

- Multi-select apply (only single-feature selection supported).
- `locationSet` geo-filtering (brand availability by country/region).
- Editing or removing individual tags outside of preset-apply (no general
  tag editor yet).
- Generic (non-brand) iD-style category presets.
- Manual "refresh NSI data now" UI control — background policy only.
