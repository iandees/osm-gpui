# Contributing to osm-gpui

Contributions are welcome. This document covers getting the code, building it, making changes, what a pull request should look like, and how releases are made.

## Getting the code

```bash
git clone https://github.com/iandees/osm-gpui.git
cd osm-gpui
```

For a change, fork the repository (or create a branch if you have access) and work on a descriptively named branch off `main`.

## Prerequisites

You need a stable Rust toolchain ([rustup](https://rustup.rs/)). GPUI is pulled from the Zed git repository, so the first build takes several minutes. macOS is the primary and best-tested platform.

- **macOS:** Xcode and its Metal Toolchain, which GPUI needs to compile shaders. If the build fails with `cannot execute tool 'metal' due to missing Metal Toolchain`, run:
  ```bash
  xcodebuild -downloadComponent MetalToolchain
  ```
- **Windows:** the MSVC toolchain (`x86_64-pc-windows-msvc`).
- **Linux:** a C toolchain, `pkg-config`, development packages for Wayland, X11/xcb, xkbcommon, fontconfig, ALSA, OpenSSL and zstd, and a Vulkan loader.

On Debian and Ubuntu (the list the release workflow installs on Ubuntu 22.04):

```bash
sudo apt-get install build-essential clang cmake pkg-config \
  libasound2-dev libfontconfig-dev libwayland-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev libx11-xcb-dev \
  libssl-dev libzstd-dev libvulkan1 libdrm-dev libgbm-dev
```

The equivalent packages on other distributions have not been tested here:

```bash
# Fedora
sudo dnf install gcc gcc-c++ clang cmake pkgconf-pkg-config \
  alsa-lib-devel fontconfig-devel wayland-devel \
  libxkbcommon-devel libxkbcommon-x11-devel libxcb-devel \
  openssl-devel libzstd-devel vulkan-loader libdrm-devel mesa-libgbm-devel

# Arch
sudo pacman -S base-devel clang cmake alsa-lib fontconfig wayland \
  libxkbcommon libxkbcommon-x11 libxcb openssl zstd vulkan-icd-loader libdrm mesa
```

A build error such as `The system library 'wayland-client' required by crate 'wayland-sys' was not found`, or one naming `fontconfig.pc` or `xkbcommon.pc`, means one of these packages is missing.

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

The app always starts in the light theme (it does not follow the system appearance). To check the dark theme without the Settings window, start with `--theme dark` (or `--theme light`), or switch inside a script with `action SetThemeDark` / `action SetThemeLight` (see `docs/screenshots/interface-polish.osmscript`).

## Developer tooling

**Scripted UI runs.** `--script <file>` runs a line-oriented `.osmscript` file (viewport, click, drag, scroll, key, load_osm, wait_idle, capture, and so on) against the live app and writes PNGs, which is useful for checking UI changes without driving the app by hand. Every op is listed in [docs/osmscript.md](docs/osmscript.md); examples are in `docs/screenshots/`, and `docs/screenshots/smoke.osmscript` is a short smoke test. The scripts under `tests/ui/` are run by the `#[ignore]`d tests in `tests/ui_scripts.rs` (they need the Keychain). Every checked-in script is parsed by `cargo test`. Other flags are `--window-size WxH`, `--keep-open` and `--language <tag>`. Windows release builds have no console, so run the harness from a debug build there to see its output.

```
viewport 47.6 -122.3 17      # centre the map (LAT LON ZOOM)
click geo:47.6,-122.3        # a position is X,Y | map:X,Y | geo:LAT,LON
drag geo:47.6,-122.3 geo:47.6,-122.2 button=left   # without button=left it pans
key cmd+z
capture out/shot.png
```

Dragging a node needs it to be the only thing moved: a press on an unselected vertex of a hovered way grabs just that node, but a press on one of several selected nodes moves them all (and never snaps). Undo selects what it touched, so press `escape` after `key cmd+z` before dragging again.

**Offline runs.** Add `--offline` to a `--script` run to deny every HTTP request (map tiles, imagery index, name suggestions, server capabilities) so the run is deterministic. `--http-fixtures <file.json>` answers chosen URLs from local files instead; see `docs/screenshots/fixtures/offline/http-fixtures.json` and the intro of `docs/osmscript.md`. Set `OSM_GPUI_TAGINFO_FIXTURE` to a JSON file (`{"<text in the request URL>": [<taginfo data entries>]}`, see `docs/screenshots/fixtures/taginfo-answers.json`) to answer taginfo lookups, which are otherwise off in scripted runs.

**In-window menu bar.** Windows and Linux have no native menu bar from GPUI, so the app draws one as the top row of the window, built from the same menu definition as the macOS menu. To see it on macOS, set `OSM_GPUI_IN_WINDOW_MENU=1` (`docs/screenshots/in-window-menu.osmscript` drives it). Off macOS the menus drop the macOS-only items and the app-name menu: Quit moves to the end of File, Settings to the end of Edit, About to the end of Help. Set `OSM_GPUI_IN_WINDOW_MENU=other` to preview that layout on macOS (`docs/screenshots/in-window-menu-other.osmscript`).

**Presets.** The iD tagging schema data in `assets/presets/` is refreshed with `cargo run --example update_presets [-- <schema-version>]` (default: the version in `assets/presets/LICENSE`), which also fetches the referenced preset icons from the upstream releases pinned in the example. It is not part of CI; review the resulting diff by hand.

## Translations

osm-gpui is available in English and German. English is the source language and the fallback for anything a catalog lacks. The German text is machine-drafted and has not been reviewed by a native speaker, so corrections and new languages are especially welcome and need no Rust knowledge.

### Interface text

Interface strings live in `locales/<tag>.yml`, one file per language named by its locale tag (`en`, `de`, `pt-BR`). The catalogs are compiled into the binary.

- **Fix or improve a translation:** edit the value in `locales/<tag>.yml`. Keep the keys and any `%{placeholder}` names exactly as they are, and keep `{z}`, `{x}` and `{y}` literal where they appear.
- **Add a language:** copy `locales/en.yml` to `locales/<tag>.yml`, translate the values, and set `meta.language_name` to the language's name written in that language. It then appears in Settings > Language. Test it with `cargo run -- --language <tag>`.
- **Plurals:** plural messages have one key per plural form (`one`, `other`, and so on). Provide the forms your language needs.
- **Whole sentences:** don't build a sentence from translated fragments. Give the translator the whole sentence with named placeholders.
- **Not translated:** tag keys and values, layer names, the debug overlay, and Name Suggestion Index brand names stay as they are.

When you add a new user-visible string in code, add it to both `locales/en.yml` and `locales/de.yml`; a test requires the two to have the same keys. Machine-drafted German is fine, but say so in the PR.

### Presets

Preset names and field labels come from the iD tagging schema and are translated separately, in `assets/presets/translations/<locale>.json`. These files are generated from the upstream translations rather than edited by hand. To add a locale:

```bash
cargo run --example update_preset_translations -- <schema-version> <locale>
```

Use the schema version recorded in `assets/presets/LICENSE`. Improvements to the preset wording itself belong upstream in the [iD tagging schema](https://github.com/openstreetmap/id-tagging-schema).

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

Define a custom `Error` or `<Module>Error` enum for each module that can fail. The project does not use `thiserror`: write `Display` and `std::error::Error` by hand, as `autosave::RecoverError` does:

```rust
#[derive(Debug)]
pub enum RecoverError {
    /// The layer's `.osm` file could not be read.
    Read(std::io::Error),
    /// The file is not valid OSM XML.
    Parse(OsmParseError),
}

impl std::fmt::Display for RecoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecoverError::Read(e) => write!(f, "{e}"),
            RecoverError::Parse(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RecoverError {}
```

A module that makes HTTP requests also implements `From<HttpError>` for its error, built on `http::HttpFailure` (the status / transport / cancelled split with the body already decoded), so the mapping lives in one place and call sites just use `?`.

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

Every user-visible string goes through the catalog (see [Translations](#translations)): add it to both `locales/en.yml` and `locales/de.yml` (a test enforces that the two have the same keys). Machine-drafted German is fine; say so in the PR.

### Changelog

Do not edit `CHANGELOG.md` in a feature PR. It is written at release time, one line per change with a link to its PR.

### Commit messages

Single-line messages in imperative present tense, e.g. "Add tag-edit dialog" rather than "Added tag-edit dialog".

### Design and architecture

- If your change alters app state flow, module boundaries, or the renderer hot path, open an issue or discussion first.
- For large features, see the [improvement-suggestions.md](docs/improvement-suggestions.md) roadmap.

## Releases

The version is `version` in `Cargo.toml`; it is also used in the HTTP User-Agent and the changeset `created_by` tag.

1. Bump the version in `Cargo.toml`.
2. Write the `## X.Y.Z` section of `CHANGELOG.md` from the pull requests merged since the last tag: one line each, with a link.
3. Merge to `main`.
4. Push a matching tag: `git tag vX.Y.Z` then `git push origin vX.Y.Z`.

`.github/workflows/release.yml` builds the macOS, Windows and Linux packages and publishes a GitHub Release with checksums, using the changelog section as the release notes. To build the macOS app locally, run `cargo build --release` then `scripts/bundle-macos.sh`, which writes `dist/osm-gpui.app`.
