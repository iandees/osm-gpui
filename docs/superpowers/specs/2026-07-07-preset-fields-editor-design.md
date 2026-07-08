# iD-style field-based tag editor — design

## Goal

Replace raw key/value tag editing, where the matched preset supports it, with
iD's own human-friendly form fields (text/combo/check/radio/multiCombo
widgets), built on top of the preset-matching system from
[docs/superpowers/specs/2026-07-07-id-preset-labels-design.md](2026-07-07-id-preset-labels-design.md)
(vendored `id-tagging-schema` presets, `PresetIndex::match_feature`,
`feature_geometry`).

Today, selecting a feature shows its matched preset's friendly name/icon
(read-only) in the Selection section, and all editing happens through the
raw key/value Tags table. This adds a new **Fields** section, populated with
typed widgets for the matched preset's fields, plus a minimal preset picker
so a feature's type can be deliberately changed (not just auto-matched).

## Non-goals

Explicit simplifications, not oversights:

- **No multi-key fields.** iD has fields spanning several tags at once (e.g.
  a combined min/max building-height field, or a structured address field
  covering `addr:housenumber`/`addr:street`/etc.). v1 only supports fields
  with exactly one `key`. Multi-key fields are simply never included in a
  preset's rendered field list — their tags remain editable only via the
  raw Tags table.
- **No dynamic/taginfo-driven combo suggestions.** iD's real editor
  sometimes populates combo options from live taginfo popularity data. This
  spec only renders combo/radio/multiCombo widgets for fields that have a
  static, vendored `options` list. Fields without one aren't given a typed
  widget — same graceful degradation as the previous spec's missing-icon
  handling.
- **No multi-select field editing.** The Fields section only appears when
  exactly one feature is selected. Multi-select continues to use the
  existing raw Tags table (which already aggregates across a selection).
- **No non-English localization.** Field labels/placeholders/option labels
  are merged from `translations/en.json` only, matching how preset names
  are already merged today.
- **No field-level validation** beyond what the widget itself constrains
  (e.g. a checkbox can only be checked/unchecked). No numeric-range
  checking, no format validation.
- **No changes to the Tags table's existing behavior** — it keeps working
  exactly as it does today, for both single- and multi-select.

## Vendored data additions

Extends `assets/presets/` (checked into git, refreshed by
`examples/update_presets.rs`, same vendoring model as the presets spec):

- `assets/presets/fields.json` — trimmed from upstream `dist/fields.json`.
  Each entry keeps:
  - `id` (string)
  - `key` (string) — the single OSM tag key this field edits. Fields whose
    upstream definition has `keys` (plural, multi-key) instead of `key` are
    dropped entirely during trimming (see Non-goals).
  - `field_type` (one of `"text"`, `"combo"`, `"check"`, `"radio"`,
    `"multiCombo"`) — mapped from upstream's more numerous type strings;
    any upstream type not in this list is dropped during trimming (e.g.
    `"address"`, `"wikidata"`, `"structureRadio"`, `"defaultCheck"` all
    become "field not vendored" rather than a fallback type).
  - `label` (string) — merged from `translations/en.json` at
    `en.presets.fields.<id>.label`, same inline-then-translation-fallback
    pattern `preset_names` already uses for preset names.
  - `placeholder` (string, optional) — same translation-merge source,
    `en.presets.fields.<id>.placeholder`.
  - `options` (list of `{value, label}`, optional) — only present for
    combo/radio/multiCombo fields that have a static upstream `options`
    array; each option's label merged from
    `en.presets.fields.<id>.options.<value>` (falling back to the raw
    value string if untranslated). A combo/radio/multiCombo field with no
    vendored `options` is dropped during trimming, same as an unsupported
    `field_type`.

- `Preset` (in `src/presets.rs`) gains two new fields:
  - `fields: Vec<String>` — field IDs shown by default (iD's `fields` list,
    filtered during trimming to only IDs that survived field-trimming).
  - `more_fields: Vec<String>` — field IDs available via an explicit
    "Add field" action but not shown by default (iD's `moreFields` list,
    same filtering).

  Both lists are trimmed by `update_presets.rs` to exclude any field ID
  that didn't make it into the vendored `fields.json` (multi-key,
  unsupported type, or missing options) — the Rust side never needs to
  handle a dangling field-ID reference.

`update_presets.rs` fetches `dist/fields.json` alongside the presets and
translations it already fetches, and filters `Preset.fields`/`more_fields`
against the resulting vendored field ID set before writing the trimmed
`presets.json`.

## Rust data model

New types in `src/presets.rs` (or a new `src/fields.rs` if the file grows
large enough to warrant a split — left as an implementation-time call):

