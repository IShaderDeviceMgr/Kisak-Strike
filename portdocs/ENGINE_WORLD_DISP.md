# `engine/`: displacements, the rendering half — `src/engine/world/disp/`

`portdocs/ENGINE.md` §7.15. The *collision* half landed with
[`ENGINE_TRACE.md`](ENGINE_TRACE.md) stage 3 and is `src/engine/trace/disp.rs`; this is
the other half of the same lumps — turning a `ddispinfo_t` into something the world
draws.

**Status: written before the port; the port has since landed as `src/engine/world/disp/`.**
See [`rustdocs/ENGINE.md`](../rustdocs/ENGINE.md) for the API that resulted and the gotchas
that only became visible while writing it. Two sections were corrected *by* the port and
say so where they stand: §4.4 (the winding, which this doc originally got backwards) and
§6.2 (`$ssbump`, whose reach turned out to be far wider than terrain).

---

## 0. Why this is small, and where the surprises are

A displacement replaces one four-sided world face with a `(2^power + 1)²` grid of
vertices. `trace/` already builds that grid — `disp::build_verts` is a faithful port of
`CCoreDispInfo::GenerateDispSurf` — so the *positions* are a solved problem and this
module is mostly about the four other things a vertex needs: a texture coordinate, a
lightmap coordinate, a blend alpha, and an index list.

Three of those four are one function in the original (`CalcDispSurfCoords`, run once for
texture coordinates and four more times for luxels) and the fourth is where all the
difficulty is. The index list is **not** the two-triangles-per-cell list the collision
tree uses, or rather it is not *obviously* that list: Valve generates render indices by
walking the displacement's quadtree and fanning around each node, gated on a per-vertex
`m_AllowedVerts` bit vector that exists to stop a high-power displacement cracking
against a lower-power neighbour. §4 is that walk, and §4.3 is the finding that makes it
checkable.

The other surprise is not in this module at all: **79% of Portal 2's displacements wear a
shader this port had not ported.** §6.

---

## 1. Inventory

| File | Lines | Disposition |
|---|---|---|
| `engine/disp.cpp` | 1,203 | Mostly **delete**. `CDispInfo`'s decal fragments, dynamic lights, LOD bookkeeping and the `r_Disp*` debug meshes. What survives: `TesselateDisplacement`'s call shape. |
| `engine/disp_mapload.cpp` | 1,009 | **Port the middle third.** `BuildDispSurfInit` (§3.2), `FillStaticBuffer` (§3), the `CDispGroup` batching (§5). Delete `AddEmptyMesh`'s static-buffer suballocation. |
| `engine/disp_interface.cpp` | 1,461 | **Delete.** The `IDispInfo` vtable, decal/shadow projection, and `CDispInfo::Render`'s debug modes. |
| `engine/disp_defs.cpp`, `disp_helpers.cpp` | 60 | **Delete.** |
| `public/builddisp.cpp` | 3,156 | **Port ~200 lines.** `GenerateDispSurf` (already ported to `trace/`), `CalcDispSurfCoords`, `CreateTris`. Delete the editor-side LOD tree, `DispUVToSurf*` and the multiblend path. |
| `public/disp_powerinfo.cpp` | 580 | **Port ~30 lines.** `g_TesselateVerts` and the child-node offsets. The rest builds the dependency graph that `vbsp` uses to *compute* `m_AllowedVerts`, which is already in the file. |
| `public/disp_tesselate.h` | 220 | **Port ~80 lines** — §4. |
| `public/disp_common.cpp` | 1,300 | **Delete.** Neighbour stitching and `SetupAllowedVerts`; `vbsp`'s, and its answer ships in the lump. |
| `public/disp_vertindex.h` | 120 | **Delete.** `CVertIndex` is `(u32, u32)`. |

~9,100 lines of C++; roughly 350 of them have a counterpart here.

**Not this module's, and each already answered elsewhere:** the vertex grid and the AABB
tree (`trace/disp.rs`), the lightmap atlas (`materials/lightmap.rs`), the
(material, page) batching (`world/mod.rs`), the `.bsp` lumps (`world/bsp.rs`).

---

## 2. What the data says

Measured over the 106 shipped maps, with a throwaway survey against the depot. Every
number below decided something in §3–§6.

