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
fn secrets_need_the_manifest_capability() {
    let mut manifest = read_manifest(&maproulette_src()).unwrap();
    manifest.capabilities.secrets = false;
    let secrets = Arc::new(MemorySecrets::default());
    let ext = LoadedExtension::from_parts(
        Arc::new(manifest),
        maproulette_src(),
        &maproulette_wasm(),
        Arc::new(Locales::default()),
        services(Arc::new(FakeClient::new(vec![])), secrets.clone()),
    )
    .unwrap();
    ext.panel_button(
        context(15.0),
        "api-key",
        "save-key",
        &[("key".to_string(), "k".to_string())],
    )
    .result
    .unwrap();
    assert_eq!(secrets.get(&secret_name("maproulette", "api-key")), None);
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

mod from_the_index {
    use super::*;
    use crate::extensions::registry::{
        download_and_install, fetch_index, install_package, package, sha256_hex, Index, IndexEntry,
        INDEX_SCHEMA_VERSION,
    };

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("osm-gpui-ext-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The MapRoulette package and its index entry.
    fn packaged(dir: &Path) -> (Vec<u8>, IndexEntry) {
        let wasm = dir.join("built.wasm");
        std::fs::write(&wasm, maproulette_wasm()).unwrap();
        let bytes = package(&maproulette_src(), &wasm).unwrap();
        let entry = IndexEntry {
            id: "maproulette".into(),
            name: "MapRoulette".into(),
            version: read_manifest(&maproulette_src()).unwrap().version,
            description: String::new(),
            authors: vec![],
            repository: String::new(),
            api_version: runtime::API_VERSION.into(),
            capabilities: Default::default(),
            names: Default::default(),
            descriptions: Default::default(),
            download_url: "https://ext.example.test/maproulette.tar.gz".into(),
            sha256: sha256_hex(&bytes),
            size: bytes.len() as u64,
        };
        (bytes, entry)
    }

    #[test]
    fn the_built_extension_reports_the_apps_api_version() {
        assert_eq!(
            runtime::api_version_of(&maproulette_wasm()).unwrap(),
            runtime::API_VERSION
        );
    }

    #[test]
    fn an_index_entry_downloads_installs_and_loads_with_its_locales() {
        let root = scratch("index-install");
        let (bytes, entry) = packaged(&root);
        let index = Index {
            schema_version: INDEX_SCHEMA_VERSION,
            extensions: vec![entry.clone()],
        };
        let http = Arc::new(FakeClient::new(vec![
            ok(200, serde_json::to_string(&index).unwrap()),
            ok(200, bytes),
        ]));
        let fetched = fetch_index(http.as_ref(), "https://ext.example.test/index.json").unwrap();
        assert_eq!(fetched, index);
        let dir = download_and_install(http.as_ref(), &root, &fetched.extensions[0]).unwrap();
        assert_eq!(installed(&root), vec![dir.clone()]);
        assert!(dev_source(&dir).is_none(), "not a dev extension");
        let ext = LoadedExtension::load(
            &dir,
            services(
                Arc::new(FakeClient::new(vec![])),
                Arc::new(MemorySecrets::default()),
            ),
        )
        .unwrap();
        assert_eq!(
            ext.locales.lookup("de", "commands.settings"),
            Some("API-Schlüssel…")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_package_that_does_not_match_its_entry_is_refused_and_leaves_nothing() {
        let root = scratch("index-refuse");
        let (bytes, entry) = packaged(&root);

        let mut tampered = bytes.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        let err = install_package(&root, &entry, &tampered).unwrap_err();
        assert!(err.contains("checksum"), "{err}");

        let wrong_version = IndexEntry {
            version: "9.9.9".into(),
            ..entry.clone()
        };
        let err = install_package(&root, &wrong_version, &bytes).unwrap_err();
        assert!(err.contains("not maproulette 9.9.9"), "{err}");

        let wrong_id = IndexEntry {
            id: "other".into(),
            ..entry
        };
        assert!(install_package(&root, &wrong_id, &bytes).is_err());
        assert!(installed(&root).is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(installed_dir(&root))
            .unwrap()
            .flatten()
            .collect();
        assert!(leftovers.is_empty(), "staging is cleaned up: {leftovers:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_archive_with_other_files_is_refused() {
        let root = scratch("index-evil");
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        let data = b"#!/bin/sh\n";
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "run.sh", &data[..]).unwrap();
        let bytes = tar.into_inner().unwrap().finish().unwrap();
        let (_, entry) = packaged(&root);
        let entry = IndexEntry {
            sha256: sha256_hex(&bytes),
            size: bytes.len() as u64,
            ..entry
        };
        let err = install_package(&root, &entry, &bytes).unwrap_err();
        assert!(err.contains("unexpected"), "{err}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_dev_install_remembers_its_source() {
        let root = scratch("dev-install");
        let wasm = root.join("built.wasm");
        std::fs::write(&wasm, maproulette_wasm()).unwrap();
        let dir = install_dev(&root, &maproulette_src(), &wasm).unwrap();
        assert_eq!(
            dev_source(&dir).unwrap(),
            maproulette_src().canonicalize().unwrap()
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

mod street_photos {
    //! The Panoramax extension against the host: tiles fetched in parallel
    //! when the view settles, markers with headings, and a photo's details
    //! and picture when one is clicked.

    use super::*;
    use crate::http::{HttpError, HttpRequest, HttpResponse};
    use std::sync::Mutex;

    fn panoramax_src() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/panoramax")
    }

    fn panoramax_wasm() -> Vec<u8> {
        static WASM: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
        WASM.get_or_init(|| {
            let target = std::env::temp_dir().join("osm-gpui-extension-test-build");
            let path = build_dev_extension_in(&panoramax_src(), Some(&target))?;
            std::fs::read(path).map_err(|e| e.to_string())
        })
        .clone()
        .unwrap_or_else(|e| panic!("building the Panoramax extension failed: {e}"))
    }

    /// Answers by URL (requests arrive in any order when sent in parallel).
    struct Routes {
        routes: Vec<(&'static str, u16, Vec<u8>)>,
        seen: Mutex<Vec<String>>,
    }

    impl HttpClient for Routes {
        fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
            self.seen.lock().unwrap().push(req.url.clone());
            let (status, body) = self
                .routes
                .iter()
                .find(|(part, _, _)| req.url.contains(part))
                .map(|(_, status, body)| (*status, body.clone()))
                .unwrap_or((204, Vec::new()));
            if (200..300).contains(&status) {
                Ok(HttpResponse { status, body })
            } else {
                Err(HttpError::Status { status, body })
            }
        }
    }

    const SEARCH: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","id":"ID","geometry":{"type":"Point","coordinates":[-93.2234,44.926]},
         "properties":{"datetime":"2025-09-16T18:02:01Z","view:azimuth":334,"license":"CC-BY-SA-4.0"},
         "providers":[{"name":"someone","roles":["producer"]}],
         "assets":{"sd":{"href":"https://panoramax.openstreetmap.fr/sd.jpg"}}}]}"#;

    /// The view: the middle of the fixture tile (z15 7898/11796).
    fn view() -> CallContext {
        let n = 2f64.powi(15);
        let lon = |x: f64| x / n * 360.0 - 180.0;
        let lat = |y: f64| {
            (std::f64::consts::PI * (1.0 - 2.0 * y / n))
                .sinh()
                .atan()
                .to_degrees()
        };
        CallContext {
            viewport: Bounds {
                min_lat: lat(11796.7),
                max_lat: lat(11796.3),
                min_lon: lon(7898.3),
                max_lon: lon(7898.7),
            },
            zoom: 17.0,
            selection: Vec::new(),
            locale: "en".into(),
        }
    }

    #[test]
    fn a_settled_view_fetches_its_tiles_and_a_click_opens_the_photo() {
        let tile =
            include_bytes!("../../extensions/street-photos/fixtures/panoramax_15_7898_11796.mvt");
        let http = Arc::new(Routes {
            routes: vec![
                ("/api/map/2/15/7898/11796.mvt", 200, tile.to_vec()),
                ("/api/search?ids=", 200, SEARCH.as_bytes().to_vec()),
            ],
            seen: Mutex::new(Vec::new()),
        });
        let manifest = Arc::new(read_manifest(&panoramax_src()).unwrap());
        assert!(manifest.subscribes("viewport"));
        let ext = LoadedExtension::from_parts(
            manifest,
            panoramax_src(),
            &panoramax_wasm(),
            Arc::new(Locales::load(&panoramax_src())),
            services(http.clone(), Arc::new(MemorySecrets::default())),
        )
        .unwrap();

        // Hidden: a settled view fetches nothing.
        ext.viewport_changed(view()).result.unwrap();
        assert!(http.seen.lock().unwrap().is_empty());

        let out = ext.run_command(view(), "show-photos");
        out.result.unwrap();
        let tiles: Vec<String> = http.seen.lock().unwrap().clone();
        assert!(
            tiles.iter().any(|u| u.ends_with("/15/7898/11796.mvt")),
            "{tiles:?}"
        );
        let m = markers(&out.effects);
        assert!(!m.is_empty());
        assert!(m.iter().any(|m| m.heading.is_some()));
        assert_eq!(m[0].color, 0x9333ea);

        // The same view again: everything is cached.
        let before = http.seen.lock().unwrap().len();
        ext.viewport_changed(view()).result.unwrap();
        assert_eq!(http.seen.lock().unwrap().len(), before);

        let out = ext.marker_clicked(view(), &m[0].id);
        out.result.unwrap();
        assert!(http
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.contains("/api/search?ids=")));
        let p = panel(&out.effects, "photo").flatten().expect("photo panel");
        assert!(p.elements.iter().any(|e| matches!(
            e,
            Element::Picture(i) if i.url == "https://panoramax.openstreetmap.fr/sd.jpg"
        )));
        assert!(markers(&out.effects).iter().any(|m| m.selected));
    }

    #[test]
    fn zoomed_out_too_far_it_asks_to_zoom_in_and_fetches_nothing() {
        let http = Arc::new(Routes {
            routes: vec![("/api/map/2/15/7898/11796.mvt", 200, b"x".to_vec())],
            seen: Mutex::new(Vec::new()),
        });
        let manifest = Arc::new(read_manifest(&panoramax_src()).unwrap());
        let ext = LoadedExtension::from_parts(
            manifest,
            panoramax_src(),
            &panoramax_wasm(),
            Arc::new(Locales::load(&panoramax_src())),
            services(http.clone(), Arc::new(MemorySecrets::default())),
        )
        .unwrap();
        // Too far out: more tiles than are fetched, so nothing is asked.
        let mut wide = view();
        wide.viewport = Bounds {
            min_lat: 40.0,
            max_lat: 50.0,
            min_lon: -100.0,
            max_lon: -90.0,
        };
        let out = ext.run_command(wide, "show-photos");
        out.result.unwrap();
        assert!(http.seen.lock().unwrap().is_empty());
        assert_eq!(
            statuses(&out.effects),
            vec!["Zoom in to see Panoramax photos"]
        );
    }
}

mod older_api_versions {
    //! Extensions built against an older API version keep working: the
    //! released MapRoulette 0.1.0 (built against API 0.1.0, kept as a fixture)
    //! loads and runs in this app.

    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extensions/maproulette-0.1.0")
    }

    #[test]
    fn every_supported_version_but_the_current_one_has_a_fixture_that_runs() {
        // A new API version must keep the previous ones running: freeze the
        // released WIT, add an adapter module, and add a fixture built
        // against it (see docs/extensions.md, "API versions").
        let older: Vec<&str> = runtime::SUPPORTED_API_VERSIONS
            .iter()
            .copied()
            .filter(|v| *v != runtime::API_VERSION)
            .collect();
        assert_eq!(
            older,
            ["0.1.0"],
            "add a fixture test for each older version"
        );
        let wasm = std::fs::read(fixture().join("extension.wasm")).unwrap();
        assert_eq!(runtime::api_version_of(&wasm).unwrap(), "0.1.0");
        assert!(registry::api_compatible("0.1.0"));
    }

    /// The released API versions' WIT, frozen. A released version never
    /// changes: change the API by adding a version (see docs/extensions.md,
    /// "Changing the API").
    const FROZEN: &[(&str, &str)] = &[
        (
            "0.1.0",
            "ec741897211fef13700727b8e6dc13ba8c4b0d6f73a6401b59ad74f88e934e96",
        ),
        (
            "0.2.0",
            "edc90f5136925fdcee07b47bd4d54158e7b3878298f07c2946f4842c712d23e7",
        ),
    ];

    fn frozen_wit(version: &str) -> String {
        let (major, minor) = {
            let mut it = version.split('.');
            (it.next().unwrap(), it.next().unwrap())
        };
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("src/extensions/wit/v{major}_{minor}/extension.wit"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test]
    fn released_api_versions_never_change() {
        assert_eq!(
            FROZEN.iter().map(|(v, _)| *v).collect::<Vec<_>>(),
            runtime::SUPPORTED_API_VERSIONS,
            "every supported version is frozen (and only those)"
        );
        for (version, hash) in FROZEN {
            let text = frozen_wit(version);
            assert!(
                text.contains(&format!("package osm-gpui:extension@{version};")),
                "{version}: the frozen WIT names its own version"
            );
            assert_eq!(
                registry::sha256_hex(text.as_bytes()),
                *hash,
                "the frozen WIT of released API {version} changed"
            );
        }
        // The SDK builds against the current version, unchanged.
        let sdk = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("crates/osm-gpui-extension-api/wit/extension.wit"),
        )
        .unwrap();
        assert_eq!(
            sdk,
            frozen_wit(runtime::API_VERSION),
            "the SDK's WIT changed: add a new API version instead"
        );
        let sdk_version = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/osm-gpui-extension-api/Cargo.toml"),
        )
        .unwrap();
        assert!(
            sdk_version.contains(&format!("version = \"{}\"", runtime::API_VERSION)),
            "the SDK crate's version is the API version"
        );
    }

    #[test]
    fn a_0_1_extension_loads_runs_and_shows_its_panel() {
        let http = Arc::new(FakeClient::new(vec![
            ok(200, TASKS),
            ok(200, TASK),
            ok(200, CHALLENGE),
        ]));
        let ext = LoadedExtension::load(
            &fixture(),
            services(http.clone(), Arc::new(MemorySecrets::default())),
        )
        .unwrap();
        assert_eq!(ext.api_version, "0.1.0");

        let out = ext.run_command(context(15.0), "show-tasks");
        out.result.unwrap();
        let m = markers(&out.effects);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].heading, None, "0.1 markers have no heading");
        assert_eq!(statuses(&out.effects), vec!["MapRoulette: 2 tasks"]);

        let out = ext.marker_clicked(context(15.0), "317096925");
        out.result.unwrap();
        let p = panel(&out.effects, "task").flatten().expect("task panel");
        assert!(p
            .elements
            .iter()
            .any(|e| matches!(e, Element::Heading(h) if h == "Traffic Light Placement West US")));

        // Events 0.1 has no exports for are no-ops, not failures.
        ext.viewport_changed(context(15.0)).result.unwrap();
        ext.selection_changed(context(15.0)).result.unwrap();
        assert_eq!(http.request_count(), 3);
    }
}
