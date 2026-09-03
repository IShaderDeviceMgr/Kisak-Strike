//! VPK archives.
//!
//! Ported from `vpklib/packedstore.cpp` + `vpklib/packedstore_internal.h`,
//! reading only. The writer (`EPADD_NEWFILE`, chunk rebuilding, signing) backs
//! `vpk.exe`-style tooling and is out of scope per `portdocs/FILESYSTEM.md`.
//!
//! The byte layout is fixed — Valve's depots produce these files — so the
//! parsing here is exact. What is modernized is the *mechanism*: every read
//! goes through a bounds-checked cursor instead of the original's raw pointer
//! walks over a `CUtlVector<uint8>`, so a truncated or corrupt directory is an
//! error rather than an out-of-bounds read.
//!
//! ## Layout
//!
//! A `foo_dir.vpk` holds the directory; bulk data lives in numbered siblings
//! `foo_000.vpk`, `foo_001.vpk`, … A part naming archive `0x7fff`
//! (`VPKFILENUMBER_EMBEDDED_IN_DIR_FILE`) lives in the dir file itself, in the
//! chunk immediately after the directory.
//!
//! ```text
//! header    v2: marker u32 = 0x55aa1234, version u32 = 2, dir_size u32,
//!               embedded_chunk_size u32, chunk_hashes u32, self_hashes u32,
//!               signature u32                                     (28 bytes)
//!           v1: marker, version = 1, dir_size                     (12 bytes)
//!           headerless: no marker; the whole file is directory      (0 bytes)
//! directory dir_size bytes:
//!             "extension\0"          "" terminates
//!               "directory\0"        "" terminates
//!                 "basename\0"       "" terminates
//!                 crc u32
//!                 metadata_size u16
//!                 { archive u16 (0xffff terminates), offset u32, length u32 }*
//!                 metadata[metadata_size]
//! embedded  embedded_chunk_size bytes
//! ```
//!
//! Three details that are easy to get wrong and silently corrupt reads:
//!
//! * **The part list terminator is the bare 2-byte `0xffff` archive index**,
//!   not a full 10-byte descriptor. `CFileHeaderFixedData::HeaderSizeIncludingMetaData`
//!   (`packedstore.cpp:60`) adds `sizeof(PackFileIndex_t)` for it, not
//!   `sizeof(CFilePartDescr)`.
//! * **Metadata follows the part list**, after that terminator — and it is
//!   *prepended to the file contents*. `CPackedStore::ReadData` serves the
//!   metadata bytes first and only then seeks into the archive, offsetting by
//!   `- m_nMetaDataSize` (`packedstore.cpp:1207`). A file's logical length is
//!   `metadata_size + part lengths`, which is why `TotalDataSize()` sums them.
//! * **Names are stored fully lowercased** — `SplitFileComponents`
//!   (`packedstore.cpp:486`) calls `V_strlower` on the directory, the basename
//!   *and* the extension, and strips the directory's trailing slash. An empty
//!   directory or absent extension is stored as a single space, `" "`.

use crate::filesystem::error::{Result, VfsError};
use crate::filesystem::mount::{Entry, Mount, ReadSeek};
use crate::filesystem::path::RelPath;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const VPK_HEADER_MARKER: u32 = 0x55aa_1234;
const VPK_VERSION_1: u32 = 1;
const VPK_VERSION_2: u32 = 2;
/// `VPKFILENUMBER_EMBEDDED_IN_DIR_FILE`.
const ARCHIVE_EMBEDDED: u16 = 0x7fff;
/// `PACKFILEINDEX_END`.
const PART_LIST_END: u16 = 0xffff;

const HEADER_SIZE_V1: u64 = 12;
const HEADER_SIZE_V2: u64 = 28;

/// One `(archive, offset, length)` descriptor.
#[derive(Debug, Clone, Copy)]
struct Part {
    archive: u16,
    offset: u32,
    len: u32,
}

/// A file inside the archive.
#[derive(Debug, Clone)]
struct VpkEntry {
    #[allow(dead_code)] // Read but unused until sv_pure verification exists.
    crc: u32,
    /// Range of the retained directory blob holding this entry's metadata,
    /// which is logically the first bytes of the file.
    preload: (usize, usize),
    parts: Vec<Part>,
    total_len: u64,
}

pub struct VpkMount {
    /// Path with `_dir.vpk`/`.vpk` stripped; numbered archives hang off it.
    base: PathBuf,
    /// The file the directory was read from.
    dir_path: PathBuf,
    /// Byte offset in `dir_path` where the embedded chunk starts.
    embedded_base: u64,
    /// The directory blob, retained so entries can slice their metadata.
    dir_blob: Vec<u8>,
    /// Folded full path -> entry.
    files: HashMap<String, VpkEntry>,
    /// Folded directory path -> immediate child names (files and directories).
    dirs: HashMap<String, BTreeSet<(String, bool)>>,
}

