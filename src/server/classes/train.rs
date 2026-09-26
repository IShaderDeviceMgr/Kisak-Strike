//! Trains: `func_tracktrain` and the `path_track` chain it follows.
//!
//! `game/server/trains.cpp` (`CFuncTrackTrain`, `:1078`–`:2823`) and
//! `game/server/pathtrack.cpp` (`CPathTrack`, all 561 lines). **233 trains and
//! 1,464 path nodes across 64 of the shipped maps**, and the class whose
//! absence broke the most *other* entities: 1,290 implemented entities name a
//! `func_tracktrain` as their `parentname` — the elevators, the cranes, the
//! moving rooms, and the props, triggers and doors that ride on them.
//!
//! ```text
//!   Spawn            ──(next tick)──►  Find       put the train on its first node
//!   StartForward / SetSpeed / MoveToPathNode ─► Start ─► Next
//!   Next, every tick while moving:
//!     LookAhead 0.1 s along the path      where should I be a tenth from now?
//!     velocity   = towards that point     (three blend modes)
//!     angular    = towards its heading    (four orientation modes)
//!     passed a node? ArriveAtNode         InPass → the node's OnPass,
//!                                         OnArrivedAtDestinationNode, node speed
//!     SetMoveDoneTime( 0.1 )              keep the pusher integrating
//!   no node ahead? coast to the end, then DeadEnd
//! ```
//!
//! # The train steers by aiming a tenth of a second ahead
//!
//! Nothing here interpolates along the path. Every tick `Next` asks
//! `LookAhead` where the train would be 0.1 s from now if it followed the
//! nodes, points the velocity at that spot, and lets the pusher integrate it
//! for one tick. A node is "arrived at" when the look-ahead point passes it,
//! which is a tenth of a second *before* the train does — and a train told
//! to stop there (`MoveToPathNode`) keeps its velocity for the 0.1 s of move
//! time it has left and coasts the rest of the way onto the node. That coast
//! is Valve's and it is reproduced: it is what puts a stopping elevator on
//! its floor rather than a tenth of a second short of it.
//!
//! # Every coordinate is local
//!
//! Valve reads `GetLocalOrigin()` on the train and on every node, and asserts
//! that they share a move parent. So does this. 19 shipped nodes and one train
//! are parented; the arithmetic is right for those only when they agree.
//!
//! # What is not here
//!
//! - **Sound.** `MoveSound`, `MovePingSound`, `StartSound`, `StopSound` and
//!   their pitch and timing keys are read and kept for `ent_dump`.
//! - **Player controls** — `func_traincontrols`, `OnControls`, and the
//!   `m_controlMins`/`m_controlMaxs` box. No shipped Portal 2 map places a
//!   `func_traincontrols`, and `Use( USE_SET )` is the only way in.
//! - **`FindPhysicsBlockerForHierarchy`**, the `vphysics` friction snapshot
//!   that an `SF_TRACKTRAIN_UNBLOCKABLE_BY_PLAYER` train uses to find and
//!   ignore a physics object in its way. The pusher here only ever pushes the
//!   player, so there is no physics blocker to find.
//! - **`NearestPath`/`OnRestore`** — the save/restore path, and no save
//!   system.
//! - **`ScriptGetFuturePosition`** — VScript's.
//! - **The alternate-ticks band-aid** in `UpdateTrainVelocity`, which doubles
//!   the velocity when `IsSimulatingOnAlternateTicks()` because a portal on a
//!   train made it simulate every other tick. This port simulates every tick.

use glam::{Mat3, Quat, Vec3};

use crate::math::{angle_matrix, matrix_angles, vector_angles_forward};
use crate::server::class::{
    Behaviour, Context, InputDef, InputDefs, SpawnResult, UseType, NEVER_THINK,
};
use crate::server::damage::{DamageInfo, DMG_CRUSH};
use crate::server::entity::{EntityCore, EntityId};
use crate::server::io::{FieldType, Input, Variant};
use crate::server::keyvalue::{atof, atoi};
use crate::server::movement::{
    MoveType, Solid, FL_ONGROUND, FL_UNBLOCKABLE_BY_PLAYER, FSOLID_NOT_SOLID,
};

// ---------------------------------------------------------------------------
// path_track
// ---------------------------------------------------------------------------

/// `SF_PATH_DISABLED` (`pathtrack.h:19`) — a disabled node ends a path for a
/// train that is moving. Set at run time by `DisablePath`; **no shipped node
/// starts disabled.**
const SF_PATH_DISABLED: u32 = 0x1;
/// `SF_PATH_ALTREVERSE` — the alternate path is taken going *backwards*.
const SF_PATH_ALTREVERSE: u32 = 0x4;
/// `SF_PATH_DISABLE_TRAIN` — a train arriving here loses its controls, which
/// is what lets the node's own `speed` take over.
const SF_PATH_DISABLE_TRAIN: u32 = 0x8;
/// `SF_PATH_TELEPORT` — a train reaching the node *before* this one jumps
/// straight here. **118 of the game's 1,464 nodes**, and the only spawnflag
/// any shipped node carries: it is how an elevator gets from the bottom of one
/// shaft to the top of the next.
const SF_PATH_TELEPORT: u32 = 0x10;
/// `SF_PATH_ALTERNATE` — the switch `EnableAlternatePath` throws. Not a map
/// flag; it lives in the spawnflags because that is where Valve put it.
const SF_PATH_ALTERNATE: u32 = 0x8000;

/// `TrackOrientationType_t` (`pathtrack.h:27`) — how a node says a train
/// passing it should face. 1,335 of the game's nodes face along the path, 90
/// use their own angles and 39 are fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOrientation {
    Fixed,
    FacePath,
    FacePathAngles,
}

impl TrackOrientation {
    fn from_key(value: i32) -> TrackOrientation {
        match value {
            0 => TrackOrientation::Fixed,
            2 => TrackOrientation::FacePathAngles,
            _ => TrackOrientation::FacePath,
        }
    }
}

/// `CPathTrack` (`pathtrack.cpp`) — one node of a train's path.
///
/// A doubly-linked list threaded through the entity list, with an optional
/// branch. `next` is the `target` key resolved; `previous` is written *by the
/// node before*, in its own `Activate`, which is why a node cannot know its
/// predecessor until every node has activated.
///
/// **`speed` is not here**: it is `CBaseEntity::m_flSpeed`, the `speed` key
/// every entity parses, and it is [`EntityCore::speed`]. A train arriving at a
/// node with a non-zero speed and no controls takes that speed.
pub struct PathTrack {
    /// `m_flRadius` — 106 of the 107 nodes that carry the key write `0`.
    radius: f32,
    /// `m_altName` — the `altpath` key. Five shipped nodes have one.
    alt_name: Option<String>,
    orientation: TrackOrientation,
    /// `m_pnext`, `m_pprevious`, `m_paltpath`.
    next: Option<EntityId>,
    previous: Option<EntityId>,
    alt_path: Option<EntityId>,
}

pub static PATH_TRACK_KEYS: &[&str] = &["radius", "altpath", "orientationtype"];

pub static PATH_TRACK_INPUTS: InputDefs = &[
    InputDef::new("InPass", FieldType::Void),
    InputDef::new("EnableAlternatePath", FieldType::Void),
    InputDef::new("DisableAlternatePath", FieldType::Void),
    InputDef::new("ToggleAlternatePath", FieldType::Void),
    InputDef::new("EnablePath", FieldType::Void),
    InputDef::new("DisablePath", FieldType::Void),
    InputDef::new("TogglePath", FieldType::Void),
];

pub static PATH_TRACK_OUTPUTS: &[&str] = &["OnPass"];

