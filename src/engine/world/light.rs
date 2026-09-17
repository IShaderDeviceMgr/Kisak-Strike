//! The light cache: how bright it is at a point, and which lights reach it.
//!
//! `engine/lightcache.cpp`'s static half — `LightcacheGetStatic` and
//! everything under it — plus the `dworldlight_t`-to-hardware-light conversion
//! that lives in `engine/l_studio.cpp`. This is what lights every model in a
//! level: a static prop at load, and an entity's model when the game places
//! one.
//!
//! Two independent terms reach [`ModelLighting`], and they come from different
//! places for different reasons:
//!
//! | Term | Source | Reference |
//! |---|---|---|
//! | the ambient cube | `vrad`'s per-leaf samples, interpolated | `Mod_LeafAmbientColorAtPos` |
//! | up to [`WORLD_LIGHTS`] local lights | `LUMP_WORLDLIGHTS`, selected per point | `AddStaticLighting` |
//!
//! # The ambient cube is the *leaf* one, and that decides a `continue`
//!
//! Valve computes a static prop's ambient term by firing 162 rays and sampling
//! the lightmap each one hits (`ComputeAmbientFromSphericalSamples`), and every
//! *other* model's by interpolating `vrad`'s baked per-leaf cubes
//! (`ComputeAmbientFromLeaf`). The two differ in exactly one way, and
//! `R_StudioGetAmbientLightForPoint` reports it as `bAddedLeafAmbientCube`:
//! **`vrad`'s bake already contains the dim `emit_surface` lights**
//! (`leaf_ambient_lighting.cpp:179`'s `AddEmitSurfaceLights`) and the runtime
//! gather does not.
//!
//! This port uses the leaf cubes for everything, so `bAddedLeafAmbientCube` is
//! *true* here and the lights flagged [`dwl::IN_AMBIENT_CUBE`] are skipped —
//! `AddStaticLighting`'s first `continue`. That is 6,731 of the game's 14,246
//! world lights, so getting it backwards is not a subtle error: it counts
//! nearly half the lights in Portal 2 twice.
//!
//! # What is not here
//!
//! **Lightstyles.** `AddStaticLighting` skips any light with a non-zero
//! `style` and `AddLightStylesForStaticProp` adds it back under
//! `LIGHTCACHEFLAGS_LIGHTSTYLE`, animated by `d_lightstylevalue[]`. Nothing
//! animates a lightstyle in this port, so [`LightCache`] carries the
//! `LIGHTCACHEFLAGS_STATIC` half alone and the styled lights are dropped at
//! load. Measured: **213 of the game's 14,246 lights** carry a style, across
//! styles 32-37, which are the switchable ones a `light` entity owns.
//!
//! **Dynamic lights** (`LIGHTCACHEFLAGS_DYNAMIC`), for the same reason —
//! `cl_dlights` does not exist.
//!
//! **The cache.** `lightcache_t`, its 200-entry LRU, the hash grid and
//! `FindNearestCache` are deleted rather than ported: they exist because Valve
//! recomputed this for moving models every frame, and everything this port
//! lights is lit once, at load, at its exact position. What survives the
//! deletion is [`light_cache_bounds`], because the grid cell is an *argument*
//! to the light selection and not just a cache key.
//!
//! **The PVS reject.** `FastRejectLightSource` asks whether the light's
//! cluster is in the sample point's PVS before doing anything else, and this
//! port has no visibility. Leaving it out is safe rather than approximate:
//! `vvis` is conservative, so a cluster it calls invisible has no sight line,
//! and the occlusion trace below rejects that light anyway. What it costs is
//! time — see [`LightCache::lighting_at`].
//!
//! [`ModelLighting`]: crate::materials::uniforms::ModelLighting

use glam::Vec3;

use crate::engine::trace::{CollisionBsp, Contents, Ray, Tracer};
use crate::engine::world::bsp::{
    dwl, emit, surf, Bsp, LeafAmbientIndex, LeafAmbientSample, WorldLight,
};
use crate::materials::uniforms::{Light, ModelLighting, AMBIENT_CUBE_FACES, MAX_LIGHTS};

/// The six ambient-cube faces, in `+x, -x, +y, -y, +z, -z` order.
pub type AmbientCube = [[f32; 3]; AMBIENT_CUBE_FACES];

/// `s_pAmbientLightDir` (`studiorendercontext.cpp:1802`), which is also
/// `vrad`'s `g_BoxDirections` — the same six axes in the same order, which is
/// why a cube `vrad` baked can be read by the runtime at all.
const BOX_DIRECTIONS: [Vec3; AMBIENT_CUBE_FACES] = [
    Vec3::new(1.0, 0.0, 0.0),
    Vec3::new(-1.0, 0.0, 0.0),
    Vec3::new(0.0, 1.0, 0.0),
    Vec3::new(0.0, -1.0, 0.0),
    Vec3::new(0.0, 0.0, 1.0),
    Vec3::new(0.0, 0.0, -1.0),
];

/// `s_Grayscale` (`lightcache.cpp:294`): the weights that turn a light's
/// radiance into the one number the four local-light slots are ranked by.
const GRAYSCALE: Vec3 = Vec3::new(0.299, 0.587, 0.114);

/// `r_worldlightmin` (`lightcache.cpp:262`), "about 1/256": below this a light
/// is too dim to be worth a slot, and too dim to be worth tracing to.
///
/// Not a cheat cvar in the original and not a cvar here, because this port
/// computes a model's lighting once at load and has no
/// `R_StudioCheckReinitLightingCache` to notice a change.
const WORLD_LIGHT_MIN: f32 = 0.0002;

/// `r_lightcache_radiusfactor` (`lightcache.cpp:92`), "allow lights to
/// influence lightcaches beyond the lights' radii".
///
/// **It multiplies a squared radius in one place and a plain one in the
/// other**, exactly as written (`lightcache.cpp:1105` against `:1117`), so its
/// effect is `sqrt(1000)` ≈ 31.6x for a point light's box test and 1000x for a
/// spotlight's sphere test. Either way it is large enough that the box tests
/// reject almost nothing and the real cull is [`WORLD_LIGHT_MIN`] — which is
/// the point: the comment beside it reads "TERROR: try harder to get
/// contributions from lights at the edges of their radii".
const LIGHT_RADIUS_FACTOR: f32 = 1000.0;

/// How many local lights one model can be given —
/// `MIN( MaxNumLights(), r_worldlights )`.
///
/// **Two, because this port is POSIX.** `r_worldlights` is `"2"` under
/// `#ifdef POSIX` and `"3"` otherwise (`lightcache.cpp:254`), over a comment
/// reading "NOTE! Changed from 4 to 3 for L4D!" and, on the POSIX branch,
/// "JasonM GL - capping at 2 world lights at the moment". So the tree offers
/// three numbers — 4 as designed, 3 as L4D left it, 2 as the GL port left it —
/// and this port compiles on the platform whose answer is 2.
///
/// It is the least certain constant in this module, and it is deliberately
/// *not* the cheapest thing to change by accident: raising it to 3 or 4 here
/// is one edit and everything downstream already carries four slots
/// ([`MAX_LIGHTS`], `ModelLighting::lights`). What changing it costs is not
/// brightness — a light that misses a slot is folded into the ambient cube by
/// [`add_to_light_cube`] instead — but directionality, so a higher number
/// gives sharper shading rather than a lighter scene.
pub const WORLD_LIGHTS: usize = 2;

/// `COORD_EXTENT * 1.74` (`lightcache.cpp:838`), the length of the ray fired
/// at the sky to decide whether a skylight reaches a point.
///
/// Valve's own comment calls it `max_range * sqrt(3)`, and 1.74 is not
/// `sqrt(3)` — the tree spells the same quantity exactly as `MAX_TRACE_LENGTH`
/// elsewhere (`worldsize.h:32`). Transcribed as written, because the ray only
/// has to leave the map and the 0.5% is not worth diverging over.
const SKY_TRACE_LENGTH: f32 = 2.0 * 16384.0 * 1.74;

/// `MASK_OPAQUE | CONTENTS_BLOCKLIGHT` — what stops light.
///
/// `CONTENTS_BLOCKLIGHT` is `vbsp`'s "this brush casts a shadow and nothing
/// else", and it is in the mask of the *old* intensity function and not the
/// new one (`lightcache.cpp:843` against `:944`). The old one is what runs:
/// the new one is reached only through a `lightzbuffer_t`, which
/// `AddStaticLighting` supplies only when `r_lightcache_zbuffercache` is set,
/// and it defaults to 0.
const LIGHT_BLOCKING: Contents = Contents(Contents::MASK_OPAQUE.0 | Contents::BLOCKLIGHT.0);

/// `LIGHT_NO_OCCLUSION_CHECK` (`lightcache.h:34`).
const NO_OCCLUSION_CHECK: u32 = 0x1;
/// `LIGHT_NO_RADIUS_CHECK`.
///
/// Set by [`intensity_and_direction_in_box`] on every call it makes, because
/// the radius has just been tested in a form that knows about the box.
const NO_RADIUS_CHECK: u32 = 0x2;
// `LIGHT_OCCLUDE_VS_PROPS` (0x4) and `LIGHT_IGNORE_LIGHTSTYLE_VALUE` (0x8) are
// the other two, and neither is reachable from here: the first selects a trace
// filter that bumps against static props, which this port has no collision
// data for, and the second belongs to the lightstyle path.

