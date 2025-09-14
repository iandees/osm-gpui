use gpui::{Asset, BackgroundExecutor, RenderImage, ImageCacheError};
use http_client::{AsyncBody, Method, Request as HttpRequest};
use futures::AsyncReadExt;
use std::fs;
use std::sync::Arc;

pub struct TileAsset;

impl Asset for TileAsset {
    type Source = String; // The tile URL
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        url: Self::Source,
        cx: &mut gpui::App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let http_client = cx.http_client().clone();

        async move {
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
                    Ok(image) => return Ok(Arc::new(image)),
                    Err(e) => {
                        // If cached file is corrupted, delete it and re-download
                        let _ = fs::remove_file(&file_path);
                        eprintln!("⚠️ Corrupted cached tile deleted: {}", e);
                    }
                }
            }

            // Ensure cache directory exists
            if let Err(e) = fs::create_dir_all(&cache_dir) {
                return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!("Failed to create cache directory: {}", e))));
            }

            // Download the image using GPUI's HTTP client
            let request = HttpRequest::builder()
                .method(Method::GET)
                .uri(&url)
                .header("User-Agent", "osm-gpui/0.1.0")
                .body(AsyncBody::empty())
                .map_err(|e| ImageCacheError::Other(Arc::new(anyhow::anyhow!("Failed to create HTTP request: {}", e))))?;

            let mut response = http_client
                .send(request)
                .await
                .map_err(|e| ImageCacheError::Other(Arc::new(anyhow::anyhow!("Failed to fetch image: {}", e))))?;

            if !response.status().is_success() {
                return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!("HTTP error {}: {}", response.status(), url))));
            }

            // Read the response body using the correct API
            let mut bytes = Vec::new();
            response.body_mut().read_to_end(&mut bytes).await
                .map_err(|e| ImageCacheError::Other(Arc::new(anyhow::anyhow!("Failed to read response body: {}", e))))?;

            // Write to file
            fs::write(&file_path, &bytes)
                .map_err(|e| ImageCacheError::Other(Arc::new(anyhow::anyhow!("Failed to write file: {}", e))))?;

            eprintln!("💾 Saved tile: {} ({} bytes)", file_path.display(), bytes.len());

            // Load the saved file as an image
            let image = load_image_from_file(&file_path)
                .map_err(|e| ImageCacheError::Other(Arc::new(anyhow::anyhow!("{}", e))))?;
            Ok(Arc::new(image))
        }
    }
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
    // We don't need to track downloads manually anymore
    // GPUI's asset system handles this automatically
}

impl TileCache {
    pub fn new(_executor: BackgroundExecutor) -> Self {
        Self {}
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
