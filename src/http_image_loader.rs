use std::sync::Arc;

/// Download image bytes from HTTP URL
pub async fn download_image_bytes(url: String) -> Result<Vec<u8>, String> {
    eprintln!("🌐 Downloading image from URL: {}", url);

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

    eprintln!("📥 Downloaded {} bytes for {}", bytes.len(), url);
    Ok(bytes.to_vec())
}
