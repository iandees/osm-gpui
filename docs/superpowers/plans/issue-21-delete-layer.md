# Issue 21: Delete layer entries via right-click

## Context

Layers are rendered as list items in the right panel of `MapViewer`
(`src/main.rs:1062–1187`). Each item has reorder buttons and a visibility
checkbox. The issue asks for layer deletion, placed on a right-click
context menu because there's no room for another button on the row.

There is no existing context-menu / popup pattern in the repo. We'll
build a minimal one scoped to just this feature: a small absolutely
positioned div that appears at the click point with a single "Delete"
entry, and dismisses on outside click or after selection.

## Approach

### State (`src/main.rs`)

- Extend `LayerRequest` (line 32) with
  `Delete { index: usize }` — index into the `LayerManager` `Vec`.
- On `MapViewer` (around line 196), add:
  `context_menu: Option<LayerContextMenu>` where
  `struct LayerContextMenu { layer_index: usize, position: Point<Pixels> }`.
  `position` is window coords so the menu can be placed absolutely.

### LayerManager (`src/layers/mod.rs`)

- `LayerManager::remove_at(&mut self, index: usize)` — bounds-checked;
  returns the removed layer so the caller can drop it explicitly.

### Request handling (`src/main.rs:477-509`)

- Match `LayerRequest::Delete { index }` → call
  `layer_manager.remove_at(index)`, call `cx.notify()` so the view
  re-renders. Also clear any `context_menu` state if it referenced the
  now-deleted or a shifted-index layer (simplest: clear unconditionally
  on any delete — the menu is already dismissed at this point anyway).

### Right-click handler (`src/main.rs` layer item div, ~line 1067)

- Add `.on_mouse_down(MouseButton::Right, cx.listener(move |this, event, _, cx| {
    this.context_menu = Some(LayerContextMenu { layer_index: index, position: event.position });
    cx.stop_propagation();
    cx.notify();
  }))`.

### Context menu rendering

- At the root `render` level (after the main layout, before `into_element`),
  if `self.context_menu.is_some()`, render an additional floating `div()`:
  - `.absolute()`, `.left(pos.x).top(pos.y)`.
  - Styled like a minimal menu: small padding, white/dark bg, 1px border,
    rounded corners, subtle shadow.
  - Single child: a `div()` "Delete" entry with
    `on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
      let index = this.context_menu.take().unwrap().layer_index;
      LAYER_REQUESTS.lock().unwrap().push(LayerRequest::Delete { index });
      cx.notify();
    }))`.
  - Hover style on that entry (matching existing list-item hover if any).
- Outside-click dismiss: also wire an
  `on_mouse_down(MouseButton::Left, …)` on the *outer* viewport /
  background that, if `context_menu.is_some()`, clears it. Precedence
  matters — the Delete-entry handler must `cx.stop_propagation()` so
  clicking the entry doesn't also trigger the outside-dismiss.

### Edge cases

- Right-clicking a layer with the menu already open on a different layer:
  the new right-click overwrites `context_menu` with the new layer index
  and position. Natural behavior.
- Right-clicking off the layer list: no new menu; existing menu dismisses
  via outside-click.
- Deleting the currently-selected/focused layer: nothing to clean up
  beyond `remove_at` — there is no separate "selected layer" state to
  invalidate.

## Verification

- `cargo build --release` (clean; no new warnings vs. main).
- `cargo test --lib`.
- Manual: launch, right-click a layer row → menu appears at click point
  with "Delete". Click Delete → layer vanishes from list. Open the menu
  again, click outside → menu dismisses without deleting.
- Smoke script: `cargo run --release -- --script docs/screenshots/smoke.osmscript` — app starts cleanly.

## Out of scope

- Undo / confirmation dialog.
- Keyboard shortcut (e.g. Delete key while a row is focused).
- Multi-select deletion.
- Reordering the menu to include other actions (rename, duplicate).
- Animations / fade-out.
- Any changes to the dead modules listed in *Repo gotchas*.
