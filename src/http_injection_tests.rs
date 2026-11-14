//! The binary makes every request through the `HttpClient` its `MapViewer`
//! holds, so a test (or a scripted run) can swap it. These tests check that
//! seam end to end and that nothing grows a second way around it.

use super::*;
use gpui::TestAppContext;
use osm_gpui::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use std::path::{Path, PathBuf};

const MAP_XML: &str = r#"<osm version="0.6">
  <bounds minlat="39.9" minlon="-74.1" maxlat="40.1" maxlon="-73.9"/>
  <node id="1" lat="40.0" lon="-74.0" version="1"/>
</osm>"#;

/// Answers by URL and records every request, so the test does not depend on
/// the order in which the app's threads ask.
struct RoutedClient {
    seen: Mutex<Vec<String>>,
}

impl HttpClient for RoutedClient {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.seen.lock().unwrap().push(req.url.clone());
        if req.url.contains("/api/0.6/map?") {
            Ok(HttpResponse {
                status: 200,
                body: MAP_XML.as_bytes().to_vec(),
            })
        } else {
            Err(HttpError::Status {
                status: 404,
                body: Vec::new(),
            })
        }
    }
}

#[gpui::test]
fn a_download_goes_through_the_viewers_client(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let client = Arc::new(RoutedClient {
        seen: Mutex::new(Vec::new()),
    });
    let window = cx.add_window(MapViewer::new);
    let injected: Arc<dyn HttpClient> = client.clone();
    window
        .update(cx, |view, _, cx| {
            view.http = injected;
            view.viewport = Viewport::new(40.0, -74.0, 18.0, gpui::size(px(800.0), px(600.0)));
            view.request_download(cx);
        })
        .unwrap();
    cx.run_until_parked();

    let seen = client.seen.lock().unwrap().clone();
    assert!(
        seen.iter().any(|u| u.contains("/api/0.6/map?bbox=")),
        "the map request must reach the injected client: {seen:?}"
    );
    window
        .update(cx, |view, _, _| {
            assert!(view.transfer.is_none(), "the download finished");
            assert_eq!(view.layer_manager.layers().len(), 1);
        })
        .unwrap();
}

#[gpui::test]
fn a_viewer_without_an_installed_client_cannot_reach_the_network(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, _, _| {
            let err = view
                .http
                .request(HttpRequest::get("https://example.invalid/"))
                .unwrap_err();
            assert!(matches!(err, HttpError::Transport(_)), "{err:?}");
        })
        .unwrap();
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `UreqClient::new()` may be written in one place only: the construction in
/// `fn main`. Anywhere else must take a client or use `http::default_client()`.
#[test]
fn the_real_client_is_constructed_only_in_main() {
    let needle = ["UreqClient", "::new()"].concat();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut found: Vec<(String, usize)> = Vec::new();
    for path in files {
        let name = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if name == "http_injection_tests.rs" {
            continue;
        }
        let count = std::fs::read_to_string(&path)
            .unwrap()
            .matches(&needle)
            .count();
        if count > 0 {
            found.push((name, count));
        }
    }
    found.sort();
    let expected: Vec<(String, usize)> = vec![("main.rs".to_string(), 1)];
    assert_eq!(
        found, expected,
        "construct the client once in `fn main` and pass it down"
    );
}
