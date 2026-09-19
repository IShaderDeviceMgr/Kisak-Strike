//! The prop family — the models the *game* places, and the one you stand on.
//!
//! `game/server/props.cpp`'s `CDynamicProp` and
//! `game/server/portal2/prop_floor_button.cpp`'s `CPropFloorButton`, which is
//! the derived class: a floor button **is** a dynamic prop, so the two belong
//! in one file and the base arrived second.
//!
//! ```text
//!   8072  prop_dynamic             CDynamicProp
//!    390  prop_dynamic_override    CDynamicProp
//!     65  prop_floor_button        CPropFloorButton : CDynamicProp
//!     65  trigger_portal_button    CPortalButtonTrigger : CBaseTrigger
//!      0  dynamic_prop             CDynamicProp
//!      0  prop_dynamic_glow        CDynamicProp
//! ```
//!
//! **8,462 of the two `prop_dynamic` classnames across 105 of the 106 maps**,
//! which makes them the commonest thing in a Portal 2 map after `logic_relay`
//! — 90 of them on `sp_a1_intro1` alone, from 52 distinct models — and they
//! carry 5,311 `SetAnimation` connections, more than any other input in the
//! game reaches a class this port implements. [`DynamicProp`] has the
//! measurements; what follows is the button.
//!
//! **65 buttons across 47 of the game's 106 maps**, including one on
//! `sp_a1_intro1`, and 227 output connections on them — `OnPressed` (126),
//! `OnUnPressed` (99) and one each of the two co-op team outputs. Four shipped
//! connections fire `PressIn` at one and four fire `PressOut`.
//!
//! # A button is two entities, and the second one is the interesting one
//!
//! `CPropFloorButton` is a prop and does not collide with anything. What
//! notices the player is a **second entity it creates in its own `Spawn`** — a
//! `trigger_portal_button`, 40 x 40 x 14 units, centred on the pad and turned
//! to match it — and the pad presses when that trigger's `OnStartTouchAll`
//! fires and releases on `OnEndTouchAll`. Everything visible about a floor
//! button is a consequence of those two moments.
//!
//! That shape is what this class cost to add, and it is all framework rather
//! than button:
//!
//! - **[`Context::create_entity`]** — `CreateEntityByName` + `DispatchSpawn`,
//!   the first time anything in this port makes an entity that was not in the
//!   `.bsp`.
//! - **[`Solid::Obb`]** and [`obb`](crate::server::obb) — the first trigger
//!   whose shape is a *box* rather than a brush model, which the engine cannot
//!   answer for because there is no map data in it.
//! - **[`Touched`]** — `OnStartTouchAll` and `OnEndTouchAll` are virtuals, and
//!   until now no class overrode either.
//!
//! # The model, and what moves it
//!
//! **The pad is drawn, and its plate goes down when you stand on it.**
//! `ResetSequence( m_DownSequence )` and `ResetSequence( m_UpSequence )` are
//! the whole of what [`FloorButton::press`] and [`FloorButton::unpress`] do
//! about animation, and what they write is a sequence *label* and the time it
//! started ([`ModelState`]) — this module owns no `.mdl` and does not look one
//! up. The renderer does, in
//! [`engine::world::entities`](crate::engine::world::entities), which is also
//! where the cycle is worked out. That split is Valve's: `CBaseAnimating`
//! networks `m_nSequence` and `m_flAnimTime` and it is the *client* that turns
//! them into a pose.
//!
//! **`AnimateThink` is still not scheduled, and that is now a saving rather
//! than an absence.** Its body is `StudioFrameAdvance`, `DispatchAnimEvents`
//! and a bone-follower update; the first is what the renderer does for itself
//! from `m_flAnimTime` (and does *smoothly*, where a 10 Hz think would step),
//! and the other two have nothing to drive. So 65 entities do not wake ten
//! times a second and no button sits in the simulation list for ever.
//!
//! What `CPropFloorButton` inherits and does not get is [`DynamicProp`]'s
//! list — bone followers, `VPhysicsInitStatic`, prop data, LOS blocking, fade
//! distances — plus `m_nSkin`, which is parsed, kept and printed by
//! `ent_dump` but not drawn, because a model's skin families are
//! `portdocs/STUDIO.md` stage 6's.
//!
//! > **`CPropFloorButton` does not contain a [`DynamicProp`]**, where
//! > `EnvLight` contains a `Light` and `RotDoor` *is* a `Door`. That is a
//! > measurement rather than a style choice: `CPropFloorButton::Spawn` calls
//! > `BaseClass::Spawn` and then overrides the solidity, the sequence, the
//! > skin and the fade distances, and it inherits **no** `CDynamicProp` field
//! > that anything reads — no `DefaultAnim`, no `HoldAnimation`, no goal
//! > sequence, and an `AnimThink` it replaces with its own. Composing them
//! > would share four lines and two unused fields.
//!
//! Also absent, and each measured rather than assumed: the weighted cube and
//! the monster box (`prop_weighted_cube` is not ported, so **the player is the
//! only thing in this port that can press a button** — which is what
//! `prop_floor_button` is for, and is why its three siblings are not here);
//! the co-op team outputs, which need `GameRules()->IsMultiplayer()`; the
//! `ACH.BOX_HOLE_IN_ONE` achievement think; and `sv_slippery_cube_button`'s
//! surface-property swap, which is vphysics.
//!
//! **`UpdateOnRemove` is not ported either, so a killed button leaves its
//! trigger behind.** `CPropFloorButton::UpdateOnRemove` is
//! `UTIL_Remove( m_hButtonTrigger )`, and there is no removal hook on
//! [`Behaviour`] to hang it on — adding one is a framework change with, today,
//! exactly one consumer and no shipped caller: **no connection in any of the
//! 106 maps fires `Kill` at a `prop_floor_button`.** What an orphan does is
//! nothing: its owner handle stops resolving, so
//! [`ButtonTrigger::passes_trigger_filters`] refuses everything and the press
//! is never sent. The condition for adding the hook is the second class that
//! needs one.
//!
//! [`Context::create_entity`]: crate::server::class::Context::create_entity
//! [`Solid::Obb`]: crate::server::movement::Solid::Obb
//! [`Touched`]: super::trigger::Touched

use glam::Vec3;

use crate::server::class::{Behaviour, Context, InputDef, InputDefs, ModelState, SpawnResult};
use crate::server::classes::trigger::BaseTrigger;
use crate::server::damage::DamageMode;
use crate::server::entity::{EntityCore, EntityId};
use crate::server::io::{FieldType, Input, Variant};
use crate::server::keyvalue::{atof, atoi, effects};
use crate::server::movement::{
    ModelBounds, MoveType, Solid, EF_NODRAW, FL_CLIENT, FSOLID_NOT_SOLID, FSOLID_TRIGGER,
};
use crate::server::sequences::{Lookup, SequenceInfo};

// ---------------------------------------------------------------------------
// CPropFloorButton
// ---------------------------------------------------------------------------

/// `PROP_FLOOR_BUTTON_DEFAULT_MODEL_NAME` (`prop_floor_button.cpp:17`).
///
/// Never reached by shipped content — **all 65 write a `model` key** — but it
/// is what `GetButtonModelName` answers with and a hand-made map would get it.
const DEFAULT_MODEL: &str = "models/props/portal_button.mdl";

/// `button_skins` (`:27`). `SetSkin( button_off_skin )` runs in `Spawn`, after
/// the `skin` key has been read, so a map cannot choose — and the 18 shipped
/// buttons that write the key all write `0` anyway.
const BUTTON_OFF_SKIN: i32 = 0;
const BUTTON_ON_SKIN: i32 = 1;

/// The two sequence labels `CPropFloorButton::LookUpAnimationSequences`
/// (`:184`) asks the model for. `CPropUnderFloorButton` overrides them to
/// `"release"` and `"press"`, which is the only thing about it that differs
/// here.
const UP_SEQUENCE: &str = "up";
const DOWN_SEQUENCE: &str = "down";

/// The trigger's size (`CPropFloorButton::CreateTriggers`, `:461`), in the
/// button's own frame.
///
/// 40 across and 14 tall — wider than the 32-unit player hull, so the pad is
/// forgiving, and short enough that jumping off it releases it.
const TRIGGER_MINS: Vec3 = Vec3::new(-20.0, -20.0, 0.0);
const TRIGGER_MAXS: Vec3 = Vec3::new(20.0, 20.0, 14.0);

/// `CPropFloorButton` (`prop_floor_button.cpp:66`) — the big red pad you stand
/// on.
pub struct FloorButton {
    /// `m_hButtonTrigger` — the `trigger_portal_button` this button made in
    /// its own `Spawn`.
    trigger: Option<EntityId>,
    /// `m_bButtonState` — is the pad down? Networked to the client in the
    /// original, which is why it is a `CNetworkVar` there and a plain `bool`
    /// here.
    pub pressed: bool,
    /// `m_nSkin`, which is `CBaseAnimating`'s and arrives as both a map key
    /// and an input (`DEFINE_INPUT( m_nSkin, FIELD_INTEGER, "skin" )`,
    /// `baseanimating.cpp:175`). Nothing draws it; `ent_dump` prints it.
    pub skin: i32,
    /// `m_nSequence`, as the label `LookupSequence` would be given.
    ///
    /// `ResetSequence( m_UpSequence )` and `ResetSequence( m_DownSequence )`
    /// are the only two things that ever set it, and
    /// `CPropFloorButton::LookUpAnimationSequences` is where those two names
    /// come from. Held as the *name* because this module owns no `.mdl` — see
    /// [`ModelState`].
    sequence: &'static str,
    /// `m_flAnimTime` — when [`sequence`](FloorButton::sequence) was reset.
    anim_time: f32,
}

