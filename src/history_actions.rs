//! The app side of the object history view: opening it for the selected
//! object, fetching the history in the background, remembering who last
//! edited what, and "Revert tags to this version".
//!
//! "Object history" is the server's versions of one object. The side panel's
//! "History" section is the undo history, a different thing.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use gpui::{actions, AppContext as _, Context, Entity, Subscription, Window};
use osm_gpui::auth;
use osm_gpui::object_history::{
    parse_response, tag_revert_edits, HistoryError, LastEdit, ObjectHistory, ObjectState,
};
use osm_gpui::selection::{FeatureKind, FeatureRef};
use osm_gpui::settings_store;
use osm_gpui::tr;
use osm_gpui::ui::object_history_view::{LoadState, ObjectHistoryView, ViewEvent};

use crate::undo::UndoableAction;
use crate::MapViewer;

actions!(osm_gpui, [ShowObjectHistory]);

/// Everything the object history feature keeps on the main view.
#[derive(Default)]
pub(crate) struct HistoryState {
    /// The open view, if any.
    pub view: Option<Entity<ObjectHistoryView>>,
    /// The object the view is about.
    feature: Option<FeatureRef>,
    /// Set to abandon the running request.
    cancel: Option<Arc<AtomicBool>>,
    /// Identifies the current request, so a late answer to an older one is
    /// ignored.
    request: u64,
    /// Script runs only: answer requests from this file instead of the
    /// server.
    fixture: Option<PathBuf>,
    /// What the server said about the last edits of objects whose history
    /// was fetched, so the Selection header can show them without another
    /// request. Per version, since the local copy may be an older one.
    last_edits: HashMap<(FeatureKind, i64), Vec<LastEdit>>,
    /// The API server `last_edits` came from.
    last_edits_server: String,
    _subscription: Option<Subscription>,
}

impl MapViewer {
    /// Edit > Show Object History.
    pub(crate) fn on_show_object_history(
        &mut self,
        _: &ShowObjectHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(why) = self.show_object_history(cx) {
            self.set_status(why);
            cx.notify();
        }
    }

    /// Why Show Object History cannot run on the current selection, if it
    /// cannot (nothing or several selected, or a new object).
    pub(crate) fn object_history_unavailable(&self) -> Option<String> {
        match self.selected.as_slice() {
            [feat] if feat.id < 0 => Some(tr!("object_history.new_object").to_string()),
            [_] => None,
            _ => Some(tr!("object_history.select_one").to_string()),
        }
    }

    /// Open the view for the selected object and start loading its history.
    pub(crate) fn show_object_history(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if let Some(why) = self.object_history_unavailable() {
            return Err(why);
        }
        let feat = self.selected[0];
        let has_it = self
            .layer_manager
            .find_layer(feat.layer_id)
            .and_then(|layer| layer.as_editable())
            .is_some_and(|editable| editable.has_feature(feat.kind, feat.id));
        if !has_it {
            return Err(tr!("object_history.select_one").to_string());
        }

        self.close_object_history(cx);
        let api = settings_store::api_base_url();
        let web = auth::oauth_base_for(&api);
        let view = cx.new(|cx| ObjectHistoryView::new(feat.kind, feat.id, web, api, cx));
        let subscription =
            cx.subscribe(
                &view,
                |this: &mut Self, _view, event: &ViewEvent, cx| match event {
                    ViewEvent::Close => this.close_object_history(cx),
                    ViewEvent::Retry => this.start_history_request(cx),
                    ViewEvent::RevertTags { version, tags } => {
                        this.revert_tags_to_version(*version, tags, cx)
                    }
                },
            );
        self.object_history.view = Some(view);
        self.object_history.feature = Some(feat);
        self.object_history._subscription = Some(subscription);
        self.start_history_request(cx);
        cx.notify();
        Ok(())
    }

    /// Close the view and abandon its request.
    pub(crate) fn close_object_history(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.object_history.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.object_history.request += 1;
        self.object_history.view = None;
        self.object_history.feature = None;
        self.object_history._subscription = None;
        cx.notify();
    }

