# Porting static props and the studio model pipeline → `src/studio/` + `src/engine/world/props/`

Status: **planned, not started.** This document is written before the port, against
`legacy/`, per `CLAUDE.md`'s "Per-module porting docs". When the module lands it gets
`rustdocs/STUDIO.md`.

Everything numeric in this document was **measured against the shipped Portal 2 depot**
at `~/Documents/SourceEngineWork/app_620/depot_621/portal2` — all 106 maps and all 2,041
models in `pak01` — rather than inferred from the C++. Where a measurement contradicts
what a Valve header suggests is possible, the measurement wins and the difference is
called out.

---

## 0. Headline decisions

1. **The work is not "port `studiorender/`".** It spans four Valve modules, and the
   largest single piece of it is in one this tree's docs do not inventory at all:
   `datacache/mdlcache.cpp` (5,807 lines) is where `.mdl`/`.vvd`/`.vtx` are actually
   read and stitched. `ENGINE.md` §7.16 files static props under the renderer front-end
   and `PORTING.md` never mentions `datacache/`. See §2.

2. **`R_StudioSetupLighting` does not exist in this tree.** `CLAUDE.md` and
   `MATERIALSYSTEM.md` §9 both name it as the next step for models; it is an older-Source
   name and a case-insensitive search for `setuplighting` across all of `legacy/` does not
   find it. The cstrike15 equivalent is `IVModelRender::SetupLighting`
   (`public/engine/ivmodelrender.h:187`) → `CModelRender::ComputeStaticLightingState`
   (`engine/l_studio.cpp:1950`) over `LightcacheGetStatic` / `LightcacheGetDynamic`
   (`engine/lightcache.h:140,155`), filling a `MaterialLightingState_t`
   (`public/materialsystem/imaterialsystem.h:271`). **Fix those two references when this
   lands.** §6.

3. **The module splits in two, along the line `world/` and `materials/` already sit on.**
   `src/studio/` is the *asset* — three file formats in, GPU-ready meshes out — and is a
   top-level sibling of `src/materials/`, which it names. `src/engine/world/props/` is the
   *map's instances* — the `sprp` game lump, transforms, per-prop lighting. That is exactly
   the relationship `world/` already has with `materials/` for lightmaps, and it keeps the
   `.mdl` reader testable with no map and no GPU. §7.1.

4. **Static props are a drastically smaller problem than models in general, and this is
   measured, not assumed.** Across all 968 models any Portal 2 map places as a static prop:
   every one has **exactly one bone**; every strip group is `STRIPGROUP_IS_HWSKINNED` and
   **none** is `STRIPGROUP_IS_DELTA_FLEXED`; every strip is `STRIP_IS_TRILIST` and **none**
   is a sub-d quad list; and **`numBones` is 0 on all 1,130 strip groups**. So skinning,
   flexes/morphs, and sub-division surfaces are not deferred from this port — they are
   *absent from the data*. That deletes ~6,400 lines of `studiorender/` outright for this
   target. §3.

   > **True of static props, and only of them.** This survey predates entity-placed
   > models, which name a much larger set: **141 of the game's 420 multi-bone models
   > share a vertex between bones**, and 290 entities wear one. Skinning was written for
   > those and has landed — `rustdocs/STUDIO.md`, "Skinning". Flexes and sub-d are still
   > absent from everything this port draws.

5. **Only `sprp` version 9 needs to be read.** All 106 shipped maps are `.bsp` version 21
   carrying `sprp` version 9 at 72 bytes per prop. `gamebspfile.h` defines seven layouts
   (V4–V10); six of them are dead code for Portal 2. §4.1.

6. **`StaticPropLumpV9_t` is 72 bytes, not 69, and nothing in the header says so.** It is
   the one struct in this port's path that is *not* `#pragma pack(1)` — the trailing
   `bool m_bDisableX360` carries three bytes of tail padding. Read it as 69 and every prop
   after the first is garbage. §4.1.

7. **Per-prop baked lighting is blocked on mounting the `.bsp` pak lump.** Each prop's
   vertex lighting is a separate `sp_hdr_<index>.vhv` file *inside* `LUMP_PAKFILE` —
   1,080 of them in `sp_a1_intro1`, exactly one per prop instance. This is the same
   unmounted-pak-lump limitation `CLAUDE.md` already records for the 8 cubemap materials,
   and it is now load-bearing for a second subsystem. §5.2.

8. **The materials half is genuinely ready.** 429 of the 466 distinct materials these
   props wear are `VertexLitGeneric`, which landed in `materialsystem` stage 6, plus 17
   `Patch` and 2 `UnlitGeneric`. Of those 429, **108 carry `$phong 1`** and so take the
   unported `Phong` path — they will draw without specular and say so once, exactly as
   models already do. §5.3.

---

## 1. What a static prop is

A static prop is a studio model the map compiler has decided never moves: it has one
bone, no animation, a fixed world transform, and its lighting is baked at compile time
rather than computed. `vrad` writes that baked lighting; `vbsp` writes the placement into
a game lump. At runtime the engine builds one vertex buffer per model and draws it once
per instance.

It is the cheapest possible consumer of the studio pipeline, which is why it is the right
first one: it exercises the three file formats, the material binding and the lighting
block, and touches none of the animation system.

The number that matters for scope: **56,955 static prop instances across the 106 shipped
maps, drawn from 968 distinct models.** `sp_a1_intro1` alone has 1,080 instances of 136
models. Every one of those 968 models is present in `pak01`; none live in a map's pak
lump, so the model *files* are reachable today even though their *lighting* is not.

---

## 2. Inventory: four modules, not one

| Valve path | Lines | Disposition |
|---|---:|---|
| `datacache/mdlcache.cpp` | 5,807 | The three-file load, LOD selection, the `.vvd` fixup. **The core of stage 1.** Not inventoried anywhere in this tree's docs. |
| `studiorender/` | 18,725 | ~12,300 deleted for this target (§3); the survivor is mesh building and lighting |
| `engine/staticpropmgr.cpp` | 2,449 | The `sprp` lump and the instance list. **Stage 2.** |
| `engine/l_studio.cpp` | 5,659 | The model render front-end and the lighting query |
| `engine/lightcache.cpp` | 3,128 | The ambient cube the `ModelLighting` block wants |
| `public/studio.h` | 3,926 | The `.mdl` format. A header, transcribed not ported |
| `public/optimize.h` | 264 | The `.vtx` format. Small and complete |
| `public/materialsystem/hardwareverts.h` | 84 | The `.vhv` format |
| `public/gamebspfile.h` | ~330 | The `sprp` lump layout |
| `datacache/mdlcombine.cpp` | 2,893 | **Delete** — runtime model combining, a CS:GO character feature |
| `datacache/datacache.cpp` | 1,535 | **Delete** — `Vec` and `Drop`, same reasoning as `zone.cpp` |

`datacache/` as a whole is 12,694 lines and only `mdlcache.cpp` survives, most of it as
knowledge rather than code: it is a cache manager whose entire reason for existing —
LRU eviction of model data under a fixed memory budget on a 2007 console — is not a
problem this port has.

### What consumes it

Nothing yet. `world/` will own the prop instances; `trace/` stage 5 wants the `.phy`
collision data (a separate format, out of scope here, and `ENGINE_TRACE.md` §5.8 already
places it with `parry`). The renderer consumes `studio/`'s output the same way it consumes
`world/`'s batches today.

---

## 3. What the data says, and what it deletes

This is the section to read before scoping anything. All figures are over the 968 models
Portal 2 places as static props, traversed at LOD 0.

