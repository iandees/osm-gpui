//! Progress model for the app's two network transfers: downloading OSM data
//! and uploading a changeset.
//!
//! The background thread doing the work owns a [`TransferHandle`] clone and
//! moves it through the phases; the UI keeps another clone, polls
//! [`TransferHandle::snapshot`] on a timer and re-renders. Nothing here
//! touches the network or the UI toolkit, so all of it is unit-testable.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::SharedString;

use crate::{tr, tr_count};

/// Minimum spacing between progress reports from a fast-reading loop, so
/// reporting never slows the transfer down.
pub const REPORT_INTERVAL: Duration = Duration::from_millis(100);

/// How often the UI re-reads the shared state while a transfer runs.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferKind {
    Download,
    Upload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferPhase {
    /// Download: request sent, waiting for the first bytes.
    Requesting,
    /// Download: the response body is arriving.
    Downloading,
    /// Download: body received, parsing the XML.
    Reading,
    /// Upload: creating the changeset.
    OpeningChangeset,
    /// Upload: sending the osmChange document.
    UploadingChanges { count: usize },
    /// Upload: closing the changeset.
    ClosingChangeset,
}

/// A point-in-time view of one transfer.
#[derive(Clone, Debug)]
pub struct TransferProgress {
    pub kind: TransferKind,
    pub phase: TransferPhase,
    /// Bytes of response body received so far (downloads).
    pub bytes: u64,
    /// Body size from `Content-Length`, when the server sent one.
    pub total: Option<u64>,
    pub started_at: Instant,
}

impl TransferProgress {
    pub fn new(kind: TransferKind, phase: TransferPhase, started_at: Instant) -> Self {
        TransferProgress {
            kind,
            phase,
            bytes: 0,
            total: None,
            started_at,
        }
    }

    /// Completed fraction in `0.0..=1.0`, only when the total size is known.
    pub fn fraction(&self) -> Option<f32> {
        match (self.phase, self.total) {
            (TransferPhase::Downloading, Some(total)) if total > 0 => {
                Some((self.bytes as f64 / total as f64).min(1.0) as f32)
            }
            _ => None,
        }
    }

    /// Whether the transfer can still be abandoned safely. A download can be
    /// until it has all arrived (parsing is not interruptible); an upload
    /// never can — a half-applied changeset is worse than waiting.
    pub fn is_cancellable(&self) -> bool {
        matches!(
            (self.kind, self.phase),
            (
                TransferKind::Download,
                TransferPhase::Requesting | TransferPhase::Downloading
            )
        )
    }

    /// The one-line description shown in the status bar and the upload dialog.
    pub fn label(&self) -> SharedString {
        match self.phase {
            TransferPhase::Requesting => tr!("transfer.download.requesting"),
            TransferPhase::Downloading if self.bytes == 0 => {
                tr!("transfer.download.downloading")
            }
            TransferPhase::Downloading => match self.total {
                Some(total) if total >= self.bytes && total > 0 => tr!(
                    "transfer.download.downloading_total",
                    size = format_bytes(self.bytes),
                    total = format_bytes(total)
                ),
                _ => tr!(
                    "transfer.download.downloading_bytes",
                    size = format_bytes(self.bytes)
                ),
            },
            TransferPhase::Reading => tr!("transfer.download.reading"),
            TransferPhase::OpeningChangeset => tr!("transfer.upload.opening"),
            TransferPhase::UploadingChanges { count } => {
                tr_count!("transfer.upload.uploading", count)
            }
            TransferPhase::ClosingChangeset => tr!("transfer.upload.closing"),
        }
    }
}

/// Shared between the worker thread (writes) and the UI (reads), plus a
/// cancel flag the UI sets and the worker polls between chunks.
#[derive(Clone)]
pub struct TransferHandle {
    state: Arc<Mutex<TransferProgress>>,
    cancel: Arc<AtomicBool>,
}

