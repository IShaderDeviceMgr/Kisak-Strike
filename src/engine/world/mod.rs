//! The world: a loaded map and the geometry it draws.
//!
//! `portdocs/ENGINE.md` §7.14 sizes the original at ~16,300 lines
//! (`modelloader.cpp`, `cmodel.cpp`, `mod_vis.cpp`, …). This is the slice of it
//! that gets a map on screen: read the `.bsp` ([`bsp`]), turn its faces into
//! vertex and index buffers grouped by material, and draw them. Everything the
//! rest of that subsystem does — visibility, collision, displacements, static
//! props, brush entities, `.mdl` models — is listed under "Not loaded" below
//! and arrives with the subsystem that needs it.
//!
//! Three of Valve's structural decisions are deliberately *not* reproduced:
//!
//! - **No hunk allocator.** `zone.cpp`/`mem.cpp` exist because the map had to
//!   live in one arena that could be freed in a single call. `Vec` and `Drop`
//!   do that, which is why `portdocs/ENGINE.md` §7.14 marks those files
//!   "delete outright".
//! - **No `model_t` cache.** `modelloader`'s reference-counted dictionary of
//!   loaded models is a [`World`] value that the engine owns and replaces.
//! - **No surface-to-material back-pointer.** Valve hung an `IMaterial*` off
//!   every `mtexinfo_t` and re-sorted surfaces by it every frame
//!   (`gl_rsurf.cpp`). Grouping happens once, here, at load.

pub mod bsp;

use std::collections::BTreeMap;
use std::sync::Arc;

use glam::Vec3;

use crate::filesystem::Vfs;
use crate::materials::context::Pass;
use crate::materials::mesh::{IndexBuffer, SimpleVertex, Vertex, VertexBuffer};
use crate::materials::{Material, MaterialCache};

use bsp::{Bsp, BspError, Face};

/// Where a batch has to be split.
///
/// [`IndexBuffer`](crate::materials::mesh::IndexBuffer) is 16-bit, so an index
/// cannot name a vertex past 65,535. `rustdocs/MATERIALS.md` records that
/// 32-bit indices exist in Valve's enum and that nothing in the engine's draw
/// paths asks for them — this is why that holds for world geometry too: the
/// world is split into per-material batches long before it reaches this bound,
/// and a batch that would exceed it is split again rather than promoted to
/// wider indices.
const MAX_BATCH_VERTICES: usize = 1 << 16;

/// The player's eye height above `info_player_start`'s origin.
///
/// `VEC_VIEW` (`game/shared/shareddefs.h`) — the entity's origin is at the
/// player's feet, and a camera placed there looks at the floor.
const EYE_HEIGHT: f32 = 64.0;

