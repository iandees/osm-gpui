//! Extensions in the window: loading the installed ones, the Extensions
//! menu, running their calls off the UI thread, applying what they ask for
//! (markers, the side-panel section, map moves, downloads), and Install Dev
//! Extension.
//!
//! The extension host itself (manifest, sandbox, WIT bindings) is
//! `osm_gpui::extensions`. Every call into an extension runs on the
//! background executor; one extension runs one call at a time, and a click
//! that arrives while it is busy is dropped with a status message rather
//! than queued.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use gpui::{actions, div, prelude::*, AnyElement, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _, Disableable as _, IconName, Sizable as _,
};
use osm_gpui::coordinates::{GeoBounds, LatLon};
use osm_gpui::extensions::manifest::CommandDef;
use osm_gpui::extensions::runtime::{
    Bounds, ButtonStyle, Element, FeatureKind as ExtFeatureKind, FeatureRef as ExtFeatureRef, Panel,
};
use osm_gpui::extensions::{self, CallContext, Effect, LoadedExtension, Outcome};
use osm_gpui::layers::extension_layer::{ExtensionLayer, MarkerStore};
use osm_gpui::layers::LayerId;
use osm_gpui::selection::FeatureKind;
use osm_gpui::ui::style::muted_text_size;

use crate::MapViewer;

actions!(osm_gpui, [InstallDevExtension]);

/// Extensions > <extension> > <command>.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunExtensionCommand {
    pub extension: String,
    pub command: String,
}

/// Extensions > <extension> > Uninstall.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = osm_gpui)]
#[serde(deny_unknown_fields)]
pub(crate) struct UninstallExtension {
    pub extension: String,
}

/// One extension as the Extensions menu lists it.
#[derive(Clone, Debug)]
pub(crate) struct MenuEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub commands: Vec<CommandDef>,
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

/// The section an extension shows in the side panel, with the inputs its
/// text boxes need (made on the next frame, which has the window).
struct OpenPanel {
    extension: String,
    panel: Panel,
    inputs: HashMap<String, Entity<InputState>>,
}

