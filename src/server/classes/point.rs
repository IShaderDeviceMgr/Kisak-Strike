//! Point entities that act on somebody else: `point_teleport`.
//!
//! `game/server/pointteleport.cpp` (`CPointTeleport`, 213 lines) — **128
//! entities across the shipped maps**, and the class that makes stage 4's
//! player entity pay for itself twice over: **121 of the 128 target the
//! literal string `!player`**, and 84 connections fire `Teleport` at one.
//! Before the player was in the entity list every one of those reached
//! nothing.
//!
//! `info_target`, `info_player_start` and the other point entities that do
//! nothing are [`PointEntity`](crate::server::class::PointEntity) and live in
//! the class table directly; this file is for the ones with behaviour.

use crate::server::class::{Behaviour, Context, InputDef, InputDefs};
use crate::server::entity::EntityCore;
use crate::server::io::{FieldType, Input};
use crate::server::touch::{self, Teleport};
use glam::Vec3;

/// `SF_TELEPORT_TO_SPAWN_POS` (`pointteleport.cpp:22`) — send the target back
/// to where the map put it rather than to where this entity is. **Zero of the
/// game's 128 set it**; all 113 that carry a `spawnflags` key carry `0`.
const SF_TELEPORT_TO_SPAWN_POS: u32 = 0x1;

/// `CPointTeleport` (`pointteleport.cpp:25`).
#[derive(Default)]
pub struct PointTeleport {
    /// `m_vSaveOrigin`/`m_vSaveAngles` — where `Teleport` sends things.
    ///
    /// Captured at `Activate`, **not** read live, which is the whole reason
    /// `TeleportToCurrentPos` exists as a second input: a `point_teleport`
    /// parented to a moving elevator (39 of the game's 128 are) remembers
    /// where the elevator was at map spawn.
    save_origin: Vec3,
    save_angles: Vec3,
}

pub static POINT_TELEPORT_INPUTS: InputDefs = &[
    InputDef::new("Teleport", FieldType::Void),
    InputDef::new("TeleportToCurrentPos", FieldType::Void),
    InputDef::new("TeleportEntity", FieldType::String),
];

impl PointTeleport {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<PointTeleport>::default()
    }

    /// `CPointTeleport::DoTeleport` (`pointteleport.cpp:159`).
    ///
    /// `override_target` is `TeleportEntity`'s: the name comes from the
    /// connection's parameter instead of from the `target` key.
    fn do_teleport(
        &self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        origin: Vec3,
        angles: Vec3,
        override_target: bool,
        cx: &mut Context<'_>,
    ) {
        let name = match override_target {
            true => input.value.to_string(),
            false => entity.target.clone().unwrap_or_default(),
        };
        let me = entity.id();
        let Some(target) = cx.find_target(&name, Some(me), input.activator, input.caller) else {
            return;
        };

        // `EntityMayTeleport`: a parented entity refuses, unless it is a
        // passenger in a vehicle — and Portal 2 has no vehicles, so the second
        // half is a warning and a refusal.
        let parented = cx.entity(target).is_some_and(|e| e.core.parent.is_some());
        if parented {
            eprintln!(
                "source-engine: server: ERROR: ({}) can't teleport object ({name}) as it has a parent",
                entity.debug_name()
            );
            return;
        }

        // `pTarget->Teleport( &vecOrigin, &angRotation, NULL )` — **a null
        // velocity, so whatever the target was doing it keeps doing.** The
        // difference from `trigger_teleport`, which rotates the velocity into
        // the destination's frame, is that this one has no frame to rotate
        // into.
        touch::teleport(
            cx,
            target,
            Teleport {
                origin: Some(origin),
                angles: Some(angles),
                velocity: None,
            },
        );
    }
}

impl Behaviour for PointTeleport {
    /// `CPointTeleport::Activate` (`pointteleport.cpp:86`).
    ///
    /// > **An entity with the spawn-position flag and no target deletes
    /// > itself.** Not reachable in Portal 2 — nothing sets the flag — and
    /// > ported because it is the only path in the class that changes the
    /// > entity list.
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.save_origin = entity.origin;
        self.save_angles = entity.angles;

        if !entity.has_spawn_flags(SF_TELEPORT_TO_SPAWN_POS) {
            return;
        }
        let name = entity.target.clone().unwrap_or_default();
        let Some(target) = cx.find_by_name(&name) else {
            eprintln!(
                "source-engine: server: ERROR: ({}) target '{name}' not found. Deleting.",
                entity.debug_name()
            );
            entity.remove();
            return;
        };
        let Some(target_entity) = cx.entity(target) else {
            return;
        };
        if target_entity.core.parent.is_some() {
            eprintln!(
                "source-engine: server: ERROR: ({}) can't teleport object ({name}) as it has a parent",
                entity.debug_name()
            );
            return;
        }
        self.save_origin = target_entity.core.origin;
        self.save_angles = target_entity.core.angles;
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("Teleport") {
            self.do_teleport(entity, input, self.save_origin, self.save_angles, false, cx);
            return true;
        }
        if is("TeleportEntity") {
            self.do_teleport(entity, input, self.save_origin, self.save_angles, true, cx);
            return true;
        }
        if is("TeleportToCurrentPos") {
            if entity.has_spawn_flags(SF_TELEPORT_TO_SPAWN_POS) {
                eprintln!(
                    "source-engine: server: {}: TeleportToCurrentPos input received; \
                     ignoring 'Teleport Home' spawnflag.",
                    entity.debug_name()
                );
            }
            let (origin, angles) = (entity.origin, entity.angles);
            self.do_teleport(entity, input, origin, angles, false, cx);
            return true;
        }
        false
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("save_origin", format!("{:?}", self.save_origin)),
            ("save_angles", format!("{:?}", self.save_angles)),
        ]
    }
}
