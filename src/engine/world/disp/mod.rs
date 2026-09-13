//! Displacements: the terrain a map draws in place of a four-sided face.
//!
//! `portdocs/ENGINE.md` §7.15 and `portdocs/ENGINE_WORLD_DISP.md`. The
//! *collision* half of the same lumps is
//! [`trace::disp`](crate::engine::trace), and landed first — so the grid of
//! positions is already a solved problem and lives on [`Bsp`], shared by both,
//! precisely so the drawn surface and the solid one cannot become different
//! surfaces.
//!
//! What is here is the other four things a drawable vertex needs:
//!
//! - a **texture coordinate**, bilinear over the base face's four corner
//!   coordinates — *not* the planar projection evaluated at the displaced
//!   position, which is metres away from the flat quad and reads as a stretched
//!   mapping ([`Displacement::build`]);
//! - a **lightmap coordinate**, which is not the base face's either: `vrad`
//!   bakes a displacement's lightmap against the *grid*, and the base face's
//!   luxel corners are computed and then overwritten with a canonical square
//!   twenty lines later (`BuildDispSurfInit`, `disp_mapload.cpp:165`);
//! - a **blend alpha**, `CDispVert::alpha / 255`, which is the only thing in
//!   the vertex not derivable from the base face — it is what
//!   `WorldVertexTransition` blends `$basetexture2` with;
//! - an **index list**, which is [`tessellate`]'s and is the hard part.
//!
//! **Normals and tangents are deliberately not built.** A
//! [`WorldVertex`](crate::materials::mesh::WorldVertex) has neither, because
//! `LightmappedGeneric`'s bumped path dots a tangent-space normal against the
//! constant basis the lightmaps were baked in and never needs a world-space
//! frame. Building them means porting `CalcNormalFromEdges`, `DoesEdgeExist`
//! and `SmoothDispSurfNormals` — the last of which needs the neighbour tables
//! this port does not read — to fill two attributes nothing samples. The
//! condition that reverses it is `$envmap` or `$seamless_scale`, which are the
//! two features that make a *world surface* want a normal; see
//! `portdocs/ENGINE_WORLD_DISP.md` §8.
//!
//! Nothing here allocates a lightmap block, splits a batch or picks a material.
//! A displacement is a surface with a material and some triangles, so it goes
//! through [`world`](super)'s existing `group_faces` → `place_lightmap` →
//! `build_page_meshes` pipeline exactly as an ordinary face does — which is
//! also what Valve's `DispInfo_CreateMaterialGroups` does, grouping by
//! `(lightmapPageID, material)`, the pair a [`Batch`](super::Batch) already is.

mod tessellate;

use glam::Vec3;

use crate::engine::world::bsp::{Bsp, DispInfo, Face};

/// One displacement's drawable grid.
///
/// Vertices are in grid order, `i * spacing + j`, which is what
/// [`indices`](Displacement::indices) names — `i` running along the base quad's
/// `p0 → p1` edge and `j` across to `p3 → p2`, the same order
/// [`Bsp::disp_grid`] builds and the same one `trace::disp` collides with.
pub(super) struct Displacement {
    pub(super) vertices: Vec<DispVertex>,
    /// Triangles, **reversed** out of Valve's winding the same way an
    /// ordinary world face's fan is — see [`Displacement::build`] and
    /// `rustdocs/ENGINE.md` gotcha 1.
    pub(super) indices: Vec<u16>,
}

/// One grid vertex, before it knows where its lightmap landed.
///
/// [`luxel`](DispVertex::luxel) is in *luxels*, not page coordinates, for the
/// same reason [`Bsp::lightmap_coordinate`] is: where the surface's block sits
/// in the atlas is decided after the geometry is built, by the packer.
pub(super) struct DispVertex {
    pub(super) position: Vec3,
    pub(super) texcoord: [f32; 2],
    pub(super) luxel: [f32; 2],
    /// `$basetexture2`'s blend factor, 0..1 — `CDispVert::alpha / 255`.
    pub(super) alpha: f32,
}

