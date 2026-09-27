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
//! # What is not here
//!
//! The cleanser's own behaviour — fizzling the player's portals when they walk
//! through it, dissolving a cube, `FizzleTouchingPortals` (8 connections),
//! `OnDissolve` (22) and the rest of its outputs — is `CTriggerPortalCleanser`,
//! whose source is not in this tree. What is here is the fizzler as the
//! *portal gun* sees it: a shot stops at an enabled one.

use crate::server::class::{Behaviour, Context, InputDef, InputDefs, SpawnResult};
use crate::server::entity::EntityCore;
use crate::server::io::{FieldType, Input};
use crate::server::keyvalue::{atof, atoi};
use crate::server::movement::{Solid, EF_NODRAW, FSOLID_NOT_SOLID, FSOLID_TRIGGER};
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
}

/// `Activate`/`Deactivate`/`Toggle` — the no-portal volume's and the bumper's.
pub static VOLUME_INPUTS: InputDefs = &[
    InputDef::new("Activate", FieldType::Void),
    InputDef::new("Deactivate", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
];

/// `Enable`/`Disable`/`Toggle` — `CBaseTrigger`'s, which the cleanser has.
pub static CLEANSER_INPUTS: InputDefs = &[
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
];

/// The cleanser's keys, as the maps write them. `UseScanline` (312) is a
/// look and is read by nothing here.
pub static CLEANSER_KEYS: &[&str] = &["StartDisabled", "Visible", "UseScanline"];

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
        })
    }

    pub fn create_bumper() -> Box<dyn Behaviour> {
        Box::new(PortalVolume {
            kind: BumperKind::Bumper,
            active: true,
            visible: false,
        })
    }

    pub fn create_cleanser() -> Box<dyn Behaviour> {
        Box::new(PortalVolume {
            kind: BumperKind::Cleanser,
            active: true,
            visible: false,
        })
    }

    /// Switches it. For a cleanser that is also `CBaseTrigger::Enable`/`Disable`
    /// — a disabled trigger is not a trigger at all, `FSOLID_TRIGGER` comes
    /// off — and, for a visible one, showing or hiding the field.
    fn set_active(&mut self, entity: &mut EntityCore, active: bool) {
        self.active = active;
        if self.kind != BumperKind::Cleanser {
            return;
        }
        match active {
            true => entity.solid_flags |= FSOLID_TRIGGER,
            false => entity.solid_flags &= !FSOLID_TRIGGER,
        }
        if self.visible {
            match active {
                true => entity.effects &= !EF_NODRAW,
                false => entity.effects |= EF_NODRAW,
            }
        }
    }
}

impl Behaviour for PortalVolume {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if self.kind != BumperKind::Cleanser {
            return false;
        }
        if key.eq_ignore_ascii_case("StartDisabled") {
            self.active = atoi(value) == 0;
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
                // `InitTrigger` (`triggers.cpp:327`): `SOLID_VPHYSICS` under a
                // parent, `SOLID_BSP` otherwise, never solid, and a trigger
                // only while enabled.
                entity.solid = match entity.parent() {
                    Some(_) => Solid::VPhysics,
                    None => Solid::Bsp,
                };
                entity.solid_flags |= FSOLID_NOT_SOLID;
                entity.effects |= EF_NODRAW;
                let active = self.active;
                self.set_active(entity, active);
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

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        _cx: &mut Context<'_>,
    ) -> bool {
        let (on, off) = match self.kind {
            BumperKind::Cleanser => ("Enable", "Disable"),
            _ => ("Activate", "Deactivate"),
        };
        if input.name.eq_ignore_ascii_case(on) {
            self.set_active(entity, true);
            return true;
        }
        if input.name.eq_ignore_ascii_case(off) {
            self.set_active(entity, false);
            return true;
        }
        if input.name.eq_ignore_ascii_case("Toggle") {
            let active = !self.active;
            self.set_active(entity, active);
            return true;
        }
        false
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![("active", self.active.to_string())]
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
