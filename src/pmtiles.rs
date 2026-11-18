//! Reader for PMTiles v3 archives: a single file of map tiles that is read
//! with byte ranges, so an archive on a plain web server (or a local file) is
//! a tile source with no tile server.
//!
//! An archive is a 127-byte header, a root directory, optional leaf
//! directories and the tile data. A directory is a sorted list of entries
//! mapping a tile's Hilbert-curve id to a range of the tile data (or, with a
//! run length of 0, to a leaf directory). Looking a tile up reads at most the
//! root and two leaf directories (cached) plus the tile itself.
//!
//! Spec: <https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md>

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};

use crate::coordinates::GeoBounds;
use crate::http::{fetch_with_retries, HttpClient, HttpRequest, RetryPolicy};

const MAGIC: &[u8; 7] = b"PMTiles";
const HEADER_LEN: usize = 127;
/// The header and (almost always) the root directory are required to fit in
/// the first 16 KiB of the file.
const FIRST_READ: u64 = 16_384;
/// Leaf directories nest at most this deep.
const MAX_DIR_DEPTH: usize = 4;
/// A tile or directory larger than this is treated as corrupt, not read.
const MAX_BLOB: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Unknown,
    None,
    Gzip,
    Brotli,
    Zstd,
}

impl Compression {
    fn from_byte(b: u8) -> Self {
        match b {
            1 => Compression::None,
            2 => Compression::Gzip,
            3 => Compression::Brotli,
            4 => Compression::Zstd,
            _ => Compression::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileType {
    Unknown,
    Mvt,
    Png,
    Jpeg,
    Webp,
    Avif,
}

impl TileType {
    fn from_byte(b: u8) -> Self {
        match b {
            1 => TileType::Mvt,
            2 => TileType::Png,
            3 => TileType::Jpeg,
            4 => TileType::Webp,
            5 => TileType::Avif,
            _ => TileType::Unknown,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TileType::Unknown => "unknown",
            TileType::Mvt => "MVT",
            TileType::Png => "PNG",
            TileType::Jpeg => "JPEG",
            TileType::Webp => "WebP",
            TileType::Avif => "AVIF",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub root_dir: (u64, u64),
    pub metadata: (u64, u64),
    pub leaf_dirs_offset: u64,
    pub tile_data_offset: u64,
    pub internal_compression: Compression,
    pub tile_compression: Compression,
    pub tile_type: TileType,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// The archive's extent in degrees.
    pub bounds: GeoBounds,
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

fn e7_at(b: &[u8], at: usize) -> f64 {
    f64::from(i32::from_le_bytes(
        b[at..at + 4].try_into().expect("4 bytes"),
    )) / 1e7
}

impl Header {
    pub fn parse(b: &[u8]) -> Result<Header> {
        if b.len() < HEADER_LEN || &b[..7] != MAGIC {
            bail!("not a PMTiles archive");
        }
        if b[7] != 3 {
            bail!("unsupported PMTiles version {} (only 3 is supported)", b[7]);
        }
        Ok(Header {
            root_dir: (u64_at(b, 8), u64_at(b, 16)),
            metadata: (u64_at(b, 24), u64_at(b, 32)),
            leaf_dirs_offset: u64_at(b, 40),
            tile_data_offset: u64_at(b, 56),
            internal_compression: Compression::from_byte(b[97]),
            tile_compression: Compression::from_byte(b[98]),
            tile_type: TileType::from_byte(b[99]),
            min_zoom: b[100],
            max_zoom: b[101],
            // The header stores min_lon, min_lat, max_lon, max_lat.
            bounds: GeoBounds::new(e7_at(b, 106), e7_at(b, 114), e7_at(b, 102), e7_at(b, 110)),
        })
    }
}

/// One directory entry. A `run_length` of 0 makes it a pointer to a leaf
/// directory (`offset`/`length` are then relative to the leaf directories).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub tile_id: u64,
    pub offset: u64,
    pub length: u32,
    pub run_length: u32,
}

struct Varints<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Varints<'_> {
    fn next(&mut self) -> Result<u64> {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let b = *self
                .bytes
                .get(self.at)
                .ok_or_else(|| anyhow!("directory ends mid-number"))?;
            self.at += 1;
            if shift >= 64 {
                bail!("directory number too large");
            }
            value |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }
}

/// Decode a decompressed directory.
pub fn parse_directory(bytes: &[u8]) -> Result<Vec<Entry>> {
    let mut r = Varints { bytes, at: 0 };
    let count = r.next()? as usize;
    // Every entry takes at least four bytes, so a count beyond that is junk.
    if count > bytes.len() {
        bail!("directory claims {count} entries");
    }
    let mut entries = vec![
        Entry {
            tile_id: 0,
            offset: 0,
            length: 0,
            run_length: 0,
        };
        count
    ];
    let mut last_id = 0u64;
    for e in &mut entries {
        last_id = last_id.wrapping_add(r.next()?);
        e.tile_id = last_id;
    }
    for e in &mut entries {
        e.run_length = r.next()? as u32;
    }
    for e in &mut entries {
        e.length = r.next()? as u32;
    }
    for i in 0..count {
        let v = r.next()?;
        entries[i].offset = if v == 0 && i > 0 {
            entries[i - 1].offset + u64::from(entries[i - 1].length)
        } else {
            v.saturating_sub(1)
        };
    }
    Ok(entries)
}

/// The Hilbert-curve id PMTiles gives tile `z/x/y`: tiles are numbered by zoom
/// first, then along the curve.
pub fn tile_id(z: u8, x: u32, y: u32) -> u64 {
    let z = u32::from(z);
    let mut acc: u64 = (0..z).map(|t| 1u64 << (2 * t)).sum();
    let n: u64 = 1 << z;
    let (mut tx, mut ty) = (u64::from(x), u64::from(y));
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from(tx & s > 0);
        let ry = u64::from(ty & s > 0);
        acc += s * s * ((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                tx = n - 1 - tx;
                ty = n - 1 - ty;
            }
            std::mem::swap(&mut tx, &mut ty);
        }
        s /= 2;
    }
    acc
}

/// Where the bytes of an archive come from.
pub trait ByteSource: Send + Sync {
    /// Up to `len` bytes from `offset` (fewer at the end of the archive).
    fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>>;
}

/// An archive on a web server that honours `Range` requests.
pub struct HttpSource {
    client: Arc<dyn HttpClient>,
    url: String,
}

impl HttpSource {
    pub fn new(client: Arc<dyn HttpClient>, url: impl Into<String>) -> Self {
        Self {
            client,
            url: url.into(),
        }
    }
}

impl ByteSource for HttpSource {
    fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let req = HttpRequest::get(self.url.clone())
            .header("Range", format!("bytes={}-{}", offset, offset + len - 1));
        let resp = fetch_with_retries(self.client.as_ref(), &req, &RetryPolicy::standard())
            .map_err(|e| anyhow!("{}: {e}", self.url))?;
        if resp.status == 206 {
            return Ok(resp.body);
        }
        // A server that ignores `Range` sends the whole file: take the part
        // asked for (a huge archive on such a server is not usable anyway).
        let body = resp.body;
        let start = offset as usize;
        if body.len() <= start {
            return Ok(Vec::new());
        }
        let end = (start + len as usize).min(body.len());
        Ok(body[start..end].to_vec())
    }
}

/// An archive in a local file.
pub struct FileSource {
    path: PathBuf,
}

impl FileSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl ByteSource for FileSource {
    fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let mut f = std::fs::File::open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = Vec::new();
        f.take(len).read_to_end(&mut buf)?;
        Ok(buf)
    }
}

/// Where an archive is: a web address or a file on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSpec {
    Url(String),
    File(PathBuf),
}