impl Displacement {
    /// Builds the drawable grid for a face that names a displacement.
    ///
    /// `CCoreDispInfo::Create` (`builddisp.cpp:2046`) minus the LOD tree, the
    /// collision data (`trace/`'s) and the normals and tangent spaces (above),
    /// plus `BuildDispSurfInit`'s lightmap-corner substitution.
    ///
    /// `None` when the face names no displacement, names one the file does not
    /// have, or is not a quad — `if ( pFaces->numedges > 4 ) continue;`
    /// (`disp_mapload.cpp:125`). Measured against the depot: all 1,181 shipped
    /// displacements have exactly four edges, so this is a guard against a
    /// malformed map rather than a case that happens.
    pub(super) fn build(bsp: &Bsp, face: &Face) -> Option<Displacement> {
        let index = usize::try_from(face.disp_info).ok()?;
        let info = bsp.disp_info.get(index)?;
        let points = bsp.disp_base_quad(face, info)?;

        let positions = bsp.disp_grid(info, &points);
        let spacing = (1usize << info.power) + 1;
        debug_assert_eq!(positions.len(), spacing * spacing);

        // The base face's texture coordinates at its four *flat* corners, in
        // the rotated order the grid runs in. Evaluating the projection at a
        // displaced position instead is the single most plausible wrong thing
        // to do here, and it does not look wrong until you stand next to a
        // cliff.
        let texcoords = points.map(|p| bsp.texture_coordinate(face, p));

        // `BuildDispSurfInit` (`disp_mapload.cpp:165`) computes the face's own
        // luxel corners and then, twenty lines later, throws them away and
        // writes a canonical square: corner 0 at (0,0), 1 at (0,h), 2 at (w,h),
        // 3 at (w,0), each `+ 0.5`. Valve's comment calls it "currently done
        // redundantly … here to get things running for (GDC, E3)", but it is
        // what `vrad` bakes against — a displacement's lightmap is
        // parameterized by the grid, not by the texinfo's lightmap axes.
        //
        // `lightmap_size` is the *extents*, one less than the block dimensions
        // `Bsp::face_lightmap_size` returns, so the range below is
        // `0.5 ..= w + 0.5` across a block `w + 1` wide: the centre of the
        // first luxel to the centre of the last.
        let (width, height) = (face.lightmap_size[0] as f32, face.lightmap_size[1] as f32);

        let first_vert = info.disp_vert_start as usize;
        let step = 1.0 / (spacing - 1) as f32;
        let mut vertices = Vec::with_capacity(positions.len());
        for i in 0..spacing {
            // `CalcDispSurfCoords` (`builddisp.cpp:1592`): the same bilinear
            // interpolation the positions use, over corner values instead of
            // corner points.
            let ends = [
                lerp2(texcoords[0], texcoords[1], i as f32 * step),
                lerp2(texcoords[3], texcoords[2], i as f32 * step),
            ];
            for j in 0..spacing {
                let n = i * spacing + j;
                vertices.push(DispVertex {
                    position: positions[n],
                    texcoord: lerp2(ends[0], ends[1], j as f32 * step),
                    // The canonical square above, collapsed: `u` runs with `j`
                    // and `v` with `i`.
                    luxel: [
                        0.5 + width * j as f32 * step,
                        0.5 + height * i as f32 * step,
                    ],
                    // `flAlpha = GetAlpha(i) * (1/255), clamped`
                    // (`disp_mapload.cpp:329`).
                    alpha: (bsp.disp_verts[first_vert + n].alpha / 255.0).clamp(0.0, 1.0),
                });
            }
        }

        // **Reversed, exactly as `world::build_page_meshes` reverses a face's
        // fan**, and for the same reason: Valve's `D3DCULL_CCW` and this port's
        // `front_face: Ccw` read identically and are not the same thing, so
        // Valve-authored geometry is `Cw`-front here. `rustdocs/ENGINE.md`
        // gotcha 1 has the full argument.
        //
        // Done here rather than inside [`tessellate`] so that the walk stays a
        // transcription of `TesselateDisplacement` and can be compared, winding
        // and all, against `trace::disp`'s collision list — which is what
        // `tessellation_matches_the_collision_surface` does.
        //
        // The direction is **measured, not argued**: two independent winding
        // conventions meet here — Valve's `(v2-v0) × (v1-v0)` collision normal
        // and this port's reversal of a world fan — and an analytical chain
        // through both is exactly the kind that comes out plausibly backwards.
        // `every_shipped_map_builds_its_displacement_geometry` checks all 1,181
        // shipped patches against the rendered winding of the base face each
        // was carved from, which is the one reference that cannot drift.
        let mut indices = tessellate::tessellate(info.power, &info.allowed_verts);
        for tri in indices.chunks_exact_mut(3) {
            tri.swap(1, 2);
        }

        Some(Displacement { vertices, indices })
    }

