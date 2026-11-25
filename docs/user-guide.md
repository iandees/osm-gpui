# osm-gpui user guide

This guide walks through a typical editing session: sign in, download data, edit it, check it, and upload it. For installing the app see the [README](../README.md).

osm-gpui can upload to the live OpenStreetMap database. Read the [OSM editing guidelines](https://wiki.openstreetmap.org/wiki/Editing) first if you are new, and consider practising on the OSM dev server (see [Sign in](#1-sign-in)).

## Contents

1. [Sign in](#1-sign-in)
2. [Download data](#2-download-data)
3. [Add imagery](#3-add-imagery)
4. [Edit the map](#4-edit-the-map)
5. [Tags, presets and fields](#5-tags-presets-and-fields)
6. [Check your work](#6-check-your-work)
7. [Upload](#7-upload)
8. [GPS traces, GPX and GeoJSON](#8-gps-traces-gpx-and-geojson)
9. [Shortcuts, undo and saving](#9-shortcuts-undo-and-saving)

## 1. Sign in

You only need to sign in to upload (and to work with notes). Everything else, including downloading and editing, works without an account.

1. Open **Settings** (`⌘,` on macOS, or the Settings item in the app or Edit menu) and go to **Account**.
2. Pick the **Server**: Primary (`api.openstreetmap.org`), Dev / testing (`master.apis.dev.openstreetmap.org`), or Custom. The dev server is a safe place to practise; it needs its own account.
3. Click **Sign in with OpenStreetMap**. Your browser opens; approve the request there. The app shows "Logged in as *name*" when it is done.

Your login is kept in the system keychain, so you stay signed in between runs. The first time, the OS may ask whether the app may use the keychain. **Sign out** is in the same place.

If you try to upload while signed out, the app asks you to sign in first.

## 2. Download data

The first time the app starts, a welcome card offers **Download OSM data here** and **Open file…**.

- **File > Download from OSM** (`⇧⌘D`) downloads what is in the current view. Zoom in first: the server limits the area (0.25 square degrees on openstreetmap.org), and the card shows "Zoom in to download" while the view is too large.
- Downloads show progress in the status bar and can be cancelled. Downloading again merges the new data into the active layer and keeps your local edits.
- **File > Open…** (`⌘O`) opens `.osm` files, plus GPX and GeoJSON files (see [section 8](#8-gps-traces-gpx-and-geojson)). You can also pass `.osm` files on the command line or drop them on the window.
- Areas you have not downloaded are dimmed, so you can tell empty from not-yet-loaded. **View > Show Downloaded Area** turns this on and off.

Each downloaded or opened file becomes a layer in the side panel, where you can show, hide, reorder and delete layers. Edits go into the **active layer**.

## 3. Add imagery

Imagery gives you something to trace over. The **Imagery** menu has:

- **OpenStreetMap Carto**: the standard map, which is the default background.
- **Available Imagery**: sources from the [Editor Layer Index](https://github.com/osmlab/editor-layer-index) that cover the area you are looking at. Recommended sources are marked with a star. Choosing one adds it as a layer.
- **Custom Imagery > Add…**: add your own tile source. Give it a name, a URL template containing `{z}`, `{x}` and `{y}` (or `{-y}`, or `{bbox-epsg-3857}` for WMS), and a minimum and maximum zoom. Saved sources are listed in the menu and in **Settings > Imagery**, where you can edit or delete them.
- **Add Esri Layer…** browses ArcGIS (Esri) map and image servers.
- **Add PMTiles Vector Layer…** adds a vector overlay from a PMTiles file or web address.
- **Adjust Imagery Offset** / **Reset Imagery Offset**: nudge an imagery layer that is misaligned with the real world. The offset is remembered per layer.

Street-level photos (Mapillary, Fotoshi, Panoramax) are extensions: install them from **Settings > Extensions**, then turn on **View > *provider* Photos**. Click a photo's dot to see it in the side panel; double-click the picture to see it full size. Mapillary needs an access token, which you enter in its row of **Settings > Extensions**; Fotoshi works without a key, and with one (also entered there) shows more photos and can list the photos of the feature you select.

Imagery attribution is shown on the map. Tiles are cached on disk; the cache limit can be changed or cleared in Settings > Imagery.

Only use imagery you are allowed to trace from. Sources in the Editor Layer Index are listed because they permit it; check the terms of any custom source.

## 4. Edit the map

Pick a mode from the toolbar, the **Mode** menu, or the keyboard. Each mode shows a hint for what the mouse does in the status bar. Modes need OSM data in the active layer, so download or open some first.

| Mode | Key | What it does |
|---|---|---|
| Select | `S` | Click to select; Shift-click to add to the selection; drag a box to select several; double-click inside a closed way to select it. Drag a selected node to move it. |
| Add | `A` | Click to place nodes and draw ways. New nodes snap onto existing ways. Press Enter or double-click to finish the way. |
| Building | `B` | Three clicks draw a rectangular building. With a squared way selected, it aligns the new building to it. |
| Extrude | `X` | Drag a way segment to reshape the way: pull out an L or push in a notch. Cmd/Ctrl-drag moves the segment; Alt-drag makes a new way. |
| Arc | `V` | Click the start, click the end, move the mouse to bend the arc, and click to finish. |
| Improve Way | `I` | With a way selected, click to move the nearest node to the cursor. Cmd/Ctrl/Alt-click inserts a node, and Shift-click deletes the nearest node. |

Esc cancels what you are drawing or deselects. Delete or Backspace removes the selection.

### Tools

Select features, then use the **Tools** menu or the right-click menu. Each action explains in the status bar why it can't run if it doesn't fit the selection.

- **Square** (`Q`) and **Circularize** (`O`) fix corners and round shapes.
- **Reverse Way** (`R`) flips direction (and direction-dependent tags).
- **Join Ways** (`C`), **Split Way** (`P`), **Merge Nodes** (`M`) and **Straighten Way** (`T`) change how ways connect.
- **Separate Node** (`G`) and **Unjoin Node from Way** pull shared nodes apart; **Extend Way** (`E`) continues a way.
- **Rotate Selection** (`W`) and **Scale Selection** (`D`) turn or resize the selected nodes and ways about their centre. Hold the key and move the pointer, then release it to apply; or tap the key, move the pointer, and click or press Enter. Rotate follows the pointer around the centre (Shift snaps to 15°); Scale follows its distance from the centre (Shift snaps to 5%). Esc cancels.
- **Extract Point** moves POI tags from a building onto a new node.
- **Copy Tags** and **Paste Tags** (`⇧⌘C`, `⇧⌘V`) copy tags between features.
- **Add to Relation** and **New Relation** manage relation membership. Selecting a relation from the side panel highlights its members and lets you edit their roles and order.

Split, Merge, Join and Delete keep relation memberships consistent for you.

Selecting a road also shows a **Lanes** section for editing lane counts and turn lanes, and selecting a junction node shows a **Junction** section for turn restrictions.

### To-do list

To work through a set of features one at a time, select them (on the map, or with **Edit > Find**) and choose **Add to To-do List** from the Tools or right-click menu. A **To-do** section appears in the side panel, above History.

- Click an entry to select it and zoom the map to it.
- When you've finished with a feature, choose **Mark To-do Done** (or click the ✓ on its row). It comes off the list, and the next entry is selected.
- **Done & Next** in the section's header does the same for the selected entry, or starts at the top of the list when none is selected.
- The **+** in the header adds the current selection.

The list is kept for the session and isn't an edit, so it has no undo and doesn't block uploading. Entries for features you created keep pointing at them after you upload. Closing the section hides it; **View > To-do List** brings it back with the list as it was.

## 5. Tags, presets and fields

Select a feature and the side panel shows it. The sections that matter most:

- **Fields:** a form for the feature type. The type is the *preset* that matches the feature's tags (for example "Cafe" or "Residential Road"), and its fields are things like name, opening hours, address and surface. Fill in a field and the right tags are written for you. Fields not shown by default are under **Add field**.
- **Tags:** the raw key/value list. Click **Add tag** to add a row. As you type, keys and values are suggested from the loaded data and the tagging schema; if you enable it in Settings, taginfo suggestions are added too (what you type is then sent to taginfo.openstreetmap.org). Values of free-text keys such as `name` and `addr:*` are never sent.

Presets come from the [iD tagging schema](https://github.com/openstreetmap/id-tagging-schema), so the fields and names match what iD users see, in your chosen language.

To change what a feature is:

- **Edit > Change Feature Type…** (or **Change feature type…** in the Fields section) opens a searchable list of presets. Recently used presets are shown first. Choosing one rewrites the tags for the new type.
- **Edit > Apply NSI Preset…** applies a [Name Suggestion Index](https://nsi.guide/) brand, such as a particular chain, adding the brand's tags in one step.

Some fields have helpers. The opening hours field has an editor that can convert pasted text from a website ("Monday – Friday: 9 AM – 5 PM") into the OSM format; check the result before applying. Address fields suggest nearby street names.

Several features can be selected at once and their shared tags edited together.

## 6. Check your work

Choose **Tools > Validate**. A **Validation** section appears in the side panel listing problems in the objects you have changed, such as untagged ways, ways that are too short, or crossing features. Each finding is either an **error** or a **warning**; click one to find it on the map.

The list updates as you keep editing, so you can fix problems one after another. Close the section with its Close button.

Some findings have a **Fix** button that resolves them in one step, for example removing a repeated node, trimming stray whitespace from tags, replacing a deprecated tag, closing a nearly closed building, or deleting an unused node. **Fix all** applies every available fix at once. A fix is only offered when it is unambiguous, so a tag is never overwritten with a guess. Each fix is an ordinary edit: it appears in History and **Undo** takes it back.

The check also runs automatically when you open the upload dialog:

- **Errors** block the upload. Fix them first.
- **Warnings** do not block it, but look at them first. The dialog offers **Upload anyway**.

The app also refuses edits as you make them when they would break the OSM API's limits, such as a way with too many nodes.

## 7. Upload

1. Make sure you are [signed in](#1-sign-in) and that the status bar shows the edits you expect.
2. Choose **File > Upload to OSM…** (`⇧⌘U`). It is disabled when there is nothing to upload.
3. Review the list of changes, and fix or acknowledge any validation findings.
4. Write a **changeset comment** that says what you changed and why, and fill in the **source** (for example the imagery you traced from; it is prefilled with the imagery that is visible).
5. Confirm. Progress is shown while the changeset is opened, filled and closed. An upload bigger than the server's changeset limit is sent as several changesets.

If someone else changed an object you edited, the dialog names it and lets you **keep your version** or **use theirs**. Taking theirs drops your change to it and clears the undo history.

If the app is interrupted during an upload, it warns you the next time you open it so you can check what reached the server.

You can also **File > Export…** (`⌘E`) to save your work, including pending edits, as a JOSM-style `.osm` file that opens here or in JOSM. There is no save back to the original file; export always asks where to write.

**File > Revert Changeset…** takes a changeset number or address and makes the edits that undo it, as one undo step you can review before uploading.

## 8. GPS traces, GPX and GeoJSON

These show up as read-only reference layers: you can trace over them but not edit them, and they are never uploaded. They draw above imagery and below your OSM data.

### Public GPS traces from OSM

**File > Download GPS Traces** downloads the public GPS points in the area around your downloaded data (or the visible map if you have not downloaded any). It works without signing in.

- Zoom in if the app says the area is too large.
- Downloads fetch up to about 50,000 points and can take a minute, because the server is slow. If the limit is reached the app says so; zoom in to see the rest.
- The layer's style controls let you colour by **Speed**, **Direction** or **Age**, choose a single colour, line width, how old a trace may be, and the speed scale (walking, cycling, town, highway). **Reset Trace Style** restores the defaults.

Speed and direction colouring are a useful way to see which way roads are used, and to find roads that have moved or are missing.

### Local GPX files

Use **File > Open…** and pick a `.gpx` file, or drop it on the window. Tracks, routes and waypoints are drawn as a reference layer, styled like GPS traces and coloured by speed, direction or age when the file has timestamps. Hover a feature to see what it is.

### Local GeoJSON files

Open a `.geojson` or `.json` file the same way to trace over points, lines and polygons from another dataset. The coordinates must be longitude and latitude in degrees (WGS84); projected data is rejected with an error. A file with nothing to draw is also reported.

If a file can't be read, a message appears in the status bar and any other files you opened still load. **View > Zoom to Data** and **Zoom to Selection** help you find a layer you just opened.

## 9. Shortcuts, undo and saving

- **Undo / Redo** (`⌘Z` / `⇧⌘Z`) work for every edit, and the **History** section in the side panel lists them. Undo and redo select the features they changed.
- Every shortcut can be changed in **Settings > Keyboard Shortcuts**. Changes and resets apply immediately; text inputs keep their standard editing shortcuts (Backspace, Select All, Copy and Paste). The single-letter map shortcuts only work while the map has focus.
- Navigation: scroll or pinch to zoom, drag to pan (or hold Space and left-drag), arrow keys to pan, `+` and `-` to zoom, and `⌘0` / `⌘J` for Zoom to Data / Selection. Right-click without dragging opens the context menu.
- The app warns on quit if you have unsaved changes. If it was closed abnormally, it offers to restore your work when next started.
- **Settings > Language** switches between English and German. For one run only, start with `--language de`.
