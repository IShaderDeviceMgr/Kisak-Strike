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

    // TODO: hand off to the engine. Nothing to hand off to yet — the engine
    // is still C++ in `legacy/` and hasn't been ported. See portdocs/ENGINE.md
    // for the subsystem breakdown and PORTING.md for sequencing.
    eprintln!(
        "source-engine: startup complete (mod: {}), but the engine is not ported yet.",
        cmdline.value_or("-game", DEFAULT_MOD)
    );
    eprintln!("See PORTING.md for the current state of the rewrite.");

    0
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
