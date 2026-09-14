//! The touch link list: who is standing in what, and when that changed.
//!
//! `touchlink_t` (`game/shared/touchlink.h`),
//! `CBaseEntity::PhysicsMarkEntitiesAsTouching`,
//! `PhysicsMarkEntityAsTouched`, `PhysicsCheckForEntityUntouch`,
//! `PhysicsNotifyOtherOfUntouch` and `PhysicsRemoveToucher`
//! (`game/shared/physics_main_shared.cpp:560-1080`), plus
//! `CEntityTouchManager` (`game/server/entitylist.cpp:1650`) and the engine
//! half, `CTouchLinks` (`engine/world.cpp:120`).
//!
//! # A touch is a fact about two entities, so nothing here is a behaviour
//!
//! Every function in this module takes `&mut Server`. That is not a
//! convenience: a touch is *symmetric* — both entities carry a link and
//! exactly one of the two carries the flag that will fire an `EndTouch` — and
//! keeping half of it inside a [`Behaviour`](super::class::Behaviour) would
//! mean a handler mutating the entity on the other side of itself. What a
//! class gets is the three callbacks (`start_touch`, `touch`, `end_touch`) and
//! its own [`EntityCore::touch_links`] to read.
//!
//! # The engine says who overlaps; the game says what that means
//!
//! This is the split the original has and it is worth keeping. `SolidMoved`
//! hands the *engine* a swept box and gets back the trigger volumes it
//! intersects (`engine/world.cpp`'s `CTouchLinks`, over the spatial
//! partition); the game then does everything below. Here the engine's half is
//! [`TouchQuery`](super::TouchQuery), implemented in `engine/mod.rs` over
//! `world/`'s placed brush models — so this module names no collision type and
//! runs in a test with no map.
//!
//! # The stamp is the mechanism, not a geometric test
//!
//! Nothing ever asks "have these two stopped overlapping". Instead:
//!
//! 1. Before a toucher re-tests what it is in, `SetCheckUntouch` bumps its
//!    [`touch_stamp`](EntityCore::touch_stamp) and puts it on the sweep list.
//! 2. Every link the test confirms is written with the *new* stamp.
//! 3. After the thinks, `Server::check_for_entity_untouch` walks the sweep list and
//!    every link still carrying an old stamp is a touch that has ended.
//!
//! The consequence is that **an `EndTouch` costs nothing to detect** and, more
//! importantly, that a trigger which is switched off simply stops being
//! reported by the query and every entity inside it leaves on the next tick.

use super::class::Context;
use super::entity::{EntityCore, EntityId};
use super::movement::{FL_ONGROUND, FSOLID_TRIGGER, FSOLID_VOLUME_CONTENTS};
use super::Server;

/// One entry of an entity's `TOUCHLINK` list.
///
/// Valve threads a doubly-linked list of these through a pooled allocator and
/// hangs the head off the entity as a named data object. A `Vec` on the entity
/// is the same structure: the list is walked, appended to and removed from by
/// value, never held across a call, and the longest one in the game is a
/// handful of entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchLink {
    /// `entityTouched`.
    pub other: EntityId,
    /// `touchStamp` — the owning entity's stamp as of the last confirmation.
    pub stamp: i32,
    /// `FTOUCHLINK_START_TOUCH` — whether creating this link fired a
    /// `StartTouch`, and therefore whether destroying it owes an `EndTouch`.
    ///
    /// > **Only one side of a touch carries it.** A trigger's link to the
    /// > player has it, because the trigger is `FSOLID_TRIGGER` and the player
    /// > is not; the player's link back does not, because the *other* side of
    /// > that link is a trigger. So `EndTouch` is called on the trigger with
    /// > the player and never the reverse, which is exactly the asymmetry a
    /// > map author sees.
    pub start_touch: bool,
}

impl Server {
    /// `CBaseEntity::SetCheckUntouch( true )` (`baseentity.cpp:6492`) — bump
    /// this entity's stamp and register it for the post-think sweep.
    ///
    /// The bump is what invalidates every link it already has; anything the
    /// caller then confirms gets the new value.
    pub(super) fn set_check_untouch(&mut self, id: EntityId) {
        let Some(entity) = self.entities.get_mut(id) else {
            return;
        };
        entity.core.touch_stamp = entity.core.touch_stamp.wrapping_add(1);
        if !entity.core.check_untouch {
            entity.core.check_untouch = true;
            self.untouch_list.push(id);
        }
    }

