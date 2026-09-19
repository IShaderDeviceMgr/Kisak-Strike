//! The entity and the list it lives in.
//!
//! `CBaseEntity`'s shared state (`game/server/baseentity.h`) and
//! `CGlobalEntityList` (`game/server/entitylist.cpp`), which between them are
//! what every entity class in the game is built on.
//!
//! # What an entity is here
//!
//! In the original, `CBaseEntity` is a 2,972-line class and everything else in
//! `game/server/` inherits from it. Here it is [`EntityCore`] — a plain struct
//! of the state *every* entity has — plus a [`Behaviour`] holding the state
//! only its class has (`portdocs/SERVER.md` §7.2). The inheritance chain does
//! **not** survive as data: a class that would derive from another *contains*
//! one instead, and calls into it where the C++ would call `BaseClass`. See
//! [`class`](super::class) for why the datadesc chain walk had nothing left to
//! do.
//!
//! **The split is what makes a behaviour callable at all.** A `Spawn` that
//! mutates the entity it belongs to is `&mut self` and `&mut Entity` at once;
//! two disjoint fields are two borrows. It is the same move
//! `Engine::frame`'s `EngineCommands` already makes, for the same reason.
//!
//! # Handles
//!
//! [`EntityId`] is `CBaseHandle` (`public/basehandle.h`): a slot index and a
//! serial number, so that a handle to a removed entity resolves to nothing
//! rather than to whatever took its slot. Valve packs both into a `u32` —
//! 14 bits of index (`NUM_ENT_ENTRIES`, 16,384) and 16 of serial — because it
//! goes over the wire. Nothing here goes over a wire, so the two are separate
//! fields and the limits go away. The largest shipped Portal 2 map places
//! 1,446 entities.

use std::ops::{Deref, DerefMut};

use glam::Vec3;

use super::class::{Behaviour, ClassDef, Context};
use super::damage::{DamageMode, LifeState};
use super::io::{EventAction, Output, Variant};
use super::movement::{ModelBounds, MoveType, Solid, FSOLID_NOT_SOLID};
use super::think::TICK_NEVER_THINK;
use super::touch::TouchLink;

/// A handle to an entity — Valve's `CBaseHandle`/`EHANDLE`.
///
/// Copy, comparable and hashable, and **it may dangle**: [`EntityList::get`]
/// returns `None` once the entity it named has been removed, which is the
/// whole point of the generation counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId {
    slot: u32,
    generation: u32,
}

impl EntityId {
    /// `INVALID_EHANDLE`. What an entity carries before the list has taken it,
    /// and what [`EntityList::get`] refuses.
    pub const INVALID: EntityId = EntityId {
        slot: u32::MAX,
        generation: u32::MAX,
    };

    /// The slot, for reporting. **Not an identity** — two entities that lived
    /// in the same slot at different times share it. `ent_dump <index>` takes
    /// one because Valve's takes an edict index and mappers think in those.
    pub fn slot(self) -> u32 {
        self.slot
    }

    /// `CBaseHandle::ToInt` — the whole handle as one integer, generation
    /// included.
    ///
    /// **This one *is* an identity**, which is the difference from
    /// [`slot`](EntityId::slot): no two entities in a level's lifetime share
    /// it. It exists so that a module which must not name an `EntityId` can
    /// still key on one — `world/` matches an entity's studio model to the
    /// instance it uploaded by this number, because a `prop_dynamic` can be
    /// killed (556 shipped connections do) and a positional list would then
    /// re-point every instance after it.
    pub fn to_int(self) -> u64 {
        u64::from(self.slot) << 32 | u64::from(self.generation)
    }
}

