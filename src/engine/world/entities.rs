//! Studio models that an **entity** places, posed by their animation.
//!
//! The third and last kind of geometry in a level shell. World faces and brush
//! entities are `.bsp` geometry drawn with a matrix; static props are `.mdl`
//! geometry the *map compiler* placed, in the `sprp` lump, never moving and lit
//! once at load. This is `.mdl` geometry the **game** places: where it is and
//! what it is doing are the server's to say, and it can change every tick.
//!
//! In Valve this is `C_BaseAnimating` and the client's renderable list. Here it
//! is one struct, because the port has exactly one animated entity class —
//! `prop_floor_button` — and the machinery a second one needs is already all
//! here.
//!
//! # The join with the game is a name, and the pose is the engine's
//!
//! The server says **which sequence** an entity is playing and **when it
//! started** ([`ModelEntity`]); the engine looks the sequence up in the model
//! by name and works out the cycle from the scene clock. That split is Valve's
//! own: `CBaseAnimating` networks `m_nSequence` and `m_flAnimTime` and it is
//! `C_BaseAnimating::FrameAdvance` on the *client* that turns them into a
//! cycle, because the server ticks at 64 Hz and animation wants to be smooth.
//!
//! It also keeps `server/`'s promise that it names no studio and no GPU type:
//! what crosses the boundary is a `&'static str`, three floats and a model
//! path.
//!
//! # Posing without skinning
//!
//! A model is drawn one **bone run** at a time — see
//! [`StudioModel::rigid_bones`](crate::studio::StudioModel::rigid_bones). Each
//! batch's triangles were sorted by bone at load, so each bone's are a
//! contiguous index range that can be drawn under that bone's own matrix, and
//! the vertex format, the shaders and the bind groups are untouched. For a
//! `prop_floor_button` that is two draws where a static prop is one: 7,263
//! triangles' worth of body under the identity and 666 of plate under whatever
//! the animation says.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec3};

use super::light::LightCache;
use super::props::models::{PropBatch, PropModel};
use super::GeometryPass;
use crate::engine::trace::CollisionBsp;
use crate::filesystem::Vfs;
use crate::materials::context::Pass;
use crate::materials::mesh::{StaticLightVertex, VertexBuffer, VertexLayout};
use crate::materials::uniforms::ModelLighting;
use crate::materials::{Material, MaterialCache};
use crate::studio::StudioModel;

/// One model-drawing entity, as the game server describes it.
///
/// `world/` names no server type and `server/` names no studio type, so this
/// is the whole vocabulary between them — the same arrangement
/// [`Placement`](super::Placement) already has for brush entities.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntity {
    /// An opaque, stable key for this entity, from whoever made the list.
    ///
    /// **Not an index into anything here.** [`sync`](EntityModels::sync)
    /// matches on it rather than on position, because an entity that places a
    /// model can be destroyed while the level runs — 556 shipped connections
    /// fire `Kill` at a `prop_dynamic` — and a positional match would then
    /// re-point every instance after the one that went.
    pub id: u64,
    /// The model path, as [`StudioModel::load`] wants it.
    pub model: String,
    pub origin: Vec3,
    /// Pitch, yaw, roll.
    pub angles: Vec3,
    /// `m_nSkin`. Not read yet — a model's skin families are
    /// `portdocs/STUDIO.md` stage 6's — and carried so that the seam does not
    /// have to change when they are.
    pub skin: i32,
    /// `ShouldDraw` — whether this entity is drawn *this frame*.
    ///
    /// An invisible instance is still loaded and still uploaded, because it
    /// can be turned back on: 1,000 `prop_dynamic`s in the game are
    /// `StartDisabled` and 206 connections toggle one.
    pub visible: bool,
    /// The sequence's **label**, as `LookupSequence` takes it: `"up"`,
    /// `"item_dropper_open"`. An empty string, or one the model does not have,
    /// is the bind pose.
    pub sequence: String,
    /// `m_flCycle` at [`anim_time`](ModelEntity::anim_time) — where in the
    /// sequence the pose was then, 0 to 1.
    pub cycle: f32,
    /// `m_flAnimTime` — the scene time the pose above was true at, which is
    /// what the elapsed time is measured from.
    pub anim_time: f32,
    /// `m_flPlaybackRate` — sequence lengths per second, signed. **Zero holds
    /// the pose**, which is how a prop that was never given an animation
    /// stands still.
    pub playback_rate: f32,
    /// `GetColorModulation()` in `rgb` and `ComputeRenderAlpha()` in `a` —
    /// `m_DiffuseModulation`, the vector
    /// `CModelRenderSystem::SetupPerInstanceColorModulation`
    /// (`modelrendersystem.cpp:1723`) hands every model draw.
    ///
    /// Computed on the server's side of the seam because that is where
    /// `rendermode`, `rendercolor` and `renderamt` are parsed and where a
    /// future `SetRenderMode` input would change them; `world/` only needs the
    /// four numbers. An alpha below 1 puts the whole instance in the
    /// translucent pass — see
    /// [`GeometryPass::of_instance`](super::GeometryPass::of_instance).
    ///
    /// Measured: **30 of the game's 8,462 `prop_dynamic`s** set a translucent
    /// render mode — 24 `kRenderTransTexture` and 6 `kRenderTransColor` — and
    /// none sets a glow mode, which is the only render mode that would need
    /// more than these four numbers (`IgnoresZBuffer()`, and with it a
    /// per-draw depth-test override).
    pub modulation: [f32; 4],
}

/// One row of [`EntityModels::sequences`] — what a `.mdl` says about one
/// sequence, in the vocabulary `server/`'s table wants.
///
/// A struct rather than a tuple because it grew a fourth number
/// (`fade_out_time`) and the two `f32`s beside a `bool` were becoming easy to
/// transpose. `world/` still names no server type: the translation into a
/// `SequenceInfo` is `engine/`'s, as it has always been.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SequenceRow<'a> {
    pub model: &'a str,
    pub label: &'a str,
    pub duration: f32,
    pub loops: bool,
    pub fade_out_time: f32,
}

/// Whether a world-space box is drawn this frame.
///
/// [`World::box_visible`](super::World::box_visible), handed in as a callback
/// rather than resolved here: an entity model is culled by its bounds, the
/// tree those bounds are tested against belongs to
/// [`World`](super::World), and this module has no business knowing what a
/// leaf is.
pub(crate) type BoxVisible<'a> = &'a dyn Fn(Vec3, Vec3) -> bool;

/// A last pad on the cull box, after the model's render bounds and the
/// playing sequence's box have both been taken.
///
/// Not doing any real work — those two boxes are the whole of
/// `C_BaseAnimating::GetRenderBounds` — and kept because a cull box is only
/// ever wrong in one direction: too small drops a prop that is on screen, too
/// large costs one draw the depth test then throws away.
const BOUNDS_SLACK: f32 = 8.0;

/// One placed instance, resolved against a loaded model.
struct Instance {
    /// [`ModelEntity::id`], which is what a sync matches on.
    id: u64,
    /// Index into [`EntityModels::models`].
    model: usize,
    /// `model_to_world` — the entity's placement, which every bone's matrix is
    /// composed onto the left of.
    transform: Mat4,
    /// Whether it is drawn. An instance whose entity has gone from the list
    /// entirely is set invisible and left in place rather than removed, so
    /// that the model it uploaded stays valid for its neighbours.
    visible: bool,
    /// `m_nSequence` — which of the model's sequences poses it.
    ///
    /// **A label that resolves to nothing is sequence 0, not "no sequence".**
    /// `m_nSequence` is an `int` that starts at zero, and `CDynamicProp` only
    /// ever moves it off zero deliberately: a prop with no `DefaultAnim` never
    /// calls `PropSetAnim` at all (`props.cpp:2036`), and one whose
    /// `DefaultAnim` names a sequence its model does not have is answered with
    /// an explicit `SetSequence( 0 )` (`props.cpp:2422`). So every prop in the
    /// game is posed by *some* sequence, and the bind pose is reached only by
    /// a model that has none at all.
    ///
    /// That distinction is not academic, because a bind pose is not a pose the
    /// artist ever looked at. `props_motel/hotel_container_furniture01`-`03`
    /// bind at `rot_x(+90)` and their `idle` holds a 120° turn about
    /// `(1,1,1)`, whose product with `poseToBone` is `rot_z(+90)` — so posing
    /// them from the bind pose turns `sp_a1_intro1`'s furniture a quarter turn
    /// and stands it in the bed.
    sequence: usize,
    cycle: f32,
    anim_time: f32,
    playback_rate: f32,
    /// [`ModelEntity::modulation`], carried through to the draw.
    modulation: [f32; 4],
    lighting: ModelLighting,
}

