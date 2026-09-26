//! The pedestal buttons — the ones you walk up to and press with `+use`.
//!
//! `game/server/portal2/prop_button.cpp`'s `CPropButton` and
//! `CPropUnderButton`, which is the same class on another model with four
//! other sequence names.
//!
//! ```text
//!     56  prop_button          CPropButton        38 maps   154 OnPressed   35 OnButtonReset
//!     27  prop_under_button    CPropUnderButton   12 maps    66 OnPressed    6 OnButtonReset
//! ```
//!
//! **The map it was ported for is `sp_a1_intro2`**, the portal carousel: the
//! three blue portals there are opened by three `prop_button`s and by nothing
//! else. The timer-driven carousel the map also contains was switched off
//! before it shipped — every connection into it targets `//count_portal_chambers`
//! or `//case_spawn_portals`, names that resolve to nothing — so until these
//! buttons existed no blue portal on that map could open.
//!
//! # The press is an animation, and so is the output
//!
//! A press does not fire `OnPressed`. It starts the `down` sequence, and
//! `OnPressed` is fired by the 10 Hz `AnimateThink` **when that sequence
//! finishes** — then the button sits in `idle_down` for `Delay` seconds, plays
//! `up`, and fires `OnButtonReset` when *that* finishes. So the timing of both
//! outputs is decided by `m_bSequenceFinished`, and this class accumulates
//! the cycle the way `StudioFrameAdvance` does rather than deriving it, because
//! the moments at which the flag goes true are what the maps observe:
//!
//! - **`switch001`'s `down` is "finished" on the first think after the
//!   press.** It is five frames at 30 fps, 0.133 seconds, and
//!   `GetLastVisibleCycle` subtracts studiomdl's 0.2-second fade from that —
//!   so the threshold is *negative* and any advance at all crosses it.
//!   `OnPressed` therefore lands up to a tenth of a second after the press.
//! - **`underground_testchamber_button`'s `press` is 0.875 seconds**, so the
//!   Wheatley-era button's `OnPressed` arrives about 0.7 seconds in, near the
//!   end of the visible travel.
//! - **The interval is measured from the last think, not from the press**,
//!   because `ResetSequence` does not touch `m_flAnimTime`. The first advance
//!   of `down` credits it with the whole tenth of a second since the previous
//!   think, however recently the press came.
//!
//! None of the four sequences loops, on either model — `0x8000` on the
//! underground one is `STUDIO_NOFORCELOOP`, not `STUDIO_LOOPING`.
//!
//! # The timer, which is a second schedule
//!
//! `IsTimer` (27 buttons write the key, 16 of them `1`) adds a think context:
//! `TimerThink` runs once a second from the moment the button is down, until
//! the goal time passes, and then fires `OnButtonReset` in place of the
//! animation's. This port has no think contexts
//! ([`EntityCore::set_next_think`] says why and when they come back), so the
//! context is a due tick kept on the class, and the entity's one think is
//! scheduled at whichever of the two is sooner. The two schedules keep their
//! own grids — the base think on 0.1 seconds, the timer on whole seconds from
//! the press — which is what separate contexts would have given.
//!
//! # Deliberately not here
//!
//! - **Every sound.** `Portal.button_down`, `button_up`, `button_locked` and
//!   the timer's `room1_TickTock`. There is no sound system; each is a comment
//!   at its site.
//! - **`OnPressedOrange` and `OnPressedBlue`**, the co-op team outputs. Both
//!   need `GameRules()->IsMultiplayer()`, and one shipped map connects each.
//!   Declared so that the connections parse as outputs.
//! - **`DispatchAnimEvents`.** Neither model has an event on any of its
//!   sequences.
//! - **`VisibilityMonitor_AddEntity_NotVisibleThroughGlass`** — the
//!   instructor hint that tells you to press the button. No hint system.
//! - **`SetFadeDistance( -1, 0 )`, `SetGlobalFadeScale( 0 )` and
//!   `FL_UNPAINTABLE`.** No distance fade and no paint.
//!
//! What collides is not this module's to say: `SetSolid( SOLID_VPHYSICS )`
//! and `CreateVPhysics`'s `VPhysicsInitStatic` are what
//! [`Physics::add_studio_entities`](crate::server::physics::Physics::add_studio_entities)
//! does for every solid entity with a studio model, and it is that static
//! body the player's `+use` rays find — see
//! [`Physics::sweep_use`](crate::server::physics::Physics::sweep_use).

