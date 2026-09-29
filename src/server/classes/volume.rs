//! The three brush volumes the portal gun asks about —
//! `func_noportal_volume`, `func_portal_bumper` and
//! `trigger_portal_cleanser`.
//!
//! ```text
//!   2,383  func_portal_bumper        a soft wall a portal is slid off once
//!     458  func_noportal_volume      no portal may overlap it
//!     371  trigger_portal_cleanser   the fizzler: a shot stops at it
//! ```
//!
//! **None of the three has its source in this tree** —
//! `func_noportal_volume.cpp`, `func_portal_bumper.cpp` and
//! `trigger_portal_cleanser.cpp` are all named by
//! `server_portal_base.vpc` and none ships. What *is* here is every line
//! that reads them: `portal_placement.cpp`'s `TraceBumpingEntities` and
//! `IsPortalIntersectingNoPortalVolume`, and `weapon_portalgun_shared.cpp`'s
//! `PortalTraceClippedByBlockers`. Between them those ask exactly two
//! questions of each — is it on (`IsActive()`, `IsEnabled()`), and where are
//! its brushes — so that is what this class is, and the input and key names
//! are the ones the shipped maps write:
//!
//! | | on/off keys | inputs the maps fire |
//! |---|---|---|
//! | `func_noportal_volume` | spawnflag 1 (20 of 458) | `Activate` 14, `Deactivate` 8 |
//! | `func_portal_bumper` | spawnflag 1 (1 of 2,383) | `Activate` 1 |
//! | `trigger_portal_cleanser` | `StartDisabled` (58 of 371) | `Enable` 66, `Disable` 112 |
//!
//! # The fizzler draws itself
//!
//! **All 1,174 of the game's `effects/fizzler*` brush faces belong to
//! `trigger_portal_cleanser` models**, and all 284 of those entities write
//! `Visible 1`. So unlike every other trigger — which `InitTrigger` makes
//! `EF_NODRAW` — a visible cleanser shows its own brushes, and a disabled one
//! hides them: switching a fizzler off in the shipped game makes the field
//! vanish. Before this class existed the field drew because *nobody* owned
//! the model, which also meant a switched-off fizzler kept drawing.
//!
//! # The cleanser's touch
//!
//! `CTriggerPortalCleanser` is a `CBaseTrigger` — its spawnflags are the
//! trigger filter bits (4105, clients and physics objects, on 117 of the 371;
//! 4104, physics objects only, on 47; 4097, clients only, on 36) and three
//! name a `filtername` — so it holds a [`BaseTrigger`] and passes its filters
//! before anything else. Its source is not in this tree; what it does is
//! reconstructed from the FGD (`bin/portal.fgd:111`, *"disolves any entities
//! that touch it and fizzles active portals when the player touches it"*), the
//! two places the tree calls into it (`CPropWeightedCube::InputDissolve` →
//! `FizzleBaseAnimating`) and the maps' own wiring:
//!
//! - **the player** touching an enabled cleanser loses the portals their gun
//!   has placed, and the cleanser fires `OnFizzle` if any went;
//! - **a cube** touching one is dissolved — its own `OnFizzled`, then the
//!   cleanser's `OnDissolve` (22 connections) and, for a cube named `Box`,
//!   `OnDissolveBox` (none);
//! - **`FizzleTouchingPortals`** (8 connections) fizzles every portal inside
//!   the volume.
//!
//! All three reach past the cleanser — the gun, the carry, the cube's class —
//! so each is asked for through [`Context::cleanse`] and done by the server.
//! What is still absent is the *look*: the cube's fizzle is instant, with no
//! float, no fade and no particles.

use crate::server::class::{Behaviour, Cleanse, Context, InputDef, InputDefs, SpawnResult};
use crate::server::classes::trigger::BaseTrigger;
use crate::server::entity::{EntityCore, EntityId};
use crate::server::io::{FieldType, Input};
use crate::server::keyvalue::{atof, atoi};
use crate::server::movement::{Solid, EF_NODRAW, FSOLID_NOT_SOLID};
use crate::server::classes::prop::WeightedCube;
use crate::server::placement::BumperKind;

/// `SF_START_INACTIVE` on a no-portal volume or a bumper.
const SF_START_INACTIVE: u32 = 1;

/// A brush volume placement consults. See the module docs.
#[derive(Debug)]
pub struct PortalVolume {
    pub kind: BumperKind,
    /// `IsActive()` / `IsEnabled()`.
    pub active: bool,
    /// The cleanser's `Visible` key — whether its brushes are the field you
    /// see. Meaningless for the other two, which never draw.
    visible: bool,
    /// The cleanser's `CBaseTrigger` — its filters, its touch list, its
    /// `Enable`/`Disable`. Unused by the other two.
    trigger: BaseTrigger,
}

