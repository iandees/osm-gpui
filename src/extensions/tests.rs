//! The host against the real MapRoulette extension, built from
//! `extensions/maproulette` for wasm32-wasip2 (once per test run), with
//! scripted HTTP.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use super::runtime::{Bounds, Element, FeatureRef};
use super::*;
use crate::http::fake::{ok, FakeClient};
use crate::http::{HttpClient, Method};

fn maproulette_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/maproulette")
}

/// The built extension. Building needs the wasm target
/// (`rustup target add wasm32-wasip2`).
fn maproulette_wasm() -> Vec<u8> {
    static WASM: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
    WASM.get_or_init(|| {
        // Its own target folder: the one `cargo test` is using stays locked
        // while the tests run.
        let target = std::env::temp_dir().join("osm-gpui-extension-test-build");
        let path = build_dev_extension_in(&maproulette_src(), Some(&target))?;
        std::fs::read(path).map_err(|e| e.to_string())
    })
    .clone()
    .unwrap_or_else(|e| panic!("building the MapRoulette extension failed: {e}"))
}

fn load(http: Arc<dyn HttpClient>) -> LoadedExtension {
    let manifest = Arc::new(read_manifest(&maproulette_src()).unwrap());
    LoadedExtension::from_parts(manifest, maproulette_src(), &maproulette_wasm(), http, None)
        .unwrap()
}

fn context(zoom: f64) -> CallContext {
    CallContext {
        viewport: Bounds {
            min_lat: 44.95,
            min_lon: -93.30,
            max_lat: 45.00,
            max_lon: -93.20,
        },
        zoom,
        selection: Vec::<FeatureRef>::new(),
    }
}

const TASKS: &str = r#"[
 {"id":317096925,"parentId":52711,"parentName":"Traffic Light Placement West US",
  "point":{"lat":44.9592107,"lng":-93.2765766},"status":0},
 {"id":360536324,"parentId":56109,"parentName":"USA - Cycleway Missing Foot Tags",
  "point":{"lat":44.962644952,"lng":-93.288595349},"status":1}]"#;

const TASK: &str = r#"{"id":317096925,"name":"node/34642174","parent":52711,"instruction":"",
 "status":0,"location":{"type":"Point","coordinates":[-93.2765766,44.9592107]},
 "geometries":{"type":"FeatureCollection","features":[{"id":"node/34642174","type":"Feature",
  "geometry":{"type":"Point","coordinates":[-93.2765766,44.9592107]},
  "properties":{"@id":"node/34642174"}}]}}"#;

const CHALLENGE: &str = r#"{"id":52711,"name":"Traffic Light Placement West US",
 "instruction":"Move the traffic signals onto each approach.",
 "description":"Lights on junction nodes.",
 "checkinComment":"Relocated highway=traffic_signals #maproulette"}"#;

fn markers(effects: &[Effect]) -> Vec<runtime::Marker> {
    effects
        .iter()
        .rev()
        .find_map(|e| match e {
            Effect::SetMarkers(m) => Some(m.clone()),
            _ => None,
        })
        .expect("no markers set")
}

fn panel(effects: &[Effect]) -> Option<runtime::Panel> {
    effects
        .iter()
        .rev()
        .find_map(|e| match e {
            Effect::ShowPanel(p) => Some(p.clone()),
            _ => None,
        })
        .expect("no panel shown")
}

#[test]
fn show_tasks_draws_a_marker_per_task_in_view() {
    let http = Arc::new(FakeClient::new(vec![ok(200, TASKS)]));
    let ext = load(http.clone());
    let out = ext.run_command(context(15.0), "show-tasks");
    out.result.unwrap();

    let url = http.requests.lock().unwrap()[0].url.clone();
    assert_eq!(
        url,
        "https://maproulette.org/api/v2/tasks/box/-93.3/44.95/-93.2/45?limit=500"
    );
    let m = markers(&out.effects);
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].id, "317096925");
    assert_ne!(
        m[0].color, m[1].color,
        "open and fixed tasks look different"
    );
    assert!(out
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Status(s) if s == "MapRoulette: 2 tasks")));
}

#[test]
fn zoomed_out_it_asks_to_zoom_in_and_fetches_nothing() {
    let http = Arc::new(FakeClient::new(vec![]));
    let ext = load(http.clone());
    let out = ext.run_command(context(6.0), "show-tasks");
    out.result.unwrap();
    assert_eq!(http.request_count(), 0);
}

