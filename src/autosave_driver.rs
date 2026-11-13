//! The running app's side of autosave and crash recovery: the timer that
//! writes layers with unsaved edits (see `osm_gpui::autosave` for the
//! store, the file layout and the guarantees), the startup offer to restore
//! what a crashed run left behind, and the hooks for clean exit.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gpui::{AppContext as _, Context, Window};

use osm_gpui::autosave::{self, CrashedSession, RecoverableLayer, Session, Snapshot, Tracker};
use osm_gpui::layers::{osm_layer::OsmLayer, LayerId};
use osm_gpui::osm::{FileEdits, OsmData};
use osm_gpui::script::AutosaveCmd;
use osm_gpui::settings_store;
use osm_gpui::ui::restore_dialog::{DialogEvent, RestoreDialog, RestoreItem};
use osm_gpui::{tr, tr_count};

use crate::MapViewer;

/// This run's session, once it exists (it is created when the first layer
/// needs writing), for the exit hooks that have no `MapViewer` to ask.
static SESSION: OnceLock<Arc<Session>> = OnceLock::new();

/// The clean exit: delete this run's autosaves. Called when the app quits
/// normally (the user confirmed discarding any unsaved edits, or there were
/// none) and when a scripted run ends; never on a crash, which is how the
/// next start knows.
pub(crate) fn finish_clean() {
    if let Some(session) = SESSION.get() {
        session.finish_clean();
    }
}

/// Where a layer's data came from, as far as the app knows. Layers carry
/// no such identity themselves (the app uploads whatever it holds to
/// whichever server is selected when the user uploads), so this record
/// exists for the autosave manifest and the restore dialog's warning.
#[derive(Debug, Clone, Default)]
pub(crate) struct LayerOrigin {
    /// The API server a download came from; `None` for a layer loaded from a
    /// file (a `.osm` file does not say).
    pub api_server: Option<String>,
    /// The file the layer was opened from.
    pub source_path: Option<String>,
}

type Loaded = Vec<(
    CrashedSession,
    Vec<(RecoverableLayer, Result<(OsmData, FileEdits), String>)>,
)>;

pub(crate) struct AutosaveState {
    root: PathBuf,
    session: Option<Arc<Session>>,
    tracker: Tracker,
    origins: HashMap<LayerId, LayerOrigin>,
    /// The session directory could not be created; stop trying.
    broken: bool,
    /// Autosave was on at the last tick.
    active: bool,
    /// Crashed sessions found at startup, waiting for the user's answer.
    offer: Option<Vec<CrashedSession>>,
    offer_shown: bool,
    /// Background restores or discards running.
    busy: usize,
}

impl AutosaveState {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            session: None,
            tracker: Tracker::default(),
            origins: HashMap::new(),
            broken: false,
            active: false,
            offer: None,
            offer_shown: false,
            busy: 0,
        }
    }
}

#[cfg(test)]
impl AutosaveState {
    /// What is recorded about where layer `id` came from.
    pub(crate) fn origin_of(&self, id: LayerId) -> LayerOrigin {
        self.origins.get(&id).cloned().unwrap_or_default()
    }
}