/// What loading a map's entity models cost.
#[derive(Debug, Clone, Default)]
pub struct EntityModelStats {
    /// Entities that name a model and were given one.
    pub instances: usize,
    /// Distinct models uploaded.
    pub models: usize,
    /// Models that would not load; their entities draw nothing.
    pub models_missing: usize,
    pub triangles: usize,
    /// Instances whose model has more than one bone — the ones the pose is
    /// actually for.
    pub animated: usize,
    /// Models whose vertices are **not** each bound to exactly one bone, and
    /// which are therefore drawn in their bind pose rather than wrongly. See
    /// [`StudioModel::rigid_bones`](crate::studio::StudioModel::rigid_bones).
    pub models_not_rigid: usize,
    /// Models whose sequences came out of a `$includemodel` companion, and the
    /// instances wearing one.
    ///
    /// Worth a line in the startup log because those instances are the ones
    /// whose `DefaultAnim` would otherwise resolve to nothing: nine models in
    /// the game do this and **926 `prop_dynamic`s wear one**. See
    /// [`StudioModel::includes`](crate::studio::StudioModel::includes).
    pub models_with_includes: usize,
    pub instances_with_includes: usize,
}

/// One model's attachment points, and everything needed to place them.
///
/// `CBaseAnimating::GetAttachment` reduced to what it reads: the attachment
/// table, the skeleton the bones are chained from, the sequence labels a
/// caller names and the animations they play. The last of those is shared
/// with [`PropModel`] rather than copied — see
/// [`StudioModel::animations`](crate::studio::StudioModel::animations).
pub struct AttachmentModel {
    attachments: Vec<crate::studio::anim::Attachment>,
    bones: Vec<crate::studio::anim::Bone>,
    /// `LookupSequence`'s table, reduced to what posing one needs.
    sequences: Vec<AttachmentSequence>,
    animations: Arc<[crate::studio::anim::Animation]>,
}

/// One sequence, as the attachment lookup reads it.
struct AttachmentSequence {
    label: String,
    /// Which of [`AttachmentModel::animations`] it plays.
    anim: usize,
    /// `STUDIO_LOOPING` — whether a cycle past 1 wraps or clamps.
    looping: bool,
}

/// Every entity model that has attachment points at all, by path.
///
/// **Most models are not in here**: an attachment is a thing a map parents to,
/// and a prop nothing hangs off has none. Built at load beside the uploads,
/// and handed to the game server, which asks it where a child of an animating
/// parent should be — `src/server/attachment.rs` for that side of it.
#[derive(Default)]
pub struct AttachmentModels {
    /// Keyed by the folded path, the same way `server/`'s sequence table is:
    /// map data spells a model path with either slash and either case.
    models: HashMap<String, AttachmentModel>,
}

impl AttachmentModels {
    /// Records one model's attachment points, if it has any.
    ///
    /// Filed under `path` rather than under the model's own
    /// `studiohdr_t::name`, because the string an entity kept is the path it
    /// asked for — the two differ on 313 of the game's models.
    ///
    /// **Needs no GPU**, which is the point of it being a method rather than
    /// part of [`EntityModels::load`]: the depot test that measures this over
    /// the whole game has no device, and a measurement made against a copy of
    /// this code would not be a measurement of it.
    pub fn insert_model(&mut self, path: &str, model: &crate::studio::StudioModel) {
        if model.attachments.is_empty() {
            return;
        }
        self.models.insert(
            fold_model(path),
            AttachmentModel {
                attachments: model.attachments.clone(),
                bones: model.bones.clone(),
                sequences: model
                    .sequences
                    .iter()
                    .map(|s| AttachmentSequence {
                        label: s.label.clone(),
                        anim: s.anim,
                        looping: s.flags & crate::studio::anim::STUDIO_LOOPING != 0,
                    })
                    .collect(),
                animations: Arc::clone(&model.animations),
            },
        );
    }

    /// `Studio_FindAttachment` (`bone_utils.cpp:3543`) — zero-based, and
    /// `None` for a model this does not have or a name it does not carry.
    pub fn lookup(&self, model: &str, name: &str) -> Option<usize> {
        self.models
            .get(&fold_model(model))?
            .attachments
            .iter()
            .position(|a| a.name.eq_ignore_ascii_case(name))
    }

    /// `CBaseAnimating::GetAttachment( iAttachment, attachmentToWorld )`
    /// (`baseanimating.cpp:2120`), in the model's own frame.
    ///
    /// Poses the whole skeleton for one attachment, which is what the original
    /// does too — `GetBoneTransform` runs `SetupBones` for every bone and
    /// caches it per entity per frame. There is no such cache here and there
    /// is no need for one yet: the callers that ask repeatedly ask for the
    /// same attachment of the same parent, and
    /// [`hierarchy`](crate::server::hierarchy) caches the answer across a
    /// parent's whole child list.
    ///
    /// > **The cycle is derived here and not taken as given**, through the
    /// > same [`advance_cycle`] the drawn pose goes through. The server's
    /// > `m_flCycle` is a checkpoint written when something decides something
    /// > — `CDynamicProp::AnimThink` stops re-arming once an animation is over
    /// > — so between writes it is stale, and only the side that owns the
    /// > `.mdl` knows the duration that turns it into a pose. Deriving it
    /// > anywhere else is how an attachment ends up somewhere the picture is
    /// > not.
    pub fn attachment_to_model(
        &self,
        model: &str,
        attachment: usize,
        sequence: &str,
        cycle: f32,
        anim_time: f32,
        playback_rate: f32,
        now: f32,
    ) -> Option<Mat4> {
        let model = self.models.get(&fold_model(model))?;
        let attachment = model.attachments.get(attachment)?;
        let found = model
            .sequences
            .iter()
            .find(|held| held.label.eq_ignore_ascii_case(sequence))
            .and_then(|held| Some((model.animations.get(held.anim)?, held.looping)));

        let (anim, cycle) = match found {
            // A sequence with no length cannot advance, which is
            // `EntityModels::cycle`'s guard and this one's for the same reason.
            Some((anim, looping)) if anim.duration() > 0.0 => (
                Some(anim),
                advance_cycle(
                    cycle,
                    (now - anim_time).max(0.0),
                    playback_rate,
                    anim.duration(),
                    looping,
                ),
            ),
            Some((anim, _)) => (Some(anim), cycle),
            // No such label: the bind pose, which is what the renderer draws
            // for it too.
            None => (None, cycle),
        };

        let bones = crate::studio::anim::bone_to_model(&model.bones, anim, cycle);
        crate::studio::anim::attachment_to_model(attachment, &bones)
    }

