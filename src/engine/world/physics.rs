//! Filling the physics environment from a loaded map.
//!
//! `PhysCreateWorld_Shared` (`legacy/game/shared/physics_shared.cpp`),
//! `PhysCreateVirtualTerrain` and
//! `CStaticPropMgr::CreateVPhysicsRepresentations`
//! (`legacy/engine/staticpropmgr.cpp:1801`) — the three things that put
//! *static* geometry into `physenv` before a single entity has spawned.
//!
//! This is `world/`'s side of the seam `portdocs/VPHYSICS.md` §5 describes:
//! the engine owns the collision data and the server owns the environment, so
//! everything here happens once at level init and the result is handed over.
//! Nothing in [`crate::vphysics`] knows what a `.bsp` is, and nothing in
//! `server/` knows what a `.phy` is.
//!
//! # What goes in, in Valve's order
//!
//! 1. **The world** — `LUMP_PHYSCOLLIDE`'s brush model 0, every solid it has.
//! 2. **Displacements** — which are *not* in that lump; see the
//!    `displacements` function below.
//! 3. **Static props** whose `m_Solid` is `SOLID_VPHYSICS`.
//!
//! Brush entities are the fourth and are not here, because their placement
//! comes from the entity list rather than from the lump — the server adds them
//! in `Server::set_physics`.

use std::collections::HashMap;

use glam::{Mat3, Vec3};

use crate::engine::world::bsp::Bsp;
use crate::engine::world::props::Props;
use crate::filesystem::Vfs;
use crate::math::matrix_angles;
use crate::vphysics::collide::{CollideError, VCollide};
use crate::vphysics::env::{Environment, Hulls, Motion};
use crate::vphysics::surfaceprops::SurfaceProps;
use crate::vphysics::Model;

/// `SOLID_VPHYSICS` (`public/const.h`) — the only static-prop solidity that
/// reads a `.phy`.
const SOLID_VPHYSICS: u8 = 6;

/// `SURF_NOPHYSICS_COLL` — a displacement with no vphysics mesh. See
/// [`displacements`].
const NOPHYSICS_COLL: u32 = 0x2;

/// `MASK_SOLID` (`public/bspflags.h:106`) — everything normally solid, which is
/// what `CBaseEntity::PhysicsSolidMaskForEntity` returns and therefore what
/// decides whether a world solid stops a physics prop. See [`add_world`].
///
/// Taken from [`Contents::MASK_SOLID`](crate::engine::trace::Contents) rather
/// than written out, so that the mask a cube falls through and the mask a
/// trace passes through cannot drift apart.
const MASK_SOLID: u32 = crate::engine::trace::Contents::MASK_SOLID.0;

/// `SURFACEPROP_MANIFEST_FILE` (`physics_shared.cpp:41`).
const MANIFEST: &str = "scripts/surfaceproperties_manifest.txt";

/// What a map contributes to the environment, plus what the server still needs
/// in order to place the rest.
pub struct WorldPhysics {
    /// The environment with the world, the terrain and the static props
    /// already in it.
    pub environment: Environment,
    /// The map's own brush models, by `"*N"` index, **unplaced**.
    ///
    /// Model 0 — worldspawn — is not here: it went into the environment above.
    /// The rest are doors, platforms, triggers and clip brushes, and where
    /// each one *is* is a question only the entity list can answer.
    pub brush_models: Vec<(usize, Model)>,
    /// The studio models this map's entities may ask for a body for, by the
    /// name they name them with (`models/props/metal_box.mdl`).
    pub models: HashMap<String, Model>,
    pub stats: PhysicsStats,
}

/// What the build found, for the load-time line and for the depot test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicsStats {
    /// Solids in the world's own collision model — four on `sp_a1_intro1`.
    pub world_solids: usize,
    /// Convex pieces those solids are made of.
    pub world_ledges: usize,
    /// …of which mix more than one surface material, and so cannot have one.
    /// See `portdocs/VPHYSICS.md` §3.5.
    pub world_mixed_ledges: usize,
    /// World solids that get no body because their contents do not intersect
    /// `MASK_SOLID` — clip brushes and water. See [`add_world`].
    pub world_solids_skipped: usize,
    pub displacements: usize,
    pub displacements_skipped: usize,
    pub brush_models: usize,
    pub static_props: usize,
    /// Static props that are `SOLID_VPHYSICS` but whose model ships no `.phy`.
    pub static_props_without_collision: usize,
    /// Distinct studio models whose `.phy` was read.
    pub models: usize,
    pub models_without_collision: usize,
    /// Ledges that could not be turned into a convex hull.
    pub degenerate: usize,
    /// Collision models that failed to parse, with the first message.
    pub failed: usize,
    pub first_failure: Option<String>,
}

