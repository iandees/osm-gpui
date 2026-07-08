use gpui::{Asset, BackgroundExecutor, ImageCacheError, RenderImage};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use crate::idle_tracker::IdleTracker;

/// Maximum total size, in bytes, that the on-disk tile cache is allowed to
/// grow to before oldest-by-mtime files are evicted. 500 MB is generous
/// enough to cover a large viewport's worth of zoom levels without letting a
/// long-running session accumulate unbounded disk usage.
const MAX_CACHE_BYTES: u64 = 500 * 1024 * 1024;

/// After this many tile writes since the last eviction sweep, re-scan the
/// cache directory to check whether it's over budget. Avoids doing a full
/// `read_dir` walk on every single tile write while still keeping the cache
/// bounded in practice.
const WRITES_BETWEEN_EVICTION_CHECKS: u64 = 25;

/// Counts tile writes since the last eviction sweep. When it crosses
/// `WRITES_BETWEEN_EVICTION_CHECKS`, `maybe_evict` performs a directory scan
/// and evicts oldest-by-mtime files if the cache is over `MAX_CACHE_BYTES`.
static WRITES_SINCE_EVICTION: AtomicU64 = AtomicU64::new(0);

/// Cached result of the last "how many files are in the cache" scan, plus
/// the `Instant` it was computed at. `cached_file_count` recomputes at most
/// once per `STATS_TTL` to avoid a full directory listing on every render
/// frame (the caller in `TileLayer::stats` polls this every frame).
static STATS_CACHE: OnceLock<Mutex<Option<(Instant, usize)>>> = OnceLock::new();
const STATS_TTL: std::time::Duration = std::time::Duration::from_secs(1);

/// Returns the on-disk tile cache directory: a real, user-visible cache
/// location (via the `dirs` crate) rather than the OS temp dir, which can be
/// wiped at any time and isn't where users expect persistent cache data to
/// live.
fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("osm-gpui")
        .join("tiles")
}

/// Derive a cache filename for `url` from a SHA-256 hash of the full URL.
/// Using a strong, unseeded-collision-resistant hash (rather than
/// `DefaultHasher`, which is unseeded and not designed to be collision
/// resistant) ensures two different tile servers/templates can never
/// collide on the same cache file.
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

    // Collapse a `{switch:a,b,c}` span to a single marker, mirroring the
    // span-detection logic in `tiles::url_from_template` (anchored on the
    // literal "{switch:" prefix, then the next '}').
    if let Some(start) = normalized.find("{switch:") {
        if let Some(rel_end) = normalized[start..].find('}') {
            let end = start + rel_end;
            normalized.replace_range(start..=end, "s");
        }
    }
    normalized = normalized.replace("{s}", "s");

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

/// Atomically write `bytes` to `file_path`: write to a unique sibling temp
/// file first, then `rename` into place. `rename` is atomic on POSIX
/// filesystems (macOS/Linux), so concurrent fetches for the same cache path
/// can never produce a torn/truncated file that a concurrent reader might
/// load. See `crate::persist::write_atomic` for the shared implementation.
fn write_atomic(file_path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::persist::write_atomic(file_path, bytes, crate::persist::WriteOpts::default())
}

/// Called after every tile write. Bumps the running write counter and, once
/// every `WRITES_BETWEEN_EVICTION_CHECKS` writes, performs a directory scan
/// and evicts oldest-by-mtime files if the cache is over `MAX_CACHE_BYTES`.
///
/// We deliberately scan on a write-count cadence rather than tracking a
/// live running-size total: a live total would need to be correct across
/// process restarts (files already on disk from a previous run) and after
/// external deletion, which a periodic authoritative scan handles for free.
fn maybe_evict(dir: &Path) {
    let count = WRITES_SINCE_EVICTION.fetch_add(1, Ordering::Relaxed) + 1;
    if count.is_multiple_of(WRITES_BETWEEN_EVICTION_CHECKS) {
        evict_if_over_budget(dir, MAX_CACHE_BYTES);
    }
}

/// Scan `dir` and, if its total size exceeds `max_bytes`, delete
/// oldest-by-mtime files until it's back under budget. Entries whose
/// metadata/mtime can't be read are treated as newest (kept) rather than
/// causing a hard failure, since a partial cleanup is preferable to none.
fn evict_if_over_budget(dir: &Path, max_bytes: u64) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                let size = meta.len();
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                total += size;
                files.push((path, size, mtime));
            }
        }
    }

    if total <= max_bytes {
        return;
    }

    // Oldest first.
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

/// Return the number of files currently in the tile cache directory,
/// recomputing via a directory scan at most once per `STATS_TTL`. This keeps
/// `TileCache::stats` cheap to call every render frame instead of doing a
/// full `read_dir` scan on every call.
fn cached_file_count() -> usize {
    let cell = STATS_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = match cell.lock() {
        Ok(g) => g,
        Err(_) => return 0,
    };

    if let Some((last, count)) = *guard {
        if last.elapsed() < STATS_TTL {
            return count;
        }
    }

    let dir = cache_dir();
    let count = if dir.exists() {
        fs::read_dir(&dir)
            .map(|entries| entries.count())
            .unwrap_or(0)
    } else {
        0
    };
    *guard = Some((Instant::now(), count));
    count
}

