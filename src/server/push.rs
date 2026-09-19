//! The pusher: a door that shoves the player, and a player who can stop a door.
//!
//! `CPhysicsPushedEntities` (`game/server/physics_main.cpp:122-1130`) and the
//! three `CBaseEntity` methods that drive it — `PerformPush` (`:1590`),
//! `PhysicsPushMove` (`:1537`) and `PhysicsPushRotate` (`:1558`).
//!
//! # What it is for
//!
//! [`movement`](super::movement) integrates a mover's velocity once a tick and
//! nothing else; until this module existed a door moved *through* whatever was
//! standing in it. This is the other half: before a mover is allowed to keep
//! the position it just moved to, everything solid it now overlaps is shoved
//! out of the way, and if any of it cannot be shoved the whole move is
//! **rolled back** — the mover's origin, its angles, its local time, and every
//! entity the attempt displaced.
//!
//! ```text
//! PerformPush( movetime )
//!   BeginPush
//!   rotating? PhysicsPushRotate -> PerformRotatePush  ─┐
//!   moving?   PhysicsPushMove   -> PerformLinearPush  ─┤
//!                                                      │
//!     SetupAllInHierarchy      the root + everything parented under it
//!     Rotate/LinearlyMoveRootEntity   the speculative move, children dragged
//!     GenerateBlockingEntityList      what is in the way now
//!     SpeculativelyCheckPush          shove each one; can any not be shoved?
//!       blocked -> RegisterBlockage, put the root back, RestoreEntities
//!       clear   -> FinishPush
//!   StartBlocked / Blocked / EndBlocked
//!   MoveDone, if the arrival alarm has come round
//! ```
//!
//! # Only the player can be pushed, and that is measured rather than assumed
//!
//! `IsPushableMoveType` (`baseentity_shared.h:316`) is *not* a list of
//! movetypes that can be pushed; it is the four that cannot —
//! `MOVETYPE_PUSH`, `MOVETYPE_NONE`, `MOVETYPE_VPHYSICS` and
//! `MOVETYPE_NOCLIP`. This port has five movetypes and three of them are on
//! that list, so what is left is [`MoveType::Walk`](super::movement::MoveType)
//! and [`MoveType::FlyGravity`](super::movement::MoveType) — **the player,
//! alive and dead**, and nothing else in the entity list. A noclipping player
//! is not pushed, which is Valve's and is the same rule `trigger_push`
//! follows.
//!
//! The candidate loop is written over the whole list anyway, because the
//! filter is what says so and a class that adds a sixth movetype should not
//! have to find this file.
//!
//! # Where the geometry comes from
//!
//! Three sweeps, three different clip chains, and they go out through
//! [`TouchQuery::push_trace`] for the same reason the touch test does: this
//! module names no collision type. [`PushClip`] is the table of which is
//! which. The one that needs saying twice is the middle one — the pushers are
//! **hidden** for the speculative shove, because the entity being shoved is by
//! definition already inside the door, and a sweep that could see the door
//! would refuse to move at fraction zero every single time.
//!
//! The placements handed over are the **speculative** ones. `world/` holds a
//! copy of every brush entity's placement, synced once a frame *after* the
//! ticks; a push happens mid-tick against a door that has already been moved
//! and may yet be moved back, so it tells the engine where the door is rather
//! than asking.
//!
//! # What is deliberately not here
//!
//! - **`UpdatePusherPhysicsEndOfTick`, `StoreMovedEntities` and
//!   `UpdatePhysicsShadowToCurrentPosition`.** All three are `vphysics`
//!   bookkeeping — the deferred queue exists so a `physicspushlist_t` can undo
//!   the push if the *physics* system blocks later. There is no physics
//!   system.
//! - **`PhysicsTouchTriggers` and `PhysicsImpact` in `FinishPush`.** A push
//!   moves at most the player, and [`Server::run_tick`](super::Server) already
//!   sweeps the player against every trigger once a tick from the previous
//!   tick's origin — so the touch the C++ generates here is generated anyway,
//!   half a tick later, by the pass that exists for it.
//! - **`CBaseEntity::CanPushEntity`.** A virtual that returns `true` and which
//!   nothing in the shipped `game/server/` overrides.
//! - **`CTraceFilterPushFinal`'s two extra tests** — ignore teammates (no
//!   teams) and skip moveable `MOVETYPE_VPHYSICS` entities (none exist).
//! - **`NotifyPushMove`**, which tells an NPC its ground moved. `m_bPusherIsGround`
//!   is computed for nobody.
//!
//! # Three things the reference does that are worth knowing before reading it
//!
//! 1. **`UnblockPusher` is `// TODO`.** `CGameMovement::UnblockPusher`
//!    (`gamemovement.cpp:3752`) has an empty body in the shipped tree, and
//!    `SpeculativelyCheckPush` calls it and then asks `if (
//!    pBlocker->GetAbsOrigin() == blockerOrigin )` — which, since nothing
//!    moved the player, is always true. So **a player who is stuck after being
//!    shoved always blocks**: the entire "fix the player" ladder below that
//!    call is dead code in the shipped game, and the four half-inch nudges
//!    below it are in the `else` branch, for blockers that are not players.
//!    Reproduced as written rather than tidied, because the shape is what says
//!    where the missing implementation goes.
//! 2. **`SpeculativelyCheckRotPush` reads uninitialised memory.** `Vector
//!    vecAbsPush;` is declared without a value and handed to
//!    `ComputeRotationalPushDirection`, which reads its sign to pick which
//!    corner of the blocker's box to rotate about — before writing it. On the
//!    second and later entities it holds the *previous* entity's push. Here it
//!    starts at zero, which selects the `vecAbsMins` corner on all three axes;
//!    see [`rotational_push_direction`].
//! 3. **`PerformPush` runs both halves and keeps the larger clock.** A mover
//!    that translates *and* rotates advances its local time twice from the
//!    same starting value and then takes the greater — Valve's own comment
//!    says "Choose the *greater* of the two?!? That's strange...". It is
//!    ported as written.