use crate::server::class::{
    Behaviour, Context, InputDef, InputDefs, ModelState, SpawnResult, UseType, FCAP_IMPULSE_USE,
};
use crate::server::entity::{EntityCore, EntityId};
use crate::server::io::{FieldType, Input, Variant};
use crate::server::keyvalue::{atof, atoi, effects};
use crate::server::movement::{MoveType, Solid, FL_CLIENT};
use crate::server::sequences::Lookup;

/// `AnimateThink`'s `SetNextThink( gpGlobals->curtime + 0.1f )`.
const ANIMATE_THINK_INTERVAL: f32 = 0.1;

/// `TimerThink`'s `SetContextThink( …, gpGlobals->curtime + 1.0f, … )`.
const TIMER_THINK_INTERVAL: f32 = 1.0;

/// `MAX_ANIMTIME_INTERVAL` (`baseanimating.cpp:442`) — the most time one
/// `StudioFrameAdvance` will credit.
const MAX_ANIMTIME_INTERVAL: f32 = 0.2;

/// Which of the two classes this is: a model and four sequence labels, which
/// is the whole of what `CPropUnderButton` overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PedestalKind {
    /// `prop_button` — the Aperture pedestal, `models/props/switch001.mdl`.
    Standard,
    /// `prop_under_button` — the 1950s one,
    /// `models/props_underground/underground_testchamber_button.mdl`.
    Underground,
}

/// The four sequences `LookUpAnimationSequences` asks for.
struct Sequences {
    up: &'static str,
    down: &'static str,
    idle_up: &'static str,
    idle_down: &'static str,
}

impl PedestalKind {
    /// `GetButtonModelName()`. **The `model` key is ignored** —
    /// `CPropButton::Spawn` calls `SetModel` with this and nothing else.
    fn model(self) -> &'static str {
        match self {
            PedestalKind::Standard => "models/props/switch001.mdl",
            PedestalKind::Underground => {
                "models/props_underground/underground_testchamber_button.mdl"
            }
        }
    }

    /// `LookUpAnimationSequences()` (`prop_button.cpp:146`, `:517`).
    fn sequences(self) -> Sequences {
        match self {
            PedestalKind::Standard => Sequences {
                up: "up",
                down: "down",
                idle_up: "idle",
                idle_down: "idle_down",
            },
            PedestalKind::Underground => Sequences {
                up: "release",
                down: "press",
                idle_up: "release_idle",
                idle_down: "press_idle",
            },
        }
    }
}

/// `CPropButton` (`prop_button.cpp:28`) and `CPropUnderButton` (`:490`).
pub struct PedestalButton {
    kind: PedestalKind,
    /// `m_bLocked`. A locked button plays a sound when used and does nothing
    /// else — it does not even record the activator.
    locked: bool,
    /// `m_flDelayBeforeReset` (`Delay`) — how long the button stays down.
    /// Two shipped buttons write `-1`, which resets on the next think.
    delay: f32,
    /// `m_bIsTimer` (`IsTimer`).
    is_timer: bool,
    /// `m_bPreventFastReset` (`PreventFastReset`) — a timer that stays down
    /// for the whole of its delay instead of springing back at once. One
    /// shipped button sets it.
    prevent_fast_reset: bool,
    /// `m_bTimerCancelled` — set by `CancelPress`, and it swallows the next
    /// `OnButtonReset`.
    timer_cancelled: bool,
    /// `m_flGoalTime` — when a pressed button comes back up.
    goal_time: f32,
    /// `m_hActivator` — whoever last pressed it, for `OnPressed`.
    activator: Option<EntityId>,
    /// `m_nSkin`, the `skin` key. Eleven `prop_button`s write `1`, which is
    /// the dirty variant `sp_a1_intro2`'s three are drawn in.
    skin: i32,
    /// `m_nSequence`, as its label.
    sequence: &'static str,
    /// `m_flCycle` — accumulated, not derived; see the module docs.
    cycle: f32,
    /// `m_flAnimTime` — when the cycle was last advanced. **Not** reset by
    /// `ResetSequence`.
    anim_time: f32,
    /// `m_bSequenceFinished`.
    sequence_finished: bool,
    /// The base think's due tick — `AnimateThink`'s schedule.
    animate_due: Option<i32>,
    /// `TimerThinkContext`'s due tick. `None` is `TICK_NEVER_THINK`.
    timer_due: Option<i32>,
}

