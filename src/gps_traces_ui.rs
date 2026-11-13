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

use gpui::{actions, div, prelude::*, px, Action, AnyElement, Context, Window};
use gpui_component::{h_flex, menu::PopupMenu, v_flex, ActiveTheme};
use osm_gpui::auth;
use osm_gpui::gps_traces::{self, GpsTracesError, PageWalker, TracePlan};
use osm_gpui::layers::reference_layer::ReferenceLayer;
use osm_gpui::layers::{LayerId, MapLayer as _};
use osm_gpui::reference::{Format, ReferenceData};
use osm_gpui::settings_store;
use osm_gpui::trace_style::{
    ColorBy, Legend, LineColor, LineWidth, MaxAge, SpeedRange, StyleChange, TraceStyle,
};
use osm_gpui::ui::style::muted_text_size;
use osm_gpui::{tr, tr_count};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::MapViewer;

actions!(osm_gpui, [DownloadGpsTraces]);

/// Change how the GPS Traces layer at `index` draws (its Layers-list
/// context menu); with no `index`, the GPS Traces layer of the last download
/// (for the script harness: `action SetGpsTraceStyle {"change":{"ColorBy":"Speed"}}`).
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = layers)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetGpsTraceStyle {
    #[serde(default)]
    pub index: Option<usize>,
    pub change: StyleChange,
}

/// The trace-style entries of a GPS Traces layer's context menu: submenus for
/// the colouring, colour, width and age filter, a private-points toggle and
/// a reset. `style` is the layer's current style, shown as check marks.
pub(crate) fn trace_style_menu(
    menu: PopupMenu,
    index: usize,
    style: TraceStyle,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let set = move |change| {
        Box::new(SetGpsTraceStyle {
            index: Some(index),
            change,
        }) as Box<dyn Action>
    };
    menu.submenu(
        tr!("gps_traces.style.color_by.title"),
        window,
        cx,
        move |mut m, _, _| {
            for c in ColorBy::ALL {
                m = m.menu_with_check(c.label(), style.color_by == c, set(StyleChange::ColorBy(c)));
            }
            m
        },
    )
    .submenu(
        tr!("gps_traces.style.speed_range.title"),
        window,
        cx,
        move |mut m, _, _| {
            // Checked only while colouring by speed; picking one switches
            // to it.
            for r in SpeedRange::ALL {
                m = m.menu_with_check(
                    r.label(),
                    style.color_by == ColorBy::Speed && style.speed_range == r,
                    set(StyleChange::SpeedRange(r)),
                );
            }
            m
        },
    )
    .submenu(
        tr!("gps_traces.style.color.title"),
        window,
        cx,
        move |mut m, _, _| {
            // The colour only shows in single-colour mode (and on the dots).
            for c in LineColor::ALL {
                m = m.menu_with_check(c.label(), style.color == c, set(StyleChange::Color(c)));
            }
            m
        },
    )
    .submenu(
        tr!("gps_traces.style.width.title"),
        window,
        cx,
        move |mut m, _, _| {
            for w in LineWidth::ALL {
                m = m.menu_with_check(w.label(), style.width == w, set(StyleChange::Width(w)));
            }
            m
        },
    )
    .submenu(
        tr!("gps_traces.style.age.title"),
        window,
        cx,
        move |mut m, _, _| {
            for a in MaxAge::ALL {
                m = m.menu_with_check(a.label(), style.max_age == a, set(StyleChange::MaxAge(a)));
            }
            m
        },
    )
    .menu_with_check(
        tr!("gps_traces.style.show_private"),
        style.show_private,
        set(StyleChange::ShowPrivate(!style.show_private)),
    )
    .menu(tr!("gps_traces.style.reset"), set(StyleChange::Reset))
}

