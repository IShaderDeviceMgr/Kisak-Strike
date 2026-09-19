//! The brush family: the entities a Portal 2 test chamber is *made* of.
//!
//! `doors.cpp` (`CBaseDoor`, `CRotDoor`), `func_movelinear.cpp`
//! (`CFuncMoveLinear`), `buttons.cpp` (`CBaseButton`), `bmodels.cpp`
//! (`CFuncRotating`) and `modelentities.cpp` (`CFuncBrush`).
//!
//! Six classnames, **3,410 of the shipped game's 60,925 entities**, and the
//! first ones in this port that *move*. Five of the six are
//! `MOVETYPE_PUSH` movers built out of
//! [`Toggle`](crate::server::movement::Toggle)'s two moves; the sixth,
//! `func_brush`, is `MOVETYPE_PUSH` only "so it doesn't get pushed by
//! anything" and exists to be switched on and off.
//!
//! ```text
//!   2502  func_brush           panels, clips, the walls that appear and vanish
//!    346  func_door_rotating   the commonest mover in the game
//!    275  func_door
//!    196  func_movelinear      pistons, monitors, the lift doors
//!     64  func_button
//!     27  func_rotating        fans, grinders, the shredder
//! ```
//!
//! # What every one of them needs and none of them has
//!
//! `CBaseDoor::Spawn`, `CBaseButton::Spawn` and `CFuncMoveLinear::Spawn` all
//! compute where they slide to from `CollisionProp()->OBBSize()` — the size of
//! the brush model the entity names. That is the one thing in this module that
//! is not in the entity lump, and it arrives as
//! [`ModelBounds`](crate::server::ModelBounds), read out of the `.bsp`'s model
//! lump by `world/` and handed to `Server::level_init` the same way the entity
//! lump is. An entity whose model is missing gets a zero-sized box and
//! therefore a zero-length travel, which is what the C++ would do too.
//!
//! # What none of them does
//!
//! Sound, damage, area portals, `+use` and touch. The first four have no
//! subsystem; the last two are stage 4's. What that costs is visible in the
//! class list: a `func_button` can be pressed by its `Press` input and not by
//! walking into it, and 42 of the game's 64 buttons are locked and unlocked by
//! I/O anyway.

use glam::Vec3;

use crate::server::class::{
    Behaviour, Context, InputDef, InputDefs, SpawnResult, UseType, NEVER_THINK,
};
use crate::server::damage::{DamageInfo, DMG_CRUSH};
use crate::server::entity::{EntityCore, EntityId};
use crate::server::io::{FieldType, Input, Variant};
use crate::server::keyvalue::{atof, atoi};
use crate::server::movement::{
    anglemod, dot_product_abs, move_dir, CollisionGroup, MoveType, Solid, Toggle, ToggleState,
    EF_NODRAW, FL_UNBLOCKABLE_BY_PLAYER, FSOLID_NOT_SOLID,
};

// ---------------------------------------------------------------------------
// func_door and func_door_rotating
// ---------------------------------------------------------------------------

/// `SF_DOOR_START_OPEN_OBSOLETE` (`doors.h:22`). **No shipped Portal 2 map
/// sets it** — the 40 doors that spawn open use `spawnpos 1` instead — so the
/// branches it selects are unreachable and are not ported. Recorded here
/// because its *absence* is what makes `spawnpos` the only spawn-position
/// mechanism worth reading.
const _SF_DOOR_START_OPEN_OBSOLETE: u32 = 1;
/// `SF_DOOR_ROTATE_BACKWARDS` — 79 of the game's 346 rotating doors.
const SF_DOOR_ROTATE_BACKWARDS: u32 = 2;
/// `SF_DOOR_NONSOLID_TO_PLAYER` — 141 of the game's 621 doors, 118 of them
/// rotating.
///
/// `COLLISION_GROUP_PASSABLE_DOOR` plus `FL_UNBLOCKABLE_BY_PLAYER`. The player
/// walks through such a door and cannot stop it; see
/// [`CollisionGroup`](crate::server::movement::CollisionGroup) for the one
/// `ShouldCollide` rule that is left and
/// [`push`](crate::server::push) for what the flag changes.
const SF_DOOR_NONSOLID_TO_PLAYER: u32 = 4;
/// `SF_DOOR_PASSABLE` — 116. `FSOLID_NOT_SOLID`: the door is scenery.
///
/// `EFL_USE_PARTITION_WHEN_NOT_SOLID` goes with it in the C++ and has no
/// counterpart — it keeps a non-solid entity in the spatial partition so
/// triggers still find it, and this port's touch query sweeps every brush
/// model whatever its solidity says (see
/// [`TouchQuery`](crate::server::TouchQuery)).
const SF_DOOR_PASSABLE: u32 = 8;
/// `SF_DOOR_NO_AUTO_RETURN` — 200. A door that stays open until told to shut.
const SF_DOOR_NO_AUTO_RETURN: u32 = 32;
/// `SF_DOOR_LOCKED` — 2 doors in the whole game.
const SF_DOOR_LOCKED: u32 = 2048;

/// `FuncDoorSpawnPos_t` (`doors.h:44`) — `FUNC_DOOR_SPAWN_OPEN`. 40 doors.
const FUNC_DOOR_SPAWN_OPEN: i32 = 1;

/// `CBaseDoor` (`game/server/doors.cpp`) and `CRotDoor` (`:1292`) — 621
/// entities, and the first thing in this port that opens.
///
/// The two are one struct with a [`rotating`](Door::rotating) flag, because
/// `CRotDoor`'s only non-`Spawn` override is `IsRotatingDoor()` returning
/// `true`: a per-class constant, which in Rust is a field set by the
/// constructor rather than a vtable slot.
///
/// # The cycle
///
/// ```text
///   Open  -> DoorGoUp   -> AngularMove/LinearMove -> DoorHitTop
///                                                      |
///                           wait seconds on the same alarm (unless -1)
///                                                      v
///   Close -> DoorGoDown -> AngularMove/LinearMove -> DoorHitBottom
/// ```
///
/// The wait between `DoorHitTop` and `DoorGoDown` is the *arrival alarm* again
/// with the door standing still, not a think — which is the whole reason
/// [`EntityCore::set_move_done_time`] is a separate timer.
///
/// # What `Activate` did, and why there is none here
///
/// `CBaseDoor::Activate` (`doors.cpp:448`) does two things and neither has
/// anywhere to land. It walks every door sharing this one's `targetname` into
/// a *movement group* and clears `m_bDoorGroup` if they disagree about where
/// they are — read only by `Blocked`, which is the pushing code stage 3
/// excludes. And it calls `UpdateAreaPortals`, which is the engine's
/// visibility system (`world/`'s, and not written).
pub struct Door {
    toggle: Toggle,
    /// `CRotDoor::IsRotatingDoor()`. Chooses `AngularMove` over `LinearMove`
    /// and changes what `Spawn` computes.
    rotating: bool,
    /// `m_vecMoveDir` — the `movedir` key, already turned from angles into a
    /// direction. Unused by a rotating door, which turns about
    /// [`Toggle::move_ang`] instead.
    move_dir: Vec3,
    /// `m_bLocked` — a locked door refuses `Open` and `Toggle`.
    locked: bool,
    /// `m_eSpawnPosition` — `spawnpos`, 0 closed and 1 open.
    spawn_position: i32,
    /// `m_bSolidBsp` — `CRotDoor`'s `solidbsp`. 5 doors set it; recorded
    /// because it is a declared key and solidity is stage 4's.
    solid_bsp: bool,
    /// `m_bForceClosed`, `m_flBlockDamage`, `m_bIgnoreDebris`,
    /// `m_bLoopMoveSound` — read so that the keys are consumed rather than
    /// counted as unknown, and acted on by nothing: all four are about
    /// blocking, damage or sound.
    force_closed: bool,
    block_damage: f32,
    ignore_debris: bool,
    loop_move_sound: bool,
    /// The four sound names and the two lock sounds. Kept as names for the
    /// same reason [`EntityCore::model`] is a name — there is no sound system,
    /// and `ent_dump` printing what a door *would* play is worth the strings.
    noise_moving: Option<String>,
    noise_arrived: Option<String>,
    noise_moving_closed: Option<String>,
    noise_arrived_closed: Option<String>,
    /// `m_hActivator` — who opened it. `DoorHitBottom` fires `OnFullyClosed`
    /// with it, which is the one place a door forwards an activator.
    activator: Option<EntityId>,
    /// `m_pfnMoveDone`, as an enum. See [`Behaviour::move_done`].
    move_done: DoorMoveDone,
}

/// Which of `CBaseDoor`'s four `SetMoveDone` targets is armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoorMoveDone {
    None,
    /// `SetMoveDone( &CBaseDoor::DoorHitTop )`.
    HitTop,
    /// `SetMoveDone( &CBaseDoor::DoorHitBottom )`.
    HitBottom,
    /// `SetMoveDone( &CBaseDoor::DoorGoDown )` — armed by `DoorHitTop` for the
    /// `wait`, with the door not moving at all.
    GoDown,
}

/// `CBaseDoor`'s keys plus `CRotDoor`'s, minus the ones
/// [`keyvalue::base_key_value`](crate::server::keyvalue::base_key_value)
/// takes.
pub static DOOR_KEYS: &[&str] = &[
    "movedir",
    "spawnpos",
    "forceclosed",
    "dmg",
    "ignoredebris",
    "loopmovesound",
    "noise1",
    "noise2",
    "startclosesound",
    "closesound",
    "locked_sentence",
    "unlocked_sentence",
    "locked_sound",
    "unlocked_sound",
    "solidbsp",
    "lip",
    "wait",
    "distance",
];

