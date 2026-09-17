//! The logic family: the entities a Portal 2 map is actually made of.
//!
//! `logicrelay.cpp`, `logicauto.cpp`, `logicentities.cpp` (`CLogicBranch`,
//! `CLogicCase`, `CTimerEntity`, `CMathCounter`) and
//! `func_instance_io_proxy.cpp`.
//!
//! Between them these seven classnames are **10,133 of the shipped game's
//! 60,925 entities**, and two of them are the first and thirteenth commonest
//! entities in the game. None of them draws anything, none of them moves, and
//! every one of them is pure entity I/O — which is why they are stage 2's
//! classes and why porting them exercises nearly the whole subsystem.

use crate::server::class::{Behaviour, Context, InputDef, InputDefs, SpawnResult, NEVER_THINK};
use crate::server::entity::{EntityCore, EntityId};
use crate::server::io::{FieldType, Input, Variant};
use crate::server::keyvalue::{atof, atoi};

// ---------------------------------------------------------------------------
// logic_relay
// ---------------------------------------------------------------------------

/// `SF_REMOVE_ON_FIRE` (`logicrelay.cpp:22`) — 308 of the game's relays.
const SF_RELAY_REMOVE_ON_FIRE: u32 = 0x001;
/// `SF_ALLOW_FAST_RETRIGGER` — 784. The other 6,990 latch.
const SF_RELAY_ALLOW_FAST_RETRIGGER: u32 = 0x002;

/// `CLogicRelay` (`game/server/logicrelay.cpp`) — the commonest entity in the
/// game, 8,082 of them, and a complete worked example of the subsystem.
///
/// It forwards one input to one output and can be switched off. Everything
/// interesting about it is the **latch**: unless the mapper set
/// `SF_ALLOW_FAST_RETRIGGER`, a relay that fires shuts itself until its own
/// slowest connection has gone out, by posting `EnableRefire` at itself at
/// `GetMaxDelay() + 0.001`. Without it a relay re-triggered during its own
/// delay double-fires, and 86% of the game's relays rely on it.
pub struct Relay {
    /// `m_bDisabled` — `StartDisabled`. 249 of the game's relays set it, and
    /// one of them is why `sp_a1_intro1` asks for an exposure ceiling of 1.5
    /// rather than 5: both `@rl_prestasis_exposure_reload` and
    /// `@rl_poststasis_exposure_reload` are triggered at map spawn, and the
    /// second is `StartDisabled 1`.
    pub disabled: bool,
    /// `m_bWaitForRefire` — the latch. Not a key; set only by firing.
    pub wait_for_refire: bool,
}

impl Relay {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(Relay {
            disabled: false,
            wait_for_refire: false,
        })
    }
}

impl Behaviour for Relay {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("StartDisabled") {
            self.disabled = atoi(value) != 0;
            return true;
        }
        false
    }

    /// `CLogicRelay::Activate` (`logicrelay.cpp:60`).
    ///
    /// > **The think is scheduled only if something is connected to
    /// > `OnSpawn`.** Otherwise the entity never thinks at all — which is what
    /// > keeps 7,754 of the game's 8,082 relays out of the think list
    /// > entirely.
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if entity.output_count("OnSpawn") > 0 {
            entity.set_next_think(cx.curtime() + 0.01, cx);
        }
    }

    /// `CLogicRelay::Think` (`logicrelay.cpp:74`) — fire `OnSpawn`.
    ///
    /// This is how a map starts itself without anything touching it, and it is
    /// the path `sp_a1_intro1` takes: `@rl_lighting_fixup`'s `OnSpawn` is what
    /// triggers the two exposure relays.
    ///
    /// The activator is **the relay itself**, not null — `FireOutput( this,
    /// this )` — so `!activator` in a chain started this way resolves to the
    /// relay.
    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let me = Some(entity.id());
        entity.fire_output("OnSpawn", Variant::Void, me, me, 0.0, cx);

        // Valve's comment: "We only get here if we had OnSpawn connections,
        // so this is safe."
        if entity.has_spawn_flags(SF_RELAY_REMOVE_ON_FIRE) {
            entity.remove();
        }
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Enable") {
            self.disabled = false;
        } else if is("Disable") {
            self.disabled = true;
        } else if is("Toggle") {
            self.disabled = !self.disabled;
        } else if is("EnableRefire") {
            // Only ever posted by this entity at itself; see `Trigger`.
            self.wait_for_refire = false;
        } else if is("CancelPending") {
            entity.cancel_pending(cx);
            // "Stop waiting; allow another Trigger."
            self.wait_for_refire = false;
        } else if is("Trigger") {
            self.trigger(entity, input, cx);
        } else {
            return false;
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("StartDisabled", self.disabled.to_string()),
            ("waiting for refire", self.wait_for_refire.to_string()),
        ]
    }
}

impl Relay {
    /// `CLogicRelay::InputTrigger` (`logicrelay.cpp:138`).
    fn trigger(&mut self, entity: &mut EntityCore, input: &Input<'_>, cx: &mut Context<'_>) {
        if self.disabled {
            return;
        }
        if self.wait_for_refire {
            // Valve prints three lines of asterisks here. One line, and only
            // once per entity, because a relay hammered by a timer would
            // otherwise fill the console.
            eprintln!(
                "source-engine: server: logic_relay {} was triggered while awaiting refire; \
                 its outputs did not fire",
                entity.debug_name()
            );
            return;
        }

        // **The activator is forwarded, not replaced.** This one line is what
        // makes `!activator` work across a chain of relays.
        let me = Some(entity.id());
        entity.fire_output("OnTrigger", Variant::Void, input.activator, me, 0.0, cx);

        if entity.has_spawn_flags(SF_RELAY_REMOVE_ON_FIRE) {
            entity.remove();
        } else if !entity.has_spawn_flags(SF_RELAY_ALLOW_FAST_RETRIGGER) {
            self.wait_for_refire = true;
            // The millisecond is Valve's and is load-bearing: at the same
            // time the last connection goes out, the queue is stable and
            // `EnableRefire` would be delivered *before* it.
            let delay = entity.max_output_delay("OnTrigger") + 0.001;
            entity.post_to_self("EnableRefire", delay, cx);
        }
    }
}

// ---------------------------------------------------------------------------
// logic_auto
// ---------------------------------------------------------------------------