/// The trace style of `layer` if it is a GPS Traces layer.
pub(crate) fn trace_style_of(layer: &dyn osm_gpui::layers::MapLayer) -> Option<TraceStyle> {
    let r = layer.as_any().downcast_ref::<ReferenceLayer>()?;
    (r.format() == Format::OsmTraces).then(|| *r.trace_style())
}

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
        let http = self.http.clone();
        cx.spawn(async move |this, cx| {
            let mut walker = PageWalker::new(cells);
            let mut pages: Vec<Arc<[u8]>> = Vec::new();
            while let Some((cell, page)) = walker.next_request() {
                let base = base_url.clone();
                let http = http.clone();
                let body = cx
                    .background_executor()
                    .spawn(async move {
                        let token = auth::fresh_token_if_loaded(
                            http.as_ref(),
                            &auth::oauth_base_for(&base),
                        )
                        .map(|t| t.access_token);
                        gps_traces::fetch_page_with(
                            http.as_ref(),
                            &base,
                            &cell,
                            page,
                            token.as_deref(),
                        )
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
        parsed: Result<Option<ReferenceData>, GpsTracesError>,
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
                if let Some(style) = trace_style_of(old) {
                    layer.set_trace_style(style);
                }
                self.layer_manager.replace(id, Box::new(layer));
                return;
            }
        }
        let name = self
            .layer_manager
            .unique_name(&tr!("gps_traces.layer_name"));
        let id = self.layer_manager.alloc_id();
        let mut layer = ReferenceLayer::new(id, name, data);
        layer.set_trace_style(settings_store::snapshot().gps_trace_style);
        self.layer_manager.add_reference_layer(Box::new(layer));
        self.gps_traces.layer = Some(id);
    }

    pub(crate) fn on_set_gps_trace_style(
        &mut self,
        action: &SetGpsTraceStyle,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = match action.index {
            Some(i) => self.layer_manager.layers().get(i).map(|l| l.id()),
            None => self.gps_traces.layer,
        };
        let Some(layer) = id.and_then(|id| self.layer_manager.find_layer_mut(id)) else {
            return;
        };
        let Some(layer) = layer
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<ReferenceLayer>())
            .filter(|l| l.format() == Format::OsmTraces)
        else {
            return;
        };
        let mut style = *layer.trace_style();
        style.apply(action.change);
        layer.set_trace_style(style);
        // The next download starts out the same way, this run and the next.
        settings_store::set_gps_trace_style(style, true);
        cx.notify();
    }

    /// What the colours mean, bottom left over the map, while a visible GPS
    /// Traces layer is coloured by something.
    pub(crate) fn render_gps_traces_legend(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let style = self
            .layer_manager
            .layers()
            .iter()
            .rev()
            .filter(|l| l.is_visible())
            .find_map(|l| trace_style_of(l.as_ref()))?;
        let muted = cx.theme().muted_foreground;
        let row = |rgb: u32, label: String| {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(div().w(px(14.0)).h(px(4.0)).rounded_sm().bg(gpui::rgb(rgb)))
                .child(div().child(label))
                .into_any_element()
        };
        let rows: Vec<AnyElement> = match style.legend()? {
            Legend::Classes(entries) => entries.into_iter().map(|(c, l)| row(c, l)).collect(),
            Legend::Gradient {
                colors,
                ticks,
                unknown,
            } => {
                const WIDTH: f32 = 180.0;
                const TICK_W: f32 = 40.0;
                let cell = WIDTH / colors.len().max(1) as f32;
                let strip = h_flex()
                    .w(px(WIDTH))
                    .h(px(8.0))
                    .rounded_sm()
                    .overflow_hidden()
                    .children(
                        colors
                            .iter()
                            .map(|&c| div().w(px(cell)).h_full().bg(gpui::rgb(c))),
                    );
                // The end labels line up with the strip's ends, the others
                // are centred under their place on it.
                let n = ticks.len();
                let last = n.saturating_sub(1).max(1) as f32;
                let labels = div().relative().w(px(WIDTH)).h(px(14.0)).children(
                    ticks.into_iter().enumerate().map(|(i, t)| {
                        let tick = div().absolute().w(px(TICK_W)).flex().child(t);
                        if i == 0 {
                            tick.left_0().justify_start()
                        } else if i + 1 == n {
                            tick.right_0().justify_end()
                        } else {
                            let x = WIDTH * i as f32 / last;
                            tick.left(px(x - TICK_W / 2.0)).justify_center()
                        }
                    }),
                );
                vec![
                    v_flex()
                        .pt_0p5()
                        .child(strip)
                        .child(labels)
                        .into_any_element(),
                    row(unknown.0, unknown.1),
                ]
            }
        };
        Some(
            v_flex()
                .id("gps-traces-legend")
                .absolute()
                .left(px(12.0))
                .bottom(px(28.0))
                .p_2()
                .gap_0p5()
                .bg(cx.theme().popover.opacity(0.92))
                .border_1()
                .border_color(cx.theme().border)
                .rounded_md()
                .text_size(muted_text_size())
                .text_color(cx.theme().popover_foreground)
                .child(
                    div()
                        .text_color(muted)
                        .child(tr!("gps_traces.legend.title", by = style.color_by.label())),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    /// The download is over: every page fetched (`Ok`) or one failed.
    fn end_gps_traces(
        &mut self,
        result: Result<PageWalker, GpsTracesError>,
        cx: &mut Context<Self>,
    ) {
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
                self.set_status(tr!("gps_traces.state.failed", detail = detail.to_string()));
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