impl PathTrack {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(PathTrack {
            radius: 0.0,
            alt_name: None,
            // The constructor's default, and 1,335 of the 1,464 say it anyway.
            orientation: TrackOrientation::FacePath,
            next: None,
            previous: None,
            alt_path: None,
        })
    }

    /// `m_pnext` — the node the `target` key named, once linked.
    #[cfg(test)]
    pub fn next(&self) -> Option<EntityId> {
        self.next
    }

    /// `m_pprevious`.
    #[cfg(test)]
    pub fn previous(&self) -> Option<EntityId> {
        self.previous
    }

    /// `CPathTrack::SetPrevious` (`pathtrack.cpp:419`) — "only set previous if
    /// this isn't my alternate path", tested by *name*.
    fn set_previous(&mut self, previous: EntityId, previous_name: Option<&str>) {
        let is_alt = match (previous_name, self.alt_name.as_deref()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        if !is_alt {
            self.previous = Some(previous);
        }
    }

    /// `CPathTrack::Link` (`pathtrack.cpp:103`).
    ///
    /// > **A node that names itself is warned about and not linked**, and
    /// > Valve's `UTIL_Remove` for it is commented out with a `FIXME` saying
    /// > why. Here the entity being dispatched is not in the list at all, so a
    /// > self-reference comes back as "not found" and is caught by name
    /// > instead.
    fn link(&mut self, entity: &EntityCore, cx: &mut Context<'_>) {
        let me = entity.id();
        let my_name = entity.name.clone();
        if let Some(target) = entity.target.as_deref() {
            if Some(target) == my_name.as_deref() {
                eprintln!(
                    "source-engine: server: path_track ({}) refers to itself as a target",
                    entity.debug_name()
                );
            } else {
                match cx.find_by_name(target) {
                    Some(next) => {
                        // `dynamic_cast<CPathTrack*>` — a target that is not a
                        // node ends the path, silently.
                        if let Some(node) = cx.behaviour_mut::<PathTrack>(next) {
                            node.set_previous(me, my_name.as_deref());
                            self.next = Some(next);
                        }
                    }
                    None => eprintln!("source-engine: server: dead end link: {target}"),
                }
            }
        }

        if let Some(alt) = self.alt_name.clone() {
            if let Some(alt) = cx.find_by_name(&alt) {
                if let Some(node) = cx.behaviour_mut::<PathTrack>(alt) {
                    node.set_previous(me, my_name.as_deref());
                    self.alt_path = Some(alt);
                }
            }
        }
    }
}

impl Behaviour for PathTrack {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("radius") {
            self.radius = atof(value);
        } else if is("altpath") {
            self.alt_name = Some(value.to_owned());
        } else if is("orientationtype") {
            self.orientation = TrackOrientation::from_key(atoi(value));
        } else {
            return false;
        }
        true
    }

    /// `CPathTrack::Spawn` — `SOLID_NONE`, a 16-unit box, and the links
    /// cleared. The box is `UTIL_SetSize` for the debug overlay and nothing
    /// here reads it.
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        entity.solid = Solid::None;
        self.next = None;
        self.previous = None;
        SpawnResult::Ok
    }

    /// `CPathTrack::Activate` — **only a named node links**. An unnamed node
    /// could not be anyone's `target`, but it could still name a `target` of
    /// its own, and Valve skips it anyway.
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if entity.name.is_some() {
            self.link(entity, cx);
        }
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);
        let has_alt = self.alt_path.is_some();

        if is("InPass") {
            // `InputPass` — the train calls this on each node it passes, and
            // the node's own `OnPass` is what 786 shipped connections hang off.
            let me = Some(entity.id());
            entity.fire_output("OnPass", Variant::Void, input.activator, me, 0.0, cx);
        } else if is("EnableAlternatePath") {
            if has_alt {
                entity.spawn_flags |= SF_PATH_ALTERNATE;
            }
        } else if is("DisableAlternatePath") {
            if has_alt {
                entity.spawn_flags &= !SF_PATH_ALTERNATE;
            }
        } else if is("ToggleAlternatePath") {
            if has_alt {
                entity.spawn_flags ^= SF_PATH_ALTERNATE;
            }
        } else if is("EnablePath") {
            entity.spawn_flags &= !SF_PATH_DISABLED;
        } else if is("DisablePath") {
            entity.spawn_flags |= SF_PATH_DISABLED;
        } else if is("TogglePath") {
            entity.spawn_flags ^= SF_PATH_DISABLED;
        } else {
            return false;
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let id = |id: Option<EntityId>| match id {
            Some(id) => format!("#{}", id.slot()),
            None => String::from("-"),
        };
        vec![
            ("next", id(self.next)),
            ("previous", id(self.previous)),
            ("alternate", id(self.alt_path)),
            ("orientation", format!("{:?}", self.orientation)),
            ("radius", self.radius.to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// walking the path — `CPathTrack`'s methods that a *train* calls
// ---------------------------------------------------------------------------
//
// Valve calls these on a `CPathTrack*` from inside `CFuncTrackTrain`. Here the
// train is the entity being dispatched and the nodes are in the list, so they
// are free functions over a `Context` rather than methods on a node: a node is
// read through `cx.entity`, and a node that has been killed simply stops
// resolving — the `NULL` the C++ would have crashed on is a `None`.

/// A node's core and class state, if `id` is still a live `path_track`.
fn node<'c>(cx: &'c Context<'_>, id: EntityId) -> Option<(&'c EntityCore, &'c PathTrack)> {
    let entity = cx.entity(id)?;
    let path = entity.behaviour.downcast_ref::<PathTrack>()?;
    Some((&entity.core, path))
}

/// `GetLocalOrigin()` of a node, or zero for one that has gone.
fn node_origin(cx: &Context<'_>, id: EntityId) -> Vec3 {
    node(cx, id).map_or(Vec3::ZERO, |(core, _)| core.local_origin)
}

/// `CPathTrack::GetNext` (`pathtrack.cpp:397`) — the alternate path while the
/// switch is thrown, unless the alternate is for going backwards.
fn get_next(cx: &Context<'_>, id: EntityId) -> Option<EntityId> {
    let (core, path) = node(cx, id)?;
    let flags = core.spawn_flags;
    if path.alt_path.is_some() && flags & SF_PATH_ALTERNATE != 0 && flags & SF_PATH_ALTREVERSE == 0
    {
        return path.alt_path;
    }
    path.next
}

/// `CPathTrack::GetPrevious` (`pathtrack.cpp:407`).
fn get_previous(cx: &Context<'_>, id: EntityId) -> Option<EntityId> {
    let (core, path) = node(cx, id)?;
    let flags = core.spawn_flags;
    if path.alt_path.is_some() && flags & SF_PATH_ALTERNATE != 0 && flags & SF_PATH_ALTREVERSE != 0
    {
        return path.alt_path;
    }
    path.previous
}

/// `CPathTrack::GetNextInDir`.
fn next_in_dir(cx: &Context<'_>, id: EntityId, forward: bool) -> Option<EntityId> {
    match forward {
        true => get_next(cx, id),
        false => get_previous(cx, id),
    }
}

/// `CPathTrack::ValidPath` (`pathtrack.cpp:343`) — the node, unless it is
/// disabled and the caller asked. **`test` is `LookAhead`'s `move`
/// argument**: a train that is actually moving is stopped by a disabled node,
/// and a train that is only *looking* (to face the right way) sees through it.
fn valid_path(cx: &Context<'_>, id: Option<EntityId>, test: bool) -> Option<EntityId> {
    let id = id?;
    let (core, _) = node(cx, id)?;
    if test && core.spawn_flags & SF_PATH_DISABLED != 0 {
        return None;
    }
    Some(id)
}

/// `CPathTrack::Project` — extend the line from `start` through `end` by
/// `dist` past `end`. What a train looking past the end of its path sees.
fn project(cx: &Context<'_>, start: Option<EntityId>, end: EntityId, origin: &mut Vec3, dist: f32) {
    if let Some(start) = start {
        if node(cx, start).is_none() || node(cx, end).is_none() {
            return;
        }
        let end_origin = node_origin(cx, end);
        let dir = (end_origin - node_origin(cx, start)).normalize_or_zero();
        *origin = end_origin + dir * dist;
    }
}

/// The most nodes `LookAhead` will step through in one call. Not Valve's:
/// a loop of nodes that are all in the same place never uses up any distance,
/// and `CPathTrack::LookAhead` would spin on it for ever. No shipped path has
/// such a loop; this is the bound that makes a hand-built one fail rather
/// than hang.
const LOOK_AHEAD_MAX_STEPS: usize = 4096;

/// `CPathTrack::LookAhead` (`pathtrack.cpp:447`) — walk `dist` units along
/// the path from `origin`, starting at `start`.
///
/// Returns the node the walk ends **past** — the one it last reached, not the
/// one it is heading for — and, as the second value, the one after that
/// (`pNextNext`). `origin` is moved to where the walk ended. A negative `dist`
/// walks backwards.
///
/// > **`None` means "ran out of path", not "no answer"**, and what happens to
/// > `origin` then depends on `moving`. A train that is actually moving
/// > (`moving`) leaves it where the last whole segment put it and the caller
/// > coasts to the end. A train that is only *looking* has the line extended
/// > past the last node by the distance it had left, so that a train at the
/// > end of its path still has something to face.
fn look_ahead(
    cx: &Context<'_>,
    start: EntityId,
    origin: &mut Vec3,
    dist: f32,
    moving: bool,
) -> (Option<EntityId>, Option<EntityId>) {
    let mut current = start;
    let original = dist;
    let mut current_pos = *origin;

    let (mut dist, forward) = match dist < 0.0 {
        true => (-dist, false),
        false => (dist, true),
    };

    let mut steps = 0;
    while dist > 0.0 {
        steps += 1;
        if steps > LOOK_AHEAD_MAX_STEPS {
            return (None, None);
        }

        // "If there is no next path track, or it's disabled, we're done."
        let Some(next) = valid_path(cx, next_in_dir(cx, current, forward), moving) else {
            if !moving {
                project(cx, next_in_dir(cx, current, !forward), current, origin, dist);
            }
            return (None, None);
        };

        let dir = node_origin(cx, next) - current_pos;
        let length = dir.length();

        // "If we are at the next node and there isn't one beyond it, return
        // the next node" — unless we have not moved at all, which is a dead
        // end.
        if length == 0.0 && valid_path(cx, next_in_dir(cx, next, forward), moving).is_none() {
            if dist == original.abs() {
                return (None, None);
            }
            return (Some(next), None);
        }

        // "If we don't hit the next path track within the distance remaining,
        // we're done."
        if length > dist {
            *origin = current_pos + dir * (dist / length);
            return (Some(current), Some(next));
        }

        dist -= length;
        current_pos = node_origin(cx, next);
        current = next;
        *origin = current_pos;
    }

    // "We consumed all of the distance, and exactly landed on a path track."
    (Some(current), next_in_dir(cx, current, forward))
}

/// `CPathTrack::GetOrientation` (`pathtrack.cpp:563`) — which way a train
/// passing this node should face.
///
/// `TrackOrientation_Fixed` is **not** special-cased here, only
/// `FacePathAngles` is: a fixed node answers the path direction like any
/// other, and it is the *train's* orientation type that decides whether to
/// listen. Valve's.
fn node_orientation(cx: &Context<'_>, id: EntityId, forward: bool) -> Vec3 {
    let Some((core, path)) = node(cx, id) else {
        return Vec3::ZERO;
    };
    if path.orientation == TrackOrientation::FacePathAngles {
        return core.local_angles;
    }

    let (prev, next) = match next_in_dir(cx, id, forward) {
        Some(next) => (Some(id), next),
        None => (next_in_dir(cx, id, !forward), id),
    };
    // A lone node, with neither neighbour. Valve dereferences a null pointer
    // here; no shipped path has a single node.
    let Some(prev) = prev else {
        return core.local_angles;
    };
    vector_angles_forward(node_origin(cx, next) - node_origin(cx, prev))
}

// ---------------------------------------------------------------------------
// func_tracktrain
// ---------------------------------------------------------------------------

/// `SF_TRACKTRAIN_NOPITCH` (`trains.h:25`) — 183 of the 233.
const SF_TRACKTRAIN_NOPITCH: u32 = 0x0001;
/// `SF_TRACKTRAIN_NOCONTROL` — 192. "No player control", which is also what
/// lets a node's `speed` set the train's.
const SF_TRACKTRAIN_NOCONTROL: u32 = 0x0002;
/// `SF_TRACKTRAIN_FORWARDONLY` — none shipped.
const SF_TRACKTRAIN_FORWARDONLY: u32 = 0x0004;
/// `SF_TRACKTRAIN_PASSABLE` — 63. Not solid at all.
const SF_TRACKTRAIN_PASSABLE: u32 = 0x0008;
/// `SF_TRACKTRAIN_FIXED_ORIENTATION` — 190. The train never turns; this is
/// the elevators, which travel straight up and down a shaft.
const SF_TRACKTRAIN_FIXED_ORIENTATION: u32 = 0x0010;
/// `SF_TRACKTRAIN_HL1TRAIN` — 23. `SOLID_BSP` rather than `SOLID_VPHYSICS`,
/// which here is a label and nothing more.
const SF_TRACKTRAIN_HL1TRAIN: u32 = 0x0080;
/// `SF_TRACKTRAIN_UNBLOCKABLE_BY_PLAYER` — 88.
const SF_TRACKTRAIN_UNBLOCKABLE_BY_PLAYER: u32 = 0x0200;
/// `SF_TRACKTRAIN_ALLOWROLL` — 14.
const SF_TRACKTRAIN_ALLOWROLL: u32 = 0x0400;

/// How far ahead `Next` looks, in seconds of travel at the current speed.
/// `Next`'s `flSpeed * 0.1` and `SetMoveDoneTime( 0.1 )` are the same tenth.
const LOOK_AHEAD_TIME: f32 = 0.1;

/// `TrainVelocityType_t` (`trains.h:47`) — 176 shipped trains are
/// instantaneous, 53 ease in and out, 4 blend linearly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainVelocity {
    Instantaneous,
    LinearBlend,
    EaseInEaseOut,
}

/// `TrainOrientationType_t` (`trains.h:55`) — 154 shipped trains turn at the
/// nodes, 62 are fixed, 16 blend linearly and one eases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainOrientation {
    Fixed,
    AtPathTracks,
    LinearBlend,
    EaseInEaseOut,
}