impl VpkMount {
    /// Opens a VPK, given either `foo_dir.vpk` or `foo.vpk`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let blob = std::fs::read(path).map_err(|e| VfsError::io(path.display().to_string(), e))?;
        Self::from_bytes(path, blob)
    }

    /// Parses a VPK whose dir file contents are already in memory.
    ///
    /// Split out so the parser is testable without touching the filesystem.
    fn from_bytes(path: &Path, blob: Vec<u8>) -> Result<Self> {
        let file_len = blob.len() as u64;
        let mut cur = Cursor::new(&blob);

        // Header. Valve reads the full v2 header optimistically and rewinds if
        // the file turns out to be v1 or headerless (`packedstore.cpp:313`).
        let (header_size, dir_size) = match (cur.u32(), cur.u32(), cur.u32()) {
            (Some(VPK_HEADER_MARKER), Some(version), Some(dir_size)) => match version {
                VPK_VERSION_1 => (HEADER_SIZE_V1, u64::from(dir_size)),
                VPK_VERSION_2 => (HEADER_SIZE_V2, u64::from(dir_size)),
                other => {
                    return Err(VfsError::vpk(
                        path,
                        format!("unsupported VPK version {other}"),
                    ))
                }
            },
            // No marker: the whole file is a raw directory with no header.
            _ => (0, file_len),
        };

        if header_size + dir_size > file_len {
            return Err(VfsError::vpk(
                path,
                format!(
                    "directory of {dir_size} bytes at offset {header_size} overruns the \
                     {file_len}-byte file"
                ),
            ));
        }

        // The embedded chunk begins immediately after the directory.
        //
        // Deliberate divergence: `packedstore.cpp:1211` computes this as
        // `dir_size + sizeof(VPKDirHeader_t)`, hardcoding 28 even for a v1 file
        // whose header is 12 bytes. That is a latent bug — it would read 16
        // bytes past the true start — which never fires because the embedded
        // chunk is a v2 feature, so no v1 file has one. Using the real header
        // size is correct for v2 and strictly better for v1.
        let embedded_base = header_size + dir_size;

        let dir_start = header_size as usize;
        let dir_end = dir_start + dir_size as usize;
        let dir_blob = blob[dir_start..dir_end].to_vec();

        let (files, dirs) = parse_directory(&dir_blob)
            .ok_or_else(|| VfsError::vpk(path, "truncated or malformed directory"))?;

        let base = strip_vpk_suffixes(path);

        Ok(VpkMount {
            base,
            dir_path: path.to_path_buf(),
            embedded_base,
            dir_blob,
            files,
            dirs,
        })
    }

    /// Number of files in the archive.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// `GetDataFileName` (`packedstore.cpp:437`).
    fn archive_path(&self, archive: u16) -> PathBuf {
        if archive == ARCHIVE_EMBEDDED {
            self.dir_path.clone()
        } else {
            let mut p = self.base.clone().into_os_string();
            p.push(format!("_{archive:03}.vpk"));
            PathBuf::from(p)
        }
    }

    /// Absolute byte offset of a part within its archive file.
    fn part_offset(&self, part: &Part) -> u64 {
        let base = if part.archive == ARCHIVE_EMBEDDED {
            self.embedded_base
        } else {
            0
        };
        base + u64::from(part.offset)
    }

    fn read_entry(&self, entry: &VpkEntry) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(entry.total_len as usize);
        out.extend_from_slice(&self.dir_blob[entry.preload.0..entry.preload.1]);

        for part in &entry.parts {
            let archive = self.archive_path(part.archive);
            let name = archive.display().to_string();
            let mut file = File::open(&archive).map_err(|e| VfsError::io(name.clone(), e))?;
            file.seek(SeekFrom::Start(self.part_offset(part)))
                .map_err(|e| VfsError::io(name.clone(), e))?;
            let mut buf = vec![0u8; part.len as usize];
            file.read_exact(&mut buf)
                .map_err(|e| VfsError::io(name, e))?;
            out.extend_from_slice(&buf);
        }
        Ok(out)
    }
}

/// Strips `_dir.vpk` / `.vpk`, matching `StripTrailingString` in the
/// `CPackedStore` constructor (`packedstore.cpp:271`).
fn strip_vpk_suffixes(path: &Path) -> PathBuf {
    let mut s = path.to_string_lossy().into_owned();
    for suffix in [".vpk", "_dir"] {
        let len = s.len();
        if len >= suffix.len() && s[len - suffix.len()..].eq_ignore_ascii_case(suffix) {
            s.truncate(len - suffix.len());
        }
    }
    PathBuf::from(s)
}