/// The inputs (`:130`) plus `CBaseAnimating`'s one.
pub static FLOOR_BUTTON_INPUTS: InputDefs = &[
    InputDef::new("PressIn", FieldType::Void),
    InputDef::new("PressOut", FieldType::Void),
    InputDef::new("skin", FieldType::Int),
];

/// The outputs (`:133`).
///
/// The two team outputs are declared and never fired: `OnPressed` reaches them
/// only under `GameRules()->IsMultiplayer()`, and one map writes each. They are
/// here so the connection parses as an output rather than as an unknown key.
pub static FLOOR_BUTTON_OUTPUTS: &[&str] = &[
    "OnPressed",
    "OnPressedOrange",
    "OnPressedBlue",
    "OnUnPressed",
];

/// The keys. `skin` is the only one the class itself consumes — `model`,
/// `angles`, `origin`, `disableshadowdepth` and `disableflashlight` are all
/// `CBaseEntity::KeyValue`'s and are already in
/// [`base_key_value`](crate::server::keyvalue::base_key_value).
pub static FLOOR_BUTTON_KEYS: &[&str] = &["skin"];

impl FloorButton {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(FloorButton {
            trigger: None,
            // "button is not pressed by default" — the constructor's one line.
            pressed: false,
            skin: BUTTON_OFF_SKIN,
            sequence: UP_SEQUENCE,
            anim_time: 0.0,
        })
    }

    /// `CPropFloorButton::ShouldPlayerTouch` (`:392`) — may a player press me?
    ///
    /// `true` here and `false` on the cube and ball buttons, which is the one
    /// thing that distinguishes them at this level. It is asked of the *owner*
    /// by [`ButtonTrigger::passes_trigger_filters`], which is Valve's
    /// `m_pOwnerButton->ShouldPlayerTouch()`.
    pub fn should_player_touch(&self) -> bool {
        true
    }

    /// `CPropFloorButton::CreateTriggers` (`:456`).
    ///
    /// > **`SetParent` is not called, and for this class it cannot matter.**
    /// > Valve parents the trigger to the button so that a button on a moving
    /// > platform carries its trigger with it. This port has no local/abs
    /// > transform pair (`rustdocs/SERVER.md` gotcha 34), and the measurement
    /// > says nothing is lost: **not one of the game's 65 `prop_floor_button`s
    /// > has a `parentname`**, and none of them is a mover. The trigger is
    /// > placed at the button's absolute origin and angles, where the parent
    /// > transform would have put it and where it stays.
    fn create_triggers(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let Some(id) = cx.create_entity("trigger_portal_button") else {
            return;
        };
        // `UTIL_SetOrigin` / `SetAbsAngles` / `UTIL_SetSize`, which is the
        // whole of what `CPortalButtonTrigger::Create` does before
        // `DispatchSpawn`.
        let (origin, angles) = (entity.origin, entity.angles);
        if let Some(core) = cx.entity_mut(id) {
            core.set_abs_placement(origin, angles);
            core.model_bounds = ModelBounds {
                mins: TRIGGER_MINS,
                maxs: TRIGGER_MAXS,
            };
        }
        // `pTrigger->m_pOwnerButton = pOwner`. The one place in this port
        // where an entity writes another's *class* state rather than sending
        // it an input — see [`Context::behaviour_mut`].
        let me = entity.id();
        if let Some(trigger) = cx.behaviour_mut::<ButtonTrigger>(id) {
            trigger.owner = Some(me);
        }
        self.trigger = Some(id);
    }

    /// `CPropFloorButton::Press` (`:293`) — the pad goes down.
    ///
    /// The three lines that are not animation: the state, the skin, and
    /// `OnPressed`.
    fn press(
        &mut self,
        entity: &mut EntityCore,
        activator: Option<EntityId>,
        cx: &mut Context<'_>,
    ) {
        self.pressed = true;
        self.skin = BUTTON_ON_SKIN;
        // `ResetSequence( m_DownSequence )` — a **cut**, not a blend, which is
        // Valve's own word for it and is why nothing here cross-fades.
        self.reset_sequence(DOWN_SEQUENCE, cx);

        // `CPropFloorButton::OnPressed` (`:334`). Its multiplayer half needs
        // `GameRules()->IsMultiplayer()` and its `prop_monster_box` and
        // `prop_weighted_cube` halves need classes this port has not got, so
        // what is left is the last line.
        let me = entity.id();
        entity.fire_output("OnPressed", Variant::Void, activator, Some(me), 0.0, cx);
    }

    /// `ResetSequence` (`baseanimating.cpp:1180`) — start a sequence from the
    /// beginning.
    ///
    /// The whole of it that survives: the new sequence, and the time it
    /// started. `ResetSequence` also clears the sequence-finished flag, rebuilds
    /// the activity list and re-derives the bounding box, none of which exists
    /// here.
    fn reset_sequence(&mut self, sequence: &'static str, cx: &Context<'_>) {
        self.sequence = sequence;
        self.anim_time = cx.curtime();
    }

    /// `CPropFloorButton::UnPress` (`:313`).
    fn unpress(
        &mut self,
        entity: &mut EntityCore,
        activator: Option<EntityId>,
        cx: &mut Context<'_>,
    ) {
        self.pressed = false;
        self.skin = BUTTON_OFF_SKIN;
        self.reset_sequence(UP_SEQUENCE, cx);

        let me = entity.id();
        entity.fire_output("OnUnPressed", Variant::Void, activator, Some(me), 0.0, cx);
    }
}

impl Behaviour for FloorButton {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("skin") {
            self.skin = atoi(value);
            return true;
        }
        false
    }

    /// `CPropFloorButton::Spawn` (`:158`), minus everything that needs a
    /// subsystem — see the module docs for the full accounting.
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult {
        // `KeyValue( "model", GetButtonModelName() )`: the map's model if it
        // wrote one, the default if not. All 65 shipped buttons write one, so
        // in practice this line re-reads what is already there (three distinct
        // models, two of them damaged variants) — and the three sibling
        // classes override `GetButtonModelName` to ignore it.
        if entity.model.is_none() {
            entity.model = Some(DEFAULT_MODEL.to_owned());
        }

        // `SetSolid( SOLID_VPHYSICS )`. Nothing collides with it: a floor
        // button's collision is its `.phy`, which is `ENGINE_TRACE.md` stage 5,
        // and `World::clip_models` only ever sees `"*N"` brush models. So you
        // walk through the pad rather than stepping onto it, which changes
        // nothing about whether it presses — the trigger is 14 units tall and
        // starts at the floor.
        entity.solid = Solid::VPhysics;
        entity.move_type = MoveType::None;

        // `SetSkin( button_off_skin )`, *after* the key has been read — so the
        // key cannot choose the starting skin, which is Valve's and is why all
        // 18 shipped `skin` keys are `0`.
        self.skin = BUTTON_OFF_SKIN;

        // `AddEffects( EF_MARKED_FOR_FAST_REFLECTION )`.
        entity.effects |= effects::MARKED_FOR_FAST_REFLECTION;

        self.create_triggers(entity, cx);
        SpawnResult::Ok
    }

    /// `CPropFloorButton::Activate` (`:218`) arms `AnimateThink` at 10 Hz, and
    /// **that think is not scheduled here**.
    ///
    /// Its body is `StudioFrameAdvance`, `DispatchAnimEvents`, a bone-follower
    /// update and an `ent_bbox` debug overlay. There is no studio animation in
    /// this port and no bone followers, so all it would do is wake 65 entities
    /// ten times a second to do nothing — and it would put every button in the
    /// game permanently into the simulation list, which is the number
    /// `rustdocs/SERVER.md` uses to say the list is being entered and left
    /// rather than filled once.
    fn activate(&mut self, _entity: &mut EntityCore, _cx: &mut Context<'_>) {}

    /// `InputPressIn` / `InputPressOut` (`:325`, `:330`), and
    /// `CBaseAnimating`'s `skin`.
    ///
    /// > **These are the *same* `Press` the trigger reaches**, which is why
    /// > the trigger sends one rather than calling in: see
    /// > [`ButtonTrigger::start_touch`]. Four shipped connections fire
    /// > `PressIn` and four fire `PressOut`.
    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        if input.name.eq_ignore_ascii_case("PressIn") {
            self.press(entity, input.activator, cx);
            return true;
        }
        if input.name.eq_ignore_ascii_case("PressOut") {
            self.unpress(entity, input.activator, cx);
            return true;
        }
        if input.name.eq_ignore_ascii_case("skin") {
            self.skin = input.value.int();
            return true;
        }
        false
    }

    /// What the renderer needs to pose this button's model.
    ///
    /// The cycle starts at zero and the rate is 1: a button's two sequences
    /// are each played once, forwards, from the beginning, which is the whole
    /// of what `ResetSequence` does. `CBaseProp::Spawn`'s rate of **0** does
    /// not reach here, because `CPropFloorButton::Spawn` ends with a
    /// `ResetSequence( m_UpSequence )` and `ResetSequenceInfo` puts the rate
    /// back to 1.
    fn model_state(&self) -> Option<ModelState<'_>> {
        Some(ModelState {
            sequence: self.sequence,
            cycle: 0.0,
            anim_time: self.anim_time,
            playback_rate: 1.0,
            skin: self.skin,
        })
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("pressed", self.pressed.to_string()),
            ("skin", self.skin.to_string()),
            ("sequence", self.sequence.to_owned()),
            ("anim_time", format!("{:.3}", self.anim_time)),
            ("trigger", format!("{:?}", self.trigger)),
        ]
    }
}

// ---------------------------------------------------------------------------
// CPortalButtonTrigger
// ---------------------------------------------------------------------------