impl SourceSpec {
    /// What the user typed: `http(s)://…` is an address, `file://…` or
    /// anything else a path (with a leading `~/` meaning the home directory).
    /// `None` for blank input.
    pub fn parse(input: &str) -> Option<SourceSpec> {
        let s = input.trim();
        if s.is_empty() {
            return None;
        }
        let lower = s.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return Some(SourceSpec::Url(s.to_string()));
        }
        let path = s.strip_prefix("file://").unwrap_or(s);
        if let Some(rest) = path.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return Some(SourceSpec::File(home.join(rest)));
            }
        }
        Some(SourceSpec::File(PathBuf::from(path)))
    }

    /// A layer name taken from the archive's file name, without its extension.
    pub fn default_name(&self) -> String {
        let last = match self {
            SourceSpec::Url(u) => u
                .split(['?', '#'])
                .next()
                .unwrap_or(u)
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string(),
            SourceSpec::File(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let stem = last.strip_suffix(".pmtiles").unwrap_or(&last);
        if stem.is_empty() {
            "PMTiles".to_string()
        } else {
            stem.to_string()
        }
    }

    /// Open the archive (reads its header and root directory).
    pub fn open(&self, client: Arc<dyn HttpClient>) -> Result<Archive> {
        let source: Box<dyn ByteSource> = match self {
            SourceSpec::Url(u) => Box::new(HttpSource::new(client, u.clone())),
            SourceSpec::File(p) => Box::new(FileSource::new(p.clone())),
        };
        Archive::open(source)
    }
}

fn decompress(bytes: Vec<u8>, how: Compression) -> Result<Vec<u8>> {
    match how {
        // "Unknown" is what writers that predate the field leave: treat the
        // bytes as they are.
        Compression::None | Compression::Unknown => Ok(bytes),
        Compression::Gzip => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(bytes.as_slice())
                .take(MAX_BLOB * 4)
                .read_to_end(&mut out)
                .context("gunzip")?;
            Ok(out)
        }
        Compression::Brotli => bail!("brotli-compressed archives are not supported"),
        Compression::Zstd => bail!("zstd-compressed archives are not supported"),
    }
}

