//! Process bootstrap: command-line handling, single-instance locking, and
//! the startup sequence that eventually hands control to the engine.
//!
//! Descended from `launcher/launcher.cpp`'s `LauncherMain` plus the
//! `launcher_main` stub, both of which existed to `dlopen` their way up a
//! chain of shared libraries. In a single statically-linked binary there's
//! nothing to load, so what's left is ordinary startup code.

pub mod cmdline;
pub mod dialog;
pub mod single_instance;

use crate::filesystem;
use cmdline::CommandLine;
use single_instance::{LockError, SingleInstanceLock};

/// The mod that boots when `-game` isn't given.
///
/// TODO(portal2): this tree is the `cstrike15` branch, so the inherited
/// default is CS:GO, but the target game is Portal 2 — see PORTING.md's "Game
/// scope". Retargeting this is part of making Portal 2 boot.
const DEFAULT_MOD: &str = "csgo";

/// Runs the startup sequence. Returns the process exit code.
pub fn run() -> i32 {
    let mut cmdline = CommandLine::from_env();

    // Locale fix: some locales use "," as the decimal separator, which breaks
    // printf/sscanf-style float parsing throughout the engine. Linux-only in
    // the original; macOS already starts in en_US.UTF-8.
    #[cfg(target_os = "linux")]
    force_c_numeric_locale();

    if !cmdline.has("-game") {
        cmdline.append("-game", Some(DEFAULT_MOD));
    }
    cmdline.dedup_game_parm();

    // Not launched through Steam, so don't advertise as a secure client.
    if !cmdline.has("-steam") {
        cmdline.append("-insecure", None);
    }

    if cmdline.has("-buildcubemaps") {
        cmdline.append("-nosound", None);
        cmdline.append("-noasync", None);
    }

    // `-allowmultiple`/`-multirun` skip the lock entirely. The guard is held
    // for the rest of `run()` and released on the way out.
    let _instance_lock = if cmdline.has("-allowmultiple") || cmdline.has("-multirun") {
        None
    } else {
        let mod_name = cmdline.value_or("-game", DEFAULT_MOD);
        match SingleInstanceLock::acquire(mod_name) {
            Ok(lock) => Some(lock),
            Err(err @ LockError::AlreadyRunning) => {
                dialog::report_error("Source - Warning", &err.to_string());
                return 1;
            }
            Err(err @ LockError::Io(_)) => {
                dialog::report_error("Source - Warning", &err.to_string());
                return 1;
            }
        }
    };

    // The engine expects to run with the game directory as the working
    // directory. The original derived this from `-basedir` only (its
    // executable-path lookup was a no-op on POSIX), so an absent `-basedir`
    // means "already in the right place".
    if let Some(base_dir) = cmdline.value("-basedir") {
        if let Err(err) = std::env::set_current_dir(base_dir) {
            dialog::report_error(
                "Source - Warning",
                &format!("could not enter base directory {base_dir}: {err}"),
            );
            return 1;
        }
    }

    // Mount the game's content. This is `FileSystem_LoadSearchPaths` plus VPK
    // discovery — see portdocs/FILESYSTEM.md. Failure isn't fatal yet: without
    // an engine to hand off to there's nothing the content would be used for,
    // and reporting the problem is more useful than exiting.
    let mod_name = cmdline.value_or("-game", DEFAULT_MOD).to_string();
    match mount_filesystem(&cmdline, &mod_name) {
        Ok(vfs) => {
            for warning in vfs.warnings() {
                eprintln!("source-engine: filesystem: {warning}");
            }
            // `CBaseFileSystem::PrintSearchPaths`. Comparing this against a
            // stock build's output is the cheapest way to verify the port.
            eprintln!("source-engine: search paths:");
            for (path_id, description) in vfs.search_paths() {
                eprintln!("  {path_id:<15?} {description}");
            }
        }
        Err(err) => {
            eprintln!("source-engine: filesystem: {err}");
        }
    }

    // TODO: hand off to the engine. Nothing to hand off to yet — the engine
    // is still C++ in `legacy/` and hasn't been ported. See portdocs/ENGINE.md
    // for the subsystem breakdown and PORTING.md for sequencing.
    eprintln!(
        "source-engine: startup complete (mod: {mod_name}), but the engine is not ported yet."
    );
    eprintln!("See PORTING.md for the current state of the rewrite.");

    0
}

/// Builds the [`Vfs`] from the command line.
///
/// [`Vfs`]: crate::filesystem::Vfs
fn mount_filesystem(cmdline: &CommandLine, mod_name: &str) -> filesystem::Result<filesystem::Vfs> {
    // `-basedir` has already been applied with `set_current_dir` above, so the
    // working directory is the base directory either way.
    let base_dir = std::env::current_dir().map_err(|e| filesystem::VfsError::io(".", e))?;

    let options = filesystem::SearchPathOptions {
        // `initInfo.m_pLanguage` is set by the engine, not by
        // filesystem_init.cpp. There's no engine yet to ask, so localized
        // search paths stay off until one exists — see portdocs/FILESYSTEM.md's
        // open question about where the language actually comes from.
        language: None,
        // `IsLowViolenceBuild()` is `return false` on POSIX except via `-lv`.
        low_violence: cmdline.has("-lv"),
        temp_content: cmdline.has("-tempcontent"),
        executable_dir: std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|p| p.to_path_buf())),
    };

    let game_dir = filesystem::locate_game_dir(&base_dir, mod_name);
    filesystem::Vfs::mount_game(&game_dir, &base_dir, &options)
}

#[cfg(target_os = "linux")]
fn force_c_numeric_locale() {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int};

    extern "C" {
        fn setenv(name: *const c_char, value: *const c_char, overwrite: c_int) -> c_int;
        fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
    }
    const LC_ALL: c_int = 6;

    let key = CString::new("LC_ALL").expect("literal");
    let en_us = CString::new("en_US.UTF-8").expect("literal");
    // SAFETY: both pointers are valid NUL-terminated strings alive for the
    // duration of the calls.
    unsafe {
        setenv(key.as_ptr(), en_us.as_ptr(), 1);
        setlocale(LC_ALL, en_us.as_ptr());
    }
}
