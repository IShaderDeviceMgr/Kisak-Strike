//! The `.bsp`'s embedded pak file — a ZIP archive mounted as a search path.
//!
//! `CZipPackFile` (`filesystem/packfile.cpp`), which the engine mounts at the
//! head of the search path when a map loads:
//!
//! ```text
//! g_pFileSystem->AddSearchPath( szNameOnDisk, "GAME", PATH_ADD_TO_HEAD );  // modelloader.cpp:4229
//! g_pFileSystem->RemoveSearchPath( szNameOnDisk, "GAME" );                 // modelloader.cpp:6269
//! ```
//!
//! **At the head, and that is the point.** `vbsp` writes the map's own
//! generated content in here — the per-cubemap material patches under
//! `materials/maps/<map>/…`, the per-prop baked lighting under `sp_*.vhv`, and
//! any model or texture the mapper embedded — and the cubemap patches share
//! their names with nothing, while a mapper's embedded copy of a shipped asset
//! is *meant* to win. Mounting it at the tail would leave the `.vhv` files
//! reachable and the overrides silently ignored.
//!
//! # What Portal 2 actually puts here
//!
//! Measured over all 106 shipped maps: 64,428 entries, **every one of them
//! stored uncompressed** (method 0). 56,955 are `.vhv` — one per static prop in
//! the game — 4,490 are `.vtf` and 2,542 `.vmt` (the cubemap patches), and 147
//! each of `.mdl`/`.vtx`/`.vvd` are models embedded by mappers.
//!
//! So **deflate is not implemented**, and that is a measurement rather than a
//! shortcut: a compressed entry is refused with a message naming the file, and
//! the day one appears is the day this port acquires a decompressor. Valve's
//! own writer defaults to storing, because the engine wants to `mmap` the lump
//! rather than inflate it. (`bspzip -compress` exists and no shipped map uses
//! it; the X360 build compressed with LZMA, which is a different mechanism
//! again and out of scope with the console.)

use std::collections::{BTreeSet, HashMap};
use std::io::Cursor;
use std::sync::Arc;

use super::{Entry, Mount, ReadSeek};
use crate::filesystem::error::{Result, VfsError};
use crate::filesystem::path::RelPath;

/// `PK\x05\x06` — end of central directory.
const EOCD_MAGIC: [u8; 4] = *b"PK\x05\x06";
/// `PK\x01\x02` — a central directory entry.
const CENTRAL_MAGIC: [u8; 4] = *b"PK\x01\x02";
/// `PK\x03\x04` — a local file header.
const LOCAL_MAGIC: [u8; 4] = *b"PK\x03\x04";

const EOCD_SIZE: usize = 22;
const CENTRAL_SIZE: usize = 46;
const LOCAL_SIZE: usize = 30;

/// The only compression method this reads. See the module docs.
const METHOD_STORE: u16 = 0;

/// One entry's location in the archive.
#[derive(Debug, Clone, Copy)]
struct PakEntry {
    /// Offset of the *local* header, which is where the data's own offset has
    /// to be computed from: the central directory's copies of the name and
    /// extra-field lengths are allowed to differ from the local ones, and in
    /// practice do.
    local_header: usize,
    size: usize,
}

/// A `.bsp`'s `LUMP_PAKFILE`, mounted.
pub struct PakMount {
    /// Which map this came from, for [`describe`](Mount::describe).
    map: String,
    /// The lump's bytes. Shared rather than copied per read, and shared with
    /// nothing else: [`Bsp`](crate::engine::world::bsp::Bsp) hands ownership
    /// over when the map loads.
    bytes: Arc<[u8]>,
    files: HashMap<String, PakEntry>,
    dirs: HashMap<String, BTreeSet<(String, bool)>>,
}

