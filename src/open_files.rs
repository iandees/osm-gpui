//! Files handed to the app by the OS: command-line paths and, on macOS,
//! `file://` URLs delivered by Finder ("Open With", double-click, drop on the
//! Dock icon).
//!
//! The OS can deliver files before the main window exists (a cold launch by
//! the open event), so everything goes through an [`OpenQueue`] that holds
//! paths until the window is ready.

use std::path::PathBuf;

/// Convert a `file://` URL (as delivered by `application:openURLs:`) to a
/// path. Returns `None` for other schemes or URLs naming a remote host.
///
/// Handles percent-encoding (`%20`, multi-byte UTF-8) and the `localhost`
/// authority.
pub fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file:///path` (empty authority) or `file://localhost/path`.
    let path_part = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost")
            .filter(|p| p.starts_with('/'))?
    };
    let decoded = urlencoding::decode(path_part).ok()?;
    if decoded.is_empty() {
        return None;
    }
    Some(PathBuf::from(decoded.into_owned()))
}

/// Convert each opened URL to a path, skipping anything that is not a local
/// file URL. A bare absolute path is accepted too.
pub fn urls_to_paths(urls: &[String]) -> Vec<PathBuf> {
    urls.iter()
        .filter_map(|u| {
            if u.starts_with("file://") {
                file_url_to_path(u)
            } else if u.starts_with('/') {
                Some(PathBuf::from(u))
            } else {
                None
            }
        })
        .collect()
}

/// Paths waiting to be opened. Nothing is handed out until [`mark_ready`]
/// is called (the main window exists); afterwards paths pass straight through.
///
/// [`mark_ready`]: OpenQueue::mark_ready
#[derive(Default, Debug)]
pub struct OpenQueue {
    pending: Vec<PathBuf>,
    ready: bool,
}

impl OpenQueue {
    pub fn push(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.pending.extend(paths);
    }

    /// The window is up: from now on [`take_ready`](Self::take_ready) yields paths.
    pub fn mark_ready(&mut self) {
        self.ready = true;
    }

    /// Paths that can be opened now, in arrival order (empty before ready).
    pub fn take_ready(&mut self) -> Vec<PathBuf> {
        if self.ready {
            std::mem::take(&mut self.pending)
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_file_url() {
        assert_eq!(
            file_url_to_path("file:///Users/me/map.osm"),
            Some(PathBuf::from("/Users/me/map.osm"))
        );
    }

    #[test]
    fn percent_encoded_spaces_and_unicode() {
        assert_eq!(
            file_url_to_path("file:///Users/me/My%20Maps/caf%C3%A9%20%2B%20bar.osm"),
            Some(PathBuf::from("/Users/me/My Maps/café + bar.osm"))
        );
    }

    #[test]
    fn localhost_authority_is_accepted_other_hosts_are_not() {
        assert_eq!(
            file_url_to_path("file://localhost/tmp/a.osm"),
            Some(PathBuf::from("/tmp/a.osm"))
        );
        assert_eq!(file_url_to_path("file://example.com/tmp/a.osm"), None);
        assert_eq!(file_url_to_path("file://localhostile/tmp/a.osm"), None);
    }

    #[test]
    fn non_file_urls_are_rejected() {
        assert_eq!(file_url_to_path("https://example.com/a.osm"), None);
        assert_eq!(file_url_to_path("osm-gpui://open"), None);
        assert_eq!(file_url_to_path("file://"), None);
    }

    #[test]
    fn urls_to_paths_skips_non_files_and_keeps_order() {
        let urls = vec![
            "file:///a%20b.osm".to_string(),
            "https://example.com/x.osm".to_string(),
            "/plain/c.osm".to_string(),
        ];
        assert_eq!(
            urls_to_paths(&urls),
            vec![PathBuf::from("/a b.osm"), PathBuf::from("/plain/c.osm")]
        );
    }

    #[test]
    fn queue_holds_paths_until_the_window_is_ready() {
        let mut q = OpenQueue::default();
        q.push([PathBuf::from("/a.osm")]);
        q.push([PathBuf::from("/b.osm")]);
        assert!(q.take_ready().is_empty());
        q.mark_ready();
        assert_eq!(
            q.take_ready(),
            vec![PathBuf::from("/a.osm"), PathBuf::from("/b.osm")]
        );
        // Drained: nothing is handed out twice.
        assert!(q.take_ready().is_empty());
        // After ready, later arrivals pass straight through.
        q.push([PathBuf::from("/c.osm")]);
        assert_eq!(q.take_ready(), vec![PathBuf::from("/c.osm")]);
    }
}
