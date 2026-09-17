# `src/studio/` and `src/engine/world/props/` — API reference

Studio models (`.mdl` / `.vvd` / `.dx90.vtx` / `.vhv`), the static props a map
places, and the **animated models its entities place**.
Two modules, because the *asset* and the *instance* have different lifetimes:
`studio/` reads files and needs no map; `world/props/` and
`world/entities/` place them and die with one. Design and the measurements
behind the scoping: `portdocs/STUDIO.md`.

| Stage (`portdocs/STUDIO.md` §8) | What | Status |
|---|---|---|
| 1 | the three readers and the join | **done**, verified against all 2,041 shipped models |
| 2 | the `sprp` game lump | **done**, verified against all 106 shipped maps |
| 3 | draw them | **done** — 1,080 props draw in `sp_a1_intro1` |
| 4 | the `.bsp` pak lump and `.vhv` per-vertex light | **done** |
| 5 | the leaf ambient cube | **done** |
| 6 | LOD selection and fade | **not started** (optional) |
| — | bones, sequences and animation | **done** for rigid models — `studio/anim.rs`, below |

Not implemented and not planned here: `.phy` collision (that is
`ENGINE_TRACE.md` stage 5), the prop leaf lists as a *visibility* structure
(read and kept, unused), decals, flexes and sub-d surfaces — and **skinning**,
which `anim.rs` deliberately substitutes a per-bone draw split for. See "What
is deliberately absent" below.

---

## Quick start

```rust
use crate::studio::StudioModel;
use crate::engine::world::props::{Props, PropModels};

// One model, as an asset. Needs a Vfs and nothing else.
let model = StudioModel::load(vfs, "models/props_bts/gantry_rails_a.mdl")?;
println!("{} triangles in {} batches", model.triangle_count(), model.batches.len());

// A map's props. `World::load` already does all of this.
let mut props = Props::load("sp_a1_intro1", &bsp)?;   // the sprp lump
props.light(&bsp, &collision);                        // the baked ambient cubes
let models = PropModels::load(vfs, materials, device, &props); // upload
// …later, inside an open pass:
models.draw(&mut pass, &props);
```

---

## `src/studio/` — the asset

### `StudioModel`

```rust
pub struct StudioModel {
    pub path: String,            // models/props_bts/gantry_rails_a.mdl
    pub name: String,            // studiohdr_t::name — the compiler's, not always the path
    pub bounds: (Vec3, Vec3),    // view_bbmin / view_bbmax, model space
    pub illum_position: Vec3,    // where lighting is sampled by default
    pub flags: StudioFlags,
    pub checksum: u32,           // shared by all three files; also what a .vhv must match
    pub vertices: Vec<ModelVertex>,
    pub indices: Vec<u32>,
    pub batches: Vec<Batch>,
    pub bones: Vec<anim::Bone>,
    pub sequences: Vec<anim::Sequence>,
    pub animations: Vec<anim::Animation>,
}

impl StudioModel {
    pub fn sequence(&self, label: &str) -> Option<usize>;   // LookupSequence
    pub fn animation(&self, sequence: usize) -> Option<&anim::Animation>;
    /// Which bone moves each vertex — `Some` only if EVERY vertex answers to
    /// exactly one. See gotcha 9.
    pub fn rigid_bones(&self) -> Option<&[u8]>;
}

pub struct BoneRun { pub bone: u16, pub first_index: u32, pub index_count: u32 }

impl StudioModel {
    pub fn load(vfs: &Vfs, name: &str) -> Result<StudioModel, StudioError>;
    pub fn triangle_count(&self) -> usize;
}

pub struct Batch {
    pub material: String,        // as MaterialCache::load wants it
    pub first_index: u32,
    pub index_count: u32,
    pub body_part: u16,
    pub model: u16,
}
```

`load` accepts the name with or without the `.mdl` extension and normalises
backslashes and case, because the `sprp` dictionary stores the extension and
most other callers do not.

One vertex buffer and one index buffer per model, sliced by material. `.vtx`
LOD 0 only.

### `anim` — bones, sequences and the pose

