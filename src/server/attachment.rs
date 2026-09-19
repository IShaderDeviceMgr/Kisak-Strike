//! What the game asks a studio model about its **attachment points** — and
//! the whole of it.
//!
//! `CBaseAnimating::LookupAttachment` and `GetAttachment`
//! (`baseanimating.cpp:2041`, `:2120`), which in the original reach a
//! `CStudioHdr` through `modelinfo->GetModelPtr`. This module names no studio
//! type, for the same reason [`sequences`](super::sequences) does not: every
//! test in `server/` runs with no window, no GPU and no map.
//!
//! # Why this one is a trait where sequences are a table
//!
//! [`SequenceTable`](super::sequences::SequenceTable) holds *answers*, copied
//! in once per level, because a sequence's duration and loop flag are two
//! numbers and a bit that cannot change. An attachment's answer is a matrix
//! that **moves every tick**: it rides a bone, the bone is posed by whichever
//! animation the parent is playing, and the parent's cycle is advanced by the
//! server itself. There is nothing to tabulate.
//!
//! So this is the shape [`TouchQuery`](super::TouchQuery) already has — a
//! question the game asks and something outside answers — rather than the
//! shape `sequences` has. What differs from `TouchQuery` is *when*: a
//! collision query is threaded through one tick, and this is held by the
//! [`Server`](super::Server) for the level's lifetime, because
//! [`hierarchy::propagate`](super::hierarchy) runs at the end of **every**
//! dispatch and a console `ent_fire` reaches one without going through a
//! frame.
//!
//! # The three failure cases are all the same failure
//!
//! `GetParentToWorldTransform` (`baseentity.cpp:6650`) asks for the
//! attachment and, if anything at all goes wrong — no model, no such
//! attachment index, the bone missing — **falls through to the parent's own
//! transform**. So there is one `None` here and not three, and the caller
//! treats it as plain parenting. That is a real path rather than a defensive
//! one: `Spawn` runs before any model is loaded, so a level's first tick asks
//! this of a table that is still empty.

use glam::Affine3A;

/// The attachment points of every model the game's entities place, live.
///
/// Implemented by whoever loaded the models; [`NoAttachments`] is the answer
/// when nobody has.
pub trait Attachments {
    /// `LookupAttachment` — which attachment `name` is, on `model`.
    ///
    /// **Zero-based**, where Valve's is one-based so that 0 can mean "none";
    /// `None` is that case. Case-insensitive on both arguments, because
    /// `Studio_FindAttachment` is `stricmp` and map data spells model paths
    /// with either slash.
    fn lookup(&self, model: &str, name: &str) -> Option<usize>;

    /// `GetAttachment( iAttachment, attachmentToWorld )` in the model's own
    /// frame — the caller multiplies by the entity's.
    ///
    /// [`Posed`] is where the parent is in its animation, which is what makes
    /// this a question rather than a table entry. `None` is every one of
    /// Valve's `return false` cases at once; see the module docs.
    fn attachment_to_model(&self, posed: &Posed<'_>, attachment: usize) -> Option<Affine3A>;
}

/// What resolving an attachment frame needs, beside the entity list.
///
/// Carried as one value because [`hierarchy`](super::hierarchy) threads it
/// through four functions and a clock without a table is no more use than a
/// table without a clock.
#[derive(Clone, Copy)]
pub struct Poser<'a> {
    pub attachments: &'a dyn Attachments,
    /// `gpGlobals->curtime` — **the server's**, which is the tick's own clock
    /// and not `Scene::curtime` (gotcha 1). An attachment resolved during a
    /// tick is therefore up to one tick behind the pose that will be *drawn*,
    /// which is the same relationship a ticked door's origin already has to
    /// the one on screen.
    pub now: f32,
}

impl Poser<'_> {
    /// A poser that knows no models, for a caller with nothing to ask.
    pub const NONE: Poser<'static> = Poser {
        attachments: &NoAttachments,
        now: 0.0,
    };
}

/// Knows no models, so every lookup fails and every parenting is plain.
///
/// The default a [`Server`](super::Server) starts with, and what every test
/// that does not care about attachments runs against.
pub struct NoAttachments;

impl Attachments for NoAttachments {
    fn lookup(&self, _model: &str, _name: &str) -> Option<usize> {
        None
    }

    fn attachment_to_model(&self, _posed: &Posed<'_>, _attachment: usize) -> Option<Affine3A> {
        None
    }
}

/// Enough of an entity to evaluate one of its attachment points: the model it
/// wears and where in its animation it is.
///
/// Gathered from the two places these live — `EntityCore::model` and
/// [`Behaviour::model_state`](super::class::Behaviour::model_state) — so that
/// [`hierarchy`](super::hierarchy) can carry one value rather than five.
///
/// > **`cycle` is a checkpoint and not the live value**, which is why all five
/// > of these travel together. `m_flCycle` is written when something
/// > *decides* something — a new sequence, a rate change, a think that has to
/// > know whether the animation is over — and between those writes the pose is
/// > `cycle + elapsed * rate / duration`. The server does not own a `.mdl`, so
/// > it does not know `duration`; whoever implements [`Attachments`] does, and
/// > it is the same code that derives the cycle the model is **drawn** at.
/// > Deriving it a third way here is how an attachment would end up somewhere
/// > the picture is not.
#[derive(Debug, Clone, Copy)]
pub struct Posed<'a> {
    pub model: &'a str,
    /// `GetSequence()`, as a label — see
    /// [`ModelState`](super::class::ModelState).
    pub sequence: &'a str,
    /// `m_flCycle` as last written. See the note above.
    pub cycle: f32,
    /// `m_flAnimTime` — when that write happened.
    pub anim_time: f32,
    pub playback_rate: f32,
    /// `gpGlobals->curtime`, the server's, copied from the
    /// [`Poser`] that built this.
    pub now: f32,
}

