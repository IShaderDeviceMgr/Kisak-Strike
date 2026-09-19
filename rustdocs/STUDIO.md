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
| — | bones, sequences and animation | **done** — `studio/anim.rs`, below |
| — | `$includemodel` | **done** — `studio/include.rs`, below; 25 shipped models declare one, 9 of them worn by 926 `prop_dynamic`s |
| — | attachment points | **done** — `Attachment`, `bone_to_model`, `attachment_to_model`; 266 shipped models carry 1,952 points. `src/server/attachment.rs` is what asks |
| — | **skinning** | **done** — `anim::skin`, `ModelVertex::bone_weights`/`bone_indices`, and the GPU palette in `materials::context`. Replaced the per-bone draw split |

Not implemented and not planned here: `.phy` collision (that is
`ENGINE_TRACE.md` stage 5), the prop leaf lists as a *visibility* structure
(read and kept, unused), decals, flexes and sub-d surfaces. See "What is
deliberately absent" below.

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
    pub bounds: (Vec3, Vec3),    // the RENDER bounds, model space — see below
    pub illum_position: Vec3,    // where lighting is sampled by default
    pub flags: StudioFlags,
    pub checksum: u32,           // shared by all three files; also what a .vhv must match
    pub vertices: Vec<ModelVertex>,
    pub indices: Vec<u32>,
    pub batches: Vec<Batch>,
    pub skin_families: usize,    // numskinfamilies — how many material sets, >= 1
    pub bones: Vec<anim::Bone>,
    pub sequences: Vec<anim::Sequence>,
    /// **Shared, not owned** — the draw path poses a model to look at it and
    /// `server/`'s attachment lookup poses the same model to place a child of
    /// it, and `models/anim_wp/room_transform` carries 1,350 animations.
    pub animations: Arc<[anim::Animation]>,
    /// The attachment points — 266 of the game's 2,017 models have any.
    pub attachments: Vec<anim::Attachment>,
    /// The `$includemodel` companions merged into the lists above.
    pub includes: Vec<String>,
}

impl StudioModel {
    pub fn sequence(&self, label: &str) -> Option<usize>;   // LookupSequence
    pub fn animation(&self, sequence: usize) -> Option<&anim::Animation>;
    /// LookupAttachment — ZERO-based, `None` for "no such attachment".
    pub fn attachment(&self, name: &str) -> Option<usize>;
    /// The largest bone index any vertex is weighted to, plus one — how many
    /// palette entries a draw of this model reads. At most `bones.len()`.
    pub fn skinned_bones(&self) -> usize;
}

impl StudioModel {
    pub fn load(vfs: &Vfs, name: &str) -> Result<StudioModel, StudioError>;
    pub fn triangle_count(&self) -> usize;
}

pub struct Batch {
    /// **One per skin family**, in family order, as MaterialCache::load wants
    /// them. Never empty. Index it with `material()`, not directly.
    pub materials: Vec<String>,
    pub first_index: u32,
    pub index_count: u32,
    pub body_part: u16,
    pub model: u16,
}

impl Batch {
    /// The material at `skin` — the raw m_nSkin, clamped to family 0.
    pub fn material(&self, skin: i32) -> &str;
}