impl PakMount {
    /// Reads the central directory of a `LUMP_PAKFILE` payload.
    ///
    /// An empty lump is not an error — a map with no embedded content has one —
    /// and gives a mount with no files rather than `None`, so the caller does
    /// not have to special-case it.
    pub fn new(map: &str, bytes: Arc<[u8]>) -> Result<PakMount> {
        let bad = |reason: String| VfsError::Pak {
            map: map.to_owned(),
            reason,
        };

        let mut files = HashMap::new();
        let mut dirs: HashMap<String, BTreeSet<(String, bool)>> = HashMap::new();

        if !bytes.is_empty() {
            // The end-of-central-directory record is last, but a ZIP may carry
            // a trailing comment, so it is found by scanning backwards for the
            // signature rather than by subtracting a fixed size. Valve writes
            // no comment; other tools do.
            let start = bytes.len().saturating_sub(EOCD_SIZE + u16::MAX as usize);
            let eocd = bytes[start..]
                .windows(4)
                .rposition(|w| w == EOCD_MAGIC)
                .map(|at| start + at)
                .ok_or_else(|| bad("no end-of-central-directory record".to_owned()))?;
            if eocd + EOCD_SIZE > bytes.len() {
                return Err(bad("the end-of-central-directory record is truncated".into()));
            }

            let count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
            let mut at = u32::from_le_bytes(
                bytes[eocd + 16..eocd + 20].try_into().expect("4 bytes"),
            ) as usize;

            for i in 0..count {
                if at + CENTRAL_SIZE > bytes.len() || bytes[at..at + 4] != CENTRAL_MAGIC {
                    return Err(bad(format!(
                        "central directory entry {i} of {count} is not where the \
                         directory says it is"
                    )));
                }
                let half = |n: usize| u16::from_le_bytes([bytes[at + n], bytes[at + n + 1]]);
                let word = |n: usize| {
                    u32::from_le_bytes(bytes[at + n..at + n + 4].try_into().expect("4 bytes"))
                        as usize
                };

                let method = half(10);
                let size = word(24);
                let (name_len, extra_len, comment_len) =
                    (half(28) as usize, half(30) as usize, half(32) as usize);
                let local_header = word(42);
                let name_at = at + CENTRAL_SIZE;
                if name_at + name_len > bytes.len() {
                    return Err(bad(format!("entry {i}'s name runs past the lump")));
                }
                // Zip names are bytes with no declared encoding, and Valve
                // writes paths a mapper typed. Lossy rather than fatal.
                let name = String::from_utf8_lossy(&bytes[name_at..name_at + name_len]);

                if method != METHOD_STORE {
                    return Err(bad(format!(
                        "{name} is compressed (method {method}); this engine reads only \
                         stored entries — see src/filesystem/mount/pak.rs"
                    )));
                }

                // A directory entry — a name ending in `/` with no content.
                // Recorded through `register_dirs` by its children instead.
                if !name.ends_with('/') {
                    let folded = fold(&name);
                    register_dirs(&mut dirs, &folded);
                    files.insert(
                        folded,
                        PakEntry {
                            local_header,
                            size,
                        },
                    );
                }
                at += CENTRAL_SIZE + name_len + extra_len + comment_len;
            }
        }

        Ok(PakMount {
            map: map.to_owned(),
            bytes,
            files,
            dirs,
        })
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The entry's bytes, resolved through its **local** header.
    ///
    /// The data does not start at a fixed distance from the local header: the
    /// name and extra field come first and their lengths are stored there, not
    /// in the central directory — whose copies are permitted to differ, and do
    /// for writers that add a Zip64 or timestamp extra to one and not the
    /// other. Reading the local ones is the difference between the right bytes
    /// and bytes a few off.
    fn slice(&self, name: &str, entry: PakEntry) -> Result<&[u8]> {
        let bad = |reason: String| VfsError::Pak {
            map: self.map.clone(),
            reason,
        };
        let at = entry.local_header;
        if at + LOCAL_SIZE > self.bytes.len() || self.bytes[at..at + 4] != LOCAL_MAGIC {
            return Err(bad(format!("{name} has no local header at {at}")));
        }
        let half = |n: usize| u16::from_le_bytes([self.bytes[at + n], self.bytes[at + n + 1]]);
        let data = at + LOCAL_SIZE + half(26) as usize + half(28) as usize;
        let end = data + entry.size;
        if end > self.bytes.len() {
            return Err(bad(format!(
                "{name} claims {} bytes at {data}, past the lump's {}",
                entry.size,
                self.bytes.len()
            )));
        }
        Ok(&self.bytes[data..end])
    }
}

/// The key an entry is looked up by. Matches [`RelPath::folded`].
fn fold(name: &str) -> String {
    name.replace('\\', "/").to_ascii_lowercase()
}

/// Records every ancestor directory of `path` so that `list` can walk them.
///
/// The same shape as the VPK's, and for the same reason: a ZIP's directory
/// entries are optional and Valve's writer omits them, so the tree has to be
/// built from the file names.
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

impl Mount for PakMount {
    fn open(&self, path: &RelPath) -> Option<Result<Box<dyn ReadSeek>>> {
        // Every entry is stored and already in memory, so `open` is `read`
        // with a cursor over it. There is no streaming to preserve.
        Some(self.read(path)?.map(|bytes| {
            let boxed: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
            boxed
        }))
    }

