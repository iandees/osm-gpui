//! PMTiles vector overlays on the map: the dialog that adds one, opening the
//! archive in the background, and fetching the tiles the view needs.
//!
//! The pieces that do not need the app live in `osm_gpui::pmtiles` (reading
//! the archive), `osm_gpui::vector_tile` (decoding a tile) and
//! `osm_gpui::layers::vector_tile_layer` (drawing them).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Window};

use osm_gpui::layers::vector_tile_layer::{
    needed_tiles, tile_zoom, VectorTileLayer, VectorTileStore,
};
use osm_gpui::layers::LayerId;
use osm_gpui::pmtiles::{Archive, SourceSpec};
use osm_gpui::tiles::TileCoord;
use osm_gpui::tr;
use osm_gpui::vector_tile::{self, DecodedTile};

use crate::MapViewer;

gpui::actions!(osm_gpui, [AddPmtilesLayer]);

/// How long a tile that failed to load is left alone before it is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Tiles in flight at once, per overlay.
const MAX_CONCURRENT_TILES: usize = 6;
/// More tiles than this under the view means the map is zoomed out too far
/// (the archive's lowest zoom is not low enough) to fetch them all.
const MAX_TILES_PER_VIEW: usize = 40;
/// Decoded tiles kept per overlay before those out of view are dropped.
const MAX_CACHED_TILES: usize = 200;

struct Overlay {
    layer: LayerId,
    name: String,
    archive: Arc<Archive>,
    store: VectorTileStore,
    in_flight: HashSet<TileCoord>,
    failed: HashMap<TileCoord, Instant>,
}

#[derive(Default)]
pub(crate) struct PmtilesState {
    overlays: Vec<Overlay>,
}

impl MapViewer {
    pub(crate) fn on_open_pmtiles_dialog(
        &mut self,
        _: &AddPmtilesLayer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_pmtiles_dialog(window, cx);
    }

