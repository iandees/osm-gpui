# Live tests against the OSM development server

These scripts drive the real app (its Add mode, side panel, upload dialog and
upload loop) against `https://master.apis.dev.openstreetmap.org`, so you can
watch uploads happen. Work is in open ocean near lat -48, lon -123 and each
script deletes what it created. Run from the repository root.

1. Sign in once (approve in the browser within three minutes):

   ```
   cargo run -- --api-server dev --script docs/live-test/00-sign-in.osmscript
   ```

2. Run a scenario (`--script-delay-ms` slows every op so you can follow):

   ```
   cargo run -- --api-server dev --script-delay-ms 400 --script docs/live-test/01-roundtrip.osmscript
   cargo run -- --api-server dev --script-delay-ms 400 --script docs/live-test/02-conflict.osmscript
   OSM_GPUI_DEBUG_CHUNK_LIMIT=25 cargo run -- --api-server dev --script-delay-ms 150 --script docs/live-test/03-chunked.osmscript
   ```

3. If a run stops part way, `99-cleanup.osmscript` deletes what is left in the
   three areas.

Captures go to `out/`. `--api-server dev|primary|URL` applies to that run only
and is never saved to `settings.json`; the server host shows in red in the
status bar and the window title whenever it is not production.

## Safety

Scripted runs never upload to production: `upload_confirm`,
`resolve_conflict` and `sign_in` refuse unless the active server's host is
exactly `master.apis.dev.openstreetmap.org`, and the app's upload code checks
the same in a scripted run whatever key or click starts it. Each script starts
with `assert_server dev`.

## New script ops

| Op | Meaning |
|---|---|
| `type TEXT` | Type into the focused input, a character at a time (appends). Rest of line, `#` starts a comment. |
| `set_comment TEXT` | Replace the upload dialog's comment. |
| `set_tag KEY VALUE` | Set a tag on the selection (the side panel's Add tag path; one undo step). Value is the rest of the line. |
| `remove_tag KEY` | Remove a tag from the selection. |
| `upload_confirm` | Press Upload / Try again. Dev server only. |
| `upload_cancel` | Press Cancel in the upload dialog. |
| `confirm_delete_layer` | Press "Delete Layer" in the delete-layer confirmation. |
| `cancel_delete_layer` | Press Cancel in the delete-layer confirmation. |
| `resolve_conflict keep_mine\|use_theirs\|drop_mine\|show` | Press a conflict button. Dev server only. |
| `wait_transfer [T]` | Wait until no download/upload/resolution is running (default 120s). |
| `capture_while_transferring PREFIX [every=D] [timeout=D]` | Capture `PREFIX-NN.png` while a transfer runs. |
| `assert_pending N` | Pending edits in the active layer. |
| `assert_status_contains TEXT` | Status bar message contains text. |
| `assert_dialog none\|editing\|in_flight\|failed\|sign_in\|notice\|delete_layer\|conflict [KIND] [node\|way\|relation]` | What is on screen; KIND is `version_mismatch`, `already_deleted`, `still_in_use`, `missing_dependency`, `other`. |
| `assert_selection none\|node\|way\|relation\|multiple` | Selection kind without ids. |
| `export_layer PATH` | Write the active layer (with pending edits) as File > Export does. Load it back with `load_osm`. |
| `sign_in`, `wait_signed_in [T]`, `assert_signed_in` | Browser sign-in. Dev server only for `sign_in`. |
| `assert_server dev` | Active server is the dev server. |
| `pause [D] MESSAGE` | Show a message in the status bar and wait (default 2s). |

`action layers::DeleteLayer {"index":1}` opens a confirmation when the layer
has unsaved edits (answer it with `confirm_delete_layer` or
`cancel_delete_layer`, check it with `assert_dialog delete_layer`) and deletes
at once otherwise. The live-test scripts only delete layers with nothing
pending, and assert `assert_dialog none` afterwards.

Other useful existing ops: `action layers::DeleteLayer {"index":1}`,
`action layers::SetActiveLayer {"index":N}`,
`action selection::RunSelectionAction {"id":"new_relation"}`,
`action SelectAll`, `action DeleteSelection`, `action DownloadFromOsm`.
Flag `--script-delay-ms N` pauses N ms after every op.
