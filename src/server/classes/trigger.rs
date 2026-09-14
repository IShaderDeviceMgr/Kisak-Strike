//! The trigger family: the first thing in this port that notices the player.
//!
//! `game/server/triggers.cpp` — `CBaseTrigger` and the five classes Portal 2
//! places most of:
//!
//! ```text
//!   1476  trigger_once        CTriggerOnce : CTriggerMultiple
//!    899  trigger_multiple    CTriggerMultiple
//!    215  trigger_hurt        CTriggerHurt
//!    192  trigger_push        CTriggerPush
//!    110  trigger_teleport    CTriggerTeleport
//! ```
//!
//! **2,892 entities, and 4,364 output connections on them** — `OnTrigger`
//! (2,497), `OnStartTouch` (1,724), `OnEndTouchAll` (545) and `OnEndTouch`
//! (194). `trigger_once` is the fifth commonest classname in the game.
//!
//! # `CBaseToggle` does not survive, and Valve says so
//!
//! `class CBaseTrigger : public CBaseToggle` carries a comment two lines
//! above it — `// DVS TODO: get rid of CBaseToggle` — and it is right: of
//! `CBaseToggle`'s dozen fields a trigger reads exactly one, `m_flWait`, and
//! only `CTriggerMultiple` reads that. So [`BaseTrigger`] holds none of the
//! mover machinery and `TriggerMultiple` keeps its own `wait`. That is
//! `PORTING.md`'s "keep the knowledge, discard the encoding" at its most
//! literal.
//!
//! # A trigger is solid *and* not solid, and both halves matter
//!
//! `InitTrigger` sets `SOLID_BSP` — so the touch query can sweep against the
//! entity's real brushes rather than its bounding box — and then sets
//! `FSOLID_NOT_SOLID` so walking into one does not stop you, and
//! `FSOLID_TRIGGER` so the touch query looks at it at all. Getting that triple
//! right is the whole reason `trace/` could not put brush entities in the
//! player's clip chain before this stage: without `FSOLID_NOT_SOLID` every
//! trigger in the game is an invisible wall.
//!
//! # What none of them does
//!
//! **Damage.** There is no health, no `TakeDamage` and no death, so
//! [`TriggerHurt`] runs the whole of Valve's timing — the half-second think,
//! the forgiveness doubling, the half-damage on the way out — and takes
//! nothing away. Its outputs fire on exactly the schedule the shipped game
//! fires them on, which is what 9 of the game's connections ask for.
//!
//! Also absent, and each measured: NPCs (`SF_TRIGGER_ALLOW_NPCS` serves 293
//! entities in the whole game), vehicles (Portal 2 has none), physics objects
//! (`ENGINE_TRACE.md` stage 5), and `trigger_look`/`trigger_playerteam`/
//! `trigger_catapult`/`trigger_portal_cleanser`, which are either
//! reconstruction jobs (`portdocs/SERVER.md` §1.3) or want a subsystem that
//! does not exist.

use glam::{Mat3, Vec3};

use crate::server::class::{Behaviour, Context, InputDef, InputDefs, SpawnResult, NEVER_THINK};
use crate::server::entity::{EntityCore, EntityId};
use crate::server::io::{FieldType, Input, Variant};
use crate::server::keyvalue::{atof, atoi, string_to_vector};
use crate::server::movement::{
    move_dir, MoveType, Solid, EF_NODRAW, FL_BASEVELOCITY, FL_CLIENT, FL_ONGROUND,
    FSOLID_NOT_SOLID, FSOLID_TRIGGER,
};
use crate::server::touch::{self, Teleport};

// ---------------------------------------------------------------------------
// spawnflags
// ---------------------------------------------------------------------------

// `TriggerSpawnflags_t` (`game/shared/triggers_shared.h:16`). The whole set is
// defined because the bits are external data — a mapper types them into
// Hammer and the `.bsp` carries the number — and because which ones Portal 2
// uses is a measurement rather than a guess.

/// Players can fire this trigger. Set on 1,220 of the game's 1,476
/// `trigger_once`s, and the bit that makes a trigger a trigger for the player.
const SF_TRIGGER_ALLOW_CLIENTS: u32 = 0x01;
/// NPCs can. There are 293 NPCs in the game and this port has none.
const SF_TRIGGER_ALLOW_NPCS: u32 = 0x02;
/// `func_pushable`s can. No shipped Portal 2 map places one.
const SF_TRIGGER_ALLOW_PUSHABLES: u32 = 0x04;
/// `MOVETYPE_VPHYSICS` objects can — cubes, mostly. `ENGINE_TRACE.md` stage 5.
const _SF_TRIGGER_ALLOW_PHYSICS: u32 = 0x08;
/// *If* NPCs can, only player-ally ones.
const SF_TRIGGER_ONLY_PLAYER_ALLY_NPCS: u32 = 0x10;
/// *If* players can, only ones in a vehicle. Portal 2 has no vehicles, so this
/// refuses everybody — which is Valve's behaviour and not a gap. Zero shipped
/// triggers set it.
const SF_TRIGGER_ONLY_CLIENTS_IN_VEHICLES: u32 = 0x20;
/// Everything except debris.
const SF_TRIGGER_ALLOW_ALL: u32 = 0x40;
/// `trigger_push`: transfer the velocity once and delete the trigger.
const SF_TRIG_PUSH_ONCE: u32 = 0x80;
/// `trigger_push`: push a player who is on a ladder. `GameHasLadders()` is
/// false for Portal (`portdocs/CLIENT.md` stage 4), so nothing reaches it.
const _SF_TRIG_PUSH_AFFECT_PLAYER_ON_LADDER: u32 = 0x100;
/// *If* players can, only ones out of a vehicle. With no vehicles this is a
/// no-op rather than a refusal — the opposite of `0x20`, and the asymmetry is
/// Valve's.
const SF_TRIGGER_ONLY_CLIENTS_OUT_OF_VEHICLES: u32 = 0x200;
/// Touch physics debris. Reaches a `COLLISION_GROUP_DEBRIS` test that is
/// `#ifdef`ed to HL2 Episodic and TF2 and is therefore compiled out of this
/// tree entirely — so in Portal 2 the flag only ever adds
/// `FSOLID_TRIGGER_TOUCH_DEBRIS`, which nothing reads.
const SF_TRIG_TOUCH_DEBRIS: u32 = 0x400;
/// *If* NPCs can, only ones in a vehicle.
const SF_TRIGGER_ONLY_NPCS_IN_VEHICLES: u32 = 0x800;
/// `trigger_push`: account for an object's mass. Hammer sets it by default,
/// which is why **1,220 of the game's `trigger_once`s carry `4097`** — this
/// bit plus `SF_TRIGGER_ALLOW_CLIENTS` — on a class that has no use for it.
const _SF_TRIGGER_PUSH_USE_MASS: u32 = 0x1000;

