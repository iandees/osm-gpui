# Custom Imagery Layers — Design

GitHub issue: [#36](https://github.com/iandees/osm-gpui/issues/36).

Add a way for the user to define arbitrary TMS imagery layers by name, URL template, and zoom range, use them as map layers, and have them persist across app restarts.

## User flow

1. The `Imagery` menu gains a top-level `Custom Imagery` submenu at the very top, containing:
   - `Add…` — opens the custom-imagery dialog.
   - A separator and one menu item per saved entry, in insertion order (omitted if none saved).
2. `Add…` opens a modal dialog with fields **Name**, **URL template**, **Min zoom**, **Max zoom**, plus `Cancel` and `Add` buttons.
3. Submitting a valid entry:
   - Appends it to the persisted store on disk.
   - Rebuilds the Imagery menu so the entry appears in the submenu.
   - Adds it to the map as an imagery layer (via the existing `LayerRequest::Imagery` path).
   - Closes the dialog.
4. Clicking a saved entry in the submenu adds it to the map immediately; no dialog opens. Editing and deletion are out of scope for this issue — a future settings window will cover them.
5. `Esc` or `Cancel` closes the dialog without changes. Clicks outside the modal frame do not dismiss it (protects against accidental loss while typing).

## Validation

Applied on `Add`; the first failing field shows an inline red error line beneath it, and the dialog stays open.

- **Name** — required, non-empty after trimming whitespace.
- **URL template** — required; must contain `{z}`, `{x}`, and exactly one of `{y}` or `{-y}`.
- **Min zoom** — blank → `0`; otherwise parses as `u32` in `0..=24`.
- **Max zoom** — blank → `19`; otherwise parses as `u32` in `0..=24`; must be `>= min_zoom`.

Duplicate names are allowed. Users clean up duplicates via the future settings window.

## Persistence

New module `src/custom_imagery_store.rs`.

- **Location:** `<dirs::config_dir()>/osm-gpui/custom-imagery.json`. If `dirs::config_dir()` returns `None`, persistence is silently disabled: reads return empty, writes are no-ops. A new `dirs` dependency is added to `Cargo.toml`.
- **Format:** JSON array of objects:
  ```json
  [
    {"name": "Example", "url_template": "https://…/{z}/{x}/{y}.png", "min_zoom": 0, "max_zoom": 19}
  ]
  ```
  Default min/max zooms are materialised at save time so every record has numeric fields.
- **API:**
  - `load() -> Vec<CustomImageryEntry>` — returns `Vec::new()` on missing file, unreadable file, or parse error, logging to stderr. Never panics.
  - `save(&[CustomImageryEntry]) -> std::io::Result<()>` — writes to a sibling temp file (`custom-imagery.json.tmp`) then `rename`s into place to avoid partial-write corruption on crash.
- **In-memory:** a new `CUSTOM_IMAGERY_STORE: OnceLock<Arc<Mutex<Vec<CustomImageryEntry>>>>` holds the current list. Populated at startup from `load()`. Mutations go through a helper that updates the mutex and calls `save()` synchronously (the file is tiny; UI-thread writes are acceptable).

## Dialog implementation

GPUI ships no ready-made text input, so the feature adds a minimal one plus a reusable modal shell. All three live under a new `src/ui/` module (with `mod.rs` re-exporting).

### `src/ui/text_input.rs` — minimal single-line text field

A GPUI `Entity` with state: `content: String`, `cursor: usize` (byte offset into `content`), `focused: bool`, `placeholder: String`.

- Key handling via GPUI `KeyDownEvent`: printable characters, Backspace, ArrowLeft, ArrowRight, Home, End.
- No selection model, clipboard support, or IME integration — intentionally minimal for this issue. These can be added later without changing the public API.
- Rendered as a bordered `div` of fixed height; focused state tints the background and shows a solid caret positioned by measuring the substring before the cursor.
- Mouse click inside the field transfers focus to that field.

### `src/ui/modal.rs` — reusable modal shell

Caller-agnostic, intended for reuse beyond this feature.

- Renders a backdrop covering the window (catches clicks, but does **not** dismiss — that's the caller's job via `Cancel`/`Esc`) with a centered frame above it.
- Accepts: `title: SharedString`, `body: impl IntoElement`, `footer: impl IntoElement`.
- Purely visual chrome — the caller owns Esc handling, focus cycling, and all key events. This keeps `Modal` simple and avoids coupling it to a specific event-handling strategy.

### `src/ui/custom_imagery_dialog.rs` — this feature

Composes `Modal` with a body containing four `TextInput` entities plus an optional inline error label, and a footer with `Cancel` and `Add` buttons. Owns the validation logic.

- On `Add`: runs validation. On success, emits `on_submit(CustomImageryEntry)` to the main view. On failure, updates an error slot and re-renders.
- `on_submit` in the main view: appends to `CUSTOM_IMAGERY_STORE`, persists, enqueues a `LayerRequest::Imagery`, triggers `rebuild_menus`, and clears `custom_imagery_dialog` back to `None`.

## `main.rs` integration

- **New actions** (via the existing `Action` derive pattern):
  - `AddCustomImagery` — zero-arg; opens the dialog.
  - `AddSavedCustomImagery { index: usize }` — adds the saved entry at that index as a layer.
- **Menu rebuild (`rebuild_menus`)**: prepend a `MenuItem::submenu` named `Custom Imagery` to `imagery_items`. The submenu's items are `MenuItem::action("Add…", AddCustomImagery)`, a separator (only if `store` is non-empty), then one `MenuItem::action(entry.name.clone(), AddSavedCustomImagery { index })` per entry.
- **Dialog entity on `OsmViewer`**: add `custom_imagery_dialog: Option<Entity<CustomImageryDialog>>`. The render path draws it on top of the map when `Some`.
- **Open-dialog plumbing**: follows the existing action→queue pattern. `AddCustomImagery` pushes onto a new `OPEN_CUSTOM_IMAGERY_DIALOG: OnceLock<Arc<Mutex<Vec<()>>>>`; the render tick drains the queue and instantiates the dialog entity into `custom_imagery_dialog`.
- **Saved-entry click**: `AddSavedCustomImagery` handler reads `CUSTOM_IMAGERY_STORE` at the given index and enqueues `LayerRequest::Imagery { name, url_template, min_zoom: Some(min), max_zoom: Some(max) }`, reusing the existing handler at `src/main.rs:515`.
- **Menu refresh after mutation**: `maybe_rebuild_imagery_menu` currently rebuilds when the viewport center moves or the ELI load state changes. Extend it to also track a "custom store dirty" flag set by the add path, so a newly saved entry shows up immediately without requiring a pan.
- **Startup**: call `custom_imagery_store::load()` synchronously before the first `rebuild_menus`, and stash the result in `CUSTOM_IMAGERY_STORE`. Keep the existing ELI fetch on a background thread.

## Files touched

New:

- `src/custom_imagery_store.rs`
- `src/ui/mod.rs`
- `src/ui/text_input.rs`
- `src/ui/modal.rs`
- `src/ui/custom_imagery_dialog.rs`

Modified:

- `src/lib.rs` — declare `custom_imagery_store` and `ui` modules.
- `src/main.rs` — new actions, menu wiring, dialog lifecycle on `OsmViewer`, startup load.
- `Cargo.toml` — add `dirs` dependency.

## Testing

- **Unit tests — `custom_imagery_store`:**
  - Round-trip save then load returns the same entries.
  - `load()` on a missing file returns `Vec::new()` without error.
  - `load()` on a corrupt JSON file returns `Vec::new()` and logs.
  - `save()` uses temp-file + rename (verify by observing no partial-file state mid-write; on Unix, rename is atomic).
- **Unit tests — validation helper** (factored out of the dialog):
  - Accepts valid `{z}/{x}/{y}` and `{z}/{x}/{-y}` templates; rejects missing placeholders; rejects both-present.
  - Zoom parsing: blank defaults, out-of-range rejected, `min > max` rejected.
- **Manual test plan** (no UI harness in repo):
  - Open `Imagery > Custom Imagery > Add…`, submit empty → name error visible, dialog stays open.
  - Submit with invalid template → template error visible.
  - Submit with valid data → dialog closes, layer appears on map, entry appears under `Custom Imagery`.
  - Click the saved entry → a second copy of the layer is added.
  - Quit and relaunch → saved entry persists and is clickable; clicking still adds the layer.
  - Corrupt `custom-imagery.json` by hand → app launches with no saved entries and logs a parse-error line.

## Out of scope

- Editing or deleting saved entries (future settings window).
- WMS, Bing, and other non-TMS imagery types.
- Rich text-input features (selection, clipboard, IME, multi-line).
- Cloud sync or export/import of the saved list.
