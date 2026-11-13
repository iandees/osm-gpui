//! Street-level imagery in the window: View > Show Mapillary/Fotoshi/Panoramax
//! Photos, fetching for the visible map, the dots, and the photo viewer in the
//! side panel.
//!
//! The model, the tile fetching and the layer live in the library
//! (`osm_gpui::street_imagery`, `osm_gpui::street_imagery_tiles`,
//! `osm_gpui::layers::street_imagery_layer`); this is the part that needs the
//! window. The tiles under the view are fetched in the background, a few at a
//! time and cached, once the map is zoomed in far enough that there are not
//! too many of them; this never blocks editing, and failures show in the
//! status bar. Clicking a dot fetches that photo's details (image URLs and
//! author, which the tiles leave out), downloads the larger rendition and
//! shows it in the side panel with its attribution.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{div, img, prelude::*, px, AnyElement, Context, RenderImage, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    label::Label,
    v_flex, ActiveTheme as _, IconName, Sizable as _,
};
use osm_gpui::coordinates::GeoBounds;
use osm_gpui::http::UreqClient;
use osm_gpui::layers::street_imagery_layer::{self, StreetImageryLayer};
use osm_gpui::layers::LayerId;
use osm_gpui::selection::{FeatureKind, FeatureRef};
use osm_gpui::settings_store;
use osm_gpui::street_imagery::{
    self as imagery, ImageryError, ObjectPhotos, PhotoStore, Provider, StreetPhoto,
};
use osm_gpui::street_imagery_tiles::{self as tiles, TileId};
use osm_gpui::tr;
use osm_gpui::ui::sign_in_dialog::SignInPurpose;
use osm_gpui::ui::style::muted_text_size;

use crate::{EditMode, MapViewer};

gpui::actions!(
    osm_gpui,
    [
        ToggleMapillary,
        ToggleFotoshi,
        TogglePanoramax,
        RefreshStreetImagery
    ]
);

/// How long a tile that failed to load is left alone before it is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Tiles in flight at once, per provider.
const MAX_CONCURRENT_TILES: usize = 6;

/// Cached tiles per provider before those out of view are dropped.
const MAX_CACHED_TILES: usize = 300;

/// Height of the photo in the side panel.
const PREVIEW_HEIGHT: f32 = 200.0;

const PROVIDERS: [Provider; 3] = Provider::ALL;

fn index(p: Provider) -> usize {
    p.index()
}

/// `b` grown by `factor` of its size on every side.
fn expand(b: &GeoBounds, factor: f64) -> GeoBounds {
    let (dw, dh) = (b.width() * factor, b.height() * factor);
    GeoBounds::new(
        (b.min_lat - dh).max(-85.0),
        (b.max_lat + dh).min(85.0),
        (b.min_lon - dw).max(-180.0),
        (b.max_lon + dw).min(180.0),
    )
}

#[derive(Default)]
struct ProviderState {
    store: PhotoStore,
    layer: Option<LayerId>,
    /// Tiles being fetched.
    in_flight: HashSet<TileId>,
    /// Tiles that failed, and when; they are left alone for a while.
    failed: HashMap<TileId, Instant>,
    /// Bumped to discard the answers of fetches that are no longer wanted.
    epoch: u64,
    /// The view needs more tiles than are fetched; the status bar said so.
    zoomed_out: bool,
    /// A failure that would only repeat (no or refused token, spent budget)
    /// stops fetching until the user acts.
    blocked: bool,
}

/// The credential `provider` is asked with: Mapillary's access token, or the
/// optional Fotoshi API key.
fn credential(provider: Provider) -> Option<String> {
    let s = settings_store::snapshot();
    let c = match provider {
        Provider::Mapillary => s.mapillary_token,
        Provider::Fotoshi => s.fotoshi_api_key,
        Provider::Panoramax => String::new(),
    };
    let c = c.trim().to_string();
    (!c.is_empty()).then_some(c)
}

