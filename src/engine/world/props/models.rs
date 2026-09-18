//! Uploading the models a map's static props name, and drawing them.
//!
//! Stage 3 of `portdocs/STUDIO.md` §8. [`super::Props`] holds *where* the props
//! are; this holds *what* they are — one GPU upload per distinct model, drawn
//! once per instance.
//!
//! # Why one upload per model and not per instance
//!
//! `sp_a1_intro1` places 1,080 props from 136 distinct models. Pre-transforming
//! each instance's vertices into world space the way [`super::super::World`]
//! does for brush faces would multiply the vertex data by 1,080/136 ≈ 8× on
//! that map and by 56,955/968 ≈ 59× across the game. So a prop is a shared
//! vertex buffer plus a per-draw model matrix — which
//! [`Pass::draw`](crate::materials::context::Pass::draw) already takes, because
//! `MATERIAL_MODEL` was always a matrix and never a bake.
//!
//! # The winding, again
//!
//! `.vtx` indices are wound the way the `.bsp`'s faces are — Valve's
//! `D3DCULL_CCW` under a Y-up framebuffer — and this port's
//! `front_face: Ccw` under `wgpu`'s Y-down framebuffer names the opposite set
//! of triangles. So every triangle is emitted reversed here for exactly the
//! reason [`build_meshes`](super::super::build_meshes) reverses its fans, and
//! the fix, if it is ever made, is one `front_face` in `PipelineCache` and the
//! deletion of both reversals. `rustdocs/ENGINE.md` gotcha #1.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;

use crate::filesystem::Vfs;
use crate::materials::context::Pass;
use crate::materials::mesh::{IndexBuffer, StaticLightVertex, VertexBuffer, VertexLayout};
use crate::materials::uniforms::ModelLighting;
use crate::materials::{Material, MaterialCache};
use crate::studio::{vhv, StudioModel, Vhv};

use super::super::vis::VisibleSet;
use super::super::GeometryPass;
use super::Props;

/// One material's slice of a model's indices.
pub struct PropBatch {
    pub material: Arc<Material>,
    pub first_index: u32,
    pub index_count: u32,
    /// The same indices, split into contiguous runs by the bone that moves
    /// them — [`studio::BoneRun`], carried through unchanged.
    ///
    /// One run under bone 0 for a model with one bone, which is every static
    /// prop in the game, so the static path can ignore this and does.
    pub bones: Vec<crate::studio::BoneRun>,
}

/// One distinct model, uploaded once and drawn by every instance of it.
// `bounds` is for the culling that is not written yet and `illum_position` for
// stage 5's lighting; both are already in hand and re-reading the `.mdl` for
// them later would be the only alternative.
#[allow(dead_code)]
pub struct PropModel {
    pub name: String,
    pub vertices: VertexBuffer,
    /// **32-bit.** A static prop is not bounded the way a brush batch is:
    /// `models/stars/allstars.mdl` has 187,676 vertices.
    pub indices: IndexBuffer,
    pub batches: Vec<PropBatch>,
    /// `view_bbmin`/`view_bbmax`, in model space. For the culling that is not
    /// written yet.
    pub bounds: (Vec3, Vec3),
    /// `illumposition` — where this model wants its lighting sampled, in model
    /// space. Read by stage 5.
    pub illum_position: Vec3,
    /// How many vertices [`vertices`](Self::vertices) holds — the length a
    /// prop's static-light stream has to match.
    pub vertex_count: usize,
    /// The `.mdl`'s, which a `.vhv` has to agree with.
    pub checksum: u32,
    /// The bone list, in file order — what [`studio::anim::pose`] chains.
    ///
    /// Empty for a model with one bone or none, in which case there is nothing
    /// to pose and the identity is the whole answer.
    ///
    /// [`studio::anim::pose`]: crate::studio::anim::pose
    pub bones: Vec<crate::studio::anim::Bone>,
    /// The sequences, in file order. `LookupSequence` searches it by label.
    pub sequences: Vec<crate::studio::anim::Sequence>,
    /// The animations a sequence names.
    pub animations: Vec<crate::studio::anim::Animation>,
    /// `STUDIOHDR_FLAGS_USES_BUMPMAPPING` — whether **any** of this model's
    /// materials lights it per pixel, which is how Valve computes the flag
    /// (`studiorendercontext.cpp:274`, once per material, ORed onto the
    /// model).
    ///
    /// It is the deciding half of `bStaticLighting`
    /// (`l_studio.cpp:3046`): a model with it on reads no `.vhv` and is lit by
    /// the light cache instead. See [`Material::uses_bumpmapping`] and
    /// [`PropModels::load`].
    ///
    /// [`Material::uses_bumpmapping`]: crate::materials::material::Material::uses_bumpmapping
    pub uses_bumpmapping: bool,
    /// The studio meshes a `.vhv`'s per-LOD blocks are matched against, in
    /// hardware vertex order.
    ///
    /// Only the map load reads this — every prop's `.vhv` is scattered through
    /// it once — so it could be dropped afterwards. It is a little over a
    /// megabyte for a whole map's models and keeping it is what would let a
    /// prop's lighting be rebuilt without re-reading the `.mdl`.
    pub meshes: Vec<crate::studio::HardwareMesh>,
}

