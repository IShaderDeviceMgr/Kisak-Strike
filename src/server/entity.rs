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
use super::io::{EventAction, Output, Variant};
use super::think::TICK_NEVER_THINK;

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
}

/// The state every entity has, whatever its class. `CBaseEntity`'s fields.
///
/// The fields are the ones stages 1 and 2 populate and nothing more. Movement
/// state (`MOVETYPE_*`, `SOLID_*`, velocity) arrives with
/// `portdocs/SERVER.md` stage 3 — an unreachable field is scaffolding, and
/// `PORTING.md` asks for the knowledge without the encoding.
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
    /// `m_iParent` — the `parentname` key, before it is resolved.
    ///
    /// Kept alongside [`parent`](EntityCore::parent) because Valve keeps both:
    /// the name outlives a parent that has not spawned yet, and
    /// `SetupParentsForSpawnList` resolves it once everything exists.
    pub parent_name: Option<String>,
    /// The resolved parent, or `None`. Set by the spawn pass.
    pub parent: Option<EntityId>,
    /// `m_vecAbsOrigin`. Valve's `KeyValue` calls `SetAbsOrigin` here rather
    /// than `SetLocalOrigin`, and asserts that nothing is parented yet —
    /// parenting happens after every key is read, which is why that holds.
    pub origin: Vec3,
    /// `m_angAbsRotation`, as pitch/yaw/roll. [`crate::math::angle_matrix`] is
    /// what turns it into a basis; see `rustdocs/STUDIO.md` on why the
    /// component order is the trap it is.
    pub angles: Vec3,
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
                parent_name: None,
                parent: None,
                origin: Vec3::ZERO,
                angles: Vec3::ZERO,
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