/// The state every entity has, whatever its class. `CBaseEntity`'s fields.
///
/// The fields are the ones stages 1 to 3 populate and nothing more — an
/// unreachable field is scaffolding, and `PORTING.md` asks for the knowledge
/// without the encoding. Stage 3 added the movement block: a move type, the
/// two velocities, `m_flSpeed`, the local clock and the move-done alarm.
/// `SOLID_*` is still not here, because nothing chooses between the solidity
/// *types*; [`solid_flags`](EntityCore::solid_flags) carries the one bit a
/// class sets.
pub struct EntityCore {
    /// The class table this entity was built from. `m_iClassname` is
    /// `class.name`, so there is no separate string.
    pub class: &'static ClassDef,
    /// This entity's own handle. `CBaseEntity::GetRefEHandle`.
    ///
    /// [`EntityId::INVALID`] until [`EntityList::insert`] takes ownership,
    /// which is the only window in which an entity exists outside the list.
    id: EntityId,
    /// `m_iName` — the `targetname`. `None` rather than `NULL_STRING`.
    pub name: Option<String>,
    /// `m_target` — the `target` key: the *other* entity this one points at.
    ///
    /// A `CBaseEntity` field (`baseentity.cpp:2217`) rather than a class's,
    /// and read by exactly two classes here: `trigger_teleport` and
    /// `point_teleport` both send whatever `target` names to wherever they
    /// are. Measured: those two are the only classnames in the whole game
    /// this port implements that carry the key at all — 73 and 128 of them.
    pub target: Option<String>,
    /// `m_iParent` — the `parentname` key, before it is resolved.
    ///
    /// Kept alongside [`parent`](EntityCore::parent) because Valve keeps both:
    /// the name outlives a parent that has not spawned yet, and
    /// `SetupParentsForSpawnList` resolves it once everything exists.
    pub parent_name: Option<String>,
    /// The resolved parent, or `None`. Set by
    /// [`hierarchy::set_parent`](super::hierarchy::set_parent), which is the
    /// only thing that may write it — a bare assignment would leave the
    /// parent's [`children`](EntityCore::children) list and this entity's
    /// [`local_origin`](EntityCore::local_origin) disagreeing with it.
    parent: Option<EntityId>,
    /// `m_iParentAttachment` — which of the parent's attachment points this
    /// entity rides, rather than the parent's own origin.
    ///
    /// **Zero-based**, where Valve's is one-based so that 0 can mean "none".
    /// `None` is that case, and it is what all but 1,376 of the game's
    /// parented entities are. Written only by
    /// [`hierarchy::set_parent`](super::hierarchy::set_parent), for the same
    /// reason [`parent`](EntityCore::parent) is: the two are one decision.
    parent_attachment: Option<usize>,
    /// Every entity whose [`parent`](EntityCore::parent) is this one.
    ///
    /// `m_hMoveChild`/`m_hMovePeer` (`hierarchy.cpp:21`) flattened: Valve
    /// threads an intrusive list through the children because a `CBaseEntity`
    /// has nowhere to put a `Vec`, and the list is only ever walked front to
    /// back. The order differs — `LinkChild` pushes onto the *front* — and
    /// nothing reads it, because every walk of this list recomputes a
    /// transform and transforms do not care what order siblings come in.
    children: Vec<EntityId>,
    /// `m_vecAbsOrigin` — where this entity is **in the world**.
    ///
    /// Read everywhere; written only by
    /// [`set_abs_placement`](EntityCore::set_abs_placement) and by
    /// [`calc_absolute_position`](EntityCore::calc_absolute_position), because
    /// it is now half of a pair. See
    /// [`local_origin`](EntityCore::local_origin).
    pub origin: Vec3,
    /// `m_angAbsRotation`, as pitch/yaw/roll. [`crate::math::angle_matrix`] is
    /// what turns it into a basis; see `rustdocs/STUDIO.md` on why the
    /// component order is the trap it is.
    pub angles: Vec3,
    /// `m_vecOrigin` — where this entity is **in its parent's frame**.
    ///
    /// Equal to [`origin`](EntityCore::origin) while nothing is parented,
    /// which is why the port got this far without it. The two differ for the
    /// 4,582 entities the shipped maps parent, and **the difference is not
    /// cosmetic for the 201 of those that are movers**: every one of
    /// `CBaseToggle`'s moves is computed in this frame, so a door parented to
    /// a moving platform that read the world pair would drive itself back to a
    /// world position each tick and tear itself off its parent.
    pub local_origin: Vec3,
    /// `m_angRotation` — this entity's orientation in its parent's frame.
    pub local_angles: Vec3,
    /// The parent's `EntityToWorldTransform()`, cached here rather than
    /// fetched — the identity while unparented.
    ///
    /// # Why a cache, when Valve has a dirty flag
    ///
    /// `CBaseEntity` goes the other way: `GetAbsOrigin` checks
    /// `EFL_DIRTY_ABSTRANSFORM` and calls `CalcAbsolutePosition` on the spot,
    /// walking up to the parent through a pointer it has. Neither half of that
    /// survives the port. There is no pointer — a parent is an
    /// [`EntityId`] that only the [`EntityList`] can resolve — and the
    /// dispatched entity is *outside* that list for the whole of its own
    /// handler ([`EntityList::detach`]), so the one moment a mover integrates
    /// its velocity is the one moment it could not look its parent up.
    ///
    /// Caching the parent's frame turns that inside out: **a write to the
    /// local pair recomputes the world pair immediately, from `&mut self`
    /// alone**, and the only operation that needs the list is pushing a change
    /// *down* to the children — [`hierarchy::propagate`](super::hierarchy::propagate),
    /// which is `InvalidatePhysicsRecursive` done eagerly instead of lazily.
    /// The staleness window is the same one Valve has; it is just closed from
    /// the other end.
    parent_to_world: glam::Affine3A,
    /// `m_pPhysicsObject` — this entity's rigid body, if it has one.
    ///
    /// Written only by [`physics::Physics`](super::physics::Physics), which is
    /// the one place that can keep the environment's side of the pairing in
    /// step. A handle to a removed body resolves to nothing rather than to
    /// somebody else's, so a stale one here is inert rather than dangerous.
    pub physics: Option<crate::vphysics::env::BodyId>,
    /// `m_spawnflags`.
    pub spawn_flags: u32,
    /// `m_ModelName` — `"*12"` for a brush model, `"models/…/x.mdl"` for a
    /// studio model. **A name, not a model**: resolving it belongs to
    /// `world/` and `studio/`, and this module must not name either
    /// (`portdocs/SERVER.md` §3).
    pub model: Option<String>,
    /// `m_iHammerID`. The `.vmf` object id, present on 60,884 of the 60,925
    /// entities the shipped maps place — the only reliable way to point at one
    /// entity in Hammer from a bug report.
    pub hammer_id: Option<u32>,
    /// `m_clrRender`, RGBA. `rendercolor` fills RGB and defaults A to 255;
    /// `renderamt` then overwrites A, which is why they are one field.
    pub render_color: [u8; 4],
    /// `m_nRenderMode`/`m_nRenderFX`, as the file spells them. Stored because
    /// `world/` already acts on `rendermode` and will eventually take its copy
    /// from here rather than re-reading the lump.
    pub render_mode: u8,
    pub render_fx: u8,
    /// `m_fEffects` — the `EF_*` bits. Six of them come from map keys; see
    /// [`keyvalue::effects`](super::keyvalue::effects).
    pub effects: u32,
    /// `m_iEFlags` — the `EFL_*` bits. One of them comes from a map key
    /// (`nodamageforces`); the rest are set from code that does not exist yet.
    pub entity_flags: u32,
    /// This entity's outputs and everything connected to them.
    ///
    /// One [`Output`] per *name*, however many times the lump repeats the key
    /// — an output key legitimately repeats, once per connection, and 38,465
    /// of the shipped maps' 38,479 duplicate keys are exactly that.
    pub outputs: Vec<Output>,
    /// Keys no class in the chain declared and no `KeyValue` consumed.
    ///
    /// Valve drops these silently. Keeping them is this port's progress
    /// metric: the exact list of what the entity system does not understand
    /// yet, per entity, which `report_entities` totals.
    pub unhandled: Vec<(String, String)>,
    /// The bounding box of the brush model [`model`](EntityCore::model) names,
    /// or zero. See [`ModelBounds`] — it is the one piece of entity state that
    /// comes from the `.bsp` rather than from the entity lump.
    pub model_bounds: ModelBounds,
    /// `m_MoveType` — how this entity is simulated. Set by a class's `Spawn`,
    /// never by a map key.
    pub move_type: MoveType,
    /// `m_vecVelocity`, in units a second. Integrated by the pusher.
    ///
    /// **Local velocity, used as absolute.** Valve keeps a local/abs pair and
    /// a parent transform; this port has neither, so for the 174 movers that
    /// name a parent the two differ — see
    /// [`movement::perform_push`](super::movement).
    pub velocity: Vec3,
    /// `m_vecAngVelocity`, in degrees a second, as pitch/yaw/roll.
    pub angular_velocity: Vec3,
    /// `m_flSpeed` — the `speed` key, and what `LinearMove`/`AngularMove` are
    /// given. A `CBaseEntity` field rather than a mover's, because
    /// `func_rotating` uses it as its *current* rotation rate rather than as a
    /// setting.
    pub speed: f32,
    /// `m_flLocalTime` — this entity's own clock, advanced only while it is
    /// being pushed.
    ///
    /// Zero at spawn and **not** `curtime`: it is "seconds this pusher has
    /// simulated", which is what lets a blocked one fall behind the world.
    /// Nothing blocks yet, so it tracks the time the entity has spent with a
    /// live alarm.
    pub local_time: f32,
    /// `m_flMoveDoneTime` — when the move in progress ends, **in local time**,
    /// or `-1` for no alarm.
    ///
    /// Private because Valve's getter and setter are not symmetric:
    /// [`set_move_done_time`](EntityCore::set_move_done_time) takes a *delay*
    /// and [`move_done_time`](EntityCore::move_done_time) returns the
    /// *remaining* time, while this field holds neither. Reading it raw is
    /// [`raw_move_done_time`](EntityCore::raw_move_done_time), which only the
    /// pusher wants.
    move_done_time: f32,
    /// `m_vecBaseVelocity` — the velocity of whatever is carrying this entity,
    /// added to its own for one move and then taken back out.
    ///
    /// Written by `trigger_push` and by nothing else here. The player's copy
    /// lives on [`crate::client::Player`] and this one is where the server
    /// puts what the trigger decided; `Engine::frame` carries it across, the
    /// same way it carries a brush entity's placement the other way.
    pub base_velocity: Vec3,
    /// `m_takedamage` — how this entity answers `TakeDamage`. See
    /// [`DamageMode`].
    pub take_damage: DamageMode,
    /// `m_iHealth`.
    ///
    /// A `CBaseEntity` field, and a *map key* (`health`,
    /// `baseentity.cpp:2261`) — which is why it is here rather than on the
    /// player. 682 of the game's entities carry the key and **every one of
    /// them writes `0`**: 346 `func_door_rotating`, 272 `func_door` and 64
    /// `func_button`, the three ported classes whose `Spawn` would make them
    /// shootable at a positive value. So the shootable-door path is dead in
    /// Portal 2 — measured, not assumed.
    pub health: i32,
    /// `m_iMaxHealth` — the ceiling [`take_health`](super::damage::take_health)
    /// refuses to go past. The `max_health` key, which **no shipped Portal 2
    /// entity carries**; `CBasePlayer::Spawn` sets it from `m_iHealth`.
    pub max_health: i32,
    /// `m_lifeState`.
    pub life_state: LifeState,
    /// `m_flDamageAccumulator` — the fraction of a point of damage carried
    /// between hits. See [`take_damage`](super::damage::take_damage) for why
    /// dropping it makes a repeating `trigger_hurt` weaker than the map asked
    /// for.
    pub damage_accumulator: f32,
    /// `m_iszDamageFilterName` — the `damagefilter` key
    /// (`baseentity.cpp:2266`). 27 entities in the game carry it, all of them
    /// props this port has no class for.
    pub damage_filter_name: Option<String>,
    /// `m_hDamageFilter`, resolved from
    /// [`damage_filter_name`](EntityCore::damage_filter_name) by
    /// `CBaseEntity::Activate` (`baseentity.cpp:1782`) and by the
    /// `SetDamageFilter` input.
    pub damage_filter: Option<EntityId>,
    /// `m_Solid` — *how* this entity is solid. See [`Solid`].
    pub solid: Solid,
    /// `m_fFlags`' solidity half — the `FSOLID_*` bits. Two are set from
    /// anywhere: [`FSOLID_NOT_SOLID`], by `func_brush` and by every trigger,
    /// and [`FSOLID_TRIGGER`](super::movement::FSOLID_TRIGGER), by every
    /// trigger.
    pub solid_flags: u32,
    /// `m_fFlags` — the `FL_*` bits.
    ///
    /// [`FL_CLIENT`](super::movement::FL_CLIENT) is the only one set at spawn;
    /// [`FL_BASEVELOCITY`](super::movement::FL_BASEVELOCITY) is set by
    /// `trigger_push` and cleared by the player's move.
    pub flags: u32,
    /// `m_CollisionGroup` — who this entity collides with at all. See
    /// [`CollisionGroup`](super::movement::CollisionGroup) for why there are
    /// two of them rather than twenty-three.
    pub collision_group: super::movement::CollisionGroup,
    /// `m_pBlocker` — what stopped this pusher last tick, or `None`.
    ///
    /// A mover's state rather than the pusher's, because the *edges* are what
    /// `StartBlocked` and `EndBlocked` fire on and an edge needs the previous
    /// answer. Written only by [`push::perform_push`](super::push::perform_push).
    pub blocker: Option<EntityId>,
    /// The entities this one is touching. Valve's `TOUCHLINK` data object
    /// (`game/shared/touchlink.h`), which is a doubly-linked list hung off the
    /// entity by name.
    ///
    /// **Both sides of a touch have a link**, and only one of the two carries
    /// [`TouchLink::start_touch`] — see [`touch`](super::touch) for which and
    /// why. A behaviour reads its own list (`CTriggerHurt::HurtAllTouchers`
    /// is the one that does); everything that *maintains* it is
    /// [`Server`](super::Server)'s, because a touch is a fact about two
    /// entities.
    pub touch_links: Vec<TouchLink>,
    /// `touchStamp` — bumped by `SetCheckUntouch` every time this entity is
    /// about to re-test what it is touching, so that a link left at the old
    /// value is a touch that has ended.
    pub(super) touch_stamp: i32,
    /// `EFL_CHECK_UNTOUCH` — whether this entity owes the post-think pass a
    /// stale-link sweep. Set by `SetCheckUntouch`, cleared by the sweep.
    pub(super) check_untouch: bool,
    /// `m_nNextThinkTick` — the tick this entity's `Think` is due, or
    /// [`TICK_NEVER_THINK`].
    ///
    /// Private because the [`ThinkList`](super::think::ThinkList) has to be
    /// told when it changes, and [`set_next_think`](EntityCore::set_next_think)
    /// is the one place that can happen.
    next_think_tick: i32,
    /// `IsMarkedForDeletion` — `UTIL_Remove` sets it and the entity is freed
    /// by the next `CleanupDeleteList`, never in place. See
    /// [`EntityList::mark_for_deletion`].
    pub removed: bool,
}