/// `Activate`/`Deactivate`/`Toggle` — the no-portal volume's and the bumper's.
pub static VOLUME_INPUTS: InputDefs = &[
    InputDef::new("Activate", FieldType::Void),
    InputDef::new("Deactivate", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
];

/// The cleanser's inputs: its own and `CBaseTrigger`'s.
pub static CLEANSER_INPUTS: InputDefs = &[
    InputDef::new("FizzleTouchingPortals", FieldType::Void),
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("TouchTest", FieldType::Void),
    InputDef::new("StartTouch", FieldType::Void),
    InputDef::new("EndTouch", FieldType::Void),
];

/// The cleanser's keys, as the maps write them. `UseScanline` (312) is a
/// look and is read by nothing here.
pub static CLEANSER_KEYS: &[&str] = &["StartDisabled", "Visible", "UseScanline", "filtername"];

/// The cleanser's outputs, declared so that the shipped connections parse as
/// outputs. None can fire; see the module docs.
pub static CLEANSER_OUTPUTS: &[&str] = &[
    "OnDissolve",
    "OnFizzle",
    "OnDissolveBox",
    "OnStartTouch",
    "OnStartTouchAll",
    "OnEndTouch",
    "OnEndTouchAll",
    "OnTouching",
    "OnNotTouching",
    "OnTrigger",
];

impl PortalVolume {
    pub fn create_no_portal_volume() -> Box<dyn Behaviour> {
        Box::new(PortalVolume {
            kind: BumperKind::NoPortalVolume,
            active: true,
            visible: false,
            trigger: BaseTrigger::default(),
        })
    }

    pub fn create_bumper() -> Box<dyn Behaviour> {
        Box::new(PortalVolume {
            kind: BumperKind::Bumper,
            active: true,
            visible: false,
            trigger: BaseTrigger::default(),
        })
    }

    pub fn create_cleanser() -> Box<dyn Behaviour> {
        Box::new(PortalVolume {
            kind: BumperKind::Cleanser,
            active: true,
            visible: false,
            trigger: BaseTrigger::default(),
        })
    }

    /// Switches a no-portal volume or a bumper — `IsActive()`.
    fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// A cleanser's state after anything changed its trigger: placement's
    /// `IsEnabled()` is `!m_bDisabled`, and a visible cleanser shows its
    /// field exactly while it is enabled.
    fn sync_cleanser(&mut self, entity: &mut EntityCore) {
        self.active = !self.trigger.is_disabled();
        if self.visible {
            match self.active {
                true => entity.effects &= !EF_NODRAW,
                false => entity.effects |= EF_NODRAW,
            }
        }
    }

    /// The cleanser's own `Touch`: filters, then who it is.
    fn cleanse(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        if self.trigger.is_disabled() || !self.trigger.passes_trigger_filters(entity, other, cx) {
            return;
        }
        let cleanser = entity.id();
        if cx.player() == Some(other) {
            cx.cleanse(Cleanse::Player { cleanser, player: other });
            return;
        }
        let dissolvable = cx
            .entity(other)
            .is_some_and(|e| !e.core.removed && e.behaviour.downcast_ref::<WeightedCube>().is_some());
        if dissolvable {
            cx.cleanse(Cleanse::Prop { cleanser, prop: other });
        }
    }
}

impl Behaviour for PortalVolume {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if self.kind != BumperKind::Cleanser {
            return false;
        }
        if self.trigger.key_value(key, value) {
            self.active = !self.trigger.is_disabled();
            return true;
        }
        if key.eq_ignore_ascii_case("Visible") {
            self.visible = atoi(value) != 0;
            return true;
        }
        key.eq_ignore_ascii_case("UseScanline")
    }

    /// Non-solid in every case, and invisible unless it is a visible, enabled
    /// fizzler.
    ///
    /// The volume and the bumper are `SOLID_VPHYSICS` and `FSOLID_NOT_SOLID`,
    /// kept for their bounds and never collided with; the cleanser is
    /// `InitTrigger`'s, `FSOLID_TRIGGER` only while it is enabled.
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        match self.kind {
            BumperKind::Cleanser => {
                self.trigger.spawn(entity);
                self.trigger.init_trigger(entity);
                self.sync_cleanser(entity);
            }
            BumperKind::NoPortalVolume | BumperKind::Bumper => {
                self.active = !entity.has_spawn_flags(SF_START_INACTIVE);
                entity.solid = Solid::VPhysics;
                entity.solid_flags |= FSOLID_NOT_SOLID;
                entity.effects |= EF_NODRAW;
            }
        }
        SpawnResult::Ok
    }

    fn activate(&mut self, _entity: &mut EntityCore, cx: &mut Context<'_>) {
        if self.kind == BumperKind::Cleanser {
            self.trigger.activate(cx);
        }
    }

    fn start_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        if self.kind == BumperKind::Cleanser {
            self.trigger.start_touch(entity, other, cx);
        }
    }

    fn touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        if self.kind == BumperKind::Cleanser {
            self.cleanse(entity, other, cx);
        }
    }

    fn end_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        if self.kind == BumperKind::Cleanser {
            self.trigger.end_touch(entity, other, cx);
        }
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        if self.kind == BumperKind::Cleanser {
            if input.name.eq_ignore_ascii_case("FizzleTouchingPortals") {
                cx.cleanse(Cleanse::TouchingPortals { cleanser: entity.id() });
                return true;
            }
            let handled = self.trigger.accept_input(entity, input, cx);
            self.sync_cleanser(entity);
            return handled;
        }
        if input.name.eq_ignore_ascii_case("Activate") {
            self.set_active(true);
            return true;
        }
        if input.name.eq_ignore_ascii_case("Deactivate") {
            self.set_active(false);
            return true;
        }
        if input.name.eq_ignore_ascii_case("Toggle") {
            let active = !self.active;
            self.set_active(active);
            return true;
        }
        false
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![("active", self.active.to_string())];
        if self.kind == BumperKind::Cleanser {
            out.extend(self.trigger.describe());
        }
        out
    }
}

