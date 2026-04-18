# Issue 25: Give a reason when tiles fail to load

## Context
Today when a tile request fails, every error path in `TileAsset::load`
collapses into an `ImageCacheError::Other(anyhow!(...))` that GPUI swallows,
and `TileLayer::render_elements` paints a hard-coded "Failed" string in the
fallback (`src/layers/tile_layer.rs:152`). HTTP status codes from `ureq` are
discarded inside `download_file_sync` because it returns
`Box<dyn Error + Send + Sync>` (`src/tile_cache.rs:104`). Users can't tell a
404 from a tile-server timeout from a "not a PNG" body. We want a short,
specific reason rendered in the tile.

## Approach

- `src/tile_cache.rs`
  - Replace `download_file_sync`'s return with a typed error, e.g.
    `enum TileFetchError { Http { status: u16, body_snippet: Option<String> },
    Transport(String), EmptyBody, NotPng, Io(String) }` with a `Display`
    impl that yields short strings ("HTTP 404", "HTTP 503: Over capacity",
    "Timeout: dns", "Empty body", "Not PNG", "Disk: ..."). Match on
    `ureq::Error::Status(code, resp)` to capture status and read up to ~120
    bytes of the body for the snippet; map `ureq::Error::Transport(t)` to
    `Transport(t.kind().to_string())`.
  - In `TileAsset::load`, on every error branch (download, empty, magic
    bytes, file write, decode, mkdir), build a short reason `String` and
    insert it into a new `pub static TILE_LOAD_ERRORS: OnceLock<Mutex<
    HashMap<String, String>>>` keyed by tile URL. On success, remove the
    URL's entry so a tile that recovers stops showing the old reason.
  - Add `pub fn last_error(url: &str) -> Option<String>` helper.

- `src/layers/tile_layer.rs`
  - In the `with_fallback(|| ...)` closure (line 141), capture
    `tile_url.clone()` and `tile_width` (already in scope). Look up
    `tile_cache::last_error(&tile_url)`; default to `"Failed"`.
  - Truncate the reason to fit: pick a char budget from `tile_width`
    (roughly `(tile_width / 6.0) as usize`, min 8, max 40) and add a
    helper `truncate_middle(s: &str, max: usize) -> String` in
    `tile_cache.rs` (or co-located in `tile_layer.rs`) that returns
    `"<head>...<tail>"` when over budget. There is no existing helper —
    `grep -n truncate src/` finds none.
  - Render with `text_xs()`, `text_color(white)`, and `overflow_hidden()`
    on the surrounding div (already set on the tile div, inherited
    visually); also clamp with `.whitespace_nowrap()` to keep it on one
    line. Keep the rose background.

- Global state plumbing: follow the same `OnceLock<Mutex<...>>` pattern
  used elsewhere in `tile_cache.rs` for `TILE_IDLE_TRACKER`. No new
  dependency crates needed.

## Verification
- `cargo build --release`
- `cargo test --lib` (add a unit test for `truncate_middle` and for the
  `Display` of `TileFetchError` variants).
- Render smoke: temporarily point a tile layer at a URL template that
  returns 404 (e.g. `https://tile.openstreetmap.org/99/{x}/{y}.png`),
  run `cargo run --release`, confirm tiles show "HTTP 404" instead of
  "Failed". Repeat with an unreachable host to see "Transport: ...".
- Eyeball: at small tile sizes the text should truncate (e.g.
  "HTTP 503: Over...") rather than overflow.

## Out of scope
- Surfacing per-tile errors anywhere outside the tile itself (no panel,
  no log toast, no debug-overlay counter).
- Retry / backoff policy changes.
- Restructuring `TileAsset` to expose `ImageCacheError` content directly
  (GPUI's API does not pass it to `with_fallback`; the URL-keyed side
  map is the pragmatic workaround).
- Touching dead modules (`map.rs`, `mercator.rs`, `background.rs`,
  `http_image_loader.rs`, `data.rs`).
- Replacing `ureq` with an async client.
