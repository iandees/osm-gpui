# OSM API Download Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `File > Download from OSM` menu item (⌘⇧D) that fetches OSM data for the current viewport bbox from `api.openstreetmap.org/api/0.6/map`, parses it with the existing `OsmParser`, and adds it as a new `OsmLayer`.

**Architecture:** A new `src/osm_api.rs` module encapsulates the area pre-check, URL construction, HTTP GET via `ureq`, and error mapping. `main.rs` adds a new action, menu entry, key binding, a transient status-message overlay, and a second cross-thread queue for API errors. Success results reuse the existing `SHARED_OSM_DATA` queue so layer creation stays in one place.

**Tech Stack:** Rust, GPUI, `ureq` 2.9 (already a dependency), `quick-xml` via the existing `OsmParser`.

---

## File Structure

- **Create:** `src/osm_api.rs` — pure-ish module holding `fetch_bbox`, `OsmApiError`, the area pre-check, and URL construction. Testable without network for everything except the HTTP call itself.
- **Modify:** `src/main.rs` — register `mod osm_api;`, add `DownloadFromOsm` action, menu entry, ⌘⇧D key binding, `status_message` field on `MapViewer`, `API_ERROR_MESSAGES` queue, `check_for_api_fetch_results`, and status-overlay rendering.

No other files are touched.

---

## Task 1: Scaffold `osm_api` module with area pre-check

**Files:**
- Create: `src/osm_api.rs`
- Modify: `src/main.rs:4-9` (add `mod osm_api;`)

- [ ] **Step 1: Write the failing tests**

Create `src/osm_api.rs` with just the test module to define the behavior we want:

```rust
use crate::coordinates::GeoBounds;
use crate::osm::OsmParseError;

const MAX_AREA_SQ_DEG: f64 = 0.25;

#[derive(Debug)]
pub enum OsmApiError {
    AreaTooLarge { area_sq_deg: f64 },
    Http { status: u16, body: String },
    Network(String),
    Parse(OsmParseError),
}

impl std::fmt::Display for OsmApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OsmApiError::AreaTooLarge { .. } => {
                write!(f, "Area too large for OSM API (zoom in and try again)")
            }
            OsmApiError::Http { status: 400, .. } => {
                write!(f, "OSM API rejected request (400) — try a smaller area")
            }
            OsmApiError::Http { status: 509, .. } => {
                write!(f, "OSM API rate-limited (509) — try again later")
            }
            OsmApiError::Http { status, body } => {
                let first_line = body.lines().next().unwrap_or("");
                write!(f, "OSM API error {}: {}", status, first_line)
            }
            OsmApiError::Network(msg) => write!(f, "Network error: {}", msg),
            OsmApiError::Parse(e) => write!(f, "Failed to parse OSM response: {}", e),
        }
    }
}

pub(crate) fn check_area(bounds: &GeoBounds) -> Result<(), OsmApiError> {
    let area = (bounds.max_lon - bounds.min_lon) * (bounds.max_lat - bounds.min_lat);
    if area > MAX_AREA_SQ_DEG {
        Err(OsmApiError::AreaTooLarge { area_sq_deg: area })
    } else {
        Ok(())
    }
}

pub(crate) fn build_url(bounds: &GeoBounds) -> String {
    format!(
        "https://api.openstreetmap.org/api/0.6/map?bbox={:.7},{:.7},{:.7},{:.7}",
        bounds.min_lon, bounds.min_lat, bounds.max_lon, bounds.max_lat
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_check_rejects_large_bbox() {
        let b = GeoBounds::new(40.0, 41.0, -75.0, -74.0); // 1.0 sq deg
        assert!(matches!(check_area(&b), Err(OsmApiError::AreaTooLarge { .. })));
    }

    #[test]
    fn area_check_accepts_small_bbox() {
        let b = GeoBounds::new(40.70, 40.75, -74.02, -73.98); // 0.002 sq deg
        assert!(check_area(&b).is_ok());
    }

    #[test]
    fn area_check_accepts_exact_limit() {
        let b = GeoBounds::new(40.0, 40.5, -74.0, -73.5); // 0.25 sq deg exactly
        assert!(check_area(&b).is_ok());
    }

    #[test]
    fn url_is_min_lon_min_lat_max_lon_max_lat() {
        let b = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let url = build_url(&b);
        assert_eq!(
            url,
            "https://api.openstreetmap.org/api/0.6/map?bbox=-74.0200000,40.7000000,-73.9800000,40.7500000"
        );
    }

    #[test]
    fn display_area_too_large_is_user_readable() {
        let e = OsmApiError::AreaTooLarge { area_sq_deg: 1.0 };
        assert_eq!(e.to_string(), "Area too large for OSM API (zoom in and try again)");
    }

    #[test]
    fn display_http_400_mentions_smaller_area() {
        let e = OsmApiError::Http { status: 400, body: "too many nodes".into() };
        assert_eq!(e.to_string(), "OSM API rejected request (400) — try a smaller area");
    }

    #[test]
    fn display_http_509_mentions_rate_limit() {
        let e = OsmApiError::Http { status: 509, body: String::new() };
        assert_eq!(e.to_string(), "OSM API rate-limited (509) — try again later");
    }

    #[test]
    fn display_http_other_uses_first_body_line() {
        let e = OsmApiError::Http { status: 503, body: "Service down\nretry later".into() };
        assert_eq!(e.to_string(), "OSM API error 503: Service down");
    }
}
```

