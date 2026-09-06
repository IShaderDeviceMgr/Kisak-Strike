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
use crate::materials::mesh::{IndexBuffer, VertexBuffer, VertexLayout};
use crate::materials::uniforms::ModelLighting;
use crate::materials::{Material, MaterialCache};
use crate::studio::StudioModel;

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
}

/// Every prop model a map needs, uploaded.
#[derive(Default)]
pub struct PropModels {
    /// Parallel to [`Props::models`]: `None` where the model would not load.
    models: Vec<Option<PropModel>>,
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
                    vertices: VertexBuffer::new(device, &model.path, &model.vertices),
                    indices: IndexBuffer::new_u32(device, &model.path, &indices),
                    batches,
                    bounds: model.bounds,
                    illum_position: model.illum_position,
                    name: model.path,
                })
            })
            .collect::<Vec<_>>();

        stats.instances_without_a_model = props
            .instances
            .iter()
            .filter(|prop| models[prop.model_index].is_none())
            .count();

        PropModels { models, stats }
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
        // Every prop shares one state when the map carries no baked ambient
        // lighting; otherwise each has its own and it is set per instance.
        if props.lighting.is_empty() {
            pass.set_model_lighting(&FLAT_LIGHTING);
        }

        for (index, model) in self.models.iter().enumerate() {
            let Some(model) = model else { continue };
            let vertices = model.vertices.slice();
            for (i, prop) in props.instances.iter().enumerate() {
                if prop.model_index != index {
                    continue;
                }
                if let Some(lighting) = props.lighting.get(i) {
                    pass.set_model_lighting(lighting);
                }
                let modulation = prop.diffuse_modulation.map(|c| f32::from(c) / 255.0);
                for batch in &model.batches {
                    pass.draw_modulated(
                        &batch.material,
                        &vertices,
                        &model.indices.range(batch.first_index, batch.index_count),
                        prop.transform,
                        modulation,
                    );
                }
            }
        }
    }

    /// A one-line summary for the startup log.
    pub fn summary(&self) -> String {
        let s = &self.stats;
        format!(
            "{} models ({} missing), {} vertices, {} triangles, {} materials ({} missing)",
            s.models, s.models_missing, s.vertices, s.triangles, s.materials, s.materials_missing
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
