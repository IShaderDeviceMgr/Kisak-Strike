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

use crate::engine::trace::CollisionBsp;
use crate::filesystem::Vfs;
use crate::materials::context::Pass;
use crate::materials::lightmap::{Allocation, LightmapAtlas, LightmapPages, WHITE_PAGE};
use crate::materials::mesh::{IndexBuffer, SimpleVertex, VertexBuffer, VertexLayout, WorldVertex};
use crate::materials::shader::Lighting;
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
    /// The lightmap atlas page every surface in this batch was packed into.
    ///
    /// This is what makes a batch a batch: Valve's *sort ID* is exactly the
    /// pair (material, lightmap page) — `AllocateLightmap` returns one and
    /// increments it whenever either half changes (`cmatlightmaps.cpp:306`) —
    /// because the page is one texture binding and cannot vary within a draw.
    /// A material whose surfaces did not all fit on one page is several
    /// batches.
    pub lightmap_page: u32,
    vertices: VertexBuffer,
    indices: IndexBuffer,
}

/// Where the player starts when a map is loaded.
#[derive(Debug, Clone, Copy)]
pub struct Spawn {
    /// The entity's origin, in world space — **the player's feet**.
    ///
    /// Not the eye. How far above this the view sits is `VEC_VIEW`
    /// (`game/shared/gamerules.cpp:38`), which is the game client's constant
    /// and lives in [`client::player`](crate::client::player); a map knows
    /// where a player stands and nothing about how tall one is.
    pub origin: Vec3,
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
    /// Materials that resolved to the error checkerboard.
    pub materials_missing: usize,
    /// Faces that got a real lightmap block.
    pub faces_lit: usize,
    /// Faces that asked for a lightmap and could not have one — no samples in
    /// the lump, `SURF_NOLIGHT`, or a block too big for a page. They bind the
    /// white page and draw fullbright.
    pub faces_fullbright: usize,
    /// Atlas pages, including the 1x1 white one.
    pub lightmap_pages: usize,
    /// Faces carrying more than one lightstyle — switchable or animated lights.
    ///
    /// Only style 0 is baked into the atlas, so these draw with their
    /// switchable lights in whatever state `vrad` compiled them. Summing the
    /// rest needs `LightStyleValue( style )` and a per-frame page rebuild
    /// (`R_BuildLightMap`, `gl_lightmap.cpp:1623`), which is the whole dynamic
    /// lighting path.
    pub faces_with_lightstyles: usize,
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
    /// Which lighting lump the atlas was built from. Portal 2 ships HDR-only
    /// maps; a map with only LDR lighting is dimmer by the overbright factor
    /// the LDR encoding divided out, which is worth knowing before blaming the
    /// exposure. See [`Bsp::lighting_is_hdr`](bsp::Bsp::lighting_is_hdr).
    pub lighting_is_hdr: bool,
    /// The lightmap atlas, one texture per page. Held by the world because it
    /// is built from the world's `.bsp` and dies with it — `CleanupLightmaps`
    /// (`cmatlightmaps.cpp:216`) is `Drop`.
    pub lightmaps: LightmapPages,
    /// The map's collision geometry — the brushes, arranged for tracing.
    ///
    /// Built from the same [`Bsp`] the geometry came from, and held here for
    /// the same reason the lightmap atlas is: it is derived from this map's
    /// file and dies with it. `trace/` reads it; nothing in `world/` does.
    pub collision: CollisionBsp,
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

        // Materials are resolved *before* the geometry, which is a change from
        // stage 4 and is forced: how wide a lightmap block a surface reserves
        // depends on whether its material has a `$bumpmap`
        // (`RegisterLightmappedSurface`, `gl_matsysiface.cpp:216`), and its
        // vertex layout depends on which shader the material named. Neither is
        // answerable from the `.bsp`.
        let (groups, mut stats) = group_faces(&bsp);
        let error_material = materials.error_material();
        let mut resolved: BTreeMap<&str, (Arc<Material>, MaterialInfo)> = BTreeMap::new();
        for name in groups.keys() {
            let material = materials.load(vfs, name);
            stats.materials += 1;
            if Arc::ptr_eq(&material, &error_material) {
                stats.materials_missing += 1;
            }
            let info = MaterialInfo {
                layout: material.shader.vertex_layout(),
                lighting: material.lighting,
            };
            resolved.insert(name, (material, info));
        }

