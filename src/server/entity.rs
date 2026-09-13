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

use super::class::{Behaviour, ClassDef};

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
    /// The slot, for reporting. **Not an identity** — two entities that lived
    /// in the same slot at different times share it. `ent_dump <index>` takes
    /// one because Valve's takes an edict index and mappers think in those.
    pub fn slot(self) -> u32 {
        self.slot
    }
}

/// The state every entity has, whatever its class. `CBaseEntity`'s fields.
///
/// The fields are the ones stage 1 populates and nothing more. Movement state
/// (`MOVETYPE_*`, `SOLID_*`, velocity) arrives with `portdocs/SERVER.md`
/// stage 3 and the think schedule with stage 2 — an unreachable field is
/// scaffolding, and `PORTING.md` asks for the knowledge without the encoding.
pub struct EntityCore {
    /// The class table this entity was built from. `m_iClassname` is
    /// `class.name`, so there is no separate string.
    pub class: &'static ClassDef,
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
    /// This entity's output connections, unparsed.
    ///
    /// `CBaseEntityOutput`'s action list is stage 2; what stage 1 needs is for
    /// an output key to be *recognised* rather than counted as unhandled, so
    /// the raw `target␛input␛parameter␛delay␛times` strings are kept exactly
    /// as they came out of the lump. 61,391 of them across the shipped maps,
    /// and an output key legitimately repeats — one entry per connection.
    pub outputs: Vec<(String, String)>,
    /// Keys no class in the chain declared and no `KeyValue` consumed.
    ///
    /// Valve drops these silently. Keeping them is this port's stage-1
    /// progress metric: the exact list of what the entity system does not
    /// understand yet, per entity, which `report_entities` totals.
    pub unhandled: Vec<(String, String)>,
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

    /// `GetDebugName` (`baseentity.cpp:5380`): the targetname if there is one,
    /// the classname otherwise. What every diagnostic in the original prints.
    pub fn debug_name(&self) -> &str {
        match &self.name {
            Some(name) => name.as_str(),
            None => self.class.name,
        }
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
    /// Valve's `g_pForceAttachEdict` dance around `CreateEntityByName`.
    pub fn insert(&mut self, entity: Entity) -> EntityId {
        self.live += 1;
        match self.free.pop() {
            Some(slot) => {
                let s = &mut self.slots[slot as usize];
                s.generation = s.generation.wrapping_add(1);
                s.entity = Some(entity);
                EntityId {
                    slot,
                    generation: s.generation,
                }
            }
            None => {
                self.slots.push(Slot {
                    entity: Some(entity),
                    generation: 0,
                });
                EntityId {
                    slot: (self.slots.len() - 1) as u32,
                    generation: 0,
                }
            }
        }
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

    /// `UTIL_Remove`: **marks**, and frees nothing.
    ///
    /// An entity being iterated can always be safely removed, which is the
    /// entire reason the original works this way — `physics_main.cpp` even
    /// disables `UTIL_RemoveImmediate` for the duration of the think loop. The
    /// Rust borrow rules want the same thing for the same reason, which is a
    /// rare case of the C++ workaround and the Rust idiom coinciding exactly.
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
}