/// `FSOLID_TRIGGER_TOUCH_DEBRIS` (`public/const.h:238`), set by
/// [`SF_TRIG_TOUCH_DEBRIS`] and read by nothing here.
const FSOLID_TRIGGER_TOUCH_DEBRIS: u32 = 0x0200;

/// `FL_NPC` (`public/const.h:141`). Nothing sets it; the constant exists so
/// that [`BaseTrigger::passes_trigger_filters`] reads the way the C++ does
/// rather than quietly dropping a term of the disjunction.
const FL_NPC: u32 = 1 << 14;

/// `DMG_RADIATION` (`public/shareddefs.h`) — the one damage bit that changes
/// how a `trigger_hurt` *thinks* rather than what it does. 27 of the game's
/// 215 set it.
const DMG_RADIATION: i32 = 1 << 18;

// ---------------------------------------------------------------------------
// CBaseTrigger
// ---------------------------------------------------------------------------

/// `CBaseTrigger` (`triggers.cpp:109`) — held, not inherited, by all five
/// classes.
#[derive(Default)]
pub struct BaseTrigger {
    /// `m_bDisabled` — the `StartDisabled` key, and then whatever
    /// `Enable`/`Disable` last said.
    ///
    /// > **`Toggle` does not change it.** `InputToggle` flips
    /// > `FSOLID_TRIGGER` and leaves this alone (`triggers.cpp:566`), so a
    /// > toggled trigger and a disabled one differ in what `TouchTest` will
    /// > answer. Valve's, and reproduced: zero shipped connections fire
    /// > `Toggle` at a trigger, so nothing in Portal 2 can tell.
    disabled: bool,
    /// `m_iFilterName` — the `filtername` key.
    filter_name: Option<String>,
    /// `m_hFilter`, resolved at `Activate`.
    filter: Option<EntityId>,
    /// `m_hTouchingEntities` — **not** the entity's
    /// [`touch_links`](EntityCore::touch_links).
    ///
    /// > The two lists answer different questions and a trigger keeps both.
    /// > The link list is everything overlapping it, filters or no filters;
    /// > this one is everything that *passed* the filters, and it is what
    /// > decides `OnStartTouchAll`, `OnEndTouchAll` and `TouchTest`. A
    /// > `trigger_multiple` filtered to cubes has a link to the player and no
    /// > entry here.
    touching: Vec<EntityId>,
}

/// The inputs `CBaseTrigger` declares (`triggers.cpp:117`).
pub static BASE_TRIGGER_INPUTS: InputDefs = &[
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("TouchTest", FieldType::Void),
    InputDef::new("StartTouch", FieldType::Void),
    InputDef::new("EndTouch", FieldType::Void),
];

/// The outputs `CBaseTrigger` declares (`triggers.cpp:124`).
pub static BASE_TRIGGER_OUTPUTS: &[&str] = &[
    "OnStartTouch",
    "OnStartTouchAll",
    "OnEndTouch",
    "OnEndTouchAll",
    "OnTouching",
    "OnNotTouching",
];

impl BaseTrigger {
    /// `CBaseTrigger`'s two keys.
    pub fn key_value(&mut self, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("StartDisabled") {
            self.disabled = atoi(value) != 0;
            return true;
        }
        if key.eq_ignore_ascii_case("filtername") {
            self.filter_name = Some(value.to_owned());
            return true;
        }
        false
    }

    /// `CBaseTrigger::Spawn` (`triggers.cpp:174`) — three spawnflag
    /// promotions and nothing else.
    ///
    /// Each is "if you asked for a *restriction* on a category, you meant to
    /// allow the category", which is a Hammer usability fix encoded in the
    /// server. None of the three is reachable in Portal 2 — the flags they
    /// test appear on zero shipped triggers — and all three are one line.
    pub fn spawn(&mut self, entity: &mut EntityCore) {
        if entity
            .has_spawn_flags(SF_TRIGGER_ONLY_PLAYER_ALLY_NPCS | SF_TRIGGER_ONLY_NPCS_IN_VEHICLES)
        {
            entity.spawn_flags |= SF_TRIGGER_ALLOW_NPCS;
        }
        if entity.has_spawn_flags(
            SF_TRIGGER_ONLY_CLIENTS_IN_VEHICLES | SF_TRIGGER_ONLY_CLIENTS_OUT_OF_VEHICLES,
        ) {
            entity.spawn_flags |= SF_TRIGGER_ALLOW_CLIENTS;
        }
    }

    /// `CBaseTrigger::InitTrigger` (`triggers.cpp:327`) — what makes a brush
    /// entity a trigger.
    ///
    /// Every derived `Spawn` calls it, and the order of the three solidity
    /// writes is the whole of the module doc's point.
    pub fn init_trigger(&mut self, entity: &mut EntityCore) {
        // `SetSolid( GetParent() ? SOLID_VPHYSICS : SOLID_BSP )`. Both are
        // "solid" for `IsSolid()`; the distinction is which collision
        // representation Valve would have used, and for a brush entity they
        // are the same brushes.
        entity.solid = match entity.parent.is_some() {
            true => Solid::VPhysics,
            false => Solid::Bsp,
        };
        entity.add_solid_flags(FSOLID_NOT_SOLID);
        match self.disabled {
            true => entity.remove_solid_flags(FSOLID_TRIGGER),
            false => entity.add_solid_flags(FSOLID_TRIGGER),
        }
        entity.move_type = MoveType::None;
        // `SetModel` — the bounds are already on the entity, put there by
        // `Server::level_init` out of the `.bsp`'s model lump.

        // `if ( showtriggers.GetInt() == 0 ) AddEffects( EF_NODRAW )`.
        // `showtriggers` is an `FCVAR_CHEAT` debug switch this port does not
        // register, so a trigger is always invisible — which is also what
        // `world/` already concluded from the `SURF_NODRAW` on its faces.
        entity.effects |= EF_NODRAW;
        self.touching.clear();
        if entity.has_spawn_flags(SF_TRIG_TOUCH_DEBRIS) {
            entity.add_solid_flags(FSOLID_TRIGGER_TOUCH_DEBRIS);
        }
    }

    /// `CBaseTrigger::Activate` (`triggers.cpp:232`) — resolve `filtername`.
    pub fn activate(&mut self, cx: &mut Context<'_>) {
        if let Some(name) = self.filter_name.as_deref() {
            self.filter = cx.filters().find(name);
        }
    }

