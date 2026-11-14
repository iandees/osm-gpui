//! Autosave and crash recovery for data layers with unsaved edits.
//!
//! The app has no native save format; the work a user would lose in a crash,
//! a force-quit or a power cut is the pending diff of each data layer against
//! what was downloaded or opened. While the app runs, every layer with
//! pending edits is written to an app-data directory, and the next start
//! offers to bring the layers back.
//!
//! # Layout
//!
//! ```text
//! <root>/                               <data_local_dir>/osm-gpui/autosave
//!   <session-id>/                       one per running instance
//!     session.lock                      held (OS file lock) while it runs
//!     heartbeat                         touched every few seconds (fallback)
//!     <layer-id>.osm                    the layer, JOSM-style, edits marked
//!     <layer-id>.json                   manifest: name, server, edits, ...
//!     upload-in-progress                present while a changeset is being sent
//! ```
//!
//! `<data_local_dir>` is `~/Library/Application Support` on macOS,
//! `$XDG_DATA_HOME` (`~/.local/share`) on Linux and `%LOCALAPPDATA%` on
//! Windows.
//!
//! # What is stored, and why it is enough
//!
//! The `.osm` file is the same document File > Export writes
//! (`osm_export`): the layer's current data, `action="modify"` on created and
//! changed elements (created ones have negative ids and no version),
//! `action="delete"` on removed ones (with the version they had), the
//! downloaded `<bounds>`. Opening it with `OsmParser::parse_str_with_edits`
//! and `baseline_for_loaded_file` rebuilds a baseline against which
//! `diff_osm_data` yields exactly the creates, modifies and deletes (and
//! base versions) the layer had, which is all an upload reads. The
//! baseline's copy of a *modified* element is a placeholder (it only has to
//! differ), so after a restore, reverting such an element by hand leaves it
//! pending instead of dropping it from the diff; that is the same behaviour
//! as reopening an exported file.
//!
//! # Sessions and the lock
//!
//! Each running instance owns one session directory and holds an exclusive
//! OS advisory lock (`File::try_lock`) on its `session.lock` for as long as
//! it lives. The kernel drops the lock when the process dies, however it
//! dies, so there is no pid to go stale or be reused: a session is crashed
//! if and only if its lock can be taken. Where the file system does not
//! support locks (some network and synced folders) the `heartbeat` file's
//! age decides instead. Whoever finds a crashed session keeps its lock while
//! it asks the user, so two instances starting together cannot both
//! "recover" the same files.
//!
//! # Writing
//!
//! Files are written to a temporary name, flushed to disk, and renamed over
//! the old file, so a crash mid-write leaves the previous autosave intact.
//! Callers do this on a background thread from a [`Snapshot`], which holds
//! the layer's data by `Arc` (no copy on the UI thread).
//!
//! # What "saved" means
//!
//! A layer is *unsaved* while it has edits that are not on the server
//! (`MapLayer::is_modified` with a non-empty upload diff). Exporting to a
//! file does not change that, so an exported-but-not-uploaded layer is still
//! autosaved. Its autosave is removed when the layer is deleted, when its
//! edits are all uploaded, when the diff becomes empty, and for everything
//! when the app quits normally.

use crate::layers::diff::{diff_osm_data, LayerDiff};
use crate::osm::{FileEdits, OsmData, OsmParseError, OsmParser};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often a layer that keeps changing is written at most.
pub const WRITE_INTERVAL: Duration = Duration::from_secs(30);
/// How long after the last change a layer is written ("edits settled").
pub const SETTLE_DELAY: Duration = Duration::from_secs(3);
/// How often the app looks at its layers.
pub const TICK: Duration = Duration::from_secs(1);
/// How often the heartbeat file is touched.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(10);
/// A heartbeat younger than this means "alive" when locks are unsupported.
const HEARTBEAT_FRESH: Duration = Duration::from_secs(90);
/// A session directory with no lock file yet is a session being created.
const NEW_SESSION_GRACE: Duration = Duration::from_secs(30);
/// Bumped if the manifest or layout changes incompatibly.
const FORMAT: u32 = 1;

const LOCK_FILE: &str = "session.lock";
const HEARTBEAT_FILE: &str = "heartbeat";
/// No `.json` extension: `list_manifests` takes every `.json` in a session
/// directory for a layer manifest.
const UPLOAD_MARKER_FILE: &str = "upload-in-progress";
/// Bumped if the marker's meaning changes incompatibly.
const MARKER_FORMAT: u32 = 1;

/// The directory autosaves live in by default, or `None` where the platform
/// has no per-user data directory. `None` unless the persistence policy is
/// `Normal` (the default is not, so no test reaches the user's real files; the
/// app's binary passes its directory in explicitly; see [`resolve_root`]).
pub fn default_root() -> Option<PathBuf> {
    if crate::persist::policy() != crate::persist::Policy::Normal {
        return None;
    }
    Some(dirs::data_local_dir()?.join("osm-gpui").join("autosave"))
}

/// Where this run keeps (and looks for) autosaves, if anywhere.
///
/// - An explicit directory (`--autosave-dir`) always wins: it is how tests
///   and scripted runs opt in.
/// - A scripted run (`--script`) otherwise has none, so existing screenshot
///   and live-test scripts never touch the user's real autosave directory.
/// - Otherwise the default directory.
pub fn resolve_root(
    script_mode: bool,
    explicit: Option<PathBuf>,
    default: Option<PathBuf>,
) -> Option<PathBuf> {
    match explicit {
        Some(dir) => Some(dir),
        None if script_mode => None,
        None => default,
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What is known about one autosaved layer, next to its `.osm` file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// The layer's id in the session that wrote it.
    pub layer_id: u64,
    pub layer_name: String,
    /// The file the layer was opened from, if it was.
    pub source_path: Option<String>,
    /// The API server the data was downloaded from, if it was downloaded.
    pub api_server: Option<String>,
    /// When it was written, seconds since the Unix epoch.
    pub saved_at: u64,
    /// Pending created + modified + deleted objects.
    pub edit_count: usize,
    pub app_version: String,
}

/// A changeset was about to be sent, or is being sent, from this session.
///
/// Kept in a file of its own beside the layers rather than in their
/// manifests: an upload spans layers, and the manifests are rewritten by
/// concurrent background writes that would have to carry it and could drop it.
/// Builds without this file ignore it (they neither delete nor misread it;
/// `FORMAT` is unchanged). Every field has a default so a marker written by a
/// build with more or fewer fields still reads; one that does not parse at all
/// still counts as a marker (see [`read_upload_marker`]).
///
/// While it is present, the server may hold some of the edits that the
/// layers' autosaves still list as pending.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UploadMarker {
    #[serde(default)]
    pub format: u32,
    /// The API server the changeset was sent to.
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub comment: String,
    /// Seconds since the Unix epoch.
    #[serde(default)]
    pub started_at: u64,
    /// Which changeset of how many (both 0 when nothing is known).
    #[serde(default)]
    pub part: usize,
    #[serde(default)]
    pub total: usize,
    /// Objects in that changeset.
    #[serde(default)]
    pub objects: usize,
    /// Known once the server opened the changeset and the app saw it.
    #[serde(default)]
    pub changeset_id: Option<u64>,
}

impl UploadMarker {
    /// The marker to make durable before `request` is sent.
    pub fn for_request(
        api_base: &str,
        request: &crate::upload_chunks::ChunkRequest,
        now: u64,
    ) -> Self {
        let (created, modified, deleted) = request.chunk.counts();
        let (part, total) = request.part.unwrap_or((1, 1));
        UploadMarker {
            format: MARKER_FORMAT,
            api_base: api_base.to_string(),
            comment: request.tags.get("comment").unwrap_or_default().to_string(),
            started_at: now,
            part,
            total,
            objects: created + modified + deleted,
            changeset_id: None,
        }
    }

    pub fn with_changeset(mut self, id: u64) -> Self {
        self.changeset_id = Some(id);
        self
    }

    /// The changeset's page on the server's website.
    pub fn changeset_url(&self) -> Option<String> {
        let id = self.changeset_id?;
        Some(format!(
            "{}/changeset/{id}",
            crate::auth::oauth_base_for(&self.api_base)
        ))
    }
}

