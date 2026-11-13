//! Street-level imagery in the window: View > Show Mapillary/Fotoshi Photos,
//! fetching for the visible map, the dots, and the photo viewer in the side
//! panel.
//!
//! The model, the provider calls and the layer live in the library
//! (`osm_gpui::street_imagery`, `osm_gpui::layers::street_imagery_layer`);
//! this is the part that needs the window. Photos are fetched in the
//! background for the visible map once it is zoomed in far enough for the
//! provider to accept the area, never block editing, and report failures in
//! the status bar. Clicking a dot downloads that photo's larger rendition and
//! shows it in the side panel with its attribution.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{div, img, prelude::*, px, AnyElement, Context, RenderImage, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    label::Label,
    v_flex, ActiveTheme as _, IconName, Sizable as _,
};
use osm_gpui::coordinates::GeoBounds;
use osm_gpui::layers::street_imagery_layer::{self, StreetImageryLayer};
use osm_gpui::layers::LayerId;
use osm_gpui::settings_store;
use osm_gpui::street_imagery::{self as imagery, ImageryError, PhotoStore, Provider, StreetPhoto};
use osm_gpui::tr;
use osm_gpui::ui::sign_in_dialog::SignInPurpose;
use osm_gpui::ui::style::muted_text_size;

use crate::{EditMode, MapViewer};

gpui::actions!(
    osm_gpui,
    [ToggleMapillary, ToggleFotoshi, RefreshStreetImagery]
);

/// How long a pan waits before the photos for the new view are fetched.
const REFETCH_DELAY: Duration = Duration::from_millis(1200);

/// Height of the photo in the side panel.
const PREVIEW_HEIGHT: f32 = 200.0;

const PROVIDERS: [Provider; 2] = [Provider::Mapillary, Provider::Fotoshi];