    /// `CBaseTrigger::PassesTriggerFilters` (`triggers.cpp:360`) — may this
    /// entity fire me?
    ///
    /// Two questions in one: the spawnflag categories, and then the
    /// `filter_*` entity if there is one. The NPC and vehicle sub-tests are
    /// dropped because nothing in this port has `FL_NPC` and Portal 2 has no
    /// vehicles at all — except for the two `IN_VEHICLES` lines, which are
    /// kept because they *refuse* rather than allow and dropping them would be
    /// a behaviour change rather than an absence.
    pub fn passes_trigger_filters(
        &self,
        entity: &EntityCore,
        other: EntityId,
        cx: &Context<'_>,
    ) -> bool {
        let Some(other_core) = cx.entity(other).map(|e| &e.core) else {
            return false;
        };

        let allowed = entity.has_spawn_flags(SF_TRIGGER_ALLOW_ALL)
            || (entity.has_spawn_flags(SF_TRIGGER_ALLOW_CLIENTS)
                && other_core.has_flags(FL_CLIENT))
            || (entity.has_spawn_flags(SF_TRIGGER_ALLOW_NPCS) && other_core.has_flags(FL_NPC))
            || (entity.has_spawn_flags(SF_TRIGGER_ALLOW_PUSHABLES)
                && other_core.classname().eq_ignore_ascii_case("func_pushable"));
        if !allowed {
            return false;
        }

        // "*if* players can, only players inside vehicles can" — and there are
        // no vehicles, so this refuses every player.
        if entity.has_spawn_flags(SF_TRIGGER_ONLY_CLIENTS_IN_VEHICLES)
            && other_core.has_flags(FL_CLIENT)
        {
            return false;
        }

        match self.filter {
            Some(filter) => cx.filters().passes(filter, entity, other_core),
            None => true,
        }
    }

    /// `CBaseTrigger::StartTouch` (`triggers.cpp:466`).
    ///
    /// Returns whether the toucher passed the filters, so a derived class can
    /// chain without asking twice.
    pub fn start_touch(
        &mut self,
        entity: &mut EntityCore,
        other: EntityId,
        cx: &mut Context<'_>,
    ) -> bool {
        if !self.passes_trigger_filters(entity, other, cx) {
            return false;
        }
        let added = !self.touching.contains(&other);
        if added {
            self.touching.push(other);
        }

        let me = entity.id();
        entity.fire_output(
            "OnStartTouch",
            Variant::Void,
            Some(other),
            Some(me),
            0.0,
            cx,
        );

        // "First entity to touch us that passes our filters." Note that it
        // tests the *count*, not `added` alone, so a second toucher arriving
        // while the first is still inside does not re-fire it.
        if added && self.touching.len() == 1 {
            entity.fire_output(
                "OnStartTouchAll",
                Variant::Void,
                Some(other),
                Some(me),
                0.0,
                cx,
            );
        }
        true
    }

    /// `CBaseTrigger::EndTouch` (`triggers.cpp:495`).
    ///
    /// > **It does not consult the filters and does not test `m_bDisabled`.**
    /// > Valve has two `//FIXME: Without this, triggers fire their EndTouch
    /// > outputs when they are disabled!` comments around the two places a
    /// > `m_bDisabled` test was commented out, and the behaviour they describe
    /// > is the shipped behaviour: disabling a trigger somebody is standing in
    /// > fires `OnEndTouch`. Reproduced, comments and all.
    pub fn end_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        let Some(index) = self.touching.iter().position(|&id| id == other) else {
            return;
        };
        self.touching.remove(index);

        let me = entity.id();
        entity.fire_output("OnEndTouch", Variant::Void, Some(other), Some(me), 0.0, cx);

        // "Loop through the touching entities backwards. Clean out old ones,
        // and look for existing" — a handle that has stopped resolving is
        // dropped rather than counted.
        self.touching.retain(|&id| cx.entity(id).is_some());
        if self.touching.is_empty() {
            entity.fire_output(
                "OnEndTouchAll",
                Variant::Void,
                Some(other),
                Some(me),
                0.0,
                cx,
            );
        }
    }

    /// `CBaseTrigger::TouchTest` (`triggers.cpp:278`).
    fn touch_test(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if self.disabled {
            return;
        }
        let me = entity.id();
        let output = match self.touching.is_empty() {
            false => "OnTouching",
            true => "OnNotTouching",
        };
        entity.fire_output(output, Variant::Void, None, Some(me), 0.0, cx);
    }

    /// `CBaseTrigger::Enable`/`Disable`/`InputToggle` (`:212`, `:260`, `:566`)
    /// and the two touch inputs, which is every input the class declares.
    ///
    /// Returns whether the name was one of them. A derived class calls this
    /// last, after its own.
    ///
    /// > **The immediate re-test is not here.** Each of the three calls
    /// > `PhysicsTouchTriggers()` straight afterwards, which in the original
    /// > re-enumerates what the trigger overlaps *now* — so enabling a trigger
    /// > the player is already standing in fires `OnStartTouch` in the same
    /// > tick. Here the touch pass is player-driven and runs once a tick, so
    /// > it fires on the next one instead: at most 15.6 ms late, and the
    /// > condition for closing the gap is a touch query the server can ask
    /// > mid-tick rather than at a fixed point in it.
    pub fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Enable") {
            self.disabled = false;
            entity.add_solid_flags(FSOLID_TRIGGER);
            return true;
        }
        if is("Disable") {
            self.disabled = true;
            entity.remove_solid_flags(FSOLID_TRIGGER);
            return true;
        }
        if is("Toggle") {
            // Deliberately not `self.disabled = !self.disabled` — see the
            // field's docs.
            match entity.is_solid_flag_set(FSOLID_TRIGGER) {
                true => entity.remove_solid_flags(FSOLID_TRIGGER),
                false => entity.add_solid_flags(FSOLID_TRIGGER),
            }
            return true;
        }
        if is("TouchTest") {
            self.touch_test(entity, cx);
            return true;
        }
        // `InputStartTouch`/`InputEndTouch` "pretend we just touched the
        // trigger", with the **caller** as the toucher. They bypass the link
        // list entirely, so a `StartTouch` faked this way owes no `EndTouch`
        // and the pair has to be sent by hand. Zero shipped connections fire
        // either.
        if is("StartTouch") {
            if let Some(caller) = input.caller {
                self.start_touch(entity, caller, cx);
            }
            return true;
        }
        if is("EndTouch") {
            if let Some(caller) = input.caller {
                self.end_touch(entity, caller, cx);
            }
            return true;
        }
        false
    }

    pub fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("disabled", self.disabled.to_string()),
            ("filtername", format!("{:?}", self.filter_name)),
            ("touching", self.touching.len().to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// trigger_multiple and trigger_once
// ---------------------------------------------------------------------------

/// Which of `CTriggerMultiple`'s three think functions is armed. Valve's
/// `SetThink` function pointer, as the enum a Rust class keeps instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum MultipleThink {
    /// `SetThink( NULL )`.
    #[default]
    None,
    /// `MultiWaitOver` — the re-trigger delay has run out.
    WaitOver,
    /// `SUB_Remove` — a `trigger_once` going away.
    Remove,
}