/// `CBaseDoor`'s inputs, minus `SetToggleState`.
///
/// `SetToggleState` is declared `FIELD_FLOAT` and read with `value.Int()`
/// (`doors.cpp:495`), which `variant_t` answers with **zero** for a float — so
/// in the shipped game it always means `TS_AT_TOP` whatever the map asked for.
/// Zero connections fire it, so it is measured out rather than reproduced.
pub static DOOR_INPUTS: InputDefs = &[
    InputDef::new("Open", FieldType::Void),
    InputDef::new("Close", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("Lock", FieldType::Void),
    InputDef::new("Unlock", FieldType::Void),
    InputDef::new("SetSpeed", FieldType::Float),
];

pub static DOOR_OUTPUTS: &[&str] = &[
    "OnClose",
    "OnOpen",
    "OnFullyClosed",
    "OnFullyOpen",
    "OnBlockedClosing",
    "OnBlockedOpening",
    "OnUnblockedClosing",
    "OnUnblockedOpening",
    "OnLockedUse",
];

impl Door {
    fn new(rotating: bool) -> Door {
        Door {
            toggle: Toggle::default(),
            rotating,
            move_dir: Vec3::ZERO,
            locked: false,
            spawn_position: 0,
            solid_bsp: false,
            force_closed: false,
            block_damage: 0.0,
            ignore_debris: false,
            loop_move_sound: false,
            noise_moving: None,
            noise_arrived: None,
            noise_moving_closed: None,
            noise_arrived_closed: None,
            activator: None,
            move_done: DoorMoveDone::None,
        }
    }

    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(Door::new(false))
    }

    /// `func_door_rotating`. `CRotDoor : public CBaseDoor`, which here is the
    /// same struct with `IsRotatingDoor()` answered at construction.
    pub(super) fn create_rotating() -> Box<dyn Behaviour> {
        Box::new(Door::new(true))
    }

    // `CBaseDoor::DoorActivate` (`doors.cpp:856`) is **not** ported. It is
    // the "something touched or used me" entry point — `DoorTouch` and
    // `ChainUse` are its only callers — and both are stage 4's; a door has no
    // `m_pfnUse` at all, so nothing reaches it through I/O. What it adds over
    // `InputToggle` is the `SF_DOOR_NO_AUTO_RETURN` branch and the `master`
    // interlock, and the second of those is measured out (see `Toggle`).

    /// `CBaseDoor::DoorGoUp` (`doors.cpp:885`).
    ///
    /// The 40-line block that decides which *way* a rotating door swings is
    /// not here: it needs `m_hActivator`'s position and
    /// `CollisionProp()->CalcNearestPoint`, and the activator is a player in
    /// every case that reaches it. Without one, `sign` stays `1.0` — which is
    /// the branch Valve takes for a door opened by I/O rather than by a
    /// person, and **every** door in Portal 2 is opened by I/O.
    fn go_up(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.toggle.state = ToggleState::GoingUp;
        self.move_done = DoorMoveDone::HitTop;

        let started = match self.rotating {
            true => self
                .toggle
                .angular_move(entity, self.toggle.angle2, entity.speed),
            false => self
                .toggle
                .linear_move(entity, self.toggle.position2, entity.speed),
        };

        // > **The arrival runs before the output, not after.**
        // > `LinearMove`/`AngularMove` call `MoveDone()` *themselves* when the
        // > destination is where they already are (`subs.cpp:219`), and that
        // > happens inside the call above — so a zero-length open delivers
        // > `OnFullyOpen`'s connections to the queue before `OnOpen`'s. Fire
        // > the output first and the two arrive in the wrong order, in the
        // > same tick, which is exactly the kind of difference a map's logic
        // > is built on.
        if !started {
            Behaviour::move_done(self, entity, cx);
        }

        let me = Some(entity.id());
        entity.fire_output("OnOpen", Variant::Void, me, me, 0.0, cx);
    }

    /// `CBaseDoor::DoorHitTop` (`doors.cpp:972`).
    fn hit_top(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.toggle.state = ToggleState::AtTop;

        if entity.has_spawn_flags(SF_DOOR_NO_AUTO_RETURN) {
            // "Toggle-doors don't come down automatically, they wait for
            // refire." The `SetTouch` this re-instates is stage 4's.
            self.move_done = DoorMoveDone::None;
        } else {
            // > **The same alarm, with the door standing still.** This is what
            // > makes `wait` work without a think, and a `wait` of exactly 0
            // > arms an alarm that can never fire — four `func_door_rotating`s
            // > in the shipped game stand open for ever because of it.
            entity.set_move_done_time(self.toggle.wait);
            self.move_done = DoorMoveDone::GoDown;
            if self.toggle.wait == -1.0 {
                entity.set_next_think(NEVER_THINK, cx);
            }
        }

        let me = Some(entity.id());
        entity.fire_output("OnFullyOpen", Variant::Void, me, me, 0.0, cx);
    }

    /// `CBaseDoor::DoorGoDown` (`doors.cpp:1029`).
    fn go_down(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.toggle.state = ToggleState::GoingDown;
        self.move_done = DoorMoveDone::HitBottom;

        let started = match self.rotating {
            true => self
                .toggle
                .angular_move(entity, self.toggle.angle1, entity.speed),
            false => self
                .toggle
                .linear_move(entity, self.toggle.position1, entity.speed),
        };

        // As in `go_up`: the arrival is inside `LinearMove`, before the output.
        if !started {
            Behaviour::move_done(self, entity, cx);
        }

        let me = Some(entity.id());
        entity.fire_output("OnClose", Variant::Void, me, me, 0.0, cx);
    }

    /// `CBaseDoor::DoorHitBottom` (`doors.cpp:1059`).
    ///
    /// > **The activator is forwarded here and nowhere else in the class.**
    /// > `m_OnFullyClosed.FireOutput( m_hActivator, this )` against
    /// > `DoorHitTop`'s `m_OnFullyOpen.FireOutput( this, this )` — Valve's
    /// > asymmetry, reproduced, because a chain that reads `!activator` after
    /// > a door shuts gets a different answer from one that reads it after the
    /// > door opens.
    fn hit_bottom(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.toggle.state = ToggleState::AtBottom;
        self.move_done = DoorMoveDone::None;

        let me = Some(entity.id());
        entity.fire_output("OnFullyClosed", Variant::Void, self.activator, me, 0.0, cx);
    }

    /// Runs whichever callback `SetMoveDone` last armed.
    fn run_move_done(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        match std::mem::replace(&mut self.move_done, DoorMoveDone::None) {
            DoorMoveDone::None => {}
            DoorMoveDone::HitTop => self.hit_top(entity, cx),
            DoorMoveDone::HitBottom => self.hit_bottom(entity, cx),
            DoorMoveDone::GoDown => self.go_down(entity, cx),
        }
    }
}