| | |
|---|---|
| Displacement faces | **1,181**, across **29** of 106 maps |
| Powers | 904 at 2, 202 at 3, 75 at 4 |
| In a brush model | **0** — every one is in model 0, the world |
| Base face not a quad | **0** |
| `SURF_BUMPLIGHT` | **1,069** of 1,181 |
| With lightmap samples | 1,130; the other 51 are the `NOLIGHT` skybox patches |
| With more than lightstyle 0 | 112 |
| With any non-zero vertex alpha | 497 |
| `m_AllowedVerts` with a bit cleared | **100** |
| `lightmap_alpha_start != 0` (multiblend) | **0** |

Shader, by displacement face:

| Shader | Faces |
|---|---|
| `WorldVertexTransition` | **937** |
| `LightmappedGeneric` | 157 |
| `UnlitGeneric` | 51 |
| `maps/<map>/…` cubemap patches | 36 (they resolve from the map's pak lump) |

Three of those rows are the plan:

- **0 in a brush model** — displacements are built for the world model and nowhere else,
  and the door/platform path in `world/mod.rs` needs no change. Valve's
  `DispInfo_LinkToParentFaces` only ever walks `pWorld->brush` too, so this is the file
  agreeing with the engine rather than a lucky map set.
- **100 restricted** — 8.5%, concentrated in `e1912`, `sp_a3_01`, `sp_a3_03`,
  `sp_a3_end`, `sp_a3_transition01` and `sp_a3_crazy_box`. Not ignorable, so §4 ports the
  real walk. **`sp_a1_intro1` has none**, so the reference map does not exercise it and a
  depot test has to.
- **937 `WorldVertexTransition`** — §6.

Per-map, the ones that matter: `sp_a3_end` 201, `sp_a3_transition01` 198, `sp_a3_03` 188,
`sp_a3_01` 186, `sp_a3_portal_intro` 137, `e1912` 77, `mp_coop_fan` 24, `sp_a1_wakeup` 22,
**`sp_a1_intro1` 11**.

Three more numbers arrived *after* the port, from the same survey, and each changed
something:

| | |
|---|---|
| Total render triangles, all 1,181 patches | **92,622** |
| Vertices dropped by `m_AllowedVerts` across the 100 | **511** |
| Triangles that face away from their own base quad | 131 of 92,622 — real overhangs, and why §4.4's anchor is per patch |
| **Non-displacement** faces naming `WorldVertexTransition` | **0** — the shader is terrain's and nothing else's |
| Drawable non-displacement world faces wearing `$ssbump` | **128,139 of 288,250** — see §6.2 |

---

## 3. A displacement's render vertex

`FillStaticBuffer` (`disp_mapload.cpp:283`) writes position, normal, tangent S/T,
texcoord, luxel coord, and a colour whose alpha is the blend factor. Of those, **this
port writes four**: position, texcoord, luxel coord, colour.

**Normals and tangents are deliberately not built**, and it is not a shortcut. The world
vertex layout (`materials::mesh::WorldVertex`) has no normal and no tangent, because
`LightmappedGeneric`'s bumped path dots a *tangent-space* normal against the constant
basis the lightmaps were baked in and never needs a world-space frame — the reasoning is
already written down on `ShaderKind::vertex_layout`. Building them would mean porting
`CalcNormalFromEdges`, `DoesEdgeExist` and `SmoothDispSurfNormals`, the last of which
needs the neighbour tables this doc deletes in §1, to fill two attributes nothing reads.
**The condition that reverses this is `$envmap` or `$seamless_scale`** (§6.3), which are
the two features that make a world surface want a normal.

### 3.1 The grid, and which way it runs

`trace::disp::build_verts` already does this, and the render side must agree with it
exactly or the collision surface and the drawn surface are different surfaces. Restating
it because everything below indexes into it:

```
i runs along p0 → p1   (and p3 → p2)
j runs along p0 → p3   (and p1 → p2)
index = i * spacing + j          spacing = 2^power + 1
```

Valve's `CVertIndex` is `(x, y)` with `InternalVertIndex = y * side + x`, so **`x` is `j`
and `y` is `i`**. The two spellings are the same linear index; §4 uses Valve's because
the tessellation tables are written in it.

`p0..p3` are the base face's four corners **rotated** so that the one nearest
`ddispinfo_t::start_position` is `p0` (`FindSurfPointStartIndex` / `AdjustSurfPointData`).
`trace/` already does the rotation; this module must use the same rotated order for
texture and luxel coordinates, which is why §3.2's corner arrays are indexed after the
rotation and not before.

### 3.2 Texture and lightmap coordinates

`CCoreDispInfo::CalcDispSurfCoords` (`builddisp.cpp:1592`) is one bilinear interpolation
over four corner values, run once with the base face's texture coordinates and again with
its luxel coordinates. In the `(i, j)` spelling above, for corner values `c0..c3`:

```
c(i, j) = lerp( lerp(c0, c1, i/n), lerp(c3, c2, i/n), j/n )      n = spacing - 1
```

**Texture coordinates are the base face's four corner coordinates, interpolated in grid
space — not the planar projection evaluated at the displaced position.** A displaced
vertex is metres away from the flat quad it came from, and projecting it through the
texinfo's texture axes gives a visibly different, stretched mapping. `Bsp::texture_
coordinate` is therefore called four times, at the *flat corners*, and never at a grid
vertex.

**Luxel coordinates are not the base face's at all.** `BuildDispSurfInit`
(`disp_mapload.cpp:165`) computes them from the face's luxel corners and then, twenty
lines later, throws that away and overwrites lightmap 0's four corners with a canonical
square:

```
corner 0 → (0,     0    )
corner 1 → (0,     height)
corner 2 → (width, height)
corner 3 → (width, 0    )        width, height = dface_t::lightmap_size, the *extents*
```

each `+ 0.5`, then scaled and offset into the atlas page. Valve's own comment calls this
"currently done redundantly … here to get things running for (GDC, E3)", but it is what
`vrad` bakes against: a displacement's lightmap is parameterized by the **grid**, not by
the texinfo's lightmap axes. Substituting the bilinear form into the canonical corners
collapses it to

```
luxel(i, j) = ( 0.5 + width  * j / n ,
                0.5 + height * i / n )
```

which is the whole of it. Note `width`/`height` here are `dface_t::lightmap_size` — the
*extents*, one less than the block dimensions `Bsp::face_lightmap_size` returns — so the
range is `0.5 .. width + 0.5` across a block `width + 1` wide: the centre of the first
luxel to the centre of the last. Page scale and offset are then exactly
`world::lightmap_texcoord`'s, and the bumped-block offset exactly
`world::lightmap_block_offset`'s.

### 3.3 Alpha

`flAlpha = clamp( CDispVert::alpha / 255, 0, 1 )`, written as `Color4f(1, 1, 1, alpha)`
(`disp_mapload.cpp:329`). It is the blend factor between `$basetexture` and
`$basetexture2` — §6 — and it is the only thing in a displacement's vertex that is not
derivable from the base face.

### 3.4 Everything in `CoreDispVert_t` that is not built

`m_FlatVert` (the undisplaced position, for decals), `m_SubdivPos` and `m_Elevation` (both
the map editor's — a `ddispinfo_t` carries neither), `m_LuxelCoords[1..3]` (the three
bumped luxel sets, which the shader derives from set 0 plus the block offset), and the
multiblend channels (`DISP_INFO_FLAG_HAS_MULTIBLEND`, set on **0** of Portal 2's 1,181).

---

## 4. The index list

### 4.1 What Valve actually runs

`disp_mapload.cpp:734` is the whole LOD policy, and it is that there is none:

```c
// If we're not using LOD, then maximally tesselate all the displacements and
// make sure they never change.
for ( iDisp=0; iDisp < nDisplacements; iDisp++ )
    pDisp->m_ActiveVerts = pDisp->m_AllowedVerts;
for ( iDisp=0; iDisp < nDisplacements; iDisp++ )
    pDisp->TesselateDisplacement();
```

`InitializeActiveVerts`' careful corner/midpoint seeding is computed and then discarded by
the first loop. So: **active verts are exactly the file's `m_AllowedVerts`, and the
tessellation runs once at load.** Everything in `disp.cpp` about LOD, error terms and
re-tessellation per frame is dead in this tree and deletes.

### 4.2 The walk

`TesselateDisplacement` (`disp_tesselate.h:195`) recurses the displacement's quadtree
breadth-first from the root node at `(side/2, side/2)`, level 0. At level `L` a node's
`vertInc` is `1 << (power - L - 1)`; its four children are at
`node + g_ChildNodeIndexMul[c] * (vertInc >> 1)` with

```
g_ChildNodeIndexMul = [ (1,1), (-1,1), (-1,-1), (1,-1) ]   // UR, UL, LL, LR
```

A child is *active* if its own centre vertex is active; the recursion descends only into
active children, and a node stops having children at `L >= power - 1`.

`TesselateDisplacementNode` then fans the node itself. It walks nine offsets clockwise
around the node — `g_TesselateVerts`, `disp_powerinfo.cpp:241` — starting and ending at
the lower-right corner:

```
( 1,-1) LR   ( 0,-1) -    (-1,-1) LL   (-1, 0) -    (-1, 1) UL
( 0, 1) -    ( 1, 1) UR   ( 1, 0) -    ( 1,-1) LR
```

The four corner entries name a child node; the four edge entries name none. For each
offset, at `sideVert = node + offset * vertInc`:

- if the entry names a child **and that child is active**, the run breaks (that quadrant
  tessellated itself, so this node must not fan over it);
- otherwise, if `sideVert` is active, it joins the run, and every time the run reaches two
  vertices a triangle `(a, b, node)` is emitted and `b` becomes the next run's `a`.

That "if `sideVert` is active" is the whole of the crack fix: a vertex `vbsp` disallowed
because the neighbouring displacement is coarser is skipped, and the two triangles that
would have met at it become one that spans it.

The node-bit bookkeeping (`m_NodeIndexIncrements`, `DispNodeInfo_t`) exists only to let
decal fragments name a subtree and **is not ported** — there are no decals.

### 4.3 The equivalence that makes this checkable

`GenerateCollisionSurface` (`builddisp.cpp:977`) — which is what `trace::disp::build_tris`
ports, and what `vbsp`, `vrad` and the collision tree all use — emits two triangles per
grid cell with the diagonal chosen on the parity of the cell's lower-left *vertex* index.
That looks unrelated to §4.2's fan. It is not:

> **When every vertex is allowed, the two produce the same triangles.**

The grid is `2^power + 1` wide, so `n = y*width + x` has the parity of `x + y`. A
deepest-level node sits at odd `(x, y)` and covers the 2×2 cell block whose corner is even;
across that block the parity rule sends each cell's diagonal through the block's centre —
which is the node — and the fan emits exactly those eight triangles, in the same winding,
up to a cyclic rotation of each triangle and the order they come out in.

That is worth more than a curiosity: it is the unit test. A displacement with no
disallowed verts must tessellate to the collision triangle set, and any error in the node
indexing, the child offsets, the winding table or the `vertInc` arithmetic breaks it. The
100 restricted displacements then get a depot test of their own (§7).

### 4.4 Winding

**Displacement indices are reversed on the way in, exactly as a world face's fan is.**
`world::build_page_meshes` emits a face as `(0, i+1, i)` rather than `(0, i, i+1)`, for
the `front_face: Ccw` reason `rustdocs/ENGINE.md` gotcha 1 sets out, and terrain gets the
same treatment in `Displacement::build`. There is no asymmetry: everything Valve-authored
is reversed at the boundary where it enters.

**This paragraph originally said the opposite**, from an argument that ran through two
independent conventions — `trace::disp`'s `(v2 - v0) × (v1 - v0)` "points out of the
terrain" and this port's reversal of a world fan — and came out backwards. The depot test
in §7 caught it on its first run, which is exactly what it was written for. The record is
kept rather than quietly corrected, because the *shape* of the mistake is the reusable
part: a winding argument with two sign conventions in it is not worth trusting, and the
anchor is cheap.

What the anchor is: for every displacement, the sum of its rendered triangle normals —
area-weighted, so a big triangle outvotes a sliver — must agree in sign with the
*rendered* normal of the base face those same vertices were carved from. Measured over the
game: **all 1,181 patches agree, and all 1,181 disagree without the reversal.** It is taken
over the patch rather than per triangle because terrain genuinely overhangs: 131 of the
92,622 individual triangles face the other way, all of them on steep patches.

---

## 5. Batching: nothing new

`DispInfo_CreateMaterialGroups` (`disp_mapload.cpp:368`) groups displacements by
`(lightmapPageID, material)` — the same pair `world::Batch` already is, and for the same
reason (the page is one texture binding and cannot vary within a draw). Valve keeps
displacement groups in a *separate* list from world surfaces only because they are
different `IMesh` allocations with a different vertex format.

Here they are neither. A displacement is a surface with a material, a lightmap allocation
and some triangles, so it goes through the existing `group_faces` → `place_lightmap` →
`build_page_meshes` pipeline unchanged, and the only edit is that `group_faces` stops
skipping `face.disp_info >= 0` and `build_page_meshes` asks a displacement for its
vertices instead of fanning the face's winding.

Consequences, all of them wanted: a displacement shares a batch with any ordinary world
face wearing the same material and landing on the same page; its lightmap is packed by
the same allocator in the same material-clustered order; it splits at 65,536 vertices by
the same rule; and its geometry is in world space under the identity matrix like the rest
of model 0.

---

## 6. The shader: `WorldVertexTransition`

**937 of 1,181 displacement faces name a shader that was not ported, including all 11 of
`sp_a1_intro1`'s.** Without it this module's output is eleven magenta checkerboards, so it
is part of the job rather than adjacent to it.

### 6.1 It is `LightmappedGeneric`

`worldvertextransition.cpp` is 222 lines, of which 190 are a parameter table and the
remaining three forward to `InitParamsLightmappedGeneric_DX9`,
`InitLightmappedGeneric_DX9` and `DrawLightmappedGeneric_DX9` — the same helper, the same
`.fxc`, the same vertex format. `lightmappedgeneric_dx9.cpp` even declares
`$basetexture2`, `$bumpmap2`, `$blendmodulatetexture` and `$ssbump` itself; the two
shaders differ only in which parameters they expose and, in practice, in whether content
happens to set `$basetexture2`.

So this is not a new shader. It is the two-layer blend turned on in the shader that is
already there, plus a second `ShaderKind` name in front of it — which mirrors Valve's own
`DEFINE_FALLBACK_SHADER( WorldVertexTransition, WorldVertexTransition_DX9 )`.

### 6.2 The four things to add

1. **`$basetexture2`, blended by vertex alpha.** `blendfactor` is
   `v.vColor.a` (the `VERTEXALPHATEXBLENDFACTOR` combo, on whenever `$basetexture2` or
   `$bumpmap2` is defined), and `baseColor = lerp(baseColor, baseColor2, blendfactor)`
   (`lightmappedgeneric_ps2_3_x.h:467`). §3.3's alpha is what feeds it.
2. **`$blendmodulatetexture`** — `FANCY_BLENDING == 1`, the setting 10 of Portal 2's
   16 blend materials use:
   ```
   minb = max(0, texel.g - texel.r);  maxb = min(1, texel.g + texel.r)
   blendfactor = smoothstep(minb, maxb, blendfactor)
   ```
   It is what turns a linear crossfade into dirt settling into the low parts of a
   cobblestone, and without it the terrain reads as two textures dissolved together.
3. **`$bumpmap2`** — `vNormal = lerp(vNormal, vNormal2, blendfactor)`.
4. **`$ssbump`**, and this one is a **fix to the existing shader**, not an addition —
   and its reach is the biggest thing this module turned up. It was written here as "every
   one of Portal 2's blend-terrain materials sets it"; measured afterwards, **128,139 of
   the game's 288,250 drawable non-displacement world faces** wear a `$ssbump` material
   too, across 92 materials. So this is not a terrain fix that happens to touch the world
   — it is a world fix that terrain happened to expose, and 44% of every wall, floor and
   ceiling in the game was being lit through the wrong decode before it.
   `bumpmap_variant = hasSSBump ? 2 : hasBump` (`lightmappedgeneric_dx9_helper.cpp:686`),
   and `BUMPMAP == 2` changes two things: the texel is used **raw** rather than
   `2*t - 1` (`ps2_3_x.h:322`, `#if BUMPMAP == 1 // not ssbump`), and the bumped lighting
   stops being the `saturate(dot(n, basis))²` weighting and becomes a plain weighted sum
   scaled by `0.57735` — `1/√3`, and the comment at `ps2_3_x.h:649` explains exactly why
   (`vrad`'s three coefficients are barycentric and sum to 1, an ssbump's sum to 1.733).
   The unconditional `2*t - 1` path mis-lights every one of them.

### 6.3 Deferred, with the numbers

- **`$seamless_scale`** — 553 displacement faces, all in the `sp_a3_*` underground maps,
  and **0 in `sp_a1_intro1`**. Seamless mapping replaces the base texture coordinate with
  a triplanar projection of `worldPos * scale` blended by the squared world normal, which
  a `WorldVertex` does not carry. It is therefore the feature that forces
  `LightmappedGeneric`'s second vertex layout — the thing `MATERIALSYSTEM.md` §10 has
  been watching for and that `LightmappedGeneric`'s bumped variant turned out not to be.
  Until then those materials draw with the texinfo's ordinary planar mapping: the right
  texture at the wrong scale, rather than nothing.
- **`$envmap`** on 553 + 216 of them — already deferred for `LightmappedGeneric`, and
  blocked on the same missing normal.
- **Layer tints, `$newlayerblending` and the border/edge terms** (`FANCY_BLENDING >= 2`),
  the drop shadow, `$detail`/`$detail2`, phong, the flashlight and cascaded shadow maps.
  No Portal 2 displacement material sets any of them.
- **`LUMP_DISP_MULTIBLEND`** — 0 displacements in the game.

---

## 7. Verification

Unit tests, no GPU, no depot:

- The grid's texture and luxel coordinates against a hand-built quad, including the
  `+0.5` and the extents-versus-block-size distinction of §3.2.
- **§4.3's equivalence**: a synthetic displacement with every vertex allowed tessellates
  to the same triangle set as `trace::disp`'s collision list, at powers 2, 3 and 4.
- A displacement with a cleared `m_AllowedVerts` bit emits no triangle naming that vertex,
  and still covers the patch (no hole, no overlap) — the two failure modes of §4.2.

Depot-gated (`KISAK_GAME_DIR`, `--ignored`), because `sp_a1_intro1` has no restricted
displacement and 100 maps' worth of them exist:

- Every one of the 1,181 shipped displacements builds, and its vertices lie inside its own
  bounds.
- **The winding anchor of §4.4**: the rendered triangle normals agree in sign with the
  base face's rendered normal.
- The 100 restricted ones tessellate without naming a disallowed vertex, and their
  triangle count is below the unrestricted count for the same power.
- Every shader's WGSL compiles and builds a pipeline against a real device. This turned
  out to be a **standing gap rather than a new test**: `preview.rs`'s GPU tests draw
  `UnlitGeneric` and `VertexLitGeneric` only, so nothing in `cargo test` had ever parsed
  `lightmappedgeneric.wgsl` — the shader most of a map wears. It lives in
  `materials::pipeline`, covers every `ShaderKind`, and was checked against a deliberately
  broken binding before being believed.

**What the depot run reports**, and what each number is load-bearing for:

```text
29 maps: 1181 displacements, 1181 built, 92622 triangles;
100 with disallowed vertices (511 vertices dropped);
92622 triangle windings checked (131 overhanging); powers {2: 904, 3: 202, 4: 75}
```

and on `sp_a1_intro1`, through the real material and lightmap path:

```text
5523/5638 faces drawn, 15954 triangles (1408 terrain, over 11 displacements),
79 batches, 76 materials (3 missing), 4857 lit ... over 13 lightmap pages
```

1,408 is 11 × 128, which is what power 3 gives. The three missing materials are still
`SolidEnergy`, `Refract` and `Black` — **the two `WorldVertexTransition` terrain materials
resolved**, and the lit count rose by exactly the 11 new surfaces.

---

## 8. Open questions

1. **When does the world vertex gain a normal?** `$seamless_scale` (553 faces) and
   `$envmap` both want one, and both are `LightmappedGeneric` features rather than
   displacement ones. A displacement can compute one (`CalcNormalFromEdges`); an ordinary
   world face already has one on its plane. The cost is not the attribute, it is
   `SmoothDispSurfNormals` and the neighbour tables §1 deletes.
2. **Does anything want the flat vertex?** `m_FlatVert` exists for decals and for
   `SurfToBaseFacePlane`. Neither exists yet; when decals do, it is one more `Vec3` per
   vertex and not a redesign.
3. **Displacements and the PVS.** Valve keys visibility off the *parent face*'s leaf, and
   a displacement can bulge well outside it. `UpdateDispBBoxes` exists for that. Nothing
   here until `world/`'s visibility lands, but it is the thing that will make this module
   interesting again.
