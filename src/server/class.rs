//! What a class *is*: the table, the trait, and what a behaviour may reach.
//!
//! `datamap_t` (`public/datamap.h`) and `LINK_ENTITY_TO_CLASS`
//! (`public/tier1/interface.h` via `EntityFactoryDictionary`), which between
//! them are how the original turns the string `"logic_relay"` into an object
//! with keys, inputs and outputs.
//!
//! # The inheritance tree does not survive, and does not need to
//!
//! `portdocs/SERVER.md` §7.3 expected [`ClassDef`] to carry a `parent` pointer
//! so that the port could reproduce the `for ( datamap_t *dmap =
//! GetDataDescMap(); dmap; dmap = dmap->baseMap )` walk that `KeyValue` and
//! `AcceptInput` do. **Writing it showed that walk dissolving into ordinary
//! composition.** A `CEnvLight` is a `CLight` in C++ and its `KeyValue` falls
//! through to `CLight::KeyValue` via `BaseClass`; here an
//! [`EnvLight`](super::classes::EnvLight) *holds* a [`Light`](super::classes::Light)
//! and calls `self.light.key_value(..)` as its last line. Same order, same
//! result, one less indirection, and the compiler checks it. So there is no
//! `parent` field: what a class inherits, it contains.
//!
//! Stage 2 confirms this for the *input* half, which §7.3 had not yet reached:
//! `AcceptInput`'s chain walk becomes exactly two steps — the class, then
//! [`base_input`] — because the only thing every entity shares is
//! `CBaseEntity` itself.
//!
//! What remains as data is the *declaration* — the key, input and output
//! names — and that is genuinely needed. An output key has to be recognised as
//! an output rather than counted as an unknown key; an input name has to carry
//! the [`FieldType`] its handler expects, because `AcceptInput` converts
//! before it dispatches.

use std::any::Any;

use super::entity::{Entity, EntityCore, EntityId, EntityList};
use super::io::{Event, EventQueue, FieldType, Input, Target, Variant};
use super::name;
use super::random::RandomStream;
use super::think::{Time, TICK_NEVER_THINK};

/// One input a class accepts. `DEFINE_INPUTFUNC( fieldType, "Name", handler )`.
///
/// The field type is not decoration: `AcceptInput` runs
/// [`Variant::convert`](super::io::Variant::convert) against it *before*
/// calling the handler, which is what lets a map write
/// `SetAutoExposureMax 1.5` — a string — and have a float handler read a
/// float. A declaration with the wrong type here is a silently wrong value,
/// not a compile error, so it is checked against the implementation by
/// `classes`' invariant test.
pub struct InputDef {
    pub name: &'static str,
    pub field: FieldType,
}

impl InputDef {
    pub const fn new(name: &'static str, field: FieldType) -> InputDef {
        InputDef { name, field }
    }
}

/// What [`ClassDef::inputs`] is, spelled once. The class files build these
/// tables and `classes`' table points at them.
pub type InputDefs = &'static [InputDef];

/// One entity class: a name, its declared keys, inputs and outputs, and how to
/// make its state. `LINK_ENTITY_TO_CLASS` plus the `BEGIN_DATADESC` block.
pub struct ClassDef {
    /// The classname as the entity lump spells it. `m_iClassname`.
    pub name: &'static str,
    /// The keys this class's [`Behaviour::key_value`] consumes.
    ///
    /// **The shared ones are not repeated here.** `targetname`, `origin`,
    /// `angles` and the rest are consumed by
    /// [`keyvalue::base_key_value`](super::keyvalue::base_key_value), which is
    /// `CBaseEntity::KeyValue`'s if-ladder and runs second, exactly as it does
    /// in the original.
    ///
    /// Declaration only: consuming a key is `key_value`'s job, and the two
    /// are checked against each other by `classes`' invariant test. That test
    /// is this field's only reader, which is the point — it is what stops the
    /// table and the code drifting apart as classes are added.
    #[allow(dead_code)]
    pub keys: &'static [&'static str],
    /// The inputs this class's [`Behaviour::accept_input`] handles.
    ///
    /// `CBaseEntity`'s own are not repeated here — see [`BASE_INPUTS`].
    pub inputs: &'static [InputDef],
    /// The output names this class fires. `DEFINE_OUTPUT`.
    ///
    /// **Load-bearing**: it is how `"OnTrigger"` is recognised as an output
    /// connection rather than counted as a key nobody understands.
    /// `OnUser1`-`OnUser4` are `CBaseEntity`'s and are not repeated here —
    /// see [`keyvalue::BASE_OUTPUTS`](super::keyvalue::BASE_OUTPUTS).
    pub outputs: &'static [&'static str],
    /// `CEntityFactory<T>::Create`.
    pub create: fn() -> Box<dyn Behaviour>,
}