impl Behaviour for Door {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);

        if is("movedir") {
            // Stored as the raw angles until `Spawn`, which is where
            // `AngleVectors` runs in the C++ too.
            self.move_dir = crate::server::keyvalue::string_to_vector(value);
        } else if is("spawnpos") {
            self.spawn_position = atoi(value);
        } else if is("forceclosed") {
            self.force_closed = atoi(value) != 0;
        } else if is("dmg") {
            self.block_damage = atof(value);
        } else if is("ignoredebris") {
            self.ignore_debris = atoi(value) != 0;
        } else if is("loopmovesound") {
            self.loop_move_sound = atoi(value) != 0;
        } else if is("solidbsp") {
            self.solid_bsp = atoi(value) != 0;
        } else if is("noise1") {
            self.noise_moving = Some(value.to_owned());
        } else if is("noise2") {
            self.noise_arrived = Some(value.to_owned());
        } else if is("startclosesound") {
            self.noise_moving_closed = Some(value.to_owned());
        } else if is("closesound") {
            self.noise_arrived_closed = Some(value.to_owned());
        } else if is("locked_sentence")
            || is("unlocked_sentence")
            || is("locked_sound")
            || is("unlocked_sound")
        {
            // `m_bLockedSentence` and friends index a sentence table that does
            // not exist. Consumed so the count is honest; 621 doors set them
            // and every value in the shipped game is 0.
        } else {
            // `BaseClass::KeyValue` — `CBaseToggle`'s. The composition that
            // replaces the chain walk (`rustdocs/SERVER.md` gotcha 13).
            return self.toggle.key_value(key, value);
        }
        true
    }

    /// `CBaseDoor::Spawn` (`doors.cpp:229`) plus `CRotDoor::Spawn` (`:1317`).
    ///
    /// The two are one function here because `CRotDoor::Spawn` begins with
    /// `BaseClass::Spawn()` and the base's own work is already branched on
    /// `IsRotatingDoor()`.
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult {
        // `AngleVectors( angMoveDir, &m_vecMoveDir )`.
        self.move_dir = move_dir(self.move_dir);

        // `if ( GetMoveParent() && GetRootMoveParent()->GetSolid() == SOLID_BSP )
        //     SetSolid( SOLID_BSP ); else SetSolid( SOLID_VPHYSICS );`
        // (`doors.cpp:233`).
        //
        // > **This is not `CBaseTrigger::InitTrigger`'s rule and it is very
        // > nearly its opposite.** A trigger is `GetParent() ? SOLID_VPHYSICS
        // > : SOLID_BSP`; a door is `SOLID_VPHYSICS` *unless* it is parented
        // > to a hierarchy whose root is `SOLID_BSP`. Stage 3 ported the
        // > trigger's rule here, when the root-parent walk did not exist and
        // > the two names were interchangeable — and they were, until
        // > [`push`](crate::server::push) made
        // > `ComputeRotationalPushDirection` branch on this exact test.
        // > **87 of the game's 621 doors name a parent**, and the walk they
        // > need is [`root_move_parent`](crate::server::hierarchy::root_move_parent),
        // > which the transform pair added.
        //
        // The root is walked from the *parent* rather than from this entity,
        // because this entity is not in the list while its own `Spawn` runs.
        entity.solid = match entity.parent() {
            Some(parent) => {
                let root = crate::server::hierarchy::root_move_parent(parent, cx.entities());
                match cx.entity(root).map(|root| root.core.solid) {
                    Some(Solid::Bsp) => Solid::Bsp,
                    _ => Solid::VPhysics,
                }
            }
            None => Solid::VPhysics,
        };
        entity.move_type = MoveType::Push;
        // "Don't allow zero or negative speeds" is `CFuncMoveLinear`'s wording;
        // a door's is `if (m_flSpeed == 0) m_flSpeed = 100`.
        if entity.speed == 0.0 {
            entity.speed = 100.0;
        }
        if entity.has_spawn_flags(SF_DOOR_LOCKED) {
            self.locked = true;
        }

        // The two solidity spawnflags, which decide whether this door is a
        // wall at all — and, since [`push`](crate::server::push) landed,
        // whether it shoves the player. The `func_water` guard around them in
        // the C++ is moot: `func_water` is `CBaseDoor`'s other classname and
        // this port does not register it (no shipped Portal 2 map places one).
        if entity.has_spawn_flags(SF_DOOR_PASSABLE) {
            entity.add_solid_flags(FSOLID_NOT_SOLID);
        }
        if entity.has_spawn_flags(SF_DOOR_NONSOLID_TO_PLAYER) {
            entity.collision_group = CollisionGroup::PassableDoor;
            entity.flags |= FL_UNBLOCKABLE_BY_PLAYER;
        }

        self.toggle.position1 = entity.local_origin;

        // > **The travel is the model's own size along `movedir`, less the
        // > lip.** `vecOBB -= Vector(2,2,2)` because "the engine expands
        // > bboxes by 1 in all directions"; keep the 2 and a door that fills
        // > its doorway exactly still clears it.
        let (mins, maxs) = (entity.model_bounds.mins, entity.model_bounds.maxs);
        let mut obb = maxs - mins;
        if entity.local_angles != Vec3::ZERO {
            // `RotateAABB`: a door placed by a Hammer instance arrives
            // pre-rotated, and its travel has to be measured in the turned
            // frame. 88 `func_door`s and 56 `func_door_rotating`s are.
            obb = rotate_aabb(entity.local_angles, mins, maxs);
        }
        obb -= Vec3::splat(2.0);
        self.toggle.position2 = self.toggle.position1
            + self.move_dir * (dot_product_abs(self.move_dir, obb) - self.toggle.lip);

        if !self.rotating {
            if self.spawn_position == FUNC_DOOR_SPAWN_OPEN {
                entity.set_local_origin(self.toggle.position2);
                self.toggle.state = ToggleState::AtTop;
            } else {
                self.toggle.state = ToggleState::AtBottom;
            }
            return SpawnResult::Ok;
        }

        // `CRotDoor::Spawn`'s half.
        self.toggle.axis_dir(entity.spawn_flags);
        if entity.has_spawn_flags(SF_DOOR_ROTATE_BACKWARDS) {
            self.toggle.move_ang = -self.toggle.move_ang;
        }
        self.toggle.angle1 = entity.local_angles;
        self.toggle.angle2 = entity.local_angles + self.toggle.move_ang * self.toggle.move_distance;

        // > **`CRotDoor::Spawn` spawns open through `Teleport`, which sets the
        // > *absolute* angles — and `m_vecAngle2` is a *local* one.** Its
        // > linear sibling four lines up uses `UTIL_SetOrigin`, which is
        // > `SetLocalOrigin`, so the two halves of the same `Spawn` disagree
        // > about which frame their destination is in. It is a bug and it is
        // > reproduced: **3 of the game's 63 parented `func_door_rotating`s
        // > spawn open**, and for those three the shipped game leaves the door
        // > at an angle that is its intended one read in the wrong frame, with
        // > a local angle that no longer matches `m_vecAngle2`. Fixing it here
        // > would move three doors the shipped game does not move.
        if self.spawn_position == FUNC_DOOR_SPAWN_OPEN {
            entity.set_abs_angles(self.toggle.angle2);
            self.toggle.state = ToggleState::AtTop;
        } else {
            self.toggle.state = ToggleState::AtBottom;
        }
        SpawnResult::Ok
    }

    /// `CBaseDoor::StartBlocked` (`doors.cpp:1141`) — which of the two
    /// blocked outputs, decided by which way the door was going.
    ///
    /// `TS_GOING_DOWN` is closing; everything else — including a door blocked
    /// while standing still, which cannot happen — is opening.
    fn start_blocked(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        let output = match self.toggle.state {
            ToggleState::GoingDown => "OnBlockedClosing",
            _ => "OnBlockedOpening",
        };
        // `m_OnBlockedClosing.FireOutput( pOther, this )` — the **blocker** is
        // the activator, which is what makes `!activator` in the chain resolve
        // to whoever is standing in the doorway.
        let me = Some(entity.id());
        entity.fire_output(output, Variant::Void, Some(other), me, 0.0, cx);
    }

    /// `CBaseDoor::EndBlocked` (`doors.cpp:1242`).
    ///
    /// The mirror image, and note that here the *door* is the activator:
    /// `m_OnUnblockedClosing.FireOutput( this, this )`. By the time it fires
    /// there is no blocker to name.
    fn end_blocked(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let output = match self.toggle.state {
            ToggleState::GoingDown => "OnUnblockedClosing",
            _ => "OnUnblockedOpening",
        };
        let me = Some(entity.id());
        entity.fire_output(output, Variant::Void, me, me, 0.0, cx);
    }

    /// `CBaseDoor::Blocked` (`doors.cpp:1161`) — hurt the blocker, then turn
    /// round.
    ///
    /// # A door with a negative `wait` does not turn round
    ///
    /// "if a door has a negative wait, it would never come back if blocked, so
    /// let it just squash the object to death real fast" — and that is not an
    /// edge case in Portal 2: **503 of the game's 621 doors have `wait < 0`**
    /// (242 rotating, 261 plain), because a chamber door that opens and stays
    /// open is written `wait -1`. So for four doors in five, being blocked
    /// means the door keeps pushing and the player keeps being shoved,
    /// which is exactly what the shipped game does.
    ///
    /// `m_bForceClosed` returns even earlier — 87 doors set it — and skips the
    /// group below as well.
    ///
    /// # What is not here
    ///
    /// **`GetDoorMovementGroup`**, the `m_bDoorGroup` block: a blocked door
    /// reaches into every *other* door sharing its `targetname` (itself
    /// excluded, `doors.cpp:1106`), copies its own origin onto the ones
    /// travelling in the same direction at the same speed, and reverses all of
    /// them. Valve's own comment on the middle of it is *"this is the most
    /// hacked, evil, bastardized thing I've ever seen. kjb"*. It needs a
    /// handler to run another entity's `DoorGoUp` **and** to write that
    /// entity's origin and velocity, which is the cross-entity dispatch this
    /// port defers through [`Context`](crate::server::class::Context) and
    /// which a queued input cannot express.
    ///
    /// It is **reachable content**, not a measured-out branch: 121 of the
    /// game's 621 doors share a `targetname` with another door and **48 of
    /// those have `wait >= 0`**, so the group loop's own guard would let them
    /// through. What is missing is a double door where blocking one leaf
    /// reopens the other; blocking one leaf still reopens *that* leaf.
    ///
    /// **`EntityPhysics_CreateSolver`**, the `vphysics` escape hatch for a
    /// prop a force-closed door cannot damage.
    fn blocked(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        // "Hurt the blocker a little." Zero on every door in the shipped game
        // — `blockdamage` is set on six `func_movelinear`s and nothing else —
        // so this is the reference's shape rather than live content.
        if self.block_damage != 0.0 {
            let me = Some(entity.id());
            cx.take_damage(other, DamageInfo::new(me, me, self.block_damage, DMG_CRUSH));
        }

        // "If we're set to force ourselves closed, keep going."
        if self.force_closed {
            return;
        }

        if self.toggle.wait < 0.0 {
            return;
        }

        match self.toggle.state {
            ToggleState::GoingDown => self.go_up(entity, cx),
            _ => self.go_down(entity, cx),
        }
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Open") {
            // `InputOpen` (`doors.cpp:783`).
            if self.toggle.state != ToggleState::AtTop
                && self.toggle.state != ToggleState::GoingUp
                && !self.locked
            {
                self.activator = input.activator;
                self.go_up(entity, cx);
            }
        } else if is("Close") {
            // `InputClose` (`:762`) — note it does *not* test `m_bLocked`.
            if self.toggle.state != ToggleState::AtBottom {
                self.activator = input.activator;
                self.go_down(entity, cx);
            }
        } else if is("Toggle") {
            // `InputToggle` (`:801`).
            if !self.locked {
                self.activator = input.activator;
                match self.toggle.state {
                    ToggleState::AtBottom => self.go_up(entity, cx),
                    ToggleState::AtTop => self.go_down(entity, cx),
                    _ => {}
                }
            }
        } else if is("Lock") {
            self.locked = true;
        } else if is("Unlock") {
            self.locked = false;
        } else if is("SetSpeed") {
            // `InputSetSpeed` (`:829`). It does **not** restart the move in
            // progress, unlike `CFuncMoveLinear`'s — so a door told to speed
            // up mid-swing finishes at the old speed and uses the new one
            // next time. Valve's, and the 47 connections that fire it all do
            // so with the door shut.
            entity.speed = input.value.float();
        } else {
            return false;
        }
        true
    }

    /// `CBaseToggle::MoveDone` (snap, stop, disarm) and then `m_pfnMoveDone`.
    fn move_done(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.toggle.move_done(entity);
        self.run_move_done(entity, cx);
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let v = |v: Vec3| format!("{:.1} {:.1} {:.1}", v.x, v.y, v.z);
        let mut out = vec![
            ("state", format!("{:?}", self.toggle.state)),
            ("rotating", self.rotating.to_string()),
            ("locked", self.locked.to_string()),
            ("wait", self.toggle.wait.to_string()),
            ("move done", format!("{:?}", self.move_done)),
        ];
        match self.rotating {
            true => {
                out.push(("angle1", v(self.toggle.angle1)));
                out.push(("angle2", v(self.toggle.angle2)));
            }
            false => {
                out.push(("position1", v(self.toggle.position1)));
                out.push(("position2", v(self.toggle.position2)));
            }
        }
        out
    }
}

/// `RotateAABB` (`mathlib_base.cpp:2050`) reduced to the size of the result.
///
/// `CBaseDoor::Spawn` builds a rotated box only to take its extent, so the
/// centre and the corners are both thrown away: what is left is the row sums
/// of `|R|` against the half-extents, doubled. The projection of a box onto an
/// axis is the same whether it is computed corner by corner or this way, which
/// is why there is no eight-way loop here.
fn rotate_aabb(angles: Vec3, mins: Vec3, maxs: Vec3) -> Vec3 {
    let m = crate::math::angle_matrix(angles);
    let half = (maxs - mins) * 0.5;
    let abs = |v: Vec3| Vec3::new(v.x.abs(), v.y.abs(), v.z.abs());
    // Column `i` of `m` is where basis vector `i` lands, so the extent along
    // world axis `j` is the sum over `i` of `|m[i][j]| * half[i]`.
    let (x, y, z) = (abs(m.x_axis), abs(m.y_axis), abs(m.z_axis));
    (x * half.x + y * half.y + z * half.z) * 2.0
}

// ---------------------------------------------------------------------------
// func_movelinear
// ---------------------------------------------------------------------------

/// `SF_MOVELINEAR_NOTSOLID` (`func_movelinear.cpp:21`) — 92 of the game's 196.
const SF_MOVELINEAR_NOTSOLID: u32 = 8;

/// `CFuncMoveLinear` (`game/server/func_movelinear.cpp`) — 196 entities: the
/// pistons, the monitor covers and the lift doors.
///
/// A door with no cycle and no wait: it has an `Open` end and a `Close` end
/// and it goes wherever it is told, including to a fraction between them
/// (`SetPosition`). The one thing it has that a door does not is
/// `startposition`, which says where along that line the map placed it — so
/// `position1` is computed *backwards* from the origin rather than being it.
pub struct MoveLinear {
    toggle: Toggle,
    /// `m_vecMoveDir`, as a direction after `Spawn`.
    move_dir: Vec3,
    /// `m_flStartPosition` — 0 at the closed end, 1 at the open end. 10 of the
    /// game's 196 spawn at 1 and one at 0.5.
    start_position: f32,
    /// `m_flMoveDistance`. **Not [`Toggle::move_distance`]**: `CFuncMoveLinear`
    /// redeclares the field under its own `MoveDistance` key, shadowing
    /// `CBaseToggle::m_flMoveDistance` and its `distance` key. Valve's
    /// shadowing, and both names appear in the FGD.
    move_distance: f32,
    /// `m_flBlockDamage` — `BlockDamage`. Read, and acted on by nothing.
    block_damage: f32,
    sound_start: Option<String>,
    sound_stop: Option<String>,
}