/// `CPortalButtonTrigger` (`prop_floor_button.cpp:34`) — the box over a floor
/// button, and the first `SOLID_OBB` trigger in this port.
///
/// It appears in no entity lump. Every one of them is made by a button's
/// `Spawn`, which is why its `ClassDef` declares the base trigger's keys it
/// will never be offered: the class table is a factory as well as a
/// declaration, and [`Context::create_entity`](crate::server::class::Context::create_entity)
/// goes through it.
#[derive(Default)]
pub struct ButtonTrigger {
    base: BaseTrigger,
    /// `m_pOwnerButton`. An `EntityId` where Valve keeps a raw pointer, so a
    /// button removed out from under its trigger is a `None` here and a crash
    /// there.
    owner: Option<EntityId>,
}

impl ButtonTrigger {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<ButtonTrigger>::default()
    }

    /// `CPortalButtonTrigger::PassesTriggerFilters` (`:526`).
    ///
    /// The base's question first — which is where `SF_TRIGGER_ALLOW_CLIENTS`
    /// and any `filtername` are answered — and then a second, narrower one:
    /// **is this the kind of thing my owner accepts?** A player passes if the
    /// owner says `ShouldPlayerTouch`, and a cube passes if its shape matches
    /// what the owner takes. Anything else is refused, which is the line that
    /// makes a floor button ignore everything that is not a player or a cube
    /// even though its trigger allows physics objects in general.
    ///
    /// The cube half is not written: `prop_weighted_cube` and
    /// `prop_monster_box` are not ported, so nothing can reach it and a
    /// `false` is the whole of the remaining branch.
    fn passes_trigger_filters(
        &self,
        entity: &EntityCore,
        other: EntityId,
        cx: &Context<'_>,
    ) -> bool {
        if !self.base.passes_trigger_filters(entity, other, cx) {
            return false;
        }
        let Some(other_core) = cx.entity(other).map(|e| &e.core) else {
            return false;
        };

        // `m_pOwnerButton->ShouldPlayerTouch()`, asked of the owner rather
        // than answered here, because it is the one thing the three sibling
        // classes disagree about.
        let accepts_players = self
            .owner
            .and_then(|id| cx.entity(id))
            .and_then(|e| e.behaviour.downcast_ref::<FloorButton>())
            .is_some_and(FloorButton::should_player_touch);

        accepts_players && other_core.has_flags(FL_CLIENT)
    }
}

impl Behaviour for ButtonTrigger {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        self.base.key_value(key, value)
    }

    /// `CPortalButtonTrigger::Spawn` (`:610`).
    ///
    /// It does **not** call `InitTrigger`, which every other trigger in the
    /// port does: there is no brush model to take a size from, so the three
    /// solidity writes are made by hand and the size arrives from
    /// [`FloorButton::create_triggers`] instead. `SetSolidFlags` *replaces*
    /// where `AddSolidFlags` would add, and the difference is real — it is
    /// what keeps `FSOLID_TRIGGER_TOUCH_DEBRIS` and friends off it.
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        entity.move_type = MoveType::None;
        entity.solid = Solid::Obb;
        entity.solid_flags = FSOLID_NOT_SOLID | FSOLID_TRIGGER;

        // `AddSpawnFlags( SF_TRIGGER_ALLOW_CLIENTS | SF_TRIGGER_ALLOW_PHYSICS )`
        // — before `BaseClass::Spawn()`, so the promotions there see them.
        // The physics bit is what would let a cube in; with no cubes it only
        // reaches [`passes_trigger_filters`](ButtonTrigger::passes_trigger_filters)'s
        // refusal.
        entity.spawn_flags |= SF_TRIGGER_ALLOW_CLIENTS | SF_TRIGGER_ALLOW_PHYSICS;
        self.base.spawn(entity);
        SpawnResult::Ok
    }

    fn activate(&mut self, _entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.base.activate(cx);
    }

    /// `CPortalButtonTrigger::StartTouch` (`:497`) and `OnStartTouchAll`
    /// (`:559`) — the moment a floor button presses.
    ///
    /// > **The press is sent as a `PressIn` input rather than called.**
    /// > Valve's `m_pOwnerButton->TriggerStartTouch( pOther )` is a direct
    /// > call into another entity's class, and this module has no way to do
    /// > that: `Server::dispatch` has lifted *this* trigger out of the entity
    /// > list, so the button is reachable as data
    /// > ([`Context::entity_mut`](crate::server::class::Context::entity_mut))
    /// > but not as code. `PressIn` is the input that already means exactly
    /// > this and that four shipped connections already fire, so the trigger
    /// > posts one, with the toucher as the activator.
    /// >
    /// > **It costs one queue hop and no tick.** The button's `OnPressed`
    /// > connections were going onto the same queue anyway, and the queue
    /// > restarts from the head after every event, so a zero-delay chain
    /// > completes within the tick it started in (`rustdocs/SERVER.md`
    /// > gotcha 4). What is observable is the ordering against other
    /// > zero-delay events in the same tick, and one extra row in
    /// > [`IoStats::dispatched`](crate::server::io::IoStats).
    fn start_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        let passed = self.passes_trigger_filters(entity, other, cx);
        let touched = self.base.start_touch_passing(passed, entity, other, cx);
        if !touched.all {
            return;
        }
        if let Some(owner) = self.owner {
            cx.post_entity(
                owner,
                "PressIn",
                Variant::Void,
                0.0,
                Some(other),
                Some(entity.id()),
            );
        }
    }

    /// `CPortalButtonTrigger::EndTouch` (`:514`) and `OnEndTouchAll` (`:572`).
    ///
    /// `CBaseTrigger::EndTouch` consults neither the filters nor
    /// `m_bDisabled`, so there is no override to make here — see
    /// `rustdocs/SERVER.md` gotcha 42 for the two commented-out tests that is.
    fn end_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        if !self.base.end_touch(entity, other, cx) {
            return;
        }
        if let Some(owner) = self.owner {
            cx.post_entity(
                owner,
                "PressOut",
                Variant::Void,
                0.0,
                Some(other),
                Some(entity.id()),
            );
        }
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
        let mut out = vec![("owner", format!("{:?}", self.owner))];
        out.extend(self.base.describe());
        out
    }
}

/// `SF_TRIGGER_ALLOW_CLIENTS` and `SF_TRIGGER_ALLOW_PHYSICS`
/// (`game/shared/triggers_shared.h:16`), which `CPortalButtonTrigger::Spawn`
/// adds. They live here rather than in [`trigger`](super::trigger) because
/// that module keeps its spawnflag table private and this is the only class
/// outside it that sets one.
const SF_TRIGGER_ALLOW_CLIENTS: u32 = 0x01;
const SF_TRIGGER_ALLOW_PHYSICS: u32 = 0x08;

// ---------------------------------------------------------------------------
// CDynamicProp
// ---------------------------------------------------------------------------

/// `SF_DYNAMICPROP_USEHITBOX_FOR_RENDERBOX` and
/// `SF_DYNAMICPROP_DISABLE_COLLISION` (`props.h:265`). 36 shipped props set
/// the first and 161 the second; `SF_DYNAMICPROP_NO_VPHYSICS` (128) is set by
/// none, so it is quoted rather than declared.
const SF_DYNAMICPROP_USEHITBOX_FOR_RENDERBOX: u32 = 64;
const SF_DYNAMICPROP_DISABLE_COLLISION: u32 = 256;

/// `AnimThink`'s cadence (`props.cpp:2358`) — ten times a second.
const ANIM_THINK_INTERVAL: f32 = 0.1;

/// `SUB_PerformFadeOut`'s "fade out over 1 second" (`baseentity.cpp:8437`),
/// as the `256 * dt` it is written as.
const FADE_ALPHA_PER_SECOND: f32 = 256.0;

