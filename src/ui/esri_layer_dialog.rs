//! Modal browser for ArcGIS REST (Esri) servers: open a services directory,
//! walk its folders, pick a MapServer or ImageServer (and, for a MapServer,
//! which of its layers to show) and add it as a custom imagery layer.

use std::collections::BTreeSet;
use std::sync::Arc;

use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _, Disableable as _,
};

use crate::custom_imagery_store::CustomImageryEntry;
use crate::http::HttpClient;
use crate::imagery::esri::{
    self, Directory, EsriError, Location, ServiceInfo, ServiceKind, ServiceRef,
};
use crate::tr;
use crate::ui::modal;
use crate::ui::style::muted_text_size;

pub enum DialogEvent {
    Submitted(CustomImageryEntry),
    Cancelled,
}

/// A loaded service: what it is, what it says about itself, and which of its
/// layers are ticked.
struct LoadedService {
    service: ServiceRef,
    info: ServiceInfo,
    checked: BTreeSet<u32>,
}

enum Content {
    /// Nothing opened yet.
    Empty,
    Loading,
    Error(SharedString),
    Directory(Directory),
    Service(LoadedService),
}

/// What a fetch brought back.
enum Loaded {
    Directory(Directory),
    Service(ServiceRef, ServiceInfo, Option<u32>),
}