Then add `mod osm_api;` to `src/main.rs` in the module list (after `mod osm;`):

```rust
mod coordinates;
mod osm;
mod osm_api;
mod tile_cache;
mod tiles;
mod viewport;
mod layers;
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test --lib osm_api`
Expected: all 8 tests pass. (The code is written alongside the tests in Step 1 because they're tightly coupled pure functions; a strict red-green split would just mean committing a non-compiling file.)

- [ ] **Step 3: Commit**

```bash
git add src/osm_api.rs src/main.rs
git commit -m "Add osm_api module with area pre-check and URL builder"
```

---

## Task 2: Implement `fetch_bbox` (HTTP + parse)

**Files:**
- Modify: `src/osm_api.rs` (add `fetch_bbox` function)

- [ ] **Step 1: Add the `fetch_bbox` function**

Append to `src/osm_api.rs` (above the `#[cfg(test)] mod tests`):

```rust
use crate::osm::{OsmData, OsmParser};

const USER_AGENT: &str = concat!("osm-gpui/", env!("CARGO_PKG_VERSION"));

/// Synchronous fetch — call from a worker thread, not the UI thread.
pub fn fetch_bbox(bounds: GeoBounds) -> Result<OsmData, OsmApiError> {
    check_area(&bounds)?;

    let url = build_url(&bounds);
    let response = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .call();

    let body = match response {
        Ok(resp) => resp
            .into_string()
            .map_err(|e| OsmApiError::Network(e.to_string()))?,
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            return Err(OsmApiError::Http { status, body });
        }
        Err(e) => return Err(OsmApiError::Network(e.to_string())),
    };

    OsmParser::new()
        .parse_str(&body)
        .map_err(OsmApiError::Parse)
}
```

- [ ] **Step 2: Verify the crate still builds**

Run: `cargo build`
Expected: builds cleanly. (No new unit test for the network path — it requires the live API and would be flaky. Manual verification happens in Task 6.)

- [ ] **Step 3: Commit**

```bash
git add src/osm_api.rs
git commit -m "Add fetch_bbox synchronous HTTP + parse function"
```

---

## Task 3: Add `DownloadFromOsm` action, menu item, and key binding

**Files:**
- Modify: `src/main.rs:17` (actions list)
- Modify: `src/main.rs:517-540` (action registration, menu, key bindings)
- Modify: `src/main.rs:574-614` (new handler alongside `open_osm_file`)

- [ ] **Step 1: Extend the actions macro**

Change line 17 in `src/main.rs` from:

```rust
actions!(osm_gpui, [OpenOsmFile, Quit, AddOsmCarto]);
```

to:

```rust
actions!(osm_gpui, [OpenOsmFile, Quit, AddOsmCarto, DownloadFromOsm]);
```

- [ ] **Step 2: Add a stub handler that just prints**

Append after `add_osm_carto` at the end of `src/main.rs`:

```rust
// Handle the File > Download from OSM menu action
fn download_from_osm(_: &DownloadFromOsm, _cx: &mut App) {
    println!("🌐 File > Download from OSM menu action triggered");
}
```

- [ ] **Step 3: Register the action and add the menu entry + key binding**

In `main()` after the existing `cx.on_action(add_osm_carto);` line (around line 520), add:

```rust
cx.on_action(download_from_osm);
```

In the File menu (around line 533-535), change:

```rust
Menu {
    name: "File".into(),
    items: vec![MenuItem::action("Open…\t⌘O", OpenOsmFile)],
},
```

to:

```rust
Menu {
    name: "File".into(),
    items: vec![
        MenuItem::action("Open…\t⌘O", OpenOsmFile),
        MenuItem::action("Download from OSM\t⌘⇧D", DownloadFromOsm),
    ],
},
```

In the `cx.bind_keys` block (around line 558-561), add the ⌘⇧D binding:

```rust
cx.bind_keys([
    KeyBinding::new("cmd-o", OpenOsmFile, None),
    KeyBinding::new("cmd-shift-d", DownloadFromOsm, None),
    KeyBinding::new("cmd-q", Quit, None),
]);
```

- [ ] **Step 4: Verify build and menu wiring**

Run: `cargo build`
Expected: builds cleanly. Manually run `cargo run`, open the File menu, confirm "Download from OSM ⌘⇧D" appears, click it, confirm the log line prints. Then quit.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "Add DownloadFromOsm action, menu item, and key binding"
```

---

## Task 4: Add status-message overlay to `MapViewer`

**Files:**
- Modify: `src/main.rs:27-32` (`MapViewer` struct)
- Modify: `src/main.rs:35-50` (`MapViewer::new`)
- Modify: `src/main.rs:376-394` (debug overlay region — add status line alongside)

Goal: a transient one-line status message rendered in a corner that auto-clears after 5 seconds.

- [ ] **Step 1: Add imports for `Instant` and `Duration`**

In `src/main.rs`, update the `std::sync` import near the top:

```rust
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
```

- [ ] **Step 2: Extend `MapViewer` with a status field**

Change the struct:

```rust
struct MapViewer {
    viewport: Viewport,
    layer_manager: LayerManager,
    tile_cache: Arc<Mutex<TileCache>>,
    first_dataset_fitted: bool,
    status_message: Option<(String, Instant)>,
}
```

And the constructor:

```rust
Self {
    viewport,
    layer_manager,
    tile_cache,
    first_dataset_fitted: false,
    status_message: None,
}
```

- [ ] **Step 3: Add a helper for setting/expiring the status message**

Add these methods to `impl MapViewer`:

```rust
fn set_status(&mut self, message: impl Into<String>) {
    self.status_message = Some((message.into(), Instant::now()));
}

fn expire_status(&mut self) {
    if let Some((_, set_at)) = &self.status_message {
        if set_at.elapsed() > Duration::from_secs(5) {
            self.status_message = None;
        }
    }
}
```

- [ ] **Step 4: Render the status message as a top-center overlay**

In `Render::render`, call `self.expire_status();` near the top (right after `self.check_for_layer_requests(cx);`).

Then, inside the map-area `div` (the one with `.relative()` starting around line 325), after the debug-info overlay child, add:

```rust
.child({
    let status = self.status_message.clone();
    if let Some((msg, _)) = status {
        div()
            .absolute()
            .top_4()
            .right_4()
            .p_3()
            .bg(gpui::black())
            .rounded_lg()
            .text_color(rgb(0xffffff))
            .text_sm()
            .opacity(0.9)
            .child(msg)
            .into_any_element()
    } else {
        div().into_any_element()
    }
})
```

- [ ] **Step 5: Verify build**

Run: `cargo build`
Expected: builds cleanly. (No unit test — this is a visual change validated manually in Task 6.)

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "Add transient status-message overlay to MapViewer"
```

---

## Task 5: Wire the handler to fetch, queue results, and surface errors

**Files:**
- Modify: `src/main.rs` — replace stub `download_from_osm`, add `API_ERROR_MESSAGES` queue, add `check_for_api_fetch_results`, invoke it in `render`.

- [ ] **Step 1: Add the error queue**

Near the existing `SHARED_OSM_DATA` / `LAYER_REQUESTS` definitions (around line 20-25), add:

```rust
static API_ERROR_MESSAGES: std::sync::OnceLock<Arc<Mutex<Vec<String>>>> =
    std::sync::OnceLock::new();
```

In `main()` where the other `OnceLock::set` calls happen (around line 510-511), add:

```rust
API_ERROR_MESSAGES.set(Arc::new(Mutex::new(Vec::new()))).unwrap();
```

- [ ] **Step 2: Replace the `download_from_osm` stub**

Replace the stub from Task 3 with:

```rust
// Handle the File > Download from OSM menu action
fn download_from_osm(_: &DownloadFromOsm, cx: &mut App) {
    println!("🌐 File > Download from OSM menu action triggered");

    // We need the current viewport bounds, but the handler runs in App context
    // (no direct access to MapViewer). Solution: the handler queues a request,
    // and MapViewer picks it up on the next render where it has `self`.
    if let Some(requests) = DOWNLOAD_REQUESTS.get() {
        if let Ok(mut q) = requests.lock() {
            q.push(());
        }
    }

    // Use the background executor via the queue pattern; the render loop kicks
    // off the actual thread because only there do we know the bbox.
    let _ = cx; // retained in case future signaling is needed
}
```

Add a small request queue — this inverts the flow from the earlier design so the bbox read happens on the UI thread (where `MapViewer` lives):

```rust
static DOWNLOAD_REQUESTS: std::sync::OnceLock<Arc<Mutex<Vec<()>>>> =
    std::sync::OnceLock::new();
```

And in `main()` alongside the other `set` calls:

```rust
DOWNLOAD_REQUESTS.set(Arc::new(Mutex::new(Vec::new()))).unwrap();
```

- [ ] **Step 3: Add `check_for_download_requests` on `MapViewer`**

Add this method to `impl MapViewer`:

```rust
fn check_for_download_requests(&mut self, cx: &mut Context<Self>) {
    let Some(requests) = DOWNLOAD_REQUESTS.get() else { return };
    let pending = if let Ok(mut guard) = requests.try_lock() {
        let n = guard.len();
        guard.clear();
        n
    } else {
        0
    };
    if pending == 0 { return }

    let bounds = self.viewport.visible_bounds();

    // Pre-check on the UI thread so the user gets instant feedback without
    // waiting for a thread to spawn.
    if let Err(e) = osm_api::check_area(&bounds) {
        self.set_status(e.to_string());
        cx.notify();
        return;
    }

    self.set_status("Downloading OSM data…");

    let data_queue = SHARED_OSM_DATA.get().unwrap().clone();
    let error_queue = API_ERROR_MESSAGES.get().unwrap().clone();
    let label = format!(
        "OSM API ({:.4},{:.4},{:.4},{:.4})",
        bounds.min_lat, bounds.min_lon, bounds.max_lat, bounds.max_lon
    );

    std::thread::spawn(move || {
        match osm_api::fetch_bbox(bounds) {
            Ok(data) => {
                if let Ok(mut q) = data_queue.lock() {
                    q.push((label, data));
                }
            }
            Err(e) => {
                if let Ok(mut q) = error_queue.lock() {
                    q.push(e.to_string());
                }
            }
        }
    });

    cx.notify();
}
```

Note: `osm_api::check_area` is currently `pub(crate)`; that stays as-is so it's callable here.

- [ ] **Step 4: Add `check_for_api_errors` on `MapViewer`**

```rust
fn check_for_api_errors(&mut self, cx: &mut Context<Self>) {
    let Some(queue) = API_ERROR_MESSAGES.get() else { return };
    if let Ok(mut guard) = queue.try_lock() {
        if let Some(msg) = guard.pop() {
            guard.clear();
            self.set_status(msg);
            cx.notify();
        }
    }
}
```

Also, when a successful OSM data push lands, clear the "Downloading…" status. Modify `check_for_new_osm_data` — find the existing `cx.notify();` at the end of its drain loop and insert just before it:

```rust
self.status_message = None;
```

(Only when `guard` was non-empty, i.e. still inside the `if !guard.is_empty() { ... }` block.)

- [ ] **Step 5: Invoke the new drains in `render`**

In `Render::render`, after the existing `self.check_for_layer_requests(cx);` call, add:

```rust
self.check_for_download_requests(cx);
self.check_for_api_errors(cx);
```

`expire_status` from Task 4 should still be called here too — order: `expire_status` last so a freshly-set message isn't cleared by a stale timer from earlier in the frame. Final order:

```rust
self.check_for_new_osm_data(cx);
self.check_for_layer_requests(cx);
self.check_for_download_requests(cx);
self.check_for_api_errors(cx);
self.expire_status();
```

- [ ] **Step 6: Verify build**

Run: `cargo build`
Expected: builds cleanly.

Run: `cargo test`
Expected: all existing tests plus the 8 new `osm_api` tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "Wire Download from OSM to fetch bbox and surface status"
```

---

## Task 6: Manual end-to-end verification

**Files:** none — manual test.

- [ ] **Step 1: Small-bbox happy path**

Run: `cargo run`

In the app: pan/zoom to a small area (e.g. a few NYC blocks — well under 0.25 sq degrees). Use File > Download from OSM (or ⌘⇧D).

Expected:
- Status overlay shows "Downloading OSM data…" within ~100ms.
- After a few seconds, a new layer named `OSM API (lat_min,lon_min,lat_max,lon_max)` appears in the right panel.
- Nodes/ways render (yellow squares + blue lines).
- Status overlay clears on success.

- [ ] **Step 2: Too-large bbox**

Zoom out so the viewport covers clearly more than 0.25 sq degrees (e.g. most of a US state). Trigger the download.

Expected:
- No network request made.
- Status overlay shows "Area too large for OSM API (zoom in and try again)" for ~5 seconds, then clears.
- No new layer added.

- [ ] **Step 3: Second download stacks a new layer**

With a small bbox, trigger the download twice at slightly different viewports.

Expected: two separate `OSM API (...)` layers in the panel, each with the bbox of its fetch — per the design's "add a new layer each time" decision.

- [ ] **Step 4: Commit anything uncovered during testing**

If the manual tests surfaced bugs, fix them in separate commits. If everything worked, no commit needed.

---

## Self-Review Notes

- **Spec coverage:** All spec sections mapped to tasks: area pre-check (Task 1/5), URL construction (Task 1), `fetch_bbox` + error variants (Task 1/2), action/menu/key (Task 3), status overlay (Task 4), queue + error surfacing + layer creation (Task 5), manual tests (Task 6).
- **Type consistency:** `check_area`, `build_url`, `fetch_bbox`, `OsmApiError` names used consistently across tasks. `status_message` field used consistently. `DOWNLOAD_REQUESTS` / `API_ERROR_MESSAGES` / `SHARED_OSM_DATA` queue names consistent.
- **Deviation from spec:** The spec mentioned a combined `API_FETCH_RESULTS` queue carrying `Result<OsmData, String>`. In the plan we split that into reuse of `SHARED_OSM_DATA` for successes + a new `API_ERROR_MESSAGES` for failures, because it avoids duplicating the layer-creation code in `check_for_new_osm_data`. Net behavior is the same. Also added `DOWNLOAD_REQUESTS` so the handler (in `App` context) can defer bbox reading to `MapViewer` (in `Context<Self>`).
