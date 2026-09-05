//! The local player: where it is, how fast, and which movement mode it is in.
//!
//! `C_BasePlayer`'s movement state, reduced to what exists before there are
//! entities. Everything else that class holds — the model, the animation state,
//! the weapon, the flags, the water level — belongs to systems this port has
//! not reached.
//!
//! # `origin` is the feet, not the eye
//!
//! Valve's player entity sits on the floor and the view sits
//! [`VEC_VIEW`](VEC_VIEW) above it (`gamerules.cpp:38`), which is what
//! `C_BasePlayer::CalcView` adds before handing the eye to the renderer.
//! `CGameMovement` moves the *origin*. Conflating the two is a 64-unit error
//! that looks like a level built slightly wrong rather than like a bug.

use glam::Vec3;

use super::ViewAngles;

/// `VEC_VIEW` (`game/shared/shareddefs.h:76`, via
/// `g_DefaultViewVectors`, `game/shared/gamerules.cpp:38`): the eye, standing.
///
/// The rest of that table, for when ducking and hulls arrive at stage 4:
/// `VEC_DUCK_VIEW` `(0,0,28)`, `VEC_HULL_MIN` `(-16,-16,0)`, `VEC_HULL_MAX`
/// `(16,16,72)`, `VEC_DUCK_HULL_MAX` `(16,16,36)`, `VEC_DEAD_VIEWHEIGHT`
/// `(0,0,14)`. They are quoted rather than declared because a constant nothing
/// reads is a constant nothing checks.
pub const VEC_VIEW: Vec3 = Vec3::new(0.0, 0.0, 64.0);

/// `MOVETYPE_*` (`public/const.h`), reduced to the two that mean anything yet.
///
/// The other seven — `NONE`, `ISOMETRIC`, `WALK`, `STEP`, `FLY`, `FLYGRAVITY`,
/// `VPHYSICS`, `PUSH`, `OBSERVER`, `CUSTOM` — arrive with the entity system,
/// and `OBSERVER` in particular shares `FullNoClipMove` with this one
/// (`gamemovement.cpp:2442`, at `sv_specspeed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveType {
    /// `MOVETYPE_WALK`. **Not implemented**: `FullWalkMove`
    /// (`gamemovement.cpp:2287`) needs collision, and `trace/` does not exist.
    /// A player in this mode does not move — see
    /// [`Client::run_move`](super::Client::run_move).
    Walk,
    /// `MOVETYPE_NOCLIP`. Fly through everything; no gravity, no collision, no
    /// ground. The one movement mode that is complete without `trace/`, which
    /// is why it is where the port starts (`portdocs/CLIENT.md` §4.5).
    Noclip,
}

/// The local player.
#[derive(Debug, Clone, Copy)]
pub struct Player {
    /// Where the player's **feet** are, in world units.
    pub origin: Vec3,
    /// Carried between commands, which is what makes `sv_noclipaccelerate`
    /// mean anything: without it every frame would start from a standstill.
    pub velocity: Vec3,
    /// Where the view points. The angles Valve keeps in `CClientState` and
    /// this port keeps here — `portdocs/CLIENT.md` §4.7.
    pub angles: ViewAngles,
    pub move_type: MoveType,
    /// The eye's offset from [`origin`](Player::origin): [`VEC_VIEW`]
    /// standing, and `VEC_DUCK_VIEW` `(0,0,28)` once ducking exists.
    pub view_offset: Vec3,
}

impl Player {
    /// A player standing at `origin` — **feet**, not eye — looking along
    /// `pitch`/`yaw`.
    pub fn new(origin: Vec3, pitch: f32, yaw: f32) -> Player {
        Player {
            origin,
            velocity: Vec3::ZERO,
            angles: ViewAngles::new(pitch, yaw),
            // Noclip, because walking is stage 4. This is the whole of what
            // makes the engine flyable today, and it is a real movetype rather
            // than a camera pretending to be one.
            move_type: MoveType::Noclip,
            view_offset: VEC_VIEW,
        }
    }

    /// `EyePosition()` — origin plus the view offset.
    ///
    /// **Not `CalcView`.** That adds view bob, view roll, punch angle and aim
    /// punch on top, and for a Portal player it interpolates the eye *through*
    /// a portal for several frames after a teleport
    /// (`c_portal_player.cpp:2772`). None of those exist yet; this is the seam
    /// they attach to, which is why the renderer asks for the eye rather than
    /// computing `origin + 64` itself.
    pub fn eye(&self) -> Vec3 {
        self.origin + self.view_offset
    }
}
