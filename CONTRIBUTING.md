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
cargo fmt --all -- --check

# Lint with strict warnings
cargo clippy --all-targets -- -D warnings
```

CI runs exactly these four checks, and a PR needs all of them green: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, `cargo test`. Run them all locally before pushing.

Release builds for performance testing:
```bash
cargo build --release
cargo run --release
```

Scripted screenshot sessions:
```bash
cargo run --release -- --script docs/screenshots/smoke.osmscript --window-size 1200x800
```

## Live test against the OSM dev server

The upload path is unit-tested against a fake HTTP client. `examples/dev_live.rs` runs the real code (changeset create, diff upload, close, conflict resolution, chunked uploads) against the OpenStreetMap development server, `https://master.apis.dev.openstreetmap.org`, a sandbox with its own accounts and database. It opens no window.

```bash
cargo run --example dev_live -- login                 # browser sign-in, once
cargo run --example dev_live -- run                   # all scenarios
cargo run --example dev_live -- run --only chunked    # one scenario
cargo run --example dev_live -- logout                # delete the saved token
```

You need an account on the dev server (it is separate from your openstreetmap.org account). `login` runs the app's OAuth2 PKCE flow with the dev server's built-in client id and the scopes `read_prefs write_api`, then prints your dev-server display name and the token file's path.

`run` prints one `PASS`/`FAIL` line per scenario with the changesets it opened (as dev-server URLs) and exits non-zero if any failed. Scenarios: `roundtrip` (create a node pair, a way and a relation with `& < > "` and non-ASCII tags, read them back byte for byte), `modify_delete`, `conflict_version` (stale edit, then "Use theirs" and "Keep mine"), `conflict_deleted` (including "Drop my change"), `chunked` (about 60 new nodes at a 25-element limit, so three changesets and ids carried between them) and `capabilities`.

What it writes, and where:

- On the dev server: a few dozen test nodes, ways and relations in open ocean (around lat -48, lon -123, shifted by up to 0.5 degrees per run), in changesets commented `osm-gpui automated live test (<scenario>)`. Each scenario deletes what it created afterwards, even when it fails; if the run is killed, leftovers are harmless sandbox data.
- On your disk: only `<config_dir>/osm-gpui/dev-live-token.json` (on macOS `~/Library/Application Support/osm-gpui/`), created with 0600 permissions. The harness never touches the platform keyring, `oauth.json` or `settings.json`, and never prints the token.

It can only talk to the dev server: the API base is a constant, there is no flag, environment variable or setting to change it, and every request passes a host check (`osm_gpui::dev_live::ensure_dev_url`) right before it is sent. A diff upload is attempted exactly once, as in the app.

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

### Localization

Every user-visible string goes through the catalog: add it to both `locales/en.yml` and `locales/de.yml` (a test enforces that the two have the same keys). Machine-drafted German is fine; say so in the PR.

### Changelog

Do not edit `CHANGELOG.md` in a feature PR. It is written at release time, one line per change with a link to its PR.

### Commit messages

Single-line messages in imperative present tense, e.g. "Add tag-edit dialog" rather than "Added tag-edit dialog".

### Design and architecture

- If your change alters app state flow, module boundaries, or the renderer hot path, open an issue or discussion first.
- For large features, see the [improvement-suggestions.md](docs/improvement-suggestions.md) roadmap.
