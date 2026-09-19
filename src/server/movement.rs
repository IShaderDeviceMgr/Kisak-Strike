//! Movetypes, the pusher, and the two moves every brush entity is built from.
//!
//! `MOVETYPE_*` (`public/const.h:172`), `CBaseEntity::PhysicsSimulate`
//! (`game/shared/physics_main_shared.cpp:1772`), `CBaseEntity::PhysicsPusher`
//! and `PerformPush` (`game/server/physics_main.cpp:1700` and `:1590`), and
//! `CBaseToggle` (`game/server/subs.cpp`, `basetoggle.h`).
//!
//! **`public/const.h`'s other enumerations live here too** — [`Solid`], the
//! `FSOLID_*` bits, [`FL_CLIENT`] and [`EF_NODRAW`]. They are one header in
//! the original and they are what say how an entity exists in the world, which
//! is the same question a movetype answers. Everything that acts on them is
//! either here or in [`touch`](super::touch).
//!
//! # A mover is four lines and an alarm
//!
//! `CBaseToggle::LinearMove` sets a velocity and an arrival time; the pusher
//! integrates the velocity once a tick and, when the arrival time comes round,
//! snaps the entity onto its destination and calls the class back. That is the
//! whole of how every door, platform and panel in Source moves, and
//! `portdocs/SERVER.md` §4.7 is right that it is astonishingly simple.
//!
//! What is *not* simple is pushing whatever is standing in the way, and that
//! is deliberately absent: `CPhysicsPushedEntities`
//! (`physics_main.cpp:130-1130`) is ~1,000 lines of speculative push, blocker
//! enumeration and rollback, it needs `ENGINE_TRACE.md` stage 4 underneath it,
//! and a door that moves through the player is a better state than a door that
//! does not move. So [`perform_push`] is `PerformPush` with the blocker always
//! null — which is the branch the shipped game takes on almost every tick
//! anyway.
//!
//! # The alarm is not the think schedule
//!
//! [`EntityCore::set_move_done_time`](super::entity::EntityCore::set_move_done_time)
//! is a second timer with its own field, and a mover uses both at once — a
//! `func_button` is travelling on the alarm while its `ButtonReturn` sits on
//! the think schedule. Conflating them is the mistake that breaks doors that
//! think while moving (`portdocs/SERVER.md` §4.7), and it is why the named
//! think *contexts* this port skipped are still not needed.
//!
//! The alarm runs on **local time**, not on `curtime`:
//! [`EntityCore::local_time`](super::entity::EntityCore::local_time) is a clock
//! that only advances while the entity is being pushed, which is what lets a
//! blocked pusher fall behind the world and catch up later. Nothing blocks
//! here, so in this port local time is simply "seconds this entity has spent
//! simulating" — but the arithmetic is Valve's and the field is where a future
//! blocker rollback puts its answer.

use glam::Vec3;

use super::class::{Behaviour, Context};
use super::entity::EntityCore;
use super::keyvalue::atof;

/// `MOVETYPE_*` (`public/const.h:172`), reduced to the four this port reaches.
///
/// Valve declares twelve. `MOVETYPE_VPHYSICS` waits for `rapier`, and
/// `STEP`/`FLY`/`FLYGRAVITY` are the NPC movetypes, which serve 293 entities
/// in the whole game (`portdocs/SERVER.md` §1.5).
///
/// # Two of these are the player's, and the server does not run them
///
/// [`Walk`](MoveType::Walk) and [`Noclip`](MoveType::Noclip) arrived at stage
/// 4 with the player entity, and [`simulate`] treats them exactly as it treats
/// [`None`](MoveType::None): the player is moved by
/// [`crate::client::Client::run_move`] on the *rendered* frame, and what the
/// server holds is a copy refreshed once a tick
/// (`portdocs/SERVER.md` §5 is why the two clocks differ). They are here
/// because `trigger_push` branches on the movetype of what it is pushing and
/// gets three different answers for the three the player can be in — a
/// `MOVETYPE_NOCLIP` player is *not* pushed, which is real behaviour and would
/// be lost if the player reported `MOVETYPE_NONE`.
///
/// [`crate::client::MoveType`] is still the client's own copy. The two merge
/// when `portdocs/SERVER.md` stage 5 moves the movement itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MoveType {
    /// `MOVETYPE_NONE`. Never moves, and is what the overwhelming majority of
    /// the entity list is — 60,925 blocks against 1,164 movers.
    #[default]
    None,
    /// `MOVETYPE_PUSH`. Every mover: doors, platforms, panels, buttons, fans.
    /// Integrates its own velocity, does not clip to the world, and pushes
    /// what is in the way — except here, where nothing is pushed yet.
    Push,
    /// `MOVETYPE_WALK`. The player, walking. Simulated by `client/`.
    Walk,
    /// `MOVETYPE_NOCLIP`. The player, flying. Simulated by `client/`.
    Noclip,
    /// `MOVETYPE_FLYGRAVITY`. The player, **dead** — `CBasePlayer::Event_Killed`
    /// sets it and nothing sets it back except a respawn. Gravity and a single
    /// swept move with no clip-and-retry; `CGameMovement::FullTossMove`, and
    /// also simulated by `client/`.
    FlyGravity,
}