    /// How many models this describes. For the startup log.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}

/// The key form a model path is filed under: lower case, and `\` as `/`.
///
/// `V_FixSlashes` plus `stricmp`, the same rule
/// [`SequenceTable`](crate::server::sequences::SequenceTable) applies one
/// layer up, so that the table a model was filed under matches the string the
/// entity kept.
fn fold_model(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '\\' => '/',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

/// Every studio model the map's entities place, uploaded and placed.
#[derive(Default)]
pub struct EntityModels {
    models: Vec<PropModel>,
    instances: Vec<Instance>,
    /// [`Instance::id`] to its index, so that a sync is a lookup rather than a
    /// scan over a map's several hundred instances.
    by_id: HashMap<u64, usize>,
    /// The black colour stream every instance binds in slot 1.
    ///
    /// An entity model has no `.vhv` — `vrad` bakes per-vertex lighting for
    /// *static* props and nothing else — so every one of these is lit by its
    /// ambient cube alone and `ModelLighting::static_light` stays 0. Something
    /// must still be bound, because the vertex layout says so.
    unlit: Option<VertexBuffer>,
    refracts: bool,
    /// The attachment points of whichever of [`models`](Self::models) have
    /// any, shared with the game server rather than copied to it.
    attachments: Arc<AttachmentModels>,
    pub stats: EntityModelStats,
}

impl EntityModels {
    /// Reads, uploads and places every model the given entities name.
    ///
    /// **Cannot fail**, for the same reason `PropModels::load` cannot: a model
    /// that will not load is an entity that does not draw, and the shipped
    /// engine logs and carries on.
    ///
    /// `lighting` and `collision` light each instance where it stands. That
    /// happens **once, here** — a `prop_floor_button` does not move, and
    /// nothing that does yet draws a model — so the condition for resampling
    /// per frame is the first entity model that travels.
    ///
    /// An entity's model never wears `vrad`'s per-vertex bake — only a
    /// `prop_static` gets a `.vhv` — so the light cache's answer is the whole
    /// of its lighting, ambient cube and local lights together, with no
    /// `bStaticLighting` question to ask.
    pub fn load(
        vfs: &Vfs,
        materials: &mut MaterialCache,
        device: &wgpu::Device,
        entities: &[ModelEntity],
        lighting: &LightCache,
        collision: &CollisionBsp,
    ) -> EntityModels {
        let mut stats = EntityModelStats::default();
        // One for the whole list, for the reason `Props::light` makes one for
        // the whole map: a `Tracer` allocates a stamp per brush.
        let mut tracer = collision.tracer();
        let mut models: Vec<PropModel> = Vec::new();
        let mut by_name: HashMap<String, Option<usize>> = HashMap::new();
        let mut resolved: HashMap<String, Arc<Material>> = HashMap::new();
        let error = materials.error_model_material();
        let mut instances = Vec::new();
        // Parallel to `models`: whether that model's sequences came out of a
        // `$includemodel` companion. Kept here rather than on `PropModel`
        // because nothing but the startup log ever asks.
        let mut from_include: Vec<bool> = Vec::new();
        let mut attachments = AttachmentModels::default();

        for entity in entities {
            let key = entity.model.to_ascii_lowercase();
            let slot = *by_name.entry(key).or_insert_with(|| {
                let model = match StudioModel::load(vfs, &entity.model) {
                    Ok(model) => model,
                    Err(e) => {
                        eprintln!("source-engine: entity models: {e}");
                        stats.models_missing += 1;
                        return None;
                    }
                };
                if model.vertices.is_empty() || model.indices.is_empty() {
                    stats.models_missing += 1;
                    return None;
                }
                if model.bones.len() > 1 && model.rigid_bones().is_none() {
                    // Drawn in its bind pose rather than wrongly: the per-bone
                    // split cannot express a vertex two bones share. No model
                    // the port loads reaches this.
                    eprintln!(
                        "source-engine: entity models: {}: vertices are shared between bones; \
                         drawing it unanimated",
                        model.path
                    );
                    stats.models_not_rigid += 1;
                }

                let batches: Vec<PropBatch> = model
                    .batches
                    .iter()
                    .map(|batch| PropBatch {
                        material: resolve_material(
                            vfs,
                            materials,
                            &mut resolved,
                            &error,
                            &batch.material,
                        ),
                        first_index: batch.first_index,
                        index_count: batch.index_count,
                        bones: batch.bones.clone(),
                    })
                    .collect();

                stats.models += 1;
                stats.triangles += model.indices.len() / 3;
                if !model.includes.is_empty() {
                    stats.models_with_includes += 1;
                }
                from_include.push(!model.includes.is_empty());
                // Before the upload, which consumes the model. Only the
                // models something can be parented *to* are kept: an
                // attachment table with no attachments in it answers nothing.
                attachments.insert_model(&model.path, &model);
                models.push(PropModel::upload(device, model, batches));
                Some(models.len() - 1)
            });

            let Some(slot) = slot else { continue };
            let model = &models[slot];
            stats.instances += 1;
            if model.bones.len() > 1 {
                stats.animated += 1;
            }
            if from_include[slot] {
                stats.instances_with_includes += 1;
            }
            instances.push(Instance {
                id: entity.id,
                model: slot,
                transform: Mat4::from_translation(entity.origin)
                    * Mat4::from_mat3(crate::math::angle_matrix(entity.angles)),
                visible: entity.visible,
                sequence: model.sequence(&entity.sequence).unwrap_or(0),
                cycle: entity.cycle,
                anim_time: entity.anim_time,
                playback_rate: entity.playback_rate,
                modulation: entity.modulation,
                lighting: lighting.lighting_at(&mut tracer, entity.origin),
            });
        }

        let widest = models.iter().map(|m| m.vertex_count).max().unwrap_or(0);
        let refracts = instances.iter().any(|instance| {
            models[instance.model]
                .batches
                .iter()
                .any(|batch| batch.material.needs_frame_buffer_copy)
        });

        let by_id = instances
            .iter()
            .enumerate()
            .map(|(i, instance)| (instance.id, i))
            .collect();

        EntityModels {
            by_id,
            unlit: (widest > 0).then(|| {
                VertexBuffer::new(
                    device,
                    "entity model lighting (none)",
                    &vec![StaticLightVertex::UNLIT; widest],
                )
            }),
            models,
            instances,
            refracts,
            attachments: Arc::new(attachments),
            stats,
        }
    }

    /// The attachment points of the models this level placed.
    ///
    /// Shared rather than copied: the game server holds this for the level's
    /// lifetime and the draw path keeps drawing from the same animations.
    pub fn attachments(&self) -> Arc<AttachmentModels> {
        Arc::clone(&self.attachments)
    }

    /// Takes each entity's placement, visibility and pose from whoever owns
    /// them — the game server — once a frame.
    ///
    /// **Matched by [`ModelEntity::id`], not by position.** It was positional
    /// while `prop_floor_button` was the only class that placed a model, on
    /// the grounds that nothing created or destroyed one after the spawn pass;
    /// `prop_dynamic` ends that, with 556 shipped `Kill` connections and 51
    /// `FadeAndKill`s. An instance whose id is missing from this frame's list
    /// is made **invisible and kept**, so that the model it uploaded — which
    /// its neighbours are very likely sharing — stays valid.
    ///
    /// An entity that is in the list and was not in [`load`](EntityModels::load)'s
    /// is ignored: its model was never read, so there is nothing to draw. Only
    /// something created at run time can be in that position, and nothing that
    /// places a model is.
    ///
    /// **The placement is taken too**, so that an entity model on a moving
    /// platform follows it — its *lighting* does not, which is the limitation
    /// [`load`](EntityModels::load) records.
    pub fn sync(&mut self, entities: &[ModelEntity]) {
        for instance in &mut self.instances {
            instance.visible = false;
        }
        for entity in entities {
            let Some(&at) = self.by_id.get(&entity.id) else {
                continue;
            };
            let instance = &mut self.instances[at];
            instance.transform = Mat4::from_translation(entity.origin)
                * Mat4::from_mat3(crate::math::angle_matrix(entity.angles));
            instance.visible = entity.visible;
            instance.sequence = self.models[instance.model]
                .sequence(&entity.sequence)
                .unwrap_or(0);
            instance.cycle = entity.cycle;
            instance.anim_time = entity.anim_time;
            instance.playback_rate = entity.playback_rate;
            instance.modulation = entity.modulation;
        }
    }

    /// Every sequence of every model loaded here, for the table the game reads
    /// durations out of.
    ///
    /// The other direction of the same seam: `world/` tells `server/` what the
    /// `.mdl`s say, once, so that `AnimThink` can tell when an animation has
    /// finished without this module and that one naming each other's types.
    /// See `crate::server::sequences`.
    pub fn sequences(&self) -> impl Iterator<Item = SequenceRow<'_>> + '_ {
        self.models.iter().flat_map(|model| {
            model.sequences.iter().map(move |sequence| SequenceRow {
                model: model.name.as_str(),
                label: sequence.label.as_str(),
                duration: model
                    .animations
                    .get(sequence.anim)
                    .map(|anim| anim.duration())
                    .unwrap_or(0.0),
                loops: sequence.flags & crate::studio::anim::STUDIO_LOOPING != 0,
                fade_out_time: sequence.fade_out_time,
            })
        })
    }

    /// A one-line summary for the startup log.
    pub fn summary(&self) -> String {
        let s = &self.stats;
        let mut out = format!(
            "{} entity models ({} models, {} triangles, {} animated)",
            s.instances, s.models, s.triangles, s.animated
        );
        if s.models_missing > 0 {
            out += &format!(", {} missing", s.models_missing);
        }
        if s.models_not_rigid > 0 {
            out += &format!(", {} not rigid (drawn unanimated)", s.models_not_rigid);
        }
        if s.models_with_includes > 0 {
            out += &format!(
                ", {} model(s) animated by a $includemodel ({} instances)",
                s.models_with_includes, s.instances_with_includes
            );
        }
        out
    }

    /// Whether any of them wears a material that reads the frame it is drawn
    /// into.
    pub fn refracts(&self) -> bool {
        self.refracts
    }

    /// Records the opaque half. `curtime` is the scene clock, which is what
    /// each instance's cycle is measured against.
    pub fn draw(&self, pass: &mut Pass<'_>, curtime: f32, visible: BoxVisible<'_>) {
        self.record(pass, curtime, false, visible);
    }

    /// Records the half whose material samples a copy of the scene — see
    /// [`World::draw_refracting`](super::World::draw_refracting).
    pub fn draw_refracting(&self, pass: &mut Pass<'_>, curtime: f32, visible: BoxVisible<'_>) {
        if self.refracts {
            self.record(pass, curtime, true, visible);
        }
    }

    /// One instance's bounding box in world space.
    ///
    /// `C_BaseAnimating::GetRenderBounds` (`c_baseanimating.cpp:6320`) under
    /// the entity's transform: the model's render bounds — which are
    /// `hull_min`/`hull_max` for **2,033 of the game's 2,041 models**, because
    /// `view_bbmin`/`view_bbmax` is zero unless the model was compiled with an
    /// explicit `$bbox` — merged with the box the compiler measured over the
    /// sequence being played.
    ///
    /// **Both halves are load-bearing and each was found the hard way.**
    /// Without the first the box is degenerate at the entity's origin and the
    /// prop disappears from some viewpoints and not others; without the second
    /// it is the model's *rest* extent, and a posed vertex reaches up to
    /// 24,188 units outside that
    /// (`models/a4_destruction/fin3_orangepipeexpl.mdl`, an explosion that
    /// throws debris across the map). Neither shows up as an error — the prop
    /// simply is not drawn.
    ///
    /// The sequence box covers the *whole* sequence rather than this
    /// instance's cycle, which is Valve's and is what keeps this cheap: no
    /// pose is computed for an entity that is then culled.
    fn world_bounds(&self, instance: &Instance) -> (Vec3, Vec3) {
        let model = &self.models[instance.model];
        cull_box(
            model.bounds,
            model.sequences.get(instance.sequence).map(|s| s.bounds),
            instance.transform,
        )
    }

    /// Offers every batch of every visible instance that belongs in the
    /// translucent pass, with the world-space centre the sort wants.
    ///
    /// The centre is the model's `view_bbmin`/`view_bbmax` centre under the
    /// entity's transform — **not under its pose**. A bone that has moved is
    /// not accounted for, which for a sort of whole entities against each other
    /// is below the resolution of the answer: the largest travel in the port's
    /// animated models is a chamber door's 53-unit slide, against models tens of
    /// units across placed metres apart.
    pub(crate) fn collect_translucent(&self, out: &mut dyn FnMut(Vec3, usize, usize)) {
        for (index, instance) in self.instances.iter().enumerate() {
            if !instance.visible {
                continue;
            }
            let model = &self.models[instance.model];
            let center = instance
                .transform
                .transform_point3((model.bounds.0 + model.bounds.1) * 0.5);
            for (batch_index, batch) in model.batches.iter().enumerate() {
                let pass =
                    GeometryPass::of_instance(&batch.material, instance.modulation[3] != 1.0);
                if pass == GeometryPass::Translucent {
                    out(center, index, batch_index);
                }
            }
        }
    }

    /// Records one (instance, batch) — the translucent list's unit of work.
    pub(crate) fn draw_one(
        &self,
        pass: &mut Pass<'_>,
        curtime: f32,
        instance: usize,
        batch: usize,
    ) {
        let instance = &self.instances[instance];
        let model = &self.models[instance.model];
        let Some(unlit) = self
            .unlit
            .as_ref()
            .map(|buffer| buffer.range(0, model.vertex_count as u32))
        else {
            return;
        };
        pass.set_model_lighting(&instance.lighting);
        pass.bind_static_light(&unlit);
        let pose = model.pose(instance.sequence, self.cycle(instance, curtime));
        self.record_batch(pass, model, instance, &pose, &model.batches[batch]);
    }

    /// One instance's one batch, bone run by bone run.
    fn record_batch(
        &self,
        pass: &mut Pass<'_>,
        model: &PropModel,
        instance: &Instance,
        pose: &[Mat4],
        batch: &PropBatch,
    ) {
        let vertices = model.vertices.slice();
        for run in &batch.bones {
            let bone = pose
                .get(usize::from(run.bone))
                .copied()
                .unwrap_or(Mat4::IDENTITY);
            let indices = model.indices.range(run.first_index, run.index_count);
            pass.draw_modulated(
                &batch.material,
                &vertices,
                &indices,
                instance.transform * bone,
                instance.modulation,
            );
        }
    }

    fn record(&self, pass: &mut Pass<'_>, curtime: f32, refracting: bool, visible: BoxVisible<'_>) {
        let wanted = match refracting {
            true => GeometryPass::Refracting,
            false => GeometryPass::Opaque,
        };
        for instance in &self.instances {
            if !instance.visible {
                continue;
            }
            let (mins, maxs) = self.world_bounds(instance);
            if !visible(mins, maxs) {
                continue;
            }
            let model = &self.models[instance.model];
            let Some(unlit) = self
                .unlit
                .as_ref()
                .map(|buffer| buffer.range(0, model.vertex_count as u32))
            else {
                continue;
            };

            // One pose per instance per frame: three `Mat4`s for a button.
            // Taken before the batch loop because every batch of one instance
            // shares it.
            let pose = model.pose(instance.sequence, self.cycle(instance, curtime));

            pass.set_model_lighting(&instance.lighting);
            pass.bind_static_light(&unlit);

            for batch in &model.batches {
                let translucent = instance.modulation[3] != 1.0;
                if GeometryPass::of_instance(&batch.material, translucent) != wanted {
                    continue;
                }
                self.record_batch(pass, model, instance, &pose, batch);
            }
        }
    }

    /// `C_BaseAnimating::FrameAdvance` — how far through the sequence we are,
    /// from where the entity says it was and how long ago that was.
    ///
    /// `StudioFrameAdvance`'s `m_flCycle += dt * rate / duration`, integrated
    /// rather than stepped: the server writes a cycle, a time and a rate and
    /// this solves for now, which is what lets a 64 Hz server drive a smooth
    /// animation. **`DynamicProp::cycle_now` computes the same expression**
    /// against the same five numbers so that `AnimThink` can tell when a
    /// sequence has finished; the two must not drift.
    ///
    /// > **A sequence that is not `STUDIO_LOOPING` clamps and stays there.**
    /// > That is what makes a floor button *stay* pressed: `down` is 11 frames
    /// > at 24 fps, and at 0.42 seconds the plate has arrived and the pose
    /// > stops changing. A looping one wraps instead — and wraps *backwards*
    /// > too, which is why this is `rem_euclid` and not `fract`: 427 shipped
    /// > connections set a playback rate of `-1`, and `fract` on a negative
    /// > number is negative.
    fn cycle(&self, instance: &Instance, curtime: f32) -> f32 {
        let model = &self.models[instance.model];
        let sequence = instance.sequence;
        let Some(anim) = model.animation(sequence) else {
            return 0.0;
        };
        let duration = anim.duration();
        if duration <= 0.0 {
            return instance.cycle;
        }
        let elapsed = (curtime - instance.anim_time).max(0.0);
        let loops = model.sequences[sequence].flags & crate::studio::anim::STUDIO_LOOPING != 0;
        advance_cycle(
            instance.cycle,
            elapsed,
            instance.playback_rate,
            duration,
            loops,
        )
    }
}

/// `m_flCycle` advanced by `elapsed` seconds — the arithmetic half of
/// [`EntityModels::cycle`], split out because it is where one bug lives and it
/// needs no GPU to test.
///
/// > **`elapsed` is a difference between two clocks and they must share an
/// > origin.** `curtime` is the scene's and `anim_time` is stamped by the
/// > server, and `Scene::curtime` is reset with the level precisely so that
/// > they do. Get that wrong and this function is where it shows: a
/// > **non-looping** sequence is pinned at whichever end `clamp` reaches on
/// > the first frame, so it snaps between two poses, while a **looping** one
/// > is unharmed because `rem_euclid` turns a constant offset into a phase
/// > shift. That asymmetry is the whole diagnostic — a level where the fans
/// > spin and the doors teleport between open and shut is this, and nothing
/// > else.
fn advance_cycle(base: f32, elapsed: f32, rate: f32, duration: f32, loops: bool) -> f32 {
    let cycle = base + elapsed * rate / duration;
    match loops {
        true => cycle.rem_euclid(1.0),
        false => cycle.clamp(0.0, 1.0),
    }
}

/// The material a batch wears, refusing one whose shader cannot take model
/// geometry.
///
/// The same rule `PropModels::load` applies, and for the same reason: a prop's
/// geometry is `ModelVertex` and nothing else, so a brush shader on it would be
/// a validation failure rather than a wrong picture.
fn resolve_material(
    vfs: &Vfs,
    materials: &mut MaterialCache,
    resolved: &mut HashMap<String, Arc<Material>>,
    error: &Arc<Material>,
    name: &str,
) -> Arc<Material> {
    resolved
        .entry(name.to_owned())
        .or_insert_with(|| {
            let material = materials.load(vfs, name);
            if Arc::ptr_eq(&material, &materials.error_material())
                || material.shader.vertex_layout() != VertexLayout::Model
            {
                return Arc::clone(error);
            }
            material
        })
        .clone()
}

/// The world-space cull box of one instance — [`EntityModels::world_bounds`]'
/// arithmetic, as a free function so it can be tested without a GPU.
///
/// `model` is the model's render bounds and `sequence` the box of the sequence
/// being played, both in model space; `transform` is the entity's placement.
/// The eight corners are transformed and re-bounded rather than the two
/// extremes, because a rotated box's extent is not its rotated extremes.
fn cull_box(
    model: (Vec3, Vec3),
    sequence: Option<(Vec3, Vec3)>,
    transform: Mat4,
) -> (Vec3, Vec3) {
    let (mut mins, mut maxs) = model;
    if let Some((lo, hi)) = sequence {
        mins = mins.min(lo);
        maxs = maxs.max(hi);
    }
    let mut bounds = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for corner in 0..8u32 {
        let pick = |axis: usize| match corner >> axis & 1 {
            0 => mins[axis],
            _ => maxs[axis],
        };
        let point = transform.transform_point3(Vec3::new(pick(0), pick(1), pick(2)));
        bounds = (bounds.0.min(point), bounds.1.max(point));
    }
    (
        bounds.0 - Vec3::splat(BOUNDS_SLACK),
        bounds.1 + Vec3::splat(BOUNDS_SLACK),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::context::{Camera, Load, RenderContext};
    use crate::materials::target::RenderTarget;
    use crate::server::Server;

    const SIZE: u32 = 256;

    /// **A model with no clipping box still gets a cull box around its
    /// geometry.**
    ///
    /// The regression this guards is the one that made `prop_dynamic`s wink
    /// out at particular angles: `view_bbmin`/`view_bbmax` is zero on **2,033
    /// of the game's 2,041 models**, so taking it straight gave a degenerate
    /// box at the entity's origin, padded by a constant, and a prop longer
    /// than that pad vanished as soon as its *origin* left the frustum.
    /// `Mdl::render_bounds` falls back to `hull_min`/`hull_max` the way
    /// `CModelInfo::GetModelRenderBounds` does, and this is the shape of the
    /// answer that has to come out the other end.
    #[test]
    fn the_cull_box_covers_a_model_that_declared_no_clipping_box() {
        // A 256-unit girder lying along +X, as `hull_min`/`hull_max` would
        // describe it once `render_bounds` has fallen back to them.
        let hull = (Vec3::new(-8.0, -8.0, -8.0), Vec3::new(256.0, 8.0, 8.0));
        let at = Vec3::new(1000.0, 2000.0, 64.0);
        let (mins, maxs) = cull_box(hull, None, Mat4::from_translation(at));
        assert!(
            mins.x <= at.x - 8.0 && maxs.x >= at.x + 256.0,
            "the box {mins} .. {maxs} does not reach the far end of the girder"
        );

        // Rotated a quarter turn about Z, the girder runs along +Y and the
        // box has to follow it. This is what the eight-corner transform buys
        // over transforming the two extremes.
        let turned = Mat4::from_translation(at) * Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let (mins, maxs) = cull_box(hull, None, turned);
        assert!(
            maxs.y >= at.y + 256.0 && maxs.x <= at.x + 16.0,
            "the box {mins} .. {maxs} did not turn with the model"
        );
    }

    /// **How many of the game's `prop_dynamic`s the old cull box would have
    /// dropped.**
    ///
    /// The regression report was "many `prop_dynamic`s disappear when looking
    /// at them from a particular angle and position, it varies from prop to
    /// prop". This is that sentence as a number: a placement wears a model
    /// whose drawn extent reaches outside the box the old code built — a
    /// degenerate box at the entity's origin, padded by 64 — and every one of
    /// those could wink out as soon as its origin left the frustum or the
    /// narrowed frustum of an areaportal it was seen through.
    ///
    /// CPU only: the entity lump and the `.mdl` headers, no device.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_old_cull_box -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn the_old_cull_box_would_have_dropped_most_of_the_games_props() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = crate::filesystem::Vfs::mount_game(&dir, &base, &Default::default())
            .expect("mount the game");

        let mut names: Vec<String> = vfs
            .list("maps")
            .expect("maps/")
            .into_iter()
            .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
            .map(|e| e.name.trim_end_matches(".bsp").to_owned())
            .collect();
        names.sort();

        // The old pad, kept as a literal so that changing `BOUNDS_SLACK` does
        // not quietly change what this measures.
        const OLD_SLACK: f32 = 64.0;
        let mut headers: std::collections::HashMap<String, Option<(Vec3, Vec3)>> =
            std::collections::HashMap::new();
        let (mut placements, mut would_drop) = (0usize, 0usize);
        let mut worst = (0.0f32, String::new());
        for name in &names {
            let Ok(bsp) = crate::engine::world::bsp::Bsp::load(&vfs, name) else {
                continue;
            };
            for entity in bsp.entities() {
                if entity.classname() != Some("prop_dynamic") {
                    continue;
                }
                let Some(model) = entity.get("model") else {
                    continue;
                };
                let model = model.to_ascii_lowercase().replace('\\', "/");
                let bounds = headers
                    .entry(model.clone())
                    .or_insert_with(|| {
                        let bytes = vfs.read(&model).ok()?;
                        let mdl = crate::studio::Mdl::parse(model.clone(), &bytes).ok()?;
                        let (mut lo, mut hi) = mdl.render_bounds();
                        for sequence in &mdl.sequences {
                            lo = lo.min(sequence.bounds.0);
                            hi = hi.max(sequence.bounds.1);
                        }
                        Some((lo, hi))
                    })
                    .to_owned();
                let Some((lo, hi)) = bounds else { continue };
                placements += 1;
                // The old box was `(0,0,0)..(0,0,0)` padded by 64 — this
                // model's geometry escapes it by this much.
                let out = (-lo - Vec3::splat(OLD_SLACK))
                    .max(hi - Vec3::splat(OLD_SLACK))
                    .max_element();
                if out > 0.0 {
                    would_drop += 1;
                    if out > worst.0 {
                        worst = (out, model);
                    }
                }
            }
        }

        println!(
            "{would_drop} of {placements} prop_dynamic placements wear a model that \
             escapes the old cull box (worst by {:.0} units, {})",
            worst.0, worst.1
        );
        assert!(placements > 5_000, "only {placements} placements");
        assert!(
            would_drop * 2 > placements,
            "only {would_drop} of {placements} placements were affected — \
             if this ever drops, the fallback in Mdl::render_bounds stopped mattering"
        );
    }

    /// **The playing sequence's own box is merged in.**
    ///
    /// `C_BaseAnimating::GetRenderBounds` does this and it is not a
    /// refinement: a model's rest extent does not contain its animated
    /// geometry. Measured over the depot, a posed vertex reaches up to 23,029
    /// units outside the model's own bounds.
    #[test]
    fn the_cull_box_follows_the_sequence_the_entity_plays() {
        let model = (Vec3::splat(-16.0), Vec3::splat(16.0));
        let travel = (Vec3::new(-16.0, -16.0, -16.0), Vec3::new(16.0, 16.0, 512.0));
        let (_, rest) = cull_box(model, None, Mat4::IDENTITY);
        let (_, moving) = cull_box(model, Some(travel), Mat4::IDENTITY);
        assert!(rest.z < 64.0, "the rest box should be the model's own");
        assert!(
            moving.z >= 512.0,
            "the sequence's 512-unit climb is not in the cull box: {moving}"
        );
    }

    /// A button's `down` is 0.4167 s and a chamber door's `open` 0.9167 s.
    /// Both are non-looping, so both ramp — and both are pinned at the far end
    /// by a clock offset of a second or more.
    const BUTTON_DOWN: f32 = 11.0 / 24.0;

    #[test]
    fn a_non_looping_sequence_ramps_across_its_duration() {
        let at = |t: f32| advance_cycle(0.0, t, 1.0, BUTTON_DOWN, false);
        assert_eq!(at(0.0), 0.0);
        assert!((at(BUTTON_DOWN / 2.0) - 0.5).abs() < 1e-6, "{}", at(0.229));
        assert_eq!(at(BUTTON_DOWN), 1.0);
        // …and holds at the end rather than wrapping.
        assert_eq!(at(BUTTON_DOWN * 3.0), 1.0);
    }

    /// **The regression that made every door and button in the game snap
    /// between two poses, and the asymmetry that identifies it.**
    ///
    /// `Scene::curtime` used to be reset at startup and never again, while
    /// `Server::level_shutdown` reset the server's clock with every map — so
    /// after one level change the two disagreed by however long the previous
    /// level had run (measured: 0.115 s on the first map, **15.03 s** one
    /// reload later). Every `anim_time` the server stamps is in its frame of
    /// reference and every `curtime` the renderer reads is in the other.
    ///
    /// A looping sequence survives that untouched, which is why a spinning fan
    /// looked perfect in the same room as a door that teleported open.
    #[test]
    fn a_stale_clock_pins_a_non_looping_sequence_and_a_looping_one_shrugs_it_off() {
        // One level's worth of skew, from the measurement that found this.
        const SKEW: f32 = 15.028;

        // The button never shows a middle pose again: it is at the end on the
        // first frame it is drawn.
        assert_eq!(advance_cycle(0.0, SKEW, 1.0, BUTTON_DOWN, false), 1.0);
        // …and played backwards it is pinned at the other end just as hard,
        // which is a door told to `Close`.
        assert_eq!(advance_cycle(1.0, SKEW, -1.0, BUTTON_DOWN, false), 0.0);

        // The fan is merely at a different phase, which nobody can see.
        let spun = advance_cycle(0.0, SKEW, 1.0, BUTTON_DOWN, true);
        assert!((0.0..1.0).contains(&spun), "{spun}");
        // And it still *advances*: a later frame is a different pose.
        let later = advance_cycle(0.0, SKEW + 0.05, 1.0, BUTTON_DOWN, true);
        assert!((spun - later).abs() > 1e-3, "{spun} vs {later}");
    }

    /// With the clocks sharing an origin the offset is one frame's worth, and
    /// a button is still part-way through its press rather than finished.
    #[test]
    fn a_levels_worth_of_reset_leaves_the_press_visible() {
        // The residue after the fix, measured: the load frame is charged to
        // the scene clock and not to the server's.
        const RESIDUE: f32 = 0.116;
        let cycle = advance_cycle(0.0, RESIDUE, 1.0, BUTTON_DOWN, false);
        assert!(cycle > 0.0 && cycle < 1.0, "{cycle}");
    }

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            eprintln!("skipping: adapter has no BC texture support");
            return None;
        }
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
            ..Default::default()
        }))
        .ok()
    }

    /// **The floor button, drawn — and drawn differently as it presses.**
    ///
    /// The one test that can say the whole path works, because every step of
    /// it is invisible on its own: the `.mdl` could load and never reach a
    /// pass, the bone runs could be drawn under the wrong matrices, the pose
    /// could be composed on the wrong side of the placement, or the material
    /// could have fallen back to the error checkerboard. All of those draw
    /// *something*; only one of them draws something that **changes when the
    /// animation does**.
    ///
    /// It renders `sp_a1_intro1`'s button from a camera a few feet in front of
    /// it, at the two ends of `down`, and compares the images.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_button_draws -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install and a GPU; set KISAK_GAME_DIR"]
    fn the_button_draws_and_moves_as_it_presses() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let Some((device, queue)) = device() else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let mut materials = MaterialCache::new(&device, &queue);
        let map = std::env::var("KISAK_MAP").unwrap_or_else(|_| "sp_a1_intro1".to_owned());
        let mut world =
            super::super::World::load(&vfs, &mut materials, &device, &map).expect("the map loads");

        // The game half: spawn the entities, then ask them what models they
        // place — the same two calls `Level::load` makes.
        let mut server = Server::new();
        server.level_init(&map, &world.entities, &world.models);
        // **Only the floor button.** `sp_a1_intro1` places 90 `prop_dynamic`s
        // as well as its one pad, and drawing them would swamp the pixel
        // comparison below — this module is what is under test, not the map.
        //
        // By model path, because the seam carries no classname and does not
        // need one; the `placements.len() == 1` below is what checks it. It is
        // a *prefix*, and that is load-bearing: the game's 65 floor buttons
        // wear three models — `portal_button.mdl` (47),
        // `portal_button_damaged01.mdl` (10) and `..._damaged02.mdl` (8) — and
        // **`sp_a1_intro1`'s is a damaged one**. All three animate `up` and
        // `down` the same way.
        const BUTTON: &str = "models/props/portal_button";
        let placements: Vec<ModelEntity> = server
            .model_entities()
            .into_iter()
            .filter(|e| e.model.to_ascii_lowercase().starts_with(BUTTON))
            .map(|e| ModelEntity {
                id: e.id,
                model: e.model,
                origin: e.origin,
                angles: e.angles,
                skin: e.skin,
                visible: e.visible,
                sequence: e.sequence,
                cycle: e.cycle,
                anim_time: e.anim_time,
                playback_rate: e.playback_rate,
                modulation: e.modulation,
            })
            .collect();
        assert!(
            !placements.is_empty(),
            "{map} places no model entity; try KISAK_MAP=sp_a1_intro1"
        );
        println!("{} model entities:", placements.len());
        for p in &placements {
            println!("  {} at {:?} angles {:?}", p.model, p.origin, p.angles);
        }

        world.load_entity_models(&vfs, &mut materials, &device, &placements);
        println!("{}", world.entity_models.summary());
        assert_eq!(world.entity_models.stats.models_missing, 0);
        assert!(world.entity_models.stats.animated > 0, "nothing to animate");

        assert_eq!(placements.len(), 1, "exactly the button");
        // Look at it from four feet in front and a little above, which is
        // roughly where a player standing next to it would.
        let target_point = placements[0].origin;
        let eye = target_point + Vec3::new(48.0, 48.0, 40.0);
        let camera = Camera::perspective(
            eye,
            glam::Mat4::look_at_rh(eye, target_point, Vec3::Z),
            75.0,
            1.0,
            1.0,
            4096.0,
        );

        let mut context = RenderContext::new(&device, &queue, materials.pipelines());
        let render_target = RenderTarget::new(
            &device,
            "button",
            SIZE,
            SIZE,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            true,
        );

        // Only the button: the world around it would swamp the comparison, and
        // what is under test is this module.
        let mut shot = |models: &EntityModels, curtime: f32| -> Vec<u8> {
            context.begin_frame();
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size: (SIZE * SIZE * 4) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = context.offscreen_pass(
                    &mut encoder,
                    materials.pipelines(),
                    &render_target,
                    &camera,
                    Load::Clear(wgpu::Color::BLACK),
                );
                models.draw(&mut pass, curtime, &|_, _| true);
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: render_target.color_texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(SIZE * 4),
                        rows_per_image: Some(SIZE),
                    },
                },
                wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
            readback.slice(..).map_async(wgpu::MapMode::Read, |r| {
                r.expect("readback mapped");
            });
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("the queue drained");
            let pixels = readback.slice(..).get_mapped_range().unwrap().to_vec();
            readback.unmap();
            pixels
        };

        // Up: the sequence the button spawned with, long finished.
        let up = shot(&world.entity_models, 10.0);
        // Against a black clear, so "drawn" is "not still the clear colour".
        // The alpha channel cannot say: `Load::Clear(BLACK)` writes alpha 1.
        let drawn = up.chunks_exact(4).filter(|p| p[0..3] != [0, 0, 0]).count();
        println!("up: {drawn} of {} pixels drawn", SIZE * SIZE);
        assert!(
            drawn > 500,
            "the button drew {drawn} pixels; it is not on screen"
        );

        // Now press it, and look at the two ends of `down`.
        world.entity_models.sync(&[ModelEntity {
            sequence: "down".to_owned(),
            anim_time: 100.0,
            ..placements[0].clone()
        }]);
        let pressing = shot(&world.entity_models, 100.0);
        let pressed = shot(&world.entity_models, 101.0);

        let differences = |a: &[u8], b: &[u8]| {
            a.chunks_exact(4)
                .zip(b.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count()
        };
        let moved = differences(&pressing, &pressed);
        println!("down: {moved} pixels differ between cycle 0 and cycle 1");
        assert!(
            moved > 100,
            "the plate drew identically at both ends of `down`: {moved} pixels differ"
        );

        // …and the start of `down` is the end of `up`, because the two are the
        // same travel in opposite directions. This is what says the pose is
        // being applied to the right geometry rather than just to *some*
        // geometry: a wrong bone or a wrong matrix order would not line these
        // two up.
        let same = differences(&up, &pressing);
        println!("up (held) vs down (cycle 0): {same} pixels differ");
        assert!(
            same * 100 < (SIZE * SIZE) as usize,
            "a released button and a just-pressed one should look alike: \
             {same} pixels differ"
        );
    }

    /// **The chamber door, drawn — and drawn open, half open and shut.**
    ///
    /// The door is the second model an entity animates in this port and the
    /// first whose animation is a *pair* of moving parts, so it exercises
    /// something the button could not: five bone runs under five different
    /// matrices, of which three move and two do not. Every step of that is
    /// invisible on its own — the leaves could be drawn under each other's
    /// matrices, or under the root's, or the pose could be composed on the
    /// wrong side of the placement — and all of those draw *something*.
    ///
    /// It renders `sp_a1_intro1`'s door from in front, at three points of
    /// `open`, and asks for the one thing only a correct pose gives: the
    /// middle of the doorway is **covered when the door is shut and clear
    /// when it is open**.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_chamber_door_draws -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install and a GPU; set KISAK_GAME_DIR"]
    fn the_chamber_door_draws_and_opens() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let Some((device, queue)) = device() else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let mut materials = MaterialCache::new(&device, &queue);
        let map = "sp_a1_intro1";
        let mut world =
            super::super::World::load(&vfs, &mut materials, &device, map).expect("the map loads");

        let mut server = Server::new();
        server.level_init(map, &world.entities, &world.models);
        // **Only the doors.** `sp_a1_intro1` places 90 `prop_dynamic`s as
        // well, and drawing them would put geometry between the camera and
        // the thing under test.
        const DOOR: &str = "models/props/portal_door_combined";
        let placements: Vec<ModelEntity> = server
            .model_entities()
            .into_iter()
            .filter(|e| e.model.to_ascii_lowercase().starts_with(DOOR))
            .map(|e| ModelEntity {
                id: e.id,
                model: e.model,
                origin: e.origin,
                angles: e.angles,
                skin: e.skin,
                visible: e.visible,
                sequence: e.sequence,
                cycle: e.cycle,
                anim_time: e.anim_time,
                playback_rate: e.playback_rate,
                modulation: e.modulation,
            })
            .collect();
        println!("{} chamber doors:", placements.len());
        for p in &placements {
            println!("  {} at {:?} angles {:?}", p.model, p.origin, p.angles);
        }
        assert_eq!(placements.len(), 2, "sp_a1_intro1 places two");

        // One of them, drawn on its own so that nothing else can be what
        // changed.
        let door = placements[0].clone();
        world.load_entity_models(&vfs, &mut materials, &device, std::slice::from_ref(&door));
        assert_eq!(world.entity_models.stats.models_missing, 0);
        assert_eq!(
            world.entity_models.stats.models_not_rigid, 0,
            "it must pose"
        );
        assert_eq!(world.entity_models.instances.len(), 1);
        println!("{}", world.entity_models.summary());

        // Where to stand. The `.mdl`'s own frame is not the obvious one — the
        // leaves are 65 units apart along its local **x** — so the camera is
        // aimed from the geometry rather than from a guess: take the shut
        // pose's world-space vertex bounds and look at the middle of them
        // from along whichever horizontal axis the door is *thinnest* in,
        // which is the way a doorway is looked through.
        let studio = StudioModel::load(&vfs, &door.model).expect("the door model");
        let bones = studio.rigid_bones().expect("rigid").to_vec();
        // Taken by value so that the borrow ends here: `sync` below needs the
        // list mutably, and the pose of a held door does not change.
        let (transform, shut, opened) = {
            let instance = &world.entity_models.instances[0];
            let model = &world.entity_models.models[instance.model];
            (
                instance.transform,
                model.pose(instance.sequence, 0.0),
                model.pose(instance.sequence, 1.0),
            )
        };
        let world_vertex = |i: usize, pose: &[Mat4]| {
            let bone = pose
                .get(usize::from(bones[i]))
                .copied()
                .unwrap_or(Mat4::IDENTITY);
            (transform * bone).transform_point3(Vec3::from(studio.vertices[i].position))
        };
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for i in 0..studio.vertices.len() {
            let p = world_vertex(i, &shut);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        let middle = (lo + hi) * 0.5;
        let size = hi - lo;
        println!("shut bounds {lo:?}..{hi:?} (size {size:?})");
        let back = match size.x < size.y {
            true => Vec3::X,
            false => Vec3::Y,
        };
        let eye = middle + back * 220.0;
        let camera = Camera::perspective(
            eye,
            glam::Mat4::look_at_rh(eye, middle, Vec3::Z),
            75.0,
            1.0,
            1.0,
            4096.0,
        );

        let mut context = RenderContext::new(&device, &queue, materials.pipelines());
        let render_target = RenderTarget::new(
            &device,
            "chamber door",
            SIZE,
            SIZE,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            true,
        );

        let mut shot = |models: &EntityModels, curtime: f32| -> Vec<u8> {
            context.begin_frame();
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size: (SIZE * SIZE * 4) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = context.offscreen_pass(
                    &mut encoder,
                    materials.pipelines(),
                    &render_target,
                    &camera,
                    Load::Clear(wgpu::Color::BLACK),
                );
                models.draw(&mut pass, curtime, &|_, _| true);
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: render_target.color_texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(SIZE * 4),
                        rows_per_image: Some(SIZE),
                    },
                },
                wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
            readback.slice(..).map_async(wgpu::MapMode::Read, |r| {
                r.expect("readback mapped");
            });
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("the queue drained");
            let pixels = readback.slice(..).get_mapped_range().unwrap().to_vec();
            readback.unmap();
            pixels
        };

        // A door held at each of three points of `open`. The rate is zero, so
        // `curtime` does not move the pose and the cycle is whatever is asked
        // for — which is exactly how a door that has arrived is held.
        let at = |cycle: f32| ModelEntity {
            sequence: "open".to_owned(),
            cycle,
            anim_time: 0.0,
            playback_rate: 0.0,
            ..door.clone()
        };
        let drawn = |image: &[u8]| {
            image
                .chunks_exact(4)
                .filter(|p| p[0..3] != [0, 0, 0])
                .count()
        };

        // **The middle of the doorway.** A shut door covers it and an open one
        // does not — which no wrong pose gives, because the geometry that has
        // to move out of the way is on the two leaf bones and nothing else.
        let centre = |image: &[u8]| {
            let mut covered = 0;
            for y in (SIZE / 2 - 16)..(SIZE / 2 + 16) {
                for x in (SIZE / 2 - 16)..(SIZE / 2 + 16) {
                    let at = ((y * SIZE + x) * 4) as usize;
                    if image[at..at + 3] != [0, 0, 0] {
                        covered += 1;
                    }
                }
            }
            covered
        };
        let differences = |a: &[u8], b: &[u8]| {
            a.chunks_exact(4)
                .zip(b.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count()
        };

        // The whole travel, sampled. **A chamber door does not open at a
        // constant rate**: the leaves hold, rotate and then retract, so most
        // of the doorway clears in the last third — which is why this is
        // measured here rather than assumed to be linear.
        let mut frames = Vec::new();
        for step in 0..=8 {
            let cycle = step as f32 / 8.0;
            world.entity_models.sync(&[at(cycle)]);
            let image = shot(&world.entity_models, 0.0);
            println!(
                "  cycle {cycle:.3}: {} pixels drawn, centre 32x32 {}/1024 covered",
                drawn(&image),
                centre(&image)
            );
            frames.push(image);
        }
        let (closed, open) = (frames.first().unwrap(), frames.last().unwrap());

        assert!(
            drawn(closed) > 5_000,
            "the door drew {} pixels; it is not on screen",
            drawn(closed)
        );
        assert_eq!(centre(closed), 32 * 32, "a shut door covers the doorway");
        assert_eq!(centre(open), 0, "an open door does not");
        // It only ever gets clearer, never darker: an animation drawn from a
        // wrong bone would not be monotonic.
        for pair in frames.windows(2) {
            assert!(
                centre(&pair[1]) <= centre(&pair[0]),
                "the doorway un-cleared part way through: {} then {}",
                centre(&pair[0]),
                centre(&pair[1])
            );
        }
        // **The first 62% of `open` draws nothing different, and that is the
        // model rather than the port.** The geometry below says why: what
        // moves over the first half is the two spinner rings, which turn
        // about their own axis *inside* the door's thickness. So the pixel
        // test can only speak for the second half, and the rest of this test
        // is geometric.
        let moved_late = differences(&frames[5], &frames[6]);
        assert!(
            moved_late > 1_000,
            "the leaves did not draw differently as they parted: {moved_late} pixels differ"
        );

        // **What `open` actually animates, in two acts.** Vertices that have
        // left where they started, per bone, and the furthest any one of them
        // went — which is the check with teeth here, because a pose composed
        // on the wrong side of the placement or read off the wrong bone would
        // move the *frame* as readily as the leaves.
        let survey = |pose: &[Mat4]| {
            let mut moved = vec![0usize; studio.bones.len()];
            let mut furthest = vec![0.0f32; studio.bones.len()];
            let mut spinner = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for i in 0..studio.vertices.len() {
                let bone = usize::from(bones[i]);
                let (was, now) = (world_vertex(i, &shut), world_vertex(i, pose));
                let distance = was.distance(now);
                if distance > 0.5 {
                    moved[bone] += 1;
                }
                furthest[bone] = furthest[bone].max(distance);
                if bone == 6 || bone == 7 {
                    spinner = (spinner.0.min(now), spinner.1.max(now));
                }
            }
            (moved, furthest, spinner)
        };

        // Act one: the rings turn, and **nothing else moves at all**. They
        // turn about their own axis, so their world bounding box is the same
        // box it was when the door was shut — which is what makes the first
        // half of the animation invisible from outside.
        let half_way = {
            let instance = &world.entity_models.instances[0];
            world.entity_models.models[instance.model].pose(instance.sequence, 0.5)
        };
        let (moved, furthest, spinner_half) = survey(&half_way);
        println!("at cycle 0.5: moved/bone {moved:?}, furthest {furthest:?}");
        assert_eq!(moved[2], 0, "the door frame must not move");
        assert_eq!((moved[4], moved[5]), (0, 0), "the leaves wait their turn");
        assert!(moved[6] > 50 && moved[7] > 50, "the rings must turn");
        assert!(
            furthest[6] > 20.0 && furthest[7] > 20.0,
            "the rings barely turned: {furthest:?}"
        );
        let (_, _, spinner_shut) = survey(&shut);
        assert!(
            spinner_half.0.abs_diff_eq(spinner_shut.0, 0.05)
                && spinner_half.1.abs_diff_eq(spinner_shut.1, 0.05),
            "a ring turning about its own axis keeps its box: \
             {spinner_shut:?} became {spinner_half:?}"
        );

        // Act two: the leaves part, 53 units each and symmetrically, and the
        // rings go with them because they hang off the leaves.
        let (moved, furthest, _) = survey(&opened);
        println!("at cycle 1.0: moved/bone {moved:?}, furthest {furthest:?}");
        assert_eq!(moved[2], 0, "the door frame must not move");
        assert_eq!((moved[4], moved[5]), (573, 578), "both leaves must move");
        assert!(
            (furthest[4] - 52.998).abs() < 0.01 && (furthest[5] - 52.998).abs() < 0.01,
            "the leaves should each travel 53 units: {furthest:?}"
        );
    }

    /// **A prop with no `DefaultAnim` is posed by sequence 0, not by its bind
    /// pose** — and `sp_a1_intro1`'s furniture is what says so.
    ///
    /// `m_nSequence` is an `int` that starts at zero, so every prop in the
    /// game is posed by *some* sequence: `CDynamicProp::Spawn` calls
    /// `PropSetAnim` only for a prop that has a `DefaultAnim`
    /// (`props.cpp:2036`), and `PropSetAnim` answers a name its model does not
    /// have with an explicit `SetSequence( 0 )` (`props.cpp:2422`). This
    /// port's seam carries a sequence *label*, and a label that resolves to
    /// nothing has to mean sequence 0 for the same reason — "no animation",
    /// which is the bind pose, is reachable only by a model with no sequences
    /// at all.
    ///
    /// For most models the two are the same matrix and the difference cannot
    /// be seen. **A bind pose is not a pose anybody ever looked at**, though,
    /// and `props_motel/hotel_container_furniture01`-`03` are a quarter turn
    /// apart: the single bone binds at `rot_x(+90)` against a `poseToBone` of
    /// `rot_x(-90)`, whose product is the identity, while `idle` frame 0 holds
    /// a 120° turn about `(1,1,1)` — `Quaternion64(0.5, 0.5, 0.5, 0.5)` — and
    /// *that* against the same `poseToBone` is `rot_z(+90)`.
    ///
    /// So posed from the bind pose the room's dresser, wardrobe and desk come
    /// out turned ninety degrees and standing in the bed. The bed is
    /// `hotel_container_furniture04`, and it is a **static prop** on a path
    /// that never looks at a sequence — so it stays where it belongs and the
    /// error is plain to see rather than moving everything together.
    ///
    /// The check is geometric rather than rendered: the posed, placed vertices
    /// of each of the three must clear the bed's own box. Sharing an anchor
    /// and a yaw with it, they cannot pass that by accident.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release posed_by_sequence_zero -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install and a GPU; set KISAK_GAME_DIR"]
    fn a_prop_with_no_default_anim_is_posed_by_sequence_zero() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let Some((device, queue)) = device() else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let mut materials = MaterialCache::new(&device, &queue);
        let map = "sp_a1_intro1";
        let mut world =
            super::super::World::load(&vfs, &mut materials, &device, map).expect("the map loads");

        let mut server = Server::new();
        server.level_init(map, &world.entities, &world.models);
        let placements: Vec<ModelEntity> = server
            .model_entities()
            .into_iter()
            .map(|e| ModelEntity {
                id: e.id,
                model: e.model,
                origin: e.origin,
                angles: e.angles,
                skin: e.skin,
                visible: e.visible,
                sequence: e.sequence,
                cycle: e.cycle,
                anim_time: e.anim_time,
                playback_rate: e.playback_rate,
                modulation: e.modulation,
            })
            .collect();
        world.load_entity_models(&vfs, &mut materials, &device, &placements);

        // What the whole map says, before the three under test: how many of
        // its models are posed differently by sequence 0 than by their bind
        // pose. It is a small minority, which is exactly why the fallback was
        // wrong for a year of frames without anybody noticing.
        let mut differ = 0;
        for instance in &world.entity_models.instances {
            let model = &world.entity_models.models[instance.model];
            let posed = model.pose(instance.sequence, 0.0);
            let bind = model.pose(usize::MAX, 0.0);
            if !posed
                .iter()
                .zip(&bind)
                .all(|(a, b)| a.abs_diff_eq(*b, 1e-4))
            {
                differ += 1;
            }
        }
        println!(
            "{} of {} instances are posed away from their bind pose",
            differ,
            world.entity_models.instances.len()
        );

        // `rot_z(+90)`: the 120° turn about `(1,1,1)` that `idle` frame 0
        // holds, times the bone's `rot_x(-90)` `poseToBone`.
        let quarter = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let mut checked = 0;
        for instance in &world.entity_models.instances {
            let model = &world.entity_models.models[instance.model];
            if !model
                .name
                .to_ascii_lowercase()
                .contains("hotel_container_furniture")
            {
                continue;
            }
            // Nothing on this map gives one a `DefaultAnim`, so the label the
            // server sends is empty and this is the fallback under test.
            assert_eq!(
                instance.sequence, 0,
                "{} should be posed by sequence 0",
                model.name
            );
            let pose = model.pose(instance.sequence, 0.0);
            assert_eq!(pose.len(), 1, "{} has one bone", model.name);
            println!("{}: {:?}", model.name, pose[0]);
            assert!(
                pose[0].abs_diff_eq(quarter, 1e-4),
                "{} is posed by {:?}, not the quarter turn its `idle` holds — \
                 it is being drawn in its bind pose, which stands it in the bed",
                model.name,
                pose[0]
            );
            checked += 1;
        }
        assert_eq!(checked, 3, "sp_a1_intro1 places three furniture props");
    }
}