impl ClassDef {
    /// This class's spelling of the output `name`, if it declares one.
    ///
    /// Case-insensitive, because every name comparison in the original is
    /// `stricmp` and map data is inconsistent about case — and it returns the
    /// **declared** spelling rather than a `bool` so that a connection is
    /// filed under one name however the map spelled the key.
    pub fn declared_output(&self, name: &str) -> Option<&'static str> {
        self.outputs
            .iter()
            .find(|o| o.eq_ignore_ascii_case(name))
            .copied()
    }

    /// Whether this class declares `name` as one of its own keys. See
    /// [`keys`](ClassDef::keys) for why only a test calls it.
    #[allow(dead_code)]
    pub fn declares_key(&self, name: &str) -> bool {
        self.keys.iter().any(|k| k.eq_ignore_ascii_case(name))
    }

    /// The type this class's handler expects for `name`, if it has one.
    ///
    /// `AcceptInput`'s datadesc walk stops at the **first** match, and the
    /// walk runs derived-before-base — so a class that redeclares a
    /// [`BASE_INPUTS`] name wins, which this ordering reproduces by being
    /// consulted first.
    pub fn input_type(&self, name: &str) -> Option<FieldType> {
        self.inputs
            .iter()
            .find(|input| input.name.eq_ignore_ascii_case(name))
            .map(|input| input.field)
    }
}

/// `CBaseEntity`'s own inputs — the ones every class has (`baseentity.cpp:2370`).
///
/// **Not all of them: the ones a shipped Portal 2 map fires at a class this
/// port implements.** Valve declares 30; the rest either need a subsystem that
/// does not exist (`SetDamageFilter`, `DispatchResponse`, `RunScriptCode`) or
/// belong to a later stage (`SetParent` and the parenting family are stage 3's,
/// `Alpha`/`Color`/`DisableDraw` are the renderer's). Each absence is a
/// measurement: the depot test's `EXPECTED_UNHANDLED_INPUTS` table lists every
/// input name in the game that reaches an implemented class and is refused.
///
/// `Use` earns its place for a reason that is easy to miss: it is the input a
/// connection gets when the mapper left the field **empty**
/// (`cbase.cpp:150`), which 22 shipped connections do. It dispatches
/// [`Behaviour::use_entity`], which is `m_pfnUse` — null for every class here
/// but `func_button` and `func_rotating`, so for the rest accepting it and
/// doing nothing is not a stub, it is the behaviour.
pub const BASE_INPUTS: &[InputDef] = &[
    InputDef::new("Kill", FieldType::Void),
    InputDef::new("Use", FieldType::Void),
    InputDef::new("FireUser1", FieldType::String),
    InputDef::new("FireUser2", FieldType::String),
    InputDef::new("FireUser3", FieldType::String),
    InputDef::new("FireUser4", FieldType::String),
];

/// The type [`BASE_INPUTS`] declares for `name`, if any.
pub fn base_input(name: &str) -> Option<FieldType> {
    BASE_INPUTS
        .iter()
        .find(|input| input.name.eq_ignore_ascii_case(name))
        .map(|input| input.field)
}

