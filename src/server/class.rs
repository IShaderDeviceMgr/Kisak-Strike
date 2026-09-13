//! What a class *is*: the table, and the trait.
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
//! What remains as data is the *declaration* — the key and output names — and
//! that is genuinely needed. An output key has to be recognised as an output
//! rather than counted as an unknown key, and a report of what the port does
//! not understand yet is only meaningful if it knows what it does.
//!
//! # What is not here yet
//!
//! `inputs` and `fieldtype_t`. Both exist only to serve `AcceptInput`, which
//! is `portdocs/SERVER.md` stage 2, and a table nothing reads is the
//! scaffolding `PORTING.md` says not to write.

use std::any::Any;

use super::entity::EntityCore;

/// One entity class: a name, its declared keys and outputs, and how to make
/// its state. `LINK_ENTITY_TO_CLASS` plus the `BEGIN_DATADESC` block.
pub struct ClassDef {
    /// The classname as the entity lump spells it. `m_iClassname`.
    pub name: &'static str,
    /// The keys this class's [`Behaviour::key_value`] consumes.
    ///
    /// **The shared ones are not repeated here.** `targetname`, `origin`,
    /// `angles` and the rest are consumed by
    /// [`keyvalue::base_key_value`](super::keyvalue::base_key_value), which is
    /// `CBaseEntity::KeyValue`'s if-ladder and runs first, exactly as it does
    /// in the original.
    ///
    /// Declaration only: consuming a key is `key_value`'s job, and the two
    /// are checked against each other by `classes`' invariant test. That test
    /// is the field's only reader, which is the point — it is what stops the
    /// table and the code drifting apart as classes are added.
    #[allow(dead_code)]
    pub keys: &'static [&'static str],
    /// The output names this class fires. `DEFINE_OUTPUT`.
    ///
    /// **This one is load-bearing**: it is how `"OnTrigger"` is recognised as
    /// an output connection rather than counted as a key nobody understands.
    /// `OnUser1`-`OnUser4` are `CBaseEntity`'s and are not repeated here —
    /// see [`keyvalue::BASE_OUTPUTS`](super::keyvalue::BASE_OUTPUTS).
    pub outputs: &'static [&'static str],
    /// `CEntityFactory<T>::Create`.
    pub create: fn() -> Box<dyn Behaviour>,
}

impl ClassDef {
    /// Whether this class declares `name` as one of its outputs.
    /// Case-insensitive, because every name comparison in the original is
    /// `stricmp` and map data is inconsistent about case.
    pub fn declares_output(&self, name: &str) -> bool {
        self.outputs.iter().any(|o| o.eq_ignore_ascii_case(name))
    }

    /// Whether this class declares `name` as one of its own keys. See
    /// [`keys`](ClassDef::keys) for why nothing outside the tests calls it.
    #[allow(dead_code)]
    pub fn declares_key(&self, name: &str) -> bool {
        self.keys.iter().any(|k| k.eq_ignore_ascii_case(name))
    }
}

/// What `Spawn` decided. `DispatchSpawn`'s return value, which is `< 0` when
/// the entity removed itself.
///
/// Two thirds of the light entities in the shipped game take
/// [`Remove`](SpawnResult::Remove): `CLight::Spawn` deletes any light without
/// a targetname, because an unnamed light has already had its whole
/// contribution baked into the lightmaps by `vrad` and there is nothing left
/// for it to do at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnResult {
    /// The entity is alive.
    Ok,
    /// `UTIL_Remove( this )` — the entity asked to be deleted during `Spawn`.
    Remove,
}

/// A class's own state and behaviour: everything `CBaseEntity` did not have.
///
/// Three methods against the C++'s hundred virtuals, because stage 1 runs
/// three things: parse keys, spawn, activate. `think`, `accept_input` and
/// `move_done` arrive with the stages that call them.
///
/// Every method takes `&mut EntityCore` alongside `&mut self`, which is the
/// whole reason [`Entity`](super::entity::Entity) is split in two — a `Spawn`
/// that mutates the entity it belongs to needs both borrows at once.
pub trait Behaviour: Any {
    /// This class's half of `CBaseEntity::KeyValue`.
    ///
    /// Returns whether the key was consumed. Called only for keys the shared
    /// ladder did not take, and a class that contains another (composition in
    /// place of `BaseClass`) ends by delegating to it.
    fn key_value(&mut self, _entity: &mut EntityCore, _key: &str, _value: &str) -> bool {
        false
    }

    /// `Spawn()`. Called once, after every key has been parsed and after
    /// parenting, in hierarchy order.
    fn spawn(&mut self, _entity: &mut EntityCore) -> SpawnResult {
        SpawnResult::Ok
    }

    /// `Activate()`. Called on every surviving entity *after* every entity has
    /// spawned, which is what makes it the first place a class may look at
    /// another one.
    ///
    /// Nothing implements it yet: the classes that do in the original —
    /// `CLogicAuto`, `CLogicRelay` — use it to schedule a think, and there is
    /// no think schedule until stage 2. The **pass** is ported here rather
    /// than the implementations, because the two-pass shape is the part that
    /// is hard to retrofit.
    fn activate(&mut self, _entity: &mut EntityCore) {}

    /// This class's state, as name/value pairs, for `ent_dump`.
    ///
    /// `DumpEntity` (`baseentity.cpp:5950`) walks the datadesc and prints
    /// every field with its `externalName`. There is no datadesc to walk here
    /// — the whole point of §7.3 is that the fields are real Rust fields — so
    /// a class that has state says what it is. It is also what stops a parsed
    /// key from having no reader at all.
    fn describe(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }
}

impl dyn Behaviour {
    /// Recovers the concrete class's state.
    ///
    /// Valve reaches a derived class with `dynamic_cast<>` and does it in
    /// anger — `mapentities.cpp` picks `CWorld`, `CNodeEnt` and `CLight` out
    /// of the spawn list that way. Here it is for tests and for the few places
    /// that genuinely need one class to read another's state — of which stage
    /// 1 has none, so the tests are the only caller so far.
    #[allow(dead_code)]
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