pub struct EsriLayerDialog {
    http: Arc<dyn HttpClient>,
    url: Entity<InputState>,
    name: Entity<InputState>,
    /// The `token` of the URL last opened, sent with every request and kept
    /// in the layer's template.
    token: Option<String>,
    location: Option<Location>,
    content: Content,
    /// Bumped by each navigation, so a slow reply to an earlier one is
    /// dropped.
    generation: u64,
    /// A name to put in the name field at the next render (a fetch completes
    /// without a window to set it with).
    pending_name: Option<String>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for EsriLayerDialog {}

impl EsriLayerDialog {
    pub fn new(http: Arc<dyn HttpClient>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = cx
            .new(|cx| InputState::new(window, cx).placeholder(tr!("dialog.esri.url_placeholder")));
        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr!("dialog.custom_imagery.name_placeholder"))
        });
        url.update(cx, |state, cx| state.focus(window, cx));
        Self {
            http,
            url,
            name,
            token: None,
            location: None,
            content: Content::Empty,
            generation: 0,
            pending_name: None,
            error: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Open the URL typed in the address field.
    fn open_typed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.url.read(cx).value().to_string();
        match esri::parse_url(&text) {
            Ok(parsed) => {
                self.token = parsed.token;
                self.navigate(parsed.location, window, cx);
            }
            Err(e) => {
                self.content = Content::Error(error_message(&e));
                cx.notify();
            }
        }
    }

    fn navigate(&mut self, location: Location, window: &mut Window, cx: &mut Context<Self>) {
        let shown = location.url();
        self.url
            .update(cx, |state, cx| state.set_value(shown, window, cx));
        self.fetch(location, cx);
    }

    fn fetch(&mut self, location: Location, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.location = Some(location.clone());
        self.content = Content::Loading;
        self.error = None;
        cx.notify();
        let http = self.http.clone();
        let token = self.token.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { load(http.as_ref(), &location, token.as_deref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.show(result);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn show(&mut self, result: Result<Loaded, EsriError>) {
        self.content = match result {
            Err(e) => Content::Error(error_message(&e)),
            Ok(Loaded::Directory(d)) => Content::Directory(d),
            Ok(Loaded::Service(service, info, layer)) => {
                let named = layer.and_then(|id| info.layers.iter().find(|l| l.id == id));
                let checked = match named {
                    Some(l) => BTreeSet::from([l.id]),
                    None => info.default_layers(),
                };
                self.pending_name = Some(
                    named
                        .map(|l| format!("{} – {}", service.short_name(), l.name))
                        .or_else(|| info.title.clone())
                        .unwrap_or_else(|| service.short_name().to_string()),
                );
                Content::Service(LoadedService {
                    service,
                    info,
                    checked,
                })
            }
        };
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(parent) = self.location.as_ref().and_then(Location::parent) {
            self.navigate(parent, window, cx);
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let Content::Service(loaded) = &self.content else {
            return;
        };
        let has_layers = !loaded.info.layers.is_empty();
        if loaded.service.kind == ServiceKind::MapServer && has_layers && loaded.checked.is_empty()
        {
            self.error = Some(tr!("dialog.esri.error.no_layers"));
            cx.notify();
            return;
        }
        let source = esri::tile_source(
            &loaded.service,
            &loaded.info,
            Some(&loaded.checked),
            self.token.as_deref(),
        );
        let typed = self.name.read(cx).value().trim().to_string();
        let name = if typed.is_empty() {
            loaded.service.short_name().to_string()
        } else {
            typed
        };
        cx.emit(DialogEvent::Submitted(CustomImageryEntry {
            name,
            url_template: source.url_template,
            min_zoom: source.min_zoom,
            max_zoom: source.max_zoom,
            attribution: loaded.info.copyright.clone(),
        }));
    }

    fn toggle_layer(&mut self, id: u32, on: bool, cx: &mut Context<Self>) {
        if let Content::Service(loaded) = &mut self.content {
            if on {
                loaded.checked.insert(id);
            } else {
                loaded.checked.remove(&id);
            }
            self.error = None;
            cx.notify();
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => cx.emit(DialogEvent::Cancelled),
            modal::ModalKey::Enter => {
                // In the address field Enter opens what was typed, unless it
                // is the service already shown: then it adds that.
                let in_address = self.url.read(cx).focus_handle(cx).is_focused(window);
                let typed = self.url.read(cx).value().trim().to_string();
                let shown_service = match (&self.content, &self.location) {
                    (Content::Service(_), Some(loc)) => Some(loc.url()),
                    _ => None,
                };
                if in_address && shown_service.as_deref() != Some(typed.as_str()) {
                    self.open_typed(window, cx);
                } else {
                    self.submit(cx);
                }
            }
            modal::ModalKey::Other => {}
        }
    }

    fn render_directory(&self, d: &Directory, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        if d.folders.is_empty() && d.services.is_empty() {
            return Label::new(tr!("dialog.esri.empty_directory"))
                .text_color(muted)
                .into_any_element();
        }
        let root = match &self.location {
            Some(Location::Directory { root, .. }) => root.clone(),
            _ => String::new(),
        };
        let row = |id: (&'static str, usize), title: String, detail: SharedString| {
            div()
                .id(id)
                .flex()
                .flex_col()
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|el| el.bg(cx.theme().accent))
                .child(Label::new(title))
                .child(
                    Label::new(detail)
                        .text_size(muted_text_size())
                        .text_color(muted),
                )
        };
        let folders = d.folders.iter().enumerate().map(|(i, f)| {
            let loc = Location::Directory {
                root: root.clone(),
                folder: Some(f.clone()),
            };
            let title = f.rsplit('/').next().unwrap_or(f).to_string();
            row(("esri-folder", i), title, tr!("dialog.esri.folder")).on_click(
                cx.listener(move |this, _, window, cx| this.navigate(loc.clone(), window, cx)),
            )
        });
        let services = d.services.iter().enumerate().map(|(i, s)| {
            let loc = Location::Service {
                root: Some(root.clone()),
                service: s.clone(),
                layer: None,
            };
            let detail = match s.kind {
                ServiceKind::MapServer => tr!("dialog.esri.map_service"),
                ServiceKind::ImageServer => tr!("dialog.esri.image_service"),
            };
            row(("esri-service", i), s.short_name().to_string(), detail).on_click(
                cx.listener(move |this, _, window, cx| this.navigate(loc.clone(), window, cx)),
            )
        });
        v_flex()
            .children(folders)
            .children(services)
            .into_any_element()
    }

    fn render_service(&self, loaded: &LoadedService, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let info = &loaded.info;
        let source = esri::tile_source(
            &loaded.service,
            info,
            Some(&loaded.checked),
            self.token.as_deref(),
        );
        let mode = if info.cache.is_some() && source.url_template.contains("/tile/{z}/") {
            tr!(
                "dialog.esri.cached",
                min = source.min_zoom,
                max = source.max_zoom
            )
        } else {
            tr!("dialog.esri.dynamic")
        };
        let kind = match loaded.service.kind {
            ServiceKind::MapServer => tr!("dialog.esri.map_service"),
            ServiceKind::ImageServer => tr!("dialog.esri.image_service"),
        };
        let small = |text: SharedString| {
            Label::new(text)
                .text_size(muted_text_size())
                .text_color(muted)
        };
        let mut body = v_flex()
            .gap_2()
            .child(Label::new(loaded.service.path.clone()))
            .child(small(format!("{kind} · {mode}").into()));
        if let Some(d) = &info.description {
            body = body.child(small(d.clone().into()));
        }
        if let Some(c) = &info.copyright {
            body = body.child(small(tr!("dialog.esri.copyright", text = c)));
        }
        if loaded.service.kind == ServiceKind::MapServer && !info.layers.is_empty() {
            let layers = info.layers.iter().enumerate().map(|(i, layer)| {
                let id = layer.id;
                let indent = gpui::px(16.0 * info.depth(layer) as f32);
                div().pl(indent).child(
                    Checkbox::new(("esri-layer", i))
                        .checked(loaded.checked.contains(&id))
                        .label(layer.name.clone())
                        .text_size(crate::ui::style::body_text_size())
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.toggle_layer(id, *checked, cx)
                        })),
                )
            });
            body = body
                .child(small(tr!("dialog.esri.layers")))
                .child(v_flex().gap_1().children(layers));
        }
        body.into_any_element()
    }
}