    /// `CBaseEntity::PhysicsMarkEntitiesAsTouching` (`:1064`) — record that
    /// `a` and `b` are in contact, both ways.
    ///
    /// The two asymmetric fix-ups at the end are Valve's and they matter: the
    /// guards in [`mark_entity_as_touched`](Server::mark_entity_as_touched)
    /// are not symmetric (a trigger refuses to link to a trigger only when
    /// *neither* is solid, and a marked-for-deletion entity refuses either
    /// way), so one side can link while the other refuses. When that happens
    /// the link that did form is taken back out, which is what keeps the list
    /// consistent enough for the stamp sweep to be the only end-of-touch test.
    pub(super) fn mark_entities_as_touching(&mut self, a: EntityId, b: EntityId) {
        let linked_a = self.mark_entity_as_touched(a, b);
        let linked_b = self.mark_entity_as_touched(b, a);

        if linked_a && !linked_b {
            self.notify_other_of_untouch(b, a);
        }
        if linked_b && !linked_a {
            self.notify_other_of_untouch(a, b);
        }
    }

    /// `CBaseEntity::PhysicsMarkEntityAsTouched` (`:955`) — `this`'s half of a
    /// touch. Returns whether a link exists afterwards.
    ///
    /// Creating one fires `StartTouch` **and** `Touch`; confirming an existing
    /// one fires `Touch` alone. Both are `PhysicsStartTouch`/`PhysicsTouch`,
    /// which is where the marked-for-deletion re-check lives.
    fn mark_entity_as_touched(&mut self, this: EntityId, other: EntityId) -> bool {
        if this == other {
            return false;
        }

        // The four remaining guards, in Valve's order. Read out of the two
        // entities in one borrow so that nothing below has to re-resolve a
        // handle.
        let Some((this_core, other_core)) = self.two_cores(this, other) else {
            return false;
        };
        // "Entities in hierarchy should not interact."
        if this_core.parent == Some(other) || other_core.parent == Some(this) {
            return false;
        }
        // `FL_DONTTOUCH` — set by nothing here, kept because the line is one
        // `|` and its absence would be invisible.
        const FL_DONTTOUCH: u32 = 1 << 23;
        if (this_core.flags | other_core.flags) & FL_DONTTOUCH != 0 {
            return false;
        }
        // "Pure triggers should not touch each other" — and note that it is
        // *pure*: two triggers that are also solid still link.
        if this_core.is_solid_flag_set(FSOLID_TRIGGER)
            && other_core.is_solid_flag_set(FSOLID_TRIGGER)
            && !this_core.is_solid()
            && !other_core.is_solid()
        {
            return false;
        }
        if this_core.removed || other_core.removed {
            return false;
        }

        let stamp = this_core.touch_stamp;
        // `bShouldTouch` decides whether this side owes a `StartTouch`, and
        // through it an eventual `EndTouch`. It is read here, while both
        // entities are in hand, and used after the borrow ends.
        let should_touch = ((this_core.is_solid()
            && !this_core.is_solid_flag_set(FSOLID_VOLUME_CONTENTS))
            || this_core.is_solid_flag_set(FSOLID_TRIGGER))
            && !other_core.is_solid_flag_set(FSOLID_TRIGGER);

        let existing = self
            .entities
            .get_mut(this)
            .and_then(|e| e.core.touch_links.iter_mut().find(|l| l.other == other));
        if let Some(link) = existing {
            link.stamp = stamp;
            // `PhysicsTouch`.
            self.dispatch(this, |core, behaviour, cx| {
                behaviour.touch(core, other, cx);
            });
            return true;
        }

        let Some(entity) = self.entities.get_mut(this) else {
            return false;
        };
        entity.core.touch_links.push(TouchLink {
            other,
            stamp,
            start_touch: should_touch,
        });

        if should_touch {
            // `PhysicsStartTouch`: both, in this order, in the same tick.
            self.dispatch(this, |core, behaviour, cx| {
                behaviour.start_touch(core, other, cx);
                behaviour.touch(core, other, cx);
            });
        }
        true
    }

