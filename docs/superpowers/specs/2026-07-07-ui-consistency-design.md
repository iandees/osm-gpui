# UI Consistency Pass — Design

**Date:** 2026-07-07
**Scope decision:** Panes + app-wide drift (canvas paint colors excluded).

## Goal

Make the UI read as one system: unify the right pane's row and action styles, add icons to the left mode panel, and eliminate hardcoded-color/size drift across the app. No behavior changes.

## Current problems

1. **Right pane (`src/side_panel.rs`, `src/fields_section.rs`)** — three idioms for clickable actions in one pane: a real gpui-component `Button` ("Add tag"), a bare text-`div` "x" (tag delete), and `Label` + `on_mouse_down` divs ("Change feature type…", "Add field"). Row padding/rounding differs per section (layer rows `px_1` rounded, selection rows fixed 22px unrounded, tag rows `px_2` bordered, history rows `py_0p5`). `theme().accent` doubles as hover color and persistent selected background, so hover and selected are indistinguishable.
2. **Left pane (`src/mode_panel.rs`)** — text-only mode buttons ("Select"/"Add"/"Bldg"/"Extr"); the doc comment claiming no usable icons exist is outdated.
3. **App-wide drift** — root background hardcodes `rgb(0x1a202c)` (`src/main.rs`) instead of a theme token; debug/status/attribution overlays hardcode black/white; modal scrim literal `rgba(0x00000099)` repeated in `src/ui/modal.rs`, `src/ui/upload_dialog.rs`, `src/ui/nsi_dialog.rs`; the 280px side-panel width is duplicated as a bare literal in `main.rs`'s map-size math; section headers (SEMIBOLD `text_sm`) and dialog headers (BOLD default size) use two title styles.

## Design

### 1. Shared style module — `src/ui/style.rs`

New module exporting the blessed building blocks:

- `pub const SIDE_PANEL_WIDTH: f32 = 280.0;` (used via `px(SIDE_PANEL_WIDTH)`) and re-export/reference `MODE_PANEL_WIDTH` so `main.rs`'s map-size math uses named constants only.
- `pub fn scrim() -> Rgba` (or a const) — the single modal-scrim color, replacing the three `rgba(0x00000099)` literals.
- `pub fn panel_row(id: impl Into<ElementId>) -> Div` (approx signature; adapt to gpui idioms) — the one row style for all right-pane list rows: fixed height 24px, `px_2`, `rounded_md`, `gap_1`, `text_sm`.
- `pub fn row_states(row: Div, selected: bool, cx) -> Div` — applies the interaction states: hover = `theme().accent` at reduced alpha (e.g. `.opacity(0.5)`); selected = full `theme().accent`. Hover and selected become visually distinct; selected rows may also use `SEMIBOLD` text.

Convention (documented in a module doc comment): every clickable action is a gpui-component `Button` — `.primary()` for the section's main action, ghost/small for inline actions, ghost icon-only (xsmall) for per-row actions like delete. No more `Label`+`on_mouse_down` or bare-div buttons.

### 2. Right pane

- All five section bodies (Layers, Selection, Fields, Tags, History) build rows with `panel_row` + `row_states`. Section body padding standardized to `px_2().py_1p5()` (already what `collapsible_section` provides); per-row deviations removed.
- Tag delete "x" → ghost icon `Button` with `IconName::Close`, xsmall, `danger` hover treatment via Button styling.
- "Change feature type…" and "Add field" (`src/fields_section.rs`) → real `Button`s (ghost/small; "Add field" entries keep the `+` affordance via `IconName::Plus`).
- "Add tag" stays a `Button` (already correct); align its size/variant with the new convention.
- Section headers keep their current structure (chevron + SEMIBOLD `text_sm` label).

### 3. Left pane (mode panel)

- `mode_button` becomes icon-above-label: `Icon` centered above a `text_xs` label, button roughly 48px square, still gpui-component `Button` with `.primary()` active state and disabled behavior unchanged.
- Icon mapping: Select → `IconName::Frame`, Add → `IconName::Plus`, Building → `IconName::Building2`, Extrude → `IconName::LayoutDashboard` (nearest fits; labels disambiguate).
- Remove the stale doc comment about missing icons.

### 4. App-wide drift fixes

- Root background: `bg(rgb(0x1a202c))` → `bg(cx.theme().background)`.
- Debug overlay, status message, attribution overlays: hardcoded black bg / white text → `theme().popover` bg + `theme().popover_foreground` text (theme-aware in light and dark). Attribution link hover → theme link/accent token instead of `rgb(0xaad4ff)`.
- Modal scrim: all three dialogs use the shared scrim from `style.rs`.
- Dialog headers align font weight with section headers (SEMIBOLD); dialog padding/size otherwise unchanged.
- Map-size math in `main.rs` uses `SIDE_PANEL_WIDTH` / `MODE_PANEL_WIDTH` constants.
- Out of scope: canvas paint colors (drag-guide blue), MapCSS rendering colors.

### 5. UI tests

Following the repo's pure-logic convention (CONTRIBUTING.md):

- Extract pure helpers where they don't already exist and unit-test them: mode → (icon, label) mapping in `mode_panel.rs`; any row-state/color-selection logic in `style.rs` that is expressible as a pure function (e.g. selected/hover token choice).
- Commit a screenshot session script `docs/screenshots/ui-consistency.osmscript` that loads the existing fixture, clicks a feature to populate the right pane, and captures the full window — a repeatable manual-regression artifact for future UI work (checked-in PNGs updated in this PR).
- Golden-image diffing in CI is explicitly out of scope (flaky across machines); the script is for human/agent inspection.

### 6. Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- Screenshot harness: run `ui-consistency.osmscript` once against baseline (before changes) and once after; inspect PNGs side by side to confirm mode-panel icons, unified rows/buttons, and theme-token overlays. Each run is a single app launch (Keychain prompts ×2 per launch).

## Error handling

No new failure modes: this is styling and widget-idiom substitution. Button `on_click` handlers carry over the existing `on_mouse_down` logic unchanged.

## Risks

- gpui-component `Button` API details (ghost/icon variants, sizing) must be checked against the vendored source under `~/.cargo/git/checkouts/gpui-component-*/`; if a variant doesn't exist, fall back to the closest supported style rather than re-inventing a div-button.
- Shared target-dir stale-cache false errors across worktrees: touch + rebuild before trusting a red build.