/// `kRenderNone` (`public/const.h:348`) — the one `rendermode` that is a flat
/// refusal to draw rather than a blend.
///
/// `C_BaseEntity::ShouldDraw` tests it alongside [`EF_NODRAW`], so the two
/// model seams and the brush seam all have to refuse it together —
/// `engine::world::RENDER_NONE` is the same number on the brush side, where it
/// hides 94 of the game's brush entities. **No `prop_dynamic` in the shipped
/// game writes it**; it is here so the paths agree rather than for content.
pub const RENDER_NONE: u8 = 10;

/// `EF_NODRAW` (`public/const.h:268`) — "don't draw entity".
///
/// The only `EF_*` bit this module sets, and it is what
/// `CFuncBrush::TurnOff` does: a switched-off `func_brush` is invisible and
/// non-solid, and this is the visible half.
pub const EF_NODRAW: u32 = 0x020;

/// `FSOLID_NOT_SOLID` (`public/const.h:230`) — "this entity is not solid".
///
/// The other half of `CFuncBrush::TurnOff`, and the first `FSOLID_*` bit the
/// port needed.
pub const FSOLID_NOT_SOLID: u32 = 0x0004;

/// `FSOLID_TRIGGER` (`public/const.h:231`) — "may be collideable but fires
/// touch functions even when it is not collideable".
///
/// The bit `ENGINE_TRACE.md` stage 2 said was missing, and stage 4's whole
/// point. **A trigger sets both this and [`FSOLID_NOT_SOLID`]**: its brushes
/// are `CONTENTS_SOLID` in the `.bsp` and what makes walking into one possible
/// is this pair, which is why nothing before this stage could put a brush
/// entity in the player's clip chain without turning every trigger in the game
/// into a wall.
pub const FSOLID_TRIGGER: u32 = 0x0008;

/// `FSOLID_VOLUME_CONTENTS` (`public/const.h:234`) — "contains volumetric
/// contents (like water)".
///
/// Read by one line — `PhysicsMarkEntityAsTouched`'s `bShouldTouch` — and set
/// by one class, `func_water_analog`, which this port does not have. Defined
/// so that the line reads the way the C++ does.
pub const FSOLID_VOLUME_CONTENTS: u32 = 0x0020;

/// `FL_ONGROUND` (`public/const.h:116`) — "at rest / on the ground".
///
/// The player's, mirrored from `client::Player::ground` once a tick.
/// `trigger_push` reads it (an upward push takes the player off the floor
/// first) and `trigger_teleport` clears it.
pub const FL_ONGROUND: u32 = 1 << 0;

/// `FL_CLIENT` (`public/const.h:128`) — "is a player".
///
/// The one `m_fFlags` bit anything here sets. `PassesTriggerFilters` tests it
/// against `SF_TRIGGER_ALLOW_CLIENTS`, which is set on 1,220 of the game's
/// 1,476 `trigger_once`s and is what makes a trigger a trigger for the player.
pub const FL_CLIENT: u32 = 1 << 8;

/// `FL_BASEVELOCITY` (`public/const.h:153`) — "base velocity has been applied
/// this frame".
///
/// Set by `trigger_push` every tick it is pushing, and cleared by the player's
/// own move. The pair is what turns a push into momentum when the player
/// leaves the trigger rather than into a velocity that never goes away —
/// `CPlayerMove::CheckMovingGround` (`player_command.cpp:93`).
pub const FL_BASEVELOCITY: u32 = 1 << 24;

/// `FL_FROZEN` (`public/const.h:126`) — "Player is frozen for 3rd person
/// camera".
///
/// Set by `CRevertSaved::InputReload` and read by
/// `CPortalGameMovement::CheckParameters`, which zeroes the whole move. Those
/// two are the entire live path in this port, and they are 11 shipped
/// connections at 9 `player_loadsaved` entities.
pub const FL_FROZEN: u32 = 1 << 6;

/// `FL_GODMODE` (`public/const.h:140`) — the `god` command.
///
/// Read by two lines, and they are not the same line:
/// `CBasePlayer::OnTakeDamage` refuses the hit outright, and
/// `CBaseCombatCharacter::OnTakeDamage_Alive` refuses it again for everything
/// that is not a player. Only the first is reachable here.
pub const FL_GODMODE: u32 = 1 << 15;

/// `FL_NOTARGET` (`public/const.h:141`). Set alongside [`FL_FROZEN`] by
/// `CRevertSaved::InputReload` and read by the AI, which is not ported —
/// carried so that the flag word an `ent_dump` prints is the one the shipped
/// game would print.
pub const FL_NOTARGET: u32 = 1 << 16;