    fn read(&self, path: &RelPath) -> Option<Result<Vec<u8>>> {
        let entry = *self.files.get(path.folded())?;
        Some(self.slice(path.folded(), entry).map(<[u8]>::to_vec))
    }

    fn contains(&self, path: &RelPath) -> bool {
        self.files.contains_key(path.folded())
    }

    fn list(&self, dir: Option<&RelPath>, out: &mut Vec<Entry>) {
        let key = dir.map(RelPath::folded).unwrap_or("");
        if let Some(children) = self.dirs.get(key) {
            out.extend(children.iter().map(|(name, is_dir)| Entry {
                name: name.clone(),
                is_dir: *is_dir,
            }));
        }
    }

    fn describe(&self) -> String {
        format!("{}.bsp/pak ({} files)", self.map, self.files.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a ZIP the way `bspzip` does: stored entries, local headers in
    /// order, then the central directory, then the EOCD.
    ///
    /// Written from the format rather than from the reader — the lesson of
    /// `portdocs/STUDIO.md` §11.1, where a fixture derived from the reader
    /// agreed with two wrong field offsets.
    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut locals = Vec::new();
        for (name, data) in entries {
            locals.push(out.len() as u32);
            out.extend_from_slice(&LOCAL_MAGIC);
            out.extend_from_slice(&[0; 4]); // version, flags
            out.extend_from_slice(&METHOD_STORE.to_le_bytes());
            out.extend_from_slice(&[0; 8]); // time, date, crc
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            // A non-zero extra field, because the whole point of reading the
            // *local* header is that its lengths are the ones that count.
            out.extend_from_slice(&4u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&[0xAB; 4]);
            out.extend_from_slice(data);
        }

        let central = out.len() as u32;
        for ((name, data), local) in entries.iter().zip(&locals) {
            out.extend_from_slice(&CENTRAL_MAGIC);
            out.extend_from_slice(&[0; 6]); // versions, flags
            out.extend_from_slice(&METHOD_STORE.to_le_bytes());
            out.extend_from_slice(&[0; 8]); // time, date, crc
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0; 2]); // extra length — deliberately not the local one
            out.extend_from_slice(&[0; 6]); // comment length, disk, internal attrs
            out.extend_from_slice(&[0; 4]); // external attrs
            out.extend_from_slice(&local.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        let central_size = out.len() as u32 - central;

        out.extend_from_slice(&EOCD_MAGIC);
        out.extend_from_slice(&[0; 4]); // disk numbers
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&central_size.to_le_bytes());
        out.extend_from_slice(&central.to_le_bytes());
        out.extend_from_slice(&[0; 2]); // comment length
        out
    }

    fn mount(entries: &[(&str, &[u8])]) -> PakMount {
        PakMount::new("test", Arc::from(zip(entries).as_slice())).expect("a well-formed zip")
    }

    fn read(pak: &PakMount, path: &str) -> Option<Vec<u8>> {
        pak.read(&RelPath::new(path).unwrap()).map(Result::unwrap)
    }