impl EntityCore {
    /// `GetClassname`.
    pub fn classname(&self) -> &'static str {
        self.class.name
    }

    /// `GetRefEHandle` — this entity's own handle.
    pub fn id(&self) -> EntityId {
        self.id
    }

    /// `GetDebugName` (`baseentity.cpp:5380`): the targetname if there is one,
    /// the classname otherwise. What every diagnostic in the original prints.
    pub fn debug_name(&self) -> &str {
        match &self.name {
            Some(name) => name.as_str(),
            None => self.class.name,
        }
    }

    /// `HasSpawnFlags`.
    pub fn has_spawn_flags(&self, flags: u32) -> bool {
        self.spawn_flags & flags != 0
    }

    /// `UTIL_Remove( this )`. **Marks; frees nothing** — see
    /// [`EntityList::mark_for_deletion`].
    pub fn remove(&mut self) {
        self.removed = true;
    }

    // -----------------------------------------------------------------------
    // the transform pair — `m_vecOrigin`/`m_vecAbsOrigin` and their angles
    // -----------------------------------------------------------------------
    //
    // Read [`local_origin`](EntityCore::local_origin) and
    // [`parent_to_world`] first. In one line: **the local pair is the truth
    // and the world pair is derived**, every write below re-derives it, and
    // the only thing missing from that sentence is the children — which
    // [`hierarchy::propagate`](super::hierarchy::propagate) does, because it
    // is the only operation here that needs the entity list.

    /// `GetMoveParent()` — the entity this one moves with, if any.
    pub fn parent(&self) -> Option<EntityId> {
        self.parent
    }

    /// `m_iParentAttachment`, zero-based — which of the parent's attachment
    /// points this entity rides, if it rides one rather than the parent
    /// itself.
    pub fn parent_attachment(&self) -> Option<usize> {
        self.parent_attachment
    }

    /// Every entity parented to this one. `FirstMoveChild`/`NextMovePeer`.
    pub fn children(&self) -> &[EntityId] {
        &self.children
    }

    /// `EntityToWorldTransform()` (`m_rgflCoordinateFrame`) — this entity's
    /// own frame, in the world.
    ///
    /// Built rather than cached, unlike Valve's: `angle_matrix` is nine
    /// multiplies and this is asked for once per parenting operation and once
    /// per child per move, not once per bone per frame.
    pub fn to_world(&self) -> glam::Affine3A {
        glam::Affine3A::from_mat3_translation(crate::math::angle_matrix(self.angles), self.origin)
    }

    /// `CalcAbsolutePosition` (`baseentity.cpp:6521`) — rebuild the world pair
    /// from the local pair and the cached parent frame.
    ///
    /// Valve's early-out on `EFL_DIRTY_ABSTRANSFORM` has no counterpart: this
    /// runs *because* something changed rather than in the hope that nothing
    /// did.
    ///
    /// **Both of Valve's exact-copy branches are load-bearing and both are
    /// here.** `MatrixAngles( AngleMatrix( a ) )` is not `a` in `f32`, so
    /// anything that round-trips through the matrix drifts in the last bits
    /// every time it is asked. `CalcAbsolutePosition` avoids it twice:
    ///
    /// - with no move parent it copies the local pair straight across, which
    ///   is the path **every unparented entity in the game takes** — and this
    ///   port has been taking it implicitly since stage 1, because until now
    ///   there was only one pair;
    /// - with a move parent but no rotation of its own it copies the
    ///   *parent's* absolute angles, so a child at rest under a still parent
    ///   does not creep.
    fn calc_absolute_position(&mut self) {
        let Some(_) = self.parent else {
            self.origin = self.local_origin;
            self.angles = self.local_angles;
            return;
        };
        let local = glam::Affine3A::from_mat3_translation(
            crate::math::angle_matrix(self.local_angles),
            self.local_origin,
        );
        let world = self.parent_to_world * local;
        self.origin = Vec3::from(world.translation);
        self.angles = match self.local_angles == Vec3::ZERO {
            true => crate::math::matrix_angles(glam::Mat3::from(self.parent_to_world.matrix3)),
            false => crate::math::matrix_angles(glam::Mat3::from(world.matrix3)),
        };
    }

    /// `SetLocalOrigin` (`baseentity.cpp:6847`) — move this entity within its
    /// parent's frame.
    ///
    /// **This is what a mover writes.** `LinearlyMoveRootEntity`
    /// (`physics_main.cpp:1057`) is `SetLocalOrigin( GetLocalOrigin() +
    /// GetLocalVelocity() * movetime )` and `CBaseToggle::LinearMove` aims at
    /// a local destination, so the whole of `subs.cpp` is in this frame.
    ///
    /// The children are *not* updated here — see
    /// [`hierarchy::propagate`](super::hierarchy::propagate) for why that one
    /// step is separate, and note that a leaf entity (which is almost all of
    /// them) therefore costs nothing extra at all.
    /// > **The equality early-out is Valve's and it is load-bearing.**
    /// > `if (m_vecOrigin != origin)` guards the whole body, so a write of the
    /// > value already there invalidates nothing. Drop it and every pusher in
    /// > the simulation list rebuilds its subtree every tick whether or not it
    /// > moved — and because `MatrixAngles(AngleMatrix(a))` is not `a`, that
    /// > is not merely wasted work: a still parent would walk its children
    /// > sideways by a ten-thousandth of a unit a tick. Measured, on the
    /// > shipped maps: **532 extra brush entities drifting off their spawn
    /// > placement in two seconds** with the guard missing, against 0 with it.
    pub fn set_local_origin(&mut self, origin: Vec3) {
        if self.local_origin == origin {
            return;
        }
        self.local_origin = origin;
        self.calc_absolute_position();
    }

    /// `SetLocalAngles` (`baseentity.cpp:6874`). Same guard, same reason.
    pub fn set_local_angles(&mut self, angles: Vec3) {
        if self.local_angles == angles {
            return;
        }
        self.local_angles = angles;
        self.calc_absolute_position();
    }

    /// `SetAbsOrigin` + `SetAbsAngles` (`baseentity.cpp:6678`, `:6722`) — put
    /// this entity somewhere in the **world**, and back-solve what that means
    /// in its parent's frame.
    ///
    /// The two are one call because every caller here sets both: a teleport,
    /// a spawn placement, the player's position coming back from the client.
    /// Valve splits them because each is a separate network variable and each
    /// one alone is a common operation in code this port does not have.
    pub fn set_abs_placement(&mut self, origin: Vec3, angles: Vec3) {
        if self.origin == origin && self.angles == angles {
            return;
        }
        self.origin = origin;
        self.angles = angles;
        match self.parent {
            None => {
                self.local_origin = origin;
                self.local_angles = angles;
            }
            Some(parent) => {
                let (frame, attachment) = (self.parent_to_world, self.parent_attachment);
                self.set_parent_frame(Some(parent), attachment, frame);
            }
        }
    }

    /// `SetAbsOrigin` alone, for the callers that move an entity without
    /// turning it. [`set_abs_placement`](EntityCore::set_abs_placement) is the
    /// real one; this is the spelling that reads right at a call site.
    pub fn set_abs_origin(&mut self, origin: Vec3) {
        let angles = self.angles;
        self.set_abs_placement(origin, angles);
    }

    /// `SetAbsAngles` alone.
    pub fn set_abs_angles(&mut self, angles: Vec3) {
        let origin = self.origin;
        self.set_abs_placement(origin, angles);
    }

    /// Re-point this entity at a parent frame, keeping its *local* placement
    /// and moving it in the world.
    ///
    /// The direction [`hierarchy::propagate`](super::hierarchy::propagate)
    /// pushes: the parent moved, so the child moves with it.
    ///
    /// Answers whether anything actually changed, which is what lets
    /// `propagate` stop rather than walk a subtree that did not move. It is
    /// the same guard `SetLocalOrigin` has and it is needed for the same
    /// reason: `MatrixAngles(AngleMatrix(a))` is not `a`, so a subtree
    /// recomputed against an unchanged frame does not come back unchanged.
    pub(super) fn follow(&mut self, parent_to_world: glam::Affine3A) -> bool {
        if self.parent_to_world == parent_to_world {
            return false;
        }
        self.parent_to_world = parent_to_world;
        self.calc_absolute_position();
        true
    }

    /// `LinkChild`/`UnlinkChild` (`hierarchy.cpp:21`), for
    /// [`hierarchy`](super::hierarchy) alone — the two halves of the list that
    /// has to stay in step with [`parent`](EntityCore::parent).
    pub(super) fn link_child(&mut self, child: EntityId) {
        self.children.push(child);
    }

    pub(super) fn unlink_child(&mut self, child: EntityId) {
        self.children.retain(|&c| c != child);
    }

    /// The other half of [`follow`](EntityCore::follow): a new parent, and the
    /// **world** placement held still while the local one is re-solved.
    pub(super) fn set_parent_frame(
        &mut self,
        parent: Option<EntityId>,
        attachment: Option<usize>,
        frame: glam::Affine3A,
    ) {
        self.parent = parent;
        self.parent_attachment = attachment;
        self.parent_to_world = frame;
        // Not [`set_abs_placement`](EntityCore::set_abs_placement): its
        // early-out is on the *world* pair, which is exactly the half this
        // holds still. What has to be re-solved is the local pair.
        match parent {
            None => {
                self.local_origin = self.origin;
                self.local_angles = self.angles;
            }
            Some(_) => {
                let world = glam::Affine3A::from_mat3_translation(
                    crate::math::angle_matrix(self.angles),
                    self.origin,
                );
                let local = frame.inverse() * world;
                self.local_origin = Vec3::from(local.translation);
                self.local_angles = crate::math::matrix_angles(glam::Mat3::from(local.matrix3));
            }
        }
    }

    // -----------------------------------------------------------------------
    // outputs
    // -----------------------------------------------------------------------

    /// The named output, if this entity has any connections on it.
    ///
    /// An output with no connections is absent rather than empty, which is
    /// what makes [`output_count`](EntityCore::output_count) answer
    /// `NumberOfElements` without allocating one [`Output`] per declared name
    /// per entity — 30 of them on every one of the game's 1,184
    /// `func_instance_io_proxy`s, for instance.
    pub fn output(&self, name: &str) -> Option<&Output> {
        self.outputs
            .iter()
            .find(|output| output.name.eq_ignore_ascii_case(name))
    }

    /// `CBaseEntityOutput::NumberOfElements` — how many connections this
    /// output has. `logic_relay`'s `Activate` asks, and schedules a think only
    /// if the answer is not zero.
    pub fn output_count(&self, name: &str) -> usize {
        self.output(name).map_or(0, Output::len)
    }

    /// `CBaseEntityOutput::GetMaxDelay`.
    pub fn max_output_delay(&self, name: &str) -> f32 {
        self.output(name).map_or(0.0, Output::max_delay)
    }

    /// Records one connection, from one output key in the entity lump.
    ///
    /// `name` is the name the **class declared**, not the spelling the map
    /// used, so that `oncase02` and `OnCase02` land on one output. Valve gets
    /// that for free by matching the datadesc field with `stricmp`.
    pub(super) fn add_connection(&mut self, name: &str, value: &str, id: u32) {
        let action = EventAction::parse(value, id);
        match self
            .outputs
            .iter_mut()
            .find(|output| output.name.eq_ignore_ascii_case(name))
        {
            Some(output) => output.add(action),
            None => {
                let mut output = Output::new(name);
                output.add(action);
                self.outputs.push(output);
            }
        }
    }

    /// `CBaseEntityOutput::FireOutput` (`cbase.cpp:262`) — post every
    /// connection on `name` to the event queue.
    ///
    /// **It does not call anything.** That is the property the whole module
    /// rests on; see [`Context`].
    ///
    /// `caller` is `Some(self.id())` for every class but one:
    /// `CFuncInstanceIoProxy` forwards the caller it was given, so a chain
    /// through a proxy looks to `!self` and `CancelEvents` as though the
    /// proxy were not there. It is passed explicitly rather than defaulted so
    /// that the one divergence is visible at the call site.
    ///
    /// # The extra delay is dropped for an overridden parameter
    ///
    /// > The no-override branch posts at `ev->m_flDelay + fDelay`; the
    /// > override branch posts at `ev->m_flDelay` alone (`cbase.cpp:280`
    /// > against `:289`). Valve's `fDelay` argument is therefore silently
    /// > ignored for any connection whose mapper typed a parameter. This is
    /// > almost certainly a bug and it is reproduced —
    /// > `portdocs/SERVER.md` §4.3 asked for it, on the grounds that
    /// > reproducing it is cheaper than discovering later that some door in
    /// > Chapter 4 is out of sync.
    pub fn fire_output(
        &mut self,
        name: &str,
        value: Variant,
        activator: Option<EntityId>,
        caller: Option<EntityId>,
        delay: f32,
        cx: &mut Context<'_>,
    ) {
        let Some(at) = self
            .outputs
            .iter()
            .position(|output| output.name.eq_ignore_ascii_case(name))
        else {
            return;
        };

        // The list is walked by index because `times_to_fire` may delete the
        // entry that was just posted, and because posting borrows `cx` while
        // the action borrows `self`.
        let mut i = 0;
        while i < self.outputs[at].actions.len() {
            let action = &self.outputs[at].actions[i];
            let (posted_value, posted_delay) = match &action.parameter {
                None => (value.clone(), action.delay + delay),
                Some(parameter) => (Variant::String(parameter.clone()), action.delay),
            };
            let (target, input, output_id) =
                (action.target.clone(), action.input.clone(), action.id);
            cx.post_named(
                &target,
                &input,
                posted_value,
                posted_delay,
                activator,
                caller,
                output_id,
            );

            // `m_nTimesToFire` counts down and the connection deletes itself
            // at zero, so an action list is mutable state rather than a
            // parsed constant. 2,925 shipped connections depend on it.
            let action = &mut self.outputs[at].actions[i];
            if action.times_to_fire != super::io::EVENT_FIRE_ALWAYS {
                action.times_to_fire -= 1;
                if action.times_to_fire == 0 {
                    self.outputs[at].actions.remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }

    /// `g_EventQueue.AddEvent( this, input, delay, this, this )` — post an
    /// input at *this* entity by handle.
    ///
    /// `logic_relay`'s re-fire latch is the only stage-2 caller, and the
    /// handle form matters there: a relay with no `targetname` (two of the
    /// game's 8,082) must still be able to re-enable itself.
    pub fn post_to_self(&mut self, input: &str, delay: f32, cx: &mut Context<'_>) {
        let me = self.id;
        cx.post_entity(me, input, Variant::Void, delay, Some(me), Some(me));
    }

    /// `g_EventQueue.CancelEvents( this )` — drop every event this entity
    /// posted and has not yet delivered.
    pub fn cancel_pending(&self, cx: &mut Context<'_>) -> usize {
        cx.cancel_from(self.id)
    }

    // -----------------------------------------------------------------------
    // solidity
    // -----------------------------------------------------------------------

    /// `IsSolid()` (`public/const.h:249`) — a solid *type* and the
    /// [`FSOLID_NOT_SOLID`] bit clear.
    ///
    /// > **Both halves are needed and neither is redundant.** A trigger is
    /// > `SOLID_BSP` *and* `FSOLID_NOT_SOLID`: it has a collision model, so
    /// > the touch query can sweep against its brushes, and it is not solid,
    /// > so walking into one does not stop you. Reading only the type makes
    /// > every trigger a wall; reading only the bit makes every point entity
    /// > a wall.
    pub fn is_solid(&self) -> bool {
        self.solid != Solid::None && self.solid_flags & FSOLID_NOT_SOLID == 0
    }

    /// `GetColorModulation()` and `CClientAlphaProperty::ComputeRenderAlpha()`
    /// as one vector — `m_DiffuseModulation`, which every model and brush draw
    /// is multiplied by (`modelrendersystem.cpp:1723`).
    ///
    /// The alpha is the whole of the render-mode question for a model: a mode
    /// other than `kRenderNormal` substitutes `renderamt` for the 255 an
    /// ordinary entity gets (`clientalphaproperty.cpp:239`), and a value below
    /// 255 is what puts the entity in the translucent list. **The mode does not
    /// pick a blend equation** — that was GoldSrc; in Source the equation comes
    /// from the material, and the only mode that does anything else is a glow,
    /// which switches the depth test off (`clientalphaproperty.h:112`) and which
    /// no ported class in Portal 2 sets.
    ///
    /// `world/`'s [`PlacedBrushModel::modulation`] is the same arithmetic over
    /// the same three keys read from the `.bsp` lump instead of from here,
    /// because a brush entity's placement is resolved before the game has
    /// spawned anything.
    ///
    /// [`PlacedBrushModel::modulation`]: crate::engine::world::PlacedBrushModel::modulation
    pub fn modulation(&self) -> [f32; 4] {
        let alpha = match self.render_mode {
            0 => 255,
            _ => self.render_color[3],
        };
        [
            f32::from(self.render_color[0]) / 255.0,
            f32::from(self.render_color[1]) / 255.0,
            f32::from(self.render_color[2]) / 255.0,
            f32::from(alpha) / 255.0,
        ]
    }

    /// `g_pGameRules->ShouldCollide( COLLISION_GROUP_PLAYER,
    /// GetCollisionGroup() )` — the one pairwise rule that survives.
    ///
    /// Read in two places and they have to agree, which is the reason it is
    /// one method: the clip chain the player's move is traced against
    /// (`Server::brush_entity` → `world::Placement::solid`), and the pusher's
    /// candidate filter, which refuses to push what it cannot collide with.
    /// A door the player walks through must not shove them, and a door that
    /// shoves them must not be walked through.
    pub fn collides_with_player(&self) -> bool {
        self.collision_group != super::movement::CollisionGroup::PassableDoor
    }

    /// `IsPointSized()` (`baseentity.h:2178`) — `CollisionProp()->BoundingRadius()
    /// == 0.0f`.
    ///
    /// The pusher's early-out: a point-sized blocker is moved and then
    /// believed, because there is no box to be stuck in. Every point entity in
    /// the game answers `true` here, which is what
    /// [`ModelBounds`](super::movement::ModelBounds)' zero default is for.
    pub fn is_point_sized(&self) -> bool {
        self.model_bounds.mins == Vec3::ZERO && self.model_bounds.maxs == Vec3::ZERO
    }

    /// The `.bsp` model lump index this entity's `model` key names, if it
    /// names one — `"*12"` is `Some(12)`.
    ///
    /// **Model 0 is excluded.** It is the world, which `worldspawn` names and
    /// which is not a placement; the same exclusion `Server::level_init` and
    /// `world::find_brush_models` both make, spelled once.
    pub fn brush_model_index(&self) -> Option<usize> {
        self.model
            .as_deref()
            .and_then(|name| name.strip_prefix('*'))
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|&i| i != 0)
    }

    /// `CCollisionProperty::WorldSpaceAABB` (`collisionproperty.cpp:756`) —
    /// this entity's own box, bounded in world space.
    ///
    /// For an unturned entity that is the box translated, which is the branch
    /// almost every brush entity in a map takes; for a turned one it is
    /// `TransformAABB`, the eight corners through
    /// [`to_world`](EntityCore::to_world) and re-bounded — deliberately not
    /// the tighter `CollisionAABBToWorldAABB` centre-and-extents form, which
    /// is the same box by a cheaper route and which this is not hot enough to
    /// need.
    ///
    /// Used as the pusher's broadphase, where Valve uses a spatial partition
    /// (`::partition->EnumerateElementsInBox`); see [`push`](super::push).
    pub fn world_space_aabb(&self) -> (Vec3, Vec3) {
        let (mins, maxs) = (self.model_bounds.mins, self.model_bounds.maxs);
        if self.angles == Vec3::ZERO {
            return (self.origin + mins, self.origin + maxs);
        }
        let frame = self.to_world();
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { mins.x } else { maxs.x },
                if i & 2 == 0 { mins.y } else { maxs.y },
                if i & 4 == 0 { mins.z } else { maxs.z },
            );
            let world = frame.transform_point3(corner);
            lo = lo.min(world);
            hi = hi.max(world);
        }
        (lo, hi)
    }

    /// `IsSolidFlagSet`. Takes a mask and asks whether *any* of it is set,
    /// which is what the C++'s `( m_usSolidFlags & flags ) != 0` does.
    pub fn is_solid_flag_set(&self, flags: u32) -> bool {
        self.solid_flags & flags != 0
    }

    /// `AddSolidFlags`.
    pub fn add_solid_flags(&mut self, flags: u32) {
        self.solid_flags |= flags;
    }

    /// `RemoveSolidFlags`.
    pub fn remove_solid_flags(&mut self, flags: u32) {
        self.solid_flags &= !flags;
    }

    /// `GetFlags() & flags`, for the `FL_*` set.
    pub fn has_flags(&self, flags: u32) -> bool {
        self.flags & flags != 0
    }

    // -----------------------------------------------------------------------
    // damage
    // -----------------------------------------------------------------------

    /// `CBaseEntity::IsAlive` (`baseentity.h`).
    ///
    /// > **It is the *life state*, not the health.** An entity is still alive
    /// > at zero health for the window between the subtraction and
    /// > `Event_Killed`, and `CGameMovement::IsDead` asks the *other* question
    /// > — `m_iHealth <= 0` (`gamemovement.cpp:1091`). The two disagree for
    /// > exactly that window, which is why the client is handed the health and
    /// > not this.
    pub fn is_alive(&self) -> bool {
        self.life_state == LifeState::Alive
    }

    // -----------------------------------------------------------------------
    // moving
    // -----------------------------------------------------------------------

    /// `CBaseEntity::SetMoveDoneTime` (`baseentity.cpp:3150`) — arm the
    /// arrival alarm `delay` seconds of *local* time from now, or disarm it
    /// with anything negative.
    ///
    /// > **This is not the think schedule.** It is a second timer with its own
    /// > field, and a mover uses both at once
    /// > (`portdocs/SERVER.md` §4.7). It is also not quantised: unlike
    /// > [`set_next_think`](EntityCore::set_next_think) it keeps the float it
    /// > was given, because the pusher lands *on* it by shortening the last
    /// > step rather than by rounding to a tick.
    ///
    /// > **A delay of exactly zero arms an alarm that can never fire**, and
    /// > takes the entity straight out of the simulation list with it — see
    /// > [`will_simulate_game_physics`](EntityCore::will_simulate_game_physics).
    /// > Valve's, and four of the shipped game's `func_door_rotating`s
    /// > (`wait 0`) stand open for ever because of it.
    pub fn set_move_done_time(&mut self, delay: f32) {
        self.move_done_time = match delay >= 0.0 {
            true => self.local_time + delay,
            false => -1.0,
        };
    }

    /// `CBaseEntity::GetMoveDoneTime` (`baseentity.h:2479`) — how much local
    /// time is left before the alarm, or `-1` if none is armed.
    ///
    /// **Not what [`set_move_done_time`](EntityCore::set_move_done_time) was
    /// given**, once any time has passed: the getter subtracts the local
    /// clock, which is what makes `min(remaining, frametime)` the pusher's
    /// whole step calculation.
    pub fn move_done_time(&self) -> f32 {
        match self.move_done_time >= 0.0 {
            true => self.move_done_time - self.local_time,
            false => -1.0,
        }
    }

    /// The alarm as stored — absolute local time, or `-1`.
    ///
    /// `PerformPush`'s test is against this rather than against the remaining
    /// time (`physics_main.cpp:1685`), and the two differ at exactly one
    /// value: an alarm armed for zero delay at local time zero.
    pub(super) fn raw_move_done_time(&self) -> f32 {
        self.move_done_time
    }

    /// `CBaseEntity::WillSimulateGamePhysics`
    /// (`baseentity_shared.cpp:1194`) — whether this entity needs a slot in
    /// the simulation list for movement, as opposed to for thinking.
    ///
    /// A `MOVETYPE_PUSH` entity qualifies only while its alarm is in the
    /// future, which is what keeps 11,000 motionless brush entities out of the
    /// per-tick loop. `MOVETYPE_NONE` never qualifies, and **neither does
    /// `MOVETYPE_VPHYSICS`** — a physics prop is moved by `PhysFrame`, after
    /// the simulation list has been walked, so putting one in that list would
    /// buy a `PhysicsNone` call and nothing else.
    pub fn will_simulate_game_physics(&self) -> bool {
        match self.move_type {
            MoveType::None
            | MoveType::Walk
            | MoveType::Noclip
            | MoveType::FlyGravity
            | MoveType::VPhysics => false,
            MoveType::Push => self.move_done_time() > 0.0,
        }
    }

    // -----------------------------------------------------------------------
    // thinking
    // -----------------------------------------------------------------------

    /// `CBaseEntity::SetNextThink` (`baseentity_shared.cpp:952`).
    ///
    /// > **The time is quantised to a tick, rounded to nearest**, and
    /// > [`NEVER_THINK`](super::class::NEVER_THINK) (`-1`) is compared before
    /// > the conversion, so it cancels rather than scheduling a think in the
    /// > past. `Time::time_to_ticks` has what that rounding costs.
    ///
    /// The named think *contexts* Valve's version takes are not ported:
    /// `m_aThinkFunctions` exists so that one entity can run several
    /// independent timers, and no class implemented so far has two. The
    /// condition that brings them back is a class that needs a second
    /// schedule alongside its base think — note that stage 3's
    /// `SetMoveDoneTime` is *not* one, it is a separate alarm with its own
    /// field, and conflating the two is the mistake that breaks doors that
    /// think while moving (`portdocs/SERVER.md` §4.7).
    pub fn set_next_think(&mut self, time: f32, cx: &Context<'_>) {
        self.next_think_tick = match time == super::class::NEVER_THINK {
            true => TICK_NEVER_THINK,
            false => cx.time.time_to_ticks(time),
        };
    }

    /// `CBaseEntity::GetNextThink` — when this entity next thinks, in seconds,
    /// or [`NEVER_THINK`](super::class::NEVER_THINK).
    ///
    /// Round-trips through the tick, so it is **not** the time that was passed
    /// to [`set_next_think`](EntityCore::set_next_think): at 64 Hz,
    /// `set_next_think(curtime + 0.2)` then `next_think()` comes back
    /// 0.203125 later, not 0.2. `logic_timer`'s `AddToTimer` reads it and adds
    /// to it, so the drift is Valve's and is observable.
    pub fn next_think(&self, cx: &Context<'_>) -> f32 {
        match self.next_think_tick == TICK_NEVER_THINK {
            true => super::class::NEVER_THINK,
            false => cx.time.ticks_to_time(self.next_think_tick),
        }
    }

    /// The raw schedule, for the [`ThinkList`](super::think::ThinkList).
    pub(super) fn next_think_tick(&self) -> i32 {
        self.next_think_tick
    }

    /// Clears the schedule without a [`Context`]. `PhysicsRunSpecificThink`
    /// does exactly this before dispatching, which is why a think that does
    /// not re-arm itself never runs again.
    pub(super) fn clear_next_think(&mut self) {
        self.next_think_tick = TICK_NEVER_THINK;
    }
}