/// `CBaseEntity`'s input handlers, for the inputs [`BASE_INPUTS`] declares.
///
/// Runs only when the class did not claim the name, which is the second and
/// last step of what `AcceptInput`'s `baseMap` walk does here.
pub fn base_accept_input(
    entity: &mut EntityCore,
    behaviour: &mut dyn Behaviour,
    input: &Input<'_>,
    cx: &mut Context<'_>,
) -> bool {
    let is = |name: &str| input.name.eq_ignore_ascii_case(name);

    if is("Kill") {
        // `InputKill` (`baseentity.cpp:4707`). The owner notification and the
        // never-delete-a-player branch both need entities this port has not
        // got; what is left is the `UTIL_Remove`.
        entity.remove();
        return true;
    }
    if is("Use") {
        // `InputUse` (`baseentity.cpp:4625`) calls `Use()`, which dispatches
        // `m_pfnUse`, and then fires a `player_use` game event for which there
        // is no event system. Two classes set a `m_pfnUse`
        // (`func_button` and `func_rotating`) and for the rest the pointer is
        // null, so accepting and doing nothing is the behaviour rather than a
        // stub — see [`Behaviour::use_entity`] and [`BASE_INPUTS`].
        behaviour.use_entity(entity, UseType::from_output_id(input.output_id), input, cx);
        return true;
    }
    for (name, output) in [
        ("FireUser1", "OnUser1"),
        ("FireUser2", "OnUser2"),
        ("FireUser3", "OnUser3"),
        ("FireUser4", "OnUser4"),
    ] {
        if is(name) {
            // `m_OnUser1.FireOutput( inputdata.pActivator, this )` — the
            // activator is forwarded and the caller becomes this entity.
            let me = entity.id();
            entity.fire_output(output, Variant::Void, input.activator, Some(me), 0.0, cx);
            return true;
        }
    }
    false
}

/// `USE_TYPE` (`game/shared/shareddefs.h:581`) — what a `Use` means.
///
/// # It is the connection's serial number, cast
///
/// `CBaseEntity::InputUse` passes `(USE_TYPE)inputdata.nOutputID`
/// (`baseentity.cpp:4627`), and `nOutputID` is the *ID stamp* of the
/// `CEventAction` that fired — an ever-increasing counter over every
/// connection in the map. So the use type of an I/O-driven `Use` is whatever
/// number that connection happened to be assigned, and only the first four
/// stamps in a level can name a real value.
///
/// Almost certainly a copy-paste of the value argument, and it is reproduced
/// because it decides something: `CFuncMoveLinear::Use` returns immediately
/// unless the type is `USE_SET`, so an I/O `Use` on a `func_movelinear` does
/// nothing in the shipped game, and a port that "fixed" this to `USE_TOGGLE`
/// would have eleven of them start moving that never move today. `func_button`
/// and `func_rotating` ignore the type, which is why they work anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseType {
    Off,
    On,
    Set,
    Toggle,
    /// Anything else the cast produced. Not a Valve value; `USE_TYPE` is a C
    /// enum and an out-of-range cast simply compares equal to none of the
    /// four, which is what this variant reproduces.
    Other,
}

impl UseType {
    /// The cast `InputUse` performs.
    pub fn from_output_id(id: u32) -> UseType {
        match id {
            0 => UseType::Off,
            1 => UseType::On,
            2 => UseType::Set,
            3 => UseType::Toggle,
            _ => UseType::Other,
        }
    }
}

/// What a behaviour may reach outside its own entity.
///
/// # Why this is small, and why that is the finding
///
/// `portdocs/SERVER.md` §7.2 expected an `EntityMut<'_>` carrying a
/// `&mut ServerContext` "for everything it needs to reach outward (fire an
/// output, find by name, remove an entity, trace)", and §10.3 flagged the
/// borrow shape as a risk: "an input handler that fires an output that reaches
/// the same entity is normal and legal in C++ — verify early that the chosen
/// shape survives it; a re-entrant `RefCell` panic discovered at stage 4 is
/// expensive."
///
/// **It survives it trivially, because the C++ is not re-entrant either.**
/// `CBaseEntityOutput::FireOutput` does not call the target — it appends to
/// [`EventQueue`], and the queue is drained by one top-level loop. So firing an
/// output is a queue append, removing an entity is a flag on the entity you
/// already hold, and *nothing* a stage-2 handler does needs to see another
/// entity — so through stage 3 this context did not name the entity list at
/// all.
///
/// # Stage 4 is the condition, and it arrived exactly as predicted
///
/// Stage 2 recorded that the shape would change for "a handler that must
/// *read* another entity during dispatch", and that the answer then was "the
/// entity list minus the one entity being dispatched, not a `RefCell`". A
/// trigger has to ask its `filter_*` entity whether the toucher passes, and
/// then push or teleport that toucher — so stage 4 carries `&mut EntityList`,
/// with the dispatched entity **lifted out of it** for the duration
/// ([`EntityList::detach`]). There is still no cell and no `unsafe`; the
/// borrow checker is satisfied because the two things really are disjoint.
///
/// The one rule that follows: **`cx.entity(self.id())` is `None` inside your
/// own handler.** You already hold `&mut EntityCore`; asking the list for
/// yourself would be asking for it twice.
pub struct Context<'a> {
    /// Where the server's clock is. **Not `Scene::curtime`** — see
    /// [`think`](super::think).
    pub time: Time,
    queue: &'a mut EventQueue,
    random: &'a mut RandomStream,
    /// Every entity but the one being dispatched — see the type's docs.
    entities: &'a mut EntityList,
    /// `UTIL_PlayerByIndex( 1 )`, for [`find_target`](Context::find_target).
    player: Option<EntityId>,
    /// Handles [`entity_mut`](Context::entity_mut) gave out, so that
    /// `Server::dispatch` can reconcile *their* schedules too and not only the
    /// dispatched entity's.
    changed: Vec<EntityId>,
    /// What [`create_entity`](Context::create_entity) made, in creation order,
    /// waiting for its `Spawn`.
    created: Vec<EntityId>,
}

