//! Extensions in the window: loading the installed ones, the Extensions
//! menu, running their calls off the UI thread, applying what they ask for
//! (markers, side-panel sections, map moves, downloads), and Install Dev
//! Extension.
//!
//! The extension host itself (manifest, sandbox, WIT bindings, locales) is
//! `osm_gpui::extensions`. Every call into an extension runs on the
//! background executor; one extension runs one call at a time, and a click
//! that arrives while it is busy is dropped with a status message rather
//! than queued.
//!
//! An extension may show several panels at once, and several extensions may
//! each show theirs. Every panel is a collapsible section of its own at the
//! end of the side panel's middle zone, keyed by (extension, panel id), in
//! the order they were first shown.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use osm_gpui::coordinates::{GeoBounds, LatLon};
use osm_gpui::extensions::manifest::CommandDef;
use osm_gpui::extensions::runtime::{
    Bounds, ButtonStyle, Element, FeatureKind as ExtFeatureKind, FeatureRef as ExtFeatureRef, Panel,
};
use osm_gpui::extensions::{CallContext, Effect, LoadedExtension, Locales, Outcome};
use osm_gpui::layers::extension_layer::{ExtensionLayer, MarkerStore};
use osm_gpui::layers::LayerId;
use osm_gpui::selection::FeatureKind;
use osm_gpui::tr;
use osm_gpui::ui::style::muted_text_size;

use crate::MapViewer;

/// Extensions > <extension> > <command>.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunExtensionCommand {
    pub extension: String,
    pub command: String,
}

/// One extension as the Extensions menu lists it.
#[derive(Clone, Debug)]
pub(crate) struct MenuEntry {
    pub id: String,
    name: String,
    pub commands: Vec<CommandDef>,
    locales: Arc<Locales>,
}

impl MenuEntry {
    /// The extension's name in `locale`.
    pub(crate) fn name_in(&self, locale: &str) -> String {
        self.locales
            .lookup(locale, "name")
            .unwrap_or(&self.name)
            .to_string()
    }

    /// Command `c`'s label in `locale` (`commands.<id>`, else the manifest's).
    pub(crate) fn command_label(&self, c: &CommandDef, locale: &str) -> String {
        self.locales
            .lookup(locale, &format!("commands.{}", c.id))
            .unwrap_or(&c.label)
            .to_string()
    }
}

/// What the menus show, kept here because they are built without the window
/// (as the saved custom imagery is).
fn menu_store() -> &'static Mutex<Vec<MenuEntry>> {
    static MENU: OnceLock<Mutex<Vec<MenuEntry>>> = OnceLock::new();
    MENU.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) fn menu_snapshot() -> Vec<MenuEntry> {
    menu_store().lock().unwrap().clone()
}

/// One section an extension shows in the side panel, with the inputs its
/// text boxes need (made on the next frame, which has the window).
struct OpenPanel {
    extension: String,
    id: String,
    panel: Panel,
    inputs: HashMap<String, Entity<InputState>>,
    /// Expanded (the header's chevron).
    expanded: bool,
}

#[derive(Default)]
pub(crate) struct ExtensionsState {
    started: bool,
    /// The running extensions (installed and turned on).
    pub(crate) loaded: Vec<Arc<LoadedExtension>>,
    /// Everything installed, on or off (see `extensions_manage`).
    pub(crate) installed: Vec<crate::extensions_manage::Installed>,
    /// Installed extensions the user turned off.
    pub(crate) disabled: std::collections::BTreeSet<String>,
    /// The extension index, once fetched.
    pub(crate) index: Option<osm_gpui::extensions::registry::Index>,
    pub(crate) index_state: osm_gpui::ui::extensions_dialog::IndexState,
    /// Extensions > Manage Extensions…, while open.
    pub(crate) dialog: Option<Entity<osm_gpui::ui::extensions_dialog::ExtensionsDialog>>,
    /// Extensions being installed, updated or rebuilt, and the last failure
    /// of each, for the manager.
    pub(crate) row_busy: HashSet<String>,
    pub(crate) row_errors: HashMap<String, String>,
    /// Each extension's marker layer, made when it first sets markers.
    layers: HashMap<String, (LayerId, MarkerStore)>,
    /// Open panels, in the order they were first shown.
    panels: Vec<OpenPanel>,
    /// Extensions with a call in flight.
    busy: HashSet<String>,
    /// The changeset tags each extension last asked for, read when the
    /// upload dialog opens (asking then could wait on a busy extension).
    changeset_tags: HashMap<String, Vec<(String, String)>>,
    pub(crate) installing_dev: bool,
}