```rust
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,   // always a LOWER index than this bone's
    pub pos: Vec3,               // the bind pose, and what an animated position adds to
    pub quat: Quat,
    pub rot: Vec3,               // the bind pose again, as a RadianEuler
    pub pos_scale: Vec3,         // the fixed-point scale an RLE channel is multiplied by
    pub rot_scale: Vec3,
    pub flags: u32,
    pub pose_to_bone: Mat4,      // the INVERSE BIND matrix
}

pub struct Sequence { pub label: String, pub flags: u32, pub anim: usize }
pub struct BoneTrack { pub bone: usize, pub pos: Vec<Vec3>, pub rot: Vec<Quat> }
pub struct Animation {
    pub name: String, pub fps: f32, pub flags: u32,
    pub frame_count: usize, pub tracks: Vec<BoneTrack>,
}
impl Animation { pub fn duration(&self) -> f32; }

pub const STUDIO_LOOPING: u32 = 0x0001;

/// R_StudioSetupBones + ComputePoseToWorld, in model space.
pub fn pose(bones: &[Bone], anim: Option<&Animation>, cycle: f32) -> Vec<Mat4>;
```

`bonesetup/bone_decode.cpp` plus the slice of `studiorender/r_studio.cpp`'s
`R_StudioSetupBones` that a model with one animation, no layers, no pose
parameters and no IK needs.

**The RLE stream is expanded at load, not walked at draw.** Valve keeps it and
decodes per frame because a `.mdl` can carry hundreds of long sequences; the
four button models carry 4 or 5 sequences of at most 11 frames over 2 to 4
bones, so a whole model's expanded animation is a few hundred bytes and `pose`
is two lookups and a blend.

**This one is bounded too.** The game holds 5,434 sequences and its longest
animation is **4,050 frames** — about a megabyte expanded, for one model.
Nothing a class loads today is near that; the first one that is, is the
condition for going back to Valve's lazy walk.

`pose` returns **pose-to-model** matrices: `boneToWorld[i] * poseToBone[i]`,
which is what a *bind-pose* vertex is multiplied by. With no animation every
one of them is the identity, which is what lets a static prop and an animated
model share one draw path.

### The three readers

`Mdl::parse`, `Vvd::parse` / `Vvd::parse_lod`, `Vtx::parse` — each takes an
owned `path` (for error messages) and a byte slice, so every format is testable
without a `Vfs`. `assemble(&mdl, &vvd, &vtx, resolve)` joins them, taking a
closure that turns a texture slot's candidate paths into the one that exists;
`StudioModel::load` passes `Vfs::exists`.

`StudioError` covers `Read`, `TooShort`, `BadIdent`, `Version`,
`ChecksumMismatch` and `Corrupt`. **There is no error fallback for geometry** —
unlike a material or a texture, a model that fails to load has no magenta
equivalent, so this is a `Result` all the way out and the caller decides.

### `src/engine/world/props/` — the instances

```rust
pub const GAMELUMP_STATIC_PROPS: u32 = 0x7370_7270;  // 'sprp'

pub struct Props {
    pub models: Vec<String>,       // the dictionary — distinct models
    pub leaves: Vec<u16>,          // the flat leaf list Prop::leaves slices
    pub instances: Vec<Prop>,
    pub lighting: Vec<ModelLighting>,  // parallel to instances; filled by `light`
}

impl Props {
    pub fn load(map: &str, bsp: &Bsp) -> Result<Props, PropLumpError>;
    pub fn from_lump(lump: &StaticPropLump) -> Result<Props, PropLumpError>;
    pub fn light(&mut self, bsp: &Bsp, collision: &CollisionBsp);
}

pub struct Prop {
    pub model: String,
    pub model_index: usize,        // into Props::models
    pub transform: Mat4,
    pub lighting_origin: Vec3,
    pub flags: PropFlags,
    pub skin: i32,
    pub fade: (f32, f32, f32),
    pub diffuse_modulation: [u8; 4],
    pub leaves: Range<usize>,
}
```

`StaticPropLump::parse(map, &game_lump)` is the raw decode (`StaticProp` rows as
written); `Props` is the resolved form. `Bsp::game_lump(id)` finds the lump.

```rust
pub struct PropModels { pub stats: PropModelStats, /* … */ }

impl PropModels {
    pub fn load(vfs, materials, device, props: &Props) -> PropModels;  // cannot fail
    pub fn draw(&self, pass: &mut Pass<'_>, props: &Props);
    pub fn get(&self, index: usize) -> Option<&PropModel>;
    pub fn summary(&self) -> String;
}
```

`PropModels::load` **cannot fail**: a prop whose model is missing is a prop that
does not draw, which is what `CStaticPropMgr` does too. The reason is on stderr,
once per model, and the count is in `stats`.