/// What loading a map's prop models turned out to cost.
#[derive(Debug, Clone, Default)]
pub struct PropModelStats {
    /// Models that loaded and uploaded.
    pub models: usize,
    /// Models whose files could not be read, whose instances draw nothing.
    pub models_missing: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub materials: usize,
    /// Materials that resolved to the error checkerboard — including the ones
    /// that resolved to a *brush* shader, which model geometry cannot feed.
    pub materials_missing: usize,
    /// Instances that name a model that failed to load.
    pub instances_without_a_model: usize,
    /// Instances whose `.vhv` was read, so they draw with `vrad`'s per-vertex
    /// bake rather than the ambient cube alone.
    pub instances_baked: usize,
    /// Instances whose `.vhv` carried a different checksum from the model.
    ///
    /// **Used anyway**, as the shipped engine does — see the comment at the
    /// check. Counted because it is the first thing to look at if a prop is lit
    /// oddly.
    pub instances_baked_stale: usize,
    /// Instances whose `.vhv` matched by checksum but not by mesh shape.
    pub instances_baked_mismatched: usize,
    /// Instances `vrad` deliberately baked no vertex lighting for —
    /// `STATIC_PROP_NO_PER_VERTEX_LIGHTING`, or simply no file. They are lit by
    /// the light cache, which is correct rather than missing.
    pub instances_not_baked: usize,
    /// Instances whose model is bumped or phong, so the shipped engine ignores
    /// the `.vhv` `vrad` wrote for them and lights them per pixel from the
    /// light cache instead — `bStaticLighting` false
    /// (`l_studio.cpp:3046`). The file is not even opened.
    pub instances_per_pixel_lit: usize,
}

/// Every prop model a map needs, uploaded.
#[derive(Default)]
pub struct PropModels {
    /// Parallel to [`Props::models`]: `None` where the model would not load.
    models: Vec<Option<PropModel>>,
    /// Which instances name each model, also parallel to it.
    ///
    /// Built once at load rather than filtered per frame: with 136 models and
    /// 1,080 instances, scanning the instance list per model is 147,000
    /// comparisons every frame to find the same 1,080 answers.
    instances: Vec<Vec<usize>>,
    /// Every instance's baked vertex lighting, concatenated.
    ///
    /// **One buffer for the whole map**, not one per prop: `sp_a1_intro1` has
    /// 1,080 props whose models total 2.3 million vertices between them, and
    /// 1,080 `wgpu` allocations to hold 9 MB would be 1,080 Metal buffers where
    /// one will do. [`static_light`](Self::static_light) slices it.
    ///
    /// `None` for a map with no baked prop lighting at all, in which case every
    /// prop takes the black stream below.
    light: Option<VertexBuffer>,
    /// Each instance's slice of [`light`](Self::light), parallel to the map's
    /// instance list. `None` where the prop has no usable `.vhv`.
    light_ranges: Vec<Option<(u32, u32)>>,
    /// A black stream long enough for the widest model, sliced for any prop
    /// with no `.vhv` of its own.
    ///
    /// Something must be bound in slot 1 for every model draw — the layout says
    /// so — and this is the "no baked light" value. It is paired with
    /// [`ModelLighting::static_light`] 0, so the shader does not read it at
    /// all; it exists to satisfy the vertex layout, not to be sampled.
    unlit: Option<VertexBuffer>,
    /// Whether any batch of any model wears a material that needs a readable
    /// copy of the scene. Computed at load because the answer is asked once a
    /// frame and cannot change: see [`PropModels::refracts`].
    refracts: bool,
    pub stats: PropModelStats,
}