/// The inputs (`prop_button.cpp:111`), plus `CBaseAnimating`'s `skin`.
pub static PEDESTAL_BUTTON_INPUTS: InputDefs = &[
    InputDef::new("Press", FieldType::Void),
    InputDef::new("Lock", FieldType::Void),
    InputDef::new("Unlock", FieldType::Void),
    InputDef::new("CancelPress", FieldType::Void),
    InputDef::new("skin", FieldType::Int),
];

/// The outputs (`:116`). The two team outputs are declared and never fired;
/// see the module docs.
pub static PEDESTAL_BUTTON_OUTPUTS: &[&str] =
    &["OnPressed", "OnPressedOrange", "OnPressedBlue", "OnButtonReset"];

/// The keys (`:100`), plus `skin`. `solid`, `rendermode` and the rest that
/// three invisible shipped buttons write are `CBaseEntity`'s.
pub static PEDESTAL_BUTTON_KEYS: &[&str] = &["Delay", "IsTimer", "PreventFastReset", "skin"];

impl PedestalButton {
    fn new(kind: PedestalKind) -> Box<dyn Behaviour> {
        Box::new(PedestalButton {
            kind,
            // The constructor's two initialisers (`:131`).
            locked: false,
            timer_cancelled: false,
            delay: 0.0,
            is_timer: false,
            prevent_fast_reset: false,
            goal_time: 0.0,
            activator: None,
            skin: 0,
            sequence: kind.sequences().idle_up,
            cycle: 0.0,
            anim_time: 0.0,
            sequence_finished: false,
            animate_due: None,
            timer_due: None,
        })
    }

    pub fn create() -> Box<dyn Behaviour> {
        PedestalButton::new(PedestalKind::Standard)
    }

    pub fn create_under() -> Box<dyn Behaviour> {
        PedestalButton::new(PedestalKind::Underground)
    }

    /// `CBaseAnimating::ResetSequence` (`baseanimating.cpp:1180`) for a
    /// sequence that does not loop, which is all eight of these: cycle 0 and
    /// `ResetSequenceInfo`'s `m_bSequenceFinished = false`. `m_flAnimTime` is
    /// left alone — the line that would set it is commented out in
    /// `ResetSequenceInfo`.
    fn reset_sequence(&mut self, sequence: &'static str) {
        self.sequence = sequence;
        self.cycle = 0.0;
        self.sequence_finished = false;
    }

    /// `CBaseAnimating::StudioFrameAdvance` (`baseanimating.cpp:495`) into
    /// `StudioFrameAdvanceInternal` (`:468`), at playback rate 1.
    ///
    /// Returns without advancing when the model is not in the table, which
    /// is Valve's `!pStudioHdr` return — and means a button whose model was
    /// never loaded never finishes a sequence and never fires `OnPressed`.
    fn studio_frame_advance(&mut self, entity: &EntityCore, cx: &Context<'_>) {
        let Some(model) = entity.model.as_deref() else {
            return;
        };
        let Lookup::Found(info) = cx.sequence(model, self.sequence) else {
            return;
        };
        let now = cx.curtime();
        let interval = (now - self.anim_time).clamp(0.0, MAX_ANIMTIME_INTERVAL);
        if interval <= 0.001 {
            return;
        }
        self.anim_time = now;

        let cycle = self.cycle + interval * info.cycle_rate();
        if !(0.0..1.0).contains(&cycle) {
            self.cycle = match info.loops {
                true => cycle.rem_euclid(1.0),
                false => cycle.clamp(0.0, 1.0),
            };
            self.sequence_finished = true;
        } else {
            if cycle > info.last_visible_cycle(1.0) {
                self.sequence_finished = true;
            }
            self.cycle = cycle;
        }
    }

