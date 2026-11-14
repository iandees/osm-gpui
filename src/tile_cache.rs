use gpui::{Asset, BackgroundExecutor, ImageCacheError, RenderImage};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use crate::idle_tracker::IdleTracker;

/// Default cache budget (500 MB), used until overridden by
/// `set_budget_mb` (normally seeded from persisted `AppSettings.cache_budget_mb`
/// at startup).
const DEFAULT_CACHE_BUDGET_BYTES: u64 = 500 * 1024 * 1024;

/// After this many tile writes since the last eviction sweep, re-scan the
/// cache directory to check whether it's over budget. Avoids doing a full
/// `read_dir` walk on every single tile write while still keeping the cache
/// bounded in practice.
const WRITES_BETWEEN_EVICTION_CHECKS: u64 = 25;

/// How long a file count / usage summary scan is reused. The tile layer polls
/// the count every frame and the settings panel the summary on every repaint.
const STATS_TTL: std::time::Duration = std::time::Duration::from_secs(1);

/// Most tile-failure reasons kept for display; the oldest are dropped first.
/// Reasons are only shown in a failed tile, so losing an old one just shows a
/// generic "Failed" there.
const MAX_TILE_ERRORS: usize = 512;

/// The most recent failure reason per tile URL, bounded to `MAX_TILE_ERRORS`.
#[derive(Default)]
struct TileErrors {
    reasons: HashMap<String, String>,
    /// URLs in the order their reason was first recorded.
    order: VecDeque<String>,
}

impl TileErrors {
    fn insert(&mut self, url: &str, reason: String) {
        if self.reasons.insert(url.to_string(), reason).is_none() {
            self.order.push_back(url.to_string());
        }
        while self.reasons.len() > MAX_TILE_ERRORS {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.reasons.remove(&oldest);
                }
                None => break,
            }
        }
    }

    fn remove(&mut self, url: &str) {
        if self.reasons.remove(url).is_some() {
            self.order.retain(|u| u != url);
        }
    }
}

/// Everything the tile cache remembers while the app runs. Tiles load through
/// gpui's `Asset` trait, whose `load` is a static function with no way to
/// reach a `TileCache`, so there is one shared store (see `shared_store`) and
/// `TileCache` is a handle to it.
struct TileStore {
    /// Live cache size budget in bytes. Changing it never triggers an
    /// eviction sweep itself: it only changes what the next sweep evicts down
    /// to, so shrinking the budget doesn't cause an immediate mass deletion.
    budget_bytes: AtomicU64,
    /// Tile writes since the last eviction sweep check.
    writes_since_eviction: AtomicU64,
    /// Last "how many files are in the cache" scan and when it ran.
    file_count: Mutex<Option<(Instant, usize)>>,
    /// Last per-source usage scan and when it ran.
    summary: Mutex<Option<(Instant, CacheSummary)>>,
    /// Tile URLs each source's layer wanted on its most recent render, keyed
    /// by source key. A queued or in-flight download whose URL is no longer in
    /// its source's set (the tile scrolled or zoomed off the map) is cancelled.
    wanted: Mutex<HashMap<String, HashSet<String>>>,
    /// Tiles whose download was cancelled. gpui caches the (failed) asset
    /// result forever, so these must be evicted before they are requested again.
    cancelled: Mutex<HashSet<TileAssetKey>>,
    /// Last failure reason per tile URL, read when drawing a failed tile.
    errors: Mutex<TileErrors>,
    /// Tile loads started and not yet finished (queued for a download slot,
    /// downloading, or decoding).
    active_loads: AtomicUsize,
    /// Tells the app when tile fetches are in flight. Set once, by the first
    /// `TileCache` constructed.
    idle: OnceLock<Arc<IdleTracker>>,
}

impl TileStore {
    fn new() -> Self {
        Self {
            budget_bytes: AtomicU64::new(DEFAULT_CACHE_BUDGET_BYTES),
            writes_since_eviction: AtomicU64::new(0),
            file_count: Mutex::new(None),
            summary: Mutex::new(None),
            wanted: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(HashSet::new()),
            errors: Mutex::new(TileErrors::default()),
            active_loads: AtomicUsize::new(0),
            idle: OnceLock::new(),
        }
    }

    fn set_budget_mb(&self, mb: u64) {
        self.budget_bytes
            .store(mb.saturating_mul(1024 * 1024), Ordering::Relaxed);
    }

    fn budget_bytes(&self) -> u64 {
        self.budget_bytes.load(Ordering::Relaxed)
    }

    /// Called after every tile write. Bumps the running write counter and, once
    /// every `WRITES_BETWEEN_EVICTION_CHECKS` writes, performs a directory scan
    /// and evicts oldest-by-mtime files if the cache is over its budget.
    ///
    /// We deliberately scan on a write-count cadence rather than tracking a
    /// live running-size total: a live total would need to be correct across
    /// process restarts (files already on disk from a previous run) and after
    /// external deletion, which a periodic authoritative scan handles for free.
    fn maybe_evict(&self, dir: &Path) {
        let count = self.writes_since_eviction.fetch_add(1, Ordering::Relaxed) + 1;
        if count.is_multiple_of(WRITES_BETWEEN_EVICTION_CHECKS) {
            evict_if_over_budget(dir, self.budget_bytes());
        }
    }

    /// Number of files in the cache directory `dir`, rescanned at most once
    /// per `STATS_TTL`.
    fn cached_file_count(&self, dir: &Path) -> usize {
        let Ok(mut guard) = self.file_count.lock() else {
            return 0;
        };
        if let Some((last, count)) = *guard {
            if last.elapsed() < STATS_TTL {
                return count;
            }
        }
        let mut files = Vec::new();
        let mut total = 0u64;
        collect_cache_files(dir, &mut files, &mut total);
        let count = files.len();
        *guard = Some((Instant::now(), count));
        count
    }

    /// `cache_summary_at(dir)`, rescanned at most once per `STATS_TTL`.
    fn cache_summary(&self, dir: &Path) -> CacheSummary {
        let Ok(mut guard) = self.summary.lock() else {
            return cache_summary_at(dir);
        };
        if let Some((last, summary)) = guard.as_ref() {
            if last.elapsed() < STATS_TTL {
                return summary.clone();
            }
        }
        let summary = cache_summary_at(dir);
        *guard = Some((Instant::now(), summary.clone()));
        summary
    }

    /// See [`set_wanted_tiles`].
    fn set_wanted(&self, source_key: &str, urls: HashSet<String>) -> Vec<TileAssetKey> {
        let revived: Vec<TileAssetKey> = {
            let mut cancelled = self.cancelled.lock().unwrap();
            let revived: Vec<_> = cancelled
                .iter()
                .filter(|k| k.source_key == source_key && urls.contains(&k.url))
                .cloned()
                .collect();
            for k in &revived {
                cancelled.remove(k);
            }
            revived
        };
        self.wanted
            .lock()
            .unwrap()
            .insert(source_key.to_string(), urls);
        revived
    }

    /// Whether a download for `url` is still wanted. Sources that have not
    /// reported a wanted set (nothing rendered yet) are never cancelled.
    fn is_wanted(&self, source_key: &str, url: &str) -> bool {
        self.wanted
            .lock()
            .unwrap()
            .get(source_key)
            .is_none_or(|set| set.contains(url))
    }

    fn mark_cancelled(&self, key: TileAssetKey) {
        self.cancelled.lock().unwrap().insert(key);
    }

    fn record_error(&self, url: &str, reason: String) {
        if let Ok(mut errors) = self.errors.lock() {
            errors.insert(url, reason);
        }
    }

