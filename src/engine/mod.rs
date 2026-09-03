//! The engine.
//!
//! `portdocs/ENGINE.md` breaks the original `engine/` module into 23
//! subsystems and concludes it must not be ported as one unit: each subsystem
//! becomes its own module here (`host/`, `net/`, `world/`, `audio/`,
//! `console/`, …), 13 of them surviving, with ~45,700 lines deleted outright.
//!
//! Only [`window`] exists so far. It is deliberately first: `winit` inverts
//! the engine's control flow, and the shape of the frame loop has to be
//! settled before anything is written against it.

pub mod window;
