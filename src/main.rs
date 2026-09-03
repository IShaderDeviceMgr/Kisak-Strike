//! Rust rewrite of the Source engine, targeting Portal 2.
//!
//! One binary, one crate, no dynamic module loading — see `PORTING.md` for
//! the architecture and `portdocs/` for per-subsystem design notes. The
//! original C++ tree lives in `legacy/` and is the reference implementation
//! this replaces, not a dependency.
//!
//! Subsystems are modules under `src/`. Only `launcher` (process bootstrap)
//! exists so far; `engine`, `filesystem`, `materials`, and the rest arrive as
//! they're ported.

mod launcher;

fn main() -> std::process::ExitCode {
    let code = launcher::run();
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}