    fn clear_error(&self, url: &str) {
        if let Ok(mut errors) = self.errors.lock() {
            errors.remove(url);
        }
    }

    fn last_error(&self, url: &str) -> Option<String> {
        self.errors
            .lock()
            .ok()
            .and_then(|e| e.reasons.get(url).cloned())
    }

    /// Count a tile load as active until the returned guard is dropped.
    fn begin_load(&self) -> LoadGuard<'_> {
        self.active_loads.fetch_add(1, Ordering::Relaxed);
        LoadGuard { store: self }
    }

    fn active_loads(&self) -> usize {
        self.active_loads.load(Ordering::Relaxed)
    }
}

struct LoadGuard<'a> {
    store: &'a TileStore,
}

impl Drop for LoadGuard<'_> {
    fn drop(&mut self) {
        self.store.active_loads.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The process-wide tile store. A global because tiles load through gpui's
/// `Asset` trait: `TileAsset::load` is a static function that gets only the
/// asset key and the `App`, so it has no other way to reach the `TileCache`.
static SHARED_STORE: OnceLock<Arc<TileStore>> = OnceLock::new();

fn shared_store() -> &'static Arc<TileStore> {
    SHARED_STORE.get_or_init(|| Arc::new(TileStore::new()))
}

/// Update the live cache budget. Does not evict anything itself: the new
/// budget applies to the next eviction sweep.
pub fn set_budget_mb(mb: u64) {
    shared_store().set_budget_mb(mb);
}

/// The on-disk tile cache directory: the user cache location rather than the
/// OS temp dir, which can be wiped at any time.
fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("osm-gpui")
        .join("tiles")
}

/// Cache filename for `url`: a SHA-256 of the full URL. A collision-resistant
/// hash (unlike `DefaultHasher`) keeps two tile servers or templates from ever
/// sharing a cache file.
fn cache_filename(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let digest = hasher.finalize();
    format!("tile_{:x}.png", digest)
}

/// Derive a stable, human-legible cache subdirectory name from an imagery
/// source's URL *template* (e.g. `https://tile.openstreetmap.org/{z}/{x}/{y}.png`),
/// not a resolved per-tile URL. Using the template means a `{switch:a,b,c}`
/// rotating-subdomain source always maps to one stable key, regardless of
/// which subdomain a given tile happens to resolve to.
pub(crate) fn source_key_for_template(template: &str) -> String {
    let mut normalized = template.to_string();

    normalized = crate::tiles::collapse_switch(&normalized, "s").replace("{s}", "s");

    normalized = normalized.replace("{zoom}", "z");
    normalized = normalized.replace("{z}", "z");
    normalized = normalized.replace("{x}", "x");
    normalized = normalized.replace("{-y}", "negy");
    normalized = normalized.replace("{y}", "y");

    let without_scheme = normalized
        .strip_prefix("https://")
        .or_else(|| normalized.strip_prefix("http://"))
        .unwrap_or(&normalized);

    // Sanitize into a filesystem-safe slug: keep alphanumerics, collapse
    // every run of other characters (dots, slashes, query separators, …)
    // into a single underscore.
    let mut slug = String::with_capacity(without_scheme.len());
    let mut last_was_sep = false;
    for c in without_scheme.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            last_was_sep = false;
        } else if !last_was_sep {
            slug.push('_');
            last_was_sep = true;
        }
    }
    let slug = slug.trim_matches('_');
    let slug: String = slug.chars().take(60).collect();

    // Short hash suffix of the *original* template guarantees uniqueness
    // even if two different templates sanitize to the same slug, or the
    // slug was truncated.
    let mut hasher = Sha256::new();
    hasher.update(template.as_bytes());
    let digest = hasher.finalize();
    let hash_suffix = format!("{:x}", digest)[..8].to_string();

    if slug.is_empty() {
        hash_suffix
    } else {
        format!("{}-{}", slug, hash_suffix)
    }
}

/// Atomically write `bytes` to `file_path` (temp file, then rename), so
/// concurrent fetches for the same cache path never leave a torn file for a
/// reader to load. See `crate::persist::write_atomic`.
fn write_atomic(file_path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::persist::write_atomic(file_path, bytes, crate::persist::WriteOpts::default())
}

/// Recursively collect every cache file under `dir`, appending
/// `(path, size, mtime)` tuples and accumulating `total`.
fn collect_cache_files(dir: &Path, files: &mut Vec<(PathBuf, u64, SystemTime)>, total: &mut u64) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect_cache_files(&path, files, total);
        } else if meta.is_file() {
            let size = meta.len();
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            *total += size;
            files.push((path, size, mtime));
        }
    }
}

/// Scan `dir` and, if its total size exceeds `max_bytes`, delete
/// oldest-by-mtime files until it's back under budget. Entries whose
/// metadata/mtime can't be read are treated as newest (kept) rather than
/// causing a hard failure, since a partial cleanup is preferable to none.
fn evict_if_over_budget(dir: &Path, max_bytes: u64) {
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total: u64 = 0;
    collect_cache_files(dir, &mut files, &mut total);

    if total <= max_bytes {
        return;
    }

    files.sort_by_key(|(_, _, mtime)| *mtime);

    for (path, size, _) in files {
        if total <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// Record the tile URLs a layer of `source_key` needs right now (replacing
/// its previous set), and return the previously cancelled tiles among them,
/// which the caller must evict from gpui's asset cache so they refetch.
pub fn set_wanted_tiles(source_key: &str, urls: HashSet<String>) -> Vec<TileAssetKey> {
    shared_store().set_wanted(source_key, urls)
}

/// Look up the most recent failure reason for a tile URL, if any.
pub fn last_error(url: &str) -> Option<String> {
    shared_store().last_error(url)
}

/// Truncate `s` to at most `max` characters, replacing the middle with "..."
/// when the string is over budget. Operates on chars, not bytes, so it is
/// safe for non-ASCII inputs.
pub fn truncate_middle(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max <= 3 {
        return s.chars().take(max).collect();
    }
    let keep = max - 3;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let head_str: String = s.chars().take(head).collect();
    let tail_str: String = s.chars().skip(count - tail).collect();
    format!("{head_str}...{tail_str}")
}

/// Typed error for the synchronous tile fetch path. The `Display` impl is
/// designed to render compactly inside a tile (e.g. "HTTP 404",
/// "Transport: Dns", "Empty body").
#[derive(Debug)]
pub enum TileFetchError {
    Http {
        status: u16,
        body_snippet: Option<String>,
    },
    Transport(String),
    EmptyBody,
    NotImage,
    Io(String),
    /// The tile was no longer wanted (scrolled off the map) before it finished.
    Cancelled,
}

impl fmt::Display for TileFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileFetchError::Http {
                status,
                body_snippet,
            } => match body_snippet {
                Some(snippet) if !snippet.is_empty() => {
                    write!(f, "HTTP {}: {}", status, snippet)
                }
                _ => write!(f, "HTTP {}", status),
            },
            TileFetchError::Transport(kind) => write!(f, "Transport: {}", kind),
            TileFetchError::EmptyBody => write!(f, "Empty body"),
            TileFetchError::NotImage => write!(f, "Not an image"),
            TileFetchError::Io(msg) => write!(f, "Disk: {}", msg),
            TileFetchError::Cancelled => write!(f, "Cancelled"),
        }
    }
}

impl std::error::Error for TileFetchError {}

pub struct TileAsset;

/// Identifies both the concrete tile URL to fetch and the cache
/// subdirectory (derived from the source's URL template via
/// `source_key_for_template`) it belongs in.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct TileAssetKey {
    pub url: String,
    pub source_key: String,
}