    /// Open the "Add PMTiles layer" dialog, if it isn't already open.
    pub(crate) fn open_pmtiles_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use osm_gpui::ui::pmtiles_dialog::{DialogEvent, PmtilesDialog};
        if self.dialogs.pmtiles_dialog.is_some() {
            return;
        }
        let dialog = cx.new(|cx| PmtilesDialog::new(window, cx));
        cx.subscribe(&dialog, |this, _entity, event: &DialogEvent, cx| {
            this.dialogs.pmtiles_dialog = None;
            match event {
                DialogEvent::Cancelled => cx.notify(),
                DialogEvent::Submitted { source, name } => {
                    this.add_pmtiles(source.clone(), name.clone(), cx)
                }
            }
        })
        .detach();
        self.dialogs.pmtiles_dialog = Some(dialog);
        cx.notify();
    }

    /// Open the archive in the background and, once it is read, show it as a
    /// layer.
    pub(crate) fn add_pmtiles(&mut self, source: SourceSpec, name: String, cx: &mut Context<Self>) {
        let name = if name.trim().is_empty() {
            source.default_name()
        } else {
            name.trim().to_string()
        };
        self.set_status(tr!("pmtiles.state.opening", name = name));
        cx.notify();
        let http = self.http.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { source.open(http) })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(archive) => this.finish_open_pmtiles(name, archive, cx),
                Err(e) => {
                    this.set_status(tr!(
                        "pmtiles.state.open_failed",
                        name = name,
                        detail = format!("{e:#}")
                    ));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn finish_open_pmtiles(&mut self, name: String, archive: Archive, cx: &mut Context<Self>) {
        let name = self.layer_manager.unique_name(&name);
        let id = self.layer_manager.alloc_id();
        let store = VectorTileStore::default();
        let layer = VectorTileLayer::new(
            id,
            name.clone(),
            u32::from(archive.header.min_zoom),
            u32::from(archive.header.max_zoom),
            store.clone(),
        );
        self.layer_manager.add_layer(Box::new(layer));
        self.pmtiles.overlays.push(Overlay {
            layer: id,
            name,
            archive: Arc::new(archive),
            store,
            in_flight: HashSet::new(),
            failed: HashMap::new(),
        });
        let last = self.pmtiles.overlays.len() - 1;
        self.fetch_pmtiles_tiles(last, cx);
        cx.notify();
    }

    /// Per-frame upkeep, called from `render`: forgets overlays whose layer
    /// was deleted and fetches the tiles the map has moved onto. Cheap when
    /// nothing is missing.
    pub(crate) fn pmtiles_frame(&mut self, cx: &mut Context<Self>) {
        if self.pmtiles.overlays.is_empty() {
            return;
        }
        let layers = &self.layer_manager;
        self.pmtiles
            .overlays
            .retain(|o| layers.find_layer(o.layer).is_some());
        if self.viewport.is_dragging {
            return;
        }
        for i in 0..self.pmtiles.overlays.len() {
            let visible = self
                .layer_manager
                .find_layer(self.pmtiles.overlays[i].layer)
                .is_some_and(|l| l.is_visible());
            if visible {
                self.fetch_pmtiles_tiles(i, cx);
            }
        }
    }

    /// Start fetching the tiles overlay `i` still needs for the visible map
    /// (and a margin around it), a few at a time, in the background.
    fn fetch_pmtiles_tiles(&mut self, i: usize, cx: &mut Context<Self>) {
        let (min_z, max_z) = {
            let h = &self.pmtiles.overlays[i].archive.header;
            (u32::from(h.min_zoom), u32::from(h.max_zoom))
        };
        let z = tile_zoom(self.viewport.zoom_level(), min_z, max_z);
        let needed = needed_tiles(&self.viewport, z);
        if needed.len() > MAX_TILES_PER_VIEW {
            return;
        }
        let o = &mut self.pmtiles.overlays[i];
        if o.store.len() > MAX_CACHED_TILES {
            // Keep what the view needs and the coarser tiles that stand in for it.
            let mut keep: HashSet<TileCoord> = HashSet::new();
            for &t in &needed {
                let mut c = Some(t);
                while let Some(t) = c {
                    keep.insert(t);
                    c = t.parent();
                }
            }
            o.store.retain(|t| keep.contains(t));
        }
        let now = Instant::now();
        let mut started = Vec::new();
        for t in needed {
            if o.in_flight.len() >= MAX_CONCURRENT_TILES {
                break;
            }
            let recently_failed = o
                .failed
                .get(&t)
                .is_some_and(|at| now.duration_since(*at) < RETRY_AFTER);
            if o.store.has(t) || o.in_flight.contains(&t) || recently_failed {
                continue;
            }
            o.in_flight.insert(t);
            started.push(t);
        }
        if started.is_empty() {
            return;
        }
        let (layer, name) = (o.layer, o.name.clone());
        let archive = o.archive.clone();
        self.set_status(tr!("pmtiles.state.loading", name = name));
        cx.notify();
        for t in started {
            let archive = archive.clone();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { load_tile(&archive, t) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.finish_pmtiles_tile(layer, t, result, cx)
                });
            })
            .detach();
        }
    }

    fn finish_pmtiles_tile(
        &mut self,
        layer: LayerId,
        t: TileCoord,
        result: Result<DecodedTile, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(o) = self.pmtiles.overlays.iter_mut().find(|o| o.layer == layer) else {
            return;
        };
        o.in_flight.remove(&t);
        let name = o.name.clone();
        match result {
            Ok(tile) => {
                o.failed.remove(&t);
                o.store.insert(t, tile);
                if o.in_flight.is_empty() {
                    self.set_status(tr!("pmtiles.state.loaded", name = name));
                }
            }
            Err(detail) => {
                o.failed.insert(t, Instant::now());
                self.set_status(tr!(
                    "pmtiles.state.tile_failed",
                    name = name,
                    detail = detail
                ));
            }
        }
        cx.notify();
    }
}

/// Read and decode tile `t`; a tile the archive does not have is an empty one.
fn load_tile(archive: &Archive, t: TileCoord) -> Result<DecodedTile, String> {
    let bytes = archive
        .tile(t.z as u8, t.x, t.y)
        .map_err(|e| format!("{e:#}"))?;
    match bytes {
        Some(b) => vector_tile::decode(&b, t),
        None => Ok(DecodedTile::default()),
    }
}