impl PhysicsStats {
    pub fn summary(&self) -> String {
        format!(
            "physics: {} world solid(s) in {} hull(s), {} displacement(s), \
             {} brush model(s), {} static prop(s), {} model(s)",
            self.world_solids,
            self.world_ledges,
            self.displacements,
            self.brush_models,
            self.static_props,
            self.models,
        )
    }
}

/// Reads the surface property database the way `physics_shared.cpp` does:
/// through the manifest, in the order it lists.
///
/// An empty database is not an error — every test that has no game directory
/// runs on one, and [`SurfaceProps::resolve`] has the fallback Valve keeps in
/// code for exactly that case.
pub fn surface_properties(vfs: &Vfs) -> SurfaceProps {
    let Ok(manifest) = vfs.read(MANIFEST) else {
        return SurfaceProps::default();
    };
    let manifest = String::from_utf8_lossy(&manifest).into_owned();
    let names = SurfaceProps::manifest(MANIFEST, &manifest);
    let texts: Vec<(String, String)> = names
        .iter()
        .filter_map(|name| {
            let bytes = vfs.read(name).ok()?;
            Some((name.clone(), String::from_utf8_lossy(&bytes).into_owned()))
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = texts
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    SurfaceProps::parse(&borrowed)
}

/// Builds the environment for one map's **static** geometry.
///
/// Everything here is the engine's own data — the collision lump, the
/// displacement grids and the static prop lump — so it happens inside
/// `World::load`. The two things that are not are added later:
/// [`WorldPhysics::add_models`] for the models the map's *entities* name, and
/// `Server::set_physics` for the brush entities' placements.
pub fn build(map: &str, bsp: &Bsp, props: &Props, vfs: &Vfs, surfaces: SurfaceProps) -> WorldPhysics {
    let models: &[String] = &[];
    let mut stats = PhysicsStats::default();
    let mut environment = Environment::new(surfaces);

    // 1. The world, and the brush models beside it.
    let lump = match VCollide::read_lump(map, &bsp.phys_collide) {
        Ok(lump) => lump,
        Err(error) => {
            stats.failed += 1;
            stats.first_failure = Some(error.to_string());
            Vec::new()
        }
    };
    let mut brush_models = Vec::new();
    let mut virtual_terrain = false;
    for (index, collide) in lump {
        if index == 0 {
            // `bCreateVirtualTerrain` (`physics_shared.cpp`) — the world's own
            // keydata says whether its displacements are in this lump or have
            // to be built at load. **All 106 shipped maps say they are not**,
            // but the flag is read rather than assumed, because a map that
            // did carry them would otherwise get its terrain twice.
            virtual_terrain = collide.virtual_terrain();
            add_world(&mut environment, &collide, &mut stats);
            continue;
        }
        // A brush model's `solid` block has no `surfaceprop`, so it gets
        // `"default"` — which is what `CreatePolyObjectStatic` passes for one
        // too.
        if let Some(model) = Model::from_vcollide(&collide) {
            stats.degenerate += model.hulls.degenerate;
            stats.brush_models += 1;
            brush_models.push((index, model));
        }
    }

    // 2. The terrain.
    let terrain = match virtual_terrain {
        true => displacements(bsp, &mut stats),
        false => Vec::new(),
    };
    for (points, indices) in terrain {
        let hulls = Hulls::from_mesh(points, indices);
        stats.degenerate += hulls.degenerate;
        if environment
            .add(Motion::Static, &hulls, Vec3::ZERO, Vec3::ZERO, "default", None)
            .is_some()
        {
            stats.displacements += 1;
        }
    }

    // 3. Everything with a `.phy`: the static props' models and the entities'.
    let mut table: HashMap<String, Model> = HashMap::new();
    let mut missing: Vec<String> = Vec::new();
    let wanted = props
        .models
        .iter()
        .chain(models.iter())
        .cloned()
        .collect::<std::collections::BTreeSet<String>>();
    for name in wanted {
        match read_model(&name, vfs) {
            Ok(Some(model)) => {
                stats.degenerate += model.hulls.degenerate;
                stats.models += 1;
                table.insert(name.to_ascii_lowercase(), model);
            }
            Ok(None) => {
                stats.models_without_collision += 1;
                missing.push(name);
            }
            Err(error) => {
                stats.failed += 1;
                stats.first_failure.get_or_insert_with(|| error.to_string());
            }
        }
    }
    let _ = missing;

    for prop in &props.instances {
        if prop.solid != SOLID_VPHYSICS {
            continue;
        }
        let Some(model) = table.get(&prop.model.to_ascii_lowercase()) else {
            stats.static_props_without_collision += 1;
            continue;
        };
        // `CStaticProp::CreateVPhysics` places the model's solid 0 at the
        // prop's own transform. The lump gives that as an origin and a
        // `QAngle`, and `Prop::transform` is exactly `AngleMatrix` with the
        // translation appended — no scale — so it decomposes back exactly.
        let origin = prop.transform.w_axis.truncate();
        let angles = matrix_angles(Mat3::from_mat4(prop.transform));
        let surface = model.params.surface_prop.clone();
        if environment
            .add(
                Motion::Static,
                &model.hulls,
                origin,
                angles,
                &surface,
                None,
            )
            .is_some()
        {
            stats.static_props += 1;
        }
    }

    WorldPhysics {
        environment,
        brush_models,
        models: table,
        stats,
    }
}

impl WorldPhysics {
    /// Reads the `.phy` of every model the map's entities place, which the
    /// caller only knows after `level_init` has spawned them.
    ///
    /// Static props' models are already here — [`build`] read them from the
    /// prop lump — and a model named by both is read once.
    pub fn add_models(&mut self, names: &[String], vfs: &Vfs) {
        for name in names {
            let key = name.to_ascii_lowercase();
            if self.models.contains_key(&key) {
                continue;
            }
            match read_model(name, vfs) {
                Ok(Some(model)) => {
                    self.stats.degenerate += model.hulls.degenerate;
                    self.stats.models += 1;
                    self.models.insert(key, model);
                }
                Ok(None) => self.stats.models_without_collision += 1,
                Err(error) => {
                    self.stats.failed += 1;
                    self.stats
                        .first_failure
                        .get_or_insert_with(|| error.to_string());
                }
            }
        }
    }
}

/// The worldspawn entry: one static body per solid, filtered by contents.
///
/// `PhysCreateWorld_Shared` creates a static object for **every** `staticsolid`
/// and then calls `SetContents`, so at first glance the contents look like
/// bookkeeping. They are not: `CCollisionEvent::ShouldCollide`
/// (`physics.cpp:487`) ends with
///
/// ```text
/// if ( !(pObj0->GetContents() & pEntity1->PhysicsSolidMaskForEntity()) || … )
///     return 0;
/// ```
///
/// and `CBaseEntity::PhysicsSolidMaskForEntity` is `MASK_SOLID`
/// (`physics_main_shared.cpp:1129`). So a world solid is in a physics prop's
/// way **iff its contents intersect `MASK_SOLID`**, and the filter is applied
/// here — once, at build — rather than per contact, because nothing in this
/// port overrides that mask. (`CBasePlayer` and `CAI_BaseNPC` do override it,
/// and neither is in the environment: the player is not a physics object here,
/// which is `rustdocs/VPHYSICS.md` §7's first entry.)
///
/// What that admits and refuses, measured over the 106 shipped maps' 384
/// `staticsolid` blocks:
///
/// | contents | count | in a cube's way? |
/// |---|---|---|
/// | `SOLID\|WINDOW\|GRATE\|…` (`0x2003003`) | 106 | **yes** — the level shell |
/// | `GRATE` (`0x8`) | 83 | **yes** — "bullets and sight pass through, but solids don't" |
/// | `PLAYERCLIP` (`0x10000`) | 103 | no |
/// | `MONSTERCLIP` (`0x20000`) | 92 | no |
/// | `WATER\|TRANSLUCENT` (`0x10000020`) | 76 | no |
///
/// The last row is also the 76 solids a `fluid { }` block names, and they would
/// be excluded twice over: `CreateFluidController` turns that object into an
/// **IVP phantom** (`physics_fluid.cpp:192`), a volume that reports what is
/// inside it and collides with nothing. A cube falls *into* goo and is then
/// pushed back out by buoyancy, which this port has not got — so here it falls
/// in and keeps going, which is the closer of the two wrong answers.
fn add_world(environment: &mut Environment, collide: &VCollide, stats: &mut PhysicsStats) {
    let contents: HashMap<usize, u32> = collide.static_solids().into_iter().collect();
    for (index, solid) in collide.solids.iter().enumerate() {
        // A solid with no `staticsolid` block is the world's own, which every
        // map declares — but defaulting to `MASK_SOLID` rather than to zero
        // means a map that declared none would still have a floor.
        let bits = contents.get(&index).copied().unwrap_or(MASK_SOLID);
        if bits & MASK_SOLID == 0 {
            stats.world_solids_skipped += 1;
            continue;
        }
        stats.world_solids += 1;
        stats.world_ledges += solid.ledges.len();
        stats.world_mixed_ledges += solid.ledges.iter().filter(|l| l.mixed_materials).count();
        let hulls = Hulls::from_solid(solid);
        stats.degenerate += hulls.degenerate;
        // The world's material is `"default"`: `PhysCreateWorld_Shared` passes
        // `physprops->GetSurfaceIndex( "default" )` and lets IVP override it
        // per *triangle* from the map's `materialtable`, which a collider
        // cannot do. `portdocs/VPHYSICS.md` §3.5 bounds the cost.
        environment.add(Motion::Static, &hulls, Vec3::ZERO, Vec3::ZERO, "default", None);
    }
}

/// Every displacement's collision mesh, as `(points, triangles)`.
///
/// `PhysCreateVirtualTerrain` (`physics_shared.cpp`) asks `modelinfo` for a
/// `CPhysCollide` per displacement, which the engine builds out of
/// `physics_virtualmesh.cpp`'s streaming mesh — 643 lines, an `IVP_SurfaceManager`
/// subclass, and a cache. **None of it is needed here**: `Bsp::disp_grid` is
/// already the grid, shared with `trace::disp` and `world::disp` precisely so
/// that the drawn surface, the traced surface and now the simulated one cannot
/// be three different surfaces.
///
/// The triangulation is `build_tris`'s (`trace/disp.rs:867`) — the diagonal
/// alternates with the cell's parity, and getting that wrong builds a patch
/// with the right silhouette and the wrong bumps.
///
/// `SURF_NOPHYSICS_COLL` is honoured, which is the first time this port has
/// read it: `trace/disp.rs` named the bit and left it for "stage 5".
fn displacements(bsp: &Bsp, stats: &mut PhysicsStats) -> Vec<(Vec<Vec3>, Vec<[u32; 3]>)> {
    if bsp.disp_info.is_empty() {
        return Vec::new();
    }
    // A displacement's base face is the one that names it, and the lump does
    // not record the reverse direction.
    let mut face_of: Vec<Option<usize>> = vec![None; bsp.disp_info.len()];
    for (index, face) in bsp.faces.iter().enumerate() {
        if let Ok(disp) = usize::try_from(face.disp_info) {
            if let Some(slot @ None) = face_of.get_mut(disp) {
                *slot = Some(index);
            }
        }
    }

    let mut out = Vec::new();
    for (index, info) in bsp.disp_info.iter().enumerate() {
        if info.disp_flags() & NOPHYSICS_COLL != 0 {
            stats.displacements_skipped += 1;
            continue;
        }
        let Some(face) = face_of[index].map(|f| &bsp.faces[f]) else {
            stats.displacements_skipped += 1;
            continue;
        };
        let Some(quad) = bsp.disp_base_quad(face, info) else {
            stats.displacements_skipped += 1;
            continue;
        };
        let verts = bsp.disp_grid(info, &quad);
        let width = (1usize << info.power) + 1;
        let mut tris = Vec::with_capacity((width - 1) * (width - 1) * 2);
        for v in 0..width - 1 {
            for u in 0..width - 1 {
                let n = (v * width + u) as u32;
                let w = width as u32;
                if n % 2 == 1 {
                    tris.push([n, n + w, n + 1]);
                    tris.push([n + 1, n + w, n + w + 1]);
                } else {
                    tris.push([n, n + w, n + w + 1]);
                    tris.push([n, n + w + 1, n + 1]);
                }
            }
        }
        out.push((verts, tris));
    }
    out
}

/// One studio model's `.phy`, or `None` if it ships none.
///
/// **Half of Portal 2's models ship none** — 1,056 of 2,041 do — and that is
/// not an error: a model with no `.phy` is `SOLID_NONE` or `SOLID_BBOX` and
/// never reaches `PhysModelCreate`.
fn read_model(name: &str, vfs: &Vfs) -> Result<Option<Model>, CollideError> {
    let path = match name.rsplit_once('.') {
        Some((stem, _)) => format!("{stem}.phy"),
        None => format!("{name}.phy"),
    };
    let Ok(bytes) = vfs.read(&path) else {
        return Ok(None);
    };
    let collide = VCollide::read_phy(&path, &bytes)?;
    Ok(Model::from_vcollide(&collide))
}