/// The number of bits of a coordinate one light-cache grid cell spans —
/// `HASH_GRID_SIZEX`, `Y` and `Z` (`lightcache.cpp:60`). 32 x 32 x 128 units.
const HASH_GRID_SIZE: [i32; 3] = [5, 5, 7];

/// Which leaf's samples light a point in `leaf`, and which range of them.
///
/// **The solid-leaf redirect lives here.** A leaf with zero samples and a
/// non-zero `first_sample` is a *solid* leaf, and its `first_sample` is a
/// **leaf index**, not a sample index — `vrad` writes it so that a prop
/// embedded in geometry borrows a neighbour's lighting instead of going black.
/// The field means two different things depending on the count beside it, and
/// nothing in the lump says so (`modelloader.cpp:7309`).
fn samples_for(index: &[LeafAmbientIndex], leaf: usize) -> Option<(usize, usize, usize)> {
    let mut leaf = leaf;
    let mut ambient = *index.get(leaf)?;
    if ambient.sample_count == 0 && ambient.first_sample != 0 {
        leaf = ambient.first_sample as usize;
        ambient = *index.get(leaf)?;
    }
    if ambient.sample_count == 0 {
        return None;
    }
    Some((
        leaf,
        ambient.first_sample as usize,
        ambient.sample_count as usize,
    ))
}

/// The interpolation itself, given a leaf's bounds and its samples.
///
/// `Mod_LeafAmbientColorAtPos`' inner loop, split out from the lookup so it can
/// be tested without a BSP tree.
fn reconstruct(
    mins: Vec3,
    maxs: Vec3,
    samples: &[LeafAmbientSample],
    position: Vec3,
) -> AmbientCube {
    let mut out = [[0.0f32; 3]; AMBIENT_CUBE_FACES];
    let mut total = 0.0f32;
    for sample in samples {
        // The original works from the leaf's centre and half-diagonal and
        // scales by `2/255`; that is `mins + (xyz/255) * (maxs - mins)` with
        // the halves cancelled, and this form does not need the centre.
        let fraction = Vec3::new(
            f32::from(sample.x) / 255.0,
            f32::from(sample.y) / 255.0,
            f32::from(sample.z) / 255.0,
        );
        let sample_position = mins + fraction * (maxs - mins);

        // Inverse *squared* distance, with the `+1` that keeps a sample the
        // prop is standing on from dominating to infinity.
        let factor = 1.0 / (sample_position.distance_squared(position) + 1.0);
        total += factor;
        for (face, colour) in sample.cube.iter().enumerate() {
            let linear = decode(*colour);
            for channel in 0..3 {
                out[face][channel] += linear[channel] * factor;
            }
        }
    }

    if total > 0.0 {
        for face in &mut out {
            for channel in face.iter_mut() {
                *channel /= total;
            }
        }
    }
    out
}

/// One `ColorRGBExp32` of an ambient cube, in the units this port's shaders
/// light with.
///
/// **`ColorRGBExp32ToVector`, not `TexLightToLinear`** — the opposite of the
/// rule the lightmap path follows, and the two differ by exactly 255. The
/// original decodes an ambient cube this way (`modelloader.cpp:7338`) and a
/// lightmap luxel the other (`gl_lightmap.cpp:572`), which reads like an
/// inconsistency and is not: the two values reach the GPU by different routes.
///
/// It was checked rather than assumed, because this port has one linear space
/// that both `LightmappedGeneric` and `VertexLitGeneric` sample directly and
/// getting it backwards is a factor of 255 either way. Over `sp_a1_intro1`,
/// mean luminance under `TexLightToLinear` is 0.0249 for the lightmap and
/// 0.0002 for the ambient cubes — 122× apart, which this closes to 0.5×. Props
/// decoded the lightmap's way are black.
fn decode(colour: crate::materials::lightmap::ColorRgbExp32) -> [f32; 3] {
    colour.to_vector()
}

/// The parts of a `.bsp` that answer "how bright is it here", kept after the
/// rest of the file is dropped.
///
/// [`World::load`](crate::engine::world::World::load) reads a map, uploads it
/// and lets the `Bsp` go; the only thing that needs it afterwards is lighting a
/// model placed at a point — which for a *static* prop is answered once at load
/// and never again, and for a model an **entity** places cannot be, because the
/// entities do not exist until the game server has spawned them.
///
/// This is `CBaseLightCache` with the *cache* removed: Valve kept 200 entries
/// on an LRU keyed by a 32x32x128 grid cell because a moving model asks this
/// question every frame, and nothing here moves. What it holds instead is the
/// four lumps the question is answered from, and all four are small —
/// `sp_a1_intro1`'s are 2,038 leaves, their 8,325 ambient samples and 43 world
/// lights, a few tens of kilobytes against the map's 12 MB of lightmaps.
#[derive(Debug, Clone, Default)]
pub struct LightCache {
    index: Vec<LeafAmbientIndex>,
    /// Each leaf's bounds, which is all [`reconstruct`] reads of a leaf.
    bounds: Vec<(Vec3, Vec3)>,
    samples: Vec<LeafAmbientSample>,
    /// The world lights that can still contribute, fixed up — see
    /// [`static_world_lights`].
    lights: Vec<WorldLight>,
}

impl LightCache {
    pub fn from_bsp(bsp: &Bsp) -> LightCache {
        let bounds = |v: [i16; 3]| Vec3::new(f32::from(v[0]), f32::from(v[1]), f32::from(v[2]));
        LightCache {
            index: bsp.leaf_ambient_index.clone(),
            bounds: bsp
                .leaves
                .iter()
                .map(|leaf| (bounds(leaf.mins), bounds(leaf.maxs)))
                .collect(),
            samples: bsp.leaf_ambient.clone(),
            lights: static_world_lights(bsp),
        }
    }

    /// The world lights that survived [`static_world_lights`]' filter, for the
    /// depot tests and the startup log.
    pub fn lights(&self) -> &[WorldLight] {
        &self.lights
    }

    /// Reconstructs the baked ambient lighting at a world position.
    ///
    /// `Mod_LeafAmbientColorAtPos` (`modelloader.cpp:7301`). Returns black
    /// when the map has no baked ambient lighting at all, which is a map
    /// compiled without `vrad`.
    ///
    /// The solid-leaf redirect is [`samples_for`]'s; the interpolation is
    /// [`reconstruct`]'s.
    pub fn ambient_at(&self, collision: &CollisionBsp, position: Vec3) -> AmbientCube {
        let Some((leaf_index, first, count)) = samples_for(&self.index, collision.leaf(position))
        else {
            return [[0.0; 3]; AMBIENT_CUBE_FACES];
        };
        let (Some(&(mins, maxs)), Some(samples)) = (
            self.bounds.get(leaf_index),
            self.samples.get(first..first + count),
        ) else {
            return [[0.0; 3]; AMBIENT_CUBE_FACES];
        };
        reconstruct(mins, maxs, samples, position)
    }

    /// The whole lighting state one model at `position` is drawn under.
    ///
    /// `LightcacheGetStatic( cache, nullptr, LIGHTCACHEFLAGS_STATIC )`, which
    /// is `ComputeStaticLightingForCacheEntry` — the ambient cube, then
    /// `AddStaticLighting` over every world light — with the lightstyle and
    /// dynamic accumulations left out because neither exists here.
    ///
    /// [`ModelLighting::static_light`] comes back **0**: whether a model also
    /// wears `vrad`'s per-vertex bake is not a question about the point it
    /// stands at, and the answer decides whether this whole state applies at
    /// all. See
    /// [`PropModels::draw`](crate::engine::world::props::PropModels::draw).
    ///
    /// # Cost
    ///
    /// One trace per light that gets past the box and intensity culls, and
    /// **nine** per skylight, so this is the expensive half of loading a map's
    /// props — measured at 1.4 s for all 56,955 static props in the game, or
    /// about 13 ms a map. `tracer` is taken rather than made because a fresh
    /// [`Tracer`] allocates a stamp per brush and per displacement — make one
    /// and light the whole map with it.
    pub fn lighting_at(&self, tracer: &mut Tracer<'_>, position: Vec3) -> ModelLighting {
        let mut state = LightingState {
            cube: self.ambient_at(tracer.collision(), position),
            lights: [0; MAX_LIGHTS],
            illum: [0.0; MAX_LIGHTS],
            count: 0,
        };
        // `AddStaticLighting` (`lightcache.cpp:1875`). Its two `continue`s —
        // the ambient-cube lights and the styled ones — are hoisted into
        // `static_world_lights`, because both are properties of the light and
        // not of the point being lit.
        for index in 0..self.lights.len() {
            self.add_world_light_to_lighting_state(index, &mut state, tracer, position);
        }
        self.model_lighting(&state)
    }

