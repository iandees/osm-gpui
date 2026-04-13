# Issue 8: Allow reordering of the layers

## Context
The layer panel (src/main.rs, ~lines 915–988) renders a list of layer rows in
whatever order `LayerManager::layers()` returns — effectively insertion order.
Users cannot change that order today. The issue asks for drag-to-reorder with a
handle on the left.

Full drag-and-drop in gpui (`.on_drag()` / `.on_drop()`) works but is fiddly —
it requires a typed drag payload, a preview entity, and drop-target geometry
computation. For an overnight autonomous PR we accept a slightly reduced scope:
implement reordering via explicit **up/down arrow buttons** on the left of each
row. Users can still reorder any layer, the map re-renders immediately, and the
UX is fully keyboard-free. True click-and-drag reordering can ship later as a
refinement if the human wants it — the `LayerManager::move_layer` plumbing
added here is the same plumbing a drag impl would call.

## Approach
- `src/layers/mod.rs`:
  - Add `pub fn move_layer(&mut self, from: usize, to: usize)`. If either
    index is out of bounds, or `from == to`, return without mutating. Uses
    `Vec::remove` + `Vec::insert`, clamping `to` after the remove.

- `src/main.rs`:
  - Add `fn reorder_layer(&mut self, from: usize, to: usize)` on `MapViewer`
    that delegates to `self.layer_manager.move_layer(from, to)`.
  - In the layer row (lines ~923–988), add a small vertical stack of `▲` and
    `▼` buttons at the left of the row (before the existing checkbox/label
    group). Each button is its own `div` with `id(("layer-up"/"layer-down", index))`,
    `cursor_pointer`, and its own `on_mouse_down` listener that calls
    `this.reorder_layer(from, to)` and `cx.notify()`.
  - Disable (visually dim + no handler) the `▲` on index 0 and the `▼` on the
    last index.
  - Critical: the arrow buttons must not also trigger the row-level visibility
    toggle. gpui's mouse event model does not bubble child `on_mouse_down`
    handlers back to parents automatically here, but to be explicit we keep
    the arrow handlers on a separate child `div` that does not itself carry a
    toggle handler. (The row-level toggle is fine as long as the arrow handler
    fires first and handles the event — verify in testing.)

- No changes to `LayerManager` ordering semantics elsewhere.

## Verification
- `cargo build --release` clean.
- `cargo test --lib` passes, including a new unit test for `LayerManager::move_layer`
  covering: move down, move up, move to same index (no-op), out-of-bounds (no-op).
- Manually: add two layers, click `▼` on the top one — order swaps, map redraws.

## Out of scope
- Full mouse-drag handle reordering (deferred; plumbing is compatible).
- Keyboard shortcuts for reordering.
- Persisting layer order across runs.
- Any visual reskin of the layer list.
