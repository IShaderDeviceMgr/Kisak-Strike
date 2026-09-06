//! Joining `.mdl`, `.vvd` and `.vtx` into drawable geometry.
//!
//! The three files describe one model from three directions and only mean
//! something together: the `.mdl` says which meshes exist and what each wears,
//! the `.vtx` says which vertices each mesh's triangles use, the `.vvd` holds
//! those vertices. This is where they meet.
//!
//! # The indirection that decides whether anything draws
//!
//! A `.vtx` index is **relative to its mesh**. Reaching the `.vvd` pool takes
//! two additions:
//!
//! ```text
//! pool index = model.vertex_index      // the model's base, from the .mdl
//!            + mesh.vertex_offset      // the mesh's base within the model
//!            + orig_mesh_vert_id       // the index itself, from the .vtx
//! ```
//!
//! Neither addition errors when omitted. Dropping `mesh.vertex_offset` draws a
//! recognisable model whose second and later meshes wear the first mesh's
//! geometry; dropping `model.vertex_index` does the same across body parts.
//! Both look like a content bug rather than a reader bug, which is why the
//! range check below is worth its cost.
//!
//! # Batching
//!
//! One batch per `(body part, model, material)`, concatenated in that order.
//!
//! Grouping by material is the same decision `world` made for faces — Valve's
//! *sort ID*, computed at load so nothing sorts per frame. Not grouping *across*
//! body parts is what keeps body-group selection possible later: a body part is
//! a set of alternatives, only one of which is drawn, so merging two of them
//! into one draw would make choosing between them impossible.

use super::mdl::Mdl;
use super::vtx::Vtx;
use super::vvd::Vvd;
use super::{Batch, StudioError, StudioModel};
use crate::materials::mesh::ModelVertex;

pub(super) fn build(
    mdl: &Mdl,
    vvd: &Vvd,
    vtx: &Vtx,
    resolve: impl Fn(&[String]) -> Option<String>,
) -> Result<StudioModel, StudioError> {
    check_checksums(mdl, vvd, vtx)?;
    check_shape(mdl, vtx)?;

    // Every mesh indexes the same pool, so the pool is uploaded once and the
    // batches index into it. `r_studiodraw.cpp` builds per-mesh vertex buffers
    // instead, which it has to because `IMesh` owned its vertices; nothing here
    // forces that, and one buffer per model is one binding per model.
    let vertices: Vec<ModelVertex> = vvd
        .vertices
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let tangent = vvd.tangent(i);
            ModelVertex {
                position: v.position.to_array(),
                normal: v.normal.to_array(),
                texcoord: v.texcoord.to_array(),
                tangent: tangent.to_array(),
                // Black until a `.vhv` supplies the baked light, which is
                // `ModelLighting::static_light`'s whole purpose — see
                // `portdocs/STUDIO.md` §5.2. Black rather than white because
                // the stream is pre-multiplied by a half, so white would be
                // twice the brightest value `vrad` can bake.
                color: [0.0, 0.0, 0.0, 0.0],
            }
        })
        .collect();

    // Resolve each texture slot once rather than once per mesh — a model with
    // eight meshes on one material would otherwise do the same lookup eight
    // times, and the lookup touches the filesystem.
    let materials: Vec<String> = (0..mdl.textures.len())
        .map(|i| {
            let candidates = mdl.material_candidates(i);
            resolve(&candidates)
                .or_else(|| candidates.first().cloned())
                .unwrap_or_default()
        })
        .collect();

    let mut indices: Vec<u32> = Vec::new();
    let mut batches: Vec<Batch> = Vec::new();

    for (bp_index, (mdl_part, vtx_part)) in
        mdl.body_parts.iter().zip(&vtx.body_parts).enumerate()
    {
        for (model_index, (mdl_model, vtx_model)) in
            mdl_part.models.iter().zip(&vtx_part.models).enumerate()
        {
            // LOD 0 — the finest. A model with no LODs at all in the `.vtx`
            // contributes nothing, which is a real case: 8 of Portal 2's static
            // prop meshes have no strip groups.
            let Some(lod) = vtx_model.lods.first() else {
                continue;
            };

            // Gather this model's meshes by material, preserving first-seen
            // order so the output is deterministic and reads in file order.
            let mut by_material: Vec<(usize, Vec<u32>)> = Vec::new();
            for (mesh_index, (mdl_mesh, vtx_mesh)) in
                mdl_model.meshes.iter().zip(&lod.meshes).enumerate()
            {
                if vtx_mesh.indices.is_empty() {
                    continue;
                }
                let base = mdl_model.vertex_index + mdl_mesh.vertex_offset;
                let slot = match by_material.iter().position(|(m, _)| *m == mdl_mesh.material) {
                    Some(slot) => slot,
                    None => {
                        by_material.push((mdl_mesh.material, Vec::new()));
                        by_material.len() - 1
                    }
                };

                for &index in &vtx_mesh.indices {
                    let pool = base + index;
                    if pool as usize >= vertices.len() {
                        return Err(StudioError::Corrupt {
                            path: vtx.path.clone(),
                            what: format!(
                                "body part {bp_index} model {model_index} mesh {mesh_index} \
                                 names vertex {pool} of {} (model base {}, mesh offset {}, \
                                 index {index})",
                                vertices.len(),
                                mdl_model.vertex_index,
                                mdl_mesh.vertex_offset
                            ),
                        });
                    }
                    by_material[slot].1.push(pool);
                }
            }

            for (material, mut group) in by_material {
                if group.is_empty() {
                    continue;
                }
                let first_index = indices.len() as u32;
                let index_count = group.len() as u32;
                indices.append(&mut group);
                batches.push(Batch {
                    material: materials.get(material).cloned().unwrap_or_default(),
                    first_index,
                    index_count,
                    body_part: bp_index as u16,
                    model: model_index as u16,
                });
            }
        }
    }

    Ok(StudioModel {
        path: mdl.path.clone(),
        name: mdl.name.clone(),
        bounds: mdl.bounds,
        illum_position: mdl.illum_position,
        flags: mdl.flags,
        checksum: mdl.checksum,
        vertices,
        indices,
        batches,
    })
}