/// One entity: the shared state, and the class's own.
///
/// [`Deref`]s to [`EntityCore`] so that `entity.origin` reads the way it does
/// in the C++, while `entity.behaviour` stays a separate field that can be
/// borrowed at the same time.
pub struct Entity {
    pub core: EntityCore,
    pub behaviour: Box<dyn Behaviour>,
}

impl Deref for Entity {
    type Target = EntityCore;

    fn deref(&self) -> &EntityCore {
        &self.core
    }
}

impl DerefMut for Entity {
    fn deref_mut(&mut self) -> &mut EntityCore {
        &mut self.core
    }
}

impl Entity {
    /// A fresh entity of `class`, with the shared state at its defaults.
    pub fn new(class: &'static ClassDef) -> Entity {
        Entity {
            core: EntityCore {
                class,
                id: EntityId::INVALID,
                name: None,
                target: None,
                parent_name: None,
                parent: None,
                parent_attachment: None,
                children: Vec::new(),
                origin: Vec3::ZERO,
                angles: Vec3::ZERO,
                local_origin: Vec3::ZERO,
                local_angles: Vec3::ZERO,
                parent_to_world: glam::Affine3A::IDENTITY,
                physics: None,
                spawn_flags: 0,
                model: None,
                hammer_id: None,
                // `color32` has no documented default; every path that reads
                // it has been through `rendercolor` first, and white is what
                // an unset one behaves as.
                render_color: [255, 255, 255, 255],
                render_mode: 0,
                render_fx: 0,
                effects: 0,
                entity_flags: 0,
                model_bounds: ModelBounds::default(),
                move_type: MoveType::None,
                velocity: Vec3::ZERO,
                angular_velocity: Vec3::ZERO,
                // `m_flSpeed` is zero-initialised in the C++ too, and every
                // mover's `Spawn` substitutes its own default for a zero —
                // 100 for a door, 40 for a button.
                speed: 0.0,
                local_time: 0.0,
                move_done_time: -1.0,
                base_velocity: Vec3::ZERO,
                take_damage: DamageMode::No,
                health: 0,
                max_health: 0,
                life_state: LifeState::Alive,
                damage_accumulator: 0.0,
                damage_filter_name: None,
                damage_filter: None,
                solid: Solid::None,
                solid_flags: 0,
                flags: 0,
                collision_group: super::movement::CollisionGroup::None,
                blocker: None,
                touch_links: Vec::new(),
                touch_stamp: 0,
                check_untouch: false,
                outputs: Vec::new(),
                unhandled: Vec::new(),
                next_think_tick: TICK_NEVER_THINK,
                removed: false,
            },
            behaviour: (class.create)(),
        }
    }
}

