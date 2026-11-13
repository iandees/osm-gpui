# osmscript reference

An `.osmscript` file is a line-oriented script that `--script <file>` runs against the live app, one op per line, in order. A run stops at the first failing op.

```bash
cargo run -- --script docs/screenshots/smoke.osmscript --window-size 1200x800
```

The parser is `src/script/parser.rs` (plus `src/script/note_ops.rs` for the notes ops). A unit test checks that every op the parser accepts has exactly one row in the tables below and that no row names an op the parser rejects, so this page stays complete. Every checked-in script is also parsed by `cargo test` (`tests/script_files_parse.rs`).

## Syntax

- One op per line: the op word, then whitespace-separated arguments.
- `#` starts a comment, to the end of the line. Write `\#` for a literal `#` (needed for hashtags in `set_comment`). Blank lines are ignored.
- Ops that take free text (`type`, `set_comment`, `set_tag` values, `log`, ...) use the rest of the line, internal spacing kept.
- Arguments in capitals are placeholders. `[...]` is optional, `a|b` is a choice, `...` repeats. `key=value` arguments may come in any order.
- A duration is `Nms` or `Ns` (`250ms`, `1.5s`).
- A position `POS` is `X,Y` (window pixels), `map:X,Y` (pixels in the map area, i.e. below the toolbar and any in-window menu bar) or `geo:LAT,LON` (a coordinate put on screen by the current viewport).
- Modifier arguments `MODS` are any of `alt=true`, `ctrl=true`, `shift=true`, `cmd=true`. They combine with modifiers held by `mod_down`.
- Ops that write to the OSM server (uploads, note writes) only run against the OSM development server; start such scripts with `assert_server dev`.

## Window and viewport

| Op | Description |
|---|---|
| `window W H` | Set the window size in pixels. |
| `viewport LAT LON ZOOM` | Centre the map on a coordinate at a zoom level. |
| `wait DURATION` | Sleep for a fixed time. |
| `wait_idle [TIMEOUT]` | Wait until tile loads and other background work settle (default 10s). |
| `pause [DURATION] MESSAGE` | Show MESSAGE in the status bar and wait (default 2s) so a person can follow a live run. |
| `log MESSAGE` | Print MESSAGE to the harness output. |
| `frame_stats [LABEL]` | Print and clear the per-phase frame timings since the last report (only with `OSM_GPUI_FRAME_STATS=1`). |
| `capture PATH` | Save a PNG screenshot of the window to PATH. |
| `capture_while_transferring PREFIX [every=DUR] [timeout=DUR]` | While a download or upload runs, save `PREFIX-NN.png` every `every` (default 700ms) until it ends or `timeout` (default 120s). |
| `crash` | Exit at once without cleaning up, as a crash would, so the next run finds an unclean session. |

## Pointer and keyboard

| Op | Description |
|---|---|
| `click POS [button=left\|right] [count=N] [MODS]` | Press and release in the current edit mode (default left button, one click; `count=2` is a double-click). |
| `drag POS POS [duration=DUR] [button=left\|pan] [MODS]` | Drag from one position to another over `duration` (default 200ms). Without `button=left` it pans the map; with it, it is a left-button gesture handled by the current edit mode. |
| `hover POS [MODS]` | Move the pointer with no button held; updates hover and the snap highlight. |
| `drag_begin POS [MODS]` | Move to POS, then press the left button. |
| `drag_move POS [MODS]` | Move with the left button held. |
| `drag_end [POS] [MODS]` | Release the left button, at POS or where the pointer is. |
| `scroll X,Y [dx=N] [dy=N]` | Scroll-wheel at window pixel X,Y; this zooms the map. |
| `mod_down NAME...` | Press modifiers on their own (`alt`, `ctrl`, `shift`, `cmd` or `all`). They stay held for later pointer ops until released. |
| `mod_up NAME...` | Release modifiers pressed with `mod_down`. |
| `key CHORD` | Press a key chord such as `cmd+shift+a`, `escape` or `cmd+z`. |
| `type TEXT` | Type TEXT into the focused input, one character at a time (appends to what is there). |
| `action NAME [JSON]` | Dispatch a named app action the way a native menu item does (for example `UploadToOsm`, or `AddImageryLayer {"id":"EsriWorldImagery"}`). |