### Lighting

A prop is lit by two things, and both are baked:

```rust
// The leaf ambient cube — `Mod_LeafAmbientColorAtPos`, inverse-squared-distance
// weighted over the leaf's samples.
pub fn ambient_at(bsp: &Bsp, collision: &CollisionBsp, position: Vec3) -> AmbientCube;
pub fn lighting_for(bsp: &Bsp, collision: &CollisionBsp, position: Vec3) -> ModelLighting;

// The per-vertex bake — one `.vhv` per placement, out of the map's pak lump.
pub struct Vhv { pub checksum: u32, pub vertex_count: u32, pub meshes: Vec<VhvMesh> }
impl Vhv {
    pub fn parse(path: String, bytes: &[u8]) -> Result<Vhv, StudioError>;
    pub fn lod_meshes(&self, lod: u32) -> impl Iterator<Item = &VhvMesh>;
    pub fn colors(&self, bytes: &[u8], lod: u32, meshes: &[HardwareMesh], vertex_count: usize)
        -> Option<Vec<StaticLightVertex>>;
}
pub fn prop_lighting_path(index: usize, hdr: bool) -> String;  // sp_hdr_<i>.vhv
```

`PropModels::load` reads every instance's `.vhv` into **one** `VertexBuffer` for
the whole map and slices it per prop; `ModelLighting::static_light` is set per
instance, because whether a prop got a file is per instance.

Local lights are **not** ported — that needs `LUMP_WORLDLIGHTS` and the
attenuation model — so beyond the ambient cube and the vertex bake a prop has no
lighting.

---

## Cross-cutting semantics

**The two-step vertex indirection.** A `.vtx` index names an entry in its strip
group's vertex table; that entry's `origMeshVertID` is relative to the **mesh**;
the mesh's `vertexoffset` is relative to its **model**; the model's
`vertexindex` is relative to the **`.vvd` pool**. `vtx.rs` collapses the first
step, `build.rs` the other two. A `StudioModel`'s indices are already flat.

**Every `.mdl` offset is relative to the struct holding it**, not to the file —
that is what all of Valve's `(byte *)this + index` accessors encode. Every
offset is resolved once at parse time into owned `Vec`s; the file bytes are
dropped.

**The `.vvd` fixup table** reorders an LOD-sorted vertex pool into mesh order.
`Vvd::parse` applies it; vertices and tangents are permuted in lockstep.

**Draw order.** `World::draw` draws brush faces first, then props, in one opaque
pass. Props are walked **model-major**, so each model's buffers and pipelines
bind once rather than once per instance.

---

## Invariants and gotchas

Ordered by how likely each is to bite. **13-16 are the animation's.**

1. **`sizeof(StaticPropLumpV9_t)` is 72, not 69.** The prop structs in
   `gamebspfile.h` are the only ones on this path *not* `#pragma pack(1)`, so
   the compiler adds three bytes of tail padding and the file inherits it.
   Valve's reader gets it free from `sizeof`; a hand-written one does not. At 69
   the first prop reads correctly and every later one drifts three bytes further
   off — a map full of props in almost the right places. `lump.rs` asserts the
   stride against the lump's own length rather than assuming it.

2. **The ambient cube decodes with `ColorRGBExp32ToVector`, the lightmap with
   `TexLightToLinear`** — the two differ by exactly 255 and this port needs the
   *opposite* rule from the one `rustdocs/MATERIALS.md` states for lightmaps.
   Measured, not assumed: over `sp_a1_intro1`, mean luminance under
   `TexLightToLinear` is 0.0249 for the lightmap and 0.0002 for the ambient
   cubes. Use `ColorRgbExp32::to_vector` here and `to_linear` there; getting it
   backwards makes every prop black.

3. **A `.vhv` is in *hardware* vertex order, not `.vvd` pool order.** Valve's
   runtime compacts a model's vertices per LOD — `studiomeshgroup_t` holds
   exactly the vertices that LOD's strips reference, in the order the `.vtx`
   strip-group tables list them — and `vrad` writes against that numbering.
   This port does not compact, so the two differ whenever a lower LOD uses a
   subset of the pool: `models/props_destruction/framework_dest_01` has 9,434
   pool vertices and 6,703 hardware vertices at LOD 0. `HardwareMesh` carries
   the mapping and `Vhv::colors` scatters through it. Reading the block as a
   run mislights 125 of `sp_a1_intro1`'s 1,080 props **and appears to work for
   the other 955**, because a single-LOD model's table is usually the identity.