impl TransferHandle {
    pub fn new(kind: TransferKind, phase: TransferPhase) -> Self {
        TransferHandle {
            state: Arc::new(Mutex::new(TransferProgress::new(
                kind,
                phase,
                Instant::now(),
            ))),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut TransferProgress) -> T) -> T {
        // A poisoned lock only means a writer panicked mid-update; the data
        // is plain numbers, so keep using it.
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    pub fn snapshot(&self) -> TransferProgress {
        self.with(|p| p.clone())
    }

    pub fn set_phase(&self, phase: TransferPhase) {
        self.with(|p| p.phase = phase);
    }

    pub fn set_bytes(&self, bytes: u64, total: Option<u64>) {
        self.with(|p| {
            p.bytes = bytes;
            if total.is_some() {
                p.total = total;
            }
        });
    }

    /// Whether `other` is a clone of the same transfer.
    pub fn is_same(&self, other: &TransferHandle) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Decides whether enough time has passed to report again.
#[derive(Debug)]
pub struct Throttle {
    interval: Duration,
    last: Option<Instant>,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Throttle {
            interval,
            last: None,
        }
    }

    /// True the first time, then at most once per `interval`.
    pub fn ready(&mut self, now: Instant) -> bool {
        match self.last {
            Some(last) if now.saturating_duration_since(last) < self.interval => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// Error a [`CountingReader`] returns when its callback asks to stop.
#[derive(Debug)]
pub struct ReadCancelled;

impl std::fmt::Display for ReadCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "read cancelled")
    }
}

impl std::error::Error for ReadCancelled {}

/// True if `err` came from a [`CountingReader`] whose callback returned `false`.
pub fn is_read_cancelled(err: &io::Error) -> bool {
    err.get_ref().is_some_and(|e| e.is::<ReadCancelled>())
}

/// Wraps a reader, counting the bytes that pass through. After every
/// non-empty read it calls `on_read(total_so_far)`; if that returns `false`
/// the read fails with [`ReadCancelled`], which drops the connection when the
/// reader is dropped.
pub struct CountingReader<R, F> {
    inner: R,
    count: u64,
    on_read: F,
}

impl<R: Read, F: FnMut(u64) -> bool> CountingReader<R, F> {
    pub fn new(inner: R, on_read: F) -> Self {
        CountingReader {
            inner,
            count: 0,
            on_read,
        }
    }

    pub fn count(&self) -> u64 {
        self.count
    }
}

impl<R: Read, F: FnMut(u64) -> bool> Read for CountingReader<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.count += n as u64;
            if !(self.on_read)(self.count) {
                return Err(io::Error::other(ReadCancelled));
            }
        }
        Ok(n)
    }
}

