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
//! One batch per `(body part, model, replaceable texture slot)`, concatenated
//! in that order. The *slot*, not the material it resolves to: a slot means the
//! same thing in every skin family and a material does not, so a batch keyed on
//! one carries a material per family and stays a single draw. See
//! `portdocs/STUDIO.md` §14.
//!
//! Grouping by material is the same decision `world` made for faces — Valve's
//! *sort ID*, computed at load so nothing sorts per frame. Not grouping *across*
//! body parts is what keeps body-group selection possible later: a body part is
//! a set of alternatives, only one of which is drawn, so merging two of them
//! into one draw would make choosing between them impossible.

use super::mdl::Mdl;
use super::vtx::Vtx;
use super::vvd::Vvd;
use super::{Batch, HardwareMesh, StudioError, StudioModel};
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
                // `mstudioboneweight_t` straight through, padded to the four
                // indices the vertex format takes. The unused slots are bone 0
                // at weight 0, which contributes nothing whatever bone 0 is
                // doing — as opposed to leaving them uninitialised, which
                // would read a bone the model may not have.
                bone_weights: v.bones.weights,
                bone_indices: [
                    v.bones.bones[0],
                    v.bones.bones[1],
                    v.bones.bones[2],
                    0,
                ],
                // No colour here: the baked static light is per *placement*,
                // not per model, and lives in the second vertex stream that
                // `engine::world::props` fills from the map's `.vhv` files.
                // See `materials::mesh::StaticLightVertex`.
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

    // Every weight names a bone the model has. The shader indexes the palette
    // by these without checking, and an index past its end reads zero under
    // `wgpu`'s bounds checking — which collapses the vertex onto the entity's
    // origin rather than erroring. Cheaper to refuse the file here, where the
    // message can say which model and which bone.
    let bone_count = mdl.bones.len().max(1) as u32;
    for (i, v) in vvd.vertices.iter().enumerate() {
        for slot in 0..usize::from(v.bones.count).min(3) {
            if u32::from(v.bones.bones[slot]) >= bone_count {
                return Err(StudioError::Corrupt {
                    path: vvd.path.clone(),
                    what: format!(
                        "vertex {i} is weighted to bone {} of {}",
                        v.bones.bones[slot],
                        mdl.bones.len()
                    ),
                });
            }
        }
    }

    let mut indices: Vec<u32> = Vec::new();
    let mut batches: Vec<Batch> = Vec::new();
    let mut meshes: Vec<HardwareMesh> = Vec::new();

    for (bp_index, (mdl_part, vtx_part)) in mdl.body_parts.iter().zip(&vtx.body_parts).enumerate() {
        for (model_index, (mdl_model, vtx_model)) in
            mdl_part.models.iter().zip(&vtx_part.models).enumerate()
        {
            // LOD 0 — the finest. A model with no LODs at all in the `.vtx`
            // contributes nothing, which is a real case: 8 of Portal 2's static
            // prop meshes have no strip groups.
            let Some(lod) = vtx_model.lods.first() else {
                continue;
            };

            // Gather this model's meshes by **replaceable texture slot**,
            // preserving first-seen order so the output is deterministic and
            // reads in file order.
            //
            // The slot and not the resolved material, which is what makes a
            // batch skin-independent: two slots that share a material in
            // family 0 can differ in family 3, so merging them here would make
            // a skin unrepresentable. It costs nothing — every shipped model
            // that does this has one mesh per slot anyway.
            let mut by_material: Vec<(usize, Vec<u32>)> = Vec::new();
            for (mesh_index, (mdl_mesh, vtx_mesh)) in
                mdl_model.meshes.iter().zip(&lod.meshes).enumerate()
            {
                let base = mdl_model.vertex_index + mdl_mesh.vertex_offset;
                // Recorded for **every** mesh, drawn or not, because a `.vhv`
                // has one entry per studio mesh at this LOD and the two lists
                // are matched positionally. Skipping the empty ones here would
                // shift every later mesh's colours onto the wrong vertices.
                //
                // The `.vtx`'s strip-group vertex tables, rebased onto the
                // pool: hardware vertex `j` of this mesh is pool vertex
                // `base + hardware[j]`. See `HardwareMesh`.
                meshes.push(HardwareMesh {
                    vertices: vtx_mesh.hardware.iter().map(|&v| base + v).collect(),
                });
                if vtx_mesh.indices.is_empty() {
                    continue;
                }
                let slot = match by_material
                    .iter()
                    .position(|(m, _)| *m == mdl_mesh.material)
                {
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

            for (slot, group) in by_material {
                if group.is_empty() {
                    continue;
                }
                let first_index = indices.len() as u32;
                let index_count = group.len() as u32;
                indices.extend(group);
                batches.push(Batch {
                    // One material per skin family, resolved through that
                    // family's row. `slot` is the *column* — a replaceable
                    // texture slot, which is what `mstudiomesh_t::material`
                    // holds — so this is `pSkinRef[pMesh->material]` once per
                    // family rather than once per draw.
                    materials: mdl
                        .skin_families
                        .iter()
                        .map(|row| {
                            row.get(slot)
                                .and_then(|&texture| materials.get(usize::from(texture)))
                                .cloned()
                                .unwrap_or_default()
                        })
                        .collect(),
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
        bounds: mdl.render_bounds(),
        illum_position: mdl.illum_position,
        flags: mdl.flags,
        checksum: mdl.checksum,
        vertices,
        indices,
        batches,
        skin_families: mdl.skin_families.len(),
        meshes,
        bones: mdl.bones.clone(),
        sequences: mdl.sequences.clone(),
        animations: mdl.animations.clone().into(),
        attachments: mdl.attachments.clone(),
        // `StudioModel::load` fills this in; `assemble` joins three files and
        // has no filesystem to resolve a fourth against.
        includes: Vec::new(),
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