pub static MOVELINEAR_KEYS: &[&str] = &[
    "movedir",
    "startposition",
    "movedistance",
    "blockdamage",
    "startsound",
    "stopsound",
    "lip",
    "wait",
    "distance",
];

pub static MOVELINEAR_INPUTS: InputDefs = &[
    InputDef::new("Open", FieldType::Void),
    InputDef::new("Close", FieldType::Void),
    InputDef::new("SetPosition", FieldType::Float),
    InputDef::new("SetSpeed", FieldType::Float),
];

impl MoveLinear {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(MoveLinear {
            toggle: Toggle::default(),
            move_dir: Vec3::ZERO,
            start_position: 0.0,
            move_distance: 0.0,
            block_damage: 0.0,
            sound_start: None,
            sound_stop: None,
        })
    }

    /// `CFuncMoveLinear::MoveTo` (`func_movelinear.cpp:196`), minus the sound.
    fn move_to(&mut self, entity: &mut EntityCore, dest: Vec3, speed: f32, cx: &mut Context<'_>) {
        if speed == 0.0 {
            return;
        }
        if !self.toggle.linear_move(entity, dest, speed) {
            Behaviour::move_done(self, entity, cx);
        }
        // `SetThink(NULL)` — the sound-stopping think, which does not exist.
        entity.set_next_think(NEVER_THINK, cx);
    }
}

impl Behaviour for MoveLinear {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);

        if is("movedir") {
            self.move_dir = crate::server::keyvalue::string_to_vector(value);
        } else if is("startposition") {
            self.start_position = atof(value);
        } else if is("movedistance") {
            self.move_distance = atof(value);
        } else if is("blockdamage") {
            self.block_damage = atof(value);
        } else if is("startsound") {
            self.sound_start = Some(value.to_owned());
        } else if is("stopsound") {
            self.sound_stop = Some(value.to_owned());
        } else {
            return self.toggle.key_value(key, value);
        }
        true
    }

    /// `CFuncMoveLinear::Spawn` (`func_movelinear.cpp:74`).
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        self.move_dir = move_dir(self.move_dir);
        entity.move_type = MoveType::Push;
        // `SetSolid( SOLID_VPHYSICS )` (`func_movelinear.cpp:111`), and 92 of
        // the game's 196 then take it back out again.
        entity.solid = Solid::VPhysics;
        if entity.has_spawn_flags(SF_MOVELINEAR_NOTSOLID) {
            entity.solid_flags |= FSOLID_NOT_SOLID;
        }

        if entity.speed <= 0.0 {
            entity.speed = 100.0;
        }

        // "If move distance is set to zero, use the width of the brush."
        if self.move_distance <= 0.0 {
            let (mins, maxs) = (entity.model_bounds.mins, entity.model_bounds.maxs);
            let obb = (maxs - mins) - Vec3::splat(2.0);
            self.move_distance = dot_product_abs(self.move_dir, obb) - self.toggle.lip;
        }

        // The origin is where the mapper *drew* it, which is `start_position`
        // of the way along — so the closed end is behind it.
        self.toggle.position1 =
            entity.local_origin - self.move_dir * self.move_distance * self.start_position;
        self.toggle.position2 = self.toggle.position1 + self.move_dir * self.move_distance;
        self.toggle.set_final_dest(entity.local_origin);

        SpawnResult::Ok
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Open") {
            if entity.local_origin != self.toggle.position2 {
                let (dest, speed) = (self.toggle.position2, entity.speed);
                self.move_to(entity, dest, speed, cx);
            }
        } else if is("Close") {
            if entity.local_origin != self.toggle.position1 {
                let (dest, speed) = (self.toggle.position1, entity.speed);
                self.move_to(entity, dest, speed, cx);
            }
        } else if is("SetPosition") {
            // `SetPosition` (`:303`) — a fraction of the way along, and it
            // refuses a move shorter than a thousandth of a unit.
            let target = self.toggle.position1
                + input.value.float() * (self.toggle.position2 - self.toggle.position1);
            if (target - entity.local_origin).length() > 0.001 {
                let speed = entity.speed;
                self.move_to(entity, target, speed, cx);
            }
        } else if is("SetSpeed") {
            // `InputSetSpeed` (`:373`) — unlike a door's, this one **restarts
            // the move in progress** at the new speed, and a speed of zero is
            // turned into a stop by aiming at where the entity already is.
            entity.speed = input.value.float();
            let dest = self.toggle.final_dest();
            if (dest - entity.local_origin).length_squared() > f32::EPSILON * f32::EPSILON {
                if entity.speed.abs() > f32::EPSILON {
                    let speed = entity.speed;
                    let _ = self.toggle.linear_move(entity, dest, speed);
                } else {
                    entity.speed = 1.0;
                    let here = entity.local_origin;
                    let _ = self.toggle.linear_move(entity, here, 1.0);
                }
            }
        } else {
            return false;
        }
        true
    }

    /// `CFuncMoveLinear::MoveDone` (`func_movelinear.cpp:264`).
    ///
    /// > **The origin is tested *after* the snap**, because
    /// > `BaseClass::MoveDone()` runs first and is what puts the entity
    /// > exactly on `position1` or `position2`. Test before it and a mover
    /// > that is one ten-thousandth short fires neither output.
    fn move_done(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.toggle.move_done(entity);

        let me = Some(entity.id());
        if entity.local_origin == self.toggle.position2 {
            entity.fire_output("OnFullyOpen", Variant::Void, me, me, 0.0, cx);
        } else if entity.local_origin == self.toggle.position1 {
            entity.fire_output("OnFullyClosed", Variant::Void, me, me, 0.0, cx);
        }
    }

    /// `CFuncMoveLinear::Use` — **`USE_SET` only**, which an I/O `Use` almost
    /// never is; see [`UseType`].
    fn use_entity(
        &mut self,
        entity: &mut EntityCore,
        use_type: UseType,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) {
        if use_type != UseType::Set {
            return;
        }
        // The value a momentary button would pass. There is no momentary
        // button, and `InputUse` passes 0, so this is the closed end.
        let value = input.value.float().min(1.0);
        let target =
            self.toggle.position1 + value * (self.toggle.position2 - self.toggle.position1);
        let speed = (target - entity.local_origin).length() * 10.0;
        self.move_to(entity, target, speed, cx);
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let v = |v: Vec3| format!("{:.1} {:.1} {:.1}", v.x, v.y, v.z);
        vec![
            ("position1", v(self.toggle.position1)),
            ("position2", v(self.toggle.position2)),
            ("movedistance", self.move_distance.to_string()),
            ("startposition", self.start_position.to_string()),
            ("blockdamage", self.block_damage.to_string()),
        ]
    }

    /// `CFuncMoveLinear::Blocked` (`func_movelinear.cpp:355`) — "hurt the
    /// blocker", and nothing else.
    ///
    /// **A `func_movelinear` does not reverse.** A blocked piston keeps
    /// pushing for as long as the thing in front of it survives, which is what
    /// a Portal 2 crusher is. `blockdamage` is set on **6 of the game's 196**
    /// and is the only live block damage in the shipped content; the other 190
    /// push without hurting.
    ///
    /// Valve's `DAMAGE_EVENTS_ONLY` branch removes a `gib`, which is a
    /// classname this port does not have.
    /// `CFuncMoveLinear::Blocked` (`func_movelinear.cpp:355`) — guarded, unlike
    /// `func_rotating`'s. Its `DAMAGE_EVENTS_ONLY` arm removes a `"gib"`; there
    /// are no gibs here and no class sets that `m_takedamage`.
    fn blocked(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        if self.block_damage == 0.0 {
            return;
        }
        let me = Some(entity.id());
        cx.take_damage(other, DamageInfo::new(me, me, self.block_damage, DMG_CRUSH));
    }
}

// ---------------------------------------------------------------------------
// func_button
// ---------------------------------------------------------------------------

/// `SF_BUTTON_DONTMOVE` (`buttons.cpp:24`) — 53 of the game's 64. A button
/// that fires without travelling anywhere, which is what a Portal 2 pedestal
/// button's *brush* is; the visible part is a `prop_button` model.
const SF_BUTTON_DONTMOVE: u32 = 1;
/// `SF_BUTTON_TOGGLE` — 24. Stays in until pressed again.
const SF_BUTTON_TOGGLE: u32 = 32;
/// `SF_BUTTON_LOCKED` — 13.
const SF_BUTTON_LOCKED: u32 = 2048;
/// `SF_BUTTON_NOTSOLID` (`buttons.cpp:33`) — **zero of the game's 64 set it**,
/// which is why `CBaseButton::Spawn`'s `SOLID_NONE` branch is unreachable in
/// Portal 2. Ported because it is the one place a class chooses `SOLID_NONE`
/// and the choice is otherwise invisible.
const SF_BUTTON_NOTSOLID: u32 = 16384;

/// `CBaseButton::BUTTON_CODE` (`buttons.h`) — which of the three inputs asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonCode {
    /// `Press` — in if out, out if in.
    Press,
    /// `PressIn` — `BUTTON_ACTIVATE`.
    Activate,
    /// `PressOut` — `BUTTON_RETURN`.
    Return,
}

