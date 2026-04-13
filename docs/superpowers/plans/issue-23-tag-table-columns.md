# Issue 23: Tag table has staggered columns when text is too long

## Context
The tag table is built in `src/main.rs` in `render_selection_panel` (lines 711-849). The header row (lines 765-792) and each data row (lines 815-841) use a key cell with `.w(px(110.0))` and a value cell with `.flex_1()`. The outer `table` container (lines 794-801) is `flex_col` with `overflow_hidden()`. The problem: inside a flex row, a `flex_1()` child has an intrinsic min-width equal to its content width, so a long unbroken value (e.g. the `name` tag) forces that row's value cell wider than other rows'. Since each row is an independent `flex_row` sibling inside a `flex_col`, nothing constrains the rows to share a common width, so rows end up with mismatched column widths and stagger visually.

## Approach
- Pin every row (header + data) to the same outer width by adding `.w_full()` to each `flex_row`, so they all stretch to the same container width rather than shrinking/growing to content.
- Add `.min_w_0()` to the value cell (`flex_1()` child) in both header row (line 783) and data rows (line 832). This lets the flex item shrink below its intrinsic content width, which is the root cause of the stagger.
- Add overflow control to the value cell: `.overflow_hidden()` plus `.text_ellipsis()` so long single-token values (URLs, long names) truncate cleanly at the column edge. Keep `.text_sm()` as-is.
- Optionally also add `.min_w_0()` on each row's flex container so its children participate in shrinking. Leave the key cell at fixed `w(px(110.0))` — that column is already well-behaved.
- Factor the repeated cell widths into local `let key_w = px(110.0);` at the top of `tags_block` (minor cleanup while we're touching it) — not strictly required.

## Verification
- cargo build --release
- cargo test --lib
- Manual: load a feature with a long `name` or `website` tag and confirm: (a) all rows' key/value column boundaries align vertically, (b) long values truncate with ellipsis inside the value cell rather than pushing the row wider, (c) the panel itself doesn't grow horizontally.

## Out of scope
- Resizable columns, tooltip-on-hover to reveal truncated text, copy-to-clipboard on cells, multi-line wrap for values, switching to a real grid primitive. A follow-up could add hover-to-reveal-full-value if truncation proves annoying.