use glam::Vec3;

use super::class::{Behaviour, Context};
use super::entity::{EntityCore, EntityId};
use super::hierarchy;
use super::movement::{MoveType, FL_ONGROUND, FL_UNBLOCKABLE_BY_PLAYER, FSOLID_VOLUME_CONTENTS};
use super::{PushClip, Pusher, TouchQuery};

/// How far below its feet an entity is asked for ground.
///
/// `IsStandingOnPusher` (`physics_main.cpp:787`) reads the *ground entity*,
/// which `CGameMovement::CategorizePosition` set from its own two-unit drop.
/// This port has no ground entity — the player's ground is found by
/// `client/`'s move and only the flag comes back across the seam — so the
/// drop is redone here, against the pushers alone. Two units is
/// `CategorizePosition`'s number.
const GROUND_PROBE: f32 = 2.0;

/// How far past the pushers' own bounds the candidate search reaches.
///
/// Valve's broadphase is `::partition->EnumerateElementsInBox` over each
/// pusher's world AABB, and an entity *standing on* a pusher is not inside
/// that box — it rests [`GROUND_PROBE`] above it, or on the epsilon a swept
/// hull keeps between itself and a surface. The box has to be at least as
/// generous as the standing test it gates, or a player riding a platform is
/// dropped by the search before the test that would have caught them ever
/// runs.
const BROADPHASE_SLOP: f32 = GROUND_PROBE;

/// One entity of the pushing hierarchy. `PhysicsPusherInfo_t`.
struct PusherEntry {
    id: EntityId,
    /// `m_vecStartAbsOrigin`, recorded before the speculative move.
    ///
    /// Read by nothing here — it is `UpdatePusherPhysicsEndOfTick`'s, for the
    /// swept trigger touch this port does elsewhere — and kept because the
    /// list it belongs to is the one thing `SetupAllInHierarchy` produces that
    /// is not obvious from the hierarchy itself.
    #[allow(dead_code)]
    start_abs_origin: Vec3,
}

/// The pushing hierarchy, after it has moved: `m_rgPusher`, plus the two
/// things every one of the three sweeps needs.
struct Pushers {
    /// The root first, then everything parented under it, breadth first.
    entries: Vec<PusherEntry>,
    /// Where the ones that can collide with a player are *now*.
    ///
    /// **Not all of `entries`.** A pusher is left out when it is not solid
    /// (`FSOLID_NOT_SOLID` — 116 shipped doors set `SF_DOOR_PASSABLE`), when
    /// it is a volume (`FSOLID_VOLUME_CONTENTS`), when its collision group
    /// says the player passes through it (141 shipped doors set
    /// `SF_DOOR_NONSOLID_TO_PLAYER`), or when it has no brush model to sweep
    /// against. An empty list is a hierarchy that cannot push anything, and
    /// the whole attempt is skipped.
    placements: Vec<Pusher>,
    /// The broadphase: the union of the solid pushers' world AABBs, grown by
    /// the move and by [`BROADPHASE_SLOP`].
    ///
    /// Valve enumerates per pusher, which is tighter. A union can only admit
    /// *more* candidates than the per-pusher boxes would, and every candidate
    /// then goes through the exact test — so the answers agree and the loop
    /// is one pass over the entity list instead of one per pusher.
    box_mins: Vec3,
    box_maxs: Vec3,
    /// `m_bIsUnblockableByPlayer`.
    unblockable: bool,
    /// The hierarchy's root — the entity being dispatched, which is **not in
    /// the entity list** for the duration.
    root: EntityId,
}

/// One entity the push is trying to move. `PhysicsPushedInfo_t`.
struct MovedInfo {
    id: EntityId,
    /// `m_vecStartAbsOrigin` — where to put it back if the push is rolled
    /// back.
    start_abs_origin: Vec3,
    /// Where the pusher it is standing on has its origin, if it is standing on
    /// one. `IsStandingOnPusher` plus the one thing the failure path does with
    /// the answer.
    standing_on: Option<Vec3>,
    /// `m_bBlocked`.
    blocked: bool,
}

// ---------------------------------------------------------------------------
// CBaseEntity::PerformPush and the two push types
// ---------------------------------------------------------------------------