/// `CBaseButton` (`game/server/buttons.cpp`) — 64 entities.
///
/// The same two-position cycle as a door, with `OnPressed`/`OnIn`/`OnOut` in
/// place of `OnOpen`/`OnFullyOpen`/`OnFullyClosed` and a `wait` that returns
/// it on the **think** schedule rather than on the alarm — which is the one
/// place in this module where the two timers are used for the same job by
/// different classes.
pub struct Button {
    toggle: Toggle,
    move_dir: Vec3,
    locked: bool,
    /// `m_fStayPushed` — set from `m_flWait == -1` in `Spawn`.
    stay_pushed: bool,
    /// `m_sounds`. A number that indexes `MakeButtonSound`'s table, or — on 10
    /// of the game's 64 — a sound *name*, which `atoi` reads as 0.
    sounds: i32,
    activator: Option<EntityId>,
    move_done: ButtonMoveDone,
    think: ButtonThink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonMoveDone {
    None,
    /// `SetMoveDone( &CBaseButton::TriggerAndWait )`.
    TriggerAndWait,
    /// `SetMoveDone( &CBaseButton::ButtonBackHome )`.
    BackHome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonThink {
    None,
    /// `SetThink( &CBaseButton::ButtonReturn )`.
    Return,
}

pub static BUTTON_KEYS: &[&str] = &[
    "movedir",
    "sounds",
    "locked_sound",
    "unlocked_sound",
    "locked_sentence",
    "unlocked_sentence",
    "min_use_angle",
    "lip",
    "wait",
    "distance",
];

pub static BUTTON_INPUTS: InputDefs = &[
    InputDef::new("Lock", FieldType::Void),
    InputDef::new("Unlock", FieldType::Void),
    InputDef::new("Press", FieldType::Void),
    InputDef::new("PressIn", FieldType::Void),
    InputDef::new("PressOut", FieldType::Void),
];

pub static BUTTON_OUTPUTS: &[&str] = &["OnDamaged", "OnPressed", "OnUseLocked", "OnIn", "OnOut"];

impl Button {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(Button {
            toggle: Toggle::default(),
            move_dir: Vec3::ZERO,
            locked: false,
            stay_pushed: false,
            sounds: 0,
            activator: None,
            move_done: ButtonMoveDone::None,
            think: ButtonThink::None,
        })
    }

    /// `CBaseButton::Press` (`buttons.cpp:218`).
    fn press(&mut self, entity: &mut EntityCore, code: ButtonCode, cx: &mut Context<'_>) {
        let state = self.toggle.state;
        let moving = state == ToggleState::GoingUp || state == ToggleState::GoingDown;

        if code == ButtonCode::Press && moving {
            return;
        }
        if code == ButtonCode::Activate
            && (state == ToggleState::GoingUp || state == ToggleState::AtTop)
        {
            return;
        }
        if code == ButtonCode::Return
            && (state == ToggleState::GoingDown || state == ToggleState::AtBottom)
        {
            return;
        }
        if self.locked {
            return;
        }

        let me = Some(entity.id());
        // The C++ condition, transcribed with Valve's precedence: `&&` binds
        // tighter than `||`, so the first clause is "a press while in" and the
        // second is "a return while in or going in".
        let out = (code == ButtonCode::Press && state == ToggleState::AtTop)
            || (code == ButtonCode::Return
                && (state == ToggleState::AtTop || state == ToggleState::GoingUp));

        if out {
            entity.fire_output("OnPressed", Variant::Void, self.activator, me, 0.0, cx);
            self.button_return(entity, cx);
        } else if code == ButtonCode::Press
            || (code == ButtonCode::Activate
                && (state == ToggleState::AtBottom || state == ToggleState::GoingDown))
        {
            entity.fire_output("OnPressed", Variant::Void, self.activator, me, 0.0, cx);
            self.button_activate(entity, cx);
        }
    }

    /// `CBaseButton::ButtonActivate` (`buttons.cpp:671`).
    fn button_activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if self.locked {
            return;
        }
        self.toggle.state = ToggleState::GoingUp;
        self.move_done = ButtonMoveDone::TriggerAndWait;
        // `m_fRotating` is only ever set by `CRotButton`, which is 2 entities
        // in the game and is not implemented; the branch is always linear.
        let (dest, speed) = (self.toggle.position2, entity.speed);
        if !self.toggle.linear_move(entity, dest, speed) {
            Behaviour::move_done(self, entity, cx);
        }
    }

    /// `CBaseButton::TriggerAndWait` (`buttons.cpp:723`).
    fn trigger_and_wait(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if self.locked {
            return;
        }
        self.toggle.state = ToggleState::AtTop;

        if self.stay_pushed || entity.has_spawn_flags(SF_BUTTON_TOGGLE) {
            self.think = ButtonThink::None;
        } else {
            // > **The return is a *think*, not the alarm** — Valve's one
            // > inconsistency with `CBaseDoor`, which uses the alarm for the
            // > same wait. It matters because a think quantises to a tick and
            // > the alarm does not.
            entity.set_next_think(cx.curtime() + self.toggle.wait, cx);
            self.think = ButtonThink::Return;
        }

        let me = Some(entity.id());
        entity.fire_output("OnIn", Variant::Void, self.activator, me, 0.0, cx);
    }

    /// `CBaseButton::ButtonReturn` (`buttons.cpp:768`).
    fn button_return(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.toggle.state = ToggleState::GoingDown;
        self.move_done = ButtonMoveDone::BackHome;
        let (dest, speed) = (self.toggle.position1, entity.speed);
        if !self.toggle.linear_move(entity, dest, speed) {
            Behaviour::move_done(self, entity, cx);
        }
    }

    /// `CBaseButton::ButtonBackHome` (`buttons.cpp:787`).
    fn button_back_home(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.toggle.state = ToggleState::AtBottom;
        let me = Some(entity.id());
        entity.fire_output("OnOut", Variant::Void, self.activator, me, 0.0, cx);
    }

    fn run_move_done(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        match std::mem::replace(&mut self.move_done, ButtonMoveDone::None) {
            ButtonMoveDone::None => {}
            ButtonMoveDone::TriggerAndWait => self.trigger_and_wait(entity, cx),
            ButtonMoveDone::BackHome => self.button_back_home(entity, cx),
        }
    }
}

impl Behaviour for Button {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);

        if is("movedir") {
            self.move_dir = crate::server::keyvalue::string_to_vector(value);
        } else if is("sounds") {
            self.sounds = atoi(value);
        } else if is("locked_sound")
            || is("unlocked_sound")
            || is("locked_sentence")
            || is("unlocked_sentence")
            || is("min_use_angle")
        {
            // Sound-table indices and the `+use` cone. No sound system, and
            // `+use` is stage 4's.
        } else {
            return self.toggle.key_value(key, value);
        }
        true
    }

    /// `CBaseButton::Spawn` (`buttons.cpp:367`).
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        self.move_dir = move_dir(self.move_dir);
        entity.move_type = MoveType::Push;
        // `SF_BUTTON_NOTSOLID` sets **both** `SOLID_NONE` and
        // `FSOLID_NOT_SOLID` (`buttons.cpp:397`), which is belt and braces:
        // either alone would do.
        match entity.has_spawn_flags(SF_BUTTON_NOTSOLID) {
            true => {
                entity.solid = Solid::None;
                entity.solid_flags |= FSOLID_NOT_SOLID;
            }
            false => entity.solid = Solid::Bsp,
        }

        if entity.speed == 0.0 {
            entity.speed = 40.0;
        }
        // > **A button's `wait` of 0 becomes 1**, which is why the alarm bug
        // > that strands four rotating doors cannot reach a button — and 14 of
        // > the game's 64 buttons say `wait 0`.
        if self.toggle.wait == 0.0 {
            self.toggle.wait = 1.0;
        }
        if self.toggle.lip == 0.0 {
            self.toggle.lip = 4.0;
        }

        self.toggle.state = ToggleState::AtBottom;
        self.toggle.position1 = entity.local_origin;

        let (mins, maxs) = (entity.model_bounds.mins, entity.model_bounds.maxs);
        let obb = (maxs - mins) - Vec3::splat(2.0);
        self.toggle.position2 = self.toggle.position1
            + self.move_dir * (dot_product_abs(self.move_dir, obb) - self.toggle.lip);

        // "Is this a non-moving button?"
        if (self.toggle.position2 - self.toggle.position1).length() < 1.0
            || entity.has_spawn_flags(SF_BUTTON_DONTMOVE)
        {
            self.toggle.position2 = self.toggle.position1;
        }

        self.stay_pushed = self.toggle.wait == -1.0;
        if entity.has_spawn_flags(SF_BUTTON_LOCKED) {
            self.locked = true;
        }

        SpawnResult::Ok
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Lock") {
            self.locked = true;
        } else if is("Unlock") {
            self.locked = false;
        } else if is("Press") || is("PressIn") || is("PressOut") {
            let code = match () {
                _ if is("PressIn") => ButtonCode::Activate,
                _ if is("PressOut") => ButtonCode::Return,
                _ => ButtonCode::Press,
            };
            self.activator = input.activator;
            self.press(entity, code, cx);
        } else {
            return false;
        }
        true
    }

    /// `CBaseButton::ButtonReturn`, scheduled by `TriggerAndWait`.
    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if std::mem::replace(&mut self.think, ButtonThink::None) == ButtonThink::Return {
            self.button_return(entity, cx);
        }
    }

    fn move_done(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.toggle.move_done(entity);
        self.run_move_done(entity, cx);
    }

    /// `CBaseButton::ButtonUse` (`buttons.cpp:536`) — `m_pfnUse`, set only
    /// when `SF_BUTTON_USE_ACTIVATES`, which **all 64** of the game's buttons
    /// carry.
    ///
    /// The use type is ignored, which is what saves it from [`UseType`]'s
    /// nonsense value.
    fn use_entity(
        &mut self,
        entity: &mut EntityCore,
        _use_type: UseType,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) {
        let state = self.toggle.state;
        if state == ToggleState::GoingUp || state == ToggleState::GoingDown {
            return;
        }
        if self.locked {
            let me = Some(entity.id());
            entity.fire_output("OnUseLocked", Variant::Void, input.activator, me, 0.0, cx);
            return;
        }
        self.activator = input.activator;
        let me = Some(entity.id());

        if state == ToggleState::AtTop {
            if entity.has_spawn_flags(SF_BUTTON_TOGGLE) {
                entity.fire_output("OnPressed", Variant::Void, self.activator, me, 0.0, cx);
                self.button_return(entity, cx);
            }
        } else {
            entity.fire_output("OnPressed", Variant::Void, self.activator, me, 0.0, cx);
            self.button_activate(entity, cx);
        }
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("state", format!("{:?}", self.toggle.state)),
            ("locked", self.locked.to_string()),
            ("stays pushed", self.stay_pushed.to_string()),
            ("wait", self.toggle.wait.to_string()),
            ("sounds", self.sounds.to_string()),
            (
                "travel",
                format!(
                    "{:.1}",
                    (self.toggle.position2 - self.toggle.position1).length()
                ),
            ),
        ]
    }
}

// ---------------------------------------------------------------------------
// func_rotating
// ---------------------------------------------------------------------------

/// `SF_BRUSH_ROTATE_START_ON` (`util.h:498`) — 18 of the game's 27.
const SF_BRUSH_ROTATE_START_ON: u32 = 1;
/// `SF_BRUSH_ROTATE_BACKWARDS` — 3.
const SF_BRUSH_ROTATE_BACKWARDS: u32 = 2;
/// `SF_BRUSH_ROTATE_Z_AXIS` — 6.
const SF_BRUSH_ROTATE_Z_AXIS: u32 = 4;
/// `SF_BRUSH_ROTATE_X_AXIS` — 7.
const SF_BRUSH_ROTATE_X_AXIS: u32 = 8;
/// `SF_ROTATING_NOT_SOLID` (`bmodels.cpp:21`) — "some special rotating objects
/// are not solid". **19 of the game's 27 `func_rotating`s**, which is most of
/// them: the fake volumetric light cones spin and are walked through.
const SF_ROTATING_NOT_SOLID: u32 = 64;
/// `SF_BRUSH_ACCDCC` (`bmodels.cpp:19`) — 8. Spin up and down rather than
/// snapping to speed.
const SF_BRUSH_ACCDCC: u32 = 16;