/// `CDynamicProp` (`props.cpp:272` in `props.h`) — **a model the map places
/// and the map animates**, and with 8,462 entities across two classnames the
/// commonest thing in Portal 2 after `logic_relay`.
///
/// ```text
///   8072  prop_dynamic            CDynamicProp
///    390  prop_dynamic_override   CDynamicProp, with propdata and health allowed
///      0  dynamic_prop            renamed to prop_dynamic by its own Spawn
///      0  prop_dynamic_glow       CDynamicProp, and no map places one
/// ```
///
/// One struct for all four, because Valve links all four to one class; what
/// differs between them is two `FClassnameIs` tests, and both are asked of
/// [`EntityCore::class`] rather than stored — see [`is_plain_dynamic`] and
/// [`allows_health`].
///
/// # What it does, in the order `Spawn` does it
///
/// The interesting half is the animation, and it is a five-field state
/// machine: a sequence label, a cycle, the time that cycle was true, a
/// playback rate, and whether the sequence has been seen to finish. Every
/// input below writes some of those five and arms [`AnimThink`]; the renderer
/// reads them through [`ModelState`] and works the pose out for itself.
///
/// # Deliberately not here, each measured
///
/// - **`ParsePropData`** — the breakable-prop system: `scripts/propdata.txt`,
///   the gib lists, the `PROPINTER_*` interactions, `prop_physics`. The one
///   thing it decides for *this* class is a deletion: a `prop_dynamic` (but
///   not a `prop_dynamic_override`, which is what the classname is *for*)
///   whose model carries a `prop_data` block is removed at load with a
///   `DevWarning`. Measured over the 106 shipped maps: 15 of the 606 models a
///   `prop_dynamic*` names have such a block, 106 entities use one, and
///   **94 of those 106 are `prop_dynamic_override`** — so the whole of what
///   this absence costs is **12 entities across three maps**
///   (`sp_a2_bts3`, `mp_coop_tbeam_end` and `sp_a1_intro7`, all of them a
///   laser gib or a lab chair) which the shipped game deletes and this port
///   draws.
/// - **Bone followers.** `CreateBoneFollowers` turns a model's `bone_followers`
///   keyvalue block into one physics entity per bone so that an animated prop
///   can *collide* as it moves. That is `vphysics`, which this port replaces
///   with `rapier` and has not reached; `DisableBoneFollowers` is parsed (260
///   props set it) and the two branches it chooses between are both absent.
/// - **`VPhysicsInitStatic`.** A prop's collision is its `.phy`, which is
///   `portdocs/ENGINE_TRACE.md` stage 5. So a `prop_dynamic` is drawn and is
///   walked through — **5,629 of them write `solid 6`** and would be solid in
///   the shipped game.
/// - **The glow block.** `m_bShouldGlow`, `m_clrGlow`, `m_nGlowStyle` and
///   their six inputs are CS:GO's wall-hack glow, and they reach the client
///   through a `CCSUsrMsg_GlowPropTurnOff` user message. **No shipped Portal 2
///   map writes `glowenabled`, `glowcolor`, `glowdist` or `glowstyle`, and no
///   connection fires one of the inputs**, so the class declares none of them.
/// - **`m_bRandomAnimator`.** Parsed, and dead: all 5,117 props that write
///   `RandomAnimation` write `0`, and `MinAnimTime`/`MaxAnimTime` are Hammer's
///   defaults of 5 and 10 on every one of the 8,462. It is the one branch of
///   `AnimThink` that needs `SelectWeightedSequence( ACT_IDLE )`, which needs
///   the activity table, which nothing else here wants.
/// - **`HandleAnimEvent`**, **`m_bUseHitboxesForRenderBox`**, **`BlockLOS`**,
///   **`SuppressAnimSounds`**, **`AnimateEveryFrame`** — each parsed where it
///   is a key and each with nothing in this port to drive it. The last two are
///   both about *how often the server advances the cycle*, which here it never
///   does; see the note on the think below.
pub struct DynamicProp {
    /// `m_iszDefaultAnim` — the idle to fall back to when a forced animation
    /// finishes. 2,416 props carry it, naming 358 distinct sequences.
    default_anim: String,
    /// `m_nSequence`, as a label. `""` is the bind pose.
    sequence: String,
    /// `m_flCycle` at [`anim_time`](DynamicProp::anim_time).
    cycle: f32,
    /// `m_flAnimTime`.
    anim_time: f32,
    /// `m_flPlaybackRate`. **Zero until a sequence is set**, which is
    /// `CBaseProp::Spawn`'s doing and is why 6,046 of the game's props stand
    /// perfectly still.
    playback_rate: f32,
    /// `m_bAnimationDone` — whether `OnAnimationDone` has already been fired
    /// for the sequence now playing.
    animation_done: bool,
    /// `m_bHoldAnimation` — hold the last frame instead of reverting to
    /// [`default_anim`](DynamicProp::default_anim). 857 props set it.
    hold_animation: bool,
    /// `m_bStartDisabled` — **1,000 props in the game start invisible.**
    start_disabled: bool,
    /// `m_nSkin`, and `m_nBody` for `SetBodyGroup`. Parsed and carried; the
    /// renderer draws neither, because skin families and bodygroups are
    /// `portdocs/STUDIO.md` stage 6's.
    skin: i32,
    body: i32,
    /// Whether [`fade_think`](DynamicProp::fade_think) is what the schedule is
    /// for — `SetThink( &CBaseEntity::SUB_FadeOut )` having replaced
    /// `AnimThink`. One `bool` where Valve has a function pointer, because
    /// this class has exactly the two thinks.
    fading: bool,
    /// `m_clrRender`'s alpha while fading. `EntityCore::render_color[3]` is
    /// the same number; this is the one the fade counts down, so that the fade
    /// does not depend on a field the renderer may start honouring.
    fade_alpha: f32,
}

/// The keys (`props.cpp:1922`), plus `CBaseAnimating`'s and `CBreakableProp`'s
/// that shipped maps write.
///
/// **Every one of these is on some shipped `prop_dynamic`**, which is the
/// point of the list: the depot test asserts that nothing declared here goes
/// unconsumed and that nothing a map writes goes unread. Four keys the maps
/// *do* write are deliberately missing — `mindxlevel`, `maxdxlevel`,
/// `disablex360` and the pair `canbecaptured`/`scalevalue` — because nothing
/// anywhere in `legacy/` reads them either; they belong with `_light` and the
/// rest of the compiler's and the mappers' leftovers.
pub static DYNAMIC_PROP_KEYS: &[&str] = &[
    // CDynamicProp's own.
    "DefaultAnim",
    "RandomAnimation",
    "MinAnimTime",
    "MaxAnimTime",
    "HoldAnimation",
    "AnimateEveryFrame",
    "DisableBoneFollowers",
    "StartDisabled",
    // CBaseAnimating's.
    "skin",
    "SetBodyGroup",
    "LightingOrigin",
    "SuppressAnimSounds",
    // CBreakableProp's and CBreakable's, all four of them inert on a prop that
    // cannot break.
    "PerformanceMode",
    "ExplodeDamage",
    "ExplodeRadius",
    "PressureDelay",
];

/// The inputs (`props.cpp:1944`), minus the six glow ones no map fires, plus
/// `CBaseAnimating`'s `skin` and `CBreakableProp`'s `Break`.
///
/// `Enable`/`Disable` are Valve's own second names for `TurnOn`/`TurnOff` —
/// two `DEFINE_INPUTFUNC`s onto one handler each — and between them the
/// shipped maps fire the pair 1,405 times.
pub static DYNAMIC_PROP_INPUTS: InputDefs = &[
    InputDef::new("SetAnimation", FieldType::String),
    InputDef::new("SetAnimationNoReset", FieldType::String),
    InputDef::new("SetDefaultAnimation", FieldType::String),
    InputDef::new("SetPlaybackRate", FieldType::Float),
    InputDef::new("TurnOn", FieldType::Void),
    InputDef::new("TurnOff", FieldType::Void),
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
    InputDef::new("EnableCollision", FieldType::Void),
    InputDef::new("DisableCollision", FieldType::Void),
    InputDef::new("BecomeRagdoll", FieldType::Void),
    InputDef::new("FadeAndKill", FieldType::Void),
    InputDef::new("Break", FieldType::Void),
    InputDef::new("SetBodyGroup", FieldType::Int),
    InputDef::new("skin", FieldType::Int),
];

/// The outputs. `OnAnimationBegun`/`OnAnimationDone` are `CDynamicProp`'s
/// (`props.cpp:1968`) and `OnBreak` is `CBreakableProp`'s; the shipped maps
/// carry 15, 181 and 16 connections of them.
pub static DYNAMIC_PROP_OUTPUTS: &[&str] = &["OnAnimationBegun", "OnAnimationDone", "OnBreak"];