impl PropModel {
    /// Uploads one already-resolved [`StudioModel`], with its materials
    /// already turned into [`PropBatch`]es.
    ///
    /// Shared by the static-prop path and by
    /// [`entities`](crate::engine::world::entities), which need the same
    /// buffers from the same files and differ only in where the instances come
    /// from and how they are posed.
    pub fn upload(device: &wgpu::Device, model: StudioModel, batches: Vec<PropBatch>) -> PropModel {
        // See the module docs: the file's winding is the reverse of what this
        // port's `front_face` names. Reversing each triangle **in place**
        // leaves every batch's and every bone run's index range where it was.
        let mut indices = model.indices.clone();
        for triangle in indices.chunks_exact_mut(3) {
            triangle.swap(0, 2);
        }

        PropModel {
            vertex_count: model.vertices.len(),
            uses_bumpmapping: batches.iter().any(|batch| batch.material.uses_bumpmapping),
            checksum: model.checksum,
            meshes: model.meshes.clone(),
            vertices: VertexBuffer::new(device, &model.path, &model.vertices),
            indices: IndexBuffer::new_u32(device, &model.path, &indices),
            batches,
            bounds: model.bounds,
            illum_position: model.illum_position,
            bones: model.bones,
            sequences: model.sequences,
            animations: model.animations,
            name: model.path,
        }
    }

    /// `LookupSequence` — a sequence's index by label, case insensitively.
    pub fn sequence(&self, label: &str) -> Option<usize> {
        self.sequences
            .iter()
            .position(|s| s.label.eq_ignore_ascii_case(label))
    }

    /// The animation a sequence plays, if it has one.
    pub fn animation(&self, sequence: usize) -> Option<&crate::studio::anim::Animation> {
        let sequence = self.sequences.get(sequence)?;
        self.animations.get(sequence.anim)
    }

    /// Where every bone is at `cycle` through `sequence`, in model space.
    ///
    /// The identity for every bone of a model with no bones, no sequences, or
    /// a sequence it does not have — which is what makes a static prop and an
    /// animated model one draw path.
    pub fn pose(&self, sequence: usize, cycle: f32) -> Vec<glam::Mat4> {
        let anim = self
            .sequences
            .get(sequence)
            .and_then(|s| self.animations.get(s.anim));
        crate::studio::anim::pose(&self.bones, anim, cycle)
    }
}