/// `CTriggerMultiple` (`triggers.cpp:100`) and `CTriggerOnce` (`:1004`) —
/// **2,375 entities, and between them the most-used trigger in the game.**
///
/// One struct for both, because `CTriggerOnce::Spawn` is `BaseClass::Spawn`
/// plus `m_flWait = -1` and nothing else. That one assignment is what makes it
/// "once": a `wait` that is not positive sends `ActivateMultiTrigger` down the
/// branch that stops touching and schedules its own removal.
pub struct TriggerMultiple {
    base: BaseTrigger,
    /// `m_flWait` — `CBaseToggle`'s, and the only field of it a trigger reads.
    /// Seconds between triggerings; `-1` means never again.
    wait: f32,
    /// Whether this is a `trigger_once`, which is entirely a statement about
    /// what `Spawn` does to [`wait`](TriggerMultiple::wait).
    once: bool,
    /// `m_pfnTouch` — `SetTouch( NULL )` stops a `trigger_once` firing twice
    /// in the 0.1 s before it deletes itself.
    touch_enabled: bool,
    think: MultipleThink,
}

pub static MULTIPLE_KEYS: &[&str] = &["StartDisabled", "filtername", "wait"];
pub static ONCE_KEYS: &[&str] = &["StartDisabled", "filtername"];

pub static MULTIPLE_OUTPUTS: &[&str] = &[
    "OnStartTouch",
    "OnStartTouchAll",
    "OnEndTouch",
    "OnEndTouchAll",
    "OnTouching",
    "OnNotTouching",
    "OnTrigger",
];

impl TriggerMultiple {
    fn new(once: bool) -> TriggerMultiple {
        TriggerMultiple {
            base: BaseTrigger::default(),
            wait: 0.0,
            once,
            touch_enabled: true,
            think: MultipleThink::None,
        }
    }

    pub fn create() -> Box<dyn Behaviour> {
        Box::new(TriggerMultiple::new(false))
    }

    pub fn create_once() -> Box<dyn Behaviour> {
        Box::new(TriggerMultiple::new(true))
    }

    /// `CTriggerMultiple::ActivateMultiTrigger` (`triggers.cpp:968`).
    ///
    /// > **The re-trigger lock-out is the think schedule**, not a separate
    /// > timer: `if ( GetNextThink() > gpGlobals->curtime ) return;`. So a
    /// > `trigger_multiple` with `wait 1` refuses everything for a second by
    /// > having a think pending, and `MultiWaitOver` exists only to be the
    /// > thing that pending think runs.
    fn activate_multi_trigger(
        &mut self,
        entity: &mut EntityCore,
        activator: EntityId,
        cx: &mut Context<'_>,
    ) {
        if entity.next_think(cx) > cx.curtime() {
            return;
        }

        let me = entity.id();
        entity.fire_output(
            "OnTrigger",
            Variant::Void,
            Some(activator),
            Some(me),
            0.0,
            cx,
        );

        if self.wait > 0.0 {
            self.think = MultipleThink::WaitOver;
            entity.set_next_think(cx.curtime() + self.wait, cx);
        } else {
            // "we can't just remove (self) here, because this is a touch
            // function called while C code is looping through area links" —
            // which is true of this port too, for a different reason: the
            // touch pass is iterating the overlap list.
            self.touch_enabled = false;
            self.think = MultipleThink::Remove;
            entity.set_next_think(cx.curtime() + 0.1, cx);
        }
    }
}

impl Behaviour for TriggerMultiple {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        // `wait` is `CBaseToggle`'s and a `trigger_once` overwrites it in
        // `Spawn`, so it is read for both and consumed for neither's benefit
        // in the once case. No shipped `trigger_once` carries the key.
        if !self.once && key.eq_ignore_ascii_case("wait") {
            self.wait = atof(value);
            return true;
        }
        self.base.key_value(key, value)
    }

    /// `CTriggerMultiple::Spawn` (`triggers.cpp:933`), then `CTriggerOnce`'s
    /// one extra line.
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        self.base.spawn(entity);
        self.base.init_trigger(entity);
        if self.wait == 0.0 {
            self.wait = 0.2;
        }
        if self.once {
            self.wait = -1.0;
        }
        SpawnResult::Ok
    }

    fn activate(&mut self, _entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.base.activate(cx);
    }

    fn think(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) {
        match std::mem::take(&mut self.think) {
            MultipleThink::None => {}
            // `MultiWaitOver` — `SetThink( NULL )` and nothing else. The
            // schedule was already cleared before this ran, which is what
            // re-opens `ActivateMultiTrigger`'s guard.
            MultipleThink::WaitOver => {}
            // `SUB_Remove`.
            MultipleThink::Remove => entity.remove(),
        }
    }

    fn start_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        self.base.start_touch(entity, other, cx);
    }

    /// `CTriggerMultiple::MultiTouch` (`triggers.cpp:955`).
    fn touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        if !self.touch_enabled {
            return;
        }
        if self.base.passes_trigger_filters(entity, other, cx) {
            self.activate_multi_trigger(entity, other, cx);
        }
    }

    fn end_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        self.base.end_touch(entity, other, cx);
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        self.base.accept_input(entity, input, cx)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = self.base.describe();
        out.push(("wait", self.wait.to_string()));
        out.push(("think", format!("{:?}", self.think)));
        out
    }
}

// ---------------------------------------------------------------------------
// trigger_hurt
// ---------------------------------------------------------------------------

/// `m_damageModel` — `DAMAGEMODEL_DOUBLE_FORGIVENESS` (`triggers.h:196`).
/// **Zero of the game's 215 `trigger_hurt`s select it**; every one carries
/// `damagemodel 0`.
const DAMAGEMODEL_DOUBLE_FORGIVENESS: i32 = 1;