4. **`vrad` writes no `.vhv` block for an empty mesh**, so a model's empty
   meshes have to be dropped before the two lists are matched — eight meshes
   and five blocks on `models/npcs/turret/turret_debris_lrg`. Matching them
   without dropping shifts every later block onto the wrong mesh.

5. **The `.vhv` checksum is counted, not enforced.** `r_ignoreStaticColorChecksum`
   defaults to 1 (`l_studio.cpp:117`) and the shipped data needs it to: 24 of
   the game's 56,801 `.vhv` files carry a checksum that is not their model's,
   and Portal 2 draws those props lit. The per-mesh vertex count is the check
   that actually protects against another model's colours, and that one is
   enforced.

6. **A `QAngle` is pitch, yaw, roll — not x, y, z** — and the composition is
   `Rz(yaw) · Ry(pitch) · Rx(roll)` (`mathlib_base.cpp:1329`'s own comment is
   `matrix = (YAW * PITCH) * ROLL`). `StaticProp::rotation` builds it from three
   explicit axis rotations rather than `Mat3::from_euler`, because every
   `EulerRot` variant encodes an intrinsic/extrinsic convention as well as an
   order and the wrong one is a silent half-right answer: props with only a yaw
   look correct and every tilted one does not. Pitch rotates `+X` towards
   `-Z`, not `+Z`.

7. **A leaf with zero ambient samples and a non-zero `first_sample` is a *solid*
   leaf, and `first_sample` is a leaf index** — not a sample index. `vrad`
   writes it so a prop embedded in geometry borrows a neighbour's lighting.
   The field means two different things depending on the count beside it and
   nothing in the lump says so. Read it the other way and every such prop is
   black.

8. **`m_LightingOrigin` is meaningless without
   `STATIC_PROP_USE_LIGHTING_ORIGIN`.** Without the flag it holds whatever the
   compiler left there. `Props::from_lump` falls back to the prop's origin.

9. **`.vtx` triangles are emitted with their winding reversed**, exactly as
   `world/`'s fans are, and for the same reason: Valve's `D3DCULL_CCW` under a
   Y-up framebuffer and this port's `front_face: Ccw` under `wgpu`'s Y-down one
   name opposite sets of triangles. The fix, if ever made, is one `front_face`
   in `PipelineCache` **and the deletion of both reversals**.
   `rustdocs/ENGINE.md` gotcha #1.

10. **A prop's index buffer is 32-bit.** `models/stars/allstars.mdl` is a static
   prop with 187,676 vertices, so `IndexBuffer::new_u32` exists and `IndexSlice`
   carries its format. The world path stays 16-bit.

11. **`OptimizedModel::Vertex_t::origMeshVertID` is at byte 4, not 5**, and
   `FileHeader_t`'s fields are not evenly spaced (`maxBonesPerStrip` and
   `maxBonesPerFace` are `unsigned short`, so `checkSum` lands at 16). Both were
   wrong in the first draft of this reader and **both passed every synthetic
   test**, because the fixture had been written from the same misreading. They
   were caught only by parsing the real depot. Write a format fixture from the
   header, never from the reader.

13. **Bone 255 terminates an animation's bone chain — it is not a bone.**
   `studiomdl` writes a link with `bone = 255` after the last real one
   (`utils/studiomdl/write.cpp:1182`) and the decoder's loop is
   `while (panim && panim->bone < 255)` (`bone_decode.cpp:1395`). Reading it as
   an index refuses models that are perfectly valid — it refused 15 of the ones
   `sp_a1_intro1` places, and every one of them still parsed as a *file*, so
   nothing but loading the real game showed it.

14. **A `RadianEuler` is `(roll, pitch, yaw)` in radians; a `QAngle` is
   `(pitch, yaw, roll)` in degrees.** `AngleQuaternion` has two overloads with
   different component orders and Valve's own comment says *"p, y, r are not in
   the same locations in QAngle + RadianEuler. Yay!"*. An animation's angles are
   `RadianEuler`s, so `crate::math::angle_matrix` is the **wrong** function for
   them: it would give a plausible rotation about the wrong axis.

