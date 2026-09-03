//! A plain directory on disk.
//!
//! Replaces `CSearchPath` with a null `CPackFile*` plus the
//! `HandleOpenRegularFile` branch of `CBaseFileSystem::FindFile`
//! (`basefilesystem.cpp:4131`), and the `FS_*` stdio wrappers in
//! `filesystem_stdio.cpp` that sit under it.
//!
//! ## Case sensitivity
//!
//! Valve content references files with inconsistent casing, which is invisible
//! on Windows and on case-insensitive macOS volumes but breaks on Linux. The
//! original patched around it with `findFileInDirCaseInsensitive`
//! (`filesystem/linux_support.cpp:208`), called as a fallback from five sites
//! after a case-sensitive miss. `portdocs/FILESYSTEM.md` records three faults
//! in it, all fixed here:
//!
//! 1. It `scandir()`s the whole containing directory on *every* miss, with no
//!    caching, so systematically-miscased content turns every open into a
//!    directory enumeration. Here each directory is enumerated at most once
//!    and the folded index is cached.
//! 2. Its `scandir` filter predicate compares against a file-scope
//!    `static char fileName[MAX_PATH]` (`linux_support.cpp:196`), so it is not
//!    thread-safe even though the filesystem around it explicitly is. The cache
//!    here is behind an `RwLock` and holds no shared scratch state.
//! 3. It is `#if defined(LINUX)` only. macOS is a first-class target for us and
//!    APFS can be case-sensitive, so the fallback runs on both.
//!
//! The exact-match fast path is kept: a correctly-cased request costs one
//! `File::open` and never enumerates anything.

use crate::filesystem::error::{Result, VfsError};
use crate::filesystem::mount::{Entry, Mount, ReadSeek};
use crate::filesystem::path::RelPath;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Maps a folded (lowercased) entry name to its real on-disk name.
type FoldedNames = HashMap<String, OsString>;

pub struct DirMount {
    root: PathBuf,
    /// Folded listings, keyed by the *real* directory path they describe.
    /// Populated lazily on the first case-insensitive miss in that directory.
    index: RwLock<HashMap<PathBuf, FoldedNames>>,
}

impl DirMount {
    /// Mounts `root`. Does not touch the filesystem — a search path naming a
    /// directory that does not exist is normal (`gameinfo.txt` lists optional
    /// DLC and language directories) and simply never resolves anything.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DirMount {
            root: root.into(),
            index: RwLock::new(HashMap::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a relative path to a real on-disk path, or `None` if absent.
    fn resolve(&self, path: &RelPath) -> Option<PathBuf> {
        // Fast path: the caller's casing is already correct.
        let direct = self.root.join(path.as_str());
        if direct.exists() {
            return Some(direct);
        }

        // Fall back to a component-by-component folded walk. Every component
        // can be miscased, not just the filename, so this cannot shortcut to
        // "fold the last component only".
        let mut current = self.root.clone();
        for want in path.folded_components() {
            current = self.resolve_child(&current, want)?;
        }
        Some(current)
    }

    /// Finds the real name of `folded_name` inside the real directory `dir`.
    fn resolve_child(&self, dir: &Path, folded_name: &str) -> Option<PathBuf> {
        if let Some(names) = self.index.read().ok()?.get(dir) {
            return names.get(folded_name).map(|real| dir.join(real));
        }

        let names = read_folded_names(dir)?;
        let resolved = names.get(folded_name).map(|real| dir.join(real));
        if let Ok(mut index) = self.index.write() {
            // Another thread may have inserted meanwhile; either copy is valid.
            index.entry(dir.to_path_buf()).or_insert(names);
        }
        resolved
    }
}

/// Enumerates `dir` into a folded-name map. `None` if it isn't a readable
/// directory.
///
/// A later entry wins on a fold collision (`Foo.txt` and `FOO.TXT` in one
/// directory). The original's `scandir` walk has the same ambiguity and
/// resolves it by directory order; neither is more correct, and real content
/// does not do this.
fn read_folded_names(dir: &Path) -> Option<FoldedNames> {
    let mut names = FoldedNames::new();
    for entry in fs::read_dir(dir).ok()? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        names.insert(name.to_string_lossy().to_ascii_lowercase(), name);
    }
    Some(names)
}

impl Mount for DirMount {
    fn open(&self, path: &RelPath) -> Option<Result<Box<dyn ReadSeek>>> {
        let real = self.resolve(path)?;
        Some(match File::open(&real) {
            Ok(file) => Ok(Box::new(file) as Box<dyn ReadSeek>),
            Err(e) => Err(VfsError::io(real.display().to_string(), e)),
        })
    }

