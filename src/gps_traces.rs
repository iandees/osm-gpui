//! Public GPS traces from the OSM API, drawn as a read-only reference layer.
//!
//! `GET /api/0.6/trackpoints?bbox=l,b,r,t&page=N` answers with GPX holding
//! at most [`PER_PAGE`] points; the box may be at most [`MAX_SPAN_DEG`] wide
//! and tall. An area is cut into cells under that span and each cell is paged
//! until a page comes back short or its share of the point budget runs out.
//! Reading is
//! anonymous. The pages are turned into a [`ReferenceData`] by
//! `gpx::parse_trackpoints`.
//!
//! Like notes, nothing here touches the window; the HTTP client is injected
//! so all of it is tested with the `FakeClient`.

use crate::coordinates::GeoBounds;
use crate::http::{
    fetch_with_retries, HttpClient, HttpError, HttpRequest, RetryPolicy, UreqClient,
};
use crate::reference::{RefError, ReferenceData};
use crate::tr;

/// Points per page (the server's `tracepoints per_page`).
pub const PER_PAGE: usize = 5000;
/// Largest width and height of one request's box, in degrees.
pub const MAX_SPAN_DEG: f64 = 0.25;
/// Most cells one download may cover; beyond this the area is too large.
pub const MAX_CELLS: usize = 16;
/// Most points one download fetches over all cells, shared evenly between
/// them (at least one page per cell). A page takes the server several
/// seconds, so this bounds a download to about a minute.
pub const MAX_POINTS: usize = 50_000;

/// What a download will request.
#[derive(Debug, Clone, PartialEq)]
pub enum TracePlan {
    /// The box around the downloaded data, cut into cells.
    Downloaded(Vec<GeoBounds>),
    /// The visible map (no downloaded data, or it spans too much).
    Viewport(Vec<GeoBounds>),
    /// Even the visible map is too large: zoom in.
    TooLarge,
}

/// Cut `b` into a grid of cells each at most `max_deg` wide and tall.
pub fn split_for_span(b: &GeoBounds, max_deg: f64) -> Vec<GeoBounds> {
    let (w, h) = (b.width(), b.height());
    if !(w.is_finite() && h.is_finite() && max_deg > 0.0) || w <= 0.0 || h <= 0.0 {
        return vec![*b];
    }
    let cols = (w / max_deg).ceil().max(1.0) as usize;
    let rows = (h / max_deg).ceil().max(1.0) as usize;
    let mut cells = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        for c in 0..cols {
            cells.push(GeoBounds::new(
                b.min_lat + h * r as f64 / rows as f64,
                b.min_lat + h * (r + 1) as f64 / rows as f64,
                b.min_lon + w * c as f64 / cols as f64,
                b.min_lon + w * (c + 1) as f64 / cols as f64,
            ));
        }
    }
    cells
}

/// The smallest box holding every one of `boxes` (`None` for none).
fn union(boxes: &[GeoBounds]) -> Option<GeoBounds> {
    boxes
        .iter()
        .filter(|b| b.width() > 0.0 && b.height() > 0.0)
        .copied()
        .reduce(|a, b| {
            GeoBounds::new(
                a.min_lat.min(b.min_lat),
                a.max_lat.max(b.max_lat),
                a.min_lon.min(b.min_lon),
                a.max_lon.max(b.max_lon),
            )
        })
}

/// Cover the downloaded data when there is some and it fits, otherwise the
/// visible map. One box around all the downloaded areas, so overlapping
/// downloads do not fetch the same points twice.
pub fn plan(downloaded: &[GeoBounds], viewport: &GeoBounds) -> TracePlan {
    if let Some(area) = union(downloaded) {
        let cells = split_for_span(&area, MAX_SPAN_DEG);
        if cells.len() <= MAX_CELLS {
            return TracePlan::Downloaded(cells);
        }
    }
    let cells = split_for_span(viewport, MAX_SPAN_DEG);
    if cells.len() <= MAX_CELLS {
        TracePlan::Viewport(cells)
    } else {
        TracePlan::TooLarge
    }
}