/// What `SetThink` last pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrainThink {
    None,
    Find,
    Next,
}

/// What `SetMoveDone` last pointed at. `Next` never arms one — it leaves the
/// arrival alarm as a metronome with nothing on the end — so the only live
/// value is the coast to a dead end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrainMoveDone {
    None,
    DeadEnd,
}

/// `CFuncTrackTrain` (`trains.cpp`) — a brush that drives itself along a
/// chain of `path_track`s.
///
/// **`m_flSpeed` is [`EntityCore::speed`]** — the `speed` key, and signed:
/// negative is backwards. `startspeed` is the *maximum*, which every
/// `StartForward` and every fractional `SetSpeed` is measured against.
pub struct TrackTrain {
    /// `m_ppath` — the node the train is on or has most recently passed.
    path: Option<EntityId>,
    /// `m_length` — the `wheels` key. How far ahead of the origin the train
    /// looks to decide which way to face. 147 shipped trains say 50 and 75
    /// say 0, which means 100.
    length: f32,
    /// `m_height` — how far above the path the origin rides.
    height: f32,
    /// `m_maxSpeed` — the `startspeed` key.
    max_speed: f32,
    /// `m_flBank` — degrees of roll into a turn. Four shipped trains.
    bank: f32,
    /// `m_flBlockDamage` — `dmg`. Ten shipped trains say 20 and one 1000.
    block_damage: f32,
    /// `m_dir` — `1` forwards, `-1` backwards.
    dir: f32,
    /// `m_oldSpeed` — what `Resume` goes back to.
    old_speed: f32,
    /// `m_strPathTarget` — the node `MoveToPathNode` is heading for.
    path_target: Option<String>,
    velocity_type: TrainVelocity,
    orientation_type: TrainOrientation,
    /// `m_bManualSpeedChanges` and the accelerations it enables. **No shipped
    /// train sets the key**, and nothing fires `SetSpeedDirAccel`, so the
    /// accelerating branch is ported against the reference and unreachable.
    manual_speed_changes: bool,
    desired_speed: f32,
    accel_speed: f32,
    decel_speed: f32,
    accel_to_speed: bool,
    /// `m_flVolume` — `volume / 10`. Sound, kept for `ent_dump`.
    volume: f32,
    sounds: TrainSounds,
    think: TrainThink,
    move_done: TrainMoveDone,
}

/// The sound keys. Read and printed; there is no sound system.
#[derive(Default)]
struct TrainSounds {
    move_sound: Option<String>,
    move_ping_sound: Option<String>,
    start_sound: Option<String>,
    stop_sound: Option<String>,
    min_pitch: i32,
    max_pitch: i32,
    min_time: f32,
    max_time: f32,
}

