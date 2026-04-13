# Issue 14: Render tags as a table

## Context
`MapViewer::render_selection_panel` (src/main.rs ~624–707) currently renders
the selected feature's tags as a `flex-col` of `flex-row` pairs, with the key
in grey and the value in white, separated only by a small gap. There are no
borders, no column alignment, and no header — it reads as a text list.

The issue asks for a table-like presentation as a stepping stone to editable
tags. The minimal table requirements: a header row labelling the two columns,
aligned key and value columns across rows, and visible row/column separators
so the layout reads as tabular.

## Approach
- `src/main.rs`, inside `render_selection_panel`, replace the `tags_block`
  construction (roughly lines 674–704) with a table-style element:
  - Outer `div().flex().flex_col()` with a top/bottom border and rounded
    corners so the whole block reads as a panel.
  - Header row: `flex-row` of two cells ("Key", "Value"), bolder text, a
    subtle background tint (e.g. `rgb(0x111827)`) and a bottom border.
  - Body rows: one `flex-row` per tag, each containing two cells with a right
    border on the key cell and a bottom border (except the last row). Use a
    fixed-ish `min_w` / `w` on the key column (~35%) so values align.
  - Keep existing colours for key (grey) and value (white).
  - Replace the "(no tags)" branch with the same framed container showing a
    single empty state row.

## Verification
- `cargo build --release` clean.
- `cargo test --lib` green.
- Manual (document in the PR test plan): click a feature; tags appear as a
  two-column table with "Key" / "Value" headers and visible grid lines.

## Out of scope
- Making any cell editable (explicitly called out as a follow-up).
- Sorting, filtering, or reordering tags.
- Multi-select or bulk edit UX.
- Any change to how `feature_tags` is collected.