/// `SF_AUTO_FIREONCE` (`logicauto.cpp:20`) — 998 of the game's 1,112.
const SF_AUTO_FIREONCE: u32 = 0x01;

/// `CLogicAuto` (`game/server/logicauto.cpp`) — how a map bootstraps itself.
///
/// 1,112 of them across 105 maps, and **every map in the game starts through
/// this one 0.2-second delay**: `Activate` schedules a think, the think fires
/// `OnMapSpawn`, and 998 of them then delete themselves.
///
/// The delay is not cosmetic. It is what guarantees that everything in the
/// level has spawned, activated and settled before any map logic runs — and it
/// is why the first fifth of a second of every Portal 2 level looks slightly
/// different from the sixth.
pub struct Auto {
    /// `m_globalstate` — fire only if this global is on. **Two entities in the
    /// whole game** carry it: `sp_a1_intro1`'s `is_console` and
    /// `sp_a1_wakeup`'s `glados_cables`.
    pub global_state: Option<String>,
}

impl Auto {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(Auto { global_state: None })
    }
}

impl Behaviour for Auto {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("globalstate") {
            self.global_state = Some(value.to_owned());
            return true;
        }
        false
    }

    /// `CLogicAuto::Activate` (`logicauto.cpp:75`). The `round_start` game
    /// event it also listens for is multiplayer's.
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        entity.set_next_think(cx.curtime() + 0.2, cx);
    }

    /// `CLogicAuto::Think` (`logicauto.cpp:89`).
    ///
    /// Three of the seven outputs are unreachable here and each is a fact
    /// rather than an omission: `OnMapTransition` and `OnLoadGame` need
    /// `gpGlobals->eLoadType`, which only a save/restore or a level transition
    /// sets and neither exists; `OnMultiNewMap` needs multiplayer game rules.
    /// A `map` command is `MapLoad_NewGame`, so `OnNewGame` and `OnMapSpawn`
    /// are what fire — 2,893 `OnMapSpawn` connections in the game against 2
    /// `OnNewGame`.
    ///
    /// **The activator is null**, unlike `logic_relay`'s: `FireOutput(NULL,
    /// this)`. A chain started by a `logic_auto` therefore has no
    /// `!activator`, which is correct — nothing did this, the map loaded.
    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        // `if (!m_globalstate || GlobalEntity_GetState(m_globalstate) == GLOBAL_ON)`.
        //
        // **The global state table is not ported** (it is `env_global`'s, 178
        // placed, and a stage-2 class list has to stop somewhere), so every
        // global reads as `GLOBAL_OFF` — which is *exactly* what
        // `CGlobalState::GetState` returns for a name nothing registered
        // (`globalstate.cpp:65`). So this is not an approximation of Valve's
        // behaviour; it is Valve's behaviour against an empty table, and the
        // only divergence is that the table stays empty. The two entities it
        // affects are named in [`Auto::global_state`].
        if self.global_state.is_some() {
            return;
        }

        let me = Some(entity.id());
        entity.fire_output("OnNewGame", Variant::Void, None, me, 0.0, cx);
        entity.fire_output("OnMapSpawn", Variant::Void, None, me, 0.0, cx);

        if entity.has_spawn_flags(SF_AUTO_FIREONCE) {
            entity.remove();
        }
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        match &self.global_state {
            Some(state) => vec![("globalstate", state.clone())],
            None => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// logic_branch
// ---------------------------------------------------------------------------

/// `CLogicBranch` (`logicentities.cpp:2556`) — a remembered boolean, 601
/// placed.
///
/// The distinction that matters is **set versus test**: `SetValue` changes the
/// value and fires nothing, `Test` fires without changing anything, and
/// `SetValueTest` does both. 1,175 of the game's 1,601 connections into a
/// branch are `SetValue`, so most of the time it is pure memory.
///
/// The other half is [`BranchList`], the `logic_branch_listener` below: a
/// branch keeps a list of the listeners monitoring it and posts
/// [`INPUT_BRANCH_CHANGED`] at each of them **when, and only when, its value
/// actually moves**. That is what makes `SetValue` — 1,175 of the game's 1,601
/// connections into a branch, and the input that fires no output of its own —
/// still reach a listener.
pub struct Branch {
    /// `m_bInValue` — `InitialValue`. 182 of the 601 set it.
    pub value: bool,
    /// `m_Listeners` — the [`BranchList`]s that registered with this branch in
    /// their own `Activate`.
    ///
    /// Held as plain ids rather than as anything resolved: a listener can be
    /// killed (three shipped connections do), and a handle that has stopped
    /// resolving is simply skipped, which is Valve's `if ( pEntity )`.
    listeners: Vec<EntityId>,
}

impl Branch {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(Branch {
            value: false,
            listeners: Vec::new(),
        })
    }

    /// `CLogicBranch::AddLogicBranchListener` (`logicentities.cpp:2731`) —
    /// the one line of this class another class reaches in to write.
    ///
    /// Valve dedups with `m_Listeners.Find( pEntity ) == -1`, and so does
    /// this: a listener naming the same branch in two `Branch*` slots would
    /// otherwise be told twice about one change.
    pub(super) fn add_listener(&mut self, listener: EntityId) {
        if !self.listeners.contains(&listener) {
            self.listeners.push(listener);
        }
    }

    /// `CLogicBranch::UpdateValue` (`logicentities.cpp:2690`).
    ///
    /// **The listener notification is guarded by the change and the output is
    /// not**, which is the whole asymmetry of the class: `Test` re-fires
    /// `OnTrue`/`OnFalse` every time it is asked (308 shipped connections do),
    /// while a listener hears nothing unless the value really moved.
    fn update(
        &mut self,
        entity: &mut EntityCore,
        new: bool,
        fire: bool,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) {
        if self.value != new {
            self.value = new;

            // `g_EventQueue.AddEvent( pEntity, "_OnLogicBranchChanged", 0,
            // this, this )` — the branch is both activator and caller, so the
            // listener's own outputs fire with the branch that moved as their
            // activator. Zero delay, so the whole chain lands inside this tick
            // (`rustdocs/SERVER.md` gotcha 4).
            let me = entity.id();
            for &listener in &self.listeners {
                cx.post_entity(
                    listener,
                    INPUT_BRANCH_CHANGED,
                    Variant::Void,
                    0.0,
                    Some(me),
                    Some(me),
                );
            }
        }
        if !fire {
            return;
        }
        let me = Some(entity.id());
        let output = match self.value {
            true => "OnTrue",
            false => "OnFalse",
        };
        entity.fire_output(output, Variant::Void, input.activator, me, 0.0, cx);
    }
}