impl PropModels {
    /// Reads and uploads each distinct model [`Props`] names.
    ///
    /// **Cannot fail.** A prop whose model is missing is a prop that does not
    /// draw, which is what the original does too — `CStaticPropMgr` logs and
    /// carries on rather than failing the map (`staticpropmgr.cpp:1633`). The
    /// reason is on stderr, once per model.
    pub fn load(
        vfs: &Vfs,
        materials: &mut MaterialCache,
        device: &wgpu::Device,
        props: &Props,
        hdr: bool,
    ) -> PropModels {
        let mut stats = PropModelStats::default();
        let mut resolved: HashMap<String, Arc<Material>> = HashMap::new();
        let error = materials.error_model_material();

        let models = props
            .models
            .iter()
            .map(|name| {
                let model = match StudioModel::load(vfs, name) {
                    Ok(model) => model,
                    Err(e) => {
                        eprintln!("source-engine: props: {e}");
                        stats.models_missing += 1;
                        return None;
                    }
                };
                if model.vertices.is_empty() || model.indices.is_empty() {
                    // Eight of Portal 2's models have a body part with no strip
                    // groups. They are legal and they draw nothing; uploading
                    // an empty buffer is what `VertexBuffer::new` asserts
                    // against, so they are dropped here instead.
                    stats.models_missing += 1;
                    return None;
                }

                let batches = model
                    .batches
                    .iter()
                    .map(|batch| {
                        let material = resolved
                            .entry(batch.material.clone())
                            .or_insert_with(|| {
                                stats.materials += 1;
                                let material = materials.load(vfs, &batch.material);
                                if Arc::ptr_eq(&material, &materials.error_material()) {
                                    stats.materials_missing += 1;
                                    return Arc::clone(&error);
                                }
                                // A prop's geometry is `ModelVertex` and
                                // nothing else, so a material whose shader
                                // wants brush vertices cannot draw it. Same
                                // decision `World::load` makes in the other
                                // direction, and for the same reason: visibly
                                // wrong beats plausibly wrong.
                                if material.shader.vertex_layout() != VertexLayout::Model {
                                    eprintln!(
                                        "source-engine: props: {}: {} does not take model \
                                         geometry",
                                        batch.material,
                                        material.shader.name()
                                    );
                                    stats.materials_missing += 1;
                                    return Arc::clone(&error);
                                }
                                material
                            })
                            .clone();
                        PropBatch {
                            material,
                            first_index: batch.first_index,
                            index_count: batch.index_count,
                            bones: batch.bones.clone(),
                        }
                    })
                    .collect();

                stats.models += 1;
                stats.vertices += model.vertices.len();
                stats.triangles += model.indices.len() / 3;
                Some(PropModel::upload(device, model, batches))
            })
            .collect::<Vec<_>>();

        let mut instances = vec![Vec::new(); models.len()];
        for (i, prop) in props.instances.iter().enumerate() {
            match models.get(prop.model_index) {
                Some(Some(_)) => instances[prop.model_index].push(i),
                _ => stats.instances_without_a_model += 1,
            }
        }

        // The per-placement baked lighting, gathered into one buffer.
        //
        // Read here rather than lazily because it lives in the map's pak lump,
        // which is mounted for exactly as long as the map is: by the time
        // anything draws, the file is gone.
        let mut light: Vec<StaticLightVertex> = Vec::new();
        let mut light_ranges = vec![None; props.instances.len()];
        for (i, prop) in props.instances.iter().enumerate() {
            let Some(Some(model)) = models.get(prop.model_index) else {
                continue;
            };
            // **`bStaticLighting`** (`l_studio.cpp:3046`). A model lit per
            // pixel does not read a colour mesh — the bumped and phong paths
            // have no `bStaticLight` at all — and the shipped engine then asks
            // `LightcacheGetStatic` for the *static* lighting it would
            // otherwise have skipped. Leaving the range `None` here is what
            // makes those two decisions one decision downstream.
            if model.uses_bumpmapping {
                stats.instances_per_pixel_lit += 1;
                continue;
            }
            // `STATIC_PROP_NO_PER_VERTEX_LIGHTING` is `vrad` saying it wrote
            // no file for this one, so this saves a lookup rather than
            // changing the answer.
            if prop
                .flags
                .contains(super::PropFlags::NO_PER_VERTEX_LIGHTING)
            {
                stats.instances_not_baked += 1;
                continue;
            }
            let path = vhv::prop_lighting_path(i, hdr);
            let Ok(bytes) = vfs.read(&path) else {
                stats.instances_not_baked += 1;
                continue;
            };
            let vhv = match Vhv::parse(path, &bytes) {
                Ok(vhv) => vhv,
                Err(e) => {
                    eprintln!("source-engine: props: {e}");
                    continue;
                }
            };
            // **Counted, not enforced.** `r_ignoreStaticColorChecksum`
            // defaults to 1 (`l_studio.cpp:117`), so the shipped engine does
            // not check this — and the shipped *data* needs it not to:
            // `mp_coop_paint_longjump_intro`'s prop 26 carries a `.vhv` whose
            // checksum is not its model's, and Portal 2 renders it. Rejecting
            // on the checksum would darken props the real game lights.
            //
            // What actually protects against colours from another model is the
            // per-mesh vertex count below, which is the check Valve does make.
            if vhv.checksum != model.checksum {
                stats.instances_baked_stale += 1;
            }
            let Some(colors) = vhv.colors(&bytes, 0, &model.meshes, model.vertex_count) else {
                stats.instances_baked_mismatched += 1;
                continue;
            };
            light_ranges[i] = Some((light.len() as u32, colors.len() as u32));
            light.extend_from_slice(&colors);
            stats.instances_baked += 1;
        }

        let widest = models
            .iter()
            .flatten()
            .map(|m| m.vertex_count)
            .max()
            .unwrap_or(0);

        let refracts = models.iter().flatten().any(|model| {
            model
                .batches
                .iter()
                .any(|batch| batch.material.needs_frame_buffer_copy)
        });

        PropModels {
            light: (!light.is_empty())
                .then(|| VertexBuffer::new(device, "static prop lighting", &light)),
            unlit: (widest > 0).then(|| {
                VertexBuffer::new(
                    device,
                    "static prop lighting (none)",
                    &vec![StaticLightVertex::UNLIT; widest],
                )
            }),
            light_ranges,
            models,
            instances,
            refracts,
            stats,
        }
    }

    #[allow(dead_code)]
    pub fn get(&self, index: usize) -> Option<&PropModel> {
        self.models.get(index)?.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.models.iter().all(Option::is_none)
    }