```rust
pub enum FieldType { Text, Combo, Check, Radio, MultiCombo }

pub struct FieldOption { pub value: String, pub label: String }

pub struct Field {
    pub id: String,
    pub key: String,
    pub field_type: FieldType,
    pub label: String,
    pub placeholder: Option<String>,
    pub options: Vec<FieldOption>, // empty for Text/Check
}

pub struct FieldIndex { /* id -> Field */ }
impl FieldIndex {
    pub fn from_json(body: &str) -> Result<Self, serde_json::Error>;
    pub fn get(&self, id: &str) -> Option<&Field>;
}
```

Global loader `field_index() -> &'static FieldIndex`, embedded via
`include_str!` exactly like `preset_index()`/`area_keys()`.

A pure resolution function (GPUI-independent, directly unit-testable):

```rust
/// Resolve a preset's default field list to `Field`s, in order, skipping
/// any id not present in `index` (shouldn't happen given vendor-time
/// filtering, but the function stays defensive rather than panicking).
pub fn resolve_fields(index: &FieldIndex, field_ids: &[String]) -> Vec<&Field>;
```

Used for both `preset.fields` (default section) and, on demand,
`preset.more_fields` (the "Add field" list, filtered to exclude fields
already shown from `preset.fields`).

## UI components

Two new files, keeping `src/side_panel.rs` from growing further:

### `src/ui/fields_section.rs`

Renders the Fields section, shown only when exactly one feature is
selected:

1. Resolve the selected feature's matched preset via the existing
   `describe_selected_feature`-style lookup (tags + `feature_geometry` +
   `PresetIndex::match_feature`).
2. Resolve `preset.fields` to `Field`s via `resolve_fields`.
3. For each `Field`, render a widget seeded from the feature's current tag
   value for `field.key` (or empty/unchecked if absent):
   - `Text` → gpui-component `input::InputState`, commits on blur/Enter
     (matches `TagEditDialog`'s existing submit-on-Enter pattern — avoids
     an undo entry per keystroke).
   - `Combo` → gpui-component `combobox`, commits immediately on selection.
   - `Check` → gpui-component `checkbox`, commits immediately on toggle,
     using OSM's conventional `yes`/`no` tag values.
   - `Radio` → gpui-component `radio` group, commits immediately on
     selection.
   - `MultiCombo` → gpui-component `combobox` for adding a value + a chip
     list of currently-selected values (each removable), committing a
     semicolon-joined tag value on each add/remove (OSM's multi-value tag
     convention).
4. An "Add field" control lists `preset.more_fields` (resolved the same
   way, excluding any already shown from `preset.fields`); selecting one
   promotes it into the rendered list for that editing session.

All commits reuse the existing undo-aware tag-set path — the same function
`TagEditDialog::submit` and `MapViewer::apply_nsi_preset` already call to
set a tag with undo support. No new tag-mutation code path.

### `src/ui/preset_picker_dialog.rs`

A modal search dialog, structurally mirroring `src/ui/nsi_dialog.rs`
(search box, ranked results, Enter/click to apply), but:

- Searches the vendored `PresetIndex` by preset `name` (substring match,
  same normalize-and-rank approach `NsiIndex::search` already uses) instead
  of NSI's brand index.
- Filters candidates to those whose `geometry` list contains the selected
  feature's already-computed `Geometry` (via `feature_geometry`) — so a
  point selection is never offered an area-only preset.
- On selection, applies the chosen preset's `tags` to the feature using
  the same tag-diff/apply logic `apply_nsi_preset` uses today (existing
  tags not mentioned by the preset, like `addr:*` or `name`, are left
  untouched — matching that function's documented behavior). If
  `apply_nsi_preset` is NSI-specific in name/signature, it's generalized
  (renamed, parameterized) to serve both callers rather than duplicated.

A "change type" affordance in the Selection section (or Fields section
header) opens this dialog for the currently selected feature.

## Data flow / state management

`MapViewer` gains per-selected-feature widget state (`InputState` /
combobox / etc. entities keyed by field id), rebuilt whenever the selected
feature or its matched preset changes — mirroring how `TagEditDialog`
already owns `InputState` entities, just persistent inline widgets instead
of a modal's. If the selected feature is deleted or deselected while the
Fields section is showing, it clears the same way `describe_selected_feature`
already degrades to `None` today (no panic, no stale widget state).

## Testing

- Unit tests in `src/presets.rs` (or `src/fields.rs`): `FieldIndex::from_json`
  parsing, `resolve_fields` ordering/skip-missing behavior, `more_fields`
  filtering-out-already-shown-fields logic — all pure, no GPUI dependency,
  using small JSON fixtures (not the full vendored file).
- Manual verification via the project's own `.osmscript` screenshot
  harness (`docs/screenshots/fixtures/select.osm` or a new fixture),
  exactly the approach that verified the previous spec's Selection-row
  rendering: load a fixture feature, confirm the Fields section renders
  the expected widgets with the expected seeded values, edit a field via
  a scripted interaction, confirm the underlying tag updates.
- `examples/update_presets.rs`'s field-vendoring additions are, like the
  rest of that tool, not unit tested — verified by running it and
  inspecting the resulting `assets/presets/fields.json` diff by hand.