/// Compose the on-disk path for one cached tile: `<base_dir>/<source_key>/tile_<hash>.png`.
fn tile_file_path(base_dir: &Path, source_key: &str, url: &str) -> PathBuf {
    base_dir.join(source_key).join(cache_filename(url))
}

impl Asset for TileAsset {
    type Source = TileAssetKey;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        key: Self::Source,
        cx: &mut gpui::App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let executor = cx.background_executor().clone();
        let store = shared_store().clone();
        let idle = store.idle.get().cloned();
        // Cloned here, on the caller's thread, so the background task owns its
        // own handle.
        let client = crate::http::default_client();

        async move {
            let _active = store.begin_load();
            if let Some(ref tracker) = idle {
                tracker.tile_fetch_started();
            }
            // The blocking HTTP request runs on the background executor.
            let result = executor
                .spawn({
                    let store = store.clone();
                    async move { load_tile(&store, client.as_ref(), &cache_dir(), &key) }
                })
                .await;
            // Exactly one finished call for the one started call above,
            // whichever way the load ended.
            if let Some(ref tracker) = idle {
                tracker.tile_fetch_finished();
            }
            result
        }
    }
}

fn cache_error(reason: String) -> ImageCacheError {
    ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason)))
}

/// Record `reason` as `url`'s failure (shown inside the tile) and return it as
/// the asset's error.
fn fail_tile(store: &TileStore, url: &str, reason: String) -> ImageCacheError {
    store.record_error(url, reason.clone());
    cache_error(reason)
}

/// Load one tile: from the disk cache under `base_dir` if it is there,
/// otherwise download it through `client`, cache it and decode it. Failures
/// are recorded in `store` for the tile's fallback to show; a download
/// abandoned because the tile scrolled off the map is remembered as cancelled.
fn load_tile(
    store: &TileStore,
    client: &dyn crate::http::HttpClient,
    base_dir: &Path,
    key: &TileAssetKey,
) -> Result<Arc<RenderImage>, ImageCacheError> {
    let TileAssetKey { url, source_key } = key;
    let file_path = tile_file_path(base_dir, source_key, url);

    if let CacheRead::Hit(image) = read_cached(&file_path) {
        store.clear_error(url);
        return Ok(Arc::new(image));
    }

    // Created even if the download then fails, so a failing source still
    // shows (empty) in the cache settings.
    if let Some(source_dir) = file_path.parent() {
        if let Err(e) = fs::create_dir_all(source_dir) {
            let reason = TileFetchError::Io(format!("mkdir: {}", e)).to_string();
            return Err(fail_tile(store, url, reason));
        }
    }

    let keep = || store.is_wanted(source_key, url);
    let bytes = match fetch(client, url, &keep) {
        Ok(bytes) => bytes,
        Err(TileFetchError::Cancelled) => {
            store.mark_cancelled(key.clone());
            return Err(cache_error("cancelled".to_string()));
        }
        Err(e) => return Err(fail_tile(store, url, e.to_string())),
    };

    if let Err(e) = write_atomic(&file_path, &bytes) {
        let reason = TileFetchError::Io(format!("write: {}", e)).to_string();
        return Err(fail_tile(store, url, reason));
    }
    store.maybe_evict(base_dir);

    match decode(&bytes) {
        Ok(image) => {
            store.clear_error(url);
            Ok(Arc::new(image))
        }
        Err(e) => Err(fail_tile(store, url, format!("Decode: {}", e))),
    }
}

/// What looking for a tile in the disk cache found.
enum CacheRead {
    Hit(RenderImage),
    Miss,
    /// A file was there but would not decode; it has been deleted.
    Corrupt,
}

/// The cached tile at `path`, if any. A corrupt file is deleted so the tile is
/// downloaded again.
fn read_cached(path: &Path) -> CacheRead {
    if !path.exists() {
        return CacheRead::Miss;
    }
    match fs::read(path)
        .map_err(|e| format!("Failed to read image file: {}", e))
        .and_then(|bytes| decode(&bytes))
    {
        Ok(image) => CacheRead::Hit(image),
        Err(_) => {
            let _ = fs::remove_file(path);
            CacheRead::Corrupt
        }
    }
}

/// What a downloaded body looks like, judged by its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SniffedImage {
    Png,
    Jpeg,
    /// Not a format tiles are accepted in (an HTML error page, WebP, ...).
    Other,
}

fn sniff_image(bytes: &[u8]) -> SniffedImage {
    // PNG starts 0x89 "PNG\r\n\x1a\n"; the 0x89 is not checked.
    if bytes.len() >= 8 && &bytes[1..4] == b"PNG" {
        SniffedImage::Png
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        SniffedImage::Jpeg
    } else {
        SniffedImage::Other
    }
}

/// Download `url`, giving up with `Cancelled` once `keep` says the tile is no
/// longer wanted, and check that the body is a PNG or JPEG.
fn fetch(
    client: &dyn crate::http::HttpClient,
    url: &str,
    keep: &dyn Fn() -> bool,
) -> Result<Vec<u8>, TileFetchError> {
    if !keep() {
        return Err(TileFetchError::Cancelled);
    }
    let bytes = download_file_cancellable(client, url, keep)?;
    if bytes.is_empty() {
        return Err(TileFetchError::EmptyBody);
    }
    match sniff_image(&bytes) {
        SniffedImage::Other => Err(TileFetchError::NotImage),
        SniffedImage::Png | SniffedImage::Jpeg => Ok(bytes),
    }
}

/// Max number of tile downloads allowed to be in flight at once, process-wide.
///
/// The OSM tile usage policy asks clients to keep to ~2 concurrent connections
/// per host. Custom imagery URLs can point anywhere, so rather than track
/// hosts this caps total downloads process-wide; nearly all traffic goes to
/// one tile host at a time, so it behaves close to a per-host cap.
const MAX_CONCURRENT_TILE_DOWNLOADS: usize = 4;

/// A simple blocking counting semaphore built on `Mutex` + `Condvar`, used to
/// cap concurrent tile downloads without pulling in an async runtime.
struct Semaphore {
    count: Mutex<usize>,
    cond: Condvar,
    max: usize,
}

impl Semaphore {
    fn new(max: usize) -> Self {
        Self {
            count: Mutex::new(0),
            cond: Condvar::new(),
            max,
        }
    }

    /// Block until a permit is available, then hold it until the returned
    /// guard is dropped.
    fn acquire(&self) -> SemaphoreGuard<'_> {
        let mut count = self.count.lock().unwrap();
        while *count >= self.max {
            count = self.cond.wait(count).unwrap();
        }
        *count += 1;
        SemaphoreGuard { sem: self }
    }
}

struct SemaphoreGuard<'a> {
    sem: &'a Semaphore,
}

impl Drop for SemaphoreGuard<'_> {
    fn drop(&mut self) {
        let mut count = self.sem.count.lock().unwrap();
        *count = count.saturating_sub(1);
        self.sem.cond.notify_one();
    }
}

static TILE_DOWNLOAD_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

fn tile_download_semaphore() -> &'static Semaphore {
    TILE_DOWNLOAD_SEMAPHORE.get_or_init(|| Semaphore::new(MAX_CONCURRENT_TILE_DOWNLOADS))
}

/// Up to this many bytes of an error response body are kept (as a sanitized snippet)
/// for display inside a tile.
const ERROR_SNIPPET_BYTES: usize = 120;

/// `download_file_cancellable` that is never cancelled.
#[cfg(test)]
pub(crate) fn download_file_sync_with(
    client: &dyn crate::http::HttpClient,
    url: &str,
) -> Result<Vec<u8>, TileFetchError> {
    download_file_cancellable(client, url, &|| true)
}

