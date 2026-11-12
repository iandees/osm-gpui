# Changelog

Release notes for osm-gpui. The section whose heading matches the pushed tag
(`## X.Y.Z`) is used as the body of the GitHub Release.

Each release's section is written just before tagging, with one line per
change and a link to its pull request. Feature pull requests do not edit this
file.

## 0.10.0

### Map styles

- Settings > Appearance has a new Map style group. Choose the built-in style, Wireframe (like JOSM's wireframe view) or iD, or add your own MapCSS style from a local file or an http(s) URL; open layers restyle live. ([#238](https://github.com/iandees/osm-gpui/pull/238))
- The Wireframe and iD styles are refined, and MapCSS gains `canvas` (background fill and hiding imagery), `symbol-shape` (circle, square, cross) and the `node:tagged`, `node:connection`, `node:unconnected`, `way:in-relation` and `area:in-relation` selectors. ([#239](https://github.com/iandees/osm-gpui/pull/239))

### Map

- The map zooms to z23, and imagery tiles are fetched up to each source's own maximum zoom, then over-zoomed (scaled) beyond it instead of disappearing. ([#237](https://github.com/iandees/osm-gpui/pull/237))
- The last map position and the main window's size and position are remembered on quit, and restored on the next start. ([#236](https://github.com/iandees/osm-gpui/pull/236))

## 0.9.0

### Map labels and styling

- Named nodes now have text labels, and nodes that match a preset with an icon show that icon in a coloured circle from zoom 17. Labels try the right, left, above and below positions to avoid each other, are placed when the map stops moving, and fade in. The selected or hovered node always keeps its label. ([#227](https://github.com/iandees/osm-gpui/pull/227)) ([#228](https://github.com/iandees/osm-gpui/pull/228))
- Named areas (buildings, parks, water, landuse and multipolygons) are labelled at the point farthest from their edges, and only where the text fits. ([#229](https://github.com/iandees/osm-gpui/pull/229))
- Street and waterway names are drawn along the way, rotated to follow it and never upside down, with a halo and a small offset so they clear the line. ([#226](https://github.com/iandees/osm-gpui/pull/226))
- With many labels on screen, the label outline is replaced by a translucent backing so drawing stays fast; labels stay on screen through a zoom instead of vanishing. ([#228](https://github.com/iandees/osm-gpui/pull/228))
- The MapCSS stylesheet supports zoom ranges (`way|z12-[highway=primary]`), `casing-width` and `casing-color`, `z-index`, `dashes`, `opacity`, `rgb()` and `rgba()` colours, and the label properties `text`, `font-size`, `text-color`, `text-halo-color`, `text-halo-radius`, `text-position`, `text-offset`, `icon-image` and `icon-width`. The default stylesheet widens and cases major roads as you zoom in, and draws ways in a stable order. ([#225](https://github.com/iandees/osm-gpui/pull/225)) ([#226](https://github.com/iandees/osm-gpui/pull/226)) ([#227](https://github.com/iandees/osm-gpui/pull/227))
- On Retina and other high-resolution displays, imagery tiles are requested one zoom level deeper so they look sharper, and the lower-zoom stand-in tile is only drawn while the real tile is loading. ([#224](https://github.com/iandees/osm-gpui/pull/224))

### Editing

- Tools > Rotate (also in the context menu, shortcut W) turns the selection around its centre by dragging; hold Shift to snap to 15 degrees. ([#218](https://github.com/iandees/osm-gpui/pull/218))
- Adding a node by dropping it on a way's segment now inserts a snapped node at the drop point. ([#221](https://github.com/iandees/osm-gpui/pull/221))
- An empty row in the Tags section suggests related and popular keys from taginfo. ([#216](https://github.com/iandees/osm-gpui/pull/216))

### Fixes

- A way you dragged stays selected after undo and redo. ([#220](https://github.com/iandees/osm-gpui/pull/220))
- The hover highlight is drawn above the selection, so a hovered vertex of a selected way is no longer hidden by the way. ([#217](https://github.com/iandees/osm-gpui/pull/217))
- Upload to OSM is disabled when there is nothing to upload. ([#222](https://github.com/iandees/osm-gpui/pull/222))

### Development

- `examples/perf_bench.rs` has label cases (`labels`, `line_labels` and `area_labels` sections), the scripting harness can drive label stress runs and force a halo mode, and new screenshot scripts cover labels, area labels, street names and curved ways. ([#226](https://github.com/iandees/osm-gpui/pull/226)) ([#227](https://github.com/iandees/osm-gpui/pull/227)) ([#228](https://github.com/iandees/osm-gpui/pull/228)) ([#229](https://github.com/iandees/osm-gpui/pull/229))
- `docs/improvement-suggestions.md` is brought up to date with the shipped work and lists the remaining label follow-ups. ([#223](https://github.com/iandees/osm-gpui/pull/223)) ([#230](https://github.com/iandees/osm-gpui/pull/230))

## 0.8.0

### Changed behaviour

- Deleting a vertex of a way now removes it from the way instead of being refused, and a way left with fewer than two nodes is deleted too. Deleting a way also deletes its untagged nodes that nothing else uses, so they are no longer left behind as orphans. ([#196](https://github.com/iandees/osm-gpui/pull/196))
- Reversing a way now swaps `forward` and `backward` member roles in the relations the way belongs to, flips `direction` tags on its nodes (a node shared with a way that is not reversed is left alone), and flips `left`/`right`/`forward`/`backward` values such as `sidewalk=left`. A numeric compass `direction` is left as it is. Reverse is refused if a tag would pass 255 characters. ([#194](https://github.com/iandees/osm-gpui/pull/194)) ([#201](https://github.com/iandees/osm-gpui/pull/201)) ([#203](https://github.com/iandees/osm-gpui/pull/203)) ([#205](https://github.com/iandees/osm-gpui/pull/205))
- Joining ways is refused instead of silently dropping a route membership when the ways are not next to each other in a shared route, or when it would give a multipolygon a ring it does not contain. Merging nodes is refused if it would leave a closed way with fewer than three distinct nodes. ([#195](https://github.com/iandees/osm-gpui/pull/195)) ([#204](https://github.com/iandees/osm-gpui/pull/204))
- Separating a node is refused when it would take a turn restriction's from or to way off its via node. ([#205](https://github.com/iandees/osm-gpui/pull/205))
- After an upload is accepted, undo steps that name objects the upload created or deleted are dropped, so undoing can no longer re-create them with ids the server refuses. ([#206](https://github.com/iandees/osm-gpui/pull/206))

### Editing

- Snapping: when you place a node while drawing, or drop a single dragged node, it snaps onto a nearby way (becoming a vertex of it) or onto an existing node, with an amber highlight showing the target. Hold Alt/Option to turn snapping off. Which features may snap together depends on their tags, so a `landuse` outline does not snap to a road. ([#209](https://github.com/iandees/osm-gpui/pull/209))
- Tools > Join Node to Way inserts a selected node into a selected way. ([#209](https://github.com/iandees/osm-gpui/pull/209))
- Tools > Create Multipolygon builds a `type=multipolygon` relation from the selected closed ways, or open ways that chain into rings, giving each ring the `outer` or `inner` role from how the rings nest. ([#207](https://github.com/iandees/osm-gpui/pull/207))
- Tools > Simplify Way removes redundant vertices from the selected ways; junctions, tagged vertices and relation members are kept. The tolerance (3 m by default) is set in Settings > Editing. ([#212](https://github.com/iandees/osm-gpui/pull/212))
- Tools > Unjoin Node from Way gives one way its own copy of a node it shares; Tools > Delete Way Segment deletes the stretch between two adjacent selected nodes. ([#211](https://github.com/iandees/osm-gpui/pull/211))
- Tools > Purge removes the selected objects from the loaded data without deleting them, so nothing is uploaded for them. ([#210](https://github.com/iandees/osm-gpui/pull/210))
- Sort members in a relation's Members section puts its ways into connected order. ([#213](https://github.com/iandees/osm-gpui/pull/213))
- Split Way works with several selected nodes at once, at a node the way passes through twice, and with one node on a closed way, which opens the ring there. ([#208](https://github.com/iandees/osm-gpui/pull/208))
- Tag keys and values autocomplete from the loaded data, the preset schema and taginfo. ([#192](https://github.com/iandees/osm-gpui/pull/192))
- The Fields section resolves preset field references and inheritance, so roads show Name and their other fields. ([#191](https://github.com/iandees/osm-gpui/pull/191))
- The selection header's history and change-type buttons are replaced by a single pencil icon button. ([#190](https://github.com/iandees/osm-gpui/pull/190))

### Fixes

- Distribute Nodes and the other geometry tools give sensible results for nodes on both sides of the 180th meridian. ([#202](https://github.com/iandees/osm-gpui/pull/202))
- On macOS, switching the appearance setting to System follows the system appearance straight away. ([#188](https://github.com/iandees/osm-gpui/pull/188))
- The map attribution is smaller and sits in the corner. ([#189](https://github.com/iandees/osm-gpui/pull/189))

### Development

- A catalogue of editing edge cases (`docs/testing/edge-cases-from-josm-and-id.md`) and hundreds of new tests in `src/edge_case_tests/` cover splitting, reversing, joining, merging, disconnecting, deleting, relations, uploads, limits and undo. ([#193](https://github.com/iandees/osm-gpui/pull/193)) ([#197](https://github.com/iandees/osm-gpui/pull/197)) ([#198](https://github.com/iandees/osm-gpui/pull/198)) ([#199](https://github.com/iandees/osm-gpui/pull/199)) ([#200](https://github.com/iandees/osm-gpui/pull/200)) ([#204](https://github.com/iandees/osm-gpui/pull/204)) ([#205](https://github.com/iandees/osm-gpui/pull/205)) ([#206](https://github.com/iandees/osm-gpui/pull/206))
- Scripts can hover, click and drag with modifier keys held, split a drag into begin, move and end to capture or check mid-drag, and assert the snap target, a way's nodes and which ways share a node; a scripted snapping suite lives in `docs/screenshots/snap-behaviour.osmscript`. ([#214](https://github.com/iandees/osm-gpui/pull/214))

## 0.7.0

### Changed behaviour

- Scrolling the mouse wheel up now zooms in by default, as in JOSM and on the OpenStreetMap website. If you turned on Settings > Appearance > Invert scroll zoom in 0.6.0 to get this direction, turn it off again. ([#170](https://github.com/iandees/osm-gpui/pull/170))
- Signing in now also asks for permission to modify notes. An existing login keeps working for editing, but commenting on or closing notes needs a new sign-in. ([#179](https://github.com/iandees/osm-gpui/pull/179))
- The History and Validation sections of the side panel start collapsed; click a header to open it. A collapsed History shows the number of undo steps, and Validation shows its error and warning counts. ([#187](https://github.com/iandees/osm-gpui/pull/187))

### Editing

- Copy (Cmd/Ctrl+C), Paste (Cmd/Ctrl+V) at the pointer, Paste at Source Position and Duplicate (Cmd/Ctrl+D) for features, within and across layers, each as one undo step. ([#175](https://github.com/iandees/osm-gpui/pull/175))
- Hovering a way from zoom 18 shows its vertices; click one to select it, or press and drag to move it in one gesture. Any visible node can be dragged without selecting it first. ([#174](https://github.com/iandees/osm-gpui/pull/174))
- Tools > Create Parallel Way makes an offset copy of the selected ways, for dual carriageways, sidewalks and fences. ([#171](https://github.com/iandees/osm-gpui/pull/171))
- Align Nodes in Line (L) and Distribute Nodes for three or more selected nodes. ([#176](https://github.com/iandees/osm-gpui/pull/176))
- Show Object History (Cmd/Ctrl+Shift+H) lists an object's versions with who changed what and when, compares your unsaved changes with the server, and can revert the tags to an earlier version. ([#177](https://github.com/iandees/osm-gpui/pull/177))

### Layers and notes

- File > Open reads GPX and GeoJSON files as read-only reference layers; clicking a feature shows its name and properties, and new nodes snap to them while drawing. ([#180](https://github.com/iandees/osm-gpui/pull/180))
- View > Show Notes shows OpenStreetMap notes for the area; open one to read its thread, comment, close or reopen it, and create notes with Tools > New Note. ([#179](https://github.com/iandees/osm-gpui/pull/179))

### Upload

- The upload dialog has a Sources field with quick choices, a review request checkbox and a list of recent comments, and changesets record the imagery used, the interface language and hashtags from the comment. A comment or source over 255 characters is refused before uploading. ([#178](https://github.com/iandees/osm-gpui/pull/178))

### Windows and Linux

- Submenus work in the in-window menu bar: Imagery has Custom Imagery and Available Imagery submenus, where 0.6.0 could not open the custom imagery list. Clicking the map to close a menu no longer changes the selection. ([#172](https://github.com/iandees/osm-gpui/pull/172))
- A dialog opened from the menu bar now takes the keyboard: typing goes into its field and Escape and Enter work. Before, the keys went to the map and could switch modes or make edits. ([#186](https://github.com/iandees/osm-gpui/pull/186))

### Fixes

- A layer is no longer shown as modified, and the quit and Delete Layer warnings no longer appear, after undoing every change. ([#185](https://github.com/iandees/osm-gpui/pull/185))
- The side panel no longer cuts off its contents: the selection or open note scrolls, and Layers, Validation and History stay within bounds at any window height. ([#187](https://github.com/iandees/osm-gpui/pull/187))
- In the upload dialog the button reads "Upload anyway" only when there are warnings and no errors, and only layers with changes are listed. ([#184](https://github.com/iandees/osm-gpui/pull/184))
- Buttons, checkbox labels and links in the Find dialog and other recent dialogs and panels use the same text sizes as the rest of the app. ([#183](https://github.com/iandees/osm-gpui/pull/183))

### Development

- Tests and scripted runs can no longer write the user's saved settings, logins or custom imagery; only the app's normal start-up enables saving. ([#173](https://github.com/iandees/osm-gpui/pull/173))

## 0.6.0

### Fixes

- Linux: the app no longer panics with "RefCell already borrowed" when the desktop reports its light or dark appearance, which could happen right after launch in 0.5.0. ([#167](https://github.com/iandees/osm-gpui/pull/167))
- macOS: the window title bar and other system chrome follow the app's theme instead of staying light or dark with the system. ([#164](https://github.com/iandees/osm-gpui/pull/164))
- Zoom to Selection on a single node or a perfectly straight horizontal or vertical way zooms in on it instead of out to about a kilometre. ([#160](https://github.com/iandees/osm-gpui/pull/160))
- Newlines and tabs in tag values are escaped in exported files and uploads instead of being turned into spaces by readers. ([#161](https://github.com/iandees/osm-gpui/pull/161))

### Editing

- Edit > Find (Cmd/Ctrl+F) selects features with a query such as `highway=residential -name=*`, `type:way nodes:10-` or `modified`, with a live match count and replace, add, remove and find-in-selection modes. ([#162](https://github.com/iandees/osm-gpui/pull/162))
- A validator checks the objects you created or changed before upload: errors block the upload, warnings can be uploaded anyway. Tools > Validate lists the problems in the side panel, where clicking one selects it and the list updates as you fix things. ([#160](https://github.com/iandees/osm-gpui/pull/160))
- Layers with unsaved edits are autosaved, and the app offers to restore them at the next start after a crash or force quit. Autosave can be turned off in Settings. ([#161](https://github.com/iandees/osm-gpui/pull/161))
- Imagery > Adjust imagery offset shifts an imagery layer by dragging, arrow keys or typed metres; the offset is shown in the Layers list and remembered near where it was set. ([#159](https://github.com/iandees/osm-gpui/pull/159))

### Map drawing

- Way vertices without tags are drawn only when the node or its way is selected, so select a way to see and move its vertices. Tagged nodes are hidden below zoom 17 and drawn as tiny empty squares at zoom 17. ([#158](https://github.com/iandees/osm-gpui/pull/158))
- Ways are drawn with joined corners and rounded ends from zoom 16, so bends and roads split into several ways look continuous. ([#169](https://github.com/iandees/osm-gpui/pull/169))
- Ways are drawn much faster on large downloads, with simplified geometry and fewer details when zoomed out. ([#157](https://github.com/iandees/osm-gpui/pull/157))

### Interface

- Settings > Appearance has an Invert scroll zoom switch for the mouse wheel. ([#166](https://github.com/iandees/osm-gpui/pull/166))
- The welcome card has a close button; Settings > Appearance > Show welcome card brings it back. ([#163](https://github.com/iandees/osm-gpui/pull/163))

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

- Settings > Appearance has a theme choice of System, Light or Dark. The default is System, so the app now follows the operating system's light or dark appearance instead of always being light. ([#155](https://github.com/iandees/osm-gpui/pull/155))
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