/// Which family a raw m_nSkin selects. `skin <= 0 || skin >= families` is 0.
pub fn family(skin: i32, families: usize) -> usize;
```

**A batch is one replaceable texture *slot*, not one material.** A slot means
the same thing in every skin family and a material does not, so the batch is
keyed on the slot and carries a material per family; the instance's skin picks
between them at record time. Two slots that resolve to the same material at
skin 0 stay two batches, because a later family can tell them apart —
`models/props/metal_box.mdl` is 12 families over 12 slots with **one mesh**, and
every family differs from family 0 in exactly the one column that mesh names.

<a id="render-bounds"></a>

#### `bounds` is the *render* bounds, and that is not `view_bbmin`

`Mdl` carries both boxes the header holds and `StudioModel::bounds` is
`Mdl::render_bounds()`, which is `CModelInfo::GetModelRenderBounds`
(`engine/ModelInfo.cpp:263`):

```rust
pub struct Mdl {
    pub bounds: (Vec3, Vec3),   // view_bbmin / view_bbmax — the clipping box
    pub hull: (Vec3, Vec3),     // hull_min / hull_max — the movement box
    …
}
impl Mdl {
    /// view_bb when either vector is non-zero; hull otherwise.
    pub fn render_bounds(&self) -> (Vec3, Vec3);
}
```

**Take `render_bounds()`, never `bounds`.** `studiomdl` writes
`view_bbmin`/`view_bbmax` only for a model compiled with an explicit `$bbox`,
and measured over the depot **2,033 of the game's 2,041 models leave both
zero**. Reading them straight gives a *degenerate box at the model's origin* —
which is not an error anywhere, it is a cull box that rejects the prop from most
viewpoints. Valve spells the same fallback in three places
(`ModelInfo.cpp:263`, `c_baseanimating.cpp:6341`, `tier3/mdlutils.cpp:40`) and
all three test "is either vector non-zero", not "is the box non-empty".

The measurement that says it works: with the fallback, **0 of the game's 1,444
static props** reach outside their render bounds; reading `view_bbmin` raw,
**all 2,017 loadable models** did, the worst by 8,972 units.
`every_shipped_studio_model_parses` asserts the zero.

For an *animated* model the render bounds are not enough on their own — see
[`Sequence::bounds`](#anim--bones-sequences-and-the-pose).

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

pub struct Sequence {
    pub label: String, pub flags: u32, pub anim: usize,
    pub fade_out_time: f32,     // mstudioseqdesc_t::fadeouttime, in SECONDS
    pub bounds: (Vec3, Vec3),   // mstudioseqdesc_t::bbmin/bbmax, model space
}
pub struct BoneTrack { pub bone: usize, pub pos: Vec<Vec3>, pub rot: Vec<Quat> }
pub struct Animation {
    pub name: String, pub fps: f32, pub flags: u32,
    pub frame_count: usize, pub tracks: Vec<BoneTrack>,
}
impl Animation { pub fn duration(&self) -> f32; }

/// mstudioattachment_t — a named frame riding a bone. 92 bytes.
pub struct Attachment {
    pub name: String,
    pub flags: u32,       // only ATTACHMENT_FLAG_WORLD_ALIGN exists
    pub bone: usize,      // into the model's OWN bone list, already remapped
    pub local: Mat4,      // where the point sits in that bone's frame
}

pub const STUDIO_LOOPING: u32 = 0x0001;
pub const ATTACHMENT_FLAG_WORLD_ALIGN: u32 = 0x10000;

/// R_StudioSetupBones + ComputePoseToWorld, in model space —
/// what a BIND-POSE VERTEX is multiplied by.
pub fn pose(bones: &[Bone], anim: Option<&Animation>, cycle: f32) -> Vec<Mat4>;
/// The same walk WITHOUT the poseToBone factor — `boneToWorld`, which is
/// what `GetBoneTransform` hands out and what an ATTACHMENT rides.
pub fn bone_to_model(bones: &[Bone], anim: Option<&Animation>, cycle: f32) -> Vec<Mat4>;
/// CBaseAnimating::GetAttachment, in model space. `bones` is
/// `bone_to_model`'s output, NOT `pose`'s.
pub fn attachment_to_model(attachment: &Attachment, bones: &[Mat4]) -> Option<Mat4>;

/// MAX_NUM_BONES_PER_VERT (studio.h:87). The format's limit, not a choice.
pub const MAX_BONES_PER_VERTEX: usize = 3;
/// SkinPositionAndNormal on the CPU: where one vertex ends up under a pose.
/// `bones` is `pose`'s output. The vertex shader's `skin_model_matrix` is the
/// same arithmetic — see "Skinning" below.
pub fn skin(vertex: &ModelVertex, bones: &[Mat4]) -> Vec3;
```

<a id="skinning"></a>

#### Skinning — and why the CPU and the GPU spell it differently

Every `ModelVertex` carries `bone_weights: [f32; 3]` and `bone_indices: [u8; 4]`,
straight out of `mstudioboneweight_t`, and the vertex shader blends up to three
of the draw's bone-palette matrices. **The palette is `pose`'s output** —
`boneToWorld * poseToBone` per bone, in model space — and the entity's
*placement* stays out of it, on the draw's own model matrix.

The two spellings of the blend are deliberately different:

| | what it does | why |
|---|---|---|
| `anim::skin` (CPU) | transform by each bone, then blend the points | reads directly; used by every census and cull-box check |
| `skin_model_matrix` (WGSL) | blend the matrices, then transform once | three multiply-adds on a matrix instead of three full transforms |

They are equal for affine matrices — `Σ wᵢ(Aᵢp + tᵢ) = (Σ wᵢAᵢ)p + Σ wᵢtᵢ` —
and `skinning_on_the_cpu_matches_what_the_shader_does` builds the shader's
matrix the shader's way and compares, because every posed bound in this port is
computed on one side and drawn on the other.

**The neutral weight is `[1, 0, 0]`, not `[0, 0, 0]`.** A vertex with no
weights blends nothing and collapses onto the entity's origin, so
`ModelVertex::new` spells the neutral value out and `assemble` pads a vertex's
unused index slots with bone 0 at weight 0.

**`assemble` refuses a file whose vertex names a bone the model has not got.**
The shader indexes the palette without checking and `wgpu`'s bounds checking
returns zero for an out-of-range storage read — which collapses the vertex
rather than erroring, so the check is cheaper here where the message can name
the model.

Measured over the depot: **1,876 models weight every vertex to one bone, 63
reach two and 78 reach three**; 141 of the 420 multi-bone models share a vertex
between bones, and the most bones any one model is skinned by is **248**
(`models/container_ride/finedebris_part12`), against `MAXSTUDIOBONES`' 256.
Seven of that family are on `sp_a1_intro1`, which is why the palette is sized
off a *pose* rather than off a model — see `rustdocs/MATERIALS.md`.

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

