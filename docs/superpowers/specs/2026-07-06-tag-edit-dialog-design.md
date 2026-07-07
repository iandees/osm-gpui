# Tag editor dialog

## Goal

Let the user edit OSM tags on the current selection (one or many features)
from the right panel's Tags section: double-click a tag's key or value to
open an edit dialog (pre-populated, with the clicked field's text
pre-selected), add a brand new tag, or delete an existing tag — all as
undoable actions.

## Scope

- Double-clicking a tag row's key zone or value zone in
  `render_tags_section` opens a `TagEditDialog` populated with that row's
  key and value. Double-clicking the key zone pre-selects the key field's
  text; double-clicking the value zone pre-selects the value field's text.
- The dialog has two text inputs (key, value) and Cancel/Save buttons,
  built the same way as `CustomImageryDialog`
  (`src/ui/custom_imagery_dialog.rs`): a focused, scrim-backed modal card.
- An "Add tag" button below the tag list opens the same dialog with both
  fields empty, targeting the full current selection.
- Each tag row also gets a small delete affordance that removes that key
  from every selected feature immediately (no dialog).
- Submitting the dialog (edit, rename, or add) and clicking delete both
  push a new `UndoableAction::SetTags` entry, so tag changes are undoable/
  redoable via the existing Edit menu and Cmd-Z/Cmd-Shift-Z, exactly like
  node moves.
- Multi-select apply semantics (edit/rename/add all share one code path):
  - Key changed from the original → remove the old key, set the new key.
  - Value box left showing the original text (including the `<N values>`
    placeholder when the row started as `TagValue::Multiple`) → each
    feature keeps its own pre-existing value for that key; only add/rename
    changes are applied.
  - Value box edited to different text → every selected feature's value
    for that key is set to the submitted text (uniform overwrite).
  - Add-tag semantics: applies to every selected feature; overwrites the
    value if a feature already has that key.
- New `OsmLayer` mutation methods `set_tag`/`remove_tag`, following the
  `commit_node_moves` idiom (clone-on-write `OsmData`, mutate, mark
  `modified = true`).

## Out of scope

- Deleting a tag via emptying the key/value fields in the dialog (delete
  is a separate, dialog-free affordance — see Scope).
- Any validation of OSM tag keys/values beyond "key must be non-empty to
  submit" (no character restrictions, no key namespacing rules).
- Editing tags on relations (no relation selection support exists yet;
  `FeatureKind` has no `Relation` variant).
- A history-list description finer than a generic count (see Data model);
  no per-key description like "Set `highway` on 2 features".

## Data model

### `OsmLayer` mutation methods (new, `src/layers/osm_layer.rs`)

Mirrors `commit_node_moves` (osm_layer.rs:287-305): clone the current
`OsmData` out of the `Arc`, mutate the target node's/way's `tags:
HashMap<String, String>`, mark `self.modified = true`, and write the
`Arc<OsmData>` back. Tag edits don't change geometry, so caches don't need
rebuilding — write straight to `self.osm_data` instead of routing through
`set_osm_data`:

```rust
/// Set (insert or overwrite) a single tag on one node or way. No-op if the
/// feature doesn't belong to this layer or isn't found.
pub fn set_tag(&mut self, kind: FeatureKind, id: i64, key: &str, value: &str) {
    let Some(current) = self.osm_data.clone() else { return; };
    let mut data = (*current).clone();
    let tags = match kind {
        FeatureKind::Node => data.nodes.get_mut(&id).map(|n| &mut n.tags),
        FeatureKind::Way => data.ways.iter_mut().find(|w| w.id == id).map(|w| &mut w.tags),
    };
    let Some(tags) = tags else { return; };
    tags.insert(key.to_string(), value.to_string());
    self.modified = true;
    self.osm_data = Some(Arc::new(data));
}

/// Remove a single tag key from one node or way. No-op if the feature or
/// key isn't present.
pub fn remove_tag(&mut self, kind: FeatureKind, id: i64, key: &str) {
    let Some(current) = self.osm_data.clone() else { return; };
    let mut data = (*current).clone();
    let tags = match kind {
        FeatureKind::Node => data.nodes.get_mut(&id).map(|n| &mut n.tags),
        FeatureKind::Way => data.ways.iter_mut().find(|w| w.id == id).map(|w| &mut w.tags),
    };
    let Some(tags) = tags else { return; };
    tags.remove(key);
    self.modified = true;
    self.osm_data = Some(Arc::new(data));
}
```

Both get a `MapLayer` trait default (no-op, matching the
`commit_node_moves` trait/impl split in `src/layers/mod.rs`) and a
forwarding override in `impl MapLayer for OsmLayer`.

### `UndoableAction::SetTags` (extend the enum in `src/main.rs`)

