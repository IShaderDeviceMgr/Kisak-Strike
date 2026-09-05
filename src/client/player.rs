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

use super::{ButtonBits, ViewAngles};

/// `VEC_VIEW` (`game/shared/shareddefs.h:76`, via
/// `g_DefaultViewVectors`, `game/shared/gamerules.cpp:38`): the eye, standing.
///
/// The rest of that table, for when ducking and hulls arrive at stage 4:
/// `VEC_DUCK_VIEW` `(0,0,28)`, `VEC_HULL_MIN` `(-16,-16,0)`, `VEC_HULL_MAX`
/// `(16,16,72)`, `VEC_DUCK_HULL_MAX` `(16,16,36)`, `VEC_DEAD_VIEWHEIGHT`
/// `(0,0,14)`. They are quoted rather than declared because a constant nothing
/// reads is a constant nothing checks.
pub const VEC_VIEW: Vec3 = Vec3::new(0.0, 0.0, 64.0);

/// `VEC_HULL_MIN`/`VEC_HULL_MAX` — the standing player's collision box,
/// relative to [`Player::origin`] (`game/shared/portal/portal_mp_gamerules.cpp:173`).
///
/// 32 wide, 32 deep, 72 tall, with the origin on the floor between the feet.
/// The duck hull is the same box 36 tall and arrives with stage 4, along with
/// everything that sweeps these.
pub const VEC_HULL_MIN: Vec3 = Vec3::new(-16.0, -16.0, 0.0);
/// See [`VEC_HULL_MIN`].
pub const VEC_HULL_MAX: Vec3 = Vec3::new(16.0, 16.0, 72.0);

/// `VEC_DUCK_HULL_MIN`/`VEC_DUCK_HULL_MAX` — the crouched hull
/// (`game/shared/portal/portal_mp_gamerules.cpp:176`). Half the height and the
/// **same minimum**: the origin stays on the floor, so crouching lowers the top
/// of the box rather than moving the player.
pub const VEC_DUCK_HULL_MIN: Vec3 = Vec3::new(-16.0, -16.0, 0.0);
/// See [`VEC_DUCK_HULL_MIN`].
pub const VEC_DUCK_HULL_MAX: Vec3 = Vec3::new(16.0, 16.0, 36.0);

/// `VEC_DUCK_VIEW` (`portal_mp_gamerules.cpp:178`) — the eye while crouched,
/// which is 28 rather than half of 64.
pub const VEC_DUCK_VIEW: Vec3 = Vec3::new(0.0, 0.0, 28.0);

/// `MOVETYPE_*` (`public/const.h`), reduced to the two that mean anything yet.
///
/// The other seven — `NONE`, `ISOMETRIC`, `WALK`, `STEP`, `FLY`, `FLYGRAVITY`,
/// `VPHYSICS`, `PUSH`, `OBSERVER`, `CUSTOM` — arrive with the entity system,
/// and `OBSERVER` in particular shares `FullNoClipMove` with this one
/// (`gamemovement.cpp:2442`, at `sv_specspeed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveType {
    /// `MOVETYPE_WALK`: gravity, friction, collision, stairs, jumping and
    /// ducking — `CPortalGameMovement::FullWalkMove`
    /// (`portal_gamemovement.cpp:3877`). Needs a map: with none loaded a
    /// walking player has nothing to stand on and does not move.
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
    /// standing, [`VEC_DUCK_VIEW`] crouched, and interpolated between the two
    /// through a duck transition.
    pub view_offset: Vec3,

    /// `player->GetGroundEntity()`, reduced to the normal of what is underfoot
    /// — `None` when airborne. See
    /// [`MoveData::ground`](super::MoveData::ground).
    pub ground: Option<Vec3>,
    /// `player->m_surfaceFriction`.
    pub surface_friction: f32,
    /// `m_Local.m_bDucked` — the hull *is* the crouched one.
    pub ducked: bool,
    /// `m_Local.m_bDucking` — mid-transition, in either direction.
    pub ducking: bool,
    /// `m_Local.m_nDuckTimeMsecs`.
    pub duck_time_msecs: i32,
    /// `mv->m_nOldButtons`: what the *previous* command held.
    ///
    /// On the player rather than in the command because jump reads it to
    /// refuse a pogo stick and duck reads it for press and release edges —
    /// both of which are questions about the frame before this one.
    pub old_buttons: ButtonBits,
}

impl Player {
    /// A player standing at `origin` — **feet**, not eye — looking along
    /// `pitch`/`yaw`.
    pub fn new(origin: Vec3, pitch: f32, yaw: f32) -> Player {
        Player {
            origin,
            velocity: Vec3::ZERO,
            angles: ViewAngles::new(pitch, yaw),
            // `MOVETYPE_WALK`, which is what a player spawns as
            // (`CBasePlayer::Spawn`). Stage 4 made this reachable; before it
            // the player spawned in `MOVETYPE_NOCLIP`, because walking had no
            // ground to stand on.
            move_type: MoveType::Walk,
            view_offset: VEC_VIEW,
            ground: None,
            surface_friction: 1.0,
            ducked: false,
            ducking: false,
            duck_time_msecs: 0,
            old_buttons: ButtonBits::NONE,
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