**`fade_out_time` is not a blend time here.** Nothing in this port cross-fades
between sequences; the field is carried for one reader, `CBaseAnimating`'s
`GetLastVisibleCycle` (`baseanimating.cpp:1043`), which turns it into the cycle
at which a non-looping sequence counts as **finished**:
`1 - fadeouttime * cycleRate * playbackRate`. So a sequence played forwards is
finished `fadeouttime` seconds before it ends, and that is what
`IsSequenceFinished()` — and so `prop_testchamber_door`'s `OnFullyOpen` — is
made of. Measured over the shipped game: **10,664 of its 10,666 sequences write
0.2 and the other two write 0.5**; none writes zero, so the term never folds
away. See `rustdocs/SERVER.md`'s `sequences` and gotcha 67.

**`Sequence::bounds` is not optional decoration — a model's own bounds do not
contain its animated geometry.** `C_BaseAnimating::GetRenderBounds`
(`c_baseanimating.cpp:6353`) merges the *playing* sequence's box into the
model's render bounds, and the reason is the size of the gap: measured over the
depot, a posed vertex reaches up to **23,029 units** outside the model's own
box (`models/a4_destruction/fin3_orangepipeexpl.mdl`, an explosion that throws
debris across the map). All 10,666 sequences in the game declare one and the
widest reaches 16,384 units — half the coordinate range, which is what a
"whole map" box looks like.

`engine::world::entities`' `cull_box` is the consumer, and it takes the whole
sequence's box rather than this instance's cycle — Valve's, and what keeps the
cull cheap: no pose is computed for an entity that is then culled.

`pose` returns **pose-to-model** matrices: `boneToWorld[i] * poseToBone[i]`,
which is what a *bind-pose* vertex is multiplied by. With no animation every
one of them is the identity, which is what lets a static prop and an animated
model share one draw path.

#### Attachment points, and the one array they must not use

An **attachment** is a named frame riding a bone: `muzzle` on a gun,
`attach_arm` on a panel arm. It is what a map parents an entity *to* —
`SetParentAttachment "attach_arm"` — and `src/server/attachment.rs` is the
other end of that. [`StudioModel::attachment`] is `LookupAttachment` and is
**zero-based**, where Valve's returns `index + 1` so that 0 can mean "none".

> **It rides [`bone_to_model`] and not [`pose`], and the difference is a wrong
> answer rather than an error.** `CBaseAnimating::GetAttachment` is
> `ConcatTransforms( bonetoworld, pattachment.local, … )`, and `bonetoworld`
> comes from `GetBoneTransform` — the array *before* the `poseToBone` concat.
> `pose`'s extra factor exists to cancel a **vertex's** bind transform, and an
> attachment's `local` is already expressed in the bone's posed frame, so
> multiplying by it first applies the bind pose twice. In the bind pose that
> makes every attachment collapse onto the model's origin, which is a place
> rather than a crash; `an_attachment_rides_the_bone_and_not_the_posed_vertex_matrix`
> pins both answers side by side.

Measured over the shipped game (`every_shipped_studio_model_parses`): **266 of
the 2,017 models have attachment points, 1,952 points between them under 854
distinct names.** Two of the branches ported with them are content-free in
Portal 2 and are recorded as such rather than deleted:

- **`ATTACHMENT_FLAG_WORLD_ALIGN`** — keep the bone's position, drop every
  rotation — is set by **none** of the 1,952.
- **`AppendAttachments`' merge** carries **none** of them: all 25
  `$includemodel` hosts own their attachment points and their companions carry
  animation and nothing else. The bone remap in it is kept anyway, because it
  is the same silent-wrong-joint trap the animation tracks have.

### `include` — `$includemodel`

A `.mdl` can name other `.mdl` files whose sequences and animations it borrows.
`StudioModel::load` merges them in before the geometry is joined, so nothing
downstream — `sequence`, `animation`, `pose`, `server::sequences` — can tell
the difference between a sequence the model owned and one it inherited.

```rust
let model = StudioModel::load(vfs, "models/anim_wp/room_transform/arm64x64_interior.mdl")?;
assert_eq!(model.includes, ["models/anim_wp/room_transform/arm64x64_interior_animation.mdl"]);
assert_eq!(model.sequences.len(), 1 + 1_350);   // one local, 1,350 inherited
model.sequence("makeramp_02open_idleend");      // resolves; the host has no such label
```

There is no public entry point: the module is `pub(super)` and the only caller
is `load`. `Mdl::include_models` is the list as the *file* declares it;
`StudioModel::includes` is the subset that could actually be read.