    /// `CBaseEntity::PhysicsNotifyOtherOfUntouch( ent, other )` (`:667`) —
    /// remove `other`'s link back to `ent`, firing `other`'s `EndTouch` if
    /// that link owes one.
    ///
    /// Named exactly as Valve names it, argument order and all, because the
    /// direction is easy to get backwards: the entity whose list is edited is
    /// the **second** one.
    fn notify_other_of_untouch(&mut self, ent: EntityId, other: EntityId) {
        let Some(entity) = self.entities.get(other) else {
            return;
        };
        let Some(index) = entity.core.touch_links.iter().position(|l| l.other == ent) else {
            return;
        };
        self.remove_toucher(other, index);
    }

    /// `CBaseEntity::PhysicsRemoveToucher( otherEntity, link )` (`:703`) —
    /// drop one link and, if it carried the start flag, fire the owner's
    /// `EndTouch`.
    ///
    /// > **`EndTouch` runs before the link is gone**, which is what lets
    /// > `CBaseTrigger::EndTouch` ask `IsTouching( pOther )` and get `true`.
    /// > It maintains its *own* list of touchers separately from this one and
    /// > removes the entry itself; the two are not the same list and it is the
    /// > only class that keeps both.
    fn remove_toucher(&mut self, owner: EntityId, index: usize) {
        let Some(entity) = self.entities.get(owner) else {
            return;
        };
        let Some(&link) = entity.core.touch_links.get(index) else {
            return;
        };

        if link.start_touch && self.entities.is_alive(link.other) {
            self.dispatch(owner, |core, behaviour, cx| {
                behaviour.end_touch(core, link.other, cx);
            });
        }

        // Found again rather than kept, because the `EndTouch` above may have
        // edited the list — Valve unlinks *before* it is safe to and gets away
        // with it because its links are pooled nodes rather than indices.
        if let Some(entity) = self.entities.get_mut(owner) {
            if let Some(index) = entity
                .core
                .touch_links
                .iter()
                .position(|l| l.other == link.other)
            {
                entity.core.touch_links.remove(index);
            }
        }
    }

    /// `CBaseEntity::PhysicsRemoveTouchedList( ent )` (`:724`) — everything
    /// this entity is touching stops touching it.
    ///
    /// Called when an entity is freed, so that the other side of every link
    /// gets its `EndTouch` rather than being left holding a dead handle. That
    /// is Valve's `UpdateOnRemove`, and it is why a `trigger_once` deleting
    /// itself 0.1 s after firing does not leave the player permanently inside
    /// something that no longer exists.
    /// > **The entity going away gets no `EndTouch` of its own.** Valve's loop
    /// > calls `PhysicsNotifyOtherOfUntouch` — which fires the *other* side's
    /// > `EndTouch` — and then `FreeTouchLink`, not `PhysicsRemoveToucher`. So
    /// > a `trigger_once` that deletes itself 0.1 s after firing never fires
    /// > `OnEndTouch`, and the 1,476 of them in the game are the reason that
    /// > is worth knowing rather than a curiosity.
    pub(super) fn remove_touched_list(&mut self, ent: EntityId) {
        loop {
            let Some(entity) = self.entities.get(ent) else {
                return;
            };
            let Some(&link) = entity.core.touch_links.first() else {
                break;
            };
            self.notify_other_of_untouch(ent, link.other);
            // `FreeTouchLink( link )`, with no `EndTouch` — see above.
            if let Some(entity) = self.entities.get_mut(ent) {
                if let Some(index) = entity
                    .core
                    .touch_links
                    .iter()
                    .position(|l| l.other == link.other)
                {
                    entity.core.touch_links.remove(index);
                }
            }
        }
        if let Some(entity) = self.entities.get_mut(ent) {
            entity.core.touch_stamp = 0;
            entity.core.check_untouch = false;
        }
        self.untouch_list.retain(|&id| id != ent);
    }