/// The marker in session directory `dir`, if there is one. A file that is
/// there but cannot be read as a marker (a newer build's, or torn) still
/// stands for an upload that may have gone through: it reads as a marker
/// with nothing known.
pub fn read_upload_marker(dir: &Path) -> Option<UploadMarker> {
    let bytes = match std::fs::read(dir.join(UPLOAD_MARKER_FILE)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(UploadMarker::default()),
    };
    Some(serde_json::from_slice(&bytes).unwrap_or_default())
}

/// Everything the writer needs, taken on the UI thread without copying the
/// data.
#[derive(Clone)]
pub struct Snapshot {
    pub layer_id: u64,
    pub layer_name: String,
    pub source_path: Option<String>,
    pub api_server: Option<String>,
    pub current: Arc<OsmData>,
    pub original: Arc<OsmData>,
}

/// A layer rendered for disk.
pub struct Rendered {
    pub manifest: Manifest,
    pub osm_xml: String,
}

fn edit_total(diff: &LayerDiff) -> usize {
    let (created, modified, deleted) = diff.counts();
    created + modified + deleted
}

/// Serialize a snapshot, or `None` when the layer has nothing pending (so
/// there is nothing to recover). The expensive part: a diff and the XML.
pub fn render(snap: &Snapshot) -> Option<Rendered> {
    let diff = diff_osm_data(&snap.original, &snap.current);
    if diff.is_empty() {
        return None;
    }
    let osm_xml = crate::osm_export::to_osm_xml_with_diff(&snap.original, &snap.current, &diff);
    Some(Rendered {
        manifest: Manifest {
            format: FORMAT,
            layer_id: snap.layer_id,
            layer_name: snap.layer_name.clone(),
            source_path: snap.source_path.clone(),
            api_server: snap.api_server.clone(),
            saved_at: now_unix(),
            edit_count: edit_total(&diff),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        osm_xml,
    })
}

/// Write `bytes` to `path` so that a crash at any moment leaves either the
/// old file or the new one: a temporary sibling, flushed to disk, renamed
/// over the target.
pub fn write_durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // Defence in depth: a path inside the user's real data directory is only
    // writable under the `Normal` persistence policy.
    if !crate::persist::write_permitted(path) {
        return Ok(());
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The layer's autosave now holds this many pending objects.
    Written(usize),
    /// Nothing was pending; the layer's autosave (if any) was removed.
    Removed,
}

/// One running instance's autosave directory, locked for as long as this
/// value (and so the process) lives.
pub struct Session {
    id: String,
    dir: PathBuf,
    /// Holds the OS lock. `None` where locking is unsupported.
    _lock: Option<File>,
    last_heartbeat: std::sync::Mutex<Option<Instant>>,
}

impl Session {
    /// Create this instance's directory under `root` and lock it.
    pub fn start(root: &Path) -> std::io::Result<Session> {
        let id = format!(
            "{:010}-{}-{:08x}",
            now_unix(),
            std::process::id(),
            rand::random::<u32>()
        );
        let dir = root.join(&id);
        std::fs::create_dir_all(&dir)?;
        let lock_file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(LOCK_FILE))?;
        let lock = match lock_file.try_lock() {
            Ok(()) => Some(lock_file),
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(std::io::Error::other("autosave session lock is taken"));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                eprintln!("autosave: file locks are not available here ({e}); using a heartbeat");
                None
            }
        };
        let session = Session {
            id,
            dir,
            _lock: lock,
            last_heartbeat: std::sync::Mutex::new(None),
        };
        session.heartbeat(true);
        Ok(session)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn osm_path(&self, layer_id: u64) -> PathBuf {
        self.dir.join(format!("{layer_id}.osm"))
    }

    pub fn manifest_path(&self, layer_id: u64) -> PathBuf {
        self.dir.join(format!("{layer_id}.json"))
    }

    /// Touch the heartbeat file, at most every few seconds unless `force`.
    pub fn heartbeat(&self, force: bool) {
        let mut last = self.last_heartbeat.lock().unwrap();
        if !force && last.is_some_and(|t| t.elapsed() < HEARTBEAT_EVERY) {
            return;
        }
        *last = Some(Instant::now());
        let _ = std::fs::write(self.dir.join(HEARTBEAT_FILE), now_unix().to_string());
    }

    /// Write a layer's autosave from a snapshot (serializing it first), or
    /// remove it if the layer has nothing pending.
    pub fn write(&self, snap: &Snapshot) -> std::io::Result<WriteOutcome> {
        let Some(rendered) = render(snap) else {
            self.remove_layer(snap.layer_id);
            return Ok(WriteOutcome::Removed);
        };
        self.write_rendered(&rendered)
    }

    fn write_rendered(&self, rendered: &Rendered) -> std::io::Result<WriteOutcome> {
        let id = rendered.manifest.layer_id;
        // The data first, the manifest last: a layer without a manifest is
        // ignored on recovery, so a crash between the two never shows a
        // half-written entry.
        write_durable(&self.osm_path(id), rendered.osm_xml.as_bytes())?;
        let json = serde_json::to_vec_pretty(&rendered.manifest)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_durable(&self.manifest_path(id), &json)?;
        self.heartbeat(false);
        Ok(WriteOutcome::Written(rendered.manifest.edit_count))
    }

    /// Make `marker` durable (replacing any earlier one). Call it before the
    /// request it describes is sent.
    pub fn write_upload_marker(&self, marker: &UploadMarker) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(marker)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_durable(&self.dir.join(UPLOAD_MARKER_FILE), &json)
    }

    /// Remove the marker (best effort; a missing one is fine).
    pub fn clear_upload_marker(&self) {
        let _ = std::fs::remove_file(self.dir.join(UPLOAD_MARKER_FILE));
    }

    /// The marker on disk, if any.
    pub fn upload_marker(&self) -> Option<UploadMarker> {
        read_upload_marker(&self.dir)
    }

    /// Delete one layer's autosave (best effort).
    pub fn remove_layer(&self, layer_id: u64) {
        // The manifest first, so a half-removed entry is not offered.
        let _ = std::fs::remove_file(self.manifest_path(layer_id));
        let _ = std::fs::remove_file(self.osm_path(layer_id));
    }

    /// Delete every layer's autosave, keeping the session.
    pub fn clear(&self) {
        for entry in read_dir_files(&self.dir) {
            let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name != LOCK_FILE && name != HEARTBEAT_FILE {
                let _ = std::fs::remove_file(&entry);
            }
        }
    }

    /// Whether any layer's autosave is on disk.
    pub fn has_layers(&self) -> bool {
        self.layer_count() > 0
    }

    /// How many layers have an autosave on disk.
    pub fn layer_count(&self) -> usize {
        list_manifests(&self.dir).len()
    }

    /// The clean exit: remove the session's directory and everything in it.
    /// Idempotent. A crash is exactly the case where this does not run.
    pub fn finish_clean(&self) {
        // On Windows a directory holding an open, locked file cannot be
        // removed, so the files go first and the lock file's removal is
        // allowed to fail (the next scan removes a lock-less empty session).
        self.clear();
        let _ = std::fs::remove_file(self.dir.join(HEARTBEAT_FILE));
        let _ = std::fs::remove_file(self.dir.join(LOCK_FILE));
        let _ = std::fs::remove_dir(&self.dir);
    }

    /// Move a recovered layer's autosave into this session under its new
    /// layer id, so it is safe here before the old session is discarded. It
    /// is copied, not renamed: at every moment at least one complete copy
    /// exists.
    pub fn adopt(&self, layer: &RecoverableLayer, new_layer_id: u64) -> std::io::Result<()> {
        let xml = std::fs::read(&layer.osm_path)?;
        write_durable(&self.osm_path(new_layer_id), &xml)?;
        let mut manifest = layer.manifest.clone();
        manifest.layer_id = new_layer_id;
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_durable(&self.manifest_path(new_layer_id), &json)
    }
}

fn read_dir_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default()
}