impl DynamicProp {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(DynamicProp {
            default_anim: String::new(),
            sequence: String::new(),
            cycle: 0.0,
            anim_time: 0.0,
            playback_rate: 0.0,
            animation_done: false,
            hold_animation: false,
            start_disabled: false,
            skin: 0,
            body: 0,
            fading: false,
            fade_alpha: 255.0,
        })
    }

    /// `FClassnameIs( this, "prop_dynamic" )`, as `CDynamicProp::Spawn` asks
    /// it — which is **after** the `dynamic_prop` rename and **before** the
    /// `prop_dynamic_override` one.
    ///
    /// > **This is the whole behavioural difference between the two classnames
    /// > a map can place**, and it is easy to miss because the rename that
    /// > hides it happens two statements later. A `prop_dynamic` with
    /// > `solid 0` is promoted to `SOLID_OBB` so that its render box turns
    /// > with it; a `prop_dynamic_override` with `solid 0` stays `SOLID_NONE`.
    /// > 2,622 of the game's props take the promotion and **211 are refused
    /// > it**. `prop_dynamic_glow` is refused too, because it is never
    /// > renamed.
    fn is_plain_dynamic(entity: &EntityCore) -> bool {
        matches!(entity.class.name, "prop_dynamic" | "dynamic_prop")
    }

    /// `CBaseProp::KeyValue`'s `health` gate (`props.cpp:303`) and
    /// `CDynamicProp::OverridePropdata` (`props.cpp:2114`), which are the same
    /// question asked twice: **only an `_override` may have propdata or
    /// health.**
    ///
    /// The shipped maps settle what that is worth: all 344 `health` keys on a
    /// `prop_dynamic*` write `0`, so the gate changes nothing anybody can see
    /// — and the *other* half of it, the propdata deletion, is the 12-entity
    /// measurement in this type's docs.
    fn allows_health(entity: &EntityCore) -> bool {
        entity.class.name == "prop_dynamic_override"
    }

    /// Where in the current sequence this prop is, now.
    ///
    /// `StudioFrameAdvance`'s accumulation solved rather than stepped:
    /// `m_flCycle += dt * rate / duration` integrates to
    /// `cycle + elapsed * rate / duration`. **`engine::world::entities` computes
    /// the same expression from the same five numbers** — that duplication is
    /// the price of the server not owning a `.mdl`, and the two must not drift.
    fn cycle_now(&self, now: f32, info: SequenceInfo) -> f32 {
        if info.duration <= 0.0 {
            return self.cycle;
        }
        let cycle =
            self.cycle + (now - self.anim_time).max(0.0) * self.playback_rate / info.duration;
        match info.loops {
            true => cycle.rem_euclid(1.0),
            false => cycle.clamp(0.0, 1.0),
        }
    }

    /// What the table says about the sequence now playing.
    fn current_sequence(&self, entity: &EntityCore, cx: &Context<'_>) -> Lookup {
        match entity.model.as_deref() {
            Some(model) => cx.sequence(model, &self.sequence),
            None => Lookup::Unknown,
        }
    }

    /// `CDynamicProp::PropSetAnim` (`props.cpp:2395`) — set a sequence by
    /// name, or complain and stand still.
    ///
    /// > **[`Lookup::Unknown`] is treated as success**, and it has to be: the
    /// > models are not loaded until after `level_init`, so the
    /// > `PropSetAnim( DefaultAnim )` that 2,416 shipped props run in their
    /// > own `Spawn` is asked of an empty table. The consequence is bounded
    /// > and measured — **183 of those 2,416 name a sequence that is in no
    /// > model** (Valve's own map errors, `arm_64x64_justtop.mdl` and
    /// > `underground_boxdropper.mdl` between them most of it) and those 183
    /// > take this branch rather than the warning below. What they get is a
    /// > label the renderer cannot resolve, which is the bind pose, where
    /// > Valve's `SetSequence( 0 )` would give them the model's first
    /// > sequence at frame zero.
    fn prop_set_anim(&mut self, entity: &mut EntityCore, anim: &str, cx: &mut Context<'_>) {
        if anim.is_empty() {
            return;
        }
        let found = match entity.model.as_deref() {
            Some(model) => !matches!(cx.sequence(model, anim), Lookup::Missing),
            None => true,
        };
        if !found {
            eprintln!(
                "source-engine: server: dynamic prop {}: no sequence named {anim}",
                entity.debug_name()
            );
            // `SetSequence( 0 )` — and **not** `PropSetSequence`, so no think
            // is armed and the playback rate stays where it was. The prop does
            // not move.
            self.sequence.clear();
            return;
        }
        self.prop_set_sequence(entity, anim, cx);
        let me = entity.id();
        entity.fire_output("OnAnimationBegun", Variant::Void, None, Some(me), 0.0, cx);
    }

    /// `CDynamicProp::PropSetSequence` (`props.cpp:2487`) plus
    /// `FinishSetSequence` (`:2472`), which in Portal 2 always run together.
    ///
    /// > **`GotoSequence` is deleted, on a measurement.** It is the sequence
    /// > *transition graph* — `$node`/`$transition` in a QC, the thing that
    /// > walks an NPC from "stand" to "crouch" through an intermediate
    /// > animation — and its very first test is "bail if we're going to or
    /// > from a node 0", which answers "go straight to the goal, forwards,
    /// > from cycle 0". Across the **2,597 sequences of the 606 models the
    /// > game's `prop_dynamic*`s name, not one has a non-zero entry or exit
    /// > node and not one has `nodeflags`** — so that first test is the only
    /// > one that ever runs, `m_iTransitionDirection` is `+1` for every prop
    /// > in the game, and `m_iGoalSequence` is always the sequence that
    /// > immediately starts playing.
    ///
    /// One consequence is worth keeping in hand: `FinishSetSequence` sets the
    /// playback rate from that direction, so **every sequence starts playing
    /// forwards** and the 427 `SetPlaybackRate -1` connections in the game are
    /// what turn one round afterwards. Valve's own `Assert` in `AnimThink`
    /// says that cannot happen.
    fn prop_set_sequence(&mut self, entity: &mut EntityCore, anim: &str, cx: &mut Context<'_>) {
        // `FinishSetSequence`: cycle 0, the clock, the sequence, and
        // `ResetSequenceInfo`'s `m_flPlaybackRate = 1.0`.
        self.sequence.clear();
        self.sequence.push_str(anim);
        self.cycle = 0.0;
        self.anim_time = cx.curtime();
        self.playback_rate = 1.0;
        self.fading = false;

        // `SetThink( &CDynamicProp::AnimThink ); if ( GetNextThink() <= curtime )`
        // — which for an unscheduled entity is `NEVER_THINK`, i.e. `-1`, and
        // so is true.
        if entity.next_think(cx) <= cx.curtime() {
            entity.set_next_think(cx.curtime() + ANIM_THINK_INTERVAL, cx);
        }
    }

    /// `CDynamicProp::AnimThink` (`props.cpp:2290`), reduced to the half that
    /// has anything to do here.
    ///
    /// # The think exists to notice an ending, and stops when there is none
    ///
    /// Valve's version is two jobs in one function: decide whether the
    /// sequence has finished, and then `StudioFrameAdvance` — *accumulate*
    /// `m_flCycle` so that the client has a fresh one to interpolate from.
    /// The second job does not exist here, because the cycle is derived from
    /// [`ModelState`]'s five fields rather than accumulated, so this think is
    /// the first job alone.
    ///
    /// > **That is a divergence with a visible shape: a think with nothing
    /// > left to decide is cancelled rather than re-armed.** A looping
    /// > sequence never ends, a zero-length one never advances, and a sequence
    /// > whose model the port never loaded has no length to measure — in
    /// > Valve all three keep waking the entity ten times a second for the
    /// > rest of the level. Nothing observable changes: `SetAnimation`,
    /// > `SetPlaybackRate` and `Spawn` all re-arm the think themselves, so a
    /// > prop that is given something new to do wakes up for it.
    fn anim_think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let Lookup::Found(info) = self.current_sequence(entity, cx) else {
            // No model, or no such sequence: nothing can finish. This is
            // Valve's `else` branch run once instead of for ever — and the
            // assignment is the load-bearing part of it, because without it a
            // later sequence that *does* finish would find the flag already
            // set and fire no `OnAnimationDone`.
            self.animation_done = false;
            return;
        };
        if info.loops || info.duration <= 0.0 {
            self.animation_done = false;
            return;
        }

        let cycle = self.cycle_now(cx.curtime(), info);
        // `bPropFinished`. The 0.999 is Valve's, and it is what makes an
        // animation that has arrived stay arrived: the derived cycle clamps at
        // 1 and never leaves.
        let forward = self.playback_rate >= 0.0;
        let finished = (forward && cycle >= 0.999) || (!forward && cycle <= 0.0);
        if !finished {
            self.animation_done = false;
            entity.set_next_think(cx.curtime() + ANIM_THINK_INTERVAL, cx);
            return;
        }

        if !self.animation_done {
            self.animation_done = true;
            let me = entity.id();
            entity.fire_output("OnAnimationDone", Variant::Void, None, Some(me), 0.0, cx);
        }

        // "If we're not a random animator, revert to the default animation" —
        // which is what makes a panel that was told to `open` end up in
        // `open_idle` without the map having to say so.
        if !self.default_anim.is_empty() && !self.hold_animation {
            let anim = std::mem::take(&mut self.default_anim);
            self.prop_set_anim(entity, &anim, cx);
            self.default_anim = anim;
        }
        // `m_bHoldAnimation`'s `SetNextThink( curtime + 0.1 )` is not
        // reproduced: it is Valve waiting for "an animation change to come
        // in", and an animation change here arms the think itself.
    }

    /// `SUB_FadeOut` (`baseentity.cpp:8466`) — the think `FadeAndKill` leaves
    /// behind.
    ///
    /// `SUB_AllowedToFade`'s "is the player looking at it" test is
    /// `#if !defined( PORTAL2 )`, and its other half needs a physics object,
    /// so for this game it is unconditionally true and is not written.
    fn fade_think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        // `int speed = MAX( 1, 256 * dt )` with `dt` the frame time capped at
        // 0.1 — on a 64 Hz server that is 4 alpha a tick, so the second the
        // comment promises.
        let dt = cx.time.interval.min(0.1);
        let speed = (FADE_ALPHA_PER_SECOND * dt).max(1.0);
        self.fade_alpha = (self.fade_alpha - speed).max(0.0);
        entity.render_color[3] = self.fade_alpha as u8;
        match self.fade_alpha <= 0.0 {
            true => entity.remove(),
            // `SetNextThink( gpGlobals->curtime )` — which is *this* tick, and
            // therefore runs on the next one: `PhysicsRunSpecificThink`
            // refuses a think scheduled for the tick it is already in.
            false => entity.set_next_think(cx.curtime(), cx),
        }
    }

    /// `CBreakableProp::Break` (`props.cpp:1680`), which is all that is left
    /// of it once the gibs, the explosions, the fire, the game event and the
    /// physics have gone.
    ///
    /// It is reachable only through the `Break` **input** — 8 shipped
    /// connections — because a `prop_dynamic` spawns at `DAMAGE_EVENTS_ONLY`
    /// with zero health and nothing can damage it into breaking. That is also
    /// why the 16 `OnBreak` connections in the game would otherwise be
    /// unreachable.
    fn break_prop(
        &mut self,
        entity: &mut EntityCore,
        breaker: Option<EntityId>,
        cx: &mut Context<'_>,
    ) {
        entity.take_damage = DamageMode::No;
        let me = entity.id();
        entity.fire_output("OnBreak", Variant::Void, breaker, Some(me), 0.0, cx);
        entity.solid_flags |= FSOLID_NOT_SOLID;
        entity.remove();
    }
}

impl Behaviour for DynamicProp {
    fn key_value(&mut self, entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);