/// `SolidType_t` (`public/const.h:216`) — *how* an entity is solid, as opposed
/// to the `FSOLID_*` bits, which say whether it is.
///
/// Four of Valve's seven. `SOLID_OBB`, `SOLID_OBB_YAW` and `SOLID_CUSTOM` are
/// not set by any class this port has — the first is marked "not implemented
/// yet" in `const.h` itself.
///
/// **The distinction that matters is only `None` versus the rest**, because
/// that is all [`EntityCore::is_solid`](super::entity::EntityCore::is_solid)
/// asks. The other three are kept apart because the classes genuinely choose
/// between them and the choice is legible: a `func_door` is `SOLID_BSP` when
/// its root parent is and `SOLID_VPHYSICS` otherwise, and a trigger is
/// `SOLID_VPHYSICS` when it has a parent.
///
/// [`VPhysics`](Solid::VPhysics) collides as a brush model here, because a
/// brush entity's `vcollide` *is* its brushes and there is no `vphysics` to
/// ask (`ENGINE_TRACE.md` stage 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Solid {
    /// `SOLID_NONE` — no collision model at all. Every point entity, and a
    /// `func_button` with `SF_BUTTON_NOTSOLID`.
    #[default]
    None,
    /// `SOLID_BSP` — the entity's own subtree of the map's BSP.
    Bsp,
    /// `SOLID_BBOX` — an axis-aligned box. The player.
    Bbox,
    /// `SOLID_OBB` — a box in the entity's *own* frame, turned by its
    /// `angles`. What a trigger built out of nothing but numbers is: the
    /// `trigger_portal_button` a `prop_floor_button` puts over itself, and
    /// every `prop_dynamic` in the game whose `solid` key is `0` — 2,622 of
    /// them, promoted here by `CDynamicProp::Spawn` so that a *turned* prop's
    /// render box turns with it.
    ///
    /// > **This is the one solidity type the port chooses between rather than
    /// > records.** [`Bsp`](Solid::Bsp) and [`VPhysics`](Solid::VPhysics) are
    /// > the same brushes by two names and nothing reads which; `Obb` decides
    /// > *which collision question gets asked* — a box entity's overlap is
    /// > answered by [`obb`](super::obb) inside this module, where a brush
    /// > entity's goes out to the engine through
    /// > [`TouchQuery`](super::TouchQuery).
    Obb,
    /// `SOLID_VPHYSICS` — the model's `vcollide`. For a brush entity that is
    /// the same brushes [`Bsp`](Solid::Bsp) names.
    VPhysics,
}

impl Solid {
    /// The `solid` map key — `DEFINE_KEYFIELD( m_nSolidType, FIELD_CHARACTER,
    /// "solid" )` (`collisionproperty.cpp:283`).
    ///
    /// It is a `CCollisionProperty` field and therefore `CBaseEntity`'s, which
    /// is why it is read by
    /// [`base_key_value`](super::keyvalue::base_key_value) rather than by a
    /// class — even though, measured over the 106 shipped maps, **only the
    /// prop family ever writes it**: 8,462 `prop_dynamic`/`prop_dynamic_override`,
    /// 5 `prop_vehicle_choreo_generic` and 3 `prop_button`, and no other
    /// classname at all.
    ///
    /// `None` for `SOLID_OBB_YAW` (4) and `SOLID_CUSTOM` (5), which have no
    /// counterpart here and which no shipped map writes; the caller leaves the
    /// field alone, the way a raw keyfield write of an unusable value would
    /// leave the *behaviour* alone.
    pub fn from_key(value: i32) -> Option<Solid> {
        match value {
            0 => Some(Solid::None),
            1 => Some(Solid::Bsp),
            2 => Some(Solid::Bbox),
            3 => Some(Solid::Obb),
            6 => Some(Solid::VPhysics),
            _ => Option::None,
        }
    }
}

/// The bounding box of the brush model an entity names, in the model's own
/// frame.
///
/// The one thing a mover needs that is **not** in the entity lump.
/// `CBaseDoor::Spawn`, `CBaseButton::Spawn` and `CFuncMoveLinear::Spawn` all
/// ask `CollisionProp()->OBBSize()` how wide the brush is so they can slide it
/// exactly its own width out of the way, and in the original that box got
/// there through `SetModel` → `UTIL_SetModel` → `SetMinMaxSize`
/// (`util.cpp:1426`) reading the `.bsp`'s model lump.
///
/// Here it is filled in by
/// [`Server::level_init`](super::Server::level_init) from the same lump, which
/// `world/` has already parsed — so this module still names no model and no
/// file format, only three numbers twice.
///
/// **Zero for an entity with no brush model**, which is what
/// `UTIL_SetModel`'s `else` branch sets too, and which gives such an entity a
/// zero-length travel rather than an error.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ModelBounds {
    pub mins: Vec3,
    pub maxs: Vec3,
}

/// `TOGGLE_STATE` (`game/server/baseentity.h:160`) — where a two-position
/// mover is in its cycle.
///
/// "Top" and "bottom" are Quake's names and mean *open* and *closed*; a door
/// that opens downwards is still going "up" when it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToggleState {
    /// `TS_AT_TOP` — fully open.
    AtTop,
    /// `TS_AT_BOTTOM` — fully closed. Where every mover in the game spawns
    /// unless it says otherwise.
    #[default]
    AtBottom,
    /// `TS_GOING_UP` — opening.
    GoingUp,
    /// `TS_GOING_DOWN` — closing.
    GoingDown,
}

/// `togglemovetypes_t` (`subs.cpp:118`) — which of the two moves is running.
///
/// Kept because `CBaseToggle::MoveDone` switches on it to decide whether to
/// snap the origin or the angles, and a mover that snapped the wrong one would
/// arrive a fraction of a tick's worth of motion past its destination and
/// never quite line up with the doorway it fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Movement {
    #[default]
    None,
    Linear,
    Angular,
}

