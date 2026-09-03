//! Relative game paths.
//!
//! Replaces the ad-hoc `char[MAX_PATH]` buffers that `basefilesystem.cpp`
//! passes around, along with `V_FixSlashes`/`V_RemoveDotSlashes` and the
//! `//pathid/file` syntax parsed by `ParsePathID` (`basefilesystem.cpp:4309`,
//! the one Valve annotated `FIXME: Pain!`). Path IDs are a separate argument
//! here — see [`PathId`] — so there is nothing to parse out of the string and
//! nothing for a stray `V_FixSlashes` call to mangle.
//!
//! [`PathId`]: super::PathId

use crate::filesystem::error::{Result, VfsError};
use std::path::{Component, Path, PathBuf};

/// A normalized path relative to a mount root.
///
/// Normalization collapses separators (both `/` and `\`, since Valve content
/// contains Windows-authored paths), removes `.` components, resolves `..`
/// lexically, and rejects anything that would escape the mount root.
///
/// Two spellings are kept: [`as_str`](Self::as_str) preserves the caller's
/// case for the exact-match fast path, and [`folded`](Self::folded) is the
/// lowercased form used as the lookup key everywhere case-insensitivity is
/// needed. See the module docs on `mount::dir` for why both exist.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RelPath {
    path: String,
    folded: String,
}

impl RelPath {
    /// Normalizes `raw`. Fails on an empty result or a `..` that escapes root.
    pub fn new(raw: &str) -> Result<Self> {
        let mut parts: Vec<&str> = Vec::new();

        for comp in raw.split(['/', '\\']) {
            match comp {
                // Repeated separators and `.` contribute nothing. Valve's
                // SplitFileComponents does the same collapsing when writing
                // VPK directory names (vpklib/packedstore.cpp:490).
                "" | "." => continue,
                ".." => {
                    if parts.pop().is_none() {
                        return Err(VfsError::InvalidPath {
                            path: raw.to_string(),
                            reason: "escapes the mount root",
                        });
                    }
                }
                other => parts.push(other),
            }
        }

        if parts.is_empty() {
            return Err(VfsError::InvalidPath {
                path: raw.to_string(),
                reason: "empty after normalization",
            });
        }

        let path = parts.join("/");
        let folded = path.to_ascii_lowercase();
        Ok(RelPath { path, folded })
    }

    /// The normalized path, with the caller's original casing preserved.
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// The normalized path, ASCII-lowercased — the case-insensitive lookup key.
    ///
    /// ASCII rather than Unicode lowercasing on purpose: it matches Valve's
    /// `V_strlower`, so a VPK directory name written by Valve's tooling folds
    /// to exactly the same bytes we compute here.
    pub fn folded(&self) -> &str {
        &self.folded
    }

    /// Path components, in order. Never empty.
    pub fn components(&self) -> std::str::Split<'_, char> {
        self.path.split('/')
    }

    /// Folded path components, in order. Never empty.
    pub fn folded_components(&self) -> std::str::Split<'_, char> {
        self.folded.split('/')
    }

    /// Splits into the parent directory (folded, `None` at the root) and the
    /// final component (folded).
    pub fn folded_split(&self) -> (Option<&str>, &str) {
        match self.folded.rsplit_once('/') {
            Some((dir, name)) => (Some(dir), name),
            None => (None, &self.folded),
        }
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.path)
    }
}

/// Resolves `.` and `..` in an absolute path without touching the filesystem.
///
/// This is `V_RemoveDotSlashes` (`FileSystem_AddLoadedSearchPath` calls it at
/// `public/filesystem_init.cpp:648` and treats failure as fatal). Purely
/// lexical: it does not resolve symlinks, matching the original, which matters
/// because search paths are compared as strings for deduplication.
pub fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop a real directory name; never pop the root prefix.
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Joins `location` onto `base` unless it is already absolute.
///
/// `Q_MakeAbsolutePath` (`public/filesystem_init.cpp:644`), then the
/// `V_RemoveDotSlashes` that follows it.
pub fn make_absolute(base: &Path, location: &str) -> PathBuf {
    // Valve content and gameinfo files may use backslashes; on POSIX those are
    // ordinary filename characters, so translate before joining.
    let location = location.replace('\\', "/");
    let candidate = Path::new(&location);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    lexically_normalize(&joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_separators_and_dots() {
        let p = RelPath::new("materials\\metal//wall.vmt").unwrap();
        assert_eq!(p.as_str(), "materials/metal/wall.vmt");

        let p = RelPath::new("./maps/./sp_a1_intro1.bsp").unwrap();
        assert_eq!(p.as_str(), "maps/sp_a1_intro1.bsp");

        let p = RelPath::new("materials/metal/../concrete/floor.vmt").unwrap();
        assert_eq!(p.as_str(), "materials/concrete/floor.vmt");
    }

    #[test]
    fn preserves_case_and_folds_separately() {
        let p = RelPath::new("Materials/Metal/Wall.VMT").unwrap();
        assert_eq!(p.as_str(), "Materials/Metal/Wall.VMT");
        assert_eq!(p.folded(), "materials/metal/wall.vmt");
    }

    #[test]
    fn strips_leading_and_trailing_separators() {
        let p = RelPath::new("/cfg/config.cfg/").unwrap();
        assert_eq!(p.as_str(), "cfg/config.cfg");
    }

    #[test]
    fn rejects_escaping_and_empty() {
        assert!(RelPath::new("../secrets").is_err());
        assert!(RelPath::new("materials/../../etc/passwd").is_err());
        assert!(RelPath::new("").is_err());
        assert!(RelPath::new("///").is_err());
        assert!(RelPath::new("./.").is_err());
    }

    #[test]
    fn folded_split_finds_parent() {
        let p = RelPath::new("materials/metal/wall.vmt").unwrap();
        assert_eq!(p.folded_split(), (Some("materials/metal"), "wall.vmt"));

        let p = RelPath::new("gameinfo.txt").unwrap();
        assert_eq!(p.folded_split(), (None, "gameinfo.txt"));
    }

    #[test]
    fn lexical_normalization_keeps_root() {
        assert_eq!(
            lexically_normalize(Path::new("/games/portal2/./portal2/../portal2")),
            PathBuf::from("/games/portal2/portal2")
        );
        // A `..` that would climb above root is retained rather than silently
        // dropped, so the resulting path fails loudly at open() time.
        assert_eq!(lexically_normalize(Path::new("/..")), PathBuf::from("/.."));
    }

    #[test]
    fn absolute_locations_ignore_the_base() {
        assert_eq!(
            make_absolute(Path::new("/games/portal2"), "portal2"),
            PathBuf::from("/games/portal2/portal2")
        );
        assert_eq!(
            make_absolute(Path::new("/games/portal2"), "/elsewhere/hl2"),
            PathBuf::from("/elsewhere/hl2")
        );
        assert_eq!(
            make_absolute(Path::new("/games/portal2"), "."),
            PathBuf::from("/games/portal2")
        );
    }
}