`CStudioHdr::ResolveIncludedModels` and the `virtualmodel_t` under it
(`public/studio_virtualmodel.cpp`), with the indirection **resolved rather
than recorded** — Valve keeps the included headers separate and hops through a
per-group remap table on every access, because they are cache entries that can
be evicted; a `StudioModel` is an owned value, so the merge happens once at
load and the groups collapse. `masterSeq`, `boneMap`, the attachment/pose/node
tables and `CModelLookupContext` go with them.

What does **not** collapse is `masterBone`, and it is the whole of the work:
an included animation's track names a bone of the *included* model, and the two
skeletons are the same bones in a different order in eight of the nine models
this reaches. See gotchas 20-23. `portdocs/STUDIO.md` §12 has the C++ anatomy
and the measurements.

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
    pub skin: i32,                 // m_nSkin — which skin family draws it
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

```rust
pub struct PropBatch {
    pub materials: Vec<Arc<Material>>,   // one per skin family; never empty
    pub first_index: u32,
    pub index_count: u32,
}

impl PropBatch {
    pub fn material(&self, skin: i32) -> &Arc<Material>;   // clamps like studio::family
}
```

**`PropBatch` mirrors `studio::Batch`**, and every draw site passes the
instance's *raw* skin rather than a precomputed family index. That is
deliberate: a skin changes while the level runs — `prop_weighted_cube` picks a
different one when it is painted — so a precomputed family would have to be
re-derived in `EntityModels::sync`. Two comparisons per draw against a class of
bug that only appears in motion.

### Lighting

A prop is lit by one of two things, and **it is one or the other, not both**:

