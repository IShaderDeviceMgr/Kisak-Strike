//! The world: a loaded map and the geometry it draws.
//!
//! `portdocs/ENGINE.md` §7.14 sizes the original at ~16,300 lines
//! (`modelloader.cpp`, `cmodel.cpp`, `mod_vis.cpp`, …). This is the slice of it
//! that gets a map on screen: read the `.bsp` ([`bsp`]), turn its faces into
//! vertex and index buffers grouped by material, and draw them — the world
//! model, the **brush entities** placed around it, and the static props on top.
//! Everything the rest of that subsystem does — visibility, displacement
//! *rendering*, dynamic lighting, the 3D skybox — arrives with the subsystem
//! that needs it. (Displacement *collision* has landed: the lumps are read by
//! [`bsp`] and [`trace`](crate::engine::trace) makes terrain solid. What is
//! missing here is drawing it.)
//!
//! A brush entity is drawn exactly as the world is, under the placement its
//! entity gives it: `R_DrawBrushModel` (`gl_rsurf.cpp`) is the world-surface
//! draw with a matrix. **Nothing moves one** — that is `server/`'s — and the
//! game state that would hide one is not here either, which is
//! [`find_brush_models`]'s caveat.
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

#[cfg(test)]
mod bench;
pub mod bsp;
pub mod disp;
pub mod props;

use std::collections::BTreeMap;
use std::sync::Arc;

use glam::Vec3;

use crate::engine::trace::{BrushModel, CollisionBsp, Contents, Ray};
use crate::filesystem::mount::pak::PakMount;
use crate::filesystem::{PathId, Vfs};
use crate::materials::context::Pass;
use crate::materials::lightmap::{Allocation, LightmapAtlas, LightmapPages, WHITE_PAGE};
use crate::materials::mesh::{IndexBuffer, SimpleVertex, VertexBuffer, VertexLayout, WorldVertex};
use crate::materials::shader::Lighting;
use crate::materials::{Material, MaterialCache};