/// "The forgive time is how long the trigger must go without harming anyone in
/// order that its accumulated damage be reset" (`triggers.cpp:845`).
const TRIGGER_HURT_FORGIVE_TIME: f32 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HurtThink {
    #[default]
    None,
    /// `HurtThink` — every half second while something is inside.
    Hurt,
    /// `RadiationThink` — every quarter second, for ever, from spawn.
    Radiation,
}

/// `CTriggerHurt` (`triggers.h:161`) — 215 entities, and the one class in this
/// stage whose *effect* is missing while its *timing* is complete.
///
/// # There is no damage, and that is the whole of what is absent
///
/// `HurtEntity` in the original computes a damage position, guesses a physics
/// force, and calls `pOther->TakeDamage( info )`. This port has no health, no
/// `CTakeDamageInfo`, no force and no death, so that middle step is gone. What
/// is not gone is everything around it: the `m_takedamage` gate, the filter
/// re-test, the choice between `OnHurtPlayer` and `OnHurt`, the half-second
/// think cadence, the half-damage on the way out, and the doubling model's
/// arithmetic on `m_flDamage`. A map's `OnHurtPlayer` therefore fires exactly
/// when the shipped game fires it — 5 connections in the game do — and the
/// player does not die.
pub struct TriggerHurt {
    base: BaseTrigger,
    /// `m_flDamage`, per second. The doubling model multiplies it in place,
    /// which is why `m_flOriginalDamage` exists.
    damage: f32,
    original_damage: f32,
    damage_cap: f32,
    /// `m_bitsDamageInflict` — the `DMG_*` set. Read for exactly one bit,
    /// [`DMG_RADIATION`].
    damage_type: i32,
    damage_model: i32,
    /// `m_bNoDmgForce`. Parsed and unread: the force it suppresses is the one
    /// this port does not apply.
    no_damage_force: bool,
    last_damage_time: f32,
    damage_reset_time: f32,
    /// `m_hurtEntities` — who was hurt during the *current* `HurtAllTouchers`.
    /// `EndTouch` reads it to decide whether to charge a parting half-dose.
    hurt_entities: Vec<EntityId>,
    think: HurtThink,
}

pub static HURT_KEYS: &[&str] = &[
    "StartDisabled",
    "filtername",
    "damage",
    "damagecap",
    "damagetype",
    "damagemodel",
    "nodmgforce",
];

pub static HURT_INPUTS: InputDefs = &[
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("TouchTest", FieldType::Void),
    InputDef::new("StartTouch", FieldType::Void),
    InputDef::new("EndTouch", FieldType::Void),
    // `DEFINE_INPUT( m_flDamage, FIELD_FLOAT, "SetDamage" )` — a bare field
    // write with no handler, which `AcceptInput` performs itself.
    InputDef::new("SetDamage", FieldType::Float),
];

pub static HURT_OUTPUTS: &[&str] = &[
    "OnStartTouch",
    "OnStartTouchAll",
    "OnEndTouch",
    "OnEndTouchAll",
    "OnTouching",
    "OnNotTouching",
    "OnHurt",
    "OnHurtPlayer",
];

impl Default for TriggerHurt {
    fn default() -> TriggerHurt {
        TriggerHurt {
            base: BaseTrigger::default(),
            damage: 0.0,
            original_damage: 0.0,
            // "This field came along after levels were built so the field
            // defaults to 20 here in the constructor" (`triggers.h:167`).
            damage_cap: 20.0,
            damage_type: 0,
            damage_model: 0,
            no_damage_force: false,
            last_damage_time: 0.0,
            damage_reset_time: 0.0,
            hurt_entities: Vec::new(),
            think: HurtThink::None,
        }
    }
}

impl TriggerHurt {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<TriggerHurt>::default()
    }

    /// `CTriggerHurt::HurtEntity` (`triggers.cpp:764`), minus the damage.
    ///
    /// Returns whether anything was hurt, which is what stops
    /// [`HurtThink`](HurtThink::Hurt) rescheduling itself once the trigger is
    /// empty.
    fn hurt_entity(
        &mut self,
        entity: &mut EntityCore,
        other: EntityId,
        cx: &mut Context<'_>,
    ) -> bool {
        let Some(other_entity) = cx.entity(other) else {
            return false;
        };
        let (takes_damage, is_player) = (
            other_entity.core.take_damage,
            other_entity.behaviour.is_player(),
        );
        if !takes_damage || !self.base.passes_trigger_filters(entity, other, cx) {
            return false;
        }

        // …`TakeDamage` would go here.

        let me = entity.id();
        let output = match is_player {
            true => "OnHurtPlayer",
            false => "OnHurt",
        };
        entity.fire_output(output, Variant::Void, Some(other), Some(me), 0.0, cx);
        self.hurt_entities.push(other);
        true
    }

    /// `CTriggerHurt::HurtAllTouchers` (`triggers.cpp:846`) — returns how many
    /// it hurt.
    ///
    /// > **It walks the *touch-link* list, not `m_hTouchingEntities`.** The
    /// > two differ by exactly the entities that failed the filters, which is
    /// > why `HurtEntity` re-tests them one at a time. Reading the trigger's
    /// > own list instead would make the filter test dead code and would hurt
    /// > nothing differently in Portal 2 — where one `trigger_hurt` in 215 has
    /// > a filter — which is precisely the kind of difference that is cheaper
    /// > to keep than to rediscover.
    fn hurt_all_touchers(&mut self, entity: &mut EntityCore, dt: f32, cx: &mut Context<'_>) -> i32 {
        self.last_damage_time = cx.curtime();
        self.hurt_entities.clear();

        let touchers: Vec<EntityId> = touch::touching(entity).collect();
        let mut hurt_count = 0;
        for other in touchers {
            if self.hurt_entity(entity, other, cx) {
                hurt_count += 1;
            }
        }

        if self.damage_model == DAMAGEMODEL_DOUBLE_FORGIVENESS {
            if hurt_count == 0 {
                if cx.curtime() > self.damage_reset_time {
                    self.damage = self.original_damage;
                }
            } else {
                self.damage *= 2.0;
                if self.damage > self.damage_cap {
                    self.damage = self.damage_cap;
                }
                self.damage_reset_time = cx.curtime() + TRIGGER_HURT_FORGIVE_TIME;
            }
        }

        // `float fldmg = m_flDamage * dt` is computed at the top of the C++
        // and passed to `HurtEntity`; with no damage applied it is dead, and
        // `dt` survives only as this note. It is `0.5` from `HurtThink` and
        // the real elapsed time from `RadiationThink`.
        let _ = dt;
        hurt_count
    }
}