/// `CBaseToggle` — the state a two-position mover keeps, and the two moves.
///
/// `CBaseDoor`, `CBaseButton` and `CFuncMoveLinear` all derive from it in the
/// original; here they **hold** one and call into it, which is the same
/// composition `EnvLight` makes of `Light` (`rustdocs/SERVER.md` gotcha 13).
/// `CFuncRotating` does *not* derive from it and does not hold one — it drives
/// [`EntityCore`]'s angular velocity directly.
///
/// # What is not here
///
/// `m_sMaster`/`IsLockedByMaster` is the `multisource` interlock, and **no
/// shipped Portal 2 map sets a `master` key on any of these classes** — so the
/// field, the key and `UTIL_IsMasterTriggered` are all measured out rather than
/// ported. `m_hActivator` is kept by the classes that fire an output with it
/// rather than here, because only two of them do.
#[derive(Debug, Default)]
pub struct Toggle {
    /// `m_toggle_state`.
    pub state: ToggleState,
    /// `m_flWait` — how long a mover rests at the top before returning.
    /// **`-1` means "stay there"**, which is what 503 of the game's 621 doors
    /// say.
    pub wait: f32,
    /// `m_flLip` — how much of the model's own width to leave behind when it
    /// slides away. 139 of 275 `func_door`s set it to 0 explicitly.
    pub lip: f32,
    /// `m_flMoveDistance` — how far a rotating door turns, in degrees. All 346
    /// of the game's set it; the commonest value is 90.
    pub move_distance: f32,
    /// `m_vecPosition1`/`m_vecPosition2` — closed and open, in world units.
    pub position1: Vec3,
    pub position2: Vec3,
    /// `m_vecAngle1`/`m_vecAngle2` — closed and open, as angles.
    pub angle1: Vec3,
    pub angle2: Vec3,
    /// `m_vecMoveAng` — the axis a rotating mover turns about, as a unit
    /// `QAngle`. `AxisDir` builds it from the spawnflags.
    pub move_ang: Vec3,
    /// `m_vecFinalDest`/`m_vecFinalAngle` — where the move in progress ends.
    /// The mover is snapped onto exactly this when the alarm goes off, so that
    /// a hundred ticks of floating-point integration cannot leave a door ajar.
    final_dest: Vec3,
    final_angle: Vec3,
    /// `m_movementType`.
    movement: Movement,
}

impl Toggle {
    /// `CBaseToggle::KeyValue` (`subs.cpp:174`), minus `master`. A holder's
    /// `key_value` ends by calling this, which *is* `BaseClass::KeyValue`.
    ///
    /// It takes `lip`, `wait` and `distance`, and a class that holds a
    /// [`Toggle`] must list all three in its own
    /// [`ClassDef::keys`](super::class::ClassDef::keys) — a contained class
    /// has no chain to be found through, so the declaration cannot be
    /// inherited either (`rustdocs/SERVER.md` gotcha 13).
    pub fn key_value(&mut self, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("lip") {
            self.lip = atof(value);
        } else if key.eq_ignore_ascii_case("wait") {
            self.wait = atof(value);
        } else if key.eq_ignore_ascii_case("distance") {
            self.move_distance = atof(value);
        } else {
            return false;
        }
        true
    }

    /// `CBaseToggle::LinearMove` (`subs.cpp:214`) — head for `dest` at
    /// `speed` units a second.
    ///
    /// Returns whether a move actually started: a mover already standing on
    /// its destination calls `MoveDone()` at once instead, which is Valve's
    /// early-out and is what stops a zero-length move dividing by zero.
    #[must_use = "a false answer means MoveDone must be run now"]
    pub fn linear_move(&mut self, entity: &mut EntityCore, dest: Vec3, speed: f32) -> bool {
        self.final_dest = dest;
        self.movement = Movement::Linear;

        // `if (vecDest == GetLocalOrigin())` — an exact comparison, kept
        // exact. 53 of the game's 64 `func_button`s are `SF_BUTTON_DONTMOVE`
        // and spawn with position2 == position1, so this is the path most
        // buttons in Portal 2 take on every press.
        //
        // **Local, not absolute.** Every coordinate `CBaseToggle` touches is
        // in the parent's frame: `m_vecPosition1`/`m_vecPosition2` are read
        // off `GetLocalOrigin()` at spawn, the velocity it computes is a
        // *local* velocity, and `LinearlyMoveRootEntity` integrates it into
        // the local origin. 201 of the game's movers are parented and for
        // those the two frames differ; for the rest they are equal and this
        // reads the same as it always did.
        if dest == entity.local_origin {
            return false;
        }

        let delta = dest - entity.local_origin;
        let travel_time = delta.length() / speed;
        entity.set_move_done_time(travel_time);
        entity.velocity = delta / travel_time;
        true
    }

    /// `CBaseToggle::AngularMove` (`subs.cpp:275`) — turn towards
    /// `dest_angle` at `speed` degrees a second.
    ///
    /// > **A `QAngle`'s "length" is the Euclidean length of its three
    /// > components** (`vector.h:2666`), so a 90° yaw at speed 100 takes 0.9
    /// > seconds and a move that turns two axes at once takes the hypotenuse.
    /// > That is Valve's, and it is why `speed` on a rotating door reads as
    /// > degrees per second only when one axis moves.
    #[must_use = "a false answer means MoveDone must be run now"]
    pub fn angular_move(&mut self, entity: &mut EntityCore, dest_angle: Vec3, speed: f32) -> bool {
        self.final_angle = dest_angle;
        self.movement = Movement::Angular;

        if dest_angle == entity.local_angles {
            return false;
        }

        let delta = dest_angle - entity.local_angles;
        let mut travel_time = delta.length() / speed;

        // `MinTravelTime` (`subs.cpp:293`): "If we only travel for a short
        // time, we can fail WillSimulateGamePhysics()". A move shorter than a
        // tick would set an alarm that is already due, and an alarm that is
        // already due takes the entity straight back out of the simulation
        // list — so the door would never move at all. The clamp is one line
        // and it is load-bearing.
        const MIN_TRAVEL_TIME: f32 = 0.01;
        if travel_time < MIN_TRAVEL_TIME {
            travel_time = MIN_TRAVEL_TIME;
        }

        entity.set_move_done_time(travel_time);
        entity.angular_velocity = delta / travel_time;
        true
    }

