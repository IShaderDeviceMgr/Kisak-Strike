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

use super::Props;

/// One material's slice of a model's indices.
pub struct PropBatch {
    pub material: Arc<Material>,
    pub first_index: u32,
    pub index_count: u32,
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
    /// the ambient cube alone, which is correct rather than missing.
    pub instances_not_baked: usize,
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
    pub stats: PropModelStats,
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
                        }
                    })
                    .collect();

                // See the module docs: the file's winding is the reverse of
                // what this port's `front_face` names.
                let mut indices = model.indices.clone();
                for triangle in indices.chunks_exact_mut(3) {
                    triangle.swap(0, 2);
                }

                stats.models += 1;
                stats.vertices += model.vertices.len();
                stats.triangles += indices.len() / 3;
                Some(PropModel {
                    vertex_count: model.vertices.len(),
                    checksum: model.checksum,
                    meshes: model.meshes.clone(),
                    vertices: VertexBuffer::new(device, &model.path, &model.vertices),
                    indices: IndexBuffer::new_u32(device, &model.path, &indices),
                    batches,
                    bounds: model.bounds,
                    illum_position: model.illum_position,
                    name: model.path,
                })
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
            // `STATIC_PROP_NO_PER_VERTEX_LIGHTING` is `vrad` saying it wrote
            // no file for this one, so this saves a lookup rather than
            // changing the answer.
            if prop.flags.contains(super::PropFlags::NO_PER_VERTEX_LIGHTING) {
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

    /// Records every instance of every model into an open pass.
    ///
    /// Instances are walked **model-major**: every prop that shares a model is
    /// drawn before the next model's, so the vertex and index buffers and each
    /// material's pipeline are bound once per model rather than once per prop.
    /// That is `CStaticPropMgr::DrawStaticProps`' grouping and the reason the
    /// dictionary exists.
    ///
    /// # Lighting
    ///
    /// One flat ambient cube for the whole scene, set once. **Deliberately
    /// wrong**: the per-prop `.vhv` bake is stage 4 and the leaf ambient cube
    /// is stage 5, and until those land every prop is lit identically. The
    /// point of stage 3 is that the props are in the right places wearing the
    /// right materials, which a flat cube does not obscure.
    pub fn draw(&self, pass: &mut Pass<'_>, props: &Props) {
        if self.is_empty() {
            return;
        }

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
        // `static_light` is per instance and not per map, because whether a
        // prop got a `.vhv` is: a lighting block whose flag says "there is a
        // baked stream" while a black one is bound would darken that prop to
        // nothing, and one that says there is not while a real stream is bound
        // would throw `vrad`'s work away.
        let slots: Vec<_> = props
            .lighting
            .iter()
            .enumerate()
            .map(|(i, lighting)| {
                let mut lighting = *lighting;
                lighting.static_light = u32::from(self.light_ranges[i].is_some());
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
             {} baked ({} unbaked, {} stale, {} mismatched)",
            s.models,
            s.models_missing,
            s.vertices,
            s.triangles,
            s.materials,
            s.materials_missing,
            s.instances_baked,
            s.instances_not_baked,
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
/// [`super::light::lighting_for`] gives each prop its own.
pub(super) const FLAT_LIGHTING: ModelLighting = ModelLighting {
    ambient_cube: [[0.35, 0.35, 0.35, 0.0]; 6],
    lights: [crate::materials::uniforms::Light::NONE; 4],
    count: 0,
    // The `.vhv` colour stream is not read yet, so every vertex's baked light
    // is black — and a black *baked* light is not the same as no baked light.
    static_light: 0,
    ambient_light: 1,
    _padding: 0,
};
