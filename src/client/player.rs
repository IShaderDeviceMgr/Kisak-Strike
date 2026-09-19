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
/// The rest of that table is declared below it, one constant at a time as
/// something came to read it: the hulls and `VEC_DUCK_VIEW` with ducking at
/// stage 4, and [`VEC_DEAD_VIEWHEIGHT`] with death at `server/` stage 5.
/// `VEC_OBS_HULL_MIN`/`MAX` are still only quoted — `(±10,±10,±10)` — because
/// there is no observer mode.
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

/// `VEC_DEAD_VIEWHEIGHT` (`gamerules.cpp:51`) — the eye of a corpse.
///
/// > **14, not 60.** The 60 in `portal_mp_gamerules.cpp:183` is the
/// > *multiplayer* table, annotated "previously 14"; Portal 2 single player is
/// > `CPortalGameRules : CHalfLife2`, which overrides no view vectors, so it
/// > gets `g_DefaultViewVectors` — the same table this file's other five
/// > constants come from. The camera drops fifty units when you die, which is
/// > the whole visible signature of a death in this port.
pub const VEC_DEAD_VIEWHEIGHT: Vec3 = Vec3::new(0.0, 0.0, 14.0);

/// `MOVETYPE_*` (`public/const.h`), reduced to the three a player can be in.
///
/// The rest — `NONE`, `ISOMETRIC`, `STEP`, `FLY`, `VPHYSICS`, `PUSH`,
/// `OBSERVER`, `CUSTOM` — are either an entity's (`PUSH` is every door, and is
/// [`server::movement::MoveType`](crate::server::movement::MoveType)'s) or
/// need a subsystem that does not exist. `OBSERVER` in particular shares
/// `FullNoClipMove` with `NOCLIP` (`gamemovement.cpp:2442`, at `sv_specspeed`).
///
/// > **This enum and the server's are deliberately separate types**, and the
/// > overlap is two names. `client::MoveType` is what the *movement* switches
/// > on; `server::movement::MoveType` is what the *simulation* switches on, and
/// > it has `Push` where this has `FlyGravity`. `Engine::frame` converts, which
/// > is the one place the two vocabularies meet.
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
    /// `MOVETYPE_FLYGRAVITY`: **the dead player**.
    ///
    /// `CBasePlayer::Event_Killed` sets it and only a respawn clears it, so
    /// this is the one move type that is not chosen by the person playing.
    /// `CGameMovement::FullTossMove` — gravity and one swept move, with no
    /// clip-and-retry and no stair stepping. The corpse slides down a slope,
    /// stops when it lands, and is still blown about by a `trigger_push`.
    FlyGravity,
}

/// The local player.
#[derive(Debug, Clone, Copy)]
pub struct Player {
    /// Where the player's **feet** are, in world units.
    pub origin: Vec3,
    /// Carried between commands, which is what makes `sv_noclipaccelerate`
    /// mean anything: without it every frame would start from a standstill.
    pub velocity: Vec3,
    /// `m_vNewVPhysicsVelocity` (`player.h:1304`) — what the last move *asked*
    /// for, which is the only thing the player's physics shadow is allowed to
    /// push a prop with.
    ///
    /// Written by `Client::run_move` from
    /// [`MoveData::out_wish_vel`](crate::client::movement::MoveData::out_wish_vel),
    /// through `PostThinkVPhysics`'s substitution; read by the server, which
    /// hands it to [`crate::vphysics::shadow::PlayerController::drive`]. It is
    /// **not** a velocity the player has and nothing in the movement reads it
    /// back.
    pub wish_velocity: Vec3,
    /// `m_vecBaseVelocity` — the velocity of whatever is carrying the player.
    ///
    /// **The server owns it.** A `trigger_push` writes it every tick it is
    /// pushing and `CPlayerMove::CheckMovingGround` converts it back into
    /// [`velocity`](Player::velocity) once the push stops; it reaches here
    /// through `Engine::frame`'s copy of
    /// [`PlayerState`](crate::server::PlayerState). The movement code adds it
    /// for the duration of a move and takes it back out, which is why a player
    /// carried along a conveyor still reports a velocity of zero.
    pub base_velocity: Vec3,
    /// Where the view points. The angles Valve keeps in `CClientState` and
    /// this port keeps here — `portdocs/CLIENT.md` §4.7.
    pub angles: ViewAngles,
    pub move_type: MoveType,
    /// `m_iHealth` — **the server's**, refreshed once a rendered frame through
    /// [`PlayerState`](crate::server::PlayerState).
    ///
    /// The movement reads it for exactly one thing and it is not a HUD:
    /// `CGameMovement::IsDead()` is `m_iHealth <= 0` (`gamemovement.cpp:1091`),
    /// and a dead player takes no input, cannot turn, and has its eye at
    /// [`VEC_DEAD_VIEWHEIGHT`].
    pub health: i32,
    /// `GetFlags() & FL_FROZEN` — **the server's**, likewise.
    ///
    /// `CRevertSaved::InputReload` is the only thing in this port that sets
    /// it: 11 shipped connections, at the nine `player_loadsaved` entities that
    /// are Portal 2's *other* way of dying.
    pub frozen: bool,
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

    /// `m_hPortalEnvironment` — the portal this player is inside the influence
    /// of, as an opaque key.
    ///
    /// Written at the end of every move by
    /// [`handle_portalling`](super::movement::MoveData::portal_environment)'s
    /// selection and read at the start of the next one, by `engine/`, to
    /// decide which carved wall to trace against. `None` is the ordinary case:
    /// the whole game has 21 scripted portals and 96 of its 106 maps place
    /// none.
    pub portal_environment: Option<u64>,
}

impl Player {
    /// A player standing at `origin` — **feet**, not eye — looking along
    /// `pitch`/`yaw`.
    pub fn new(origin: Vec3, pitch: f32, yaw: f32) -> Player {
        Player {
            origin,
            velocity: Vec3::ZERO,
            wish_velocity: Vec3::ZERO,
            base_velocity: Vec3::ZERO,
            angles: ViewAngles::new(pitch, yaw),
            // `MOVETYPE_WALK`, which is what a player spawns as
            // (`CBasePlayer::Spawn`). Stage 4 made this reachable; before it
            // the player spawned in `MOVETYPE_NOCLIP`, because walking had no
            // ground to stand on.
            move_type: MoveType::Walk,
            // `CBasePlayer::SharedSpawn`'s `m_iHealth = 100`
            // (`baseplayer_shared.cpp:2415`). Written as a literal rather than
            // taken from `server::classes::PLAYER_HEALTH`, because `client/`
            // naming a `server/` constant would be the first code dependency
            // between the two — everything else they share crosses as a
            // `PlayerState`. What matters here is only that a client with no
            // server, which is every unit test in this module, is *alive*:
            // `CGameMovement::IsDead` is what would otherwise stop it moving.
            health: 100,
            frozen: false,
            view_offset: VEC_VIEW,
            ground: None,
            surface_friction: 1.0,
            ducked: false,
            ducking: false,
            duck_time_msecs: 0,
            old_buttons: ButtonBits::NONE,
            portal_environment: None,
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