    /// `CBaseToggle::MoveDone` (`subs.cpp:240`): snap onto the destination,
    /// stop, and disarm.
    ///
    /// A holder's [`Behaviour::move_done`] calls this **first** and then runs
    /// its own callback, which is the order `BaseClass::MoveDone()` at the end
    /// of `CBaseToggle::MoveDone` produces.
    pub fn move_done(&mut self, entity: &mut EntityCore) {
        match self.movement {
            // `LinearMoveDone` (`subs.cpp:258`) — `UTIL_SetOrigin`, which is
            // `SetLocalOrigin` and nothing else (`util.cpp:1394`).
            Movement::Linear => {
                entity.set_local_origin(self.final_dest);
                entity.velocity = Vec3::ZERO;
                entity.set_move_done_time(-1.0);
            }
            // `AngularMoveDone` (`subs.cpp:310`).
            Movement::Angular => {
                entity.set_local_angles(self.final_angle);
                entity.angular_velocity = Vec3::ZERO;
                entity.set_move_done_time(-1.0);
            }
            Movement::None => {}
        }
        self.movement = Movement::None;
    }

    /// `CBaseToggle::AxisDir` (`subs.cpp:344`) — which axis the spawnflags
    /// say to turn about, as a unit `QAngle`.
    ///
    /// Yaw unless told otherwise, which is what "all doors face East at all
    /// times and twist their local angle to open" means.
    pub fn axis_dir(&mut self, spawn_flags: u32) {
        self.move_ang = if spawn_flags & SF_DOOR_ROTATE_ROLL != 0 {
            Vec3::new(0.0, 0.0, 1.0)
        } else if spawn_flags & SF_DOOR_ROTATE_PITCH != 0 {
            Vec3::new(1.0, 0.0, 0.0)
        } else {
            Vec3::new(0.0, 1.0, 0.0)
        };
    }

    /// Where the move in progress ends. `m_vecFinalDest`, for `ent_dump`.
    pub fn final_dest(&self) -> Vec3 {
        self.final_dest
    }

    /// `m_vecFinalDest = vecDest` without starting a move.
    ///
    /// `CFuncMoveLinear::Spawn` does exactly this so that `SetSpeed` has
    /// somewhere to aim before the first move — the one caller.
    pub fn set_final_dest(&mut self, dest: Vec3) {
        self.final_dest = dest;
    }
}

// ---------------------------------------------------------------------------
// the door spawnflags, which `CBaseToggle` reads and three classes share
// ---------------------------------------------------------------------------

/// `SF_DOOR_ROTATE_ROLL` (`doors.h:29`). 94 of the game's 346 rotating doors.
pub const SF_DOOR_ROTATE_ROLL: u32 = 64;
/// `SF_DOOR_ROTATE_PITCH` (`doors.h:30`). 143 — the commonest non-yaw axis,
/// because a Portal 2 wall panel hinges about its long edge.
pub const SF_DOOR_ROTATE_PITCH: u32 = 128;

// ---------------------------------------------------------------------------
// the pusher
// ---------------------------------------------------------------------------

/// `Physics_SimulateEntity` (`physics_main.cpp:2209`) →
/// `CBaseEntity::PhysicsSimulate` (`physics_main_shared.cpp:1772`), reduced to
/// the movetypes that exist.
///
/// Run once per tick for every entity the simulation list holds, which is
/// every entity that will think this tick *or* is a mover with a live alarm —
/// see [`ThinkList`](super::think::ThinkList).
///
/// The base-velocity block, the ground-entity check and the move-parent
/// recursion are all absent because the state they read does not exist: there
/// is no ground, no base velocity, and parenting resolves a handle and moves
/// nothing (see [`perform_push`]).
pub fn simulate(entity: &mut EntityCore, behaviour: &mut dyn Behaviour, cx: &mut Context<'_>) {
    match entity.move_type {
        // `PhysicsNone` (`physics_main.cpp:1722`) — "non moving objects can
        // only think". The player's three movetypes take this branch as well,
        // because `client/` is what moves the player and the server holds a
        // copy; see [`MoveType`]. That includes `MOVETYPE_FLYGRAVITY`, which
        // is the dead player — and it is what makes `PlayerDeathThink` run at
        // all, since a think is the only thing this branch does.
        MoveType::None | MoveType::Walk | MoveType::Noclip | MoveType::FlyGravity => {
            physics_run_think(entity, behaviour, cx);
        }
        MoveType::Push => physics_pusher(entity, behaviour, cx),
    }
}

/// `CBaseEntity::PhysicsRunThink` → `PhysicsRunSpecificThink`
/// (`physics_main_shared.cpp:2080`).
///
/// Returns whether the entity survived, which is what `PhysicsPusher` tests
/// before pushing a corpse.
///
/// > **The tick is re-checked here**, because the simulation list does not
/// > filter on it for a mover: an entity with a live alarm is copied out every
/// > tick whatever its think schedule says, so a think that is not due yet has
/// > to refuse for itself. `thinktick <= 0` is the other half of the same
/// > guard and is why `SetNextThink(0)` means "not scheduled".
///
/// > **The schedule is cleared before the think runs**, so a recurring
/// > behaviour must re-arm itself on the way out
/// > (`rustdocs/SERVER.md` gotcha 6).
fn physics_run_think(
    entity: &mut EntityCore,
    behaviour: &mut dyn Behaviour,
    cx: &mut Context<'_>,
) -> bool {
    let think_tick = entity.next_think_tick();
    if think_tick <= 0 || think_tick > cx.time.tick {
        return !entity.removed;
    }

    entity.clear_next_think();
    behaviour.think(entity, cx);

    !entity.removed
}