/// Download `url`, abandoning it (with `TileFetchError::Cancelled`) once
/// `keep` returns false. `keep` is checked after waiting for a download slot
/// and while the body is being read.
pub(crate) fn download_file_cancellable(
    client: &dyn crate::http::HttpClient,
    url: &str,
    keep: &dyn Fn() -> bool,
) -> Result<Vec<u8>, TileFetchError> {
    // Held for the whole request, retries included.
    let _permit = tile_download_semaphore().acquire();
    if !keep() {
        return Err(TileFetchError::Cancelled);
    }

    let req =
        crate::http::HttpRequest::get(url).header("Referer", "https://github.com/iandees/osm-gpui");

    match crate::http::fetch_with_retries_progress(
        client,
        &req,
        &crate::http::RetryPolicy::standard(),
        &mut |_, _| keep(),
    ) {
        Ok(resp) => Ok(resp.body),
        Err(crate::http::HttpError::Status { status, body }) => {
            let snippet = sanitize_snippet(&body);
            Err(TileFetchError::Http {
                status,
                body_snippet: snippet,
            })
        }
        Err(crate::http::HttpError::Transport(msg)) => Err(TileFetchError::Transport(msg)),
        Err(crate::http::HttpError::Cancelled) => Err(TileFetchError::Cancelled),
    }
}

/// Sanitize up to `ERROR_SNIPPET_BYTES` bytes of an error body into a short,
/// control-character-free snippet suitable for display inside a tile.
fn sanitize_snippet(body: &[u8]) -> Option<String> {
    let truncated = &body[..body.len().min(ERROR_SNIPPET_BYTES)];
    if truncated.is_empty() {
        return None;
    }
    let raw = String::from_utf8_lossy(truncated);
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Decode image bytes into the BGRA frame gpui draws.
fn decode(bytes: &[u8]) -> Result<RenderImage, String> {
    let img =
        image::load_from_memory(bytes).map_err(|e| format!("Failed to decode image: {}", e))?;

    let mut rgba = img.to_rgba8();
    // gpui wants BGRA: swap red and blue.
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }

    let mut frames = smallvec::SmallVec::new();
    frames.push(image::Frame::new(rgba));
    Ok(RenderImage::new(frames))
}

/// A handle to the shared tile store: what the app holds on to and asks for
/// statistics. The tiles themselves load through gpui's asset system.
#[derive(Clone)]
pub struct TileCache {
    store: Arc<TileStore>,
}

impl TileCache {
    pub fn new(_executor: BackgroundExecutor, idle: Arc<IdleTracker>) -> Self {
        let store = shared_store().clone();
        // The first tracker wins; later caches (in tests) share it.
        let _ = store.idle.set(idle);
        Self { store }
    }

    /// `(cached files, tile loads in flight)`. The file count is rescanned at
    /// most once a second so calling this every render frame doesn't hit the
    /// filesystem every time.
    pub fn stats(&self) -> (usize, usize) {
        (
            self.store.cached_file_count(&cache_dir()),
            self.store.active_loads(),
        )
    }
}

/// Per-source-key aggregate cache usage, as reported by `cache_summary`.
#[derive(Clone)]
pub struct CacheSourceUsage {
    pub key: String,
    pub bytes: u64,
    pub file_count: usize,
}

/// Aggregate tile cache usage across every source, plus a per-source
/// breakdown. Computed via a fresh, on-demand directory scan — safe to call
/// only occasionally (e.g. when a settings panel is open), not on a hot
/// per-frame path (see `cached_file_count` for that).
#[derive(Clone)]
pub struct CacheSummary {
    pub total_bytes: u64,
    pub total_files: usize,
    /// Sorted by `bytes`, descending.
    pub sources: Vec<CacheSourceUsage>,
}

/// Bucket for cache files that aren't inside any source-key subdirectory.
const UNCATEGORIZED_KEY: &str = "(uncategorized)";

/// The uncategorized bucket key, for callers outside this module (the
/// settings UI).
pub(crate) fn uncategorized_key() -> &'static str {
    UNCATEGORIZED_KEY
}

/// Aggregate usage of the real tile cache directory; see `TileStore::cache_summary`.
pub fn cache_summary() -> CacheSummary {
    shared_store().cache_summary(&cache_dir())
}

fn cache_summary_at(dir: &Path) -> CacheSummary {
    let mut per_source: HashMap<String, (u64, usize)> = HashMap::new();
    let mut total_bytes = 0u64;
    let mut total_files = 0usize;

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            return CacheSummary {
                total_bytes: 0,
                total_files: 0,
                sources: Vec::new(),
            };
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            let key = entry.file_name().to_string_lossy().to_string();
            let mut files = Vec::new();
            let mut bytes = 0u64;
            collect_cache_files(&path, &mut files, &mut bytes);
            total_bytes += bytes;
            total_files += files.len();
            per_source.insert(key, (bytes, files.len()));
        } else if meta.is_file() {
            let size = meta.len();
            total_bytes += size;
            total_files += 1;
            let bucket = per_source
                .entry(UNCATEGORIZED_KEY.to_string())
                .or_insert((0, 0));
            bucket.0 += size;
            bucket.1 += 1;
        }
    }

    let mut sources: Vec<CacheSourceUsage> = per_source
        .into_iter()
        .map(|(key, (bytes, file_count))| CacheSourceUsage {
            key,
            bytes,
            file_count,
        })
        .collect();
    sources.sort_by_key(|s| std::cmp::Reverse(s.bytes));

    CacheSummary {
        total_bytes,
        total_files,
        sources,
    }
}

/// Remove all cached tiles for one source (or the `"(uncategorized)"`
/// bucket of loose top-level files). A missing directory is not an error.
pub fn clear_source(key: &str) -> std::io::Result<()> {
    clear_source_at(&cache_dir(), key)
}

fn clear_source_at(dir: &Path, key: &str) -> std::io::Result<()> {
    if key == UNCATEGORIZED_KEY {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                fs::remove_file(&path)?;
            }
        }
        return Ok(());
    }

    // Defense in depth: keys normally come from `cache_summary()` (sanitized
    // directory names, see `source_key_for_template`), but never join a path
    // separator or traversal sequence onto the cache directory before
    // `remove_dir_all`.
    if key.contains('/') || key.contains('\\') || key.contains("..") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to clear cache source with unsafe key: {key:?}"),
        ));
    }

    let source_dir = dir.join(key);
    if source_dir.exists() {
        fs::remove_dir_all(&source_dir)?;
    }
    Ok(())
}

/// Remove the entire tile cache. Recreated lazily on the next tile write.
pub fn clear_all_cache() -> std::io::Result<()> {
    clear_all_cache_at(&cache_dir())
}