/// An opened archive. Cheap to share; reads happen on the calling thread.
pub struct Archive {
    source: Box<dyn ByteSource>,
    pub header: Header,
    root: Arc<Vec<Entry>>,
    leaves: Mutex<HashMap<u64, Arc<Vec<Entry>>>>,
}

impl Archive {
    /// Read the header and root directory.
    pub fn open(source: Box<dyn ByteSource>) -> Result<Archive> {
        let first = source.read(0, FIRST_READ)?;
        let header = Header::parse(&first)?;
        if header.tile_type != TileType::Mvt {
            bail!(
                "this archive holds {} tiles; only vector (MVT) archives can be shown as an overlay",
                header.tile_type.name()
            );
        }
        let (root_off, root_len) = header.root_dir;
        let root_bytes = if root_off + root_len <= first.len() as u64 {
            first[root_off as usize..(root_off + root_len) as usize].to_vec()
        } else {
            source.read(root_off, root_len)?
        };
        let root = parse_directory(&decompress(root_bytes, header.internal_compression)?)?;
        Ok(Archive {
            source,
            header,
            root: Arc::new(root),
            leaves: Mutex::new(HashMap::new()),
        })
    }

    fn leaf(&self, entry: &Entry) -> Result<Arc<Vec<Entry>>> {
        let key = entry.offset;
        if let Some(d) = self
            .leaves
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Ok(d.clone());
        }
        if u64::from(entry.length) > MAX_BLOB {
            bail!("leaf directory of {} bytes", entry.length);
        }
        let raw = self.source.read(
            self.header.leaf_dirs_offset + entry.offset,
            u64::from(entry.length),
        )?;
        let dir = Arc::new(parse_directory(&decompress(
            raw,
            self.header.internal_compression,
        )?)?);
        self.leaves
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, dir.clone());
        Ok(dir)
    }

    /// The (decompressed) MVT bytes of tile `z/x/y`, or `None` when the
    /// archive has no such tile.
    pub fn tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        if z < self.header.min_zoom || z > self.header.max_zoom {
            return Ok(None);
        }
        let id = tile_id(z, x, y);
        let mut dir = self.root.clone();
        for _ in 0..MAX_DIR_DEPTH {
            // The last entry that starts at or before the tile.
            let i = dir.partition_point(|e| e.tile_id <= id);
            let Some(e) = i.checked_sub(1).map(|i| dir[i]) else {
                return Ok(None);
            };
            if e.run_length == 0 {
                dir = self.leaf(&e)?;
                continue;
            }
            if id >= e.tile_id + u64::from(e.run_length) {
                return Ok(None);
            }
            if u64::from(e.length) > MAX_BLOB {
                bail!("tile of {} bytes", e.length);
            }
            let raw = self
                .source
                .read(self.header.tile_data_offset + e.offset, u64::from(e.length))?;
            return decompress(raw, self.header.tile_compression).map(Some);
        }
        bail!("directories nest too deeply")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_ids_follow_the_hilbert_curve_by_zoom() {
        assert_eq!(tile_id(0, 0, 0), 0);
        assert_eq!(tile_id(1, 0, 0), 1);
        assert_eq!(tile_id(1, 0, 1), 2);
        assert_eq!(tile_id(1, 1, 1), 3);
        assert_eq!(tile_id(1, 1, 0), 4);
        // First tile of z2 follows the 1 + 4 tiles above it.
        assert_eq!(tile_id(2, 0, 0), 5);
    }

    #[test]
    fn a_zoom_is_a_continuous_walk_over_its_tiles() {
        // z3 holds ids 21..85; consecutive ids are edge-adjacent tiles.
        let mut at: Vec<Option<(i32, i32)>> = vec![None; 64];
        for x in 0..8 {
            for y in 0..8 {
                let id = tile_id(3, x, y);
                assert!((21..85).contains(&id));
                assert!(at[(id - 21) as usize]
                    .replace((x as i32, y as i32))
                    .is_none());
            }
        }
        for w in at.windows(2) {
            let (a, b) = (w[0].unwrap(), w[1].unwrap());
            assert_eq!((a.0 - b.0).abs() + (a.1 - b.1).abs(), 1);
        }
    }

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    /// Encode a directory: ids, run lengths, lengths, then offsets (0 meaning
    /// "right after the previous entry").
    fn encode_dir(entries: &[Entry]) -> Vec<u8> {
        let mut out = Vec::new();
        varint(entries.len() as u64, &mut out);
        let mut last = 0;
        for e in entries {
            varint(e.tile_id - last, &mut out);
            last = e.tile_id;
        }
        for e in entries {
            varint(u64::from(e.run_length), &mut out);
        }
        for e in entries {
            varint(u64::from(e.length), &mut out);
        }
        for (i, e) in entries.iter().enumerate() {
            let contiguous =
                i > 0 && e.offset == entries[i - 1].offset + u64::from(entries[i - 1].length);
            varint(if contiguous { 0 } else { e.offset + 1 }, &mut out);
        }
        out
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    }

    struct Bytes(Vec<u8>);
    impl ByteSource for Bytes {
        fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
            let s = (offset as usize).min(self.0.len());
            let e = (s + len as usize).min(self.0.len());
            Ok(self.0[s..e].to_vec())
        }
    }

    /// A tiny archive: z0 and z1 tiles in the root, one z2 tile behind a
    /// leaf directory; gzip everywhere.
    fn sample() -> Vec<u8> {
        let tile0 = gzip(b"tile-zero");
        let tile1 = gzip(b"tile-one");
        let tile2 = gzip(b"tile-two");
        let mut data = Vec::new();
        let mut at = |b: &[u8]| {
            let o = data.len() as u64;
            data.extend_from_slice(b);
            (o, b.len() as u32)
        };
        let (o0, l0) = at(&tile0);
        let (o1, l1) = at(&tile1);
        let (o2, l2) = at(&tile2);

        let leaf = gzip(&encode_dir(&[Entry {
            tile_id: tile_id(2, 3, 3),
            offset: o2,
            length: l2,
            run_length: 1,
        }]));
        let root = gzip(&encode_dir(&[
            Entry {
                tile_id: 0,
                offset: o0,
                length: l0,
                run_length: 1,
            },
            // One entry covering z1 tiles 1..=2 (a run of the same tile).
            Entry {
                tile_id: 1,
                offset: o1,
                length: l1,
                run_length: 2,
            },
            // A leaf covers everything from the first z2 tile on.
            Entry {
                tile_id: 5,
                offset: 0,
                length: leaf.len() as u32,
                run_length: 0,
            },
        ]));

        let root_off = HEADER_LEN as u64;
        let leaf_off = root_off + root.len() as u64;
        let data_off = leaf_off + leaf.len() as u64;
        let mut h = vec![0u8; HEADER_LEN];
        h[..7].copy_from_slice(MAGIC);
        h[7] = 3;
        h[8..16].copy_from_slice(&root_off.to_le_bytes());
        h[16..24].copy_from_slice(&(root.len() as u64).to_le_bytes());
        h[40..48].copy_from_slice(&leaf_off.to_le_bytes());
        h[56..64].copy_from_slice(&data_off.to_le_bytes());
        h[97] = 2; // internal: gzip
        h[98] = 2; // tiles: gzip
        h[99] = 1; // MVT
        h[100] = 0;
        h[101] = 2;
        h[102..106].copy_from_slice(&(-1_800_000_000i32).to_le_bytes());
        h[106..110].copy_from_slice(&(-850_000_000i32).to_le_bytes());
        h[110..114].copy_from_slice(&1_790_000_000i32.to_le_bytes());
        h[114..118].copy_from_slice(&840_000_000i32.to_le_bytes());
        let mut out = h;
        out.extend(root);
        out.extend(leaf);
        out.extend(data);
        out
    }

    #[test]
    fn directories_round_trip() {
        let entries = vec![
            Entry {
                tile_id: 3,
                offset: 0,
                length: 10,
                run_length: 1,
            },
            Entry {
                tile_id: 9,
                offset: 10,
                length: 5,
                run_length: 4,
            },
            Entry {
                tile_id: 40,
                offset: 100,
                length: 7,
                run_length: 0,
            },
        ];
        assert_eq!(parse_directory(&encode_dir(&entries)).unwrap(), entries);
    }

    #[test]
    fn a_truncated_directory_is_an_error_not_a_panic() {
        let bytes = encode_dir(&[Entry {
            tile_id: 3,
            offset: 0,
            length: 10,
            run_length: 1,
        }]);
        assert!(parse_directory(&bytes[..bytes.len() - 1]).is_err());
        assert!(parse_directory(&[0xff; 12]).is_err());
    }

    #[test]
    fn the_header_is_read() {
        let a = Archive::open(Box::new(Bytes(sample()))).unwrap();
        assert_eq!(a.header.tile_type, TileType::Mvt);
        assert_eq!(a.header.max_zoom, 2);
        assert_eq!(a.header.bounds, GeoBounds::new(-85.0, 84.0, -180.0, 179.0));
    }

    #[test]
    fn tiles_are_found_in_the_root_a_run_and_a_leaf() {
        let a = Archive::open(Box::new(Bytes(sample()))).unwrap();
        assert_eq!(a.tile(0, 0, 0).unwrap().unwrap(), b"tile-zero");
        // Both z1 tiles of the run share one blob.
        assert_eq!(a.tile(1, 0, 0).unwrap().unwrap(), b"tile-one");
        assert_eq!(a.tile(1, 0, 1).unwrap().unwrap(), b"tile-one");
        // z1 (1,1) is past the run.
        assert_eq!(a.tile(1, 1, 1).unwrap(), None);
        assert_eq!(a.tile(2, 3, 3).unwrap().unwrap(), b"tile-two");
        assert_eq!(a.tile(2, 0, 0).unwrap(), None);
        // Outside the archive's zooms.
        assert_eq!(a.tile(3, 0, 0).unwrap(), None);
    }

    #[test]
    fn non_archives_and_raster_archives_are_refused() {
        assert!(Archive::open(Box::new(Bytes(b"hello".to_vec()))).is_err());
        let mut raster = sample();
        raster[99] = 2;
        let err = Archive::open(Box::new(Bytes(raster))).err().unwrap();
        assert!(err.to_string().contains("PNG"));
    }

    #[test]
    fn sources_are_told_apart_and_named_after_their_file() {
        let url = SourceSpec::parse(" https://example.com/maps/city.pmtiles?x=1 ").unwrap();
        assert_eq!(
            url,
            SourceSpec::Url("https://example.com/maps/city.pmtiles?x=1".into())
        );
        assert_eq!(url.default_name(), "city");
        let file = SourceSpec::parse("/data/roads.pmtiles").unwrap();
        assert_eq!(file, SourceSpec::File("/data/roads.pmtiles".into()));
        assert_eq!(file.default_name(), "roads");
        assert_eq!(
            SourceSpec::parse("file:///data/a.pmtiles"),
            Some(SourceSpec::File("/data/a.pmtiles".into()))
        );
        assert_eq!(SourceSpec::parse("   "), None);
        assert_eq!(
            SourceSpec::parse("https://example.com/")
                .unwrap()
                .default_name(),
            "PMTiles"
        );
    }

    #[test]
    fn a_ranged_http_read_sends_the_range_and_accepts_206() {
        use crate::http::fake::{ok, FakeClient};
        let client = Arc::new(FakeClient::new(vec![ok(206, "abc")]));
        let src = HttpSource::new(client.clone(), "https://example.com/a.pmtiles");
        assert_eq!(src.read(10, 3).unwrap(), b"abc");
        let reqs = client.requests.lock().unwrap();
        assert!(reqs[0]
            .headers
            .iter()
            .any(|(k, v)| k == "Range" && v == "bytes=10-12"));
    }

    #[test]
    fn a_server_that_ignores_range_still_yields_the_right_slice() {
        use crate::http::fake::{ok, FakeClient};
        let client = Arc::new(FakeClient::new(vec![ok(200, "0123456789")]));
        let src = HttpSource::new(client, "https://example.com/a.pmtiles");
        assert_eq!(src.read(2, 3).unwrap(), b"234");
    }
}
