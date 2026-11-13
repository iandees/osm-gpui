//! The app side of File > Show Changeset…: asking for a changeset number,
//! fetching the changeset in the background, and adding its three layers
//! (changes, before, after) to the map. See `changeset_view`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use gpui::{actions, AppContext as _, Context, Entity, Window};
use osm_gpui::changeset_view::{ChangesetError, ChangesetView};
use osm_gpui::layers::changeset_layer::{ChangesetLayer, Part};
#[cfg(test)]
use osm_gpui::layers::MapLayer as _;
use osm_gpui::settings_store;
use osm_gpui::tr;
use osm_gpui::ui::changeset_dialog::{ChangesetDialog, DialogEvent};

use crate::MapViewer;

actions!(osm_gpui, [ShowChangeset]);

#[derive(Default)]
pub(crate) struct ChangesetState {
    pub dialog: Option<Entity<ChangesetDialog>>,
    /// Set to abandon the running request.
    cancel: Option<Arc<AtomicBool>>,
    request: u64,
}

impl MapViewer {
    /// File > Show Changeset…
    pub(crate) fn on_show_changeset(
        &mut self,
        _: &ShowChangeset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_changeset_dialog(window, cx);
    }

    pub(crate) fn changeset_dialog_element(&self) -> Option<Entity<ChangesetDialog>> {
        self.changeset.dialog.clone()
    }

    fn open_changeset_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.changeset.dialog.is_some() {
            return;
        }
        let dialog = cx.new(|cx| ChangesetDialog::new(window, cx));
        cx.subscribe(
            &dialog,
            |this: &mut Self, _dialog, event: &DialogEvent, cx| match event {
                DialogEvent::Cancelled => this.close_changeset_dialog(cx),
                DialogEvent::Submit(id) => this.start_changeset_request(*id, cx),
            },
        )
        .detach();
        self.changeset.dialog = Some(dialog);
        cx.notify();
    }

    fn close_changeset_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.changeset.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.changeset.request += 1;
        self.changeset.dialog = None;
        cx.notify();
    }

    /// Fetch changeset `id` on a worker thread; the answer is picked up by a
    /// short poll, so editing is never blocked.
    pub(crate) fn start_changeset_request(&mut self, id: i64, cx: &mut Context<Self>) {
        if let Some(old) = self.changeset.cancel.take() {
            old.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.changeset.cancel = Some(cancel.clone());
        self.changeset.request += 1;
        let request = self.changeset.request;
        if let Some(dialog) = &self.changeset.dialog {
            dialog.update(cx, |d, cx| d.begin(cx));
        }

        let api = settings_store::api_base_url();
        let (tx, rx) = mpsc::channel();
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || {
            let _ = tx.send(fetch_from_server(&api, id, &worker_cancel));
        });
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            match rx.try_recv() {
                Ok(result) => {
                    let _ = this.update(cx, |this, cx| {
                        this.finish_changeset_request(request, result, cx)
                    });
                    break;
                }
                Err(mpsc::TryRecvError::Empty) if !cancel.load(Ordering::Relaxed) => {}
                Err(_) => break,
            }
        })
        .detach();
    }

    pub(crate) fn finish_changeset_request(
        &mut self,
        request: u64,
        result: Result<ChangesetView, ChangesetError>,
        cx: &mut Context<Self>,
    ) {
        if request != self.changeset.request {
            return;
        }
        self.changeset.cancel = None;
        match result {
            Ok(view) => {
                self.close_changeset_dialog(cx);
                self.add_changeset_layers(view, cx);
            }
            Err(ChangesetError::Cancelled) => {}
            Err(error) => {
                if let Some(dialog) = &self.changeset.dialog {
                    dialog.update(cx, |d, cx| d.fail(error.message().into(), cx));
                }
            }
        }
        cx.notify();
    }

    /// Add the changeset's three layers above the imagery and below the data
    /// layers: "changes" shown, "before" and "after" hidden until wanted. Zoom
    /// to the changeset.
    pub(crate) fn add_changeset_layers(&mut self, view: ChangesetView, cx: &mut Context<Self>) {
        let title = match (&view.comment, &view.user) {
            (Some(c), _) if !c.is_empty() => {
                let c: String = c.chars().take(40).collect();
                format!("#{} {c}", view.id)
            }
            (_, Some(u)) => format!("#{} ({u})", view.id),
            _ => format!("#{}", view.id),
        };
        let extent = view.extent();
        // Bottom to top: after, before, changes, so the changes sit on top.
        let layers = [
            (Part::After, view.after.clone(), false),
            (Part::Before, view.before.clone(), false),
            (Part::Changes, view.changes.clone(), true),
        ];
        for (part, shapes, visible) in layers {
            let name = self
                .layer_manager
                .unique_name(&format!("{} · {title}", part_label(part)));
            let id = self.layer_manager.alloc_id();
            let layer = ChangesetLayer::new(id, name, view.id, part, shapes, visible);
            self.layer_manager.add_reference_layer(Box::new(layer));
        }
        if let Some(extent) = extent {
            self.fit_to_extent(extent);
        }
        let mut status = tr!(
            "changeset.status.loaded",
            id = view.id,
            created = view.nodes.created + view.ways.created,
            modified = view.nodes.modified + view.ways.modified,
            deleted = view.nodes.deleted + view.ways.deleted
        )
        .to_string();
        if view.nodes_from_latest > 0 {
            status.push(' ');
            status.push_str(&tr!(
                "changeset.status.latest",
                count = view.nodes_from_latest
            ));
        }
        self.set_status(status);
        cx.notify();
    }
}