    /// Whether any of these models refracts, and therefore whether a copy of
    /// the frame buffer has to be taken before
    /// [`draw_refracting`](PropModels::draw_refracting) is called.
    ///
    /// `ERENDERFLAGS_NEEDS_POWER_OF_TWO_FB` gathered over a map's props, once
    /// at load. `sp_a1_intro1` answers `true` for exactly one of its 136
    /// models — `models/props_lab/glass_observation_2.mdl`, whose
    /// `models/props_lab/glasswindow_observation` has no `$basetexture` of its
    /// own to warp — and 71 of the game's 106 maps answer `true` for
    /// something.
    pub fn refracts(&self) -> bool {
        self.refracts
    }

    /// Records every instance of every model into an open pass, except the
    /// batches that need a copy of the frame buffer.
    ///
    /// Instances are walked **model-major**: every prop that shares a model is
    /// drawn before the next model's, so the vertex and index buffers and each
    /// material's pipeline are bound once per model rather than once per prop.
    /// That is `CStaticPropMgr::DrawStaticProps`' grouping and the reason the
    /// dictionary exists.
    pub fn draw(&self, pass: &mut Pass<'_>, props: &Props, visible: &VisibleSet) {
        self.record(pass, props, false, visible);
    }

    /// The batches [`draw`](PropModels::draw) left out: the ones whose material
    /// reads the scene behind it.
    ///
    /// Call after [`RenderContext::update_refract_texture`][update], in a
    /// second pass against the same target. Draws nothing if
    /// [`refracts`](PropModels::refracts) is false.
    ///
    /// **The split is per *batch*, not per prop, and that is a divergence
    /// rather than a refinement.** `CRendering3dView` sorts whole
    /// *renderables* into the opaque and translucent lists, so a prop with one
    /// refracting material among several is drawn entirely in the translucent
    /// pass — its opaque parts included. Splitting per batch leaves those in
    /// the opaque pass, where they depth-test against the world without being
    /// sorted.
    ///
    /// It matters, because mixing is the **normal** case and not the corner
    /// one: of the 66 models in the depot that wear a frame-buffer-refracting
    /// material, **60 also wear something else** — every
    /// `props_destruction/glass_*` pane is a refracting sheet plus an opaque
    /// `glass_fracture_*_inner` edge, and `props_bts/vactube_*_neurotoxin` is
    /// glass over an opaque pipe. Six do not, and one of those six is
    /// `props_lab/glass_observation_2.mdl`, the only such model
    /// `sp_a1_intro1` places as a static prop.
    ///
    /// The per-batch split is the one that is right without a depth sort, which
    /// this port does not have; the condition to revisit it is translucency
    /// sorting landing, at which point a whole-renderable split becomes
    /// expressible and the choice can be made on looks rather than on what is
    /// available.
    ///
    /// [update]: crate::materials::context::RenderContext::update_refract_texture
    pub fn draw_refracting(&self, pass: &mut Pass<'_>, props: &Props, visible: &VisibleSet) {
        if !self.refracts {
            return;
        }
        self.record(pass, props, true, visible);
    }

    /// Offers every batch of every instance that belongs in the translucent
    /// pass, with the world-space centre the sort wants.
    ///
    /// A static prop has no `rendermode`, but it does have
    /// `m_DiffuseModulation` in the `sprp` lump — so a prop can be
    /// alpha-modulated even though nothing in the entity lump says so, and the
    /// instance test below is that.
    ///
    /// The centre is the model's own `view_bbmin`/`view_bbmax` centre put
    /// through the prop's transform, which is `BuildRenderListInfo_t`'s box
    /// centre up to the difference between a transformed box and the box of a
    /// transformed model. They agree for an axis-aligned prop and differ by at
    /// most the box's own size for a turned one, which is below the resolution
    /// a back-to-front sort of whole props has anyway.
    pub(crate) fn collect_translucent(
        &self,
        props: &Props,
        out: &mut dyn FnMut(Vec3, usize, usize, usize),
    ) {
        for (index, model) in self.models.iter().enumerate() {
            let Some(model) = model else { continue };
            let center = (model.bounds.0 + model.bounds.1) * 0.5;
            for (batch_index, batch) in model.batches.iter().enumerate() {
                for &i in &self.instances[index] {
                    let prop = &props.instances[i];
                    let pass =
                        GeometryPass::of_instance(&batch.material, prop.modulation[3] != 1.0);
                    if pass == GeometryPass::Translucent {
                        out(
                            prop.transform.transform_point3(center),
                            index,
                            batch_index,
                            i,
                        );
                    }
                }
            }
        }
    }

