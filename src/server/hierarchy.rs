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
//! # Parenting to an attachment point
//!
//! A child names `m_iParentAttachment` and then hangs off a **named frame on
//! one of its parent's bones** rather than off the parent's origin. The
//! shipped maps declare **1,362** `SetParentAttachment*` connections against
//! `SetParent`'s 143, and 1,040 entities end up riding a bone. The composition is unchanged — the child's local pair is still applied
//! to a parent frame — so only the *frame* differs, and
//! [`parent_to_world`] is the one function that knows it:
//! `GetParentToWorldTransform` (`baseentity.cpp:6650`), which asks
//! [`Attachments`] and falls back to the parent's own transform if anything
//! at all goes wrong.
//!
//! **That frame moves every tick, which plain parenting's does not.** A
//! `prop_dynamic` arm advances its cycle in `AnimThink`, and the attachment
//! rides a bone that the new cycle has moved — so a child of an attachment
//! has to be recomputed on a tick where the parent's *origin* did not change
//! at all. [`EntityCore::follow`]'s early-out still works, because it compares
//! frames rather than origins and the frame it is handed is the attachment's.
//!
//! The map-key form `parentname "arm,attachment"` is a different thing and is
//! still not read: **zero of the shipped maps' 4,582 parented entities use
//! it**, which is why `extract_parent_name` can drop the half after the comma.
//!
//! # What is deliberately not here
//!
//! **The velocity pair.** `CalcAbsoluteVelocity` (`baseentity.cpp:6588`)
//! rotates a local velocity into the world and adds the parent's. Nothing here
//! reads an absolute velocity of a parented entity: the pusher integrates the
//! local one, and `trigger_push` writes a base velocity onto the player, who
//! has no parent. It becomes necessary the moment something rides a *moving*
//! parent and is then let go.

use glam::Affine3A;

use super::attachment::{self, Posed, Poser};
use super::entity::{Entity, EntityCore, EntityId, EntityList};

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
pub fn set_parent(
    core: &mut EntityCore,
    entities: &mut EntityList,
    parent: Option<EntityId>,
    attachment: Option<usize>,
    poser: Poser<'_>,
) {
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
                Some(parent_to_world(entity, attachment, poser))
            }
            None => None,
        },
        _ => None,
    };

    match frame {
        Some(frame) => core.set_parent_frame(parent, attachment, frame),
        // An attachment without a parent is not an attachment: Valve's
        // `SetParent( NULL )` leaves `m_iParentAttachment` set but nothing
        // ever reads it again, because every read is behind a move parent.
        None => core.set_parent_frame(None, None, Affine3A::IDENTITY),
    }
}