/// "512 B", "1.4 KB", "12.0 MB" — decimal units, as macOS shows file sizes.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_picks_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1000), "1.0 KB");
        assert_eq!(format_bytes(1_400_000), "1.4 MB");
        assert_eq!(format_bytes(12_000_000), "12.0 MB");
        assert_eq!(format_bytes(3_210_000_000), "3.2 GB");
    }

    #[test]
    fn format_bytes_rounds_up_across_a_unit_boundary() {
        assert_eq!(format_bytes(999_960), "1.0 MB");
    }

    #[test]
    fn counting_reader_counts_and_reports() {
        let data = vec![7u8; 10_000];
        let mut seen = Vec::new();
        let mut reader = CountingReader::new(&data[..], |n| {
            seen.push(n);
            true
        });
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = reader.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(reader.count(), 10_000);
        assert_eq!(out, data);
        assert_eq!(seen, vec![4096, 8192, 10_000]);
    }

    #[test]
    fn counting_reader_stops_when_callback_declines() {
        let data = vec![0u8; 10_000];
        let mut reader = CountingReader::new(&data[..], |n| n < 5000);
        let mut sink = Vec::new();
        let err = reader.read_to_end(&mut sink).unwrap_err();
        assert!(is_read_cancelled(&err));
        assert!(reader.count() >= 5000);
        assert!(sink.len() < 10_000);
    }

    #[test]
    fn other_io_errors_are_not_cancellation() {
        let err = io::Error::new(io::ErrorKind::TimedOut, "slow");
        assert!(!is_read_cancelled(&err));
    }

    #[test]
    fn throttle_fires_first_then_once_per_interval() {
        let t0 = Instant::now();
        let mut throttle = Throttle::new(Duration::from_millis(100));
        assert!(throttle.ready(t0));
        assert!(!throttle.ready(t0 + Duration::from_millis(50)));
        assert!(!throttle.ready(t0 + Duration::from_millis(99)));
        assert!(throttle.ready(t0 + Duration::from_millis(100)));
        // The window restarts from the last report, not from the start.
        assert!(!throttle.ready(t0 + Duration::from_millis(150)));
        assert!(throttle.ready(t0 + Duration::from_millis(200)));
    }

    #[test]
    fn download_phases_have_distinct_labels() {
        let now = Instant::now();
        let mut p = TransferProgress::new(TransferKind::Download, TransferPhase::Requesting, now);
        assert_eq!(p.label(), "Requesting OSM data…");
        p.phase = TransferPhase::Downloading;
        assert_eq!(p.label(), "Downloading OSM data…");
        p.bytes = 1_400_000;
        assert_eq!(p.label(), "Downloading OSM data… 1.4 MB");
        p.total = Some(2_800_000);
        assert_eq!(p.label(), "Downloading OSM data… 1.4 MB of 2.8 MB");
        p.phase = TransferPhase::Reading;
        assert_eq!(p.label(), "Reading OSM data…");
    }

    #[test]
    fn upload_phases_have_labels_with_plurals() {
        let now = Instant::now();
        let mut p =
            TransferProgress::new(TransferKind::Upload, TransferPhase::OpeningChangeset, now);
        assert_eq!(p.label(), "Opening changeset…");
        p.phase = TransferPhase::UploadingChanges { count: 1 };
        assert_eq!(p.label(), "Uploading 1 change…");
        p.phase = TransferPhase::UploadingChanges { count: 12 };
        assert_eq!(p.label(), "Uploading 12 changes…");
        p.phase = TransferPhase::ClosingChangeset;
        assert_eq!(p.label(), "Closing changeset…");
    }

    #[test]
    fn a_total_smaller_than_the_bytes_seen_is_ignored() {
        let mut p = TransferProgress::new(
            TransferKind::Download,
            TransferPhase::Downloading,
            Instant::now(),
        );
        p.bytes = 2_000;
        p.total = Some(1_000);
        assert_eq!(p.label(), "Downloading OSM data… 2.0 KB");
    }

    #[test]
    fn fraction_needs_a_known_total() {
        let mut p = TransferProgress::new(
            TransferKind::Download,
            TransferPhase::Downloading,
            Instant::now(),
        );
        p.bytes = 500;
        assert_eq!(p.fraction(), None);
        p.total = Some(1000);
        assert_eq!(p.fraction(), Some(0.5));
        p.bytes = 5000;
        assert_eq!(p.fraction(), Some(1.0));
        p.total = Some(0);
        assert_eq!(p.fraction(), None);
        p.total = Some(1000);
        p.phase = TransferPhase::Reading;
        assert_eq!(p.fraction(), None);
    }

    #[test]
    fn only_an_unfinished_download_is_cancellable() {
        let now = Instant::now();
        let dl = |phase| TransferProgress::new(TransferKind::Download, phase, now);
        assert!(dl(TransferPhase::Requesting).is_cancellable());
        assert!(dl(TransferPhase::Downloading).is_cancellable());
        assert!(!dl(TransferPhase::Reading).is_cancellable());
        for phase in [
            TransferPhase::OpeningChangeset,
            TransferPhase::UploadingChanges { count: 3 },
            TransferPhase::ClosingChangeset,
        ] {
            assert!(!TransferProgress::new(TransferKind::Upload, phase, now).is_cancellable());
        }
    }

    #[test]
    fn handle_phase_transitions_are_visible_to_other_clones() {
        let worker = TransferHandle::new(TransferKind::Download, TransferPhase::Requesting);
        let ui = worker.clone();
        assert_eq!(ui.snapshot().phase, TransferPhase::Requesting);
        worker.set_phase(TransferPhase::Downloading);
        worker.set_bytes(1234, Some(5000));
        let snap = ui.snapshot();
        assert_eq!(snap.phase, TransferPhase::Downloading);
        assert_eq!((snap.bytes, snap.total), (1234, Some(5000)));
        // A later report without a total keeps the one already learned.
        worker.set_bytes(2000, None);
        assert_eq!(ui.snapshot().total, Some(5000));
        worker.set_phase(TransferPhase::Reading);
        assert_eq!(ui.snapshot().phase, TransferPhase::Reading);
    }

    #[test]
    fn cancel_flag_is_shared() {
        let worker = TransferHandle::new(TransferKind::Download, TransferPhase::Requesting);
        let ui = worker.clone();
        assert!(!worker.is_cancelled());
        ui.cancel();
        assert!(worker.is_cancelled());
    }
}