/// `CBaseEntity::PhysicsPusher` (`physics_main.cpp:1700`).
///
/// Think first, then move by however much of the remaining travel fits in one
/// tick. `GetMoveDoneTime()` is `-1` when nothing is armed, which is `<= 0`
/// and therefore not a move — so a `MOVETYPE_PUSH` entity that is standing
/// still costs exactly one comparison.
fn physics_pusher(entity: &mut EntityCore, behaviour: &mut dyn Behaviour, cx: &mut Context<'_>) {
    if !physics_run_think(entity, behaviour, cx) {
        return;
    }

    // `gpGlobals->frametime` on the server is the tick interval, because
    // `_Host_RunFrame_Server` sets it to `interval_per_tick` before calling
    // the game's frame.
    let mut movetime = entity.move_done_time();
    if movetime > cx.time.interval {
        movetime = cx.time.interval;
    }

    perform_push(entity, behaviour, cx, movetime);
}

/// `CBaseEntity::PerformPush` (`physics_main.cpp:1590`) with no blocker.
///
/// Three things happen and the order is Valve's: local time advances, the
/// entity rotates and then translates, and the arrival alarm is tested.
///
/// > **The last step of a move is exactly as long as the travel that is left**,
/// > because `PhysicsPusher` clamps `movetime` to the *remaining* time rather
/// > than always taking a whole tick. So the alarm goes off on the tick the
/// > move ends, not the tick after, and `local_time` lands on
/// > `move_done_time` rather than stepping over it. Get this wrong and every
/// > door in the game arrives up to a sixty-fourth of a second late and a
/// > fraction of a unit long.
///
/// # What the blocker would have done
///
/// `PerformPush` calls `PhysicsPushRotate` and `PhysicsPushMove`, each of
/// which asks `CPhysicsPushedEntities` to move the whole hierarchy and rolls
/// local time *back* if something was in the way; the blocker then reaches
/// `StartBlocked`/`Blocked`/`EndBlocked`. None of that is here
/// (`portdocs/SERVER.md` stage 3 excludes it), so the two branches collapse
/// into "rotate if rotating, translate if translating" and local time never
/// goes backwards.
///
/// # A parented mover moves in its parent's frame
///
/// Valve integrates `GetLocalVelocity()` into `GetLocalOrigin()`, which for an
/// entity with a parent is that parent's frame — and so does this, since
/// [`EntityCore`] grew the transform pair. **201 of the game's movers name a
/// parent** (87 `func_movelinear`, 63 `func_door_rotating`, 24 `func_door`,
/// 16 `func_button`, 9 `func_rotating`, 1 each of `momentary_rot_button` and
/// `func_tracktrain`), and before the pair existed every one of them drove
/// itself back towards a fixed world position whenever the thing it is bolted
/// to moved.
///
/// **Nothing here pushes the change down to the children.**
/// `SetupAllInHierarchy` (`physics_main.cpp:889`) is what does that in the
/// C++; here it is one call in `Server::dispatch`, after the handler returns,
/// so that a mover that moves *and then snaps* in `MoveDone` drags its subtree
/// exactly once and onto the final placement — see
/// [`hierarchy`](super::hierarchy).
fn perform_push(
    entity: &mut EntityCore,
    behaviour: &mut dyn Behaviour,
    cx: &mut Context<'_>,
    movetime: f32,
) {
    if movetime > 0.0 {
        // `IncrementLocalTime( movetime )`, which both push paths do and
        // which happens *before* the velocity is looked at — a pusher with no
        // velocity still spends the time, and that is what lets the alarm
        // double as a plain wait timer (`CBaseDoor::DoorHitTop` sets it to
        // `m_flWait` with the door standing still).
        entity.local_time += movetime;

        // `RotateRootEntity` then `LinearlyMoveRootEntity`, in that order.
        // Valve runs rotation first and says so; with no blocker the order is
        // not observable, and it is kept because the ordering *is* observable
        // the moment a blocker exists.
        //
        // Both are `SetLocal*( GetLocal*() + GetLocal*Velocity() * movetime )`
        // (`physics_main.cpp:993` and `:1057`) — **the pusher integrates in
        // the parent's frame**, which is what makes a door on a moving
        // platform open relative to the platform instead of driving itself
        // back to a fixed world position every tick.
        if entity.angular_velocity != Vec3::ZERO {
            entity.set_local_angles(entity.local_angles + entity.angular_velocity * movetime);
        }
        if entity.velocity != Vec3::ZERO {
            entity.set_local_origin(entity.local_origin + entity.velocity * movetime);
        }
    }

    // `if ( m_flMoveDoneTime <= m_flLocalTime && m_flMoveDoneTime > 0 )`
    // (`physics_main.cpp:1685`). Note that both halves test the *absolute*
    // alarm rather than the remaining time, so an alarm set for local time
    // zero never fires.
    let alarm = entity.raw_move_done_time();
    if alarm <= entity.local_time && alarm > 0.0 {
        entity.set_move_done_time(-1.0);
        behaviour.move_done(entity, cx);
    }
}