/// Zoom and pan of the full-size photo. The photo is drawn in a box the size
/// of the window, scaled by `zoom` about the window's top-left and shifted by
/// `(x, y)`; at zoom 1 it is fitted whole and cannot be moved.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PhotoView {
    zoom: f32,
    x: f32,
    y: f32,
    /// A drag in progress: where it started and the offset then.
    drag: Option<((f32, f32), (f32, f32))>,
}

impl Default for PhotoView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            x: 0.0,
            y: 0.0,
            drag: None,
        }
    }
}

const MAX_PHOTO_ZOOM: f32 = 12.0;

impl PhotoView {
    /// Keep the scaled box covering the window.
    fn clamp(&mut self, w: f32, h: f32) {
        self.zoom = self.zoom.clamp(1.0, MAX_PHOTO_ZOOM);
        self.x = self.x.clamp(w - w * self.zoom, 0.0);
        self.y = self.y.clamp(h - h * self.zoom, 0.0);
    }

    /// Multiply the zoom by `factor`, keeping the photo point under `(px, py)`
    /// (window pixels) where it is.
    fn zoom_at(&mut self, px: f32, py: f32, factor: f32, w: f32, h: f32) {
        let old = self.zoom;
        let new = (old * factor).clamp(1.0, MAX_PHOTO_ZOOM);
        let k = new / old;
        self.x = px - (px - self.x) * k;
        self.y = py - (py - self.y) * k;
        self.zoom = new;
        self.clamp(w, h);
    }

    fn pan_by(&mut self, dx: f32, dy: f32, w: f32, h: f32) {
        self.x += dx;
        self.y += dy;
        self.clamp(w, h);
    }
}

enum Picture {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
}

pub(crate) struct OpenPhoto {
    photo: StreetPhoto,
    picture: Picture,
    /// Set when the photo was opened from a feature's Fotoshi list: it then
    /// stays while that feature stays selected, instead of replacing the
    /// selection.
    for_feature: Option<FeatureRef>,
}

/// What a feature's Fotoshi lookup found.
enum ObjectState {
    Loading,
    Found(ObjectPhotos),
    Failed(SharedString),
}

struct ObjectLookup {
    feature: FeatureRef,
    state: ObjectState,
}

#[derive(Default)]
pub(crate) struct ImageryState {
    providers: [ProviderState; 3],
    open: Option<OpenPhoto>,
    /// Bumped to discard a photo download that is no longer wanted.
    photo_epoch: u64,
    /// The open photo is shown full size over the window.
    fullscreen: bool,
    /// Zoom and pan of the full-size view.
    view: PhotoView,
    /// A dot was clicked; handled on the next frame.
    pending_open: Option<StreetPhoto>,
    /// The Fotoshi lookup for the selected feature, when that is on.
    object: Option<ObjectLookup>,
    /// Bumped to discard the answer of a lookup that is no longer wanted.
    object_epoch: u64,
}

/// Decode downloaded bytes into what gpui draws (BGRA).
fn decode(bytes: &[u8]) -> Result<RenderImage, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let mut rgba = img.to_rgba8();
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let mut frames = smallvec::SmallVec::new();
    frames.push(image::Frame::new(rgba));
    Ok(RenderImage::new(frames))
}

fn failure_message(provider: Provider, e: &ImageryError) -> SharedString {
    let name = provider.name();
    match e {
        ImageryError::NoToken => tr!("street_imagery.state.no_token"),
        ImageryError::TooLarge => tr!("street_imagery.state.zoom_in", provider = name),
        ImageryError::Unauthorized => tr!("street_imagery.state.unauthorized"),
        ImageryError::RateLimited => tr!("street_imagery.state.rate_limited", provider = name),
        other => tr!(
            "street_imagery.state.fetch_failed",
            provider = name,
            detail = other
        ),
    }
}

impl MapViewer {
    pub(crate) fn shows_imagery(&self, provider: Provider) -> bool {
        self.imagery.providers[index(provider)].layer.is_some()
    }