impl Behaviour for Branch {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("InitialValue") {
            self.value = atoi(value) != 0;
            return true;
        }
        false
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("SetValue") {
            let value = input.value.bool();
            self.update(entity, value, false, input, cx);
        } else if is("SetValueTest") {
            let value = input.value.bool();
            self.update(entity, value, true, input, cx);
        } else if is("Toggle") {
            let value = !self.value;
            self.update(entity, value, false, input, cx);
        } else if is("ToggleTest") {
            let value = !self.value;
            self.update(entity, value, true, input, cx);
        } else if is("Test") {
            let value = self.value;
            self.update(entity, value, true, input, cx);
        } else {
            return false;
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("value", self.value.to_string()),
            ("listeners", self.listeners.len().to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// logic_branch_listener
// ---------------------------------------------------------------------------

/// `MAX_LOGIC_BRANCH_NAMES` (`logicentities.cpp:3024`).
///
/// Sixteen slots; **no shipped map uses more than ten**, and 137 of the 158
/// listeners use exactly two.
const MAX_LOGIC_BRANCH_NAMES: usize = 16;

/// `"_OnLogicBranchChanged"` — the private input a [`Branch`] posts at each of
/// its listeners when its value moves.
///
/// It is not a mapper-facing input: the leading underscore is Valve's marker
/// for one entity talking to another through the event queue rather than
/// through a connection a `.vmf` could name. Nothing in any shipped map fires
/// it.
pub(super) const INPUT_BRANCH_CHANGED: &str = "_OnLogicBranchChanged";

/// `"_OnLogicBranchRemoved"` — declared, handled, and **unreachable**.
///
/// `CLogicBranch::UpdateOnRemove` (`logicentities.cpp:2622`) walks its
/// listener list and then posts the event at `this` — the branch — instead of
/// at the listener it just looked up:
///
/// ```text
/// CBaseEntity *pEntity = m_Listeners.Element( i ).Get();
/// if ( pEntity )
///     g_EventQueue.AddEvent( this, "_OnLogicBranchRemoved", 0, this, this );
/// ```
///
/// So in the shipped game a dying branch tells *itself*, a listener never
/// drops the dead branch, and the stale handle is counted as **false** for the
/// rest of the level by `DoTest`'s `if ( pBranch && … )`. That is also what
/// this port does, for free and by a different route: there is no
/// `UpdateOnRemove` hook on [`Behaviour`], nothing posts this input, and a
/// branch whose id no longer resolves reads as false in
/// [`BranchList::do_test`]. **No shipped map fires `Kill` at a
/// `logic_branch`**, so the two are indistinguishable anyway.
pub(super) const INPUT_BRANCH_REMOVED: &str = "_OnLogicBranchRemoved";

/// `LogicBranchListenerLastState_t` (`logicentities.cpp:3036`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BranchState {
    /// `LOGIC_BRANCH_LISTENER_NOT_INIT` — nothing has been reported yet, so
    /// the next test fires whatever it finds.
    NotInit,
    AllTrue,
    AllFalse,
    Mixed,
}

impl BranchState {
    fn name(self) -> &'static str {
        match self {
            BranchState::NotInit => "not-init",
            BranchState::AllTrue => "all-true",
            BranchState::AllFalse => "all-false",
            BranchState::Mixed => "mixed",
        }
    }
}

/// `CLogicBranchList` (`logicentities.cpp:3026`) — `logic_branch_listener`,
/// an AND gate over a handful of [`Branch`]es. **158 placed across 46 of the
/// 106 maps.**
///
/// It is what a Portal 2 test chamber closes its door with. `Branch01` is "the
/// map wants this door shut" and `Branch02` is "the player is not standing in
/// the doorway"; when both become true the listener's `OnAllTrue` fires the
/// relay that sends the door `Close`. That is 130 of the game's 138
/// `prop_testchamber_door`s, and it is why `OnAllTrue` carries **272 of the
/// class's 307 output connections** against `OnAllFalse`'s 19 and `OnMixed`'s
/// 16.
///
/// Three things about it read as bugs until you check the reference.
///
/// **It fires nothing at level start.** `Spawn` is empty, `Activate` only
/// registers, and `m_eLastState` is `NOT_INIT` — so a map whose branches are
/// already all true when it loads gets no `OnAllTrue` until one of them
/// *changes*. Every door in the game depends on that: both of its branches
/// would otherwise report shut-and-clear at spawn and close a door that is
/// already closed.
///
/// **An empty list is `OnMixed`, not `OnAllTrue`.** With no branches neither
/// `bOneTrue` nor `bOneFalse` is set, so `DoTest`'s first two arms are both
/// skipped and the `else` fires. Unreachable in shipped content — all 350
/// `Branch*` keys in the game resolve — and kept because it is free and
/// because the alternative reading is the one a reimplementation reaches for.
///
/// **`Test` forces an output and the private input does not.** `InputTest`
/// resets `m_eLastState` to `NOT_INIT` first, so it always reports; a branch
/// change reports only when the *verdict* changes, which is what stops a
/// two-branch listener firing `OnMixed` twice on its way from all-false to
/// all-true. No shipped map fires `Test` at a listener.
pub struct BranchList {
    /// `m_nLogicBranchNames[0..16]` — `Branch01`…`Branch16`.
    names: [Option<String>; MAX_LOGIC_BRANCH_NAMES],
    /// `m_LogicBranchList` — resolved once, in `Activate`.
    branches: Vec<EntityId>,
    /// `m_eLastState`.
    last: BranchState,
}

impl BranchList {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(BranchList {
            names: [const { None }; MAX_LOGIC_BRANCH_NAMES],
            branches: Vec::new(),
            last: BranchState::NotInit,
        })
    }