## Layers and files

| Op | Description |
|---|---|
| `load_osm PATH` | Load an `.osm` file as a new layer. |
| `load_file PATH` | Open a file the way File > Open does, routed by type (`.osm`, `.gpx`, `.geojson`/`.json`). |
| `export_layer PATH` | Write the active layer, with its pending edits, to an `.osm` file, without the save dialog. |
| `autosave_now` | Write every layer's autosave and wait until it is on disk (needs `--autosave-dir`). |
| `restore_autosave` | Press Restore in the restore dialog and wait for the layers to load. |
| `discard_autosave` | Press Discard in the restore dialog. |
| `confirm_delete_layer` | Press Delete Layer in the delete-layer confirmation. |
| `cancel_delete_layer` | Press Cancel in the delete-layer confirmation. |
| `panel validation\|history expand\|collapse` | Expand or collapse a side panel section, as a click on its header does. |
| `show_finding ROW\|CHECK_ID` | Click a validator finding in the Validation section: the Nth row (0 is the first) or the first finding of a check id such as `crossing_ways`. |
| `show_history` | Open the object history view for the selected object and wait for its versions to arrive. |
| `load_history_file PATH` | Answer the next object history request from a local `/history` document instead of the server. |

## Editing

| Op | Description |
|---|---|
| `set_tag KEY VALUE` | Set a tag on the selection, as the side panel's Add tag row does (one undo step; replaces an existing value). |
| `remove_tag KEY` | Remove a tag from the selection. |

Other edits are driven with `click`, `drag`, `key` and `action` in the relevant edit mode.

## Assertions

An assertion that does not hold fails the run with a message naming the line.

| Op | Description |
|---|---|
| `assert_mode select\|add\|building\|extrude\|arc\|improve` | The active edit mode. |
| `assert_selected none\|node ID\|way ID\|relation ID` | Exactly that one feature is selected, or nothing is. |
| `assert_selected_count COUNT` | Exactly COUNT features are selected. |
| `assert_selected_at LAT LON [tol=METRES]` | Exactly one node is selected, within the tolerance (default 0.5 m) of the point. |
| `assert_selection none\|node\|way\|relation\|multiple` | The kind of the current selection. |
| `assert_snap none\|node ID\|way ID segment N` | What the snap highlight targets now; segment 0 lies between a way's first two nodes. |
| `assert_way_nodes WAY NODE...` | The way's nodes, in order. |
| `assert_node_shared NODE [WAY...]` | Exactly these ways use the node, in any order. |
| `assert_layer gpx\|geojson\|osm\|imagery\|grid NAME` | A layer of that kind and exact name exists (NAME is the rest of the line). |
| `assert_layer_count COUNT` | Exactly COUNT layers (imagery and data) are on the map. |
| `assert_pending COUNT` | The active layer has COUNT pending edits (created, modified and deleted). |
| `assert_zoom LEVEL [tol=N]` | The map zoom, within the tolerance (default 0.001). |
| `assert_status_contains TEXT` | The status bar message contains TEXT. |
| `assert_dialog none\|editing\|in_flight\|failed\|sign_in\|notice\|delete_layer\|restore\|find\|history\|conflict [KIND] [node\|way\|relation]` | Which dialog is on screen. `conflict` may name a kind (`version_mismatch`, `already_deleted`, `still_in_use`, `missing_dependency`, `other`) and the object kind involved. |
| `assert_find_preview TEXT` | The open Find dialog's match-count line contains TEXT. |
| `assert_validation errors=N warnings=N CHECK_ID=COUNT...` | What the validator finds in the pending changes: error and warning totals and per-check counts; give at least one pair. |
| `assert_panel validation\|history expanded\|collapsed` | A side panel section is expanded or collapsed. |
| `assert_validation_panel shown\|hidden` | The Validation section is present or absent. |
| `assert_native_appearance system\|light\|dark` | The native title bar appearance as read back from the OS. |
| `assert_imagery_offset EAST NORTH [tol=METRES]` | The imagery offset in metres (of the layer being adjusted, else the topmost visible imagery layer); default tolerance 0.05 m. |
| `assert_adjusting_offset on\|off` | Whether the adjust-imagery-offset mode is active. |
| `assert_autosave_exists [COUNT]` | This run's autosave directory holds a layer file, or exactly COUNT of them. |
| `assert_history_versions COUNT` | The open object history view lists exactly COUNT versions. |
| `assert_changeset_tag KEY VALUE` | The tags the upload dialog would write include KEY with VALUE. Nothing is sent. |
| `assert_no_changeset_tag KEY` | The upload dialog would not write KEY. |
| `assert_signed_in` | The active API server has a signed-in user. |
| `assert_server dev` | The active API server is the OSM development server. |

