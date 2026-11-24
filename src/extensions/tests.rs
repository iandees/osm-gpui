//! The host against the real MapRoulette extension, built from
//! `extensions/maproulette` for wasm32-wasip2 (once per test run), with
//! scripted HTTP and in-memory secrets.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use super::runtime::{secret_name, Bounds, Element, FeatureRef, MemorySecrets, SecretStore};
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

fn services(http: Arc<dyn HttpClient>, secrets: Arc<dyn SecretStore>) -> Services {
    Services {
        http,
        secrets,
        storage: None,
    }
}

fn load_with(http: Arc<dyn HttpClient>, secrets: Arc<dyn SecretStore>) -> LoadedExtension {
    let manifest = Arc::new(read_manifest(&maproulette_src()).unwrap());
    LoadedExtension::from_parts(
        manifest,
        maproulette_src(),
        &maproulette_wasm(),
        Arc::new(Locales::load(&maproulette_src())),
        services(http, secrets),
    )
    .unwrap()
}

fn load(http: Arc<dyn HttpClient>) -> LoadedExtension {
    load_with(http, Arc::new(MemorySecrets::default()))
}

fn context_in(zoom: f64, locale: &str) -> CallContext {
    CallContext {
        viewport: Bounds {
            min_lat: 44.95,
            min_lon: -93.30,
            max_lat: 45.00,
            max_lon: -93.20,
        },
        zoom,
        selection: Vec::<FeatureRef>::new(),
        locale: locale.into(),
    }
}