    /// `AddWorldLightToLightingState` (`lightcache.cpp:1379`) with `dynamic`
    /// false and `r_oldlightselection` 0 — decide whether this light is one of
    /// the brightest few, and if it is not, fold it into the ambient cube.
    ///
    /// **The early `return` is the whole of the design.** A light that wins a
    /// slot is *not* added to the cube; one that loses is. So the two terms
    /// partition the lights rather than overlapping, and the energy of a light
    /// that just missed the cut does not disappear — it stops being
    /// directional.
    fn add_world_light_to_lighting_state(
        &self,
        mut index: usize,
        state: &mut LightingState,
        tracer: &mut Tracer<'_>,
        origin: Vec3,
    ) {
        let mut light = &self.lights[index];
        // **Not the model's own bounding box**: the box here is the light
        // cache's 32x32x128 grid cell around the sample point, which is what
        // `AddWorldLightToLightingState` computes whoever is asking. The
        // prop's real box is only used by the dlight path
        // (`AddWorldLightToLightingStateForStaticProps`), which is not ported.
        let (mins, maxs) = light_cache_bounds(origin);
        let mut direction = Vec3::ZERO;
        let mut ratio =
            intensity_and_direction_in_box(light, origin, mins, maxs, 0, &mut direction, tracer);
        if ratio <= 0.0 {
            return;
        }

        let mut angular = world_light_angle(light, Vec3::from(light.normal), direction, direction);
        let mut illum = ratio * Vec3::from(light.intensity).dot(GRAYSCALE);

        // "See the comment titled EMIT_SURFACE LIGHTS at the top for info":
        // an `emit_surface` light is never rejected for being dim, because the
        // dim ones were already taken out of this list by `vrad` and the ones
        // left are the bright ones.
        if light.emit_type == emit::SURFACE || illum >= WORLD_LIGHT_MIN {
            if state.count < WORLD_LIGHTS {
                state.lights[state.count] = index;
                state.illum[state.count] = illum;
                state.count += 1;
                return;
            }
            if let Some(dimmest) = find_darkest_world_light(state.count, &state.illum, illum) {
                // The new light takes the slot and `light` now names the one
                // it evicted — which is why everything below is recomputed.
                std::mem::swap(&mut index, &mut state.lights[dimmest]);
                std::mem::swap(&mut illum, &mut state.illum[dimmest]);
                light = &self.lights[index];

                // "NOTE: We know the dot product can't be zero or illum would
                // have been 0 to start with!" — which is true of every light
                // that can reach here *except* an `emit_surface` one, whose
                // slot did not depend on its illum. Measured on the depot:
                // one world light in Portal 2 has a zero grayscale intensity
                // and it is an `emit_skylight`, so Valve's division is never
                // actually the 0/0 it looks like. Guarded anyway, because the
                // result of getting it wrong is a NaN in an ambient cube.
                let grey = Vec3::from(light.intensity).dot(GRAYSCALE);
                ratio = if grey != 0.0 { illum / grey } else { 0.0 };

                if light.emit_type == emit::SKYLIGHT {
                    direction = Vec3::ZERO;
                    angular = 1.0;
                } else {
                    direction = Vec3::from(light.origin) - origin;
                    normalize_in_place(&mut direction);
                    angular =
                        world_light_angle(light, Vec3::from(light.normal), direction, direction);
                }
            }
        }

        add_to_light_cube(light, &mut state.cube, direction, ratio * angular);
    }

    /// The selected lights and the accumulated cube, in the shape the shader's
    /// group 3 wants.
    ///
    /// `R_SetNonAmbientLightingState` (`l_studio.cpp:318`), which is where a
    /// light that has no hardware form quietly disappears: `emit_quakelight`
    /// cannot be expressed as a `LightDesc_t` ("Can't do quake lights in
    /// hardware (x-r factor)") and `emit_skyambient` "doesn't factor into
    /// local lighting", so both are skipped *after* winning a slot and the
    /// count comes out lower than [`LightingState::count`]. Neither appears in
    /// Portal 2 with a slot to lose.
    ///
    /// The `LightStyleValue( style )` bias the original applies to each
    /// colour here is 1 for every light in this list, because
    /// `d_lightstylevalue[0]` is 264 and the list holds nothing but style 0.
    fn model_lighting(&self, state: &LightingState) -> ModelLighting {
        let mut lights = [Light::NONE; MAX_LIGHTS];
        let mut count = 0;
        for &index in &state.lights[..state.count] {
            if let Some(light) = to_hardware_light(&self.lights[index]) {
                lights[count] = light;
                count += 1;
            }
        }

        let mut ambient_cube = [[0.0f32; 4]; AMBIENT_CUBE_FACES];
        for (out, face) in ambient_cube.iter_mut().zip(state.cube) {
            *out = [face[0], face[1], face[2], 0.0];
        }

        ModelLighting {
            ambient_cube,
            lights,
            count: count as u32,
            // Not this function's to answer — see `lighting_at`.
            static_light: 0,
            ambient_light: 1,
            _padding: 0,
        }
    }
}

/// `LightingState_t` + `LightingStateInfo_t` (`lightcache.cpp:128`), which are
/// two structs in the original only because the second is cached separately.
///
/// `lights` holds *indices* into [`LightCache::lights`] where Valve holds
/// pointers, which is the same thing with the swap in
/// `AddWorldLightToLightingState` still expressible.
struct LightingState {
    cube: AmbientCube,
    lights: [usize; MAX_LIGHTS],
    /// `m_pIllum` — the grayscale brightness each slot was won with, and the
    /// only thing [`find_darkest_world_light`] ranks by.
    illum: [f32; MAX_LIGHTS],
    count: usize,
}

const _: () = assert!(WORLD_LIGHTS <= MAX_LIGHTS);

/// `FindDarkestWorldLight` (`lightcache.cpp:1281`): the dimmest slot that is
/// dimmer than `new_illum`, or `None` when the newcomer is the dimmest of all.
fn find_darkest_world_light(
    count: usize,
    illum: &[f32; MAX_LIGHTS],
    new_illum: f32,
) -> Option<usize> {
    let mut dimmest = None;
    let mut min = new_illum;
    for (j, &value) in illum.iter().enumerate().take(count) {
        // "only check ones dimmer than have already been checked"
        if value < min {
            min = value;
            dimmest = Some(j);
        }
    }
    dimmest
}

/// `AddWorldLightToLightCube` (`lightcache.cpp:1303`): spread a light that did
/// not win a slot over the six ambient faces it shines towards.
///
/// Valve's own comment on the method is "FIXME: This method is a guess, I don't
/// know how it should be done".
fn add_to_light_cube(light: &WorldLight, cube: &mut AmbientCube, direction: Vec3, ratio: f32) {
    if ratio == 0.0 {
        return;
    }
    let intensity = light.intensity;
    for (face, &axis) in cube.iter_mut().zip(BOX_DIRECTIONS.iter()) {
        let t = axis.dot(direction);
        if t > 0.0 {
            for (channel, value) in face.iter_mut().enumerate() {
                *value += ratio * t * intensity[channel];
            }
        }
    }
}

/// `ComputeLightcacheBounds` (`lightcache.cpp:549`): the grid cell a point
/// falls in.
///
/// The arithmetic looks wrong and is not. A right shift of a negative number
/// is not the floor Valve wants, so the magnitude is shifted and the sign put
/// back by hand — `-(i + 1)` for a negative axis, which is what makes the cell
/// containing -40 be [-64, -32) rather than (-32, -64]. The original's own
/// comment on the same trick elsewhere is "this is suspicious and *maybe*
/// wrong".
fn light_cache_bounds(origin: Vec3) -> (Vec3, Vec3) {
    let mut mins = Vec3::ZERO;
    let mut maxs = Vec3::ZERO;
    for axis in 0..3 {
        let shift = HASH_GRID_SIZE[axis];
        let magnitude = (origin[axis].abs() as i32) >> shift;
        let cell = if origin[axis] >= 0.0 {
            magnitude
        } else {
            -(magnitude + 1)
        };
        mins[axis] = (cell << shift) as f32;
        maxs[axis] = ((cell << shift) + (1 << shift)) as f32;
    }
    (mins, maxs)
}