    /// Records one (model, batch, instance) — the translucent list's unit of
    /// work, where [`record`](PropModels::record) draws every instance of a
    /// batch in one go.
    ///
    /// The batching the opaque path gets is exactly what a back-to-front sort
    /// costs, and Valve pays it too: `DrawTranslucentRenderables` walks its
    /// sorted list one renderable at a time.
    pub(crate) fn draw_one(
        &self,
        pass: &mut Pass<'_>,
        props: &Props,
        model: usize,
        batch: usize,
        instance: usize,
    ) {
        let Some(model_data) = self.models.get(model).and_then(Option::as_ref) else {
            return;
        };
        let batch = &model_data.batches[batch];
        let prop = &props.instances[instance];

        let lighting = match self.light_ranges[instance].is_some() {
            true => BAKED_LIGHTING,
            false => props
                .lighting
                .get(instance)
                .copied()
                .unwrap_or(FLAT_LIGHTING),
        };
        pass.set_model_lighting(&lighting);

        let unlit = self
            .unlit
            .as_ref()
            .map(|buffer| buffer.range(0, model_data.vertex_count as u32));
        let light = match (&self.light, self.light_ranges[instance]) {
            (Some(buffer), Some((first, count))) => Some(buffer.range(first, count)),
            _ => unlit,
        };
        let Some(light) = light else { return };
        pass.bind_static_light(&light);

        let indices = model_data
            .indices
            .range(batch.first_index, batch.index_count);
        pass.draw_modulated(
            &batch.material,
            &model_data.vertices.slice(),
            &indices,
            prop.transform,
            prop.modulation,
        );
    }

    /// The body both entry points share. `refracting` selects which half of
    /// each model's batches to record.
    ///
    /// # Lighting
    ///
    /// One lighting slot per instance, taken up front — see the comment inside.
    /// A refracting draw does not read one (`Refract` binds a copy of the scene
    /// in group 3, not lighting), so the second call's slots are wasted; that
    /// is a few hundred bytes of arena on the one map in the game that has any,
    /// against threading a flag through the loop.
    fn record(&self, pass: &mut Pass<'_>, props: &Props, refracting: bool, visible: &VisibleSet) {
        if self.is_empty() {
            return;
        }
        let wanted = match refracting {
            true => GeometryPass::Refracting,
            false => GeometryPass::Opaque,
        };

        // Phase one: one lighting slot per instance, taken up front.
        //
        // The draws below are **batch-major** — every instance of a model's
        // first batch, then every instance of its second — so that the
        // pipeline, the material bind group and the vertex and index buffers
        // are bound once per batch rather than once per instance. Pushing the
        // lighting as it drew would then cost a slot per (batch, instance)
        // instead of per instance, so it is taken here instead. The order the
        // slots come back in does not matter; the offsets do.
        if props.lighting.is_empty() {
            // A map compiled without `vrad` has no baked cubes; every prop
            // shares one slot.
            pass.set_model_lighting(&FLAT_LIGHTING);
        }
        // **The two terms do not add up; they replace each other**, and that
        // is `StudioSetupLighting`'s one real decision (`l_studio.cpp:1430`).
        // A prop that uses `vrad`'s per-vertex bake asks
        // `LightcacheGetStatic` for `LIGHTCACHEFLAGS_DYNAMIC |
        // LIGHTCACHEFLAGS_LIGHTSTYLE` — *without* `LIGHTCACHEFLAGS_STATIC` —
        // so its ambient cube and its local lights come back zeroed and the
        // baked stream is the whole of its lighting. One that does not asks
        // for all three and gets the light cache instead.
        //
        // Both halves are already decided by `light_ranges[i]`, which is
        // `Some` exactly when Valve's `bStaticLighting` is true: see
        // `PropModels::load`.
        let slots: Vec<_> = props
            .lighting
            .iter()
            .enumerate()
            .map(|(i, lighting)| {
                let lighting = match self.light_ranges[i].is_some() {
                    true => BAKED_LIGHTING,
                    false => *lighting,
                };
                pass.push_model_lighting(&lighting)
            })
            .collect();

        for (index, model) in self.models.iter().enumerate() {
            let Some(model) = model else { continue };
            let instances = &self.instances[index];
            if instances.is_empty() {
                continue;
            }
            let vertices = model.vertices.slice();
            // What a prop with no `.vhv` binds: a black stream long enough for
            // this model. Something must be bound in slot 1 for every model
            // draw, and its `ModelLighting::static_light` is 0 so the shader
            // never reads it.
            let unlit = self
                .unlit
                .as_ref()
                .map(|buffer| buffer.range(0, model.vertex_count as u32));
            for batch in &model.batches {
                let indices = model.indices.range(batch.first_index, batch.index_count);
                for &i in instances {
                    let prop = &props.instances[i];
                    // `Map_AreAnyLeavesVisible( m_LeafList )` — the whole of
                    // how `CStaticPropMgr` culls one (`staticpropmgr.cpp`),
                    // and exact rather than a bounding-box guess because
                    // `vbsp` wrote the leaf list per prop.
                    if !visible.any_leaf(&props.leaves[prop.leaves.clone()]) {
                        continue;
                    }
                    // Per instance rather than per batch: a prop whose
                    // `m_DiffuseModulation` alpha is below 1 is translucent
                    // even where the material is not.
                    if GeometryPass::of_instance(&batch.material, prop.modulation[3] != 1.0)
                        != wanted
                    {
                        continue;
                    }
                    if let Some(&slot) = slots.get(i) {
                        pass.set_model_lighting_slot(slot);
                    }
                    let light = match (&self.light, self.light_ranges[i]) {
                        (Some(buffer), Some((first, count))) => Some(buffer.range(first, count)),
                        _ => unlit.clone(),
                    };
                    let Some(light) = light else { continue };
                    pass.bind_static_light(&light);
                    pass.draw_modulated(
                        &batch.material,
                        &vertices,
                        &indices,
                        prop.transform,
                        prop.modulation,
                    );
                }
            }
        }
    }

