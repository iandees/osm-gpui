# Changelog

Release notes for osm-gpui. The section whose heading matches the pushed tag
(`## X.Y.Z`) is used as the body of the GitHub Release.

Each release's section is written just before tagging, with one line per
change and a link to its pull request. Feature pull requests do not edit this
file.

## 0.5.0

### Windows and Linux

- The menus are drawn as a menu bar inside the window; earlier builds had no menus at all on Windows and Linux. Default shortcuts use Ctrl there instead of the Windows/Super key. ([#153](https://github.com/iandees/osm-gpui/pull/153))
- macOS-only items (Hide, Hide Others, Show All, Services) are left out on Windows and Linux, and Quit, Settings and About are in File, Edit and Help. ([#154](https://github.com/iandees/osm-gpui/pull/154))

### Relations

- Route relations and turn restrictions are drawn on the map. ([#144](https://github.com/iandees/osm-gpui/pull/144))

### Upload and sign-in

- The saved login is no longer lost when the keychain cannot be read, and the upload dialog says when it is waiting for the keychain. ([#151](https://github.com/iandees/osm-gpui/pull/151))
- The changeset comment is kept when the upload dialog is closed, the side panel cannot edit while an upload runs, and undo history for uploaded edits is trimmed. ([#145](https://github.com/iandees/osm-gpui/pull/145))

### Editing

- Deleting a layer that has unsaved edits asks for confirmation first. ([#152](https://github.com/iandees/osm-gpui/pull/152))
- Deleting a layer removes its steps from the undo history. ([#150](https://github.com/iandees/osm-gpui/pull/150))
- Select All includes relations, and deleting a selection of ways together with their nodes removes everything. ([#147](https://github.com/iandees/osm-gpui/pull/147))

### Interface

- The preset picker lists recently used and common presets before you type a search. ([#142](https://github.com/iandees/osm-gpui/pull/142))
- Untagged vertices are drawn only from zoom 18, and tagged nodes are small squares when zoomed out, which makes large downloads much faster to draw. ([#149](https://github.com/iandees/osm-gpui/pull/149))
- Shorter mode hints in the status bar, a clearer downloaded-area wash, and coloured iD preset icons. ([#143](https://github.com/iandees/osm-gpui/pull/143))

### Development

- The script harness can drive real uploads against the OpenStreetMap development server, and refuses to upload to any other server. ([#147](https://github.com/iandees/osm-gpui/pull/147))
- Opt-in per-frame render timing (`OSM_GPUI_FRAME_STATS=1`), a benchmark script and a measurement report. ([#148](https://github.com/iandees/osm-gpui/pull/148))

## 0.4.0

### Editing

- Extrude now reshapes the way you drag: pull a wall out, add an L, or push in a notch. Option-drag makes a separate way as before, and Cmd-drag moves the segment. ([#133](https://github.com/iandees/osm-gpui/pull/133))
- Closed ways are offered area presets such as Building, including freshly drawn untagged shapes, and `area=yes` is added or removed as the chosen preset needs. Presets with a wildcard value no longer write a literal `*`. ([#131](https://github.com/iandees/osm-gpui/pull/131))
- Undo and redo select the features they changed, and never leave a deleted feature selected. ([#138](https://github.com/iandees/osm-gpui/pull/138))
- Edits that would make a way or relation larger than the server allows are refused, and oversized ways and relations are caught before upload. ([#124](https://github.com/iandees/osm-gpui/pull/124))

### Relations

- Multipolygon and boundary relations are drawn as filled areas with holes, and a double-click inside one selects the relation. ([#139](https://github.com/iandees/osm-gpui/pull/139))

### Upload

- When the server refuses an upload because of a conflict, the dialog names the object and offers Keep mine, Use theirs, or Show; a failed upload no longer leaves its changeset open. ([#128](https://github.com/iandees/osm-gpui/pull/128))
- The upload request is sent once and never retried, so a lost response cannot create objects twice; an uncertain result is reported with a link to the changeset. ([#132](https://github.com/iandees/osm-gpui/pull/132))
- Edits larger than the server's changeset limit are uploaded as several changesets, each applied locally as it is accepted; editing and quitting are blocked while an upload runs. ([#136](https://github.com/iandees/osm-gpui/pull/136), [#140](https://github.com/iandees/osm-gpui/pull/140))

### Interface

- Preset icons from Temaki, iD, Röntgen and Font Awesome are bundled alongside Maki and are now compiled into the app; packaged builds previously showed no preset icons. ([#137](https://github.com/iandees/osm-gpui/pull/137))
- Side panel: consistent Fields and Tags layout, History pinned to the bottom, and relation member buttons that appear on keyboard focus. ([#125](https://github.com/iandees/osm-gpui/pull/125), [#138](https://github.com/iandees/osm-gpui/pull/138))
- View > Show Downloaded Area toggles the dimming outside downloaded data, and its edge is visible over aerial imagery. ([#126](https://github.com/iandees/osm-gpui/pull/126))

### Files and packaging

- `.osm` files can be opened from the command line, from Finder's Open With, by dropping on the Dock icon, and from Linux file managers. ([#134](https://github.com/iandees/osm-gpui/pull/134))
- The project is licensed under MIT, with a LICENSE file. ([#127](https://github.com/iandees/osm-gpui/pull/127))

## 0.3.0

### Fixes

- The Linux build now includes the Wayland and X11 windowing backends; the 0.2.0 Linux binary panicked at startup. ([#113](https://github.com/iandees/osm-gpui/pull/113))
- File > Upload to OSM and Add Custom Imagery… did nothing when chosen from the menu or their shortcuts; both work again. ([#115](https://github.com/iandees/osm-gpui/pull/115))

### Relations

- Modified relations are uploaded and exported, and Split, Merge, Join and Delete keep relation memberships intact. ([#110](https://github.com/iandees/osm-gpui/pull/110))
- Undoing a delete keeps the object's version, so the restored object uploads correctly. ([#111](https://github.com/iandees/osm-gpui/pull/111))
- Select a relation from a member's Relations list to highlight its members and edit its tags, member order and roles; Add to Relation… adds the selection to an existing relation. ([#112](https://github.com/iandees/osm-gpui/pull/112))
- Create a relation from the selection (New Relation) and delete a selected relation, with undo. ([#114](https://github.com/iandees/osm-gpui/pull/114))

### Download and upload

- Progress is shown while downloading (with Cancel) and while uploading, and a failed upload keeps the dialog open with a Try again button. ([#119](https://github.com/iandees/osm-gpui/pull/119))
- The area outside the downloaded data is dimmed and outlined, and the downloaded bounds are kept when exporting. ([#120](https://github.com/iandees/osm-gpui/pull/120))
- The download size limit comes from the server's capabilities instead of a fixed value, and a server that reports itself offline is not queried. ([#121](https://github.com/iandees/osm-gpui/pull/121), [#122](https://github.com/iandees/osm-gpui/pull/122))

### Interface

- The app opens on OpenStreetMap Carto with a welcome card offering Download and Open until data is loaded. ([#116](https://github.com/iandees/osm-gpui/pull/116))
- Clearer side panel: distinct Layers, Selection and History areas, a header for the selected feature, and a single message when nothing is selected. ([#117](https://github.com/iandees/osm-gpui/pull/117))

### Documentation

- Rewritten README covering features, install, usage, shortcuts and development, with Linux build prerequisites per distribution. ([#118](https://github.com/iandees/osm-gpui/pull/118))

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