impl Posed<'_> {
    /// `CBaseAnimating::GetAttachment`, in the model's frame.
    pub fn attachment_to_model(
        &self,
        attachment: usize,
        attachments: &dyn Attachments,
    ) -> Option<Affine3A> {
        attachments.attachment_to_model(self, attachment)
    }
}

/// A [`Posed`] with its two strings owned.
///
/// For the one caller that cannot hold a borrow of the entity for as long as
/// it needs the answer: [`push`](super::push) has the model on a `&mut
/// EntityCore` and the sequence on a `&mut dyn Behaviour`, and moves the first
/// several times between propagations.
///
/// **Built only when it can matter** — see [`PosedSnapshot::of`] — so a brush
/// mover, which is every mover in the shipped game, allocates nothing.
#[derive(Debug, Clone)]
pub struct PosedSnapshot {
    model: String,
    sequence: String,
    cycle: f32,
    anim_time: f32,
    playback_rate: f32,
    now: f32,
}

impl PosedSnapshot {
    /// The snapshot, or `None` when nothing could read it: an entity with no
    /// children, one wearing a brush model, or one whose class has no
    /// `model_state`.
    pub fn of(
        core: &super::entity::EntityCore,
        behaviour: &dyn super::class::Behaviour,
        poser: Poser<'_>,
    ) -> Option<PosedSnapshot> {
        if core.children().is_empty() {
            return None;
        }
        let model = core.model.as_deref()?;
        if model.starts_with('*') {
            return None;
        }
        let state = behaviour.model_state()?;
        Some(PosedSnapshot {
            model: model.to_owned(),
            sequence: state.sequence.to_owned(),
            cycle: state.cycle,
            anim_time: state.anim_time,
            playback_rate: state.playback_rate,
            now: poser.now,
        })
    }

    pub fn as_posed(&self) -> Posed<'_> {
        Posed {
            model: &self.model,
            sequence: &self.sequence,
            cycle: self.cycle,
            anim_time: self.anim_time,
            playback_rate: self.playback_rate,
            now: self.now,
        }
    }
}

/// What an entity is wearing, if it is wearing a studio model at all.
///
/// `CBaseEntity::GetBaseAnimating()` — the `dynamic_cast` that
/// `SetParentAttachment`'s second guard makes and **returns** on.
///
/// The test here is "wears a `.mdl` **and** reports a
/// [`ModelState`](super::class::ModelState)", which in this port is exactly
/// the four classes that have one: `prop_dynamic`, `prop_dynamic_override`'s
/// share of it, `prop_floor_button`, `prop_testchamber_door` and
/// `prop_portal`. A brush model (`"*12"`) is not a `CBaseAnimating` and
/// neither is an entity with no model — both are refusals rather than
/// fallbacks, because `SetParentAttachment` returns on this guard.
pub fn posed<'a>(entity: &'a super::entity::Entity, poser: Poser<'_>) -> Option<Posed<'a>> {
    let model = entity.core.model.as_deref()?;
    if model.starts_with('*') {
        return None;
    }
    let state = entity.behaviour.model_state()?;
    Some(Posed {
        model,
        sequence: state.sequence,
        cycle: state.cycle,
        anim_time: state.anim_time,
        playback_rate: state.playback_rate,
        now: poser.now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::classes;
    use crate::server::entity::Entity;

    fn entity(classname: &str, model: Option<&str>) -> Entity {
        let class = classes::lookup(classname).expect("registered");
        let mut entity = Entity::new(class);
        entity.core.model = model.map(str::to_owned);
        entity
    }

    /// The `NoAttachments` default is what a level runs on until its models
    /// load, which is every `Spawn` in the game.
    #[test]
    fn nothing_is_attached_to_a_model_nobody_loaded() {
        let posed = Posed {
            model: "models/arm.mdl",
            sequence: "idle",
            cycle: 0.0,
            anim_time: 0.0,
            playback_rate: 1.0,
            now: 0.0,
        };
        assert_eq!(NoAttachments.lookup("models/arm.mdl", "muzzle"), None);
        assert_eq!(NoAttachments.attachment_to_model(&posed, 0), None);
    }

    /// `GetBaseAnimating()` — `SetParentAttachment`'s second guard, and the
    /// three shapes it has to tell apart. Measured over the shipped maps, **no
    /// connection in the game aims at an entity whose parent is a brush
    /// model** and 6 aim at one with no parent at all, so this holds the
    /// branch rather than the content.
    #[test]
    fn only_an_entity_wearing_a_studio_model_can_be_posed() {
        let poser = Poser::NONE;
        assert!(posed(&entity("prop_dynamic", Some("models/arm.mdl")), poser).is_some());
        assert!(
            posed(&entity("func_brush", Some("*12")), poser).is_none(),
            "a brush model is not a CBaseAnimating"
        );
        assert!(posed(&entity("info_target", None), poser).is_none());
    }

    /// The snapshot exists so the pusher can hold an answer across a `&mut`
    /// borrow, and it refuses to allocate for the case that cannot need one:
    /// **an entity with no children**, which is 59,017 of the game's 60,925.
    #[test]
    fn a_snapshot_is_not_taken_for_an_entity_nothing_hangs_off() {
        let entity = entity("prop_dynamic", Some("models/arm.mdl"));
        assert!(entity.core.children().is_empty());
        assert!(
            PosedSnapshot::of(&entity.core, entity.behaviour.as_ref(), Poser::NONE).is_none(),
            "no children, so nothing could read it"
        );
    }
}