/// Fetch what `location` points at (blocking; run off the UI thread).
fn load(
    http: &dyn HttpClient,
    location: &Location,
    token: Option<&str>,
) -> Result<Loaded, EsriError> {
    match location {
        Location::Directory { root, folder } => {
            esri::fetch_directory(http, root, folder.as_deref(), token).map(Loaded::Directory)
        }
        Location::Service { service, layer, .. } => esri::fetch_service(http, service, token)
            .map(|info| Loaded::Service(service.clone(), info, *layer)),
    }
}

fn error_message(e: &EsriError) -> SharedString {
    match e {
        EsriError::NotArcGisUrl => tr!("dialog.esri.error.not_arcgis"),
        other => tr!("dialog.esri.error.load", error = other),
    }
}

impl crate::ui::modal::ModalDialog for EsriLayerDialog {
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        self.url.read(cx).focus_handle(cx)
    }
}

impl Focusable for EsriLayerDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EsriLayerDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(name) = self.pending_name.take() {
            self.name
                .update(cx, |state, cx| state.set_value(name, window, cx));
        }
        let muted = cx.theme().muted_foreground;
        let can_go_up = self.location.as_ref().and_then(Location::parent).is_some();
        let is_service = matches!(self.content, Content::Service(_));

        let address = h_flex()
            .gap_2()
            .child(div().flex_1().child(Input::new(&self.url)))
            .child(
                Button::new("esri-open")
                    .label(tr!("dialog.esri.open"))
                    .on_click(cx.listener(|this, _, window, cx| this.open_typed(window, cx))),
            )
            .child(
                Button::new("esri-up")
                    .label(tr!("dialog.esri.up"))
                    .disabled(!can_go_up)
                    .on_click(cx.listener(|this, _, window, cx| this.go_up(window, cx))),
            );

        let content: gpui::AnyElement = match &self.content {
            Content::Empty => Label::new(tr!("dialog.esri.hint"))
                .text_color(muted)
                .into_any_element(),
            Content::Loading => Label::new(tr!("dialog.esri.loading"))
                .text_color(muted)
                .into_any_element(),
            Content::Error(msg) => Label::new(msg.clone())
                .text_color(cx.theme().danger)
                .into_any_element(),
            Content::Directory(d) => self.render_directory(d, cx),
            Content::Service(s) => self.render_service(s, cx),
        };

        let mut body = v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(tr!("dialog.esri.url"))
                            .text_size(muted_text_size())
                            .text_color(muted),
                    )
                    .child(address),
            )
            .child(
                div()
                    .id("esri-content")
                    .h(gpui::px(280.0))
                    .overflow_y_scroll()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .p_2()
                    .child(content),
            );
        if is_service {
            body = body.child(modal::field_row(
                tr!("dialog.custom_imagery.name"),
                &self.name,
                muted,
            ));
        }
        if let Some(msg) = self.error.clone() {
            body = body.child(Label::new(msg).text_color(cx.theme().danger));
        }

        let footer = modal::footer_buttons(
            Button::new("esri-cancel")
                .label(tr!("dialog.cancel"))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Cancelled))),
            Button::new("esri-add")
                .primary()
                .label(tr!("dialog.custom_imagery.confirm"))
                .disabled(!is_service)
                .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
        );

        let frame =
            modal::dialog_frame(cx, gpui::px(560.0), tr!("dialog.esri.title"), body, footer);
        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