| Measurement | Result | Consequence |
|---|---|---|
| `numbones` | **1 on all 968** | No skinning *for a static prop*. One `mat4` per instance — still true, and `bSkinning` is off for every one of them. The models an *entity* places needed the palette; see the note above. |
| Strip group flags | **`0x02` on all 1,130** (`STRIPGROUP_IS_HWSKINNED`) | No `STRIPGROUP_IS_DELTA_FLEXED` ⇒ **no flexes** |
| Strip flags | **`STRIP_IS_TRILIST` on 1,129 of 1,130** | No `STRIP_IS_QUADLIST_*` ⇒ **no sub-d** |
| `StripHeader_t::numBones` | **0 on all 1,130** | The bone-state-change path is never taken |
| Body parts per model | 959 × 1, 6 × 2, 3 × 3 | Body groups are near-vestigial but not absent |
| Models per body part | **1 on all 980** | No body-group *choice* to make |
| Strip groups per mesh | 1,130 × 1, 8 × 0 | One vertex/index range per mesh. 8 empty meshes to skip |
| LOD count | 819 × 1, then 2–8 | LOD selection is real but the common case is trivial |
| `.vvd` fixup table | **present on only 15 of 968** | Rare — and 14 of the 15 are genuine permutations, so it cannot be skipped |
| LOD 0 vertices | min 4, median 917, max 52,478, **total 2,429,824** | The whole static-prop vertex set is ~117 MB as `ModelVertex` |
| LOD 0 triangles | **1,787,478** | |
| `studiohdr_t::flags` | `STATIC_PROP` + `AUTOGENERATED_HITBOX` on all 968; `TRANSLUCENT_TWOPASS` on 38; `CAST_TEXTURE_SHADOWS` on 60; `NO_FORCED_FADE` on 11 | Only `TRANSLUCENT_TWOPASS` affects drawing |
| Topology indices | present on 63 strip groups | Sub-d topology; ignorable because the strips are trilists |

**So the following are deleted rather than deferred**, because for this target there is no
data that reaches them: `r_studiosubd.cpp` + `r_studiosubd_patches.cpp` (2,193),
`r_studioflex.cpp` (935), `r_studiodraw_computeflexedvertex.cpp` (1,609),
`flexrenderdata.cpp` (243), `r_studiodecal.cpp` (2,460, decals are their own subsystem),
`r_studiogettriangles.cpp` (143, a tool path). That is **7,583 lines**, before counting
the `IShaderAPI`-tower plumbing in `studiorender.cpp` and `studiorendercontext.cpp` that
goes the same way `shaderapidx9` did.

They come back when *animated* models land — a character has flexes and many bones. This
section is about static props only, and says so loudly so that nobody reads the deletion
as a claim about models in general.

---

## 4. The formats

Three files per model, plus a game lump, plus one lighting file per instance. All four
are external content and therefore **fixed** in PORTING.md's sense: parse them with
`binrw`/`nom`, never redesign them.

Measured across the whole shipped game, the versions are uniform and match the constants
in the headers exactly:

| File | Ident | Version | Count | Header constant |
|---|---|---|---|---|
| `.mdl` | `IDST` | **49** | 2,041 | `STUDIO_VERSION` (`studio.h:63`) |
| `.vvd` | `IDSV` | **4** | 2,033 | `MODEL_VERTEX_FILE_VERSION` (`studio.h:2303`) |
| `.vtx` | — | **7** | 4,066 | `OPTIMIZED_MODEL_FILE_VERSION` (`optimize.h:19`) |

**There is no version branching to write.** Refuse anything else with a real error, the
way `bsp.rs` refuses a leaf version it does not read.

### 4.0 Which `.vtx` to open

Two `.vtx` ship per model: `<name>.dx90.vtx` and a plain `<name>.vtx`, 2,033 of each.
Over 300 sampled pairs they are **byte-identical**. `CMDLCache::GetVTXExtension()`
(`datacache/mdlcache.cpp:3492`) returns `".dx90.vtx"` unconditionally — the `dx80`/`sw`
variants other Source branches select are not in this tree's runtime path, only in
`MapReslistGenerator.cpp`'s file-list generator. **Open `.dx90.vtx`.**

### 4.1 The `sprp` game lump

`LUMP_GAME_LUMP` is lump 35. Its directory is `dgamelumpheader_t`
(`bspfile.h:421`) — an `int` count followed by `dgamelump_t[count]`
(`bspfile.h:437`): `{ int id; u16 flags; u16 version; int fileofs; int filelen; }`, 16
bytes. **`fileofs` is absolute in the file**, not relative to the game lump.

`id` is the four-CC `'sprp'` = `0x73707270`. Portal 2 maps carry two game lumps: `sprp`
and a 12-byte `dprp` (detail props — empty, and out of scope).

The `sprp` payload is three counted arrays back to back:

```
i32 dict_count;   StaticPropDictLump_t[dict_count]   // char name[128]
i32 leaf_count;   u16[leaf_count]                    // StaticPropLeafLump_t
i32 prop_count;   StaticPropLumpV9_t[prop_count]     // 72 bytes each
```

**The 72 bytes are the trap.** `gamebspfile.h`'s prop structs are the only ones in this
path *not* under `#pragma pack(1)`, and `StaticPropLumpV9_t`'s fields sum to 69:

| Offset | Field | Bytes |
|---:|---|---:|
| 0 | `m_Origin` | 12 |
| 12 | `m_Angles` (pitch, yaw, roll) | 12 |
| 24 | `m_PropType` → index into the dict | 2 |
| 26 | `m_FirstLeaf` | 2 |
| 28 | `m_LeafCount` | 2 |
| 30 | `m_Solid` | 1 |
| 31 | `m_Flags` | 1 |
| 32 | `m_Skin` | 4 |
| 36 | `m_FadeMinDist` | 4 |
| 40 | `m_FadeMaxDist` | 4 |
| 44 | `m_LightingOrigin` | 12 |
| 56 | `m_flForcedFadeScale` | 4 |
| 60 | `m_nMin/MaxCPULevel`, `m_nMin/MaxGPULevel` | 4 |
| 64 | `m_DiffuseModulation` (RGBA8) | 4 |
| 68 | `m_bDisableX360` | 1 |
| 69 | **tail padding** | **3** |
| | | **72** |

Confirmed twice: arithmetically, and by measuring `(lump_end - prop_array_start) /
prop_count` = exactly 72.0 on all 104 maps that have props. Valve's own reader gets this
for free because it does `buf.Get(&lump, sizeof(StaticPropLumpV9_t))`
(`staticpropmgr.cpp:1599`) into the V10 struct, and `sizeof` includes the padding. A
hand-written Rust reader has to know.

`m_PropType` indexes the dictionary; `m_FirstLeaf`/`m_LeafCount` slice the leaf array,
which is a PVS acceleration structure and is **not needed until visibility lands** — read
it, keep it, don't use it yet.

### 4.2 `.mdl` — `studiohdr_t`

Only a small part of a 3,926-line header matters here. Field offsets, verified
empirically (the material offsets resolved 466 real `.vmt` files):