15. **An animated rotation is added to the bind pose in *Euler* space, before
   it becomes a quaternion**, and two frames are then blended as quaternions.
   Interpolating the angles and interpolating the quaternions are not the same
   operation, and `CalcBoneQuaternion` does the second of them over values
   produced by the first. The blend itself is `QuaternionBlend` — align, then a
   **normalized lerp, not a slerp**.

16. **`pose` needs `Bone::pose_to_bone`, and skipping it is not subtle.**
   `poseToBone` is the *inverse bind* matrix and `R_StudioSetupBones` ends in
   `ConcatTransforms( boneToWorld[i], poseToBone[i], poseToWorld[i] )`. Without
   it every bone applies its own bind transform twice and the model turns
   inside out — and with no animation playing, *with* it every matrix is the
   identity, which is the property that lets one draw path serve both.

   **What does not follow is that the bind pose is a safe default.** `pose`'s
   `anim: None` is the bind pose, and that is the right answer only for a model
   with no sequences at all — never for "I could not resolve which sequence".
   `m_nSequence` starts at zero, so real content is always posed by sequence 0,
   and **an artist never looks at the bind pose**:
   `props_motel/hotel_container_furniture01`-`03` bind at `rot_x(+90)` with a
   `poseToBone` of `rot_x(-90)` — identity, as above — while their one sequence
   holds a 120° turn about `(1,1,1)`, so the two are a quarter turn apart and
   the bind pose stands `sp_a1_intro1`'s furniture inside the bed. A quick way
   to spot a model like that without rendering it: its `hull_min`/`hull_max`
   and its `.vvd` vertex bounds will not agree.

17. **A material naming a brush shader cannot draw a prop**, and vice versa. A
   prop's geometry is `ModelVertex` and nothing else, so `PropModels::load`
   substitutes `MaterialCache::error_model_material()` — a second checkerboard
   under `VertexLitGeneric`, because this port picks the vertex layout per
   shader where Valve picked it per `$model` flag.

13. **`Prop::leaves` is a PVS structure and is not used yet.** Every prop is
    drawn every frame.

14. **`TRANSLUCENT_TWOPASS` models draw unsorted.** 38 of Portal 2's models set
    it and this port has no sorted translucent pass.

---

## What is deliberately absent