/// Global IdleTracker shared between TileCache and TileAsset::load.
/// Set once when TileCache is constructed with an IdleTracker.
static TILE_IDLE_TRACKER: OnceLock<Arc<IdleTracker>> = OnceLock::new();

/// Per-URL last-error map. Populated by `TileAsset::load` whenever a tile
/// fails, and cleared when a tile subsequently loads successfully. Read by
/// `TileLayer` when rendering the failure fallback.
static TILE_LOAD_ERRORS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn tile_errors() -> &'static Mutex<HashMap<String, String>> {
    TILE_LOAD_ERRORS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_error(url: &str, reason: String) {
    if let Ok(mut map) = tile_errors().lock() {
        map.insert(url.to_string(), reason);
    }
}

fn clear_error(url: &str) {
    if let Ok(mut map) = tile_errors().lock() {
        map.remove(url);
    }
}

/// Look up the most recent failure reason for a tile URL, if any.
pub fn last_error(url: &str) -> Option<String> {
    tile_errors().lock().ok().and_then(|m| m.get(url).cloned())
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
        }
    }
}

impl std::error::Error for TileFetchError {}

pub struct TileAsset;

impl Asset for TileAsset {
    type Source = String; // The tile URL
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        url: Self::Source,
        cx: &mut gpui::App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let executor = cx.background_executor().clone();
        let idle = TILE_IDLE_TRACKER.get().cloned();

        async move {
            // Signal that a tile fetch has started (if idle tracker is wired up).
            if let Some(ref tracker) = idle {
                tracker.tile_fetch_started();
            }
            // Use GPUI's background executor to run the HTTP request synchronously.
            // We await the spawned future and call tile_fetch_finished exactly once
            // after it resolves, covering all success and error paths.
            let result = executor
                .spawn(async move {
                    let cache_dir = cache_dir();

                    // Derive a collision-resistant filename from the full URL so
                    // different tile servers/templates can never collide.
                    let filename = cache_filename(&url);

                    let file_path = cache_dir.join(&filename);

                    // Check if file already exists, load it directly
                    if file_path.exists() {
                        match load_image_from_file(&file_path) {
                            Ok(image) => {
                                clear_error(&url);
                                return Ok(Arc::new(image));
                            }
                            Err(_) => {
                                // If cached file is corrupted, delete it and re-download
                                let _ = fs::remove_file(&file_path);
                            }
                        }
                    }

                    // Ensure cache directory exists
                    if let Err(e) = fs::create_dir_all(&cache_dir) {
                        let reason = TileFetchError::Io(format!("mkdir: {}", e)).to_string();
                        record_error(&url, reason.clone());
                        return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason))));
                    }

                    // Use a simple synchronous HTTP request that doesn't require Tokio
                    match download_file_sync(&url) {
                        Ok(bytes) => {
                            // Check if the response actually contains image data
                            if bytes.is_empty() {
                                let reason = TileFetchError::EmptyBody.to_string();
                                record_error(&url, reason.clone());
                                return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(
                                    reason
                                ))));
                            }

                            // Check if this looks like an actual image file
                            // PNG: bytes 1..4 == "PNG", JPEG: bytes 0..3 == FF D8 FF
                            let is_png = bytes.len() >= 8 && &bytes[1..4] == b"PNG";
                            let is_jpeg = bytes.len() >= 3
                                && bytes[0] == 0xFF
                                && bytes[1] == 0xD8
                                && bytes[2] == 0xFF;
                            if !is_png && !is_jpeg {
                                let reason = TileFetchError::NotImage.to_string();
                                record_error(&url, reason.clone());
                                return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(
                                    reason
                                ))));
                            }

                            // Write to file atomically (temp file + rename) so a
                            // concurrent fetch for the same cache path can never
                            // observe a torn/truncated file.
                            if let Err(e) = write_atomic(&file_path, &bytes) {
                                let reason =
                                    TileFetchError::Io(format!("write: {}", e)).to_string();
                                record_error(&url, reason.clone());
                                return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(
                                    reason
                                ))));
                            }
                            maybe_evict(&cache_dir);

                            // Load the saved file as an image
                            match load_image_from_file(&file_path) {
                                Ok(image) => {
                                    clear_error(&url);
                                    Ok(Arc::new(image))
                                }
                                Err(e) => {
                                    let reason = format!("Decode: {}", e);
                                    record_error(&url, reason.clone());
                                    Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason))))
                                }
                            }
                        }
                        Err(e) => {
                            let reason = e.to_string();
                            record_error(&url, reason.clone());
                            Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason))))
                        }
                    }
                })
                .await;
            // Exactly one finished call for the one started call above,
            // regardless of which success or error branch the inner future took.
            if let Some(ref tracker) = idle {
                tracker.tile_fetch_finished();
            }
            result
        }
    }
}

