//! Managing extensions: what is installed (on or off), the extension index,
//! and Extensions > Manage Extensions… (installing from the index, updating,
//! turning off, uninstalling, Install Dev Extension and Rebuild).
//!
//! The index is fetched in the background at startup (failures are quiet
//! until the manager is opened) and again from the manager's Refresh. Updates
//! are never installed by themselves: the status bar says when there are
//! some, and the manager offers them. Installing, updating and rebuilding run
//! in the background, one at a time per extension; the extension is then
//! (re)loaded unless the user turned it off.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{actions, AppContext as _, Context, Window};
use osm_gpui::extensions::manifest::Capabilities;
use osm_gpui::extensions::registry;
use osm_gpui::extensions::runtime::KeychainSecrets;
use osm_gpui::extensions::{self, LoadedExtension, Locales, Manifest, Services};
use osm_gpui::tr;
use osm_gpui::ui::extensions_dialog::{DialogEvent, ExtensionsDialog, IndexState, Model, Row};

use crate::MapViewer;

actions!(osm_gpui, [ManageExtensions, InstallDevExtension]);

/// One installed extension, whether or not it is running.
#[derive(Clone)]
pub(crate) struct Installed {
    pub(crate) dir: PathBuf,
    pub(crate) manifest: Manifest,
    pub(crate) locales: Arc<Locales>,
    /// Its source folder, for a dev extension.
    pub(crate) dev_source: Option<PathBuf>,
}

impl Installed {
    fn read(dir: &Path) -> Option<Installed> {
        Some(Installed {
            manifest: extensions::read_manifest(dir).ok()?,
            locales: Arc::new(Locales::load(dir)),
            dev_source: extensions::dev_source(dir),
            dir: dir.to_path_buf(),
        })
    }
}

/// What `c` lets an extension do, one line each, in the app's language.
fn capability_lines(c: &Capabilities) -> Vec<String> {
    let mut lines = vec![if c.http.is_empty() {
        tr!("dialog.extensions.no_network").to_string()
    } else {
        tr!("dialog.extensions.can_reach", hosts = c.http.join(", ")).to_string()
    }];
    if c.secrets {
        lines.push(tr!("dialog.extensions.can_store_secrets").to_string());
    }
    lines
}

/// What a background install, update or rebuild produced.
type JobResult = Result<LoadedExtension, String>;

impl MapViewer {
    fn extension_services(&self, root: &Path, id: &str) -> Services {
        Services {
            http: self.http.clone(),
            secrets: Arc::new(KeychainSecrets),
            storage: Some(extensions::storage_path(root, id)),
        }
    }