    /// Fetch the history of the view's object on a worker thread; the answer
    /// is picked up by a short poll, so editing is never blocked.
    fn start_history_request(&mut self, cx: &mut Context<Self>) {
        let Some(feat) = self.object_history.feature else {
            return;
        };
        if let Some(old) = self.object_history.cancel.take() {
            old.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.object_history.cancel = Some(cancel.clone());
        self.object_history.request += 1;
        let request = self.object_history.request;
        if let Some(view) = &self.object_history.view {
            view.update(cx, |view, cx| view.set_loading(cx));
        }

        let fixture = self.object_history.fixture.clone();
        let api = settings_store::api_base_url();
        let (tx, rx) = mpsc::channel();
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || {
            let result = match fixture {
                Some(path) => std::fs::read_to_string(&path)
                    .map_err(|e| HistoryError::Network(format!("{}: {e}", path.display())))
                    .and_then(|body| parse_response(&body, feat.kind, feat.id)),
                None => fetch_from_server(&api, &feat, &worker_cancel),
            };
            let _ = tx.send(result);
        });
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            match rx.try_recv() {
                Ok(result) => {
                    let _ = this.update(cx, |this, cx| {
                        this.finish_history_request(request, result, cx)
                    });
                    break;
                }
                Err(mpsc::TryRecvError::Empty) if !cancel.load(Ordering::Relaxed) => {}
                Err(_) => break,
            }
        })
        .detach();
    }

    fn finish_history_request(
        &mut self,
        request: u64,
        result: Result<ObjectHistory, HistoryError>,
        cx: &mut Context<Self>,
    ) {
        if request != self.object_history.request {
            return;
        }
        let (Some(feat), Some(view)) = (
            self.object_history.feature,
            self.object_history.view.clone(),
        ) else {
            return;
        };
        self.object_history.cancel = None;
        match result {
            Ok(history) => {
                let local = self.local_object_state(&feat);
                self.remember_last_edits(&history);
                view.update(cx, |view, cx| view.set_loaded(history, local, cx));
            }
            Err(HistoryError::Cancelled) => {}
            Err(error) => view.update(cx, |view, cx| view.set_failed(error, cx)),
        }
        cx.notify();
    }

    /// Keep who edited each version, for the Selection header's last-edit
    /// line.
    fn remember_last_edits(&mut self, history: &ObjectHistory) {
        let server = settings_store::api_base_url();
        if self.object_history.last_edits_server != server {
            self.object_history.last_edits.clear();
            self.object_history.last_edits_server = server;
        }
        let edits = history
            .versions
            .iter()
            .filter(|v| v.visible)
            .map(LastEdit::from_version)
            .collect();
        self.object_history
            .last_edits
            .insert((history.kind, history.id), edits);
    }

    /// What the server said about the version of `feat` this layer holds,
    /// if its history has been fetched. Never starts a request.
    pub(crate) fn last_edit_for(&self, feat: &FeatureRef) -> Option<LastEdit> {
        if feat.id < 0 {
            return None;
        }
        let editable = self
            .layer_manager
            .find_layer(feat.layer_id)?
            .as_editable()?;
        let version = u32::try_from(editable.feature_version(feat.kind, feat.id)?).ok()?;
        self.object_history
            .last_edits
            .get(&(feat.kind, feat.id))?
            .iter()
            .find(|e| e.version == version)
            .cloned()
    }

    /// The website matching the API server in use, for the header's links.
    pub(crate) fn web_base(&self) -> String {
        auth::oauth_base_for(&settings_store::api_base_url())
    }

    /// `feat` as the editor has it now, for diffing against the server.
    pub(crate) fn local_object_state(&self, feat: &FeatureRef) -> Option<ObjectState> {
        let editable = self
            .layer_manager
            .find_layer(feat.layer_id)?
            .as_editable()?;
        let tags: BTreeMap<String, String> = editable.feature_tags(feat)?.into_iter().collect();
        let mut state = ObjectState {
            tags,
            ..ObjectState::default()
        };
        match feat.kind {
            FeatureKind::Node => state.position = editable.node_lat_lon(feat.id),
            FeatureKind::Way => state.nodes = editable.way_node_ids(feat.id)?,
            FeatureKind::Relation => state.members = editable.relation(feat.id)?.members,
        }
        Some(state)
    }

    /// Keep the view's "your unsaved changes" row in step with edits made
    /// while it is open. Called every frame; does nothing unless something
    /// changed.
    pub(crate) fn sync_history_local(&mut self, cx: &mut Context<Self>) {
        let (Some(feat), Some(view)) = (
            self.object_history.feature,
            self.object_history.view.clone(),
        ) else {
            return;
        };
        let local = self.local_object_state(&feat);
        view.update(cx, |view, cx| view.set_local(local, cx));
    }

    /// "Revert tags to this version": make the object's tags `target`, as
    /// one undoable tag edit. Geometry is never touched.
    pub(crate) fn revert_tags_to_version(
        &mut self,
        version: u32,
        target: &BTreeMap<String, String>,
        cx: &mut Context<Self>,
    ) {
        if self.edits_blocked(cx) {
            self.set_status(tr!("object_history.revert_edits_blocked"));
            return;
        }
        let Some(feat) = self.object_history.feature else {
            return;
        };
        let Some((_, current)) = self.feature_tag_snapshots(&[feat]).into_iter().next() else {
            return;
        };
        let current: BTreeMap<String, String> = current.into_iter().collect();
        let entries: Vec<_> = tag_revert_edits(&current, target)
            .into_iter()
            .map(|(key, before, after)| (feat, key, before, after))
            .collect();
        if entries.is_empty() {
            self.set_status(tr!("object_history.revert_nothing", version = version));
            cx.notify();
            return;
        }
        for (feature, key, _before, after) in &entries {
            let Some(editable) = self
                .layer_manager
                .find_layer_mut(feature.layer_id)
                .and_then(|layer| layer.as_editable_mut())
            else {
                continue;
            };
            match after {
                Some(value) => editable.set_tag(feature.kind, feature.id, key, value),
                None => editable.remove_tag(feature.kind, feature.id, key),
            }
        }
        self.undo_stack.push(UndoableAction::SetTags { entries });
        self.set_status(tr!("object_history.reverted", version = version));
        self.sync_history_local(cx);
        cx.notify();
    }

    // ------------------------------------------------------ script ops

    pub(crate) fn script_history(
        &mut self,
        cmd: osm_gpui::script::HistoryCmd,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        use osm_gpui::script::HistoryCmd;
        match cmd {
            HistoryCmd::Show => self.show_object_history(cx),
            HistoryCmd::ChangesetPart(part) => self.script_changeset_part(&part, cx),
            HistoryCmd::Changeset { id, dir } => self.script_show_changeset(id, &dir, cx),
            HistoryCmd::LoadFile(path) => {
                if !path.is_file() {
                    return Err(format!("{} is not a file", path.display()));
                }
                self.object_history.fixture = Some(path);
                Ok(())
            }
        }
    }

    pub(crate) fn script_history_versions(
        &self,
        want: usize,
        cx: &gpui::App,
    ) -> Result<(), String> {
        let Some(view) = &self.object_history.view else {
            return Err("the object history view is not open".to_string());
        };
        let view = view.read(cx);
        match view.state() {
            LoadState::Loaded(_) if view.version_count() == want => Ok(()),
            LoadState::Loaded(_) => Err(format!(
                "the object history lists {} versions, expected {want}",
                view.version_count()
            )),
            LoadState::Loading => Err("the object history is still loading".to_string()),
            LoadState::Failed(error) => Err(format!(
                "the object history failed to load: {}",
                error.message("")
            )),
        }
    }

    pub(crate) fn script_history_settled(&self, cx: &gpui::App) -> Result<(), String> {
        match self
            .object_history
            .view
            .as_ref()
            .map(|v| v.read(cx).state())
        {
            None => Err("the object history view is not open".to_string()),
            Some(LoadState::Loading) => Err("the object history is still loading".to_string()),
            Some(_) => Ok(()),
        }
    }
}