fn list_manifests(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = read_dir_files(dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    found.sort();
    found
}

/// One layer found in a crashed session.
#[derive(Debug, Clone)]
pub struct RecoverableLayer {
    pub manifest: Manifest,
    pub osm_path: PathBuf,
}

/// Why an autosaved layer could not be loaded back.
#[derive(Debug)]
pub enum RecoverError {
    /// The layer's `.osm` file could not be read.
    Read(std::io::Error),
    /// The file is not valid OSM XML.
    Parse(OsmParseError),
}

impl std::fmt::Display for RecoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecoverError::Read(e) => write!(f, "{e}"),
            RecoverError::Parse(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RecoverError {}

impl RecoverableLayer {
    /// Parse the layer's data and its recorded pending edits.
    pub fn load(&self) -> Result<(OsmData, FileEdits), RecoverError> {
        let xml = std::fs::read_to_string(&self.osm_path).map_err(RecoverError::Read)?;
        OsmParser::new()
            .parse_str_with_edits(&xml)
            .map_err(RecoverError::Parse)
    }
}

/// A session whose owner is gone, with the layers it left behind. Holds the
/// session's lock until dropped, so nobody else can claim it meanwhile.
pub struct CrashedSession {
    pub dir: PathBuf,
    pub layers: Vec<RecoverableLayer>,
    /// Set when the session died with an upload in flight: the server may
    /// already hold some of the layers' pending edits.
    pub upload_marker: Option<UploadMarker>,
    lock: Option<File>,
}

impl CrashedSession {
    /// Throw the session's files away.
    pub fn discard(self) {
        let CrashedSession { dir, lock, .. } = self;
        drop(lock);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Delete one layer's files once it has been safely taken over.
    pub fn remove_layer(&self, layer: &RecoverableLayer) {
        let _ = std::fs::remove_file(layer.osm_path.with_extension("json"));
        let _ = std::fs::remove_file(&layer.osm_path);
    }

    /// Remove the session if every layer in it has been taken over; a
    /// session that still holds a layer (one that could not be loaded) is
    /// left for the next start.
    pub fn discard_if_empty(self) {
        if list_manifests(&self.dir).is_empty() {
            self.discard();
        }
    }
}

enum Liveness {
    Alive,
    /// The owner is gone; carries the lock just taken, if locks work here.
    Dead(Option<File>),
}

fn mtime_age(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now().duration_since(modified).ok()
}

/// Whether the instance that owns `dir` is still running.
fn probe(dir: &Path) -> Liveness {
    let lock_path = dir.join(LOCK_FILE);
    let file = match File::options().read(true).write(true).open(&lock_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No lock file: a session that is still being set up, or one
            // whose lock file was removed by an interrupted clean-up.
            return if mtime_age(dir).is_some_and(|age| age < NEW_SESSION_GRACE) {
                Liveness::Alive
            } else {
                Liveness::Dead(None)
            };
        }
        // Cannot even open it (permissions, another user): not ours to touch.
        Err(_) => return Liveness::Alive,
    };
    match file.try_lock() {
        Ok(()) => Liveness::Dead(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Liveness::Alive,
        Err(std::fs::TryLockError::Error(_)) => {
            // Locks unsupported: go by the heartbeat.
            let age = mtime_age(&dir.join(HEARTBEAT_FILE)).or_else(|| mtime_age(&lock_path));
            if age.is_some_and(|age| age < HEARTBEAT_FRESH) {
                Liveness::Alive
            } else {
                Liveness::Dead(None)
            }
        }
    }
}

/// What reading one layer's manifest found.
#[derive(Debug, PartialEq)]
enum ManifestRead {
    Current(Manifest),
    /// A well-formed manifest written in a format this build does not know.
    /// Another build wrote it and may still be able to restore it, so it must
    /// be left on disk.
    OtherFormat(u32),
    /// Missing, torn or not a manifest at all.
    Unreadable,
}

/// Just enough of any manifest to tell which format wrote it, so a newer
/// build's manifest is recognised even when its other fields have changed.
#[derive(Deserialize)]
struct FormatProbe {
    format: u32,
}

fn read_manifest(path: &Path) -> ManifestRead {
    let Ok(bytes) = std::fs::read(path) else {
        return ManifestRead::Unreadable;
    };
    let Ok(probe) = serde_json::from_slice::<FormatProbe>(&bytes) else {
        return ManifestRead::Unreadable;
    };
    if probe.format != FORMAT {
        return ManifestRead::OtherFormat(probe.format);
    }
    match serde_json::from_slice::<Manifest>(&bytes) {
        Ok(manifest) => ManifestRead::Current(manifest),
        Err(_) => ManifestRead::Unreadable,
    }
}

/// The result of looking for what earlier runs left behind.
#[derive(Default)]
pub struct CrashScan {
    /// Sessions that can be offered for restore, oldest first.
    pub sessions: Vec<CrashedSession>,
    /// How many dead sessions hold autosaves from another format version.
    /// They are kept on disk but cannot be restored by this build.
    pub other_format: usize,
}

/// The sessions under `root` whose owner is gone and that left at least one
/// complete layer behind, oldest first. Sessions with nothing recoverable
/// are removed. Never touches a live session (including `own`).
pub fn find_crashed(root: &Path, own: Option<&str>) -> Vec<CrashedSession> {
    scan_crashed(root, own).sessions
}

/// [`find_crashed`], also counting the sessions kept because another build
/// wrote them in a format this one cannot read.
pub fn scan_crashed(root: &Path, own: Option<&str>) -> CrashScan {
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => return CrashScan::default(),
    };
    dirs.sort();
    let mut scan = CrashScan::default();
    for dir in dirs {
        if own.is_some_and(|id| dir.file_name().is_some_and(|n| n == id)) {
            continue;
        }
        let Liveness::Dead(lock) = probe(&dir) else {
            continue;
        };
        let mut layers = Vec::new();
        let mut other_format = false;
        for manifest_path in list_manifests(&dir) {
            let manifest = match read_manifest(&manifest_path) {
                ManifestRead::Current(manifest) => manifest,
                ManifestRead::OtherFormat(_) => {
                    other_format = true;
                    continue;
                }
                ManifestRead::Unreadable => continue,
            };
            let osm_path = manifest_path.with_extension("osm");
            if osm_path.is_file() {
                layers.push(RecoverableLayer { manifest, osm_path });
            }
        }
        if other_format {
            scan.other_format += 1;
        }
        if layers.is_empty() {
            drop(lock);
            // A newer build's edits are not junk: an older build running
            // after it must not delete them.
            if !other_format {
                let _ = std::fs::remove_dir_all(&dir);
            }
            continue;
        }
        let upload_marker = read_upload_marker(&dir);
        scan.sessions.push(CrashedSession {
            dir,
            layers,
            upload_marker,
            lock,
        });
    }
    scan
}

/// Decides when a layer is written. Pure, driven by the clock the caller
/// passes in, so the timing rules are testable.
///
/// A layer is written when it has changed since its last write, and either
/// it has been quiet for [`SETTLE_DELAY`] or it has been waiting for
/// [`WRITE_INTERVAL`] (a layer being edited without a pause still gets
/// saved). One write per layer at a time.
#[derive(Default)]
pub struct Tracker {
    layers: HashMap<u64, LayerState>,
}

#[derive(Default)]
struct LayerState {
    /// The revision last written (or found to need no file).
    saved: Option<u64>,
    /// The latest revision seen, and when it first appeared.
    seen: u64,
    seen_at: Option<Instant>,
    /// When the layer first differed from what was saved.
    dirty_since: Option<Instant>,
    in_flight: bool,
    /// A file for this layer is on disk.
    has_file: bool,
    force: bool,
}

impl Tracker {
    /// Look at a layer. `pending` is whether it has unsaved edits. Returns
    /// whether to write it now; if so the caller must call [`started`], then
    /// [`finished`] with the revision it snapshotted.
    ///
    /// [`started`]: Tracker::started
    /// [`finished`]: Tracker::finished
    pub fn poll(&mut self, now: Instant, id: u64, revision: u64, pending: bool) -> bool {
        let s = self.layers.entry(id).or_default();
        if s.in_flight {
            return false;
        }
        if s.saved == Some(revision) && !s.force {
            return false;
        }
        if !pending && !s.has_file {
            // Never had edits worth keeping; nothing to write or remove.
            s.saved = Some(revision);
            s.force = false;
            return false;
        }
        if s.seen_at.is_none() || s.seen != revision {
            s.seen = revision;
            s.seen_at = Some(now);
        }
        let dirty_since = *s.dirty_since.get_or_insert(now);
        let settled = s
            .seen_at
            .is_some_and(|at| now.duration_since(at) >= SETTLE_DELAY);
        let waited = now.duration_since(dirty_since) >= WRITE_INTERVAL;
        s.force || settled || waited
    }

    /// A write was handed to the background.
    pub fn started(&mut self, id: u64) {
        let s = self.layers.entry(id).or_default();
        s.in_flight = true;
        s.force = false;
    }

    /// The write of `revision` ended. On failure the layer stays dirty and
    /// is retried at the next opportunity.
    pub fn finished(&mut self, id: u64, revision: u64, outcome: Option<WriteOutcome>) {
        let s = self.layers.entry(id).or_default();
        s.in_flight = false;
        if let Some(outcome) = outcome {
            s.saved = Some(revision);
            s.has_file = matches!(outcome, WriteOutcome::Written(_));
            s.dirty_since = None;
            s.seen_at = None;
        }
    }

    /// Write this layer at the next poll, whatever the timers say.
    pub fn force(&mut self, id: u64) {
        self.layers.entry(id).or_default().force = true;
    }

    /// Write every known layer at the next poll.
    pub fn force_all(&mut self) {
        for s in self.layers.values_mut() {
            s.force = true;
        }
    }

    /// A layer whose files already exist at `revision` (it was just
    /// recovered and adopted).
    pub fn mark_saved(&mut self, id: u64, revision: u64) {
        let s = self.layers.entry(id).or_default();
        s.saved = Some(revision);
        s.has_file = true;
    }

    /// Stop tracking a layer (it was deleted).
    pub fn forget(&mut self, id: u64) {
        self.layers.remove(&id);
    }

    /// Whether the layer's autosave is up to date: nothing running, and
    /// either written at this revision or nothing worth keeping.
    pub fn is_settled(&self, id: u64, revision: u64, pending: bool) -> bool {
        match self.layers.get(&id) {
            None => !pending,
            Some(s) => {
                !s.in_flight && !s.force && (s.saved == Some(revision) || (!pending && !s.has_file))
            }
        }
    }

    /// Whether a write for this layer is running.
    pub fn in_flight(&self, id: u64) -> bool {
        self.layers.get(&id).is_some_and(|s| s.in_flight)
    }

    /// Whether any write is running.
    pub fn any_in_flight(&self) -> bool {
        self.layers.values().any(|s| s.in_flight)
    }

    /// Whether the layer's autosave is known to be on disk.
    pub fn has_file(&self, id: u64) -> bool {
        self.layers.get(&id).is_some_and(|s| s.has_file)
    }

    /// Forget everything about files (autosave was switched off).
    pub fn reset(&mut self) {
        self.layers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::osm_layer::OsmLayer;
    use crate::layers::{EditableLayer, LayerId};
    use crate::osm::{MemberType, OsmMember, OsmNode, OsmRelation, OsmWay};
    use crate::selection::FeatureKind;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "osm-gpui-autosave-{tag}-{}-{:08x}",
            std::process::id(),
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn node(id: i64, lat: f64, lon: f64, version: i32, t: &[(&str, &str)]) -> OsmNode {
        OsmNode {
            id,
            lat,
            lon,
            version,
            tags: tags(t),
        }
    }

    fn member(kind: MemberType, reference: i64, role: &str) -> OsmMember {
        OsmMember {
            member_type: kind,
            reference,
            role: role.to_string(),
        }
    }

    /// A small downloaded area: a street, a building, a route relation and
    /// a few loose nodes, at server versions that are not all 1.
    fn downloaded() -> OsmData {
        let nodes = vec![
            node(1, 40.0, -74.0, 3, &[("name", "Corner \"A\" & Co\nline 2")]),
            node(2, 40.001, -74.0, 4, &[]),
            node(3, 40.002, -74.0, 2, &[("highway", "crossing")]),
            node(4, 40.0, -74.001, 7, &[]),
            node(5, 40.001, -74.001, 7, &[]),
            node(6, 40.001, -74.002, 7, &[]),
            node(7, 40.0, -74.002, 7, &[]),
            node(8, 40.01, -74.01, 1, &[("amenity", "bench")]),
            node(9, 40.011, -74.011, 5, &[("amenity", "cafe")]),
        ];
        let ways = vec![
            OsmWay {
                id: 100,
                nodes: vec![1, 2, 3],
                version: 6,
                tags: tags(&[("highway", "residential"), ("name", "Main St")]),
            },
            OsmWay {
                id: 101,
                nodes: vec![4, 5, 6, 7, 4],
                version: 2,
                tags: tags(&[("building", "yes")]),
            },
        ];
        let relations = vec![
            OsmRelation {
                id: 200,
                members: vec![member(MemberType::Way, 100, "forward")],
                version: 9,
                tags: tags(&[("type", "route"), ("route", "bus")]),
            },
            OsmRelation {
                id: 201,
                members: vec![member(MemberType::Node, 8, "stop")],
                version: 3,
                tags: tags(&[("type", "route")]),
            },
        ];
        OsmData {
            nodes: nodes.into_iter().map(|n| (n.id, n)).collect(),
            ways: ways.into_iter().map(|w| (w.id, w)).collect(),
            relations,
            bounds: vec![crate::osm::OsmBounds {
                min_lat: 39.99,
                max_lat: 40.02,
                min_lon: -74.02,
                max_lon: -73.99,
            }],
        }
    }

    /// Every edit the app offers, on one layer: moves, tag changes, created
    /// nodes/ways/relations (negative ids), deleted objects of every kind, a
    /// membership change, and deletions and tag changes that are undone.
    fn heavily_edited() -> OsmLayer {
        let mut layer = OsmLayer::new_with_data(LayerId(1), "Edited", Arc::new(downloaded()));
        // Geometry and tags of existing objects.
        layer
            .commit_node_moves(&[(2, 40.00123456789, -74.00012345)])
            .unwrap();
        layer
            .set_tag(FeatureKind::Way, 100, "name", "Main Street")
            .unwrap();
        layer
            .set_tag(FeatureKind::Way, 100, "surface", "asphalt")
            .unwrap();
        layer.remove_tag(FeatureKind::Node, 3, "highway").unwrap();
        // New objects.
        let a = layer.add_node(40.02, -74.02);
        let b = layer.add_node(40.021, -74.021);
        let c = layer.add_node(40.022, -74.019);
        assert!(a < 0 && b < 0 && c < 0);
        let new_way = layer.add_way(
            vec![a, b, c, a],
            vec![("building".to_string(), "house".to_string())],
        );
        assert!(new_way < 0);
        let new_relation = layer
            .create_relation(
                vec![("type".to_string(), "multipolygon".to_string())],
                vec![
                    member(MemberType::Way, new_way, "outer"),
                    member(MemberType::Way, 101, "inner"),
                ],
            )
            .unwrap();
        assert!(new_relation < 0);
        // A relation's membership changes.
        layer
            .set_relation_members(
                200,
                vec![
                    member(MemberType::Way, 100, "forward"),
                    member(MemberType::Way, new_way, "backward"),
                ],
            )
            .unwrap();
        // Deleted objects: a relation, a way and a loose node.
        layer.delete_relation(201).unwrap();
        layer.delete_feature(FeatureKind::Way, 101).unwrap();
        layer.delete_feature(FeatureKind::Node, 9).unwrap();
        // A node created and deleted again never existed on the server.
        let gone = layer.add_node(41.0, -75.0);
        layer.delete_feature(FeatureKind::Node, gone).unwrap();
        // Undone work: deleted and restored, changed and changed back.
        let snap = layer.delete_feature(FeatureKind::Node, 8).unwrap();
        layer.restore_feature(snap).unwrap();
        layer.set_tag(FeatureKind::Node, 4, "note", "tmp").unwrap();
        layer.remove_tag(FeatureKind::Node, 4, "note").unwrap();
        layer
    }

    /// A pending diff as plain, order-independent facts. Versions of
    /// created objects are left out: the export carries none for them (a
    /// reload reads them as 1), and an upload ignores them.
    fn facts(diff: &LayerDiff) -> Vec<String> {
        let mut out = Vec::new();
        for n in &diff.created_nodes {
            out.push(format!(
                "create node {} {} {} {:?}",
                n.id,
                n.lat,
                n.lon,
                sorted(&n.tags)
            ));
        }
        for n in &diff.modified_nodes {
            out.push(format!(
                "modify node {} v{} {} {} {:?}",
                n.id,
                n.version,
                n.lat,
                n.lon,
                sorted(&n.tags)
            ));
        }
        for (id, v) in &diff.deleted_node_ids {
            out.push(format!("delete node {id} v{v}"));
        }
        for w in &diff.created_ways {
            out.push(format!(
                "create way {} {:?} {:?}",
                w.id,
                w.nodes,
                sorted(&w.tags)
            ));
        }
        for w in &diff.modified_ways {
            out.push(format!(
                "modify way {} v{} {:?} {:?}",
                w.id,
                w.version,
                w.nodes,
                sorted(&w.tags)
            ));
        }
        for (id, v) in &diff.deleted_way_ids {
            out.push(format!("delete way {id} v{v}"));
        }
        for r in &diff.created_relations {
            out.push(format!(
                "create relation {} {:?} {:?}",
                r.id,
                r.members,
                sorted(&r.tags)
            ));
        }
        for r in &diff.modified_relations {
            out.push(format!(
                "modify relation {} v{} {:?} {:?}",
                r.id,
                r.version,
                r.members,
                sorted(&r.tags)
            ));
        }
        for (id, v) in &diff.deleted_relation_ids {
            out.push(format!("delete relation {id} v{v}"));
        }
        out.sort();
        out
    }

    fn sorted(tags: &HashMap<String, String>) -> Vec<(&String, &String)> {
        let mut v: Vec<_> = tags.iter().collect();
        v.sort();
        v
    }

    fn snapshot_of(layer: &OsmLayer, id: u64) -> Snapshot {
        let (current, original) = layer.autosave_snapshot().unwrap();
        Snapshot {
            layer_id: id,
            layer_name: "Edited".into(),
            source_path: None,
            api_server: Some("https://api.openstreetmap.org/api/0.6".into()),
            current,
            original,
        }
    }

    /// Write `layer` into a fresh session, crash (drop nothing: just stop
    /// using it), and read it back as the next start would.
    fn restore(layer: &OsmLayer, root: &Path) -> OsmLayer {
        let session = Session::start(root).unwrap();
        session.write(&snapshot_of(layer, 1)).unwrap();
        drop(session); // the lock goes with the process
        let mut crashed = find_crashed(root, None);
        assert_eq!(crashed.len(), 1);
        let found = crashed.remove(0);
        let rec = found.layers[0].clone();
        let (data, edits) = rec.load().unwrap();
        found.discard();
        OsmLayer::new_with_edits(LayerId(7), rec.manifest.layer_name, Arc::new(data), &edits)
    }

    fn recoverable_at(osm_path: PathBuf) -> RecoverableLayer {
        RecoverableLayer {
            manifest: Manifest {
                format: 1,
                layer_id: 1,
                layer_name: "L".into(),
                source_path: None,
                api_server: None,
                saved_at: 0,
                edit_count: 0,
                app_version: "test".into(),
            },
            osm_path,
        }
    }

    #[test]
    fn loading_a_layer_reports_a_missing_file_and_a_corrupt_one_separately() {
        let root = tmp_root("recover-errors");
        let missing = recoverable_at(root.join("gone.osm"));
        assert!(matches!(missing.load(), Err(RecoverError::Read(_))));

        let corrupt_path = root.join("corrupt.osm");
        std::fs::write(&corrupt_path, "<osm><node id=\"1\"").unwrap();
        let err = recoverable_at(corrupt_path).load().unwrap_err();
        assert!(matches!(err, RecoverError::Parse(_)), "{err:?}");
        assert!(!err.to_string().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restoring_gives_back_exactly_the_pending_upload_diff() {
        let layer = heavily_edited();
        let before = facts(&layer.diff_for_upload());
        // The scenario really exercises every kind of change.
        for prefix in [
            "create node",
            "modify node",
            "delete node",
            "create way",
            "modify way",
            "delete way",
            "create relation",
            "modify relation",
            "delete relation",
        ] {
            assert!(
                before.iter().any(|f| f.starts_with(prefix)),
                "scenario lacks '{prefix}': {before:#?}"
            );
        }
        // Undone work leaves no trace in the diff.
        assert!(before.iter().all(|f| !f.contains("node 8 ")), "{before:#?}");
        assert!(before.iter().all(|f| !f.contains("node 4 ")), "{before:#?}");

        let root = tmp_root("roundtrip");
        let restored = restore(&layer, &root);
        let after = facts(&restored.diff_for_upload());
        assert_eq!(after, before);
        assert!(restored.is_modified());

        // Same data, byte for byte, including the downloaded bounds.
        let current = layer.autosave_snapshot().unwrap().0;
        let restored_current = restored.autosave_snapshot().unwrap().0;
        // (A created object has no server version; the app gives it 0 and a
        // reload reads 1. Nothing uploads it.)
        let unversioned = |nodes: &HashMap<i64, OsmNode>| {
            let mut nodes = nodes.clone();
            for n in nodes.values_mut().filter(|n| n.id < 0) {
                n.version = 0;
            }
            nodes
        };
        assert_eq!(
            unversioned(&restored_current.nodes),
            unversioned(&current.nodes)
        );
        let unversioned_ways = |ways: &HashMap<i64, OsmWay>| {
            let mut ways = ways.clone();
            for w in ways.values_mut().filter(|w| w.id < 0) {
                w.version = 0;
            }
            ways
        };
        assert_eq!(
            unversioned_ways(&restored_current.ways),
            unversioned_ways(&current.ways)
        );
        assert_eq!(restored_current.bounds, current.bounds);
        let tag_node = &restored_current.nodes[&1];
        assert_eq!(tag_node.tags["name"], "Corner \"A\" & Co\nline 2");
        assert_eq!(
            restored_current.nodes[&2].lat, 40.00123456789,
            "coordinates keep every digit"
        );

        // And a second crash after the restore changes nothing.
        let again = restore(&restored, &root);
        assert_eq!(facts(&again.diff_for_upload()), before);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_deleted_node_way_and_relation_are_still_deleted_after_a_restore() {
        let mut layer = OsmLayer::new_with_data(LayerId(1), "Deleted", Arc::new(downloaded()));
        layer.delete_relation(201).unwrap();
        layer.delete_feature(FeatureKind::Way, 101).unwrap();
        layer.delete_feature(FeatureKind::Node, 9).unwrap();
        let before = layer.diff_for_upload();
        assert_eq!(before.deleted_relation_ids, vec![(201, 3)]);

        let root = tmp_root("deleted-roundtrip");
        let restored = restore(&layer, &root);
        let after = restored.diff_for_upload();
        assert_eq!(after.deleted_relation_ids, vec![(201, 3)]);
        assert_eq!(after.deleted_way_ids, vec![(101, 2)]);
        assert_eq!(after.deleted_node_ids, vec![(9, 5)]);
        assert_eq!(facts(&after), facts(&before));

        let change = crate::osm_upload::build_osm_change_xml(1, &[("Deleted", after)]);
        assert!(
            change.contains("<relation id=\"201\" version=\"3\""),
            "{change}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restoring_after_a_partial_upload_leaves_only_the_unsent_edits() {
        // Edits, one accepted chunk applied locally, then the autosave that
        // the app triggers right after: a restore must not offer the
        // accepted objects again.
        let mut layer = OsmLayer::new_with_data(LayerId(1), "Partial", Arc::new(downloaded()));
        layer
            .set_tag(FeatureKind::Way, 100, "name", "Changed")
            .unwrap();
        let a = layer.add_node(40.5, -74.5);
        layer
            .set_tag(FeatureKind::Node, 9, "name", "Second chunk")
            .unwrap();

        let full = layer.diff_for_upload();
        let chunk = LayerDiff {
            created_nodes: full.created_nodes.clone(),
            modified_ways: full.modified_ways.clone(),
            ..Default::default()
        };
        let result = crate::osm_upload::UploadResult {
            node_id_remap: HashMap::from([(a, (5000, 1))]),
            way_id_remap: HashMap::from([(100, (100, 7))]),
            relation_id_remap: HashMap::new(),
        };
        layer.apply_chunk_result(&result, &chunk);
        let before = facts(&layer.diff_for_upload());
        assert_eq!(before.len(), 1, "{before:#?}");
        assert!(before[0].starts_with("modify node 9"));

        let root = tmp_root("partial");
        let restored = restore(&layer, &root);
        assert_eq!(facts(&restored.diff_for_upload()), before);
        // The accepted objects are part of the restored data, not pending.
        let data = restored.autosave_snapshot().unwrap().0;
        assert!(data.nodes.contains_key(&5000));
        assert_eq!(data.ways[&100].version, 7);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_layer_with_nothing_pending_writes_no_file() {
        let layer = OsmLayer::new_with_data(LayerId(1), "Clean", Arc::new(downloaded()));
        let root = tmp_root("clean-layer");
        let session = Session::start(&root).unwrap();
        let outcome = session.write(&snapshot_of(&layer, 1)).unwrap();
        assert_eq!(outcome, WriteOutcome::Removed);
        assert!(!session.has_layers());
        assert!(!session.osm_path(1).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_manifest_records_what_the_dialog_and_the_server_check_need() {
        let layer = heavily_edited();
        let root = tmp_root("manifest");
        let session = Session::start(&root).unwrap();
        let mut snap = snapshot_of(&layer, 3);
        snap.source_path = Some("/tmp/area.osm".into());
        let outcome = session.write(&snap).unwrap();
        let expected = layer.diff_for_upload().counts();
        assert_eq!(
            outcome,
            WriteOutcome::Written(expected.0 + expected.1 + expected.2)
        );
        let ManifestRead::Current(manifest) = read_manifest(&session.manifest_path(3)) else {
            panic!("a current manifest");
        };
        assert_eq!(manifest.layer_name, "Edited");
        assert_eq!(manifest.layer_id, 3);
        assert_eq!(manifest.source_path.as_deref(), Some("/tmp/area.osm"));
        assert_eq!(
            manifest.api_server.as_deref(),
            Some("https://api.openstreetmap.org/api/0.6")
        );
        assert_eq!(manifest.edit_count, expected.0 + expected.1 + expected.2);
        assert_eq!(manifest.app_version, env!("CARGO_PKG_VERSION"));
        assert!(manifest.saved_at > 1_700_000_000);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn writes_are_atomic_and_leave_no_temporary_files() {
        let root = tmp_root("atomic");
        let path = root.join("layer.osm");
        write_durable(&path, b"first").unwrap();
        write_durable(&path, b"second, longer").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second, longer");
        assert!(!root.join("layer.osm.tmp").exists());

        // A write that cannot complete (the temp name is taken by a
        // directory) fails without touching the previous contents.
        std::fs::create_dir(root.join("layer.osm.tmp")).unwrap();
        assert!(write_durable(&path, b"third").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"second, longer");

        // A crash mid-write leaves a stray temporary file next to the good
        // one; the good one is what recovery reads.
        std::fs::remove_dir(root.join("layer.osm.tmp")).unwrap();
        std::fs::write(root.join("layer.osm.tmp"), b"half a wri").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second, longer");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_crash_between_writes_keeps_the_previous_autosave_readable() {
        let mut layer = OsmLayer::new_with_data(LayerId(1), "L", Arc::new(downloaded()));
        layer
            .set_tag(FeatureKind::Way, 100, "name", "First")
            .unwrap();
        let root = tmp_root("crash-mid");
        let session = Session::start(&root).unwrap();
        session.write(&snapshot_of(&layer, 1)).unwrap();
        // The next write died after creating its temporary file.
        std::fs::write(session.osm_path(1).with_extension("osm.tmp"), b"<osm><nod").unwrap();
        drop(session);
        let crashed = find_crashed(&root, None);
        assert_eq!(crashed.len(), 1);
        let (data, edits) = crashed[0].layers[0].load().unwrap();
        assert_eq!(data.ways[&100].tags["name"], "First");
        assert!(edits.modified_ways.contains(&100));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_clean_exit_removes_the_session() {
        let layer = heavily_edited();
        let root = tmp_root("clean-exit");
        let session = Session::start(&root).unwrap();
        session.write(&snapshot_of(&layer, 1)).unwrap();
        session.write(&snapshot_of(&layer, 2)).unwrap();
        assert!(session.has_layers());
        let dir = session.dir().to_path_buf();
        session.finish_clean();
        assert!(!dir.exists(), "the session directory is gone");
        assert!(find_crashed(&root, None).is_empty());
        // Idempotent.
        session.finish_clean();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn removing_one_layer_keeps_the_others() {
        let layer = heavily_edited();
        let root = tmp_root("remove-one");
        let session = Session::start(&root).unwrap();
        session.write(&snapshot_of(&layer, 1)).unwrap();
        session.write(&snapshot_of(&layer, 2)).unwrap();
        session.remove_layer(1);
        assert!(!session.osm_path(1).exists());
        assert!(!session.manifest_path(1).exists());
        assert!(session.osm_path(2).exists());
        assert!(session.manifest_path(2).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_running_session_is_not_taken_for_a_crashed_one() {
        let layer = heavily_edited();
        let root = tmp_root("live");
        let live = Session::start(&root).unwrap();
        live.write(&snapshot_of(&layer, 1)).unwrap();

        // Another instance starting now sees it, but it is alive.
        assert!(find_crashed(&root, None).is_empty());
        assert!(live.osm_path(1).exists(), "the live autosave is untouched");

        // The owner dies (its lock is released): now it is crashed.
        let id = live.id().to_string();
        drop(live);
        let crashed = find_crashed(&root, None);
        assert_eq!(crashed.len(), 1);
        assert!(crashed[0].dir.ends_with(&id));
        assert_eq!(crashed[0].layers[0].manifest.layer_name, "Edited");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_session_being_recovered_cannot_be_claimed_twice() {
        let layer = heavily_edited();
        let root = tmp_root("claim");
        let dead = Session::start(&root).unwrap();
        dead.write(&snapshot_of(&layer, 1)).unwrap();
        drop(dead);

        let first = find_crashed(&root, None);
        assert_eq!(first.len(), 1);
        // A second instance scanning while the first shows its dialog.
        assert!(find_crashed(&root, None).is_empty());
        // The first gives up (Escape): the files stay for next time.
        drop(first);
        assert_eq!(find_crashed(&root, None).len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_own_session_is_skipped_and_a_second_instance_leaves_it_be() {
        let layer = heavily_edited();
        let root = tmp_root("two");
        let a = Session::start(&root).unwrap();
        let b = Session::start(&root).unwrap();
        a.write(&snapshot_of(&layer, 1)).unwrap();
        b.write(&snapshot_of(&layer, 1)).unwrap();
        assert!(find_crashed(&root, Some(a.id())).is_empty());
        assert!(a.osm_path(1).exists() && b.osm_path(1).exists());
        // One crashes; only its files are offered.
        let a_id = a.id().to_string();
        drop(a);
        let crashed = find_crashed(&root, Some(b.id()));
        assert_eq!(crashed.len(), 1);
        assert!(crashed[0].dir.ends_with(a_id));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_session_directory_still_being_created_is_not_crashed() {
        let root = tmp_root("fresh");
        std::fs::create_dir_all(root.join("0000000001-1-aaaaaaaa")).unwrap();
        // No lock file yet, but brand new: left alone.
        assert!(root.join("0000000001-1-aaaaaaaa").exists());
        assert!(find_crashed(&root, None).is_empty());
        assert!(
            root.join("0000000001-1-aaaaaaaa").exists(),
            "a young lock-less directory is not removed"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dead_sessions_with_nothing_recoverable_are_cleaned_up() {
        let root = tmp_root("junk");
        // Locked once and abandoned with no layers.
        let empty = Session::start(&root).unwrap();
        let empty_dir = empty.dir().to_path_buf();
        drop(empty);
        // A manifest without its data, a data file without its manifest,
        // and garbage.
        let torn = Session::start(&root).unwrap();
        std::fs::write(
            torn.manifest_path(1),
            serde_json::to_vec(&Manifest {
                format: FORMAT,
                layer_id: 1,
                layer_name: "x".into(),
                source_path: None,
                api_server: None,
                saved_at: 1,
                edit_count: 1,
                app_version: "0".into(),
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(torn.osm_path(2), b"<osm/>").unwrap();
        std::fs::write(torn.manifest_path(3), b"not json").unwrap();
        std::fs::write(torn.osm_path(3), b"<osm/>").unwrap();
        let torn_dir = torn.dir().to_path_buf();
        drop(torn);

        assert!(find_crashed(&root, None).is_empty());
        assert!(!empty_dir.exists());
        assert!(!torn_dir.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_session_from_a_newer_format_is_kept_and_not_offered() {
        let root = tmp_root("newer");
        let newer = Session::start(&root).unwrap();
        // A newer build may add required fields, so only `format` is relied on.
        std::fs::write(
            newer.manifest_path(1),
            format!(r#"{{"format":{},"something_new":true}}"#, FORMAT + 1),
        )
        .unwrap();
        std::fs::write(newer.osm_path(1), b"<osm/>").unwrap();
        let newer_dir = newer.dir().to_path_buf();
        assert_eq!(
            read_manifest(&newer.manifest_path(1)),
            ManifestRead::OtherFormat(FORMAT + 1)
        );
        drop(newer);

        let scan = scan_crashed(&root, None);
        assert!(scan.sessions.is_empty());
        assert_eq!(scan.other_format, 1);
        assert!(newer_dir.exists(), "the newer build's autosave is kept");
        // Still kept on the next start.
        assert!(find_crashed(&root, None).is_empty());
        assert!(newer_dir.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    fn request(part: crate::upload_chunks::Part) -> crate::upload_chunks::ChunkRequest {
        crate::upload_chunks::ChunkRequest {
            part,
            tags: "Fix the bus stops".into(),
            chunk: LayerDiff {
                created_nodes: vec![node(-1, 1.0, 2.0, 0, &[]), node(-2, 1.0, 2.0, 0, &[])],
                deleted_node_ids: vec![(5, 1)],
                ..Default::default()
            },
        }
    }

    #[test]
    fn a_marker_describes_the_request_it_was_written_for() {
        let marker = UploadMarker::for_request(
            "https://api.openstreetmap.org/",
            &request(Some((2, 3))),
            1_000,
        );
        assert_eq!((marker.part, marker.total, marker.objects), (2, 3, 3));
        assert_eq!(marker.comment, "Fix the bus stops");
        assert_eq!(marker.started_at, 1_000);
        assert_eq!(marker.changeset_id, None);
        // A single-changeset upload is part 1 of 1.
        let single = UploadMarker::for_request("https://x.test", &request(None), 0);
        assert_eq!((single.part, single.total), (1, 1));
        // The id, once known, links to the site the API base belongs to.
        assert_eq!(
            marker.clone().with_changeset(42).changeset_url().as_deref(),
            Some("https://www.openstreetmap.org/changeset/42")
        );
        assert_eq!(marker.changeset_url(), None);
    }

    #[test]
    fn a_marker_survives_a_crash_and_a_cleared_one_does_not() {
        let layer = heavily_edited();
        let root = tmp_root("marker");
        let dead = Session::start(&root).unwrap();
        dead.write(&snapshot_of(&layer, 1)).unwrap();
        let marker = UploadMarker::for_request("https://x.test/api/0.6", &request(Some((2, 3))), 5)
            .with_changeset(77);
        dead.write_upload_marker(&marker).unwrap();
        assert_eq!(dead.layer_count(), 1, "the marker is not a layer");
        drop(dead);

        let scan = scan_crashed(&root, None);
        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.sessions[0].layers.len(), 1);
        assert_eq!(scan.sessions[0].upload_marker.as_ref(), Some(&marker));
        let dir = scan.sessions[0].dir.clone();
        drop(scan);

        std::fs::remove_file(dir.join(UPLOAD_MARKER_FILE)).unwrap();
        let scan = scan_crashed(&root, None);
        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.sessions[0].upload_marker, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn clearing_and_the_clean_exit_remove_the_marker() {
        let root = tmp_root("marker-clear");
        let session = Session::start(&root).unwrap();
        let marker = UploadMarker::for_request("https://x.test", &request(None), 5);
        session.write_upload_marker(&marker).unwrap();
        assert_eq!(session.upload_marker(), Some(marker.clone()));
        session.clear_upload_marker();
        assert_eq!(session.upload_marker(), None);
        // Clearing what is not there is fine.
        session.clear_upload_marker();
        session.write_upload_marker(&marker).unwrap();
        session.clear();
        assert_eq!(session.upload_marker(), None);
        session.write_upload_marker(&marker).unwrap();
        session.finish_clean();
        assert!(!session.dir().exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn markers_of_other_builds_still_read_and_never_vanish() {
        let root = tmp_root("marker-compat");
        let session = Session::start(&root).unwrap();
        let path = session.dir().join(UPLOAD_MARKER_FILE);

        // A newer build with an extra field and a missing one.
        std::fs::write(
            &path,
            br#"{"format":9,"api_base":"https://x.test","extra":[1]}"#,
        )
        .unwrap();
        let read = session.upload_marker().unwrap();
        assert_eq!(read.api_base, "https://x.test");
        assert_eq!(read.total, 0);
        // A marker this build wrote read by a build whose struct has fewer
        // fields (an older marker type): unknown fields are ignored.
        #[derive(Deserialize)]
        struct Older {
            api_base: String,
        }
        let mine = UploadMarker::for_request("https://y.test", &request(Some((1, 2))), 1);
        let json = serde_json::to_vec(&mine).unwrap();
        assert_eq!(
            serde_json::from_slice::<Older>(&json).unwrap().api_base,
            "https://y.test"
        );

        // Torn or unreadable: still a marker, with nothing known.
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(session.upload_marker(), Some(UploadMarker::default()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_build_without_markers_sees_an_ordinary_session() {
        // What an older build does with a session that carries a marker: it
        // only looks at `.json` manifests and the lock, so the layer is
        // offered normally and the extra file is just removed with the rest.
        let layer = heavily_edited();
        let root = tmp_root("marker-old");
        let dead = Session::start(&root).unwrap();
        dead.write(&snapshot_of(&layer, 1)).unwrap();
        dead.write_upload_marker(&UploadMarker::for_request(
            "https://x.test",
            &request(None),
            1,
        ))
        .unwrap();
        let dir = dead.dir().to_path_buf();
        drop(dead);
        assert_eq!(list_manifests(&dir).len(), 1);
        let ManifestRead::Current(manifest) = read_manifest(&list_manifests(&dir)[0]) else {
            panic!("the manifest still reads");
        };
        assert_eq!(manifest.format, FORMAT);
        let mut crashed = find_crashed(&root, None);
        crashed.remove(0).discard();
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_root_is_no_crashed_sessions() {
        let root = tmp_root("absent").join("not-created");
        assert!(find_crashed(&root, None).is_empty());
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn adopting_a_recovered_layer_keeps_a_copy_before_the_old_session_goes() {
        let layer = heavily_edited();
        let root = tmp_root("adopt");
        let dead = Session::start(&root).unwrap();
        dead.write(&snapshot_of(&layer, 1)).unwrap();
        drop(dead);
        let mut crashed = find_crashed(&root, None);
        let old = crashed.remove(0);

        let session = Session::start(&root).unwrap();
        session.adopt(&old.layers[0], 9).unwrap();
        assert!(
            old.layers[0].osm_path.exists(),
            "the old copy is still there"
        );
        let ManifestRead::Current(manifest) = read_manifest(&session.manifest_path(9)) else {
            panic!("a current manifest");
        };
        assert_eq!(manifest.layer_id, 9);
        assert_eq!(manifest.layer_name, "Edited");
        old.discard();

        // The adopted copy restores to the same pending diff.
        let before = facts(&layer.diff_for_upload());
        drop(session);
        let mut again = find_crashed(&root, None);
        let rec = again.remove(0).layers.remove(0);
        let (data, edits) = rec.load().unwrap();
        let restored = OsmLayer::new_with_edits(LayerId(2), "x", Arc::new(data), &edits);
        assert_eq!(facts(&restored.diff_for_upload()), before);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn script_runs_have_no_autosave_unless_a_directory_is_given() {
        let default = Some(PathBuf::from("/home/me/.local/share/osm-gpui/autosave"));
        // A normal run uses the default location.
        assert_eq!(resolve_root(false, None, default.clone()), default);
        // A scripted run touches nothing by default...
        assert_eq!(resolve_root(true, None, default.clone()), None);
        // ...and an explicit directory opts in, in either mode, and is used
        // instead of the default.
        let dir = PathBuf::from("/tmp/x");
        assert_eq!(
            resolve_root(true, Some(dir.clone()), default.clone()),
            Some(dir.clone())
        );
        assert_eq!(resolve_root(false, Some(dir.clone()), default), Some(dir));
    }

    #[test]
    fn unit_tests_never_resolve_the_real_data_directory() {
        assert_eq!(default_root(), None);
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn an_unchanged_layer_is_never_written_again() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        // Write it once.
        assert!(!t.poll(t0, 1, 1, true));
        assert!(t.poll(t0 + SETTLE_DELAY, 1, 1, true));
        t.started(1);
        t.finished(1, 1, Some(WriteOutcome::Written(3)));
        for s in [1, 5, 31, 300] {
            assert!(!t.poll(t0 + secs(s), 1, 1, true), "no change at {s}s");
        }
    }

    #[test]
    fn a_change_is_written_after_it_settles() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        assert!(
            !t.poll(t0, 1, 5, true),
            "just changed: wait for it to settle"
        );
        assert!(!t.poll(t0 + secs(2), 1, 5, true));
        assert!(t.poll(t0 + SETTLE_DELAY, 1, 5, true));
    }

    #[test]
    fn continuous_editing_is_written_at_the_interval() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        // A new revision every second: never quiet for 3 s.
        let mut written_at = None;
        for s in 0..40u64 {
            if t.poll(t0 + secs(s), 1, s + 1, true) {
                written_at = Some(s);
                break;
            }
        }
        assert_eq!(written_at, Some(30));
    }

    #[test]
    fn only_one_write_per_layer_runs_at_a_time() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        assert!(!t.poll(t0, 1, 2, true));
        assert!(t.poll(t0 + secs(5), 1, 2, true));
        t.started(1);
        assert!(!t.poll(t0 + secs(60), 1, 3, true), "a write is running");
        assert!(t.in_flight(1) && t.any_in_flight());
        t.finished(1, 2, Some(WriteOutcome::Written(1)));
        // Edited again while it ran: written next, once settled.
        assert!(!t.poll(t0 + secs(61), 1, 3, true));
        assert!(t.poll(t0 + secs(65), 1, 3, true));
    }

    #[test]
    fn a_failed_write_is_retried() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        assert!(!t.poll(t0, 1, 1, true));
        assert!(t.poll(t0 + secs(4), 1, 1, true));
        t.started(1);
        t.finished(1, 1, None);
        assert!(t.poll(t0 + secs(5), 1, 1, true));
    }

    #[test]
    fn forcing_writes_at_once_even_if_unchanged() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        let _ = t.poll(t0, 1, 1, true);
        t.started(1);
        t.finished(1, 1, Some(WriteOutcome::Written(1)));
        assert!(!t.poll(t0 + secs(1), 1, 1, true));
        t.force(1);
        assert!(t.poll(t0 + secs(1), 1, 1, true));
    }

    #[test]
    fn a_layer_that_never_had_edits_costs_nothing() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        // Not pending and no file: never written, not even after a long time.
        assert!(!t.poll(t0, 1, 0, false));
        assert!(!t.poll(t0 + secs(100), 1, 0, false));
        // Edited, written, then fully uploaded: the file is removed.
        let _ = t.poll(t0, 1, 1, true);
        assert!(t.poll(t0 + secs(4), 1, 1, true));
        t.started(1);
        t.finished(1, 1, Some(WriteOutcome::Written(2)));
        assert!(t.has_file(1));
        assert!(!t.poll(t0 + secs(10), 1, 1, true));
        let _ = t.poll(t0 + secs(10), 1, 2, false);
        assert!(t.poll(t0 + secs(14), 1, 2, false), "removal is a write");
        t.started(1);
        t.finished(1, 2, Some(WriteOutcome::Removed));
        assert!(!t.has_file(1));
        assert!(!t.poll(t0 + secs(100), 1, 2, false));
    }

    #[test]
    fn an_adopted_layer_is_not_rewritten_until_it_changes() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        t.mark_saved(4, 12);
        assert!(!t.poll(t0 + secs(100), 4, 12, true));
        let _ = t.poll(t0 + secs(100), 4, 13, true);
        assert!(t.poll(t0 + secs(104), 4, 13, true));
    }

    /// Not a correctness test: prints what snapshotting and writing a big
    /// layer costs. Run with
    /// `cargo test --release large_layer -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn large_layer_costs() {
        let n = 300_000i64;
        let mut nodes = HashMap::new();
        for i in 1..=n {
            let t: &[(&str, &str)] = if i % 10 == 0 {
                &[("amenity", "bench"), ("name", "Some bench")]
            } else {
                &[]
            };
            nodes.insert(
                i,
                node(
                    i,
                    40.0 + (i as f64) * 1e-6,
                    -74.0 + (i as f64) * 1e-6,
                    (i % 5) as i32 + 1,
                    t,
                ),
            );
        }
        let mut ways = HashMap::new();
        for w in 0..30_000i64 {
            let first = w * 10 + 1;
            ways.insert(
                w + 1,
                OsmWay {
                    id: w + 1,
                    nodes: (first..first + 10).collect(),
                    version: 2,
                    tags: tags(&[("highway", "residential"), ("name", "Some Street")]),
                },
            );
        }
        let data = OsmData {
            nodes,
            ways,
            relations: Vec::new(),
            bounds: Vec::new(),
        };
        let t = Instant::now();
        let mut layer = OsmLayer::new_with_data(LayerId(1), "Big", Arc::new(data));
        println!("build layer (not part of autosave): {:?}", t.elapsed());
        for i in 0..200 {
            layer
                .set_tag(FeatureKind::Way, i + 1, "surface", "asphalt")
                .unwrap();
        }
        for i in 0..100 {
            layer.add_node(41.0 + i as f64 * 1e-4, -75.0);
        }

        let t = Instant::now();
        let snap = snapshot_of(&layer, 1);
        println!("UI thread: snapshot (two Arc clones): {:?}", t.elapsed());

        // An edit while the writer still holds the snapshot pays a
        // copy-on-write of the data, once.
        let t = Instant::now();
        layer
            .set_tag(FeatureKind::Way, 500, "surface", "gravel")
            .unwrap();
        println!(
            "UI thread: first edit while the writer holds the snapshot: {:?}",
            t.elapsed()
        );
        let t = Instant::now();
        layer
            .set_tag(FeatureKind::Way, 501, "surface", "gravel")
            .unwrap();
        println!("UI thread: next edit: {:?}", t.elapsed());

        let t = Instant::now();
        let rendered = render(&snap).unwrap();
        println!(
            "background: diff + serialize {} bytes: {:?}",
            rendered.osm_xml.len(),
            t.elapsed()
        );
        let root = tmp_root("big");
        let session = Session::start(&root).unwrap();
        let t = Instant::now();
        session.write_rendered(&rendered).unwrap();
        println!("background: durable write: {:?}", t.elapsed());
        let t = Instant::now();
        let rec = {
            drop(session);
            let mut crashed = find_crashed(&root, None);
            crashed.remove(0).layers.remove(0)
        };
        let (data, edits) = rec.load().unwrap();
        println!("restore: read + parse: {:?}", t.elapsed());
        let restored = OsmLayer::new_with_edits(LayerId(2), "x", Arc::new(data), &edits);
        println!(
            "restore: pending after = {}",
            edit_total(&restored.diff_for_upload())
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