/// Composes the stored `(extension, directory, basename)` triple back into a
/// path. A single space means "none" for both directory and extension.
fn compose_path(ext: &str, dir: &str, base: &str) -> String {
    let name = if ext == " " {
        base.to_string()
    } else {
        format!("{base}.{ext}")
    };
    if dir == " " || dir.is_empty() {
        name
    } else {
        format!("{dir}/{name}")
    }
}

type ParsedDirectory = (
    HashMap<String, VpkEntry>,
    HashMap<String, BTreeSet<(String, bool)>>,
);

/// Walks the three-level extension/directory/basename tree.
///
/// Returns `None` on any truncation; the caller turns that into a `VfsError`.
fn parse_directory(blob: &[u8]) -> Option<ParsedDirectory> {
    let mut files: HashMap<String, VpkEntry> = HashMap::new();
    let mut dirs: HashMap<String, BTreeSet<(String, bool)>> = HashMap::new();
    let mut cur = Cursor::new(blob);

    loop {
        let ext = cur.cstr()?;
        if ext.is_empty() {
            break;
        }
        loop {
            let dir = cur.cstr()?;
            if dir.is_empty() {
                break;
            }
            loop {
                let base = cur.cstr()?;
                if base.is_empty() {
                    break;
                }

                let crc = cur.u32()?;
                let meta_len = cur.u16()? as usize;

                let mut parts = Vec::new();
                loop {
                    let archive = cur.u16()?;
                    if archive == PART_LIST_END {
                        break;
                    }
                    parts.push(Part {
                        archive,
                        offset: cur.u32()?,
                        len: cur.u32()?,
                    });
                }

                // Metadata comes *after* the terminator, and is the leading
                // bytes of the file's contents.
                let meta_start = cur.pos;
                cur.take(meta_len)?;

                let total_len =
                    meta_len as u64 + parts.iter().map(|p| u64::from(p.len)).sum::<u64>();

                let full = compose_path(ext, dir, base);
                let folded = full.to_ascii_lowercase();

                register_dirs(&mut dirs, &folded);
                files.insert(
                    folded,
                    VpkEntry {
                        crc,
                        preload: (meta_start, meta_start + meta_len),
                        parts,
                        total_len,
                    },
                );
            }
        }
    }

    Some((files, dirs))
}

/// Records `path` and every ancestor directory into the listing index.
fn register_dirs(dirs: &mut HashMap<String, BTreeSet<(String, bool)>>, path: &str) {
    let mut parent = String::new();
    let mut rest = path;

    while let Some((head, tail)) = rest.split_once('/') {
        dirs.entry(parent.clone())
            .or_default()
            .insert((head.to_string(), true));
        parent = if parent.is_empty() {
            head.to_string()
        } else {
            format!("{parent}/{head}")
        };
        rest = tail;
    }
    dirs.entry(parent)
        .or_default()
        .insert((rest.to_string(), false));
}

impl Mount for VpkMount {
    fn open(&self, path: &RelPath) -> Option<Result<Box<dyn ReadSeek>>> {
        let entry = self.files.get(path.folded())?;
        let preload = self.dir_blob[entry.preload.0..entry.preload.1].to_vec();
        let segments = std::iter::once(Segment::Preload(preload))
            .chain(entry.parts.iter().map(|p| Segment::File {
                path: self.archive_path(p.archive),
                offset: self.part_offset(p),
                len: u64::from(p.len),
            }))
            .filter(|s| s.len() > 0)
            .collect();

        Some(Ok(Box::new(VpkReader {
            segments,
            total_len: entry.total_len,
            pos: 0,
            open: None,
        })))
    }

    fn read(&self, path: &RelPath) -> Option<Result<Vec<u8>>> {
        let entry = self.files.get(path.folded())?;
        Some(self.read_entry(entry))
    }

    fn contains(&self, path: &RelPath) -> bool {
        self.files.contains_key(path.folded())
    }

    fn list(&self, dir: Option<&RelPath>, out: &mut Vec<Entry>) {
        let key = dir.map(|d| d.folded()).unwrap_or("");
        if let Some(children) = self.dirs.get(key) {
            out.extend(children.iter().map(|(name, is_dir)| Entry {
                name: name.clone(),
                is_dir: *is_dir,
            }));
        }
    }

    fn describe(&self) -> String {
        format!("{} ({} files)", self.dir_path.display(), self.files.len())
    }
}

