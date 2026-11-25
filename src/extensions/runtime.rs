//! Running one extension: its wasmtime component, the sandbox it runs in, the
//! host functions it may call, and the requests it leaves for the app.
//!
//! An extension runs on a background thread (each call takes its lock), never
//! on the UI thread: its calls may block on HTTP. Host functions that would
//! change the app do not touch it; they queue an [`Effect`] that the caller
//! applies on the UI thread once the call returns.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use super::locales::Locales;
use super::manifest::Manifest;
use crate::http::{Body, HttpClient, HttpRequest, Method};

/// Older API versions, adapted to the current one.
mod v0_1;

mod bindings {
    wasmtime::component::bindgen!({
        path: "crates/osm-gpui-extension-api/wit",
        world: "extension",
    });
}

pub use bindings::osm_gpui::extension::types::{
    Bounds, Button, ButtonStyle, Element, FeatureKind, FeatureRef, HttpMethod, HttpResponse, Image,
    LatLon, Marker, Panel, TextInput,
};
use bindings::osm_gpui::extension::{host, types};

/// Wasm instructions one call may run (HTTP waits do not count). A runaway
/// loop traps instead of hanging the extension's thread forever.
const FUEL_PER_CALL: u64 = 20_000_000_000;
/// Largest HTTP response body handed to an extension.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Largest value, and total, an extension may keep in its storage.
const MAX_STORAGE_BYTES: usize = 256 * 1024;

/// The app's state as an extension sees it during one call.
#[derive(Debug, Clone)]
pub struct CallContext {
    pub viewport: Bounds,
    pub zoom: f64,
    pub selection: Vec<FeatureRef>,
    /// The app's UI language.
    pub locale: String,
}

impl Default for CallContext {
    fn default() -> Self {
        CallContext {
            viewport: Bounds {
                min_lat: 0.0,
                min_lon: 0.0,
                max_lat: 0.0,
                max_lon: 0.0,
            },
            zoom: 0.0,
            selection: Vec::new(),
            locale: "en".into(),
        }
    }
}

/// Something an extension asked the app to do, in the order it asked.
#[derive(Debug, Clone)]
pub enum Effect {
    FlyTo {
        at: LatLon,
        zoom: f64,
    },
    DownloadArea(Bounds),
    Status(String),
    OpenUrl(String),
    SetMarkers(Vec<Marker>),
    /// Show (`Some`) or close (`None`) the extension's panel `id`.
    ShowPanel {
        id: String,
        panel: Option<Panel>,
    },
}

/// Where extensions keep secrets. Names are already namespaced per
/// extension by the caller.
pub trait SecretStore: Send + Sync {
    fn get(&self, name: &str) -> Option<String>;
    /// `None` deletes.
    fn set(&self, name: &str, value: Option<&str>);
}

/// The app's secret storage (`crate::secrets`): the system keychain, or an
/// owner-only file where there is none; cached for the session.
pub struct KeychainSecrets;

impl SecretStore for KeychainSecrets {
    fn get(&self, name: &str) -> Option<String> {
        crate::secrets::get(name)
    }

    fn set(&self, name: &str, value: Option<&str>) {
        match value {
            Some(v) => crate::secrets::set(name, v),
            None => crate::secrets::clear(name),
        }
    }
}

/// Secrets held in memory only (tests).
#[derive(Default)]
pub struct MemorySecrets(Mutex<BTreeMap<String, String>>);

impl SecretStore for MemorySecrets {
    fn get(&self, name: &str) -> Option<String> {
        self.0.lock().unwrap().get(name).cloned()
    }

    fn set(&self, name: &str, value: Option<&str>) {
        let mut m = self.0.lock().unwrap();
        match value {
            Some(v) => {
                m.insert(name.to_string(), v.to_string());
            }
            None => {
                m.remove(name);
            }
        }
    }
}

/// The secret-store name of extension `id`'s secret `key`.
pub fn secret_name(id: &str, key: &str) -> String {
    format!("extension:{id}:{key}")
}

/// Longest secret key or value an extension may store.
const MAX_SECRET_BYTES: usize = 8 * 1024;

