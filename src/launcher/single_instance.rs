//! Single-instance locking: only one copy of the game runs per mod at a time.
//!
//! Descended from `GrabSourceMutex`/`ReleaseSourceMutex`
//! (`legacy/launcher/launcher.cpp:1082-1211` in the original tree, POSIX
//! branch). The locking *strategy* is preserved — a lock file keyed on a
//! CRC-32 of the mod name, `fcntl(F_SETLK)` on Linux and `O_EXLOCK` on macOS
//! — but the paired grab/release free functions and their file-scope globals
//! are replaced by an RAII guard, per PORTING.md's rule that lifecycle pairs
//! become construction + `Drop`.

use std::ffi::CString;
use std::fmt;
use std::io;
use std::os::raw::{c_char, c_int};

#[allow(non_camel_case_types)]
type mode_t = u32;

extern "C" {
    fn open(path: *const c_char, flags: c_int, mode: mode_t) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn unlink(path: *const c_char) -> c_int;
    fn fchmod(fd: c_int, mode: mode_t) -> c_int;
    #[cfg(target_os = "linux")]
    fn fcntl(fd: c_int, cmd: c_int, lock: *mut Flock) -> c_int;
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct Flock {
    l_type: i16,
    l_whence: i16,
    l_start: i64,
    l_len: i64,
    l_pid: i32,
}

#[cfg(target_os = "linux")]
mod flags {
    use std::os::raw::c_int;
    pub const O_CREAT: c_int = 0o100;
    pub const O_WRONLY: c_int = 0o1;
    pub const F_SETLK: c_int = 6;
    pub const F_WRLCK: i16 = 1;
    pub const SEEK_SET: i16 = 0;
}

#[cfg(target_os = "macos")]
mod flags {
    use std::os::raw::c_int;
    pub const O_CREAT: c_int = 0x0200;
    pub const O_WRONLY: c_int = 0x0001;
    pub const O_EXLOCK: c_int = 0x0020;
    pub const O_NONBLOCK: c_int = 0x0004;
    pub const O_TRUNC: c_int = 0x0400;
    pub const EWOULDBLOCK: i32 = 35; // == EAGAIN on Darwin
}

/// Why acquiring the single-instance lock failed.
#[derive(Debug)]
pub enum LockError {
    /// Another instance holds the lock for this mod.
    AlreadyRunning,
    /// The lock file couldn't be opened at all.
    Io(io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => {
                write!(f, "only one instance of the game can be running at one time")
            }
            Self::Io(e) => write!(f, "could not open the single-instance lock file: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// Holds the single-instance lock for as long as it's alive. Releases the
/// lock and removes the lock file on drop.
#[derive(Debug)]
pub struct SingleInstanceLock {
    fd: c_int,
    path: CString,
}

impl SingleInstanceLock {
    /// Attempts to take the lock for `mod_name` (the `-game` value).
    pub fn acquire(mod_name: &str) -> Result<Self, LockError> {
        let path = lock_file_path(mod_name);
        let path_c = CString::new(path).expect("lock path is built from safe components");
        Self::acquire_at(path_c)
    }

    #[cfg(target_os = "linux")]
    fn acquire_at(path: CString) -> Result<Self, LockError> {
        use flags::*;

        // SAFETY: `path` is a valid NUL-terminated string alive for the call.
        let fd = unsafe { open(path.as_ptr(), O_WRONLY | O_CREAT, 0o666) };
        if fd == -1 {
            return Err(LockError::Io(io::Error::last_os_error()));
        }

        // Force 0666 regardless of umask, so a crashed process owned by
        // another user doesn't lock everyone else out of the game.
        unsafe { fchmod(fd, 0o666) };

        let mut lock = Flock {
            l_type: F_WRLCK,
            l_whence: SEEK_SET,
            l_start: 0,
            l_len: 1,
            l_pid: 0,
        };
        // SAFETY: `fd` is open; `lock` is a valid, fully-initialized struct.
        if unsafe { fcntl(fd, F_SETLK, &mut lock) } == -1 {
            unsafe { close(fd) };
            return Err(LockError::AlreadyRunning);
        }

        Ok(Self { fd, path })
    }

    #[cfg(target_os = "macos")]
    fn acquire_at(path: CString) -> Result<Self, LockError> {
        use flags::*;

        // SAFETY: `path` is a valid NUL-terminated string alive for the call.
        let fd = unsafe {
            open(
                path.as_ptr(),
                O_CREAT | O_WRONLY | O_EXLOCK | O_NONBLOCK | O_TRUNC,
                0o777,
            )
        };
        if fd >= 0 {
            unsafe { fchmod(fd, 0o777) };
            return Ok(Self { fd, path });
        }

        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(EWOULDBLOCK) {
            return Err(LockError::AlreadyRunning);
        }

        // The original deliberately lets the game start when the failure isn't
        // a lock conflict, rather than blocking launch on an error it doesn't
        // understand. Preserved, but surfaced instead of silent.
        eprintln!(
            "warning: could not lock {}: {err}; starting anyway",
            path.to_string_lossy()
        );
        Ok(Self { fd: -1, path })
    }
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        if self.fd == -1 {
            return;
        }
        // SAFETY: `fd` was opened by `acquire_at` and is closed exactly once,
        // since `Drop` runs once and nothing else touches the field.
        unsafe {
            close(self.fd);
            unlink(self.path.as_ptr());
        }
    }
}

/// CRC-32 (IEEE 802.3, polynomial 0xEDB88320) — the same checksum Source's
/// `CRC32_ProcessBuffer` computes, so the lock file name matches what the
/// original picks for a given mod.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn lock_file_path(mod_name: &str) -> String {
    let crc = crc32(mod_name.as_bytes());

    // Linux honors $TMPDIR when it points at a real directory; macOS always
    // used /tmp in the original.
    let dir = if cfg!(target_os = "linux") {
        std::env::var("TMPDIR")
            .ok()
            .filter(|d| std::path::Path::new(d).is_dir())
            .unwrap_or_else(|| "/tmp".to_string())
    } else {
        "/tmp".to_string()
    };

    format!("{dir}/source_engine_{crc}.lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_vectors() {
        // Standard CRC-32 check values.
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn lock_path_is_stable_and_mod_specific() {
        assert_eq!(lock_file_path("portal2"), lock_file_path("portal2"));
        assert_ne!(lock_file_path("portal2"), lock_file_path("csgo"));
        assert!(lock_file_path("portal2").ends_with(".lock"));
    }

    #[test]
    fn second_acquire_of_the_same_mod_reports_already_running() {
        let name = format!("ks-test-{}", std::process::id());
        let first = SingleInstanceLock::acquire(&name).expect("first acquire should succeed");
        match SingleInstanceLock::acquire(&name) {
            Err(LockError::AlreadyRunning) => {}
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
        drop(first);
        // Once released, the lock is available again.
        SingleInstanceLock::acquire(&name).expect("acquire after release should succeed");
    }
}