    #[test]
    fn an_entry_reads_back() {
        let pak = mount(&[("materials/a.vmt", b"one"), ("sp_hdr_0.vhv", b"two")]);
        assert_eq!(pak.len(), 2);
        assert_eq!(read(&pak, "materials/a.vmt").as_deref(), Some(&b"one"[..]));
        assert_eq!(read(&pak, "sp_hdr_0.vhv").as_deref(), Some(&b"two"[..]));
        assert_eq!(read(&pak, "materials/b.vmt"), None);
    }

    /// The data's offset comes from the **local** header's name and extra
    /// lengths, which the central directory is allowed to disagree with — and
    /// here does. Reading the central copy shifts every entry by four bytes.
    #[test]
    fn the_local_headers_extra_field_is_what_locates_the_data() {
        let pak = mount(&[("a", b"payload")]);
        assert_eq!(read(&pak, "a").as_deref(), Some(&b"payload"[..]));
    }

    /// Lookups are case-folded and slash-normalised, like every other mount.
    #[test]
    fn lookups_ignore_case_and_slashes() {
        let pak = mount(&[("Materials/Maps/Test/Thing.vmt", b"x")]);
        assert!(pak.contains(&RelPath::new("materials/maps/test/thing.vmt").unwrap()));
        assert!(pak.contains(&RelPath::new("MATERIALS\\maps\\test\\thing.VMT").unwrap()));
    }

    /// A ZIP's directory entries are optional and `bspzip` omits them, so the
    /// tree has to come from the file names.
    #[test]
    fn directories_are_derived_from_the_names() {
        let pak = mount(&[
            ("materials/maps/test/a.vmt", b"a"),
            ("materials/maps/test/b.vmt", b"b"),
            ("sp_hdr_0.vhv", b"c"),
        ]);
        let mut root = Vec::new();
        pak.list(None, &mut root);
        assert_eq!(
            root,
            [
                Entry {
                    name: "materials".into(),
                    is_dir: true
                },
                Entry {
                    name: "sp_hdr_0.vhv".into(),
                    is_dir: false
                },
            ]
        );

        let mut dir = Vec::new();
        pak.list(Some(&RelPath::new("materials/maps/test").unwrap()), &mut dir);
        assert_eq!(dir.len(), 2);
        assert!(dir.iter().all(|e| !e.is_dir));
    }

    /// A map with nothing embedded has an empty lump, which is not an error.
    #[test]
    fn an_empty_lump_mounts_as_an_empty_pak() {
        let pak = PakMount::new("test", Arc::from(&[][..])).expect("legal");
        assert!(pak.is_empty());
        assert!(!pak.contains(&RelPath::new("anything").unwrap()));
    }

    /// Deflate is refused rather than misread, and the message names the file.
    /// Every one of the 64,428 entries Portal 2 ships is stored; see the module
    /// docs for why that is a measurement and not an assumption.
    #[test]
    fn a_compressed_entry_is_refused_by_name() {
        let mut bytes = zip(&[("materials/a.vmt", b"x")]);
        // Method 8 (deflate) in the central directory entry, which is what the
        // reader looks at.
        let at = bytes
            .windows(4)
            .rposition(|w| w == CENTRAL_MAGIC)
            .expect("a central directory");
        bytes[at + 10..at + 12].copy_from_slice(&8u16.to_le_bytes());
        let Err(err) = PakMount::new("test", Arc::from(bytes.as_slice())) else {
            panic!("a compressed entry must be refused");
        };
        let text = err.to_string();
        assert!(text.contains("materials/a.vmt"), "{text}");
        assert!(text.contains("compressed"), "{text}");
    }

    /// Rubbish is refused rather than read as an empty archive, which would
    /// silently cost a map its embedded content.
    #[test]
    fn a_lump_that_is_not_a_zip_is_refused() {
        let bytes: Vec<u8> = b"this is not a zip file at all".to_vec();
        assert!(PakMount::new("test", Arc::from(bytes.as_slice())).is_err());
    }
}