```rust
// The light cache: the leaf ambient cube plus the world lights that reach the
// point. `rustdocs/ENGINE.md`, "world::light".
pub struct LightCache { /* private */ }
impl LightCache {
    pub fn lighting_at(&self, tracer: &mut Tracer<'_>, position: Vec3) -> ModelLighting;
}

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

`PropModels::load` reads every usable instance's `.vhv` into **one**
`VertexBuffer` for the whole map and slices it per prop.

**Which of the two a prop gets is `bStaticLighting`** (`l_studio.cpp:3046`), and
the deciding term is `PropModel::uses_bumpmapping` — set for a model any of
whose materials has a `$bumpmap` or a non-zero `$phong`. Such a model is lit per
pixel, has no `bStaticLight` in its shader at all, and the shipped engine does
not fetch its colour mesh; every other prop with a file uses the bake and gets a
**zeroed** ambient cube and no local lights, because `StudioSetupLighting` asks
`LightcacheGetStatic` without `LIGHTCACHEFLAGS_STATIC`. Adding the two together
double-counts a prop's indirect light. On `sp_a1_intro1` the split is **816
baked, 246 per-pixel, 18 with no file at all**.

`rustdocs/ENGINE.md`'s "world::light" section is the whole of the light cache
half, including the twelve rules that produce a plausible wrong picture.

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

18. **`Prop::leaves` is a PVS structure and is not used yet.** Every prop is
    drawn every frame.

19. **`TRANSLUCENT_TWOPASS` models draw unsorted.** 38 of Portal 2's models set
    it and this port has no sorted translucent pass.

20. **An included animation's track names the *include's* bone, not the
    host's** — `mstudio_rle_anim_t::bone` is an index into the file the
    animation came from, and `CalcVirtualAnimation` puts the decoded value at
    `masterBone[panim->bone]`. The two skeletons are the same bones in a
    **different order** in eight of the nine models this reaches, so a merge
    that skipped the remap would draw a panel arm bending at the wrong joint —
    a plausible wrong picture, not an error. An included bone the host has not
    got is dropped along with its track (one in the shipped game:
    `thigh_A_R_GRP`, in `models/eggbot_animations.mdl`).

21. **The RLE is decoded against the *include's* bones and that is not the same
    remap.** `posscale`, `rotscale` and the bind-pose `rot` an animated value
    is added to all come from `pAnimbone[panim->bone]` — the animation file's
    bone. It happens for free here because the include is parsed as its own
    file; it would *not* if anything ever tried to decode an included stream
    against the host's bone table.

22. **The host wins every name collision, and an animation's dedup is by
    *name*.** `AppendAnimations`/`AppendSequences` keep the first thing they
    saw and group 0 is the host, so `arm64x64_interior`'s one local `BindPose`
    survives its include's 1,350 sequences. **The window is `int numCheck =
    m_anim.Count()`, captured before the loop**, so a name repeated *inside one
    include* is appended twice rather than folded together — searching the
    whole list as it grows is the obvious implementation and silently plays the
    wrong animation. No shipped companion has a duplicate name, so only a unit
    test reaches it. The consequence worth knowing:
    because animations dedup by name independently of sequences, an *included*
    sequence can end up pointing at the **host's** animation — which is then
    not remapped, because it never belonged to the include. That is Valve's
    and it is deliberate.

23. **`StudioModel::includes` is empty from `assemble`, and that is not a
    bug.** `assemble` joins three already-parsed files and has no filesystem to
    resolve a fourth against; only `StudioModel::load` merges. A test that
    builds a model from fixtures therefore never sees an include.

24. **`view_bbmin`/`view_bbmax` is zero on 2,033 of the game's 2,041 models**,
    so anything that reads `Mdl::bounds` directly is reading a degenerate box
    at the origin. Take [`render_bounds`](#render-bounds). This produced a real
    regression — `prop_dynamic`s winking out at particular angles, and rooms
    behind an areaportal looking empty — and nothing about it reads as an
    error: the prop is simply not drawn.

25. **This port poses 135 models outside the box their own sequences declare**,
    the worst by 23,029 units. `studiomdl` computes `mstudioseqdesc_t::bbmin`/
    `bbmax` from the animated geometry, so a gap that large means the pose is
    *wrong*, not the box — almost certainly the sequences whose animation lives
    in an external `.ani` block, which is unported (see "What is deliberately
    absent"). They are the `a4_destruction` set, the cloth sims and the
    `props_vac_anim` models. The cull box follows Valve exactly either way;
    what is wrong is upstream of it.
    `every_shipped_studio_model_parses` prints the list, worst first.

---

## What is deliberately absent

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
- **External `.ani` animation blocks** are now the binding constraint on
  animation, where `$includemodel` was. `mstudioanimdesc_t::animblock != 0`
  means the frames live in a companion `.ani` rather than in the `.mdl`, and
  such an animation reads as empty here — which holds the bind pose rather
  than mis-reading bytes. It did not matter until the merge landed. The nine
  models a `prop_dynamic` includes for name **eight distinct companions**
  (both panel arms share one), and only two of the eight are inline:
  `arm64x64_interior_animation` (1,350 of 1,350) and
  `personality_sphere_animation` (313 of 318). `eggbot_animations`,
  `ballbot_animations`, `player_animations`, `headless_player_animations`,
  `headless_s8player_animations` and `glados_wheatley_boss_animation` keep
  almost all of theirs in an `.ani`, so their labels now resolve and their
  poses are empty.

  > **Skinning has landed, so this is now the largest gap in the module.**
  > Until it did, every model that reaches those six was drawn in its bind
  > pose anyway — the panel arms were the only two of the nine that could be
  > posed at all — and reading `.ani` bought nothing. That argument is spent:
  > all nine can be posed now, and what they are missing is the animation
  > data itself. **135 models pose outside the box their own sequences
  > declare, the worst by 23,029 units**, and since `studiomdl` computes that
  > box from the animated geometry it is the pose that is wrong.
- **Sequence bone weights** (`mstudioseqdesc_t::weightlistindex`) — a
  per-sequence, per-bone weight that `CalcVirtualAnimation` uses to leave a
  bone at its bind pose. 85 sequences across the nine companions set one to
  zero, all of them on the same three models above. `pose` takes an
  `Animation`, not a `Sequence`, so this would change its signature; the
  condition for doing it is a model that both needs the weights and can be
  posed at all.
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
  > measured; the larger, `$includemodel`'s 9 models and 926 entities, is
  > closed.
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
- **Skinned models** — `vtx` stops discarding `StripHeader_t`'s bone plumbing
  and the bone matrices move to the GPU. (`vvd::BoneWeights` already carries
  `bones`, `weights` and `count`; the gap is the strip plumbing and the
  shaders, not the reader.)
- **Sharing an included model between hosts** — both panel arms include the
  same 1.3 MB companion, and a map placing both reads and expands it twice
  (about 50 ms and 7 MB each, at level load; the frame path is untouched).
  Valve gets the sharing free from `CMDLCache`; the equivalent here is a cache
  above `StudioModel::load`, and the condition for writing one is a level load
  that is actually too slow.
- **Body groups** (`m_nBody`) — the other half of the selector family skin
  families belong to, and the one still missing. It chooses which *model*
  inside a body part draws, which is geometry rather than materials;
  `build.rs` already keeps body parts in separate batches precisely so that it
  can be added without a rewrite. 959 of 968 models have exactly one body part,
  so it is near-vestigial on props and matters for characters.
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
| `studio::tests::a_mesh_material_is_a_column_of_the_skin_table` | that the indirection is walked at all — `mesh->material` is a slot, not a texture |
| `studio::tests::a_skin_outside_the_table_draws_family_zero` | the clamp, in `r_studiodraw.cpp:2911`'s spelling rather than the one that indexes negatively |
| `studio::tests::a_model_with_no_skin_table_gets_an_identity_row` | that a file with no table still answers for skin 0 — the shape every pre-families test uses |
| `studio::tests::two_slots_sharing_a_material_stay_two_batches` | **the batching key**: grouping by resolved material instead of by slot would make the other families unrepresentable |
| `studio::tests::a_skin_family_naming_a_missing_texture_is_refused`, `a_mesh_naming_a_slot_the_table_has_not_got_is_refused` | the two range checks a misread `skinindex` trips |
| `studio::tests::the_weighted_cube_draws_in_twelve_material_sets` | the widest table in the game, resolved for real — 12 families over one mesh |
| `engine::world::props::tests::every_shipped_map_places_its_props` | **the size of the skin gap**: 10,030 of 56,955 placements name a non-zero family and 10,002 draw a different material for it, over 101 maps, 269 on `sp_a1_intro1` |
| `server::tests::every_shipped_map_spawns_its_entities` | the entity half — 663 model entities on a non-zero family, 659 of which remap — and that 15 of the game's 98 cubes now draw in the skin their map asked for |
| `engine::world::entities::tests::the_cull_box_covers_a_model_that_declared_no_clipping_box` | gotcha 24 — the fallback's *shape*, including the eight-corner transform a rotated prop needs |
| `engine::world::entities::tests::the_cull_box_follows_the_sequence_the_entity_plays` | the sequence box being merged in |
| `engine::world::entities::tests::the_old_cull_box_would_have_dropped_most_of_the_games_props` | **the size of gotcha 24**, against the depot: 7,515 of 8,072 `prop_dynamic` placements wear a model that escapes the box the port used to build |
| `studio::tests::every_shipped_studio_model_parses` | also the census behind gotchas 24 and 25, the assertion that **no** static prop reaches outside its render bounds, and that **family 0 is the identity permutation in all 2,041 models** — the finding that makes reading the skin table incapable of changing a picture that was already right |
| `studio::anim::tests::skinning_on_the_cpu_matches_what_the_shader_does` | **`anim::skin` against the vertex shader's own arithmetic**, built the shader's way through `bone_rows` — the two spellings the whole skinning path rests on |
| `studio::anim::tests::an_unweighted_slot_contributes_nothing` | that the *weight* and not the index decides, including for a bone the palette has not got |
| `materials::preview::tests::a_skinned_vertex_rides_the_bone_its_weights_name` | the palette being read at all, on a real GPU — the quad's own coordinates put it on the left and the bone moves it right |
| `materials::preview::tests::two_bones_at_half_weight_put_the_vertex_between_them` | the case the per-bone draw split could not express, which is why skinning was written |
| `materials::preview::tests::clearing_the_pose_draws_the_vertex_where_it_was_authored` | `bSkinning` off — the palette bound and not read |
| `materials::preview::tests::a_palette_that_outgrows_its_buffer_keeps_the_poses_already_recorded` | the bone buffer growing **mid-pass** with a draw already recorded against the old one |
| `studio::anim::tests::a_radian_euler_is_roll_pitch_yaw_and_not_a_qangle` | gotcha 14 |
| `studio::anim::tests::the_rle_walk_repeats_the_last_valid_value` | `ExtractAnimValue`'s run encoding |
| `studio::anim::tests::a_compressed_quaternion_rebuilds_its_w` | `Quaternion48`/`Quaternion64` |
| `studio::anim::tests::the_blend_aligns_and_normalizes` | gotcha 15's blend half |
| `studio::anim::tests::halves_decode` | `Vector48` |
| `studio::anim::tests::the_bind_pose_is_the_identity` | gotcha 16 |
| `studio::include::tests::a_tracks_bone_is_remapped_by_name_and_not_by_index` | gotcha 20 — the one merge failure that draws something wrong rather than nothing |
| `studio::include::tests::a_track_for_a_bone_the_host_does_not_have_is_dropped` | `masterBone` of -1 |
| `studio::include::tests::bones_sequences_and_animations_all_match_case_insensitively`, `a_deduplicated_animation_keeps_the_hosts_own_tracks` | gotcha 22 — `AppendAnimations`/`AppendSequences`' dedup, and its subtle half |
| `studio::include::tests::the_dedup_window_is_fixed_before_the_include_rather_than_growing` | `numCheck` being captured before the loop — invisible in Portal 2, and wrong the obvious way |
| `studio::include::tests::a_missing_include_is_skipped_and_not_an_error`, `a_cycle_terminates` | `FindModel` returning null, and the cycle guard Valve has not got |
| `studio::anim_depot_tests::an_included_model_supplies_the_sequences_a_map_asks_for` | **the merge, against the real panel arm** — 1,351 sequences, a pose 112 units off the bind pose, and all sixteen bones agreeing with the companion's own frame at three cycles |
| `studio::anim::tests::an_attachment_rides_the_bone_and_not_the_posed_vertex_matrix` | **the one attachment mistake that gives a place rather than an error** — `bone_to_model` against `pose`, with the wrong answer asserted alongside the right one |
| `studio::anim::tests::an_attachments_local_offset_is_in_the_bones_frame` | `ConcatTransforms( bonetoworld, local )`, the way round it is |
| `studio::anim::tests::a_world_aligned_attachment_keeps_the_place_and_drops_the_turn` | `ATTACHMENT_FLAG_WORLD_ALIGN` — unreachable in Portal 2, so this is the only thing holding it |
| `studio::anim::tests::an_attachment_whose_bone_is_missing_has_no_answer` | `GetAttachment` returning false, which the caller reads as plain parenting |
| `studio::include::tests::an_included_attachments_bone_is_remapped_by_name_and_not_by_index`, `the_host_keeps_its_own_attachment_and_drops_one_with_no_bone` | `AppendAttachments` — also unreachable in Portal 2, and the same remap trap as the tracks |
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
  found gotcha 8.** It also carries the `$includemodel` census: **25 models
  declare one, 24 companions can be read, and they carry 5,232 of the 10,666
  sequences the game's models hold** — that total was 5,434 before the merge.

  **And the bounds census**, added when the cull box turned out to be
  degenerate: 2,033 of 2,041 models declare no `view_bbmin`/`view_bbmax`, 2,030
  of those have a hull to fall back to, **0 of the 1,444 static props reach
  outside their render bounds** where all 2,017 loadable models did before, and
  135 models pose outside the box their own sequences declare (gotcha 25),
  printed worst-first.
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

---

## What has landed, and what each stage found

> Moved here from `CLAUDE.md`, which had grown to 2,126 lines by accumulating a
> paragraph per landed stage. This is the narrative history of the module: what
> was ported, in what order, what it cost and what the measurements said.
> `CLAUDE.md` keeps a one-line summary and points here. **The invariants and
> gotchas above are the normative part of this document**; this section is the
> record of how they were arrived at.

**`src/studio/` — stages 1-5 of `portdocs/STUDIO.md`'s six ported, plus animation**,
and with them **static props draw, lit the way the shipped game lights them, and an
entity's model animates**. `.mdl`/`.vvd`/`.dx90.vtx` become a `StudioModel`: one vertex
buffer, one index buffer, per-material `Batch`es. The instances are
`src/engine/world/props/` — the `sprp` game lump, `AngleMatrix` transforms, one upload
per distinct model and one draw per instance, lit by `world/light.rs`'s light cache.
`sp_a1_intro1` now draws **1,080 props from 136 models, 224,924 triangles** on top of
the world's 14,546, **816 of them wearing `vrad`'s per-vertex bake and 246 lit per
pixel by the world lights instead** (18 have neither and take the cache too). Stage 4
also mounted the `.bsp`'s `LUMP_PAKFILE` as a search path, which is what the `.vhv`
files live in and **which also fixed the 8 `maps/<map>/…` cubemap materials** that used
to draw as checkerboards — one change, two subsystems, as predicted. Not done: LOD
selection (stage 6) and `.phy` collision (that is `ENGINE_TRACE.md`'s). **`studio/anim.rs` landed later, with `prop_floor_button`** — bones,
sequences and the RLE animation blocks, plus the `R_StudioSetupBones` slice that poses
them; skinning is *replaced* by a per-bone draw split rather than deferred, which is
exact for every model the port draws. See `src/server/`, below.
**`prop_dynamic` measured two gaps in that half, and the larger one is now closed.**
**`studio/include.rs` is `$includemodel`** — `CStudioHdr::ResolveIncludedModels` and
the `virtualmodel_t` under it: 9 of the 606 models the game's props name keep their
sequences in a companion `*_animation.mdl`, **926 entities wear one**, and until it
landed those 926 stood in their bind pose. Valve keeps the included headers separate
and hops through a per-group remap table on every access, because they are cache
entries that can be evicted; a `StudioModel` is an owned value, so the merge happens
**once, at load**, and `masterSeq`, `boneMap`, the attachment/pose/node tables and
`CModelLookupContext` all delete. What does not delete is `masterBone`: an included
animation's track names a bone of the *included* model, and the two skeletons are the
same bones **in a different order** in eight of the nine — so a merge without the
remap bends a panel arm at the wrong joint, which is a wrong picture and not an
error. Measured on the running maps: of the 2,738 props playing a sequence two
seconds into their level, the labels that resolve went from 1,666 to **2,556** and
the ones that do not from 897 to **182** — and that remainder is Valve's own map
errors rather than a gap. `portdocs/STUDIO.md` §12 has the anatomy; the rest of the
measurements are there too, including the one that made it simple (**host and include
bind poses agree to 4e-6**, so `boneMap` buys nothing) and the one that bounds it
(**nothing nests**, and `STUDIO_OVERRIDE` is set on 0 of the game's 7,885 sequences).
**`.ani` animation blocks are the binding constraint now**: the nine models name
eight distinct companions (both panel arms share one) and only two are inline —
`arm64x64_interior_animation`, all 1,350 of them, and `personality_sphere_animation`,
313 of 318 — while the other six keep almost all of theirs in a companion `.ani`, so
their labels resolve and their poses are empty. Every model but the two panel arms is
one this port draws in its bind pose for want of skinning anyway, so skinning comes
first and the arms are the whole visible payoff: **898 of the 926**.
And **flex deltas stop being academic**: the 16 models the reader
refuses are `models/props_destruction/toxin*`, 15 of them are placed as
`prop_dynamic`s by **41 entities**, and those 41 draw nothing. The "absent from the
data" claim below is about *static props* and is still exactly true; `prop_dynamic`
is the first thing in the port that places a model that is not one. `CMDLCache`'s eviction, budgets and async queues are
**deleted rather than deferred**, and so are skinning, flexes and sub-d — which are
absent from the *data*: all 968 models Portal 2 places as static props have one bone,
trilist strips and no flex deltas. **API: `rustdocs/STUDIO.md`** — read it before
calling in, in particular for the gotchas that produce a plausible wrong picture rather
than an error: **`sizeof(StaticPropLumpV9_t)` is 72 and not 69**, because Valve's prop
structs are the only ones on this path not `#pragma pack(1)`, and at 69 every prop
after the first drifts; **the ambient cube decodes with `ColorRGBExp32ToVector` and the
lightmap with `TexLightToLinear`**, which is the *opposite* of `rustdocs/MATERIALS.md`'s
rule and 255× either way (measured: 0.0249 against 0.0002 mean luminance on
`sp_a1_intro1`); **a `QAngle` is pitch, yaw, roll** composed `Rz·Ry·Rx`, so props with
only a yaw look right under any other reading and tilted ones do not; and **a leaf with
zero ambient samples and a non-zero `first_sample` is a solid leaf whose `first_sample`
is a *leaf* index**, which is what keeps a prop embedded in geometry lit.
Two more gotchas arrived with stage 4, and the first is the worst in the module:
**a `.vhv` is in *hardware* vertex order, not `.vvd` pool order** — Valve's runtime
compacts a model's vertices per LOD and bakes against that numbering, this port does
not compact, and reading the block as a run over the pool mislights 125 of
`sp_a1_intro1`'s 1,080 props **while appearing to work for the other 955**
(`HardwareMesh` carries the mapping); and **`vrad` writes no block for an empty mesh**,
so a model's empty meshes must be dropped before the lists are matched. The `.vhv`
checksum is **counted, not enforced**, because `r_ignoreStaticColorChecksum` defaults to
1 and 24 of the game's 56,801 files need it to.
Two verifications run against the real depot behind `KISAK_GAME_DIR` and `--ignored`:
**2,017 of the 2,041 shipped models parse** (all 1,444 flagged `STATIC_PROP`; the 16
refusals are animated flex-delta models and are correct), and **all 106 shipped maps
place their props — 56,955 of them — with all 56,801 `.vhv` files describing the model
they are for**. The first of those found two wrong `.vtx` field offsets that **every
synthetic test had passed**, because the fixture had been written from the reader
instead of from `optimize.h`; the second found the hardware-order rule.
`portdocs/STUDIO.md` §11 has both.

