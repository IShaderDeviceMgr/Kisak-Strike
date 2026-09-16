//! The prop family — model entities that are *in* the puzzle rather than
//! decorating it.
//!
//! `game/server/portal2/prop_floor_button.cpp`, which is the first file in
//! `game/server/portal2/` this port has reached and the first class here that
//! is Portal 2's rather than Source's.
//!
//! ```text
//!     65  prop_floor_button        CPropFloorButton : CDynamicProp
//!     65  trigger_portal_button    CPortalButtonTrigger : CBaseTrigger
//! ```
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
//! Still absent from `CDynamicProp`, and none of it observable: bone
//! followers, `VPhysicsInitStatic`, prop data, LOS blocking, fade distances,
//! and `m_nSkin` — which is parsed, kept and printed by `ent_dump`, but not
//! drawn, because a model's skin families are `portdocs/STUDIO.md` stage 6's.
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
use crate::server::entity::{EntityCore, EntityId};
use crate::server::io::{FieldType, Input, Variant};
use crate::server::keyvalue::{atoi, effects};
use crate::server::movement::{
    ModelBounds, MoveType, Solid, FSOLID_NOT_SOLID, FSOLID_TRIGGER, FL_CLIENT,
};

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
            core.origin = origin;
            core.angles = angles;
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
    fn press(&mut self, entity: &mut EntityCore, activator: Option<EntityId>, cx: &mut Context<'_>) {
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
    fn model_state(&self) -> Option<ModelState> {
        Some(ModelState {
            sequence: self.sequence,
            anim_time: self.anim_time,
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