/// `CFuncRotating` (`game/server/bmodels.cpp:398`) — 27 entities: the fans,
/// the grinder and the shredder.
///
/// The only mover here that is **not** a `CBaseToggle`. It has no two
/// positions: it spins, and everything interesting about it is how it gets to
/// and from a speed. `m_flSpeed` is its *current* angular rate, which is why
/// [`EntityCore::speed`] is shared state rather than a mover's field.
///
/// The alarm is used as a repeating tick — `SetMoveDoneTime(0.1)` at the end
/// of every spin step — which is Valve using the arrival timer as a metronome
/// and is why `RotateMove` arms it for ten seconds when there is nothing to do.
pub struct Rotating {
    /// `m_vecMoveAng` — the axis, from the spawnflags.
    move_ang: Vec3,
    /// `m_flFanFriction` — `fanfriction / 100`. How fast it spins up.
    fan_friction: f32,
    /// `m_flVolume` — `Volume / 10`, clamped. Sound, read and unused.
    volume: f32,
    /// `m_flTargetSpeed`/`m_flMaxSpeed`.
    target_speed: f32,
    max_speed: f32,
    /// `m_flBlockDamage` — `dmg`.
    block_damage: f32,
    /// `m_bReversed`.
    reversed: bool,
    /// `m_angStart` and `m_bStopAtStartPos`.
    ang_start: Vec3,
    stop_at_start_pos: bool,
    solid_bsp: bool,
    noise_running: Option<String>,
    move_done: RotatingMoveDone,
    think: RotatingThink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RotatingMoveDone {
    None,
    SpinUp,
    SpinDown,
    Reverse,
    Rotate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RotatingThink {
    None,
    /// `SetThink( &CFuncRotating::SUB_CallUseToggle )` — the START_ON
    /// bootstrap, "a magic delay for the client to start up".
    CallUseToggle,
    /// `SetThink( &CFuncRotating::NormalizeAngleIfNeeded )`.
    Normalize,
}

pub static ROTATING_KEYS: &[&str] = &[
    "fanfriction",
    "volume",
    "maxspeed",
    "dmg",
    "message",
    "solidbsp",
];

pub static ROTATING_INPUTS: InputDefs = &[
    InputDef::new("SetSpeed", FieldType::Float),
    InputDef::new("GetSpeed", FieldType::Void),
    InputDef::new("Start", FieldType::Void),
    InputDef::new("Stop", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("Reverse", FieldType::Void),
    InputDef::new("StartForward", FieldType::Void),
    InputDef::new("StartBackward", FieldType::Void),
    InputDef::new("StopAtStartPos", FieldType::Void),
];

pub static ROTATING_OUTPUTS: &[&str] = &["OnGetSpeed"];

impl Rotating {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(Rotating {
            move_ang: Vec3::ZERO,
            fan_friction: 0.0,
            volume: 0.0,
            target_speed: 0.0,
            max_speed: 0.0,
            block_damage: 0.0,
            reversed: false,
            ang_start: Vec3::ZERO,
            stop_at_start_pos: false,
            solid_bsp: false,
            noise_running: None,
            move_done: RotatingMoveDone::None,
            think: RotatingThink::None,
        })
    }

    /// `CFuncRotating::GetNextMoveInterval` (`bmodels.cpp:861`).
    fn next_move_interval(&self, cx: &Context<'_>) -> f32 {
        match self.stop_at_start_pos {
            true => cx.time.interval,
            false => 0.1,
        }
    }

    /// Which euler component the axis uses. `checkAxis` in `UpdateSpeed` and
    /// `RotateMove`, spelled once.
    fn check_axis(&self) -> usize {
        if self.move_ang.x != 0.0 {
            0
        } else if self.move_ang.y != 0.0 {
            1
        } else {
            2
        }
    }

    /// `CFuncRotating::UpdateSpeed` (`bmodels.cpp:910`), minus the sound.
    fn update_speed(&mut self, entity: &mut EntityCore, new_speed: f32) {
        let old_speed = entity.speed;
        entity.speed = new_speed.clamp(-self.max_speed, self.max_speed);

        if self.stop_at_start_pos {
            let axis = self.check_axis();
            let mut delta = anglemod(entity.local_angles[axis] - self.ang_start[axis]);
            if delta > 180.0 {
                delta -= 360.0;
            }

            if new_speed < 100.0 {
                if new_speed <= 25.0 && delta.abs() < 1.0 {
                    self.target_speed = 0.0;
                    self.stop_at_start_pos = false;
                    entity.speed = 0.0;
                    entity.set_local_angles(self.ang_start);
                } else if delta.abs() > 90.0 {
                    // "Keep rotating at same speed for now."
                    entity.speed = old_speed;
                } else {
                    let min_speed = delta.abs().max(20.0);
                    entity.speed = match old_speed > 0.0 {
                        true => min_speed,
                        false => -min_speed,
                    };
                }
            }
        }

        entity.angular_velocity = self.move_ang * entity.speed;
    }

    /// `CFuncRotating::SpinUpMove` (`bmodels.cpp:995`).
    fn spin_up_move(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let mut spin_up_done = false;
        let mut new_speed = entity.speed.abs() + 0.2 * self.max_speed * self.fan_friction;
        if new_speed.abs() >= self.target_speed.abs() {
            new_speed = self.target_speed;
            spin_up_done = !self.stop_at_start_pos;
        } else if self.target_speed < 0.0 {
            new_speed = -new_speed;
        }

        self.update_speed(entity, new_speed);

        if spin_up_done {
            self.move_done = RotatingMoveDone::Rotate;
            self.rotate_move(entity, cx);
        }
        // Unconditional, and after `RotateMove` — so a spin-up that just
        // finished overwrites the ten-second alarm `RotateMove` armed with a
        // tenth of a second. Valve's order, kept.
        let interval = self.next_move_interval(cx);
        entity.set_move_done_time(interval);
    }

    /// `CFuncRotating::SpinDown` (`bmodels.cpp:1037`) — returns whether the
    /// target was reached.
    fn spin_down(&mut self, entity: &mut EntityCore, target: f32) -> bool {
        let mut spin_down_done = false;
        let mut new_speed = entity.speed.abs() - 0.1 * self.max_speed * self.fan_friction;
        if new_speed < 0.0 {
            new_speed = 0.0;
        }
        if new_speed.abs() <= target.abs() {
            new_speed = target;
            spin_down_done = !self.stop_at_start_pos;
        } else if entity.speed < 0.0 {
            new_speed = -new_speed;
        }

        self.update_speed(entity, new_speed);
        spin_down_done
    }

    /// `CFuncRotating::RotateMove` (`bmodels.cpp:1114`).
    fn rotate_move(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        entity.set_move_done_time(10.0);

        if !self.stop_at_start_pos {
            return;
        }
        let interval = self.next_move_interval(cx);
        entity.set_move_done_time(interval);

        let axis = self.check_axis();
        let mut delta = anglemod(entity.local_angles[axis] - self.ang_start[axis]);
        if delta > 180.0 {
            delta -= 360.0;
        }
        let per_tick = entity.angular_velocity * cx.time.interval;
        if delta.abs() < per_tick[axis].abs() {
            self.set_target_speed(entity, 0.0, cx);
            entity.set_local_angles(self.ang_start);
            self.stop_at_start_pos = false;
        }
    }

    /// `CFuncRotating::SetTargetSpeed` (`bmodels.cpp:1177`).
    fn set_target_speed(&mut self, entity: &mut EntityCore, speed: f32, cx: &mut Context<'_>) {
        let mut speed = speed.abs();
        if self.reversed {
            speed = -speed;
        }
        self.target_speed = speed;

        if !entity.has_spawn_flags(SF_BRUSH_ACCDCC) {
            let target = self.target_speed;
            self.update_speed(entity, target);
            self.move_done = RotatingMoveDone::Rotate;
        } else if (entity.speed > 0.0 && self.target_speed < 0.0)
            || (entity.speed < 0.0 && self.target_speed > 0.0)
        {
            self.move_done = RotatingMoveDone::Reverse;
        } else if entity.speed.abs() < self.target_speed.abs() {
            self.move_done = RotatingMoveDone::SpinUp;
        } else if entity.speed.abs() > self.target_speed.abs() {
            self.move_done = RotatingMoveDone::SpinDown;
        } else {
            self.move_done = RotatingMoveDone::Rotate;
        }

        let interval = self.next_move_interval(cx);
        entity.set_move_done_time(interval);
    }

    /// `CFuncRotating::RotatingUse` (`bmodels.cpp:1246`).
    fn rotating_use(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        match entity.speed != 0.0 {
            true => self.set_target_speed(entity, 0.0, cx),
            false => self.set_target_speed(entity, self.max_speed, cx),
        }
        self.think = RotatingThink::Normalize;
        entity.set_next_think(cx.curtime() + 0.2, cx);
    }

    /// `CFuncRotating::NormalizeAngleIfNeeded` (`bmodels.cpp:873`).
    ///
    /// A fan left running for an hour accumulates a euler angle in the tens of
    /// thousands of degrees; `m_angRotation` is networked as a fixed-point
    /// value with a limit, so Valve folds it back every fifteen to thirty
    /// seconds. Nothing here is networked, but the fold is also what stops an
    /// `f32` losing its low bits — at 200 turns the angle is 72,000 and the
    /// spacing between representable values is already a hundredth of a degree.
    fn normalize_angle_if_needed(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        /// "Entity max angle is 1000 rotations, so we'll renormalize after
        /// ~200. Note that we expect this to be a multiple of 360."
        const MAX_ANGLE: f32 = 200.0 * 360.0;
        const TIME_TO_RENORMALIZE: f32 = 15.0;

        let mut angle = entity.local_angles;
        for i in 0..3 {
            if angle[i] > MAX_ANGLE {
                angle[i] -= MAX_ANGLE;
            }
            if angle[i] < -MAX_ANGLE {
                angle[i] += MAX_ANGLE;
            }
        }
        if angle != entity.local_angles {
            entity.set_local_angles(angle);
        }

        // "Think at semi-random intervals so func rotatings don't all stack up
        // on the same tick."
        let delay = cx
            .random()
            .float(TIME_TO_RENORMALIZE, TIME_TO_RENORMALIZE * 2.0);
        self.think = RotatingThink::Normalize;
        entity.set_next_think(cx.curtime() + delay, cx);
    }
}

impl Behaviour for Rotating {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);

        if is("fanfriction") {
            self.fan_friction = atof(value) / 100.0;
        } else if is("volume") {
            self.volume = (atof(value) / 10.0).clamp(0.0, 1.0);
        } else if is("maxspeed") {
            self.max_speed = atof(value);
        } else if is("dmg") {
            self.block_damage = atof(value);
        } else if is("message") {
            self.noise_running = Some(value.to_owned());
        } else if is("solidbsp") {
            self.solid_bsp = atoi(value) != 0;
        } else {
            return false;
        }
        true
    }

