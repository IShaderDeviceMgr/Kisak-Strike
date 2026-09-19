//! Parenting: who moves with whom, and what that does to a transform.
//!
//! `game/server/hierarchy.cpp` (180 lines) plus `CBaseEntity::SetParent`
//! (`baseentity.cpp:1558`) and the half of `InvalidatePhysicsRecursive`
//! (`baseentity_shared.cpp:1633`) that is about position rather than about
//! bone caches, shadow volumes and the client leaf system.
//!
//! # What parenting is for
//!
//! 4,582 of the shipped maps' 60,925 entities name a `parentname`, and that
//! number is not evenly spread: **2,355 of the game's 8,462 `prop_dynamic`s**
//! are parented, and so are **201 movers**, which is the half that has teeth.
//! A Portal 2 chamber is built out of arms and panels that carry doors, clip
//! brushes and triggers, and every one of those riders is placed in Hammer at
//! its world position and then told to follow something.
//!
//! # The three operations, and why only one of them needs the list
//!
//! [`EntityCore`] already does everything that can be done from one entity:
//! writing the local pair re-derives the world pair against a cached parent
//! frame (see [`EntityCore::local_origin`] for why it is cached rather than
//! fetched). What is left here is the three things that are about *two*
//! entities:
//!
//! | | what it is | Valve |
//! |---|---|---|
//! | [`set_parent`] | re-point a child, holding its world placement still | `CBaseEntity::SetParent` + `UnlinkFromParent` |
//! | [`propagate`] | a parent moved; drag the subtree along | `InvalidatePhysicsRecursive( POSITION_CHANGED )` |
//! | [`descendants`] | everything under an entity, for removal | `GetAllChildren` |
//!
//! # Eager, where Valve is lazy — and the reason is `detach`
//!
//! Valve marks `EFL_DIRTY_ABSTRANSFORM` down the subtree and lets the next
//! `GetAbsOrigin` pay for it. That works because a child can reach its parent
//! through a pointer at any moment. Here it cannot: the entity being
//! dispatched is lifted out of the [`EntityList`] for the whole of its handler
//! ([`EntityList::detach`]), so the one instant a mover integrates its
//! velocity is the one instant its children could not resolve it — and the
//! one instant a *child* runs is an instant its parent is not resolvable
//! either, if the parent is the one dispatching.
//!
//! So the flow is turned around: a move recomputes its subtree then and there,
//! from the moved entity's own frame outwards, and nothing is ever dirty.
//! `Server::dispatch` calls [`propagate`] once on the way out of every
//! handler, which is the only place that can see both the entity that moved
//! and the list it moved within.
//!
//! **That is affordable because of two early-outs, and neither is optional.**
//! [`propagate`] returns at once on an empty child list — **59,017 of the
//! game's 60,925 entities have no children at all**; only 1,908 are anybody's
//! parent, and the widest child list in the game is 170. And
//! [`EntityCore::follow`] compares the frame it is handed against the one the
//! child already holds, so a subtree under an entity that was dispatched
//! without moving is not walked either. Drop the second and it is not merely
//! wasted work: `MatrixAngles(AngleMatrix(a))` is not `a` in `f32`, so a still
//! parent would shuffle its children sideways every tick — measured at 532
//! extra brush entities drifting off their spawn placement in the first two
//! seconds of the shipped maps.
//!
//! # What is deliberately not here
//!
//! **Attachment parenting.** `m_iParentAttachment` and the
//! `SetParentAttachment`/`SetParentAttachmentMaintainOffset` inputs need
//! `CBaseAnimating::LookupAttachment`, and 1,376 of the 1,454 shipped
//! connections that fire one really do reach the lookup — see the
//! expected-unhandled table in `tests` for the whole breakdown. The map-key
//! form `parentname "arm,attachment"` is not affected: **zero of the shipped
//! maps' 4,582 parented entities use it**, which is why
//! `extract_parent_name` can drop the half after the comma.
//!
//! **The velocity pair.** `CalcAbsoluteVelocity` (`baseentity.cpp:6588`)
//! rotates a local velocity into the world and adds the parent's. Nothing here
//! reads an absolute velocity of a parented entity: the pusher integrates the
//! local one, and `trigger_push` writes a base velocity onto the player, who
//! has no parent. It becomes necessary the moment something rides a *moving*
//! parent and is then let go.

use glam::Affine3A;

use super::entity::{EntityCore, EntityId, EntityList};