    /// `CLogicBranchList::DoTest` (`logicentities.cpp:3182`).
    ///
    /// A branch whose id has stopped resolving counts as **false**, which is
    /// Valve's `if ( pBranch && pBranch->GetLogicBranchState() )` — the null
    /// check and the value share one arm. See [`INPUT_BRANCH_REMOVED`] for why
    /// that is the only cleanup a dead branch ever gets.
    fn do_test(
        &mut self,
        entity: &mut EntityCore,
        activator: Option<EntityId>,
        cx: &mut Context<'_>,
    ) {
        let mut one_true = false;
        let mut one_false = false;
        for &id in &self.branches {
            let value = cx
                .entity(id)
                .and_then(|other| other.behaviour.downcast_ref::<Branch>())
                .is_some_and(|branch| branch.value);
            match value {
                true => one_true = true,
                false => one_false = true,
            }
        }

        let state = match (one_true, one_false) {
            (true, false) => BranchState::AllTrue,
            (false, true) => BranchState::AllFalse,
            _ => BranchState::Mixed,
        };
        if state == self.last {
            return;
        }
        self.last = state;

        let output = match state {
            BranchState::AllTrue => "OnAllTrue",
            BranchState::AllFalse => "OnAllFalse",
            _ => "OnMixed",
        };
        let me = Some(entity.id());
        entity.fire_output(output, Variant::Void, activator, me, 0.0, cx);
    }
}

impl Behaviour for BranchList {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        for (i, slot) in self.names.iter_mut().enumerate() {
            if key.eq_ignore_ascii_case(&format!("Branch{:02}", i + 1)) {
                *slot = Some(value.to_owned());
                return true;
            }
        }
        false
    }

    /// `CLogicBranchList::Activate` (`logicentities.cpp:3117`) — find every
    /// branch named and register with it in both directions.
    ///
    /// The registration is the one place this class reaches into another's own
    /// fields, and it is exactly what
    /// [`Context::behaviour_mut`](crate::server::class::Context::behaviour_mut)
    /// is for. It cannot be an input, because the branch has to hold the
    /// listener's id *before* anything fires — and it is safe here for the
    /// same reason `Activate` is where a class may look at another entity at
    /// all: every entity in the map has spawned.
    ///
    /// Valve's `DevWarning` for a name that is not a `logic_branch` is kept as
    /// the same test and no message; nothing in the game trips it.
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let me = entity.id();
        let names: Vec<String> = self.names.iter().flatten().cloned().collect();
        for name in &names {
            for id in cx.find_all_by_name(name) {
                let is_branch = cx
                    .entity(id)
                    .is_some_and(|other| other.classname() == "logic_branch");
                if !is_branch {
                    continue;
                }
                if let Some(branch) = cx.behaviour_mut::<Branch>(id) {
                    branch.add_listener(me);
                }
                self.branches.push(id);
            }
        }
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Test") {
            // `InputTest` (`:3171`): "Force an output."
            self.last = BranchState::NotInit;
            self.do_test(entity, input.activator, cx);
        } else if is(INPUT_BRANCH_CHANGED) {
            self.do_test(entity, input.activator, cx);
        } else if is(INPUT_BRANCH_REMOVED) {
            // `Input_OnLogicBranchRemoved` (`:3145`). Unreachable — see
            // [`INPUT_BRANCH_REMOVED`] — and written out anyway because the
            // datadesc declares it and the `FastRemove` is the behaviour a
            // reader would otherwise have to go and look up.
            if let Some(dead) = input.activator {
                self.branches.retain(|&id| id != dead);
            }
            self.do_test(entity, input.activator, cx);
        } else {
            return false;
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("branches", self.branches.len().to_string()),
            ("state", self.last.name().to_owned()),
        ]
    }
}

impl BranchList {
    /// The branches `Activate` resolved, in `Branch01`…`Branch16` order.
    ///
    /// Read by the tests and by nothing else — the same convention as
    /// [`TestChamberDoor::is_open`](super::TestChamberDoor::is_open) — because
    /// what the class does with them reaches `ent_dump` through
    /// [`describe`](Behaviour::describe) and reaches the rest of the map
    /// through its three outputs.
    #[allow(dead_code)]
    pub fn branches(&self) -> &[EntityId] {
        &self.branches
    }

    /// The last verdict reported, as `DrawDebugTextOverlays` would print it.
    #[allow(dead_code)]
    pub fn state(&self) -> &'static str {
        self.last.name()
    }
}

// ---------------------------------------------------------------------------
// logic_case
// ---------------------------------------------------------------------------

/// `MAX_LOGIC_CASES` (`logicentities.cpp:2173`).
const MAX_LOGIC_CASES: usize = 16;

/// `CLogicCase` (`logicentities.cpp:2175`) — a switch statement, and the
/// game's random picker. 84 placed.
///
/// Two thirds of its use in Portal 2 is not the switch at all: 56 of its 110
/// incoming connections are `PickRandom` or `PickRandomShuffle`, which ignore
/// the case *values* entirely and choose among the case outputs that have
/// something connected to them.
pub struct Case {
    /// `m_nCase[0..16]` — `Case01`…`Case16`. `None` for a key the map did not
    /// set; **no shipped Portal 2 map sets one to the empty string**, so the
    /// `NULL_STRING` test and an is-empty test cannot disagree here.
    cases: [Option<String>; MAX_LOGIC_CASES],
    /// `m_nShuffleCases` — how many of the shuffle batch are left.
    shuffle_remaining: usize,
    /// `m_nLastShuffleCase` — the case the previous batch ended on, so that a
    /// batch boundary cannot repeat. `-1` is "no previous".
    last_shuffle_case: i32,
    /// `m_uchShuffleCaseMap` — the batch, packed.
    shuffle_map: [u8; MAX_LOGIC_CASES],
}

impl Case {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(Case {
            cases: Default::default(),
            shuffle_remaining: 0,
            last_shuffle_case: -1,
            shuffle_map: [0; MAX_LOGIC_CASES],
        })
    }

    /// The name of case `i`'s output — `OnCase01`, one-based and zero-padded.
    fn output_name(index: usize) -> &'static str {
        ON_CASE[index]
    }

    /// `CLogicCase::BuildCaseMap` (`logicentities.cpp:2292`).
    ///
    /// > **A case is eligible if its *output* has connections, not if its
    /// > *value* is set.** `PickRandom` on a `logic_case` with sixteen `CaseNN`
    /// > keys and one `OnCase03` connection always picks case 3. That is what
    /// > lets a mapper use the entity as a random picker with no case values
    /// > at all, which is how Portal 2 mostly uses it.
    fn build_case_map(entity: &EntityCore, map: &mut [u8; MAX_LOGIC_CASES]) -> usize {
        let mut count = 0;
        for i in 0..MAX_LOGIC_CASES {
            if entity.output_count(Case::output_name(i)) > 0 {
                map[count] = i as u8;
                count += 1;
            }
        }
        count
    }
}