impl MapViewer {
    /// Script op: show changeset `id`, answering the API from files in `dir`.
    pub(crate) fn script_show_changeset(
        &mut self,
        id: i64,
        dir: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let client = FixtureClient {
            dir: dir.to_path_buf(),
        };
        let cancel = AtomicBool::new(false);
        let view =
            osm_gpui::changeset_view::fetch_changeset_with(&client, "http://fixture", id, &cancel)
                .map_err(|e| e.message())?;
        self.add_changeset_layers(view, cx);
        Ok(())
    }
}

impl MapViewer {
    /// Script op: show only the changeset layer for `part`.
    pub(crate) fn script_changeset_part(
        &mut self,
        part: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let mut found = false;
        for layer in self.layer_manager.layers_mut() {
            let Some(c) = layer.as_any().downcast_ref::<ChangesetLayer>() else {
                continue;
            };
            let show = c.part().tag() == part;
            found |= show;
            layer.set_visible(show);
        }
        cx.notify();
        found
            .then_some(())
            .ok_or_else(|| "no changeset layers".to_string())
    }
}

/// Answers the changeset requests from local files, for scripted runs:
/// `download.xml` (the osmChange), `changeset.xml`, `old.xml` (previous
/// versions, asked for with `idvN`) and `latest.xml` (current nodes).
struct FixtureClient {
    dir: std::path::PathBuf,
}

impl osm_gpui::http::HttpClient for FixtureClient {
    fn request(
        &self,
        req: osm_gpui::http::HttpRequest,
    ) -> Result<osm_gpui::http::HttpResponse, osm_gpui::http::HttpError> {
        let url = req.url;
        let file = if url.ends_with("/download") {
            "download.xml"
        } else if url.contains("/changeset/") {
            "changeset.xml"
        } else if url.split('?').nth(1).is_some_and(|q| q.contains('v')) {
            "old.xml"
        } else {
            "latest.xml"
        };
        match std::fs::read(self.dir.join(file)) {
            Ok(body) => Ok(osm_gpui::http::HttpResponse { status: 200, body }),
            Err(_) => Err(osm_gpui::http::HttpError::Status {
                status: 404,
                body: Vec::new(),
            }),
        }
    }
}

fn part_label(part: Part) -> String {
    match part {
        Part::Changes => tr!("changeset.layer.changes"),
        Part::Before => tr!("changeset.layer.before"),
        Part::After => tr!("changeset.layer.after"),
    }
    .to_string()
}

