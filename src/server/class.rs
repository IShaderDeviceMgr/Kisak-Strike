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

use super::entity::{EntityCore, EntityId};
use super::io::{Event, EventQueue, FieldType, Input, Target, Variant};
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
/// (`cbase.cpp:150`), which 22 shipped connections do. `CBaseEntity::Use` is a
/// null function pointer for every class here, so accepting it and doing
/// nothing is not a stub — it is the behaviour.
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
pub fn base_accept_input(entity: &mut EntityCore, input: &Input<'_>, cx: &mut Context<'_>) -> bool {
    let is = |name: &str| input.name.eq_ignore_ascii_case(name);

    if is("Kill") {
        // `InputKill` (`baseentity.cpp:4707`). The owner notification and the
        // never-delete-a-player branch both need entities this port has not
        // got; what is left is the `UTIL_Remove`.
        entity.remove();
        return true;
    }
    if is("Use") {
        // `InputUse` calls `Use()`, which dispatches `m_pfnUse` — null for
        // every class implemented so far — and fires a `player_use` game
        // event, for which there is no event system. Accepted and ignored;
        // see [`BASE_INPUTS`].
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
/// entity. This context therefore does not borrow the entity list at all,
/// which is why a handler can hold `&mut EntityCore` and this at the same time
/// with no cell, no index juggling and no `unsafe`.
///
/// The condition that changes it is a handler that must *read* another entity
/// during dispatch — `logic_branch_listener` polling its branches is the first
/// one in the game. When that arrives, the shape to reach for is the entity
/// list minus the one entity being dispatched, not a `RefCell`.
pub struct Context<'a> {
    /// Where the server's clock is. **Not `Scene::curtime`** — see
    /// [`think`](super::think).
    pub time: Time,
    queue: &'a mut EventQueue,
    random: &'a mut RandomStream,
}

impl<'a> Context<'a> {
    pub(super) fn new(
        time: Time,
        queue: &'a mut EventQueue,
        random: &'a mut RandomStream,
    ) -> Context<'a> {
        Context {
            time,
            queue,
            random,
        }
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
/// Five methods against the C++'s hundred virtuals, because that is what the
/// port runs: parse keys, spawn, activate, think, take an input. `move_done`
/// arrives with `portdocs/SERVER.md` stage 3, which is the stage that has
/// movers.
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