/// `info_placement_helper` — a point a portal shot near it snaps to.
///
/// ```text
///   392 placed; radius on all of them, snap_to_helper_angles on 160,
///   force_placement on 229, StartDisabled on 27
/// ```
///
/// **Its source is not in this tree either** (`info_placement_helper.cpp`),
/// nor is `UTIL_FindPlacementHelper`, the search the gun runs. What is here is
/// the gun's half, `CWeaponPortalgun::AttemptSnapToPlacementHelper`
/// (`weapon_portalgun_shared.cpp:1625`), and it reads four things off a
/// helper: its origin, `GetTargetRadius()`, `ShouldUseHelperAngles()` and
/// `GetTargetAngles()` — the `radius` and `snap_to_helper_angles` keys and the
/// entity's own angles. The search is reconstructed as the nearest enabled
/// helper whose radius reaches the point the shot hit, which is also the
/// test the gun then repeats itself against the re-traced surface; see
/// [`crate::server::portalgun`].
///
/// `force_placement`, `hide_until_placed`, `usesizelimit` and `target_size`
/// are read and kept: nothing in this tree's gun reads them.
#[derive(Debug)]
pub struct PlacementHelper {
    /// `GetTargetRadius()` — `radius`.
    pub radius: f32,
    /// `ShouldUseHelperAngles()` — `snap_to_helper_angles`.
    pub use_angles: bool,
    /// `!StartDisabled`, then `Enable`/`Disable`.
    pub enabled: bool,
    force_placement: bool,
    hide_until_placed: bool,
}

pub static HELPER_KEYS: &[&str] = &[
    "radius",
    "snap_to_helper_angles",
    "force_placement",
    "hide_until_placed",
    "usesizelimit",
    "target_size",
    "StartDisabled",
];

pub static HELPER_INPUTS: InputDefs = &[
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
];

impl PlacementHelper {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(PlacementHelper {
            radius: 0.0,
            use_angles: false,
            enabled: true,
            force_placement: false,
            hide_until_placed: false,
        })
    }
}

impl Behaviour for PlacementHelper {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        match key.to_ascii_lowercase().as_str() {
            "radius" => self.radius = atof(value),
            "snap_to_helper_angles" => self.use_angles = atoi(value) != 0,
            "force_placement" => self.force_placement = atoi(value) != 0,
            "hide_until_placed" => self.hide_until_placed = atoi(value) != 0,
            "startdisabled" => self.enabled = atoi(value) == 0,
            "usesizelimit" | "target_size" => {}
            _ => return false,
        }
        true
    }

    fn accept_input(
        &mut self,
        _entity: &mut EntityCore,
        input: &Input<'_>,
        _cx: &mut Context<'_>,
    ) -> bool {
        if input.name.eq_ignore_ascii_case("Enable") {
            self.enabled = true;
            return true;
        }
        if input.name.eq_ignore_ascii_case("Disable") {
            self.enabled = false;
            return true;
        }
        false
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("radius", self.radius.to_string()),
            ("snap_to_helper_angles", self.use_angles.to_string()),
            ("enabled", self.enabled.to_string()),
            ("force_placement", self.force_placement.to_string()),
            ("hide_until_placed", self.hide_until_placed.to_string()),
        ]
    }
}