fn index(p: Provider) -> usize {
    match p {
        Provider::Mapillary => 0,
        Provider::Fotoshi => 1,
    }
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

fn contains(outer: &GeoBounds, inner: &GeoBounds) -> bool {
    inner.min_lat >= outer.min_lat
        && inner.max_lat <= outer.max_lat
        && inner.min_lon >= outer.min_lon
        && inner.max_lon <= outer.max_lon
}

/// A view that is a little smaller than the provider allows still gets a
/// margin around the screen, so a small pan needs no new fetch.
fn fetch_area(provider: Provider, view: &GeoBounds) -> Option<GeoBounds> {
    let wide = expand(view, 0.25);
    if imagery::area_allowed(provider, &wide) {
        Some(wide)
    } else if imagery::area_allowed(provider, view) {
        Some(*view)
    } else {
        None
    }
}

#[derive(Default)]
struct ProviderState {
    store: PhotoStore,
    layer: Option<LayerId>,
    fetching: bool,
    /// Bumped to discard the answer of a fetch that is no longer wanted.
    epoch: u64,
    /// The area of the last fetch and when it started.
    covered: Option<(GeoBounds, Instant)>,
    /// The last failure that stops refetching until the user acts.
    blocked: bool,
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
}

#[derive(Default)]
pub(crate) struct ImageryState {
    providers: [ProviderState; 2],
    open: Option<OpenPhoto>,
    /// Bumped to discard a photo download that is no longer wanted.
    photo_epoch: u64,
    /// The open photo is shown full size over the window.
    fullscreen: bool,
    /// Zoom and pan of the full-size view.
    view: PhotoView,
    /// A dot was clicked; handled on the next frame.
    pending_open: Option<StreetPhoto>,
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
        osm_gpui::street_imagery::set_menu_shows(
            self.shows_imagery(Provider::Mapillary),
            self.shows_imagery(Provider::Fotoshi),
        );
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
            self.fetch_imagery(provider, cx);
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
        st.fetching = false;
        st.epoch += 1;
        st.covered = None;
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

    pub(crate) fn on_refresh_street_imagery(
        &mut self,
        _: &RefreshStreetImagery,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for provider in PROVIDERS {
            self.imagery.providers[index(provider)].blocked = false;
            self.fetch_imagery(provider, cx);
        }
    }

    /// Fetch `provider`'s photos for the visible map, in the background.
    fn fetch_imagery(&mut self, provider: Provider, cx: &mut Context<Self>) {
        let i = index(provider);
        if !self.shows_imagery(provider) || self.imagery.providers[i].fetching {
            return;
        }
        let token = settings_store::snapshot().mapillary_token;
        if provider.needs_token() && token.trim().is_empty() {
            self.imagery.providers[i].blocked = true;
            self.set_status(tr!("street_imagery.state.no_token"));
            cx.notify();
            return;
        }
        let view = self.viewport.visible_bounds();
        let Some(area) = fetch_area(provider, &view) else {
            // Too far out: nothing to draw until the user zooms in.
            self.imagery.providers[i].store.clear();
            self.imagery.providers[i].covered = None;
            self.set_status(tr!(
                "street_imagery.state.zoom_in",
                provider = provider.name()
            ));
            cx.notify();
            return;
        };
        let st = &mut self.imagery.providers[i];
        st.fetching = true;
        st.epoch += 1;
        let epoch = st.epoch;
        st.covered = Some((area, Instant::now()));
        self.set_status(tr!(
            "street_imagery.state.loading",
            provider = provider.name()
        ));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { imagery::fetch(provider, &area, Some(&token)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_imagery_fetch(provider, epoch, result, cx)
            });
        })
        .detach();
    }

    fn finish_imagery_fetch(
        &mut self,
        provider: Provider,
        epoch: u64,
        result: Result<(Vec<StreetPhoto>, bool), ImageryError>,
        cx: &mut Context<Self>,
    ) {
        let i = index(provider);
        if epoch != self.imagery.providers[i].epoch || !self.shows_imagery(provider) {
            return;
        }
        self.imagery.providers[i].fetching = false;
        match result {
            Ok((photos, truncated)) => {
                let count = photos.len();
                self.imagery.providers[i].store.replace(photos);
                if truncated {
                    self.set_status(tr!(
                        "street_imagery.state.truncated",
                        provider = provider.name(),
                        count = count
                    ));
                } else {
                    self.set_status(osm_gpui::tr_count!(
                        "street_imagery.state.loaded",
                        count,
                        provider = provider.name()
                    ));
                }
            }
            Err(e) => {
                // A refused token or a spent budget would only repeat.
                self.imagery.providers[i].blocked = matches!(
                    e,
                    ImageryError::NoToken | ImageryError::Unauthorized | ImageryError::RateLimited
                );
                self.set_status(failure_message(provider, &e));
            }
        }
        cx.notify();
    }

    /// Per-frame upkeep, called from `render`: follows a layer being deleted
    /// from the Layers list, follows the map once it leaves the fetched area,
    /// and opens the photo the last click asked for.
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
            let st = &self.imagery.providers[i];
            if st.fetching || st.blocked || self.viewport.is_dragging {
                continue;
            }
            let stale = match &st.covered {
                Some((area, at)) => {
                    at.elapsed() >= REFETCH_DELAY
                        && !contains(area, &self.viewport.visible_bounds())
                }
                // Zoomed out earlier: try again once the view fits.
                None => fetch_area(provider, &self.viewport.visible_bounds()).is_some(),
            };
            if stale {
                self.fetch_imagery(provider, cx);
            }
        }
        if let Some(photo) = self.imagery.pending_open.take() {
            self.open_photo(photo, cx);
        }
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

    fn open_photo(&mut self, photo: StreetPhoto, cx: &mut Context<Self>) {
        self.set_selection(Vec::new());
        self.close_note_panel();
        for st in &self.imagery.providers {
            st.store.set_selected(None);
        }
        self.imagery.providers[index(photo.provider)]
            .store
            .set_selected(Some((photo.provider, photo.id.clone())));
        self.imagery.photo_epoch += 1;
        let epoch = self.imagery.photo_epoch;
        let url = photo.large_url.clone().or_else(|| photo.thumb_url.clone());
        let picture = if url.is_some() {
            Picture::Loading
        } else {
            Picture::Failed
        };
        self.imagery.open = Some(OpenPhoto { photo, picture });
        cx.notify();
        let Some(url) = url else { return };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let bytes = imagery::fetch_image(&url).map_err(|e| e.to_string())?;
                    decode(&bytes)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if epoch != this.imagery.photo_epoch {
                    return;
                }
                if let Some(open) = &mut this.imagery.open {
                    open.picture = match result {
                        Ok(image) => Picture::Ready(Arc::new(image)),
                        Err(_) => Picture::Failed,
                    };
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
        if !self.selected.is_empty() {
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
    fn the_fetch_area_gets_a_margin_only_when_the_provider_allows_it() {
        // 0.08 x 0.08 deg: fine for Mapillary alone (0.0064 < 0.01) but not
        // with a 25% margin on each side (0.12 x 0.12 = 0.0144).
        let view = GeoBounds::new(45.0, 45.08, -111.08, -111.0);
        let area = fetch_area(Provider::Mapillary, &view).unwrap();
        assert_eq!(area, view);
        // A small view gets the margin.
        let small = GeoBounds::new(45.0, 45.02, -111.02, -111.0);
        let area = fetch_area(Provider::Mapillary, &small).unwrap();
        assert!(contains(&area, &small) && area != small);
        // Too wide for the provider at all.
        let wide = GeoBounds::new(45.0, 45.2, -111.2, -111.0);
        assert!(fetch_area(Provider::Mapillary, &wide).is_none());
        assert!(fetch_area(Provider::Fotoshi, &wide).is_some());
    }

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