    /// `CFuncRotating::Spawn` (`bmodels.cpp:611`).
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult {
        if self.volume == 0.0 {
            self.volume = 1.0;
        }
        // "Prevent divide by zero if level designer forgets friction!"
        if self.fan_friction == 0.0 {
            self.fan_friction = 1.0;
        }

        self.move_ang = if entity.has_spawn_flags(SF_BRUSH_ROTATE_Z_AXIS) {
            Vec3::new(0.0, 0.0, 1.0)
        } else if entity.has_spawn_flags(SF_BRUSH_ROTATE_X_AXIS) {
            Vec3::new(1.0, 0.0, 0.0)
        } else {
            Vec3::new(0.0, 1.0, 0.0)
        };
        if entity.has_spawn_flags(SF_BRUSH_ROTATE_BACKWARDS) {
            self.move_ang = -self.move_ang;
        }

        // "Some rotating objects like fake volumetric lights will not be
        // solid" (`bmodels.cpp:677`). The `Remove` branch is the *else*, and
        // it is there because `CFuncRotating` can be re-spawned.
        entity.solid = Solid::VPhysics;
        match entity.has_spawn_flags(SF_ROTATING_NOT_SOLID) {
            true => entity.solid_flags |= FSOLID_NOT_SOLID,
            false => entity.solid_flags &= !FSOLID_NOT_SOLID,
        }
        entity.move_type = MoveType::Push;

        // "Did level designer forget to assign a maximum speed?"
        self.max_speed = self.max_speed.abs();
        if self.max_speed == 0.0 {
            self.max_speed = 100.0;
        }

        // Both branches schedule a think 0.2 s out; only the function differs.
        self.think = match entity.has_spawn_flags(SF_BRUSH_ROTATE_START_ON) {
            true => RotatingThink::CallUseToggle,
            false => RotatingThink::Normalize,
        };
        entity.set_next_think(cx.curtime() + 0.2, cx);

        // "Set speed to 0 in case there's an old 'speed' key lying around."
        entity.speed = 0.0;
        self.ang_start = entity.local_angles;

        SpawnResult::Ok
    }

    /// `CFuncRotating::Blocked` (`bmodels.cpp:1375`) — one line, and
    /// **unguarded**.
    ///
    /// `pOther->TakeDamage( CTakeDamageInfo( this, this, m_flBlockDamage,
    /// DMG_CRUSH ) )` with no `if`, where `CBaseDoor` and `CFuncMoveLinear`
    /// both test the amount first. That difference is preserved: a zero-damage
    /// hit still reaches `OnTakeDamage`, which is observable through a
    /// `filter_damage_type` and through `m_OnHurt`-shaped logic.
    ///
    /// **A blocked fan does not stop.** `CFuncRotating` has no `MoveDone` and
    /// no reversal; the blocker simply stays in the way, and the fan's local
    /// time stops advancing for as long as it does. `dmg` is set on **7 of the
    /// game's 27** `func_rotating`s — the grinders and the shredder, which is
    /// the one place in Portal 2 where standing in a mover kills you through
    /// this path rather than through a `trigger_hurt`.
    /// `CFuncRotating::Blocked` (`bmodels.cpp:1375`) — one line, and **no
    /// `if ( m_flBlockDamage )` guard**, where its linear sibling four files
    /// away has one. The asymmetry is Valve's and is kept: a `dmg` of zero
    /// still runs the victim's `OnTakeDamage`, which is observable through a
    /// damage filter even when no health moves.
    fn blocked(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        let me = Some(entity.id());
        cx.take_damage(other, DamageInfo::new(me, me, self.block_damage, DMG_CRUSH));
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("SetSpeed") {
            // A **fraction of the maximum**, not a rate: `clamp(|v|, 0, 1) *
            // m_flMaxSpeed`, with the sign taken as the direction.
            self.stop_at_start_pos = false;
            let speed = input.value.float();
            self.reversed = speed < 0.0;
            let target = speed.abs().clamp(0.0, 1.0) * self.max_speed;
            self.set_target_speed(entity, target, cx);
        } else if is("GetSpeed") {
            let me = Some(entity.id());
            let speed = entity.speed.abs();
            entity.fire_output(
                "OnGetSpeed",
                Variant::Float(speed),
                input.activator,
                me,
                0.0,
                cx,
            );
        } else if is("Start") {
            self.stop_at_start_pos = false;
            let max = self.max_speed;
            self.set_target_speed(entity, max, cx);
        } else if is("StartForward") {
            // Valve does **not** clear `m_bStopAtStartPos` here, where every
            // other start does. Reproduced.
            self.reversed = false;
            let max = self.max_speed;
            self.set_target_speed(entity, max, cx);
        } else if is("StartBackward") {
            self.stop_at_start_pos = false;
            self.reversed = true;
            let max = self.max_speed;
            self.set_target_speed(entity, max, cx);
        } else if is("Stop") {
            self.stop_at_start_pos = false;
            self.set_target_speed(entity, 0.0, cx);
        } else if is("StopAtStartPos") {
            self.stop_at_start_pos = true;
            self.set_target_speed(entity, 0.0, cx);
            let interval = self.next_move_interval(cx);
            entity.set_move_done_time(interval);
        } else if is("Toggle") {
            // `m_flSpeed > 0` rather than `!= 0`, so a reversed rotator asked
            // to toggle speeds *up* instead of stopping. Valve's.
            match entity.speed > 0.0 {
                true => self.set_target_speed(entity, 0.0, cx),
                false => {
                    let max = self.max_speed;
                    self.set_target_speed(entity, max, cx)
                }
            }
        } else if is("Reverse") {
            self.stop_at_start_pos = false;
            self.reversed = !self.reversed;
            let speed = entity.speed;
            self.set_target_speed(entity, speed, cx);
        } else {
            return false;
        }
        true
    }

    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        match std::mem::replace(&mut self.think, RotatingThink::None) {
            RotatingThink::None => {}
            // `SUB_CallUseToggle` — `Use( this, this, USE_TOGGLE, 0 )`.
            RotatingThink::CallUseToggle => self.rotating_use(entity, cx),
            RotatingThink::Normalize => self.normalize_angle_if_needed(entity, cx),
        }
    }

    fn move_done(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        // No `CBaseToggle` here: `CFuncRotating` derives straight from
        // `CBaseEntity`, so `MoveDone` is only the function pointer and there
        // is nothing to snap.
        match self.move_done {
            RotatingMoveDone::None => {}
            RotatingMoveDone::SpinUp => self.spin_up_move(entity, cx),
            RotatingMoveDone::SpinDown => {
                // `SpinDownMove` (`bmodels.cpp:1076`).
                if self.spin_down(entity, self.target_speed) {
                    self.move_done = RotatingMoveDone::Rotate;
                    self.rotate_move(entity, cx);
                } else {
                    let interval = self.next_move_interval(cx);
                    entity.set_move_done_time(interval);
                }
            }
            RotatingMoveDone::Reverse => {
                // `ReverseMove` (`bmodels.cpp:1097`).
                if self.spin_down(entity, 0.0) {
                    let target = self.target_speed;
                    self.set_target_speed(entity, target, cx);
                } else {
                    let interval = self.next_move_interval(cx);
                    entity.set_move_done_time(interval);
                }
            }
            RotatingMoveDone::Rotate => self.rotate_move(entity, cx),
        }
    }

    /// `m_pfnUse` — `SetUse( &CFuncRotating::RotatingUse )`, unconditionally,
    /// in `Spawn`. The use type is ignored.
    fn use_entity(
        &mut self,
        entity: &mut EntityCore,
        _use_type: UseType,
        _input: &Input<'_>,
        cx: &mut Context<'_>,
    ) {
        self.rotating_use(entity, cx);
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("maxspeed", self.max_speed.to_string()),
            ("target speed", self.target_speed.to_string()),
            ("reversed", self.reversed.to_string()),
            ("fanfriction", self.fan_friction.to_string()),
            (
                "axis",
                format!(
                    "{:.0} {:.0} {:.0}",
                    self.move_ang.x, self.move_ang.y, self.move_ang.z
                ),
            ),
            ("move done", format!("{:?}", self.move_done)),
        ]
    }
}

// ---------------------------------------------------------------------------
// func_brush
// ---------------------------------------------------------------------------

/// `CFuncBrush::BrushSolidities_e` (`modelentities.h:48`) — the `Solidity` key.
/// 1,873 of the game's 2,502 are `TOGGLE`, 581 `NEVER` and 43 `ALWAYS`.
const BRUSHSOLID_NEVER: i32 = 1;
const BRUSHSOLID_ALWAYS: i32 = 2;

/// `CFuncBrush` (`game/server/modelentities.cpp:20`) — **2,502 entities, the
/// third commonest classname in the game**, and the only one here that does
/// not move.
///
/// It is a piece of world that can be switched off: the panels a test chamber
/// folds away, the player clips around a lift, the walls that appear when a
/// puzzle is solved. `MOVETYPE_PUSH` is set "so it doesn't get pushed by
/// anything" and its velocity is never non-zero.
///
/// This is where **`StartDisabled` comes home**, which
/// `portdocs/SERVER.md` §7.4 promised: 337 of the game's `func_brush`es start
/// switched off, and until this class existed `world/` drew every one of them.
pub struct Brush {
    /// `m_iDisabled` — `StartDisabled`, and then the live state.
    disabled: bool,
    /// `m_iSolidity`.
    solidity: i32,
    solid_bsp: bool,
    /// `m_iszExcludedClass`/`m_bInvertExclusion` — an NPC filter. Read,
    /// settable by input, and consulted by nothing: it is asked about in
    /// `CFuncBrush::IsOn`'s callers inside the AI code, which is the 122,298
    /// lines `portdocs/SERVER.md` §1.5 measured out.
    excluded_class: Option<String>,
    invert_exclusion: bool,
}

pub static BRUSH_KEYS: &[&str] = &[
    "StartDisabled",
    "Solidity",
    "solidbsp",
    "excludednpc",
    "invert_exclusion",
];

pub static BRUSH_INPUTS: InputDefs = &[
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("SetExcluded", FieldType::String),
    InputDef::new("SetInvert", FieldType::Bool),
];