pub static TRACK_TRAIN_KEYS: &[&str] = &[
    "wheels",
    "height",
    "startspeed",
    "bank",
    "dmg",
    "volume",
    "MoveSound",
    "MovePingSound",
    "StartSound",
    "StopSound",
    "MoveSoundMinPitch",
    "MoveSoundMaxPitch",
    "MoveSoundMinTime",
    "MoveSoundMaxTime",
    "velocitytype",
    "orientationtype",
    "ManualSpeedChanges",
    "ManualAccelSpeed",
    "ManualDecelSpeed",
];

pub static TRACK_TRAIN_INPUTS: InputDefs = &[
    InputDef::new("Stop", FieldType::Void),
    InputDef::new("StartForward", FieldType::Void),
    InputDef::new("StartBackward", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("Resume", FieldType::Void),
    InputDef::new("Reverse", FieldType::Void),
    InputDef::new("SetSpeed", FieldType::Float),
    InputDef::new("SetSpeedDir", FieldType::Float),
    InputDef::new("SetSpeedReal", FieldType::Float),
    InputDef::new("SetMaxSpeed", FieldType::Float),
    InputDef::new("SetSpeedDirAccel", FieldType::Float),
    InputDef::new("MoveToPathNode", FieldType::String),
    InputDef::new("TeleportToPathNode", FieldType::String),
    InputDef::new("LockOrientation", FieldType::Void),
    InputDef::new("UnlockOrientation", FieldType::Void),
];

pub static TRACK_TRAIN_OUTPUTS: &[&str] = &["OnStart", "OnNextPoint", "OnArrivedAtDestinationNode"];

impl TrackTrain {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(TrackTrain {
            path: None,
            length: 0.0,
            height: 0.0,
            max_speed: 0.0,
            bank: 0.0,
            block_damage: 0.0,
            dir: 1.0,
            old_speed: 0.0,
            path_target: None,
            // "These defaults match old func_tracktrains."
            velocity_type: TrainVelocity::Instantaneous,
            orientation_type: TrainOrientation::AtPathTracks,
            manual_speed_changes: false,
            desired_speed: 0.0,
            accel_speed: 0.0,
            decel_speed: 0.0,
            accel_to_speed: false,
            volume: 0.0,
            sounds: TrainSounds::default(),
            think: TrainThink::None,
            move_done: TrainMoveDone::None,
        })
    }

    /// `m_ppath` — the node the train is on or last passed.
    #[cfg(test)]
    pub fn path(&self) -> Option<EntityId> {
        self.path
    }

    /// `m_height`.
    #[cfg(test)]
    pub fn height(&self) -> f32 {
        self.height
    }

    fn is_dir_forward(&self) -> bool {
        self.dir == 1.0
    }

    /// `CFuncTrackTrain::SetDirForward` (`trains.cpp:1277`). Turning round
    /// steps `m_ppath` one node the other way, because "the node I have
    /// passed" is a different node depending on which way I am going.
    fn set_dir_forward(&mut self, forward: bool, cx: &Context<'_>) {
        if forward && self.dir != 1.0 {
            if let Some(prev) = self.path.and_then(|p| get_previous(cx, p)) {
                self.path = Some(prev);
            }
            self.dir = 1.0;
        } else if !forward && self.dir != -1.0 {
            if let Some(next) = self.path.and_then(|p| get_next(cx, p)) {
                self.path = Some(next);
            }
            self.dir = -1.0;
        }
    }

    /// `CFuncTrackTrain::SetSpeed` (`trains.cpp:1556`).
    fn set_speed(&mut self, entity: &mut EntityCore, speed: f32, accel: bool, cx: &mut Context<'_>) {
        self.accel_to_speed = accel;
        let old_speed = entity.speed;

        if accel {
            self.desired_speed = speed.abs() * self.dir;
            if entity.speed == 0.0 && self.desired_speed.abs() > 0.0 {
                // "little push to get us going"
                entity.speed = 0.1;
            }
            self.start(entity, cx);
            return;
        }

        entity.speed = speed.abs() * self.dir;
        if entity.speed != old_speed {
            if entity.speed != 0.0 {
                match old_speed == 0.0 {
                    true => self.start(entity, cx),
                    false => self.next(entity, cx),
                }
            } else {
                self.stop(entity);
            }
        }
    }

    /// `CFuncTrackTrain::Start`.
    fn start(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let me = Some(entity.id());
        entity.fire_output("OnStart", Variant::Void, me, me, 0.0, cx);
        self.next(entity, cx);
    }

    /// `CFuncTrackTrain::Stop` (`trains.cpp:1609`) — both velocities zeroed
    /// and the think function cleared. **The arrival alarm is left alone**,
    /// so the pusher runs out the tenth of a second it had left with a
    /// velocity of zero.
    fn stop(&mut self, entity: &mut EntityCore) {
        entity.velocity = Vec3::ZERO;
        entity.angular_velocity = Vec3::ZERO;
        self.old_speed = entity.speed;
        entity.speed = 0.0;
        // `SetThink( NULL )` — the schedule stays, and runs nothing.
        self.think = TrainThink::None;
    }

    /// `CFuncTrackTrain::Find` (`trains.cpp:2586`) — put the train on the
    /// node its `target` names, facing along the path, and start it if it
    /// has a `speed`.
    fn find(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let Some(target) = entity.target.clone() else {
            return;
        };
        let found = cx.find_by_name(&target);
        self.path = found.filter(|&id| node(cx, id).is_some());
        let Some(path) = self.path else {
            if found.is_some() {
                eprintln!(
                    "source-engine: server: func_track_train must be on a path of path_track ({})",
                    entity.debug_name()
                );
            }
            return;
        };

        let mut next_pos = node_origin(cx, path);
        let mut look = next_pos;
        look_ahead(cx, path, &mut look, self.length, false);
        next_pos.z += self.height;
        look.z += self.height;

        let angles = match entity.has_spawn_flags(SF_TRACKTRAIN_FIXED_ORIENTATION) {
            true => entity.local_angles,
            false => {
                let mut angles = vector_angles_forward(look - next_pos);
                if entity.has_spawn_flags(SF_TRACKTRAIN_NOPITCH) {
                    angles.x = 0.0;
                }
                angles
            }
        };

        // `Teleport( &nextPos, &nextAngles, NULL )` — an *absolute* placement
        // from *local* coordinates, which only agree for an unparented train.
        // Valve's; one shipped train is parented.
        entity.set_abs_placement(next_pos, angles);

        self.arrive_at_node(entity, path, cx);

        if entity.speed != 0.0 {
            self.think = TrainThink::Next;
            entity.set_next_think(cx.curtime() + 0.1, cx);
        }
    }

    /// `CFuncTrackTrain::Next` (`trains.cpp:2314`) — the whole of a moving
    /// train, once a tick. See the module docs for the shape.
    fn next(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if entity.speed == 0.0 {
            return;
        }
        let Some(path) = self.path else {
            // "Lost path."
            entity.speed = 0.0;
            return;
        };

        let speed = entity.speed;
        let mut next_pos = entity.local_origin;
        next_pos.z -= self.height;
        let (mut next, next_next) = look_ahead(cx, path, &mut next_pos, speed * LOOK_AHEAD_TIME, true);

        // "If we're moving towards a dead end, but our desired speed goes in
        // the opposite direction this fixes us from stalling."
        if self.manual_speed_changes && (speed < 0.0) != (self.desired_speed < 0.0) && next.is_none() {
            next = Some(path);
        }

        next_pos.z += self.height;
        let interval = cx.time.interval;

        match next {
            Some(next) => {
                self.update_velocity(entity, cx, Some(next), next_next, next_pos);
                self.update_orientation(entity, cx, Some(next), next_next, interval);

                if Some(next) != self.path {
                    // "We have reached a new path track. Fire its OnPass
                    // output."
                    self.path = Some(next);
                    self.arrive_at_node(entity, next, cx);

                    // "See if we should teleport to the next path track."
                    if let Some(teleport) = get_next(cx, next) {
                        let flags = node(cx, teleport).map_or(0, |(core, _)| core.spawn_flags);
                        if flags & SF_PATH_TELEPORT != 0 {
                            self.teleport_to_path_track(entity, teleport, cx);
                        }
                    }
                }

                let me = Some(entity.id());
                entity.fire_output("OnNextPoint", Variant::Void, Some(next), me, 0.0, cx);

                // `SetThink( Next ); SetMoveDoneTime( 0.1 ); SetNextThink(
                // curtime ); SetMoveDone( NULL )` — think again next tick, and
                // keep the pusher integrating for a tenth of a second in case
                // nothing does.
                self.think = TrainThink::Next;
                entity.set_move_done_time(LOOK_AHEAD_TIME);
                entity.set_next_think(cx.curtime(), cx);
                self.move_done = TrainMoveDone::None;
            }
            None => {
                // "We've reached the end of the path, stop." — then coast the
                // last stretch at the old speed and call `DeadEnd` on arrival.
                entity.velocity = next_pos - entity.local_origin;
                entity.angular_velocity = Vec3::ZERO;
                let distance = entity.velocity.length();
                self.old_speed = entity.speed;
                entity.speed = 0.0;

                if distance > 0.0 {
                    let time = distance / self.old_speed.abs();
                    entity.velocity *= self.old_speed.abs() / distance;
                    self.move_done = TrainMoveDone::DeadEnd;
                    self.think = TrainThink::None;
                    entity.set_next_think(NEVER_THINK, cx);
                    entity.set_move_done_time(time);
                } else {
                    self.dead_end(entity, cx);
                }
            }
        }
    }

    /// `CFuncTrackTrain::ArriveAtNode` (`trains.cpp:1880`).
    ///
    /// > **`OnPass` goes out one tick late**, and it is the one place this
    /// > class diverges on timing. Valve calls `AcceptInput( "InPass" )` on
    /// > the node directly; here the node is another entity and a handler
    /// > cannot run another entity's code, so `InPass` is *posted* with no
    /// > delay. The queue is serviced after the thinks in the same tick, so
    /// > the node's `OnPass` connections are queued in the tick the train
    /// > passed it — what differs is only that an `OnPass` with no delay of
    /// > its own is delivered in that tick's queue pass rather than inside
    /// > the train's think.
    fn arrive_at_node(&mut self, entity: &mut EntityCore, node_id: EntityId, cx: &mut Context<'_>) {
        // `FirePassInputs( pNode, pNode->GetNext(), true )` — with its own
        // "BUGBUG: This is wrong" — which only ever reaches `pNode` itself,
        // because the walk stops at the node after it.
        self.fire_pass_inputs(entity, node_id, get_next(cx, node_id), cx);

        let (flags, node_speed) = node(cx, node_id).map_or((0, 0.0), |(core, _)| (core.spawn_flags, core.speed));
        if flags & SF_PATH_DISABLE_TRAIN != 0 {
            entity.spawn_flags |= SF_TRACKTRAIN_NOCONTROL;
        }

        // "Do we have a node move target?"
        if let Some(target) = self.path_target.clone() {
            if cx.find_by_name(&target) == Some(node_id) {
                let me = Some(entity.id());
                entity.fire_output("OnArrivedAtDestinationNode", Variant::Void, Some(node_id), me, 0.0, cx);
                self.path_target = None;
                self.old_speed = entity.speed;
                entity.speed = 0.0;
                return;
            }
        }

        // "Don't override the train speed if it's under user control" — and
        // don't copy a node speed of zero, which means "not set".
        if entity.has_spawn_flags(SF_TRACKTRAIN_NOCONTROL) && node_speed != 0.0 {
            self.set_speed(entity, node_speed, false, cx);
        }
    }

    /// `CFuncTrackTrain::FirePassInputs` (`trains.cpp:2464`), forwards only —
    /// the one caller passes `true`.
    fn fire_pass_inputs(
        &mut self,
        entity: &EntityCore,
        start: EntityId,
        end: Option<EntityId>,
        cx: &mut Context<'_>,
    ) {
        let me = Some(entity.id());
        let mut current = Some(start);
        let mut steps = 0;
        while let Some(id) = current {
            if Some(id) == end || steps >= LOOK_AHEAD_MAX_STEPS {
                break;
            }
            if node(cx, id).is_none() {
                break;
            }
            cx.post_entity(id, "InPass", Variant::Void, 0.0, me, me);
            current = get_next(cx, id);
            steps += 1;
        }
    }

    /// `CFuncTrackTrain::DeadEnd` (`trains.cpp:2485`) — the train has coasted
    /// to the end of its path.
    ///
    /// "HACKHACK -- This is bugly, but the train can actually stop moving at a
    /// different node depending on its speed so we have to traverse the list
    /// to its end" — so it walks to the last enabled node in the direction it
    /// was going and calls *that* the one it stopped at.
    fn dead_end(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let mut track = self.path;
        if let Some(mut current) = track {
            let backwards = self.old_speed < 0.0;
            let mut steps = 0;
            loop {
                let step = match backwards {
                    true => get_previous(cx, current),
                    false => get_next(cx, current),
                };
                match valid_path(cx, step, true) {
                    Some(next) if steps < LOOK_AHEAD_MAX_STEPS => {
                        current = next;
                        steps += 1;
                    }
                    _ => break,
                }
            }
            track = Some(current);
        }

        entity.velocity = Vec3::ZERO;
        entity.angular_velocity = Vec3::ZERO;
        if let Some(track) = track {
            self.path = Some(track);
            let me = Some(entity.id());
            cx.post_entity(track, "InPass", Variant::Void, 0.0, me, me);

            // "also check to see if we were assigned to move here."
            if let Some(target) = self.path_target.clone() {
                if cx.find_by_name(&target) == Some(track) {
                    entity.fire_output("OnArrivedAtDestinationNode", Variant::Void, Some(track), me, 0.0, cx);
                    self.path_target = None;
                }
            }
        }
    }

    /// `CFuncTrackTrain::TeleportToPathTrack` (`trains.cpp:2284`).
    ///
    /// **The train's `height` is not added**, where `Find` adds it. Valve's;
    /// every shipped train that teleports has a `height` of 0, 1 or 4.
    fn teleport_to_path_track(&mut self, entity: &mut EntityCore, teleport: EntityId, cx: &Context<'_>) {
        let current = entity.local_angles;
        let next_pos = node_origin(cx, teleport);
        let mut look = next_pos;
        look_ahead(cx, teleport, &mut look, self.length, false);

        let angles = match entity.has_spawn_flags(SF_TRACKTRAIN_FIXED_ORIENTATION) || look == next_pos {
            true => entity.local_angles,
            false => {
                let mut angles = node_orientation(cx, teleport, self.is_dir_forward());
                if entity.has_spawn_flags(SF_TRACKTRAIN_NOPITCH) {
                    angles.x = current.x;
                }
                angles
            }
        };

        entity.set_abs_placement(next_pos, angles);
        entity.angular_velocity = Vec3::ZERO;
    }

    /// `CFuncTrackTrain::UpdateTrainVelocity` (`trains.cpp:1938`). Valve's
    /// parameter names are `pPrev` and `pNext`, and the caller passes
    /// `pNext` and `pNextNext` — kept as `prev`/`next` here so that the body
    /// reads like the original.
    fn update_velocity(
        &mut self,
        entity: &mut EntityCore,
        cx: &Context<'_>,
        prev: Option<EntityId>,
        next: Option<EntityId>,
        next_pos: Vec3,
    ) {
        match self.velocity_type {
            TrainVelocity::Instantaneous => {}
            TrainVelocity::LinearBlend | TrainVelocity::EaseInEaseOut => {
                if self.accel_to_speed {
                    let (prev_speed, next_speed) = (entity.speed, self.desired_speed);
                    if prev_speed != next_speed {
                        let change = match next_speed.abs() > prev_speed.abs() {
                            true => self.accel_speed,
                            false => self.decel_speed,
                        };
                        entity.speed = approach(self.desired_speed, entity.speed, change * cx.time.interval);
                    }
                } else if let (Some(prev), Some(next)) = (prev, next) {
                    let prev_core = node(cx, prev).map(|(core, _)| core);
                    let next_core = node(cx, next).map(|(core, _)| core);
                    if let (Some(prev_core), Some(next_core)) = (prev_core, next_core) {
                        // "Get the speed to blend from" and to — a node speed of
                        // zero means "whatever it was".
                        let prev_speed = match prev_core.speed {
                            0.0 => entity.speed,
                            s => s,
                        };
                        let next_speed = match next_core.speed {
                            0.0 => prev_speed,
                            s => s,
                        };

                        if prev_speed != next_speed {
                            let segment = next_core.local_origin - prev_core.local_origin;
                            let length = segment.length();
                            if length != 0.0 {
                                let mut p = (entity.local_origin - prev_core.local_origin).length() / length;
                                if self.velocity_type == TrainVelocity::EaseInEaseOut {
                                    p = simple_spline_remap(p);
                                }
                                entity.speed = self.dir * (prev_speed * (1.0 - p) + next_speed * p);
                            }
                        } else {
                            entity.speed = self.dir * prev_speed;
                        }
                    }
                }
            }
        }

        entity.velocity = (next_pos - entity.local_origin).normalize_or_zero() * entity.speed.abs();
    }

    /// `CFuncTrackTrain::UpdateTrainOrientation` (`trains.cpp:2038`).
    fn update_orientation(
        &mut self,
        entity: &mut EntityCore,
        cx: &Context<'_>,
        prev: Option<EntityId>,
        next: Option<EntityId>,
        interval: f32,
    ) {
        // "FIXME: old way of doing fixed orienation trains, remove!"
        if entity.has_spawn_flags(SF_TRACKTRAIN_FIXED_ORIENTATION) {
            return;
        }
        match self.orientation_type {
            TrainOrientation::Fixed => {}
            TrainOrientation::AtPathTracks => self.orientation_at_path_tracks(entity, cx, prev, interval),
            TrainOrientation::LinearBlend | TrainOrientation::EaseInEaseOut => {
                if let Some(prev) = prev {
                    self.orientation_blend(entity, cx, prev, next, interval);
                }
            }
        }
    }

    /// `CFuncTrackTrain::UpdateOrientationAtPathTracks` (`trains.cpp:2077`) —
    /// face the point `wheels` units ahead along the path, "a la HL1 trains".
    fn orientation_at_path_tracks(
        &mut self,
        entity: &mut EntityCore,
        cx: &Context<'_>,
        prev: Option<EntityId>,
        interval: f32,
    ) {
        let Some(path) = self.path else {
            return;
        };
        let forward = self.is_dir_forward();

        let mut front = entity.local_origin;
        front.z -= self.height;
        let reach = match self.length > 0.0 {
            true => self.length,
            false => 100.0,
        };
        let (_, next_node) = look_ahead(cx, path, &mut front, if forward { reach } else { -reach }, false);
        front.z += self.height;

        let mut face = front - entity.local_origin;
        if !forward {
            face = -face;
        }
        let mut angles = fixup_angles(vector_angles_forward(face));

        // "Wrapped with this bool so we don't affect old trains."
        if self.manual_speed_changes {
            if let Some(next_node) = next_node {
                if node(cx, next_node).map(|(_, p)| p.orientation) == Some(TrackOrientation::FacePathAngles) {
                    angles = node_orientation(cx, next_node, forward);
                }
            }
        }

        let current = fixup_angles(entity.local_angles);
        if prev.is_none() || (face.x == 0.0 && face.y == 0.0) {
            angles = current;
        }
        self.do_update_orientation(entity, current, angles, interval);
    }

    /// `CFuncTrackTrain::UpdateOrientationBlend` (`trains.cpp:2131`) — slerp
    /// between the two nodes' headings by how far along the segment the train
    /// is.
    fn orientation_blend(
        &mut self,
        entity: &mut EntityCore,
        cx: &Context<'_>,
        prev: EntityId,
        next: Option<EntityId>,
        interval: f32,
    ) {
        let forward = self.is_dir_forward();
        let mut ang_prev = fixup_angles(node_orientation(cx, prev, forward));
        let mut ang_next = match next {
            Some(next) => fixup_angles(node_orientation(cx, next, forward)),
            // "At a dead end, just use the last path track's angles."
            None => ang_prev,
        };
        let no_pitch = entity.has_spawn_flags(SF_TRACKTRAIN_NOPITCH);
        if no_pitch {
            ang_next.x = ang_prev.x;
        }

        let mut p = 0.0;
        if ang_prev != ang_next {
            if let Some(next) = next {
                let prev_origin = node_origin(cx, prev);
                let length = (node_origin(cx, next) - prev_origin).length();
                if length != 0.0 {
                    p = (entity.local_origin - prev_origin).length() / length;
                }
            }
        }
        if self.orientation_type == TrainOrientation::EaseInEaseOut {
            p = simple_spline_remap(p);
        }

        // "hack to avoid gimble lock" — and the last of the four lines turns
        // -90 into **+89**, not -89. Valve's typo, reproduced; it flips a
        // train pointing straight down to pointing nearly straight up, and no
        // shipped node points straight down.
        if ang_prev.x == 90.0 {
            ang_prev.x = 89.0;
        }
        if ang_prev.x == -90.0 {
            ang_prev.x = -89.0;
        }
        if ang_next.x == 90.0 {
            ang_next.x = 89.0;
        }
        if ang_next.x == -90.0 {
            ang_next.x = 89.0;
        }

        let q_prev = angle_quaternion(ang_prev);
        let q_next = angle_quaternion(ang_next);
        let mut ang_new = ang_next;
        if quaternion_angle_diff(q_prev, q_next) != 0.0 {
            ang_new = quaternion_angles(q_prev.slerp(q_next, p));
        }
        if no_pitch {
            ang_new.x = ang_prev.x;
        }

        let current = entity.local_angles;
        self.do_update_orientation(entity, current, ang_new, interval);
    }

    /// `CFuncTrackTrain::DoUpdateOrientation` (`trains.cpp:2213`) — turn the
    /// angle difference into an angular velocity that closes it in one
    /// interval, plus the bank.
    fn do_update_orientation(&self, entity: &mut EntityCore, current: Vec3, angles: Vec3, interval: f32) {
        let mut vx = match entity.has_spawn_flags(SF_TRACKTRAIN_NOPITCH) {
            true => 0.0,
            false => angle_distance(angles.x, current.x),
        };
        let mut vy = angle_distance(angles.y, current.y);
        let mut vz = match entity.has_spawn_flags(SF_TRACKTRAIN_ALLOWROLL) {
            true => angle_distance(angles.z, current.z),
            false => 0.0,
        };

        // "HACKHACK: Clamp really small angular deltas to avoid rotating
        // movement on things that are close enough."
        for v in [&mut vx, &mut vy, &mut vz] {
            if v.abs() < 0.1 {
                *v = 0.0;
            }
        }

        let interval = match interval == 0.0 {
            true => 0.1,
            false => interval,
        };
        let mut angular = Vec3::new(vx / interval, vy / interval, vz / interval);

        if self.bank != 0.0 {
            let bank = self.bank;
            angular.z = if angular.y < -5.0 {
                angle_distance(approach_angle(-bank, current.z, bank * 2.0), current.z)
            } else if angular.y > 5.0 {
                angle_distance(approach_angle(bank, current.z, bank * 2.0), current.z)
            } else {
                angle_distance(approach_angle(0.0, current.z, bank * 4.0), current.z) * 4.0
            };
        }

        entity.angular_velocity = angular;
    }

    /// `CFuncTrackTrain::InputMoveToPathNode` (`trains.cpp:1438`) — find the
    /// named node forwards, then backwards, and drive towards it at the
    /// current node's speed (or the maximum).
    fn move_to_path_node(&mut self, entity: &mut EntityCore, name: &str, cx: &mut Context<'_>) {
        /// `MAX_SEARCH_LENGTH`.
        const MAX_SEARCH: usize = 1000;

        self.path_target = Some(name.to_owned());
        let target = cx.find_by_name(name);
        let (Some(track), Some(target)) = (self.path, target) else {
            return;
        };

        let track_speed = node(cx, track).map_or(0.0, |(core, _)| core.speed);
        let desired = match track_speed {
            0.0 => self.max_speed,
            s => s,
        };

        // "If our current path is what we want then we can short circuit.
        // Still move forward — we will stop when we pass the track."
        if target == track {
            if self.is_dir_forward() {
                if get_next(cx, track).is_some() {
                    self.set_dir_forward(false, cx);
                    self.set_speed(entity, desired, false, cx);
                    return;
                }
            } else if get_previous(cx, track).is_some() {
                self.set_dir_forward(true, cx);
                self.set_speed(entity, desired, false, cx);
                return;
            }
            self.stop(entity);
            return;
        }

        for forward in [true, false] {
            let mut current = track;
            let mut found = false;
            for _ in 0..MAX_SEARCH {
                let Some(step) = next_in_dir(cx, current, forward) else {
                    break;
                };
                current = step;
                if step == target {
                    found = true;
                    break;
                }
            }
            if found {
                self.set_dir_forward(forward, cx);
                self.set_speed(entity, desired, false, cx);
                return;
            }
        }
    }

    /// `CFuncTrackTrain::Blocked` (`trains.cpp:1658`).
    ///
    /// > **"On the ground on the train" is rebuilt from geometry**, because
    /// > there is no ground entity here — the same gap
    /// > [`push`](crate::server::push)'s `IsStandingOnPusher` fills. A
    /// > blocker that is on the ground with its feet within two units of the
    /// > top of the train's box, and inside it horizontally, is taken to be
    /// > riding it.
    fn blocked_by(&mut self, entity: &EntityCore, other: EntityId, cx: &mut Context<'_>) {
        let Some(other_entity) = cx.entity(other) else {
            return;
        };
        let is_player = other_entity.behaviour.is_player();
        let (other_origin, on_ground) = (other_entity.core.origin, other_entity.core.has_flags(FL_ONGROUND));
        let (mins, maxs) = entity.world_space_aabb();
        let riding = on_ground
            && (other_origin.z - maxs.z).abs() <= 2.0
            && other_origin.x >= mins.x
            && other_origin.x <= maxs.x
            && other_origin.y >= mins.y
            && other_origin.y <= maxs.y;

        if riding {
            // "Blocker is on-ground on the train" — pop it up, gently.
            let delta = entity.speed.abs().min(50.0);
            if let Some(core) = cx.entity_mut(other) {
                if core.velocity.z == 0.0 {
                    core.velocity.z += delta;
                }
            }
            return;
        }

        // Shove it straight away from the train's origin at `dmg` units a
        // second — which is to say, **stop it dead** on the 222 shipped trains
        // whose `dmg` is zero. Valve's.
        if let Some(core) = cx.entity_mut(other) {
            core.velocity = (other_origin - entity.origin).normalize_or_zero() * self.block_damage;
        }

        // "unblockable shouldn't damage the player in this case"
        if entity.has_spawn_flags(SF_TRACKTRAIN_UNBLOCKABLE_BY_PLAYER) && is_player {
            return;
        }
        if self.block_damage <= 0.0 {
            return;
        }
        let me = Some(entity.id());
        cx.take_damage(other, DamageInfo::new(me, me, self.block_damage, DMG_CRUSH));
    }
}