impl<'a> Context<'a> {
    pub(super) fn new(
        time: Time,
        queue: &'a mut EventQueue,
        random: &'a mut RandomStream,
        entities: &'a mut EntityList,
        player: Option<EntityId>,
    ) -> Context<'a> {
        Context {
            time,
            queue,
            random,
            entities,
            player,
            changed: Vec::new(),
            created: Vec::new(),
        }
    }

    /// `CreateEntityByName` (`game/server/entitylist.cpp:206`) — a new entity,
    /// of a class this port implements, added to the list here and now.
    ///
    /// `None` for a classname the port has not got, which is the same answer
    /// `EntityFactoryDictionary()->Create` gives for one the game has not got.
    /// The handle resolves immediately, so the caller can finish building the
    /// entity through [`entity_mut`](Context::entity_mut) and
    /// [`behaviour_mut`](Context::behaviour_mut) and keep the handle.
    ///
    /// > **`Spawn` has not run yet, and that is the divergence.** In the C++
    /// > the creator calls `DispatchSpawn( pEnt )` itself, part-way through
    /// > its own `Spawn` — plain re-entrancy, which this module does not have:
    /// > `Server::dispatch` has lifted the *creator* out of the entity list
    /// > for the duration, so nothing can dispatch into the list while it
    /// > runs. So a created entity is **queued**, exactly the way
    /// > [`EntityCore::remove`] queues a deletion, and `Server::dispatch`
    /// > spawns it the moment the current handler returns. Everything a
    /// > creator does between `CreateEntityByName` and `DispatchSpawn` — set
    /// > the origin, the angles, the size, the owner — happens before the
    /// > `Spawn` either way, which is the order that matters.
    ///
    /// The one thing that order changes is a creator that reads its child's
    /// *post-`Spawn`* state before returning. Nothing does; `CreateTriggers`
    /// stores the handle and stops.
    pub fn create_entity(&mut self, classname: &str) -> Option<EntityId> {
        let class = super::classes::lookup(classname)?;
        let id = self.entities.insert(Entity::new(class));
        self.created.push(id);
        Some(id)
    }

    /// Another entity's *class* state, to write — the narrow counterpart of
    /// [`entity_mut`](Context::entity_mut), which reaches only the shared
    /// [`EntityCore`].
    ///
    /// This is `assert_cast< CPortalButtonTrigger* >( pTrigger )->m_pOwnerButton
    /// = pOwner`: the handful of lines in the game where one entity reaches
    /// into another's own fields rather than sending it an input. It is
    /// deliberately not a way to run the other class's code — there is no
    /// `&mut dyn Behaviour` on offer, because calling into a behaviour from
    /// inside a behaviour is the re-entrancy
    /// [`create_entity`](Context::create_entity) exists to avoid.
    ///
    /// `None` if the handle has stopped resolving, if it is the entity this
    /// handler belongs to, or if the class is not `T`.
    pub fn behaviour_mut<T: Behaviour>(&mut self, id: EntityId) -> Option<&mut T> {
        let entity = self.entities.get_mut(id)?;
        self.changed.push(id);
        entity.behaviour.downcast_mut::<T>()
    }

    /// Another entity, read-only. `EHANDLE::Get()`.
    ///
    /// `None` for a handle that has stopped resolving **and for the entity
    /// this handler belongs to** — see the type's docs.
    pub fn entity(&self, id: EntityId) -> Option<&Entity> {
        self.entities.get(id)
    }

    /// Another entity's shared state, to write. The narrow half of
    /// [`entity`](Context::entity): a handler may move, push or flag another
    /// entity, and may not run its code.
    ///
    /// Every handle handed out here is reconciled with the simulation list
    /// after the handler returns, so a write that changes whether the other
    /// entity needs simulating is not lost.
    pub fn entity_mut(&mut self, id: EntityId) -> Option<&mut EntityCore> {
        let entity = self.entities.get_mut(id)?;
        self.changed.push(id);
        Some(&mut entity.core)
    }

    /// `gEntList.FindEntityByName( NULL, name )` — the first match, or `None`.
    ///
    /// Procedural names (`!activator`, `!self`, …) are **not** resolved here:
    /// they need the I/O context that only [`Server::deliver`](super::Server)
    /// has. Every caller in this module is a class looking up a `targetname`
    /// it was given as a map key, which is what `FindEntityByName`'s plain
    /// form does.
    pub fn find_by_name(&self, query: &str) -> Option<EntityId> {
        name::find_by_name(self.entities, query).next()
    }

    /// `gEntList.FindEntityByName( NULL, name, pSearching, pActivator,
    /// pCaller )` — the first match, procedural names included.
    ///
    /// The form a class uses for a `target` key, as opposed to
    /// [`find_by_name`](Context::find_by_name)'s plain list search. **121 of
    /// the game's 128 `point_teleport`s need it**, because what they target is
    /// the literal string `!player`.
    pub fn find_target(
        &self,
        query: &str,
        searching: Option<EntityId>,
        activator: Option<EntityId>,
        caller: Option<EntityId>,
    ) -> Option<EntityId> {
        if name::is_procedural(query) {
            return match name::find_procedural(query, searching, activator, caller, self.player) {
                name::Procedural::Resolved(id) => id,
                _ => None,
            };
        }
        self.find_by_name(query)
    }

    /// The read-only view a filter chain runs against.
    pub fn filters(&self) -> Filters<'_> {
        Filters {
            entities: self.entities,
            depth: 0,
        }
    }

    /// The handles [`entity_mut`](Context::entity_mut) gave out.
    pub(super) fn take_changed(&mut self) -> Vec<EntityId> {
        std::mem::take(&mut self.changed)
    }

    /// The entities [`create_entity`](Context::create_entity) made, in
    /// creation order, for `Server::dispatch` to spawn.
    pub(super) fn take_created(&mut self) -> Vec<EntityId> {
        std::mem::take(&mut self.created)
    }

    /// `gpGlobals->curtime`.
    pub fn curtime(&self) -> f32 {
        self.time.curtime
    }

    /// The game's random stream — `random->RandomInt` and friends. See
    /// [`RandomStream`] for why it is not a global.
    pub fn random(&mut self) -> &mut RandomStream {
        self.random
    }

    /// `CEventQueue::AddEvent` with a named target.
    pub(super) fn post_named(
        &mut self,
        target: &str,
        input: &str,
        value: Variant,
        delay: f32,
        activator: Option<EntityId>,
        caller: Option<EntityId>,
        output_id: u32,
    ) {
        self.queue.add(Event {
            fire_time: self.time.curtime + delay,
            target: Target::Name(target.to_owned()),
            input: input.to_owned(),
            value,
            activator,
            caller,
            output_id,
        });
    }

    /// `CEventQueue::AddEvent` with a direct handle.
    pub(super) fn post_entity(
        &mut self,
        target: EntityId,
        input: &str,
        value: Variant,
        delay: f32,
        activator: Option<EntityId>,
        caller: Option<EntityId>,
    ) {
        self.queue.add(Event {
            fire_time: self.time.curtime + delay,
            target: Target::Entity(target),
            input: input.to_owned(),
            value,
            activator,
            caller,
            output_id: 0,
        });
    }

    /// `CEventQueue::CancelEvents( this )` — drop everything `caller` posted.
    pub(super) fn cancel_from(&mut self, caller: EntityId) -> usize {
        self.queue.cancel_from(caller)
    }
}