impl Brush {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(Brush {
            disabled: false,
            solidity: 0,
            solid_bsp: false,
            excluded_class: None,
            invert_exclusion: false,
        })
    }

    /// `CFuncBrush::IsOn` — **`!IsEffectActive( EF_NODRAW )`**, not
    /// `!m_iDisabled`. The two are kept in step by `TurnOn`/`TurnOff` and the
    /// effect bit is the authority, which matters because `world/` reads the
    /// same bit to decide whether to draw.
    fn is_on(entity: &EntityCore) -> bool {
        entity.effects & EF_NODRAW == 0
    }

    /// `CFuncBrush::TurnOff` (`modelentities.cpp:172`).
    fn turn_off(&mut self, entity: &mut EntityCore) {
        if !Brush::is_on(entity) {
            return;
        }
        if self.solidity != BRUSHSOLID_ALWAYS {
            entity.solid_flags |= FSOLID_NOT_SOLID;
        }
        entity.effects |= EF_NODRAW;
        self.disabled = true;
    }

    /// `CFuncBrush::TurnOn` (`modelentities.cpp:203`).
    fn turn_on(&mut self, entity: &mut EntityCore) {
        if Brush::is_on(entity) {
            return;
        }
        if self.solidity != BRUSHSOLID_NEVER {
            entity.solid_flags &= !FSOLID_NOT_SOLID;
        }
        entity.effects &= !EF_NODRAW;
        self.disabled = false;
    }
}

impl Behaviour for Brush {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);

        if is("StartDisabled") {
            self.disabled = atoi(value) != 0;
        } else if is("Solidity") {
            self.solidity = atoi(value);
        } else if is("solidbsp") {
            self.solid_bsp = atoi(value) != 0;
        } else if is("excludednpc") {
            self.excluded_class = Some(value.to_owned());
        } else if is("invert_exclusion") {
            self.invert_exclusion = atoi(value) != 0;
        } else {
            return false;
        }
        true
    }

    /// `CFuncBrush::Spawn` (`modelentities.cpp:42`).
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        entity.move_type = MoveType::Push;
        entity.solid = Solid::VPhysics;

        if self.solidity == BRUSHSOLID_NEVER {
            entity.solid_flags |= FSOLID_NOT_SOLID;
        }

        // > **`TurnOff` is called *after* the solidity is already set**, and
        // > `TurnOff` early-outs on `IsOn()` — which is true here, because
        // > nothing has set `EF_NODRAW` yet. So a `Solidity` of `NEVER` plus
        // > `StartDisabled` sets the flag twice, and a `Solidity` of `ALWAYS`
        // > plus `StartDisabled` leaves the brush solid and invisible.
        if self.disabled {
            self.turn_off(entity);
        }

        // "Slam the object back to solid - if we really want it to be solid."
        // The last line of `CFuncBrush::Spawn`, and it runs *after* `TurnOff`,
        // so `solidbsp` on a `StartDisabled` brush restores the solid type and
        // leaves `FSOLID_NOT_SOLID` set. 5 of the game's 2,502 set it.
        if self.solid_bsp {
            entity.solid = Solid::Bsp;
        }

        SpawnResult::Ok
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        _cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Enable") {
            self.turn_on(entity);
        } else if is("Disable") {
            self.turn_off(entity);
        } else if is("Toggle") {
            match Brush::is_on(entity) {
                true => self.turn_off(entity),
                false => self.turn_on(entity),
            }
        } else if is("SetExcluded") {
            self.excluded_class = Some(input.value.to_string());
        } else if is("SetInvert") {
            self.invert_exclusion = input.value.bool();
        } else {
            return false;
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("disabled", self.disabled.to_string()),
            (
                "solidity",
                match self.solidity {
                    BRUSHSOLID_NEVER => String::from("never"),
                    BRUSHSOLID_ALWAYS => String::from("always"),
                    _ => String::from("toggle"),
                },
            ),
            (
                "excludednpc",
                self.excluded_class.clone().unwrap_or_default(),
            ),
        ]
    }
}

// ---------------------------------------------------------------------------
// func_areaportal and func_areaportalwindow
// ---------------------------------------------------------------------------

/// `AREAPORTAL_CLOSED`/`AREAPORTAL_OPEN` (`func_areaportal.cpp:18`).
const AREAPORTAL_OPEN: i32 = 1;

/// `CAreaPortal` (`game/server/func_areaportal.cpp:25`) and
/// `CFuncAreaPortalWindow` (`func_areaportalwindow.cpp`) — **409 entities over
/// 121 of the game's 106 maps**, 206 of the first and 203 of the second.
///
/// By the time it reaches the game it is not a brush entity at all: `vbsp`
/// takes the brush away and leaves a point entity whose whole content is a
/// `portalnumber` matching an `m_PortalKey` in `LUMP_AREAPORTALS`. Opening or
/// closing it re-floods the area graph and changes what the renderer can see
/// through the doorway — see [`Visibility`](crate::engine::world::vis::Visibility).
///
/// **It starts open unless `StartOpen` says otherwise**, because
/// `CAreaPortal`'s constructor sets `m_state = AREAPORTAL_OPEN` and `Precache`
/// pushes that straight down to the engine. 39 of the game's 206
/// `func_areaportal`s start closed.
/// The engine's own array starts all-closed (`cmodel_bsp.cpp:959`) and is
/// opened entity by entity; this port starts it open instead, because the
/// depot holds **922 areaportal records against 409 entities** and the ones
/// with no entity would otherwise never open at all.
///
/// # What the window half does not do
///
/// `CFuncAreaPortalWindow` opens and closes itself by *distance*:
/// `UpdateVisibility` closes the portal when the viewer is further away than
/// `FadeStartDist` so that the fogged pane can stand in for the geometry
/// behind it. That needs a per-view update inside the render loop and the
/// translucent pane itself, neither of which exists here, so a window is a
/// `func_areaportal` that happens to start open and answers the same three
/// inputs. The cost is drawing through a window that the shipped game would
/// have shut — too much, not too little.
pub struct AreaPortal {
    /// `m_portalNumber` — the key into `LUMP_AREAPORTALS`.
    portal_number: i32,
    /// `m_state`.
    state: i32,
}

pub static AREAPORTAL_KEYS: &[&str] = &[
    "portalnumber",
    // `CAreaPortal::KeyValue` (`func_areaportal.cpp:162`), and the only key
    // either class has that changes anything. **39 of the game's 206
    // `func_areaportal`s start closed**; the other 167 say so explicitly and
    // no `func_areaportalwindow` names it at all, which is right —
    // `CFuncAreaPortalWindow` is not a `CAreaPortal` and has no such key.
    "StartOpen",
    // `CFuncAreaPortalWindow`'s, all read and none used — see the type.
    "PortalVersion",
    "FadeStartDist",
    "FadeDist",
    "TranslucencyLimit",
    "BackgroundBModel",
];

pub static AREAPORTAL_INPUTS: InputDefs = &[
    InputDef::new("Open", FieldType::Void),
    InputDef::new("Close", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    // "TODO: obsolete! remove" says the datadesc, and the two are **crossed
    // over**: `TurnOn` closes and `TurnOff` opens (`func_areaportal.cpp:65`).
    InputDef::new("TurnOn", FieldType::Void),
    InputDef::new("TurnOff", FieldType::Void),
];

impl AreaPortal {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(AreaPortal {
            portal_number: 0,
            state: AREAPORTAL_OPEN,
        })
    }

    /// What the engine reads back: the key and whether it is open.
    pub fn state(&self) -> (u16, bool) {
        (
            u16::try_from(self.portal_number).unwrap_or(0),
            self.state == AREAPORTAL_OPEN,
        )
    }
}

impl Behaviour for AreaPortal {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("portalnumber") {
            self.portal_number = atoi(value);
            return true;
        }
        if key.eq_ignore_ascii_case("StartOpen") {
            self.state = match atoi(value) != 0 {
                true => AREAPORTAL_OPEN,
                false => 0,
            };
            return true;
        }
        // The window's own keys — the fade distances and the pane's model.
        // Accepted so that they are not reported unhandled, and unused for the
        // reason in the type's documentation.
        AREAPORTAL_KEYS.iter().any(|k| key.eq_ignore_ascii_case(k))
    }

    /// `CAreaPortal::Spawn` plus `Precache`, which is where `UpdateState`
    /// first tells the engine anything.
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        // `AddEffects( EF_NORECEIVESHADOW | EF_NOSHADOW )`, and nothing else:
        // it has no model, no movement and no solidity.
        entity.solid = Solid::None;
        SpawnResult::Ok
    }

    fn accept_input(
        &mut self,
        _entity: &mut EntityCore,
        input: &Input<'_>,
        _cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Open") || is("TurnOff") {
            self.state = AREAPORTAL_OPEN;
        } else if is("Close") || is("TurnOn") {
            self.state = 0;
        } else if is("Toggle") {
            self.state = match self.state == AREAPORTAL_OPEN {
                true => 0,
                false => AREAPORTAL_OPEN,
            };
        } else {
            return false;
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("portalnumber", self.portal_number.to_string()),
            (
                "state",
                match self.state == AREAPORTAL_OPEN {
                    true => "open".to_owned(),
                    false => "closed".to_owned(),
                },
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RotateAABB` (`mathlib_base.cpp:3646`) reduced to an extent, checked
    /// against the thing it is reduced from.
    ///
    /// The trap is the transpose: the extent along world axis `j` is Valve's
    /// **row** `j` against the half-extents, which is `glam`'s columns read
    /// across. Get it backwards and a door rotated 30° slides the wrong
    /// distance — and slides the *right* distance at 0° and 90°, which is
    /// every case a hand-built test would think to try.
    #[test]
    fn a_rotated_box_is_measured_by_its_projection() {
        let close = |a: Vec3, b: Vec3| assert!((a - b).length() < 1e-4, "{a} vs {b}");

        // No rotation: the size, unchanged.
        let (mins, maxs) = (Vec3::new(-32.0, -4.0, -16.0), Vec3::new(32.0, 4.0, 16.0));
        close(rotate_aabb(Vec3::ZERO, mins, maxs), maxs - mins);

        // A quarter turn about yaw swaps X and Y.
        close(
            rotate_aabb(Vec3::new(0.0, 90.0, 0.0), mins, maxs),
            Vec3::new(8.0, 64.0, 32.0),
        );

        // 45° about yaw: both X and Y become (64 + 8) / sqrt(2).
        let d = (64.0 + 8.0) / 2.0f32.sqrt();
        close(
            rotate_aabb(Vec3::new(0.0, 45.0, 0.0), mins, maxs),
            Vec3::new(d, d, 32.0),
        );

        // And the general case, against the definition: the extent along each
        // world axis is the sum of |rotated basis component| times half-size.
        let angles = Vec3::new(30.0, 45.0, 60.0);
        let m = crate::math::angle_matrix(angles);
        let half = (maxs - mins) * 0.5;
        let by_hand = Vec3::new(
            (m.x_axis.x * half.x).abs() + (m.y_axis.x * half.y).abs() + (m.z_axis.x * half.z).abs(),
            (m.x_axis.y * half.x).abs() + (m.y_axis.y * half.y).abs() + (m.z_axis.y * half.z).abs(),
            (m.x_axis.z * half.x).abs() + (m.y_axis.z * half.y).abs() + (m.z_axis.z * half.z).abs(),
        ) * 2.0;
        close(rotate_aabb(angles, mins, maxs), by_hand);
    }
}