#[derive(Default)]
pub(crate) struct ExtensionsState {
    started: bool,
    loaded: Vec<Arc<LoadedExtension>>,
    /// Each extension's marker layer, made when it first sets markers.
    layers: HashMap<String, (LayerId, MarkerStore)>,
    panel: Option<OpenPanel>,
    /// Extensions with a call in flight.
    busy: HashSet<String>,
    /// The changeset tags each extension last asked for, read when the
    /// upload dialog opens (asking then could wait on a busy extension).
    changeset_tags: HashMap<String, Vec<(String, String)>>,
    installing: bool,
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

impl MapViewer {
    /// Per-frame upkeep, called from `render`: loads the installed
    /// extensions once, follows a marker layer deleted from the Layers list,
    /// and makes the open panel's text boxes.
    pub(crate) fn extensions_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.extensions.started {
            self.extensions.started = true;
            self.load_extensions(cx);
        }
        let present: HashSet<LayerId> =
            self.layer_manager.layers().iter().map(|l| l.id()).collect();
        self.extensions
            .layers
            .retain(|_, (id, _)| present.contains(id));
        if let Some(open) = &mut self.extensions.panel {
            for el in &open.panel.elements {
                if let Element::Input(input) = el {
                    if !open.inputs.contains_key(&input.id) {
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
        }
    }

    /// Load every installed extension, in the background.
    fn load_extensions(&mut self, cx: &mut Context<Self>) {
        let Some(root) = extensions::root_dir() else {
            return;
        };
        let http = self.http.clone();
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    extensions::installed(&root)
                        .into_iter()
                        .map(|dir| {
                            let id = dir
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            let storage = Some(extensions::storage_path(&root, &id));
                            LoadedExtension::load(&dir, http.clone(), storage)
                                .map_err(|e| format!("{}: {e}", dir.display()))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                for r in results {
                    match r {
                        Ok(ext) => this.add_loaded_extension(ext),
                        Err(e) => {
                            eprintln!("extension failed to load: {e}");
                            this.set_status(format!("An extension failed to load: {e}"));
                        }
                    }
                }
                this.refresh_extension_menu(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn add_loaded_extension(&mut self, ext: LoadedExtension) {
        let id = ext.manifest.id.clone();
        self.extensions.loaded.retain(|e| e.manifest.id != id);
        self.extensions.loaded.push(Arc::new(ext));
        self.extensions
            .loaded
            .sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    }

    fn refresh_extension_menu(&mut self, cx: &mut Context<Self>) {
        *menu_store().lock().unwrap() = self
            .extensions
            .loaded
            .iter()
            .map(|e| MenuEntry {
                id: e.manifest.id.clone(),
                name: e.manifest.name.clone(),
                version: e.manifest.version.clone(),
                commands: e.manifest.commands.clone(),
            })
            .collect();
        let (center, state) = self.imagery_menu_context();
        crate::menu::rebuild_menus(&mut *cx, center, state, self.menu_toggles());
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
        let Some(ext) = self
            .extensions
            .loaded
            .iter()
            .find(|e| e.manifest.id == id)
            .cloned()
        else {
            self.set_status(format!("Extension {id} is not installed"));
            cx.notify();
            return;
        };
        if !self.extensions.busy.insert(id.to_string()) {
            self.set_status(format!("{} is still working…", ext.manifest.name));
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
                    None => self.set_status(format!("{id} asked to download a bad area")),
                },
                Effect::OpenUrl(url) => {
                    if let Err(e) = open::that(&url) {
                        self.set_status(format!("Could not open {url}: {e}"));
                    }
                }
                Effect::SetMarkers(markers) => self.set_extension_markers(id, markers),
                Effect::ShowPanel(panel) => self.show_extension_panel(id, panel),
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
        let name = self
            .extensions
            .loaded
            .iter()
            .find(|e| e.manifest.id == id)
            .map(|e| e.manifest.name.clone())
            .unwrap_or_else(|| id.to_string());
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

    fn show_extension_panel(&mut self, id: &str, panel: Option<Panel>) {
        match panel {
            Some(panel) => {
                // The same extension's inputs are kept when the panel is
                // redrawn with the same input ids, so typed text survives.
                let inputs = match self.extensions.panel.take() {
                    Some(old) if old.extension == id => old.inputs,
                    _ => HashMap::new(),
                };
                self.close_note_panel();
                self.close_photo_panel();
                self.extensions.panel = Some(OpenPanel {
                    extension: id.to_string(),
                    panel,
                    inputs,
                });
            }
            None => {
                if self
                    .extensions
                    .panel
                    .as_ref()
                    .is_some_and(|p| p.extension == id)
                {
                    self.extensions.panel = None;
                }
            }
        }
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
        self.set_selection(Vec::new());
        self.call_extension(
            extension,
            move |ext, context| ext.marker_clicked(context, &marker),
            cx,
        );
    }

    fn extension_panel_button(&mut self, button: String, cx: &mut Context<Self>) {
        let Some(open) = &self.extensions.panel else {
            return;
        };
        let extension = open.extension.clone();
        let inputs: Vec<(String, String)> = open
            .inputs
            .iter()
            .map(|(id, state)| (id.clone(), state.read(cx).value().to_string()))
            .collect();
        self.call_extension(
            &extension,
            move |ext, context| ext.panel_button(context, &button, &inputs),
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

    pub(crate) fn on_install_dev_extension(
        &mut self,
        _: &InstallDevExtension,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.extensions.installing {
            return;
        }
        let Some(root) = extensions::root_dir() else {
            return;
        };
        let picked = rfd::AsyncFileDialog::new()
            .set_title("Choose an extension folder (with extension.toml)")
            .pick_folder();
        let http = self.http.clone();
        cx.spawn(async move |this, cx| {
            let Some(folder) = picked.await else {
                return;
            };
            let src: PathBuf = folder.path().to_path_buf();
            let _ = this.update(cx, |this, cx| {
                this.extensions.installing = true;
                this.set_status(format!("Building the extension in {}…", src.display()));
                cx.notify();
            });
            let result = cx
                .background_executor()
                .spawn(async move {
                    let manifest = extensions::read_manifest(&src)?;
                    // A prebuilt extension.wasm next to the manifest is used
                    // as is; otherwise the crate is built.
                    let prebuilt = src.join("extension.wasm");
                    let wasm = if prebuilt.is_file() {
                        prebuilt
                    } else {
                        extensions::build_dev_extension(&src)?
                    };
                    let dir = extensions::install(&root, &src, &wasm)?;
                    let storage = Some(extensions::storage_path(&root, &manifest.id));
                    LoadedExtension::load(&dir, http, storage)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.extensions.installing = false;
                match result {
                    Ok(ext) => {
                        let msg =
                            format!("Installed {} {}", ext.manifest.name, ext.manifest.version);
                        this.drop_extension_ui(&ext.manifest.id.clone());
                        this.add_loaded_extension(ext);
                        this.refresh_extension_menu(cx);
                        this.set_status(msg);
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        this.set_status(format!(
                            "Installing the extension failed: {}",
                            e.lines().next().unwrap_or("")
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn on_uninstall_extension(
        &mut self,
        action: &UninstallExtension,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = extensions::root_dir() else {
            return;
        };
        let id = action.extension.clone();
        match extensions::uninstall(&root, &id) {
            Ok(()) => {
                self.drop_extension_ui(&id);
                self.extensions.loaded.retain(|e| e.manifest.id != id);
                self.refresh_extension_menu(cx);
                self.set_status(format!("Uninstalled {id}"));
            }
            Err(e) => self.set_status(format!("Could not uninstall {id}: {e}")),
        }
        cx.notify();
    }

    /// Remove extension `id`'s markers and panel (it is being replaced or
    /// uninstalled).
    fn drop_extension_ui(&mut self, id: &str) {
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
        if self
            .extensions
            .panel
            .as_ref()
            .is_some_and(|p| p.extension == id)
        {
            self.extensions.panel = None;
        }
        self.extensions.changeset_tags.remove(id);
    }

    pub(crate) fn close_extension_panel(&mut self) {
        self.extensions.panel = None;
    }

    /// The open extension panel for the side panel's middle zone, while no
    /// feature is selected: its title, its pinned header and its body.
    pub(crate) fn render_extension_section(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(SharedString, AnyElement, AnyElement)> {
        if !self.selected.is_empty() {
            return None;
        }
        let open = self.extensions.panel.as_ref()?;
        let busy = self.extensions.busy.contains(&open.extension);
        let theme = cx.theme();
        let (muted, border, foreground) = (theme.muted_foreground, theme.border, theme.foreground);
        let name = self
            .extensions
            .loaded
            .iter()
            .find(|e| e.manifest.id == open.extension)
            .map(|e| e.manifest.name.clone())
            .unwrap_or_else(|| open.extension.clone());

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                div().flex_1().child(
                    Label::new(format!("Extension · {name}"))
                        .text_size(muted_text_size())
                        .text_color(muted),
                ),
            )
            .children(busy.then(|| {
                Label::new("Working…")
                    .text_size(muted_text_size())
                    .text_color(muted)
            }))
            .child(
                Button::new("extension-close-panel")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("Close")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_extension_panel();
                        cx.notify();
                    })),
            );

        let mut body = v_flex().flex_shrink_0().gap_2().px_2().py_2();
        for (i, el) in open.panel.elements.iter().enumerate() {
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
                            .w(gpui::px(64.0))
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
                        let button = Button::new(("extension-button", i * 100 + j))
                            .small()
                            .label(b.label.clone())
                            .disabled(busy || !b.enabled)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.extension_panel_button(id.clone(), cx);
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
                    let link = cx.theme().link;
                    div()
                        .id(("extension-link", i))
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
                Element::Divider => div().h(gpui::px(1.0)).bg(border).into_any_element(),
            };
            body = body.child(row);
        }

        Some((
            SharedString::from(open.panel.title.clone()),
            header.into_any_element(),
            body.into_any_element(),
        ))
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
}