## Upload dialog and network

| Op | Description |
|---|---|
| `open_upload_dialog_preview` | Open the upload dialog as a preview: no sign-in needed, Upload never works. |
| `set_comment TEXT` | Replace the changeset comment (unlike `type`, which appends). |
| `set_source TEXT` | Replace the changeset Sources text. |
| `press_source_chip TEXT` | Press the quick-add source button with that label. |
| `set_review_requested on\|off` | Tick or untick the review request. |
| `clear_imagery_used on\|off` | Leave `imagery_used` out of this upload (`on`) or put it back. |
| `add_recent_comment TEXT` | Add a comment to the in-memory recent-comments list (never saved). |
| `toggle_recent_comments` | Open or close the recent-comments list. |
| `pick_recent_comment INDEX` | Pick the Nth recent comment (0 is the newest). |
| `upload_confirm` | Press the dialog's Upload (or Try again) button. Development server only. |
| `upload_cancel` | Press the dialog's Cancel button. |
| `resolve_conflict keep_mine\|use_theirs\|drop_mine\|show` | Press one of the conflict dialog's buttons. |
| `wait_transfer [TIMEOUT]` | Wait until no download or upload is running (default 120s). |
| `sign_in` | Start the browser sign-in for the active API server. |
| `wait_signed_in [TIMEOUT]` | Wait for the sign-in to finish, approved by a person in the browser (default 200s). |

## Notes

The four writes (`note_comment`, `note_close`, `note_reopen`, `new_note`) only run against the OSM development server. Because `#` starts a script comment, it cannot appear in note text.

| Op | Description |
|---|---|
| `show_notes on\|off` | Show or hide the notes layer (View > Show Notes). |
| `load_notes_file PATH` | Show the notes in a local API response instead of fetching. |
| `open_note ID` | Open a loaded note in the side panel. |
| `note_comment TEXT` | Comment on the open note (a write). |
| `note_close [TEXT]` | Close the open note, optionally with a comment (a write). |
| `note_reopen [TEXT]` | Reopen the open note, optionally with a comment (a write). |
| `new_note LAT LON TEXT` | Create a note at a coordinate (a write). |
| `assert_notes COUNT` | Exactly COUNT notes are loaded. |
| `assert_note_status open\|closed\|none` | The open note's status, or that no note is open. |
| `assert_note_comments COUNT` | The open note has COUNT comments. |

## Examples

Scripts live in `docs/screenshots/` (screenshots and regression suites such as `snap-behaviour.osmscript`), `docs/live-test/` (against the development server), `docs/perf/` and `tests/ui/`.

Dragging a node needs it to be the only thing moved: a press on an unselected vertex of a hovered way grabs just that node, but a press on one of several selected nodes moves them all (and never snaps). Undo selects what it touched, so press `escape` after `key cmd+z` before dragging again.