/// What one call left behind.
#[derive(Debug)]
pub struct Outcome<T> {
    /// The extension's own answer, or why the call failed (an `err` it
    /// returned, a trap, running out of fuel).
    pub result: Result<T, String>,
    pub effects: Vec<Effect>,
}

/// The extension's private key-value storage, a JSON file.
struct Storage {
    path: Option<PathBuf>,
    values: BTreeMap<String, String>,
}

impl Storage {
    fn open(path: Option<PathBuf>) -> Storage {
        let values = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Storage { path, values }
    }

    fn set(&mut self, key: String, value: Option<String>) {
        match value {
            Some(v) => {
                let total: usize = self
                    .values
                    .iter()
                    .filter(|(k, _)| **k != key)
                    .map(|(k, v)| k.len() + v.len())
                    .sum();
                if total + key.len() + v.len() > MAX_STORAGE_BYTES {
                    eprintln!("extension storage full; {key:?} not saved");
                    return;
                }
                self.values.insert(key, v);
            }
            None => {
                self.values.remove(&key);
            }
        }
        if let Some(path) = &self.path {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(text) = serde_json::to_string_pretty(&self.values) {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
        }
    }
}

/// Send one extension request: refused unless the manifest allows its URL;
/// any server answer (2xx or not) is `Ok`.
fn send(
    manifest: &Manifest,
    http: &dyn HttpClient,
    request: types::HttpRequest,
) -> Result<HttpResponse, String> {
    if !manifest.allows_url(&request.url) {
        return Err(format!(
            "{} may not reach {} (not in its manifest's http capabilities)",
            manifest.id, request.url
        ));
    }
    let method = match request.method {
        HttpMethod::Get => Method::Get,
        HttpMethod::Post => Method::Post,
        HttpMethod::Put => Method::Put,
        HttpMethod::Delete => return Err("DELETE is not supported yet".into()),
    };
    let body = match request.body {
        Some(bytes) => Some(String::from_utf8(bytes).map_err(|_| "body must be UTF-8")?),
        None => None,
    };
    let mut req = HttpRequest::with_text(method, request.url, body);
    for (name, value) in request.headers {
        req = req.header(&name, value);
    }
    // An empty PUT/POST still needs a body for some servers.
    if req.body.is_none() && method != Method::Get {
        req.body = Some(Body::Text(String::new()));
    }
    let answer =
        http.request_with_progress(req, &mut |read, _| read as usize <= MAX_RESPONSE_BYTES);
    match answer {
        Ok(resp) => Ok(HttpResponse {
            status: resp.status,
            body: resp.body,
        }),
        Err(crate::http::HttpError::Status { status, body }) => Ok(HttpResponse { status, body }),
        Err(crate::http::HttpError::Cancelled) => {
            Err(format!("response larger than {MAX_RESPONSE_BYTES} bytes"))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Requests `http-fetch-many` sends at once.
const PARALLEL_REQUESTS: usize = 6;

/// The store's data: WASI's (empty) context and everything the host
/// functions need.
struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    manifest: Arc<Manifest>,
    http: Arc<dyn HttpClient>,
    context: CallContext,
    effects: Vec<Effect>,
    storage: Storage,
    secrets: Arc<dyn SecretStore>,
    locales: Arc<Locales>,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl types::Host for HostState {}

impl host::Host for HostState {
    fn http_fetch(&mut self, request: types::HttpRequest) -> Result<types::HttpResponse, String> {
        send(&self.manifest, self.http.as_ref(), request)
    }

    fn http_fetch_many(
        &mut self,
        requests: Vec<types::HttpRequest>,
    ) -> Vec<Result<types::HttpResponse, String>> {
        let (manifest, http) = (&self.manifest, self.http.as_ref());
        let mut answers: Vec<Option<Result<HttpResponse, String>>> =
            (0..requests.len()).map(|_| None).collect();
        let mut queue: Vec<(usize, types::HttpRequest)> =
            requests.into_iter().enumerate().collect();
        queue.reverse();
        // A few at a time, so one extension cannot open a flood of
        // connections.
        while !queue.is_empty() {
            let batch: Vec<(usize, types::HttpRequest)> =
                (0..PARALLEL_REQUESTS).filter_map(|_| queue.pop()).collect();
            let done: Vec<(usize, Result<HttpResponse, String>)> = std::thread::scope(|scope| {
                let handles: Vec<_> = batch
                    .into_iter()
                    .map(|(i, req)| scope.spawn(move || (i, send(manifest, http, req))))
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|_| (usize::MAX, Err("request panicked".into())))
                    })
                    .collect()
            });
            for (i, answer) in done {
                if let Some(slot) = answers.get_mut(i) {
                    *slot = Some(answer);
                }
            }
        }
        answers
            .into_iter()
            .map(|a| a.unwrap_or_else(|| Err("request lost".into())))
            .collect()
    }

    fn viewport(&mut self) -> Bounds {
        self.context.viewport
    }

    fn zoom(&mut self) -> f64 {
        self.context.zoom
    }

    fn selection(&mut self) -> Vec<FeatureRef> {
        self.context.selection.clone()
    }

    fn fly_to(&mut self, at: LatLon, zoom: f64) {
        if at.lat.is_finite() && at.lon.is_finite() && zoom.is_finite() {
            self.effects.push(Effect::FlyTo { at, zoom });
        }
    }

    fn download_area(&mut self, area: Bounds) {
        self.effects.push(Effect::DownloadArea(area));
    }

    fn set_status(&mut self, message: String) {
        self.effects.push(Effect::Status(message));
    }

    fn open_url(&mut self, url: String) {
        if url.starts_with("https://") || url.starts_with("http://") {
            self.effects.push(Effect::OpenUrl(url));
        }
    }

    fn set_markers(&mut self, markers: Vec<Marker>) {
        self.effects.push(Effect::SetMarkers(markers));
    }

    fn show_panel(&mut self, id: String, panel: Option<Panel>) {
        self.effects.push(Effect::ShowPanel { id, panel });
    }

    fn secret_get(&mut self, key: String) -> Option<String> {
        if !self.manifest.capabilities.secrets {
            eprintln!(
                "[ext {}] secret-get refused: the manifest does not ask for `secrets`",
                self.manifest.id
            );
            return None;
        }
        if key.is_empty() || key.len() > 128 {
            return None;
        }
        self.secrets
            .get(&secret_name(&self.manifest.id, &key))
            .filter(|v| !v.is_empty())
    }

    fn secret_set(&mut self, key: String, value: Option<String>) {
        if !self.manifest.capabilities.secrets {
            eprintln!(
                "[ext {}] secret-set refused: the manifest does not ask for `secrets`",
                self.manifest.id
            );
            return;
        }
        if key.is_empty() || key.len() > 128 {
            return;
        }
        let value = value.filter(|v| !v.is_empty());
        if value.as_ref().is_some_and(|v| v.len() > MAX_SECRET_BYTES) {
            eprintln!("[ext {}] secret {key:?} too large", self.manifest.id);
            return;
        }
        self.secrets
            .set(&secret_name(&self.manifest.id, &key), value.as_deref());
    }

    fn locale(&mut self) -> String {
        self.context.locale.clone()
    }

    fn translate(&mut self, key: String, args: Vec<(String, String)>) -> String {
        self.locales.translate(&self.context.locale, &key, &args)
    }

    fn storage_get(&mut self, key: String) -> Option<String> {
        self.storage.values.get(&key).cloned()
    }

    fn storage_set(&mut self, key: String, value: Option<String>) {
        self.storage.set(key, value);
    }

    fn log(&mut self, message: String) {
        eprintln!("[ext {}] {message}", self.manifest.id);
    }
}

/// The current extension API version: the WIT package version of
/// `crates/osm-gpui-extension-api/wit`, which the SDK builds against.
pub const API_VERSION: &str = "0.2.0";

/// Every API version this app runs extensions for, oldest first. Each older
/// one has a module here (`v0_1`) that adapts it to the current host.
pub const SUPPORTED_API_VERSIONS: &[&str] = &["0.1.0", API_VERSION];

/// The extension API version `wasm` was built against, read from its import
/// of `osm-gpui:extension/host@<version>`. `Err` when it is not a component
/// or imports no version of the host interface.
pub fn api_version_of(wasm: &[u8]) -> Result<String, String> {
    let component = Component::new(engine(), wasm).map_err(|e| e.to_string())?;
    api_version_of_component(&component)
}

fn api_version_of_component(component: &Component) -> Result<String, String> {
    let engine = engine();
    let ty = component.component_type();
    let version = ty.imports(engine).find_map(|(name, _)| {
        name.strip_prefix("osm-gpui:extension/host@")
            .map(str::to_string)
    });
    version.ok_or_else(|| "not an osm-gpui extension (no host import)".to_string())
}

/// One engine for every extension.
fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        config.max_wasm_stack(1024 * 1024);
        Engine::new(&config).expect("wasmtime engine")
    })
}

