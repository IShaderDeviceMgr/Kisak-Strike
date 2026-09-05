//! The command: everything one tick of input asks the player to do.
//!
//! `CUserCmd` (`game/shared/usercmd.h:63`), which is this module's entire
//! output. Everything else in `src/client/` exists to fill one in or to consume
//! one.
//!
//! # Why it exists before there is anything to send it to
//!
//! With no server and no prediction a command is created and run once,
//! immediately, and a struct is not obviously better than passing four floats
//! around. It is, for two reasons. The command is what makes
//! [`KButton::key_state`](super::KButton::key_state)'s fraction meaningful —
//! `forwardmove` is a *velocity*, not an axis, so "held for a quarter of the
//! frame" has somewhere to go. And prediction, when it arrives, wraps the
//! function that consumes one rather than rewriting it
//! (`portdocs/CLIENT.md` §4.8).

use super::{ButtonBits, ViewAngles};

/// One tick's worth of intent.
///
/// Valve's virtual destructor, hand-written `operator=`, `GetChecksum` and
/// split-screen array are artefacts of the C++ and are not ported. The Portal 2
/// fields — `player_held_entity`, `held_entity_was_grabbed_through_portal`,
/// `command_acknowledgements_pending`, `predictedPortalTeleportations`
/// (`usercmd.h:271-281`) — are real and are not stage 1: they exist because
/// Portal 2's grab code lives on the client so co-op can predict it, and
/// because a portal teleport changes the view angles underneath a command that
/// is already in flight.
///
/// **The wire encoding is not pinned yet.** `ReadUsercmd`/`WriteUsercmd`
/// (`game/shared/usercmd.cpp`) are a bit-packed delta against a previous
/// command; per `PORTING.md` the format becomes ours once both ends are Rust,
/// and both ends will be. Design for clarity here; `net/` owns the encoder.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UserCmd {
    /// `command_number` — for matching a command to the server's
    /// acknowledgement of it, and to a prediction of it.
    pub command_number: i32,
    /// The tick the client created this command on.
    ///
    /// **Not a simulation rate yet.** `host/` paces frames, not ticks, and
    /// nothing steps at a fixed interval; this counts commands. Valve's PC tick
    /// is `1.0 / 64.0` (`public/const.h:29`) and becomes real with `server/`.
    pub tick_count: i32,
    /// Where the view points for this command.
    pub viewangles: ViewAngles,
    /// **Intended velocities in units per second**, not axes in `[-1, 1]`:
    /// `cl_forwardspeed` and friends are baked in here, on the client, and the
    /// server clips them against the player's real maximum speed
    /// (`CheckParameters`, `gamemovement.cpp:1137`).
    pub forwardmove: f32,
    pub sidemove: f32,
    pub upmove: f32,
    pub buttons: ButtonBits,
    /// `impulse 101` and the rest. Latched and cleared per command.
    pub impulse: u8,
    /// The **scaled** mouse delta, truncated to an integer — Valve's
    /// `cmd->mousedx = (int)mouse_x` at `in_mouse.cpp:604`, after `ScaleMouse`,
    /// not the raw device units. Nothing local reads it; it is the server's,
    /// for lag compensation.
    pub mousedx: i16,
    pub mousedy: i16,
    /// The shared-random seed, `MD5_PseudoRandom( command_number ) &
    /// 0x7fffffff` (`in_main.cpp:1489`).
    ///
    /// **Zero until there are two ends to agree.** Its only purpose is making
    /// client and server draw the same "random" numbers for the same command,
    /// so a value that is not Valve's MD5 would be worse than no value — it
    /// would look like it worked. The seed function arrives with `server/`.
    pub random_seed: i32,
}

impl UserCmd {
    /// A command with nothing in it but its identity. `CUserCmd::Reset`
    /// followed by the two assignments at `in_main.cpp:1358`.
    pub fn new(command_number: i32, tick_count: i32) -> UserCmd {
        UserCmd {
            command_number,
            tick_count,
            ..UserCmd::default()
        }
    }
}