/// `CBaseEntity::PerformPush` (`physics_main.cpp:1590`).
///
/// Three things happen and the order is Valve's: the entity rotates and then
/// translates, each advancing its own copy of local time; the blocked edges
/// are fired; and the arrival alarm is tested.
///
/// > **The last step of a move is exactly as long as the travel that is
/// > left**, because
/// > [`physics_pusher`](super::movement) clamps `movetime` to the *remaining*
/// > time rather than always taking a whole tick. So the alarm goes off on the
/// > tick the move ends, not the tick after, and `local_time` lands on
/// > `move_done_time` rather than stepping over it. Get this wrong and every
/// > door in the game arrives up to a sixty-fourth of a second late and a
/// > fraction of a unit long.
///
/// > **A blocked push rolls local time back too.** That is what makes a
/// > blocked door resume where it left off instead of teleporting forward when
/// > the blocker steps aside: the alarm is on local time, and local time only
/// > advances on a tick the entity actually moved.
pub fn perform_push(
    entity: &mut EntityCore,
    behaviour: &mut dyn Behaviour,
    cx: &mut Context<'_>,
    query: &mut dyn TouchQuery,
    movetime: f32,
) {
    // "NOTE: Use handle index because the previous blocker could have been
    // deleted" — an `EntityId` is a generational handle and comparing two of
    // them is the same test.
    let previously = entity.blocker;

    if movetime > 0.0 {
        let blocker = match (
            entity.angular_velocity != Vec3::ZERO,
            entity.velocity != Vec3::ZERO,
        ) {
            // "moving and rotating, so rotate first, then move". Both halves
            // advance local time from the *same* starting value and the
            // greater one wins — see the module docs.
            (true, true) => {
                let initial = entity.local_time;
                match physics_push_rotate(entity, cx, query, movetime) {
                    Some(blocker) => Some(blocker),
                    None => {
                        let rotated = entity.local_time;
                        entity.local_time = initial;
                        let blocker = physics_push_move(entity, cx, query, movetime);
                        if entity.local_time < rotated {
                            entity.local_time = rotated;
                        }
                        blocker
                    }
                }
            }
            (true, false) => physics_push_rotate(entity, cx, query, movetime),
            // Also the branch a pusher with no velocity at all takes, which is
            // what makes the alarm double as a plain wait timer:
            // `PhysicsPushMove` increments local time before it looks at the
            // velocity, so a door standing still at the top of its travel
            // still spends `m_flWait`.
            (false, _) => physics_push_move(entity, cx, query, movetime),
        };

        entity.blocker = blocker;
        if blocker != previously {
            if previously.is_some() {
                behaviour.end_blocked(entity, cx);
            }
            if let Some(blocker) = blocker {
                behaviour.start_blocked(entity, blocker, cx);
            }
        }
        if let Some(blocker) = blocker {
            behaviour.blocked(entity, blocker, cx);
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

/// `CBaseEntity::PhysicsPushMove` (`physics_main.cpp:1537`).
fn physics_push_move(
    entity: &mut EntityCore,
    cx: &mut Context<'_>,
    query: &mut dyn TouchQuery,
    movetime: f32,
) -> Option<EntityId> {
    // "If this entity isn't moving, just update the time" — before the
    // velocity test, deliberately.
    entity.local_time += movetime;
    if entity.velocity == Vec3::ZERO {
        return None;
    }

    let blocker = perform_linear_push(entity, cx, query, movetime);
    if blocker.is_some() {
        entity.local_time -= movetime;
    }
    blocker
}

/// `CBaseEntity::PhysicsPushRotate` (`physics_main.cpp:1558`).
fn physics_push_rotate(
    entity: &mut EntityCore,
    cx: &mut Context<'_>,
    query: &mut dyn TouchQuery,
    movetime: f32,
) -> Option<EntityId> {
    entity.local_time += movetime;
    if entity.angular_velocity == Vec3::ZERO {
        return None;
    }

    let blocker = perform_rotate_push(entity, cx, query, movetime);
    if blocker.is_some() {
        entity.local_time -= movetime;
    }
    blocker
}

/// `CPhysicsPushedEntities::PerformLinearPush` (`physics_main.cpp:1075`).
fn perform_linear_push(
    root: &mut EntityCore,
    cx: &mut Context<'_>,
    query: &mut dyn TouchQuery,
    movetime: f32,
) -> Option<EntityId> {
    let mut pushers = setup_all_in_hierarchy(root, cx);

    // "save where we started from, in case we're blocked"
    let previous_local_origin = root.local_origin;
    let previous_abs_origin = root.origin;

    // `LinearlyMoveRootEntity` (`physics_main.cpp:1057`) — `SetLocalOrigin(
    // GetLocalOrigin() + GetLocalVelocity() * movetime )`, in the *parent's*
    // frame, followed by the walk that drags the subtree with it.
    root.set_local_origin(root.local_origin + root.velocity * movetime);
    hierarchy::propagate(root, cx.entities_mut());

    // `*pAbsPushVector = pRoot->GetAbsVelocity() * movetime`. This port has no
    // absolute velocity — [`hierarchy`](super::hierarchy) records why the
    // velocity pair is not there — so the push vector is the displacement the
    // root actually underwent, which is the same number for an unparented root
    // and the *right* number for a parented one, where Valve's would be a tick
    // stale whenever the parent is itself moving.
    let abs_push = root.origin - previous_abs_origin;

    pushers.refresh(root, cx, abs_push);
    let moved = generate_blocking_entity_list(&pushers, cx, query);

    match speculatively_check_linear_push(&pushers, moved, abs_push, root, cx, query) {
        Ok(moved) => {
            finish_push(&moved, None, cx);
            None
        }
        Err((moved, blocker)) => {
            root.set_local_origin(previous_local_origin);
            hierarchy::propagate(root, cx.entities_mut());
            restore_entities(&moved, cx);
            Some(blocker)
        }
    }
}

/// `CPhysicsPushedEntities::PerformRotatePush` (`physics_main.cpp:1017`).
fn perform_rotate_push(
    root: &mut EntityCore,
    cx: &mut Context<'_>,
    query: &mut dyn TouchQuery,
    movetime: f32,
) -> Option<EntityId> {
    let mut pushers = setup_all_in_hierarchy(root, cx);

    let previous_local_angles = root.local_angles;

    // `RotateRootEntity` (`physics_main.cpp:993`), which fills in a
    // `RotatingPushMove_t`: the angular step, and the root's own frame on
    // either side of it. Both frames are needed because the direction a
    // rotation pushes a blocker depends on where the blocker is, which is a
    // question about the *change* of frame rather than about the angles.
    let rotation = RotatingPushMove {
        amove: root.angular_velocity * movetime,
        start_local_to_world: root.to_world(),
        end_local_to_world: {
            root.set_local_angles(root.local_angles + root.angular_velocity * movetime);
            hierarchy::propagate(root, cx.entities_mut());
            root.to_world()
        },
    };

    // `GenerateBlockingEntityList()` — the plain one. A rotation has no single
    // displacement to grow the search box by, so Valve uses the pushers' own
    // bounds at their final orientation and nothing more.
    pushers.refresh(root, cx, Vec3::ZERO);
    let moved = generate_blocking_entity_list(&pushers, cx, query);

    match speculatively_check_rot_push(&pushers, moved, &rotation, root, cx, query) {
        Ok(moved) => {
            finish_push(&moved, Some(&rotation), cx);
            None
        }
        Err((moved, blocker)) => {
            root.set_local_angles(previous_local_angles);
            hierarchy::propagate(root, cx.entities_mut());
            restore_entities(&moved, cx);
            Some(blocker)
        }
    }
}

/// `RotatingPushMove_t` (`physics_main.h:26`) — one tick of a rotation, as the
/// two frames it happened between.
///
/// Valve carries `origin` as well; nothing reads it.
struct RotatingPushMove {
    /// The angular step, in degrees. `amove`.
    amove: Vec3,
    start_local_to_world: glam::Affine3A,
    end_local_to_world: glam::Affine3A,
}

// ---------------------------------------------------------------------------
// the pusher list
// ---------------------------------------------------------------------------

/// `CPhysicsPushedEntities::SetupAllInHierarchy` (`physics_main.cpp:947`) —
/// the root and everything parented under it, breadth first, with every
/// entity's starting position recorded.
///
/// Called **before** the speculative move, which is what the C++'s "make sure
/// to snack the position +before+ relink" comment is about — and here it is
/// not merely a comment, because this port propagates eagerly: a child that
/// was read after the root moved would report where it is going rather than
/// where it was.
fn setup_all_in_hierarchy(root: &EntityCore, cx: &Context<'_>) -> Pushers {
    let mut entries = vec![PusherEntry {
        id: root.id(),
        start_abs_origin: root.origin,
    }];
    for id in hierarchy::descendants(root, cx.entities()) {
        let Some(entity) = cx.entity(id) else {
            continue;
        };
        entries.push(PusherEntry {
            id,
            start_abs_origin: entity.core.origin,
        });
    }

    Pushers {
        entries,
        placements: Vec::new(),
        box_mins: Vec3::ZERO,
        box_maxs: Vec3::ZERO,
        unblockable: root.has_flags(FL_UNBLOCKABLE_BY_PLAYER),
        root: root.id(),
    }
}

impl Pushers {
    /// Re-reads where the hierarchy is now and what of it can push, after the
    /// speculative move.
    ///
    /// `moved` is the displacement the root underwent, which grows the search
    /// box *backwards* — `GenerateBlockingEntityListAddBox`
    /// (`physics_main.cpp:907`) — so that something the pusher has just swept
    /// through is still a candidate at the position it was swept from.
    fn refresh(&mut self, root: &EntityCore, cx: &Context<'_>, moved: Vec3) {
        self.placements.clear();
        let mut mins = Vec3::splat(f32::INFINITY);
        let mut maxs = Vec3::splat(f32::NEG_INFINITY);

        for index in 0..self.entries.len() {
            let id = self.entries[index].id;
            // The root is out of the list for the whole dispatch; every other
            // pusher is in it.
            let core = match id == self.root {
                true => root,
                false => match cx.entity(id) {
                    Some(entity) => &entity.core,
                    None => continue,
                },
            };

            // `if ( !pPusher->IsSolid() || pPusher->IsSolidFlagSet(
            // FSOLID_VOLUME_CONTENTS ) ) continue;`, plus the collision-group
            // test `GetPushableEntity` makes on the other side of the same
            // question — see the field's docs.
            if !core.is_solid() || core.is_solid_flag_set(FSOLID_VOLUME_CONTENTS) {
                continue;
            }
            if !core.collides_with_player() {
                continue;
            }
            let Some(model) = core.brush_model_index() else {
                continue;
            };

            self.placements.push(Pusher {
                model,
                origin: core.origin,
                angles: core.angles,
            });

            let (lo, hi) = core.world_space_aabb();
            mins = mins.min(lo);
            maxs = maxs.max(hi);
        }

        if self.placements.is_empty() {
            self.box_mins = Vec3::ZERO;
            self.box_maxs = Vec3::ZERO;
            return;
        }

        // `if ( vecMoved[iAxis] >= 0.0f ) mins -= moved; else maxs -= moved;`
        // — each axis grown on the side the pushers came from, never shrunk.
        for axis in 0..3 {
            match moved[axis] >= 0.0 {
                true => mins[axis] -= moved[axis],
                false => maxs[axis] -= moved[axis],
            }
        }
        self.box_mins = mins - Vec3::splat(BROADPHASE_SLOP);
        self.box_maxs = maxs + Vec3::splat(BROADPHASE_SLOP);
    }
}

/// `CPhysicsPushedEntities::GenerateBlockingEntityList` and its `AddBox`
/// twin (`physics_main.cpp:879`, `:907`) — everything that could be in the
/// way, now that the hierarchy has moved.
///
/// The partition enumerator is a linear pass over the entity list, which is
/// `Tracer::with_entities`' precedent and is affordable for the same reason:
/// the first test is a movetype comparison and it rejects every entity in a
/// Portal 2 map but one.
fn generate_blocking_entity_list(
    pushers: &Pushers,
    cx: &mut Context<'_>,
    query: &mut dyn TouchQuery,
) -> Vec<MovedInfo> {
    let mut moved: Vec<MovedInfo> = Vec::new();
    if pushers.placements.is_empty() {
        return moved;
    }

    // `CPushBlockerEnum::GetPushableEntity` (`physics_main.cpp:815`), in its
    // order: seen already, solid, movetype, collision group, hierarchy — then
    // the two geometric tests.
    let candidates: Vec<EntityId> = cx
        .entities()
        .iter()
        .filter(|(_, entity)| is_pushable_move_type(entity.core.move_type))
        .filter(|(_, entity)| entity.core.is_solid())
        .filter(|(_, entity)| entity.core.collides_with_player())
        .map(|(id, _)| id)
        .collect();

    for id in candidates {
        // "NOTE: This is pretty tricky here. If a rigidly attached child comes
        // into contact with a pusher, we *cannot* push the child. Instead, we
        // must push the highest parent of that child."
        let id = hierarchy::root_move_parent(id, cx.entities());
        if id == pushers.root || moved.iter().any(|info| info.id == id) {
            continue;
        }
        let Some(entity) = cx.entity(id) else {
            continue;
        };
        let (origin, mins, maxs) = (
            entity.core.origin,
            entity.core.model_bounds.mins,
            entity.core.model_bounds.maxs,
        );
        let on_ground = entity.core.has_flags(FL_ONGROUND);
        let (lo, hi) = entity.core.world_space_aabb();
        if lo.cmpgt(pushers.box_maxs).any() || hi.cmplt(pushers.box_mins).any() {
            continue;
        }

        // "If we're standing on the pusher or any rigidly attached child of
        // the pusher, we don't need to bother checking for interpenetration" —
        // and it is not an optimisation, it is the whole of how a platform
        // carries what is riding it. Something standing on a pusher is
        // *beside* it, not inside it, so the interpenetration test below
        // answers no.
        let standing_on = standing_on_pusher(pushers, query, origin, mins, maxs, on_ground);
        if standing_on.is_none() && !intersects_pushers(pushers, query, origin, mins, maxs) {
            continue;
        }

        moved.push(MovedInfo {
            id,
            start_abs_origin: origin,
            standing_on,
            blocked: false,
        });
    }

    moved
}

/// `IsPushableMoveType` (`baseentity_shared.h:316`) — the four that are *not*,
/// negated, exactly as the original spells it.
fn is_pushable_move_type(move_type: MoveType) -> bool {
    !matches!(
        move_type,
        MoveType::Push | MoveType::None | MoveType::Noclip
    )
}

/// `CPushBlockerEnum::IsStandingOnPusher` (`physics_main.cpp:787`), rebuilt
/// from geometry because there is no ground entity to ask.
///
/// Returns where the pusher it is standing on has its **origin**, which is the
/// one thing the failure path downstream does with the answer. The origin is
/// the root's: a `PushHit` says whether something was hit and not what, and
/// the hierarchy's members share a frame, which is what makes that
/// substitution legitimate rather than convenient — see `rustdocs/SERVER.md`.
fn standing_on_pusher(
    pushers: &Pushers,
    query: &mut dyn TouchQuery,
    origin: Vec3,
    mins: Vec3,
    maxs: Vec3,
    on_ground: bool,
) -> Option<Vec3> {
    // `if ( pCheck->GetFlags() & FL_ONGROUND || pGroundEnt )` — something in
    // mid air is standing on nothing, whatever is under it.
    if !on_ground {
        return None;
    }
    let below = origin - Vec3::Z * GROUND_PROBE;
    let hit = query.push_trace(
        PushClip::PushersOnly,
        origin,
        below,
        mins,
        maxs,
        &pushers.placements,
    );
    let landed = hit.start_solid || hit.fraction < 1.0;
    landed.then(|| pushers.placements.first().map_or(Vec3::ZERO, |p| p.origin))
}

/// `CPushBlockerEnum::IntersectsPushers` (`physics_main.cpp:800`) — "our
/// surrounding boxes are touching. But we may well not be colliding.... see if
/// the ent's bbox is inside the pusher's final position".
///
/// An unswept sweep against the pushers alone, which is
/// `enginetrace->SweepCollideable( …, origin, origin, …, &m_pushersOnly, &tr
/// )` and reads only `startsolid`.
fn intersects_pushers(
    pushers: &Pushers,
    query: &mut dyn TouchQuery,
    origin: Vec3,
    mins: Vec3,
    maxs: Vec3,
) -> bool {
    query
        .push_trace(
            PushClip::PushersOnly,
            origin,
            origin,
            mins,
            maxs,
            &pushers.placements,
        )
        .start_solid
}

// ---------------------------------------------------------------------------
// the speculative push
// ---------------------------------------------------------------------------

/// The two `SpeculativelyCheck*Push` loops' shared result: the list back, or
/// the list back with the entity that blocked.
type Checked = Result<Vec<MovedInfo>, (Vec<MovedInfo>, EntityId)>;

/// `CPhysicsPushedEntities::SpeculativelyCheckLinearPush`
/// (`physics_main.cpp:506`).
fn speculatively_check_linear_push(
    pushers: &Pushers,
    mut moved: Vec<MovedInfo>,
    abs_push: Vec3,
    root: &EntityCore,
    cx: &mut Context<'_>,
    query: &mut dyn TouchQuery,
) -> Checked {
    // Backwards, which is Valve's and is observable only through the
    // uninitialised-push bug the rotational twin has.
    for index in (0..moved.len()).rev() {
        if !speculatively_check_push(pushers, &mut moved, index, abs_push, false, root, cx, query) {
            let blocker = register_blockage(&moved, index);
            return Err((moved, blocker));
        }
    }
    Ok(moved)
}

/// `CPhysicsPushedEntities::SpeculativelyCheckRotPush`
/// (`physics_main.cpp:483`).
fn speculatively_check_rot_push(
    pushers: &Pushers,
    mut moved: Vec<MovedInfo>,
    rotation: &RotatingPushMove,
    root: &EntityCore,
    cx: &mut Context<'_>,
    query: &mut dyn TouchQuery,
) -> Checked {
    // Valve's `Vector vecAbsPush;`, which is uninitialised on the first pass
    // and holds the previous blocker's push on every one after it. Zero here —
    // see the module docs, finding 2.
    let mut abs_push = Vec3::ZERO;
    for index in (0..moved.len()).rev() {
        abs_push = rotational_push_direction(&moved[index], rotation, abs_push, root, cx);
        if !speculatively_check_push(pushers, &mut moved, index, abs_push, true, root, cx, query) {
            let blocker = register_blockage(&moved, index);
            return Err((moved, blocker));
        }
    }
    Ok(moved)
}

/// `CPhysicsPushedEntities::ComputeRotationalPushDirection`
/// (`physics_main.cpp:161`) — where a rotation of the root carries a blocker.
///
/// The blocker's position is expressed in the root's frame *before* the
/// rotation and read back out of the frame *after* it; the difference is the
/// push. That is the only way to get a translation out of a rotation without
/// assuming the blocker turns, and blockers do not turn — a player stays
/// upright on a revolving door.
///
/// > **The `SOLID_VPHYSICS` branch is a documented hack and it is the branch
/// > almost every rotation takes.** "Use move dir to guess which corner of the
/// > box determines contact and rotate the box so that corner remains in the
/// > same local position. BUGBUG: This will break, but not as badly as the
/// > previous solution!!!" A `func_door_rotating` is `SOLID_VPHYSICS` unless
/// > it hangs off a `SOLID_BSP` root (`doors.cpp:233`), so the rotation is
/// > measured from a *corner* of the blocker's world box rather than from its
/// > origin — and which corner is chosen by the sign of the previous push,
/// > which on the first pass is Valve's uninitialised stack. Zero selects the
/// > low corner on all three axes.
///
/// > That test is also why the door's solidity had to be corrected before this
/// > module could be trusted: stage 3 had it inverted, and nothing read it
/// > until now. See `CBaseDoor::Spawn` in
/// > [`classes::brush`](crate::server::classes::brush).
fn rotational_push_direction(
    info: &MovedInfo,
    rotation: &RotatingPushMove,
    previous_push: Vec3,
    root: &EntityCore,
    cx: &Context<'_>,
) -> Vec3 {
    let Some(entity) = cx.entity(info.id) else {
        return Vec3::ZERO;
    };

    // `GetCollisionOrigin()`, which for everything this port can push is the
    // entity's own absolute origin.
    let mut start = entity.core.origin;
    if root.solid == super::movement::Solid::VPhysics {
        let (mins, maxs) = entity.core.world_space_aabb();
        for axis in 0..3 {
            start[axis] = match previous_push[axis] < 0.0 {
                true => maxs[axis],
                false => mins[axis],
            };
        }
    }

    // `VectorITransform` then `VectorTransform` — into the old frame, out of
    // the new one.
    let local = rotation
        .start_local_to_world
        .inverse()
        .transform_point3(start);
    let end = rotation.end_local_to_world.transform_point3(local);
    end - start
}

/// `CPhysicsPushedEntities::SpeculativelyCheckPush` (`physics_main.cpp:317`) —
/// shove one entity, and say whether it went.
///
/// Returns `false` for a blocker, which is the only way the whole push fails.
#[allow(clippy::too_many_arguments)]
fn speculatively_check_push(
    pushers: &Pushers,
    moved: &mut [MovedInfo],
    index: usize,
    abs_push: Vec3,
    rotational: bool,
    root: &EntityCore,
    cx: &mut Context<'_>,
    query: &mut dyn TouchQuery,
) -> bool {
    let id = moved[index].id;
    let Some(entity) = cx.entity(id) else {
        return true;
    };
    let origin = entity.core.origin;
    let (mins, maxs) = (entity.core.model_bounds.mins, entity.core.model_bounds.maxs);
    let point_sized = entity.core.is_point_sized();
    let solid = entity.core.is_solid();
    let volume = entity.core.is_solid_flag_set(FSOLID_VOLUME_CONTENTS);
    let is_player = entity.behaviour.is_player();

    let destination = origin + abs_push;

    // The speculative shove, with every pusher hidden — `UnlinkPusherList`.
    let hit = query.push_trace(
        PushClip::WithoutPushers,
        origin,
        destination,
        mins,
        maxs,
        &pushers.placements,
    );

    // `m_bIsUnblockableByPlayer && (IsPlayer() || MyNPCPointer())`.
    let unblockable = pushers.unblockable && is_player;
    if unblockable {
        set_abs_origin(cx, id, destination);
    } else {
        // "Move the blocker into its new position." The `if ( trace.fraction )`
        // is Valve's: a sweep that could not move at all leaves the entity
        // where it is rather than writing the same number back.
        if hit.fraction != 0.0 {
            set_abs_origin(cx, id, hit.end);
        }

        // "We're not blocked if the blocker is point-sized or non-solid."
        if point_sized || !solid || volume {
            return true;
        }

        // A linear push that reached its destination is believed. Valve
        // re-checks and prints "Interpenetrating entities!" if it was wrong;
        // the check has no other effect, so what is ported is the early-out.
        if !rotational && hit.fraction == 1.0 {
            return true;
        }
    }

    if !is_stuck(pushers, query, cx, id) {
        moved[index].blocked = false;
        return true;
    }
    moved[index].blocked = true;

    // "if the player is blocking the train try nudging him around to fix
    // accumulated error" — a door flagged unblockable never fails, it only
    // tries to fail gracefully.
    if unblockable {
        if nudge_along_pusher_axes(pushers, query, cx, id, root) {
            return true;
        }
        // "Ignoring player blocking train!"
        set_abs_origin(cx, id, destination);
        return true;
    }

    // "If a player is blocking us, try nudging him around to fix accumulated
    // errors" — toward the centre of whatever they are standing on, when that
    // is not the world. The port can answer that question only for a pusher,
    // which is the case the branch is written for.
    let after_push = current_origin(cx, id).unwrap_or(origin);
    if let Some(ground) = moved[index].standing_on {
        let mut to_centre = ground - after_push;
        to_centre.z = 0.0;
        if to_centre != Vec3::ZERO {
            set_abs_origin(cx, id, after_push + to_centre.normalize() * 16.0);
            if !is_stuck(pushers, query, cx, id) {
                // "Fixing player blocking train by moving to center!"
                moved[index].blocked = false;
                return true;
            }
        }
    }

    if is_player {
        // `pBlocker->SetAbsOrigin( blockerOrigin )` — the origin from *before*
        // the shove — then `UnblockPusher`, which is `// TODO` in the shipped
        // tree. Its emptiness is what makes the test below always true and the
        // player always a blocker; see the module docs, finding 1.
        set_abs_origin(cx, id, origin);
        return false;
    }

    if nudge_along_pusher_axes(pushers, query, cx, id, root) {
        moved[index].blocked = false;
        return true;
    }

    // "restore origin..."
    set_abs_origin(cx, id, after_push);
    false
}

/// The four half-inch nudges both failure paths try: along the root's own X
/// and Y axes, in each direction.
///
/// `MatrixGetColumn( EntityToWorldTransform(), checkCount>>1, move )` with
/// `checkCount` running 0 to 3, so the columns are 0, 0, 1, 1 — **the Z axis
/// is never tried**, and the alternating sign means the four moves are
/// `+½x, −½x, +½y, −½y`.
///
/// Answers whether one of them worked, leaving the entity wherever it got to.
fn nudge_along_pusher_axes(
    pushers: &Pushers,
    query: &mut dyn TouchQuery,
    cx: &mut Context<'_>,
    id: EntityId,
    root: &EntityCore,
) -> bool {
    let Some(origin) = current_origin(cx, id) else {
        return false;
    };
    let frame = root.to_world();
    for check in 0..4 {
        let axis = Vec3::from(frame.matrix3.col(check >> 1));
        let factor = match check & 1 {
            0 => 0.5,
            _ => -0.5,
        };
        set_abs_origin(cx, id, origin + axis * factor);
        if !is_stuck(pushers, query, cx, id) {
            // "Fixing player blocking train!"
            return true;
        }
    }
    false
}

/// `!CPhysicsPushedEntities::IsPushedPositionValid` (`physics_main.cpp:234`) —
/// did this entity end up inside something?
///
/// An unswept trace against **everything, pushers included**, which is the one
/// sweep of the three that can see the door. `startsolid` is the whole answer,
/// so it is returned as it stands and the *name* is inverted instead: Valve's
/// function asks "is it valid" and returns `!startsolid`, and every one of its
/// three call sites then negates that back. Naming the negation is what makes
/// `if !is_stuck(…)` read as the branch it is.
///
/// An entity the list cannot resolve any more is **not** stuck, which is the
/// permissive answer: the push goes through. Valve would dereference a null
/// pointer here, so there is no behaviour to match.
fn is_stuck(
    pushers: &Pushers,
    query: &mut dyn TouchQuery,
    cx: &mut Context<'_>,
    id: EntityId,
) -> bool {
    let Some(entity) = cx.entity(id) else {
        return false;
    };
    let origin = entity.core.origin;
    let (mins, maxs) = (entity.core.model_bounds.mins, entity.core.model_bounds.maxs);
    query
        .push_trace(
            PushClip::Everything,
            origin,
            origin,
            mins,
            maxs,
            &pushers.placements,
        )
        .start_solid
}

/// `pBlocker->SetAbsOrigin( … )`, through the entity list.
///
/// The pusher writes an origin many times per tick and reconciles once, which
/// is why this is [`Context::entities_mut`] rather than `entity_mut` — see
/// that method.
fn set_abs_origin(cx: &mut Context<'_>, id: EntityId, origin: Vec3) {
    if let Some(entity) = cx.entities_mut().get_mut(id) {
        entity.core.set_abs_origin(origin);
    }
}

fn current_origin(cx: &Context<'_>, id: EntityId) -> Option<Vec3> {
    cx.entity(id).map(|entity| entity.core.origin)
}

// ---------------------------------------------------------------------------
// finishing, and putting it all back
// ---------------------------------------------------------------------------

/// `CPhysicsPushedEntities::RegisterBlockage` (`physics_main.cpp:671`) — who
/// stopped us.
///
/// Valve also generates a `PhysicsImpact` against whatever the blocker's own
/// sweep hit; that is a touch link, and this port's touch pass makes the same
/// links a fraction of a tick later. What is left is the name.
fn register_blockage(moved: &[MovedInfo], index: usize) -> EntityId {
    moved[index].id
}

/// `CPhysicsPushedEntities::RestoreEntities` (`physics_main.cpp:692`) — put
/// everything the attempt displaced back where it was.
///
/// Absolute, not local: `SetAbsOrigin( m_vecStartAbsOrigin )`. Nothing this
/// port can push has a parent, and if one ever does then where it was in the
/// world is still what "back" means.
fn restore_entities(moved: &[MovedInfo], cx: &mut Context<'_>) {
    for info in moved {
        set_abs_origin(cx, info.id, info.start_abs_origin);
        cx.note_changed(info.id);
    }
}

/// `CPhysicsPushedEntities::FinishPush` (`physics_main.cpp:605`), reduced to
/// the one thing in it that is not `vphysics` or a touch link this port makes
/// elsewhere: the rotation a turning pusher imparts.
fn finish_push(moved: &[MovedInfo], rotation: Option<&RotatingPushMove>, cx: &mut Context<'_>) {
    for info in moved {
        if let Some(rotation) = rotation {
            finish_rot_pushed_entity(info.id, rotation, cx);
        }
        cx.note_changed(info.id);
    }
}

/// `CPhysicsPushedEntities::FinishRotPushedEntity` (`physics_main.cpp:571`) —
/// **a turning platform turns you with it**.
///
/// Two different things under one name, and the split is the player:
///
/// - A **player** has the whole angular step added to their view.
///   `pl.fixangle = FIXANGLE_RELATIVE; pl.anglechange += rotPushMove.amove`,
///   accumulated rather than assigned because several ticks can run inside one
///   frame — which is exactly the arrangement here, so the accumulation is the
///   port's too, through
///   [`PlayerState::angles`](super::PlayerState) going back out to `client/`
///   once per frame. Valve sets the player's angular *velocity* alongside it;
///   that field is networked to a consumer this port does not have, and it is
///   set anyway so that `ent_dump` shows what the shipped game would.
/// - **Everything else** gets yaw only, on its absolute angles: "only rotate
///   YAW with pushing. Freely rotateable entities should either use VPHYSICS
///   or be set up as children."
fn finish_rot_pushed_entity(id: EntityId, rotation: &RotatingPushMove, cx: &mut Context<'_>) {
    let Some(entity) = cx.entities_mut().get_mut(id) else {
        return;
    };
    match entity.behaviour.is_player() {
        true => {
            entity.core.angular_velocity.y = rotation.amove.y;
            let angles = entity.core.angles + rotation.amove;
            entity.core.set_abs_angles(angles);
        }
        false => {
            let mut angles = entity.core.angles;
            angles.y += rotation.amove.y;
            entity.core.set_abs_angles(angles);
        }
    }
}