/// The extension's exports, through the bindings of the API version it was
/// built against.
enum Guest {
    Current(bindings::Extension),
    V0_1(v0_1::Extension),
}

/// Call export `$f` with `$args` on whichever bindings `$guest` has; `$old`
/// is what a 0.1 guest without it answers.
macro_rules! dispatch {
    ($guest:expr, $store:expr, $f:ident($($args:expr),*)) => {
        match $guest {
            Guest::Current(b) => b.$f($store $(, $args)*),
            Guest::V0_1(b) => b.$f($store $(, $args)*),
        }
    };
}

struct Instance {
    store: Store<HostState>,
    guest: Guest,
}

/// What the app hands an extension when it loads it.
pub struct Services {
    pub http: Arc<dyn HttpClient>,
    pub secrets: Arc<dyn SecretStore>,
    /// Where its key-value storage lives (`None`: in memory only).
    pub storage: Option<PathBuf>,
}

/// A loaded, initialised extension.
pub struct LoadedExtension {
    pub manifest: Arc<Manifest>,
    pub dir: PathBuf,
    /// Its translations (`locales/*.toml`).
    pub locales: Arc<Locales>,
    /// The extension API version it was built against.
    pub api_version: String,
    instance: Mutex<Instance>,
}

impl std::fmt::Debug for LoadedExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedExtension")
            .field("id", &self.manifest.id)
            .field("dir", &self.dir)
            .finish()
    }
}

