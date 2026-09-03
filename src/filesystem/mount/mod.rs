//! Mounts: the individual sources a [`Vfs`] searches.
//!
//! Replaces `CSearchPath` + `CPackFile` (`filesystem/basefilesystem.h:754`),
//! where a search path was a directory *or* a pack file depending on whether a
//! `CPackFile*` member was null. Here they're distinct implementors of one
//! trait, so there is no null to check and no branch to forget.
//!
//! [`Vfs`]: super::Vfs

pub mod dir;
pub mod vpk;

use crate::filesystem::error::Result;
use crate::filesystem::path::RelPath;
use std::io::{Read, Seek};

/// A seekable byte stream.
///
/// Replaces `FileHandle_t`, which is `typedef void *` in `public/filesystem.h`.
/// `Close` is gone: the handle closes when this drops.
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// One entry returned by [`Mount::list`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    /// The entry's own name, not a path. Original case as stored.
    pub name: String,
    pub is_dir: bool,
}

/// One mounted source of files.
///
/// Every lookup returns `Option<Result<_>>`: `None` means "this mount does not
/// have that path, keep searching", `Some(Err(_))` means "it is mine and
/// reading it failed". Collapsing those two into one error is how the original
/// ends up treating an unreadable file as a missing one and silently falling
/// through to a stale copy in a later search path.
pub trait Mount: Send + Sync {
    /// Opens a stream, if this mount has the path.
    fn open(&self, path: &RelPath) -> Option<Result<Box<dyn ReadSeek>>>;

    /// Reads a whole file, if this mount has the path.
    ///
    /// Separate from `open` because a VPK can serve this without constructing
    /// a seekable reader, and whole-file reads are the overwhelmingly common
    /// case (`IFileSystem::ReadFile`).
    fn read(&self, path: &RelPath) -> Option<Result<Vec<u8>>>;

    /// Whether this mount has the path. Must agree with `open`.
    fn contains(&self, path: &RelPath) -> bool;

    /// Appends the immediate children of `dir` to `out`.
    ///
    /// `dir` is `None` for the mount root. Duplicates across mounts are the
    /// caller's problem to merge; see [`Vfs::list`].
    ///
    /// [`Vfs::list`]: super::Vfs::list
    fn list(&self, dir: Option<&RelPath>, out: &mut Vec<Entry>);

    /// A short human-readable identifier, for `PrintSearchPaths`-style output.
    fn describe(&self) -> String;
}