use bsp::{Bsp, BspError, Face};
use props::{PropModels, Props};

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
    /// Faces that are displacements — terrain, drawn as the
    /// `(2^power + 1)²` grid in `LUMP_DISPINFO`/`LUMP_DISP_VERTS` rather than
    /// as this face's winding.
    ///
    /// **A subset of [`faces_drawn`](WorldStats::faces_drawn), not a sibling
    /// of it**: a displacement is selected, materialed and lightmapped by the
    /// same rules as any other surface, so it is counted there too. The
    /// triangles it contributes are [`triangles_displaced`](WorldStats::triangles_displaced).
    pub faces_displaced: usize,
    /// How many of [`triangles`](WorldStats::triangles) came from terrain — a
    /// power-4 patch is 512 of them, so a map with a lot of terrain has a
    /// triangle count that says nothing about how big its level shell is.
    pub triangles_displaced: usize,
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
    /// Brush entities with drawable geometry, out of
    /// [`World::brush_models`]'s total.
    ///
    /// These four are kept apart from the face counters above rather than
    /// summed into them, because `faces_total`/`faces_drawn` answer "how much
    /// of the level shell is on screen" and mixing a map's doors into that
    /// makes both numbers harder to read.
    pub brush_models_drawn: usize,
    pub brush_model_faces: usize,
    pub brush_model_triangles: usize,
    pub brush_model_faces_lit: usize,
    /// Static prop instances placed by the `sprp` lump.
    pub props: usize,
    /// Distinct models those instances name — the number of models that
    /// actually have to be loaded, which is far smaller.
    pub prop_models: usize,
    /// Files in the map's embedded pak lump, mounted for its lifetime.
    pub pak_files: usize,
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
    /// The `.bsp`'s model lump: the world model and every brush model, with
    /// their bounding boxes.
    ///
    /// Kept because the *server* needs it — a `func_door` computes how far it
    /// slides from the size of its own brushes, which is in the file and not
    /// in the entity lump (`UTIL_SetModel`, `game/server/util.cpp:1426`). It
    /// is handed to [`Server::level_init`](crate::server::Server::level_init)
    /// beside [`entities`](World::entities), for the same reason and by the
    /// same caller. 32 bytes each; the largest shipped map has 258.
    pub models: Vec<bsp::Model>,
    /// The map's brush entities, resolved to something the trace can sweep
    /// against — `ENGINE_TRACE.md` stage 2.
    ///
    /// **Placements, not policy.** Every entity naming a `"*N"` model is here,
    /// including the ones a game would never collide with: triggers, and the
    /// `func_brush`es that are switched off. Whether a given one is solid is
    /// the game's answer (`SOLID_BSP` plus `FSOLID_*`, set by the entity's own
    /// spawn code) and there is no game to give it, so nothing is filtered out
    /// here — a consumer that knows better filters on [`classname`].
    ///
    /// Read, drawn ([`brush_model_geometry`](World::brush_model_geometry)),
    /// traced by the `trace` console command — and **moved**, by
    /// [`sync_brush_models`](World::sync_brush_models), which takes each
    /// placement from the entity that owns it once a frame.
    ///
    /// [`classname`]: PlacedBrushModel::classname
    pub brush_models: Vec<PlacedBrushModel>,
    /// The solid ones among them, as the player's clip chain wants them —
    /// see [`clip_models`](World::clip_models). Rebuilt by
    /// [`sync_brush_models`](World::sync_brush_models).
    clip_models: Vec<BrushModel>,
    /// The drawable ones among them, with their geometry.
    ///
    /// Shorter than [`brush_models`](World::brush_models) — most brush entities
    /// in a Portal 2 map are triggers and keep no drawable face — and each
    /// entry names the placement it belongs to.
    pub brush_model_geometry: Vec<BrushModelGeometry>,
    /// The map's static prop placements — the `sprp` game lump, resolved.
    ///
    /// Stage 2 of `portdocs/STUDIO.md` §8: read and transformed, **not drawn**.
    /// The models themselves are not loaded here either; that is stage 3's, and
    /// it is what turns [`Props::models`] into uploaded geometry.
    ///
    /// [`Props::models`]: props::Props::models
    pub props: Props,
    /// The models those placements name, uploaded once each.
    pub prop_models: PropModels,
    /// The entity lump, parsed, kept for the map's lifetime.
    ///
    /// The engine reads the `.bsp`, so the engine is what holds the lump and
    /// hands it to the game — `CServerGameDLL::LevelInit( pMapName,
    /// pMapEntities, ... )` (`gameinterface.cpp:1167`) is given it for exactly
    /// this reason. [`crate::server::Server::level_init`] is the consumer;
    /// `world/`'s own uses of it ([`spawn`](World::spawn),
    /// [`sky_name`](World::sky_name), [`brush_models`](World::brush_models))
    /// are resolved at load and do not read it again.
    pub entities: Vec<bsp::Entity>,
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
        // Cleared first, so that a map that fails to load never leaves the
        // *previous* map's embedded content mounted over the game's.
        vfs.set_map_pak(None);
        let bsp = Bsp::load(vfs, name)?;

        // Mounted before anything reads a file, because almost everything below
        // this line can want something out of it: the `materials/maps/<map>/...`
        // cubemap patches `vbsp` generated, a model the mapper embedded, and
        // the per-prop `.vhv` lighting. `AddSearchPath( ..., PATH_ADD_TO_HEAD )`
        // (`modelloader.cpp:4229`).
        //
        // A pak that will not parse costs the map its embedded content and
        // nothing else - the same call in the original is not checked at all -
        // so it is reported and stepped over rather than failing the load.
        let mut pak_files = 0;
        match PakMount::new(name, Arc::clone(&bsp.pak)) {
            Ok(pak) => {
                pak_files = pak.len();
                vfs.set_map_pak(Some((PathId::Game, Arc::new(pak))));
            }
            Err(e) => eprintln!("source-engine: world: {e}"),
        }

        // Materials are resolved *before* the geometry, which is a change from
        // stage 4 and is forced: how wide a lightmap block a surface reserves
        // depends on whether its material has a `$bumpmap`
        // (`RegisterLightmappedSurface`, `gl_matsysiface.cpp:216`), and its
        // vertex layout depends on which shader the material named. Neither is
        // answerable from the `.bsp`.
        let mut stats = WorldStats::default();
        let groups = group_faces(&bsp, bsp.world_model(), &mut stats);

        // The entity lump and the collision tree are read here rather than
        // after the geometry, because the brush models need both *before* their
        // faces can be grouped: the entity lump says which models are placed
        // and the collision tree resolves a `"*N"` into a placement. The props
        // below want the same tree for their leaf lookup.
        let entities = bsp.entities();
        let collision = CollisionBsp::build(&bsp);
        let brush_models = find_brush_models(&entities, &collision);

        // One group map per placement, and empty for the ones that draw
        // nothing — which is most of them. Counted into their own stats block
        // so that "5,512 of 5,638 faces" stays a statement about the world.
        let mut brush_stats = WorldStats::default();
        let brush_groups: Vec<BTreeMap<&str, Vec<&Face>>> = brush_models
            .iter()
            .map(|placed| {
                if placed.render_mode == RENDER_NONE {
                    return BTreeMap::new();
                }
                group_faces(&bsp, &bsp.models[placed.index], &mut brush_stats)
            })
            .collect();

        let error_material = materials.error_material();
        let mut resolved: BTreeMap<&str, (Arc<Material>, MaterialInfo)> = BTreeMap::new();
        // The union, because a door can wear a material no world surface does
        // — and can equally share one, which must not be loaded or counted
        // twice.
        let material_names: std::collections::BTreeSet<&str> = groups
            .keys()
            .copied()
            .chain(brush_groups.iter().flat_map(|g| g.keys().copied()))
            .collect();
        for name in &material_names {
            let mut material = materials.load(vfs, name);
            stats.materials += 1;
            if Arc::ptr_eq(&material, &error_material) {
                stats.materials_missing += 1;
            }
            // A brush face is not a model, and this builder can only write the
            // two layouts a `.bsp` has the data for: it has no per-vertex
            // normal, no tangent and no baked static-light stream, which is
            // what [`VertexLayout::Model`] is made of. A mapper *can* put a
            // model shader on a world surface, so this is reachable — and
            // there is no honest geometry to emit for it, only a guess. The
            // error material is the same answer the cache gives an unknown
            // shader, one step later: visibly wrong beats plausibly wrong.
            if !matches!(
                material.shader.vertex_layout(),
                VertexLayout::Simple | VertexLayout::World
            ) {
                eprintln!(
                    "source-engine: world: {name}: {} needs model geometry, \
                     which a brush face does not have",
                    material.shader.name()
                );
                material = Arc::clone(&error_material);
                stats.materials_missing += 1;
            }
            let info = MaterialInfo {
                layout: material.shader.vertex_layout(),
                lighting: material.lighting,
            };
            resolved.insert(*name, (material, info));
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

        // After the world, into the same atlas: a brush model's faces carry
        // ordinary `vrad` lightmap samples in the same lighting lump, so they
        // pack the same way and read the same pages. Doing it per model costs a
        // little packing efficiency — `begin_material` closes pages whenever the
        // material changes, and it changes more often across many small models
        // — and buys each model its own batches, which is what a per-model
        // transform needs.
        let brush_model_geometry = brush_groups
            .iter()
            .enumerate()
            .filter(|(_, groups)| !groups.is_empty())
            .map(|(placement, groups)| {
                let meshes = build_meshes(&bsp, groups, &mut lightmaps, &mut brush_stats, |name| {
                    resolved[name].1
                });
                (placement, meshes)
            })
            // Collected before any buffer is created, because `build_meshes`
            // borrows the atlas mutably and uploading does not.
            .collect::<Vec<_>>();

        stats.lightmap_pages = lightmaps.page_count() as usize;

        let batches = upload_batches(device, &meshes, &resolved);
        let brush_model_geometry: Vec<BrushModelGeometry> = brush_model_geometry
            .into_iter()
            .map(|(placement, meshes)| BrushModelGeometry {
                placement,
                batches: upload_batches(device, &meshes, &resolved),
            })
            .collect();

        stats.brush_models_drawn = brush_model_geometry.len();
        stats.brush_model_faces = brush_stats.faces_drawn;
        stats.brush_model_triangles = brush_stats.triangles;
        stats.brush_model_faces_lit = brush_stats.faces_lit;

        let lightmaps = lightmaps.upload(device, materials.queue(), materials.layouts());

        let model = bsp.world_model();

        // A malformed `sprp` lump loses the map's props, not the map: the world
        // geometry is already built and drawable by this point, and a map with
        // no furniture beats no map at all. Valve is stricter — `Host_Error`
        // — but it had no way to draw the level without the prop manager
        // having run, and this port does.
        let mut props = match Props::load(name, &bsp) {
            Ok(props) => props,
            Err(e) => {
                eprintln!("source-engine: world: {e}");
                Props::default()
            }
        };
        // The one built above, not a second one. It used to be rebuilt here
        // and the duplicate was cheap while the tree was brushes; it stopped
        // being cheap when `ENGINE_TRACE.md` stage 3 made building it also
        // build an AABB tree per displacement.
        props.light(&bsp, &collision);
        stats.pak_files = pak_files;
        stats.props = props.instances.len();
        stats.prop_models = props.models.len();
        let prop_models = PropModels::load(vfs, materials, device, &props, bsp.lighting_is_hdr);

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
            models: bsp.models.clone(),
            brush_models,
            // Empty until the first `sync_brush_models`, which `Scene::load`
            // runs before the world is handed over: nothing is in the clip
            // chain until the game has said what is solid.
            clip_models: Vec::new(),
            brush_model_geometry,
            collision,
            props,
            prop_models,
            entities,
            stats,
        })
    }

    /// Records every batch into an open pass.
    ///
    /// The model matrix is the identity: world geometry is already in world
    /// space, which is the whole difference between the world model and the
    /// brush models that are not drawn yet.
    pub fn draw(&self, pass: &mut Pass<'_>) {
        self.draw_brushes(pass);
        // Brush entities next: they are part of the level shell — a door in a
        // doorway, a panel in a wall — so they belong with the world rather
        // than with its furniture. `R_DrawBrushModel` (`gl_rsurf.cpp`) is
        // likewise a world-surface draw with a matrix, not a model draw.
        self.draw_brush_models(pass);
        // After the world, because a prop sits on top of the geometry it is
        // placed against and the depth test is cheaper when the near thing is
        // already there. `CStaticPropMgr::DrawStaticProps` runs in the same
        // opaque pass for the same reason.
        self.prop_models.draw(pass, &self.props);
    }

    /// Takes every brush entity's placement from whoever owns it — the game
    /// server — and writes it into the one transform the draw and the trace
    /// both read.
    ///
    /// `placement` is asked once per placed model, by the model's `"*N"`
    /// index; `None` leaves that one exactly where the entity lump put it,
    /// which is the right answer for the 8,225 brush entities in the game
    /// whose classname the server has no implementation for.
    ///
    /// # Once a frame, after the server's ticks and before anything reads it
    ///
    /// `Engine::frame` calls this between `Server::frame` and
    /// `update_client`, so the player is traced against the doors as they are
    /// *now* and the renderer draws the same thing later in the same frame.
    /// Call it more often and nothing breaks; call it less and a door is drawn
    /// where it was.
    ///
    /// `world/` names no server type: the caller converts, which is the same
    /// split `engine/mod.rs` already makes between `console/` and `input/`.
    pub fn sync_brush_models(&mut self, placement: impl Fn(usize) -> Option<Placement>) {
        sync_placements(&mut self.brush_models, placement);
        rebuild_clip_models(&self.brush_models, &mut self.clip_models);
    }

    /// Every brush model the player's trace should be clipped against —
    /// `ENGINE_TRACE.md` stage 4's half of the clip chain.
    ///
    /// **The game decides what is in it**, through
    /// [`PlacedBrushModel::owned`] and [`PlacedBrushModel::solid`]; see the
    /// first of those for why the default is to leave a model out. A trigger
    /// is excluded automatically, because `InitTrigger` sets
    /// `FSOLID_NOT_SOLID`.
    ///
    /// Hand it to
    /// [`Tracer::with_entities`](crate::engine::trace::Tracer::with_entities).
    /// It is a *slice* rather than an iterator because a `Tracer` holds it for
    /// its whole lifetime and a movement command traces through one a dozen
    /// times; [`sync_brush_models`](World::sync_brush_models) rebuilds it once
    /// a frame, which on the largest shipped map is a few hundred `Copy`s.
    pub fn clip_models(&self) -> &[BrushModel] {
        &self.clip_models
    }

    /// `engine->SolidMoved` (`engine/world.cpp`'s `CTouchLinks`) — every brush
    /// model the swept box `mins`-`maxs` meets on its way from `start` to
    /// `end`, by `"*N"` model index.
    ///
    /// The engine's half of the touch test, and the reason `src/server/` can
    /// run triggers without naming a collision type. Two things it is not:
    ///
    /// - **Not a bounding-box overlap.** Valve's enumerator ends in
    ///   `ClipRayToCollideable( ray, MASK_SOLID, pTrigger, &tr )` and takes
    ///   the hit only if `tr.contents & MASK_SOLID` — the swept box against
    ///   the trigger's *actual brushes*. A test chamber's triggers are L- and
    ///   U-shaped often enough that the difference is visible.
    /// - **Not filtered to triggers.** Which of these is a trigger is
    ///   `FSOLID_TRIGGER`, which is the server's live state; keeping a copy
    ///   here would be a frame stale every time something was enabled. The
    ///   server filters, and pays one brush sweep per non-trigger brush entity
    ///   per tick for the privilege — `sp_a1_intro1` has 78 brush models in
    ///   total.
    ///
    /// Appends to `out`; does not clear it.
    pub fn brush_models_touching(
        &self,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        out: &mut Vec<usize>,
    ) {
        brush_models_touching(
            &self.collision,
            &self.brush_models,
            start,
            end,
            mins,
            maxs,
            out,
        );
    }

    /// Records every brush entity's batches, each under its own transform.
    ///
    /// `R_DrawBrushModel` (`engine/gl_rsurf.cpp`): the same world-surface draw
    /// as [`draw_brushes`](World::draw_brushes), with the entity's
    /// model-to-world matrix in place of the identity. Valve pushed that matrix
    /// onto the matrix stack and popped it afterwards; here it is an argument,
    /// which is the same deletion `rustdocs/MATERIALS.md` records for the
    /// render-target and scissor stacks.
    ///
    /// **The transform is asked for once per model and not cached**, so it can
    /// never disagree with what `trace/` collides against — see
    /// [`BrushModelGeometry`].
    pub(crate) fn draw_brush_models(&self, pass: &mut Pass<'_>) {
        for geometry in &self.brush_model_geometry {
            let placed = &self.brush_models[geometry.placement];
            // `EF_NODRAW`, which a `func_brush` toggles. The `rendermode 10`
            // test happened at load, because that one cannot change; this one
            // can, on any tick.
            if !placed.visible {
                continue;
            }
            let model_to_world = placed.model.model_to_world();
            for batch in &geometry.batches {
                pass.bind_lightmap_page(self.lightmaps.page(batch.lightmap_page));
                pass.draw(
                    &batch.material,
                    &batch.vertices.slice(),
                    &batch.indices.slice(),
                    model_to_world,
                );
            }
        }
    }

    pub(crate) fn draw_brushes(&self, pass: &mut Pass<'_>) {
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
             ({} hidden{primitives}), \
             {} vertices, {} triangles ({} terrain, over {} displacements), {} batches, \
             {} materials ({} missing), \
             {} lit ({} lightstyled) + {} fullbright over {} lightmap pages ({} MiB {}); \
             {}/{} brush models drawn ({} faces, {} triangles, {} lit); \
             {} static props from {} models ({}); \
             {} files in the map pak; \
             collision: {}",
            self.name,
            self.bsp_version,
            self.bsp_revision,
            s.faces_drawn,
            s.faces_total,
            s.faces_not_drawn,
            s.vertices,
            s.triangles,
            s.triangles_displaced,
            s.faces_displaced,
            self.batches.len(),
            s.materials,
            s.materials_missing,
            s.faces_lit,
            s.faces_with_lightstyles,
            s.faces_fullbright,
            self.lightmaps.len(),
            self.lightmaps.bytes() / (1024 * 1024),
            if self.lighting_is_hdr { "hdr" } else { "ldr" },
            s.brush_models_drawn,
            self.brush_models.len(),
            s.brush_model_faces,
            s.brush_model_triangles,
            s.brush_model_faces_lit,
            s.props,
            s.prop_models,
            self.prop_models.summary(),
            s.pak_files,
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
    /// # Panics
    ///
    /// On [`VertexLayout::Model`], which [`World::load`] substitutes the error
    /// material for before any geometry is built — a brush face has no normal,
    /// tangent or static-light stream to fill one with.
    fn empty(layout: VertexLayout) -> MeshVertices {
        match layout {
            VertexLayout::Simple => MeshVertices::Simple(Vec::new()),
            VertexLayout::World => MeshVertices::World(Vec::new()),
            VertexLayout::Model | VertexLayout::StaticLight => {
                unreachable!("model geometry is not built from a .bsp face")
            }
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

/// Selects the faces worth drawing in one model and groups them by material
/// name.
///
/// Face *selection* is separate from geometry building because it runs twice
/// over: once to learn which materials the map uses, and again once those are
/// resolved. Groups are keyed by name so the output is ordered and a `.bsp`
/// always produces the same batches; Valve sorted by the material's
/// enumeration ID, which is allocation order and therefore not reproducible.
///
/// `model` is the world for the level shell and a brush entity's for a door or
/// a platform — the selection rules are identical, which is the finding that
/// makes brush-model rendering small. In particular **the `SURF_*` filter is
/// the whole of the visibility question**: every `trigger_*` class in Portal 2
/// compiles to `SURF_NODRAW`/`SURF_TRIGGER` faces and drops out here on its
/// own, with no per-classname rule. Measured over all 106 shipped maps: of
/// 11,635 brush entities only 2,697 keep a single drawable face, and the ones
/// that do are the ones you can see — including `trigger_portal_cleanser`,
/// whose fizzler field is genuinely visible.
///
/// `stats` is the caller's, because the world's face counts and the brush
/// models' are reported separately and summing them would hide both.
fn group_faces<'a>(
    bsp: &'a Bsp,
    model: &bsp::Model,
    stats: &mut WorldStats,
) -> BTreeMap<&'a str, Vec<&'a Face>> {
    let mut groups: BTreeMap<&str, Vec<&Face>> = BTreeMap::new();

    for face in bsp.model_faces(model) {
        stats.faces_total += 1;

        // A displacement's geometry is a subdivided grid in
        // `LUMP_DISPINFO`/`LUMP_DISP_VERTS` rather than this face's winding,
        // and `build_page_meshes` emits that grid instead of fanning the
        // quad — but everything *else* about the surface is ordinary, so it
        // is selected, materialed, sorted and lightmapped by the same rules.
        // That is `DispInfo_CreateMaterialGroups` (`disp_mapload.cpp:368`)
        // grouping by `(lightmapPageID, material)`, which is what a `Batch`
        // already is.
        if face.disp_info >= 0 {
            stats.faces_displaced += 1;
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

    groups
}

/// Uploads built meshes to the device as drawable batches.
///
/// The one step in the pipeline that needs a GPU, kept apart from the rest so
/// that everything above it — face selection, coordinate generation, lightmap
/// packing, batch splitting — stays testable without one.
fn upload_batches(
    device: &wgpu::Device,
    meshes: &[Mesh],
    resolved: &BTreeMap<&str, (Arc<Material>, MaterialInfo)>,
) -> Vec<Batch> {
    meshes
        .iter()
        .map(|mesh| Batch {
            material: Arc::clone(&resolved[mesh.material.as_str()].0),
            lightmap_page: mesh.lightmap_page,
            vertices: match &mesh.vertices {
                MeshVertices::Simple(v) => VertexBuffer::new(device, &mesh.material, v),
                MeshVertices::World(v) => VertexBuffer::new(device, &mesh.material, v),
            },
            indices: IndexBuffer::new(device, &mesh.material, &mesh.indices),
        })
        .collect()
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
            build_page_meshes(
                bsp,
                lightmaps,
                material,
                info,
                page,
                &faces,
                stats,
                &mut meshes,
            );
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

    let mut flush =
        |vertices: &mut MeshVertices, indices: &mut Vec<u16>, stats: &mut WorldStats| {
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
        let displaced = face.disp_info >= 0;
        let count = match displaced {
            true => disp::Displacement::vertex_count(bsp, face),
            false => face.num_edges as usize,
        };

        // Split before the surface that would overflow 16-bit indices, never in
        // the middle of one: a surface's vertices have to be contiguous for the
        // indices below to name them.
        if vertices.len() + count > MAX_BATCH_VERTICES {
            flush(&mut vertices, &mut indices, stats);
        }

        let base = vertices.len() as u16;
        let lightmap_offset = lightmap_block_offset(face, info.lighting, page_size);

        // A displacement replaces the face's winding with its own grid, and
        // brings its own texture and lightmap coordinates with it — a
        // displacement's lightmap is parameterized by the grid rather than by
        // the texinfo's lightmap axes, which is `portdocs/ENGINE_WORLD_DISP.md`
        // §3.2 and the one thing here that is not the ordinary face path.
        if displaced {
            let Some(patch) = disp::Displacement::build(bsp, face) else {
                // A displacement that will not build loses its terrain and not
                // the map. Unreachable for shipped content — every one of the
                // 1,181 is a quad naming a real entry — so it is reported.
                eprintln!(
                    "source-engine: world: a displacement on a {}-edge face did not build",
                    face.num_edges
                );
                stats.faces_drawn = stats.faces_drawn.saturating_sub(1);
                continue;
            };
            for vertex in &patch.vertices {
                let mut out = WorldVertex::new(vertex.position.to_array(), vertex.texcoord);
                out.lightmap_texcoord = lightmap_page_texcoord(vertex.luxel, allocation, page_size);
                out.lightmap_offset = lightmap_offset;
                // `builder.Color4f( 1, 1, 1, flAlpha )`
                // (`disp_mapload.cpp:330`) — the blend factor between
                // `$basetexture` and `$basetexture2`.
                out.color = [1.0, 1.0, 1.0, vertex.alpha];
                vertices.push(out);
            }
            // Already reversed, by `Displacement::build`, for the same
            // `front_face: Ccw` reason the fan below is reversed here.
            indices.extend(patch.indices.iter().map(|i| base + i));
            stats.triangles_displaced += patch.indices.len() / 3;
            continue;
        }

        for position in bsp.face_vertices(face) {
            let mut vertex =
                WorldVertex::new(position.to_array(), bsp.texture_coordinate(face, position));
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
    // `else if ( MSurf_LightmapExtents( surfID )[0] == 0 )` — Valve tests the
    // s extent only, and takes the luxel centre on both axes when it is zero.
    let luxel = if face.lightmap_size[0] == 0 {
        [0.5, 0.5]
    } else {
        bsp.lightmap_coordinate(face, position)
    };
    lightmap_page_texcoord(luxel, allocation, page_size)
}

/// `SurfSetupSurfaceContext`'s half of the above: a luxel coordinate scaled
/// into its page and offset to its block, then clamped into the page.
///
/// Split out because a displacement computes its luxel coordinate from the grid
/// rather than from a plane projection — `SurfaceCtx_t`'s `m_Scale` and
/// `m_Offset` are then applied to it identically, which is exactly what
/// `BuildDispSurfInit` (`disp_mapload.cpp:189`) does.
fn lightmap_page_texcoord(
    luxel: [f32; 2],
    allocation: Option<Allocation>,
    page_size: (u32, u32),
) -> [f32; 2] {
    let Some(allocation) = allocation else {
        return [0.5, 0.5];
    };
    let scale = (1.0 / page_size.0 as f32, 1.0 / page_size.1 as f32);
    let offset = (allocation.x as f32 * scale.0, allocation.y as f32 * scale.1);

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

/// A brush entity, resolved to something [`Tracer::trace_model`] can sweep
/// against.
///
/// [`Tracer::trace_model`]: crate::engine::trace::Tracer::trace_model
#[derive(Debug, Clone)]
pub struct PlacedBrushModel {
    /// The entity's `classname` — `func_door`, `trigger_multiple`,
    /// `func_brush`. Carried because it is the only thing here that says what
    /// the model is *for*, and every consumer's first question is whether to
    /// collide with it at all.
    pub classname: String,
    /// Which model the entity named: `"model" "*12"` is 12.
    pub index: usize,
    pub model: BrushModel,
    /// The entity's `rendermode` — `RenderMode_t` (`public/const.h:336`),
    /// 0 (`kRenderNormal`) when the key is absent.
    ///
    /// Stored as the file's number rather than interpreted, because the
    /// interpretation differs by consumer: [`RENDER_NONE`] means "do not draw"
    /// and is the one value [`World`] acts on, while the translucent modes
    /// 1-5 and 7-9 need a blended pass that does not exist. Collision ignores
    /// it entirely — a `rendermode 10` brush is invisible and still solid.
    pub render_mode: i32,
    /// Whether the game says to draw this one *right now* — `EF_NODRAW`
    /// cleared.
    ///
    /// Unlike [`render_mode`](PlacedBrushModel::render_mode) this is live
    /// state, not a map key: a `func_brush` is switched on and off by
    /// `Enable`/`Disable` all through a level, and 337 of the game's 2,502
    /// start switched off. Refreshed by
    /// [`sync_brush_models`](World::sync_brush_models); `true` for a model
    /// whose entity the server has no class for.
    pub visible: bool,
    /// Whether the game says to collide with it — `FSOLID_NOT_SOLID` clear.
    ///
    /// The same live state as [`visible`](PlacedBrushModel::visible) and set
    /// by the same `func_brush` inputs. **It is not the whole solidity
    /// question**: a `trigger_multiple`'s brushes are `CONTENTS_SOLID` in the
    /// file and non-solid in the game because of `FSOLID_TRIGGER`, which is
    /// `ENGINE_TRACE.md` stage 4's along with the entity clip chain that would
    /// read it. So `true` here means "nothing has said otherwise", not "solid".
    pub solid: bool,
    /// Whether the game server has a class for the entity that owns this
    /// model, and is therefore answering for it.
    ///
    /// # This is what keeps the clip chain honest
    ///
    /// `ENGINE_TRACE.md` stage 4 puts brush entities in the player's trace,
    /// and the map's 11,635 of them are **not** all solid: a
    /// `trigger_portal_cleanser` is a fizzler you walk through, and
    /// `func_portal_bumper` — 2,383 of them, the ninth commonest classname in
    /// the game — exists only to stop a portal landing on a wall. Whether one
    /// is solid is `FSOLID_NOT_SOLID`, which is *game* state, and this port
    /// has classes for 6,302 of the 11,635.
    ///
    /// So [`clip_models`](World::clip_models) yields only the ones
    /// where this is `true` **and** [`solid`](PlacedBrushModel::solid) is:
    /// "collide with what the game has told us about" rather than "assume
    /// everything is a wall". The alternative fills every Portal 2 chamber
    /// with invisible walls, and would do it silently.
    ///
    /// `false` until [`sync_brush_models`](World::sync_brush_models) hears
    /// otherwise.
    pub owned: bool,
}

/// Where a brush entity is, and whether it counts — the answer
/// [`World::sync_brush_models`] asks for.
///
/// Deliberately a plain value with no server types in it: `world/` does not
/// name `src/server/` and `src/server/` does not name `world/`, and the two
/// are joined in `engine/mod.rs`, which already knows both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// The entity's `m_vecAbsOrigin`.
    pub origin: Vec3,
    /// Its `m_angAbsRotation`, pitch/yaw/roll.
    pub angles: Vec3,
    /// `!IsEffectActive( EF_NODRAW )`.
    pub visible: bool,
    /// `!IsSolidFlagSet( FSOLID_NOT_SOLID )`.
    pub solid: bool,
}

/// `kRenderNone` (`public/const.h:348`) — the one render mode that is a flat
/// refusal to draw.
///
/// `C_BaseEntity::ShouldDraw` (`c_baseentity.cpp:1884`) tests exactly this and
/// nothing else among the modes, which is why it is the only one honoured
/// here. 94 brush entities across the 106 shipped maps set it.
pub const RENDER_NONE: i32 = 10;

/// One brush entity's drawable geometry.
///
/// Separate from [`PlacedBrushModel`] rather than a field on it, for two
/// reasons: not every placement has any (a trigger keeps no drawable face, and
/// nor does a `rendermode 10` brush), and a placement is a plain value that
/// `trace/`'s tests build without a GPU while this owns device buffers.
///
/// **No transform is stored.** It is
/// [`BrushModel::model_to_world`](crate::engine::trace::BrushModel::model_to_world),
/// recomputed per draw, so that what is drawn and what is collided with cannot
/// drift apart. It is two `Mat4` products for each of a map's few dozen brush
/// models, against a frame that already records a thousand draws.
pub struct BrushModelGeometry {
    /// Which entry of [`World::brush_models`] this draws.
    pub placement: usize,
    /// Its batches, grouped by (material, lightmap page) exactly as the
    /// world's are — a brush model is world geometry that happens to move.
    pub batches: Vec<Batch>,
}

/// Every entity that names a brush model, placed.
///
/// A brush entity carries its geometry as `"*N"` — an index into the `.bsp`'s
/// model lump — plus the `"origin"` and `"angles"` that say where that
/// geometry goes. Both default to zero, and for most brush entities both
/// *are* zero: `vbsp` leaves the geometry where the mapper drew it and only
/// rebases a model onto an origin brush, which is what a rotating door needs
/// and a wall does not.
///
/// An entity naming a model the map does not have is skipped rather than
/// refused — the same rule the entity parser itself follows, and one bad
/// entity should not cost the map.
///
/// # What is deliberately not read
///
/// Three entity keys change whether a brush entity is drawn in the shipped game
/// and are ignored here, because acting on them means running the game logic
/// that owns them. Each is recorded with how much it actually costs, measured
/// over the 106 shipped maps:
///
/// - **`StartDisabled`** — a `func_brush` that starts switched off is invisible
///   *and* non-solid until something enables it. That is `CFuncBrush`'s spawn
///   code and so `server/`'s, the same argument [`PlacedBrushModel`] makes for
///   solidity. **86 of the 2,608 drawable brush entities** set it, so it is a
///   footnote rather than a visible problem.
/// - **The translucent render modes** (1-5, 7-9). Honouring them needs a sorted
///   blended pass, which does not exist; they draw opaque. Only
///   [`RENDER_NONE`] is acted on, which is also the only one
///   `C_BaseEntity::ShouldDraw` rejects. Five entities in the whole game set a
///   translucent mode.
/// - **`renderamt`**, for the same reason — there is nothing to fade into.
pub(crate) fn find_brush_models(
    entities: &[bsp::Entity],
    collision: &CollisionBsp,
) -> Vec<PlacedBrushModel> {
    entities
        .iter()
        .filter_map(|entity| {
            let index: usize = entity.get("model")?.strip_prefix('*')?.parse().ok()?;
            // Model 0 is the world, which `worldspawn` names and which
            // `Tracer::trace` already covers; carrying it here would have
            // every consumer trace the whole map twice.
            if index == 0 {
                return None;
            }
            Some(PlacedBrushModel {
                classname: entity.classname().unwrap_or_default().to_owned(),
                index,
                model: collision.brush_model(
                    index,
                    entity.vector("origin").unwrap_or(Vec3::ZERO),
                    entity.vector("angles").unwrap_or(Vec3::ZERO),
                )?,
                // Absent, or unparseable, is `kRenderNormal` — the same
                // default the entity system's keyvalue would have left.
                render_mode: entity
                    .get("rendermode")
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0),
                // Until the server says otherwise, which it does once a frame
                // for the classnames it implements.
                visible: true,
                solid: true,
                owned: false,
            })
        })
        .collect()
}

/// [`World::sync_brush_models`]'s loop, over the slice rather than the world.
///
/// Separate so that a test can run the real thing: a [`World`] cannot be built
/// without a GPU and this has nothing to do with one.
/// Refills the clip chain from the placements — see
/// [`PlacedBrushModel::owned`] for the rule and why it is a refusal by
/// default.
fn rebuild_clip_models(models: &[PlacedBrushModel], out: &mut Vec<BrushModel>) {
    out.clear();
    out.extend(
        models
            .iter()
            .filter(|placed| placed.owned && placed.solid)
            .map(|placed| placed.model),
    );
}

/// [`World::brush_models_touching`]'s body, over the two things it needs.
///
/// Free rather than a method so that the depot test — which has no GPU and so
/// cannot build a [`World`] — sweeps the same code the running game does
/// rather than a copy of it.
pub(crate) fn brush_models_touching(
    collision: &CollisionBsp,
    models: &[PlacedBrushModel],
    start: Vec3,
    end: Vec3,
    mins: Vec3,
    maxs: Vec3,
    out: &mut Vec<usize>,
) {
    let ray = Ray::hull(start, end, mins, maxs);
    let mut tracer = collision.tracer();
    for placed in models {
        if !placed.owned {
            continue;
        }
        let trace = tracer.trace_model(&ray, &placed.model, Contents::MASK_SOLID);
        // `if ( !(tr.contents & MASK_SOLID) ) return ITERATION_CONTINUE;` —
        // and `contents` is left at `CONTENTS_EMPTY` by a clean miss, so this
        // one test covers both "missed" and "hit something the mask does not
        // want".
        if trace.contents.intersects(Contents::MASK_SOLID) {
            out.push(placed.index);
        }
    }
}

fn sync_placements(
    models: &mut [PlacedBrushModel],
    placement: impl Fn(usize) -> Option<Placement>,
) {
    for placed in models {
        let Some(p) = placement(placed.index) else {
            continue;
        };
        placed.model.set_placement(p.origin, p.angles);
        placed.visible = p.visible;
        placed.solid = p.solid;
        placed.owned = true;
    }
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
    use bsp::DispInfo;

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
        let mut stats = WorldStats::default();
        let groups = group_faces(bsp, bsp.world_model(), &mut stats);
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
    fn a_displacement_that_will_not_build_loses_its_terrain_and_not_the_map() {
        // `disp_info` naming an entry the file does not have. Unreachable for
        // shipped content, but it is untrusted input, and the answer is one
        // missing patch rather than a missing map — so it is counted as
        // displaced and *not* as drawn.
        let mut bsp = test_bsp();
        bsp.faces[0].disp_info = 0;
        assert!(bsp.disp_info.is_empty());

        let (meshes, stats, _) = meshes_of(&bsp, UNLIT);
        assert!(meshes.is_empty());
        assert_eq!(stats.faces_displaced, 1);
        assert_eq!(stats.faces_drawn, 0);
        assert_eq!(stats.faces_not_drawn, 0);
    }

    /// A flat 64x64 power-2 patch as its own `.bsp`, drawable by
    /// [`meshes_of`]. `alpha(i, j)` fills `CDispVert::alpha`, 0..255.
    fn displaced_bsp(alpha: impl Fn(usize, usize) -> f32) -> Bsp {
        use crate::engine::trace::fixture::Fixture;
        use crate::engine::trace::Contents;

        let mut fixture = Fixture::default();
        let corners = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 64.0, 0.0),
            Vec3::new(64.0, 64.0, 0.0),
            Vec3::new(64.0, 0.0, 0.0),
        ];
        fixture.add_displacement(corners, corners[0], 2, Contents::SOLID, 0, |_, _| 0.0);
        let spacing = 5;
        for i in 0..spacing {
            for j in 0..spacing {
                fixture.disp_verts[i * spacing + j].alpha = alpha(i, j);
            }
        }
        let mut bsp = fixture.bsp();
        // The fixture builds a model for the collision tree, which has no
        // faces on it; the world draw walks `model_faces`.
        bsp.models[0].num_faces = bsp.faces.len() as i32;
        bsp
    }

    /// **A displacement draws its grid, not its base quad.** The face is four
    /// vertices and two triangles; the power-2 patch it names is 25 and 32.
    ///
    /// Drawing the face anyway is the failure this replaced: a flat floor where
    /// there should be terrain, in exactly the right outline.
    #[test]
    fn a_displacement_draws_its_grid_rather_than_its_face() {
        let (meshes, stats, _) = meshes_of(&displaced_bsp(|_, _| 0.0), LIGHTMAPPED);

        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].vertices.len(), 25);
        assert_eq!(meshes[0].indices.len() / 3, 32);

        // `faces_displaced` is a subset of `faces_drawn`, not a sibling of it.
        assert_eq!(stats.faces_total, 1);
        assert_eq!(stats.faces_drawn, 1);
        assert_eq!(stats.faces_displaced, 1);
        assert_eq!(stats.triangles, 32);
        assert_eq!(stats.triangles_displaced, 32);
    }

    /// The per-vertex blend alpha reaches the vertex colour, which is what
    /// `WorldVertexTransition` lerps `$basetexture2` with. Nothing else writes
    /// a world vertex's alpha, so a patch drawn with alpha 1 everywhere is a
    /// patch wearing only its second texture.
    #[test]
    fn the_blend_alpha_reaches_the_vertex_colour() {
        let bsp = displaced_bsp(|i, j| match (i, j) {
            (0, 0) => 0.0,
            (4, 4) => 255.0,
            _ => 127.5,
        });
        let (meshes, _, _) = meshes_of(&bsp, LIGHTMAPPED);
        let vertices = world_vertices(&meshes[0]);

        assert_eq!(vertices[0].color, [1.0, 1.0, 1.0, 0.0]);
        assert_eq!(vertices[24].color, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(vertices[12].color, [1.0, 1.0, 1.0, 0.5]);
    }

    /// A displacement's lightmap coordinates land inside its own atlas block,
    /// and span it corner to corner — the grid parameterization of
    /// `BuildDispSurfInit`, mapped into the page by the *same*
    /// [`lightmap_page_texcoord`] an ordinary face uses.
    #[test]
    fn a_displacement_lands_in_its_own_lightmap_block() {
        let mut bsp = displaced_bsp(|_, _| 0.0);
        bsp.faces[0].lightmap_size = [7, 7];
        bsp.faces[0].styles = [0, 255, 255, 255];
        bsp.faces[0].light_ofs = 0;
        bsp.lighting = vec![
            crate::materials::lightmap::ColorRgbExp32 {
                r: 8,
                g: 8,
                b: 8,
                exponent: 0,
            };
            8 * 8
        ];

        let (meshes, stats, lightmaps) = meshes_of(&bsp, LIGHTMAPPED);
        assert_eq!(stats.faces_lit, 1, "the patch got a real block");

        let page = lightmaps.page_size(meshes[0].lightmap_page);
        let vertices = world_vertices(&meshes[0]);
        let s: Vec<f32> = vertices.iter().map(|v| v.lightmap_texcoord[0]).collect();
        let t: Vec<f32> = vertices.iter().map(|v| v.lightmap_texcoord[1]).collect();

        let lo = |v: &[f32]| v.iter().copied().fold(f32::MAX, f32::min);
        let hi = |v: &[f32]| v.iter().copied().fold(f32::MIN, f32::max);

        // The block is 8x8 luxels at the page's origin, and the grid spans the
        // centres of its first and last luxel: 0.5/page .. 7.5/page.
        assert!((lo(&s) - 0.5 / page.0 as f32).abs() < 1e-6, "{}", lo(&s));
        assert!((hi(&s) - 7.5 / page.0 as f32).abs() < 1e-6, "{}", hi(&s));
        assert!((lo(&t) - 0.5 / page.1 as f32).abs() < 1e-6);
        assert!((hi(&t) - 7.5 / page.1 as f32).abs() < 1e-6);
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

    /// The one-face map with a second model added — a brush entity.
    ///
    /// `faces` is how many of the map's single face that model claims: 0 makes
    /// a brush entity with nothing to draw, 1 makes one that shares the
    /// world's face. A real map never shares a face between two models, but
    /// this fixture has only one and what is under test is *which model was
    /// asked about*.
    fn with_brush_model(faces: i32, keys: &str) -> Bsp {
        let mut bsp = test_bsp();
        let mut model = bsp.models[0];
        model.first_face = 0;
        model.num_faces = faces;
        bsp.models.push(model);
        bsp.entity_lump = format!("{{\n\"classname\" \"func_door\"\n\"model\" \"*1\"\n{keys}}}\n");
        bsp
    }

    /// The model argument is honoured, and a model claiming no faces draws
    /// nothing — which is what a trigger looks like once the `SURF_*` filter
    /// has had it.
    #[test]
    fn a_brush_models_faces_come_from_its_own_model() {
        let bsp = with_brush_model(1, "");
        let mut stats = WorldStats::default();
        let groups = group_faces(&bsp, &bsp.models[1], &mut stats);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups.values().next().map(Vec::len), Some(1));
        assert_eq!(stats.faces_drawn, 1);

        let bsp = with_brush_model(0, "");
        let mut stats = WorldStats::default();
        assert!(group_faces(&bsp, &bsp.models[1], &mut stats).is_empty());
        assert_eq!(stats.faces_drawn, 0);
        assert_eq!(stats.faces_total, 0);
    }

    /// The stage-3 seam: a placement that was baked at load can be **moved**,
    /// and the one transform the draw and the trace share moves with it.
    ///
    /// Before `server/` stage 3 this could not happen — the origin and the
    /// rotation were written once by `find_brush_models` and never again,
    /// which is exactly why nothing in a map moved
    /// (`portdocs/SERVER.md` §7.4).
    #[test]
    fn a_placement_can_be_moved_by_whoever_owns_it() {
        let bsp = with_brush_model(1, "\"origin\" \"0 0 0\"\n");
        let collision = CollisionBsp::build(&bsp);
        let mut world_brush_models = find_brush_models(&bsp.entities(), &collision);
        assert!(world_brush_models[0].visible && world_brush_models[0].solid);

        // The placement is the identity to begin with.
        let before = world_brush_models[0].model.model_to_world();
        assert!((before.transform_point3(Vec3::ZERO)).length() < 1e-4);

        // Move it, turn it, and switch it off — which is what one tick of a
        // `func_door` opening and a `func_brush` being disabled looks like.
        world_brush_models[0]
            .model
            .set_placement(Vec3::new(0.0, 0.0, 64.0), Vec3::new(0.0, 90.0, 0.0));
        world_brush_models[0].visible = false;
        world_brush_models[0].solid = false;

        let after = world_brush_models[0].model.model_to_world();
        assert!(
            (after.transform_point3(Vec3::ZERO) - Vec3::new(0.0, 0.0, 64.0)).length() < 1e-4,
            "the origin moved"
        );
        assert!(
            (after.transform_point3(Vec3::X) - Vec3::new(0.0, 1.0, 64.0)).length() < 1e-4,
            "and the rotation came with it"
        );

        // …and back to unrotated, which must drop the matrix rather than keep
        // a stale one: a door that swings shut is not rotated any more.
        world_brush_models[0]
            .model
            .set_placement(Vec3::ZERO, Vec3::ZERO);
        let back = world_brush_models[0].model.model_to_world();
        assert!((back.transform_point3(Vec3::X) - Vec3::X).length() < 1e-4);
    }

    /// `sync_placements` asks by model index and **leaves alone anything the
    /// answer does not cover** — which is most of a map, because 8,225 of the
    /// game's 11,635 brush entities name a classname the server has no class
    /// for.
    #[test]
    fn syncing_leaves_a_placement_nobody_owns_where_it_was() {
        let bsp = with_brush_model(1, "\"origin\" \"10 0 0\"\n");
        let collision = CollisionBsp::build(&bsp);
        let mut placed = find_brush_models(&bsp.entities(), &collision);

        sync_placements(&mut placed, |_| None);
        let at = |p: &PlacedBrushModel| p.model.model_to_world().transform_point3(Vec3::ZERO);
        assert!((at(&placed[0]) - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-4);
        assert!(placed[0].visible);

        // A different index is not this one.
        sync_placements(&mut placed, |index| {
            (index == 7).then_some(Placement {
                origin: Vec3::new(0.0, 0.0, 99.0),
                angles: Vec3::ZERO,
                visible: false,
                solid: false,
            })
        });
        assert!((at(&placed[0]) - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-4);

        // …and its own index is.
        sync_placements(&mut placed, |index| {
            (index == 1).then_some(Placement {
                origin: Vec3::new(0.0, 0.0, 99.0),
                angles: Vec3::ZERO,
                visible: false,
                solid: false,
            })
        });
        assert!((at(&placed[0]) - Vec3::new(0.0, 0.0, 99.0)).length() < 1e-4);
        assert!(!placed[0].visible && !placed[0].solid);
    }

    /// **The clip chain is opt-in, and that is what stops 5,333 brush entities
    /// this port has no class for becoming invisible walls** — `ENGINE_TRACE.md`
    /// stage 4's one real hazard. See [`PlacedBrushModel::owned`].
    #[test]
    fn only_a_brush_model_the_game_answers_for_is_in_the_clip_chain() {
        let bsp = with_brush_model(1, "\"origin\" \"0 0 0\"\n");
        let collision = CollisionBsp::build(&bsp);
        let mut placed = find_brush_models(&bsp.entities(), &collision);
        let mut clip = Vec::new();

        // Nobody has said anything yet — `func_portal_bumper`'s case, and the
        // case of every brush entity between now and the classes that own it.
        rebuild_clip_models(&placed, &mut clip);
        assert!(clip.is_empty(), "unowned is not solid");

        // A `func_door`: owned and solid.
        let say = |solid| {
            move |index: usize| {
                (index == 1).then_some(Placement {
                    origin: Vec3::ZERO,
                    angles: Vec3::ZERO,
                    visible: true,
                    solid,
                })
            }
        };
        sync_placements(&mut placed, say(true));
        rebuild_clip_models(&placed, &mut clip);
        assert_eq!(clip.len(), 1, "a door is a wall");

        // A trigger: owned, and `InitTrigger` said `FSOLID_NOT_SOLID`.
        sync_placements(&mut placed, say(false));
        rebuild_clip_models(&placed, &mut clip);
        assert!(clip.is_empty(), "a trigger is walked through");
    }

    /// `engine->SolidMoved` — the swept box against the model's **own
    /// brushes**, and only against the ones the game answers for.
    ///
    /// The fixture gives model 1 a real 200-unit cube of its own so that the
    /// answer is a brush test rather than a bounding-box one, which is the
    /// difference `World::brush_models_touching` exists to preserve.
    #[test]
    fn the_touch_query_reports_the_models_a_swept_box_meets() {
        use crate::engine::trace::fixture::Fixture;
        use crate::engine::trace::Contents;

        let mut fixture = Fixture::default();
        let volume = fixture.add_box(
            Vec3::splat(-100.0),
            Vec3::splat(100.0),
            Contents::SOLID,
            true,
        );
        let collision = fixture.world_and_model(&[], &[volume]);

        let mut placed = vec![PlacedBrushModel {
            classname: String::from("trigger_once"),
            index: 1,
            model: collision
                .brush_model(1, Vec3::ZERO, Vec3::ZERO)
                .expect("model 1"),
            render_mode: 0,
            visible: true,
            solid: true,
            owned: false,
        }];

        let (mins, maxs) = (Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 72.0));
        let inside = Vec3::new(0.0, 0.0, -36.0);
        let mut out = Vec::new();

        // Unowned: invisible to the query, exactly as it is to the clip chain.
        brush_models_touching(&collision, &placed, inside, inside, mins, maxs, &mut out);
        assert!(out.is_empty(), "the game has not claimed it");

        // A trigger — non-solid, and still reported: **which of these is a
        // trigger is the server's question, not this one's.**
        placed[0].owned = true;
        placed[0].solid = false;
        out.clear();
        brush_models_touching(&collision, &placed, inside, inside, mins, maxs, &mut out);
        assert_eq!(out, vec![1]);

        // Well outside, and not swept through it.
        let away = Vec3::new(0.0, 0.0, 4096.0);
        out.clear();
        brush_models_touching(&collision, &placed, away, away, mins, maxs, &mut out);
        assert!(out.is_empty());

        // …but swept *from* outside *to* inside, it is met on the way — which
        // is what stops a fast player crossing a thin trigger between two
        // ticks without ever being reported inside it.
        out.clear();
        brush_models_touching(&collision, &placed, away, inside, mins, maxs, &mut out);
        assert_eq!(out, vec![1]);
    }

    /// The placement comes from the entity, not from the model lump, and the
    /// transform it produces is the one the trace collides against.
    #[test]
    fn a_brush_entitys_placement_is_read_from_its_keys() {
        let bsp = with_brush_model(1, "\"origin\" \"100 20 4\"\n\"angles\" \"0 90 0\"\n");
        let collision = CollisionBsp::build(&bsp);
        let placed = find_brush_models(&bsp.entities(), &collision);

        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].index, 1);
        assert_eq!(placed[0].classname, "func_door");
        assert_eq!(
            placed[0].render_mode, 0,
            "absent rendermode is kRenderNormal"
        );

        let m = placed[0].model.model_to_world();
        // The model's own origin lands on the entity's.
        assert!(
            (m.transform_point3(Vec3::ZERO) - Vec3::new(100.0, 20.0, 4.0)).length() < 1e-4,
            "{m}"
        );
        // A yaw of 90 turns the model's +X onto the world's +Y.
        assert!(
            (m.transform_point3(Vec3::X * 10.0) - Vec3::new(100.0, 30.0, 4.0)).length() < 1e-4,
            "{m}"
        );
    }

    /// `Model::origin` is "for sounds and lights, not a render transform"
    /// (`bsp.rs`), so a model whose lump origin is set and whose *entity* has
    /// no `origin` key draws where its vertices already are.
    #[test]
    fn the_model_lumps_origin_is_not_a_transform() {
        let mut bsp = with_brush_model(1, "");
        bsp.models[1].origin = [500.0, 600.0, 700.0];
        let collision = CollisionBsp::build(&bsp);
        let placed = find_brush_models(&bsp.entities(), &collision);

        assert_eq!(
            placed[0].model.model_to_world(),
            glam::Mat4::IDENTITY,
            "the lump origin must not reach the transform"
        );
    }

    /// `rendermode 10` is `kRenderNone`, and `C_BaseEntity::ShouldDraw`'s only
    /// render-mode refusal. It must not reach the geometry — and must not
    /// affect collision, which has its own reasons to care about a brush.
    #[test]
    fn rendermode_none_is_read_and_leaves_collision_alone() {
        let bsp = with_brush_model(1, "\"rendermode\" \"10\"\n");
        let collision = CollisionBsp::build(&bsp);
        let placed = find_brush_models(&bsp.entities(), &collision);

        assert_eq!(placed.len(), 1, "still placed, and still solid");
        assert_eq!(placed[0].render_mode, RENDER_NONE);

        // The translucent modes are *not* honoured — they need a blended pass
        // — so they must read as ordinary and be drawn rather than silently
        // dropped.
        for mode in ["0", "1", "5", "9"] {
            let bsp = with_brush_model(1, &format!("\"rendermode\" \"{mode}\"\n"));
            let collision = CollisionBsp::build(&bsp);
            let placed = find_brush_models(&bsp.entities(), &collision);
            assert_ne!(placed[0].render_mode, RENDER_NONE, "mode {mode}");
        }
    }

    /// Every shipped map's brush entities, built for real.
    ///
    /// Ignored by default and gated on `KISAK_GAME_DIR`, like the `studio/` and
    /// `trace/` depot tests. Needs no GPU — everything up to
    /// [`upload_batches`] runs on the CPU, which is the reason it is split out.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_map_brush_geometry -- --ignored --nocapture
    /// ```
    ///
    /// The assertion that earns the runtime: **a drawn brush model's vertices,
    /// carried through `model_to_world`, must land inside its own model box
    /// carried through the same transform**. `trace/`'s depot test already
    /// established that box is where the map's *collision* puts the entity, so
    /// this is the statement that what gets drawn and what gets walked into are
    /// the same object. Drop the transform, apply it twice, or reach for
    /// `Model::origin` instead of the entity's, and a displaced entity fails.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_map_builds_its_brush_model_geometry() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let mut names: Vec<String> = vfs
            .list("maps")
            .expect("maps/")
            .into_iter()
            .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
            .map(|e| e.name.trim_end_matches(".bsp").to_owned())
            .collect();
        names.sort();
        assert!(names.len() > 50, "only {} maps found", names.len());

        let (mut maps, mut placed, mut drawn) = (0, 0usize, 0usize);
        let (mut faces, mut triangles, mut lit, mut none) = (0usize, 0usize, 0usize, 0usize);
        for name in &names {
            let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
            let collision = CollisionBsp::build(&bsp);
            let brush_models = find_brush_models(&bsp.entities(), &collision);
            maps += 1;
            placed += brush_models.len();

            for model in &brush_models {
                if model.render_mode == RENDER_NONE {
                    none += 1;
                    continue;
                }
                let lump = &bsp.models[model.index];
                let mut stats = WorldStats::default();
                let groups = group_faces(&bsp, lump, &mut stats);
                if groups.is_empty() {
                    continue;
                }
                drawn += 1;

                let mut lightmaps = LightmapAtlas::new();
                let meshes =
                    build_meshes(&bsp, &groups, &mut lightmaps, &mut stats, |_| LIGHTMAPPED);
                assert!(!meshes.is_empty(), "{name}: *{} built nothing", model.index);
                faces += stats.faces_drawn;
                triangles += stats.triangles;
                lit += stats.faces_lit;

                // The model's own box, and its vertices, both carried out to
                // world space by the placement. One unit of slack for `f32`
                // across a 16k map.
                let to_world = model.model.model_to_world();
                let (lo, hi) = (Vec3::from(lump.mins), Vec3::from(lump.maxs));
                let (mut bmin, mut bmax) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                for i in 0..8 {
                    let corner = to_world.transform_point3(Vec3::new(
                        if i & 1 == 0 { lo.x } else { hi.x },
                        if i & 2 == 0 { lo.y } else { hi.y },
                        if i & 4 == 0 { lo.z } else { hi.z },
                    ));
                    bmin = bmin.min(corner);
                    bmax = bmax.max(corner);
                }
                // ...and the map's own box, which is the *independent* half of
                // the check: the box above is derived with the same transform,
                // so it proves the geometry and the box share a frame but not
                // that the frame is the right one. The world model's bounds come
                // from a different lump entry entirely. Measured over the whole
                // game: all 92,870 brush-model vertices land inside it once
                // transformed, with a worst-case excursion of 0.00 units —
                // while 13,122 of them fall outside if the transform is skipped.
                let world_model = bsp.world_model();
                let (wlo, whi) = (Vec3::from(world_model.mins), Vec3::from(world_model.maxs));

                let slack = Vec3::ONE;
                for mesh in &meshes {
                    for vertex in world_vertices(mesh) {
                        let world = to_world.transform_point3(Vec3::from(vertex.position));
                        assert!(
                            world.cmpge(bmin - slack).all() && world.cmple(bmax + slack).all(),
                            "{name}: *{} \"{}\" vertex {world} outside its own box \
                             {bmin}..{bmax}",
                            model.index,
                            model.classname,
                        );
                        assert!(
                            world.cmpge(wlo - slack).all() && world.cmple(whi + slack).all(),
                            "{name}: *{} \"{}\" vertex {world} outside the map \
                             {wlo}..{whi} — is the placement being applied?",
                            model.index,
                            model.classname,
                        );
                    }
                }
            }
        }

        println!(
            "\n{maps} maps: {placed} brush entities placed, {drawn} drawn \
             ({none} refused for rendermode 10), {faces} faces, {triangles} triangles, \
             {lit} lit"
        );
        assert!(drawn > 1000, "only {drawn} drawn across {maps} maps");
    }

    /// Every shipped map's terrain, built for real —
    /// `portdocs/ENGINE_WORLD_DISP.md` §7.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_map_displacement_geometry -- --ignored --nocapture
    /// ```
    ///
    /// **The assertion that earns the runtime is the winding anchor.** Every
    /// analytical argument about which way a displacement triangle faces runs
    /// through two independent conventions — Valve's `(v2-v0) × (v1-v0)`
    /// collision normal and this port's reversal of a world fan — and getting
    /// either backwards produces terrain that is invisible from above and
    /// solid anyway. So it is not argued here: the base face those same
    /// vertices were carved from is a world surface, its rendered winding is
    /// already pinned by `a_quad_triangulates_as_a_reversed_fan_from_its_first_vertex`,
    /// and a displacement is a perturbation of it. The two must agree in sign.
    ///
    /// The other two assertions cover what `sp_a1_intro1` cannot: it has none
    /// of the **100 displacements with a disallowed vertex**, which is the
    /// whole of `tessellate`'s reason to exist.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_map_builds_its_displacement_geometry() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let mut names: Vec<String> = vfs
            .list("maps")
            .expect("maps/")
            .into_iter()
            .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
            .map(|e| e.name.trim_end_matches(".bsp").to_owned())
            .collect();
        names.sort();
        assert!(names.len() > 50, "only {} maps found", names.len());

        let (mut maps, mut total, mut built, mut restricted) = (0, 0usize, 0usize, 0usize);
        let (mut triangles, mut dropped, mut checked) = (0usize, 0usize, 0usize);
        let mut overhanging = 0usize;
        let mut powers = BTreeMap::<i32, usize>::new();

        for name in &names {
            let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
            if bsp.disp_info.is_empty() {
                continue;
            }
            maps += 1;

            for face in bsp.model_faces(bsp.world_model()) {
                let Ok(index) = usize::try_from(face.disp_info) else {
                    continue;
                };
                let info = &bsp.disp_info[index];
                total += 1;
                *powers.entry(info.power).or_default() += 1;

                let patch = disp::Displacement::build(&bsp, face)
                    .unwrap_or_else(|| panic!("{name}: displacement {index} did not build"));
                built += 1;
                triangles += patch.indices.len() / 3;

                let grid = DispInfo::vert_count(info.power);
                assert_eq!(patch.vertices.len(), grid, "{name}: disp {index}");

                // **The disallowed set is honoured.** 100 of the game's 1,181
                // have one, and a patch that named one would be drawing a
                // vertex its lower-power neighbour does not have — which is
                // the crack `vbsp` cleared the bit to prevent.
                let allowed = |v: usize| info.allowed_verts[v / 32] & (1u32 << (v % 32)) != 0;
                let disallowed: Vec<usize> = (0..grid).filter(|&v| !allowed(v)).collect();
                if !disallowed.is_empty() {
                    restricted += 1;
                    dropped += disallowed.len();
                    for &v in &disallowed {
                        assert!(
                            !patch.indices.contains(&(v as u16)),
                            "{name}: disp {index} draws disallowed vertex {v}"
                        );
                    }
                    assert!(
                        patch.indices.len() < DispInfo::tri_count(info.power) * 3,
                        "{name}: disp {index} dropped a vertex and kept every triangle"
                    );
                }

                assert!(!patch.indices.is_empty(), "{name}: disp {index} is empty");

                // **The winding anchor**, and the reason this test is worth its
                // runtime. The reference is the base face's own *rendered*
                // winding: `build_page_meshes` emits `(0, i+1, i)`, the
                // reversed fan, so `(c2-c0) x (c1-c0)` is which way that
                // surface would have faced. A displacement is a perturbation of
                // that quad, so its geometry has to face the same way.
                //
                // Taken over the **patch**, area-weighted, rather than triangle
                // by triangle -- because terrain genuinely overhangs. Measured
                // over the game: 131 of 92,622 individual triangles face the
                // other way, all of them on steep patches, while **all 1,181
                // patches agree** -- and all 1,181 disagree if the reversal in
                // `Displacement::build` is dropped. So the per-patch form is
                // exact where the per-triangle form is a heuristic.
                let corners: Vec<Vec3> = bsp.face_vertices(face).collect();
                let face_normal = (corners[2] - corners[0]).cross(corners[1] - corners[0]);

                let mut area = Vec3::ZERO;
                for tri in patch.indices.chunks_exact(3) {
                    let v = |k: usize| patch.vertices[tri[k] as usize].position;
                    let normal = (v(1) - v(0)).cross(v(2) - v(0));
                    area += normal;
                    if normal.length_squared() > 1e-6 {
                        checked += 1;
                        if normal.dot(face_normal) <= 0.0 {
                            overhanging += 1;
                        }
                    }
                }
                assert!(
                    area.dot(face_normal) > 0.0,
                    "{name}: disp {index} faces {area} against its base face's {face_normal} -- the winding is inverted, and the terrain will be invisible from the side you stand on"
                );
            }
        }

        println!(
            "\n{maps} maps: {total} displacements, {built} built, {triangles} triangles; \
             {restricted} with disallowed vertices ({dropped} vertices dropped); \
             {checked} triangle windings checked ({overhanging} overhanging); powers {powers:?}"
        );
        assert_eq!(built, total);
        assert!(
            total > 1000,
            "only {total} displacements across {maps} maps"
        );
        assert!(
            restricted > 50,
            "only {restricted} restricted — is the mask read?"
        );
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