impl Behaviour for TrackTrain {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        let text = || match value.is_empty() {
            true => None,
            false => Some(value.to_owned()),
        };

        if is("wheels") {
            self.length = atof(value);
        } else if is("height") {
            self.height = atof(value);
        } else if is("startspeed") {
            self.max_speed = atof(value);
        } else if is("bank") {
            self.bank = atof(value);
        } else if is("dmg") {
            self.block_damage = atof(value);
        } else if is("volume") {
            // `CFuncTrackTrain::KeyValue` — an integer out of ten.
            self.volume = atoi(value) as f32 * 0.1;
        } else if is("MoveSound") {
            self.sounds.move_sound = text();
        } else if is("MovePingSound") {
            self.sounds.move_ping_sound = text();
        } else if is("StartSound") {
            self.sounds.start_sound = text();
        } else if is("StopSound") {
            self.sounds.stop_sound = text();
        } else if is("MoveSoundMinPitch") {
            self.sounds.min_pitch = atoi(value);
        } else if is("MoveSoundMaxPitch") {
            self.sounds.max_pitch = atoi(value);
        } else if is("MoveSoundMinTime") {
            self.sounds.min_time = atof(value);
        } else if is("MoveSoundMaxTime") {
            self.sounds.max_time = atof(value);
        } else if is("velocitytype") {
            self.velocity_type = match atoi(value) {
                1 => TrainVelocity::LinearBlend,
                2 => TrainVelocity::EaseInEaseOut,
                _ => TrainVelocity::Instantaneous,
            };
        } else if is("orientationtype") {
            self.orientation_type = match atoi(value) {
                0 => TrainOrientation::Fixed,
                2 => TrainOrientation::LinearBlend,
                3 => TrainOrientation::EaseInEaseOut,
                _ => TrainOrientation::AtPathTracks,
            };
        } else if is("ManualSpeedChanges") {
            self.manual_speed_changes = atoi(value) != 0;
        } else if is("ManualAccelSpeed") {
            self.accel_speed = atof(value);
        } else if is("ManualDecelSpeed") {
            self.decel_speed = atof(value);
        } else {
            return false;
        }
        true
    }

    /// `CFuncTrackTrain::Spawn` (`trains.cpp:2709`).
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult {
        if self.max_speed == 0.0 {
            self.max_speed = match entity.speed {
                0.0 => 100.0,
                s => s,
            };
        }
        if self.sounds.min_pitch == 0 {
            self.sounds.min_pitch = 60;
        }
        if self.sounds.max_pitch == 0 {
            self.sounds.max_pitch = 200;
        }
        // `Precache` — "if (m_flVolume == 0.0) m_flVolume = 1.0".
        if self.volume == 0.0 {
            self.volume = 1.0;
        }

        entity.velocity = Vec3::ZERO;
        entity.angular_velocity = Vec3::ZERO;
        self.dir = 1.0;

        if entity.target.is_none() {
            eprintln!(
                "source-engine: server: FuncTrackTrain '{}' has no target.",
                entity.debug_name()
            );
        }

        entity.move_type = MoveType::Push;
        entity.solid = match entity.has_spawn_flags(SF_TRACKTRAIN_HL1TRAIN) {
            true => Solid::Bsp,
            false => Solid::VPhysics,
        };
        if entity.has_spawn_flags(SF_TRACKTRAIN_UNBLOCKABLE_BY_PLAYER) {
            entity.flags |= FL_UNBLOCKABLE_BY_PLAYER;
        }
        if entity.has_spawn_flags(SF_TRACKTRAIN_PASSABLE) {
            entity.solid_flags |= FSOLID_NOT_SOLID;
        }

        // "start trains on the next frame, to make sure their targets have had
        // a chance to spawn/activate" — `SetNextThink( gpGlobals->curtime )`.
        // **At tick zero that is "never"** (gotcha 63), and a map's `Spawn`
        // runs at tick zero here, so the first tick is asked for explicitly.
        // It is the tick the comment means.
        self.think = TrainThink::Find;
        entity.set_next_think(cx.curtime().max(cx.time.interval), cx);

        SpawnResult::Ok
    }

    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        match std::mem::replace(&mut self.think, TrainThink::None) {
            TrainThink::None => {}
            TrainThink::Find => self.find(entity, cx),
            TrainThink::Next => self.next(entity, cx),
        }
    }

    /// `CFuncTrackTrain::MoveDone` — Valve resets the physics-blocker timer
    /// that is not ported, then runs `m_pfnMoveDone`.
    fn move_done(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        match std::mem::replace(&mut self.move_done, TrainMoveDone::None) {
            TrainMoveDone::None => {}
            TrainMoveDone::DeadEnd => self.dead_end(entity, cx),
        }
    }

    fn blocked(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        self.blocked_by(entity, other, cx);
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Stop") {
            self.stop(entity);
        } else if is("Resume") {
            entity.speed = self.old_speed;
            self.start(entity, cx);
        } else if is("Reverse") {
            let forward = !self.is_dir_forward();
            self.set_dir_forward(forward, cx);
            let speed = entity.speed;
            self.set_speed(entity, speed, false, cx);
        } else if is("StartForward") {
            self.set_dir_forward(true, cx);
            let max = self.max_speed;
            self.set_speed(entity, max, false, cx);
        } else if is("StartBackward") {
            self.set_dir_forward(false, cx);
            let max = self.max_speed;
            self.set_speed(entity, max, false, cx);
        } else if is("Toggle") {
            let speed = match entity.speed == 0.0 {
                true => self.max_speed,
                false => 0.0,
            };
            self.set_speed(entity, speed, false, cx);
        } else if is("SetSpeedReal") {
            // Units a second, clamped to the maximum.
            let speed = input.value.float().clamp(0.0, self.max_speed.max(0.0));
            self.set_speed(entity, speed, false, cx);
        } else if is("SetSpeed") {
            // **A fraction of the maximum**, not a speed.
            let speed = self.max_speed * input.value.float().clamp(0.0, 1.0);
            self.set_speed(entity, speed, false, cx);
        } else if is("SetMaxSpeed") {
            self.max_speed = input.value.float();
        } else if is("SetSpeedDir") || is("SetSpeedDirAccel") {
            // A signed fraction: the sign is the direction.
            let value = input.value.float();
            self.set_dir_forward(value >= 0.0, cx);
            let speed = self.max_speed * value.abs().clamp(0.0, 1.0);
            let accel = is("SetSpeedDirAccel");
            self.set_speed(entity, speed, accel, cx);
        } else if is("MoveToPathNode") {
            let name = input.value.to_string();
            self.move_to_path_node(entity, &name, cx);
        } else if is("TeleportToPathNode") {
            let name = input.value.to_string();
            self.path_target = Some(name.clone());
            // `(CPathTrack *)pEntity` — an unchecked cast in the original;
            // here a name that is not a node does nothing.
            if let Some(target) = cx.find_by_name(&name).filter(|&id| node(cx, id).is_some()) {
                self.path = Some(target);
                self.arrive_at_node(entity, target, cx);
                self.teleport_to_path_track(entity, target, cx);
            }
        } else if is("LockOrientation") {
            entity.spawn_flags |= SF_TRACKTRAIN_FIXED_ORIENTATION;
            entity.angular_velocity = Vec3::ZERO;
        } else if is("UnlockOrientation") {
            entity.spawn_flags &= !SF_TRACKTRAIN_FIXED_ORIENTATION;
        } else {
            return false;
        }
        true
    }

    /// `CFuncTrackTrain::Use` (`trains.cpp:1351`) — `USE_SET` only, which is
    /// the player's throttle: the value is a step of a quarter of the maximum
    /// speed, forwards or back.
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
        // `((int)(m_flSpeed * 4) / (int)m_maxSpeed) * 0.25` — integer division,
        // so the current speed is rounded down to a quarter step. A maximum
        // below one would divide by zero in the original.
        let max = self.max_speed as i32;
        let step = match max {
            0 => 0,
            max => (entity.speed * 4.0) as i32 / max,
        };
        let mut delta = step as f32 * 0.25 + 0.25 * input.value.float();
        delta = delta.clamp(-0.25, 1.0);
        if entity.has_spawn_flags(SF_TRACKTRAIN_FORWARDONLY) && delta < 0.0 {
            delta = 0.0;
        }
        self.set_dir_forward(delta >= 0.0, cx);
        let speed = self.max_speed * delta.abs();
        self.set_speed(entity, speed, false, cx);
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "path",
                match self.path {
                    Some(id) => format!("#{}", id.slot()),
                    None => String::from("-"),
                },
            ),
            ("startspeed", self.max_speed.to_string()),
            ("direction", self.dir.to_string()),
            ("wheels", self.length.to_string()),
            ("height", self.height.to_string()),
            ("velocity type", format!("{:?}", self.velocity_type)),
            ("orientation type", format!("{:?}", self.orientation_type)),
            (
                "path target",
                self.path_target.clone().unwrap_or_else(|| String::from("-")),
            ),
            ("think", format!("{:?}", self.think)),
            ("move done", format!("{:?}", self.move_done)),
            ("volume", self.volume.to_string()),
            (
                "move sound",
                self.sounds.move_sound.clone().unwrap_or_else(|| String::from("-")),
            ),
            (
                "sound pitch",
                format!("{}..{}", self.sounds.min_pitch, self.sounds.max_pitch),
            ),
            (
                "ping sound",
                format!(
                    "{} every {}..{} s",
                    self.sounds.move_ping_sound.as_deref().unwrap_or("-"),
                    self.sounds.min_time,
                    self.sounds.max_time
                ),
            ),
            (
                "start/stop sound",
                format!(
                    "{} / {}",
                    self.sounds.start_sound.as_deref().unwrap_or("-"),
                    self.sounds.stop_sound.as_deref().unwrap_or("-")
                ),
            ),
        ]
    }
}

