# Undo/redo for mutation actions

## Goal

A global undo/redo stack for data-mutating actions, starting with the
drag-to-move feature. An Edit menu (Cmd-Z / Cmd-Shift-Z) drives undo/redo,
and the right panel shows a passive history list.

## Scope

- One global undo stack shared across all layers, in the order actions
  happened. There is no per-layer stack.
- Only one action kind exists today: `MoveNodes`, produced by committing a
  drag-to-move. The design leaves room for future mutation kinds (tag
  edits, deletes, etc.) as additional enum variants without restructuring
  the stack.
- The history list in the right panel is a passive display: it shows every
  action with the current stack position marked, and does not support
  clicking an entry to jump to it. Undo/redo only happen via the Edit menu
  or its keyboard shortcuts.
- Undo/redo never clear a layer's `modified` flag (added in the drag-to-move
  feature). That flag stays a one-way "has ever been edited this session"
  signal; it is not reconciled against the undo stack's position.
- Undo/redo when the stack has nothing in that direction is a silent no-op
  (GPUI can't disable menu items; this matches the existing
  `NoOpImageryInfo` precedent in this codebase for menu entries that can't
  always act).
- Pushing a new action after undoing discards any actions past the current
  cursor (standard undo-branch-discard behavior).

## Out of scope

- Persisting the undo stack across app restarts.
- Undo for anything other than node moves (no other mutation exists yet).
- Click-to-jump in the history list.
- A stack size cap (unbounded for this iteration).

## Data model

### `UndoableAction` (new, in `src/main.rs` or a new `src/undo.rs`)

```rust
enum UndoableAction {
    MoveNodes {
        /// Per layer: node id -> (before (lat, lon), after (lat, lon)).
        per_layer: Vec<(String, Vec<(i64, (f64, f64), (f64, f64))>)>,
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
        }
    }
}
```

Both the before and after position are stored so undo and redo are
symmetric: both call the existing `OsmLayer::commit_node_moves` (added in
the drag-to-move feature) with one side or the other. No new mutation
primitive is needed on `OsmLayer`.

### `UndoStack` (new)

```rust
struct UndoStack {
    actions: Vec<UndoableAction>,
    /// Index of the next action that would be redone. Equals
    /// `actions.len()` when at the tip (nothing to redo).
    cursor: usize,
}

impl UndoStack {
    fn push(&mut self, action: UndoableAction) {
        self.actions.truncate(self.cursor);
        self.actions.push(action);
        self.cursor = self.actions.len();
    }

    /// Returns the action to invert, and moves the cursor back.
    fn undo(&mut self) -> Option<&UndoableAction> {
        if self.cursor == 0 { return None; }
        self.cursor -= 1;
        Some(&self.actions[self.cursor])
    }

    /// Returns the action to reapply, and moves the cursor forward.
    fn redo(&mut self) -> Option<&UndoableAction> {
        if self.cursor == self.actions.len() { return None; }
        let action = &self.actions[self.cursor];
        self.cursor += 1;
        Some(action)
    }
}
```

`MapViewer` gains `undo_stack: UndoStack`.

## Wiring into the move-drag commit

`MapViewer::handle_mouse_up`'s move-commit branch already computes, per
affected layer, the node ids' before (`originals`) and after
(`new_lat`/`new_lon`) positions when building the `moves` list passed to
`commit_node_moves`. After committing (existing behavior unchanged), build
an `UndoableAction::MoveNodes` from that same before/after data and
`self.undo_stack.push(action)`.

## Applying undo/redo

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
    }
}

fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
    if let Some(action) = self.undo_stack.undo() {
        let action = action.clone(); // avoid holding the borrow across the &mut self call
        self.apply_undo_action(&action, false);
        cx.notify();
    }
}

fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
    if let Some(action) = self.undo_stack.redo() {
        let action = action.clone();
        self.apply_undo_action(&action, true);
        cx.notify();
    }
}
```

(`UndoableAction` derives `Clone` to make the borrow-avoidance above
trivial; the data is small.)

## Edit menu

New `Undo`/`Redo` actions (via the existing `actions!` macro) bound to
Cmd-Z / Cmd-Shift-Z via `KeyBinding::new`, alongside a new `Menu { name:
"Edit", items: [MenuItem::action("Undo", Undo), MenuItem::action("Redo",
Redo)] }` inserted into the existing `cx.set_menus(...)` list (after "File",
before "Imagery" — matches typical app menu ordering).

## Right panel: History section

A 4th collapsible section alongside Layers/Selection/Tags (same
`collapsible_section` mechanism, index 3, added to
`side_panel_open`'s default-open set or left closed by default — closed by
default since it's the newest/least-central section).

Content: for `i in 0..undo_stack.actions.len()`, a row with
`undo_stack.actions[i].description()`. Rows before `undo_stack.cursor`
render normally; the row at `cursor - 1` (the most recently applied action)
gets an accent-colored background to mark "you are here"; rows at index
`>= cursor` (available to redo) render dimmed/italic. Empty stack shows
"No actions yet." matching the empty-state style already used by the
Selection/Tags sections.

## Testing

- `UndoStack` unit tests (pure, no GPUI): push/undo/redo sequencing,
  truncation of redo-able actions on a new push after undo, undo/redo
  no-op at the respective ends of the stack.
- `UndoableAction::description()` unit test for singular/plural wording.
- No GUI automation available in this sandbox (documented limitation from
  the drag-to-move feature); the Edit menu wiring and history list
  rendering aren't exercised by tests, only the underlying stack logic.