/// `OnCase01`…`OnCase16`, in order. The seventeenth output, `OnDefault`, is
/// not a case and is not in here.
static ON_CASE: [&str; MAX_LOGIC_CASES] = [
    "OnCase01", "OnCase02", "OnCase03", "OnCase04", "OnCase05", "OnCase06", "OnCase07", "OnCase08",
    "OnCase09", "OnCase10", "OnCase11", "OnCase12", "OnCase13", "OnCase14", "OnCase15", "OnCase16",
];

/// `Case01`…`Case16`, in order — the class's key declaration and its lookup
/// table at once.
pub(super) static CASE_KEYS: &[&str] = &[
    "Case01", "Case02", "Case03", "Case04", "Case05", "Case06", "Case07", "Case08", "Case09",
    "Case10", "Case11", "Case12", "Case13", "Case14", "Case15", "Case16",
];

impl Behaviour for Case {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        for (i, name) in CASE_KEYS.iter().enumerate() {
            if key.eq_ignore_ascii_case(name) {
                self.cases[i] = Some(value.to_owned());
                return true;
            }
        }
        false
    }

    /// `CLogicCase::Spawn` — one line, and it is the one that stops the first
    /// shuffle batch from excluding case 0.
    fn spawn(&mut self, _entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        self.last_shuffle_case = -1;
        SpawnResult::Ok
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("InValue") {
            self.in_value(entity, input, cx);
        } else if is("PickRandom") {
            self.pick_random(entity, input, cx);
        } else if is("PickRandomShuffle") {
            self.pick_random_shuffle(entity, input, cx);
        } else {
            return false;
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        for (i, case) in self.cases.iter().enumerate() {
            if let Some(case) = case {
                out.push((CASE_KEYS[i], case.clone()));
            }
        }
        out
    }
}

impl Case {
    /// `CLogicCase::InputValue` (`logicentities.cpp:2275`).
    ///
    /// The comparison is against the value's **string form**, whatever type it
    /// arrived as — the input is declared `FIELD_INPUT` precisely so that the
    /// variant is not converted first and `Value.String()` can render it. So
    /// a float 1.5 matches a case of `"1.5"` and an int 1 matches `"1"`, and
    /// the formatting rule in [`Variant::to_string`](crate::server::io::Variant::to_string)
    /// is behaviour rather than presentation.
    fn in_value(&mut self, entity: &mut EntityCore, input: &Input<'_>, cx: &mut Context<'_>) {
        let value = input.value.to_string();
        let me = Some(entity.id());
        for i in 0..MAX_LOGIC_CASES {
            let matched = match &self.cases[i] {
                Some(case) => case.eq_ignore_ascii_case(&value),
                None => false,
            };
            if matched {
                let name = Case::output_name(i);
                entity.fire_output(name, Variant::Void, input.activator, me, 0.0, cx);
                return;
            }
        }
        // `m_OnDefault` is a `COutputVariant`: it fires carrying the value
        // that matched nothing, so a chain can pass the unmatched case on.
        entity.fire_output(
            "OnDefault",
            input.value.clone(),
            input.activator,
            me,
            0.0,
            cx,
        );
    }

    /// `CLogicCase::InputPickRandom` (`logicentities.cpp:2311`).
    fn pick_random(&mut self, entity: &mut EntityCore, input: &Input<'_>, cx: &mut Context<'_>) {
        let mut map = [0u8; MAX_LOGIC_CASES];
        let count = Case::build_case_map(entity, &mut map);
        if count == 0 {
            return;
        }
        let chosen = map[cx.random().int(0, count as i32 - 1) as usize] as usize;
        let me = Some(entity.id());
        let name = Case::output_name(chosen);
        entity.fire_output(name, Variant::Void, input.activator, me, 0.0, cx);
    }

    /// `CLogicCase::InputPickRandomShuffle` (`logicentities.cpp:2338`).
    ///
    /// Deals the eligible cases out one at a time without repeating, then
    /// reshuffles — and, at a batch boundary, holds back the case the previous
    /// batch ended on so that a repeat cannot straddle two batches. The
    /// swap-to-the-end trick is Valve's and is reproduced exactly, because the
    /// *sequence* it produces from a given seed is the behaviour.
    fn pick_random_shuffle(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) {
        let mut case_count = self.shuffle_remaining;

        if case_count == 0 {
            // Starting a new batch.
            let mut map = [0u8; MAX_LOGIC_CASES];
            let count = Case::build_case_map(entity, &mut map);
            self.shuffle_map = map;
            self.shuffle_remaining = count;
            case_count = count;

            if self.shuffle_remaining > 1 && self.last_shuffle_case != -1 {
                let avoid = self.last_shuffle_case as u8;
                for i in 0..self.shuffle_remaining {
                    if self.shuffle_map[i] == avoid {
                        self.shuffle_map.swap(i, case_count - 1);
                        case_count -= 1;
                        break;
                    }
                }
            }
        }

        if case_count == 0 {
            return;
        }
        let picked = cx.random().int(0, case_count as i32 - 1) as usize;
        let chosen = self.shuffle_map[picked] as usize;

        let me = Some(entity.id());
        let name = Case::output_name(chosen);
        entity.fire_output(name, Variant::Void, input.activator, me, 0.0, cx);

        self.shuffle_map[picked] = self.shuffle_map[self.shuffle_remaining - 1];
        self.shuffle_remaining -= 1;
        self.last_shuffle_case = chosen as i32;
    }
}

// ---------------------------------------------------------------------------
// logic_timer
// ---------------------------------------------------------------------------

/// `SF_TIMER_UPDOWN` (`logicentities.cpp:552`) — alternate two outputs instead
/// of firing one. 9 of the game's 151 timers.
const SF_TIMER_UPDOWN: u32 = 1;
/// `LOGIC_TIMER_MIN_INTERVAL` (`logicentities.cpp:553`).
const LOGIC_TIMER_MIN_INTERVAL: f32 = 0.01;