// ---------------------------------------------------------------------------
// mathlib, the parts only a train uses
// ---------------------------------------------------------------------------

/// `FixupAngles` (`movement.cpp:41`) — each component into `[0, 360]`, by
/// repeated addition rather than `fmod`, so exactly 360 stays 360.
fn fixup_angles(v: Vec3) -> Vec3 {
    let fix = |mut a: f32| {
        while a < 0.0 {
            a += 360.0;
        }
        while a > 360.0 {
            a -= 360.0;
        }
        a
    };
    Vec3::new(fix(v.x), fix(v.y), fix(v.z))
}

/// `AngleDistance` (`mathlib_base.cpp:4094`) — `next - cur`, folded once into
/// `[-180, 180]`. Once: a difference of 540 comes back as 180, not wrapped
/// twice.
fn angle_distance(next: f32, cur: f32) -> f32 {
    let delta = next - cur;
    if delta < -180.0 {
        delta + 360.0
    } else if delta > 180.0 {
        delta - 360.0
    } else {
        delta
    }
}

/// `Approach` (`mathlib.h:2840`).
fn approach(target: f32, value: f32, speed: f32) -> f32 {
    let delta = target - value;
    if delta > speed {
        value + speed
    } else if delta < -speed {
        value - speed
    } else {
        target
    }
}