fn context(zoom: f64) -> CallContext {
    context_in(zoom, "en")
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

/// The last thing asked of panel `id`: `Some(Some(panel))` shown,
/// `Some(None)` closed, `None` untouched.
fn panel(effects: &[Effect], id: &str) -> Option<Option<runtime::Panel>> {
    effects.iter().rev().find_map(|e| match e {
        Effect::ShowPanel { id: pid, panel } if pid == id => Some(panel.clone()),
        _ => None,
    })
}

fn statuses(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Status(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
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
    assert_eq!(statuses(&out.effects), vec!["MapRoulette: 2 tasks"]);
}

#[test]
fn messages_come_from_the_extensions_own_catalog_in_the_apps_language() {
    let http = Arc::new(FakeClient::new(vec![ok(200, TASKS)]));
    let ext = load(http);
    let out = ext.run_command(context_in(15.0, "de-AT"), "show-tasks");
    out.result.unwrap();
    assert_eq!(statuses(&out.effects), vec!["MapRoulette: 2 Aufgaben"]);
    // The menu labels come from the same catalogs.
    assert_eq!(
        ext.locales.lookup("de", "commands.show-tasks"),
        Some("Aufgaben hier zeigen")
    );
    assert_eq!(ext.display_name("fr"), "MapRoulette");
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
    let p = panel(&out.effects, "task")
        .flatten()
        .expect("task panel open");
    assert!(p
        .elements
        .iter()
        .any(|e| matches!(e, Element::Heading(h) if h == "Traffic Light Placement West US")));
    assert!(p
        .elements
        .iter()
        .any(|e| matches!(e, Element::Text(t) if t.contains("approach"))));
    assert!(markers(&out.effects)[0].selected);

    let out = ext.panel_button(context(15.0), "task", "load", &[]);
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

    // Closing the task panel deselects the pin.
    let out = ext.panel_closed(context(15.0), "task");
    out.result.unwrap();
    assert!(!markers(&out.effects)[0].selected);
}

#[test]
fn the_api_key_is_its_own_panel_beside_the_task_and_lives_in_the_secret_store() {
    let http = Arc::new(FakeClient::new(vec![
        ok(200, TASKS),
        ok(200, TASK),
        ok(200, CHALLENGE),
        ok(200, "{}"),
    ]));
    let secrets = Arc::new(MemorySecrets::default());
    let ext = load_with(http.clone(), secrets.clone());
    ext.run_command(context(15.0), "show-tasks").result.unwrap();
    ext.marker_clicked(context(15.0), "317096925")
        .result
        .unwrap();

    // Asking for the key opens a second panel; the task panel stays.
    let out = ext.panel_button(context(15.0), "task", "ask-key", &[]);
    out.result.unwrap();
    assert!(panel(&out.effects, "api-key").flatten().is_some());
    assert!(
        panel(&out.effects, "task").is_none(),
        "task panel untouched"
    );

    let out = ext.panel_button(
        context(15.0),
        "api-key",
        "save-key",
        &[("key".to_string(), " secret-key ".to_string())],
    );
    out.result.unwrap();
    assert!(
        matches!(panel(&out.effects, "api-key"), Some(None)),
        "key panel closes"
    );
    assert_eq!(
        secrets
            .get(&secret_name("maproulette", "api-key"))
            .as_deref(),
        Some("secret-key")
    );

    let out = ext.panel_button(context(15.0), "task", "status-1", &[]);
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
fn secrets_are_namespaced_per_extension() {
    let secrets = Arc::new(MemorySecrets::default());
    secrets.set(&secret_name("other", "api-key"), Some("not yours"));
    let ext = load_with(Arc::new(FakeClient::new(vec![])), secrets.clone());
    let out = ext.run_command(context(15.0), "settings");
    out.result.unwrap();
    let p = panel(&out.effects, "api-key").flatten().unwrap();
    assert!(
        !p.elements
            .iter()
            .any(|e| matches!(e, Element::Buttons(b) if b.iter().any(|b| b.id == "forget-key"))),
        "another extension's secret is not this one's"
    );
}

#[test]
fn a_key_kept_in_plain_storage_moves_to_the_secret_store() {
    let dir = std::env::temp_dir().join(format!("osm-gpui-ext-mig-{}", std::process::id()));
    let storage = dir.join("maproulette.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&storage, r#"{"api-key":"old-key"}"#).unwrap();
    let secrets = Arc::new(MemorySecrets::default());
    let manifest = Arc::new(read_manifest(&maproulette_src()).unwrap());
    let ext = LoadedExtension::from_parts(
        manifest,
        maproulette_src(),
        &maproulette_wasm(),
        Arc::new(Locales::load(&maproulette_src())),
        Services {
            http: Arc::new(FakeClient::new(vec![])),
            secrets: secrets.clone(),
            storage: Some(storage.clone()),
        },
    )
    .unwrap();
    ext.run_command(context(15.0), "settings").result.unwrap();
    assert_eq!(
        secrets
            .get(&secret_name("maproulette", "api-key"))
            .as_deref(),
        Some("old-key")
    );
    assert!(!std::fs::read_to_string(&storage)
        .unwrap()
        .contains("old-key"));
    let _ = std::fs::remove_dir_all(dir);
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
        Arc::new(Locales::default()),
        services(http.clone(), Arc::new(MemorySecrets::default())),
    )
    .unwrap();
    let out = ext.run_command(context(15.0), "show-tasks");
    let err = out.result.unwrap_err();
    assert!(err.contains("may not reach"), "{err}");
    assert_eq!(http.request_count(), 0);
}

#[test]
fn install_copies_the_manifest_wasm_and_locales_and_uninstall_removes_them() {
    let root = std::env::temp_dir().join(format!("osm-gpui-ext-inst-{}", std::process::id()));
    let wasm = root.join("built.wasm");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&wasm, maproulette_wasm()).unwrap();
    let dest = install(&root, &maproulette_src(), &wasm).unwrap();
    assert_eq!(installed(&root), vec![dest.clone()]);
    assert!(dest.join("locales/de.toml").is_file());
    let ext = LoadedExtension::load(
        &dest,
        services(
            Arc::new(FakeClient::new(vec![])),
            Arc::new(MemorySecrets::default()),
        ),
    )
    .unwrap();
    assert_eq!(
        ext.locales.lookup("de", "commands.hide-tasks"),
        Some("Aufgaben ausblenden")
    );
    uninstall(&root, "maproulette").unwrap();
    assert!(installed(&root).is_empty());
    let _ = std::fs::remove_dir_all(root);
}