    fn refresh_menus_for_imagery(&mut self, cx: &mut Context<Self>) {
        for provider in PROVIDERS {
            osm_gpui::street_imagery::set_menu_shows(provider, self.shows_imagery(provider));
        }
        let (lat, lon, state) = self.imagery_menu_context();
        crate::menu::rebuild_menus(
            &mut *cx,
            lat,
            lon,
            state,
            self.show_downloaded_area,
            self.has_uploadable_changes(),
        );
    }

    pub(crate) fn set_show_imagery(
        &mut self,
        provider: Provider,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if on == self.shows_imagery(provider) {
            return;
        }
        let i = index(provider);
        if on {
            let id = self.layer_manager.alloc_id();
            let layer = StreetImageryLayer::new(
                id,
                tr!("street_imagery.layer_name", provider = provider.name()).to_string(),
                provider,
                self.imagery.providers[i].store.clone(),
            );
            self.layer_manager.add_layer(Box::new(layer));
            self.imagery.providers[i].layer = Some(id);
            self.imagery.providers[i].blocked = false;
            self.refresh_menus_for_imagery(cx);
            self.fetch_tiles(provider, cx);
        } else {
            if let Some(id) = self.imagery.providers[i].layer {
                if let Some(pos) = self
                    .layer_manager
                    .layers()
                    .iter()
                    .position(|l| l.id() == id)
                {
                    self.layer_manager.remove_at(pos);
                }
            }
            self.imagery_off(provider);
            self.refresh_menus_for_imagery(cx);
        }
        cx.notify();
    }

    /// Forget everything about a provider's photos (its layer is already gone).
    fn imagery_off(&mut self, provider: Provider) {
        let st = &mut self.imagery.providers[index(provider)];
        st.layer = None;
        st.store.clear();
        st.in_flight.clear();
        st.failed.clear();
        st.epoch += 1;
        st.zoomed_out = false;
        st.blocked = false;
        if self
            .imagery
            .open
            .as_ref()
            .is_some_and(|o| o.photo.provider == provider)
        {
            self.close_photo_panel();
        }
    }

    pub(crate) fn on_toggle_mapillary(
        &mut self,
        _: &ToggleMapillary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Turning it on without a token would show an empty layer: explain
        // and offer Settings instead.
        if !self.shows_imagery(Provider::Mapillary)
            && settings_store::snapshot().mapillary_token.trim().is_empty()
        {
            self.show_sign_in_dialog(SignInPurpose::Mapillary, window, cx);
            return;
        }
        self.set_show_imagery(
            Provider::Mapillary,
            !self.shows_imagery(Provider::Mapillary),
            cx,
        );
    }

    pub(crate) fn on_toggle_fotoshi(
        &mut self,
        _: &ToggleFotoshi,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_show_imagery(
            Provider::Fotoshi,
            !self.shows_imagery(Provider::Fotoshi),
            cx,
        );
    }

    pub(crate) fn on_toggle_panoramax(
        &mut self,
        _: &TogglePanoramax,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_show_imagery(
            Provider::Panoramax,
            !self.shows_imagery(Provider::Panoramax),
            cx,
        );
    }

    pub(crate) fn on_refresh_street_imagery(
        &mut self,
        _: &RefreshStreetImagery,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for provider in PROVIDERS {
            // Refresh means: forget what was loaded and look again.
            let st = &mut self.imagery.providers[index(provider)];
            if st.layer.is_none() {
                continue;
            }
            st.blocked = false;
            st.failed.clear();
            st.in_flight.clear();
            st.epoch += 1;
            st.store.retain_tiles(|_| false);
            self.fetch_tiles(provider, cx);
        }
    }