fn to_ext_bounds(b: &GeoBounds) -> Bounds {
    Bounds {
        min_lat: b.min_lat,
        min_lon: b.min_lon,
        max_lat: b.max_lat,
        max_lon: b.max_lon,
    }
}

/// An extension's area as the app's, refused when it is not a real area.
fn from_ext_bounds(b: &Bounds) -> Option<GeoBounds> {
    let ok = [b.min_lat, b.max_lat, b.min_lon, b.max_lon]
        .iter()
        .all(|v| v.is_finite())
        && b.min_lat < b.max_lat
        && b.min_lon < b.max_lon
        && b.min_lat >= -90.0
        && b.max_lat <= 90.0
        && b.min_lon >= -180.0
        && b.max_lon <= 180.0;
    ok.then(|| GeoBounds::new(b.min_lat, b.max_lat, b.min_lon, b.max_lon))
}

/// Show, replace or close panel `id` of `extension` in `panels`. A shown
/// panel keeps its place, expansion and typed text when it is replaced.
fn apply_show_panel(
    panels: &mut Vec<OpenPanel>,
    extension: &str,
    id: String,
    panel: Option<Panel>,
) {
    let at = panels
        .iter()
        .position(|p| p.extension == extension && p.id == id);
    match (panel, at) {
        (Some(panel), Some(i)) => {
            // Inputs no longer in the panel are dropped.
            let keep: HashSet<&str> = panel
                .elements
                .iter()
                .filter_map(|e| match e {
                    Element::Input(i) => Some(i.id.as_str()),
                    _ => None,
                })
                .collect();
            panels[i].inputs.retain(|k, _| keep.contains(k.as_str()));
            panels[i].panel = panel;
        }
        (Some(panel), None) => panels.push(OpenPanel {
            extension: extension.to_string(),
            id,
            panel,
            inputs: HashMap::new(),
            expanded: true,
        }),
        (None, Some(i)) => {
            panels.remove(i);
        }
        (None, None) => {}
    }
}