/// `ApproachAngle` (`mathlib_base.cpp:4047`) — both angles through
/// `anglemod` first, so the answer is in `[0, 360)`.
fn approach_angle(target: f32, value: f32, speed: f32) -> f32 {
    use crate::server::movement::anglemod;
    let target = anglemod(target);
    let value = anglemod(value);
    let speed = speed.abs();
    let mut delta = target - value;
    if delta < -180.0 {
        delta += 360.0;
    } else if delta > 180.0 {
        delta -= 360.0;
    }
    if delta > speed {
        value + speed
    } else if delta < -speed {
        value - speed
    } else {
        target
    }
}

/// `SimpleSplineRemapVal( p, 0, 1, 0, 1 )` — `3p² - 2p³`.
fn simple_spline_remap(p: f32) -> f32 {
    let sq = p * p;
    3.0 * sq - 2.0 * sq * p
}

/// `AngleQuaternion` — the rotation [`angle_matrix`] builds, as a quaternion.
fn angle_quaternion(angles: Vec3) -> Quat {
    Quat::from_mat3(&angle_matrix(angles))
}

/// `QuaternionAngles` — through the matrix, exactly as Valve's `#if 1` does.
fn quaternion_angles(q: Quat) -> Vec3 {
    matrix_angles(Mat3::from_quat(q))
}