    fn read(&self, path: &RelPath) -> Option<Result<Vec<u8>>> {
        let real = self.resolve(path)?;
        // Not `fs::read`: `resolve` accepts directories (they are legitimate
        // path components), and reading one should be an error rather than a
        // confusing success or a silent fall-through to the next mount.
        Some((|| {
            let mut file =
                File::open(&real).map_err(|e| VfsError::io(real.display().to_string(), e))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .map_err(|e| VfsError::io(real.display().to_string(), e))?;
            Ok(buf)
        })())
    }

    fn contains(&self, path: &RelPath) -> bool {
        self.resolve(path).is_some_and(|p| p.is_file())
    }

    fn list(&self, dir: Option<&RelPath>, out: &mut Vec<Entry>) {
        let real = match dir {
            None => self.root.clone(),
            Some(d) => match self.resolve(d) {
                Some(p) => p,
                None => return,
            },
        };
        let Ok(entries) = fs::read_dir(&real) else {
            return;
        };
        for entry in entries.flatten() {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            out.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
            });
        }
    }

    fn describe(&self) -> String {
        self.root.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    /// A scratch directory that removes itself on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "kisak-fs-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn write(&self, rel: &str, bytes: &[u8]) {
            let full = self.0.join(rel);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::File::create(&full).unwrap().write_all(bytes).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_an_exactly_cased_file() {
        let tmp = TempDir::new("exact");
        tmp.write("materials/metal/wall.vmt", b"LightmappedGeneric");
        let mount = DirMount::new(tmp.path());

        let p = RelPath::new("materials/metal/wall.vmt").unwrap();
        assert!(mount.contains(&p));
        assert_eq!(
            mount.read(&p).unwrap().unwrap(),
            b"LightmappedGeneric".to_vec()
        );
    }

    #[test]
    fn resolves_wrong_case_in_every_component() {
        let tmp = TempDir::new("case");
        tmp.write("Materials/Metal/Wall.VMT", b"ok");
        let mount = DirMount::new(tmp.path());

        // Every component miscased, which is what Valve content actually does.
        let p = RelPath::new("materials/metal/wall.vmt").unwrap();
        assert!(mount.contains(&p), "case-insensitive fallback failed");
        assert_eq!(mount.read(&p).unwrap().unwrap(), b"ok".to_vec());

        // And the cached index serves a repeat lookup identically.
        assert!(mount.contains(&p));
    }

    #[test]
    fn missing_files_return_none_not_an_error() {
        let tmp = TempDir::new("missing");
        let mount = DirMount::new(tmp.path());
        let p = RelPath::new("nope/absent.txt").unwrap();
        assert!(mount.read(&p).is_none());
        assert!(mount.open(&p).is_none());
        assert!(!mount.contains(&p));
    }

    #[test]
    fn a_directory_is_not_a_file() {
        let tmp = TempDir::new("isdir");
        tmp.write("maps/placeholder", b"");
        let mount = DirMount::new(tmp.path());
        assert!(!mount.contains(&RelPath::new("maps").unwrap()));
    }

    #[test]
    fn nonexistent_root_resolves_nothing() {
        let mount = DirMount::new("/definitely/not/a/real/path/for/tests");
        assert!(!mount.contains(&RelPath::new("anything.txt").unwrap()));
        let mut out = Vec::new();
        mount.list(None, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn opened_streams_seek() {
        let tmp = TempDir::new("seek");
        tmp.write("a.bin", b"0123456789");
        let mount = DirMount::new(tmp.path());

        let mut s = mount
            .open(&RelPath::new("a.bin").unwrap())
            .unwrap()
            .unwrap();
        s.seek(SeekFrom::Start(4)).unwrap();
        let mut buf = [0u8; 3];
        s.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"456");
    }

    #[test]
    fn lists_immediate_children() {
        let tmp = TempDir::new("list");
        tmp.write("maps/a.bsp", b"");
        tmp.write("maps/b.bsp", b"");
        tmp.write("maps/graphs/c.ain", b"");
        let mount = DirMount::new(tmp.path());

        let mut out = Vec::new();
        mount.list(Some(&RelPath::new("maps").unwrap()), &mut out);
        out.sort();
        assert_eq!(
            out,
            vec![
                Entry {
                    name: "a.bsp".into(),
                    is_dir: false
                },
                Entry {
                    name: "b.bsp".into(),
                    is_dir: false
                },
                Entry {
                    name: "graphs".into(),
                    is_dir: true
                },
            ]
        );
    }
}
