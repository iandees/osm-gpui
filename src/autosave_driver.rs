//! The running app's side of autosave and crash recovery: the timer that
//! writes layers with unsaved edits (see `osm_gpui::autosave` for the
//! store, the file layout and the guarantees), the startup offer to restore
//! what a crashed run left behind, and the hooks for clean exit.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gpui::{AppContext as _, Context, Window};

use osm_gpui::autosave::{
    self, CrashedSession, RecoverError, RecoverableLayer, Session, Snapshot, Tracker, UploadMarker,
};
use osm_gpui::layers::{osm_layer::OsmLayer, LayerId, MapLayer};
use osm_gpui::osm::{FileEdits, OsmData};
use osm_gpui::script::AutosaveCmd;
use osm_gpui::settings_store;
use osm_gpui::ui::restore_dialog::{
    interrupted_uploads_lines, DialogEvent, RestoreDialog, RestoreItem,
};
use osm_gpui::upload_chunks::MarkerAfter;
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
    Vec<(RecoverableLayer, Result<(OsmData, FileEdits), RecoverError>)>,
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
    /// The upload marker this run last made durable (see `UploadMarker`).
    marker: Option<UploadMarker>,
    /// The marker may go as soon as every layer's autosave is on disk.
    marker_release_pending: bool,
    /// Layers restored from a session that died mid-upload, until the user
    /// uploads again.
    restored_upload: Option<RestoredUpload>,
}

/// The upload markers of the restored sessions (oldest first) and the layers
/// they apply to.
pub(crate) struct RestoredUpload {
    pub(crate) markers: Vec<UploadMarker>,
    pub(crate) layers: Vec<LayerId>,
}

/// A marker to make durable, to be run on a background thread before the
/// request it describes is sent.
pub(crate) struct MarkerWrite {
    session: Arc<Session>,
    marker: UploadMarker,
}

impl MarkerWrite {
    pub(crate) fn run(&self) {
        if let Err(e) = self.session.write_upload_marker(&self.marker) {
            eprintln!("autosave: could not record the upload in progress: {e}");
        }
    }

    /// The server just opened the changeset: rewrite the marker with its id,
    /// before the request that may be lost is answered. Plain file writes
    /// behind an `Arc<Session>`, so it runs on the upload's worker thread,
    /// where the phase callback fires.
    pub(crate) fn record_changeset(&self, changeset_id: u64) {
        MarkerWrite {
            session: self.session.clone(),
            marker: self.marker.clone().with_changeset(changeset_id),
        }
        .run();
    }

    /// Feed it each phase the upload reports.
    pub(crate) fn observe(&self, phase: &osm_gpui::transfer::TransferPhase) {
        if let osm_gpui::transfer::TransferPhase::UploadingChanges { changeset_id, .. } = phase {
            self.record_changeset(*changeset_id);
        }
    }
}

impl AutosaveState {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            marker: None,
            marker_release_pending: false,
            restored_upload: None,
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