    /// `CEntityTouchManager::FrameUpdatePostEntityThink` (`entitylist.cpp:1718`)
    /// plus the `PhysicsCheckForEntityUntouch` it calls.
    ///
    /// Every entity that re-tested its touches this tick sweeps its own list;
    /// a link still carrying an old stamp is a touch that ended.
    ///
    /// **Its place in the tick is `FrameUpdatePostEntityThinkAllSystems`** —
    /// after the thinks and *before* `ServiceEventQueue` — so an `OnEndTouch`
    /// is delivered in the same tick it was detected. Move it after the queue
    /// and every `EndTouch` in the game arrives a tick late.
    pub(super) fn check_for_entity_untouch(&mut self) {
        // "copy off the list, clear it" — a `StartTouch` fired by an
        // `EndTouch` handler must register for the *next* sweep, not this one.
        let list = std::mem::take(&mut self.untouch_list);

        for id in list {
            let Some(entity) = self.entities.get_mut(id) else {
                continue;
            };
            if !entity.core.check_untouch {
                continue;
            }
            entity.core.check_untouch = false;
            let stamp = entity.core.touch_stamp;

            // Indices are not stable across the calls below, so the stale set
            // is collected by handle first.
            let stale: Vec<EntityId> = entity
                .core
                .touch_links
                .iter()
                .filter(|link| link.stamp != stamp)
                .map(|link| link.other)
                .collect();

            for other in stale {
                self.notify_other_of_untouch(id, other);
                let Some(entity) = self.entities.get(id) else {
                    break;
                };
                if let Some(index) = entity
                    .core
                    .touch_links
                    .iter()
                    .position(|l| l.other == other)
                {
                    self.remove_toucher(id, index);
                }
            }
        }
    }

    /// Both entities' shared state at once, for the guards that need to
    /// compare them. `None` if either handle has stopped resolving.
    ///
    /// Shared rather than exclusive: the guards only *read*, and asking for
    /// two `&mut`s out of one `Vec` would need a split the borrow checker
    /// cannot see through. The writes happen afterwards, one entity at a
    /// time.
    fn two_cores(&self, a: EntityId, b: EntityId) -> Option<(&EntityCore, &EntityCore)> {
        Some((&self.entities.get(a)?.core, &self.entities.get(b)?.core))
    }
}

/// Everything `entity` is currently touching, as handles.
///
/// `CTriggerHurt::HurtAllTouchers` walks exactly this list — **not** the
/// trigger's own `m_hTouchingEntities` — which is why an entity that failed
/// the trigger's filters is in it and is re-tested per hurt.
pub fn touching<'a>(entity: &'a EntityCore) -> impl Iterator<Item = EntityId> + 'a {
    entity.touch_links.iter().map(|link| link.other)
}

/// `CBaseEntity::Teleport( &origin, &angles, &velocity )`, reduced to what a
/// map entity asks for: any of the three, or none.
///
/// It is an ordinary field write even for the player, because the player
/// entity's origin, angles and velocity are copied **out** of the entity list
/// at the end of every `Server::frame` as well as in at the start — see
/// [`PlayerState`](super::PlayerState). That symmetry is what lets
/// `trigger_teleport` and `point_teleport` be written without either of them
/// knowing what kind of entity it is talking to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Teleport {
    pub origin: Option<glam::Vec3>,
    /// Pitch/yaw/roll. For a player these are the *view* angles.
    pub angles: Option<glam::Vec3>,
    pub velocity: Option<glam::Vec3>,
}

impl Teleport {
    /// Writes this teleport onto an ordinary entity.
    pub fn apply(&self, entity: &mut EntityCore) {
        if let Some(origin) = self.origin {
            entity.origin = origin;
        }
        if let Some(angles) = self.angles {
            entity.angles = angles;
        }
        if let Some(velocity) = self.velocity {
            entity.velocity = velocity;
        }
    }
}

/// Sends `target` to a place. The one operation `trigger_teleport` and
/// `point_teleport` share.
pub(super) fn teleport(cx: &mut Context<'_>, target: EntityId, teleport: Teleport) {
    if let Some(core) = cx.entity_mut(target) {
        teleport.apply(core);
        // `pOther->SetGroundEntity( NULL )`, which both callers do before the
        // move. For the player it reaches `client::Player::ground` through the
        // same copy-out the origin does.
        core.flags &= !FL_ONGROUND;
    }
}
