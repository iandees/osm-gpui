use gpui::{Asset, BackgroundExecutor, RenderImage, ImageCacheError};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::sync::{Arc, Mutex, OnceLock};
use std::io::Read;

use crate::idle_tracker::IdleTracker;

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
    Http { status: u16, body_snippet: Option<String> },
    Transport(String),
    EmptyBody,
    NotPng,
    Io(String),
}

impl fmt::Display for TileFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileFetchError::Http { status, body_snippet } => match body_snippet {
                Some(snippet) if !snippet.is_empty() => {
                    write!(f, "HTTP {}: {}", status, snippet)
                }
                _ => write!(f, "HTTP {}", status),
            },
            TileFetchError::Transport(kind) => write!(f, "Transport: {}", kind),
            TileFetchError::EmptyBody => write!(f, "Empty body"),
            TileFetchError::NotPng => write!(f, "Not PNG"),
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
            let result = executor.spawn(async move {
                let cache_dir = std::env::temp_dir().join("osm-gpui-tiles");

                // Create a safe filename from the URL
                let filename = if let Some(parts) = url.strip_prefix("https://tile.openstreetmap.org/") {
                    parts.replace('/', "_")
                } else {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    let mut hasher = DefaultHasher::new();
                    url.hash(&mut hasher);
                    format!("tile_{:x}.png", hasher.finish())
                };

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
                            return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason))));
                        }

                        // Check if this looks like an actual image file (PNG should start with PNG signature)
                        if bytes.len() < 8 || &bytes[1..4] != b"PNG" {
                            let reason = TileFetchError::NotPng.to_string();
                            record_error(&url, reason.clone());
                            return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason))));
                        }

                        // Write to file
                        if let Err(e) = fs::write(&file_path, &bytes) {
                            let reason = TileFetchError::Io(format!("write: {}", e)).to_string();
                            record_error(&url, reason.clone());
                            return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(reason))));
                        }

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
            }).await;
            // Exactly one finished call for the one started call above,
            // regardless of which success or error branch the inner future took.
            if let Some(ref tracker) = idle {
                tracker.tile_fetch_finished();
            }
            result
        }
    }
}

fn download_file_sync(url: &str) -> Result<Vec<u8>, TileFetchError> {
    // Use ureq for synchronous HTTP requests that don't require Tokio
    let result = ureq::get(url)
        .set("User-Agent", "osm-gpui/0.1.0 (https://github.com/iandees/osm-gpui) Rust/GPUI")
        .set("Referer", "https://github.com/iandees/osm-gpui")
        .timeout(std::time::Duration::from_secs(30))
        .call();

    let response = match result {
        Ok(resp) => resp,
        Err(ureq::Error::Status(status, resp)) => {
            // Read up to ~120 bytes of the body for a snippet, sanitize whitespace.
            let mut buf = Vec::new();
            let _ = resp.into_reader().take(120).read_to_end(&mut buf);
            let snippet = if buf.is_empty() {
                None
            } else {
                let raw = String::from_utf8_lossy(&buf);
                let cleaned: String = raw
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect();
                let trimmed = cleaned.trim().to_string();
                if trimmed.is_empty() { None } else { Some(trimmed) }
            };
            return Err(TileFetchError::Http { status, body_snippet: snippet });
        }
        Err(ureq::Error::Transport(t)) => {
            return Err(TileFetchError::Transport(format!("{:?}", t.kind())));
        }
    };

    let mut bytes = Vec::new();
    if let Err(e) = response.into_reader().read_to_end(&mut bytes) {
        return Err(TileFetchError::Io(format!("read: {}", e)));
    }
    Ok(bytes)
}

fn load_image_from_file(file_path: &std::path::Path) -> Result<RenderImage, String> {
    let bytes = fs::read(file_path)
        .map_err(|e| format!("Failed to read image file: {}", e))?;

    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Failed to decode image: {}", e))?;

    // Convert to RGBA8 format first
    let mut rgba = img.to_rgba8();

    // Convert RGBA to BGRA format that GPUI expects
    // We need to swap the red and blue channels for each pixel
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2); // Swap R and B channels (RGBA -> BGRA)
    }

    // Create a frame for the image
    let frame = image::Frame::new(rgba.into());
    let mut frames = smallvec::SmallVec::new();
    frames.push(frame);
    Ok(RenderImage::new(frames))
}

#[derive(Clone)]
pub struct TileCache {
    idle: Arc<IdleTracker>,
}

impl TileCache {
    pub fn new(_executor: BackgroundExecutor, idle: Arc<IdleTracker>) -> Self {
        // Register the tracker globally so TileAsset::load can access it.
        // If already set (e.g. in tests), we simply use whichever was set first.
        let _ = TILE_IDLE_TRACKER.set(idle.clone());
        Self { idle }
    }

    /// Get statistics about the cache
    pub fn stats(&self) -> (usize, usize) {
        let cache_dir = std::env::temp_dir().join("osm-gpui-tiles");
        let cached_files = if cache_dir.exists() {
            std::fs::read_dir(&cache_dir)
                .map(|entries| entries.count())
                .unwrap_or(0)
        } else {
            0
        };

        // We can't easily track active downloads with the asset system
        // but GPUI handles this internally
        (cached_files, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let out = truncate_middle("\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}\u{516d}\u{4e03}\u{516b}", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.contains("..."));
    }

    #[test]
    fn display_http_no_snippet() {
        let e = TileFetchError::Http { status: 404, body_snippet: None };
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
        assert_eq!(TileFetchError::Transport("Dns".into()).to_string(), "Transport: Dns");
        assert_eq!(TileFetchError::EmptyBody.to_string(), "Empty body");
        assert_eq!(TileFetchError::NotPng.to_string(), "Not PNG");
        assert_eq!(TileFetchError::Io("write: nope".into()).to_string(), "Disk: write: nope");
    }

    #[test]
    fn record_and_clear_error() {
        let url = "https://example.test/record_and_clear/1.png";
        record_error(url, "HTTP 418".to_string());
        assert_eq!(last_error(url).as_deref(), Some("HTTP 418"));
        clear_error(url);
        assert_eq!(last_error(url), None);
    }
}