/// `CBaseEntity::GetParentToWorldTransform` (`baseentity.cpp:6650`) — the
/// frame a child of `parent` hangs from.
///
/// The parent's own transform, unless the child names an attachment point and
/// that point can be found, in which case it is the attachment's. **Every
/// failure falls back to the parent's own transform**, which is Valve's
/// comment at the bottom of the function — "if we fall through to here, then
/// just use the move parent's abs origin and angles" — and is not a defensive
/// branch: a level's first tick asks this before any model has been loaded.
pub fn parent_to_world(
    parent: &Entity,
    attachment: Option<usize>,
    poser: Poser<'_>,
) -> Affine3A {
    let entity_to_world = parent.core.to_world();
    let Some(index) = attachment else {
        return entity_to_world;
    };
    match attachment::posed(parent, poser)
        .and_then(|posed| posed.attachment_to_model(index, poser.attachments))
    {
        // `ConcatTransforms` — the attachment is in the model's frame and the
        // model is in the world's.
        Some(attachment_to_model) => entity_to_world * attachment_to_model,
        None => entity_to_world,
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
pub fn propagate(
    core: &EntityCore,
    posed: Option<Posed<'_>>,
    entities: &mut EntityList,
    poser: Poser<'_>,
) {
    if core.children().is_empty() {
        return;
    }
    let mut work = Vec::new();
    frames_for(
        core.to_world(),
        posed,
        core.children(),
        entities,
        poser,
        &mut work,
    );
    push_down(work, entities, poser);
}

/// [`propagate`] for an entity that is *in* the list rather than detached.
///
/// The form `Server::dispatch` uses for everything a handler reached through
/// [`Context::entity_mut`](super::class::Context::entity_mut), which it knows
/// only by handle.
pub fn propagate_id(id: EntityId, entities: &mut EntityList, poser: Poser<'_>) {
    let Some(entity) = entities.get(id) else {
        return;
    };
    if entity.core.children().is_empty() {
        return;
    }
    let (frame, children) = (entity.core.to_world(), entity.core.children().to_vec());
    let mut work = Vec::new();
    frames_for(
        frame,
        attachment::posed(entity, poser),
        &children,
        entities,
        poser,
        &mut work,
    );
    push_down(work, entities, poser);
}

/// Which frame each of `children` hangs from, given a parent that is at
/// `entity_to_world` and posed as `posed`.
///
/// Almost every child takes the parent's own transform and the whole function
/// is one copy. The attachment path is the exception, and it **caches by
/// attachment index** because posing a skeleton is the expensive half and a
/// Hammer instance parents its whole clip set to the same point — 903 of the
/// game's 1,037 resolvable attachment connections name a `func_brush`, and
/// they arrive through one input name on a handful of arms.
fn frames_for(
    entity_to_world: Affine3A,
    posed: Option<Posed<'_>>,
    children: &[EntityId],
    entities: &EntityList,
    poser: Poser<'_>,
    out: &mut Vec<(EntityId, Affine3A)>,
) {
    let mut cache: Vec<(usize, Affine3A)> = Vec::new();
    for &child in children {
        let index = entities
            .get(child)
            .and_then(|child| child.core.parent_attachment());
        let frame = match index {
            None => entity_to_world,
            Some(index) => match cache.iter().find(|(held, _)| *held == index) {
                Some(&(_, frame)) => frame,
                None => {
                    let frame = posed
                        .and_then(|posed| posed.attachment_to_model(index, poser.attachments))
                        .map_or(entity_to_world, |local| entity_to_world * local);
                    cache.push((index, frame));
                    frame
                }
            },
        };
        out.push((child, frame));
    }
}

/// The walk itself: a frame, the entities it applies to, and their subtrees.
///
/// Iterative with an explicit stack rather than recursive, because the borrow
/// of `entities` cannot be handed to a recursive call that also writes through
/// it. **The descent stops wherever nothing changed** — see
/// [`EntityCore::follow`] — which is what makes calling this after every
/// dispatch cost nothing for the entities that did not move.
fn push_down(
    mut work: Vec<(EntityId, Affine3A)>,
    entities: &mut EntityList,
    poser: Poser<'_>,
) {
    while let Some((id, frame)) = work.pop() {
        let Some(entity) = entities.get_mut(id) else {
            continue;
        };
        if !entity.core.follow(frame) {
            continue;
        }
        if entity.core.children().is_empty() {
            continue;
        }
        let (next, children) = (entity.core.to_world(), entity.core.children().to_vec());
        // Re-fetched immutably rather than kept from above: the child lookups
        // in `frames_for` borrow the same list, and a posed parent's
        // `model_state` borrows the parent.
        let posed = entities.get(id).and_then(|parent| attachment::posed(parent, poser));
        frames_for(next, posed, &children, entities, poser, &mut work);
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

/// `GetRootMoveParent()` (`baseentity.cpp:6960`) — the top of the chain `id`
/// hangs from, which is `id` itself when it has no parent.
///
/// > **An id the list cannot resolve is a root**, and that is not a failure
/// > case: the entity being dispatched is
/// > detached for the whole of its handler, so a child
/// > of the pusher walks up to a parent that is not there — and that parent is
/// > exactly the root the caller is asking about. See [`push`](super::push),
/// > which is the one caller.
///
/// The walk is bounded by the list's length. `set_parent` refuses to make an
/// entity its own parent and nothing builds a longer cycle, but a cycle here
/// would hang the tick rather than produce a wrong answer, which is the wrong
/// way round.
pub fn root_move_parent(id: EntityId, entities: &EntityList) -> EntityId {
    let mut current = id;
    for _ in 0..=entities.len() {
        let Some(entity) = entities.get(current) else {
            return current;
        };
        match entity.core.parent() {
            Some(parent) => current = parent,
            None => return current,
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::attachment::Poser;
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
        with_poser(entities, id, Poser::NONE, f)
    }

    /// [`with`], for a list whose models have attachment points.
    fn with_poser(
        entities: &mut EntityList,
        id: EntityId,
        poser: Poser<'_>,
        f: impl FnOnce(&mut EntityCore, &mut EntityList),
    ) {
        let mut entity = entities.detach(id).expect("live");
        f(&mut entity.core, entities);
        let posed = attachment::posed(&entity, poser);
        propagate(&entity.core, posed, entities, poser);
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
            set_parent(core, entities, Some(parent), None, Poser::NONE);
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
            set_parent(core, entities, Some(parent), None, Poser::NONE);
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
            set_parent(core, entities, Some(parent), None, Poser::NONE);
        });
        // The parent moves, taking the child with it, and only then is the
        // child let go.
        with(&mut entities, parent, |core, _| {
            core.set_local_origin(Vec3::new(0.0, 0.0, 128.0));
        });
        let carried = entities.get(child).unwrap().core.origin;
        with(&mut entities, child, |core, entities| {
            set_parent(core, entities, None, None, Poser::NONE);
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
                set_parent(core, entities, Some(parent), None, Poser::NONE);
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
            set_parent(core, entities, Some(me), None, Poser::NONE);
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