/// One slot in the list. `CEntInfo`.
struct Slot {
    entity: Option<Entity>,
    /// Bumped every time the slot is filled, so a handle to the previous
    /// occupant stops resolving. Valve bumps it on *free*; either works, and
    /// bumping on fill leaves a never-reused slot at generation 0.
    generation: u32,
}

/// The entity list. `CGlobalEntityList`/`gEntList`.
///
/// A generational arena. Valve's version is a fixed array of `NUM_ENT_ENTRIES`
/// with an intrusive linked list threaded through it so that iteration skips
/// holes; a `Vec` of `Option` is the same structure without the list, and
/// iteration filters instead.
#[derive(Default)]
pub struct EntityList {
    slots: Vec<Slot>,
    /// Slots whose entity has been freed. Valve reuses the *lowest* free
    /// edict because the index goes over the wire and low indices are cheaper
    /// to encode; nothing here depends on which, so a stack is fine.
    free: Vec<u32>,
    /// How many entities are live. Kept rather than counted, because the level
    /// summary and `report_entities` both want it.
    live: usize,
}

impl EntityList {
    pub fn new() -> EntityList {
        EntityList::default()
    }

    /// `CreateEntityByName` plus the list insert.
    ///
    /// An entity is never outside the list in this port, which is what removes
    /// Valve's `g_pForceAttachEdict` dance around `CreateEntityByName`. The
    /// handle is written back into the entity, because everything an entity
    /// does to the world — firing an output, posting to itself, cancelling —
    /// names itself as the caller.
    pub fn insert(&mut self, mut entity: Entity) -> EntityId {
        self.live += 1;
        let id = match self.free.pop() {
            Some(slot) => {
                let generation = {
                    let s = &mut self.slots[slot as usize];
                    s.generation = s.generation.wrapping_add(1);
                    s.generation
                };
                EntityId { slot, generation }
            }
            None => EntityId {
                slot: self.slots.len() as u32,
                generation: 0,
            },
        };
        entity.core.id = id;
        match self.slots.get_mut(id.slot as usize) {
            Some(slot) => slot.entity = Some(entity),
            None => self.slots.push(Slot {
                entity: Some(entity),
                generation: 0,
            }),
        }
        id
    }