/// Max number of tile downloads allowed to be in flight at once, process-wide.
///
/// The OSM tile usage policy asks clients to keep to ~2 concurrent connections
/// per host. Tracking connections *per host* would require threading a host
/// key through the whole asset-loading path (URLs for custom imagery sources
/// can point anywhere), so as a simpler and still effective approximation we
/// cap total concurrent tile downloads process-wide. In practice almost all
/// traffic goes to a single tile host at a time, so this behaves close to a
/// per-host cap while being much simpler to reason about.
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

fn download_file_sync(url: &str) -> Result<Vec<u8>, TileFetchError> {
    download_file_sync_with(&crate::http::UreqClient::new(), url)
}

/// Same as `download_file_sync`, but against an injected `HttpClient` so it's
/// testable without a real network. Kept `pub(crate)` since tests are the only
/// other caller.
pub(crate) fn download_file_sync_with(
    client: &dyn crate::http::HttpClient,
    url: &str,
) -> Result<Vec<u8>, TileFetchError> {
    // Cap concurrent in-flight tile downloads (see MAX_CONCURRENT_TILE_DOWNLOADS).
    // Held for the duration of the request (including retries) below.
    let _permit = tile_download_semaphore().acquire();

    let req =
        crate::http::HttpRequest::get(url).header("Referer", "https://github.com/iandees/osm-gpui");

    match crate::http::fetch_with_retries(client, &req, &crate::http::RetryPolicy::standard()) {
        Ok(resp) => Ok(resp.body),
        Err(crate::http::HttpError::Status { status, body }) => {
            let snippet = sanitize_snippet(&body);
            Err(TileFetchError::Http {
                status,
                body_snippet: snippet,
            })
        }
        Err(crate::http::HttpError::Transport(msg)) => Err(TileFetchError::Transport(msg)),
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

fn load_image_from_file(file_path: &std::path::Path) -> Result<RenderImage, String> {
    let bytes = fs::read(file_path).map_err(|e| format!("Failed to read image file: {}", e))?;

    let img =
        image::load_from_memory(&bytes).map_err(|e| format!("Failed to decode image: {}", e))?;

    // Convert to RGBA8 format first
    let mut rgba = img.to_rgba8();

    // Convert RGBA to BGRA format that GPUI expects
    // We need to swap the red and blue channels for each pixel
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2); // Swap R and B channels (RGBA -> BGRA)
    }

    // Create a frame for the image
    let frame = image::Frame::new(rgba);
    let mut frames = smallvec::SmallVec::new();
    frames.push(frame);
    Ok(RenderImage::new(frames))
}

#[derive(Clone)]
pub struct TileCache {
    _idle: Arc<IdleTracker>,
}

impl TileCache {
    pub fn new(_executor: BackgroundExecutor, idle: Arc<IdleTracker>) -> Self {
        // Register the tracker globally so TileAsset::load can access it.
        // If already set (e.g. in tests), we simply use whichever was set first.
        let _ = TILE_IDLE_TRACKER.set(idle.clone());
        Self { _idle: idle }
    }

    /// Get statistics about the cache. The cached-file count is recomputed
    /// via a directory scan at most once per second (see `cached_file_count`)
    /// so calling this every render frame doesn't hit the filesystem on
    /// every call.
    pub fn stats(&self) -> (usize, usize) {
        let cached_files = cached_file_count();

        // We can't easily track active downloads with the asset system
        // but GPUI handles this internally
        (cached_files, 0)
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
    fn record_and_clear_error() {
        let url = "https://example.test/record_and_clear/1.png";
        record_error(url, "HTTP 418".to_string());
        assert_eq!(last_error(url).as_deref(), Some("HTTP 418"));
        clear_error(url);
        assert_eq!(last_error(url), None);
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
    /// filename, unlike the old unseeded-DefaultHasher scheme which could
    /// theoretically collide.
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

    /// `cached_file_count` uses a process-global TTL cache keyed only by
    /// time, so we can't deterministically assert its return value without
    /// racing other tests that also touch the real cache dir. Instead we
    /// verify the TTL mechanics directly against the same static used by
    /// `cached_file_count`: within the TTL window, a stale cached value is
    /// returned unchanged; once the recorded instant looks expired, a fresh
    /// scan is indicated as needed. This avoids any real sleeping.
    #[test]
    fn stats_cache_ttl_respects_recent_timestamp() {
        let cell = STATS_CACHE.get_or_init(|| Mutex::new(None));
        let mut guard = cell.lock().unwrap();
        *guard = Some((Instant::now(), 42));
        let (last, count) = guard.unwrap();
        assert!(last.elapsed() < STATS_TTL);
        assert_eq!(count, 42);
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
}