/// `anglemod` (`public/mathlib/mathlib.h:952`) — an angle folded into
/// `[0, 360)` **through a 16-bit fixed-point round trip**.
///
/// Not `rem_euclid(360.0)`: Valve quantises to 65,536ths of a turn on the way
/// through, so the answer is a multiple of 360/65536 ≈ 0.0055° and a negative
/// input comes back through the `& 65535` rather than through a branch. One
/// caller — `func_rotating`'s stop-at-start-position — and it compares the
/// result against thresholds of 1° and 90°, so the quantisation never decides
/// anything; it is ported because rewriting arithmetic is how an epsilon
/// silently changes (`PORTING.md`).
pub fn anglemod(a: f32) -> f32 {
    (360.0 / 65536.0) * ((a * (65536.0 / 360.0)) as i32 & 65535) as f32
}

/// `DotProductAbs` (`public/mathlib/vector.h:1418`) — the sum of the absolute
/// products, component by component.
///
/// **Not `|a · b|`.** It is `|a.x·b.x| + |a.y·b.y| + |a.z·b.z|`, which for a
/// unit `a` and a box size `b` is "how far the box extends along `a`,
/// whichever way `a` points" — which is exactly what a door needs in order to
/// slide its own width out of a doorway regardless of the sign of its
/// `movedir`.
pub fn dot_product_abs(a: Vec3, b: Vec3) -> f32 {
    (a.x * b.x).abs() + (a.y * b.y).abs() + (a.z * b.z).abs()
}