/// `CTimerEntity` (`logicentities.cpp:556`) — the only stage-2 class that
/// thinks repeatedly. 151 placed.
///
/// It is also the class that makes the fixed tick necessary rather than
/// merely tidy: its floor is 0.01 s, which is *less than a tick* at every
/// rate the engine allows, so a timer at its minimum re-arms for a tick that
/// has already passed and fires every tick. That is Valve's behaviour and it
/// is why `portdocs/SERVER.md` §5 named `logic_timer` as the thing a
/// variable-`dt` schedule would get wrong.
pub struct Timer {
    /// `m_iDisabled` — `StartDisabled`. 129 of the 151 carry the key.
    pub disabled: bool,
    /// `m_flRefireTime` — `RefireTime`.
    pub refire_time: f32,
    /// `m_bUpDownState` — which of the two outputs is next, under
    /// [`SF_TIMER_UPDOWN`].
    up_down_state: bool,
    /// `m_iUseRandomTime` — a `DEFINE_INPUT`, so it is both a map key and a
    /// run-time input. 134 of the 151 carry it.
    pub use_random_time: bool,
    pub lower_random_bound: f32,
    pub upper_random_bound: f32,
}

impl Timer {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(Timer {
            disabled: false,
            refire_time: 0.0,
            up_down_state: false,
            use_random_time: false,
            lower_random_bound: 0.0,
            upper_random_bound: 0.0,
        })
    }

    /// `CTimerEntity::ResetTimer` (`logicentities.cpp:666`).
    fn reset(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if self.disabled {
            return;
        }
        if self.use_random_time {
            self.refire_time = cx
                .random()
                .float(self.lower_random_bound, self.upper_random_bound);
        }
        let next = cx.curtime() + self.refire_time;
        entity.set_next_think(next, cx);
    }

    /// `CTimerEntity::Enable`.
    fn enable(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.disabled = false;
        self.reset(entity, cx);
    }

    /// `CTimerEntity::Disable`.
    fn disable(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.disabled = true;
        entity.set_next_think(NEVER_THINK, cx);
    }

    /// `CTimerEntity::FireTimer` (`logicentities.cpp:718`).
    fn fire(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if self.disabled {
            return;
        }
        // `FireOutput( this, this )` — a timer is its own activator.
        let me = Some(entity.id());
        match entity.has_spawn_flags(SF_TIMER_UPDOWN) {
            true => {
                let output = match self.up_down_state {
                    true => "OnTimerHigh",
                    false => "OnTimerLow",
                };
                entity.fire_output(output, Variant::Void, me, me, 0.0, cx);
                self.up_down_state = !self.up_down_state;
            }
            false => entity.fire_output("OnTimer", Variant::Void, me, me, 0.0, cx),
        }
        self.reset(entity, cx);
    }
}