| Offset | Field |
|---:|---|
| 0 | `id` (`IDST`), 4 `version` (49), 8 `checksum` |
| 12 | `name[64]` |
| 76 | `length` |
| 92 | `illumposition` — the lighting sample point when no lighting origin is given |
| 104/116 | `hull_min` / `hull_max` |
| 128/140 | `view_bbmin` / `view_bbmax` — the render bounds |
| 152 | `flags` |
| 156/160 | `numbones` / `boneindex` |
| 204/208 | `numtextures` / `textureindex` → `mstudiotexture_t[]` |
| 212/216 | `numcdtextures` / `cdtextureindex` → `i32[]`, each an offset to a path string |
| 220/224/228 | `numskinref` / `numskinfamilies` / `skinindex` |
| 232/236 | `numbodyparts` / `bodypartindex` |

**Every offset in the `.mdl` is relative to the start of the struct that contains it**,
not to the file — `mstudiobodyparts_t::modelindex` is relative to the body part, and
`mstudiomesh_t`'s offsets are relative to the mesh. This is what all those
`(byte *)this + index` accessors encode. In Rust this is the single biggest structural
difference: rather than keeping the file bytes alive and chasing pointers, **resolve every
offset once at parse time into owned `Vec`s**, exactly as `bsp.rs` already does for the
map ("Valve mapped the file and kept pointers into it, which is why so much of
`modelloader` is lifetime management by hand").

The hierarchy that matters:

```
studiohdr_t
  bodyparts[]        mstudiobodyparts_t   (959 of 968 models have exactly 1)
    models[]         mstudiomodel_t       (always exactly 1 per body part here)
      meshes[]       mstudiomesh_t        { material, numvertices, vertexoffset }
```

`mstudiomesh_t::material` indexes `textureindex`; `vertexoffset` is the mesh's first
vertex **within its model**, which is what the `.vtx`'s `origMeshVertID` is relative to
(§4.5).

**Material resolution** is a cross product, and it is not the filesystem's search path:
for each `mstudiotexture_t` name, try `materials/<cdtexture[i]><name>.vmt` for each of the
`numcdtextures` directories in order, first hit wins. Portal 2's static props reference
466 distinct materials this way; **8 references across the whole set resolve to nothing**
(`lambert1`, `greygrid` — shipped content bugs, and the error material is the right
answer for them).

### 4.3 `.vvd` — the vertex pool

```c
struct vertexFileHeader_t {          // studio.h:2309
    i32 id, version, checksum;
    i32 numLODs;
    i32 numLODVertexes[8];
    i32 numFixups;
    i32 fixupTableStart;             // offsets from the start of the file
    i32 vertexDataStart;
    i32 tangentDataStart;
};
```

At `vertexDataStart`: `mstudiovertex_t[numLODVertexes[0]]`, **48 bytes each**
(`studio.h:1447`, and the comment there says so):

| Offset | Field |
|---:|---|
| 0 | `m_BoneWeights.weight[3]` (f32) |
| 12 | `m_BoneWeights.bone[3]` (u8) |
| 15 | `m_BoneWeights.numbones` (u8) |
| 16 | `m_vecPosition` |
| 28 | `m_vecNormal` |
| 40 | `m_vecTexCoord` |

At `tangentDataStart`: `Vector4D[numLODVertexes[0]]` — the tangent, `w` carrying the
bitangent sign. `materials` stage 6 already expects exactly this (`ModelVertex`).

**The bone weights are dead data here** — one bone, weight 1. Read past them.

#### The fixup table, which is the one real algorithm in the format

`Studio_LoadVertexes` (`studio.h:3776`) — an `inline` function in a header, which is why
it does not appear in any `.cpp` inventory. The vertex pool is stored **sorted by LOD**;
the fixup table re-establishes **mesh order**. Each `vertexFileFixup_t` (`studio.h:2380`)
is `{ i32 lod; i32 sourceVertexID; i32 numVertexes; }`.

```
if numFixups == 0:
    take the first numLODVertexes[root_lod] vertices verbatim
else:
    target = 0
    for each fixup in table order:
        if fixup.lod < root_lod: continue        // skip higher-detail LODs
        copy numVertexes vertices from sourceVertexID to target
        target += numVertexes
```

The tangent array is permuted by the **same** table, in lockstep. Permuting one and not
the other is a bug that produces correct silhouettes with wrong lighting.

**Measured: only 15 of 968 models have a fixup table at all — and 14 of those 15 are a
genuine permutation even at `root_lod = 0`.** So the table is rare enough to be easy to
forget and not rare enough to skip: get it wrong and 14 models are visibly scrambled while
954 are perfect, which is a maximally confusing failure. `studiomdl` only writes a table
when a model has multiple meshes *and* multiple LODs (`utils/studiomdl/write.cpp:4382`),
which is why it is rare.

### 4.4 `.vtx` — the index buffer

`optimize.h` is 264 lines and all of it is `#pragma pack(1)`. The nesting mirrors the
`.mdl`'s, one level deeper:

```
FileHeader_t (36 bytes)
  BodyPartHeader_t[numBodyParts]        (8)   ← must match the .mdl's body part count
    ModelHeader_t[numModels]            (8)
      ModelLODHeader_t[numLODs]         (12)
        MeshHeader_t[numMeshes]         (9)   ← must match the .mdl model's mesh count
          StripGroupHeader_t[]          (33)
            Vertex_t[numVerts]          (9)
            u16[numIndices]
            StripHeader_t[numStrips]    (35)
```

Every offset is relative to the struct containing it, as in the `.mdl`.

`FileHeader_t::checkSum` must equal the `.mdl`'s and the `.vvd`'s — that is the format's
own guard against a mismatched trio, and it is worth enforcing, because a stale `.vtx`
against a fresh `.mdl` produces indices pointing at the wrong vertices rather than an
error.

### 4.5 The one indirection that decides whether anything draws

`Vertex_t::origMeshVertID` is a **`u16`**, relative to the mesh. The absolute index into
the `.vvd` pool is:

```
vvd_index = model.vertexindex_in_pool + mesh.vertexoffset + vertex.origMeshVertID
```

where `mesh.vertexoffset` is `mstudiomesh_t::vertexoffset` and the model's own base comes
from accumulating `mstudiomodel_t::numvertices` across the model list. Getting this wrong
does not error — it draws a recognisable model with its vertices shuffled, or a different
mesh's geometry entirely.

Because `origMeshVertID` is 16 bits, **a single mesh cannot exceed 65,536 vertices**;
the largest model in the set is 52,478 vertices *in total* across its meshes, so this is
not a constraint in practice, but it is why the pipeline is per-mesh rather than
per-model.

---

## 5. Lighting

A static prop gets its light from two independent places, and `materials` stage 6 already
built the destination for both.

### 5.1 The ambient cube — from the map

`MaterialLightingState_t` (`imaterialsystem.h:271`) is
`{ Vector m_vecAmbientCube[6]; Vector m_vecLightingOrigin; int m_nLocalLightCount;
LightDesc_t m_pLocalLightDesc[4]; }` — which is field for field
`materials::uniforms::ModelLighting` (`src/materials/uniforms.rs:379`) with its
`ambient_cube[6]`, `lights[4]` and `count`. That correspondence is not a coincidence;
stage 6 was written against this struct.

The cube comes from the map's per-leaf ambient samples, which `bsp.rs` does not read yet:

| Lump | | Portal 2 `sp_a1_intro1` |
|---|---|---|
| `LUMP_LEAF_AMBIENT_LIGHTING_HDR` | 55, v1, 28-byte `dleafambientlighting_t` | 8,325 samples |
| `LUMP_LEAF_AMBIENT_INDEX_HDR` | 51, 4-byte `dleafambientindex_t` | 2,038 entries |
| `LUMP_LEAFS` | 10, v1 | 2,038 leaves |

`dleafambientlighting_t` (`bspfile.h:967`) is a `CompressedLightCube` (six
`ColorRGBExp32`) plus `x,y,z` fixed-point fractions of the leaf bounds — so a leaf holds
*several* samples at known positions and the runtime interpolates between them by distance
from the lighting origin. The LDR lumps carry exactly one sample per leaf (2,038); the HDR
lumps carry 8,325, which is the interpolation actually being used. **Read the HDR pair**,
consistent with `bsp.rs`'s existing `lighting_is_hdr` decision.

Decoding is `TexLightToLinear`, not `ColorRGBExp32ToVector` — `rustdocs/MATERIALS.md`
gotcha #2 already records why, and this is a second caller of it.

### 5.2 The per-vertex baked colour — from the pak lump, which is not mounted

`vrad` bakes a colour per vertex per prop and writes it as a **separate file inside the
`.bsp`'s `LUMP_PAKFILE`**, named `sp_hdr_<prop index>.vhv`. Measured on `sp_a1_intro1`:
1,120 pak entries, of which **1,080 are `.vhv`, indices 0..1079 contiguous, against
exactly 1,080 prop instances.** One per prop, indexed by position in the prop array.
`sp_a2_bts2` has 673 for 673, `mp_coop_doors` 159 for 159.

The format is `HardwareVerts::FileHeader_t` (`public/materialsystem/hardwareverts.h`),
`#pragma pack(1)`, 40 bytes:

```c
i32 m_nVersion;        // 2
u32 m_nChecksum;       // must match the .mdl
u32 m_nVertexFlags;    // 0x4 observed
u32 m_nVertexSize;     // 4 — one RGBA8 per vertex
u32 m_nVertexes;
i32 m_nMeshes;
u32 m_nUnused[4];
```

then `MeshHeader_t[m_nMeshes]` (28 bytes: `lod`, `vertexes`, `offset`, 4 unused), each
`offset` **absolute from the start of the file**. The data is 512-byte sector aligned —
`sp_hdr_0.vhv` is 7,680 bytes for 1,676 vertices × 4, with the payload starting at 512.

This is `ModelLighting::static_light`'s reason for existing: stage 6 built the flag that
distinguishes "black because unlit" from "black because absent", and this is the stream it
switches on.

**It is unreachable today.** `rustdocs/FILESYSTEM.md` records `.bsp` pak lumps as deferred,
and `CLAUDE.md` blames the same gap for the 8 cubemap materials that draw as
checkerboards. Mounting the pak lump is now a prerequisite for two subsystems rather than
a nicety for one, and it is small: the lump is an ordinary zip (43.6 MB in
`sp_a1_intro1`), so it is a `Mount` implementation over a zip reader, ordered ahead of the
game's VPKs.

### 5.3 The shaders these props actually want

Measured over the 466 distinct materials the 968 models reference:

| Shader | Materials | Ported? |
|---|---:|---|
| `VertexLitGeneric` | 429 | **yes** — stage 6 |
| `Patch` | 17 | yes — `Vmt` patch chains |
| `Refract` | 12 | no |
| `UnlitTwoTexture` | 5 | no |
| `UnlitGeneric` | 2 | **yes** — stage 3 |
| `Black` | 1 | no |

So **448 of 466 resolve to a ported shader.** Of the 429 `VertexLitGeneric`, **108 carry
`$phong 1`** and therefore take `WantsPhongShader`'s branch to the unported
`DrawPhong_DX9` — they draw without specular and warn once, which is the behaviour
`CLAUDE.md` already documents for models generally. The remaining 321 are fully covered.

---

## 6. Corrections owed to other documents

Fix these when this module lands; they are wrong now and will mislead a cold session.

- **`CLAUDE.md`** ("Next", candidate 3) and **`portdocs/MATERIALSYSTEM.md` §9** both name
  `R_StudioSetupLighting` as the missing piece. It does not exist in this tree (§0.2).
  Replace with `CModelRender::ComputeStaticLightingState` / `LightcacheGetStatic`.
- **`CLAUDE.md`** says what is missing is "a `.mdl`/`.vvd`/`.vtx` reader, the `.bsp`'s
  `sprp` game lump, and `R_StudioSetupLighting`". The first two are right; it omits that
  the reader's reference implementation is `datacache/mdlcache.cpp`, a module no doc
  inventories, and that the per-prop lighting needs the pak lump.
- **`portdocs/ENGINE.md` §7.16** files `staticpropmgr.cpp` under the renderer front-end
  bound for `render/`. The instance list is map data and belongs with `world/` (§7.1);
  only the *draw* half is `render/`'s.
- **`PORTING.md`'s Status section is stale** relative to the last two commits: it records
  `engine` as "4 of 14 modules" with no `trace/`, and `client/` at stages 1–3. `CLAUDE.md`
  has the current picture (6 of 14, `trace/` stage 1, walking, `materials` stage 6).
  `PORTING.md` is declared the source of truth, so this inversion should be repaired.

---

## 7. The Rust design

### 7.1 Module layout

```
src/studio/            the asset — sibling of src/materials/, which it names
  mod.rs               StudioModel, load(), the public surface
  mdl.rs               .mdl  — studiohdr_t, body parts, meshes, material resolution
  vvd.rs               .vvd  — the vertex pool and the fixup table
  vtx.rs               .vtx  — strip groups and indices
  build.rs             the join: (mdl, vvd, vtx) -> per-material mesh batches
  vhv.rs               .vhv  — per-instance baked vertex colour

src/engine/world/props/
  mod.rs               PropDict, Prop, the instance list, transforms
  lump.rs              the sprp game lump reader
  light.rs             the leaf ambient cube -> ModelLighting
```

**Why `src/studio/` is top-level rather than under `engine/`.** It reads three asset
formats and produces GPU meshes and material handles, and it needs no map, no engine and
no window to do it — the same shape as `materials/`'s `Vtf` and `Vmt`. Putting it under
`engine/` would mean the `.mdl` reader's tests need an `Engine`. It names `filesystem/`
and `materials/` and nothing else, which is what makes it testable in isolation.

**Why the instances are `world/`'s.** A prop's placement comes out of the `.bsp`, is
meaningless without one, and dies with the map — exactly like the lightmap pages `world/`
already packs. `world/` : `studio/` reproduces `world/` : `materials/`.

### 7.2 Types, roughly

```rust
/// One .mdl/.vvd/.vtx trio, resolved and ready to draw.
pub struct StudioModel {
    pub name: String,
    /// `view_bbmin`/`view_bbmax` — render bounds in model space.
    pub bounds: (Vec3, Vec3),
    /// `illumposition` — where lighting is sampled when the prop names no origin.
    pub illum_position: Vec3,
    pub flags: StudioFlags,
    /// LOD 0 only for now; `Vec<Lod>` when LOD selection lands.
    pub batches: Vec<Batch>,
    pub vertex_count: u32,
}

/// One (material, index range) draw. This is `world/`'s per-(material, page) batch
/// with the lightmap page dropped — the same idea, and deliberately the same word.
pub struct Batch {
    pub material: MaterialHandle,
    pub first_index: u32,
    pub index_count: u32,
}
```

The vertex buffer is `materials::mesh::ModelVertex`, which already exists and already
carries position, normal, tangent, texcoord and the baked colour.

### 7.3 The seams

- `studio/` → `materials/`: `MaterialCache::get`, `ModelVertex`, a static
  `mesh::Buffer`. No new material-system API is needed; stage 6 built it.
- `studio/` → `filesystem/`: three `Vfs::read` calls per model.
- `world/props/` → `studio/`: `StudioModel::load(vfs, materials, name)`.
- `world/props/` → `materials/`: fills `ModelLighting` per instance.
- Nothing depends on `studio/` in the other direction.

---

## 8. Staged plan

Ordered so each stage is verifiable on its own, and so the first thing on screen comes
before the lighting that makes it correct.

### Stage 1 — the three readers and the join *(the bulk of the work)* — **DONE**
`mdl.rs`, `vvd.rs`, `vtx.rs`, `build.rs`. No GPU, no map, no engine. Produces a
`StudioModel` with per-material batches over a `Vec<ModelVertex>`.

Verified by: parsing all 968 static-prop models out of `pak01` in a test and asserting
the invariants §3 measured — one bone, trilist strips, mesh/body-part counts agreeing
between `.mdl` and `.vtx`, checksums agreeing across all three files, and every
`origMeshVertID` in range. That test is a strong one precisely because the answers are
already known.

### Stage 2 — the `sprp` lump — **DONE**
`bsp.rs` grows game-lump support (a directory, then a `sprp` reader), and
`world/props/` grows the dictionary and instance list with their `AngleMatrix`
transforms (`staticpropmgr.cpp:579` — Valve's pitch/yaw/roll order, which `glam` does not
give for free). Verified against the measured 136/1,080 for `sp_a1_intro1`.

### Stage 3 — draw them — **DONE**
Upload each distinct model once, draw once per instance with a per-instance transform,
under `VertexLitGeneric`. Lighting is a flat ambient cube at this stage — deliberately
wrong, and the point is that 1,080 props appear in the right places with the right
materials. This is where the winding question (`rustdocs/ENGINE.md` gotcha #1) has to be
answered a second time; `.vtx` indices are D3D-wound like the world's.

### Stage 4 — the pak lump, and real per-vertex lighting — **DONE**
`filesystem/` gains a zip `Mount` over `LUMP_PAKFILE`; `vhv.rs` reads
`sp_hdr_<i>.vhv` into the colour stream and sets `ModelLighting::static_light`. **Also
fixes the 8 cubemap materials** that draw as checkerboards today, which is a second
subsystem's bug closed by the same change.

### Stage 5 — the ambient cube — **DONE** (out of order; it is independent of stage 4)
`bsp.rs` reads the leaf-ambient pair; `world/light.rs` finds the leaf for a prop's
lighting origin, interpolates the samples, decodes with `ColorRGBExp32ToVector` (see
§11.2 — this plan said `TexLightToLinear` and was wrong), and fills
`ModelLighting::ambient_cube`. After this a prop is lit the way the shipped game lights
it, less the local lights — **which §13 supplies**.

### Stage 6 — LOD selection and fade *(optional, and small)* — **not started**
`switchPoint` per LOD, `m_FadeMinDist`/`m_FadeMaxDist`/`m_flForcedFadeScale` per prop.
819 of 968 models have one LOD, so this is a performance stage, not a correctness one, and
it is the right place to stop unless profiling says otherwise.

**Not in this plan, deliberately:** `.phy` collision (that is `ENGINE_TRACE.md` stage 5),
the prop leaf lists (they are a PVS structure and wait for visibility), decals, and every
animated-model concern in §3.

---

## 9. Open questions and risks

- **Where does the per-instance transform live?** `world/`'s existing batches are
  pre-transformed into world space at load. Doing that for props would multiply the
  vertex data by 56,955/968 ≈ 59× and is obviously wrong; so props need a per-draw
  model matrix, which the world path does not currently have. Whether that is a new
  uniform slot or an instance buffer is a `materials/` question that stage 3 has to
  settle — and **`rustdocs/MATERIALS.md`'s "per-draw constants need distinct arena
  slots"** gotcha applies directly.
- **Does one vertex buffer per model or one per map win?** 2.4M vertices across the whole
  game, but only 136 models in a typical map. One buffer per model is simpler and probably
  fine; one buffer per map is fewer bindings. Measure at stage 3 rather than guessing.
- **The 8 empty meshes** (zero strip groups) and the **1 strip with flags 0** are
  content oddities that a strict reader would reject. Skip them; do not error.
- **`TRANSLUCENT_TWOPASS` on 38 models** needs a sorted translucent pass, which this port
  does not have at all yet. They will draw wrong (unsorted) until it does. Acceptable at
  stage 3; record it.
- **Whether the leaf-ambient interpolation matters visually.** The HDR lumps carry 4×
  more samples than leaves, so Valve clearly thought so, but a single-sample-per-leaf
  approximation is much simpler and might be indistinguishable for props. Try the simple
  one at stage 5 and look.

---

## 10. Notes for whoever picks this up

- **The measurements in §3 and §4 are the most valuable thing in this document.** They
  were cheap to take (a ~60-line Python VPK reader and a `.bsp` game-lump parser) and they
  converted five open scoping questions into settled ones. If a later stage raises a
  similar question — how many models use X, does any map do Y — take the measurement; the
  depot is right there.
- **Read `rustdocs/MATERIALS.md` before stage 3.** Three of its five gotchas apply to this
  path unchanged: column-major matrices, per-draw arena slots, and the `near`/`far` sign.
- The reference for the *load* is `datacache/mdlcache.cpp`; for the *join* it is
  `Studio_LoadVertexes` in `public/studio.h:3776` and `r_studiodraw.cpp`; for the
  *instances* it is `engine/staticpropmgr.cpp`; for the *lighting* it is
  `engine/lightcache.cpp`. Nothing useful is in `studiorender/studiorender.cpp` — that is
  the `IShaderAPI` tower.
- `legacy/` is latin-1. The `Grep` tool handles it; shell `grep` needs `-a` or it reports
  these files as binary and finds nothing. This document's searches all used `-a`.


---

## 11. What the implementation found

Written after stages 1, 2, 3 and 5 landed. The API reference is
`rustdocs/STUDIO.md`; this section records only what changes something *this*
document said.

### 11.1 Two `.vtx` field offsets in §4.5 were wrong, and the synthetic tests could not tell

`OptimizedModel::FileHeader_t`'s fields are not evenly spaced —
`maxBonesPerStrip` and `maxBonesPerFace` are `unsigned short` at 8 and 10, so
`checkSum` is at **16**, `numLODs` at 20, `numBodyParts` at 28 and
`bodyPartOffset` at 32. And `Vertex_t::origMeshVertID` is at **4**, after
`boneWeightIndex[3]` and `numBones` and before `boneID[3]`.

The first draft had both wrong and **all 18 synthetic tests passed**, because
the fixture had been written from the reader rather than from `optimize.h`. Every
one of the 2,041 shipped models failed to parse the moment the depot test ran.

The lesson generalises past this module: **a format fixture written from the
reader tests nothing about the format.** Write it from the header, and treat a
depot pass as the acceptance criterion for a format reader rather than as a
bonus.

### 11.2 The ambient cube's decode is the *opposite* of the lightmap's

§5 assumed `TexLightToLinear` throughout, on the strength of
`rustdocs/MATERIALS.md`'s lightmap gotcha. It is wrong for this lump:
`Mod_LeafAmbientColorAtPos` calls `ColorRGBExp32ToVector`
(`modelloader.cpp:7338`), which is 255× larger, and the two are correct in
their own places because they reach the GPU by different routes.

Settled by measurement rather than by argument, since this port has one linear
space that both shaders sample: over `sp_a1_intro1`, mean luminance under
`TexLightToLinear` is 0.0249 for the lightmap lump and **0.0002** for the
ambient cubes — 122× apart, which the 255 closes to 0.5×. Props decoded the
lightmap's way are black.
`props::tests::every_shipped_map_places_its_props` prints both numbers.

### 11.3 §9's open questions, answered

- **Where does the per-instance transform live?** It was already built:
  `Pass::draw` takes a `Mat4` model matrix, because `MATERIAL_MODEL` was always
  a matrix and never a bake. No new `materials/` API was needed.
- **One vertex buffer per model or per map?** Per model, and it is not close:
  `sp_a1_intro1` is 136 models against 1,080 instances and the whole game is
  968 against 56,955. Per-map baking would multiply the vertex data by 59×.
- **Does the leaf-ambient interpolation matter?** Kept — it is six lines and it
  is what makes two props in one room light differently.
- **The 8 empty meshes** are skipped rather than refused, as planned.

### 11.4 One thing §4 did not anticipate: 32-bit indices

`models/stars/allstars.mdl` is flagged `STATIC_PROP` and has **187,676
vertices**, almost three times what a `u16` can name. Valve never needs
`MATERIAL_INDEX_FORMAT_32BIT` because `CMeshDX8` splits a model into
sub-buffers; this port added `IndexBuffer::new_u32` and a format on
`IndexSlice` instead, which is the same GPU work and none of the bookkeeping.

### 11.5 A second error material

This port picks a vertex layout per *shader*; Valve picked it per `$model`
flag on one material. So the checkerboard had to become two materials —
`MaterialCache::error_material` (`UnlitGeneric`, brush vertices) and
`error_model_material` (`VertexLitGeneric`, model vertices) — from the same
three keys, so a broken prop shows the same checkerboard a broken brush face
does.


### 11.6 Stage 4's real difficulty was not the ZIP

The pak lump is a plain ZIP and **every one of the 64,428 entries in the shipped
game is stored uncompressed**, so it needed no decompressor and no new
dependency — 260 lines and done. The `.vhv` header is equally simple.

What was hard is a thing neither `hardwareverts.h` nor `l_studio.cpp` says out
loud: **a `.vhv` is written in *hardware* vertex order.** Valve's runtime
compacts a model's vertices per LOD — `studiomeshgroup_t` holds exactly the
vertices that LOD's strips reference, in the order the `.vtx` strip-group
`Vertex_t` tables list them — and `vrad` bakes against that numbering. This port
does not compact: it uploads the whole `.vvd` pool and indexes into it. The two
numberings coincide for a single-LOD model and diverge for everything else.

Read as a run over the pool, that lights **125 of `sp_a1_intro1`'s 1,080 props
from the wrong vertices — and silently appears to work for the other 955.** It
was caught only by checking the per-mesh vertex counts against the whole depot,
where 125 refused to line up at all. Two smaller rules came out of the same
check: `vrad` writes **no block for an empty mesh** (eight meshes, five blocks on
`models/npcs/turret/turret_debris_lrg`), and **the checksum cannot be enforced**
— `r_ignoreStaticColorChecksum` defaults to 1 and 24 of the game's 56,801 files
need it to.

`StudioModel::meshes` (`HardwareMesh`) exists for exactly this and nothing else.

### 11.7 The per-placement stream forced a second vertex buffer

`ModelVertex` lost its `color`, and `VertexLayout::Model` became two buffers with
`StaticLightVertex` (`Unorm8x4`) in slot 1. That is Valve's design —
`m_pColorMeshData` is a separate `IMesh` and `VERTEX_COLOR_STREAM_1` is what
names it — and it is forced by arithmetic rather than by fidelity: folding the
colour into the geometry means uploading a model once per placement, which is
2.3 million vertices instead of 284 thousand on `sp_a1_intro1` and 59× across
the game.

`Pass::bind_static_light` is the binding, and every model draw needs one — a
prop with no `.vhv` binds a shared black stream and clears
`ModelLighting::static_light`.

### 11.8 The frame cost, and what it cost to find

Drawing 1,080 props cost **12.3 ms of CPU a frame** in a release build when
stage 3 landed — a 78 fps ceiling before the GPU does anything, and unplayable
in a debug build. None of it was visible from outside the process: macOS stops
delivering redraws to an occluded window, so `sample` shows a main thread parked
in `mach_msg`. `engine::world::bench` exists to make it measurable — a real map,
a real device, no window — and it is the thing to reach for before and after any
change to the draw path.

Three causes, in order of size:

1. **One `Queue::write_buffer` per draw.** It is not a memcpy: each call takes a
   staging-belt chunk, maps it, and records a copy. At one per draw plus one per
   instance for lighting, that was ~3,300 calls a frame. Now staged into a CPU
   mirror and flushed once per arena per pass.
2. **Redundant pipeline and bind-group state.** Every instance of a model re-set
   the same pipeline, material and buffers. `Pass` now elides what has not
   changed, and props draw **batch-major** so a model's state is set once per
   batch rather than once per instance.
3. **An O(models x instances) scan** to find each model's instances, redone every
   frame. Precomputed at load.

Result on `sp_a1_intro1`: **12.74 ms -> 0.86 ms** for the whole scene, and the
brush path got 2.8x faster with it. Stage 4's per-instance stream brought it back
to **1.00 ms**, which is the cost of the feature and is paid for. (Run the three
sub-benchmarks separately: back to back they share thermal state and read about
twice as high.)

A fourth cause was not in this port's code at all: a debug build leaves `wgpu`
unoptimised, and almost all of a frame's CPU time is inside it. `[profile.dev.package."*"] opt-level = 3`
takes the debug frame from 37 ms to 6.6 ms and costs nothing at the debugger.

---

## 12. `$includemodel`, which was not on the plan

> Written **after** the port, like `portdocs/CLIENT_TONEMAP.md` and for the
> same reason: this is not one of §8's six stages. It arrived as a measurement
> that `prop_dynamic` took — nine of the 606 models the game's props name keep
> their animation somewhere else, and **926 entities wear one** — so the
> analysis below is the justification for the shape `src/studio/include.rs`
> has rather than a plan to follow. The API is `rustdocs/STUDIO.md`.

### 12.1 What the C++ does

`studiohdr_t::numincludemodels` / `includemodelindex` (`studio.h:2767`) is an
array of `mstudiomodelgroup_t` (`studio.h:662`), which is two struct-relative
offsets — a label nothing reads and a **file name**, a plain model path that
`FindModel` hands straight to `CMDLCache::FindMDL`.

`virtualmodel_t::AppendModels` (`public/studio_virtualmodel.cpp:79`) walks that
list depth first, adding one `virtualgroup_t` per file, and each group carries
five remapping tables:

| Table | Maps | Ported |
|---|---|---|
| `masterBone` | included bone -> host bone, matched by **name** | **yes**, and it is the whole difficulty |
| `boneMap` | host bone -> included bone | no — only used to seed an unanimated bone from the sequence model's bind pose, and the two bind poses are identical (§12.3) |
| `masterAnim` | included animation -> merged animation | **yes**, as the index the merged sequence is rewritten to hold |
| `masterSeq` | included sequence -> merged sequence | no — it exists for `iRelativeSeq`, which serves `nextseq` and autolayers, and neither is ported |
| `masterAttachment` / `masterPose` / `masterNode` | the rest | no — none of those subsystems exists here |

Valve keeps the included `studiohdr_t`s **separate** and indirects through the
tables on every `pSeqdesc`, `pAnimdesc` and `CalcVirtualAnimation` call,
because they are cache entries that can be evicted independently. Nothing in
this port can be evicted — a `StudioModel` is an owned value — so the groups
collapse and the remap is applied once, at load. That deletes `masterSeq`,
`boneMap`, `CModelLookupContext`'s string-table optimisation, the thread mutex,
the `HandleAndHeader_t list[64]` stack guard and the whole lazy-resolution
path: `GetNumSeq_Internal`, `pSeqdesc_Internal`, `pAnimdesc_Internal`,
`iRelativeAnim_Internal` and `iRelativeSeq_Internal` all exist only to hide the
indirection and have nothing left to hide.

The one line to carry across exactly is in `CalcVirtualAnimation`
(`bone_decode.cpp:1397`):

```cpp
j = pAnimGroup->masterBone[panim->bone];
…
CalcBoneQuaternion( iLocalFrame, s, &pAnimbone[panim->bone], …, q[j] );
```

The RLE stream is decoded against the **animation file's** bone — its
`posscale`, `rotscale` and bind-pose `rot`, which are the numbers its
fixed-point values were written against — and the *result* is stored at the
**host's** bone index. Parsing the include as its own file gives the first half
for free; the second half is `masterBone`.

### 12.2 The measurements that scoped it

Over the shipped depot, and over the search path `portal2/gameinfo.txt`
actually mounts:

- **25 models declare an `$includemodel`** and **24 of those companions can be
  read**. The three that cannot are all `models/props_lab/bot_male.mdl`'s
  (`bot_male_animations`, `_gestures`, `_postures`), which are in no VPK — so
  `FindModel` returns null and that model keeps its seven local sequences.
  (A 26th host, `models/info_character/info_character_player.mdl`, is in
  `portal2_dlc2`, which this mount does not reach.)
- **Nothing nests.** No included model includes anything, so the recursion is
  there because Valve's is.
- **Nine of the 25 are placed by a `prop_dynamic`, by 926 entities**, and
  **898 of those 926 are the two `anim_wp/room_transform` panel arms**, both
  including the same `arm64x64_interior_animation.mdl`.
- **270 distinct `DefaultAnim` labels — 849 entities' worth — resolve only
  through an include.** Another 183 entities name a label that is in no model
  at all, which is Valve's own map errors and not this feature's to fix.
- **`STUDIO_OVERRIDE` is set on 0 of the game's 7,885 sequences**, so
  `AppendSequences`' "the one in memory is a forward declaration, replace it"
  branch is unreachable and is not ported.
- **Sequence bone weights are 1 everywhere it matters.** Across the nine
  companions a prop model includes, 85 sequences carry a zero `pBoneweight` —
  44 in `eggbot_animations`, 36 in `ballbot_animations`, 5 in
  `player_animations` — and **none at all** in the two panel-arm files, the
  personality sphere's or the Wheatley boss's. Every model that has one is
  drawn in its bind pose here anyway, for want of skinning. Not ported; see
  §12.4.

### 12.3 The finding that made the merge simple

`CalcVirtualAnimation` seeds a bone the animation does not mention from the
**sequence model's** bind pose (`pSeqbone[ boneMap[nBone] ]`), where
`anim::pose` seeds it from the host's. Those are different models, so on paper
this is a divergence.

Measured, it is not: across all nine hosts, every bone that matches by name has
a bind `pos` within **4e-6 units** and a bind `quat` within **0** of its
counterpart's. An animation-only `.mdl` is compiled from the same skeleton as
the model it animates, and `studiomdl` writes the same numbers. So `boneMap`
buys nothing here and is not built — and the depot test proves the equivalence
rather than assuming it, by posing the host with the merged animation and the
companion with its own, and comparing all sixteen bones **by name** at three
cycles.

### 12.4 What is still missing, and what would make it worth having

- **External `.ani` animation blocks.** This is now the binding constraint
  rather than `$includemodel`. The nine prop models name **eight distinct
  companions** (both panel arms share one), and only two of the eight keep
  their animations inline: `arm64x64_interior_animation` has all 1,350 of
  them, and `personality_sphere_animation` 313 of 318. The other six —
  `eggbot_animations`, `ballbot_animations`, `player_animations`,
  `headless_player_animations`, `headless_s8player_animations` and
  `glados_wheatley_boss_animation` — keep almost all of theirs in a companion
  `.ani`, so their labels now *resolve* and their poses are empty (1, 1, 0, 0,
  0 and 2 animations with data respectively).

  > **Skinning has since landed, which spends this paragraph's argument.**
  > Until it did, all six — and the personality sphere — were models this port
  > drew in its bind pose whatever their animation said, so reading `.ani`
  > changed no pixel for them and the panel arms were the whole visible payoff
  > of the merge. All nine can be posed now, and what they are missing is the
  > animation data itself, so `.ani` is the next thing worth doing here.
- **Sequence bone weights** (`mstudioseqdesc_t::weightlistindex`): 85
  sequences, all on the same three models above. `pose` takes an `Animation`
  and not a `Sequence`, so this would change its signature; the condition is a
  model that both needs the weights and has animation data to pose with.
- **One parse per host, not per file.** Both panel arms include the same 1.3 MB
  companion, so a map placing both reads and expands it twice — about 50 ms and
  7 MB each, at level load and not per frame. Valve gets sharing free from
  `CMDLCache`; the equivalent here is a cache above `StudioModel::load`, and the
  condition for writing one is a map whose load time is actually a problem.

---

## 13. The local lights, which §5 left out

> Written **after** the port, like §12 and `portdocs/CLIENT_TONEMAP.md`, and
> for the same reason: it is not one of §8's six stages. §5 split a prop's
> lighting into "the ambient cube" and "the per-vertex bake" and said local
> lights were "not ported — that needs `LUMP_WORLDLIGHTS` and the attenuation
> model". This is that, plus the thing §5 did not see: **the two terms it
> named do not add together.** The API is `rustdocs/ENGINE.md`, "world::light".

### 13.1 What §5's split got wrong

§5's table has two independent terms. The shipped engine has two *alternatives*
and a third term, and the choice between them is one `bool`:

```c
// CModelRender::DrawModelExStaticProp, l_studio.cpp:3046
bool bStaticLighting = (( drawFlags & STUDIORENDER_DRAW_STATIC_LIGHTING ) &&
    ( pStudioHdr->flags & STUDIOHDR_FLAGS_STATIC_PROP ) &&
    ( !bUsesBumpmapping || numLightingComponents > 1 ) &&
    ( pInfo.instance != MODEL_INSTANCE_INVALID ));
```

and then, in `StudioSetupLighting` (`l_studio.cpp:1430`):

```c
if ( bStaticLighting )
    pLightingState = LightcacheGetStatic( *pLightcache, &pEnvCubemapTexture,
                                          LIGHTCACHEFLAGS_DYNAMIC | LIGHTCACHEFLAGS_LIGHTSTYLE );
else
    lightingState = *(LightcacheGetStatic( *pLightcache, &pEnvCubemapTexture ));
    //                          default flags: STATIC | DYNAMIC | LIGHTSTYLE
```

`LIGHTCACHEFLAGS_STATIC` is what decides whether `LightcacheGetStatic` starts
from `m_StaticLightingState` or from `ZeroLightingState()`. So a prop that uses
its `.vhv` gets **a zeroed ambient cube and no local lights** — the bake is
everything — and one that does not gets the light cache and no bake.

This port had been adding the leaf ambient cube on top of the bake since stage
5, which double-counts a prop's indirect light. It did not read as a bug
because both terms are dim and the tone mapper absorbs the difference.

`numLightingComponents` is `r_staticlight_streams`, `"1"`
(`vertexlitgeneric_dx9_helper.cpp:54`), so the term is always false and
`bUsesBumpmapping` alone decides. That flag is
`STUDIOHDR_FLAGS_USES_BUMPMAPPING`, ORed over a model's materials by
`CStudioRenderContext::ComputeModelFlags` (`studiorendercontext.cpp:274`): a
`$bumpmap` that needs tangent space, **or `$phong` non-zero on its own**. The
second is a wider net than `WantsPhongShader` casts, so it is read from the
`.vmt` rather than from `ShaderKind`.

There is no third possibility to worry about:
`STUDIOHDR_BAKED_VERTEX_LIGHTING_IS_INDIRECT_ONLY`, which would ask for *both*,
is set only when `r_staticlight_streams == 3` or
`r_staticlight_streams_indirect_only` is true, and neither is reachable from a
shipped Portal 2 configuration.

**On `sp_a1_intro1`: 816 baked, 246 per-pixel, 18 with no file at all.**

### 13.2 The lump

`LUMP_WORLDLIGHTS` (15) and `LUMP_WORLDLIGHTS_HDR` (54), `dworldlight_t`
(`bspfile.h:1101`), 100 bytes at version 1 and 88 at version 0. Measured over
the depot: **106 maps, 14,246 lights, every one of them version 1 in the HDR
lump**, so the version-0 widening path has no data to exercise it and is
refused rather than written — the same decision `LUMP_LEAFS` already took.

| | count | |
|---|---|---|
| `emit_surface` | 7,073 | one per emissive face; `vrad` makes them |
| `emit_point` | 4,651 | |
| `emit_spotlight` | 2,470 | |
| `emit_skylight` | 28 | |
| `emit_skyambient` | 24 | ignored at runtime, before anything else |
| `emit_quakelight` | 0 | and it has no hardware form anyway |

Two numbers decide the shape of the port:

- **6,731 of the 7,073 `emit_surface` lights carry `DWL_FLAGS_INAMBIENTCUBE`**,
  which is `vrad` saying it already folded them into the per-leaf cubes
  (`leaf_ambient_lighting.cpp:179`). §13.3 is why that matters.
- **14,237 of the 14,246 have no radius**, so `ComputeLightRadius`
  (`gl_drawlights.cpp:46`) is the normal path rather than an upgrade path — and
  its HDR branch, which halves the cutoff because "usually our designers scale
  the light intensity by 0.5 in HDR", applies to every Portal 2 map.

A third: **213 carry a lightstyle**, across styles 32-37. Those belong to
`LIGHTCACHEFLAGS_LIGHTSTYLE` and are dropped at load, because nothing here
animates `d_lightstylevalue[]`.

### 13.3 The ambient cube this port uses is not the one a static prop gets

`R_StudioGetAmbientLightForPoint` branches on `r_radiosity` (4) *and* on whether
the caller is a static prop:

```c
case 4:
    if (bIsStaticProp)  ComputeAmbientFromSphericalSamples( start, pLightBoxColor );
    else                ComputeAmbientFromLeaf( start, leafID, pLightBoxColor,
                                                bAddedLeafAmbientCube );
```

The two are the *same function*, once in `lightcache.cpp` and once in
`vrad`'s `leaf_ambient_lighting.cpp` — except that `vrad`'s ends with
`AddEmitSurfaceLights`, which adds the direct light of exactly the
`DWL_FLAGS_INAMBIENTCUBE` lights. That is what `bAddedLeafAmbientCube` reports,
and it is what `AddStaticLighting`'s first `continue` reads:

```c
if ( bAddedLeafAmbientCube && (wl->flags & DWL_FLAGS_INAMBIENTCUBE) )
    continue;
```

So the engine's two paths carry the same total energy by different routes: a
static prop gathers bounce at runtime and takes *all* the world lights
including the dim surface ones, and everything else reads `vrad`'s cube — which
already has them — and skips them.

**This port uses the leaf cube for everything** (§5 and §11.2), so
`bAddedLeafAmbientCube` is true here and the 6,731 are skipped. That is a
consistent choice rather than an approximation, and it is also the cheaper one:
the alternative is 162 rays per prop against the lightmaps.

### 13.4 What the selection actually is

`AddStaticLighting` (`lightcache.cpp:1875`) walks every world light and hands
each to `AddWorldLightToLightingState`, which:

1. culls it against the light cache's **32x32x128 grid cell** around the sample
   point — not the prop's own box, which is only used by the dlight path;
2. measures it at the point with `LightIntensityAndDirectionAtPointOld`, which
   ends in **one trace from the point to the light**;
3. ranks it by `ratio * dot( intensity, (0.299, 0.587, 0.114) )`;
4. gives it one of `MIN( MaxNumLights(), r_worldlights )` slots if it is bright
   enough, evicting the dimmest if it must — and **returns**;
5. or, if it got no slot, folds it into the ambient cube by
   `AddWorldLightToLightCube`, which spreads `ratio * angular * intensity` over
   the faces pointing at it.

Steps 4 and 5 are exclusive, which is the part worth stating out loud: **no
light is in both terms and none is lost.** A light that just misses the cut
stops being directional rather than stopping existing.

`r_worldlights` is the number to argue about. The tree has three answers —
4 as designed, 3 "Changed from 4 to 3 for L4D!", and 2 under `#ifdef POSIX`
with "JasonM GL - capping at 2 world lights at the moment" — and this port
compiles on the platform whose answer is 2. Measured: **44,421 of the game's
56,955 props fill both slots**, so the cap binds almost everywhere and is worth
revisiting if the shading ever looks flat.

### 13.5 What it cost, and what the PVS reject would have bought

**1.4 seconds to light all 56,955 static props in the game**, about 13 ms a
map, against 0.25 s for `sp_a1_intro1`'s collision model alone. `vvis` is
conservative, so `FastRejectLightSource` can only reject lights the occlusion
trace would reject anyway; leaving it out is a time cost and not a picture
change, and 13 ms is not a time cost worth writing a PVS reader for. The
`world/` visibility work will bring one for its own reasons, and this can use it
then.

The frame cost did not move: the whole of this is load-time, and what reaches
the GPU is two more iterations of a loop that was already there.

### 13.6 Things that read as bugs and are not

Twelve are listed in `rustdocs/ENGINE.md`; three are worth repeating here,
because they are the ones that would be "fixed" by someone reading only the
reference.

- **`1 / (thetaDot - phiDot)` is 1 when the two are equal, not 0.**
  `WorldLightToMaterialLight` turns every `emit_surface` light into a
  180-degree spotlight with both cone cosines 0 and writes that register as 1
  by hand; `RecalculateOneOverThetaDotMinusPhiDot` does the same for anything
  else, with the comment "hard falloff instead of divide by zero". The shader
  computes `pow( max( 1e-4, (cos - phiDot) * ood ), falloff )`, so a 0 there
  turns 7,073 lights off.
- **A skylight's direction comes back from the last corner tested, not the
  brightest one.** `LightIntensityAndDirectionInBox` takes the maximum of nine
  ratios and leaves `pDirection` holding whatever the ninth call wrote — so a
  cell whose `maxs` corner is in shadow gets a ratio of 1 and a zero direction,
  which the angular term then turns into 0. Reproduced.
- **Eight units of slack decide shadowing**, under a comment reading `// hack`:
  `if ( (1.f - pm.fraction) * dist > 8 )`. It is what lets a light embedded in
  the surface of its own fixture still light the room.