impl Behaviour for TriggerHurt {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("damage") {
            self.damage = atof(value);
            return true;
        }
        if is("damagecap") {
            self.damage_cap = atof(value);
            return true;
        }
        if is("damagetype") {
            self.damage_type = atoi(value);
            return true;
        }
        if is("damagemodel") {
            self.damage_model = atoi(value);
            return true;
        }
        if is("nodmgforce") {
            self.no_damage_force = atoi(value) != 0;
            return true;
        }
        self.base.key_value(key, value)
    }

    /// `CTriggerHurt::Spawn` (`triggers.cpp:668`).
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult {
        self.base.spawn(entity);
        self.base.init_trigger(entity);
        self.original_damage = self.damage;

        entity.set_next_think(NEVER_THINK, cx);
        self.think = HurtThink::None;
        if self.damage_type & DMG_RADIATION != 0 {
            // A random start time, so that the 27 radiation triggers in the
            // game do not all think on the same tick. It is the only place in
            // this module that touches the random stream.
            self.think = HurtThink::Radiation;
            let delay = cx.random().float(0.0, 0.5);
            entity.set_next_think(cx.curtime() + delay, cx);
        }
        SpawnResult::Ok
    }

    fn activate(&mut self, _entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.base.activate(cx);
    }

    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        match self.think {
            HurtThink::None => {}
            // `CTriggerHurt::HurtThink` (`:809`): keep going while it is
            // hurting somebody.
            HurtThink::Hurt => match self.hurt_all_touchers(entity, 0.5, cx) <= 0 {
                true => self.think = HurtThink::None,
                false => entity.set_next_think(cx.curtime() + 0.5, cx),
            },
            // `CTriggerHurt::RadiationThink` (`:734`), minus the geiger
            // counter — which is a client-side sound and a HUD element.
            // **It re-arms unconditionally**, so a radiation trigger thinks
            // four times a second for the whole level whether or not anybody
            // is in it.
            HurtThink::Radiation => {
                let dt = cx.curtime() - self.last_damage_time;
                if dt >= 0.5 {
                    self.hurt_all_touchers(entity, dt, cx);
                }
                entity.set_next_think(cx.curtime() + 0.25, cx);
            }
        }
    }

    fn start_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        self.base.start_touch(entity, other, cx);
    }

    /// `CTriggerHurt::Touch` (`triggers.cpp:904`) — arm the half-second think
    /// if it is not already armed, and nothing else.
    ///
    /// > **A radiation trigger never takes this branch**, because its think
    /// > function is never null. That is Valve's `if ( m_pfnThink == NULL )`
    /// > doing double duty as "am I already hurting people".
    fn touch(&mut self, entity: &mut EntityCore, _other: EntityId, cx: &mut Context<'_>) {
        if self.think == HurtThink::None {
            self.think = HurtThink::Hurt;
            entity.set_next_think(cx.curtime(), cx);
        }
    }

    /// `CTriggerHurt::EndTouch` (`triggers.cpp:822`).
    ///
    /// A parting half-dose for anything that was inside but had not yet been
    /// caught by a `HurtThink` — otherwise a fast walk through a slime pit
    /// would be free.
    fn end_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        if self.base.passes_trigger_filters(entity, other, cx)
            && !self.hurt_entities.contains(&other)
        {
            self.hurt_entity(entity, other, cx);
        }
        self.base.end_touch(entity, other, cx);
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        if input.name.eq_ignore_ascii_case("SetDamage") {
            self.damage = input.value.float();
            return true;
        }
        self.base.accept_input(entity, input, cx)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = self.base.describe();
        out.push(("damage", self.damage.to_string()));
        out.push(("damagecap", self.damage_cap.to_string()));
        out.push(("damagetype", self.damage_type.to_string()));
        out.push(("think", format!("{:?}", self.think)));
        out
    }
}

// ---------------------------------------------------------------------------
// trigger_push
// ---------------------------------------------------------------------------

/// `CTriggerPush` (`triggers.cpp:2450`) — 192 entities: the airlock blowers,
/// the excursion funnels' guide rails, and the fans that keep you out of the
/// scenery.
pub struct TriggerPush {
    base: BaseTrigger,
    /// `m_vecPushDir` — written as *angles* by the mapper and stored, after
    /// `Spawn`, as a direction **in the entity's own frame**.
    push_dir: Vec3,
    /// `m_flAlternateTicksFix`. 190 of the 192 leave it at zero.
    alternate_ticks_fix: f32,
    /// `m_flPushSpeed` — `m_flSpeed` after `Activate`'s scaling. See
    /// [`TriggerPush::activate`](Behaviour::activate).
    push_speed: f32,
}

pub static PUSH_KEYS: &[&str] = &[
    "StartDisabled",
    "filtername",
    "pushdir",
    "alternateticksfix",
];

pub static PUSH_INPUTS: InputDefs = &[
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("TouchTest", FieldType::Void),
    InputDef::new("StartTouch", FieldType::Void),
    InputDef::new("EndTouch", FieldType::Void),
    InputDef::new("SetPushDirection", FieldType::Vector),
];

impl Default for TriggerPush {
    fn default() -> TriggerPush {
        TriggerPush {
            base: BaseTrigger::default(),
            push_dir: Vec3::ZERO,
            alternate_ticks_fix: 0.0,
            push_speed: 0.0,
        }
    }
}

impl TriggerPush {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<TriggerPush>::default()
    }

    /// `VectorIRotate( AngleVectors( pushdir ), EntityToWorldTransform() )` —
    /// the angles the mapper typed, turned into a direction and then moved
    /// *into* the entity's frame, which is where `m_vecPushDir` lives from
    /// `Spawn` onwards.
    ///
    /// The round trip through the entity's rotation is an identity for the
    /// 107 pushes in the game that are not turned, and is what makes a
    /// `trigger_push` parented to a rotating platform push the right way.
    fn to_local(angles_as_dir: Vec3, entity: &EntityCore) -> Vec3 {
        Self::world_to_local(entity) * angles_as_dir
    }

    fn world_to_local(entity: &EntityCore) -> Mat3 {
        // The inverse of an orthonormal rotation is its transpose, which is
        // what `VectorIRotate` does.
        crate::math::angle_matrix(entity.angles).transpose()
    }
}