pub fn trackpoints_url(base_url: &str, cell: &GeoBounds, page: usize) -> String {
    format!(
        "{}/api/0.6/trackpoints?bbox={:.7},{:.7},{:.7},{:.7}&page={}",
        base_url.trim_end_matches('/'),
        cell.min_lon,
        cell.min_lat,
        cell.max_lon,
        cell.max_lat,
        page
    )
}

/// The requests of one download, in order: page after page of each cell
/// until a page comes back short or the cell's share of [`MAX_POINTS`] is
/// used. The share is even so every cell gets its newest traces (the server
/// sends trackable traces newest first, and a busy town holds more points
/// than the whole budget). Pages are slow (seconds each), so the window
/// drives this one request at a time and shows what has arrived.
#[derive(Debug, Clone)]
pub struct PageWalker {
    cells: Vec<GeoBounds>,
    pages_per_cell: usize,
    cell: usize,
    page: usize,
    /// Points received so far.
    pub points: usize,
    /// Some cell still had pages when its share ran out.
    pub truncated: bool,
}

impl PageWalker {
    pub fn new(cells: Vec<GeoBounds>) -> Self {
        let pages_per_cell = (MAX_POINTS / PER_PAGE / cells.len().max(1)).max(1);
        PageWalker {
            cells,
            pages_per_cell,
            cell: 0,
            page: 0,
            points: 0,
            truncated: false,
        }
    }

    /// The next request's box and page number; `None` when the download is
    /// complete.
    pub fn next_request(&self) -> Option<(GeoBounds, usize)> {
        self.cells.get(self.cell).map(|c| (*c, self.page))
    }

    /// The request from `next_request` returned `n` points.
    pub fn record(&mut self, n: usize) {
        self.points += n;
        self.page += 1;
        if n < PER_PAGE {
            self.next_cell();
        } else if self.page >= self.pages_per_cell {
            self.truncated = true;
            self.next_cell();
        }
    }

    fn next_cell(&mut self) {
        self.cell += 1;
        self.page = 0;
    }
}

fn http_message(e: HttpError) -> String {
    match e {
        HttpError::Status { status, body } => {
            let body = String::from_utf8_lossy(&body);
            let detail = body.lines().next().unwrap_or("").trim().to_string();
            match status {
                429 | 509 => tr!("error.api.rate_limited").to_string(),
                _ => tr!("error.api.http", status = status, detail = detail).to_string(),
            }
        }
        HttpError::Transport(msg) => tr!("error.network", detail = msg).to_string(),
        HttpError::Cancelled => tr!("error.network", detail = "cancelled").to_string(),
    }
}

/// `GET` one page. Anonymous unless `token` is given; retried like the map
/// download. `Err` is a sentence for the user.
pub fn fetch_page_with(
    client: &dyn HttpClient,
    base_url: &str,
    cell: &GeoBounds,
    page: usize,
    token: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut req = HttpRequest::get(trackpoints_url(base_url, cell, page));
    if let Some(token) = token {
        req = req.bearer(token);
    }
    fetch_with_retries(client, &req, &RetryPolicy::standard())
        .map(|resp| resp.body)
        .map_err(http_message)
}

pub fn fetch_page(
    base_url: &str,
    cell: &GeoBounds,
    page: usize,
    token: Option<&str>,
) -> Result<Vec<u8>, String> {
    fetch_page_with(&UreqClient::new(), base_url, cell, page, token)
}