/// The anonymous request to the API server. Unit tests never touch the
/// network.
fn fetch_from_server(
    api: &str,
    feat: &FeatureRef,
    cancel: &AtomicBool,
) -> Result<ObjectHistory, HistoryError> {
    #[cfg(test)]
    {
        let _ = (api, feat, cancel);
        Err(HistoryError::Network("no network in tests".to_string()))
    }
    #[cfg(not(test))]
    {
        osm_gpui::object_history::fetch_history(api, feat.kind, feat.id, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size, TestAppContext};
    use osm_gpui::layers::osm_layer::OsmLayer;
    use osm_gpui::osm::OsmParser;
    use osm_gpui::transfer::{TransferHandle, TransferKind, TransferPhase};

    const OSM: &str = "docs/screenshots/fixtures/object-history.osm";
    const HISTORY: &str = "docs/screenshots/fixtures/object-history-way-3001.history.xml";

    fn path(rel: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    fn setup(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
        let data = OsmParser::new()
            .parse_file(path(OSM).to_str().unwrap())
            .unwrap();
        cx.update(gpui_component::init);
        let window = cx.add_window(MapViewer::new);
        window
            .update(cx, |view, _window, _cx| {
                view.viewport = osm_gpui::viewport::Viewport::new(
                    40.712,
                    -74.006,
                    18.0,
                    size(px(800.0), px(600.0)),
                );
                let layer_id = view.layer_manager.alloc_id();
                let layer =
                    OsmLayer::new_with_data(layer_id, "Fixture".to_string(), Arc::new(data));
                view.layer_manager.add_layer(Box::new(layer));
                view.active_layer = Some(layer_id);
            })
            .unwrap();
        window
    }

    fn select_way(view: &mut MapViewer, id: i64) -> FeatureRef {
        let feat = FeatureRef {
            layer_id: view.active_layer.unwrap(),
            kind: FeatureKind::Way,
            id,
        };
        view.set_selection(vec![feat]);
        feat
    }

    fn way_tags(view: &MapViewer, feat: &FeatureRef) -> BTreeMap<String, String> {
        view.local_object_state(feat).unwrap().tags
    }

    fn canned_history() -> ObjectHistory {
        let xml = std::fs::read_to_string(path(HISTORY)).unwrap();
        parse_response(&xml, FeatureKind::Way, 3001).unwrap()
    }

    /// Open the view and deliver the canned history as the answer.
    fn open_with_history(view: &mut MapViewer, cx: &mut Context<MapViewer>) {
        view.show_object_history(cx).unwrap();
        let request = view.object_history.request;
        view.finish_history_request(request, Ok(canned_history()), cx);
    }

    #[gpui::test]
    fn showing_history_needs_exactly_one_existing_object(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                view.set_selection(vec![]);
                assert!(view.show_object_history(cx).is_err());
                assert!(view.object_history.view.is_none());

                let layer_id = view.active_layer.unwrap();
                view.set_selection(vec![
                    FeatureRef {
                        layer_id,
                        kind: FeatureKind::Way,
                        id: 3001,
                    },
                    FeatureRef {
                        layer_id,
                        kind: FeatureKind::Node,
                        id: 1001,
                    },
                ]);
                assert!(view.show_object_history(cx).is_err());

                // A new object has no server history.
                view.set_selection(vec![FeatureRef {
                    layer_id,
                    kind: FeatureKind::Node,
                    id: -1,
                }]);
                let why = view.show_object_history(cx).unwrap_err();
                assert!(why.contains("no history"), "{why}");
                assert!(view.object_history.view.is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_view_loads_diffs_and_teaches_the_header_who_last_edited(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                let feat = select_way(view, 3001);
                // Nothing is known before the history is fetched.
                assert_eq!(view.last_edit_for(&feat), None);

                view.show_object_history(cx).unwrap();
                let shown = view.object_history.view.clone().unwrap();
                assert_eq!(shown.read(cx).state(), &LoadState::Loading);

                // A stale answer (an older request) is ignored.
                let request = view.object_history.request;
                view.finish_history_request(request - 1, Ok(canned_history()), cx);
                assert_eq!(shown.read(cx).state(), &LoadState::Loading);

                view.finish_history_request(request, Ok(canned_history()), cx);
                let loaded = shown.read(cx);
                // Versions 1, 3, 4 and 5 (2 is redacted).
                assert_eq!(loaded.version_count(), 4);
                // The local way matches the newest version.
                assert!(!loaded.has_local_row());

                let edit = view.last_edit_for(&feat).expect("version 5 is known");
                assert_eq!(edit.version, 5);
                assert_eq!(edit.user.as_deref(), Some("Joe Mapper"));
                assert_eq!(edit.changeset, Some(134_000_005));
            })
            .unwrap();
    }

    #[gpui::test]
    fn unsaved_edits_show_up_while_the_view_is_open(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                let feat = select_way(view, 3001);
                open_with_history(view, cx);
                let shown = view.object_history.view.clone().unwrap();
                assert!(!shown.read(cx).has_local_row());

                view.layer_manager
                    .find_layer_mut(feat.layer_id)
                    .unwrap()
                    .as_editable_mut()
                    .unwrap()
                    .set_tag(FeatureKind::Way, 3001, "lit", "yes");
                view.sync_history_local(cx);
                assert!(shown.read(cx).has_local_row());
                // The header still describes the downloaded version.
                assert_eq!(view.last_edit_for(&feat).unwrap().version, 5);
            })
            .unwrap();
    }

    #[gpui::test]
    fn closing_the_view_abandons_the_request(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                select_way(view, 3001);
                view.show_object_history(cx).unwrap();
                let cancel = view.object_history.cancel.clone().unwrap();
                let request = view.object_history.request;
                view.close_object_history(cx);
                assert!(view.object_history.view.is_none());
                assert!(cancel.load(Ordering::Relaxed));
                // An answer arriving afterwards changes nothing.
                view.finish_history_request(request, Ok(canned_history()), cx);
                assert!(view.object_history.view.is_none());
                assert_eq!(view.last_edit_for(&view.selected[0]), None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_failed_request_shows_the_error_and_retry_asks_again(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                select_way(view, 3001);
                view.show_object_history(cx).unwrap();
                let shown = view.object_history.view.clone().unwrap();
                let request = view.object_history.request;
                view.finish_history_request(
                    request,
                    Err(HistoryError::Network("offline".into())),
                    cx,
                );
                assert_eq!(
                    shown.read(cx).state(),
                    &LoadState::Failed(HistoryError::Network("offline".into()))
                );
                // Retry: a new request, loading again.
                view.start_history_request(cx);
                assert_eq!(shown.read(cx).state(), &LoadState::Loading);
                assert_eq!(view.object_history.request, request + 1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn reverting_tags_is_one_undoable_tag_only_edit(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                let feat = select_way(view, 3001);
                open_with_history(view, cx);
                let before = way_tags(view, &feat);
                let nodes_before = view.local_object_state(&feat).unwrap().nodes;
                let v4 = canned_history().version(4).unwrap().state.tags.clone();

                view.revert_tags_to_version(4, &v4, cx);
                assert_eq!(way_tags(view, &feat), v4);
                assert_eq!(view.undo_stack.actions.len(), 1);
                // Tags only: the geometry is untouched.
                assert_eq!(view.local_object_state(&feat).unwrap().nodes, nodes_before);
                // The view now shows the difference to the newest version.
                assert!(view
                    .object_history
                    .view
                    .clone()
                    .unwrap()
                    .read(cx)
                    .has_local_row());

                // Reverting again to the same tags is not an edit.
                view.revert_tags_to_version(4, &v4, cx);
                assert_eq!(view.undo_stack.actions.len(), 1);

                let action = view.undo_stack.undo().unwrap();
                view.apply_undo_action(&action, false);
                assert_eq!(way_tags(view, &feat), before);
                let action = view.undo_stack.redo().unwrap();
                view.apply_undo_action(&action, true);
                assert_eq!(way_tags(view, &feat), v4);
            })
            .unwrap();
    }

    #[gpui::test]
    fn reverting_tags_is_refused_while_an_upload_runs(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |view, _window, cx| {
                let feat = select_way(view, 3001);
                open_with_history(view, cx);
                let before = way_tags(view, &feat);
                let v4 = canned_history().version(4).unwrap().state.tags.clone();

                view.transfer = Some(TransferHandle::new(
                    TransferKind::Upload,
                    TransferPhase::UploadingChanges { count: 1 },
                ));
                view.revert_tags_to_version(4, &v4, cx);
                assert_eq!(way_tags(view, &feat), before);
                assert!(view.undo_stack.actions.is_empty());
            })
            .unwrap();
    }
}
