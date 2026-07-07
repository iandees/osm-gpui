# Dead Module Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the five source files the README already lists as dead, and update the README's "Dead code" section and "Not implemented" list to match.

**Architecture:** None of `src/map.rs`, `src/mercator.rs`, `src/background.rs`, `src/http_image_loader.rs`, `src/data.rs` are referenced by any `mod` declaration in `src/lib.rs` or `src/main.rs` (verified by `grep -rn "mod map\b\|mod mercator\b\|mod background\b\|mod http_image_loader\b\|mod data\b" src/` returning nothing) — they are orphaned files cargo never compiles, not live dead-code paths. This is simpler than the README implies ("these files compile but aren't wired into the app" is stale wording): deletion is a pure file removal with no `mod`/`use` edits required anywhere else.

**Tech Stack:** None — no code changes beyond deletion, no dependency changes (`reqwest`, used only by the two deleted files that reference it, was never in `Cargo.toml` to begin with — those files never compiled as part of this crate).

## Global Constraints

- Single-line git commit messages, no `Co-Authored-By` trailer.
- `cargo build`, `cargo clippy`, `cargo test` must stay clean/green after deletion (expected: identical output to before, since these files were never compiled).
- Do not delete or modify any other file's content beyond the README edit in Task 2.

---

### Task 1: Delete the five dead source files

**Files:**
- Delete: `src/map.rs`
- Delete: `src/mercator.rs`
- Delete: `src/background.rs`
- Delete: `src/http_image_loader.rs`
- Delete: `src/data.rs`

**Interfaces:** None — no other file imports or references these.

- [ ] **Step 1: Confirm nothing references them (sanity check before deleting)**

Run: `grep -rn "mod map\b\|mod mercator\b\|mod background\b\|mod http_image_loader\b\|mod data\b" src/`
Expected: no output (confirms these are orphaned, matching the Architecture note above). If this produces any output, STOP — something references one of these files and it is not safe to delete without further investigation; do not proceed with this task.

- [ ] **Step 2: Delete the files**

```bash
git rm src/map.rs src/mercator.rs src/background.rs src/http_image_loader.rs src/data.rs
```

- [ ] **Step 3: Build and test**

Run: `cargo build`
Expected: succeeds, identical warning/error output to before deletion (these files weren't part of the build graph, so nothing should change).

Run: `cargo test`
Expected: same pass count as before deletion (no tests lived in these files' `mod tests` blocks that were part of the build — confirm this by checking `grep -l "#\[test\]" src/map.rs src/mercator.rs src/background.rs src/http_image_loader.rs src/data.rs` against the pre-deletion working tree if unsure; since these files were never compiled, any `#[test]` in them was already dead and never ran).

Run: `cargo clippy`
Expected: clean, same as before.

- [ ] **Step 4: Commit**

```bash
git commit -m "Delete orphaned dead-code modules"
```

---

### Task 2: Update README to match

**Files:**
- Modify: `README.md`

**Interfaces:** None.

- [ ] **Step 1: Remove the "Dead code" table**

In `README.md`, delete the entire section from `### Dead code — do not extend without asking` through the table listing `src/map.rs` / `src/mercator.rs` / `src/background.rs` / `src/http_image_loader.rs` / `src/data.rs` / `examples/`, including its introductory sentence. If `examples/` (mentioned in that table as "empty/stale") still exists as an actual directory, leave a one-line note in its place: "`examples/` is empty/stale and not part of the build." If `examples/` doesn't exist, drop the whole section with no replacement.

- [ ] **Step 2: Update the "Not implemented" bullet mentioning GeoJSON**

Find the line `- GeoJSON loading in the UI (code exists in \`src/data.rs\` but is dead).` under `## Status (honest)` → `**Not implemented**` and remove it (the code no longer exists to reference). If GeoJSON loading is still a desired future feature, it's already implicitly covered by nothing in the roadmap needing this specific stale pointer — do not add a replacement bullet; this plan only removes stale references, it doesn't re-scope future work.

- [ ] **Step 3: Verify no other README references to the deleted files remain**

Run: `grep -n "map\.rs\|mercator\.rs\|background\.rs\|http_image_loader\.rs\|data\.rs" README.md`
Expected: no output. If any remain, remove or update that line so the README doesn't point at files that no longer exist.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "Update README after dead-module cleanup"
```
