# osm-gpui

An experimental OpenStreetMap editor written in Rust on [GPUI](https://github.com/zed-industries/zed), the UI framework behind Zed. The goal is a JOSM-like editor with efficient rendering and a native feel, especially on macOS.

This is early software. It can upload edits to the live OpenStreetMap database, so review your changes carefully before uploading, and consider trying the OSM dev server first (Settings > Account).

## Features

- Download from the OSM API or open `.osm` files, and edit in layers over raster imagery.
- Select, Add, Building, Extrude, Arc and Improve Way modes, plus Square, Circularize, Split, Join, Merge and other selection actions.
- Tag editing driven by the [iD tagging schema](https://github.com/openstreetmap/id-tagging-schema) presets, with Name Suggestion Index support.
- Relation editing, with multipolygon, route and turn restriction rendering.
- Undo and redo for every edit.
- Upload via OpenStreetMap OAuth, or export to a JOSM-compatible `.osm` file.
- Customizable keyboard shortcuts; English and German interface.

## Using it

See the [user guide](docs/user-guide.md) for signing in, downloading data, editing, validating and uploading.

## Download

Get a build for macOS, Windows or Linux from the [releases page](https://github.com/iandees/osm-gpui/releases). macOS is the primary and best-tested platform.

The macOS app is only ad-hoc signed, so Gatekeeper blocks the first launch of a downloaded copy. Right-click the app and choose Open, or run `xattr -dr com.apple.quarantine osm-gpui.app`.

## Development

Install a stable Rust toolchain ([rustup](https://rustup.rs/)) and run:

```bash
cargo run --release
```

The first build takes several minutes because GPUI is built from the Zed repository. macOS needs Xcode with the Metal Toolchain; Linux needs a C toolchain and the Wayland, X11, xkbcommon, fontconfig, ALSA, OpenSSL, zstd and Vulkan development packages.

Before opening a pull request, run the checks CI runs: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build` and `cargo test`. See [CONTRIBUTING.md](CONTRIBUTING.md) for conventions, and [docs/osmscript.md](docs/osmscript.md) for the scripted UI harness.

## License

Licensed under the [MIT license](LICENSE). The bundled iD tagging schema data and preset icons have their own licenses; see `assets/presets/LICENSE` and `assets/presets/icons/LICENSES.md`.