- **Skinning** — replaced rather than deferred, and `prop_dynamic` has now put
  a price on it. A model is drawn
  one [`BoneRun`](#studiomodel) at a time, each under its own bone's matrix,
  which is **exact** when every vertex answers to exactly one bone and needs no
  change to the vertex format, the shaders or the bind groups. A
  `prop_floor_button`'s 7,929 vertices split 7,263 on the body and 666 on the
  plate, and every model the port drew until `prop_dynamic` was like that.

  **It does not generalise, and the number is known.** Across the game 420 of
  2,017 models have more than one bone and **141 of those share a vertex
  between two** — the `a4_destruction` set, Wheatley's chamber falling apart.
  `StudioModel::rigid_bones` is where the precondition is checked rather than
  assumed; a model that fails it is drawn in its **bind pose** and counted.

  > **`prop_dynamic` turned that from a bound into a bill.** It is the first
  > class that places models the map chose rather than models the compiler
  > placed, so it reaches them: **74 of the 591 readable models the game's
  > props name share a vertex between bones, and 290 entities wear one**. Seven
  > of the 74 are on `sp_a1_intro1` — the `models/container_ride/finedebris_part*`
  > set — so the substitution is visible on the default map rather than only in
  > a census, and the startup log says so per model.
  > `server::tests::every_shipped_prop_dynamic_plays_the_animation_its_map_asks_for`
  > pins both numbers. It is the second-largest gap in this module, behind
  > `$includemodel`'s 926 entities.
- **Everything in `bone_setup.cpp` that blends** — ~5,000 lines of layering,
  pose parameters, IK, procedural bones, bone controllers and blend sequences.
  A sequence here has one animation (`numblends` is 1 for every sequence in
  every model the port loads), there are no pose parameters, and switching
  sequences is a cut — which is what `ResetSequence` means and what the only
  caller does.
- **External `.ani` animation blocks** (`animblock != 0`), `STUDIO_FRAMEANIM`'s
  frame-major encoding, `STUDIO_ALLZEROS`, delta animations, zero-frame spans
  and `BONE_FIXED_ALIGNMENT`'s `QuaternionAlign`. Each is detected and skipped —
  the animation reads as empty, which holds the bind pose — rather than
  mis-read. 68 files in the game have an `.ani` and no model the port loads is
  among them.
- **`$includemodel`** — `studiohdr_t::numincludemodels` /
  `includemodelindex`, and the `virtualmodel_t` that
  `CStudioHdr::ResolveIncludedModels` builds out of it: a model whose sequences
  and animations live in a *different* `.mdl`, merged in at load with each
  included bone remapped onto the host's by **name**. `StudioModel::load`
  ignores the list, so such a model has only its own local sequences.

  > **Measured, and it is the largest gap in this module.** Across the 606
  > models the shipped game's `prop_dynamic`s name, **9 do this — and 926
  > entities wear one**, the `models/anim_wp/room_transform` panel-arm set
  > chief among them. Their `DefaultAnim` and `SetAnimation` labels resolve to
  > nothing, so they draw in their bind pose and their `AnimThink` cancels
  > (`rustdocs/SERVER.md` gotcha 64). Of the game's 2,416 `DefaultAnim` keys,
  > 849 resolve **only** through an include.
  >
  > The work is the bone remap, not the parse: an included animation's
  > `BoneTrack::bone` indexes the *included* model's bone list, so merging
  > without remapping poses the wrong bones — which would be a silently wrong
  > picture rather than an error.
- **Flexes and sub-division surfaces** — absent from the *static prop* data.
  Every strip group a static prop uses is `STRIPGROUP_IS_HWSKINNED` with no
  `STRIPGROUP_IS_DELTA_FLEXED`, every strip is `STRIP_IS_TRILIST`, and
  `StripHeader_t::numBones` is 0 throughout. The readers refuse flex deltas and
  quad lists rather than drawing them wrong, which is why 16 of the game's
  *animated* `props_destruction` models are refused.

  > **Those 16 stopped being academic when `prop_dynamic` landed.** They are
  > all `models/props_destruction/toxin*`, and **15 of them are placed as
  > `prop_dynamic`s, by 41 entities across the shipped maps** — so 41 entities
  > that the shipped game draws as toxin pipework draw nothing here. The claim
  > above is still exactly true and is about static props; `prop_dynamic` is
  > the first thing in the port that places a model which is not one.
  > `server::tests::every_shipped_prop_dynamic_plays_the_animation_its_map_asks_for`
  > pins the number. It is the smaller of the two studio gaps that class
  > measured — `$includemodel` is 9 models and 926 entities.
- **`CMDLCache`'s cache management** — LRU eviction, memory budgets, async
  queues, lock/unlock refcounting, `CreateThinVertexes`. All of it existed to
  fit models into a 2007 console; a `StudioModel` is an owned value and dropping
  it frees it.
- **The local lights** on a prop (`LightcacheGetStatic`'s `dworldlight_t` walk).
- **The 3-stream `.vhv`** (`r_staticlight_streams` 3, the cascaded-shadow
  path). Every shipped file is `m_nVertexSize` 4, so a wider one is refused
  rather than misread.
- **LZMA-compressed `.vhv`** and compressed pak entries. Both are X360-only;
  all 64,428 pak entries in the shipped game are stored.

---

## Extending it

- **LOD** — `Vvd::parse_lod` already takes a root LOD and `Vtx` keeps every
  LOD's indices, so selection is a parameter and a `switchPoint` comparison, not
  a rewrite. 819 of 968 models have one LOD, so this is performance, not
  correctness.
- **Skinned models** — `vvd::Vertex` grows a `bones` field, `vtx` stops
  discarding `StripHeader_t`'s bone plumbing, and `mdl` reads the bone array it
  currently counts and skips.
- **Skin families** — `Prop::skin` is parsed and ignored; `mdl` reads
  `numskinref`/`skinindex` but does not resolve them.
- **Culling** — `PropModel::bounds` is already in hand for it.

---

## Which tests guard what

| Test | Guards |
|---|---|
| `studio::tests::struct_strides_match_the_shipped_files` | the `.mdl` strides (a wrong one reads plausible nonsense) |
| `studio::tests::a_mesh_vertex_offset_shifts_its_indices` | the mesh half of the indirection |
| `studio::tests::a_model_base_offsets_into_the_pool` | the model half |
| `studio::tests::the_fixup_table_reorders_the_vertex_pool`, `fixups_permute_the_tangents_with_the_vertices` | the `.vvd` fixup |
| `studio::tests::materials_resolve_through_the_cdtexture_cross_product` | material resolution order |
| `studio::tests::quad_lists_and_flex_deltas_are_refused` | the refusals §3 justifies |
| `studio::tests::a_stale_companion_file_is_refused` | the checksum guard |
| `studio::anim::tests::a_radian_euler_is_roll_pitch_yaw_and_not_a_qangle` | gotcha 14 |
| `studio::anim::tests::the_rle_walk_repeats_the_last_valid_value` | `ExtractAnimValue`'s run encoding |
| `studio::anim::tests::a_compressed_quaternion_rebuilds_its_w` | `Quaternion48`/`Quaternion64` |
| `studio::anim::tests::the_blend_aligns_and_normalizes` | gotcha 15's blend half |
| `studio::anim::tests::halves_decode` | `Vector48` |
| `studio::anim::tests::the_bind_pose_is_the_identity` | gotcha 16 |
| `studio::anim_depot_tests::the_floor_button_model_animates` | **the decoder, against the real `portal_button.mdl`** — bones, sequences, 7.29 units of plate travel, and `up` retracing `down` |
| `engine::world::entities::tests::the_button_draws_and_moves_as_it_presses` | **the whole path, on real pixels** — the model on screen, and the image changing as it presses |
| `props::tests::the_second_prop_lands_on_the_seventy_two_byte_boundary` | gotcha 1 |
| `props::tests::a_stride_that_is_not_seventy_two_is_refused` | the stride assertion |
| `props::tests::valves_angle_order_is_yaw_then_pitch_then_roll` | gotcha 3, against `AngleMatrix` evaluated by hand |
| `props::tests::the_lighting_origin_needs_its_flag` | gotcha 5 |
| `props::light::tests::a_solid_leaf_borrows_its_neighbours_samples` | gotcha 4 |
| `props::light::tests::the_cube_decodes_the_ambient_way_and_not_the_lightmap_way` | gotcha 2 |
| `props::light::tests::the_nearer_sample_dominates` | inverse-*squared* weighting |
| `studio::vhv::tests::colours_follow_the_hardware_vertex_order` | gotcha 3 — the one that fails silently |
| `studio::vhv::tests::empty_meshes_are_not_written_and_must_not_be_matched` | gotcha 4 |
| `studio::vhv::tests::a_file_that_does_not_describe_the_model_is_refused` | the shape check gotcha 5 relies on |
| `materials::preview::tests::one_model_can_be_drawn_under_two_static_light_streams` | the per-placement stream, on real pixels |
| `filesystem::mount::pak::tests::*` | the ZIP reader, against a spec-derived fixture |

Two tests are `#[ignore]`d and gated on `KISAK_GAME_DIR`, because the depot is
not in this repository and the rest of the suite deliberately needs no game
files:

```text
KISAK_GAME_DIR=/path/to/portal2 cargo test --release -- --ignored --nocapture
```

- `studio::tests::every_shipped_studio_model_parses` — 2,041 models, of which
  2,017 load (all 1,444 flagged `STATIC_PROP`), 8 ship without companions and
  16 are animated flex-delta models refused by design. **This is the test that
  found gotcha 8.**
- `props::tests::every_shipped_map_places_its_props` — 106 maps, 104 with props,
  56,955 props placed; asserts `sp_a1_intro1`'s measured 1,080 props from 136
  models, prints the luminance comparison behind gotcha 2, and checks that
  **all 56,801 `.vhv` files in the game describe the model they are for**.
  That last one is what found gotchas 3 and 4.
- `engine::world::bench::tests::frame_cost` is not a test but a stopwatch: it
  loads a real map and times the CPU cost of recording a frame, with no window
  in the way. Use it before and after any change to the draw path.

  It reports five figures, and on `sp_a1_intro1` they are **0.25 ms of world
  brushes, 0.10 of brush models, 1.01 of static props, 0.72 of entity models
  and 1.86 for the lot** (2.14 with the refracting pass). Run them on their own
  — back to back they share thermal state and read 2-3x high.

  > **It spawns a `Server`**, which is the only reason a benchmark in `world/`
  > names a `server/` type: `World::load` cannot read the models an entity
  > places, because they are named by the entity lump it has just parsed. Until
  > `prop_dynamic` that omission was invisible, because the only model a game
  > entity placed was one floor button; now it is 91 instances and 355,469
  > triangles on the default map — more than all 1,080 static props — so a
  > stopwatch that skipped them would be measuring the wrong frame.
