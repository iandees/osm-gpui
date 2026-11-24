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

use super::manifest::Manifest;
use crate::http::{Body, HttpClient, HttpRequest, Method};

mod bindings {
    wasmtime::component::bindgen!({
        path: "crates/osm-gpui-extension-api/wit",
        world: "extension",
    });
}

pub use bindings::osm_gpui::extension::types::{
    Bounds, Button, ButtonStyle, Element, FeatureKind, FeatureRef, HttpMethod, HttpResponse,
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
        }
    }
}

/// Something an extension asked the app to do, in the order it asked.
#[derive(Debug, Clone)]
pub enum Effect {
    FlyTo { at: LatLon, zoom: f64 },
    DownloadArea(Bounds),
    Status(String),
    OpenUrl(String),
    SetMarkers(Vec<Marker>),
    ShowPanel(Option<Panel>),
}

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
        if !self.manifest.allows_url(&request.url) {
            return Err(format!(
                "{} may not reach {} (not in its manifest's http capabilities)",
                self.manifest.id, request.url
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
        let mut len = 0usize;
        let answer = self.http.request_with_progress(req, &mut |read, _| {
            len = read as usize;
            len <= MAX_RESPONSE_BYTES
        });
        match answer {
            Ok(resp) => Ok(HttpResponse {
                status: resp.status,
                body: resp.body,
            }),
            Err(crate::http::HttpError::Status { status, body }) => {
                Ok(HttpResponse { status, body })
            }
            Err(crate::http::HttpError::Cancelled) => {
                Err(format!("response larger than {MAX_RESPONSE_BYTES} bytes"))
            }
            Err(e) => Err(e.to_string()),
        }
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

    fn show_panel(&mut self, panel: Option<Panel>) {
        self.effects.push(Effect::ShowPanel(panel));
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

struct Instance {
    store: Store<HostState>,
    bindings: bindings::Extension,
}

/// A loaded, initialised extension.
pub struct LoadedExtension {
    pub manifest: Arc<Manifest>,
    pub dir: PathBuf,
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
    pub fn load(
        dir: &Path,
        http: Arc<dyn HttpClient>,
        storage: Option<PathBuf>,
    ) -> Result<LoadedExtension, String> {
        let text = std::fs::read_to_string(dir.join("extension.toml"))
            .map_err(|e| format!("{}: {e}", dir.join("extension.toml").display()))?;
        let manifest = Arc::new(Manifest::parse(&text)?);
        let wasm = std::fs::read(dir.join("extension.wasm"))
            .map_err(|e| format!("{}: {e}", dir.join("extension.wasm").display()))?;
        Self::from_parts(manifest, dir.to_path_buf(), &wasm, http, storage)
    }

    pub fn from_parts(
        manifest: Arc<Manifest>,
        dir: PathBuf,
        wasm: &[u8],
        http: Arc<dyn HttpClient>,
        storage: Option<PathBuf>,
    ) -> Result<LoadedExtension, String> {
        let engine = engine();
        let component =
            Component::new(engine, wasm).map_err(|e| format!("{}: {e}", manifest.id))?;
        let mut linker: Linker<HostState> = Linker::new(engine);
        // WASI for Rust's std, with nothing granted: no files, no sockets,
        // no environment, no arguments. Its stderr goes to the app's, for
        // panics.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| e.to_string())?;
        bindings::Extension::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)
            .map_err(|e| e.to_string())?;
        let wasi = WasiCtxBuilder::new().inherit_stderr().build();
        let state = HostState {
            wasi,
            table: ResourceTable::new(),
            manifest: manifest.clone(),
            http,
            context: CallContext::default(),
            effects: Vec::new(),
            storage: Storage::open(storage),
        };
        let mut store = Store::new(engine, state);
        store.set_fuel(FUEL_PER_CALL).map_err(|e| e.to_string())?;
        let bindings =
            bindings::Extension::instantiate(&mut store, &component, &linker).map_err(|e| {
                format!(
                    "{}: {e} (built against a different extension API version?)",
                    manifest.id
                )
            })?;
        let ext = LoadedExtension {
            manifest,
            dir,
            instance: Mutex::new(Instance { store, bindings }),
        };
        let init = ext.call(CallContext::default(), |b, s| b.call_init(s));
        init.result?;
        Ok(ext)
    }

    /// Run one export with `context` as the app's state, and collect what it
    /// asked for.
    fn call<T>(
        &self,
        context: CallContext,
        f: impl FnOnce(
            &bindings::Extension,
            &mut Store<HostState>,
        ) -> wasmtime::Result<Result<T, String>>,
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
        let Instance { store, bindings } = &mut *guard;
        store.data_mut().context = context;
        store.data_mut().effects.clear();
        let _ = store.set_fuel(FUEL_PER_CALL);
        let result = match f(bindings, store) {
            Ok(r) => r,
            Err(trap) => Err(format!("{} failed: {trap}", self.manifest.id)),
        };
        let effects = std::mem::take(&mut store.data_mut().effects);
        Outcome { result, effects }
    }

    pub fn run_command(&self, context: CallContext, id: &str) -> Outcome<()> {
        self.call(context, |b, s| b.call_run_command(s, id))
    }

    pub fn marker_clicked(&self, context: CallContext, id: &str) -> Outcome<()> {
        self.call(context, |b, s| b.call_marker_clicked(s, id))
    }

    pub fn panel_button(
        &self,
        context: CallContext,
        id: &str,
        inputs: &[(String, String)],
    ) -> Outcome<()> {
        self.call(context, |b, s| b.call_panel_button(s, id, inputs))
    }

    pub fn changeset_tags(&self, context: CallContext) -> Outcome<Vec<(String, String)>> {
        self.call(context, |b, s| b.call_changeset_tags(s).map(Ok))
    }

    pub fn upload_finished(&self, context: CallContext, changesets: &[i64]) -> Outcome<()> {
        self.call(context, |b, s| {
            b.call_upload_finished(s, changesets).map(Ok)
        })
    }
}