    /// Resolves a handle. `CBaseHandle::Get`.
    pub fn get(&self, id: EntityId) -> Option<&Entity> {
        let slot = self.slots.get(id.slot as usize)?;
        match slot.generation == id.generation {
            true => slot.entity.as_ref(),
            false => None,
        }
    }

    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut Entity> {
        let slot = self.slots.get_mut(id.slot as usize)?;
        match slot.generation == id.generation {
            true => slot.entity.as_mut(),
            false => None,
        }
    }

    /// Whether this handle still resolves.
    pub fn is_alive(&self, id: EntityId) -> bool {
        self.get(id).is_some()
    }

    /// Lifts one entity out of the list, leaving the slot empty and the
    /// generation alone.
    ///
    /// # This is the borrow seam, and it is the shape `rustdocs/SERVER.md` predicted
    ///
    /// Stage 2 shipped a [`Context`] that could not see the entity list at
    /// all, and recorded that the condition for changing that was "a handler
    /// that must *read* another entity during dispatch", with the shape to
    /// reach for being "the entity list minus the one entity being dispatched,
    /// not a `RefCell`". Stage 4 is that condition — a trigger has to ask its
    /// `filter_*` entity whether the toucher passes, and then hand the toucher
    /// a push or a teleport — and this pair is that shape, literally: the
    /// dispatched entity is *owned by the stack frame running it*, so the rest
    /// of the list is free to be borrowed however the handler likes.
    ///
    /// The consequence a caller must know is that **an entity cannot see
    /// itself through its `Context`** for the duration of its own handler:
    /// `cx.entity(self.id())` is `None`. Nothing wants to — it already has
    /// `&mut EntityCore` — and a class that reached for it would be asking for
    /// two mutable borrows of one entity, which is the bug this prevents
    /// rather than a limitation it imposes.
    ///
    /// `live` is decremented so that [`len`](EntityList::len) never disagrees
    /// with [`iter`](EntityList::iter).
    pub(super) fn detach(&mut self, id: EntityId) -> Option<Entity> {
        let slot = self.slots.get_mut(id.slot as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        let entity = slot.entity.take()?;
        self.live -= 1;
        Some(entity)
    }

    /// Puts back what [`detach`](EntityList::detach) took.
    pub(super) fn attach(&mut self, id: EntityId, entity: Entity) {
        if let Some(slot) = self.slots.get_mut(id.slot as usize) {
            debug_assert!(slot.entity.is_none(), "attaching over a live slot");
            slot.entity = Some(entity);
            self.live += 1;
        }
    }

    /// `UTIL_Remove`: **marks**, and frees nothing.
    ///
    /// An entity being iterated can always be safely removed, which is the
    /// entire reason the original works this way — `physics_main.cpp` even
    /// disables `UTIL_RemoveImmediate` for the duration of the think loop. The
    /// Rust borrow rules want the same thing for the same reason, which is a
    /// rare case of the C++ workaround and the Rust idiom coinciding exactly.
    ///
    /// The by-handle form. A class removing *itself* calls
    /// [`EntityCore::remove`] instead, which is the same flag and the only
    /// form stage 2 reaches — everything that removes an entity here is that
    /// entity's own `Spawn`, `Think` or `Kill` handler.
    #[allow(dead_code)]
    pub fn mark_for_deletion(&mut self, id: EntityId) {
        if let Some(entity) = self.get_mut(id) {
            entity.removed = true;
        }
    }

    /// `CGlobalEntityList::CleanupDeleteList`. Returns how many were freed.
    pub fn cleanup_delete_list(&mut self) -> usize {
        let mut freed = 0;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let Some(entity) = &slot.entity else { continue };
            if !entity.removed {
                continue;
            }
            slot.entity = None;
            self.free.push(i as u32);
            freed += 1;
        }
        self.live -= freed;
        freed
    }