/// Anything that stops a map from loading.
#[derive(Debug, thiserror::Error)]
pub enum WorldError {
    #[error(transparent)]
    Bsp(#[from] BspError),

    #[error("{map} has no drawable faces")]
    NothingToDraw { map: String },
}

/// One draw call's worth of world geometry: every face in the map that shares
/// a material, up to [`MAX_BATCH_VERTICES`].
///
/// This is `CMatRenderContext`'s static-vertices/static-indices case. The
/// engine's own world draw uses static vertices with *dynamic* indices gathered
/// per frame from the PVS (`gl_rsurf.cpp:1168`), and
/// `rustdocs/MATERIALS.md` explains why `VertexSlice` and `IndexSlice` are
/// separate arguments for exactly that reason. **Both halves are static here**
/// because there is no visibility yet: every face is drawn every frame, so
/// there is nothing per-frame to gather. When `mod_vis` lands, the vertex
/// buffers stay and the index buffers become dynamic.
pub struct Batch {
    pub material: Arc<Material>,
    vertices: VertexBuffer,
    indices: IndexBuffer,
}

/// Where the view starts when a map is loaded.
#[derive(Debug, Clone, Copy)]
pub struct Spawn {
    /// Eye position in world space — the entity origin plus [`EYE_HEIGHT`].
    pub eye: Vec3,
    /// Valve's `angles` are `pitch yaw roll`, in degrees.
    pub pitch: f32,
    pub yaw: f32,
}

/// What the map turned out to contain. Printed at load, and the cheapest way to
/// tell a map that loaded from a map that loaded *and drew*.
#[derive(Debug, Clone, Default)]
pub struct WorldStats {
    pub faces_total: usize,
    pub faces_drawn: usize,
    /// Faces skipped for a [`surf`](bsp::surf) flag — sky, nodraw, hints.
    pub faces_not_drawn: usize,
    /// Faces skipped because they are displacements, whose geometry lives in
    /// lumps this reader does not open yet.
    pub faces_displaced: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub materials: usize,
    /// Materials that resolved to the error checkerboard. Expect this to be
    /// most of them until `LightmappedGeneric` exists.
    pub materials_missing: usize,
    /// Faces carrying explicit primitives, which are fan-triangulated here
    /// instead. See [`build_meshes`].
    pub faces_with_primitives: usize,
}

/// A loaded map.
pub struct World {
    pub name: String,
    /// The `.bsp` version and `mapRevision` this was built from. Logged,
    /// because "which map exactly" is the first question about a rendering bug.
    pub bsp_version: i32,
    pub bsp_revision: i32,
    pub batches: Vec<Batch>,
    /// The world model's bounding box, in Source units.
    pub bounds: (Vec3, Vec3),
    pub spawn: Option<Spawn>,
    /// `worldspawn`'s `skyname`. Read, recorded, and not yet drawn — the 3D
    /// skybox is a second camera over a second set of geometry.
    pub sky_name: Option<String>,
    pub stats: WorldStats,
}

impl World {
    /// Reads `maps/<name>.bsp` and uploads its geometry.
    ///
    /// This is `HostState_NewGame` → `Host_NewGame` → `modelloader->GetModelForName`
    /// (`engine/host_state.cpp:428`) collapsed into the one step that currently
    /// has meaning. A material that fails to load is the error material, not an
    /// error — `MaterialCache::load` cannot fail — so the only failures here are
    /// a missing or malformed `.bsp`.
    pub fn load(
        vfs: &Vfs,
        materials: &mut MaterialCache,
        device: &wgpu::Device,
        name: &str,
    ) -> Result<World, WorldError> {
        let bsp = Bsp::load(vfs, name)?;
        let (meshes, mut stats) = build_meshes(&bsp);

        if meshes.is_empty() {
            return Err(WorldError::NothingToDraw {
                map: name.to_owned(),
            });
        }

        let error_material = materials.error_material();
        let mut batches = Vec::with_capacity(meshes.len());
        let mut last_material: Option<&str> = None;

        for mesh in &meshes {
            let material = materials.load(vfs, &mesh.material);

            // Count each distinct material once, not once per batch: a material
            // covering more than 65,536 vertices produces several batches.
            if last_material != Some(mesh.material.as_str()) {
                last_material = Some(mesh.material.as_str());
                stats.materials += 1;
                if Arc::ptr_eq(&material, &error_material) {
                    stats.materials_missing += 1;
                }
            }

            // `Pass::draw` panics on a layout mismatch, by design — see
            // `rustdocs/MATERIALS.md` gotcha #8. Every shader in the set today
            // reads `Simple`, so this cannot fire; it is here because the first
            // shader that reads a different layout (`LightmappedGeneric`, which
            // wants lightmap coordinates) will need this builder to produce that
            // vertex struct, and drawing its geometry through the wrong one
            // should be a checkerboard rather than a panic.
            let material = if material.shader.vertex_layout() == <SimpleVertex as Vertex>::LAYOUT {
                material
            } else {
                Arc::clone(&error_material)
            };

            batches.push(Batch {
                material,
                vertices: VertexBuffer::new(device, &mesh.material, &mesh.vertices),
                indices: IndexBuffer::new(device, &mesh.material, &mesh.indices),
            });
        }

        let model = bsp.world_model();
        let entities = bsp.entities();

        Ok(World {
            name: name.to_owned(),
            bsp_version: bsp.version,
            bsp_revision: bsp.revision,
            batches,
            bounds: (Vec3::from(model.mins), Vec3::from(model.maxs)),
            spawn: find_spawn(&entities),
            sky_name: entities
                .iter()
                .find(|e| e.classname() == Some("worldspawn"))
                .and_then(|e| e.get("skyname"))
                .map(str::to_owned),
            stats,
        })
    }