fn same_server(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

impl MapViewer {
    /// Turn autosave on for this run, rooted at `root` (`None`: off). Looks
    /// for what a crashed run left behind, in the background, and starts
    /// the timer.
    pub(crate) fn init_autosave(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(root) = root else {
            return;
        };
        self.autosave = Some(AutosaveState::new(root.clone()));

        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { autosave::scan_crashed(&root, None) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if found.other_format > 0 {
                    this.set_status(tr!("status.autosave_newer_version"));
                    cx.notify();
                }
                if let Some(state) = this.autosave.as_mut() {
                    if !found.sessions.is_empty() {
                        state.offer = Some(found.sessions);
                        cx.notify();
                    }
                }
            });
        })
        .detach();

        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(autosave::TICK).await;
            if this.update(cx, |this, cx| this.autosave_tick(cx)).is_err() {
                break;
            }
        })
        .detach();
    }

    /// Record where a new layer's data came from.
    pub(crate) fn note_layer_origin(&mut self, id: LayerId, origin: LayerOrigin) {
        if let Some(state) = self.autosave.as_mut() {
            state.origins.insert(id, origin);
        }
    }

    /// The most recently added layer was opened from `path`.
    pub(crate) fn note_last_layer_source(&mut self, path: &std::path::Path) {
        let Some(id) = self.layer_manager.layers().last().map(|l| l.id()) else {
            return;
        };
        self.note_layer_origin(
            id,
            LayerOrigin {
                api_server: None,
                source_path: Some(path.to_string_lossy().into_owned()),
            },
        );
    }

    /// The session, created on first use.
    fn autosave_session(&mut self) -> Option<Arc<Session>> {
        let state = self.autosave.as_mut()?;
        if state.session.is_none() && !state.broken {
            match Session::start(&state.root) {
                Ok(session) => {
                    let session = Arc::new(session);
                    let _ = SESSION.set(session.clone());
                    state.session = Some(session);
                }
                Err(e) => {
                    eprintln!(
                        "autosave: cannot use {}: {e}; autosave is off for this run",
                        state.root.display()
                    );
                    state.broken = true;
                }
            }
        }
        state.session.clone()
    }

    /// Once a second: write whichever layers are due (see `Tracker`). Cheap
    /// when nothing changed: a revision comparison per layer. The writes
    /// themselves, diff and serialization included, run on the background
    /// executor from a snapshot that shares the layer's data.
    pub(crate) fn autosave_tick(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.autosave.as_mut() else {
            return;
        };
        if state.broken {
            return;
        }
        if !settings_store::snapshot().autosave {
            // Switched off: this run's autosaves go, and nothing new is
            // written.
            if state.active {
                state.active = false;
                state.tracker.reset();
                if let Some(session) = state.session.clone() {
                    cx.background_executor()
                        .spawn(async move { session.clear() })
                        .detach();
                }
            }
            return;
        }
        state.active = true;
        if let Some(session) = &state.session {
            session.heartbeat(false);
        }

        let now = Instant::now();
        let mut jobs: Vec<(u64, u64, Snapshot)> = Vec::new();
        for layer in self.layer_manager.layers() {
            let id = layer.id();
            let revision = layer.edit_revision();
            if !state.tracker.poll(now, id.0, revision, layer.is_modified()) {
                continue;
            }
            let Some((current, original)) = layer.autosave_snapshot() else {
                continue;
            };
            let origin = state.origins.get(&id).cloned().unwrap_or_default();
            jobs.push((
                id.0,
                revision,
                Snapshot {
                    layer_id: id.0,
                    layer_name: layer.name().to_string(),
                    source_path: origin.source_path,
                    api_server: origin.api_server,
                    current,
                    original,
                },
            ));
        }
        if jobs.is_empty() {
            return;
        }
        let Some(session) = self.autosave_session() else {
            return;
        };
        for (id, revision, snapshot) in jobs {
            if let Some(state) = self.autosave.as_mut() {
                state.tracker.started(id);
            }
            let session = session.clone();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { session.write(&snapshot) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.autosave_finished(id, revision, result, cx)
                });
            })
            .detach();
        }
    }

    fn autosave_finished(
        &mut self,
        id: u64,
        revision: u64,
        result: std::io::Result<autosave::WriteOutcome>,
        cx: &mut Context<Self>,
    ) {
        if let Err(e) = &result {
            eprintln!("autosave: writing layer {id} failed: {e}");
        }
        let gone = self.layer_manager.find_layer(LayerId(id)).is_none();
        let Some(state) = self.autosave.as_mut() else {
            return;
        };
        state.tracker.finished(id, revision, result.ok());
        if gone {
            // Deleted while its write ran: the write may have put the files
            // back.
            state.tracker.forget(id);
            if let Some(session) = state.session.clone() {
                cx.background_executor()
                    .spawn(async move { session.remove_layer(id) })
                    .detach();
            }
        }
    }

    /// Write every layer's autosave at the next opportunity (now), whatever
    /// the timers say. Used right after a changeset is applied locally, so
    /// the autosave never lags behind what the server has accepted.
    pub(crate) fn autosave_now(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.autosave.as_mut() else {
            return;
        };
        for layer in self.layer_manager.layers() {
            state.tracker.force(layer.id().0);
        }
        self.autosave_tick(cx);
    }

    /// A layer is gone for good: its autosave goes too.
    pub(crate) fn autosave_forget_layer(&mut self, id: LayerId, cx: &mut Context<Self>) {
        let Some(state) = self.autosave.as_mut() else {
            return;
        };
        state.origins.remove(&id);
        let in_flight = state.tracker.in_flight(id.0);
        if in_flight {
            // Its write is running; `autosave_finished` cleans up after it.
            return;
        }
        state.tracker.forget(id.0);
        if let Some(session) = state.session.clone() {
            cx.background_executor()
                .spawn(async move { session.remove_layer(id.0) })
                .detach();
        }
    }

    /// Show the restore offer once the window can render it.
    pub(crate) fn check_for_pending_restore(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restore_dialog.is_some() {
            return;
        }
        let Some(state) = self.autosave.as_mut() else {
            return;
        };
        let Some(sessions) = state.offer.as_ref() else {
            return;
        };
        if state.offer_shown {
            return;
        }
        state.offer_shown = true;
        let current = settings_store::api_base_url();
        let items: Vec<RestoreItem> = sessions
            .iter()
            .flat_map(|s| s.layers.iter())
            .map(|rec| RestoreItem {
                layer_name: rec.manifest.layer_name.clone(),
                edits: rec.manifest.edit_count,
                saved_at: rec.manifest.saved_at,
                other_server: rec
                    .manifest
                    .api_server
                    .as_ref()
                    .filter(|from| !same_server(from, &current))
                    .map(|from| (from.clone(), current.clone())),
            })
            .collect();

        let dialog = cx.new(|cx| RestoreDialog::new(items, window, cx));
        cx.subscribe(&dialog, |this, _entity, event: &DialogEvent, cx| {
            this.restore_dialog = None;
            let sessions = this
                .autosave
                .as_mut()
                .and_then(|state| {
                    state.offer_shown = false;
                    state.offer.take()
                })
                .unwrap_or_default();
            match event {
                // Keep the files; they are offered again at the next start.
                // Dropping the sessions releases their locks.
                DialogEvent::Later => drop(sessions),
                DialogEvent::Discard => this.discard_autosaved(sessions, cx),
                DialogEvent::Restore => this.restore_autosaved(sessions, cx),
            }
            cx.notify();
        })
        .detach();
        self.restore_dialog = Some(dialog);
        cx.notify();
    }

    fn discard_autosaved(&mut self, sessions: Vec<CrashedSession>, cx: &mut Context<Self>) {
        let Some(state) = self.autosave.as_mut() else {
            return;
        };
        state.busy += 1;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async move {
                    for session in sessions {
                        session.discard();
                    }
                })
                .await;
            let _ = this.update(cx, |this, _| {
                if let Some(state) = this.autosave.as_mut() {
                    state.busy -= 1;
                }
            });
        })
        .detach();
    }

    fn restore_autosaved(&mut self, sessions: Vec<CrashedSession>, cx: &mut Context<Self>) {
        let Some(state) = self.autosave.as_mut() else {
            return;
        };
        state.busy += 1;
        cx.spawn(async move |this, cx| {
            let loaded: Loaded = cx
                .background_executor()
                .spawn(async move {
                    sessions
                        .into_iter()
                        .map(|session| {
                            let layers = session
                                .layers
                                .iter()
                                .map(|rec| (rec.clone(), rec.load()))
                                .collect();
                            (session, layers)
                        })
                        .collect()
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_restore(loaded, cx));
        })
        .detach();
    }

    /// Put the loaded layers on the map with their edits pending, move their
    /// autosave files into this run's session, and only then let go of the
    /// crashed session's copy.
    fn finish_restore(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        // With autosave switched off nothing is kept for the restored
        // layers either, as for any other layer.
        let keep = settings_store::snapshot().autosave;
        let session = if keep { self.autosave_session() } else { None };
        let mut restored = 0usize;
        let mut fit: Option<Arc<OsmData>> = None;

        for (crashed, layers) in loaded {
            for (rec, result) in layers {
                let (data, edits) = match result {
                    Ok(loaded) => loaded,
                    Err(e) => {
                        // Its files stay where they are.
                        self.set_status(tr!(
                            "status.restore_failed",
                            layer = rec.manifest.layer_name,
                            error = e
                        ));
                        continue;
                    }
                };
                let data = Arc::new(data);
                let id = self.layer_manager.alloc_id();
                let name = self.layer_manager.unique_name(&rec.manifest.layer_name);
                let layer = OsmLayer::new_with_edits(id, name, data.clone(), &edits);
                let revision = layer.edit_revision();
                self.layer_manager.add_layer(Box::new(layer));
                if self.active_layer.is_none() {
                    self.active_layer = Some(id);
                }

                let safe = match &session {
                    Some(session) => match session.adopt(&rec, id.0) {
                        Ok(()) => true,
                        Err(e) => {
                            eprintln!(
                                "autosave: could not carry {} over: {e}",
                                rec.manifest.layer_name
                            );
                            false
                        }
                    },
                    None => !keep,
                };
                if let Some(state) = self.autosave.as_mut() {
                    state.origins.insert(
                        id,
                        LayerOrigin {
                            api_server: rec.manifest.api_server.clone(),
                            source_path: rec.manifest.source_path.clone(),
                        },
                    );
                    if session.is_some() && safe {
                        state.tracker.mark_saved(id.0, revision);
                    }
                }
                if safe {
                    crashed.remove_layer(&rec);
                }
                fit.get_or_insert(data);
                restored += 1;
            }
            crashed.discard_if_empty();
        }

        if let Some(data) = fit {
            self.fit_to_osm_data(&data);
            self.first_dataset_fitted = true;
        }
        if restored > 0 {
            self.data_generation += 1;
            self.set_status(tr_count!("status.restored_autosave", restored));
        }
        if let Some(state) = self.autosave.as_mut() {
            state.busy = state.busy.saturating_sub(1);
        }
        cx.notify();
    }

    /// Record the API server a freshly downloaded layer came from.
    pub(crate) fn note_download_origin(&mut self, id: LayerId) {
        self.note_layer_origin(
            id,
            LayerOrigin {
                api_server: Some(settings_store::api_base_url()),
                source_path: None,
            },
        );
    }

    /// The `autosave_now`, `restore_autosave`, `discard_autosave` script ops.
    pub(crate) fn script_autosave(
        &mut self,
        cmd: AutosaveCmd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.autosave.is_none() {
            return Err(
                "autosave is not enabled for this run (scripted runs need --autosave-dir PATH)"
                    .into(),
            );
        }
        match cmd {
            AutosaveCmd::Now => {
                self.autosave_now(cx);
                Ok(())
            }
            AutosaveCmd::Restore | AutosaveCmd::Discard => {
                let Some(dialog) = self.restore_dialog.clone() else {
                    return Err("the restore dialog is not open".into());
                };
                // Deferred like the other dialogs' button presses: the event
                // is handled by this view, which is being rendered right now.
                window.defer(cx, move |_window, cx| {
                    dialog.update(cx, |dialog, cx| {
                        if cmd == AutosaveCmd::Restore {
                            dialog.press_restore(cx)
                        } else {
                            dialog.press_discard(cx)
                        }
                    });
                });
                Ok(())
            }
            // Handled on the runner thread (it must not wait for a frame).
            AutosaveCmd::Crash => Err("crash is handled by the runner".into()),
        }
    }

    /// `Check::AutosaveIdle`.
    pub(crate) fn script_autosave_idle(&self) -> Result<(), String> {
        let Some(state) = &self.autosave else {
            return Err("autosave is not enabled for this run".into());
        };
        if state.busy > 0 {
            return Err("a restore or discard is still running".into());
        }
        for layer in self.layer_manager.layers() {
            if !state
                .tracker
                .is_settled(layer.id().0, layer.edit_revision(), layer.is_modified())
            {
                return Err(format!(
                    "layer \"{}\" has an autosave write pending",
                    layer.name()
                ));
            }
        }
        Ok(())
    }

    /// `Check::AutosaveFiles`.
    pub(crate) fn script_autosave_files(&self, want: Option<usize>) -> Result<(), String> {
        let Some(state) = &self.autosave else {
            return Err("autosave is not enabled for this run".into());
        };
        let found = state.session.as_ref().map_or(0, |s| s.layer_count());
        match want {
            None if found > 0 => Ok(()),
            None => Err("no layer has an autosave file".into()),
            Some(n) if found == n => Ok(()),
            Some(n) => Err(format!("expected {n} autosaved layers, found {found}")),
        }
    }
}
