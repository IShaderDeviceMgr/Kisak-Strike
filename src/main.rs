//! Rust rewrite of the Source engine, targeting Portal 2.
//!
//! One binary, one crate, no dynamic module loading — see `PORTING.md` for
//! the architecture and `portdocs/` for per-subsystem design notes. The
//! original C++ tree lives in `legacy/` and is the reference implementation
//! this replaces, not a dependency.
//!
//! Subsystems are modules under `src/`. `launcher` (process bootstrap),
//! `filesystem` (search paths, VPKs), `materials` (the GPU device) and most of
//! `engine` exist so far, and `client` and `server` are the *game* modules —
//! Valve's `client.so` and `server.so`, siblings of `engine.so` and so
//! siblings of `engine` here. The rest arrives as it is ported.
//!
//! `cmdline` and `math` are the exceptions to "one module per Valve module".
//! Valve kept `CommandLine()` in `tier0` because *everything* reads it, and it
//! sits at the crate root here for the same reason — it moved out of
//! `launcher/` when `engine::console` became its third consumer (`stuffcmds`
//! and the `+<cvar>` default seeding both read it,
//! `portdocs/ENGINE_CONSOLE.md` §6.5). `math` holds the parts of `mathlib`
//! that are a *convention* rather than arithmetic, which `glam` therefore
//! cannot supply; its consumers are in `engine/` and `client/`, which are
//! siblings, so it can live nowhere below the root either.

mod client;
mod cmdline;
mod engine;
mod filesystem;
mod launcher;
mod materials;
mod math;
mod server;
mod studio;

fn main() -> std::process::ExitCode {
    let code = launcher::run();
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}