/// `LightIntensityAndDirectionInBox` (`lightcache.cpp:1087`): how much of this
/// light reaches anywhere in `mins`..`maxs`, and from which direction.
///
/// The box tests are a cull and nothing else — whatever survives them is
/// measured at `mid` by [`intensity_and_direction_at_point`], with
/// [`NO_RADIUS_CHECK`] set because the radius has just been checked here. The
/// exception is a skylight, which is answered *here*, by taking the brightest
/// of the cell's centre and its eight corners: a prop half in shadow at the
/// edge of a sunbeam is lit by it rather than not.
fn intensity_and_direction_in_box(
    light: &WorldLight,
    mid: Vec3,
    mins: Vec3,
    maxs: Vec3,
    flags: u32,
    direction: &mut Vec3,
    tracer: &mut Tracer<'_>,
) -> f32 {
    let origin = Vec3::from(light.origin);
    let normal = Vec3::from(light.normal);
    match light.emit_type {
        emit::SPOTLIGHT | emit::POINT => {
            // The spotlight arm falls through into the point one in the
            // original — "NOTE: fall through to radius check in point case".
            if light.emit_type == emit::SPOTLIGHT {
                let sphere_radius = (maxs - mid).length();
                let distance = (origin - mid).length();
                if distance > sphere_radius + light.radius * LIGHT_RADIUS_FACTOR {
                    return 0.0;
                }
                // "PERFORMANCE: precalc this and store in the light?"
                let sine = light.stopdot2.acos().sin();
                if !is_sphere_intersecting_cone(
                    mid,
                    sphere_radius,
                    origin,
                    normal,
                    sine,
                    light.stopdot2,
                ) {
                    return 0.0;
                }
            }
            // **The factor multiplies the squared radius here** and the plain
            // radius above; see `LIGHT_RADIUS_FACTOR`.
            if sqr_distance_to_aabb(mins, maxs, origin)
                > light.radius * light.radius * LIGHT_RADIUS_FACTOR
            {
                return 0.0;
            }
        }
        emit::SURFACE => {
            // A fixed 90-degree cone, and no radius factor: an `emit_surface`
            // light really does stop at its radius.
            let sphere_radius = (maxs - mid).length();
            let distance = (origin - mid).length();
            if distance > sphere_radius + light.radius {
                return 0.0;
            }
            if !is_sphere_intersecting_cone(mid, sphere_radius, origin, normal, 1.0, 0.0) {
                return 0.0;
            }
        }
        emit::SKYLIGHT => {
            let corners = [
                mins,
                Vec3::new(maxs.x, mins.y, mins.z),
                Vec3::new(mins.x, mins.y, maxs.z),
                Vec3::new(mins.x, maxs.y, mins.z),
                Vec3::new(mins.x, maxs.y, maxs.z),
                Vec3::new(maxs.x, maxs.y, mins.z),
                Vec3::new(maxs.x, mins.y, maxs.z),
                maxs,
            ];
            let mut max = intensity_and_direction_at_point(
                light,
                mid,
                flags | NO_RADIUS_CHECK,
                direction,
                tracer,
            );
            for corner in corners {
                // **`direction` is left holding the *last* corner's answer**,
                // not the brightest one's — so a cell whose `maxs` corner is
                // in shadow comes back with a ratio of 1 and a zero direction,
                // which `world_light_angle` then turns into an angular ratio
                // of 0. Valve's, and load-bearing in the sense that fixing it
                // would light props the shipped game leaves dark.
                max = max.max(intensity_and_direction_at_point(
                    light,
                    corner,
                    flags | NO_RADIUS_CHECK,
                    direction,
                    tracer,
                ));
            }
            return max;
        }
        _ => {}
    }

    intensity_and_direction_at_point(light, mid, flags | NO_RADIUS_CHECK, direction, tracer)
}

/// `LightIntensityAndDirectionAtPointOld` (`lightcache.cpp:815`): the fraction
/// of this light that reaches `mid`, and the unit vector towards it.
///
/// **The `Old` one is the one that runs.** `LightIntensityAndDirectionAtPoint`
/// picks the new shadow-z-buffer form only when handed a `lightzbuffer_t`, and
/// `AddStaticLighting` only has one when `r_lightcache_zbuffercache` is set,
/// which defaults to 0. The two differ in more than caching — the old traces
/// from the point to the light and the new from the light to the point, and
/// only the old one puts `CONTENTS_BLOCKLIGHT` in the mask.
fn intensity_and_direction_at_point(
    light: &WorldLight,
    mid: Vec3,
    flags: u32,
    direction: &mut Vec3,
    tracer: &mut Tracer<'_>,
) -> f32 {
    match light.emit_type {
        emit::SKYLIGHT => {
            // "There can be more than one skylight, but we should only ever be
            // affected by one of them (multiple ones are created from a single
            // light in vrad)."
            *direction = Vec3::ZERO;
            let end = mid - Vec3::from(light.normal) * SKY_TRACE_LENGTH;
            let sky = tracer.trace_world(&Ray::line(mid, end), LIGHT_BLOCKING);
            if sky.surface_flags & surf::SKY == 0 {
                // Did not reach the sky texture, so this point is in shadow.
                return 0.0;
            }
            *direction = -Vec3::from(light.normal);
            return 1.0;
        }
        // "always ignore these" — the sky's ambient term is `vrad`'s business.
        emit::SKYAMBIENT => return 0.0,
        _ => {}
    }

    *direction = Vec3::from(light.origin) - mid;
    let ratio = distance_falloff(light, *direction, flags & NO_RADIUS_CHECK != 0);
    // `ratio *= LightStyleValue( style )` goes here, and is 1 for every light
    // this port keeps — see `LightCache::model_lighting`.

    // "Early out for really low-intensity lights. That way we don't need to
    // ray-cast or normalize." This is the cull that actually does the work:
    // the box tests above pass almost everything.
    let intensity = light.intensity[0]
        .max(light.intensity[1])
        .max(light.intensity[2]);
    if light.emit_type != emit::SURFACE && intensity * ratio < WORLD_LIGHT_MIN {
        return 0.0;
    }

    let distance = normalize_in_place(direction);
    if flags & NO_OCCLUSION_CHECK != 0 {
        return ratio;
    }

    let blocked = tracer.trace_world(&Ray::line(mid, Vec3::from(light.origin)), LIGHT_BLOCKING);
    // Valve's comment here is the single word "hack". The eight units of slack
    // are what let a light sitting in the surface of its own fixture still
    // light the room: a trace that stops just short of the light is not a
    // shadow.
    if (1.0 - blocked.fraction) * distance > 8.0 {
        return 0.0;
    }
    ratio
}

/// `Engine_WorldLightDistanceFalloff` (`l_studio.cpp:365`).
///
/// `no_radius_check` reaches every call this port makes, because
/// [`intensity_and_direction_in_box`] has already done the radius test in a
/// form that knows about the box. It does **not** disable an `emit_surface`
/// light's radius, which is tested unconditionally — Valve's asymmetry, and
/// the reason those lights need no other cull.
fn distance_falloff(light: &WorldLight, delta: Vec3, no_radius_check: bool) -> f32 {
    match light.emit_type {
        emit::SURFACE => {
            if light.radius != 0.0 && delta.length_squared() > light.radius * light.radius {
                return 0.0;
            }
            inv_r_squared(delta)
        }
        emit::SKYLIGHT | emit::SKYAMBIENT => 1.0,
        emit::QUAKELIGHT => {
            // `X - r`, a linear falloff. None in Portal 2.
            (light.linear_attn - delta.length()).max(0.0)
        }
        emit::POINT | emit::SPOTLIGHT => {
            let distance_squared = delta.length_squared();
            let distance = distance_squared.sqrt();
            if !no_radius_check && light.radius != 0.0 && distance > light.radius {
                return 0.0;
            }
            1.0 / (light.constant_attn
                + light.linear_attn * distance
                + light.quadratic_attn * distance_squared)
        }
        // "Bug: need to return an error" — a light of an unknown type is fully
        // bright at every distance, which is what the original does.
        _ => 1.0,
    }
}

/// `Engine_WorldLightAngle` (`l_studio.cpp:437`): the angular term, which is
/// the cone for a spotlight and the cosine for everything else.
///
/// Called throughout as `world_light_angle( light, light.normal, direction,
/// direction )` — the surface normal and the direction to the light are the
/// *same vector*, because the thing being lit here is a point rather than a
/// surface. That is why a point light's term comes out as 1 and not as a
/// Lambert factor: `dot( direction, direction )` of a unit vector.
fn world_light_angle(light: &WorldLight, light_normal: Vec3, normal: Vec3, delta: Vec3) -> f32 {
    match light.emit_type {
        emit::SURFACE => {
            let dot = normal.dot(delta);
            if dot < 0.0 {
                return 0.0;
            }
            let facing = -delta.dot(light_normal);
            // `ON_EPSILON / 10` (`qlimits.h:23` — `ON_EPSILON` is 0.1).
            if facing <= 0.01 {
                return 0.0; // behind the light surface
            }
            dot * facing
        }
        emit::POINT | emit::QUAKELIGHT => normal.dot(delta).max(0.0),
        emit::SPOTLIGHT => {
            let dot = normal.dot(delta);
            if dot < 0.0 {
                return 0.0;
            }
            let facing = -delta.dot(light_normal);
            if facing <= light.stopdot2 {
                return 0.0; // outside the light cone
            }
            if facing >= light.stopdot {
                return dot; // inside the inner cone
            }
            // The penumbra. `(exponent == 1 || exponent == 0)` takes the
            // linear form; the loader has already turned a 0 into a 1, so the
            // second half of that test is for a light this port never sees.
            let penumbra = (facing - light.stopdot2) / (light.stopdot - light.stopdot2);
            match light.exponent == 1.0 || light.exponent == 0.0 {
                true => dot * penumbra,
                false => dot * penumbra.powf(light.exponent),
            }
        }
        emit::SKYLIGHT => (-normal.dot(light_normal)).max(0.0),
        // "not supported"
        emit::SKYAMBIENT => 1.0,
        _ => 0.0,
    }
}