    /// How many entities are live.
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Every live entity, in slot order. `FirstEnt`/`NextEnt`.
    pub fn iter(&self) -> impl Iterator<Item = (EntityId, &Entity)> {
        self.slots.iter().enumerate().filter_map(|(i, slot)| {
            let entity = slot.entity.as_ref()?;
            Some((
                EntityId {
                    slot: i as u32,
                    generation: slot.generation,
                },
                entity,
            ))
        })
    }

    /// `LevelShutdownPostEntity`: drop everything.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free.clear();
        self.live = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::classes;

    fn entity() -> Entity {
        Entity::new(classes::lookup("info_target").expect("info_target is registered"))
    }

    #[test]
    fn a_handle_to_a_removed_entity_stops_resolving() {
        let mut list = EntityList::new();
        let id = list.insert(entity());
        assert!(list.get(id).is_some());

        list.mark_for_deletion(id);
        // Still resolvable until the list is cleaned: a think that removes an
        // entity must be able to finish running against it.
        assert!(list.get(id).is_some(), "removal is deferred, not immediate");
        assert_eq!(list.cleanup_delete_list(), 1);
        assert!(list.get(id).is_none());
    }

    #[test]
    fn a_reused_slot_does_not_answer_the_old_handle() {
        let mut list = EntityList::new();
        let first = list.insert(entity());
        list.mark_for_deletion(first);
        list.cleanup_delete_list();

        let second = list.insert(entity());
        assert_eq!(
            first.slot(),
            second.slot(),
            "the free slot should be the one reused"
        );
        assert_ne!(first, second);
        assert!(
            list.get(first).is_none(),
            "the stale handle must not resolve"
        );
        assert!(list.get(second).is_some());
    }