    /// How many vertices a displacement of this power has, without building it
    /// — what the batch splitter needs to know before it commits.
    pub(super) fn vertex_count(bsp: &Bsp, face: &Face) -> usize {
        usize::try_from(face.disp_info)
            .ok()
            .and_then(|i| bsp.disp_info.get(i))
            .map_or(0, |info| DispInfo::vert_count(info.power))
    }
}

fn lerp2(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::trace::fixture::Fixture;
    use crate::engine::trace::Contents;

    /// A flat 64x64 patch in the z=0 plane, power `power`, with the grid's
    /// `i` axis running +y and its `j` axis running +x.
    fn flat(power: i32) -> Bsp {
        let mut fixture = Fixture::default();
        let corners = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 64.0, 0.0),
            Vec3::new(64.0, 64.0, 0.0),
            Vec3::new(64.0, 0.0, 0.0),
        ];
        fixture.add_displacement(corners, corners[0], power, Contents::SOLID, 0, |_, _| 0.0);
        fixture.bsp()
    }

    #[test]
    fn the_grid_runs_the_way_the_collision_grid_does() {
        let bsp = flat(2);
        let disp = Displacement::build(&bsp, &bsp.faces[0]).expect("a quad displacement");
        let spacing = 5;
        assert_eq!(disp.vertices.len(), spacing * spacing);

        // `i` along p0 -> p1, which is +y; `j` along p0 -> p3, which is +x.
        let at = |i: usize, j: usize| disp.vertices[i * spacing + j].position;
        assert_eq!(at(0, 0), Vec3::ZERO);
        assert_eq!(at(4, 0), Vec3::new(0.0, 64.0, 0.0));
        assert_eq!(at(0, 4), Vec3::new(64.0, 0.0, 0.0));
        assert_eq!(at(4, 4), Vec3::new(64.0, 64.0, 0.0));
    }

    /// The luxel square of `BuildDispSurfInit`: `0.5` at the first vertex and
    /// `extent + 0.5` at the last, on both axes, with `u` following `j`.
    ///
    /// Getting the axes the wrong way round mirrors every displacement's
    /// lighting about its diagonal, which on a mostly-flat patch looks like
    /// nothing at all.
    #[test]
    fn luxel_coordinates_are_the_canonical_square() {
        let mut bsp = flat(3);
        bsp.faces[0].lightmap_size = [12, 6];
        let disp = Displacement::build(&bsp, &bsp.faces[0]).unwrap();
        let spacing = 9;
        let at = |i: usize, j: usize| disp.vertices[i * spacing + j].luxel;

        assert_eq!(at(0, 0), [0.5, 0.5]);
        assert_eq!(at(0, 8), [12.5, 0.5]);
        assert_eq!(at(8, 0), [0.5, 6.5]);
        assert_eq!(at(8, 8), [12.5, 6.5]);
        // Evenly spaced in between, which is what makes it bilinear.
        assert_eq!(at(4, 4), [6.5, 3.5]);
    }

    /// A one-luxel-wide surface is `lightmap_size` `[0, 0]`, and every vertex
    /// then wants the middle of that luxel — which the canonical square gives
    /// for free, with no special case.
    #[test]
    fn a_degenerate_lightmap_collapses_to_one_luxel() {
        let bsp = flat(2);
        assert_eq!(bsp.faces[0].lightmap_size, [0, 0]);
        let disp = Displacement::build(&bsp, &bsp.faces[0]).unwrap();
        assert!(disp.vertices.iter().all(|v| v.luxel == [0.5, 0.5]));
    }

    /// Texture coordinates come from the base face's corners and interpolate
    /// across the grid — they are not the projection of the displaced position.
    #[test]
    fn texture_coordinates_are_bilinear_over_the_base_corners() {
        let mut bsp = flat(2);
        // One texel per world unit on a 64x64 texture, so corner coordinates
        // are the corners' world positions over 64.
        bsp.texinfo[0].texture_vecs = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]];
        // Push the middle of the patch a long way off the base plane. If the
        // coordinates were projected from the displaced position, the centre
        // would not still be the centre.
        let spacing = 5;
        for i in 0..spacing {
            for j in 0..spacing {
                bsp.disp_verts[i * spacing + j].dist = 256.0;
            }
        }

        let disp = Displacement::build(&bsp, &bsp.faces[0]).unwrap();
        let at = |i: usize, j: usize| disp.vertices[i * spacing + j].texcoord;
        assert_eq!(at(0, 0), [0.0, 0.0]);
        assert_eq!(at(0, 4), [1.0, 0.0]);
        assert_eq!(at(4, 0), [0.0, 1.0]);
        assert_eq!(at(2, 2), [0.5, 0.5]);
        assert_ne!(disp.vertices[0].position.z, 256.0 * 0.0);
    }

    /// Alpha is the blend factor, scaled out of the lump's 0..255 and clamped.
    #[test]
    fn alpha_is_normalized_and_clamped() {
        let mut bsp = flat(2);
        bsp.disp_verts[0].alpha = 0.0;
        bsp.disp_verts[1].alpha = 255.0;
        bsp.disp_verts[2].alpha = 127.5;
        // `vrad` has been known to write outside the range; Valve clamps.
        bsp.disp_verts[3].alpha = 400.0;
        bsp.disp_verts[4].alpha = -5.0;

        let disp = Displacement::build(&bsp, &bsp.faces[0]).unwrap();
        assert_eq!(disp.vertices[0].alpha, 0.0);
        assert_eq!(disp.vertices[1].alpha, 1.0);
        assert_eq!(disp.vertices[2].alpha, 0.5);
        assert_eq!(disp.vertices[3].alpha, 1.0);
        assert_eq!(disp.vertices[4].alpha, 0.0);
    }

    /// The drawn surface and the solid one are the same surface — the reason
    /// [`Bsp::disp_grid`] is shared rather than written twice.
    #[test]
    fn the_drawn_grid_is_the_collided_grid() {
        let bsp = flat(3);
        let info = &bsp.disp_info[0];
        let points = bsp.disp_base_quad(&bsp.faces[0], info).unwrap();
        let collided = bsp.disp_grid(info, &points);
        let drawn = Displacement::build(&bsp, &bsp.faces[0]).unwrap();

        assert_eq!(collided.len(), drawn.vertices.len());
        for (c, d) in collided.iter().zip(&drawn.vertices) {
            assert_eq!(*c, d.position);
        }
    }

    /// A face that names no displacement, and one that names a missing entry,
    /// are both `None` rather than a panic or an empty patch that draws.
    #[test]
    fn a_face_without_a_displacement_builds_nothing() {
        let mut bsp = flat(2);
        bsp.faces[0].disp_info = -1;
        assert!(Displacement::build(&bsp, &bsp.faces[0]).is_none());

        bsp.faces[0].disp_info = 7;
        assert!(Displacement::build(&bsp, &bsp.faces[0]).is_none());
    }
}