    /// A one-line summary for the startup log.
    pub fn summary(&self) -> String {
        let s = &self.stats;
        format!(
            "{} models ({} missing), {} vertices, {} triangles, {} materials ({} missing), \
             {} baked ({} unbaked, {} per-pixel, {} stale, {} mismatched)",
            s.models,
            s.models_missing,
            s.vertices,
            s.triangles,
            s.materials,
            s.materials_missing,
            s.instances_baked,
            s.instances_not_baked,
            s.instances_per_pixel_lit,
            s.instances_baked_stale,
            s.instances_baked_mismatched
        )
    }
}

/// The placeholder lighting a prop wears when the map has no baked ambient
/// cubes at all.
///
/// A mid-grey ambient cube and no local lights — enough to see a model's shape
/// through its normals, and obviously not a lighting environment. It is a
/// constant rather than [`ModelLighting::fullbright`] so that it reads as a
/// stand-in in the frame it is looked at.
///
/// Reachable only on a map compiled without `vrad`: with lighting baked,
/// [`LightCache::lighting_at`] gives each prop its own.
///
/// [`LightCache::lighting_at`]: crate::engine::world::light::LightCache::lighting_at
pub(super) const FLAT_LIGHTING: ModelLighting = ModelLighting {
    ambient_cube: [[0.35, 0.35, 0.35, 0.0]; 6],
    lights: [crate::materials::uniforms::Light::NONE; 4],
    count: 0,
    // A map with no baked ambient cubes has no `.vhv` files either, so every
    // vertex's baked light would be black — and a black *baked* light is not
    // the same as no baked light.
    static_light: 0,
    ambient_light: 1,
    _padding: 0,
};

/// What a prop with a usable `.vhv` is drawn under: the baked stream, and
/// nothing else.
///
/// This is the state `LightcacheGetStatic` returns when it is asked without
/// `LIGHTCACHEFLAGS_STATIC` and the map has no dynamic lights and no
/// lightstyles — `ZeroLightingState()` plus `mat_ambient_light_r/g/b`, which
/// default to 0. It is not "unlit": `static_light` is what says the vertex
/// stream bound beside it is real.
///
/// `ambient_light` stays 1 over a zeroed cube, which is the literal
/// transcription — Valve zeroes the cube and never clears `m_bAmbientLight` —
/// and the same six products either way.
pub(super) const BAKED_LIGHTING: ModelLighting = ModelLighting {
    ambient_cube: [[0.0, 0.0, 0.0, 0.0]; 6],
    lights: [crate::materials::uniforms::Light::NONE; 4],
    count: 0,
    static_light: 1,
    ambient_light: 1,
    _padding: 0,
};

#[cfg(test)]
mod tests {
    use crate::filesystem::Vfs;
    use crate::materials::context::{Camera, Load, RenderContext};
    use crate::materials::target::RenderTarget;
    use crate::materials::MaterialCache;
    use glam::Vec3;

    const SIZE: u32 = 256;

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