    /// Everything an entity does names itself as the caller, so the handle has
    /// to be on the entity and has to survive a slot reuse.
    #[test]
    fn an_entity_knows_its_own_handle() {
        let mut list = EntityList::new();
        assert_eq!(entity().core.id(), EntityId::INVALID);

        let first = list.insert(entity());
        assert_eq!(list.get(first).unwrap().id(), first);

        list.mark_for_deletion(first);
        list.cleanup_delete_list();
        let second = list.insert(entity());
        assert_eq!(list.get(second).unwrap().id(), second);
        assert_ne!(list.get(second).unwrap().id(), first);
    }

    #[test]
    fn the_live_count_tracks_inserts_and_cleanups() {
        let mut list = EntityList::new();
        assert!(list.is_empty());
        let ids: Vec<_> = (0..4).map(|_| list.insert(entity())).collect();
        assert_eq!(list.len(), 4);

        list.mark_for_deletion(ids[1]);
        list.mark_for_deletion(ids[2]);
        assert_eq!(list.len(), 4, "marked is not freed");
        assert_eq!(list.cleanup_delete_list(), 2);
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().count(), 2);
        assert_eq!(list.cleanup_delete_list(), 0, "nothing left to free");
    }

    /// One output name, however many times the lump repeats the key.
    #[test]
    fn repeated_output_keys_collect_onto_one_output() {
        let mut entity = entity();
        entity.add_connection("OnUser1", "a\u{1b}Trigger\u{1b}\u{1b}0\u{1b}-1", 1);
        entity.add_connection("OnUser1", "b\u{1b}Trigger\u{1b}\u{1b}0\u{1b}-1", 2);
        entity.add_connection("OnUser2", "c\u{1b}Trigger\u{1b}\u{1b}0\u{1b}-1", 3);

        assert_eq!(entity.outputs.len(), 2);
        assert_eq!(entity.output_count("OnUser1"), 2);
        assert_eq!(entity.output_count("onuser1"), 2, "case insensitive");
        assert_eq!(entity.output_count("OnUser3"), 0);
    }
}