    /// `CPropButton::AnimateThink` (`prop_button.cpp:210`) — "this loop runs
    /// every time an animation finishes and figures out the next animation to
    /// play".
    fn animate_think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.studio_frame_advance(entity, cx);

        if self.sequence_finished {
            let seq = self.kind.sequences();
            let now = cx.curtime();
            if self.sequence == seq.up {
                self.reset_sequence(seq.idle_up);
                self.on_button_reset(entity, cx);
            } else if self.sequence == seq.down {
                self.reset_sequence(seq.idle_down);
                self.goal_time = now + self.delay;
                self.on_pressed(entity, cx);
                if self.is_timer {
                    self.timer_due = Some(cx.time.time_to_ticks(now + TIMER_THINK_INTERVAL));
                    if !self.prevent_fast_reset {
                        // "since this is a timer button the button resets to
                        // the up position immediately after being pressed".
                        self.reset_sequence(seq.up);
                    }
                }
            } else if self.sequence == seq.idle_down && now > self.goal_time {
                self.reset_sequence(seq.up);
            }
        }

        self.animate_due = Some(cx.time.time_to_ticks(cx.curtime() + ANIMATE_THINK_INTERVAL));
    }

    /// `CPropButton::TimerThink` (`:276`).
    fn timer_think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if self.goal_time > cx.curtime() {
            // `EmitSound( "Portal.room1_TickTock" )`.
            self.timer_due = Some(cx.time.time_to_ticks(cx.curtime() + TIMER_THINK_INTERVAL));
            return;
        }
        self.timer_due = None;
        if self.timer_cancelled {
            self.timer_cancelled = false;
        } else {
            // `EmitSound( "Portal.button_up" )`.
            let me = entity.id();
            entity.fire_output("OnButtonReset", Variant::Void, Some(me), Some(me), 0.0, cx);
        }
    }

    /// Arms the entity's one think at whichever schedule is due first.
    fn schedule(&self, entity: &mut EntityCore, cx: &Context<'_>) {
        let due = match (self.animate_due, self.timer_due) {
            (Some(a), Some(t)) => a.min(t),
            (Some(a), None) => a,
            (None, Some(t)) => t,
            (None, None) => return,
        };
        entity.set_next_think(cx.time.ticks_to_time(due), cx);
    }

    /// `CPropButton::Press` (`:311`).
    ///
    /// **Only an idle, raised button goes down** — a press while it is
    /// already travelling, or waiting to come back up, does nothing to the
    /// animation. It still replaces the activator, so the `OnPressed` that is
    /// already on its way names whoever pressed last.
    fn press(&mut self, activator: Option<EntityId>) {
        if self.locked {
            // `EmitSound( "Portal.button_locked" )`.
            return;
        }
        let seq = self.kind.sequences();
        if self.sequence == seq.idle_up {
            self.reset_sequence(seq.down);
            // `EmitSound( "Portal.button_down" )`.
        }
        self.activator = activator;
    }

    /// `CPropButton::OnPressed` (`:382`). The multiplayer team half is not
    /// here; what is left is `OnPressed`, from the activator if there is one
    /// and from the button itself if not.
    fn on_pressed(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let me = entity.id();
        let activator = self
            .activator
            .filter(|&id| cx.entity(id).is_some())
            .unwrap_or(me);
        entity.fire_output("OnPressed", Variant::Void, Some(activator), Some(me), 0.0, cx);
    }

    /// `CPropButton::OnButtonReset` (`:407`) — the animation's reset, which a
    /// timer defers to [`timer_think`](PedestalButton::timer_think).
    fn on_button_reset(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if self.timer_cancelled {
            self.timer_cancelled = false;
        } else if !self.is_timer {
            // `EmitSound( "Portal.button_up" )`.
            let me = entity.id();
            entity.fire_output("OnButtonReset", Variant::Void, Some(me), Some(me), 0.0, cx);
        }
        // The timer branch is `STEAMWORKS_SELFCHECK()`, a DRM check.
    }

    /// `m_bLocked`, for the tests and for `ent_dump`.
    #[allow(dead_code)]
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// The current sequence's label.
    #[allow(dead_code)]
    pub fn sequence(&self) -> &'static str {
        self.sequence
    }
}