    /// **A prop that is lit per pixel looks different with the world lights
    /// than without them**, which is the one thing the light cache's local
    /// half can be asked that nothing else in the port answers.
    ///
    /// Every other check of that work is a count — how many lights survived
    /// the filters, how many props took a slot — and a count cannot tell a
    /// `ModelLighting` block that reached the GPU from one that was built
    /// correctly and then ignored. The bind group could be the wrong shape,
    /// `count` could be read as a byte, the pipeline could be `Phong`'s while
    /// the uniform is `VertexLitGeneric`'s: all of those draw a prop, and none
    /// of them draws a prop that *changes* when the lights are taken away.
    ///
    /// It picks a prop on `sp_a1_intro1` whose model is bumped or phong — so
    /// it reads no `.vhv` and the light cache is the whole of its lighting —
    /// and which has at least one local light, renders it, zeroes the local
    /// lights and renders it again.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release a_per_pixel_prop -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install and a GPU; set KISAK_GAME_DIR"]
    fn a_per_pixel_prop_is_lit_by_the_world_lights() {
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
        let mut world = crate::engine::world::World::load(&vfs, &mut materials, &device, &map)
            .expect("the map loads");
        println!("{}", world.prop_models.summary());

        // The brightest-lit per-pixel prop in the map, so that the difference
        // below is as large as the map makes it.
        let chosen = world
            .props
            .instances
            .iter()
            .enumerate()
            .filter(|(i, prop)| {
                world.props.lighting[*i].count > 0
                    && world
                        .prop_models
                        .get(prop.model_index)
                        .is_some_and(|model| model.uses_bumpmapping)
            })
            .max_by(|(a, _), (b, _)| {
                let brightness = |i: usize| {
                    world.props.lighting[i].lights[0].color[0]
                        / world.props.lighting[i].lights[0].attenuation[2].max(1.0)
                };
                brightness(*a).total_cmp(&brightness(*b))
            })
            .map(|(i, prop)| (i, prop.clone()))
            .expect("a per-pixel prop with a local light");
        let (index, prop) = chosen;
        println!(
            "prop {index}: {} at {:?}, {} local light(s)",
            prop.model, prop.lighting_origin, world.props.lighting[index].count
        );

        // Close enough that this prop is most of the frame. Other props still
        // draw — `PropModels::draw` has no culling — but they are elsewhere.
        let target_point = prop.transform.w_axis.truncate();
        let eye = target_point + Vec3::new(64.0, 64.0, 48.0);
        let camera = Camera::perspective(
            eye,
            glam::camera::rh::view::look_at_mat4(eye, target_point, Vec3::Z),
            75.0,
            1.0,
            1.0,
            4096.0,
        );

        let mut context = RenderContext::new(&device, &queue, materials.pipelines());
        let render_target = RenderTarget::new(
            &device,
            "prop",
            SIZE,
            SIZE,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            true,
        );

        // Props only: the world around them would swamp the comparison, and
        // what is under test is their lighting.
        let mut shot = |world: &crate::engine::world::World| -> Vec<u8> {
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
                world.prop_models.draw(
                    &mut pass,
                    &world.props,
                    &crate::engine::world::vis::VisibleSet::everything(),
                );
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

        let lit = shot(&world);
        // Against a black clear, so "drawn" is "not still the clear colour".
        let drawn = lit.chunks_exact(4).filter(|p| p[0..3] != [0, 0, 0]).count();
        println!("{drawn} of {} pixels drawn", SIZE * SIZE);
        assert!(
            drawn > 500,
            "only {drawn} pixels drawn; nothing is on screen"
        );

        // The same frame with the local lights taken away and nothing else
        // changed — which is what `world::props` drew before this landed.
        for state in &mut world.props.lighting {
            state.count = 0;
        }
        let unlit = shot(&world);

        let mean = |pixels: &[u8]| {
            pixels
                .chunks_exact(4)
                .map(|p| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]))
                .sum::<u32>() as f64
                / (pixels.len() / 4 * 3) as f64
        };
        let differences = lit
            .chunks_exact(4)
            .zip(unlit.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        println!(
            "mean channel {:.2} lit against {:.2} unlit, {differences} pixels differ",
            mean(&lit),
            mean(&unlit)
        );

        assert!(
            differences > drawn / 10,
            "only {differences} of {drawn} drawn pixels changed when the local \
             lights were removed"
        );
        assert!(
            mean(&lit) > mean(&unlit),
            "removing the local lights made the frame brighter"
        );
    }
}