The existing enum's doc comment (main.rs:284-286) already anticipates this.
One entry per affected feature, storing before/after `Option<String>` for
whichever key that feature was touched under (`None` = key absent):

```rust
enum UndoableAction {
    MoveNodes { per_layer: NodeMoveUndoEntries },
    SetTags {
        /// One entry per affected feature: which key, and its value
        /// before/after (`None` = key was/becomes absent).
        entries: Vec<(FeatureRef, String /* key */, Option<String>, Option<String>)>,
    },
}
```

A rename (key changed) is represented as two entries for the same feature:
one `(feature, old_key, Some(old_value), None)` and one `(feature, new_key,
None, Some(new_value))` — i.e. rename is modeled as remove-old +
add-new, not as a distinct variant. This keeps `apply_undo_action` a single
uniform loop (see below) and keeps `SetTags` reusable for delete (all
entries have `after: None`) and add (all entries have `before: None`).

`description()` gains a `SetTags` arm: `"Changed 1 tag"` /
`"Changed N tags"` (count = `entries.len()`, matching the existing
singular/plural pattern used for `MoveNodes`).

### Applying undo/redo (`apply_undo_action`, main.rs:700-717)

New match arm, symmetric with the existing one:

```rust
UndoableAction::SetTags { entries } => {
    for (feature, key, before, after) in entries {
        let Some(layer) = self.layer_manager.find_layer_mut(&feature.layer_name) else { continue; };
        let value = if forward { after } else { before };
        match value {
            Some(v) => layer.set_tag(feature.kind, feature.id, key, v),
            None => layer.remove_tag(feature.kind, feature.id, key),
        }
    }
}
```

## `TagEditDialog` (new, `src/ui/tag_edit_dialog.rs`)

Structured exactly like `CustomImageryDialog`:

```rust
pub enum DialogEvent {
    Submitted { key: String, value: String },
    Cancelled,
}

pub struct TagEditDialog {
    key: Entity<InputState>,
    value: Entity<InputState>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}
impl EventEmitter<DialogEvent> for TagEditDialog {}
```

`TagEditDialog::new(window, cx, initial_key, initial_value, select: TagEditField)`
where `enum TagEditField { Key, Value, None }` (the last used for the
"Add tag" case). Builds both `InputState`s via
`InputState::new(window, cx)`, calls `set_value` on each with the initial
text, focuses the field named by `select` (or `key` by default when
`select` is `None`), and — because `InputState::select_all` is
`pub(super)` inside gpui-component and not reachable directly — selects
that field's text by dispatching the public `SelectAll` action on the
window right after focusing it: `window.dispatch_action(Box::new(
gpui_component::input::SelectAll), cx)`. This relies on gpui's normal
action-dispatch-along-focus-chain, the same mechanism the cmd-a keybinding
already uses internally, just invoked programmatically instead of from a
keystroke.

Submit (`Save` button / Enter key, following the `on_key_down` pattern at
custom_imagery_dialog.rs:208-235): reads `.value()` off both `InputState`s;
if the key is empty, sets `self.error` and does not emit; otherwise emits
`DialogEvent::Submitted { key, value }`. Escape / Cancel button emits
`DialogEvent::Cancelled`.

Render: same modal-overlay structure as `CustomImageryDialog::render`
(scrim `div` with `.absolute().inset_0().bg(rgba(0x00000099))`, centered
bordered `v_flex` card with title/body/footer, Cancel + Save buttons).
Title reads "Add tag" when opened with empty fields, "Edit tag" otherwise.

## Wiring into `MapViewer`

### New field and dialog-open sites

```rust
/// Active tag-edit dialog, if open, plus the context needed to apply its
/// result: which features it targets, and — for edit/rename (not
/// add) — the row's original key and value text (the latter possibly the
/// "<N values>" placeholder) so submit can tell whether the value box was
/// actually touched.
tag_edit_dialog: Option<(Entity<osm_gpui::ui::tag_edit_dialog::TagEditDialog>, TagEditContext)>,
```
```rust
struct TagEditContext {
    features: Vec<FeatureRef>,
    original_key: String,
    original_value: String,
}
```

Two open sites, both constructing the dialog directly (no static queue is
needed here, unlike `custom_imagery_dialog`, because both sites already run
inside a method with `window: &mut Window` and `cx: &mut Context<Self>`):

- **Row double-click** (in `render_tags_section`): each key/value zone's
  `on_mouse_down` handler (see Double-click detection below) opens the
  dialog with `initial_key`/`initial_value` from that row, targeting
  `self.selected.clone()`, `select: Key` or `Value` depending on which
  zone was clicked.
- **"Add tag" button**: opens the dialog with empty key/value,
  `select: None`, targeting `self.selected.clone()`.

