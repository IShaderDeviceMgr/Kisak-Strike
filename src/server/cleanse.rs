//! What a `trigger_portal_cleanser` does to what touches it, done by the
//! server — see [`classes::volume`](super::classes::volume) for the class and
//! for what was reconstructed from what.

use glam::Vec3;

use super::class::Cleanse;
use super::classes::{PortalVolume, PropPortal, WeaponPortalgun};
use super::entity::EntityId;
use super::io::Variant;
use super::obb::obb_intersects_obb;
use super::Server;

/// How thick a portal is for `FizzleTouchingPortals`' overlap test — the
/// quad, a unit either side of its plane.
const PORTAL_THICKNESS: f32 = 1.0;

impl Server {
    /// Serves every [`Context::spawn_template`](super::class::Context::spawn_template)
    /// and [`Context::cleanse`](super::class::Context::cleanse) request,
    /// including the ones serving them makes — only at the outermost
    /// dispatch, as a loop.
    pub(super) fn serve_requests(&mut self) {
        if self.serving_requests {
            return;
        }
        self.serving_requests = true;
        // Far more than any shipped chain can need; a template that spawns a
        // relay that spawns the template would otherwise never stop.
        const ROUNDS: usize = 64;
        for _ in 0..ROUNDS {
            if self.pending_template_spawns.is_empty() && self.pending_cleanses.is_empty() {
                break;
            }
            for request in std::mem::take(&mut self.pending_template_spawns) {
                self.serve_template_spawn(request);
            }
            for cleanse in std::mem::take(&mut self.pending_cleanses) {
                self.serve_cleanse(cleanse);
            }
        }
        self.pending_template_spawns.clear();
        self.pending_cleanses.clear();
        self.serving_requests = false;
    }

    fn serve_cleanse(&mut self, cleanse: Cleanse) {
        match cleanse {
            Cleanse::Player { cleanser, player } => self.fizzle_player_portals(cleanser, player),
            Cleanse::Prop { cleanser, prop } => self.dissolve_prop(cleanser, prop),
            Cleanse::TouchingPortals { cleanser } => self.fizzle_portals_inside(cleanser),
        }
    }

    /// The player walked into an enabled cleanser: every portal their gun
    /// has placed fizzles — both colours, whichever chips the gun has — and
    /// if any did, the gun shows no colour and the cleanser fires `OnFizzle`
    /// with the player as activator.
    fn fizzle_player_portals(&mut self, cleanser: EntityId, player: EntityId) {
        let Some(gun) = self.player_portalgun() else {
            return;
        };
        let Some(group) = self
            .entities
            .get(gun)
            .and_then(|e| e.behaviour.downcast_ref::<WeaponPortalgun>())
            .map(|g| g.linkage_group)
        else {
            return;
        };
        let open: Vec<EntityId> = self
            .entities
            .iter()
            .filter(|(_, e)| !e.core.removed)
            .filter(|(_, e)| {
                e.behaviour
                    .downcast_ref::<PropPortal>()
                    .is_some_and(|p| p.activated && p.linkage_group == group)
            })
            .map(|(id, _)| id)
            .collect();
        if open.is_empty() {
            return;
        }
        for portal in open {
            self.accept_input(portal, "Fizzle", Variant::Void, Some(player), Some(cleanser), 0);
        }
        if let Some(gun) = self
            .entities
            .get_mut(gun)
            .and_then(|e| e.behaviour.downcast_mut::<WeaponPortalgun>())
        {
            gun.last_fired_portal = 0;
        }
        self.dispatch(cleanser, |core, _, cx| {
            core.fire_output("OnFizzle", Variant::Void, Some(player), Some(core.id()), 0.0, cx);
        });
    }

    /// `CTriggerPortalCleanser::FizzleBaseAnimating` for a cube: out of the
    /// player's hands, then its own `Dissolve` — `OnFizzled` and gone — then
    /// the cleanser's `OnDissolve`, and `OnDissolveBox` for a cube named
    /// `Box`, each with the cube as activator.
    fn dissolve_prop(&mut self, cleanser: EntityId, prop: EntityId) {
        let Some(entity) = self.entities.get(prop) else {
            return;
        };
        // `IsDissolving()` — a second touch in the same tick finds it gone.
        if entity.core.removed {
            return;
        }
        let is_box = entity
            .core
            .name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("Box"));
        if self.carried() == Some(prop) {
            self.drop_carried(true);
        }
        self.accept_input(prop, "Dissolve", Variant::Void, Some(cleanser), Some(cleanser), 0);
        self.dispatch(cleanser, |core, _, cx| {
            let me = Some(core.id());
            core.fire_output("OnDissolve", Variant::Void, Some(prop), me, 0.0, cx);
            if is_box {
                core.fire_output("OnDissolveBox", Variant::Void, Some(prop), me, 0.0, cx);
            }
        });
    }

    /// `InputFizzleTouchingPortals` — every active portal whose quad overlaps
    /// the cleanser's box fizzles.
    fn fizzle_portals_inside(&mut self, cleanser: EntityId) {
        let Some(volume) = self.entities.get(cleanser) else {
            return;
        };
        if volume.behaviour.downcast_ref::<PortalVolume>().is_none() {
            return;
        }
        let (origin, angles, bounds) = (volume.core.origin, volume.core.angles, volume.core.model_bounds);
        let inside: Vec<EntityId> = self
            .entities
            .iter()
            .filter(|(_, e)| !e.core.removed)
            .filter_map(|(id, e)| {
                let portal = e.behaviour.downcast_ref::<PropPortal>()?;
                if !portal.activated {
                    return None;
                }
                let half = Vec3::new(PORTAL_THICKNESS, portal.half_width, portal.half_height);
                obb_intersects_obb(e.core.origin, e.core.angles, -half, half, origin, angles, bounds.mins, bounds.maxs)
                    .then_some(id)
            })
            .collect();
        for portal in inside {
            self.accept_input(portal, "Fizzle", Variant::Void, Some(cleanser), Some(cleanser), 0);
        }
    }
}

#[cfg(test)]
mod tests;