/// The anonymous request to the API server. Unit tests never touch the
/// network.
fn fetch_from_server(
    api: &str,
    id: i64,
    cancel: &AtomicBool,
) -> Result<ChangesetView, ChangesetError> {
    #[cfg(test)]
    {
        let _ = (api, id, cancel);
        Err(ChangesetError::Network("no network in tests".to_string()))
    }
    #[cfg(not(test))]
    {
        osm_gpui::changeset_view::fetch_changeset(api, id, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size, TestAppContext};
    use osm_gpui::changeset_view::{assemble, parse_elements, Context as Ctx, Tint};

    fn view() -> ChangesetView {
        let change = parse_elements(
            r#"<osmChange><create><node id="1" version="1" lat="40.7" lon="-74.0"><tag k="shop" v="x"/></node></create></osmChange>"#,
        )
        .unwrap();
        assemble(77, &change, &Ctx::default())
    }

    fn setup(cx: &mut TestAppContext) -> gpui::WindowHandle<MapViewer> {
        cx.update(gpui_component::init);
        let window = cx.add_window(MapViewer::new);
        window
            .update(cx, |v, _w, _cx| {
                v.viewport =
                    osm_gpui::viewport::Viewport::new(0.0, 0.0, 10.0, size(px(800.0), px(600.0)));
            })
            .unwrap();
        window
    }

    #[gpui::test]
    fn a_loaded_changeset_becomes_three_layers_with_only_changes_shown(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |v, _w, cx| {
                v.start_changeset_request(77, cx);
                let request = v.changeset.request;
                v.finish_changeset_request(request, Ok(view()), cx);
                let layers: Vec<(String, bool)> = v
                    .layer_manager
                    .layers()
                    .iter()
                    .filter_map(|l| {
                        l.as_any()
                            .downcast_ref::<ChangesetLayer>()
                            .map(|c| (c.part().tag().to_string(), c.is_visible()))
                    })
                    .collect();
                assert_eq!(layers.len(), 3);
                assert!(layers.contains(&("changes".to_string(), true)));
                assert!(layers.contains(&("before".to_string(), false)));
                assert!(layers.contains(&("after".to_string(), false)));
                let changes = v
                    .layer_manager
                    .layers()
                    .iter()
                    .filter_map(|l| l.as_any().downcast_ref::<ChangesetLayer>())
                    .find(|c| c.part() == Part::Changes)
                    .unwrap();
                assert_eq!(changes.shapes().points, vec![(Tint::Added, (40.7, -74.0))]);
                // Zoomed to the changeset.
                assert!((v.viewport.center().0 - 40.7).abs() < 0.01);
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_failed_request_keeps_the_dialog_and_shows_why(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MapViewer::new(window, cx));
            gpui_component::Root::new(view, window, cx)
        });
        let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let root = window.root(vcx).unwrap();
        let view: Entity<MapViewer> = root
            .read_with(vcx, |root, _| root.view().clone())
            .downcast::<MapViewer>()
            .unwrap();
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| v.open_changeset_dialog(window, cx));
        });
        view.update(vcx, |v, cx| {
            v.start_changeset_request(5, cx);
            let request = v.changeset.request;
            v.finish_changeset_request(request, Err(ChangesetError::NotFound), cx);
            let dialog = v.changeset_dialog_element().unwrap();
            assert!(dialog.read(cx).error().is_some());
            assert!(!dialog.read(cx).is_busy());
            assert!(v
                .layer_manager
                .layers()
                .iter()
                .all(|l| l.as_any().downcast_ref::<ChangesetLayer>().is_none()));
        });
    }

    #[gpui::test]
    fn an_answer_to_a_superseded_request_is_ignored(cx: &mut TestAppContext) {
        let window = setup(cx);
        window
            .update(cx, |v, _w, cx| {
                v.start_changeset_request(1, cx);
                let old = v.changeset.request;
                v.start_changeset_request(2, cx);
                v.finish_changeset_request(old, Ok(view()), cx);
                assert!(v
                    .layer_manager
                    .layers()
                    .iter()
                    .all(|l| l.as_any().downcast_ref::<ChangesetLayer>().is_none()));
            })
            .unwrap();
    }
}
