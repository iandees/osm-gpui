# iD preset friendly labels/icons — design

## Goal

Show a human-readable feature type (name + icon) for selected features in the
side panel's Selection section, sourced from the
[iD tagging schema](https://github.com/openstreetmap/id-tagging-schema)
presets, alongside the existing NSI brand-preset support (`src/nsi.rs`,
`src/ui/nsi_dialog.rs`).

Today, `render_selection_section` in `src/side_panel.rs` shows only
`"Node 123456"` / `"Way 123456"`. This adds a matched preset's icon + name,
e.g. `[cafe icon] Cafe · Node 123456`.

Out of scope for v1 (explicit non-goals, not oversights):

- No generic preset *search* dialog (that's a separate future feature).
- No unification with the NSI dialog.
- No use of preset `terms`, `fields`, `addTags`/`removeTags`.
- No `locationSet` (country) restriction filtering — a preset matches
  anywhere it's tag/geometry-eligible, regardless of location.
- No display in the Tags section — Selection-row rows only.
- No map-canvas POI icon rendering (side panel only).

## Vendored data

Checked into git under `assets/presets/`, not fetched at runtime (unlike
NSI's jsDelivr fetch-and-cache):

- `assets/presets/presets.json` — trimmed subset of upstream
  `id-tagging-schema` presets. Each entry keeps only:
  - `id` (string)
  - `name` (string)
  - `icon` (string, e.g. `"maki-cafe"`)
  - `tags` (map of required key → value; value `"*"` means "key present,
    any value")
  - `geometry` (list of `"point"` / `"vertex"` / `"line"` / `"area"` /
    `"relation"`)
  - `match_score` (float, defaults to `1.0` if upstream omits it)

  Upstream's own generic fallback presets (`point`, `vertex`, `line`,
  `area`, `relation`) are included in this trimmed set like any other
  preset — they have empty `tags` and match only on geometry, which is
  what makes them a fallback.

- `assets/presets/area_keys.json` — trimmed vendor of iD's `areaKeys.json`,
  used only to decide whether a closed way is a `line` or an `area`.

- `assets/presets/icons/*.svg` — only the Maki/Temaki icons actually
  referenced by `presets.json`, plus a `LICENSE` file for attribution
  (Maki and Temaki are both CC0, but attribution is included as good
  practice).

## Update tooling

`examples/update_presets.rs`, run manually via
`cargo run --example update_presets` (same convention as the existing
`examples/perf_bench.rs`). Not part of the app runtime or CI.

Steps:

1. Fetch `id-tagging-schema`'s built `dist/presets.json` and iD's
   `dist/areaKeys.json` from jsDelivr, using `src/http.rs`'s
   retry-aware client (same transport `nsi.rs` uses).
2. Extract only the fields listed above, write
   `assets/presets/presets.json` and `assets/presets/area_keys.json`.
3. Collect the distinct icon names referenced by the trimmed presets
   (plus the small fixed set of generic fallback icons), fetch each SVG
   from the Maki/Temaki npm packages via jsDelivr, and write them to
   `assets/presets/icons/`. Delete any vendored icon file that's no
   longer referenced, so re-running the tool keeps the directory in
   sync with current upstream rather than accumulating stale files.

A developer runs this occasionally and reviews/commits the resulting
diff — there is no automatic refresh.

## Rust data model & loading

New module `src/presets.rs` (mirroring the shape of `src/nsi.rs`, minus
the network/caching parts since this is vendored):

```rust
pub struct Preset {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub tags: HashMap<String, String>, // value "*" = key-present match
    pub geometry: Vec<Geometry>,
    pub match_score: f32,
}

pub enum Geometry { Point, Vertex, Line, Area, Relation }

pub struct PresetIndex { presets: Vec<Preset> }
```

Loaded once at startup from the embedded/vendored `assets/presets/`
files (read via `std::fs` relative to the executable/assets dir, same
pattern as `assets/default.mapcss` is loaded today) into a process-wide
`OnceLock`, analogous to `nsi::current()` but populated synchronously
at startup since there's no network fetch involved.

## Geometry classification

New helper computing a feature's `Geometry` from `OsmData`:

- Node not referenced by any way → `Point`
- Node referenced by ≥1 way → `Vertex`
- Way, first node id ≠ last node id → `Line`
- Way, closed (first == last node id) → `Area` if its tags qualify per
  `area_keys.json`, else `Line`

`area_keys.json` shape (mirroring iD's own): a map of tag key → either
`true` (any value on a closed way implies area) or a map of specific
values that are *excluded* from implying area (i.e. a closed way with
that key/value is still a line).

Relations are out of scope for v1 matching (the app doesn't render/edit
them yet per the main README); `Geometry::Relation` exists in the enum
for schema completeness but nothing currently produces it.

## Matching algorithm

Given a feature's tags and computed `Geometry`:

1. Filter `PresetIndex` to presets whose `geometry` list contains the
   feature's geometry.
2. Keep only presets whose `tags` are a full subset match: every
   key/value pair in `preset.tags` must be present on the feature
   (`"*"` matches any value for that key, including an empty string).
3. Among matches, rank by number of matched tag pairs descending
   (more specific tag match wins), tie-broken by `match_score`
   descending.
4. Take the top-ranked preset. Because the generic fallback presets
   (`point`/`vertex`/`line`/`area`) have empty `tags` (0 matched pairs)
   and are geometry-filtered like everything else, they only win when
   nothing more specific matches — guaranteeing every feature gets a
   label.

## UI integration

In `src/side_panel.rs::render_selection_section`, for each selected
feature:

1. Look up its tags and computed geometry from the currently loaded
   `OsmData`.
2. Match against the global `PresetIndex`.
3. Render the preset's icon (via GPUI's `svg().external_path(...)`
   pointing at the vendored `assets/presets/icons/<icon>.svg`, falling
   back to no icon if the preset has none or the file is missing) and
   name ahead of the existing `"{kind_label} {feat.id}"` text, e.g.:

   ```
   [cafe icon] Cafe · Node 123456
   ```

## Testing

- Unit tests for geometry classification: point vs. vertex, open vs.
  closed way, area-key inclusion/exclusion edge cases.
- Unit tests for preset matching: exact tag match, wildcard (`"*"`)
  value match, specificity ranking with multiple candidates,
  `match_score` tie-break, fallback-to-generic-geometry-preset when
  nothing else matches.
- A small fixture subset of vendored `presets.json` used in tests
  (not the full vendored file) so tests don't depend on vendor content
  staying exactly as-is.
- `update_presets.rs` itself is not unit tested (it's a dev-only tool
  hitting real network endpoints); manual review of its output diff is
  the verification step when it's run.