    /// Start fetching the tiles `provider` still needs for the visible map
    /// (and a margin around it), a few at a time, in the background. Cheap
    /// when nothing is missing, so it runs every frame.
    fn fetch_tiles(&mut self, provider: Provider, cx: &mut Context<Self>) {
        let i = index(provider);
        if !self.shows_imagery(provider) {
            return;
        }
        let cred = credential(provider);
        if provider.needs_token() && cred.is_none() {
            self.imagery.providers[i].blocked = true;
            self.set_status(tr!("street_imagery.state.no_token"));
            cx.notify();
            return;
        }
        let view = expand(&self.viewport.visible_bounds(), 0.15);
        let needed = tiles::tiles_covering(&view, tiles::tile_zoom(provider));
        if needed.len() > tiles::MAX_TILES_PER_VIEW {
            // Too far out to show photos: say so once.
            if !std::mem::replace(&mut self.imagery.providers[i].zoomed_out, true) {
                self.set_status(tr!(
                    "street_imagery.state.zoom_in",
                    provider = provider.name()
                ));
                cx.notify();
            }
            return;
        }
        let st = &mut self.imagery.providers[i];
        st.zoomed_out = false;
        if st.store.tile_count() > MAX_CACHED_TILES {
            let keep: HashSet<TileId> = needed.iter().copied().collect();
            st.store.retain_tiles(|t| keep.contains(t));
        }
        let now = Instant::now();
        let mut started = Vec::new();
        for t in needed {
            if st.in_flight.len() >= MAX_CONCURRENT_TILES {
                break;
            }
            let recently_failed = st
                .failed
                .get(&t)
                .is_some_and(|at| now.duration_since(*at) < RETRY_AFTER);
            if st.store.has_tile(t) || st.in_flight.contains(&t) || recently_failed {
                continue;
            }
            st.in_flight.insert(t);
            started.push(t);
        }
        if started.is_empty() {
            return;
        }
        let epoch = st.epoch;
        self.set_status(tr!(
            "street_imagery.state.loading",
            provider = provider.name()
        ));
        cx.notify();
        for t in started {
            let cred = cred.clone();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        tiles::fetch_tile_with(&UreqClient::new(), provider, t, cred.as_deref())
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.finish_tile(provider, t, epoch, result, cx)
                });
            })
            .detach();
        }
    }

    fn finish_tile(
        &mut self,
        provider: Provider,
        t: TileId,
        epoch: u64,
        result: Result<(Vec<StreetPhoto>, bool), ImageryError>,
        cx: &mut Context<Self>,
    ) {
        let st = &mut self.imagery.providers[index(provider)];
        if epoch != st.epoch || st.layer.is_none() {
            return;
        }
        st.in_flight.remove(&t);
        match result {
            Ok((photos, _may_have_more)) => {
                st.failed.remove(&t);
                st.store.insert_tile(t, photos);
                if st.in_flight.is_empty() {
                    let count = st.store.len();
                    self.set_status(osm_gpui::tr_count!(
                        "street_imagery.state.loaded",
                        count,
                        provider = provider.name()
                    ));
                }
            }
            Err(e) => {
                st.failed.insert(t, Instant::now());
                // A refused token or a spent budget would only repeat.
                st.blocked = matches!(
                    e,
                    ImageryError::NoToken | ImageryError::Unauthorized | ImageryError::RateLimited
                );
                self.set_status(failure_message(provider, &e));
            }
        }
        cx.notify();
    }

    /// Per-frame upkeep, called from `render`: follows a layer being deleted
    /// from the Layers list, fetches the tiles the map has moved onto, and
    /// opens the photo the last click asked for.
    pub(crate) fn imagery_frame(&mut self, cx: &mut Context<Self>) {
        for provider in PROVIDERS {
            let i = index(provider);
            let Some(id) = self.imagery.providers[i].layer else {
                continue;
            };
            if self.layer_manager.find_layer(id).is_none() {
                self.imagery_off(provider);
                self.refresh_menus_for_imagery(cx);
                continue;
            }
            if !self.imagery.providers[i].blocked && !self.viewport.is_dragging {
                self.fetch_tiles(provider, cx);
            }
        }
        self.update_object_lookup(cx);
        if let Some(photo) = self.imagery.pending_open.take() {
            self.open_photo(photo, None, cx);
        }
    }

    /// The one OSM feature selected, as a Fotoshi `osm_id`, when looking up
    /// its photos is turned on, has a key, and the feature is on the server
    /// (new features have negative ids).
    fn fotoshi_lookup_target(&self) -> Option<(FeatureRef, String)> {
        let settings = settings_store::snapshot();
        if !settings.fotoshi_lookup_selected || settings.fotoshi_api_key.trim().is_empty() {
            return None;
        }
        let [f] = self.selected.as_slice() else {
            return None;
        };
        let kind = match f.kind {
            FeatureKind::Node => 'n',
            FeatureKind::Way => 'w',
            FeatureKind::Relation => 'r',
            #[allow(unreachable_patterns)]
            _ => return None,
        };
        (f.id > 0).then(|| (*f, imagery::fotoshi_osm_id(kind, f.id)))
    }

    /// Keep the lookup in step with the selection: start one when a
    /// feature is selected, forget it when the selection moves on.
    fn update_object_lookup(&mut self, cx: &mut Context<Self>) {
        let want = self.fotoshi_lookup_target();
        let have = self.imagery.object.as_ref().map(|o| o.feature);
        if want.as_ref().map(|(f, _)| *f) == have {
            return;
        }
        self.imagery.object_epoch += 1;
        self.imagery.object = None;
        cx.notify();
        let Some((feature, osm_id)) = want else {
            return;
        };
        self.imagery.object = Some(ObjectLookup {
            feature,
            state: ObjectState::Loading,
        });
        let epoch = self.imagery.object_epoch;
        let key = settings_store::snapshot().fotoshi_api_key;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { imagery::fetch_object_photos(&osm_id, &key) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if epoch != this.imagery.object_epoch {
                    return;
                }
                if let Some(o) = &mut this.imagery.object {
                    o.state = match result {
                        Ok(found) => ObjectState::Found(found),
                        Err(e) => ObjectState::Failed(failure_message(Provider::Fotoshi, &e)),
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The selected feature's Fotoshi photos, as a section for the side
    /// panel: a row per photo that opens it in the viewer. Nothing while
    /// there is no lookup or Fotoshi has no photos of the feature.
    pub(crate) fn render_object_photos_section(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let lookup = self.imagery.object.as_ref()?;
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let feature = lookup.feature;
        let body = match &lookup.state {
            ObjectState::Loading => Label::new(tr!("street_imagery.object.checking"))
                .text_size(muted_text_size())
                .text_color(muted)
                .into_any_element(),
            ObjectState::Failed(message) => Label::new(message.clone())
                .text_size(muted_text_size())
                .text_color(theme.danger)
                .into_any_element(),
            ObjectState::Found(found) if found.photos.is_empty() => return None,
            ObjectState::Found(found) => {
                let rows = found.photos.iter().enumerate().map(|(n, photo)| {
                    let label = match (&photo.captured, &photo.creator) {
                        (Some(d), Some(c)) => format!("{d} · {c}"),
                        (Some(d), None) => d.clone(),
                        (None, Some(c)) => c.clone(),
                        (None, None) => {
                            tr!("street_imagery.object.photo", number = n + 1).to_string()
                        }
                    };
                    let photo = photo.clone();
                    Button::new(("object-photo", n))
                        .ghost()
                        .compact()
                        .label(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_photo(photo.clone(), Some(feature), cx);
                        }))
                });
                v_flex()
                    .gap_0p5()
                    .child(
                        Label::new(osm_gpui::tr_count!(
                            "street_imagery.object.title",
                            found.count
                        ))
                        .font_weight(gpui::FontWeight::SEMIBOLD),
                    )
                    .children(rows)
                    .into_any_element()
            }
        };
        Some(
            div()
                .px_2()
                .py_1p5()
                .border_b_1()
                .border_color(border)
                .child(body)
                .into_any_element(),
        )
    }

    /// A click on the map at `pos` (map-local pixels): opens the photo whose
    /// dot was clicked. Returns whether the click was used.
    pub(crate) fn imagery_click(&mut self, pos: gpui::Point<gpui::Pixels>) -> bool {
        if self.mode != EditMode::Select {
            self.close_photo_panel();
            return false;
        }
        let mut stores = Vec::new();
        for st in &self.imagery.providers {
            let visible = st
                .layer
                .and_then(|id| self.layer_manager.find_layer(id))
                .is_some_and(|l| l.is_visible());
            if visible {
                stores.push(&st.store);
            }
        }
        match street_imagery_layer::hit_test(&stores, &self.viewport, pos) {
            Some(photo) => {
                self.imagery.pending_open = Some(photo);
                true
            }
            // Clicking away from the photos deselects the open one; the click
            // still does whatever it would have done.
            None => {
                self.close_photo_panel();
                false
            }
        }
    }

    fn open_photo(
        &mut self,
        photo: StreetPhoto,
        for_feature: Option<FeatureRef>,
        cx: &mut Context<Self>,
    ) {
        if for_feature.is_none() {
            self.set_selection(Vec::new());
        }
        self.close_note_panel();
        for st in &self.imagery.providers {
            st.store.set_selected(None);
        }
        self.imagery.providers[index(photo.provider)]
            .store
            .set_selected(Some((photo.provider, photo.id.clone())));
        self.imagery.photo_epoch += 1;
        let epoch = self.imagery.photo_epoch;
        self.imagery.open = Some(OpenPhoto {
            photo: photo.clone(),
            picture: Picture::Loading,
            for_feature,
        });
        cx.notify();
        let cred = credential(photo.provider);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let client = UreqClient::new();
                    // A tile leaves out the image URLs and the author.
                    let photo = if photo.large_url.is_none() {
                        match tiles::fetch_detail_with(&client, &photo, cred.as_deref()) {
                            Ok(detailed) => detailed,
                            Err(e) => return (photo, Err(e)),
                        }
                    } else {
                        photo
                    };
                    let image = (|| {
                        let url = photo
                            .large_url
                            .clone()
                            .or_else(|| photo.thumb_url.clone())
                            .ok_or_else(|| ImageryError::Parse("no image".into()))?;
                        let bytes = imagery::fetch_image(&url)?;
                        decode(&bytes).map_err(ImageryError::Parse)
                    })();
                    (photo, image)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if epoch != this.imagery.photo_epoch {
                    return;
                }
                let (photo, image) = result;
                this.imagery.providers[index(photo.provider)]
                    .store
                    .update_photo(&photo);
                let failure = image.as_ref().err().cloned();
                if let Some(open) = &mut this.imagery.open {
                    open.photo = photo.clone();
                    open.picture = match image {
                        Ok(image) => Picture::Ready(Arc::new(image)),
                        Err(_) => Picture::Failed,
                    };
                }
                if let Some(e) = failure {
                    this.set_status(failure_message(photo.provider, &e));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Escape closes the full-size view. Returns whether one was open.
    pub(crate) fn close_photo_fullscreen(&mut self) -> bool {
        std::mem::take(&mut self.imagery.fullscreen)
    }

    /// The open photo as large as the window allows, with zoom (wheel, Cmd or
    /// Ctrl + trackpad scroll, pinch, double-click to reset) and pan (drag, or
    /// trackpad scroll). The close button or Escape closes it.
    pub(crate) fn render_photo_fullscreen(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.imagery.fullscreen {
            return None;
        }
        let Some(OpenPhoto {
            picture: Picture::Ready(image),
            ..
        }) = &self.imagery.open
        else {
            self.imagery.fullscreen = false;
            return None;
        };
        let size = window.viewport_size();
        let (w, h) = (size.width.as_f32(), size.height.as_f32());
        let view = self.imagery.view;
        let dragging = view.drag.is_some();

        let photo = img(image.clone())
            .absolute()
            .left(px(view.x))
            .top(px(view.y))
            .w(px(w * view.zoom))
            .h(px(h * view.zoom))
            .object_fit(gpui::ObjectFit::Contain);

        Some(
            div()
                .id("photo-fullscreen")
                .absolute()
                .inset_0()
                .occlude()
                .overflow_hidden()
                .bg(gpui::rgba(0x000000f0))
                .when(dragging, |d| d.cursor_grabbing())
                .when(!dragging, |d| d.cursor_grab())
                .on_scroll_wheel(
                    cx.listener(|this, ev: &gpui::ScrollWheelEvent, window, cx| {
                        let size = window.viewport_size();
                        let (w, h) = (size.width.as_f32(), size.height.as_f32());
                        let (px_, py_) = (ev.position.x.as_f32(), ev.position.y.as_f32());
                        let invert = settings_store::snapshot().invert_scroll_zoom;
                        let sign = if invert { -1.0 } else { 1.0 };
                        let zoom_modifier = ev.modifiers.platform || ev.modifiers.control;
                        let view = &mut this.imagery.view;
                        match ev.delta {
                            gpui::ScrollDelta::Lines(d) => {
                                view.zoom_at(px_, py_, (sign * d.y * 0.15).exp(), w, h)
                            }
                            gpui::ScrollDelta::Pixels(d) if zoom_modifier => {
                                view.zoom_at(px_, py_, (sign * d.y.as_f32() * 0.01).exp(), w, h)
                            }
                            gpui::ScrollDelta::Pixels(d) => {
                                view.pan_by(d.x.as_f32(), d.y.as_f32(), w, h)
                            }
                        }
                        cx.notify();
                    }),
                )
                .on_pinch(cx.listener(|this, ev: &gpui::PinchEvent, window, cx| {
                    let size = window.viewport_size();
                    this.imagery.view.zoom_at(
                        ev.position.x.as_f32(),
                        ev.position.y.as_f32(),
                        ev.delta.exp(),
                        size.width.as_f32(),
                        size.height.as_f32(),
                    );
                    cx.notify();
                }))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                        let view = &mut this.imagery.view;
                        if ev.click_count >= 2 {
                            *view = PhotoView::default();
                        } else {
                            view.drag = Some((
                                (ev.position.x.as_f32(), ev.position.y.as_f32()),
                                (view.x, view.y),
                            ));
                        }
                        cx.notify();
                    }),
                )
                .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, window, cx| {
                    let Some(((sx, sy), (ox, oy))) = this.imagery.view.drag else {
                        return;
                    };
                    let size = window.viewport_size();
                    let view = &mut this.imagery.view;
                    view.x = ox + (ev.position.x.as_f32() - sx);
                    view.y = oy + (ev.position.y.as_f32() - sy);
                    view.clamp(size.width.as_f32(), size.height.as_f32());
                    cx.notify();
                }))
                .on_mouse_up(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.imagery.view.drag = None;
                        cx.notify();
                    }),
                )
                .on_mouse_up_out(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.imagery.view.drag = None;
                        cx.notify();
                    }),
                )
                .child(photo)
                .child(
                    div().absolute().top_3().right_3().child(
                        Button::new("photo-fullscreen-close")
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip(tr!("street_imagery.panel.close"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.imagery.fullscreen = false;
                                cx.notify();
                            })),
                    ),
                )
                .into_any_element(),
        )
    }

    pub(crate) fn close_photo_panel(&mut self) {
        self.imagery.fullscreen = false;
        self.imagery.open = None;
        self.imagery.photo_epoch += 1;
        for st in &self.imagery.providers {
            st.store.set_selected(None);
        }
    }

    /// The photo section while a photo is open, shaped like the note
    /// section: its title, a pinned status row and the scrolled rest.
    pub(crate) fn render_photo_section(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(SharedString, AnyElement, AnyElement)> {
        let open = self.imagery.open.as_ref()?;
        let photo = &open.photo;
        // A dot's photo replaces the selection; a feature's own photo stays
        // while that feature is the selection.
        let stays = match open.for_feature {
            Some(f) => self.selected.as_slice() == [f],
            None => self.selected.is_empty(),
        };
        if !stays {
            self.close_photo_panel();
            return None;
        }
        let theme = cx.theme();
        let (muted, foreground, border) = (theme.muted_foreground, theme.foreground, theme.border);

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                div().flex_1().child(
                    Label::new(photo.provider.name())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(foreground),
                ),
            )
            .child(
                Button::new("photo-close-panel")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip(tr!("street_imagery.panel.close"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_photo_panel();
                        cx.notify();
                    })),
            )
            .into_any_element();

        let picture = match &open.picture {
            // Capped in height (photos are often portrait) and letterboxed;
            // a double-click opens it full size.
            Picture::Ready(image) => div()
                .id("photo-preview")
                .w_full()
                .h(px(PREVIEW_HEIGHT))
                .flex_shrink_0()
                .overflow_hidden()
                .cursor_pointer()
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    if ev.click_count() >= 2 {
                        this.imagery.view = PhotoView::default();
                        this.imagery.fullscreen = true;
                        cx.notify();
                    }
                }))
                .child(
                    // An explicit height: a percentage one is resolved
                    // against the photo's own size, which overflows.
                    img(image.clone())
                        .w_full()
                        .h(px(PREVIEW_HEIGHT))
                        .rounded_sm()
                        .object_fit(gpui::ObjectFit::Contain),
                )
                .into_any_element(),
            Picture::Loading => Label::new(tr!("street_imagery.panel.loading"))
                .text_size(muted_text_size())
                .text_color(muted)
                .into_any_element(),
            Picture::Failed => Label::new(tr!("street_imagery.panel.load_failed"))
                .text_size(muted_text_size())
                .text_color(cx.theme().danger)
                .into_any_element(),
        };

        let mut facts: Vec<String> = Vec::new();
        if let Some(date) = &photo.captured {
            facts.push(tr!("street_imagery.panel.captured", date = date).to_string());
        }
        if let Some(creator) = &photo.creator {
            facts.push(tr!("street_imagery.panel.by", creator = creator).to_string());
        }
        if let Some(license) = &photo.license {
            facts.push(tr!("street_imagery.panel.license", license = license).to_string());
        }
        if let Some(h) = photo.heading {
            facts.push(tr!("street_imagery.panel.heading", degrees = h.round() as i32).to_string());
        }
        let url = photo.web_url();
        let provider_name = photo.provider.name();

        let body = v_flex()
            .gap_2()
            .p_2()
            .border_t_1()
            .border_color(border)
            .child(picture)
            .children(
                facts
                    .into_iter()
                    .map(|f| Label::new(f).text_size(muted_text_size()).text_color(muted)),
            )
            .child(
                Button::new("photo-open-web")
                    .label(tr!(
                        "street_imagery.panel.open_in_browser",
                        provider = provider_name
                    ))
                    .compact()
                    .on_click(move |_, _, _| {
                        let _ = open::that(&url);
                    }),
            )
            .into_any_element();

        Some((tr!("street_imagery.panel.title"), header, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_point_under_the_cursor_and_stays_covering_the_window() {
        let (w, h) = (800.0, 600.0);
        let mut v = PhotoView::default();
        // Zooming out at fit does nothing; no panning at fit either.
        v.zoom_at(400.0, 300.0, 0.5, w, h);
        assert_eq!(v, PhotoView::default());
        v.pan_by(50.0, 50.0, w, h);
        assert_eq!((v.x, v.y), (0.0, 0.0));
        // Zoom 2x about (600, 150): that point of the photo stays there.
        v.zoom_at(600.0, 150.0, 2.0, w, h);
        assert_eq!(v.zoom, 2.0);
        let photo_x = (600.0 - v.x) / v.zoom;
        assert!((photo_x - 600.0).abs() < 0.01);
        // Panning is limited to the photo's edges.
        v.pan_by(10_000.0, 10_000.0, w, h);
        assert_eq!((v.x, v.y), (0.0, 0.0));
        v.pan_by(-10_000.0, -10_000.0, w, h);
        assert_eq!((v.x, v.y), (-800.0, -600.0));
        // The zoom is capped.
        v.zoom_at(0.0, 0.0, 1000.0, w, h);
        assert_eq!(v.zoom, MAX_PHOTO_ZOOM);
    }

    #[test]
    fn decode_swaps_to_bgra() {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let out = decode(&png).unwrap();
        assert_eq!(out.as_bytes(0).unwrap()[..3], [30, 20, 10]);
        assert!(decode(b"not an image").is_err());
    }
}