impl MapViewer {
    /// Per-frame upkeep, called from `render`: loads the installed
    /// extensions once, follows a marker layer deleted from the Layers list,
    /// and makes the open panels' text boxes.
    pub(crate) fn extensions_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.extensions.started {
            self.extensions.started = true;
            self.start_extensions(cx);
        }
        let present: HashSet<LayerId> =
            self.layer_manager.layers().iter().map(|l| l.id()).collect();
        self.extensions
            .layers
            .retain(|_, (id, _)| present.contains(id));
        for open in &mut self.extensions.panels {
            for el in &open.panel.elements {
                let Element::Input(input) = el else {
                    continue;
                };
                if open.inputs.contains_key(&input.id) {
                    continue;
                }
                let (placeholder, value, secret) =
                    (input.placeholder.clone(), input.value.clone(), input.secret);
                let state = cx.new(|cx| {
                    let mut s = InputState::new(window, cx)
                        .placeholder(placeholder)
                        .masked(secret);
                    if !value.is_empty() {
                        s.set_value(value, window, cx);
                    }
                    s
                });
                open.inputs.insert(input.id.clone(), state);
            }
        }
    }

    pub(crate) fn add_loaded_extension(&mut self, ext: LoadedExtension) {
        let id = ext.manifest.id.clone();
        self.extensions.loaded.retain(|e| e.manifest.id != id);
        self.extensions.loaded.push(Arc::new(ext));
        self.extensions
            .loaded
            .sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    }

    pub(crate) fn refresh_extension_menu(&mut self, cx: &mut Context<Self>) {
        *menu_store().lock().unwrap() = self
            .extensions
            .loaded
            .iter()
            .map(|e| MenuEntry {
                id: e.manifest.id.clone(),
                name: e.manifest.name.clone(),
                commands: e.manifest.commands.clone(),
                locales: e.locales.clone(),
            })
            .collect();
        let (center, state) = self.imagery_menu_context();
        crate::menu::rebuild_menus(&mut *cx, center, state, self.menu_toggles());
    }

    fn find_extension(&self, id: &str) -> Option<&Arc<LoadedExtension>> {
        self.extensions.loaded.iter().find(|e| e.manifest.id == id)
    }

    /// Extension `id`'s name in the app's language.
    pub(crate) fn extension_name(&self, id: &str) -> String {
        self.find_extension(id)
            .map(|e| e.display_name(&osm_gpui::i18n::current_locale()))
            .unwrap_or_else(|| id.to_string())
    }

    /// The app's state for an extension call.
    fn extension_context(&self) -> CallContext {
        CallContext {
            viewport: to_ext_bounds(&self.viewport.visible_bounds()),
            zoom: self.viewport.zoom_level(),
            selection: self
                .selected
                .iter()
                .map(|f| ExtFeatureRef {
                    kind: match f.kind {
                        FeatureKind::Node => ExtFeatureKind::Node,
                        FeatureKind::Way => ExtFeatureKind::Way,
                        FeatureKind::Relation => ExtFeatureKind::Relation,
                    },
                    id: f.id,
                })
                .collect(),
            locale: osm_gpui::i18n::current_locale(),
        }
    }

    /// Run `call` on extension `id` in the background, then apply what it
    /// asked for.
    fn call_extension(
        &mut self,
        id: &str,
        call: impl FnOnce(&LoadedExtension, CallContext) -> Outcome<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(ext) = self.find_extension(id).cloned() else {
            self.set_status(tr!("extensions.not_installed", id = id));
            cx.notify();
            return;
        };
        if !self.extensions.busy.insert(id.to_string()) {
            self.set_status(tr!("extensions.busy", name = self.extension_name(id)));
            cx.notify();
            return;
        }
        let context = self.extension_context();
        let id = id.to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (outcome, tags) = cx
                .background_executor()
                .spawn(async move {
                    let outcome = call(&ext, context.clone());
                    let tags = ext.changeset_tags(context).result.unwrap_or_default();
                    (outcome, tags)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.extensions.busy.remove(&id);
                this.extensions.changeset_tags.insert(id.clone(), tags);
                this.apply_extension_effects(&id, outcome.effects, cx);
                if let Err(e) = outcome.result {
                    this.set_status(e);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_extension_effects(&mut self, id: &str, effects: Vec<Effect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                Effect::Status(message) => self.set_status(message),
                Effect::FlyTo { at, zoom } => {
                    self.viewport.pan_to(LatLon::new(at.lat, at.lon));
                    self.viewport.set_zoom(zoom.clamp(1.0, 22.0));
                }
                Effect::DownloadArea(area) => match from_ext_bounds(&area) {
                    Some(bounds) => self.request_download_of(bounds, cx),
                    None => {
                        self.set_status(tr!("extensions.bad_area", name = self.extension_name(id)))
                    }
                },
                Effect::OpenUrl(url) => {
                    if let Err(e) = open::that(&url) {
                        self.set_status(tr!(
                            "extensions.open_url_failed",
                            url = url,
                            detail = e.to_string()
                        ));
                    }
                }
                Effect::SetMarkers(markers) => self.set_extension_markers(id, markers),
                Effect::ShowPanel {
                    id: panel_id,
                    panel,
                } => apply_show_panel(&mut self.extensions.panels, id, panel_id, panel),
            }
        }
    }

    fn set_extension_markers(
        &mut self,
        id: &str,
        markers: Vec<osm_gpui::extensions::runtime::Marker>,
    ) {
        if let Some((_, store)) = self.extensions.layers.get(id) {
            store.replace(markers);
            return;
        }
        if markers.is_empty() {
            return;
        }
        let name = self.extension_name(id);
        let store = MarkerStore::default();
        store.replace(markers);
        let layer_id = self.layer_manager.alloc_id();
        self.layer_manager.add_layer(Box::new(ExtensionLayer::new(
            layer_id,
            name,
            id.to_string(),
            store.clone(),
        )));
        self.extensions
            .layers
            .insert(id.to_string(), (layer_id, store));
    }

    pub(crate) fn on_run_extension_command(
        &mut self,
        action: &RunExtensionCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = action.command.clone();
        self.call_extension(
            &action.extension,
            move |ext, context| ext.run_command(context, &command),
            cx,
        );
    }

    pub(crate) fn extension_marker_clicked(
        &mut self,
        extension: &str,
        marker: &str,
        cx: &mut Context<Self>,
    ) {
        let marker = marker.to_string();
        self.call_extension(
            extension,
            move |ext, context| ext.marker_clicked(context, &marker),
            cx,
        );
    }

    fn extension_panel_button(&mut self, index: usize, button: String, cx: &mut Context<Self>) {
        let Some(open) = self.extensions.panels.get(index) else {
            return;
        };
        let (extension, panel) = (open.extension.clone(), open.id.clone());
        let inputs: Vec<(String, String)> = open
            .inputs
            .iter()
            .map(|(id, state)| (id.clone(), state.read(cx).value().to_string()))
            .collect();
        self.call_extension(
            &extension,
            move |ext, context| ext.panel_button(context, &panel, &button, &inputs),
            cx,
        );
    }

    /// The close button of panel `index`: remove it and tell its extension.
    fn close_extension_panel(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.extensions.panels.len() {
            return;
        }
        let open = self.extensions.panels.remove(index);
        let panel = open.id;
        self.call_extension(
            &open.extension,
            move |ext, context| ext.panel_closed(context, &panel),
            cx,
        );
    }

    /// The comment an extension suggests for the next upload: its `comment`
    /// tag, with its `hashtags` added when the comment lacks them.
    pub(crate) fn extension_changeset_comment(&self) -> Option<String> {
        self.extensions.changeset_tags.values().find_map(|tags| {
            let get = |k: &str| {
                tags.iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.clone())
            };
            let mut comment = get("comment")?;
            if let Some(hashtags) = get("hashtags") {
                for tag in hashtags.split([' ', ';']).filter(|t| !t.is_empty()) {
                    if !comment.contains(tag) {
                        comment.push(' ');
                        comment.push_str(tag);
                    }
                }
            }
            Some(comment)
        })
    }

    pub(crate) fn extensions_upload_finished(
        &mut self,
        changesets: Vec<i64>,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<String> = self
            .extensions
            .loaded
            .iter()
            .map(|e| e.manifest.id.clone())
            .collect();
        for id in ids {
            let changesets = changesets.clone();
            self.call_extension(
                &id,
                move |ext, context| ext.upload_finished(context, &changesets),
                cx,
            );
        }
    }

    /// Remove extension `id`'s markers and panels (it is being replaced or
    /// uninstalled).
    pub(crate) fn drop_extension_ui(&mut self, id: &str) {
        if let Some((layer, _)) = self.extensions.layers.remove(id) {
            if let Some(i) = self
                .layer_manager
                .layers()
                .iter()
                .position(|l| l.id() == layer)
            {
                self.layer_manager.remove_at(i);
            }
        }
        self.extensions.panels.retain(|p| p.extension != id);
        self.extensions.changeset_tags.remove(id);
    }

    /// Every open extension panel, each a collapsible section for the side
    /// panel's middle zone.
    pub(crate) fn render_extension_panels(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, border, foreground, hover, link) = (
            theme.muted_foreground,
            theme.border,
            theme.foreground,
            theme.accent,
            theme.link,
        );
        let mut sections = Vec::new();
        for (index, open) in self.extensions.panels.iter().enumerate() {
            let busy = self.extensions.busy.contains(&open.extension);
            let name = self.extension_name(&open.extension);
            let expanded = open.expanded;

            let header = div()
                .id(("extension-panel-header", index))
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .px_3()
                .h(px(32.))
                .cursor_pointer()
                .hover(move |this| this.bg(hover))
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall()
                    .text_color(muted),
                )
                .child(Label::new(open.panel.title.clone()).font_weight(gpui::FontWeight::SEMIBOLD))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(muted_text_size())
                        .text_color(muted)
                        .child(if busy {
                            tr!("extensions.panel.working")
                        } else {
                            SharedString::from(name)
                        }),
                )
                .child(
                    Button::new(("extension-close-panel", index))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip(tr!("extensions.panel.close"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_extension_panel(index, cx);
                            cx.notify();
                        })),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(p) = this.extensions.panels.get_mut(index) {
                        p.expanded = !p.expanded;
                    }
                    cx.notify();
                }));

            let mut body = v_flex().flex_shrink_0().gap_2().px_3().pt_0p5().pb_2();
            for (i, el) in open.panel.elements.iter().enumerate() {
                let key = index * 1000 + i;
                let row: AnyElement = match el {
                    Element::Heading(text) => Label::new(text.clone())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(foreground)
                        .into_any_element(),
                    Element::Text(text) => div()
                        .text_color(foreground)
                        .child(SharedString::from(text.clone()))
                        .into_any_element(),
                    Element::Muted(text) => div()
                        .text_size(muted_text_size())
                        .text_color(muted)
                        .child(SharedString::from(text.clone()))
                        .into_any_element(),
                    Element::Property((label, value)) => div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            div()
                                .w(px(64.0))
                                .flex_shrink_0()
                                .text_color(muted)
                                .child(SharedString::from(label.clone())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(foreground)
                                .child(SharedString::from(value.clone())),
                        )
                        .into_any_element(),
                    Element::Buttons(buttons) => div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_2()
                        .children(buttons.iter().enumerate().map(|(j, b)| {
                            let id = b.id.clone();
                            let button = Button::new(("extension-button", key * 100 + j))
                                .small()
                                .label(b.label.clone())
                                .disabled(busy || !b.enabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.extension_panel_button(index, id.clone(), cx);
                                }));
                            match b.style {
                                ButtonStyle::Primary => button.primary(),
                                ButtonStyle::Danger => button.danger(),
                                ButtonStyle::Ghost => button.ghost(),
                                ButtonStyle::Secondary => button,
                            }
                        }))
                        .into_any_element(),
                    Element::Input(input) => {
                        let label = (!input.label.is_empty()).then(|| {
                            div()
                                .text_size(muted_text_size())
                                .text_color(muted)
                                .child(SharedString::from(input.label.clone()))
                        });
                        match open.inputs.get(&input.id) {
                            Some(state) => v_flex()
                                .gap_1()
                                .children(label)
                                .child(Input::new(state).disabled(busy))
                                .into_any_element(),
                            // Made on the next frame.
                            None => v_flex().children(label).into_any_element(),
                        }
                    }
                    Element::Link((label, url)) => {
                        let url = url.clone();
                        div()
                            .id(("extension-link", key))
                            .text_color(link)
                            .underline()
                            .cursor_pointer()
                            .child(SharedString::from(label.clone()))
                            .on_click(move |_, _, _| {
                                if url.starts_with("https://") || url.starts_with("http://") {
                                    let _ = open::that(&url);
                                }
                            })
                            .into_any_element()
                    }
                    Element::Divider => div().h(px(1.0)).bg(border).into_any_element(),
                };
                body = body.child(row);
            }

            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(border)
                    .child(header)
                    .when(expanded, |this| this.child(body))
                    .into_any_element(),
            );
        }
        sections
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_areas_are_downloaded() {
        let ok = Bounds {
            min_lat: 44.9,
            min_lon: -93.3,
            max_lat: 45.0,
            max_lon: -93.2,
        };
        assert!(from_ext_bounds(&ok).is_some());
        assert!(from_ext_bounds(&Bounds {
            min_lat: 45.1,
            ..ok
        })
        .is_none());
        assert!(from_ext_bounds(&Bounds {
            max_lon: f64::NAN,
            ..ok
        })
        .is_none());
        assert!(from_ext_bounds(&Bounds {
            max_lat: 91.0,
            ..ok
        })
        .is_none());
    }

    fn panel(title: &str) -> Panel {
        Panel {
            title: title.into(),
            elements: vec![],
        }
    }

    #[test]
    fn panels_are_kept_per_extension_and_id_in_the_order_first_shown() {
        let mut panels = Vec::new();
        apply_show_panel(&mut panels, "a", "task".into(), Some(panel("A task")));
        apply_show_panel(&mut panels, "b", "task".into(), Some(panel("B task")));
        apply_show_panel(&mut panels, "a", "key".into(), Some(panel("A key")));
        let titles = |p: &Vec<OpenPanel>| -> Vec<String> {
            p.iter().map(|p| p.panel.title.clone()).collect()
        };
        assert_eq!(titles(&panels), vec!["A task", "B task", "A key"]);

        // Replacing keeps the place and the expansion.
        panels[0].expanded = false;
        apply_show_panel(&mut panels, "a", "task".into(), Some(panel("A task 2")));
        assert_eq!(titles(&panels), vec!["A task 2", "B task", "A key"]);
        assert!(!panels[0].expanded);

        // Closing one leaves the others, including another extension's
        // panel with the same id.
        apply_show_panel(&mut panels, "a", "task".into(), None);
        assert_eq!(titles(&panels), vec!["B task", "A key"]);
        apply_show_panel(&mut panels, "a", "nothing".into(), None);
        assert_eq!(panels.len(), 2);
    }
}