/// How deep a `filter_multi` chain may go before it is called a cycle.
///
/// Valve has no bound: `CFilterMultiple::PassesFilterImpl` calls
/// `PassesFilter` on each sub-filter, and a `filter_multi` naming itself
/// recurses until the stack runs out. **No shipped map has a chain deeper than
/// one**, so this is the same divergence — and the same reasoning — as the
/// parent-cycle bound in `Server::spawn_hierarchy_depth`.
const MAX_FILTER_DEPTH: u32 = 8;

/// The read-only view a `filter_*` class evaluates against.
///
/// `CBaseFilter::PassesFilter` takes an entity and answers yes or no, and a
/// `filter_multi` answers by asking the filters it names. That recursion is
/// the only reason this type exists: a filter needs to reach *other* entities
/// while every caller of it holds one already, so it gets the narrowest thing
/// that works — the list, immutably, and a depth counter.
///
/// Handed out by [`Context::filters`]. Like the context it came from, it
/// cannot see the entity currently being dispatched.
pub struct Filters<'a> {
    entities: &'a EntityList,
    depth: u32,
}

impl Filters<'_> {
    /// `pFilter->PassesFilter( pCaller, pEntity )`.
    ///
    /// `true` when `filter` names nothing — a trigger with no filter passes
    /// everything, which is `(!pFilter) ? true : …` at
    /// `triggers.cpp:420`. Also `true` past [`MAX_FILTER_DEPTH`], which is a
    /// cycle and is reported once.
    pub fn passes(&self, filter: EntityId, caller: &EntityCore, other: &EntityCore) -> bool {
        if self.depth >= MAX_FILTER_DEPTH {
            eprintln!(
                "source-engine: server: LEVEL DESIGN ERROR: filter chain from {} is a cycle",
                caller.debug_name()
            );
            return true;
        }
        let Some(entity) = self.entities.get(filter) else {
            return true;
        };
        let deeper = Filters {
            entities: self.entities,
            depth: self.depth + 1,
        };
        entity.behaviour.passes_filter(&entity.core, other, &deeper)
    }

    /// `gEntList.FindEntityByName( NULL, name )`, restricted to filters:
    /// `dynamic_cast<CBaseFilter *>` returning null is a *warning and no
    /// filter* in the original, not an error.
    ///
    /// Used by `filter_multi`'s `Activate` as well as by every trigger's, so
    /// it lives here rather than on either.
    pub fn find(&self, name: &str) -> Option<EntityId> {
        let id = name::find_by_name(self.entities, name).next()?;
        let entity = self.entities.get(id)?;
        match entity.behaviour.is_filter() {
            true => Some(id),
            false => {
                eprintln!(
                    "source-engine: server: tried to filter through {name}, \
                     which is a {} and not a filter",
                    entity.classname()
                );
                None
            }
        }
    }
}

