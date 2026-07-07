# Contributing to osm-gpui

Contributions are welcome. This document outlines prerequisites, build setup, and code conventions.

## Prerequisites

- **macOS** — the project uses GPUI, which is tightly coupled to Cocoa/Metal.
- **Rust stable** — install via [rustup](https://rustup.rs/).
- **Metal Toolchain** — required for compile-time Metal shader compilation:
  ```bash
  xcodebuild -downloadComponent MetalToolchain
  ```

## Optional: Out-of-tree build cache

`.cargo/config.toml` points `target-dir` to `~/.rust/osm-gpui/target` to keep build artifacts (~1 GB) out of Dropbox/Synology-synced folders. The `.cargo/` directory is gitignored because the path is user-specific. If cloning fresh on another machine, recreate it:

```toml
# .cargo/config.toml
[build]
target-dir = "/Users/<your-user>/.rust/osm-gpui/target"
```

(Adjust the user path as needed.)

## Build, run, test, and lint

```bash
# Build (debug)
cargo build

# Run the app
cargo run

# Run all tests (unit + integration)
cargo test

# Check formatting
cargo fmt --check

# Lint with strict warnings
cargo clippy --all-targets -- -D warnings
```

Release builds for performance testing:
```bash
cargo build --release
cargo run --release
```

Scripted screenshot sessions:
```bash
cargo run --release -- --script docs/screenshots/smoke.osmscript --window-size 1200x800
```

## Code conventions

### Pure logic and testability

Extract editing logic into testable modules with unit tests. Good examples:

- **`src/selection.rs`** — Feature hit-test, multi-select aggregation, and S-frame state all tested without a UI.
- **`src/undo.rs`** — Undo stack, action variants, and branching logic are pure and fully unit-tested.

Avoid large closures and complex state machines embedded in UI rendering code. Instead:
1. Extract pure `fn(input) -> output` logic.
2. Write unit tests for the pure function.
3. Call it from the UI or handler, passing the result to GPUI/state updates.

### Custom error types per module

Define a custom `Error` or `<Module>Error` enum for each module that can fail. Example:

```rust
#[derive(thiserror::Error, Debug)]
pub enum OsmParseError {
    #[error("XML parse failed: {0}")]
    XmlError(String),
    #[error("Invalid coordinate: {0}")]
    InvalidCoord(String),
}
```

Use `?` operator and `.map_err()` to propagate. Avoid generic `String` errors and bare `.unwrap()`.

### Avoid new global statics

- Static `OnceLock<T>` or `Mutex` used for initialization or once-computed values are acceptable (e.g., `TILE_DOWNLOAD_SEMAPHORE`).
- Stateful globals (e.g., ring buffers, caches with side effects) should live as `App` or layer fields instead.
- If a global is truly necessary, document why and add a comment explaining the initialization or cleanup strategy.

## Pull request guidance

### Keep PRs focused

- One feature/fix per PR.
- If a PR touches multiple concerns, break it into independent PRs or clearly separate commits.
- Avoid refactoring-in-the-large unless it's the stated goal.

### Tests for new pure logic

- If you add a new testable module or function (e.g., a new undo variant, a new style rule matcher), include unit tests.
- GUI/rendering changes: no test needed; verify with the running app or scripted screenshots.

### Commit messages

- Single-line messages in imperative present tense (e.g., "Add tag-edit dialog" not "Added tag-edit dialog").
- If the commit needs elaboration, add a blank line and paragraphs, but keep the first line ≤70 characters.

Example:
```
Fix click hit-test performance via R-tree indexing

geo_to_screen was re-projecting the viewport center on every call.
Use the cached mercator_center_x/y fields; convert click point to
mercator envelope; query R-trees for candidates.

Measured: 1.9 ms → 0.012 ms per click on 106k-node dataset.
```

### Design and architecture

- If your change alters app state flow, module boundaries, or the renderer hot path, open an issue or discussion first.
- For large features (e.g., upload, relation rendering), refer to the [improvement-suggestions.md](docs/improvement-suggestions.md) roadmap and suggested sequencing.
- Check the "Dead code" section of [README.md](README.md) before wiring up old modules — some are candidates for deletion.