impl Behaviour for Timer {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("StartDisabled") {
            self.disabled = atoi(value) != 0;
        } else if is("RefireTime") {
            self.refire_time = atof(value);
        } else if is("UseRandomTime") {
            self.use_random_time = atoi(value) != 0;
        } else if is("LowerRandomBound") {
            self.lower_random_bound = atof(value);
        } else if is("UpperRandomBound") {
            self.upper_random_bound = atof(value);
        } else {
            return false;
        }
        true
    }

    /// `CTimerEntity::Spawn` (`logicentities.cpp:637`).
    ///
    /// The floor is applied **only when the timer is not random**, so a random
    /// timer with bounds below 0.01 is allowed through — Valve's asymmetry,
    /// and `ResetTimer` does not re-check.
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult {
        if !self.use_random_time && self.refire_time < LOGIC_TIMER_MIN_INTERVAL {
            self.refire_time = LOGIC_TIMER_MIN_INTERVAL;
        }
        match !self.disabled && (self.refire_time > 0.0 || self.use_random_time) {
            true => self.enable(entity, cx),
            false => self.disable(entity, cx),
        }
        SpawnResult::Ok
    }

    /// `CTimerEntity::Think` — one line, `FireTimer()`.
    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.fire(entity, cx);
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Enable") {
            self.enable(entity, cx);
        } else if is("Disable") {
            self.disable(entity, cx);
        } else if is("Toggle") {
            match self.disabled {
                true => self.enable(entity, cx),
                false => self.disable(entity, cx),
            }
        } else if is("FireTimer") {
            self.fire(entity, cx);
        } else if is("ResetTimer") {
            // "don't reset the timer if it isn't enabled"
            if !self.disabled {
                self.reset(entity, cx);
            }
        } else if is("RefireTime") {
            let mut interval = input.value.float();
            if interval < LOGIC_TIMER_MIN_INTERVAL {
                interval = LOGIC_TIMER_MIN_INTERVAL;
            }
            // Only resets when the value actually changed, so a timer told its
            // own interval keeps counting down rather than restarting.
            if self.refire_time != interval {
                self.refire_time = interval;
                self.reset(entity, cx);
            }
        } else if is("AddToTimer") {
            if !self.disabled {
                let next = entity.next_think(cx) + input.value.float();
                entity.set_next_think(next, cx);
            }
        } else if is("SubtractFromTimer") {
            if !self.disabled {
                let next = entity.next_think(cx);
                // "don't let the timer go negative"
                let next = match next - cx.curtime() <= input.value.float() {
                    true => cx.curtime(),
                    false => next - input.value.float(),
                };
                entity.set_next_think(next, cx);
            }
        } else if is("UseRandomTime") {
            // `DEFINE_INPUT`: no handler in the C++ at all — `AcceptInput`
            // writes the field by offset. Three of them on this class.
            self.use_random_time = input.value.int() != 0;
        } else if is("LowerRandomBound") {
            self.lower_random_bound = input.value.float();
        } else if is("UpperRandomBound") {
            self.upper_random_bound = input.value.float();
        } else {
            return false;
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![
            ("StartDisabled", self.disabled.to_string()),
            ("RefireTime", format!("{:.2}", self.refire_time)),
        ];
        if self.use_random_time {
            out.push((
                "random bounds",
                format!("{} .. {}", self.lower_random_bound, self.upper_random_bound),
            ));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// math_counter
// ---------------------------------------------------------------------------

/// `CMathCounter` (`logicentities.cpp:1716`) — an accumulator with a clamp and
/// four edge outputs. 102 placed.
pub struct MathCounter {
    /// `m_OutValue.Get()` — the counter. Held here rather than as the output's
    /// stored value, because [`Output`](crate::server::io::Output) does not
    /// keep one: `CBaseEntityOutput::m_Value` exists so that save/restore can
    /// round-trip it, and save/restore is deleted.
    pub value: f32,
    /// `m_flMin`/`m_flMax` — `min` and `max`. **If both are zero there is no
    /// clamping at all**, which is Valve's sentinel and is why 43 of the 102
    /// set `min` explicitly to something.
    pub min: f32,
    pub max: f32,
    /// `m_bHitMin`/`m_bHitMax` — latches, so that `OnHitMax` fires on the
    /// edge rather than on every input once the ceiling is reached.
    hit_min: bool,
    hit_max: bool,
    /// `m_bDisabled` — `StartDisabled`.
    pub disabled: bool,
}

impl MathCounter {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(MathCounter {
            value: 0.0,
            min: 0.0,
            max: 0.0,
            hit_min: false,
            hit_max: false,
            disabled: false,
        })
    }

    /// Whether clamping is on at all — `(m_flMin != 0) || (m_flMax != 0)`.
    fn clamps(&self) -> bool {
        self.min != 0.0 || self.max != 0.0
    }

    /// `CMathCounter::UpdateOutValue` (`logicentities.cpp:2109`).
    ///
    /// The order is load-bearing: the edge outputs are decided against the
    /// **old** value and the **unclamped** new one, and only then is the value
    /// clamped and `OutValue` fired.
    fn update(
        &mut self,
        entity: &mut EntityCore,
        new: f32,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) {
        let me = Some(entity.id());
        let mut new = new;

        if self.clamps() {
            if new >= self.max {
                if !self.hit_max {
                    self.hit_max = true;
                    entity.fire_output("OnHitMax", Variant::Void, input.activator, me, 0.0, cx);
                }
            } else {
                if self.value == self.max {
                    entity.fire_output(
                        "OnChangedFromMax",
                        Variant::Void,
                        input.activator,
                        me,
                        0.0,
                        cx,
                    );
                }
                self.hit_max = false;
            }

            if new <= self.min {
                if !self.hit_min {
                    self.hit_min = true;
                    entity.fire_output("OnHitMin", Variant::Void, input.activator, me, 0.0, cx);
                }
            } else {
                if self.value == self.min {
                    entity.fire_output(
                        "OnChangedFromMin",
                        Variant::Void,
                        input.activator,
                        me,
                        0.0,
                        cx,
                    );
                }
                self.hit_min = false;
            }

            new = new.clamp(self.min, self.max);
        }

        self.value = new;
        // `m_OutValue.Set( fNewValue, pActivator, this )` — a `COutputFloat`,
        // so the connection carries the new value as its parameter unless the
        // mapper overrode it.
        entity.fire_output(
            "OutValue",
            Variant::Float(new),
            input.activator,
            me,
            0.0,
            cx,
        );
    }

    /// The clamp `SetValueNoFire` and the two `Set…ValueNoFire` inputs apply.
    fn clamped(&self, value: f32) -> f32 {
        match self.clamps() {
            true => value.clamp(self.min, self.max),
            false => value,
        }
    }
}

impl Behaviour for MathCounter {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("min") {
            self.min = atof(value);
        } else if is("max") {
            self.max = atof(value);
        } else if is("StartDisabled") {
            self.disabled = atoi(value) != 0;
        } else if is("startvalue") {
            // **`atoi`, not `atof`** (`logicentities.cpp:1812`):
            // `m_OutValue.Init(atoi(szValue))`. A `startvalue` of `2.5` starts
            // at 2. Valve's, and the only key on this class that is not read
            // the way its type suggests.
            self.value = atoi(value) as f32;
        } else {
            return false;
        }
        true
    }

    /// `CMathCounter::Spawn` (`logicentities.cpp:1826`) — order the bounds,
    /// then clamp the starting value into them.
    fn spawn(&mut self, _entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        if self.min > self.max {
            std::mem::swap(&mut self.min, &mut self.max);
        }
        if self.clamps() {
            self.value = self.value.clamp(self.min, self.max);
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

        // `SetHitMax`/`SetHitMin` and `GetValue` are the three that work while
        // disabled; every arithmetic input refuses. Valve prints a `DevMsg`
        // for each refusal, which this port drops — a counter hammered by a
        // timer would print it every tick.
        if is("SetHitMax") {
            self.max = input.value.float();
            if self.max < self.min {
                self.min = self.max;
            }
            let value = self.value;
            self.update(entity, value, input, cx);
            return true;
        }
        if is("SetHitMin") {
            self.min = input.value.float();
            if self.max < self.min {
                self.max = self.min;
            }
            let value = self.value;
            self.update(entity, value, input, cx);
            return true;
        }
        if is("GetValue") {
            let value = self.value;
            // `m_OnGetValue.Set( flOutValue, pActivator, pCaller )` — note the
            // caller is the *incoming* caller here and not `this`, which is
            // the one place on this class Valve does not pass itself.
            entity.fire_output(
                "OnGetValue",
                Variant::Float(value),
                input.activator,
                input.caller,
                0.0,
                cx,
            );
            return true;
        }
        if is("Enable") {
            self.disabled = false;
            return true;
        }
        if is("Disable") {
            self.disabled = true;
            return true;
        }

        let arithmetic = is("Add")
            || is("Subtract")
            || is("Multiply")
            || is("Divide")
            || is("SetValue")
            || is("SetValueNoFire")
            || is("SetMaxValueNoFire")
            || is("SetMinValueNoFire");
        if !arithmetic {
            return false;
        }
        if self.disabled {
            return true;
        }

        let operand = input.value.float();
        if is("Add") {
            let value = self.value + operand;
            self.update(entity, value, input, cx);
        } else if is("Subtract") {
            let value = self.value - operand;
            self.update(entity, value, input, cx);
        } else if is("Multiply") {
            let value = self.value * operand;
            self.update(entity, value, input, cx);
        } else if is("Divide") {
            // A divide by zero keeps the value and still fires `OutValue`,
            // which is Valve's "LEVEL DESIGN ERROR" branch.
            let value = match operand != 0.0 {
                true => self.value / operand,
                false => self.value,
            };
            self.update(entity, value, input, cx);
        } else if is("SetValue") {
            self.update(entity, operand, input, cx);
        } else if is("SetValueNoFire") {
            self.value = self.clamped(operand);
        } else if is("SetMaxValueNoFire") {
            // Refused outright if it would invert the range — it does not
            // clamp, it ignores.
            if operand >= self.min {
                self.max = operand;
                self.value = self.clamped(self.value);
            }
        } else if is("SetMinValueNoFire") {
            if operand <= self.max {
                self.min = operand;
                self.value = self.clamped(self.value);
            }
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("value", self.value.to_string()),
            ("min", self.min.to_string()),
            ("max", self.max.to_string()),
            ("StartDisabled", self.disabled.to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// func_instance_io_proxy
// ---------------------------------------------------------------------------

/// `OnProxyRelay1` … `OnProxyRelay30` — the proxy's output names, **and its
/// input names**, which are the same thirty strings.
///
/// Valve's datadesc declares **31** `DEFINE_OUTPUT` lines for 30 names —
/// `OnProxyRelay16` appears twice (`func_instance_io_proxy.cpp:140` and
/// `:141`). Harmless there, because `AcceptInput`'s walk takes the first
/// match; thirty distinct names here.
///
/// The FGD declares the *unnumbered* `OnProxyRelay`, which is what a mapper
/// sees; Hammer's instance compiler emits the numbered ones. 135 entities in
/// the shipped maps still carry the unnumbered key and 71 connections fire an
/// unnumbered `ProxyRelay` input, neither of which any version of the server
/// has ever handled — both show up as unhandled, correctly.
pub(super) static PROXY_RELAYS: &[&str] = &[
    "OnProxyRelay1",
    "OnProxyRelay2",
    "OnProxyRelay3",
    "OnProxyRelay4",
    "OnProxyRelay5",
    "OnProxyRelay6",
    "OnProxyRelay7",
    "OnProxyRelay8",
    "OnProxyRelay9",
    "OnProxyRelay10",
    "OnProxyRelay11",
    "OnProxyRelay12",
    "OnProxyRelay13",
    "OnProxyRelay14",
    "OnProxyRelay15",
    "OnProxyRelay16",
    "OnProxyRelay17",
    "OnProxyRelay18",
    "OnProxyRelay19",
    "OnProxyRelay20",
    "OnProxyRelay21",
    "OnProxyRelay22",
    "OnProxyRelay23",
    "OnProxyRelay24",
    "OnProxyRelay25",
    "OnProxyRelay26",
    "OnProxyRelay27",
    "OnProxyRelay28",
    "OnProxyRelay29",
    "OnProxyRelay30",
];

/// `CFuncInstanceIoProxy` (`game/server/func_instance_io_proxy.cpp`) — thirty
/// pass-through relays and nothing else.
///
/// 1,184 of them in the shipped maps, the thirteenth commonest entity in the
/// game, and it has **no state at all**: input *n* fires output *n*, and the
/// input and the output have the same name. The whole class is its table,
/// which is why it is 312 lines of C++ and one unit struct here.
///
/// > **It forwards the caller as well as the activator.** Every other class
/// > here fires its outputs with itself as the caller;
/// > `m_OnProxyRelay1.FireOutput( inputdata.pActivator, inputdata.pCaller )`
/// > passes the incoming caller straight through, so a chain that crosses a
/// > proxy looks to `!self` and to `CancelEvents` as though the proxy were not
/// > there. That is what makes it a *proxy* rather than a relay.
pub struct InstanceIoProxy;

impl InstanceIoProxy {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(InstanceIoProxy)
    }
}

impl Behaviour for InstanceIoProxy {
    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let Some(name) = PROXY_RELAYS
            .iter()
            .find(|relay| relay.eq_ignore_ascii_case(input.name))
        else {
            return false;
        };
        entity.fire_output(name, Variant::Void, input.activator, input.caller, 0.0, cx);
        true
    }
}

/// The proxy's inputs: the same thirty names, all `FIELD_STRING`.
///
/// `FIELD_STRING` and not `FIELD_VOID`, which is Valve's and is observable:
/// an input carrying no value at all (`FIELD_VOID`) is let through without
/// conversion by `AcceptInput`'s "allow empty strings" exemption, and anything
/// else is coerced to a string it then ignores.
pub(super) static PROXY_INPUTS: InputDefs = &[
    InputDef::new("OnProxyRelay1", FieldType::String),
    InputDef::new("OnProxyRelay2", FieldType::String),
    InputDef::new("OnProxyRelay3", FieldType::String),
    InputDef::new("OnProxyRelay4", FieldType::String),
    InputDef::new("OnProxyRelay5", FieldType::String),
    InputDef::new("OnProxyRelay6", FieldType::String),
    InputDef::new("OnProxyRelay7", FieldType::String),
    InputDef::new("OnProxyRelay8", FieldType::String),
    InputDef::new("OnProxyRelay9", FieldType::String),
    InputDef::new("OnProxyRelay10", FieldType::String),
    InputDef::new("OnProxyRelay11", FieldType::String),
    InputDef::new("OnProxyRelay12", FieldType::String),
    InputDef::new("OnProxyRelay13", FieldType::String),
    InputDef::new("OnProxyRelay14", FieldType::String),
    InputDef::new("OnProxyRelay15", FieldType::String),
    InputDef::new("OnProxyRelay16", FieldType::String),
    InputDef::new("OnProxyRelay17", FieldType::String),
    InputDef::new("OnProxyRelay18", FieldType::String),
    InputDef::new("OnProxyRelay19", FieldType::String),
    InputDef::new("OnProxyRelay20", FieldType::String),
    InputDef::new("OnProxyRelay21", FieldType::String),
    InputDef::new("OnProxyRelay22", FieldType::String),
    InputDef::new("OnProxyRelay23", FieldType::String),
    InputDef::new("OnProxyRelay24", FieldType::String),
    InputDef::new("OnProxyRelay25", FieldType::String),
    InputDef::new("OnProxyRelay26", FieldType::String),
    InputDef::new("OnProxyRelay27", FieldType::String),
    InputDef::new("OnProxyRelay28", FieldType::String),
    InputDef::new("OnProxyRelay29", FieldType::String),
    InputDef::new("OnProxyRelay30", FieldType::String),
];

/// `OnCase01`…`OnCase16` plus `OnDefault` — `logic_case`'s output declaration.
pub(super) static CASE_OUTPUTS: &[&str] = &[
    "OnCase01",
    "OnCase02",
    "OnCase03",
    "OnCase04",
    "OnCase05",
    "OnCase06",
    "OnCase07",
    "OnCase08",
    "OnCase09",
    "OnCase10",
    "OnCase11",
    "OnCase12",
    "OnCase13",
    "OnCase14",
    "OnCase15",
    "OnCase16",
    "OnDefault",
];