        if is("DefaultAnim") {
            self.default_anim = value.to_owned();
            return true;
        }
        if is("HoldAnimation") {
            self.hold_animation = atoi(value) != 0;
            return true;
        }
        if is("StartDisabled") {
            self.start_disabled = atoi(value) != 0;
            return true;
        }
        if is("skin") {
            self.skin = atoi(value);
            return true;
        }
        if is("SetBodyGroup") {
            self.body = atoi(value);
            return true;
        }
        // `CBaseProp::KeyValue`'s one line: **health is swallowed unless this
        // is an override**, and swallowed means "consumed and dropped", not
        // "passed on". So a `prop_dynamic` cannot be given health by a map
        // however hard the map tries.
        if is("health") && !DynamicProp::allows_health(entity) {
            return true;
        }
        // Parsed, kept nowhere, and each one measured in this type's docs:
        // the random animator (dead in Portal 2), the bone followers, the
        // anim-sound suppression, the every-frame flag, the lighting origin,
        // and the four breakable keys a prop that cannot break still carries.
        if is("RandomAnimation")
            || is("MinAnimTime")
            || is("MaxAnimTime")
            || is("AnimateEveryFrame")
            || is("DisableBoneFollowers")
            || is("SuppressAnimSounds")
            || is("LightingOrigin")
            || is("PerformanceMode")
            || is("ExplodeDamage")
            || is("ExplodeRadius")
            || is("PressureDelay")
        {
            return true;
        }
        false
    }

    /// `CDynamicProp::Spawn` (`props.cpp:2001`), with `CBreakableProp::Spawn`
    /// and `CBaseProp::Spawn` under it — in Valve's order, because the first
    /// thing it does happens *before* the base call and the rest happens
    /// after.
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult {
        // The solidity promotion, which is the whole of the `prop_dynamic` /
        // `prop_dynamic_override` difference — see [`is_plain_dynamic`].
        if entity.solid == Solid::None && DynamicProp::is_plain_dynamic(entity) {
            entity.solid = Solid::Obb;
            entity.solid_flags |= FSOLID_NOT_SOLID;
        }

        // --- CBaseProp::Spawn (`props.cpp:206`) ---
        // "prop %s at %.0f %.0f %0.f missing modelname" — and then
        // `UTIL_Remove`. Every one of the game's 8,462 writes a `model`, so
        // this is the hand-made-map path.
        if entity.model.as_deref().unwrap_or("").is_empty() {
            eprintln!(
                "source-engine: server: prop {} at ({:.0} {:.0} {:.0}) has no model",
                entity.class.name, entity.origin.x, entity.origin.y, entity.origin.z
            );
            return SpawnResult::Remove;
        }
        // `SetNextThink( TICK_NEVER_THINK )` is the default here.
        entity.move_type = MoveType::Push;
        self.anim_time = cx.curtime();
        self.playback_rate = 0.0;
        self.cycle = 0.0;

        // --- CBreakableProp::Spawn (`props.cpp:861`) ---
        // `m_iHealth` is zero for every prop in the game — the plain classname
        // cannot be given one and all 344 override keys write 0 — so the
        // breakable branch is unreachable and this is the whole of it.
        entity.health = 0;
        entity.max_health = 1;
        entity.take_damage = DamageMode::EventsOnly;

        // `AddFlag( FL_UNPAINTABLE )` is **not** reproduced, and it does
        // nothing in the shipped game either: `FL_UNPAINTABLE` is `(1<<32)` on
        // a 32-bit `m_fFlags`, under Valve's own
        // `// FIXME[HPE]: this won't actually work - we're out of bits. :(`
        // (`public/const.h:161`). There is no paint system here to read it.
        //
        // `AddFlag( FL_STATICPROP )` is not reproduced either: it is a hint to
        // `C_BaseEntity` about lighting, and it is removed again one statement
        // later for every prop that animates.

        if !self.default_anim.is_empty() {
            let anim = std::mem::take(&mut self.default_anim);
            self.prop_set_anim(entity, &anim, cx);
            self.default_anim = anim;
        }

        // `CreateVPhysics()` — see the type's docs.

        if self.start_disabled {
            entity.effects |= EF_NODRAW;
        }

        // `m_bUseHitboxesForRenderBox = HasSpawnFlags( ... )`, which is a
        // render-bounds hint with no renderer here to take it.
        let _ = SF_DYNAMICPROP_USEHITBOX_FOR_RENDERBOX;
        if entity.spawn_flags & SF_DYNAMICPROP_DISABLE_COLLISION != 0 {
            entity.solid_flags |= FSOLID_NOT_SOLID;
        }
        SpawnResult::Ok
    }

    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        match self.fading {
            true => self.fade_think(entity, cx),
            false => self.anim_think(entity, cx),
        }
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        // `InputSetAnimation` — 5,311 connections, the commonest input aimed
        // at any class this port implements.
        if is("SetAnimation") {
            let anim = input.value.to_string();
            self.prop_set_anim(entity, &anim, cx);
            return true;
        }
        // `InputSetAnimationNoReset` (`props.cpp:2425`). Valve compares
        // sequence *indices*; labels are unique within a model, so comparing
        // them is the same test one step earlier — including for a name the
        // model does not have, where `LookupSequence` gives `-1` and the
        // comparison therefore succeeds in setting it and warning.
        if is("SetAnimationNoReset") {
            let anim = input.value.to_string();
            if !self.sequence.eq_ignore_ascii_case(&anim) {
                self.prop_set_anim(entity, &anim, cx);
            }
            return true;
        }
        if is("SetDefaultAnimation") {
            self.default_anim = input.value.to_string();
            return true;
        }
        // `InputSetPlaybackRate` (`props.cpp:2440`).
        //
        // > **The one divergence in this class that changes a field Valve does
        // > not touch.** Valve accumulates `m_flCycle`, so changing the rate
        // > mid-animation simply changes how fast it grows from where it is.
        // > Here the cycle is derived from `anim_time`, so the *same*
        // > continuity needs the pose to be re-based: take the cycle now,
        // > make it the new starting cycle, and restart the clock. Skip it and
        // > the 427 shipped `SetPlaybackRate -1` connections each snap their
        // > prop to a different frame before running it backwards.
        if is("SetPlaybackRate") {
            let rate = input.value.float();
            if rate != self.playback_rate {
                if let Lookup::Found(info) = self.current_sequence(entity, cx) {
                    self.cycle = self.cycle_now(cx.curtime(), info);
                }
                self.anim_time = cx.curtime();
                self.playback_rate = rate;
                if entity.next_think(cx) <= cx.curtime() {
                    self.fading = false;
                    entity.set_next_think(cx.curtime(), cx);
                }
            }
            return true;
        }
        // `InputTurnOn`/`InputTurnOff`, and Valve's own aliases for them.
        // "NOTE: To avoid risk, currently these do nothing about collisions,
        // only visually on/off" — so a switched-off prop is still in the way,
        // which for this port means nothing yet and will when `.phy`
        // collision lands.
        if is("TurnOn") || is("Enable") {
            entity.effects &= !EF_NODRAW;
            return true;
        }
        if is("TurnOff") || is("Disable") {
            entity.effects |= EF_NODRAW;
            return true;
        }
        if is("EnableCollision") {
            entity.solid_flags &= !FSOLID_NOT_SOLID;
            return true;
        }
        if is("DisableCollision") {
            entity.solid_flags |= FSOLID_NOT_SOLID;
            return true;
        }
        // `InputBecomeRagdoll` — `BecomeRagdollOnClient`, which replaces the
        // prop with a client-side ragdoll. There are no ragdolls here and
        // **no shipped connection fires it**; it is declared so that one would
        // parse as an input rather than as an unknown key.
        if is("BecomeRagdoll") {
            return true;
        }
        // `InputFadeAndKill` → `SUB_StartFadeOutInstant` → `SUB_StartFadeOut( 0, true )`.
        if is("FadeAndKill") {
            self.fading = true;
            self.fade_alpha = 255.0;
            entity.render_color[3] = 255;
            entity.render_mode = 0;
            entity.solid_flags |= FSOLID_NOT_SOLID;
            entity.angular_velocity = Vec3::ZERO;
            entity.set_next_think(cx.curtime(), cx);
            return true;
        }
        if is("Break") {
            self.break_prop(entity, input.activator, cx);
            return true;
        }
        if is("skin") {
            self.skin = input.value.int();
            return true;
        }
        if is("SetBodyGroup") {
            self.body = input.value.int();
            return true;
        }
        false
    }

    fn model_state(&self) -> Option<ModelState<'_>> {
        Some(ModelState {
            sequence: &self.sequence,
            cycle: self.cycle,
            anim_time: self.anim_time,
            playback_rate: self.playback_rate,
            skin: self.skin,
        })
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("sequence", self.sequence.clone()),
            ("default_anim", self.default_anim.clone()),
            ("cycle", format!("{:.3}", self.cycle)),
            ("anim_time", format!("{:.3}", self.anim_time)),
            ("playback_rate", format!("{:.2}", self.playback_rate)),
            ("animation_done", self.animation_done.to_string()),
            ("hold_animation", self.hold_animation.to_string()),
            ("skin", self.skin.to_string()),
            ("body", self.body.to_string()),
            ("fading", self.fading.to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// CPropTestChamberDoor
// ---------------------------------------------------------------------------

/// `TESTCHAMBER_DOOR_MODEL_NAME` (`prop_testchamber_door.cpp:15`).
///
/// Hard-coded in `Spawn` and not a key: **no shipped door writes `model`**,
/// and one that did would be overwritten here as it is there.
const TESTCHAMBER_DOOR_MODEL: &str = "models/props/portal_door_combined.mdl";

/// The one sequence a test chamber door ever plays.
///
/// > **`open` played backwards is how the door shuts** — `close` is never
/// > used. `Spawn` looks up four sequences and caches them in four fields, and
/// > `m_nSequenceOpenIdle`, `m_nSequenceClose` and `m_nSequenceCloseIdle` are
/// > read by nothing in the class or anywhere else in the tree. All the door
/// > does is `SetPlaybackRate( ±1 )` against the sequence `Spawn` reset it to.
/// >
/// > The model agrees that this is deliberate rather than an oversight:
/// > `open` is 23 frames and `close` is **36**, so they are not the same
/// > travel and shutting a door by playing `close` would take 1.46 seconds
/// > where the shipped game takes 0.92.
const OPEN_SEQUENCE: &str = "open";

/// `AnimateThink`'s cadence (`prop_testchamber_door.cpp:379`) — ten times a
/// second, the same as [`ANIM_THINK_INTERVAL`] and from a different file.
const ANIMATE_THINK_INTERVAL: f32 = 0.1;

/// `CPropTestChamberDoor` (`prop_testchamber_door.h:22`,
/// `prop_testchamber_door.cpp:55`) — the big round
/// chamber door, and the second class in this port from
/// `game/server/portal2/`.
///
/// ```text
///    138  prop_testchamber_door    across 71 of the game's 106 maps
/// ```
///
/// **Two of them are on `sp_a1_intro1`**, the map this port loads by default.
/// They carry 247 output connections — `OnFullyClosed` (150), `OnOpen` (66)
/// and `OnFullyOpen` (31), and **not one `OnClose`** — and 296 shipped
/// connections fire an input at one: `Open` (137, one of them spelled
/// `open`), `Close` (130) and `LockOpen` (29). `Lock` and `Unlock` are
/// declared by the class and fired by nothing.
///
/// # It is a `CBaseAnimating`, not a prop
///
/// The classname says `prop_`; the class does not. `CPropTestChamberDoor`
/// derives straight from `CBaseAnimating`, so it has no `DefaultAnim`, no
/// `SetAnimation`, no propdata, no `StartDisabled` and none of
/// [`DynamicProp`]'s inputs — it is five inputs and a playback rate. It lives
/// in this file because what it needs is what this file already has: a model
/// name, a sequence label and the five [`ModelState`] fields.
///
/// # What it does
///
/// `Spawn` resets it to cycle 0 of `open` at **playback rate zero**, which is
/// the shut pose held still. `Open` sets the rate to `+1` and `Close` to
/// `-1`; `Activate` arms a 10 Hz think whose only job is to notice
/// `IsSequenceFinished()` and fire `OnFullyOpen` or `OnFullyClosed`. Both
/// doors are refused while `m_bIsLocked`, and `LockOpen` is `Open` followed
/// by the lock.
///
/// # Deliberately not here, each measured
///
/// - **The area portal window.** Four keys — 84 doors name a
///   `func_areaportalwindow` and 94 write the fade triple — and all
///   `AreaPortalOpen`/`AreaPortalClose` do is write `m_flFadeStartDist` and
///   `m_flFadeDist` on it. `func_areaportalwindow` is part of the visibility
///   system (areas and areaportals, `cmodel.cpp`), which `CLAUDE.md` lists as
///   unported, so there is nothing to write to. The keys are consumed and
///   kept — see [`describe`](TestChamberDoor::describe) — and the two calls
///   are at their sites as comments, so that wiring them up later is one line
///   each.
/// - **Bone followers, and therefore collision.** `CreateVPhysics` turns the
///   model's `bone_followers` block into one physics entity per moving bone
///   and then makes the door itself `FSOLID_NOT_SOLID`; that is `vphysics`,
///   which this port replaces with `rapier` and has not reached. So the door
///   is drawn and **walked through**, exactly as a `prop_floor_button` is.
///   `TestCollision`'s bone-follower loop goes with it.
/// - **The two sounds.** `prop_portal_door.open` and `.close`, which is the
///   whole of what `EmitSound` is asked for. There is no sound system.
/// - **`SetFadeDistance( -1, 0 )` and `SetGlobalFadeScale( 0 )`** — "never let
///   crucial game components fade out". The three fade keys are already read
///   and dropped by
///   [`base_key_value`](crate::server::keyvalue::base_key_value) because there
///   is no per-instance distance fade in `world/`, so the override has nothing
///   to override.
/// - **`UpdateOnRemove`**, which destroys the bone followers there and has
///   none to destroy here.
pub struct TestChamberDoor {
    /// `m_bIsOpen` — where the door is *going*, not where it is. It is set the
    /// instant `Open` is accepted, three quarters of a second before the door
    /// has finished opening, and it is what decides which of the two "fully"
    /// outputs the think fires.
    open: bool,
    /// `m_bIsAnimating` — whether a "fully" output is still owed.
    animating: bool,
    /// `m_bIsLocked`.
    locked: bool,
    /// `m_flCycle` at [`anim_time`](TestChamberDoor::anim_time), 0 to 1 over
    /// `open`.
    cycle: f32,
    /// `m_flAnimTime`.
    anim_time: f32,
    /// `m_flPlaybackRate` — `0` shut or held open, `+1` opening, `-1`
    /// shutting.
    playback_rate: f32,
    /// `m_bSequenceFinished`, and **it is sticky** — see
    /// [`studio_frame_advance`](TestChamberDoor::studio_frame_advance).
    sequence_finished: bool,
    /// `m_strAreaPortalWindowName`, and the fade triple beside it. Parsed,
    /// kept and printed; nothing reads them, because this port has no
    /// `func_areaportalwindow` — see the type's docs.
    area_portal_window: Option<String>,
    use_area_portal_fade: bool,
    area_portal_fade_start: f32,
    area_portal_fade_end: f32,
}

/// The keys (`prop_testchamber_door.cpp:33`).
///
/// All four are the area portal block, and the maps write three of them onto
/// 94 doors and the name onto 84. Two doors write Hammer instance-fixup
/// leftovers into them (`"$FadeStartDistance"`, `"pre_solved_chamber-???"`),
/// which is what a `$`-prefixed instance parameter that was never given a
/// value looks like after compilation; [`atof`] reads those as zero, and so
/// does Valve's.
pub static TESTCHAMBER_DOOR_KEYS: &[&str] = &[
    "AreaPortalWindow",
    "UseAreaPortalFade",
    "AreaPortalFadeStart",
    "AreaPortalFadeEnd",
];

/// The inputs (`prop_testchamber_door.cpp:40`), all five and all `FIELD_VOID`.
///
/// `CBaseAnimating`'s `skin` and `SetBodyGroup` are **not** declared, on a
/// measurement: no connection in any of the 106 maps fires either at a door,
/// and the class has no skin families to choose between anyway.
pub static TESTCHAMBER_DOOR_INPUTS: InputDefs = &[
    InputDef::new("Open", FieldType::Void),
    InputDef::new("Close", FieldType::Void),
    InputDef::new("Lock", FieldType::Void),
    InputDef::new("LockOpen", FieldType::Void),
    InputDef::new("Unlock", FieldType::Void),
];

/// The outputs (`prop_testchamber_door.cpp:46`).
///
/// **`OnClose` is declared, fired and connected to nothing**: all 247 output
/// connections on the game's 138 doors are one of the other three. It is here
/// for the reason `BecomeRagdoll` is on [`DynamicProp`] — so that a map which
/// wrote one would parse it as an output rather than count it as a key nobody
/// understood.
pub static TESTCHAMBER_DOOR_OUTPUTS: &[&str] =
    &["OnOpen", "OnClose", "OnFullyOpen", "OnFullyClosed"];

impl TestChamberDoor {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(TestChamberDoor {
            // The constructor's three initialisers (`:60`).
            open: false,
            animating: false,
            locked: false,
            cycle: 0.0,
            anim_time: 0.0,
            playback_rate: 0.0,
            sequence_finished: false,
            area_portal_window: None,
            use_area_portal_fade: false,
            area_portal_fade_start: 0.0,
            area_portal_fade_end: 0.0,
        })
    }

    /// What the sequence table says about `open`, for this door's model.
    fn current_sequence(&self, entity: &EntityCore, cx: &Context<'_>) -> Lookup {
        match entity.model.as_deref() {
            Some(model) => cx.sequence(model, OPEN_SEQUENCE),
            None => Lookup::Unknown,
        }
    }

    /// The cycle `StudioFrameAdvanceInternal` would have arrived at, **before
    /// it clamps** — which is the number the finished test is made against.
    fn raw_cycle(&self, now: f32, info: SequenceInfo) -> f32 {
        self.cycle + (now - self.anim_time).max(0.0) * self.playback_rate * info.cycle_rate()
    }

    /// The pose, clamped as `SetCycle` leaves it and as the renderer will show
    /// it.
    ///
    /// Must agree with `engine::world::entities`' `cycle` — see
    /// [`ModelState`].
    fn cycle_now(&self, now: f32, info: SequenceInfo) -> f32 {
        let cycle = self.raw_cycle(now, info);
        match info.loops {
            true => cycle.rem_euclid(1.0),
            false => cycle.clamp(0.0, 1.0),
        }
    }

    /// `SetPlaybackRate`, **re-based** — the same divergence
    /// [`DynamicProp`]'s `SetPlaybackRate` input records, and here it is not
    /// optional.
    ///
    /// Valve accumulates `m_flCycle`, so changing the rate simply changes how
    /// fast it grows from where it already is. This port derives the cycle
    /// from `(cycle, anim_time, rate)`, so the *same* continuity needs the
    /// pose pinned first: take the cycle now, make it the new starting cycle,
    /// and restart the clock. Skip it and a door told to `Close` would
    /// compute its position from the moment it spawned.
    fn set_playback_rate(&mut self, rate: f32, entity: &EntityCore, cx: &Context<'_>) {
        if let Lookup::Found(info) = self.current_sequence(entity, cx) {
            self.cycle = self.cycle_now(cx.curtime(), info);
        }
        self.anim_time = cx.curtime();
        self.playback_rate = rate;
    }

    /// `CPropTestChamberDoor::Open` (`:163`).
    fn open_door(
        &mut self,
        entity: &mut EntityCore,
        activator: Option<EntityId>,
        cx: &mut Context<'_>,
    ) {
        // "Don't fire the output if the door is already opened or locked".
        if self.open || self.locked {
            return;
        }
        // "Play the open animation forwards".
        self.set_playback_rate(1.0, entity, cx);
        self.open = true;
        self.animating = true;

        // `OnOpen` (`:385`), minus `EmitSound( "prop_portal_door.open" )` and
        // `AreaPortalOpen()` — see the type's docs for both.
        let me = entity.id();
        entity.fire_output("OnOpen", Variant::Void, activator, Some(me), 0.0, cx);
    }

    /// `CPropTestChamberDoor::Close` (`:197`).
    fn close_door(
        &mut self,
        entity: &mut EntityCore,
        activator: Option<EntityId>,
        cx: &mut Context<'_>,
    ) {
        if !self.open || self.locked {
            return;
        }
        // "Play the open animation backwards".
        self.set_playback_rate(-1.0, entity, cx);
        self.open = false;
        self.animating = true;

        // `OnClose` (`:399`), minus `EmitSound( "prop_portal_door.close" )`.
        // **No `AreaPortalClose` here** — that is on `OnFullyClosed`, so the
        // window stays open for as long as the door is visibly shutting.
        let me = entity.id();
        entity.fire_output("OnClose", Variant::Void, activator, Some(me), 0.0, cx);
    }

    /// The half of `StudioFrameAdvanceInternal` (`baseanimating.cpp:468`) that
    /// this port has anything to do: **when does `m_bSequenceFinished` go
    /// true?**
    ///
    /// The other half — writing the advanced cycle back — does not exist here,
    /// because the cycle is derived from [`ModelState`]'s fields rather than
    /// accumulated.
    ///
    /// Valve's two branches both set the same flag, so they are one test:
    /// the cycle ran off either end, or it passed
    /// [`last_visible_cycle`](SequenceInfo::last_visible_cycle).
    ///
    /// > **The flag is sticky, and that is the single most surprising thing
    /// > about this class.** Nothing clears `m_bSequenceFinished` except
    /// > `ResetSequenceInfo`, and `CPropTestChamberDoor` calls `ResetSequence`
    /// > exactly once, in `Spawn`. So only a door's **first** open reports its
    /// > own completion honestly, at 0.72 seconds; from then on the flag is
    /// > already true and **every later `OnFullyOpen` and every
    /// > `OnFullyClosed` fires on the first think after the input**, about a
    /// > tenth of a second in, while the door is still visibly moving.
    /// >
    /// > That is the shipped game's behaviour and it is reproduced
    /// > deliberately, because 150 connections hang off `OnFullyClosed` and
    /// > they were authored against it — `door_physics_clip Disable` on 29 of
    /// > them, a fizzler on 25 more. Making the door "correct" would delay
    /// > every one of them by three quarters of a second.
    /// >
    /// > It is also why `Close` needs no `last_visible_cycle` of its own:
    /// > played backwards the threshold is above 1 and unreachable, and the
    /// > `cycle < 0` branch that would fire at the true end of the travel is
    /// > never the one that gets there first.
    fn studio_frame_advance(&mut self, entity: &EntityCore, cx: &Context<'_>) {
        // No model, or no `open` sequence: nothing can be decided, and Valve's
        // `StudioFrameAdvance` returns at `!pStudioHdr` too. See
        // `server::sequences` for why `Unknown` is not `Missing`.
        let Lookup::Found(info) = self.current_sequence(entity, cx) else {
            return;
        };
        let raw = self.raw_cycle(cx.curtime(), info);
        if raw < 0.0 || raw >= 1.0 || raw > info.last_visible_cycle(self.playback_rate) {
            self.sequence_finished = true;
        }
    }

    /// `CPropTestChamberDoor::AnimateThink` (`:354`).
    ///
    /// `DispatchAnimEvents` and `UpdateBoneFollowers` are the two lines
    /// between the advance and the test in the original; neither has anything
    /// here to drive.
    ///
    /// > **The think re-arms unconditionally, which is Valve's, and this class
    /// > deliberately does not take [`DynamicProp::anim_think`]'s divergence
    /// > of cancelling itself when there is nothing left to decide.** The
    /// > reason that divergence was worth it there was 8,462 entities;
    /// > there are 138 doors in the whole game, two per map. What re-arming
    /// > buys is the exact 0.1-second grid, and the grid is what decides when
    /// > `OnFullyOpen` and `OnFullyClosed` fire — cancelling and re-arming on
    /// > the next `Open` would shift every one of those 181 connections by up
    /// > to a tenth of a second.
    fn animate_think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.studio_frame_advance(entity, cx);

        if self.animating && self.sequence_finished {
            let me = entity.id();
            match self.open {
                // `OnFullyOpened` (`:411`).
                true => entity.fire_output("OnFullyOpen", Variant::Void, None, Some(me), 0.0, cx),
                // `OnFullyClosed` (`:419`), minus its `AreaPortalClose()`.
                false => {
                    entity.fire_output("OnFullyClosed", Variant::Void, None, Some(me), 0.0, cx)
                }
            }
            self.animating = false;
        }

        entity.set_next_think(cx.curtime() + ANIMATE_THINK_INTERVAL, cx);
    }

    /// Whether the door is open or opening. `IsOpen()` (`:181`); `IsClosed()`
    /// is its negation and is written nowhere but in `Close`.
    ///
    /// The three below are read by the tests and by nothing else, the way
    /// [`ClassDef::keys`](crate::server::class::ClassDef::keys) is — the
    /// class's own state reaches `ent_dump` through
    /// [`describe`](TestChamberDoor::describe) and reaches the renderer
    /// through [`model_state`](TestChamberDoor::model_state), so there is no
    /// third caller to have.
    #[allow(dead_code)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    #[allow(dead_code)]
    pub fn is_animating(&self) -> bool {
        self.animating
    }

    #[allow(dead_code)]
    pub fn is_locked(&self) -> bool {
        self.locked
    }
}