fn clear_all_cache_at(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

/// Format a byte count as a short, human-readable string (e.g. "342 MB",
/// "1.2 GB"), for display in the cache settings panel.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semaphore_caps_concurrent_holders() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;

        let sem = Arc::new(Semaphore::new(2));
        let current = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let sem = sem.clone();
                let current = current.clone();
                let max_seen = max_seen.clone();
                thread::spawn(move || {
                    let _permit = sem.acquire();
                    let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    thread::sleep(std::time::Duration::from_millis(10));
                    current.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        assert!(max_seen.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn truncate_middle_short() {
        assert_eq!(truncate_middle("abc", 10), "abc");
        assert_eq!(truncate_middle("abcdefghij", 10), "abcdefghij");
    }

    #[test]
    fn truncate_middle_long() {
        // 20 chars truncated to 10 -> 4 head + "..." + 3 tail = 10
        let out = truncate_middle("abcdefghijklmnopqrst", 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.starts_with("abcd"));
        assert!(out.ends_with("rst"));
        assert!(out.contains("..."));
    }

    #[test]
    fn truncate_middle_tiny_budget() {
        assert_eq!(truncate_middle("abcdef", 3), "abc");
        assert_eq!(truncate_middle("abcdef", 2), "ab");
        assert_eq!(truncate_middle("abcdef", 0), "");
    }

    #[test]
    fn truncate_middle_unicode() {
        // Characters >1 byte; ensure we slice on char boundaries.
        let out = truncate_middle(
            "\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}\u{516d}\u{4e03}\u{516b}",
            5,
        );
        assert_eq!(out.chars().count(), 5);
        assert!(out.contains("..."));
    }

    #[test]
    fn display_http_no_snippet() {
        let e = TileFetchError::Http {
            status: 404,
            body_snippet: None,
        };
        assert_eq!(e.to_string(), "HTTP 404");
    }

    #[test]
    fn display_http_with_snippet() {
        let e = TileFetchError::Http {
            status: 503,
            body_snippet: Some("Over capacity".to_string()),
        };
        assert_eq!(e.to_string(), "HTTP 503: Over capacity");
    }

    #[test]
    fn display_http_empty_snippet_falls_back() {
        let e = TileFetchError::Http {
            status: 500,
            body_snippet: Some(String::new()),
        };
        assert_eq!(e.to_string(), "HTTP 500");
    }

    #[test]
    fn display_other_variants() {
        assert_eq!(
            TileFetchError::Transport("Dns".into()).to_string(),
            "Transport: Dns"
        );
        assert_eq!(TileFetchError::EmptyBody.to_string(), "Empty body");
        assert_eq!(TileFetchError::NotImage.to_string(), "Not an image");
        assert_eq!(
            TileFetchError::Io("write: nope".into()).to_string(),
            "Disk: write: nope"
        );
    }

    #[test]
    fn download_file_sync_with_retries_then_succeeds() {
        use crate::http::fake::{ok, status_err, FakeClient};
        let client = FakeClient::new(vec![status_err(503, "busy"), ok(200, vec![1u8, 2, 3])]);
        let bytes =
            download_file_sync_with(&client, "https://tile.example.test/1/2/3.png").unwrap();
        assert_eq!(bytes, vec![1u8, 2, 3]);
        assert_eq!(client.request_count(), 2);
    }

    #[test]
    fn download_file_sync_with_maps_http_error_to_snippet() {
        use crate::http::fake::{status_err, FakeClient};
        let client = FakeClient::new(vec![status_err(404, "not found here")]);
        let err =
            download_file_sync_with(&client, "https://tile.example.test/1/2/3.png").unwrap_err();
        match err {
            TileFetchError::Http {
                status,
                body_snippet,
            } => {
                assert_eq!(status, 404);
                assert_eq!(body_snippet.as_deref(), Some("not found here"));
            }
            other => panic!("expected Http error, got {:?}", other),
        }
    }

    #[test]
    fn download_file_sync_with_maps_transport_error() {
        use crate::http::fake::{transport_err, FakeClient};
        let client = FakeClient::new(vec![
            transport_err("dns"),
            transport_err("dns"),
            transport_err("dns"),
        ]);
        let err =
            download_file_sync_with(&client, "https://tile.example.test/1/2/3.png").unwrap_err();
        assert!(matches!(err, TileFetchError::Transport(_)));
    }

    #[test]
    fn cancelled_download_makes_no_request() {
        let client =
            crate::http::fake::FakeClient::new(vec![crate::http::fake::ok(200, b"x".to_vec())]);
        let err = download_file_cancellable(&client, "http://t/1.png", &|| false).unwrap_err();
        assert!(matches!(err, TileFetchError::Cancelled));
        assert_eq!(client.request_count(), 0);
    }

    #[test]
    fn unwanted_tiles_are_cancelled_and_revived_when_wanted_again() {
        let store = TileStore::new();
        let src = "test-src-cancel";
        assert!(store.is_wanted(src, "u1"));
        store.set_wanted(src, HashSet::from(["u2".to_string()]));
        assert!(!store.is_wanted(src, "u1"));
        assert!(store.is_wanted(src, "u2"));
        let key = TileAssetKey {
            url: "u1".into(),
            source_key: src.into(),
        };
        store.mark_cancelled(key.clone());
        let revived = store.set_wanted(src, HashSet::from(["u1".to_string()]));
        assert!(revived.contains(&key));
        assert!(store
            .set_wanted(src, HashSet::from(["u1".to_string()]))
            .is_empty());
    }

    #[test]
    fn record_and_clear_error() {
        let store = TileStore::new();
        let url = "https://example.test/record_and_clear/1.png";
        store.record_error(url, "HTTP 418".to_string());
        assert_eq!(store.last_error(url).as_deref(), Some("HTTP 418"));
        store.clear_error(url);
        assert_eq!(store.last_error(url), None);
    }

    #[test]
    fn error_reasons_are_bounded_and_drop_the_oldest_first() {
        let store = TileStore::new();
        for i in 0..MAX_TILE_ERRORS + 10 {
            store.record_error(&format!("u{i}"), format!("HTTP {i}"));
        }
        let errors = store.errors.lock().unwrap();
        assert_eq!(errors.reasons.len(), MAX_TILE_ERRORS);
        assert_eq!(errors.order.len(), MAX_TILE_ERRORS);
        drop(errors);
        assert_eq!(store.last_error("u0"), None);
        assert_eq!(store.last_error("u9"), None);
        assert_eq!(store.last_error("u10").as_deref(), Some("HTTP 10"));
        let newest = MAX_TILE_ERRORS + 9;
        assert_eq!(
            store.last_error(&format!("u{newest}")).as_deref(),
            Some(format!("HTTP {newest}").as_str())
        );
    }

    #[test]
    fn re_recording_and_clearing_errors_does_not_grow_the_order_queue() {
        let store = TileStore::new();
        for round in 0..50 {
            store.record_error("same", format!("try {round}"));
            store.record_error("same", format!("again {round}"));
            store.clear_error("same");
        }
        store.record_error("same", "last".into());
        let errors = store.errors.lock().unwrap();
        assert_eq!(errors.reasons.len(), 1);
        assert_eq!(errors.order.len(), 1);
    }

    #[test]
    fn active_loads_count_until_the_guard_drops() {
        let store = TileStore::new();
        assert_eq!(store.active_loads(), 0);
        let a = store.begin_load();
        let b = store.begin_load();
        assert_eq!(store.active_loads(), 2);
        drop(a);
        assert_eq!(store.active_loads(), 1);
        drop(b);
        assert_eq!(store.active_loads(), 0);
    }

    #[test]
    fn the_handle_reports_the_shared_stores_active_loads() {
        let handle = TileCache {
            store: shared_store().clone(),
        };
        let before = handle.stats().1;
        let guard = handle.store.begin_load();
        assert_eq!(handle.stats().1, before + 1);
        drop(guard);
        assert_eq!(handle.stats().1, before);
    }

    #[test]
    fn cache_filename_is_deterministic() {
        let url = "https://tile.example.test/1/2/3.png";
        assert_eq!(cache_filename(url), cache_filename(url));
    }

    #[test]
    fn cache_filename_differs_for_different_urls() {
        let a = cache_filename("https://tile-a.example.test/1/2/3.png");
        let b = cache_filename("https://tile-b.example.test/1/2/3.png");
        assert_ne!(a, b);
    }

    #[test]
    fn cache_filename_has_expected_shape() {
        let name = cache_filename("https://tile.example.test/1/2/3.png");
        assert!(name.starts_with("tile_"));
        assert!(name.ends_with(".png"));
        // "tile_" + 64 hex chars (SHA-256) + ".png"
        assert_eq!(name.len(), "tile_".len() + 64 + ".png".len());
    }

    /// Two different tile-server URLs must never produce the same cache
    /// filename.
    #[test]
    fn cache_filename_no_collision_across_many_urls() {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for i in 0..500 {
            let url = format!(
                "https://server-{}.example.test/{}/{}/{}.png",
                i % 5,
                i,
                i + 1,
                i + 2
            );
            assert!(seen.insert(cache_filename(&url)), "collision for {url}");
        }
    }

    #[test]
    fn write_atomic_leaves_no_tmp_files_and_full_content() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-write-atomic-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("tile_test.png");

        let payload = vec![0xABu8; 4096];
        write_atomic(&target, &payload).expect("atomic write should succeed");

        // Target file exists with the full, untruncated content.
        let written = fs::read(&target).expect("target file should exist");
        assert_eq!(written, payload);

        // No leftover .tmp.* siblings.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {:?}", leftovers);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_overwrites_existing_file_fully() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-write-atomic-overwrite-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("tile_test.png");

        write_atomic(&target, &[0x11u8; 10]).unwrap();
        write_atomic(&target, &[0x22u8; 20]).unwrap();

        let written = fs::read(&target).unwrap();
        assert_eq!(written, vec![0x22u8; 20]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn evict_if_over_budget_removes_oldest_first() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-evict-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Three 100-byte files, written with explicit, distinct mtimes so
        // ordering is deterministic (no reliance on sleeping/wall-clock
        // timing between writes).
        let now = SystemTime::now();
        let paths = ["oldest.png", "middle.png", "newest.png"];
        for (i, name) in paths.iter().enumerate() {
            let p = dir.join(name);
            fs::write(&p, vec![0u8; 100]).unwrap();
            let mtime = now - std::time::Duration::from_secs((paths.len() - i) as u64 * 60);
            let file = fs::File::open(&p).unwrap();
            file.set_modified(mtime).unwrap();
        }

        // Budget allows only ~1 file; the two oldest should be evicted,
        // leaving "newest.png" behind.
        evict_if_over_budget(&dir, 150);

        let remaining: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(remaining, vec!["newest.png".to_string()]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn evict_if_over_budget_noop_when_under_budget() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-evict-noop-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.png"), vec![0u8; 10]).unwrap();

        evict_if_over_budget(&dir, 1_000_000);

        let remaining: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(remaining.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_count_is_reused_within_the_ttl_and_rescanned_after() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-file-count-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.png"), [0u8; 4]).unwrap();

        let store = TileStore::new();
        assert_eq!(store.cached_file_count(&dir), 1);
        fs::write(dir.join("b.png"), [0u8; 4]).unwrap();
        // Pretend the scan is fresh: the stale count is returned.
        *store.file_count.lock().unwrap() = Some((Instant::now(), 1));
        assert_eq!(store.cached_file_count(&dir), 1);
        // Pretend it is old: a rescan sees both files.
        *store.file_count.lock().unwrap() = Some((Instant::now() - STATS_TTL * 2, 1));
        assert_eq!(store.cached_file_count(&dir), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_count_of_a_missing_directory_is_zero() {
        let dir = std::env::temp_dir().join("osm-gpui-test-file-count-missing-dir");
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(TileStore::new().cached_file_count(&dir), 0);
    }

    #[test]
    fn source_key_deterministic_for_same_template() {
        let a = source_key_for_template("https://tile.openstreetmap.org/{z}/{x}/{y}.png");
        let b = source_key_for_template("https://tile.openstreetmap.org/{z}/{x}/{y}.png");
        assert_eq!(a, b);
    }

    #[test]
    fn source_key_differs_for_different_templates() {
        let a = source_key_for_template("https://tile-a.example.test/{z}/{x}/{y}.png");
        let b = source_key_for_template("https://tile-b.example.test/{z}/{x}/{y}.png");
        assert_ne!(a, b);
    }

    #[test]
    fn source_key_is_filesystem_safe() {
        let key =
            source_key_for_template("https://tile.example.test/a?b={z}/{x}/{y}.png&key=SECRET123");
        assert!(key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }

    #[test]
    fn source_key_ignores_switch_subdomain_rotation() {
        // A `{switch:a,b,c}` template must produce one stable key, since
        // `url_from_template` picks a different literal subdomain per tile.
        let template = "https://{switch:a,b,c}.tile.example.test/{z}/{x}/{y}.png";
        let key1 = source_key_for_template(template);
        let key2 = source_key_for_template(template);
        assert_eq!(key1, key2);
    }

    #[test]
    fn source_key_has_readable_prefix() {
        let key = source_key_for_template("https://tile.openstreetmap.org/{z}/{x}/{y}.png");
        assert!(key.starts_with("tile_openstreetmap_org_z_x_y"));
    }

    #[test]
    fn tile_file_path_differs_by_source_key() {
        let base = Path::new("/cache/tiles");
        let url = "https://tile.example.test/1/2/3.png";
        let a = tile_file_path(base, "source-a", url);
        let b = tile_file_path(base, "source-b", url);
        assert_ne!(a, b);
        assert!(a.starts_with(base.join("source-a")));
        assert!(b.starts_with(base.join("source-b")));
    }

    #[test]
    fn tile_file_path_deterministic() {
        let base = Path::new("/cache/tiles");
        let url = "https://tile.example.test/1/2/3.png";
        assert_eq!(
            tile_file_path(base, "source-a", url),
            tile_file_path(base, "source-a", url)
        );
    }

    #[test]
    fn collect_cache_files_sums_nested_directories() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-collect-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("source-a")).unwrap();
        fs::create_dir_all(dir.join("source-b")).unwrap();
        fs::write(dir.join("source-a").join("a.png"), vec![0u8; 100]).unwrap();
        fs::write(dir.join("source-b").join("b.png"), vec![0u8; 50]).unwrap();
        fs::write(dir.join("loose.png"), vec![0u8; 10]).unwrap();

        let mut files = Vec::new();
        let mut total = 0u64;
        collect_cache_files(&dir, &mut files, &mut total);

        assert_eq!(total, 160);
        assert_eq!(files.len(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn evict_if_over_budget_scans_nested_source_directories() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-evict-nested-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let source_a = dir.join("source-a");
        let source_b = dir.join("source-b");
        fs::create_dir_all(&source_a).unwrap();
        fs::create_dir_all(&source_b).unwrap();

        let now = SystemTime::now();
        let write = |path: &Path, age_secs: u64| {
            fs::write(path, vec![0u8; 100]).unwrap();
            let file = fs::File::open(path).unwrap();
            file.set_modified(now - std::time::Duration::from_secs(age_secs))
                .unwrap();
        };
        write(&source_a.join("oldest.png"), 180);
        write(&source_b.join("middle.png"), 120);
        write(&source_a.join("newest.png"), 60);

        // Budget allows only ~1 file; the two oldest (across both source
        // directories) should be evicted, leaving "newest.png" behind.
        evict_if_over_budget(&dir, 150);

        let mut files = Vec::new();
        let mut total = 0u64;
        collect_cache_files(&dir, &mut files, &mut total);
        let mut remaining: Vec<String> = files
            .into_iter()
            .map(|(path, _, _)| path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        remaining.sort();
        assert_eq!(remaining, vec!["newest.png".to_string()]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_summary_groups_by_source_and_buckets_loose_files() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-summary-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("source-a")).unwrap();
        fs::create_dir_all(dir.join("source-b")).unwrap();
        fs::write(dir.join("source-a").join("a1.png"), vec![0u8; 100]).unwrap();
        fs::write(dir.join("source-a").join("a2.png"), vec![0u8; 50]).unwrap();
        fs::write(dir.join("source-b").join("b1.png"), vec![0u8; 20]).unwrap();
        fs::write(dir.join("stray.png"), vec![0u8; 5]).unwrap();

        let summary = cache_summary_at(&dir);

        assert_eq!(summary.total_bytes, 175);
        assert_eq!(summary.total_files, 4);
        assert_eq!(summary.sources.len(), 3);

        let source_a = summary
            .sources
            .iter()
            .find(|s| s.key == "source-a")
            .unwrap();
        assert_eq!(source_a.bytes, 150);
        assert_eq!(source_a.file_count, 2);

        let uncategorized = summary
            .sources
            .iter()
            .find(|s| s.key == UNCATEGORIZED_KEY)
            .unwrap();
        assert_eq!(uncategorized.bytes, 5);
        assert_eq!(uncategorized.file_count, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_summary_sorts_sources_by_size_descending() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-summary-sort-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("small")).unwrap();
        fs::create_dir_all(dir.join("big")).unwrap();
        fs::write(dir.join("small").join("f.png"), vec![0u8; 10]).unwrap();
        fs::write(dir.join("big").join("f.png"), vec![0u8; 1000]).unwrap();

        let summary = cache_summary_at(&dir);
        assert_eq!(summary.sources[0].key, "big");
        assert_eq!(summary.sources[1].key, "small");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_summary_missing_dir_is_empty() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-summary-missing-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let summary = cache_summary_at(&dir);
        assert_eq!(summary.total_bytes, 0);
        assert_eq!(summary.total_files, 0);
        assert!(summary.sources.is_empty());
    }

    #[test]
    fn clear_source_removes_only_targeted_subdirectory() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-clear-source-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("source-a")).unwrap();
        fs::create_dir_all(dir.join("source-b")).unwrap();
        fs::write(dir.join("source-a").join("a.png"), vec![0u8; 10]).unwrap();
        fs::write(dir.join("source-b").join("b.png"), vec![0u8; 10]).unwrap();

        clear_source_at(&dir, "source-a").unwrap();

        assert!(!dir.join("source-a").exists());
        assert!(dir.join("source-b").join("b.png").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_source_uncategorized_removes_only_loose_files() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-clear-uncategorized-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("source-a")).unwrap();
        fs::write(dir.join("source-a").join("a.png"), vec![0u8; 10]).unwrap();
        fs::write(dir.join("stray.png"), vec![0u8; 10]).unwrap();

        clear_source_at(&dir, UNCATEGORIZED_KEY).unwrap();

        assert!(!dir.join("stray.png").exists());
        assert!(dir.join("source-a").join("a.png").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_source_rejects_path_traversal_key() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-clear-traversal-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("source-a")).unwrap();
        fs::write(dir.join("source-a").join("a.png"), vec![0u8; 10]).unwrap();

        let result = clear_source_at(&dir, "../source-a");
        assert!(result.is_err());
        // Sibling directory must be untouched since the call was rejected
        // before ever reaching `remove_dir_all`.
        assert!(dir.join("source-a").join("a.png").exists());

        let result = clear_source_at(&dir, "source-a/../source-a");
        assert!(result.is_err());
        assert!(dir.join("source-a").join("a.png").exists());

        let result = clear_source_at(&dir, "nested/traversal");
        assert!(result.is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_source_still_accepts_uncategorized_key() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-clear-uncategorized-safe-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("stray.png"), vec![0u8; 10]).unwrap();

        assert!(clear_source_at(&dir, UNCATEGORIZED_KEY).is_ok());
        assert!(!dir.join("stray.png").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_summary_is_reused_within_the_ttl() {
        // A fresh cached value is returned unchanged even though the
        // directory (which we never create) would give an empty summary.
        let store = TileStore::new();
        let cached = CacheSummary {
            total_bytes: 12345,
            total_files: 7,
            sources: vec![CacheSourceUsage {
                key: "fake-source".to_string(),
                bytes: 12345,
                file_count: 7,
            }],
        };
        *store.summary.lock().unwrap() = Some((Instant::now(), cached));
        let missing = std::env::temp_dir().join("osm-gpui-test-summary-ttl-missing-dir");

        let result = store.cache_summary(&missing);
        assert_eq!(result.total_bytes, 12345);
        assert_eq!(result.total_files, 7);
        assert_eq!(result.sources[0].key, "fake-source");

        let stale = store.summary.lock().unwrap().take();
        *store.summary.lock().unwrap() =
            stale.map(|(_, summary)| (Instant::now() - STATS_TTL * 2, summary));
        assert_eq!(store.cache_summary(&missing).total_files, 0);
    }

    #[test]
    fn clear_all_cache_removes_everything() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-clear-all-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("source-a")).unwrap();
        fs::write(dir.join("source-a").join("a.png"), vec![0u8; 10]).unwrap();
        fs::write(dir.join("stray.png"), vec![0u8; 10]).unwrap();

        clear_all_cache_at(&dir).unwrap();

        assert!(!dir.exists());
    }

    #[test]
    fn format_bytes_picks_appropriate_unit() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn set_budget_mb_updates_current_budget_without_touching_disk() {
        let store = TileStore::new();
        assert_eq!(store.budget_bytes(), DEFAULT_CACHE_BUDGET_BYTES);
        store.set_budget_mb(10);
        assert_eq!(store.budget_bytes(), 10 * 1024 * 1024);
    }

    #[test]
    fn eviction_runs_on_every_nth_write_only() {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-maybe-evict-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("big.png"), vec![0u8; 100]).unwrap();

        let store = TileStore::new();
        store.set_budget_mb(0);
        for _ in 0..WRITES_BETWEEN_EVICTION_CHECKS - 1 {
            store.maybe_evict(&dir);
        }
        assert!(
            dir.join("big.png").exists(),
            "no sweep before the Nth write"
        );
        store.maybe_evict(&dir);
        assert!(!dir.join("big.png").exists(), "the Nth write sweeps");

        let _ = fs::remove_dir_all(&dir);
    }

    fn tiny_png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn tiny_jpeg() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(8, 8, image::Rgb([10, 20, 30]))
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-test-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sniff_recognises_png_and_jpeg() {
        assert_eq!(sniff_image(&tiny_png()), SniffedImage::Png);
        assert_eq!(sniff_image(&tiny_jpeg()), SniffedImage::Jpeg);
    }

    #[test]
    fn sniff_rejects_html_webp_garbage_and_short_input() {
        assert_eq!(
            sniff_image(b"<!DOCTYPE html><html>Too many requests</html>"),
            SniffedImage::Other
        );
        // WebP is not accepted as a tile format.
        assert_eq!(
            sniff_image(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            SniffedImage::Other
        );
        assert_eq!(sniff_image(&[0u8; 32]), SniffedImage::Other);
        assert_eq!(sniff_image(b""), SniffedImage::Other);
        assert_eq!(sniff_image(&[0xFF, 0xD8]), SniffedImage::Other);
        // A PNG signature needs 8 bytes to count.
        assert_eq!(sniff_image(b"\x89PNG"), SniffedImage::Other);
    }

    #[test]
    fn sanitize_snippet_replaces_control_characters_and_trims() {
        assert_eq!(
            sanitize_snippet(b"\n\tover\x00 capacity\r\n").as_deref(),
            Some("over  capacity")
        );
    }

    #[test]
    fn sanitize_snippet_of_nothing_printable_is_none() {
        assert_eq!(sanitize_snippet(b""), None);
        assert_eq!(sanitize_snippet(b" \n\t\x00\x01 "), None);
    }

    #[test]
    fn sanitize_snippet_survives_binary_input() {
        let snippet = sanitize_snippet(&[0x89, b'P', b'N', b'G', 0x00, 0xFF, 0xFE, b'x']).unwrap();
        assert!(snippet.contains("PNG"));
        assert!(snippet.ends_with('x'));
        assert!(!snippet.chars().any(|c| c.is_control()));
    }

    #[test]
    fn sanitize_snippet_keeps_only_the_first_bytes() {
        let long = vec![b'a'; ERROR_SNIPPET_BYTES * 3];
        assert_eq!(sanitize_snippet(&long).unwrap().len(), ERROR_SNIPPET_BYTES);
    }

    #[test]
    fn decode_swaps_red_and_blue() {
        let image = decode(&tiny_png()).unwrap();
        let bgra = image.as_bytes(0).unwrap();
        assert_eq!(&bgra[..4], &[30, 20, 10, 255]);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(b"definitely not an image").is_err());
    }

    #[test]
    fn read_cached_hit_miss_and_corrupt() {
        let dir = temp_dir("read-cached");
        let good = dir.join("good.png");
        fs::write(&good, tiny_png()).unwrap();
        assert!(matches!(read_cached(&good), CacheRead::Hit(_)));
        assert!(good.exists());

        assert!(matches!(
            read_cached(&dir.join("missing.png")),
            CacheRead::Miss
        ));

        let bad = dir.join("bad.png");
        fs::write(&bad, b"half a tile").unwrap();
        assert!(matches!(read_cached(&bad), CacheRead::Corrupt));
        assert!(
            !bad.exists(),
            "a corrupt file is deleted so it is refetched"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    const URL: &str = "https://tile.example.test/1/2/3.png";

    fn always() -> bool {
        true
    }

    #[test]
    fn fetch_returns_an_image_body() {
        use crate::http::fake::{ok, FakeClient};
        let client = FakeClient::new(vec![ok(200, tiny_png())]);
        assert_eq!(fetch(&client, URL, &always).unwrap(), tiny_png());
    }

    #[test]
    fn fetch_reports_http_errors() {
        use crate::http::fake::{status_err, FakeClient};
        let client = FakeClient::new(vec![status_err(404, "no such tile")]);
        let err = fetch(&client, URL, &always).unwrap_err();
        assert_eq!(err.to_string(), "HTTP 404: no such tile");
    }

    #[test]
    fn fetch_rejects_an_html_error_page_served_with_200() {
        use crate::http::fake::{ok, FakeClient};
        let client = FakeClient::new(vec![ok(200, b"<html>Rate limited</html>".to_vec())]);
        assert!(matches!(
            fetch(&client, URL, &always),
            Err(TileFetchError::NotImage)
        ));
    }

    #[test]
    fn fetch_rejects_an_empty_body() {
        use crate::http::fake::{ok, FakeClient};
        let client = FakeClient::new(vec![ok(200, Vec::new())]);
        assert!(matches!(
            fetch(&client, URL, &always),
            Err(TileFetchError::EmptyBody)
        ));
    }

    #[test]
    fn fetch_cancelled_before_the_request_sends_nothing() {
        use crate::http::fake::{ok, FakeClient};
        let client = FakeClient::new(vec![ok(200, tiny_png())]);
        assert!(matches!(
            fetch(&client, URL, &|| false),
            Err(TileFetchError::Cancelled)
        ));
        assert_eq!(client.request_count(), 0);
    }

    fn key(source: &str) -> TileAssetKey {
        TileAssetKey {
            url: URL.to_string(),
            source_key: source.to_string(),
        }
    }

    #[test]
    fn load_tile_downloads_caches_then_serves_from_disk() {
        use crate::http::fake::{ok, FakeClient};
        let dir = temp_dir("load-tile");
        let store = TileStore::new();
        let client = FakeClient::new(vec![ok(200, tiny_png())]);
        let key = key("src");

        assert!(load_tile(&store, &client, &dir, &key).is_ok());
        assert_eq!(client.request_count(), 1);
        let cached = tile_file_path(&dir, "src", URL);
        assert_eq!(fs::read(&cached).unwrap(), tiny_png());

        // The second load needs no response: the client has none left.
        assert!(load_tile(&store, &client, &dir, &key).is_ok());
        assert_eq!(client.request_count(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    /// The process-wide client can be installed only once, so this does not
    /// install a fake. It checks the other half: `TileAsset::load` takes
    /// `default_client()`, which in tests is the offline client, so a tile load
    /// fails with a recorded transport error instead of reaching the network.
    #[test]
    fn load_tile_through_the_default_client_stays_offline_in_tests() {
        let dir = temp_dir("load-tile-default-client");
        let store = TileStore::new();
        let client = crate::http::default_client();
        let key = key("src");

        assert!(load_tile(&store, client.as_ref(), &dir, &key).is_err());
        assert!(store.last_error(URL).is_some());
        assert!(!tile_file_path(&dir, "src", URL).exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_tile_refetches_over_a_corrupt_cache_file() {
        use crate::http::fake::{ok, FakeClient};
        let dir = temp_dir("load-tile-corrupt");
        let store = TileStore::new();
        let cached = tile_file_path(&dir, "src", URL);
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::write(&cached, b"garbage").unwrap();
        let client = FakeClient::new(vec![ok(200, tiny_png())]);

        assert!(load_tile(&store, &client, &dir, &key("src")).is_ok());
        assert_eq!(client.request_count(), 1);
        assert_eq!(fs::read(&cached).unwrap(), tiny_png());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_tile_records_why_a_tile_failed_and_clears_it_on_success() {
        use crate::http::fake::{ok, status_err, FakeClient};
        let dir = temp_dir("load-tile-error");
        let store = TileStore::new();
        let client = FakeClient::new(vec![
            status_err(404, "gone"),
            ok(200, b"<html></html>".to_vec()),
            ok(200, tiny_png()),
        ]);
        let key = key("src");

        assert!(load_tile(&store, &client, &dir, &key).is_err());
        assert_eq!(store.last_error(URL).as_deref(), Some("HTTP 404: gone"));
        assert!(!tile_file_path(&dir, "src", URL).exists());

        assert!(load_tile(&store, &client, &dir, &key).is_err());
        assert_eq!(store.last_error(URL).as_deref(), Some("Not an image"));
        assert!(!tile_file_path(&dir, "src", URL).exists());

        assert!(load_tile(&store, &client, &dir, &key).is_ok());
        assert_eq!(store.last_error(URL), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_tile_for_an_unwanted_tile_is_cancelled_without_a_request() {
        use crate::http::fake::{ok, FakeClient};
        let dir = temp_dir("load-tile-cancel");
        let store = TileStore::new();
        store.set_wanted("src", HashSet::from(["some other url".to_string()]));
        let client = FakeClient::new(vec![ok(200, tiny_png())]);
        let key = key("src");

        assert!(load_tile(&store, &client, &dir, &key).is_err());
        assert_eq!(client.request_count(), 0);
        assert_eq!(store.last_error(URL), None, "a cancel is not a failure");
        // Wanted again: the cancelled tile is handed back for re-requesting.
        let revived = store.set_wanted("src", HashSet::from([URL.to_string()]));
        assert_eq!(revived, vec![key]);

        let _ = fs::remove_dir_all(&dir);
    }
}