impl Behaviour for PedestalButton {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("Delay") {
            self.delay = atof(value);
            return true;
        }
        if is("IsTimer") {
            self.is_timer = atoi(value) != 0;
            return true;
        }
        if is("PreventFastReset") {
            self.prevent_fast_reset = atoi(value) != 0;
            return true;
        }
        if is("skin") {
            self.skin = atoi(value);
            return true;
        }
        false
    }

    /// `CPropButton::Spawn` (`:171`).
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult {
        entity.move_type = MoveType::None;
        entity.solid = Solid::VPhysics;
        entity.model = Some(self.kind.model().to_owned());
        // `CPropButton::CPropButton` — `AddEffects( EF_MARKED_FOR_FAST_REFLECTION )`.
        entity.effects |= effects::MARKED_FOR_FAST_REFLECTION;

        self.goal_time = 0.0;
        // `CBaseAnimating`'s constructor sets `m_flAnimTime` to the time the
        // entity was made.
        self.anim_time = cx.curtime();
        // "Start 'up'".
        self.reset_sequence(self.kind.sequences().idle_up);
        SpawnResult::Ok
    }

    /// `CPropButton::Activate` (`:200`) — arms `AnimateThink`.
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.animate_due = Some(cx.time.time_to_ticks(cx.curtime() + ANIMATE_THINK_INTERVAL));
        self.schedule(entity, cx);
    }

    /// Both schedules, the base think first — `PhysicsRunThink` runs the
    /// base think before the contexts.
    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let tick = cx.time.tick;
        if self.animate_due.is_some_and(|due| due <= tick) {
            self.animate_think(entity, cx);
        }
        if self.timer_due.is_some_and(|due| due <= tick) {
            self.timer_think(entity, cx);
        }
        self.schedule(entity, cx);
    }

    fn accept_input(
        &mut self,
        _entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);
        // `InputPress` (`:334`) — 12 shipped connections.
        if is("Press") {
            self.press(input.activator);
            return true;
        }
        // `InputLock` / `InputUnlock` (`:465`, `:475`). 16 `Lock`s and 9
        // unlocks in the shipped game, 8 of them spelled `UnLock`.
        if is("Lock") {
            self.locked = true;
            return true;
        }
        if is("Unlock") {
            self.locked = false;
            return true;
        }
        // `InputCancelPress` (`:342`) — "expire the timer". 8 connections.
        if is("CancelPress") {
            self.timer_cancelled = true;
            self.goal_time = cx.curtime();
            return true;
        }
        if is("skin") {
            self.skin = input.value.int();
            return true;
        }
        false
    }

    /// `CPropButton::Use` (`:445`) — **only a player presses it**. An I/O
    /// `Use` from a relay has no player for an activator and is ignored,
    /// which is why the maps fire `Press` instead.
    fn use_entity(
        &mut self,
        _entity: &mut EntityCore,
        _use_type: UseType,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) {
        let by_player = input
            .activator
            .and_then(|id| cx.entity(id))
            .is_some_and(|activator| activator.has_flags(FL_CLIENT));
        if by_player {
            self.press(input.activator);
        }
    }

    /// `ObjectCaps() | FCAP_IMPULSE_USE` (`:41`).
    fn object_caps(&self) -> u32 {
        FCAP_IMPULSE_USE
    }

    fn model_state(&self) -> Option<ModelState<'_>> {
        Some(ModelState {
            sequence: self.sequence,
            cycle: self.cycle,
            anim_time: self.anim_time,
            playback_rate: 1.0,
            skin: self.skin,
        })
    }

    /// `DrawDebugTextOverlays` (`:486`), which prints the same four things.
    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("locked", self.locked.to_string()),
            ("is_timer", self.is_timer.to_string()),
            ("delay", format!("{:.2}", self.delay)),
            ("goal_time", format!("{:.3}", self.goal_time)),
            ("sequence", self.sequence.to_owned()),
            ("cycle", format!("{:.3}", self.cycle)),
            ("sequence_finished", self.sequence_finished.to_string()),
            ("skin", self.skin.to_string()),
        ]
    }
}
