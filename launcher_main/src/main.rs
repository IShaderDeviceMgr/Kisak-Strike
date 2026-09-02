//! Rust rewrite of the POSIX path in `launcher_main/main.cpp` (see the
//! `#elif defined(POSIX)` branch there for the original).
//!
//! `launcher_main` is just a redirection stub: it `dlopen()`s
//! `launcher_client.{so,dylib}` and calls its exported `LauncherMain`
//! (defined in `launcher/launcher.cpp` as
//! `extern "C" DLL_EXPORT int LauncherMain(int argc, char **argv)`), so
//! the real launcher DLL can be swapped/reloaded without recompiling the
//! executable. That's the whole program.
//!
//! Per PORTING.md, this module needed no `cxx` bridge and no vtable shim:
//! it never implements or consumes an `IAppSystem` interface, it just
//! `dlopen`/`dlsym`s a plain C-ABI function pointer and calls it, exactly
//! like the C++ version. It's as close to a "no seam" port as this codebase
//! offers, which is why it's the first one done.

use std::env;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;

// Mirrors DLL_EXT_STRING (public/tier0/basetypes.h) combined with the
// "bin/<platsubdir>/" prefix main.cpp builds for the PLATFORM_64BITS case.
#[cfg(all(target_os = "macos", target_pointer_width = "64"))]
const LAUNCHER_PATH: &str = "bin/osx64/launcher_client.dylib";
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
const LAUNCHER_PATH: &str = "bin/linux64/launcher_client.so";
// 32-bit builds aren't split by OS in the original either.
#[cfg(not(target_pointer_width = "64"))]
const LAUNCHER_PATH: &str = "bin/launcher_client.so";

// <dlfcn.h>'s RTLD_NOW is 0x2 on both Linux (glibc) and macOS.
const RTLD_NOW: c_int = 2;

type LauncherMainFn = unsafe extern "C" fn(argc: c_int, argv: *mut *mut c_char) -> c_int;

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

/// Reads libdl's last error message. `dlerror()` returns a pointer to a
/// static, NUL-terminated string owned by libdl, or NULL if nothing failed.
fn dlerror_message() -> String {
    unsafe {
        let ptr = dlerror();
        if ptr.is_null() {
            "unknown error".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

fn main() {
    let launcher_path =
        CString::new(LAUNCHER_PATH).expect("LAUNCHER_PATH is a fixed, NUL-free string");

    // SAFETY: dlopen is a plain libdl call; `launcher_path` stays alive for
    // the duration of the call, and the returned handle is only ever
    // passed back into dlsym below.
    let handle = unsafe { dlopen(launcher_path.as_ptr(), RTLD_NOW) };
    if handle.is_null() {
        eprintln!(
            "Failed to load the launcher({}) ({})",
            LAUNCHER_PATH,
            dlerror_message()
        );
        // The original hangs forever here (`while(1);`) rather than
        // exiting. That looks like a leftover debugging aid, not
        // intentional behavior, so this port exits with an error instead.
        std::process::exit(1);
    }

    let symbol_name = CString::new("LauncherMain").unwrap();
    // SAFETY: `handle` came from the successful dlopen above.
    let symbol = unsafe { dlsym(handle, symbol_name.as_ptr()) };
    if symbol.is_null() {
        eprintln!("Failed to load the launcher entry proc");
        std::process::exit(1);
    }

    // SAFETY: `symbol` is non-null and, by the contract of
    // launcher/launcher.cpp's `LauncherMain` export, points to a function
    // matching `LauncherMainFn`. Keep this cast in sync with that
    // signature if it ever changes.
    let launcher_main: LauncherMainFn = unsafe { std::mem::transmute(symbol) };

    // Rust doesn't expose the process's original argv pointer, so rebuild
    // an equivalent C-style argv (NUL-terminated array of NUL-terminated
    // strings, non-UTF8-safe via OsStrExt) to hand to LauncherMain.
    let args: Vec<CString> = env::args_os()
        .map(|arg| CString::new(arg.as_bytes()).expect("argv values must not contain NUL bytes"))
        .collect();
    let mut argv: Vec<*mut c_char> = args.iter().map(|arg| arg.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());

    // SAFETY: `argv` is a valid, NUL-terminated array of NUL-terminated C
    // strings; `args` (which owns the backing bytes) outlives this call.
    let result = unsafe { launcher_main(args.len() as c_int, argv.as_mut_ptr()) };

    std::process::exit(result);
}