// ---------------------------------------------------------------------------
// Segmented reader
// ---------------------------------------------------------------------------

enum Segment {
    Preload(Vec<u8>),
    File {
        path: PathBuf,
        offset: u64,
        len: u64,
    },
}

impl Segment {
    fn len(&self) -> u64 {
        match self {
            Segment::Preload(b) => b.len() as u64,
            Segment::File { len, .. } => *len,
        }
    }
}

/// `Read + Seek` over a file's metadata prefix followed by its archive parts.
///
/// Valve's runtime only ever tracks a single part per open handle
/// (`CPackedStoreFileHandle` has one `m_nFileNumber`/`m_nFileOffset`), so
/// multi-part files are a format capability its reader cannot actually serve.
/// Supporting N parts here costs nothing and removes a silent truncation.
struct VpkReader {
    segments: Vec<Segment>,
    total_len: u64,
    pos: u64,
    /// The archive file currently open, and which segment it belongs to.
    open: Option<(usize, File)>,
}

impl Read for VpkReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.total_len {
            return Ok(0);
        }

        // Locate the segment holding `pos`.
        let mut seg_start = 0u64;
        for (idx, seg) in self.segments.iter().enumerate() {
            let seg_len = seg.len();
            if self.pos < seg_start + seg_len {
                let within = self.pos - seg_start;
                let available = (seg_len - within) as usize;
                let want = buf.len().min(available);

                let read = match seg {
                    Segment::Preload(bytes) => {
                        let from = within as usize;
                        buf[..want].copy_from_slice(&bytes[from..from + want]);
                        want
                    }
                    Segment::File { path, offset, .. } => {
                        if !matches!(self.open, Some((open_idx, _)) if open_idx == idx) {
                            self.open = Some((idx, File::open(path)?));
                        }
                        let (_, file) = self.open.as_mut().expect("just set");
                        file.seek(SeekFrom::Start(offset + within))?;
                        file.read(&mut buf[..want])?
                    }
                };

                self.pos += read as u64;
                return Ok(read);
            }
            seg_start += seg_len;
        }
        Ok(0)
    }
}

impl Seek for VpkReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(n) => self.total_len as i64 + n,
            SeekFrom::Current(n) => self.pos as i64 + n,
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start of file",
            ));
        }
        // Seeking past the end is legal and yields EOF on the next read,
        // matching `std::fs::File`.
        self.pos = target as u64;
        Ok(self.pos)
    }
}

