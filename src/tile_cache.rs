use gpui::{BackgroundExecutor, Task};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct TileCache {
    cache_dir: PathBuf,
    downloads: Arc<Mutex<HashMap<String, Task<Result<PathBuf, String>>>>>,
    executor: BackgroundExecutor,
}

impl TileCache {
    pub fn new(executor: BackgroundExecutor) -> Self {
        // Create cache directory in temp
        let cache_dir = std::env::temp_dir().join("osm-gpui-tiles");

        Self {
            cache_dir,
            downloads: Arc::new(Mutex::new(HashMap::new())),
            executor,
        }
    }

    /// Get the local file path for a tile URL, downloading if necessary
    pub fn get_tile_path(&self, url: String) -> Option<PathBuf> {
        // Create a safe filename from the URL
        let filename = self.url_to_filename(&url);
        let file_path = self.cache_dir.join(&filename);

        // Check if file already exists
        if file_path.exists() {
            return Some(file_path);
        }

        // Check if download is already in progress
        {
            let downloads = self.downloads.lock().unwrap();
            if downloads.contains_key(&url) {
                eprintln!("⏳ Download in progress: {}", filename);
                return None; // Still downloading
            }
        }

        // Start download
        eprintln!("🌐 Starting download: {}", url);
        let download_handle = self.start_download(url.clone(), file_path.clone());

        {
            let mut downloads = self.downloads.lock().unwrap();
            downloads.insert(url.clone(), download_handle);
        }

        None // Download started, not ready yet
    }

    /// Check if a download has completed (simplified - just return empty for now)
    pub fn check_downloads(&self) -> Vec<String> {
        // TODO: Implement proper task completion checking
        // For now, return empty vec to avoid runtime issues
        Vec::new()
    }

    fn start_download(&self, url: String, file_path: PathBuf) -> Task<Result<PathBuf, String>> {
        let cache_dir = self.cache_dir.clone();

        self.executor.spawn(async move {
            // Create a Tokio runtime for HTTP operations
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| format!("Failed to create tokio runtime: {}", e))?;

            rt.block_on(async move {
                // Ensure cache directory exists
                if let Err(e) = fs::create_dir_all(&cache_dir) {
                    return Err(format!("Failed to create cache directory: {}", e));
                }

                // Download the image
                let client = reqwest::Client::builder()
                    .user_agent("osm-gpui/0.1.0")
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

                let response = client
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| format!("Failed to fetch image: {}", e))?;

                if !response.status().is_success() {
                    return Err(format!("HTTP error {}: {}", response.status(), url));
                }

                let bytes = response
                    .bytes()
                    .await
                    .map_err(|e| format!("Failed to read response bytes: {}", e))?;

                // Write to file
                fs::write(&file_path, &bytes)
                    .map_err(|e| format!("Failed to write file: {}", e))?;

                eprintln!(
                    "💾 Saved tile: {} ({} bytes)",
                    file_path.display(),
                    bytes.len()
                );
                Ok(file_path)
            })
        })
    }

    fn url_to_filename(&self, url: &str) -> String {
        // Extract tile coordinates from URL like: https://tile.openstreetmap.org/11/602/769.png
        if let Some(parts) = url.strip_prefix("https://tile.openstreetmap.org/") {
            // Replace slashes with underscores to create a safe filename
            parts.replace('/', "_")
        } else {
            // Fallback: use a hash of the URL
            format!("tile_{:x}.png", self.simple_hash(url))
        }
    }

    fn simple_hash(&self, s: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        s.hash(&mut hasher);
        hasher.finish()
    }

    /// Get statistics about the cache
    pub fn stats(&self) -> (usize, usize) {
        let downloads = self.downloads.lock().unwrap();
        let active_downloads = downloads.len();

        // Count cached files
        let cached_files = if self.cache_dir.exists() {
            std::fs::read_dir(&self.cache_dir)
                .map(|entries| entries.count())
                .unwrap_or(0)
        } else {
            0
        };

        (cached_files, active_downloads)
    }
}