    /// This run's session directory, once it exists.
    pub(crate) fn session_dir(&self) -> Option<PathBuf> {
        self.session.as_ref().map(|s| s.dir().to_path_buf())
    }
}

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
        for layer in self
            .layer_manager
            .layers()
            .iter()
            .filter_map(|l| l.as_osm())
        {
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

    /// Record, durably and before the first request of a changeset is sent,
    /// that an upload is in flight. Returns the write for the caller to run
    /// on a background thread ahead of the request; `None` where autosave is
    /// off (scripted runs and tests without a directory, or switched off in
    /// Settings). Starting an upload is also the user acting on a restored
    /// session's warning, so that warning ends here.
    pub(crate) fn upload_marker_prepare(&mut self, marker: UploadMarker) -> Option<MarkerWrite> {
        let state = self.autosave.as_mut()?;
        if state.broken || !settings_store::snapshot().autosave {
            return None;
        }
        state.restored_upload = None;
        state.marker_release_pending = false;
        state.marker = Some(marker.clone());
        let session = self.autosave_session()?;
        Some(MarkerWrite { session, marker })
    }

    /// The driver has an answer for the request the marker was written for
    /// (see `upload_chunks::marker_after`).
    pub(crate) fn upload_marker_after(&mut self, after: MarkerAfter, cx: &mut Context<Self>) {
        let Some(state) = self.autosave.as_mut() else {
            return;
        };
        match after {
            MarkerAfter::Release => {
                state.marker_release_pending = state.marker.is_some();
                self.release_upload_marker_if_settled();
            }
            MarkerAfter::Keep { changeset_id } => {
                state.marker_release_pending = false;
                let (Some(id), Some(marker)) = (changeset_id, state.marker.take()) else {
                    return;
                };
                let marker = marker.with_changeset(id);
                state.marker = Some(marker.clone());
                if let Some(session) = state.session.clone() {
                    cx.background_executor()
                        .spawn(async move { MarkerWrite { session, marker }.run() })
                        .detach();
                }
            }
        }
    }

    /// Remove the marker once nothing the server accepted is still listed as
    /// pending in an autosave on disk: every layer's write has landed.
    fn release_upload_marker_if_settled(&mut self) {
        let Some(state) = self.autosave.as_mut() else {
            return;
        };
        if !state.marker_release_pending {
            return;
        }
        let settled = self
            .layer_manager
            .layers()
            .iter()
            .filter_map(|l| l.as_osm())
            .all(|l| {
                state
                    .tracker
                    .is_settled(l.id().0, l.edit_revision(), l.is_modified())
            });
        if !settled {
            return;
        }
        state.marker_release_pending = false;
        state.marker = None;
        if let Some(session) = &state.session {
            session.clear_upload_marker();
        }
    }

    /// The warning to put in front of the upload dialog: the session these
    /// layers were restored from died mid-upload and none of them has been
    /// uploaded since.
    pub(crate) fn restored_upload_warning(&self) -> Option<String> {
        let restored = self.autosave.as_ref()?.restored_upload.as_ref()?;
        let still_pending = restored.layers.iter().any(|id| {
            self.layer_manager
                .find_layer(*id)
                .and_then(|l| l.as_osm())
                .is_some_and(|l| l.is_modified())
        });
        still_pending.then(|| {
            osm_gpui::ui::restore_dialog::interrupted_uploads_lines(&restored.markers, now_unix())
                .join("\n")
        })
    }

    /// The pending objects that were in the changeset(s) a restored session
    /// was sending, while the warning stands.
    pub(crate) fn in_flight_features(&self) -> Vec<osm_gpui::selection::FeatureRef> {
        let Some(restored) = self
            .autosave
            .as_ref()
            .and_then(|state| state.restored_upload.as_ref())
        else {
            return Vec::new();
        };
        let diffs: Vec<(LayerId, osm_gpui::layers::diff::LayerDiff)> = restored
            .layers
            .iter()
            .filter_map(|id| {
                let layer = self.layer_manager.find_layer(*id)?.as_osm()?;
                Some((*id, layer.diff_for_upload()))
            })
            .collect();
        let refs: Vec<(LayerId, &osm_gpui::layers::diff::LayerDiff)> =
            diffs.iter().map(|(id, diff)| (*id, diff)).collect();
        autosave::in_flight_features(&restored.markers, &refs)
    }

    /// The upload dialog's "select the objects that were being uploaded":
    /// select them, show them, and close the dialog.
    pub(crate) fn select_in_flight(&mut self, cx: &mut Context<Self>) {
        let features = self.in_flight_features();
        if features.is_empty() {
            return;
        }
        self.close_upload_dialog_keeping_draft(cx);
        self.set_selection(features.clone());
        self.fit_to_features(&features);
        self.set_status(tr_count!("status.selected_in_flight", features.len()));
        cx.notify();
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
        self.release_upload_marker_if_settled();
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
        if self.dialogs.restore_dialog.is_some() {
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

        let now = now_unix();
        let notices: Vec<Vec<String>> = sessions
            .iter()
            .filter(|s| !s.upload_markers.is_empty())
            .map(|s| interrupted_uploads_lines(&s.upload_markers, now))
            .collect();
        let dialog = cx.new(|cx| RestoreDialog::new(items, window, cx));
        if !notices.is_empty() {
            dialog.update(cx, |dialog, cx| dialog.set_upload_notices(notices, cx));
        }
        cx.subscribe(&dialog, |this, _entity, event: &DialogEvent, cx| {
            this.dialogs.restore_dialog = None;
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
        self.dialogs.restore_dialog = Some(dialog);
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

        // The warnings travel with the layers: every crashed session's markers
        // (and any this run already holds) are written into this run's
        // session before any layer is moved, so that at every moment the
        // files that list the edits also say an upload may have taken them.
        let mut all_markers: Vec<UploadMarker> = loaded
            .iter()
            .flat_map(|(crashed, _)| crashed.upload_markers.iter().cloned())
            .collect();
        let carried = match &session {
            Some(session) if !all_markers.is_empty() => {
                all_markers.extend(session.upload_markers());
                session.write_upload_markers(&all_markers).is_ok()
            }
            _ => true,
        };
        let mut interrupted_markers: Vec<UploadMarker> = Vec::new();
        let mut interrupted_layers: Vec<LayerId> = Vec::new();

        for (crashed, layers) in loaded {
            let mut from_this_session: Vec<LayerId> = Vec::new();
            for (rec, result) in layers {
                let (data, edits) = match result {
                    Ok(loaded) => loaded,
                    Err(e) => {
                        // Its files stay where they are.
                        self.set_status(tr!(
                            "status.restore_failed",
                            layer = rec.manifest.layer_name,
                            error = e.to_string()
                        ));
                        continue;
                    }
                };
                let data = Arc::new(data);
                let id = self.layer_manager.alloc_id();
                let name = self.layer_manager.unique_name(&rec.manifest.layer_name);
                let layer = OsmLayer::new_with_edits_styled(
                    id,
                    name,
                    data.clone(),
                    &edits,
                    osm_gpui::style::custom::active_stylesheet(),
                );
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
                } && carried;
                from_this_session.push(id);
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
            if !crashed.upload_markers.is_empty() && !from_this_session.is_empty() {
                interrupted_markers.extend(crashed.upload_markers.iter().cloned());
                interrupted_layers.extend(from_this_session);
            }
            crashed.discard_if_empty();
        }
        interrupted_markers.sort_by_key(|m| m.started_at);
        let warn_upload = interrupted_markers.len();
        if !interrupted_markers.is_empty() {
            if let Some(state) = self.autosave.as_mut() {
                state.marker = interrupted_markers.last().cloned();
                state.restored_upload = Some(RestoredUpload {
                    markers: interrupted_markers,
                    layers: interrupted_layers,
                });
            }
        }

        if let Some(data) = fit {
            self.fit_to_osm_data(&data);
            self.first_dataset_fitted = true;
        }
        if restored > 0 {
            self.data_generation += 1;
            if warn_upload > 1 {
                self.set_status(tr!(
                    "status.restored_uploads_interrupted",
                    count = warn_upload
                ));
            } else if warn_upload == 1 {
                self.set_status(tr!("status.restored_upload_interrupted"));
            } else {
                self.set_status(tr_count!("status.restored_autosave", restored));
            }
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
                let Some(dialog) = self.dialogs.restore_dialog.clone() else {
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
        for layer in self
            .layer_manager
            .layers()
            .iter()
            .filter_map(|l| l.as_osm())
        {
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