    /// Records every batch into an open pass.
    ///
    /// The model matrix is the identity: world geometry is already in world
    /// space, which is the whole difference between the world model and the
    /// brush models that are not drawn yet.
    pub fn draw(&self, pass: &mut Pass<'_>) {
        for batch in &self.batches {
            pass.draw(
                &batch.material,
                &batch.vertices.slice(),
                &batch.indices.slice(),
                glam::Mat4::IDENTITY,
            );
        }
    }

    /// The centre of the world's bounding box — where the view goes when the
    /// map has no `info_player_start`.
    pub fn center(&self) -> Vec3 {
        (self.bounds.0 + self.bounds.1) * 0.5
    }

    /// A one-line summary for the startup log.
    pub fn summary(&self) -> String {
        let s = &self.stats;
        let primitives = if s.faces_with_primitives > 0 {
            format!(", {} fan-approximated", s.faces_with_primitives)
        } else {
            String::new()
        };
        format!(
            "{} (bsp v{}, revision {}): {}/{} faces drawn \
             ({} hidden, {} displacement{primitives}), \
             {} vertices, {} triangles, {} batches, \
             {} materials ({} missing)",
            self.name,
            self.bsp_version,
            self.bsp_revision,
            s.faces_drawn,
            s.faces_total,
            s.faces_not_drawn,
            s.faces_displaced,
            s.vertices,
            s.triangles,
            self.batches.len(),
            s.materials,
            s.materials_missing,
        )
    }
}

/// Geometry for one batch, before it reaches the GPU.
///
/// Split out from [`World::load`] so that face selection, texture-coordinate
/// generation and batch splitting are testable without a device — the
/// interesting logic is all here, and none of it needs a GPU to be wrong.
struct Mesh {
    material: String,
    vertices: Vec<SimpleVertex>,
    indices: Vec<u16>,
}

/// Turns the world model's faces into per-material meshes.
///
/// Faces are grouped by material name and emitted in name order, so the same
/// `.bsp` always produces the same batches. Valve sorted by the material
/// *pointer* and re-sorted per frame; sorting by name once at load costs
/// nothing and makes the output reproducible.
fn build_meshes(bsp: &Bsp) -> (Vec<Mesh>, WorldStats) {
    let mut stats = WorldStats::default();
    let mut groups: BTreeMap<&str, Vec<&Face>> = BTreeMap::new();

    for face in bsp.model_faces(bsp.world_model()) {
        stats.faces_total += 1;

        // A displacement's rendered geometry is a subdivided grid in
        // `LUMP_DISPINFO`/`LUMP_DISP_VERTS`, not this face's winding. Drawing
        // the face anyway gives the flat quad the displacement was carved from
        // — a floor where there should be terrain — so it is skipped until
        // `world/disp/` exists (`portdocs/ENGINE.md` §7.15).
        if face.disp_info >= 0 {
            stats.faces_displaced += 1;
            continue;
        }
        if face.num_edges < 3 {
            stats.faces_not_drawn += 1;
            continue;
        }

        let Some(info) = bsp.texinfo.get(face.tex_info.max(0) as usize) else {
            stats.faces_not_drawn += 1;
            continue;
        };
        if face.tex_info < 0 || info.flags & bsp::surf::NOT_DRAWN != 0 {
            stats.faces_not_drawn += 1;
            continue;
        }
        let Some(material) = bsp.face_material(face) else {
            stats.faces_not_drawn += 1;
            continue;
        };

        stats.faces_drawn += 1;
        if face.prim_count() > 0 {
            stats.faces_with_primitives += 1;
        }
        groups.entry(material).or_default().push(face);
    }

    let mut meshes = Vec::new();
    for (material, faces) in groups {
        let mut vertices: Vec<SimpleVertex> = Vec::new();
        let mut indices: Vec<u16> = Vec::new();

        for face in faces {
            let count = face.num_edges as usize;

            // Split before the face that would overflow 16-bit indices, never
            // in the middle of one: a face's vertices have to be contiguous for
            // the fan below to index them.
            if vertices.len() + count > MAX_BATCH_VERTICES {
                stats.vertices += vertices.len();
                stats.triangles += indices.len() / 3;
                meshes.push(Mesh {
                    material: material.to_owned(),
                    vertices: std::mem::take(&mut vertices),
                    indices: std::mem::take(&mut indices),
                });
            }

            let base = vertices.len() as u16;
            for position in bsp.face_vertices(face) {
                vertices.push(SimpleVertex::new(
                    position.to_array(),
                    bsp.texture_coordinate(face, position),
                ));
            }

            // `BuildIndicesForSurface` (`engine/gl_rsurf.h:145`): a face is a
            // convex polygon, so it triangulates as a fan from its first
            // vertex. Valve's `FastPolygon` is this loop with the bounds
            // checks removed.
            //
            // **The fan is emitted in reverse**, and that is a real
            // divergence rather than a slip. Measured against
            // `sp_a1_intro1`: in file order every world surface is
            // back-facing here and the map draws as an empty clear colour.
            // Why, precisely:
            //
            //   - Valve sets `D3DRS_CULLMODE = D3DCULL_CCW`
            //     (`shaderapidx9/shaderapidx8.cpp:4067`), and its own D3D→GL
            //     layer translates that to `glFrontFace(GL_CCW)` with back-face
            //     culling on (`shaderapidx9/dxabstract.cpp:4107`) — which reads
            //     exactly like this port's `front_face: Ccw, cull_mode: Back`.
            //   - It is not the same thing. GL's framebuffer origin is
            //     bottom-left and WebGPU's is top-left, and facing is decided
            //     *after* the viewport transform that flips between them. The
            //     same `Ccw` therefore names the opposite set of triangles, so
            //     Valve's content is `Cw`-front here.
            //
            // The reversal is done here, once, at the boundary where external
            // content enters — the same treatment `rustdocs/MATERIALS.md`
            // gives Valve's row-major matrices, which are transposed on the
            // way in and never again.
            //
            // **The alternative is to flip `front_face` in `PipelineCache`**,
            // which is arguably the more correct fix since it would let every
            // future Valve-authored mesh (`.mdl` next) load in file order. It
            // is not done here because `src/materials/` has no Valve-authored
            // geometry yet — every vertex it draws is hand-wound in
            // `preview.rs` for the current convention — so flipping it fails 17
            // of the stage-4 GPU tests and would have to re-wind the preview
            // cube, the ground quad and every test quad with it. That is a
            // material-system decision, not a map-loading one.
            //
            // **A face with primitives is fanned anyway, which is an
            // approximation.** `BuildIndicesForWorldSurface`
            // (`engine/gl_rsurf.h:170`) reads an explicit index list out of
            // `LUMP_PRIMINDICES` for those, and Valve's own assert there says
            // it always holds `(vertCount - 2) * 3` indices — the same count a
            // fan produces. So the triangle *count* is right and only the
            // *arrangement* differs, which is visible solely on the non-convex
            // surfaces the primitive list exists for (water, mainly).
            // `WorldStats::faces_with_primitives` counts them so that a map
            // where this matters is visible rather than merely wrong.
            //
            // The vertices are emitted in file order and the fan preserves it,
            // which is what keeps the port's culling agreeing with Valve's —
            // see `Pass`'s `front_face: Ccw, cull_mode: Back`.
            for i in 1..count as u16 - 1 {
                indices.extend_from_slice(&[base, base + i + 1, base + i]);
            }
        }

        if !vertices.is_empty() {
            stats.vertices += vertices.len();
            stats.triangles += indices.len() / 3;
            meshes.push(Mesh {
                material: material.to_owned(),
                vertices,
                indices,
            });
        }
    }

    (meshes, stats)
}

/// The player start, if the map has one.
///
/// `info_player_start` is the single-player spawn every Portal 2 map places.
/// Multiplayer spawns (`info_player_teamspawn` and the CS:GO
/// `info_player_counterterrorist`/`terrorist` pair) are deliberately not
/// consulted: this is a view placement, not a spawn system, and the game layer
/// owns that question when it arrives.
fn find_spawn(entities: &[bsp::Entity]) -> Option<Spawn> {
    let entity = entities
        .iter()
        .find(|e| e.classname() == Some("info_player_start"))?;
    let origin = entity.vector("origin")?;
    let angles = entity.vector("angles").unwrap_or(Vec3::ZERO);
    Some(Spawn {
        eye: origin + Vec3::Z * EYE_HEIGHT,
        pitch: angles.x,
        yaw: angles.y,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_bsp() -> Bsp {
        Bsp::parse("test.bsp".into(), &bsp::one_face_bsp()).expect("valid")
    }

    #[test]
    fn one_face_becomes_one_batch_of_two_triangles() {
        let (meshes, stats) = build_meshes(&test_bsp());
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].material, "tools/toolsblack");
        assert_eq!(meshes[0].vertices.len(), 4);
        assert_eq!(stats.faces_total, 1);
        assert_eq!(stats.faces_drawn, 1);
        assert_eq!(stats.triangles, 2);
    }

    /// `BuildIndicesForSurface`'s `FastQuad` is (0,1,2) and (0,2,3) in the
    /// file's order; each triangle is emitted reversed because Valve's content
    /// is `Cw`-front under WebGPU's framebuffer orientation. The long version
    /// is at the loop in [`build_meshes`].
    ///
    /// Getting this backwards does not draw a subtly wrong picture — it draws
    /// *nothing*, because a viewer standing inside a sealed room sees only
    /// back faces. That is what it did before this test existed.
    #[test]
    fn a_quad_triangulates_as_a_reversed_fan_from_its_first_vertex() {
        let (meshes, _) = build_meshes(&test_bsp());
        assert_eq!(meshes[0].indices, [0, 2, 1, 0, 3, 2]);
    }

    #[test]
    fn texture_coordinates_reach_the_vertices() {
        let (meshes, _) = build_meshes(&test_bsp());
        // The fixture's face is a 64-unit square at one texel per unit over a
        // 64-texel texture, so its corners are the corners of the 0..1 square.
        let uvs: Vec<[f32; 2]> = meshes[0].vertices.iter().map(|v| v.texcoord).collect();
        assert_eq!(uvs, [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    }

    /// Every one of these flags means "this is not world geometry", and each
    /// one that leaks through puts something in the map that should not be
    /// there — a sky-blue box around the level, a hint plane across a corridor.
    #[test]
    fn faces_the_compiler_marked_undrawable_are_skipped() {
        for flag in [
            bsp::surf::NODRAW,
            bsp::surf::SKY,
            bsp::surf::SKY2D,
            bsp::surf::HINT,
            bsp::surf::SKIP,
            bsp::surf::TRIGGER,
        ] {
            let mut bsp = test_bsp();
            bsp.texinfo[0].flags = flag;
            let (meshes, stats) = build_meshes(&bsp);
            assert!(meshes.is_empty(), "flag {flag:#x} was drawn");
            assert_eq!(stats.faces_not_drawn, 1, "flag {flag:#x}");
            assert_eq!(stats.faces_drawn, 0, "flag {flag:#x}");
        }
    }

    #[test]
    fn displacement_faces_are_counted_separately_from_hidden_ones() {
        // They are skipped for a different reason — the geometry is elsewhere,
        // not absent — and conflating the two would hide how much of a map is
        // missing once displacements matter.
        let mut bsp = test_bsp();
        bsp.faces[0].disp_info = 0;
        let (meshes, stats) = build_meshes(&bsp);
        assert!(meshes.is_empty());
        assert_eq!(stats.faces_displaced, 1);
        assert_eq!(stats.faces_not_drawn, 0);
    }

    #[test]
    fn a_batch_splits_before_it_runs_out_of_16_bit_indices() {
        // Repeat the one face until the batch has to split. Every copy shares a
        // material, so without the split they would be one buffer of 4 * n
        // vertices and the indices past 65,535 would wrap to zero.
        let mut bsp = test_bsp();
        let face = bsp.faces[0];
        let copies = MAX_BATCH_VERTICES / 4 + 2;
        bsp.faces = vec![face; copies];
        bsp.models[0].num_faces = copies as i32;

        let (meshes, stats) = build_meshes(&bsp);
        assert_eq!(meshes.len(), 2, "should have split exactly once");
        for mesh in &meshes {
            assert!(
                mesh.vertices.len() <= MAX_BATCH_VERTICES,
                "{} vertices in a 16-bit index buffer",
                mesh.vertices.len()
            );
            assert!(mesh
                .indices
                .iter()
                .all(|&i| (i as usize) < mesh.vertices.len()));
        }
        assert_eq!(stats.vertices, copies * 4);
        assert_eq!(stats.triangles, copies * 2);
    }

    #[test]
    fn faces_are_grouped_by_material_in_a_stable_order() {
        let mut bsp = test_bsp();
        // Two more texinfos naming two more materials, one sorting before the
        // fixture's and one after.
        bsp.texdata_string_table = vec![
            "tools/toolsblack".into(),
            "aaa/first".into(),
            "zzz/last".into(),
        ];
        let texdata = bsp.texdata[0];
        bsp.texdata.push(bsp::TexData {
            name_string_table_id: 1,
            ..texdata
        });
        bsp.texdata.push(bsp::TexData {
            name_string_table_id: 2,
            ..texdata
        });
        let info = bsp.texinfo[0];
        bsp.texinfo.push(bsp::TexInfo {
            tex_data: 1,
            ..info
        });
        bsp.texinfo.push(bsp::TexInfo {
            tex_data: 2,
            ..info
        });

        let face = bsp.faces[0];
        bsp.faces = vec![
            bsp::Face {
                tex_info: 2,
                ..face
            },
            bsp::Face {
                tex_info: 1,
                ..face
            },
            face,
        ];
        bsp.models[0].num_faces = 3;

        let (meshes, stats) = build_meshes(&bsp);
        let names: Vec<&str> = meshes.iter().map(|m| m.material.as_str()).collect();
        assert_eq!(names, ["aaa/first", "tools/toolsblack", "zzz/last"]);
        assert_eq!(stats.faces_drawn, 3);
    }

    #[test]
    fn the_spawn_point_is_the_player_start_raised_to_eye_height() {
        let entities = bsp::Bsp::parse("t.bsp".into(), &bsp::one_face_bsp())
            .expect("valid")
            .entities();
        assert!(find_spawn(&entities).is_none(), "the fixture has no start");

        let entities = super::bsp::Entity {
            pairs: vec![
                ("classname".into(), "info_player_start".into()),
                ("origin".into(), "16 32 0".into()),
                ("angles".into(), "0 90 0".into()),
            ],
        };
        let spawn = find_spawn(&[entities]).expect("a start");
        assert_eq!(spawn.eye, Vec3::new(16.0, 32.0, EYE_HEIGHT));
        assert_eq!(spawn.yaw, 90.0);
        assert_eq!(spawn.pitch, 0.0);
    }
}