/// `AngleVectors( angMoveDir, &m_vecMoveDir )` — the `movedir` key, which is
/// written as angles and used as a direction.
///
/// The forward vector is the first column of `AngleMatrix`, so this is
/// [`crate::math::angle_matrix`] applied to `+X` rather than a second copy of
/// the trigonometry. Every mover in the game has one and reads it the same
/// way: `"90 0 0"` is straight down, `"-90 0 0"` straight up, `"0 90 0"`
/// towards `+Y`.
pub fn move_dir(angles: Vec3) -> Vec3 {
    crate::math::angle_matrix(angles) * Vec3::X
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::class::SpawnResult;
    use crate::server::classes;
    use crate::server::entity::{Entity, EntityCore};
    use crate::server::io::Input;
    use crate::server::test_support::Harness;

    /// A behaviour that is nothing but a [`Toggle`], so that this module's
    /// tests exercise the pusher without going through a real class.
    #[derive(Default)]
    struct TestMover {
        toggle: Toggle,
        arrivals: u32,
    }

    impl Behaviour for TestMover {
        fn spawn(&mut self, _entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
            SpawnResult::Ok
        }

        fn accept_input(
            &mut self,
            _entity: &mut EntityCore,
            _input: &Input<'_>,
            _cx: &mut Context<'_>,
        ) -> bool {
            false
        }

        /// The holder's half: snap through the contained [`Toggle`], then do
        /// whatever this class does. See `rustdocs/SERVER.md` gotcha 13.
        fn move_done(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) {
            self.toggle.move_done(entity);
            self.arrivals += 1;
        }
    }

    /// A bare entity, borrowed only for its [`EntityCore`] — the behaviour the
    /// tests drive is [`TestMover`], which no class table knows about.
    fn core() -> Entity {
        Entity::new(classes::lookup("info_target").expect("registered"))
    }

    /// `"90 0 0"` is pitch 90, and pitch turns `+X` towards `-Z`.
    #[test]
    fn movedir_is_angles_read_as_a_forward_vector() {
        let close = |a: Vec3, b: Vec3| assert!((a - b).length() < 1e-5, "{a} vs {b}");
        close(move_dir(Vec3::new(90.0, 0.0, 0.0)), -Vec3::Z);
        close(move_dir(Vec3::new(-90.0, 0.0, 0.0)), Vec3::Z);
        close(move_dir(Vec3::new(0.0, 90.0, 0.0)), Vec3::Y);
        close(move_dir(Vec3::ZERO), Vec3::X);
    }

    /// Not `|a·b|`: the two disagree the moment any component product is
    /// negative, which is the case a door's `movedir` is chosen to produce.
    #[test]
    fn dot_product_abs_is_not_the_absolute_dot_product() {
        let a = Vec3::new(1.0, 0.0, 0.0);
        let b = Vec3::new(-64.0, 8.0, 8.0);
        assert_eq!(dot_product_abs(a, b), 64.0);
        assert_eq!(a.dot(b).abs(), 64.0, "they agree on one axis");

        let a = Vec3::new(0.6, 0.8, 0.0);
        let b = Vec3::new(-10.0, 10.0, 0.0);
        assert!((dot_product_abs(a, b) - 14.0).abs() < 1e-5);
        assert!((a.dot(b) - 2.0).abs() < 1e-5, "and not on two");
    }

    /// The quantised fold, including the negative case that has no branch.
    #[test]
    fn anglemod_folds_through_sixteen_bits() {
        assert!(anglemod(0.0).abs() < 1e-3);
        assert!((anglemod(90.0) - 90.0).abs() < 0.01);
        assert!((anglemod(361.0) - 1.0).abs() < 0.01);
        // -90 comes back as 270, through the mask rather than through a
        // conditional add.
        assert!((anglemod(-90.0) - 270.0).abs() < 0.01);
    }

    /// The whole mover in miniature: set a velocity and an alarm, integrate it
    /// a tick at a time, and land exactly on the destination.
    #[test]
    fn a_linear_move_arrives_on_the_tick_it_was_scheduled_for() {
        let mut harness = Harness::new();
        let mut entity = core();
        entity.core.move_type = MoveType::Push;
        let mut mover = TestMover::default();

        // 100 units at 100 units a second is one second — 64 ticks.
        assert!(mover
            .toggle
            .linear_move(&mut entity.core, Vec3::new(100.0, 0.0, 0.0), 100.0));
        assert_eq!(entity.core.velocity, Vec3::new(100.0, 0.0, 0.0));
        assert!((entity.core.move_done_time() - 1.0).abs() < 1e-6);

        let mut ticks = 0;
        while mover.arrivals == 0 {
            harness.tick(&mut entity.core, &mut mover);
            ticks += 1;
            assert!(ticks < 200, "the move never finished");
        }
        assert_eq!(ticks, 64, "one second at 64 Hz");
        assert_eq!(
            entity.core.origin,
            Vec3::new(100.0, 0.0, 0.0),
            "snapped onto the destination, not integrated onto it"
        );
        assert_eq!(entity.core.velocity, Vec3::ZERO);
        assert!(!entity.core.will_simulate_game_physics(), "and disarmed");
    }

    /// Halfway through, the mover really is halfway: the integration is not
    /// deferred to the arrival.
    #[test]
    fn a_move_in_progress_is_where_it_should_be() {
        let mut harness = Harness::new();
        let mut entity = core();
        entity.core.move_type = MoveType::Push;
        let mut mover = TestMover::default();
        assert!(mover
            .toggle
            .linear_move(&mut entity.core, Vec3::new(64.0, 0.0, 0.0), 64.0));

        for _ in 0..32 {
            harness.tick(&mut entity.core, &mut mover);
        }
        assert_eq!(mover.arrivals, 0, "still travelling");
        assert!(
            (entity.core.origin.x - 32.0).abs() < 1e-3,
            "{}",
            entity.core.origin
        );
    }

    /// An angular move that would take less than a hundredth of a second is
    /// stretched to one, because a shorter alarm is already due and an alarm
    /// that is already due takes the entity out of the simulation list.
    #[test]
    fn a_very_short_angular_move_is_stretched_to_a_hundredth_of_a_second() {
        let mut entity = core();
        entity.core.move_type = MoveType::Push;
        let mut mover = TestMover::default();
        assert!(mover
            .toggle
            .angular_move(&mut entity.core, Vec3::new(0.0, 0.1, 0.0), 1000.0));
        assert!((entity.core.move_done_time() - 0.01).abs() < 1e-6);
        assert!(entity.core.will_simulate_game_physics());
    }

    /// A mover already standing on its destination does not move, and says so
    /// rather than dividing by a zero travel time.
    #[test]
    fn a_move_to_where_we_already_are_starts_nothing() {
        let mut entity = core();
        let mut mover = TestMover::default();
        assert!(!mover
            .toggle
            .linear_move(&mut entity.core, Vec3::ZERO, 100.0));
        assert_eq!(entity.core.velocity, Vec3::ZERO);
        assert_eq!(entity.core.move_done_time(), -1.0, "nothing was armed");

        assert!(!mover
            .toggle
            .angular_move(&mut entity.core, Vec3::ZERO, 100.0));
        assert_eq!(entity.core.angular_velocity, Vec3::ZERO);
    }

    /// The alarm doubles as a wait timer: no velocity, and it still fires.
    /// `CBaseDoor::DoorHitTop` is the caller that depends on it.
    #[test]
    fn the_alarm_runs_down_with_no_velocity_at_all() {
        let mut harness = Harness::new();
        let mut entity = core();
        entity.core.move_type = MoveType::Push;
        entity.core.set_move_done_time(0.5);
        let mut mover = TestMover::default();

        assert!(entity.core.will_simulate_game_physics());
        let mut ticks = 0;
        while mover.arrivals == 0 {
            harness.tick(&mut entity.core, &mut mover);
            ticks += 1;
            assert!(ticks < 100, "the alarm never went off");
        }
        assert_eq!(ticks, 32, "half a second at 64 Hz");
        assert_eq!(entity.core.origin, Vec3::ZERO, "and nothing moved");
    }

    /// An alarm armed for a delay of zero is never due, because the test is
    /// against the *absolute* alarm and zero is not greater than zero. This is
    /// Valve's, and it is what leaves four of the shipped game's rotating
    /// doors standing open for ever — see [`perform_push`].
    #[test]
    fn an_alarm_armed_for_no_delay_at_all_never_fires() {
        let mut harness = Harness::new();
        let mut entity = core();
        entity.core.move_type = MoveType::Push;
        let mut mover = TestMover::default();

        entity.core.set_move_done_time(0.0);
        assert_eq!(entity.core.raw_move_done_time(), 0.0);
        assert!(
            !entity.core.will_simulate_game_physics(),
            "and so it leaves the simulation list entirely"
        );
        for _ in 0..10 {
            harness.tick(&mut entity.core, &mut mover);
        }
        assert_eq!(mover.arrivals, 0);
    }

    /// A `MOVETYPE_NONE` entity is never pushed, whatever its velocity says.
    #[test]
    fn a_still_entity_does_not_move() {
        let mut harness = Harness::new();
        let mut entity = core();
        entity.core.velocity = Vec3::new(100.0, 0.0, 0.0);
        entity.core.set_move_done_time(1.0);
        let mut mover = TestMover::default();

        for _ in 0..64 {
            harness.tick(&mut entity.core, &mut mover);
        }
        assert_eq!(entity.core.origin, Vec3::ZERO);
        assert_eq!(mover.arrivals, 0);
    }
}