/// What `Spawn` decided. `DispatchSpawn`'s return value, which is `< 0` when
/// the entity removed itself.
///
/// Two thirds of the light entities in the shipped game take
/// [`Remove`](SpawnResult::Remove): `CLight::Spawn` deletes any light without
/// a targetname, because an unnamed light has already had its whole
/// contribution baked into the lightmaps by `vrad`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnResult {
    /// The entity is alive.
    Ok,
    /// `UTIL_Remove( this )` — the entity asked to be deleted during `Spawn`.
    Remove,
}

/// A class's own state and behaviour: everything `CBaseEntity` did not have.
///
/// Eight methods against the C++'s hundred virtuals, because that is what the
/// port runs: parse keys, spawn, activate, think, arrive, get used, take an
/// input, describe yourself. `move_done` and `use_entity` arrived with
/// `portdocs/SERVER.md` stage 3, which is the stage that has movers.
///
/// Every method takes `&mut EntityCore` alongside `&mut self`, which is the
/// whole reason [`Entity`](super::entity::Entity) is split in two — a handler
/// that mutates the entity it belongs to needs both borrows at once.
pub trait Behaviour: Any {
    /// This class's half of `CBaseEntity::KeyValue`.
    ///
    /// Returns whether the key was consumed. Called *before* the shared
    /// ladder, and a class that contains another (composition in place of
    /// `BaseClass`) ends by delegating to it.
    fn key_value(&mut self, _entity: &mut EntityCore, _key: &str, _value: &str) -> bool {
        false
    }

    /// `Spawn()`. Called once, after every key has been parsed and after
    /// parenting, in hierarchy order.
    fn spawn(&mut self, _entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        SpawnResult::Ok
    }