#[test]
fn clicking_a_task_opens_its_challenge_and_load_data_downloads_around_it() {
    let http = Arc::new(FakeClient::new(vec![
        ok(200, TASKS),
        ok(200, TASK),
        ok(200, CHALLENGE),
    ]));
    let ext = load(http.clone());
    ext.run_command(context(15.0), "show-tasks").result.unwrap();

    let out = ext.marker_clicked(context(15.0), "317096925");
    out.result.unwrap();
    let p = panel(&out.effects).expect("panel open");
    assert!(p
        .elements
        .iter()
        .any(|e| matches!(e, Element::Heading(h) if h == "Traffic Light Placement West US")));
    assert!(p
        .elements
        .iter()
        .any(|e| matches!(e, Element::Text(t) if t.contains("approach"))));
    assert!(markers(&out.effects)[0].selected);

    let out = ext.panel_button(context(15.0), "load", &[]);
    out.result.unwrap();
    let area = out
        .effects
        .iter()
        .find_map(|e| match e {
            Effect::DownloadArea(b) => Some(*b),
            _ => None,
        })
        .expect("a download");
    assert!(area.min_lat < 44.9592107 && area.max_lat > 44.9592107);
    assert!(area.min_lon < -93.2765766 && area.max_lon > -93.2765766);

    let tags = ext.changeset_tags(context(15.0)).result.unwrap();
    assert!(tags.contains(&(
        "comment".to_string(),
        "Relocated highway=traffic_signals #maproulette".to_string()
    )));
}

#[test]
fn marking_a_task_sends_the_saved_api_key() {
    let http = Arc::new(FakeClient::new(vec![
        ok(200, TASKS),
        ok(200, TASK),
        ok(200, CHALLENGE),
        ok(200, "{}"),
    ]));
    let ext = load(http.clone());
    ext.run_command(context(15.0), "show-tasks").result.unwrap();
    ext.marker_clicked(context(15.0), "317096925")
        .result
        .unwrap();
    ext.panel_button(
        context(15.0),
        "save-key",
        &[("key".to_string(), " secret-key ".to_string())],
    )
    .result
    .unwrap();

    let out = ext.panel_button(context(15.0), "status-1", &[]);
    out.result.unwrap();
    let reqs = http.requests.lock().unwrap();
    let put = reqs.last().unwrap();
    assert_eq!(put.method, Method::Put);
    assert_eq!(put.url, "https://maproulette.org/api/v2/task/317096925/1");
    assert!(put
        .headers
        .contains(&("apiKey".to_string(), "secret-key".to_string())));
    assert_eq!(
        markers(&out.effects)[0].color,
        markers(&out.effects)[1].color
    );
}

#[test]
fn http_outside_the_manifest_is_refused_before_it_is_sent() {
    // The extension cannot be told to fetch elsewhere, so check the gate on
    // a manifest that does not list maproulette.org.
    let mut manifest = read_manifest(&maproulette_src()).unwrap();
    manifest.capabilities.http = vec!["example.org".into()];
    let http = Arc::new(FakeClient::new(vec![]));
    let ext = LoadedExtension::from_parts(
        Arc::new(manifest),
        maproulette_src(),
        &maproulette_wasm(),
        http.clone(),
        None,
    )
    .unwrap();
    let out = ext.run_command(context(15.0), "show-tasks");
    let err = out.result.unwrap_err();
    assert!(err.contains("may not reach"), "{err}");
    assert_eq!(http.request_count(), 0);
}

#[test]
fn storage_persists_across_loads() {
    let dir = std::env::temp_dir().join(format!("osm-gpui-ext-test-{}", std::process::id()));
    let path = dir.join("maproulette.json");
    let manifest = Arc::new(read_manifest(&maproulette_src()).unwrap());
    let http: Arc<dyn HttpClient> = Arc::new(FakeClient::new(vec![]));
    {
        let ext = LoadedExtension::from_parts(
            manifest.clone(),
            maproulette_src(),
            &maproulette_wasm(),
            http.clone(),
            Some(path.clone()),
        )
        .unwrap();
        ext.panel_button(
            context(15.0),
            "save-key",
            &[("key".to_string(), "k1".to_string())],
        )
        .result
        .unwrap();
    }
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("k1"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn install_copies_the_manifest_and_wasm_and_uninstall_removes_them() {
    let root = std::env::temp_dir().join(format!("osm-gpui-ext-inst-{}", std::process::id()));
    let wasm = root.join("built.wasm");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&wasm, maproulette_wasm()).unwrap();
    let dest = install(&root, &maproulette_src(), &wasm).unwrap();
    assert_eq!(installed(&root), vec![dest.clone()]);
    LoadedExtension::load(&dest, Arc::new(FakeClient::new(vec![])), None).unwrap();
    uninstall(&root, "maproulette").unwrap();
    assert!(installed(&root).is_empty());
    let _ = std::fs::remove_dir_all(root);
}