/// `CBaseEntity::SetParent` (`baseentity.cpp:1558`) — move `core` into
/// `parent`'s frame, or, for `None`, back out into the world's.
///
/// **The world placement does not change.** Both directions preserve it and
/// re-solve the local pair around it, which is what makes `SetParent` an
/// operation a map can fire at a prop in mid-air without the prop jumping:
/// `SetParent` runs `UnlinkFromParent` first (`hierarchy.cpp:98`), which
/// writes the *absolute* pair into the *local* pair, and then rebases that
/// through the new parent — so the entity ends up where it already was, twice
/// over.
///
/// > That reading is worth stating because the code does not look like it.
/// > `SetParent` computes the new local origin as `matrix.WorldToLocal(
/// > GetLocalOrigin() )` — the **local** origin, not the absolute one — which
/// > only makes sense once you have read `UnlinkFromParent` and know the two
/// > are equal by the time that line runs.
///
/// `core` is passed separately from `entities` because the entity being
/// dispatched is not in the list; the parent and the old parent are, and are
/// the only two things looked up. Parenting to an entity that is not in the
/// list — which includes parenting to *oneself* — is refused, the way Valve's
/// `Assert(0); m_pParent = NULL;` refuses the self case.
pub fn set_parent(core: &mut EntityCore, entities: &mut EntityList, parent: Option<EntityId>) {
    // `UnlinkFromParent( this )`: leave the old parent's child list. The local
    // pair becoming the world pair is what `set_parent_frame` does below, so
    // it is not spelled twice.
    if let Some(old) = core.parent() {
        if let Some(entity) = entities.get_mut(old) {
            entity.core.unlink_child(core.id());
        }
    }

    let frame = match parent {
        // `if ( m_pParent == this ) { Assert(0); m_pParent = NULL; }`, and the
        // same answer for a handle that no longer resolves.
        Some(parent) if parent != core.id() => match entities.get_mut(parent) {
            Some(entity) => {
                entity.core.link_child(core.id());
                Some(entity.core.to_world())
            }
            None => None,
        },
        _ => None,
    };

    match frame {
        Some(frame) => core.set_parent_frame(parent, frame),
        None => core.set_parent_frame(None, Affine3A::IDENTITY),
    }
}

/// `InvalidatePhysicsRecursive( POSITION_CHANGED | ANGLES_CHANGED )`
/// (`baseentity_shared.cpp:1633`) — `core` has moved, so recompute every
/// entity under it.
///
/// Call it **after** the write, and only after the last write of a tick: it is
/// idempotent, so calling it twice is merely wasted work, but calling it
/// before the final write leaves the subtree a tick behind.
///
/// `core` itself is not touched — its own world pair was re-derived by
/// whichever setter moved it.
pub fn propagate(core: &EntityCore, entities: &mut EntityList) {
    push_down(core.to_world(), core.children(), entities);
}

/// [`propagate`] for an entity that is *in* the list rather than detached.
///
/// The form `Server::dispatch` uses for everything a handler reached through
/// [`Context::entity_mut`](super::class::Context::entity_mut), which it knows
/// only by handle.
pub fn propagate_id(id: EntityId, entities: &mut EntityList) {
    let Some(entity) = entities.get(id) else {
        return;
    };
    if entity.core.children().is_empty() {
        return;
    }
    let (frame, children) = (entity.core.to_world(), entity.core.children().to_vec());
    push_down(frame, &children, entities);
}

/// The walk itself: a frame, the entities it applies to, and their subtrees.
///
/// Iterative with an explicit stack rather than recursive, because the borrow
/// of `entities` cannot be handed to a recursive call that also writes through
/// it. **The descent stops wherever nothing changed** — see
/// [`EntityCore::follow`] — which is what makes calling this after every
/// dispatch cost nothing for the entities that did not move.
fn push_down(frame: Affine3A, children: &[EntityId], entities: &mut EntityList) {
    if children.is_empty() {
        return;
    }
    let mut work: Vec<(EntityId, Affine3A)> =
        children.iter().map(|&child| (child, frame)).collect();
    while let Some((id, frame)) = work.pop() {
        let Some(entity) = entities.get_mut(id) else {
            continue;
        };
        if !entity.core.follow(frame) {
            continue;
        }
        let next = entity.core.to_world();
        work.extend(entity.core.children().iter().map(|&child| (child, next)));
    }
}