    /// `Activate()`. Called on every surviving entity *after* every entity has
    /// spawned, which is what makes it the first place a class may look at
    /// another one — and, for `logic_auto` and `logic_relay`, the place a
    /// think is scheduled so that a map can start itself.
    fn activate(&mut self, _entity: &mut EntityCore, _cx: &mut Context<'_>) {}

    /// `Think()`. The schedule is **already cleared** when this runs — see
    /// [`EntityCore::set_next_think`] — so a recurring behaviour must re-arm
    /// itself on the way out.
    fn think(&mut self, _entity: &mut EntityCore, _cx: &mut Context<'_>) {}

    /// `MoveDone()` — the arrival alarm has gone off.
    ///
    /// `CBaseEntity::MoveDone` dispatches `m_pfnMoveDone`, a function pointer
    /// a mover re-points at each step of its cycle: a door hitting the top
    /// sets it to `DoorGoDown` so the *same* alarm serves the wait. A class
    /// keeps its own enum in place of the pointer, and one that holds a
    /// [`Toggle`](super::movement::Toggle) calls
    /// [`Toggle::move_done`](super::movement::Toggle::move_done) first —
    /// that call *is* `CBaseToggle::MoveDone`, and skipping it leaves the
    /// mover a fraction of a tick past its destination with its velocity
    /// still set.
    fn move_done(&mut self, _entity: &mut EntityCore, _cx: &mut Context<'_>) {}

    /// `Use()` — `m_pfnUse`, dispatched by `CBaseEntity::InputUse`.
    ///
    /// Null for every class but two, so the default is to do nothing and that
    /// is the behaviour rather than a stub. See [`UseType`] for why the type
    /// argument is a connection serial number.
    fn use_entity(
        &mut self,
        _entity: &mut EntityCore,
        _use_type: UseType,
        _input: &Input<'_>,
        _cx: &mut Context<'_>,
    ) {
    }

    /// `StartTouch( pOther )` — something has begun touching this entity.
    ///
    /// Called once, when the link between the two is created, and always
    /// followed immediately by [`touch`](Behaviour::touch) on the same tick —
    /// which is `PhysicsStartTouch` (`physics_main_shared.cpp:940`) calling
    /// both in a row and is why a `trigger_once` fires `OnStartTouch` and
    /// `OnTrigger` together.
    fn start_touch(&mut self, _entity: &mut EntityCore, _other: EntityId, _cx: &mut Context<'_>) {}

    /// `Touch( pOther )` — something is touching this entity, this tick.
    ///
    /// Called every tick the touch persists, including the first.
    fn touch(&mut self, _entity: &mut EntityCore, _other: EntityId, _cx: &mut Context<'_>) {}

    /// `EndTouch( pOther )` — something has stopped touching this entity.
    ///
    /// Driven by the touch *stamp* rather than by a geometric test: a link
    /// that was not restamped this tick is a touch that ended. See
    /// [`touch`](super::touch).
    fn end_touch(&mut self, _entity: &mut EntityCore, _other: EntityId, _cx: &mut Context<'_>) {}

    /// `dynamic_cast<CBaseFilter *>( pEntity ) != NULL`.
    ///
    /// A `filtername` that names something which is not a filter is a warning
    /// and no filter, not an error, and this is the test that decides —
    /// `triggers.cpp:236`, `filters.cpp:147`.
    fn is_filter(&self) -> bool {
        false
    }

    /// `CBaseFilter::PassesFilterImpl` — does `other` match this filter's
    /// criteria?
    ///
    /// **The negation is the caller's**, not this method's:
    /// `CBaseFilter::PassesFilter` is `m_bNegated ? !Impl() : Impl()`, and a
    /// `filter_multi` combines the *un*negated results of its children with
    /// their own negations already applied. Each class here therefore applies
    /// its own `Negated` at the end of its own implementation, which is what
    /// the two-method split in the C++ buys and is the only place the split
    /// matters.
    ///
    /// Only ever called on a class whose [`is_filter`](Behaviour::is_filter)
    /// is `true`.
    fn passes_filter(
        &self,
        _entity: &EntityCore,
        _other: &EntityCore,
        _filters: &Filters<'_>,
    ) -> bool {
        true
    }

    /// `CBaseEntity::IsPlayer()`.
    ///
    /// One class overrides it, and three things read it: `trigger_hurt`
    /// choosing between `OnHurtPlayer` and `OnHurt`, `filter_activator_name`'s
    /// special case for the literal string `!player`, and `trigger_teleport`
    /// taking the eye angles rather than the entity angles.
    /// What this entity's studio model is doing, if it draws one.
    ///
    /// `None` — the default — for the 35 classes that draw no model or whose
    /// model never animates. `Some` is `CBaseAnimating`'s networked animation
    /// state, reduced to what the renderer needs to pose a model: which
    /// sequence, and when it started.
    ///
    /// **This is the whole of the seam** between the game and the model
    /// renderer, which is why the sequence is a `&'static str` rather than an
    /// index: looking a label up in a `.mdl` needs the `.mdl`, and this module
    /// names no studio type.
    fn model_state(&self) -> Option<ModelState> {
        None
    }

    fn is_player(&self) -> bool {
        false
    }

    /// `AcceptInput`'s dispatch half, for the inputs this class declares.
    ///
    /// The value has already been converted to the type
    /// [`ClassDef::inputs`] declared, so a handler reads it without checking.
    /// Returning `false` for a name the class declared is a bug the invariant
    /// test catches.
    fn accept_input(
        &mut self,
        _entity: &mut EntityCore,
        _input: &Input<'_>,
        _cx: &mut Context<'_>,
    ) -> bool {
        false
    }

    /// This class's state, as name/value pairs, for `ent_dump`.
    ///
    /// `DumpEntity` (`baseentity.cpp:5950`) walks the datadesc and prints
    /// every field with its `externalName`. There is no datadesc to walk here
    /// — the whole point of §7.3 is that the fields are real Rust fields — so
    /// a class that has state says what it is.
    fn describe(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }
}

impl dyn Behaviour {
    /// Recovers the concrete class's state.
    ///
    /// Valve reaches a derived class with `dynamic_cast<>` and does it in
    /// anger — `mapentities.cpp` picks `CWorld`, `CNodeEnt` and `CLight` out
    /// of the spawn list that way, and `CTonemapSystem::LevelInitPostEntity`
    /// picks out the master tone mapper. So does this port, in the same two
    /// places: the tests, and the tone mapper.
    pub fn downcast_ref<T: Behaviour>(&self) -> Option<&T> {
        (self as &dyn Any).downcast_ref::<T>()
    }

    /// The same, to write — what [`Context::behaviour_mut`] is built on.
    pub fn downcast_mut<T: Behaviour>(&mut self) -> Option<&mut T> {
        (self as &mut dyn Any).downcast_mut::<T>()
    }
}

/// `CBaseAnimating`'s animation state, as much of it as anything reads.
///
/// `m_nSequence` and `m_flAnimTime` (`baseanimating.h`), plus the skin. Valve
/// networks all three to the client, which is where they are turned into a
/// pose — see
/// [`entities`](crate::engine::world::entities) for why the split survives here
/// with one process.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelState {
    /// The sequence's label, as `LookupSequence` takes it. `""` is the bind
    /// pose.
    pub sequence: &'static str,
    /// `m_flAnimTime` — when the sequence was last reset.
    ///
    /// > **It is the *server's* clock**, and the renderer measures against the
    /// > scene's (gotcha 1). The two track each other and differ by at most one
    /// > tick, because the server's is the scene's quantised down; the renderer
    /// > clamps a negative elapsed time to zero so the worst case is a single
    /// > frame of an animation not having started yet.
    pub anim_time: f32,
    /// `m_nSkin`.
    pub skin: i32,
}

/// The behaviour of a class with no state of its own.
///
/// `CPointEntity` (`baseentity.cpp:8300`) — "an entity that has no model, no
/// solidity, and does nothing but sit where it was put". `info_target` and
/// `info_player_start` are both this, and between them the shipped maps place
/// 547 of them.
pub struct PointEntity;

impl Behaviour for PointEntity {}

impl PointEntity {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(PointEntity)
    }
}

/// `TICK_NEVER_THINK` as a *time*, for [`EntityCore::set_next_think`].
///
/// `SetNextThink` compares its `float` argument against `TICK_NEVER_THINK`
/// before converting, so `-1` is the value that means "cancel" rather than a
/// time one tick before the level started.
pub const NEVER_THINK: f32 = TICK_NEVER_THINK as f32;
