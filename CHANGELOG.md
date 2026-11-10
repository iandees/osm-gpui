# Changelog

Release notes for osm-gpui. The section whose heading matches the pushed tag
(`## X.Y.Z`) is used as the body of the GitHub Release.

## Unreleased

- Downloads and uploads now show their progress: the status bar shows the stage (requesting, bytes received, reading data) with a spinner and a Cancel button for downloads, and the upload dialog shows the changeset stage and stays open to show an error and offer a retry.

## 0.2.0

First tagged release. osm-gpui is an experimental OpenStreetMap editor; much of
OSM's data model (relations in particular) is not editable yet.

### Editing

- Select mode: click, Shift-click and box-select nodes and ways; double-click inside a closed way to select it (the smallest of overlapping ways).
- Add mode draws nodes and ways, snapping new nodes onto existing ways. Building mode draws a rectangular building (and aligns it to a selected squared way). Extrude mode pulls a building edge out.
- Drag nodes to move them. Tags are edited in place in the side panel, with preset-aware fields, a preset picker and Name Suggestion Index brand presets.
- Selection actions, available from the Tools menu, the right-click context menu and shortcuts: Square, Circularize, Reverse Way (direction-aware), Join Ways, Separate Node, Extend Way, Split Way, Merge Nodes, Straighten Way, Copy/Paste Tags and Delete.
- Undo and redo for every edit, with a History panel.

### Map display

- Raster imagery from OpenStreetMap Carto, the Editor Layer Index and user-added custom sources, with attribution shown on the map and a bounded on-disk tile cache.
- Data drawn for tracing over imagery: thin cased ways, round node markers, direction arrows, area fills and hover highlighting.
- MapCSS-subset styling (`node`, `way` and `area` selectors with tag conditions; colour, width, symbol size and fill).
- Mac-native navigation (trackpad pan and pinch zoom, mouse wheel, drag, keyboard) and an adjustable lat/lon grid overlay.

### Data

- Open OSM XML files and download the current view from the OSM API; a download is merged into the active layer and keeps local edits.
- Sign in with OSM OAuth (tokens kept in the system keyring) and upload edits as a changeset after a review step.
- Export a layer to a JOSM-style `.osm` file that keeps pending edits, so it can be reopened here or in JOSM.
- Warning on quit when there are unsaved changes.

### Interface

- Menu bar, mode toolbar, status bar, resizable side panel with layer visibility toggles, and a Settings window (API server, OAuth client IDs, imagery, text size, keyboard shortcuts).
- About dialog in the app menu showing the version.
- Scripted screenshot harness (`--script file.osmscript`) for visual checks and automation.

### Localization

- Interface available in English and German; the German text is machine-drafted and not yet reviewed by a native speaker.
- Preset names, field labels and options follow the UI language (German bundled).
- Language is chosen in Settings, or per run with `--language <tag>`.

### Releases

- Tagged releases build a universal macOS `.app` (ad-hoc signed, not notarized), a Windows `.zip` and a Linux `.tar.gz` with SHA-256 checksums.
- Placeholder app icon for the macOS bundle and the Windows executable; the Linux archive includes the icon next to the `.desktop` file.
- Release builds on Windows do not open a console window, so `--script` output is not shown there.
- The version in `Cargo.toml` is used for the HTTP User-Agent and the changeset `created_by` tag.