**Skin families landed after `src/vphysics/`**, and the measurement is what moved
them off `portdocs/STUDIO.md` §8's optional stage 6 and in front of LOD selection.
`mstudiomesh_t::material` is not an index into the texture list — it is a *column*
of a `numskinfamilies × numskinref` table of `short`s at `skinindex`, and the material
is `pSkinRef[skin * numskinref][mesh->material]`. **10,030 of the game's 56,955 static
prop placements ask for a family other than 0 and 10,002 of them draw a different
material for it**, across 101 of 106 maps and **269 of them on `sp_a1_intro1`** —
17.6% of the game's props, drawing the wrong materials until this. The worst single
model is not the cube: it is `models/anim_wp/framework/squarebeam_off.mdl` at
**5,666 placements**, the white beam Aperture's walls are built out of, with
`props_lab/glass_lightcover` second at 1,652. The entity half is **663 model entities
on a non-zero family, 659 of which remap** — 609 of the raw keys are `prop_dynamic` —
and it includes **15 of the game's 98 weighted cubes**, one of which is the cube on
`sp_a1_intro1`: it drew `metal_box` and now draws `metal_box_skin003`, the rusted one
its map asked for. Three things the implementation found. **Family 0 is the identity
permutation in every one of the 2,041 shipped models**, and `numskinref == numtextures`
in every one too, which is why the port's old direct index was exactly right for skin 0
and why reading the table cannot regress a picture that was already correct — the
depot census asserts it. **The batch key had to stay the slot rather than become the
resolved material**: `models/props/metal_box.mdl` is 12 families over 12 slots with a
single mesh on slot 0, so grouping by material would have collapsed all twelve into
one; a batch now carries `Vec<String>` — one material per family — and the instance's
raw `m_nSkin` picks at record time, unclamped on the instance because a cube changes
skin when it is painted. And **`uses_bumpmapping` had to widen with it**: it is
`bStaticLighting`'s deciding half, so a model that is per-pixel only at skin 2 must
answer yes for every placement, which is why Valve ORs it over the whole `ppMaterials`
array. `sp_a1_intro1`'s static props went from **67 materials to 81** at load; the
frame cost is in `rustdocs/ENGINE.md`, "Frame cost, measured".