        let mut lightmaps = LightmapAtlas::new();
        let meshes = build_meshes(&bsp, &groups, &mut lightmaps, &mut stats, |name| {
            resolved[name].1
        });
        if meshes.is_empty() {
            return Err(WorldError::NothingToDraw {
                map: name.to_owned(),
            });
        }
        stats.lightmap_pages = lightmaps.page_count() as usize;

        let batches = meshes
            .iter()
            .map(|mesh| {
                let material = Arc::clone(&resolved[mesh.material.as_str()].0);
                let vertices = match &mesh.vertices {
                    MeshVertices::Simple(v) => VertexBuffer::new(device, &mesh.material, v),
                    MeshVertices::World(v) => VertexBuffer::new(device, &mesh.material, v),
                };
                Batch {
                    material,
                    lightmap_page: mesh.lightmap_page,
                    vertices,
                    indices: IndexBuffer::new(device, &mesh.material, &mesh.indices),
                }
            })
            .collect();

        let lightmaps = lightmaps.upload(device, materials.queue(), materials.layouts());

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
            lighting_is_hdr: bsp.lighting_is_hdr,
            lightmaps,
            collision: CollisionBsp::build(&bsp),
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
            // `BindLightmapPage( pSortList->lightmapPageID )` before the batch
            // that reads it (`gl_rsurf.cpp:1150`). Cheap and unconditional:
            // batches are page-ordered within a material, so consecutive draws
            // usually name the same page, and a shader that does not read one
            // ignores it.
            pass.bind_lightmap_page(self.lightmaps.page(batch.lightmap_page));
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
             {} materials ({} missing), \
             {} lit ({} lightstyled) + {} fullbright over {} lightmap pages ({} MiB {}); \
             collision: {}",
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
            s.faces_lit,
            s.faces_with_lightstyles,
            s.faces_fullbright,
            self.lightmaps.len(),
            self.lightmaps.bytes() / (1024 * 1024),
            if self.lighting_is_hdr { "hdr" } else { "ldr" },
            self.collision.summary(),
        )
    }
}

/// What the geometry builder needs to know about a material.
///
/// Both answers come from the material and neither from the `.bsp`, which is
/// why materials are resolved first: the shader decides the vertex layout, and
/// `$bumpmap` decides how wide a lightmap block the surface reserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MaterialInfo {
    layout: VertexLayout,
    lighting: Lighting,
}

/// Geometry for one batch, before it reaches the GPU.
///
/// Split out from [`World::load`] so that face selection, coordinate
/// generation, lightmap packing and batch splitting are testable without a
/// device — the interesting logic is all here, and none of it needs a GPU to
/// be wrong.
struct Mesh {
    material: String,
    vertices: MeshVertices,
    indices: Vec<u16>,
    lightmap_page: u32,
}

/// A batch's vertices, in whichever layout its shader declared.
///
/// Two arms because a map draws two shaders: world surfaces are
/// `LightmappedGeneric` and read [`WorldVertex`], and the tool textures and
/// error materials among them are `UnlitGeneric` and read [`SimpleVertex`].
/// [`Pass::draw`](crate::materials::context::Pass::draw) panics on a mismatch
/// by design, so the builder emits what the material asked for rather than one
/// layout and a hope.
enum MeshVertices {
    Simple(Vec<SimpleVertex>),
    World(Vec<WorldVertex>),
}

impl MeshVertices {
    fn empty(layout: VertexLayout) -> MeshVertices {
        match layout {
            VertexLayout::Simple => MeshVertices::Simple(Vec::new()),
            VertexLayout::World => MeshVertices::World(Vec::new()),
        }
    }