impl Behaviour for TriggerPush {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("pushdir") {
            // Still angles at this point; `Spawn` converts.
            self.push_dir = string_to_vector(value);
            return true;
        }
        if key.eq_ignore_ascii_case("alternateticksfix") {
            self.alternate_ticks_fix = atof(value);
            return true;
        }
        self.base.key_value(key, value)
    }

    /// `CTriggerPush::Spawn` (`triggers.cpp:2488`).
    ///
    /// Note the order: the `pushdir` conversion happens **before**
    /// `BaseClass::Spawn`, which is the one place in this family where a
    /// derived class does work first.
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        self.push_dir = TriggerPush::to_local(move_dir(self.push_dir), entity);
        self.base.spawn(entity);
        self.base.init_trigger(entity);
        if entity.speed == 0.0 {
            // `m_flSpeed = 100`, which is **not** the 40 the FGD offers as a
            // default. The code wins; 30 of the game's 192 rely on it.
            entity.speed = 100.0;
        }
        SpawnResult::Ok
    }

    /// `CTriggerPush::Activate` (`triggers.cpp:2511`).
    ///
    /// > **A Portal 2 single-player push is twice as strong as the map says**,
    /// > and Valve labels the reason `DIRTY HACK TO FOLLOW`: the game was
    /// > tuned with `sv_alternateticks 1` and ships with it at `0` on PC
    /// > (`baseserver.cpp:221`), so `CTriggerPush::Activate` doubles the speed
    /// > to compensate whenever `maxClients == 1`. Both conditions are
    /// > constants here — this port has one player and no alternate ticks —
    /// > so the doubling is unconditional, and dropping it would make every
    /// > airlock in the game half as strong as the shipped one.
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.base.activate(cx);
        // The `m_flAlternateTicksFix != 0 && IsSimulatingOnAlternateTicks()`
        // branch is unreachable for the second reason above; 190 of the 192
        // fail the first as well.
        self.push_speed = entity.speed * 2.0;
    }

    fn start_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        self.base.start_touch(entity, other, cx);
    }

    /// `CTriggerPush::Touch` (`triggers.cpp:2545`).
    fn touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        let Some(other_core) = cx.entity(other).map(|e| &e.core) else {
            return;
        };
        let (solid, move_type, parented, flags, base_velocity) = (
            other_core.is_solid(),
            other_core.move_type,
            other_core.parent.is_some(),
            other_core.flags,
            other_core.base_velocity,
        );

        // "if ( !pOther->IsSolid() || (movetype == PUSH || movetype == NONE) )"
        if !solid || matches!(move_type, MoveType::Push | MoveType::None) {
            return;
        }
        if !self.base.passes_trigger_filters(entity, other, cx) {
            return;
        }
        // "FIXME: If something is hierarchically attached, should we try to
        // push the parent?"
        if parented {
            return;
        }

        let dir = crate::math::angle_matrix(entity.angles) * self.push_dir;

        if entity.has_spawn_flags(SF_TRIG_PUSH_ONCE) {
            if let Some(core) = cx.entity_mut(other) {
                // `ApplyAbsVelocityImpulse`.
                core.velocity += self.push_speed * dir;
                if dir.z > 0.0 {
                    core.flags &= !FL_ONGROUND;
                }
            }
            entity.remove();
            return;
        }

        match move_type {
            // `MOVETYPE_NOCLIP` falls straight out of the switch, so a
            // noclipping player is not pushed. Worth keeping: it is the
            // difference between `noclip` being a debug tool and `noclip`
            // being subject to the level's fans.
            MoveType::None | MoveType::Push | MoveType::Noclip => {}
            MoveType::Walk => {
                let mut push = self.push_speed * dir;
                if flags & FL_BASEVELOCITY != 0 {
                    push += base_velocity;
                }
                let lift = push.z > 0.0 && flags & FL_ONGROUND != 0;
                if let Some(core) = cx.entity_mut(other) {
                    if lift {
                        core.flags &= !FL_ONGROUND;
                        core.origin.z += 1.0;
                    }
                    core.base_velocity = push;
                    core.flags |= FL_BASEVELOCITY;
                }
            }
        }
    }

    fn end_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        self.base.end_touch(entity, other, cx);
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        if input.name.eq_ignore_ascii_case("SetPushDirection") {
            let angles = match &input.value {
                Variant::Vector(v) => *v,
                _ => Vec3::ZERO,
            };
            self.push_dir = TriggerPush::to_local(move_dir(angles), entity);
            return true;
        }
        self.base.accept_input(entity, input, cx)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = self.base.describe();
        out.push(("pushdir (local)", format!("{:?}", self.push_dir)));
        out.push(("pushspeed", self.push_speed.to_string()));
        out
    }
}

// ---------------------------------------------------------------------------
// trigger_teleport
// ---------------------------------------------------------------------------

/// `CTriggerTeleport` (`triggers.cpp:2714`) — 110 entities, 37 of which are
/// the elevator that carries you between chapters.
#[derive(Default)]
pub struct TriggerTeleport {
    base: BaseTrigger,
    /// `m_iLandmark` — the local reference point. 41 of the 110 set it, and
    /// its presence changes the teleport from "put them at the target" to
    /// "carry their offset across".
    landmark: Option<String>,
    /// `m_bUseLandmarkAngles`. Two of the 110 set it.
    use_landmark_angles: bool,
    /// `m_bCheckDestIfClearForPlayer` — needs `IsSpawnPointValid`, which is
    /// game rules. Zero shipped maps set it.
    check_dest_if_clear: bool,
}

pub static TELEPORT_KEYS: &[&str] = &[
    "StartDisabled",
    "filtername",
    "landmark",
    "UseLandmarkAngles",
    "CheckDestIfClearForPlayer",
];

pub static TELEPORT_INPUTS: InputDefs = &[
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("TouchTest", FieldType::Void),
    InputDef::new("StartTouch", FieldType::Void),
    InputDef::new("EndTouch", FieldType::Void),
    InputDef::new("SetRemoteDestination", FieldType::String),
];

impl TriggerTeleport {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<TriggerTeleport>::default()
    }
}