/// `QuaternionAngleDiff` (`mathlib_base.cpp:1797`) — the angle between two
/// orientations, in degrees, by `asin` rather than `acos` so that small ones
/// do not round to zero.
fn quaternion_angle_diff(p: Quat, q: Quat) -> f32 {
    let diff = p * q.conjugate();
    let sin = diff.xyz().length().min(1.0);
    (2.0 * sin.asin()).to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angle_distance_folds_once() {
        assert_eq!(angle_distance(10.0, 350.0), 20.0);
        assert_eq!(angle_distance(350.0, 10.0), -20.0);
        assert_eq!(angle_distance(90.0, 0.0), 90.0);
    }

    #[test]
    fn fixup_keeps_three_sixty() {
        assert_eq!(fixup_angles(Vec3::new(-90.0, 360.0, 725.0)), Vec3::new(270.0, 360.0, 5.0));
    }

    #[test]
    fn quaternion_round_trip_and_diff() {
        let a = Vec3::new(10.0, 45.0, 0.0);
        let back = quaternion_angles(angle_quaternion(a));
        assert!((back - a).length() < 1e-3, "{back}");
        let b = Vec3::new(10.0, 75.0, 0.0);
        let diff = quaternion_angle_diff(angle_quaternion(a), angle_quaternion(b));
        assert!((diff - 30.0).abs() < 0.05, "{diff}");
    }
}
