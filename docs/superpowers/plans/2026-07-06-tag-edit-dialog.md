# Tag Editor Dialog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user edit, add, and delete OSM tags on the current selection from the right panel's Tags section, with every change undoable via the existing Edit menu / Cmd-Z / Cmd-Shift-Z.

**Architecture:** A new `TagEditDialog` (modeled on the existing `CustomImageryDialog`) opens on double-click of a tag row's key or value, or via a new "Add tag" button; submitting it computes per-feature tag mutations through a new pure function `compute_tag_edit_entries` (in `src/selection.rs`, alongside the existing `aggregate_tags`), applies them through two new `OsmLayer` methods (`set_tag`/`remove_tag`, modeled on `commit_node_moves`), and records them as a new `UndoableAction::SetTags` variant. Deleting a tag skips the dialog and goes straight from a per-row delete icon to the same apply/undo path.

**Tech Stack:** Rust, gpui, gpui-component (`InputState`/`Input`, `Button`).

## Global Constraints

- Follow this repo's commit convention: single-line commit messages, no `Co-Authored-By` trailers (per user's global git preferences).
- No GUI automation is available in this sandbox; every task must be verified via `cargo test` / `cargo build`, not by driving the running app.
- Multi-select edit semantics (from the approved spec, `docs/superpowers/specs/2026-07-06-tag-edit-dialog-design.md`):
  - Editing a value (key unchanged): set on every selected feature, unless the value box is left showing its original text (including the `<N values>` placeholder), in which case each feature keeps its own current value.
  - Renaming a key: only features that already have the original key are touched (remove old key, set new key); features without the key are left alone.
  - Adding a new tag (via the "Add tag" button): set on every selected feature, overwriting any existing value for that key.
  - Deleting a tag: removes the key from every selected feature that has it, no dialog.
- Tag edits are undoable/redoable through the existing `UndoStack`/`Undo`/`Redo` machinery — no new keybindings or menu items.

---

### Task 1: `OsmLayer::set_tag` / `remove_tag`

**Files:**
- Modify: `src/layers/mod.rs` (add trait defaults, near `commit_node_moves` at line 102-104)
- Modify: `src/layers/osm_layer.rs` (add inherent methods near `commit_node_moves` at line 287-305; add trait-forwarding overrides near line 376-378; add tests near line 903-911)

**Interfaces:**
- Consumes: `crate::selection::FeatureKind` (already imported in `osm_layer.rs` at line 9), `crate::osm::OsmData`/`OsmNode`/`OsmWay` (`tags: HashMap<String, String>` field on both).
- Produces: `OsmLayer::set_tag(&mut self, kind: FeatureKind, id: i64, key: &str, value: &str)`, `OsmLayer::remove_tag(&mut self, kind: FeatureKind, id: i64, key: &str)`, and the same two methods as `MapLayer` trait methods (default no-op + `OsmLayer` override). Later tasks call these only through the `MapLayer` trait (`layer.set_tag(...)` / `layer.remove_tag(...)` via `&mut Box<dyn MapLayer>` from `LayerManager::find_layer_mut`).

- [ ] **Step 1: Add trait defaults to `MapLayer`**

In `src/layers/mod.rs`, immediately after the `commit_node_moves` default (currently lines 102-104):

```rust
    /// Set (insert or overwrite) a single tag on a feature this layer owns.
    /// Default: no-op.
    fn set_tag(&mut self, _kind: crate::selection::FeatureKind, _id: i64, _key: &str, _value: &str) {}

    /// Remove a single tag key from a feature this layer owns. Default: no-op.
    fn remove_tag(&mut self, _kind: crate::selection::FeatureKind, _id: i64, _key: &str) {}
```

- [ ] **Step 2: Write the failing tests**

In `src/layers/osm_layer.rs`, inside the `#[cfg(test)] mod tests` block, immediately after `commit_node_moves_empty_is_noop` (currently ending at line 911):

```rust
    #[test]
    fn set_tag_inserts_and_overwrites_on_node() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.set_tag(FeatureKind::Node, 1, "highway", "residential");
        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.nodes.get(&1).unwrap().tags.get("highway"),
            Some(&"residential".to_string())
        );

        layer.set_tag(FeatureKind::Node, 1, "highway", "trunk");
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.nodes.get(&1).unwrap().tags.get("highway"),
            Some(&"trunk".to_string())
        );
    }

    #[test]
    fn set_tag_on_way() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let n2 = OsmNode { id: 2, lat: 40.001, lon: -74.001, tags: empty_tags() };
        let way = OsmWay { id: 10, nodes: vec![1, 2], tags: empty_tags() };
        let data = data_with(vec![n1, n2], vec![way]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.set_tag(FeatureKind::Way, 10, "surface", "paved");
        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(
            updated.ways[0].tags.get("surface"),
            Some(&"paved".to_string())
        );
    }

    #[test]
    fn set_tag_missing_feature_id_is_noop() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.set_tag(FeatureKind::Node, 999, "highway", "residential");
        assert!(!layer.is_modified());
    }

    #[test]
    fn remove_tag_removes_existing_key() {
        let mut tags = empty_tags();
        tags.insert("highway".to_string(), "residential".to_string());
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.remove_tag(FeatureKind::Node, 1, "highway");
        assert!(layer.is_modified());
        let updated = layer.get_osm_data().unwrap();
        assert_eq!(updated.nodes.get(&1).unwrap().tags.get("highway"), None);
    }

    #[test]
    fn remove_tag_missing_feature_id_is_noop() {
        let n1 = OsmNode { id: 1, lat: 40.0, lon: -74.0, tags: empty_tags() };
        let data = data_with(vec![n1], vec![]);
        let mut layer = OsmLayer::new_with_data("L", data);

        layer.remove_tag(FeatureKind::Node, 999, "highway");
        assert!(!layer.is_modified());
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib layers::osm_layer::tests::set_tag`
Expected: FAIL to compile (`set_tag`/`remove_tag` not found on `OsmLayer`).

