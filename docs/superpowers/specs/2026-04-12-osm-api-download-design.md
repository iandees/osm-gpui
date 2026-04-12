# OSM API Download — Design

## Goal

Add the ability to fetch OSM data for the current viewport directly from the
OpenStreetMap main API (`api.openstreetmap.org/api/0.6/map`), parse it with the
existing `OsmParser`, and display it as a new `OsmLayer`. This complements the
existing File > Open flow and lays groundwork for a future editing/upload path.

Overpass is out of scope; this targets the main OSM API because editing
(eventual goal) requires it.

## User-facing behavior

- New menu item **File > Download from OSM** with accelerator **⌘⇧D**.
- Selecting it fetches OSM data for the current viewport bbox.
- While the request is in flight, a one-line status overlay reads
  `Downloading OSM data…`.
- On success, a new `OsmLayer` is added to the `LayerManager`, labeled
  `OSM API (<min_lat>,<min_lon>,<max_lat>,<max_lon>)` (4-decimal precision).
  Existing layers are kept — repeated downloads stack, matching today's
  File > Open behavior. (Merging layers is a future concern.)
- On failure, the status overlay shows a readable error for ~5 seconds or
  until the next action clears it.

## Pre-check

Before making any network request, compute the viewport bbox area:

```
area_sq_deg = (max_lon - min_lon) * (max_lat - min_lat)
```

If `area_sq_deg > 0.25`, abort with the error
`Area too large for OSM API (zoom in and try again)`. This mirrors the
server's own limit and avoids a wasted round-trip.

## Module layout

New module `src/osm_api.rs`:

```rust
pub struct OsmApiRequest {
    pub bounds: GeoBounds,
}

pub enum OsmApiError {
    AreaTooLarge { area_sq_deg: f64 },
    Http { status: u16, body: String },
    Network(String),       // from ureq::Error, stringified for Send + 'static
    Parse(OsmParseError),
}

pub fn fetch_bbox(bounds: GeoBounds) -> Result<OsmData, OsmApiError>;
```

`fetch_bbox` is synchronous — it is called from a worker thread, not the UI
thread. It performs: area pre-check → URL construction → `ureq` GET with a
`User-Agent: osm-gpui/<CARGO_PKG_VERSION>` header → status dispatch →
`OsmParser::parse_from_bytes` on 200.

URL format:
`https://api.openstreetmap.org/api/0.6/map?bbox={min_lon},{min_lat},{max_lon},{max_lat}`
with 7-decimal precision (sufficient for ~1cm on the ground).

## Threading / cross-thread queue

Follows the existing `SHARED_OSM_DATA` pattern in `main.rs`:

- Add `static API_FETCH_RESULTS: OnceLock<Mutex<Vec<ApiFetchResult>>>` where
  `ApiFetchResult = Result<OsmData, String>` (stringified error for display).
- Menu action spawns a `std::thread` that calls `osm_api::fetch_bbox`, maps
  the error to a user-readable string, and pushes the result into the queue.
- `MapViewer::render` gains a `check_for_api_fetch_results()` call that drains
  the queue. On `Ok(data)` it constructs a new `OsmLayer` and adds it to the
  layer manager. On `Err(msg)` it stores the message in
  `MapViewer::status_message`.

A companion `status_message: Option<(String, Instant)>` field on `MapViewer`
holds the transient "Downloading…" / error text. `render` clears it after
5 seconds.

## Action wiring

- Define `DownloadFromOsm` action alongside `OpenOsmFile` in `main.rs`.
- Register ⌘⇧D key binding.
- Add a `File` menu entry.
- Handler reads the current viewport bounds from
  `MapViewer.viewport.visible_bounds()` (or the existing accessor — confirm
  during implementation), performs the area pre-check synchronously on the UI
  thread (so immediate errors don't need the queue), sets
  `status_message = Some(("Downloading OSM data…", now))`, then spawns the
  worker thread.

## Error messages

| Variant | User-visible text |
|---|---|
| `AreaTooLarge` | `Area too large for OSM API (zoom in and try again)` |
| `Http { status: 400, .. }` | `OSM API rejected request (400) — try a smaller area` |
| `Http { status: 509, .. }` | `OSM API rate-limited (509) — try again later` |
| `Http { status, body }` | `OSM API error <status>: <first line of body>` |
| `Network(..)` | `Network error: <message>` |
| `Parse(..)` | `Failed to parse OSM response: <message>` |

## Testing

Unit tests live next to `src/osm_api.rs`:

- `area_pre_check_rejects_large_bbox`
- `area_pre_check_accepts_small_bbox`
- `url_is_constructed_in_min_lon_min_lat_max_lon_max_lat_order`
- Error-mapping tests for each `OsmApiError` → user-string conversion.

No network tests — `fetch_bbox` is not factored further since `ureq` is hard
to fake without adding a trait indirection that YAGNI rules out. Manual
verification covers the network path.

## Out of scope

- Merging downloaded data with existing layers.
- Upload / editing.
- Progress percentage (XML is streamed by `quick-xml` but total size is
  unknown up front; a spinner-level indicator is sufficient).
- Overpass API.
- Persistent cache of downloaded data.