/// `WorldLightToMaterialLight` (`l_studio.cpp:182`) and
/// `CompileVertexShaderLocalLights` (`shaderapidx8.cpp:14022`) fused: one of
/// `vrad`'s lights as five constant registers.
///
/// `None` for the two kinds that have no hardware form —
/// `emit_quakelight`'s `x - r` falloff cannot be written as
/// `1/(a + bd + cd²)`, and `emit_skyambient` "doesn't factor into local
/// lighting".
fn to_hardware_light(light: &WorldLight) -> Option<Light> {
    match light.emit_type {
        // "A 180 degree spotlight" — Valve's comment; the half-angle is 90,
        // so both cone cosines are 0 and the cone covers the hemisphere the
        // surface faces. Its `1 / (thetaDot - phiDot)` is written as **1**
        // rather than computed, which is the case `Light::spot`'s zero-spread
        // branch exists for.
        emit::SURFACE => Some(Light::spot(
            light.intensity,
            light.origin,
            light.normal,
            [0.0, 0.0, 1.0],
            1.0,
            0.0,
            0.0,
        )),
        emit::SPOTLIGHT => Some(Light::spot(
            light.intensity,
            light.origin,
            light.normal,
            hardware_attenuation(light),
            // `m_Falloff = exponent ? exponent : 1.0f`, which after the
            // loader's fixup is always the exponent.
            if light.exponent != 0.0 {
                light.exponent
            } else {
                1.0
            },
            light.stopdot,
            light.stopdot2,
        )),
        emit::POINT => Some(Light::point(
            light.intensity,
            light.origin,
            hardware_attenuation(light),
        )),
        // Valve writes the light's origin into the position register as well.
        // Nothing reads it — a directional light's shading is selected off
        // `color.w` before the position is used — so this port's
        // `Light::directional`, which leaves it at the origin, is the same
        // light.
        emit::SKYLIGHT => Some(Light::directional(light.intensity, light.normal)),
        _ => None,
    }
}

/// `WorldLightToMaterialLight`'s "No attenuation case".
///
/// Unreachable for a Portal 2 light and ported anyway: `Mod_LoadWorldlights`
/// has already replaced an all-zero attenuation with a quadratic 1 for the two
/// types that get here, so this can only fire for a light the loader did not
/// fix up.
fn hardware_attenuation(light: &WorldLight) -> [f32; 3] {
    let attenuation = [light.constant_attn, light.linear_attn, light.quadratic_attn];
    match attenuation == [0.0; 3] {
        true => [1.0, 0.0, 0.0],
        false => attenuation,
    }
}

/// The world lights a [`LightCache`] keeps, fixed up as
/// `Mod_LoadWorldlights` (`modelloader.cpp:1392`) fixes them.
///
/// Two filters and four fixups, and the filters are the ones `AddStaticLighting`
/// applies per light per point — hoisted here because neither depends on where
/// the light is being measured from:
///
/// - **`style != 0`** is a lightstyle light, which belongs to
///   `LIGHTCACHEFLAGS_LIGHTSTYLE` and is animated by machinery this port does
///   not have. 213 of Portal 2's 14,246.
/// - **[`dwl::IN_AMBIENT_CUBE`]** is a light `vrad` already put in the leaf
///   cubes this module reads. 6,731 of them — see the module docs.
///
/// The fixups are Valve's "fixup for backward compatability", and the last of
/// them is not: **14,237 of the game's 14,246 lights have no radius** and get
/// one computed here, so `ComputeLightRadius` is the normal path rather than
/// an upgrade path.
fn static_world_lights(bsp: &Bsp) -> Vec<WorldLight> {
    bsp.world_lights
        .iter()
        .filter(|light| light.style == 0)
        .filter(|light| light.flags & dwl::IN_AMBIENT_CUBE == 0)
        .map(|light| {
            let mut light = *light;
            let unattenuated = light.constant_attn == 0.0
                && light.linear_attn == 0.0
                && light.quadratic_attn == 0.0;
            if light.emit_type == emit::SPOTLIGHT {
                if unattenuated {
                    light.quadratic_attn = 1.0;
                }
                if light.exponent == 0.0 {
                    light.exponent = 1.0;
                }
            } else if light.emit_type == emit::POINT && unattenuated {
                // "To match earlier lighting, use quadratic..."
                light.quadratic_attn = 1.0;
            }
            // "I replaced the cuttoff_dot field (which took a value from 0 to
            // 1) with a max light radius. Radius of less than 1 will never
            // happen, so I can get away with this."
            if light.radius < 1.0 {
                light.radius = compute_light_radius(&light, bsp.world_lights_are_hdr);
            }
            light
        })
        .collect()
}

/// `ComputeLightRadius` (`gl_drawlights.cpp:46`): how far a light with no
/// radius of its own reaches before it is dimmer than `MIN_LIGHT_VALUE`.
///
/// **The HDR halving is not a rounding detail.** "HACKHACK: Usually our
/// designers scale the light intensity by 0.5 in HDR. This keeps the behavior
/// of the cutoff radius consistent between LDR and HDR" — so an HDR map's
/// lights reach `sqrt(2)` further for the same intensity, and Portal 2's maps
/// are all HDR.
///
/// Note the guard is `radius != 0`, not the caller's `radius < 1`: a light
/// with a radius of half a unit keeps it.
fn compute_light_radius(light: &WorldLight, is_hdr: bool) -> f32 {
    /// `LIGHT_MIN_LIGHT_VALUE` (`gl_drawlights.cpp:44`).
    const MIN_LIGHT_VALUE: f32 = 0.03;
    /// What the original calls infinite: "Infinite, but we're not going to
    /// draw it as such".
    const INFINITE: f32 = 2000.0;

    if light.radius != 0.0 {
        return light.radius;
    }
    let minimum = if is_hdr {
        MIN_LIGHT_VALUE * 0.5
    } else {
        MIN_LIGHT_VALUE
    };
    let intensity = Vec3::from(light.intensity).length();

    if light.quadratic_attn == 0.0 {
        if light.linear_attn == 0.0 {
            return INFINITE;
        }
        return (intensity / minimum - light.constant_attn) / light.linear_attn;
    }

    // The positive root of `qa·r² + la·r + (ca - I/min) = 0`.
    let (a, b) = (light.quadratic_attn, light.linear_attn);
    let c = light.constant_attn - intensity / minimum;
    let discriminant = b * b - 4.0 * a * c;
    if discriminant < 0.0 {
        return INFINITE;
    }
    ((-b + discriminant.sqrt()) / (2.0 * a)).max(0.0)
}

/// `IsSphereIntersectingCone` (`collisionutils.cpp:516`).
///
/// `cone_sine` and `cone_cosine` are of the cone's *half-angle*. The sphere is
/// tested against the cone pushed back along its own axis by
/// `radius / sine`, which is the standard exact test; the second half handles
/// the sphere sitting behind the apex.
fn is_sphere_intersecting_cone(
    center: Vec3,
    radius: f32,
    cone_origin: Vec3,
    cone_normal: Vec3,
    cone_sine: f32,
    cone_cosine: f32,
) -> bool {
    let back_center = cone_origin - (radius / cone_sine) * cone_normal;
    let mut delta = center - back_center;
    let mut length = delta.length();
    if cone_normal.dot(delta) < length * cone_cosine {
        return false;
    }
    delta = center - cone_origin;
    length = delta.length();
    if -cone_normal.dot(delta) >= length * cone_sine {
        return length <= radius;
    }
    true
}

/// `CalcSqrDistanceToAABB` (`mathlib_base.cpp:3699`): zero for a point inside
/// the box.
fn sqr_distance_to_aabb(mins: Vec3, maxs: Vec3, point: Vec3) -> f32 {
    let mut distance = 0.0;
    for axis in 0..3 {
        let delta = if point[axis] < mins[axis] {
            mins[axis] - point[axis]
        } else if point[axis] > maxs[axis] {
            point[axis] - maxs[axis]
        } else {
            continue;
        };
        distance += delta * delta;
    }
    distance
}

/// `InvRSquared` (`vector.h:2777`): `1/r²`, clamped so that a point closer
/// than one unit does not divide by nearly nothing.
fn inv_r_squared(v: Vec3) -> f32 {
    1.0 / v.length_squared().max(1.0)
}