impl Behaviour for TestChamberDoor {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);

        if is("AreaPortalWindow") {
            self.area_portal_window = Some(value.to_owned());
            return true;
        }
        if is("UseAreaPortalFade") {
            self.use_area_portal_fade = atoi(value) != 0;
            return true;
        }
        if is("AreaPortalFadeStart") {
            self.area_portal_fade_start = atof(value);
            return true;
        }
        if is("AreaPortalFadeEnd") {
            self.area_portal_fade_end = atof(value);
            return true;
        }
        false
    }

    /// `CPropTestChamberDoor::Spawn` (`:83`), in Valve's order.
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult {
        entity.move_type = MoveType::None;
        // `SetSolid( SOLID_VPHYSICS )`. Nothing collides with it here —
        // `CreateVPhysics`'s bone followers are absent and `World::clip_models`
        // only ever sees `"*N"` brush models — so the door is a wall in the
        // shipped game and is not one yet in this port.
        entity.solid = Solid::VPhysics;
        entity.effects |= effects::NOSHADOW;
        entity.model = Some(TESTCHAMBER_DOOR_MODEL.to_owned());

        // `CreateVPhysics()` — see the type's docs.

        // The four `LookupSequence` calls have no counterpart: [`ModelState`]
        // carries a *label*, so there is no index to cache and — since the
        // models are not read until after `level_init` — nothing here to ask.
        // Three of the four are dead in the original anyway; see
        // [`OPEN_SEQUENCE`].

        // `ResetSequence( m_nSequenceOpen )` — cycle 0, the clock, and
        // `ResetSequenceInfo`'s `m_bSequenceFinished = false`. Its
        // `m_flPlaybackRate = 1.0` is overwritten one line later by
        // `SetPlaybackRate( 0.0f )`, which is what makes a door spawn **shut
        // and still** rather than opening itself.
        self.cycle = 0.0;
        self.anim_time = cx.curtime();
        self.sequence_finished = false;
        self.playback_rate = 0.0;

        // The `AreaPortalWindow` lookup and its `AreaPortalClose()` go here.

        entity.effects |= effects::MARKED_FOR_FAST_REFLECTION;

        // `SetFadeDistance( -1.0f, 0.0f )` / `SetGlobalFadeScale( 0.0f )` —
        // see the type's docs.
        SpawnResult::Ok
    }

    /// `CPropTestChamberDoor::Activate` (`:133`) — "start our animation
    /// cycle".
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        entity.set_next_think(cx.curtime() + ANIMATE_THINK_INTERVAL, cx);
    }

    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.animate_think(entity, cx);
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        // `InputOpen` (`:155`). 137 shipped connections, and **one of them
        // spells it `open`** — which works because every name comparison in
        // the original is `stricmp`.
        if is("Open") {
            self.open_door(entity, input.activator, cx);
            return true;
        }
        if is("Close") {
            self.close_door(entity, input.activator, cx);
            return true;
        }
        // `InputLock` / `InputUnlock` (`:223`, `:241`). Declared by the class
        // and fired by no shipped connection; `LockOpen` is what the maps use.
        if is("Lock") {
            self.locked = true;
            return true;
        }
        // `InputLockOpen` (`:231`) — open it *and then* lock it, in that
        // order, so the open is the one input that gets through. 29 shipped
        // connections.
        if is("LockOpen") {
            self.open_door(entity, input.activator, cx);
            self.locked = true;
            return true;
        }
        if is("Unlock") {
            self.locked = false;
            return true;
        }
        false
    }

    /// The pose. The sequence is always `open`; what changes is the rate.
    fn model_state(&self) -> Option<ModelState<'_>> {
        Some(ModelState {
            sequence: OPEN_SEQUENCE,
            cycle: self.cycle,
            anim_time: self.anim_time,
            playback_rate: self.playback_rate,
            // `m_nSkin` is `CBaseAnimating`'s and this class never writes it.
            skin: 0,
        })
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("open", self.open.to_string()),
            ("animating", self.animating.to_string()),
            ("locked", self.locked.to_string()),
            ("cycle", format!("{:.3}", self.cycle)),
            ("anim_time", format!("{:.3}", self.anim_time)),
            ("playback_rate", format!("{:.2}", self.playback_rate)),
            ("sequence_finished", self.sequence_finished.to_string()),
            (
                "area_portal_window",
                self.area_portal_window.clone().unwrap_or_default(),
            ),
            (
                "area_portal_fade",
                match self.use_area_portal_fade {
                    true => format!(
                        "{:.0}..{:.0}",
                        self.area_portal_fade_start, self.area_portal_fade_end
                    ),
                    false => "off".to_owned(),
                },
            ),
        ]
    }
}