### Subscription and apply (mirrors `check_for_dialog_queue`'s subscribe block)

```rust
cx.subscribe(&dialog, |this, _entity, event, cx| {
    match event {
        DialogEvent::Cancelled => {
            this.tag_edit_dialog = None;
            cx.notify();
        }
        DialogEvent::Submitted { key, value } => {
            this.apply_tag_edit(key, value, cx);
            this.tag_edit_dialog = None;
            cx.notify();
        }
    }
}).detach();
```

`apply_tag_edit(&mut self, key: &str, value: &str, cx)` (new method): takes
the `TagEditContext` from `self.tag_edit_dialog`, builds `SetTags` undo
entries per the Scope section's multi-select rules —

- `key_changed = key != ctx.original_key`
- `value_touched = value != ctx.original_value`
- For each feature in `ctx.features`, read its current value for
  `ctx.original_key` via `layer.feature_tags(feature)` (`None` if absent).
  - If `key_changed`: entry removing `ctx.original_key` (before = that
    feature's current value, after = `None`), plus an entry adding `key`
    (before = that feature's current value for `key` if any, after =
    `Some(new value)`, where new value = the feature's own original value
    if `!value_touched`, else the submitted `value`).
  - Else (same key): one entry for `key` (before = feature's current
    value, after = `Some(if value_touched { value } else { feature's own
    current value })`) — when `!value_touched` this entry is a no-op
    (before == after) and can be skipped entirely.
- Apply each entry's `after` immediately via `layer.set_tag`/`remove_tag`
  (same as `apply_undo_action`'s forward case), then push
  `UndoableAction::SetTags { entries }` — entries with `before == after`
  are filtered out before pushing so a no-op edit doesn't create an empty
  undo step.

Delete-tag button (per row, next to the value): calls a small
`delete_tag(&mut self, key: &str, cx)` that builds one `SetTags` entry per
currently-selected feature that has `key` (before = current value, after =
`None`), applies immediately, and pushes the action — skipping the dialog
entirely.

## Double-click detection (new — none exists in this codebase today)

Confirmed by search: no double-click handling exists anywhere in `src/`.
Add minimal state to `MapViewer`:

```rust
/// Last click's time and target, for double-click detection on tag rows.
last_tag_row_click: Option<(std::time::Instant, TagRowZone)>,
```
```rust
struct TagRowZone { key: String, part: TagEditField } // Key or Value
```

Each tag row's key/value `div` gets `.on_mouse_down(MouseButton::Left,
cx.listener(move |this, _event, _window, cx| { ... }))`. Handler: if
`last_tag_row_click` is `Some((t, zone))` with `zone == this_zone` and
`t.elapsed() < Duration::from_millis(400)`, treat as double-click: open
the dialog (per Wiring section) and clear `last_tag_row_click`. Otherwise
record `Some((Instant::now(), this_zone))` and do nothing else. 400ms
matches common desktop double-click thresholds; no existing constant in
this codebase to reuse.

## `render_tags_section` changes

Replace the `DescriptionList`/`DescriptionItem` rows (main.rs:1504-1514)
with custom `div`-based rows (since `DescriptionItem` has no click support
at all), keeping the same bordered two-column visual layout: one `div` for
the key label (double-click zone), one for the value label (double-click
zone) plus an adjacent small delete-icon `div` (single-click, not part of
double-click detection). Below the row list, an "Add tag" `Button`.
`TagValue::Multiple(n)` keeps rendering as `"<n values>"` text (existing
wording, unchanged) both in the row and as the pre-populated dialog value.

## Testing

- `OsmLayer::set_tag`/`remove_tag` unit tests: set on existing node/way,
  set on missing feature (no-op), remove existing key, remove missing key
  (no-op), and that `modified` becomes `true` only on an actual mutation
  path being invoked (matches existing `commit_node_moves` behavior of
  always setting `modified = true` when not a no-op moves list — same
  precedent applies here: called at all ⇒ `modified = true`, no finer
  no-op distinction, consistent with how `commit_node_moves` doesn't check
  whether values actually changed).
- `apply_tag_edit`'s entry-building logic (edit / rename / add, single vs.
  multi-select, value-untouched-preserves-per-feature-value) as a pure
  function over `Vec<(FeatureRef, HashMap<String,String>)>` snapshots,
  independent of GPUI, following the existing `aggregate_tags` testing
  style in `src/selection.rs:184-236`.
- `UndoableAction::SetTags` description-string unit test (singular/plural),
  matching the existing `MoveNodes` description test.
- No GUI automation available in this sandbox (documented limitation
  carried over from prior UI features): double-click detection, dialog
  focus/select-all behavior, and row rendering aren't exercised by
  automated tests, only the underlying mutation/undo logic.
