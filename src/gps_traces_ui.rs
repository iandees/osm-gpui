//! File > Download GPS Traces: fetch the public GPS traces around the
//! downloaded data (or the visible map) from the OSM API and show them as a
//! read-only "GPS Traces" reference layer below the data.
//!
//! The paging, fetching and parsing live in the library
//! (`osm_gpui::gps_traces`); this is the part that needs the window. A page
//! takes the server several seconds, so the pages are fetched one at a time
//! in the background and the layer is redrawn as each arrives. Reads are
//! anonymous unless a login is already in memory (so this never raises the
//! keychain prompt); progress and failures show in the status bar.
//! Downloading again replaces the layer; deleting the layer from the Layers
//! list stops a running download.

use std::sync::Arc;

use gpui::{actions, Context, Window};
use osm_gpui::auth;
use osm_gpui::gps_traces::{self, PageWalker, TracePlan};
use osm_gpui::layers::reference_layer::ReferenceLayer;
use osm_gpui::layers::{LayerId, MapLayer as _};
use osm_gpui::reference::ReferenceData;
use osm_gpui::settings_store;
use osm_gpui::{tr, tr_count};

use crate::MapViewer;

actions!(osm_gpui, [DownloadGpsTraces]);

/// Everything about downloaded GPS traces that the window holds.
#[derive(Default)]
pub(crate) struct GpsTracesState {
    /// The GPS Traces layer (it may since have been deleted from the Layers
    /// list).
    layer: Option<LayerId>,
    fetching: bool,
    /// The running download has put its traces on the map.
    shown: bool,
}

impl MapViewer {
    pub(crate) fn on_download_gps_traces(
        &mut self,
        _: &DownloadGpsTraces,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.download_gps_traces(cx);
    }

    pub(crate) fn download_gps_traces(&mut self, cx: &mut Context<Self>) {
        if self.gps_traces.fetching {
            self.set_status(tr!("gps_traces.state.busy"));
            cx.notify();
            return;
        }
        let downloaded: Vec<_> = self
            .layer_manager
            .layers()
            .iter()
            .filter(|l| l.is_visible())
            .flat_map(|l| l.download_rects().iter().map(osm_gpui::notes::merc_to_geo))
            .collect();
        let cells = match gps_traces::plan(&downloaded, &self.viewport.visible_bounds()) {
            TracePlan::Downloaded(cells) | TracePlan::Viewport(cells) => cells,
            TracePlan::TooLarge => {
                self.set_status(tr!("gps_traces.state.too_large"));
                cx.notify();
                return;
            }
        };

        self.gps_traces.fetching = true;
        self.gps_traces.shown = false;
        self.set_status(tr!("gps_traces.state.loading"));
        cx.notify();

        let base_url = settings_store::api_base_url();
        cx.spawn(async move |this, cx| {
            let mut walker = PageWalker::new(cells);
            let mut pages: Vec<Arc<[u8]>> = Vec::new();
            while let Some((cell, page)) = walker.next_request() {
                let base = base_url.clone();
                let body = cx
                    .background_executor()
                    .spawn(async move {
                        let token = auth::fresh_token_if_loaded(&auth::oauth_base_for(&base))
                            .map(|t| t.access_token);
                        gps_traces::fetch_page(&base, &cell, page, token.as_deref())
                    })
                    .await;
                let body = match body {
                    Ok(body) => body,
                    Err(e) => {
                        let _ = this.update(cx, |this, cx| this.end_gps_traces(Err(e), cx));
                        return;
                    }
                };
                let n = osm_gpui::gpx::count_trackpoints(&body);
                walker.record(n);
                if n == 0 {
                    continue;
                }
                pages.push(body.into());
                // Parse everything so far off the UI thread; the layer is
                // rebuilt from all pages (tracks are split across them).
                let snapshot = pages.clone();
                let parsed = cx
                    .background_executor()
                    .spawn(async move { gps_traces::parse_pages(&snapshot) })
                    .await;
                let points = walker.points;
                let more = this
                    .update(cx, |this, cx| this.gps_traces_page(parsed, points, cx))
                    .unwrap_or(false);
                if !more {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| this.end_gps_traces(Ok(walker), cx));
        })
        .detach();
    }

    /// A page arrived and everything so far parsed to `parsed`. Returns
    /// whether to keep downloading.
    fn gps_traces_page(
        &mut self,
        parsed: Result<Option<ReferenceData>, String>,
        points: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.gps_traces.shown && !self.gps_traces_layer_present() {
            // Deleted from the Layers list mid-download: stop.
            self.gps_traces.fetching = false;
            self.gps_traces.layer = None;
            cx.notify();
            return false;
        }
        match parsed {
            Ok(Some(data)) => {
                self.show_gps_traces(data);
                self.set_status(tr_count!("gps_traces.state.progress", points));
                cx.notify();
                true
            }
            Ok(None) => true,
            Err(e) => {
                self.end_gps_traces(Err(e), cx);
                false
            }
        }
    }

    fn gps_traces_layer_present(&self) -> bool {
        self.gps_traces
            .layer
            .is_some_and(|id| self.layer_manager.find_layer(id).is_some())
    }

    /// Put `data` on the map: in place of the GPS Traces layer (keeping its
    /// place in the list and its visibility) or as a new reference layer.
    fn show_gps_traces(&mut self, data: ReferenceData) {
        self.gps_traces.shown = true;
        if let Some(id) = self.gps_traces.layer {
            if let Some(old) = self.layer_manager.find_layer(id) {
                let mut layer = ReferenceLayer::new(id, old.name().to_string(), data);
                layer.set_visible(old.is_visible());
                self.layer_manager.replace(id, Box::new(layer));
                return;
            }
        }
        let name = self
            .layer_manager
            .unique_name(&tr!("gps_traces.layer_name"));
        let id = self.layer_manager.alloc_id();
        self.layer_manager
            .add_reference_layer(Box::new(ReferenceLayer::new(id, name, data)));
        self.gps_traces.layer = Some(id);
    }

    /// The download is over: every page fetched (`Ok`) or one failed.
    fn end_gps_traces(&mut self, result: Result<PageWalker, String>, cx: &mut Context<Self>) {
        self.gps_traces.fetching = false;
        match result {
            Ok(_) if !self.gps_traces.shown => {
                // Nothing here: drop the last download's traces, which
                // belong to another area.
                self.remove_gps_traces_layer();
                self.set_status(tr!("gps_traces.state.none"));
            }
            Ok(walker) if walker.truncated => {
                self.set_status(tr!("gps_traces.state.truncated", count = walker.points));
            }
            Ok(walker) => {
                self.set_status(tr_count!("gps_traces.state.loaded", walker.points));
            }
            Err(detail) => {
                // What already arrived stays on the map.
                self.set_status(tr!("gps_traces.state.failed", detail = detail));
            }
        }
        cx.notify();
    }

    /// Drop the GPS Traces layer, if it is still in the list.
    fn remove_gps_traces_layer(&mut self) {
        let Some(id) = self.gps_traces.layer.take() else {
            return;
        };
        if let Some(i) = self
            .layer_manager
            .layers()
            .iter()
            .position(|l| l.id() == id)
        {
            self.layer_manager.remove_at(i);
        }
    }
}