impl LoadedExtension {
    /// Compile and start the extension in `dir` (`extension.toml` and
    /// `extension.wasm`), keeping its storage at `storage` (none: kept in
    /// memory only). Slow (it compiles the wasm): call off the UI thread.
    pub fn load(dir: &Path, services: Services) -> Result<LoadedExtension, String> {
        let text = std::fs::read_to_string(dir.join("extension.toml"))
            .map_err(|e| format!("{}: {e}", dir.join("extension.toml").display()))?;
        let manifest = Arc::new(Manifest::parse(&text)?);
        let wasm = std::fs::read(dir.join("extension.wasm"))
            .map_err(|e| format!("{}: {e}", dir.join("extension.wasm").display()))?;
        let locales = Arc::new(Locales::load(dir));
        Self::from_parts(manifest, dir.to_path_buf(), &wasm, locales, services)
    }

    pub fn from_parts(
        manifest: Arc<Manifest>,
        dir: PathBuf,
        wasm: &[u8],
        locales: Arc<Locales>,
        services: Services,
    ) -> Result<LoadedExtension, String> {
        let Services {
            http,
            secrets,
            storage,
        } = services;
        let engine = engine();
        let component =
            Component::new(engine, wasm).map_err(|e| format!("{}: {e}", manifest.id))?;
        let mut linker: Linker<HostState> = Linker::new(engine);
        // WASI for Rust's std, with nothing granted: no files, no sockets,
        // no environment, no arguments. Its stderr goes to the app's, for
        // panics.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| e.to_string())?;
        // Every supported API version's host, side by side: each is its own
        // versioned interface, so a guest links to the one it was built for.
        bindings::Extension::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)
            .map_err(|e| e.to_string())?;
        v0_1::Extension::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)
            .map_err(|e| e.to_string())?;
        let version =
            api_version_of_component(&component).map_err(|e| format!("{}: {e}", manifest.id))?;
        let wasi = WasiCtxBuilder::new().inherit_stderr().build();
        let state = HostState {
            wasi,
            table: ResourceTable::new(),
            manifest: manifest.clone(),
            http,
            context: CallContext::default(),
            effects: Vec::new(),
            storage: Storage::open(storage),
            secrets,
            locales: locales.clone(),
        };
        let mut store = Store::new(engine, state);
        store.set_fuel(FUEL_PER_CALL).map_err(|e| e.to_string())?;
        let failed = |e: wasmtime::Error| format!("{}: {e}", manifest.id);
        let guest = match version.as_str() {
            v if v == API_VERSION => Guest::Current(
                bindings::Extension::instantiate(&mut store, &component, &linker)
                    .map_err(failed)?,
            ),
            "0.1.0" => Guest::V0_1(
                v0_1::Extension::instantiate(&mut store, &component, &linker).map_err(failed)?,
            ),
            other => {
                return Err(format!(
                    "{} was built against extension API {other}; this app runs {}",
                    manifest.id,
                    SUPPORTED_API_VERSIONS.join(", ")
                ))
            }
        };
        let ext = LoadedExtension {
            manifest,
            dir,
            locales,
            api_version: version,
            instance: Mutex::new(Instance { store, guest }),
        };
        let init = ext.call(CallContext::default(), |g, s| dispatch!(g, s, call_init()));
        init.result?;
        Ok(ext)
    }

    /// Run one export with `context` as the app's state, and collect what it
    /// asked for.
    fn call<T>(
        &self,
        context: CallContext,
        f: impl FnOnce(&Guest, &mut Store<HostState>) -> wasmtime::Result<Result<T, String>>,
    ) -> Outcome<T> {
        let mut guard = match self.instance.lock() {
            Ok(g) => g,
            Err(_) => {
                return Outcome {
                    result: Err(format!("{} crashed earlier", self.manifest.id)),
                    effects: Vec::new(),
                }
            }
        };
        let Instance { store, guest } = &mut *guard;
        store.data_mut().context = context;
        store.data_mut().effects.clear();
        let _ = store.set_fuel(FUEL_PER_CALL);
        let result = match f(guest, store) {
            Ok(r) => r,
            Err(trap) => Err(format!("{} failed: {trap}", self.manifest.id)),
        };
        let effects = std::mem::take(&mut store.data_mut().effects);
        Outcome { result, effects }
    }

    pub fn run_command(&self, context: CallContext, id: &str) -> Outcome<()> {
        self.call(context, |g, s| dispatch!(g, s, call_run_command(id)))
    }

    pub fn marker_clicked(&self, context: CallContext, id: &str) -> Outcome<()> {
        self.call(context, |g, s| dispatch!(g, s, call_marker_clicked(id)))
    }

    pub fn panel_button(
        &self,
        context: CallContext,
        panel: &str,
        button: &str,
        inputs: &[(String, String)],
    ) -> Outcome<()> {
        self.call(context, |g, s| {
            dispatch!(g, s, call_panel_button(panel, button, inputs))
        })
    }

    /// The map settled. A 0.1 extension cannot hear of it (it could not
    /// subscribe either).
    pub fn viewport_changed(&self, context: CallContext) -> Outcome<()> {
        self.call(context, |g, s| match g {
            Guest::Current(b) => b.call_viewport_changed(s),
            Guest::V0_1(_) => Ok(Ok(())),
        })
    }

    /// The selection changed. As `viewport_changed`, nothing for 0.1.
    pub fn selection_changed(&self, context: CallContext) -> Outcome<()> {
        self.call(context, |g, s| match g {
            Guest::Current(b) => b.call_selection_changed(s),
            Guest::V0_1(_) => Ok(Ok(())),
        })
    }

    pub fn panel_closed(&self, context: CallContext, panel: &str) -> Outcome<()> {
        self.call(context, |g, s| {
            dispatch!(g, s, call_panel_closed(panel)).map(Ok)
        })
    }

    /// The extension's name in `locale` (its `name` message, else the
    /// manifest's).
    pub fn display_name(&self, locale: &str) -> String {
        self.locales
            .lookup(locale, "name")
            .unwrap_or(&self.manifest.name)
            .to_string()
    }

    pub fn changeset_tags(&self, context: CallContext) -> Outcome<Vec<(String, String)>> {
        self.call(context, |g, s| {
            dispatch!(g, s, call_changeset_tags()).map(Ok)
        })
    }

    pub fn upload_finished(&self, context: CallContext, changesets: &[i64]) -> Outcome<()> {
        self.call(context, |g, s| {
            dispatch!(g, s, call_upload_finished(changesets)).map(Ok)
        })
    }
}