impl Behaviour for TriggerTeleport {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("landmark") {
            self.landmark = Some(value.to_owned());
            return true;
        }
        if is("UseLandmarkAngles") {
            self.use_landmark_angles = atoi(value) != 0;
            return true;
        }
        if is("CheckDestIfClearForPlayer") {
            self.check_dest_if_clear = atoi(value) != 0;
            return true;
        }
        self.base.key_value(key, value)
    }

    /// `CTriggerTeleport::Spawn` (`triggers.cpp:2750`).
    ///
    /// **It does not call `BaseClass::Spawn`**, so the three spawnflag
    /// promotions never run for a teleport. Reproduced; none of the flags they
    /// test appears on any of the game's 110.
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        self.base.init_trigger(entity);
        SpawnResult::Ok
    }

    fn activate(&mut self, _entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.base.activate(cx);
    }

    fn start_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        self.base.start_touch(entity, other, cx);
    }

    /// `CTriggerTeleport::Touch` (`triggers.cpp:2776`).
    fn touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        if !self.base.passes_trigger_filters(entity, other, cx) {
            return;
        }
        let Some(target_name) = entity.target.clone() else {
            return;
        };
        let Some(target) = cx.find_by_name(&target_name) else {
            eprintln!(
                "source-engine: server: teleport trigger '{}' cannot find destination named '{target_name}'",
                entity.debug_name()
            );
            return;
        };

        let Some(other_entity) = cx.entity(other) else {
            return;
        };
        let is_player = other_entity.behaviour.is_player();
        let origin = other_entity.core.origin;
        let velocity = other_entity.core.velocity;
        // `QAngle qActivatorEyeAngles = pOther->GetAbsAngles()`, replaced by
        // `EyeAngles()` for a player — and the player entity's `angles` *are*
        // its view angles here, so the branch collapses. See
        // `classes::Player`.
        let angles = other_entity.core.angles;
        let mins_z = other_entity.core.model_bounds.mins.z;

        let Some(target_entity) = cx.entity(target) else {
            return;
        };
        let (target_origin, target_angles) = (target_entity.core.origin, target_entity.core.angles);

        // The landmark is looked up with the toucher as both activator and
        // caller, so `!activator` in a landmark name would resolve — no
        // shipped map does that.
        let landmark = self
            .landmark
            .as_deref()
            .and_then(|name| cx.find_by_name(name))
            .and_then(|id| cx.entity(id))
            .map(|e| (e.core.origin, e.core.angles));

        let (new_origin, mut new_angles, new_velocity);
        match landmark {
            Some((landmark_origin, landmark_angles)) => {
                // `ConcatTransforms( target->EntityToWorld,
                //   MatrixInvert( landmark->EntityToWorld ) )`, applied to the
                // toucher's origin, angles and velocity. Written out with
                // `glam` rather than with Valve's three matrix helpers; the
                // arithmetic is the same and the convention difference is
                // `rustdocs/MATERIALS.md`'s (column-major, multiply on the
                // left).
                let to_world = crate::math::angle_matrix(target_angles);
                let from_local = crate::math::angle_matrix(landmark_angles).transpose();
                let rotation = to_world * from_local;
                new_origin = target_origin + rotation * (origin - landmark_origin);
                new_velocity = rotation * velocity;
                new_angles = matrix_angles(rotation * crate::math::angle_matrix(angles));
            }
            None => {
                // "make origin adjustments in case the teleportee is a player
                // (origin in center, not at feet)". Portal 2's player hull has
                // `mins.z == 0`, so this is a no-op for every teleport in the
                // game — and it is the line that would matter for anything
                // whose origin is not at its feet.
                new_origin = match is_player {
                    true => target_origin - Vec3::new(0.0, 0.0, mins_z),
                    false => target_origin,
                };
                // "there isn't a local landmark specified so just set the
                // origin to the origin of the destination landmark" — and the
                // velocity and angles are the toucher's, untouched.
                new_angles = angles;
                new_velocity = velocity;
            }
        }

        if self.use_landmark_angles {
            new_angles = target_angles;
        }

        touch::teleport(
            cx,
            other,
            Teleport {
                origin: Some(new_origin),
                angles: Some(new_angles),
                velocity: Some(new_velocity),
            },
        );
    }

    fn end_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        self.base.end_touch(entity, other, cx);
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        if input.name.eq_ignore_ascii_case("SetRemoteDestination") {
            entity.target = Some(input.value.to_string());
            return true;
        }
        self.base.accept_input(entity, input, cx)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = self.base.describe();
        out.push(("landmark", format!("{:?}", self.landmark)));
        out.push(("UseLandmarkAngles", self.use_landmark_angles.to_string()));
        out.push((
            "CheckDestIfClearForPlayer",
            self.check_dest_if_clear.to_string(),
        ));
        out
    }
}

/// `MatrixAngles` (`mathlib/mathlib_base.cpp:812`) — a rotation back into
/// pitch/yaw/roll.
///
/// The inverse of [`crate::math::angle_matrix`], and the only place this port
/// needs one: `TransformAnglesToWorldSpace` is how a landmark teleport carries
/// the toucher's facing across, and there is no way to compose two rotations
/// as angles without going through a matrix.
///
/// The gimbal-lock branch is Valve's: within about a thousandth of a degree of
/// straight up or down the yaw and roll are the same axis, and it puts all of
/// the turn into yaw.
fn matrix_angles(m: Mat3) -> Vec3 {
    // `angle_matrix`'s columns are forward, left and up, so the components
    // below are read out of the same places `MatrixAngles` reads them.
    let forward = m.x_axis;
    let left = m.y_axis;
    let up = m.z_axis;

    let xy_dist = (forward.x * forward.x + forward.y * forward.y).sqrt();
    // Valve's `0.001f`.
    if xy_dist > 0.001 {
        Vec3::new(
            (-forward.z).atan2(xy_dist).to_degrees(),
            forward.y.atan2(forward.x).to_degrees(),
            left.z.atan2(up.z).to_degrees(),
        )
    } else {
        Vec3::new(
            (-forward.z).atan2(xy_dist).to_degrees(),
            (-left.x).atan2(left.y).to_degrees(),
            0.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MatrixAngles( AngleMatrix( a ) ) == a`, which is the property the
    /// landmark teleport rests on.
    #[test]
    fn matrix_angles_inverts_angle_matrix() {
        for angles in [
            Vec3::ZERO,
            Vec3::new(0.0, 90.0, 0.0),
            Vec3::new(-30.0, 145.0, 0.0),
            Vec3::new(20.0, -75.0, 15.0),
        ] {
            let round_trip = matrix_angles(crate::math::angle_matrix(angles));
            for i in 0..3 {
                let (a, b) = (angles[i], round_trip[i]);
                // Angles come back in (-180, 180]; compare the turn rather
                // than the number.
                let delta = ((a - b + 540.0) % 360.0) - 180.0;
                assert!(
                    delta.abs() < 1e-3,
                    "{angles:?} round-tripped to {round_trip:?}"
                );
            }
        }
    }

    /// Straight up: yaw and roll are the same axis and Valve puts the whole
    /// turn into yaw.
    #[test]
    fn matrix_angles_folds_gimbal_lock_into_yaw() {
        let angles = matrix_angles(crate::math::angle_matrix(Vec3::new(-90.0, 40.0, 0.0)));
        assert!((angles.x + 90.0).abs() < 1e-3, "pitch is straight up");
        assert_eq!(angles.z, 0.0, "roll is given up");
    }
}
