//! Early-boot error reporting.
//!
//! Replaces the native message boxes `CLauncherLoggingListener` and
//! `GrabSourceMutex`'s failure path used to show (`SDL_ShowSimpleMessageBox`
//! on Linux, `CFUserNotificationDisplayAlert` on macOS) — both gone with SDL2
//! and Cocoa per PORTING.md's windowing decision.
//!
//! This prints to stderr for now. A real native dialog (e.g. via `rfd`) is
//! worth revisiting for the cases this actually covers — failures early
//! enough that no window exists yet, where a user who launched from a desktop
//! icon would otherwise see nothing. Deferred to keep the dependency set at
//! zero while there's no window system at all.

/// Reports a fatal problem to the user before any window exists.
pub fn report_error(title: &str, message: &str) {
    eprintln!("[{title}] {message}");
}