/// `GetAllChildren` (`hierarchy.cpp:165`) — everything under `core`, deepest
/// last.
///
/// One caller: `UpdateOnRemove` (`baseentity.cpp:2632`), which marks the whole
/// subtree for deletion — "Warning: Deleting orphaned children of %s". An
/// orphan is not re-parented to the world and is not left where it is; it goes
/// with its parent.
pub fn descendants(core: &EntityCore, entities: &EntityList) -> Vec<EntityId> {
    let mut found: Vec<EntityId> = core.children().to_vec();
    let mut next = 0;
    while next < found.len() {
        let id = found[next];
        next += 1;
        if let Some(entity) = entities.get(id) {
            found.extend_from_slice(entity.core.children());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::classes;
    use crate::server::entity::Entity;
    use glam::Vec3;

    fn list() -> (EntityList, EntityId, EntityId) {
        let class = classes::lookup("info_target").expect("info_target is registered");
        let mut entities = EntityList::new();
        let parent = entities.insert(Entity::new(class));
        let child = entities.insert(Entity::new(class));
        (entities, parent, child)
    }

    /// Move a detached core the way `Server::dispatch` would, then put it
    /// back — the shape every real caller has.
    fn with(
        entities: &mut EntityList,
        id: EntityId,
        f: impl FnOnce(&mut EntityCore, &mut EntityList),
    ) {
        let mut entity = entities.detach(id).expect("live");
        f(&mut entity.core, entities);
        propagate(&entity.core, entities);
        entities.attach(id, entity);
    }

    #[test]
    fn parenting_holds_the_world_placement_still_and_solves_the_local_one() {
        let (mut entities, parent, child) = list();
        entities
            .get_mut(parent)
            .unwrap()
            .core
            .set_abs_placement(Vec3::new(100.0, 0.0, 0.0), Vec3::new(0.0, 90.0, 0.0));
        with(&mut entities, child, |core, entities| {
            core.set_abs_placement(Vec3::new(110.0, 0.0, 0.0), Vec3::ZERO);
            set_parent(core, entities, Some(parent));
        });

        let child = &entities.get(child).unwrap().core;
        // The world placement is untouched by the parenting itself.
        assert_eq!(child.origin, Vec3::new(110.0, 0.0, 0.0));
        // …and in a frame yawed 90°, "ten units further along +X" reads as
        // ten units along the parent's own -Y.
        assert!(
            (child.local_origin - Vec3::new(0.0, -10.0, 0.0)).length() < 1e-4,
            "{:?}",
            child.local_origin
        );
    }

    #[test]
    fn a_parent_that_turns_carries_its_child_around_it() {
        let (mut entities, parent, child) = list();
        with(&mut entities, child, |core, entities| {
            core.set_abs_placement(Vec3::new(10.0, 0.0, 0.0), Vec3::ZERO);
            set_parent(core, entities, Some(parent));
        });
        // The parent yaws a quarter turn about the origin it sits on.
        with(&mut entities, parent, |core, _| {
            core.set_local_angles(Vec3::new(0.0, 90.0, 0.0));
        });

        let child = &entities.get(child).unwrap().core;
        assert!(
            (child.origin - Vec3::new(0.0, 10.0, 0.0)).length() < 1e-4,
            "{:?}",
            child.origin
        );
        // The local pair is what did not move, which is the whole point.
        assert_eq!(child.local_origin, Vec3::new(10.0, 0.0, 0.0));
        assert!((child.angles.y - 90.0).abs() < 1e-3, "{:?}", child.angles);
    }

    #[test]
    fn clearing_a_parent_leaves_the_entity_exactly_where_it_was() {
        let (mut entities, parent, child) = list();
        with(&mut entities, parent, |core, _| {
            core.set_abs_placement(Vec3::new(0.0, 0.0, 64.0), Vec3::new(0.0, 45.0, 0.0));
        });
        with(&mut entities, child, |core, entities| {
            core.set_abs_placement(Vec3::new(32.0, 0.0, 64.0), Vec3::ZERO);
            set_parent(core, entities, Some(parent));
        });
        // The parent moves, taking the child with it, and only then is the
        // child let go.
        with(&mut entities, parent, |core, _| {
            core.set_local_origin(Vec3::new(0.0, 0.0, 128.0));
        });
        let carried = entities.get(child).unwrap().core.origin;
        with(&mut entities, child, |core, entities| {
            set_parent(core, entities, None);
        });

        let child = &entities.get(child).unwrap().core;
        assert_eq!(child.origin, carried, "the world placement survives");
        assert_eq!(
            child.local_origin, carried,
            "and the local pair becomes it — `UnlinkFromParent`'s two lines"
        );
        assert!(entities.get(parent).unwrap().core.children().is_empty());
    }

    #[test]
    fn a_grandchild_moves_with_the_root() {
        let class = classes::lookup("info_target").expect("info_target is registered");
        let (mut entities, root, child) = list();
        let grandchild = entities.insert(Entity::new(class));
        for (id, parent) in [(child, root), (grandchild, child)] {
            with(&mut entities, id, |core, entities| {
                set_parent(core, entities, Some(parent));
            });
        }
        with(&mut entities, root, |core, _| {
            core.set_local_origin(Vec3::new(0.0, 0.0, 256.0));
        });

        assert_eq!(
            entities.get(grandchild).unwrap().core.origin,
            Vec3::new(0.0, 0.0, 256.0)
        );
        assert_eq!(
            descendants(&entities.get(root).unwrap().core, &entities).len(),
            2
        );
    }

    #[test]
    fn an_entity_cannot_be_its_own_parent() {
        let (mut entities, parent, _) = list();
        with(&mut entities, parent, |core, entities| {
            let me = core.id();
            set_parent(core, entities, Some(me));
        });
        assert!(entities.get(parent).unwrap().core.parent().is_none());
    }

    /// The unparented path must not go near `MatrixAngles`, because it does
    /// not round-trip — `CalcAbsolutePosition`'s no-move-parent branch is a
    /// straight copy and this is what that buys.
    #[test]
    fn an_unparented_entity_keeps_its_angles_bit_for_bit() {
        let (mut entities, id, _) = list();
        let odd = Vec3::new(13.700001, -157.30002, 0.5000001);
        let core = &mut entities.get_mut(id).unwrap().core;
        core.set_local_angles(odd);
        assert_eq!(core.angles, odd);
        core.set_abs_placement(Vec3::ZERO, odd);
        assert_eq!(core.local_angles, odd);
    }
}