- [ ] **Step 4: Implement `set_tag`/`remove_tag` on `OsmLayer`**

In `src/layers/osm_layer.rs`, immediately after `commit_node_moves` (currently ending at line 305, right before `pub fn get_osm_data`):

```rust
    /// Set (insert or overwrite) a single tag on one node or way this layer
    /// owns. Marks the layer modified whenever the feature is found (same
    /// precedent as `commit_node_moves`: called at all implies modified,
    /// no finer no-op distinction). Doesn't rebuild geometry caches since
    /// tags don't affect them. No-op if the feature isn't found.
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

    /// Remove a single tag key from one node or way this layer owns. Marks
    /// the layer modified whenever the feature is found, same precedent as
    /// `set_tag`. No-op if the feature isn't found.
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

Then add trait-forwarding overrides in `impl MapLayer for OsmLayer`, immediately after `commit_node_moves` (currently lines 376-378):

```rust
    fn set_tag(&mut self, kind: FeatureKind, id: i64, key: &str, value: &str) {
        OsmLayer::set_tag(self, kind, id, key, value);
    }

    fn remove_tag(&mut self, kind: FeatureKind, id: i64, key: &str) {
        OsmLayer::remove_tag(self, kind, id, key);
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib layers::osm_layer::tests::`
Expected: PASS, including the new `set_tag_*`/`remove_tag_*` tests.

- [ ] **Step 6: Commit**

```bash
git add src/layers/mod.rs src/layers/osm_layer.rs
git commit -m "Add OsmLayer::set_tag and remove_tag"
```

---

### Task 2: `UndoableAction::SetTags`

**Files:**
- Modify: `src/main.rs` (extend `UndoableAction` enum at lines 284-307, `apply_undo_action` at lines 700-717, `undo_stack_tests` module at lines 348-415)

**Interfaces:**
- Consumes: `osm_gpui::selection::FeatureRef` (already used elsewhere in `main.rs`, e.g. field `selected: Vec<osm_gpui::selection::FeatureRef>` at line 252), `OsmLayer::set_tag`/`remove_tag` via the `MapLayer` trait (Task 1), `LayerManager::find_layer_mut` (existing, `src/layers/mod.rs:160`).
- Produces: `UndoableAction::SetTags { entries: Vec<(osm_gpui::selection::FeatureRef, String, Option<String>, Option<String>)> }` and its `apply_undo_action` handling. Task 5 constructs and pushes this variant.

- [ ] **Step 1: Write the failing test**

In `src/main.rs`, inside `mod undo_stack_tests` (currently lines 348-415), add after `push_after_undo_discards_redo_branch` (ends at line 414):

```rust
    fn tag_change(
        feature: osm_gpui::selection::FeatureRef,
        key: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> (osm_gpui::selection::FeatureRef, String, Option<String>, Option<String>) {
        (
            feature,
            key.to_string(),
            before.map(|s| s.to_string()),
            after.map(|s| s.to_string()),
        )
    }

    #[test]
    fn set_tags_description_singular_and_plural() {
        use osm_gpui::selection::{FeatureKind, FeatureRef};
        let f = FeatureRef { layer_name: "L".to_string(), kind: FeatureKind::Node, id: 1 };

        let one = UndoableAction::SetTags {
            entries: vec![tag_change(f.clone(), "highway", None, Some("residential"))],
        };
        assert_eq!(one.description(), "Changed 1 tag");

        let two = UndoableAction::SetTags {
            entries: vec![
                tag_change(f.clone(), "highway", None, Some("residential")),
                tag_change(f, "surface", None, Some("paved")),
            ],
        };
        assert_eq!(two.description(), "Changed 2 tags");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib undo_stack_tests::set_tags_description`
Expected: FAIL to compile (`UndoableAction::SetTags` variant doesn't exist).

- [ ] **Step 3: Extend `UndoableAction` and `apply_undo_action`**

In `src/main.rs`, replace the `UndoableAction` enum and its `description` impl (currently lines 284-307):

```rust
/// A single reversible data mutation, recorded on the global undo stack.
#[derive(Clone)]
enum UndoableAction {
    MoveNodes { per_layer: NodeMoveUndoEntries },
    /// One entry per affected feature: which key, and its value before/
    /// after (`None` = key was/becomes absent). A key rename is modeled as
    /// two entries for the same feature — remove-old plus add-new — so
    /// this stays a single uniform apply loop.
    SetTags {
        entries: Vec<(osm_gpui::selection::FeatureRef, String, Option<String>, Option<String>)>,
    },
}

impl UndoableAction {
    /// Human-readable label for the history list, e.g. "Moved 3 nodes".
    fn description(&self) -> String {
        match self {
            UndoableAction::MoveNodes { per_layer } => {
                let count: usize = per_layer.iter().map(|(_, entries)| entries.len()).sum();
                if count == 1 {
                    "Moved 1 node".to_string()
                } else {
                    format!("Moved {} nodes", count)
                }
            }
            UndoableAction::SetTags { entries } => {
                if entries.len() == 1 {
                    "Changed 1 tag".to_string()
                } else {
                    format!("Changed {} tags", entries.len())
                }
            }
        }
    }
}
```

Then extend `apply_undo_action` (currently lines 700-717) with a new match arm:

```rust
    fn apply_undo_action(&mut self, action: &UndoableAction, forward: bool) {
        match action {
            UndoableAction::MoveNodes { per_layer } => {
                for (layer_name, entries) in per_layer {
                    let moves: Vec<(i64, f64, f64)> = entries
                        .iter()
                        .map(|&(id, before, after)| {
                            let (lat, lon) = if forward { after } else { before };
                            (id, lat, lon)
                        })
                        .collect();
                    if let Some(layer) = self.layer_manager.find_layer_mut(layer_name) {
                        layer.commit_node_moves(&moves);
                    }
                }
            }
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
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib undo_stack_tests::`
Expected: PASS, including `set_tags_description_singular_and_plural` and all pre-existing `undo_stack_tests`.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "Add UndoableAction::SetTags and wire it into apply_undo_action"
```

---

### Task 3: `compute_tag_edit_entries` (pure multi-select apply logic)

**Files:**
- Modify: `src/selection.rs` (add function after `aggregate_tags`, currently ending at line 111; add tests in the existing `#[cfg(test)] mod tests` block, currently lines 113-237)

**Interfaces:**
- Consumes: `FeatureRef` (this file, lines 11-16).
- Produces: `pub fn compute_tag_edit_entries(features: &[(FeatureRef, Vec<(String, String)>)], original_key: &str, original_value: &str, new_key: &str, new_value: &str, is_add: bool) -> Vec<(FeatureRef, String, Option<String>, Option<String>)>`. Task 5's `apply_tag_edit` calls this to build the list it hands to `UndoableAction::SetTags` (Task 2) and applies via `set_tag`/`remove_tag` (Task 1).

- [ ] **Step 1: Write the failing tests**

In `src/selection.rs`, inside `#[cfg(test)] mod tests`, add after `aggregate_empty_input_returns_empty` (currently ending at line 236):

```rust
    fn tags(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn edit_same_key_uniform_value_when_touched() {
        let a = fref("L", FeatureKind::Node, 1);
        let b = fref("L", FeatureKind::Node, 2);
        let features = vec![
            (a.clone(), tags(&[("highway", "residential")])),
            (b.clone(), tags(&[("highway", "trunk")])),
        ];
        let entries = compute_tag_edit_entries(&features, "highway", "<2 values>", "highway", "service", false);
        assert_eq!(
            entries,
            vec![
                (a, "highway".to_string(), Some("residential".to_string()), Some("service".to_string())),
                (b, "highway".to_string(), Some("trunk".to_string()), Some("service".to_string())),
            ]
        );
    }

    #[test]
    fn edit_same_key_untouched_value_preserves_per_feature() {
        let a = fref("L", FeatureKind::Node, 1);
        let b = fref("L", FeatureKind::Node, 2);
        let features = vec![
            (a.clone(), tags(&[("highway", "residential")])),
            (b.clone(), tags(&[("highway", "trunk")])),
        ];
        // Value box left showing the original "<2 values>" placeholder text.
        let entries = compute_tag_edit_entries(&features, "highway", "<2 values>", "highway", "<2 values>", false);
        assert!(entries.is_empty(), "no feature's value actually changes: {:?}", entries);
    }

    #[test]
    fn edit_same_key_noop_produces_no_entries() {
        let a = fref("L", FeatureKind::Node, 1);
        let features = vec![(a, tags(&[("highway", "residential")]))];
        let entries = compute_tag_edit_entries(&features, "highway", "residential", "highway", "residential", false);
        assert!(entries.is_empty());
    }

    #[test]
    fn rename_moves_value_and_skips_features_without_key() {
        let a = fref("L", FeatureKind::Node, 1);
        let b = fref("L", FeatureKind::Node, 2);
        let features = vec![
            (a.clone(), tags(&[("highway", "residential")])),
            (b.clone(), tags(&[])), // b never had "highway"
        ];
        let entries = compute_tag_edit_entries(&features, "highway", "residential", "highway_type", "residential", false);
        assert_eq!(
            entries,
            vec![
                (a.clone(), "highway".to_string(), Some("residential".to_string()), None),
                (a, "highway_type".to_string(), None, Some("residential".to_string())),
            ]
        );
        // b is untouched entirely — it never had "highway" to rename.
    }

    #[test]
    fn rename_with_touched_value_uses_new_value() {
        let a = fref("L", FeatureKind::Node, 1);
        let features = vec![(a.clone(), tags(&[("highway", "residential")]))];
        let entries = compute_tag_edit_entries(&features, "highway", "residential", "highway_type", "trunk", false);
        assert_eq!(
            entries,
            vec![
                (a.clone(), "highway".to_string(), Some("residential".to_string()), None),
                (a, "highway_type".to_string(), None, Some("trunk".to_string())),
            ]
        );
    }

    #[test]
    fn add_sets_new_key_on_all_features_overwriting_existing() {
        let a = fref("L", FeatureKind::Node, 1);
        let b = fref("L", FeatureKind::Node, 2);
        let features = vec![
            (a.clone(), tags(&[])),
            (b.clone(), tags(&[("surface", "gravel")])),
        ];
        let entries = compute_tag_edit_entries(&features, "", "", "surface", "paved", true);
        assert_eq!(
            entries,
            vec![
                (a, "surface".to_string(), None, Some("paved".to_string())),
                (b, "surface".to_string(), Some("gravel".to_string()), Some("paved".to_string())),
            ]
        );
    }

    #[test]
    fn add_noop_when_value_already_matches() {
        let a = fref("L", FeatureKind::Node, 1);
        let features = vec![(a, tags(&[("surface", "paved")]))];
        let entries = compute_tag_edit_entries(&features, "", "", "surface", "paved", true);
        assert!(entries.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib selection::tests::edit_same_key`
Expected: FAIL to compile (`compute_tag_edit_entries` not found).

- [ ] **Step 3: Implement `compute_tag_edit_entries`**

In `src/selection.rs`, immediately after `aggregate_tags` (currently ending at line 111):

```rust
/// Computes the tag mutations to apply across `features` for a tag-edit
/// dialog submission. Each feature's current tags are supplied as
/// `(FeatureRef, Vec<(String, String)>)`. Returns one entry per feature per
/// key actually touched, as `(feature, key, before, after)` (`before`/
/// `after` are `None` when the key is absent/removed); entries where
/// `before == after` are omitted since they're no-ops.
///
/// `is_add` is true for the "Add tag" flow (dialog opened with empty
/// key/value, targeting the whole selection): `new_key` is set to
/// `new_value` on every feature, overwriting any existing value.
///
/// Otherwise this is an edit or rename of an existing row:
/// - If `new_key != original_key` (rename): for each feature that already
///   has `original_key`, remove it and set `new_key` — to the feature's
///   own preserved value if `new_value == original_value` (value box left
///   untouched), else to `new_value` uniformly. Features that never had
///   `original_key` are left untouched entirely (nothing to rename).
/// - Otherwise (same key): set `original_key` to `new_value` on every
///   feature, unless `new_value == original_value` (untouched), in which
///   case each feature keeps its own current value (a no-op, omitted).
pub fn compute_tag_edit_entries(
    features: &[(FeatureRef, Vec<(String, String)>)],
    original_key: &str,
    original_value: &str,
    new_key: &str,
    new_value: &str,
    is_add: bool,
) -> Vec<(FeatureRef, String, Option<String>, Option<String>)> {
    let value_touched = new_value != original_value;
    let mut out = Vec::new();

    for (feature, tags) in features {
        let current = |key: &str| tags.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

        if is_add {
            let before = current(new_key);
            if before.as_deref() != Some(new_value) {
                out.push((feature.clone(), new_key.to_string(), before, Some(new_value.to_string())));
            }
            continue;
        }

        if new_key != original_key {
            let Some(old_before) = current(original_key) else {
                continue; // never had the key being renamed — nothing to do
            };
            out.push((feature.clone(), original_key.to_string(), Some(old_before.clone()), None));

            let new_before = current(new_key);
            let after_value = if value_touched { new_value.to_string() } else { old_before };
            if new_before.as_deref() != Some(after_value.as_str()) {
                out.push((feature.clone(), new_key.to_string(), new_before, Some(after_value)));
            }
        } else {
            let before = current(original_key);
            let after = if value_touched { Some(new_value.to_string()) } else { before.clone() };
            if before != after {
                out.push((feature.clone(), original_key.to_string(), before, after));
            }
        }
    }

    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib selection::tests::`
Expected: PASS, including all new `compute_tag_edit_entries` tests and the pre-existing `aggregate_tags`/hit-test tests.

- [ ] **Step 5: Commit**

```bash
git add src/selection.rs
git commit -m "Add compute_tag_edit_entries for multi-select tag edit apply logic"
```

---

### Task 4: `TagEditDialog` UI component

**Files:**
- Create: `src/ui/tag_edit_dialog.rs`
- Modify: `src/ui/mod.rs` (register the new module)

**Interfaces:**
- Consumes: `gpui_component::input::{Input, InputState}`, `gpui_component::button::{Button, ButtonVariants}`, same imports as `src/ui/custom_imagery_dialog.rs`.
- Produces: `pub enum TagEditField { Key, Value, None }`; `pub enum DialogEvent { Submitted { key: String, value: String }, Cancelled }`; `pub struct TagEditDialog` with `pub fn new(window: &mut Window, cx: &mut Context<Self>, title: SharedString, initial_key: String, initial_value: String, select: TagEditField) -> Self`. Task 5 constructs this via `cx.new(|cx| TagEditDialog::new(window, cx, ...))` and subscribes to its `DialogEvent`s, exactly mirroring how `check_for_dialog_queue` (main.rs:1023-1069) drives `CustomImageryDialog`.

- [ ] **Step 1: Register the module**

In `src/ui/mod.rs`:

```rust
//! UI components shared across the app: dialogs.

pub mod custom_imagery_dialog;
pub mod settings_window;
pub mod tag_edit_dialog;
```

- [ ] **Step 2: Write the failing test**

Create `src/ui/tag_edit_dialog.rs` with just the test module first (no implementation yet), to drive out the field-selection logic that's worth unit-testing without GPUI — which `TagEditField` a fresh dialog should focus/select:

```rust
//! Modal dialog to add, edit, or rename a single OSM tag key/value on the
//! current selection.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagEditField {
    Key,
    Value,
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_variants_are_distinct() {
        assert_ne!(TagEditField::Key, TagEditField::Value);
        assert_ne!(TagEditField::Value, TagEditField::None);
    }
}
```

(This is a placeholder step to get the file and module wired up before adding the GPUI-dependent dialog struct, which isn't unit-testable in this sandbox — see Global Constraints.)

- [ ] **Step 3: Run test to verify it fails, then passes**

Run: `cargo test --lib ui::tag_edit_dialog::tests::`
Expected: first FAIL (module not yet registered/compiling until Step 1 is also done), then PASS once Steps 1-2 are both in place.

- [ ] **Step 4: Implement the dialog struct, modeled on `CustomImageryDialog`**

Append to `src/ui/tag_edit_dialog.rs` (after the `TagEditField` enum and its test module):

```rust
use gpui::{
    div, prelude::*, rgba, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState, SelectAll},
    label::Label,
    v_flex, ActiveTheme as _,
};

pub enum DialogEvent {
    Submitted { key: String, value: String },
    Cancelled,
}

pub struct TagEditDialog {
    title: SharedString,
    key: Entity<InputState>,
    value: Entity<InputState>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for TagEditDialog {}

impl TagEditDialog {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        title: impl Into<SharedString>,
        initial_key: String,
        initial_value: String,
        select: TagEditField,
    ) -> Self {
        let key = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("key");
            state.set_value(initial_key, window, cx);
            state
        });
        let value = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("value");
            state.set_value(initial_value, window, cx);
            state
        });
        let focus_handle = cx.focus_handle();

        match select {
            TagEditField::Key => {
                key.update(cx, |state, cx| state.focus(window, cx));
                window.dispatch_action(Box::new(SelectAll), cx);
            }
            TagEditField::Value => {
                value.update(cx, |state, cx| state.focus(window, cx));
                window.dispatch_action(Box::new(SelectAll), cx);
            }
            TagEditField::None => {
                key.update(cx, |state, cx| state.focus(window, cx));
            }
        }

        Self {
            title: title.into(),
            key,
            value,
            error: None,
            focus_handle,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let key = self.key.read(cx).value().trim().to_string();
        let value = self.value.read(cx).value().to_string();
        if key.is_empty() {
            self.error = Some("Tag key is required.".into());
            cx.notify();
            return;
        }
        self.error = None;
        cx.emit(DialogEvent::Submitted { key, value });
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.cancel(cx),
            "enter" => self.submit(cx),
            _ => {}
        }
    }
}

impl Focusable for TagEditDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TagEditDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let field_row = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap_1()
                .child(Label::new(label).text_xs().text_color(muted))
                .child(Input::new(input))
        };

        let mut body = v_flex()
            .gap_3()
            .child(field_row("Key", &self.key))
            .child(field_row("Value", &self.value));

        if let Some(msg) = self.error.clone() {
            body = body.child(Label::new(msg).text_sm().text_color(cx.theme().danger));
        }

        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _w, cx| this.cancel(cx))),
            )
            .child(
                Button::new("save")
                    .primary()
                    .label("Save")
                    .on_click(cx.listener(|this, _, _w, cx| this.submit(cx))),
            );

        let frame = v_flex()
            .w(gpui::px(360.0))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_color(cx.theme().foreground)
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(self.title.clone()),
            )
            .child(div().p_4().child(body))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(footer),
            );

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .absolute()
            .inset_0()
            .bg(rgba(0x00000099))
            .flex()
            .justify_center()
            .items_center()
            .child(frame)
    }
}
```

- [ ] **Step 5: Verify the crate builds**

Run: `cargo build`
Expected: builds cleanly (this task adds a new, currently-unused-outside-its-own-module component; Task 5 wires it up).

- [ ] **Step 6: Commit**

```bash
git add src/ui/tag_edit_dialog.rs src/ui/mod.rs
git commit -m "Add TagEditDialog component"
```

---

### Task 5: Wire the dialog into `MapViewer`

**IMPORTANT architectural constraint** (discovered during Task 4's review): `TagEditDialog` MUST be constructed from *inside* a `Render::render` pass, never directly from a click/action handler. Task 4's dialog defers its "select all on open" behavior via `window.on_next_frame(...)`, which only lands correctly if the dialog's first paint happens in the *same* draw pass that constructs it — exactly how the existing `CustomImageryDialog` is built via `check_for_dialog_queue` (called at the top of `Render::render`, main.rs:1530), never from inside a click handler. If `TagEditDialog` is instead constructed directly inside an `on_mouse_down`/`on_click` listener (outside of `render()`), the deferred select-all silently fails (it would fire one frame too early, before the dialog's first paint). This task therefore uses a **pending-open-request** pattern: click/button handlers only record *that* a dialog should open (into a new `pending_tag_edit_open: Option<PendingTagEditOpen>` field), and a new method — checked at the top of `render()`, alongside `check_for_dialog_queue` — is what actually constructs the dialog.

**Files:**
- Modify: `src/main.rs`:
  - `use gpui_component::{...}` block (lines 22-28): add `button::{Button, ButtonVariants as _}`.
  - `struct MapViewer` (lines 246-276): add `tag_edit_dialog` and `pending_tag_edit_open` fields.
  - `MapViewer::new` (lines 450-468): initialize both new fields.
  - `render_tags_section` (lines 1478-1515): rewrite rows as custom double-click-aware `div`s, add delete icon per row, add "Add tag" button — each interaction sets `pending_tag_edit_open`, none construct the dialog directly.
  - New methods: `check_for_pending_tag_edit_dialog`, `apply_tag_edit`, `delete_tag`.
  - `impl Render for MapViewer`: call `self.check_for_pending_tag_edit_dialog(window, cx);` alongside the existing `self.check_for_dialog_queue(window, cx);` call (main.rs:1530); also render `.children(self.tag_edit_dialog.clone().map(|(d, _)| d))` next to `.children(self.custom_imagery_dialog.clone())` (around line 1716).

**Interfaces:**
- Consumes: `osm_gpui::ui::tag_edit_dialog::{TagEditDialog, TagEditField, DialogEvent}` (Task 4), `osm_gpui::selection::compute_tag_edit_entries` (Task 3), `UndoableAction::SetTags` (Task 2), `layer.set_tag`/`layer.remove_tag` (Task 1), `layer.feature_tags` (existing), `osm_gpui::selection::{FeatureRef, TagValue, aggregate_tags}` (existing).
- Produces: nothing consumed by later tasks — this is the last task.

- [ ] **Step 1: Add the import and the `tag_edit_dialog` field**

In `src/main.rs`, update the `gpui_component` import block (currently lines 22-28):

```rust
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    description_list::{DescriptionItem, DescriptionList},
    label::Label,
    menu::ContextMenuExt,
};
```

Add two structs right after the `NodeMoveUndoEntries` type alias (currently line 282, before the `UndoableAction` enum):

```rust
/// Which features a `TagEditDialog` targets and the row's original text
/// (used to detect whether the value box was actually touched before
/// applying — see `compute_tag_edit_entries`). `original_key`/
/// `original_value` are both empty for the "Add tag" flow.
struct TagEditContext {
    features: Vec<osm_gpui::selection::FeatureRef>,
    original_key: String,
    original_value: String,
    is_add: bool,
}

/// Recorded by a row's double-click or the "Add tag" button; consumed by
/// `check_for_pending_tag_edit_dialog` (called from `Render::render`) to
/// actually construct the dialog. This indirection exists so the dialog is
/// always built from inside a render pass — never directly inside a click
/// handler — which `TagEditDialog`'s deferred select-all-on-open (Task 4)
/// depends on.
struct PendingTagEditOpen {
    features: Vec<osm_gpui::selection::FeatureRef>,
    original_key: String,
    original_value: String,
    select: osm_gpui::ui::tag_edit_dialog::TagEditField,
    is_add: bool,
}
```

In `struct MapViewer` (currently lines 246-276), add after `custom_imagery_dialog` (line 268):

```rust
    /// Active tag-edit dialog, if open, plus the context needed to apply
    /// its result.
    tag_edit_dialog: Option<(gpui::Entity<osm_gpui::ui::tag_edit_dialog::TagEditDialog>, TagEditContext)>,
    /// A dialog-open request recorded by a row/button click, to be acted on
    /// during the next `render()` — see `PendingTagEditOpen`'s doc comment.
    pending_tag_edit_open: Option<PendingTagEditOpen>,
```

In `MapViewer::new` (currently lines 450-468), add after `custom_imagery_dialog: None,` (line 464):

```rust
            tag_edit_dialog: None,
            pending_tag_edit_open: None,
```

- [ ] **Step 2: Run a build to check the field wiring compiles**

Run: `cargo build`
Expected: builds cleanly (new field is `None`-initialized and not yet read/written elsewhere).

- [ ] **Step 3: Add `check_for_pending_tag_edit_dialog`, `apply_tag_edit`, `delete_tag`**

Add these methods to `impl MapViewer`, near `apply_undo_action` (after it, i.e. after line 717 in the pre-Task-2 numbering — place directly below the method added in Task 2):

```rust
    /// Snapshot every currently-selected feature's tags from its owning
    /// layer, as `(FeatureRef, Vec<(String, String)>)` — the shape
    /// `compute_tag_edit_entries` expects.
    fn selected_feature_tag_snapshots(&self) -> Vec<(osm_gpui::selection::FeatureRef, Vec<(String, String)>)> {
        self.selected
            .iter()
            .filter_map(|sel| {
                self.layer_manager
                    .find_layer(&sel.layer_name)
                    .and_then(|layer| layer.feature_tags(sel))
                    .map(|tags| (sel.clone(), tags))
            })
            .collect()
    }

    /// If a row/button click recorded a pending tag-edit-dialog open
    /// request, construct the dialog now. Called from `Render::render` (see
    /// Step 6) — never call this, or construct `TagEditDialog` directly,
    /// from inside a click/action listener: `TagEditDialog`'s deferred
    /// select-all-on-open (Task 4) only lands correctly when the dialog is
    /// built during the same draw pass that first paints it, exactly like
    /// `check_for_dialog_queue` builds `CustomImageryDialog`.
    fn check_for_pending_tag_edit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_tag_edit_open.take() else { return };
        if self.tag_edit_dialog.is_some() {
            return; // one at a time; drop the request rather than queue it
        }
        let PendingTagEditOpen { features, original_key, original_value, select, is_add } = pending;
        let title = if is_add { "Add tag" } else { "Edit tag" };
        let dialog = cx.new(|cx| {
            osm_gpui::ui::tag_edit_dialog::TagEditDialog::new(
                window,
                cx,
                title,
                original_key.clone(),
                original_value.clone(),
                select,
            )
        });
        cx.subscribe(&dialog, |this, _entity, event, cx| {
            use osm_gpui::ui::tag_edit_dialog::DialogEvent;
            match event {
                DialogEvent::Cancelled => {
                    this.tag_edit_dialog = None;
                    cx.notify();
                }
                DialogEvent::Submitted { key, value } => {
                    this.apply_tag_edit(key, value);
                    this.tag_edit_dialog = None;
                    cx.notify();
                }
            }
        })
        .detach();
        self.tag_edit_dialog = Some((
            dialog,
            TagEditContext { features, original_key, original_value, is_add },
        ));
        cx.notify();
    }

    /// Apply a submitted tag-edit dialog result: compute the per-feature
    /// mutations via `compute_tag_edit_entries`, apply them immediately,
    /// and push one `UndoableAction::SetTags` (skipped entirely if there
    /// were no actual changes).
    fn apply_tag_edit(&mut self, key: &str, value: &str) {
        let Some((_, ctx)) = self.tag_edit_dialog.take() else { return };
        let snapshots: Vec<_> = self
            .selected_feature_tag_snapshots()
            .into_iter()
            .filter(|(f, _)| ctx.features.contains(f))
            .collect();

        let entries = osm_gpui::selection::compute_tag_edit_entries(
            &snapshots,
            &ctx.original_key,
            &ctx.original_value,
            key,
            value,
            ctx.is_add,
        );
        if entries.is_empty() {
            return;
        }

        for (feature, k, _before, after) in &entries {
            let Some(layer) = self.layer_manager.find_layer_mut(&feature.layer_name) else { continue };
            match after {
                Some(v) => layer.set_tag(feature.kind, feature.id, k, v),
                None => layer.remove_tag(feature.kind, feature.id, k),
            }
        }
        self.undo_stack.push(UndoableAction::SetTags { entries });
    }

    /// Delete `key` from every currently-selected feature that has it,
    /// applying immediately and pushing one `UndoableAction::SetTags` (no
    /// dialog involved).
    fn delete_tag(&mut self, key: &str, cx: &mut Context<Self>) {
        let entries: Vec<_> = self
            .selected_feature_tag_snapshots()
            .into_iter()
            .filter_map(|(feature, tags)| {
                tags.into_iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| (feature, key.to_string(), Some(v), None))
            })
            .collect();
        if entries.is_empty() {
            return;
        }

        for (feature, k, _before, _after) in &entries {
            if let Some(layer) = self.layer_manager.find_layer_mut(&feature.layer_name) {
                layer.remove_tag(feature.kind, feature.id, k);
            }
        }
        self.undo_stack.push(UndoableAction::SetTags { entries });
        cx.notify();
    }
```

- [ ] **Step 4: Run a build to check the new methods compile**

Run: `cargo build`
Expected: builds cleanly (methods added but not yet called from rendering).

- [ ] **Step 5: Rewrite `render_tags_section` with double-click rows, delete icon, and Add-tag button**

Replace `render_tags_section` (currently lines 1478-1515) with:

```rust
    /// The Tags accordion section: tags aggregated across every selected
    /// feature. A key with one distinct value (among features that have it)
    /// shows that value; a key with several shows "<N values>". Double-
    /// clicking the key or value opens the tag-edit dialog with that field
    /// pre-selected; the trailing "x" removes the tag immediately. An "Add
    /// tag" button below the list opens the same dialog with empty fields.
    fn render_tags_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use osm_gpui::ui::tag_edit_dialog::TagEditField;

        if self.selected.is_empty() {
            return Label::new("No selection.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let per_feature: Vec<Vec<(String, String)>> = self
            .selected
            .iter()
            .filter_map(|sel| {
                self.layer_manager
                    .find_layer(&sel.layer_name)
                    .and_then(|layer| layer.feature_tags(sel))
            })
            .collect();

        let aggregated = osm_gpui::selection::aggregate_tags(&per_feature);
        let selection = self.selected.clone();

        let mut list = div().flex().flex_col();

        if aggregated.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("(no tags)"),
            );
        } else {
            list = list.children(aggregated.into_iter().map(|(k, v)| {
                let value_text = match v {
                    osm_gpui::selection::TagValue::Single(s) => s,
                    osm_gpui::selection::TagValue::Multiple(n) => format!("<{} values>", n),
                };

                let key_for_key_click = k.clone();
                let value_for_key_click = value_text.clone();
                let selection_for_key_click = selection.clone();

                let key_for_value_click = k.clone();
                let value_for_value_click = value_text.clone();
                let selection_for_value_click = selection.clone();

                let key_for_delete = k.clone();

                div()
                    .id(("tag-row", k.clone()))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .cursor_pointer()
                            .child(k.clone())
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                                    if ev.click_count == 2 {
                                        this.pending_tag_edit_open = Some(PendingTagEditOpen {
                                            features: selection_for_key_click.clone(),
                                            original_key: key_for_key_click.clone(),
                                            original_value: value_for_key_click.clone(),
                                            select: TagEditField::Key,
                                            is_add: false,
                                        });
                                        cx.notify();
                                    }
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .cursor_pointer()
                            .child(value_text.clone())
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                                    if ev.click_count == 2 {
                                        this.pending_tag_edit_open = Some(PendingTagEditOpen {
                                            features: selection_for_value_click.clone(),
                                            original_key: key_for_value_click.clone(),
                                            original_value: value_for_value_click.clone(),
                                            select: TagEditField::Value,
                                            is_add: false,
                                        });
                                        cx.notify();
                                    }
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id(("tag-delete", k.clone()))
                            .cursor_pointer()
                            .text_color(cx.theme().muted_foreground)
                            .hover(|this| this.text_color(cx.theme().danger))
                            .child("x")
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                                    this.delete_tag(&key_for_delete, cx);
                                }),
                            ),
                    )
                    .into_any_element()
            }));
        }

        let add_selection = selection.clone();
        list.child(
            Button::new("add-tag")
                .label("Add tag")
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.pending_tag_edit_open = Some(PendingTagEditOpen {
                        features: add_selection.clone(),
                        original_key: String::new(),
                        original_value: String::new(),
                        select: TagEditField::None,
                        is_add: true,
                    });
                    cx.notify();
                })),
        )
        .into_any_element()
    }
```

Note this drops the `DescriptionList`/`DescriptionItem` usage entirely in favor of plain `div` rows (needed for the double-click handlers, which `DescriptionItem` doesn't support). Remove the now-unused `description_list::{DescriptionItem, DescriptionList}` import from the `gpui_component` use block (lines 22-28) **only if** nothing else in `main.rs` still uses them — check with `grep -n "DescriptionList\|DescriptionItem" src/main.rs` first; if another section still renders a `DescriptionList` (e.g. `render_selection_section` or elsewhere), keep the import.

- [ ] **Step 6: Wire `check_for_pending_tag_edit_dialog` into `render()` and update the call site**

`render_tags_section`'s call site (currently `let tags_section = self.render_tags_section(cx);` around line 1241) is unchanged — it still only takes `cx`, since it no longer constructs the dialog directly.

In `Render::render`, add a call to `check_for_pending_tag_edit_dialog` right after the existing `self.check_for_dialog_queue(window, cx);` (main.rs:1530):

```rust
        self.check_for_dialog_queue(window, cx);
        self.check_for_pending_tag_edit_dialog(window, cx);
```

Add the dialog to the render tree, next to the existing `.children(self.custom_imagery_dialog.clone())` (around line 1716):

```rust
            .children(self.custom_imagery_dialog.clone())
            .children(self.tag_edit_dialog.as_ref().map(|(dialog, _)| dialog.clone()))
```

- [ ] **Step 7: Run the full test suite and build**

Run: `cargo test`
Expected: PASS — all pre-existing tests plus every test added in Tasks 1-4.

Run: `cargo build`
Expected: builds cleanly with no warnings about unused imports (verify the `DescriptionList`/`DescriptionItem` import per Step 5's note; remove it if it's now dead).

- [ ] **Step 8: Commit**

```bash
git add src/main.rs
git commit -m "Wire tag-edit dialog into the Tags panel: double-click, add, delete"
```

---

## Final verification

- [ ] Run `cargo test` once more from a clean state and confirm all tests pass.
- [ ] Run `cargo build --release` to confirm no release-profile-only warnings/errors.
- [ ] Skim the diff (`git diff main...HEAD` or equivalent) against the spec's Scope section one more time: double-click open with pre-selected field, multi-select semantics, add-tag button, delete-tag icon, and undo/redo integration should all be present.