/// The format's own guard against a stale companion file.
///
/// A `.vtx` built against a different revision of a `.mdl` parses perfectly and
/// indexes the wrong vertices, so this is the only cheap way to catch it.
fn check_checksums(mdl: &Mdl, vvd: &Vvd, vtx: &Vtx) -> Result<(), StudioError> {
    for (path, found) in [(&vvd.path, vvd.checksum), (&vtx.path, vtx.checksum)] {
        if found != mdl.checksum {
            return Err(StudioError::ChecksumMismatch {
                path: path.clone(),
                mdl_path: mdl.path.clone(),
                found,
                expected: mdl.checksum,
            });
        }
    }
    Ok(())
}

/// The `.vtx` hierarchy must mirror the `.mdl`'s exactly.
///
/// Checked up front so the `zip`s above cannot silently pair a body part with
/// the wrong one — `zip` stops at the shorter side, which would drop geometry
/// rather than report anything.
fn check_shape(mdl: &Mdl, vtx: &Vtx) -> Result<(), StudioError> {
    let corrupt = |what: String| StudioError::Corrupt {
        path: vtx.path.clone(),
        what,
    };

    if mdl.body_parts.len() != vtx.body_parts.len() {
        return Err(corrupt(format!(
            "it has {} body parts but {} has {}",
            vtx.body_parts.len(),
            mdl.path,
            mdl.body_parts.len()
        )));
    }
    for (i, (mdl_part, vtx_part)) in mdl.body_parts.iter().zip(&vtx.body_parts).enumerate() {
        if mdl_part.models.len() != vtx_part.models.len() {
            return Err(corrupt(format!(
                "body part {i} has {} models but {} has {}",
                vtx_part.models.len(),
                mdl.path,
                mdl_part.models.len()
            )));
        }
        for (j, (mdl_model, vtx_model)) in mdl_part.models.iter().zip(&vtx_part.models).enumerate()
        {
            // A `.vtx` LOD holds one entry per `.mdl` mesh; an absent LOD 0 is
            // handled by the caller, but a present one must match.
            if let Some(lod) = vtx_model.lods.first() {
                if mdl_model.meshes.len() != lod.meshes.len() {
                    return Err(corrupt(format!(
                        "body part {i} model {j} LOD 0 has {} meshes but {} has {}",
                        lod.meshes.len(),
                        mdl.path,
                        mdl_model.meshes.len()
                    )));
                }
            }
        }
    }
    Ok(())
}
