//! Error type for the virtual filesystem.
//!
//! Replaces `FSReturnCode_t` plus the `g_FileSystemError` global string
//! (`public/filesystem_init.cpp:266`) and the `bool`-return/sentinel-value
//! convention used throughout `IFileSystem`.

use std::io;
use std::path::PathBuf;

/// Anything that can go wrong resolving or reading through the [`Vfs`].
///
/// [`Vfs`]: super::Vfs
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    /// The path was not found in any mount that the lookup was allowed to search.
    #[error("file not found: {path}")]
    NotFound { path: String },

    /// The path could not be normalized — empty, or it escaped the mount root.
    #[error("invalid path {path:?}: {reason}")]
    InvalidPath { path: String, reason: &'static str },

    /// The file was located but reading it failed.
    #[error("i/o error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },

    /// No `gameinfo.txt` in the directory we were pointed at.
    #[error("no gameinfo.txt in {}", .dir.display())]
    GameInfoMissing { dir: PathBuf },

    /// `gameinfo.txt` parsed but doesn't have the structure the engine needs.
    #[error("{}: {reason}", .path.display())]
    GameInfoInvalid { path: PathBuf, reason: String },

    /// A VPK's directory could not be parsed.
    #[error("{}: malformed VPK: {reason}", .path.display())]
    Vpk { path: PathBuf, reason: String },

    /// A `.bsp`'s embedded pak file could not be read.
    #[error("{map}.bsp: malformed pak lump: {reason}")]
    Pak { map: String, reason: String },

    /// A KeyValues document could not be parsed.
    #[error("{source_name}:{line}: {reason}")]
    KeyValues {
        source_name: String,
        line: usize,
        reason: String,
    },
}

impl VfsError {
    pub(crate) fn io(path: impl Into<String>, source: io::Error) -> Self {
        VfsError::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn vpk(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        VfsError::Vpk {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, VfsError>;
