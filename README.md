# osm-gpui

An experimental OpenStreetMap editor written in Rust on [GPUI](https://github.com/zed-industries/zed), the UI framework behind Zed. The goal is a JOSM-like editor with more efficient rendering and a native feel, especially on macOS.

This is early software. It can upload edits to the live OpenStreetMap database, so review your changes carefully before uploading, and consider trying it against the OSM dev server first (Settings > Account).

## Features

**Viewing and imagery**
- Pan and zoom with trackpad scroll and pinch, mouse wheel, drags, and the keyboard.
- Raster imagery from OpenStreetMap Carto, the [Editor Layer Index](https://github.com/osmlab/editor-layer-index) (filtered to the current view), and custom tile URL templates you add yourself. Attribution is shown on the map, and tiles are cached on disk within a size limit you can set and clear in Settings > Imagery.
- A lat/lon grid layer, and a layer list in the side panel with per-layer visibility, ordering, and an "active layer" that the editing tools write into.
- OSM data is drawn for tracing over imagery: thin cased ways, round node markers, direction arrows, area fills, and hover highlighting.

**Editing modes**
- Select: click, Shift-click, box-select, and double-click inside a closed way. Drag selected nodes to move them.
- Add: draw nodes and ways, snapping new nodes onto existing ways.
- Building: draw a rectangular building (three clicks), or align one to a selected squared way.
- Extrude: drag a way segment to reshape the way (pull out an L, push in a notch); Cmd/Ctrl-drag moves the segment, Alt-drag makes a new way.
- Arc: click the start, click the end, then move the mouse to bend the arc and click to finish; nodes are placed every 5° along the circle.

**Selection actions**
- Square, Circularize, Reverse Way, Join Ways, Separate Node, Extend Way, Split Way, Merge Nodes, Straighten Way, Copy Tags, Paste Tags, Add to Relation, and Delete.

**Tags and presets**
- Tags are edited in the side panel. Fields come from the bundled [iD tagging schema](https://github.com/openstreetmap/id-tagging-schema) preset that matches the feature. Edit > Change Feature Type picks a preset, and Edit > Apply NSI Preset applies a Name Suggestion Index brand.

**Relations**
- A node's or way's relation memberships are listed in the side panel. Selecting a relation highlights its members on the map and lets you edit its tags, member roles and order, and remove members.
- New Relation creates a relation from the selection, Add to Relation appends the selection to a relation chosen from a search dialog, and Delete removes a selected relation without touching its members.
- Split, Merge, Join, and Delete keep relation memberships consistent. Modified relations are included in uploads and exports.
- Multipolygon and boundary relations are drawn as filled areas with holes. Double-click inside one to select the relation.
- Route relations are drawn as a translucent coloured line under each member way (the relation's `colour` tag, else a colour by route type; a way in several routes shows the lowest relation id's). Turn restrictions get a small marker beside the via node (red ring for `no_*`, blue disc for `only_*`) at zoom 16 and closer; click it to select the restriction.

**Data**
- On first launch the map opens on OpenStreetMap Carto with a welcome card offering Download and Open.
- Open `.osm` XML files, and download the current view from the OSM API, up to the area the server allows (0.25 square degrees on openstreetmap.org). Downloads show progress, can be cancelled, merge into the active layer and keep local edits.
- The area outside the downloaded data is dimmed, so it is clear where the map is empty and where it simply has not been downloaded.
- Sign in with OpenStreetMap OAuth (tokens go in the platform keychain) and upload your edits as a changeset after a review step, with progress shown while it uploads. Edits larger than the server's changeset limit are sent as several changesets.
- If the server refuses an upload because someone else changed an object, the dialog names the object and offers to keep your version or take the server's.
- Ways and relations larger than the server allows are refused before upload, and edits that would exceed those limits are refused as you make them.
- Export a layer to a JOSM-style `.osm` file that keeps pending edits, so you can reopen it here or in JOSM.
- A warning on quit when there are unsaved changes.

**Undo**
- Undo and redo for every edit, with a History list in the side panel. Undo and redo select the features they changed.

**Localization**
- The interface is available in English and German, and preset names and field labels follow the language. Choose it in Settings > Language, or for one run with `--language <tag>` (for example `--language de`).

## Install

Download a build for your platform from the [releases page](https://github.com/iandees/osm-gpui/releases):

- macOS: a universal `.app` in a `.zip` (Apple Silicon and Intel). It is only ad-hoc signed, not notarized, so Gatekeeper blocks the first launch of a downloaded copy. Right-click the app and choose Open, or run `xattr -dr com.apple.quarantine osm-gpui.app`.
- Windows: a `.zip` containing `osm-gpui.exe`.
- Linux: a `.tar.gz` containing the binary, a `.desktop` file, and an icon.

macOS is the primary and best-tested platform. The Windows and Linux builds are provided, but expect rough edges there.

### Build from source

You need a stable Rust toolchain ([rustup](https://rustup.rs/)). GPUI is pulled from the Zed git repository, so the first build takes several minutes.

- macOS: Xcode and its Metal Toolchain, which GPUI needs to compile shaders. If the build fails with `cannot execute tool 'metal' due to missing Metal Toolchain`, run `xcodebuild -downloadComponent MetalToolchain`.
- Windows: the MSVC toolchain (`x86_64-pc-windows-msvc`).
- Linux: a C toolchain, `pkg-config`, and the development packages for Wayland, X11/xcb, xkbcommon, fontconfig, ALSA, OpenSSL and zstd, plus a Vulkan loader. See the commands below.

On Debian and Ubuntu (this is the list the release workflow installs on Ubuntu 22.04):

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

A build error such as `The system library 'wayland-client' required by crate 'wayland-sys' was not found`, or one naming `fontconfig.pc` or `xkbcommon.pc`, means one of these development packages is missing.

Then, on any platform:

```bash
cargo run --release
```

Release builds are much faster than debug builds for panning and zooming large datasets; measure performance with `--release`.

### Opening files

Pass `.osm` files on the command line to open each as a layer at startup, for example `osm-gpui survey.osm other.osm`; flags such as `--language` can go anywhere among them. A file that cannot be read shows a message in the status bar and the rest still open.

- macOS: the app declares itself as an editor for `.osm` files (at "Alternate" rank, so it does not take over from JOSM or another default). Use Finder's right-click > Open With, or drop a file on the Dock icon; in an already-running app the file opens as a new layer in the existing window.
- Linux: the `.desktop` file passes files to the app (`Exec=osm-gpui %F`) and lists the `application/vnd.openstreetmap.data+xml` type. Most systems know no type for `.osm`, so the tarball includes `share/mime/packages/osm-gpui.xml`; install it, plus the `.desktop` file, to get "Open With":

  ```bash
  xdg-mime install --mode user share/mime/packages/osm-gpui.xml
  desktop-file-install --dir ~/.local/share/applications osm-gpui.desktop
  update-desktop-database ~/.local/share/applications
  ```

  The `Exec=` line needs `osm-gpui` on your `PATH`; edit it to the full path if not.
- Windows: there is no installer, so no file association is registered. Use "Open with" > "Choose another app" and pick `osm-gpui.exe`, or drag a file onto the executable.

## Getting started

1. Open an `.osm` file (File > Open) or zoom in on the map and choose File > Download from OSM. Zoom in further if it reports the area is too large.
2. Optionally add imagery from the Imagery menu to trace over.
3. Edit. Press `S`, `A`, `B`, or `X` to switch modes, select features to edit their tags in the side panel, and use the Tools menu or right-click menu for actions. The status bar shows a hint for the current mode and the number of unsaved edits.
4. To upload, sign in under Settings > Account (Settings is in the app menu), then choose File > Upload to OSM, enter a changeset comment, and confirm. Settings > Account also selects the API server (primary, dev, or custom) and optional per-server OAuth client IDs.
5. Or choose File > Export to save your work, with pending edits, as an `.osm` file.

## Keyboard shortcuts

These are the defaults, written with macOS modifier symbols. Everything in the table can be changed in Settings > Keyboard Shortcuts. The single-letter shortcuts only work while the map has focus.

| Area | Shortcut | Action |
|---|---|---|
| General | `⌘,` / `⌘Q` | Settings / Quit |
| File | `⌘O` / `⌘E` | Open / Export |
| File | `⇧⌘D` / `⇧⌘U` | Download from OSM / Upload to OSM |
| Edit | `⌘Z` / `⇧⌘Z` | Undo / Redo |
| Edit | `⌘A` | Select All |
| Edit | `⇧⌘C` / `⇧⌘V` | Copy Tags / Paste Tags |
| View | `⌘=` / `⌘-` | Zoom In / Zoom Out |
| View | `⌘0` / `⌘J` | Zoom to Data / Zoom to Selection |
| Modes | `S` `A` `B` `X` `V` | Select / Add / Building / Extrude / Arc |
| Selection actions | `Q` `O` | Square / Circularize |
| Selection actions | `R` `C` | Reverse Way / Join Ways |
| Selection actions | `G` `E` | Separate Node / Extend Way |
| Selection actions | `P` `M` `T` | Split Way / Merge Nodes / Straighten Way |

Fixed keys: Delete or Backspace removes the selection, Esc cancels or deselects, Enter finishes a way in Add mode, arrow keys pan, `+` and `-` zoom, and holding Space turns a left-drag into a pan. Right-drag and middle-drag also pan, and a right-click (without dragging) opens the context menu. `⌘M`, `⌘H`, and `⌥⌘H` are the standard macOS minimize and hide keys.

## Development

CI (`.github/workflows/ci.yml`) runs four checks; run all of them before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build
cargo test
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for code conventions and pull request guidance.

**Scripted taginfo answers.** Scripted runs never touch the network, so tag suggestions from taginfo are off in them. Set `OSM_GPUI_TAGINFO_FIXTURE` to a JSON file (`{"<text in the request URL>": [<taginfo data entries>]}`, see `docs/screenshots/fixtures/taginfo-answers.json`) to answer taginfo lookups from it; `docs/screenshots/tag-autocomplete-related.osmscript` uses this.

**Scripted screenshots.** `--script <file>` runs a line-oriented `.osmscript` file (viewport, click, drag, scroll, key, load_osm, wait_idle, capture, and so on) against the live app and writes PNGs, which is useful for checking UI changes without driving the app by hand. Examples are in `docs/screenshots/`; `docs/screenshots/smoke.osmscript` exercises every operation and the scripts under `tests/ui/` run as tests.

```bash
cargo run -- --script docs/screenshots/smoke.osmscript --window-size 1200x800
```

**Pointer, modifier and snapping ops.** Where an op takes a position it is `X,Y` (window pixels), `map:X,Y` (pixels in the map area) or `geo:LAT,LON` (put on screen by the current viewport). Modifiers are `alt=true`, `ctrl=true`, `shift=true`, `cmd=true`.

| Op | Meaning |
|---|---|
| `hover POS [mods]` | Move the pointer with no button held (updates hover and the snap highlight). |
| `click POS [button=left\|right] [count=N] [mods]` | Press and release in the current edit mode. |
| `drag POS POS button=left [mods]` | Hover, press, move, release, as a real mouse would. Without `button=left` it pans the map. |
| `drag_begin POS [mods]` | Arrive at POS, then press the left button. |
| `drag_move POS [mods]` | Move with the button held; a `capture` or assert may follow. |
| `drag_end [POS] [mods]` | Release (at POS, or where the pointer is). |
| `mod_down KEY...`, `mod_up KEY...` | Press or release `alt`, `ctrl`, `shift`, `cmd` or `all` on their own; held keys apply to every later pointer op. A key press refreshes the snap highlight without a mouse move. |
| `assert_snap none\|node ID\|way ID segment N` | What the snap highlight targets now. |
| `assert_way_nodes WAY ID...` | The way's nodes, in order. |
| `assert_node_shared NODE [WAY...]` | Exactly these ways use the node. |
| `assert_status_contains TEXT` | The status bar contains the text. |

Dragging a node needs it to be the only thing moved: a press on an unselected vertex of a hovered way grabs just that node, but a press on one of several selected nodes moves them all (and never snaps). Undo selects what it touched, so press `escape` after `key cmd+z` before dragging again. `docs/screenshots/snap-behaviour.osmscript` is a regression suite for snapping built from these ops.

Other flags are `--window-size WxH`, `--keep-open` (stay open after the last step), and `--language <tag>`. Windows release builds have no console, so run the harness from a debug build there to see its output.

**In-window menu bar.** Windows and Linux have no native menu bar from GPUI, so the app draws one as the top row of the window there, built from the same menu definition as the macOS menu. To see it on macOS (in addition to the native menu), set `OSM_GPUI_IN_WINDOW_MENU=1`; `docs/screenshots/in-window-menu.osmscript` drives it:

```bash
OSM_GPUI_IN_WINDOW_MENU=1 cargo run -- --script docs/screenshots/in-window-menu.osmscript
```

Off macOS the menus drop the macOS-only items (Services, Hide, Hide Others, Show All) and the app-name menu: Quit moves to the end of File, Settings to the end of Edit, About to the end of Help. To preview that layout on macOS, set `OSM_GPUI_IN_WINDOW_MENU=other`: the in-window bar then shows the Windows/Linux contents while the native menu stays the macOS one (`docs/screenshots/in-window-menu-other.osmscript`).

**Localization.** Interface text lives in `locales/<tag>.yml` (English is the source and fallback). To add a language, copy `locales/en.yml`, translate the values, and set `meta.language_name`. Preset translations are `assets/presets/translations/<locale>.json`; add one with `cargo run --example update_preset_translations -- <schema-version> <locale>`, using the version in `assets/presets/LICENSE`.

**Presets.** The iD tagging schema data in `assets/presets/` is refreshed with `cargo run --example update_presets [-- <schema-version>]` (default: the version in `assets/presets/LICENSE`), which also fetches the referenced preset icons from the upstream releases pinned in the example. It is not part of CI; review the resulting diff by hand.

**Releases.** The version is `version` in `Cargo.toml`; it is also used in the HTTP User-Agent and the changeset `created_by` tag. To release, bump it, write the `## X.Y.Z` section of `CHANGELOG.md` from the pull requests merged since the last tag (one line each, with a link; feature pull requests do not edit the changelog), merge to `main`, then push a matching tag (`git tag vX.Y.Z`, `git push origin vX.Y.Z`). `.github/workflows/release.yml` builds the macOS, Windows, and Linux packages and publishes a GitHub Release with checksums, using the changelog section as the release notes. To build the macOS app locally, run `cargo build --release` then `scripts/bundle-macos.sh`, which writes `dist/osm-gpui.app`.

## Known limitations

- Only multipolygon, boundary, route and turn restriction relations are drawn. Other relations show only as highlighted members when selected, and can only be selected through one of their members. Route overlays do not follow the order of a route's ways and show one route per way; restriction markers have no per-type arrows.
- Upload conflict handling, multi-changeset uploads, and uploads of created, modified or deleted relations have been tested against a simulated server only, not a live OSM server.
- Upload conflicts are resolved one object at a time, and taking the server's version of an object clears the undo history.
- Export always asks where to write; there is no save back to the opened file.
- The German text is machine-drafted and has not been reviewed by a native speaker. Tag keys and values, layer names, the debug overlay, and Name Suggestion Index brand names are not translated.

## License

Licensed under the [MIT license](LICENSE).

The bundled iD tagging schema data under `assets/presets/` is ISC licensed; see `assets/presets/LICENSE`.

The preset icons under `assets/presets/icons/` come from several sets with their own licences: Maki and Temaki (CC0), iD (ISC), and Röntgen by Sergey Vartanov and Font Awesome Free by Fonticons, Inc. (both CC BY 4.0, attribution required). Versions, sources and attribution are in `assets/presets/icons/LICENSES.md`.