    fn len(&self) -> usize {
        match self {
            MeshVertices::Simple(v) => v.len(),
            MeshVertices::World(v) => v.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Appends one vertex, dropping whichever attributes this layout has no
    /// room for.
    fn push(&mut self, vertex: WorldVertex) {
        match self {
            MeshVertices::Simple(v) => {
                v.push(SimpleVertex {
                    position: vertex.position,
                    texcoord: vertex.texcoord,
                    color: vertex.color,
                });
            }
            MeshVertices::World(v) => v.push(vertex),
        }
    }

    fn take(&mut self) -> MeshVertices {
        match self {
            MeshVertices::Simple(v) => MeshVertices::Simple(std::mem::take(v)),
            MeshVertices::World(v) => MeshVertices::World(std::mem::take(v)),
        }
    }
}

/// Selects the faces worth drawing and groups them by material name.
///
/// Face *selection* is separate from geometry building because it runs twice
/// over: once to learn which materials the map uses, and again once those are
/// resolved. Groups are keyed by name so the output is ordered and a `.bsp`
/// always produces the same batches; Valve sorted by the material's
/// enumeration ID, which is allocation order and therefore not reproducible.
fn group_faces(bsp: &Bsp) -> (BTreeMap<&str, Vec<&Face>>, WorldStats) {
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

    (groups, stats)
}

/// Packs every face's lightmap, then turns the faces into per-batch meshes.
///
/// One material at a time, which is what the atlas allocator expects: it closes
/// all but the most recent page whenever the material changes, so that a
/// material's surfaces cluster onto as few pages as possible
/// (`CMatLightmaps::AllocateLightmap`, `cmatlightmaps.cpp:306`).
fn build_meshes(
    bsp: &Bsp,
    groups: &BTreeMap<&str, Vec<&Face>>,
    lightmaps: &mut LightmapAtlas,
    stats: &mut WorldStats,
    info: impl Fn(&str) -> MaterialInfo,
) -> Vec<Mesh> {
    let mut meshes = Vec::new();

    for (&material, faces) in groups {
        let info = info(material);
        lightmaps.begin_material();

        // `LightmapLess` (`gl_matsysiface.cpp:262`) restricted to one material,
        // which is where this runs: lit surfaces before unlit ones, then
        // largest lightmap first. The area sort is a packing heuristic —
        // Valve's comment says greatest-area-first produced fewer material
        // splits than the minimum-height rule it replaced — and the lit-first
        // rule keeps the white-page surfaces in one run at the end, where they
        // become a single extra batch instead of interleaving.
        let mut faces: Vec<&Face> = faces.clone();
        faces.sort_by_key(|face| {
            let lit = bsp.face_lightmap_samples(face).is_some() && info.lighting.needs_lightmap();
            let (width, height) = Bsp::face_lightmap_size(face);
            (!lit, std::cmp::Reverse(width * height))
        });

        // Pack first, because a face's lightmap coordinates depend on where it
        // landed, and its *batch* depends on which page that was.
        let mut placed: BTreeMap<u32, Vec<(&Face, Option<Allocation>)>> = BTreeMap::new();
        for face in faces {
            let allocation = place_lightmap(bsp, lightmaps, face, info.lighting, stats);
            let page = allocation.map_or(WHITE_PAGE, |a| a.page);
            placed.entry(page).or_default().push((face, allocation));
        }

        for (page, faces) in placed {
            build_page_meshes(bsp, lightmaps, material, info, page, &faces, stats, &mut meshes);
        }
    }

    meshes
}

/// Reserves a face's block in the atlas and writes its samples into it.
///
/// `RegisterLightmappedSurface` / `RegisterUnlightmappedSurface`
/// (`gl_matsysiface.cpp:216`, `:256`). `None` means the surface gets the white
/// page: it has no samples, its material is not lit, or its block is too big
/// for a page — the last of which the original treated as a fatal `Error()`.
fn place_lightmap(
    bsp: &Bsp,
    lightmaps: &mut LightmapAtlas,
    face: &Face,
    lighting: Lighting,
    stats: &mut WorldStats,
) -> Option<Allocation> {
    if !lighting.needs_lightmap() {
        return None;
    }
    if Bsp::face_lightstyle_count(face) > 1 {
        stats.faces_with_lightstyles += 1;
    }
    let Some(samples) = bsp.face_lightmap_samples(face) else {
        stats.faces_fullbright += 1;
        return None;
    };

    let (width, height) = Bsp::face_lightmap_size(face);
    let blocks = lighting.blocks();
    let Some(allocation) = lightmaps.allocate(width * blocks, height) else {
        eprintln!(
            "source-engine: world: a {}x{} lightmap does not fit a page; drawing fullbright",
            width * blocks,
            height
        );
        stats.faces_fullbright += 1;
        return None;
    };

    lightmaps.write(allocation, width, height, blocks, samples);
    stats.faces_lit += 1;
    Some(allocation)
}

/// Emits the meshes for one (material, page) pair — one, or more if the
/// vertices overflow a 16-bit index.
#[allow(clippy::too_many_arguments)]
fn build_page_meshes(
    bsp: &Bsp,
    lightmaps: &LightmapAtlas,
    material: &str,
    info: MaterialInfo,
    page: u32,
    faces: &[(&Face, Option<Allocation>)],
    stats: &mut WorldStats,
    meshes: &mut Vec<Mesh>,
) {
    let page_size = lightmaps.page_size(page);
    let mut vertices = MeshVertices::empty(info.layout);
    let mut indices: Vec<u16> = Vec::new();

    let mut flush = |vertices: &mut MeshVertices, indices: &mut Vec<u16>, stats: &mut WorldStats| {
        if vertices.is_empty() {
            return;
        }
        stats.vertices += vertices.len();
        stats.triangles += indices.len() / 3;
        meshes.push(Mesh {
            material: material.to_owned(),
            vertices: vertices.take(),
            indices: std::mem::take(indices),
            lightmap_page: page,
        });
    };

    for &(face, allocation) in faces {
        let count = face.num_edges as usize;

        // Split before the face that would overflow 16-bit indices, never in
        // the middle of one: a face's vertices have to be contiguous for the
        // fan below to index them.
        if vertices.len() + count > MAX_BATCH_VERTICES {
            flush(&mut vertices, &mut indices, stats);
        }

        let base = vertices.len() as u16;
        let lightmap_offset = lightmap_block_offset(face, info.lighting, page_size);
        for position in bsp.face_vertices(face) {
            let mut vertex = WorldVertex::new(
                position.to_array(),
                bsp.texture_coordinate(face, position),
            );
            vertex.lightmap_texcoord =
                lightmap_texcoord(bsp, face, position, allocation, page_size);
            vertex.lightmap_offset = lightmap_offset;
            vertices.push(vertex);
        }

        // `BuildIndicesForSurface` (`engine/gl_rsurf.h:145`): a face is a
        // convex polygon, so it triangulates as a fan from its first vertex.
        // Valve's `FastPolygon` is this loop with the bounds checks removed.
        //
        // **The fan is emitted in reverse**, and that is a real divergence
        // rather than a slip. Measured against `sp_a1_intro1`: in file order
        // every world surface is back-facing here and the map draws as an
        // empty clear colour. Why, precisely:
        //
        //   - Valve sets `D3DRS_CULLMODE = D3DCULL_CCW`
        //     (`shaderapidx9/shaderapidx8.cpp:4067`), and its own D3D->GL
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
        // content enters — the same treatment `rustdocs/MATERIALS.md` gives
        // Valve's row-major matrices, which are transposed on the way in and
        // never again.
        //
        // **The alternative is to flip `front_face` in `PipelineCache`**,
        // which is arguably the more correct fix since it would let every
        // future Valve-authored mesh (`.mdl` next) load in file order. It is
        // not done here because `src/materials/` has no Valve-authored
        // geometry yet — every vertex it draws is hand-wound in `preview.rs`
        // for the current convention — so flipping it fails the stage-4 GPU
        // tests and would have to re-wind the preview cube, the ground quad
        // and every test quad with it. That is a material-system decision, not
        // a map-loading one.
        //
        // **A face with primitives is fanned anyway, which is an
        // approximation.** `BuildIndicesForWorldSurface` (`gl_rsurf.h:170`)
        // reads an explicit index list out of `LUMP_PRIMINDICES` for those, and
        // Valve's own assert there says it always holds `(vertCount - 2) * 3`
        // indices — the same count a fan produces. So the triangle *count* is
        // right and only the *arrangement* differs, which is visible solely on
        // the non-convex surfaces the primitive list exists for (water,
        // mainly). `WorldStats::faces_with_primitives` counts them so that a
        // map where this matters is visible rather than merely wrong.
        for i in 1..count as u16 - 1 {
            indices.extend_from_slice(&[base, base + i + 1, base + i]);
        }
    }

    flush(&mut vertices, &mut indices, stats);
}

/// The lightmap coordinate for one vertex, normalized into its page.
///
/// `SurfComputeLightmapCoordinate` + `SurfSetupSurfaceContext`
/// (`engine/matsys_interface.cpp:1956`, `:2000`). Three cases, all Valve's:
///
/// - no lightmap: the middle of the 1x1 white page;
/// - a lightmap one luxel wide: the middle of that luxel, with no projection —
///   the plane projection is degenerate for a surface that thin;
/// - otherwise the luxel coordinate, scaled by the page and offset to the
///   block, then clamped into the page.
fn lightmap_texcoord(
    bsp: &Bsp,
    face: &Face,
    position: Vec3,
    allocation: Option<Allocation>,
    page_size: (u32, u32),
) -> [f32; 2] {
    let Some(allocation) = allocation else {
        return [0.5, 0.5];
    };
    let scale = (1.0 / page_size.0 as f32, 1.0 / page_size.1 as f32);
    let offset = (
        allocation.x as f32 * scale.0,
        allocation.y as f32 * scale.1,
    );

    // `else if ( MSurf_LightmapExtents( surfID )[0] == 0 )` — Valve tests the
    // s extent only, and takes the luxel centre on both axes when it is zero.
    let luxel = if face.lightmap_size[0] == 0 {
        [0.5, 0.5]
    } else {
        bsp.lightmap_coordinate(face, position)
    };

    [
        (luxel[0] * scale.0 + offset.0).clamp(0.0, 1.0),
        (luxel[1] * scale.1 + offset.1).clamp(0.0, 1.0),
    ]
}

/// `SurfaceCtx_t::m_BumpSTexCoordOffset`: the width of one lightmap block as a
/// fraction of the page, so the shader can step from the flat block to each
/// directional one by adding it.
///
/// Zero unless the material is bumped, matching `BuildMSurfaceVertexArrays`'
/// two branches (`matsys_interface.cpp:1493`) — whose `else` carries the
/// comment *"PORTAL 2 FIX - paint shader assumes it can use 3 lightmapped
/// coordinates in all cases, so set the offset to something reasonable"*, which
/// is why the attribute exists on every world vertex rather than only on
/// bumped ones.
fn lightmap_block_offset(face: &Face, lighting: Lighting, page_size: (u32, u32)) -> f32 {
    if lighting != Lighting::BumpedLightmap || page_size.0 == 0 {
        return 0.0;
    }
    Bsp::face_lightmap_size(face).0 as f32 / page_size.0 as f32
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
        origin,
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

    fn lit_bsp(bumped: bool) -> Bsp {
        Bsp::parse("lit.bsp".into(), &bsp::lit_face_bsp(bumped)).expect("valid")
    }

    /// An `UnlitGeneric`-shaped material: `Simple` vertices, no lightmap.
    const UNLIT: MaterialInfo = MaterialInfo {
        layout: VertexLayout::Simple,
        lighting: Lighting::None,
    };

    /// A `LightmappedGeneric` without a `$bumpmap`.
    const LIGHTMAPPED: MaterialInfo = MaterialInfo {
        layout: VertexLayout::World,
        lighting: Lighting::Lightmap,
    };

    /// A `LightmappedGeneric` with one.
    const BUMPED: MaterialInfo = MaterialInfo {
        layout: VertexLayout::World,
        lighting: Lighting::BumpedLightmap,
    };

    /// [`build_meshes`] with the two things a caller normally has to resolve
    /// first supplied directly: one material description for every material in
    /// the map, and a fresh atlas.
    fn meshes_of(bsp: &Bsp, info: MaterialInfo) -> (Vec<Mesh>, WorldStats, LightmapAtlas) {
        let (groups, mut stats) = group_faces(bsp);
        let mut lightmaps = LightmapAtlas::new();
        let meshes = build_meshes(bsp, &groups, &mut lightmaps, &mut stats, |_| info);
        stats.lightmap_pages = lightmaps.page_count() as usize;
        (meshes, stats, lightmaps)
    }

    /// The world vertices of the first mesh, for the tests that read them.
    fn world_vertices(mesh: &Mesh) -> &[WorldVertex] {
        match &mesh.vertices {
            MeshVertices::World(v) => v,
            MeshVertices::Simple(_) => panic!("expected the World layout"),
        }
    }

    #[test]
    fn one_face_becomes_one_batch_of_two_triangles() {
        let (meshes, stats, _) = meshes_of(&test_bsp(), UNLIT);
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
        let (meshes, _, _) = meshes_of(&test_bsp(), UNLIT);
        assert_eq!(meshes[0].indices, [0, 2, 1, 0, 3, 2]);
    }

    #[test]
    fn texture_coordinates_reach_the_vertices() {
        let (meshes, _, _) = meshes_of(&test_bsp(), LIGHTMAPPED);
        // The fixture's face is a 64-unit square at one texel per unit over a
        // 64-texel texture, so its corners are the corners of the 0..1 square.
        let uvs: Vec<[f32; 2]> = world_vertices(&meshes[0])
            .iter()
            .map(|v| v.texcoord)
            .collect();
        assert_eq!(uvs, [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    }

    /// The whole point of the stage: a lit surface's vertices carry a
    /// coordinate that lands on its own block of its own page.
    #[test]
    fn a_lit_face_gets_lightmap_coordinates_inside_its_block() {
        let bsp = lit_bsp(false);
        let (meshes, stats, lightmaps) = meshes_of(&bsp, LIGHTMAPPED);
        assert_eq!(stats.faces_lit, 1);
        assert_eq!(stats.faces_fullbright, 0);
        assert_eq!(meshes.len(), 1);
        assert_ne!(meshes[0].lightmap_page, WHITE_PAGE, "not the white page");

        let (page_width, page_height) = lightmaps.page_size(meshes[0].lightmap_page);
        let uvs: Vec<[f32; 2]> = world_vertices(&meshes[0])
            .iter()
            .map(|v| v.lightmap_texcoord)
            .collect();

        // A 2x2-luxel block at the page origin, sampled at luxel centres: the
        // 0.5 offset in `SurfComputeLightmapCoordinate` is what puts the first
        // corner half a luxel in rather than on the block boundary.
        let texel = |u: f32, v: f32| [u / page_width as f32, v / page_height as f32];
        assert_eq!(uvs[0], texel(0.5, 0.5));
        assert_eq!(uvs[2], texel(2.5, 2.5));
        for uv in uvs {
            assert!((0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1]));
        }
    }

    /// A surface whose material has no lightmap, or whose face has no samples,
    /// binds the 1x1 white page and samples its middle. Getting this wrong is
    /// a black surface, not a missing one.
    #[test]
    fn an_unlit_face_binds_the_white_page() {
        // The fixture without lighting, through a lightmapped material.
        let (meshes, stats, _) = meshes_of(&test_bsp(), LIGHTMAPPED);
        assert_eq!(stats.faces_lit, 0);
        assert_eq!(stats.faces_fullbright, 1);
        assert_eq!(meshes[0].lightmap_page, WHITE_PAGE);
        for vertex in world_vertices(&meshes[0]) {
            assert_eq!(vertex.lightmap_texcoord, [0.5, 0.5]);
        }

        // And a lit face through an unlit material: no allocation at all.
        let (meshes, stats, lightmaps) = meshes_of(&lit_bsp(false), UNLIT);
        assert_eq!(stats.faces_lit, 0);
        assert_eq!(meshes[0].lightmap_page, WHITE_PAGE);
        assert_eq!(lightmaps.page_count(), 1, "only the white page exists");
    }

    /// A bumped material reserves four blocks and the vertices carry the step
    /// between them. Without the step every bumped surface samples the same
    /// block three times and the normal map does nothing.
    #[test]
    fn a_bumped_material_reserves_four_blocks_and_says_how_wide_one_is() {
        let bsp = lit_bsp(true);
        let (meshes, stats, lightmaps) = meshes_of(&bsp, BUMPED);
        assert_eq!(stats.faces_lit, 1);

        let (page_width, _) = lightmaps.page_size(meshes[0].lightmap_page);
        let expected = 2.0 / page_width as f32; // the block is 2 luxels wide
        for vertex in world_vertices(&meshes[0]) {
            assert_eq!(vertex.lightmap_offset, expected);
        }

        // An unbumped material never steps, whatever the face holds.
        let (meshes, _, _) = meshes_of(&bsp, LIGHTMAPPED);
        for vertex in world_vertices(&meshes[0]) {
            assert_eq!(vertex.lightmap_offset, 0.0);
        }
    }

    /// A batch is a (material, page) pair, so a material whose surfaces did
    /// not all fit on one page is more than one batch — which is what Valve's
    /// sort ID encodes.
    #[test]
    fn a_material_that_overflows_a_page_becomes_several_batches() {
        // Two faces, each a full-page-wide half-height block. The second
        // cannot go above the first, because `AddBlock` reserves the last row.
        let mut bsp = lit_bsp(false);
        let face = bsp::Face {
            lightmap_size: [511, 127],
            ..bsp.faces[0]
        };
        bsp.faces = vec![face; 2];
        bsp.models[0].num_faces = 2;
        bsp.lighting = vec![
            crate::materials::lightmap::ColorRgbExp32 {
                r: 4,
                g: 4,
                b: 4,
                exponent: 0,
            };
            1 + 2 * 512 * 128
        ];

        let (meshes, stats, lightmaps) = meshes_of(&bsp, LIGHTMAPPED);
        assert_eq!(stats.faces_lit, 2);
        assert!(lightmaps.page_count() > 2, "the white page plus two more");
        assert!(
            meshes.len() > 1,
            "one material over two pages is two batches, got {}",
            meshes.len()
        );
        let pages: Vec<u32> = meshes.iter().map(|m| m.lightmap_page).collect();
        assert!(
            pages.windows(2).all(|w| w[0] < w[1]),
            "batches are emitted in page order: {pages:?}"
        );
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
            let (meshes, stats, _) = meshes_of(&bsp, UNLIT);
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
        let (meshes, stats, _) = meshes_of(&bsp, UNLIT);
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

        let (meshes, stats, _) = meshes_of(&bsp, UNLIT);
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

        let (meshes, stats, _) = meshes_of(&bsp, UNLIT);
        let names: Vec<&str> = meshes.iter().map(|m| m.material.as_str()).collect();
        assert_eq!(names, ["aaa/first", "tools/toolsblack", "zzz/last"]);
        assert_eq!(stats.faces_drawn, 3);
    }

    #[test]
    fn the_spawn_point_is_the_player_start_entity_origin() {
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
        assert_eq!(spawn.origin, Vec3::new(16.0, 32.0, 0.0), "feet, not eye");
        assert_eq!(spawn.yaw, 90.0);
        assert_eq!(spawn.pitch, 0.0);
    }
}