    /// At the first frame: read what is installed, start the ones that are
    /// on, and fetch the index.
    pub(crate) fn start_extensions(&mut self, cx: &mut Context<Self>) {
        let Some(root) = extensions::root_dir() else {
            return;
        };
        self.extensions.disabled = registry::load_state(&root).disabled;
        self.scan_installed(&root);
        let jobs: Vec<(PathBuf, Services)> = self
            .extensions
            .installed
            .iter()
            .filter(|i| !self.extensions.disabled.contains(&i.manifest.id))
            .map(|i| {
                (
                    i.dir.clone(),
                    self.extension_services(&root, &i.manifest.id),
                )
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    jobs.into_iter()
                        .map(|(dir, services)| {
                            LoadedExtension::load(&dir, services)
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
                            this.set_status(tr!("extensions.load_failed", detail = e));
                        }
                    }
                }
                this.refresh_extension_menu(cx);
                this.refresh_extensions_dialog(cx);
                cx.notify();
            });
        })
        .detach();
        self.fetch_extension_index(false, cx);
    }

    fn scan_installed(&mut self, root: &Path) {
        self.extensions.installed = extensions::installed(root)
            .iter()
            .filter_map(|d| Installed::read(d))
            .collect();
    }

    /// Fetch the index in the background. `loud`: say in the status bar when
    /// it fails (the user asked); otherwise only the manager shows it.
    pub(crate) fn fetch_extension_index(&mut self, loud: bool, cx: &mut Context<Self>) {
        self.extensions.index_state = IndexState::Loading;
        self.refresh_extensions_dialog(cx);
        let http = self.http.clone();
        let url = registry::index_url();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { registry::fetch_index(http.as_ref(), &url) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(index) => {
                        this.extensions.index = Some(index);
                        this.extensions.index_state = IndexState::Loaded;
                        let updates = this
                            .extension_rows()
                            .iter()
                            .filter(|r| r.has_update())
                            .count();
                        if updates > 0 {
                            this.set_status(tr!("extensions.updates_available", count = updates));
                        }
                    }
                    Err(e) => {
                        if loud {
                            this.set_status(tr!(
                                "dialog.extensions.index_failed",
                                detail = e.clone()
                            ));
                        }
                        this.extensions.index_state = IndexState::Failed(e);
                    }
                }
                this.refresh_extensions_dialog(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The manager's rows: every installed extension, then what the index
    /// offers that is not installed, by name.
    fn extension_rows(&self) -> Vec<Row> {
        let locale = osm_gpui::i18n::current_locale();
        let index_entry = |id: &str| {
            self.extensions
                .index
                .as_ref()
                .and_then(|i| i.extensions.iter().find(|e| e.id == id))
        };
        let mut rows: Vec<Row> = self
            .extensions
            .installed
            .iter()
            .map(|i| {
                let m = &i.manifest;
                let entry = index_entry(&m.id);
                Row {
                    id: m.id.clone(),
                    name: i
                        .locales
                        .lookup(&locale, "name")
                        .unwrap_or(&m.name)
                        .to_string(),
                    description: i
                        .locales
                        .lookup(&locale, "description")
                        .unwrap_or(&m.description)
                        .to_string(),
                    authors: m.authors.join(", "),
                    installed_version: Some(m.version.clone()),
                    index_version: entry.map(|e| e.version.clone()),
                    capabilities: capability_lines(&m.capabilities),
                    enabled: !self.extensions.disabled.contains(&m.id),
                    dev: i.dev_source.is_some(),
                    compatible: entry.is_none_or(|e| e.is_compatible()),
                    busy: self.extensions.row_busy.contains(&m.id),
                    error: self.extensions.row_errors.get(&m.id).cloned(),
                }
            })
            .collect();
        if let Some(index) = &self.extensions.index {
            for e in &index.extensions {
                if rows.iter().any(|r| r.id == e.id) {
                    continue;
                }
                rows.push(Row {
                    id: e.id.clone(),
                    name: e.name_in(&locale).to_string(),
                    description: e.description_in(&locale).to_string(),
                    authors: e.authors.join(", "),
                    installed_version: None,
                    index_version: Some(e.version.clone()),
                    capabilities: capability_lines(&e.capabilities),
                    enabled: false,
                    dev: false,
                    compatible: e.is_compatible(),
                    busy: self.extensions.row_busy.contains(&e.id),
                    error: self.extensions.row_errors.get(&e.id).cloned(),
                });
            }
        }
        rows.sort_by_key(|r| r.name.to_lowercase());
        rows
    }

    fn extensions_model(&self) -> Model {
        Model {
            rows: self.extension_rows(),
            index: self.extensions.index_state.clone(),
            installing_dev: self.extensions.installing_dev,
        }
    }

    /// Hand the open manager the current state.
    pub(crate) fn refresh_extensions_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.extensions.dialog.clone() {
            let model = self.extensions_model();
            dialog.update(cx, |d, cx| d.set_model(model, cx));
        }
    }

    pub(crate) fn on_manage_extensions(
        &mut self,
        _: &ManageExtensions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.extensions.dialog.is_some() {
            return;
        }
        let model = self.extensions_model();
        let dialog = cx.new(|cx| ExtensionsDialog::new(model, window, cx));
        cx.subscribe(&dialog, |this, _, event: &DialogEvent, cx| {
            this.on_extensions_dialog_event(event.clone(), cx);
        })
        .detach();
        self.extensions.dialog = Some(dialog);
        // A failed or missing index is tried again when the manager opens.
        if !matches!(
            self.extensions.index_state,
            IndexState::Loaded | IndexState::Loading
        ) {
            self.fetch_extension_index(false, cx);
        }
        cx.notify();
    }

    fn on_extensions_dialog_event(&mut self, event: DialogEvent, cx: &mut Context<Self>) {
        match event {
            DialogEvent::Closed => self.extensions.dialog = None,
            DialogEvent::Install(id) | DialogEvent::Update(id) => self.install_from_index(&id, cx),
            DialogEvent::Uninstall(id) => self.uninstall_extension(&id, cx),
            DialogEvent::SetEnabled(id, on) => self.set_extension_enabled(&id, on, cx),
            DialogEvent::Rebuild(id) => self.rebuild_dev_extension(&id, cx),
            DialogEvent::InstallDev => self.install_dev_extension(cx),
            DialogEvent::Refresh => self.fetch_extension_index(true, cx),
        }
        self.refresh_extensions_dialog(cx);
        cx.notify();
    }

    /// Run `job` (which installs extension `id` and loads it) in the
    /// background, then put the result in place.
    fn run_extension_job(
        &mut self,
        id: &str,
        job: impl FnOnce() -> JobResult + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if !self.extensions.row_busy.insert(id.to_string()) {
            return;
        }
        self.extensions.row_errors.remove(id);
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update(cx, |this, cx| {
                this.extensions.row_busy.remove(&id);
                this.finish_extension_job(&id, result, cx);
            });
        })
        .detach();
    }

    fn finish_extension_job(&mut self, id: &str, result: JobResult, cx: &mut Context<Self>) {
        self.extensions.installing_dev = false;
        let Some(root) = extensions::root_dir() else {
            return;
        };
        match result {
            Ok(ext) => {
                let (name, version) = (
                    ext.display_name(&osm_gpui::i18n::current_locale()),
                    ext.manifest.version.clone(),
                );
                // The old copy's markers and panels go with it.
                self.drop_extension_ui(id);
                self.extensions.loaded.retain(|e| e.manifest.id != id);
                if !self.extensions.disabled.contains(id) {
                    self.add_loaded_extension(ext);
                }
                self.scan_installed(&root);
                self.refresh_extension_menu(cx);
                self.set_status(tr!("extensions.installed", name = name, version = version));
            }
            Err(e) => {
                eprintln!("extension {id}: {e}");
                let first = e.lines().next().unwrap_or("").to_string();
                self.set_status(tr!("extensions.install_failed", detail = first.clone()));
                self.extensions.row_errors.insert(id.to_string(), first);
            }
        }
        self.refresh_extensions_dialog(cx);
        cx.notify();
    }

    /// Install (or update to) the index's version of `id`.
    fn install_from_index(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(root) = extensions::root_dir() else {
            return;
        };
        let Some(entry) = self
            .extensions
            .index
            .as_ref()
            .and_then(|i| i.extensions.iter().find(|e| e.id == id))
            .cloned()
        else {
            return;
        };
        let http = self.http.clone();
        let services = self.extension_services(&root, id);
        self.run_extension_job(
            id,
            move || {
                let dir = registry::download_and_install(http.as_ref(), &root, &entry)?;
                LoadedExtension::load(&dir, services)
            },
            cx,
        );
    }

    fn uninstall_extension(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(root) = extensions::root_dir() else {
            return;
        };
        let name = self.extension_name(id);
        match extensions::uninstall(&root, id) {
            Ok(()) => {
                self.drop_extension_ui(id);
                self.extensions.loaded.retain(|e| e.manifest.id != id);
                if self.extensions.disabled.remove(id) {
                    let _ = registry::save_state(
                        &root,
                        &registry::State {
                            disabled: self.extensions.disabled.clone(),
                        },
                    );
                }
                self.scan_installed(&root);
                self.refresh_extension_menu(cx);
                self.set_status(tr!("extensions.uninstalled", name = name));
            }
            Err(e) => self.set_status(tr!("extensions.uninstall_failed", name = name, detail = e)),
        }
    }

    fn set_extension_enabled(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        let Some(root) = extensions::root_dir() else {
            return;
        };
        let changed = if on {
            self.extensions.disabled.remove(id)
        } else {
            self.extensions.disabled.insert(id.to_string())
        };
        if !changed {
            return;
        }
        if let Err(e) = registry::save_state(
            &root,
            &registry::State {
                disabled: self.extensions.disabled.clone(),
            },
        ) {
            eprintln!("saving extension state: {e}");
        }
        if on {
            let Some(dir) = self
                .extensions
                .installed
                .iter()
                .find(|i| i.manifest.id == id)
                .map(|i| i.dir.clone())
            else {
                return;
            };
            let services = self.extension_services(&root, id);
            self.run_extension_job(id, move || LoadedExtension::load(&dir, services), cx);
        } else {
            self.drop_extension_ui(id);
            self.extensions.loaded.retain(|e| e.manifest.id != id);
            self.refresh_extension_menu(cx);
        }
    }

    /// Build the dev extension in `src` and install and load it.
    fn dev_job(&self, src: PathBuf) -> Option<impl FnOnce() -> JobResult + Send + 'static> {
        let root = extensions::root_dir()?;
        let http = self.http.clone();
        Some(move || {
            let manifest = extensions::read_manifest(&src)?;
            // A prebuilt extension.wasm next to the manifest is used as is;
            // otherwise the crate is built.
            let prebuilt = src.join("extension.wasm");
            let wasm = if prebuilt.is_file() {
                prebuilt
            } else {
                extensions::build_dev_extension(&src)?
            };
            let dir = extensions::install_dev(&root, &src, &wasm)?;
            LoadedExtension::load(
                &dir,
                Services {
                    http,
                    secrets: Arc::new(KeychainSecrets),
                    storage: Some(extensions::storage_path(&root, &manifest.id)),
                },
            )
        })
    }

    fn rebuild_dev_extension(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(src) = self
            .extensions
            .installed
            .iter()
            .find(|i| i.manifest.id == id)
            .and_then(|i| i.dev_source.clone())
        else {
            return;
        };
        if let Some(job) = self.dev_job(src) {
            self.run_extension_job(id, job, cx);
        }
    }

    pub(crate) fn on_install_dev_extension(
        &mut self,
        _: &InstallDevExtension,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.install_dev_extension(cx);
    }

    /// Install Dev Extension…: pick a folder, build it, install it.
    fn install_dev_extension(&mut self, cx: &mut Context<Self>) {
        if self.extensions.installing_dev {
            return;
        }
        let picked = rfd::AsyncFileDialog::new()
            .set_title(tr!("extensions.pick_folder").to_string())
            .pick_folder();
        cx.spawn(async move |this, cx| {
            let Some(folder) = picked.await else {
                return;
            };
            let src = folder.path().to_path_buf();
            let _ = this.update(cx, |this, cx| {
                let id = match extensions::read_manifest(&src) {
                    Ok(m) => m.id,
                    Err(e) => {
                        this.set_status(tr!("extensions.install_failed", detail = e));
                        cx.notify();
                        return;
                    }
                };
                this.set_status(tr!("extensions.building", path = src.display().to_string()));
                this.extensions.installing_dev = true;
                if let Some(job) = this.dev_job(src) {
                    this.run_extension_job(&id, job, cx);
                }
                this.refresh_extensions_dialog(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
