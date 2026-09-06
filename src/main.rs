//! Rust rewrite of the Source engine, targeting Portal 2.
//!
//! One binary, one crate, no dynamic module loading — see `PORTING.md` for
//! the architecture and `portdocs/` for per-subsystem design notes. The
//! original C++ tree lives in `legacy/` and is the reference implementation
//! this replaces, not a dependency.
//!
//! Subsystems are modules under `src/`. `launcher` (process bootstrap),
//! `filesystem` (search paths, VPKs), `materials` (the GPU device) and most of
//! `engine` exist so far, and `client` is the first of the *game* modules —
//! Valve's `client.so`, a sibling of `engine.so` and so a sibling of `engine`
//! here. The rest arrives as it is ported.
//!
//! `cmdline` is the exception to "one module per Valve module": Valve kept
//! `CommandLine()` in `tier0` because *everything* reads it, and it sits at the
//! crate root here for the same reason. It moved out of `launcher/` when
//! `engine::console` became its third consumer — `stuffcmds` and the `+<cvar>`
//! default seeding both read it (`portdocs/ENGINE_CONSOLE.md` §6.5).

mod client;
mod cmdline;
mod engine;
mod filesystem;
mod launcher;
mod materials;
mod studio;

fn main() -> std::process::ExitCode {
    let code = launcher::run();
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}