/// The traces in `pages` (non-empty pages, in the order they arrived);
/// `None` when there are none.
pub fn parse_pages<P: AsRef<[u8]>>(pages: &[P]) -> Result<Option<ReferenceData>, String> {
    let slices: Vec<&[u8]> = pages.iter().map(AsRef::as_ref).collect();
    match crate::gpx::parse_trackpoints(&slices) {
        Ok(data) => Ok(Some(data)),
        Err(RefError::NoGeometry) => Ok(None),
        Err(e) => Err(tr!("gps_traces.error.parse", detail = e.to_string()).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{ok, status_err, FakeClient};

    const BASE: &str = "https://api.openstreetmap.org/";

    fn page_of(n: usize) -> String {
        let mut s = String::from(r#"<gpx version="1.0"><trk><trkseg>"#);
        for i in 0..n {
            let lat = 51.0 + (i % 1000) as f64 * 1e-5;
            s.push_str(&format!(
                r#"<trkpt lat="{lat}" lon="-0.1"><time>2020-01-01T00:00:00Z</time></trkpt>"#
            ));
        }
        s.push_str("</trkseg></trk></gpx>");
        s
    }

    #[test]
    fn url_lists_the_box_as_left_bottom_right_top() {
        let cell = GeoBounds::new(51.0, 51.1, -0.2, -0.1);
        assert_eq!(
            trackpoints_url(BASE, &cell, 2),
            "https://api.openstreetmap.org/api/0.6/trackpoints?bbox=-0.2000000,51.0000000,-0.1000000,51.1000000&page=2"
        );
    }

    #[test]
    fn split_keeps_every_cell_within_the_span_and_covers_the_box() {
        let b = GeoBounds::new(10.0, 10.6, 20.0, 20.3);
        let cells = split_for_span(&b, MAX_SPAN_DEG);
        assert_eq!(cells.len(), 3 * 2);
        for c in &cells {
            assert!(c.width() <= MAX_SPAN_DEG + 1e-12 && c.height() <= MAX_SPAN_DEG + 1e-12);
        }
        let area: f64 = cells.iter().map(|c| c.width() * c.height()).sum();
        assert!((area - b.width() * b.height()).abs() < 1e-9);
        // Small boxes stay whole.
        let small = GeoBounds::new(0.0, 0.1, 0.0, 0.1);
        assert_eq!(split_for_span(&small, MAX_SPAN_DEG), vec![small]);
    }

    #[test]
    fn plan_prefers_one_box_around_the_downloaded_areas() {
        let view = GeoBounds::new(0.0, 0.01, 0.0, 0.01);
        let a = GeoBounds::new(50.0, 50.1, 1.0, 1.1);
        let b = GeoBounds::new(50.05, 50.2, 1.05, 1.2);
        assert_eq!(
            plan(&[a, b], &view),
            TracePlan::Downloaded(vec![GeoBounds::new(50.0, 50.2, 1.0, 1.2)])
        );
        // No downloaded data: the view.
        assert_eq!(plan(&[], &view), TracePlan::Viewport(vec![view]));
        // Downloaded areas far apart span too much: the view instead.
        let far = GeoBounds::new(-30.0, -29.9, 100.0, 100.1);
        assert_eq!(plan(&[a, far], &view), TracePlan::Viewport(vec![view]));
        // A whole-country view is too large.
        let huge = GeoBounds::new(40.0, 45.0, -80.0, -70.0);
        assert_eq!(plan(&[], &huge), TracePlan::TooLarge);
    }

    /// The walker, the requested `(cell index, page)` pairs and the kept
    /// pages.
    type Walked = (PageWalker, Vec<(usize, usize)>, Vec<Vec<u8>>);

    /// Drive a walker the way the window does: fetch, record, keep non-empty
    /// pages.
    fn walk(client: &FakeClient, cells: &[GeoBounds]) -> Result<Walked, String> {
        let mut walker = PageWalker::new(cells.to_vec());
        let (mut asked, mut pages) = (Vec::new(), Vec::new());
        while let Some((cell, page)) = walker.next_request() {
            let i = cells.iter().position(|c| *c == cell).unwrap();
            asked.push((i, page));
            let body = fetch_page_with(client, BASE, &cell, page, None)?;
            let n = crate::gpx::count_trackpoints(&body);
            walker.record(n);
            if n > 0 {
                pages.push(body);
            }
        }
        Ok((walker, asked, pages))
    }

    #[test]
    fn pages_until_a_short_page_then_moves_to_the_next_cell() {
        let cells = [
            GeoBounds::new(51.0, 51.1, -0.2, -0.1),
            GeoBounds::new(51.1, 51.2, -0.2, -0.1),
        ];
        let client = FakeClient::new(vec![
            ok(200, page_of(PER_PAGE)),
            ok(200, page_of(10)),
            ok(200, r#"<gpx version="1.0"></gpx>"#),
        ]);
        let (walker, asked, pages) = walk(&client, &cells).unwrap();
        assert_eq!(asked, vec![(0, 0), (0, 1), (1, 0)]);
        assert_eq!(walker.points, PER_PAGE + 10);
        assert!(!walker.truncated);
        let data = parse_pages(&pages).unwrap().unwrap();
        assert_eq!(data.vertex_count, PER_PAGE + 10);
    }

    #[test]
    fn an_area_without_traces_is_not_an_error() {
        let cells = [GeoBounds::new(0.0, 0.1, 0.0, 0.1)];
        let client = FakeClient::new(vec![ok(200, r#"<gpx version="1.0"></gpx>"#)]);
        let (walker, _, pages) = walk(&client, &cells).unwrap();
        assert_eq!(walker.points, 0);
        assert!(parse_pages(&pages).unwrap().is_none());
    }

    #[test]
    fn stops_at_the_point_budget() {
        let cells = [GeoBounds::new(0.0, 0.1, 0.0, 0.1)];
        let pages = MAX_POINTS / PER_PAGE;
        let client = FakeClient::new((0..pages).map(|_| ok(200, page_of(PER_PAGE))).collect());
        let (walker, asked, _) = walk(&client, &cells).unwrap();
        assert_eq!(asked.len(), pages);
        assert!(walker.truncated);
        assert_eq!(walker.points, MAX_POINTS);
    }

    #[test]
    fn the_budget_is_shared_so_every_cell_gets_some() {
        // Every cell is busy; with the budget split four ways each gets the
        // same number of pages, and the last cell is not starved.
        let cells: Vec<GeoBounds> = (0..4)
            .map(|i| GeoBounds::new(0.0, 0.1, i as f64 * 0.1, (i + 1) as f64 * 0.1))
            .collect();
        let per_cell = (MAX_POINTS / PER_PAGE / cells.len()).max(1);
        let total = per_cell * cells.len();
        let client = FakeClient::new((0..total).map(|_| ok(200, page_of(PER_PAGE))).collect());
        let (walker, asked, _) = walk(&client, &cells).unwrap();
        assert_eq!(asked.len(), total);
        assert_eq!(asked.iter().filter(|(i, _)| *i == 3).count(), per_cell);
        assert!(walker.truncated);
    }

    #[test]
    fn many_cells_still_get_one_page_each() {
        let cells: Vec<GeoBounds> = (0..MAX_CELLS)
            .map(|i| GeoBounds::new(0.0, 0.1, i as f64 * 0.1, (i + 1) as f64 * 0.1))
            .collect();
        let mut walker = PageWalker::new(cells);
        let mut requests = 0;
        while walker.next_request().is_some() {
            walker.record(PER_PAGE);
            requests += 1;
        }
        assert_eq!(requests, MAX_CELLS.max(MAX_POINTS / PER_PAGE));
    }

    #[test]
    fn a_server_error_is_reported() {
        let cell = GeoBounds::new(0.0, 0.1, 0.0, 0.1);
        let client = FakeClient::new(vec![status_err(400, "The bbox is too big")]);
        let err = fetch_page_with(&client, BASE, &cell, 0, None).unwrap_err();
        assert!(err.contains("400") && err.contains("bbox"), "{err}");
    }
}