// ---------------------------------------------------------------------------
// Bounds-checked cursor
// ---------------------------------------------------------------------------

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A NUL-terminated string. Valve writes ASCII here; a stray non-UTF-8
    /// byte is replaced rather than failing the whole archive.
    fn cstr(&mut self) -> Option<&'a str> {
        let rest = self.data.get(self.pos..)?;
        let nul = rest.iter().position(|&b| b == 0)?;
        let s = std::str::from_utf8(&rest[..nul]).ok()?;
        self.pos += nul + 1;
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a v2 VPK dir file in memory. Every file's data is embedded in
    /// the dir file, which exercises the `0x7fff` path and the embedded-chunk
    /// base offset without needing sibling archives on disk.
    /// `(extension, directory, basename, metadata, data)`.
    type BuilderFile = (String, String, String, Vec<u8>, Vec<u8>);
    /// `(basename, metadata, data)`.
    type BuilderEntry = (String, Vec<u8>, Vec<u8>);
    /// `(directory, entries)`.
    type BuilderDir = (String, Vec<BuilderEntry>);
    /// `(extension, directories)`.
    type BuilderExt = (String, Vec<BuilderDir>);

    struct VpkBuilder {
        files: Vec<BuilderFile>,
    }

    impl VpkBuilder {
        fn new() -> Self {
            VpkBuilder { files: Vec::new() }
        }

        /// `path` must already be lowercase, as Valve's writer stores it.
        fn add(mut self, path: &str, meta: &[u8], data: &[u8]) -> Self {
            let (dir, name) = match path.rsplit_once('/') {
                Some((d, n)) => (d.to_string(), n.to_string()),
                None => (" ".to_string(), path.to_string()),
            };
            let (base, ext) = match name.rsplit_once('.') {
                Some((b, e)) => (b.to_string(), e.to_string()),
                None => (name.clone(), " ".to_string()),
            };
            self.files
                .push((ext, dir, base, meta.to_vec(), data.to_vec()));
            self
        }

        fn build(self) -> Vec<u8> {
            // Group by extension, then directory, preserving insertion order.
            let mut by_ext: Vec<BuilderExt> = Vec::new();
            for (ext, dir, base, meta, data) in self.files {
                let e = match by_ext.iter_mut().find(|(k, _)| *k == ext) {
                    Some(e) => e,
                    None => {
                        by_ext.push((ext.clone(), Vec::new()));
                        by_ext.last_mut().unwrap()
                    }
                };
                let d = match e.1.iter_mut().find(|(k, _)| *k == dir) {
                    Some(d) => d,
                    None => {
                        e.1.push((dir.clone(), Vec::new()));
                        e.1.last_mut().unwrap()
                    }
                };
                d.1.push((base, meta, data));
            }

            // First pass: lay out the embedded chunk so offsets are known.
            let mut embedded = Vec::new();
            let mut offsets: HashMap<(String, String, String), (u32, u32)> = HashMap::new();
            for (ext, dirs) in &by_ext {
                for (dir, entries) in dirs {
                    for (base, _, data) in entries {
                        let off = embedded.len() as u32;
                        embedded.extend_from_slice(data);
                        offsets.insert(
                            (ext.clone(), dir.clone(), base.clone()),
                            (off, data.len() as u32),
                        );
                    }
                }
            }

            let mut dir_blob = Vec::new();
            let cstr = |v: &mut Vec<u8>, s: &str| {
                v.extend_from_slice(s.as_bytes());
                v.push(0);
            };
            for (ext, dirs) in &by_ext {
                cstr(&mut dir_blob, ext);
                for (dir, entries) in dirs {
                    cstr(&mut dir_blob, dir);
                    for (base, meta, _) in entries {
                        cstr(&mut dir_blob, base);
                        let (off, len) = offsets[&(ext.clone(), dir.clone(), base.clone())];
                        dir_blob.extend_from_slice(&0u32.to_le_bytes()); // crc
                        dir_blob.extend_from_slice(&(meta.len() as u16).to_le_bytes());
                        dir_blob.extend_from_slice(&ARCHIVE_EMBEDDED.to_le_bytes());
                        dir_blob.extend_from_slice(&off.to_le_bytes());
                        dir_blob.extend_from_slice(&len.to_le_bytes());
                        dir_blob.extend_from_slice(&PART_LIST_END.to_le_bytes());
                        dir_blob.extend_from_slice(meta);
                    }
                    dir_blob.push(0); // end of basenames
                }
                dir_blob.push(0); // end of directories
            }
            dir_blob.push(0); // end of extensions

            let mut out = Vec::new();
            out.extend_from_slice(&VPK_HEADER_MARKER.to_le_bytes());
            out.extend_from_slice(&VPK_VERSION_2.to_le_bytes());
            out.extend_from_slice(&(dir_blob.len() as u32).to_le_bytes());
            out.extend_from_slice(&(embedded.len() as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // chunk hashes
            out.extend_from_slice(&0u32.to_le_bytes()); // self hashes
            out.extend_from_slice(&0u32.to_le_bytes()); // signature
            assert_eq!(out.len() as u64, HEADER_SIZE_V2);
            out.extend_from_slice(&dir_blob);
            out.extend_from_slice(&embedded);
            out
        }
    }

    /// Parses without touching the filesystem. Only valid for archives whose
    /// files are metadata-only, since a real part read reopens the dir file.
    fn parse_only(blob: Vec<u8>) -> VpkMount {
        VpkMount::from_bytes(Path::new("/tmp/test_dir.vpk"), blob).unwrap()
    }

    /// A real `test_dir.vpk` on disk, removed on drop.
    ///
    /// Embedded parts are read back out of the dir file by seeking to
    /// `embedded_base + offset`, so exercising that path — the one most likely
    /// to be off by a header's worth of bytes — needs a real file.
    struct TempVpk {
        dir: PathBuf,
        mount: VpkMount,
    }

    impl TempVpk {
        fn new(tag: &str, blob: Vec<u8>) -> Self {
            let mut dir = std::env::temp_dir();
            dir.push(format!("kisak-vpk-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("test_dir.vpk");
            std::fs::write(&path, &blob).unwrap();
            let mount = VpkMount::open(&path).unwrap();
            TempVpk { dir, mount }
        }
    }

    impl Drop for TempVpk {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl std::ops::Deref for TempVpk {
        type Target = VpkMount;
        fn deref(&self) -> &VpkMount {
            &self.mount
        }
    }

    fn read(m: &VpkMount, path: &str) -> Option<Vec<u8>> {
        let p = RelPath::new(path).ok()?;
        m.read(&p).map(|r| r.unwrap())
    }

    #[test]
    fn reads_an_embedded_file() {
        let m = TempVpk::new(
            "embedded",
            VpkBuilder::new()
                .add("materials/metal/wall.vmt", b"", b"LightmappedGeneric")
                .build(),
        );
        assert_eq!(m.len(), 1);
        assert_eq!(
            read(&m, "materials/metal/wall.vmt").unwrap(),
            b"LightmappedGeneric"
        );
    }

    #[test]
    fn metadata_is_prepended_to_the_contents() {
        // The detail that silently corrupts every read if missed.
        let m = TempVpk::new(
            "meta",
            VpkBuilder::new()
                .add("cfg/config.cfg", b"PRELOAD--", b"REST")
                .build(),
        );
        assert_eq!(read(&m, "cfg/config.cfg").unwrap(), b"PRELOAD--REST");
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let m = TempVpk::new(
            "case",
            VpkBuilder::new().add("maps/sp_a1.bsp", b"", b"x").build(),
        );
        assert_eq!(read(&m, "Maps/SP_A1.BSP").unwrap(), b"x");
        assert_eq!(read(&m, "maps\\sp_a1.bsp").unwrap(), b"x");
    }

    #[test]
    fn root_files_and_extensionless_files() {
        let m = TempVpk::new(
            "root",
            VpkBuilder::new()
                .add("gameinfo.txt", b"", b"gi")
                .add("readme", b"", b"rd")
                .build(),
        );
        assert_eq!(read(&m, "gameinfo.txt").unwrap(), b"gi");
        assert_eq!(read(&m, "readme").unwrap(), b"rd");
    }

    #[test]
    fn missing_files_return_none() {
        let m = TempVpk::new("miss", VpkBuilder::new().add("a/b.txt", b"", b"x").build());
        assert!(read(&m, "a/c.txt").is_none());
        assert!(!m.contains(&RelPath::new("a/c.txt").unwrap()));
    }

    #[test]
    fn many_files_across_extensions_and_dirs() {
        let m = TempVpk::new(
            "many",
            VpkBuilder::new()
                .add("materials/a.vmt", b"", b"1")
                .add("materials/b.vmt", b"", b"22")
                .add("materials/sub/c.vtf", b"", b"333")
                .add("models/d.mdl", b"", b"4444")
                .build(),
        );
        assert_eq!(m.len(), 4);
        assert_eq!(read(&m, "materials/a.vmt").unwrap(), b"1");
        assert_eq!(read(&m, "materials/b.vmt").unwrap(), b"22");
        assert_eq!(read(&m, "materials/sub/c.vtf").unwrap(), b"333");
        assert_eq!(read(&m, "models/d.mdl").unwrap(), b"4444");
    }

    #[test]
    fn streaming_reader_matches_whole_file_read() {
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let m = TempVpk::new(
            "stream",
            VpkBuilder::new().add("big.bin", b"HEAD", &data).build(),
        );

        let mut expected = b"HEAD".to_vec();
        expected.extend_from_slice(&data);

        let p = RelPath::new("big.bin").unwrap();
        assert_eq!(m.read(&p).unwrap().unwrap(), expected);

        let mut s = m.open(&p).unwrap().unwrap();
        let mut got = Vec::new();
        s.read_to_end(&mut got).unwrap();
        assert_eq!(got, expected);

        // A seek that lands inside the archive segment, past the preload.
        s.seek(SeekFrom::Start(4)).unwrap();
        let mut first = [0u8; 8];
        s.read_exact(&mut first).unwrap();
        assert_eq!(&first, &data[..8]);

        // And one that straddles the preload/archive boundary.
        s.seek(SeekFrom::Start(2)).unwrap();
        let mut straddle = [0u8; 6];
        s.read_exact(&mut straddle).unwrap();
        assert_eq!(&straddle, b"AD\x00\x01\x02\x03");

        assert_eq!(s.seek(SeekFrom::End(0)).unwrap(), expected.len() as u64);
        assert_eq!(s.read(&mut [0u8; 4]).unwrap(), 0);
    }

    #[test]
    fn lists_directory_contents() {
        let m = parse_only(
            VpkBuilder::new()
                .add("materials/a.vmt", b"", b"")
                .add("materials/sub/c.vtf", b"", b"")
                .add("root.txt", b"", b"")
                .build(),
        );

        let mut out = Vec::new();
        m.list(None, &mut out);
        out.sort();
        assert_eq!(
            out,
            vec![
                Entry {
                    name: "materials".into(),
                    is_dir: true
                },
                Entry {
                    name: "root.txt".into(),
                    is_dir: false
                },
            ]
        );

        let mut out = Vec::new();
        m.list(Some(&RelPath::new("materials").unwrap()), &mut out);
        out.sort();
        assert_eq!(
            out,
            vec![
                Entry {
                    name: "a.vmt".into(),
                    is_dir: false
                },
                Entry {
                    name: "sub".into(),
                    is_dir: true
                },
            ]
        );
    }

    #[test]
    fn version_1_header_is_accepted() {
        let mut dir_blob = Vec::new();
        dir_blob.extend_from_slice(b"txt\0");
        dir_blob.extend_from_slice(b" \0"); // root directory
        dir_blob.extend_from_slice(b"a\0");
        dir_blob.extend_from_slice(&0u32.to_le_bytes()); // crc
        dir_blob.extend_from_slice(&4u16.to_le_bytes()); // metadata size
        dir_blob.extend_from_slice(&PART_LIST_END.to_le_bytes()); // no parts
        dir_blob.extend_from_slice(b"DATA"); // metadata is the whole file
        dir_blob.push(0); // end basenames
        dir_blob.push(0); // end dirs
        dir_blob.push(0); // end extensions

        let mut blob = Vec::new();
        blob.extend_from_slice(&VPK_HEADER_MARKER.to_le_bytes());
        blob.extend_from_slice(&VPK_VERSION_1.to_le_bytes());
        blob.extend_from_slice(&(dir_blob.len() as u32).to_le_bytes());
        assert_eq!(blob.len() as u64, HEADER_SIZE_V1);
        blob.extend_from_slice(&dir_blob);

        let m = parse_only(blob);
        assert_eq!(read(&m, "a.txt").unwrap(), b"DATA");
    }

    #[test]
    fn headerless_directory_is_accepted() {
        let mut blob = Vec::new();
        blob.extend_from_slice(b"txt\0 \0a\0");
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&2u16.to_le_bytes());
        blob.extend_from_slice(&PART_LIST_END.to_le_bytes());
        blob.extend_from_slice(b"hi");
        blob.extend_from_slice(&[0, 0, 0]);

        let m = parse_only(blob);
        assert_eq!(read(&m, "a.txt").unwrap(), b"hi");
    }

    #[test]
    fn rejects_a_bad_version() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&VPK_HEADER_MARKER.to_le_bytes());
        blob.extend_from_slice(&99u32.to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes());
        assert!(VpkMount::from_bytes(Path::new("x_dir.vpk"), blob).is_err());
    }

    #[test]
    fn rejects_a_truncated_directory() {
        let mut good = VpkBuilder::new().add("a/b.txt", b"", b"xyz").build();
        good.truncate(HEADER_SIZE_V2 as usize + 4);
        assert!(VpkMount::from_bytes(Path::new("x_dir.vpk"), good).is_err());
    }

    #[test]
    fn rejects_a_directory_size_that_overruns_the_file() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&VPK_HEADER_MARKER.to_le_bytes());
        blob.extend_from_slice(&VPK_VERSION_2.to_le_bytes());
        blob.extend_from_slice(&0xffff_ffffu32.to_le_bytes()); // absurd dir size
        blob.extend_from_slice(&[0u8; 16]);
        assert!(VpkMount::from_bytes(Path::new("x_dir.vpk"), blob).is_err());
    }

    // -----------------------------------------------------------------
    // Independently-encoded fixture
    //
    // Every other test here round-trips through `VpkBuilder`, i.e. through
    // this file's own understanding of the format, so a misreading of the spec
    // would cancel out and pass. These bytes were produced by a separate
    // encoder written from `vpklib/fileformat.txt` and
    // `vpklib/packedstore_internal.h` alone, so they check the reader against
    // the specification rather than against itself.
    //
    // Contents: `materials/metal/wall.vmt` and `maps/sp_a1_intro1.bsp` in
    // archive 000 (the latter with a "VBSP" metadata prefix), and
    // `cfg/autoexec.cfg` in the dir file's embedded chunk (archive 0x7fff)
    // with its own metadata prefix.
    // -----------------------------------------------------------------

    const FIXTURE_DIR_VPK: &[u8] = &[
        0x34, 0x12, 0xaa, 0x55, 0x02, 0x00, 0x00, 0x00, 0x8c, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x76, 0x6d,
        0x74, 0x00, 0x6d, 0x61, 0x74, 0x65, 0x72, 0x69, 0x61, 0x6c, 0x73, 0x2f, 0x6d, 0x65, 0x74,
        0x61, 0x6c, 0x00, 0x77, 0x61, 0x6c, 0x6c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x34, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0x62, 0x73,
        0x70, 0x00, 0x6d, 0x61, 0x70, 0x73, 0x00, 0x73, 0x70, 0x5f, 0x61, 0x31, 0x5f, 0x69, 0x6e,
        0x74, 0x72, 0x6f, 0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x34, 0x00,
        0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0xff, 0xff, 0x56, 0x42, 0x53, 0x50, 0x00, 0x00, 0x63,
        0x66, 0x67, 0x00, 0x63, 0x66, 0x67, 0x00, 0x61, 0x75, 0x74, 0x6f, 0x65, 0x78, 0x65, 0x63,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x00, 0xff, 0x7f, 0x00, 0x00, 0x00, 0x00, 0x22, 0x00,
        0x00, 0x00, 0xff, 0xff, 0x2f, 0x2f, 0x20, 0x70, 0x72, 0x65, 0x6c, 0x6f, 0x61, 0x64, 0x0a,
        0x00, 0x00, 0x00, 0x65, 0x63, 0x68, 0x6f, 0x20, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x20, 0x66,
        0x72, 0x6f, 0x6d, 0x20, 0x61, 0x6e, 0x20, 0x65, 0x6d, 0x62, 0x65, 0x64, 0x64, 0x65, 0x64,
        0x20, 0x63, 0x68, 0x75, 0x6e, 0x6b, 0x0a,
    ];

    const FIXTURE_000_VPK: &[u8] = &[
        0x22, 0x4c, 0x69, 0x67, 0x68, 0x74, 0x6d, 0x61, 0x70, 0x70, 0x65, 0x64, 0x47, 0x65, 0x6e,
        0x65, 0x72, 0x69, 0x63, 0x22, 0x20, 0x7b, 0x20, 0x22, 0x24, 0x62, 0x61, 0x73, 0x65, 0x74,
        0x65, 0x78, 0x74, 0x75, 0x72, 0x65, 0x22, 0x20, 0x22, 0x6d, 0x65, 0x74, 0x61, 0x6c, 0x2f,
        0x77, 0x61, 0x6c, 0x6c, 0x22, 0x20, 0x7d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn reads_a_vpk_from_an_independent_encoder() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("kisak-vpk-fixture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pak01_dir.vpk"), FIXTURE_DIR_VPK).unwrap();
        std::fs::write(dir.join("pak01_000.vpk"), FIXTURE_000_VPK).unwrap();

        let m = VpkMount::open(dir.join("pak01_dir.vpk")).unwrap();
        assert_eq!(m.len(), 3);

        // From numbered archive 000, no metadata prefix.
        assert_eq!(
            read(&m, "materials/metal/wall.vmt").unwrap(),
            br#""LightmappedGeneric" { "$basetexture" "metal/wall" }"#.to_vec()
        );

        // From numbered archive 000, with a metadata prefix: the prefix must
        // come first and the archive data must not be shifted by its length.
        let bsp = read(&m, "maps/sp_a1_intro1.bsp").unwrap();
        assert_eq!(&bsp[..4], b"VBSP");
        assert_eq!(bsp.len(), 4 + 64);
        assert!(bsp[4..].iter().all(|&b| b == 0));

        // From the embedded chunk in the dir file, which is where an incorrect
        // embedded-chunk base offset shows up.
        assert_eq!(
            read(&m, "cfg/autoexec.cfg").unwrap(),
            b"// preload
echo hello from an embedded chunk
"
            .to_vec()
        );

        // And the streaming reader agrees with the whole-file reads.
        for path in [
            "materials/metal/wall.vmt",
            "maps/sp_a1_intro1.bsp",
            "cfg/autoexec.cfg",
        ] {
            let rel = RelPath::new(path).unwrap();
            let mut s = m.open(&rel).unwrap().unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).unwrap();
            assert_eq!(got, m.read(&rel).unwrap().unwrap(), "streaming {path}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strips_suffixes_to_find_sibling_archives() {
        assert_eq!(
            strip_vpk_suffixes(Path::new("/g/portal2/pak01_dir.vpk")),
            PathBuf::from("/g/portal2/pak01")
        );
        assert_eq!(
            strip_vpk_suffixes(Path::new("/g/portal2/pak01.vpk")),
            PathBuf::from("/g/portal2/pak01")
        );
    }

    #[test]
    fn numbered_archive_paths() {
        let m = parse_only(VpkBuilder::new().add("a.txt", b"", b"x").build());
        assert_eq!(m.archive_path(0), PathBuf::from("/tmp/test_000.vpk"));
        assert_eq!(m.archive_path(7), PathBuf::from("/tmp/test_007.vpk"));
        assert_eq!(m.archive_path(123), PathBuf::from("/tmp/test_123.vpk"));
        assert_eq!(
            m.archive_path(ARCHIVE_EMBEDDED),
            PathBuf::from("/tmp/test_dir.vpk")
        );
    }
}
