//! Rust rewrite of the Source engine, targeting Portal 2.
//!
//! One binary, one crate, no dynamic module loading — see `PORTING.md` for
//! the architecture and `portdocs/` for per-subsystem design notes. The
//! original C++ tree lives in `legacy/` and is the reference implementation
//! this replaces, not a dependency.
//!
//! Subsystems are modules under `src/`. `launcher` (process bootstrap),
//! `filesystem` (search paths, VPKs), `materials` (the GPU device) and
//! `engine::window` (the game window) exist so far; the rest of `engine` and
//! the game layer arrive as they're ported.

mod engine;
mod filesystem;
mod launcher;
mod materials;

fn main() -> std::process::ExitCode {
    let code = launcher::run();
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}