/// `VectorNormalize` (`mathlib_base.cpp:77`): normalises in place and returns
/// the length it had.
///
/// The `+ FLT_EPSILON` is Valve's, and its comment says what it is for —
/// "eliminate the possibility of divide by zero". It matters here rather than
/// being defensive: a light can sit exactly on the point being lit, and
/// `glam`'s own `normalize` would give a `NaN` direction that then propagates
/// into an ambient cube.
fn normalize_in_place(v: &mut Vec3) -> f32 {
    let length = v.length();
    *v *= 1.0 / (length + f32::EPSILON);
    length
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::lightmap::ColorRgbExp32;

    /// A sample at `(x, y, z)` of the leaf's bounds whose whole cube is one
    /// grey value.
    fn sample(x: u8, y: u8, z: u8, value: u8) -> LeafAmbientSample {
        LeafAmbientSample {
            cube: [ColorRgbExp32 {
                r: value,
                g: value,
                b: value,
                // 2^0 = 1, so `to_vector` gives the mantissa back unchanged.
                exponent: 0,
            }; 6],
            x,
            y,
            z,
            _pad: 0,
        }
    }

    const MINS: Vec3 = Vec3::new(0.0, 0.0, 0.0);
    const MAXS: Vec3 = Vec3::new(255.0, 255.0, 255.0);

    /// The sample positions are fractions of the leaf's bounds, not world
    /// coordinates. Getting that wrong puts every sample at the map's origin
    /// and lights the whole level from one point.
    #[test]
    fn a_sample_sits_where_its_fraction_of_the_leaf_says() {
        // One sample, so the answer is that sample wherever it is asked from.
        let one = [sample(255, 255, 255, 100)];
        let at_it = reconstruct(MINS, MAXS, &one, Vec3::splat(255.0));
        let far = reconstruct(MINS, MAXS, &one, Vec3::ZERO);
        assert_eq!(at_it, far, "one sample is the whole answer");
        assert!((at_it[0][0] - 100.0).abs() < 1e-3);
    }

    /// Inverse *squared* distance: the nearer sample dominates, and it is not
    /// a plain average.
    #[test]
    fn the_nearer_sample_dominates() {
        let two = [sample(0, 0, 0, 200), sample(255, 255, 255, 0)];
        let near_dark = reconstruct(MINS, MAXS, &two, Vec3::splat(250.0));
        let near_bright = reconstruct(MINS, MAXS, &two, Vec3::splat(5.0));
        assert!(near_bright[0][0] > 150.0, "{:?}", near_bright[0]);
        assert!(near_dark[0][0] < 50.0, "{:?}", near_dark[0]);
        // Exactly between them it is the average, which is where a plain mean
        // and this agree and so is not what distinguishes them.
        let middle = reconstruct(MINS, MAXS, &two, Vec3::splat(127.5));
        assert!((middle[0][0] - 100.0).abs() < 1.0, "{:?}", middle[0]);
    }

    /// A leaf with samples uses its own; a solid leaf with none borrows the
    /// leaf its `first_sample` names.
    #[test]
    fn a_solid_leaf_borrows_its_neighbours_samples() {
        let index = [
            // Leaf 0: solid, redirects to leaf 1.
            LeafAmbientIndex {
                sample_count: 0,
                first_sample: 1,
            },
            // Leaf 1: two real samples starting at 5.
            LeafAmbientIndex {
                sample_count: 2,
                first_sample: 5,
            },
            // Leaf 2: genuinely empty — no samples, no redirect.
            LeafAmbientIndex {
                sample_count: 0,
                first_sample: 0,
            },
        ];
        assert_eq!(samples_for(&index, 0), Some((1, 5, 2)), "the redirect");
        assert_eq!(samples_for(&index, 1), Some((1, 5, 2)));
        assert_eq!(samples_for(&index, 2), None);
        assert_eq!(samples_for(&index, 9), None, "past the end");
    }

    /// The ambient cube decodes with `ColorRGBExp32ToVector` and not with the
    /// lightmap's `TexLightToLinear`. See [`decode`] — the difference is 255×
    /// and it is the difference between lit props and black ones.
    #[test]
    fn the_cube_decodes_the_ambient_way_and_not_the_lightmap_way() {
        let one = [sample(0, 0, 0, 51)];
        let cube = reconstruct(MINS, MAXS, &one, Vec3::ZERO);
        assert!((cube[0][0] - 51.0).abs() < 1e-3, "{:?}", cube[0]);
        // What the lightmap decode would have given.
        assert!((cube[0][0] - 51.0 / 255.0).abs() > 1.0);
    }

    /// A map with no baked ambient lighting gives black rather than a panic,
    /// and `lighting_for` still produces a valid state.
    #[test]
    fn no_samples_is_black_and_not_an_error() {
        assert_eq!(reconstruct(MINS, MAXS, &[], Vec3::ZERO), [[0.0; 3]; 6]);
    }

    // ----------------------------------------------------------------------
    // The world lights
    // ----------------------------------------------------------------------

    /// A light of the given type at the given place, with nothing else set —
    /// the shape the lump holds before [`static_world_lights`]' fixups.
    fn world_light(emit_type: i32, origin: Vec3, intensity: [f32; 3]) -> WorldLight {
        WorldLight {
            origin: origin.into(),
            intensity,
            normal: [0.0, 0.0, -1.0],
            shadow_cast_offset: [0.0; 3],
            cluster: 0,
            emit_type,
            style: 0,
            stopdot: 1.0,
            stopdot2: 0.0,
            exponent: 0.0,
            radius: 0.0,
            constant_attn: 0.0,
            linear_attn: 0.0,
            quadratic_attn: 0.0,
            flags: 0,
            tex_info: -1,
            owner: -1,
        }
    }

    /// A `Bsp` carrying nothing but these lights, for [`static_world_lights`].
    fn bsp_with_lights(lights: Vec<WorldLight>, hdr: bool) -> Bsp {
        let mut bsp = crate::engine::trace::fixture::Fixture::default().bsp();
        bsp.world_lights = lights;
        bsp.world_lights_are_hdr = hdr;
        bsp
    }

    /// The two filters `AddStaticLighting` applies per light, hoisted to load.
    ///
    /// Both are silent when wrong: keeping a styled light lights a level from
    /// switched-off lamps, and keeping an ambient-cube one counts 6,731 of
    /// Portal 2's 14,246 lights twice.
    #[test]
    fn a_styled_or_already_baked_light_is_not_a_static_one() {
        let mut styled = world_light(emit::POINT, Vec3::ZERO, [100.0; 3]);
        styled.style = 32;
        let mut baked = world_light(emit::SURFACE, Vec3::ZERO, [100.0; 3]);
        baked.flags = dwl::IN_AMBIENT_CUBE;
        let plain = world_light(emit::POINT, Vec3::X, [100.0; 3]);

        let kept = static_world_lights(&bsp_with_lights(vec![styled, baked, plain], true));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].origin, [1.0, 0.0, 0.0]);
    }

    /// `Mod_LoadWorldlights`' fixups, which are not upgrade paths: **14,237 of
    /// the game's 14,246 lights** arrive with no radius and no attenuation.
    #[test]
    fn the_loader_fills_in_the_attenuation_the_exponent_and_the_radius() {
        let lights = vec![
            world_light(emit::POINT, Vec3::ZERO, [100.0; 3]),
            world_light(emit::SPOTLIGHT, Vec3::ZERO, [100.0; 3]),
        ];
        let kept = static_world_lights(&bsp_with_lights(lights, true));

        // "To match earlier lighting, use quadratic..."
        assert_eq!(kept[0].quadratic_attn, 1.0);
        assert_eq!(kept[1].quadratic_attn, 1.0);
        // A spotlight with no exponent falls off linearly across its penumbra.
        assert_eq!(kept[0].exponent, 0.0, "only a spotlight gets this one");
        assert_eq!(kept[1].exponent, 1.0);
        // `|(100,100,100)| / 0.015` under a pure quadratic falloff.
        let expected = (100.0f32 * 3.0f32.sqrt() / 0.015).sqrt();
        assert!(
            (kept[0].radius - expected).abs() < 0.5,
            "{}",
            kept[0].radius
        );
    }

    /// The HDR halving in `ComputeLightRadius`, which is a `sqrt(2)` on the
    /// radius of every light in every Portal 2 map.
    #[test]
    fn an_hdr_map_gives_its_lights_a_longer_reach() {
        let light = world_light(emit::POINT, Vec3::ZERO, [100.0; 3]);
        let hdr = static_world_lights(&bsp_with_lights(vec![light], true))[0].radius;
        let ldr = static_world_lights(&bsp_with_lights(vec![light], false))[0].radius;
        assert!((hdr / ldr - 2.0f32.sqrt()).abs() < 1e-3, "{hdr} vs {ldr}");

        // A light with no falloff at all is "infinite, but we're not going to
        // draw it as such" — and the guard is `radius != 0`, not the caller's
        // `radius < 1`, so half a unit survives.
        let mut none = light;
        none.radius = 0.0;
        assert_eq!(compute_light_radius(&none, true), 2000.0);
        none.radius = 0.5;
        assert_eq!(compute_light_radius(&none, true), 0.5);
    }

    /// The light cache's grid cell contains the point it was asked about, on
    /// both sides of the origin — which is the whole reason for the
    /// `-(i + 1)` the function looks wrong for.
    #[test]
    fn a_grid_cell_contains_its_point_on_either_side_of_the_origin() {
        for point in [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(40.0, -40.0, 200.0),
            Vec3::new(-1.0, -0.5, -0.25),
            Vec3::new(-2048.5, 511.9, -1000.0),
            Vec3::new(31.999, -32.0, 127.5),
        ] {
            let (mins, maxs) = light_cache_bounds(point);
            assert!(
                mins.cmple(point).all() && maxs.cmpge(point).all(),
                "{point} not in {mins}..{maxs}"
            );
            assert_eq!(maxs - mins, Vec3::new(32.0, 32.0, 128.0), "cell size");
        }
        // The cell holding -40 is [-64, -32), not the shift's [-32, 0).
        assert_eq!(light_cache_bounds(Vec3::new(-40.0, 0.0, 0.0)).0.x, -64.0);
    }

    /// The spotlight cone: full inside the inner angle, nothing outside the
    /// outer one, and an interpolation between them.
    #[test]
    fn a_spotlight_falls_off_across_its_penumbra() {
        let mut light = world_light(emit::SPOTLIGHT, Vec3::ZERO, [100.0; 3]);
        light.normal = [0.0, 0.0, -1.0];
        light.stopdot = 0.8;
        light.stopdot2 = 0.2;
        light.exponent = 1.0;

        // The direction *to* the light from a point below it, which is what
        // every caller passes as both `normal` and `delta`. **That is why the
        // `dot( snormal, delta )` term is always 1 here**: the two arguments
        // are the same unit vector, so the Lambert factor a surface would get
        // folds away and the cone is the whole answer.
        let angle = |direction: Vec3| {
            world_light_angle(&light, Vec3::from(light.normal), direction, direction)
        };
        // Straight below: the light shines along -z, so the point sees it
        // along +z and `-dot(delta, normal)` is 1.
        assert_eq!(angle(Vec3::Z), 1.0, "inside the inner cone");
        // 90 degrees off: outside the outer cone.
        assert_eq!(angle(Vec3::X), 0.0);
        // Half way across the penumbra by cosine, which the exponent-1 form
        // interpolates linearly: `(0.5 - 0.2) / (0.8 - 0.2)`.
        let half = Vec3::new(0.0, (1.0f32 - 0.25).sqrt(), 0.5);
        assert!((angle(half) - 0.5).abs() < 1e-5, "{}", angle(half));
    }

    /// An `emit_surface` light is a one-sided hemisphere, and the side is what
    /// its normal says.
    #[test]
    fn a_surface_light_only_shines_out_of_its_own_face() {
        let light = world_light(emit::SURFACE, Vec3::ZERO, [100.0; 3]);
        let angle = |direction: Vec3| {
            world_light_angle(&light, Vec3::from(light.normal), direction, direction)
        };
        // The light faces -z, so a point below it is lit and one above is not.
        assert_eq!(angle(Vec3::Z), 1.0);
        assert_eq!(angle(-Vec3::Z), 0.0, "behind the light surface");
        // Exactly edge-on is refused by `ON_EPSILON / 10` rather than by the
        // sign, which is what keeps a luxel on the light's own plane dark.
        assert_eq!(angle(Vec3::X), 0.0);
    }

    /// `FindDarkestWorldLight` ranks by the illum a slot was won with, and
    /// answers `None` when the newcomer is the dimmest — which is what makes a
    /// dim light fall through into the ambient cube.
    #[test]
    fn the_dimmest_slot_is_the_one_a_brighter_light_evicts() {
        let illum = [4.0, 2.0, 0.0, 0.0];
        assert_eq!(find_darkest_world_light(2, &illum, 3.0), Some(1));
        assert_eq!(find_darkest_world_light(2, &illum, 5.0), Some(1));
        assert_eq!(find_darkest_world_light(2, &illum, 1.0), None);
        assert_eq!(find_darkest_world_light(0, &illum, 1.0), None);
    }

    /// A light that misses a slot spreads over the faces pointing at it and no
    /// others.
    #[test]
    fn the_light_cube_only_gains_on_the_faces_facing_the_light() {
        let light = world_light(emit::POINT, Vec3::ZERO, [10.0, 20.0, 30.0]);
        let mut cube = [[0.0f32; 3]; AMBIENT_CUBE_FACES];
        // Straight up: `+z` gains the lot, `-z` nothing, and the four sides
        // nothing because their dot is exactly 0.
        add_to_light_cube(&light, &mut cube, Vec3::Z, 0.5);
        assert_eq!(cube[4], [5.0, 10.0, 15.0]);
        assert_eq!(cube[5], [0.0; 3]);
        assert_eq!(cube[0], [0.0; 3]);

        // A zero ratio is the early `return`, not a multiply by zero: a
        // skylight whose direction came back zero must not touch the cube.
        let before = cube;
        add_to_light_cube(&light, &mut cube, Vec3::Z, 0.0);
        assert_eq!(cube, before);
    }

    /// The five constant registers each kind of light becomes, and the two
    /// that have no hardware form at all.
    #[test]
    fn a_world_light_becomes_the_registers_the_shader_reads() {
        let point = to_hardware_light(
            &static_world_lights(&bsp_with_lights(
                vec![world_light(emit::POINT, Vec3::X, [1.0, 2.0, 3.0])],
                true,
            ))[0],
        )
        .expect("a point light has a hardware form");
        assert_eq!(point.color, [1.0, 2.0, 3.0, 0.0], "w 0: not directional");
        assert_eq!(
            point.direction, [0.0; 4],
            "a point light writes no direction"
        );
        assert_eq!(point.position, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(point.spot, [0.0, 1.0, 1.0, 1.0], "the fold-to-1 spot term");
        assert_eq!(
            point.attenuation,
            [0.0, 0.0, 1.0, 0.0],
            "the loader's fixup"
        );

        // **The one that is silent when wrong.** An `emit_surface` light is a
        // 180-degree spotlight whose two cone cosines are both 0, so the
        // penumbra reciprocal is the zero-spread case — and Valve writes 1
        // there, not 0. A 0 makes `pow( max( 1e-4, 0 ), 1 )` the attenuation
        // and turns off 7,073 of Portal 2's 14,246 lights.
        let surface = to_hardware_light(&world_light(emit::SURFACE, Vec3::ZERO, [1.0; 3]))
            .expect("a surface light has a hardware form");
        assert_eq!(surface.spot, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(surface.direction[3], 1.0, "w 1: it is a spot light");
        assert_eq!(surface.attenuation, [0.0, 0.0, 1.0, 0.0], "pure quadratic");

        let sky = to_hardware_light(&world_light(emit::SKYLIGHT, Vec3::ZERO, [1.0; 3]))
            .expect("a skylight has a hardware form");
        assert_eq!(sky.color[3], 1.0, "w 1: directional");
        assert_eq!(sky.direction, [0.0, 0.0, -1.0, 0.0]);
        assert_eq!(sky.attenuation, [1.0, 0.0, 0.0, 0.0], "no falloff");

        // "Can't do quake lights in hardware (x-r factor)", and a sky ambient
        // "doesn't factor into local lighting".
        assert!(to_hardware_light(&world_light(emit::QUAKELIGHT, Vec3::ZERO, [1.0; 3])).is_none());
        assert!(to_hardware_light(&world_light(emit::SKYAMBIENT, Vec3::ZERO, [1.0; 3])).is_none());
    }

    // ----------------------------------------------------------------------
    // The selection, against a real collision model
    // ----------------------------------------------------------------------

    /// A wall standing in the `x = 0` plane, 1,024 units tall and open above:
    /// a point at `-x` sees `+x` over the top of it and not through it.
    fn walled_world() -> crate::engine::trace::CollisionBsp {
        let mut fixture = crate::engine::trace::fixture::Fixture::default();
        fixture.add_box(
            Vec3::new(-8.0, -4096.0, -512.0),
            Vec3::new(8.0, 4096.0, 512.0),
            Contents::SOLID,
            true,
        );
        fixture.single_leaf()
    }

    fn cache_with(lights: Vec<WorldLight>) -> LightCache {
        LightCache {
            lights: static_world_lights(&bsp_with_lights(lights, true)),
            ..LightCache::default()
        }
    }

    /// The trace is the whole of the shadowing: a light behind a wall
    /// contributes nothing, to the local lights *or* to the ambient cube.
    #[test]
    fn a_wall_between_the_point_and_the_light_takes_it_away() {
        let world = walled_world();
        // Above the wall's top, so a point at the same height sees it and one
        // on the floor does not.
        let light = Vec3::new(64.0, 0.0, 700.0);
        let cache = cache_with(vec![world_light(emit::POINT, light, [1000.0; 3])]);

        let blocked = cache.lighting_at(&mut world.tracer(), Vec3::new(-64.0, 0.0, 0.0));
        assert_eq!(blocked.count, 0, "the wall is in the way");
        assert_eq!(
            blocked.ambient_cube, [[0.0; 4]; AMBIENT_CUBE_FACES],
            "and a blocked light is not in the cube either"
        );

        let seen = cache.lighting_at(&mut world.tracer(), Vec3::new(-64.0, 0.0, 700.0));
        assert_eq!(seen.count, 1, "over the top of it");
        assert_eq!(seen.lights[0].position, [64.0, 0.0, 700.0, 1.0]);
    }

    /// **A light that wins a slot is not also in the ambient cube**, and one
    /// that loses is. Getting that wrong double-counts the brightest lights in
    /// every room in the game.
    #[test]
    fn the_local_lights_and_the_ambient_cube_partition_the_lights() {
        let mut fixture = crate::engine::trace::fixture::Fixture::default();
        // Nothing to block anything, so every light reaches the point.
        fixture.add_box(
            Vec3::new(-4096.0, -4096.0, -4096.0),
            Vec3::new(-4000.0, -4000.0, -4000.0),
            Contents::SOLID,
            true,
        );
        let world = fixture.single_leaf();

        // `WORLD_LIGHTS` + 1 point lights of decreasing brightness, all the
        // same distance away along a different axis each.
        let places = [Vec3::X, Vec3::Y, Vec3::Z, -Vec3::X, -Vec3::Y];
        let lights: Vec<WorldLight> = places
            .iter()
            .take(WORLD_LIGHTS + 1)
            .enumerate()
            .map(|(i, axis)| {
                let brightness = 10000.0 / (i + 1) as f32;
                world_light(emit::POINT, *axis * 64.0, [brightness; 3])
            })
            .collect();
        let cache = cache_with(lights);

        let lit = cache.lighting_at(&mut world.tracer(), Vec3::ZERO);
        assert_eq!(lit.count as usize, WORLD_LIGHTS, "every slot is taken");
        // The brightest lights took the slots, in order.
        for (slot, axis) in places.iter().enumerate().take(WORLD_LIGHTS) {
            let expected = *axis * 64.0;
            assert_eq!(
                lit.lights[slot].position,
                [expected.x, expected.y, expected.z, 1.0],
                "slot {slot}"
            );
        }
        // And the one that missed is in the cube, on the face pointing at it —
        // which is the `-x` face, because the light that missed is at `+z`
        // for `WORLD_LIGHTS == 2` and the cube face it lights is `+z`.
        let missed = places[WORLD_LIGHTS];
        let face = BOX_DIRECTIONS
            .iter()
            .position(|axis| axis.dot(missed) > 0.0)
            .expect("the axis the loser sits on");
        assert!(lit.ambient_cube[face][0] > 0.0, "{:?}", lit.ambient_cube);
        // The faces the winners sit on gained nothing, because a light with a
        // slot returns before the cube is touched.
        let winner = BOX_DIRECTIONS
            .iter()
            .position(|axis| axis.dot(places[0]) > 0.0)
            .expect("the axis the brightest light sits on");
        assert_eq!(lit.ambient_cube[winner][0], 0.0);
    }

    /// A skylight is decided by a trace at the sky texture, and by nothing
    /// else — no radius, no falloff.
    #[test]
    fn a_skylight_reaches_a_point_that_can_see_a_sky_surface() {
        let mut fixture = crate::engine::trace::fixture::Fixture::default();
        // A ceiling of sky over the left half of the world and solid rock over
        // the right.
        fixture.add_surfaced_box(
            Vec3::new(-4096.0, -4096.0, 1024.0),
            Vec3::new(0.0, 4096.0, 1088.0),
            Contents::SOLID,
            surf::SKY,
        );
        fixture.add_box(
            Vec3::new(0.0, -4096.0, 1024.0),
            Vec3::new(4096.0, 4096.0, 1088.0),
            Contents::SOLID,
            true,
        );
        let world = fixture.single_leaf();

        let mut sun = world_light(emit::SKYLIGHT, Vec3::new(0.0, 0.0, 8192.0), [0.6; 3]);
        // Shining straight down, so the trace goes straight up.
        sun.normal = [0.0, 0.0, -1.0];
        let cache = cache_with(vec![sun]);

        let outdoors = cache.lighting_at(&mut world.tracer(), Vec3::new(-512.0, 0.0, 0.0));
        assert_eq!(outdoors.count, 1, "under the sky");
        // **The register holds the direction the light *shines*, not the one
        // it is seen from** — `m_Direction = pWorldLight->normal`
        // (`l_studio.cpp:252`), and the shader negates it
        // (`mix( point_dir, -light.direction.xyz, color.w )`). Writing the
        // reverse here lights every outdoor map from underneath.
        assert_eq!(outdoors.lights[0].direction, [0.0, 0.0, -1.0, 0.0]);
        assert_eq!(outdoors.lights[0].color[3], 1.0, "directional");

        let indoors = cache.lighting_at(&mut world.tracer(), Vec3::new(512.0, 0.0, 0.0));
        assert_eq!(indoors.count, 0, "under rock");
        assert_eq!(indoors.ambient_cube, [[0.0; 4]; AMBIENT_CUBE_FACES]);
    }

    // ----------------------------------------------------------------------
    // Against the shipped game
    // ----------------------------------------------------------------------

    /// Every shipped map's world lights read, and every static prop in the
    /// game lit by them.
    ///
    /// Ignored by default and gated on `KISAK_GAME_DIR`, like the rest of the
    /// depot tests. Run with:
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_map_lights -- --ignored --nocapture
    /// ```
    ///
    /// Four things only the real data can say:
    ///
    /// - the lump's **stride** is right, because `records` refuses a ragged
    ///   lump and 106 maps go through it;
    /// - the **filters** keep the share the census predicts, so a mistake in
    ///   the flag or the style test shows up as a count rather than as a
    ///   brightness;
    /// - no prop comes out with a `NaN` anywhere in its lighting, which is the
    ///   failure mode every guarded division in this module exists for;
    /// - and what it **costs**, which is the number that decides whether the
    ///   PVS reject the module docs leave out has to be written after all.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_map_lights_its_props() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = crate::filesystem::Vfs::mount_game(&dir, &base, &Default::default())
            .expect("mount the game");

        let mut names: Vec<String> = vfs
            .list("maps")
            .expect("maps/")
            .into_iter()
            .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
            .map(|e| e.name.trim_end_matches(".bsp").to_owned())
            .collect();
        names.sort();
        assert!(names.len() > 50, "only {} maps found", names.len());

        let (mut lights, mut kept, mut hdr) = (0usize, 0usize, 0usize);
        let mut by_type = [0usize; 6];
        let (mut props, mut props_lit) = (0usize, 0usize);
        let mut by_count = vec![0usize; MAX_LIGHTS + 1];
        let mut cube_energy = 0.0f64;
        let mut elapsed = std::time::Duration::ZERO;

        for name in &names {
            let bsp = crate::engine::world::bsp::Bsp::load(&vfs, name).expect("a shipped map");
            lights += bsp.world_lights.len();
            hdr += usize::from(bsp.world_lights_are_hdr);
            for light in &bsp.world_lights {
                if let Some(slot) = by_type.get_mut(light.emit_type as usize) {
                    *slot += 1;
                } else {
                    panic!("{name}: emit type {}", light.emit_type);
                }
            }

            let cache = LightCache::from_bsp(&bsp);
            kept += cache.lights().len();
            let collision = crate::engine::trace::CollisionBsp::build(&bsp);
            let placements = crate::engine::world::props::Props::load(name, &bsp)
                .unwrap_or_else(|e| panic!("{name}: {e}"));

            let started = std::time::Instant::now();
            let mut tracer = collision.tracer();
            let lit: Vec<ModelLighting> = placements
                .instances
                .iter()
                .map(|prop| cache.lighting_at(&mut tracer, prop.lighting_origin))
                .collect();
            elapsed += started.elapsed();

            props += lit.len();
            for state in &lit {
                by_count[state.count as usize] += 1;
                props_lit += usize::from(state.count > 0);
                for face in state.ambient_cube {
                    for channel in &face[..3] {
                        assert!(channel.is_finite() && *channel >= 0.0, "{name}: {face:?}");
                        cube_energy += f64::from(*channel);
                    }
                }
                for light in &state.lights[..state.count as usize] {
                    for value in bytemuck::bytes_of(light).chunks_exact(4) {
                        let value = f32::from_le_bytes(value.try_into().expect("4 bytes"));
                        assert!(value.is_finite(), "{name}: {light:?}");
                    }
                }
            }

            if name == "sp_a1_intro1" {
                // The reference map, whose numbers the docs quote.
                assert_eq!(bsp.world_lights.len(), 43, "sp_a1_intro1 world lights");
                assert_eq!(cache.lights().len(), 39, "after the two filters");
                let with = lit.iter().filter(|state| state.count > 0).count();
                println!(
                    "sp_a1_intro1: {with} of {} props take a local light",
                    lit.len()
                );
                assert!(with > 0, "the default map lights nothing");
            }
        }

        println!(
            "{} maps, all HDR: {}; {lights} world lights ({kept} static, {} filtered out)",
            names.len(),
            hdr == names.len(),
            lights - kept,
        );
        println!(
            "  by type: {} surface, {} point, {} spot, {} sky, {} quake, {} skyambient",
            by_type[0], by_type[1], by_type[2], by_type[3], by_type[4], by_type[5],
        );
        println!(
            "  {props} props, {props_lit} with a local light; counts {by_count:?}; \
             {:.0} ms to light them all, mean cube channel {:.4}",
            elapsed.as_secs_f64() * 1000.0,
            cube_energy / (props * 18).max(1) as f64,
        );

        // A light that misses a slot lands in the cube and one that wins does
        // not, so "no prop anywhere took a local light" is the shape of a
        // filter that rejected everything.
        assert!(props_lit > props / 10, "{props_lit} of {props} props lit");
        assert_eq!(hdr, names.len(), "Portal 2 ships HDR world lights");
        // `emit_quakelight` is Quake's and `vrad` never writes one.
        assert_eq!(by_type[4], 0, "quake lights");
        for count in by_count.iter().skip(WORLD_LIGHTS + 1) {
            assert_eq!(*count, 0, "more lights than {WORLD_LIGHTS} slots");
        }
    }
}
