# `game/client/portal/portalrender.cpp`: the recursive view

The one part of a portal that is *only* drawing: the picture of the other room,
seen through the opening, with the portal in that room showing the first room
again, and so on until the stencil runs out.

`portdocs/PORTAL.md` §7 was written to justify leaving this out; this document is
what it deferred to. Everything else in that doc has landed — the class, the hole
in the collision, the teleport, the oval — so a portal here already *works*. What
is missing is that it is **opaque**: an oval painted on an unbroken wall.

Sizes and line numbers are from `legacy/`. Every count of entities, materials or
maps is measured against the shipped game.

---

## 0. The shape of the problem, and the one decision that sets everything else

Drawing through a portal is drawing the scene twice: once from the player's eye,
and once from a **virtual eye** — the player's position carried through the
teleport matrix into the exit portal's room. The second picture must appear only
inside the portal's opening, must not include the wall the exit portal is
mounted on, and must leave the depth buffer as it found it so that everything
drawn afterwards still composites.

Three mechanisms do those three jobs, and they are the whole module:

| Job | Mechanism |
|---|---|
| Confine the second picture to the opening | **the stencil buffer** |
| Cut away the exit portal's own wall | **an oblique near plane** |
| Put the depth buffer back | **a second draw of the opening, depth-only** |

The decision that sets the rest is the first one. **The stencil, not a render
target**, and `rustdocs/MATERIALS.md` has been carrying `Depth24PlusStencil8`
since stage 1 waiting for it (`target.rs:30`: *"The stencil is load-bearing for
Portal 2"*). Valve ships both — `DrawPortalsUsingStencils` and
`RenderPortalViewToTexture` — and the texture path is the fallback for hardware
with no stencil, which is not a case this port has.

What that buys, and it is the thing that makes the port small: **the whole
recursion is one `wgpu` render pass.** Stencil compare functions, masks and
operations live in the pipeline, the reference value is dynamic state, and
`PipelineCache` already keys on render state — so "set the stencil and draw the
world again" is a `StateOverride` and a second camera, not a second target, a
second attachment or a second `begin_render_pass`. There is no extra memory and
no extra tile load.

---

## 1. Inventory

| File | Lines | Disposition |
|---|---|---|
| `game/client/portal/portalrender.cpp` | 2,113 | **Port ~200.** `DrawPortalsUsingStencils_Old` (`:1399-1744`) is the algorithm — see §2.1 for why the *old* path and not the shipped fast one. Delete the view-ID node tree, the occlusion queries, the pixel-visibility feedback, the ghost locations, the depth doubler, the fog stack, `DrawEarlyZPortals`, `HandlePortalPlaybackMessage` and the tools recording. |
| `game/client/portal/portalrenderable_flatbasic.cpp` | 1,747 | **Port ~250.** `RenderPortalViewToBackBuffer` (`:347`) — the virtual camera and the clip plane. `Internal_DrawRenderFixMesh` (`:1360`) and `CreateMeshForPortals`' near-cap loop (`:1056-1160`) — the same routine twice; §5. `ShouldUpdatePortalView_BasedOnView` (`:1693`) and `ComputeClipSpacePortalCorners` (`:1181`) — §4. `PortalMoved` (`:63`), for the five PVS points. Delete `CalcFrustumThroughPolygon`'s unbounded frustum (§4.2), `RenderPortalViewToTexture`, `DrawDepthDoublerMesh`, `RenderFogQuad`, `CreateRingMesh`. |
| `materialsystem/shaderapidx9/shaderapidx8.cpp` | — | **Port 25 lines:** `ApplyClipPlaneToProjectionMatrix` (`:6889`), the oblique near plane. §3.2. |
| `stdshaders/portal_refract_ps2x.fxc` | 282 | **Port the `STAGE == 1` branch** — nine lines of it. §6.1. |
| `stdshaders/BufferClearObeyStencil_dx9.cpp` + `_vs20.fxc` | 140 | **Port.** A full-screen quad that writes depth and obeys the stencil; `wgpu` has no partial clear. §6.2. |
| `stdshaders/writez_dx9.cpp` | 103 | **Not needed after all** — §6.3. The stencil-hole material restores the depth, which saves a shader. |
| `game/client/portal/c_portalghostrenderable.cpp` | 980 | **Delete for now** — §8. The half of an entity that sticks out of the far portal. Nothing but the player can be in a portal in this port, and the player has no drawn model. |
| `game/client/portal/portal_render_targets.cpp` | 190 | **Delete.** `_rt_Portal1`/`_rt_Portal2` exist for the texture path only. |
| `mathlib/polyhedron.cpp`, `staticcollisionpolyhedroncache.cpp` | — | Already deleted by `portdocs/PORTAL.md` §4.3; nothing here brings them back. |

~5,400 lines of reference for ~600 of port, and the ratio is what it is because
four fifths of `portalrender.cpp` is bookkeeping for things this port does not
have: per-view render-list caching, occlusion queries feeding back into next
frame's culling, split-screen slots, and the depth doubler (which reuses *last
frame's* image for the deepest recursion level).

---

## 2. The algorithm

### 2.1 Two stencil schemes ship, and the older one is the better port

`DrawPortalsUsingStencils` picks between two implementations on
`r_portal_fastpath` (default `1`):

- **The fast path** (`:726-1341`) packs the recursion level *and* the portal
  index into one byte: `ComputeStencilRefValue` (`:495`) returns
  `(1 << portal) & 0xF` at level 0 and `((1 << portal) << 4) | (1 << parent)` at
  level 1, with `Assert( nViewRecursionLevel < 2 )` — **it supports exactly two
  levels and at most four portals per level.** It buys one thing with that: a
  single cached vertex buffer holding every portal's quad, built once a frame.
- **The old path** (`:1399-1744`) uses the recursion level itself as the
  reference value and `INCREMENT_CLAMP`/`DECREMENT_CLAMP` to move between
  levels. It has no depth limit but the stencil's range, and no portal-count
  limit at all.

**This port takes the old path's scheme and the fast path's materials.** The
increment/decrement scheme is simpler, general in depth, and the vertex-buffer
caching it gives up is meaningless here: a portal's quad is four vertices built
from four numbers every frame (`world/portals.rs`), and the whole map's worth of
them is two.

The fast path's *materials* are the ones to keep, because the old path predates
them: `DrawStencilMask` there punches the hole with `WriteZ` over the whole
**rectangle**, where the fast path uses `portal_stencil_hole.vmt` — `$Stage 1`,
whose alpha test cuts the **oval**. The rectangle is a visible bug in the
shipped game if you turn the fast path off.

### 2.2 One level, in order

With the parent's stencil reference `p` and the child's `c = p + 1`:

| Step | Stencil | Depth | Draw |
|---|---|---|---|
| 1 | `compare Equal(p)`, `pass IncrementClamp`, masks `0xFF` | test on, write on | the portal's quad, `$Stage 1` — **plus the near-plane cap** (§5) |
| 2 | `compare Equal(c)`, `pass Keep` | test off, write on | a full-screen quad at the far plane (§6.2) |
| 3 | `compare Equal(c)`, `pass Keep` | normal | **the whole scene again**, from the virtual camera, with the oblique projection — then recurse, then that view's translucent pass |
| 4 | `compare Equal(c)`, `pass DecrementClamp` | test **off**, write on | the portal's quad and its cap again, colour writes off |

Step 4 does steps 4 *and* 5 of the reference at once: Valve restores depth with
`WriteZ` and the stencil with a separate `SET_TO_REFERENCE`/full-screen
`DECREMENT`, and one draw does both because the region the stencil test admits is
exactly the region that was incremented. §6.3.

After the loop the stencil is back to `p` everywhere, so the caller's state is
untouched and no draw after the recursion needs to know it happened.

**Order within step 3 is the reference's** and is not obvious: opaque world and
entities, *then* the recursion one level deeper, *then* that level's translucent
pass. `CBaseWorldView::DrawExecute` (`viewrender.cpp:8021`) calls
`DrawRecursivePortalViews()` between `DrawWorld` and `DrawTranslucentRenderables`
for the main view, and `ViewDrawScene_PortalStencil` re-enters the same function
for every level below it.

### 2.3 Where it goes in this port's frame

`Engine::render`'s four passes become four passes and one insertion:

```
  pass 1  Load::Clear   world.draw()            ← opaque
                        world.draw_portal_views()   ← NEW, same pass
          update_refract_texture()
  pass 2  Load::Keep    world.draw_refracting()
  pass 3  Load::Keep    world.draw_translucent()    ← the ovals
          post.resolve()
```

Same pass as the opaque draw, because the recursion is only a stencil state and
a camera, and because that is where the reference puts it.

---

## 3. The virtual camera

### 3.1 The view matrix

`RenderPortalViewToBackBuffer` (`:395-412`):

```
ptPOVOrigin = m_matrixThisToLinked * cameraView.origin
matTemp     = matCurrentView * m_pLinkedPortal->m_matrixThisToLinked
```

The second line's matrix is the *linked* portal's, which is this one's inverse,
so in this port's column-major convention it is one expression:

```rust
let eye  = matrix.transform_point3(camera.eye);
let view = camera.view * matrix.inverse();
```

There is exactly one teleport matrix in this port —
`server::classes::portal::teleport_matrix`, which `PortalState::matrix` already
carries across the seam for the carve (`portdocs/PORTAL.md` §12.2) — and the
renderer reads that one rather than deriving a second. §3.2 of `PORTAL.md` is the
reason: a second spelling can silently lose the 180° about up.

**No cull-mode flip.** A mirror needs one because its view matrix is reflected;
the portal transform is a rotation and a translation, determinant `+1`, so
winding is preserved. `StateOverride::cull` stays alone.

### 3.2 The oblique near plane

Geometry behind the exit portal — the wall it is mounted on, and anything inside
that wall — must not draw, or the second picture is a picture of the inside of a
wall. Valve uses a user clip plane
(`vCustomClipPlane = ( vRemotePortalForward, forward·remoteOrigin - 2.0 )`,
`:434`), and where the hardware has none it shears the projection matrix so that
the near plane *becomes* that plane. **WGSL has no clip distance, so the shear is
the only option** — which is fine, because it is also the path Valve's own
`mat_alternatefastclipalgorithm` defaults to.

`ApplyClipPlaneToProjectionMatrix` (`shaderapidx8.cpp:6889`) is Lengyel's
method. Transcribing it is wrong twice over — it is D3D's row-major layout with
vectors on the left, and it indexes `_13.._43`, which is the *column* that
produces clip `z` — so it is derived instead. For a plane `C = (n, -d)` in view
space, kept where `C · (p,1) >= 0`, and a projection whose clip `z` comes from
row 2:

```
q = ( (sgn(C.x) + P[0][2]) / P[0][0],
      (sgn(C.y) + P[1][2]) / P[1][1],
      -1,
      (1 + P[2][2]) / P[2][3] )
row2(P) := C / dot(C, q)
```

`q` is the far-plane corner opposite the clip plane; scaling `C` so that
`row2·q = 1` puts that corner exactly on the far plane, which keeps as much of
the depth range as the shear allows. For this port's `directx::perspective` the
last component works out to `1/far`, which is a cheap check that the derivation
is right.

**The plane goes back 2 units** (`- 2.0f`, *"moving it back a smidge to eliminate
visual artifacts for half-in objects"*), and there is a guard, because a clip
plane at or behind the eye makes the projection degenerate:

```
camDist = n · virtualEye - d
if camDist > -1.0:  d += camDist + 1.0
```

**This port fixes a bug here, and it must be recorded.** Valve computes that
distance as `DotProduct( cameraView.origin, vRemotePortalForward )` — the *real*
camera's origin against the *exit* portal's normal, two different rooms. The
quantity that matters is the virtual eye's distance, and it is the negative of
the real eye's distance in front of the *entrance*. Valve's version only ever ran
on PS3 (`UseFastClipping()` is false on PC, which has real clip planes), where
the two rooms' arithmetic happened to be close enough often enough. Here the
shear is the only path, so the guard fires every time the player is within about
2 units of a portal — which is every time they walk through one — and it has to
be the right quantity.

### 3.3 Two projections, not one

The oblique matrix is what the sub-scene is **drawn** with. It is not what the
sub-scene is **culled** with: the shear tilts the far plane, and
`vis::Frustum::new` would extract that tilted plane and cull geometry that is
plainly visible. So the virtual camera exists twice — once with
`Camera::perspective`, whose `view_proj` goes to `World::visible`, and once with
the sheared projection, which goes to the pass. Culling stays conservative and
clipping stays exact.

---

## 4. Visibility, which is the part `world/vis.rs` was left ready for

### 4.1 The virtual eye is inside a wall

This is the finding that decides the whole section, and it follows from
`portdocs/PORTAL.md` §9's invariant 15: *a point in front of the entrance images
to the same distance behind the exit*. The virtual eye is therefore **behind the
exit portal's plane, inside the wall it is mounted on** — a solid leaf, whose
cluster is `-1`, whose PVS row is empty. Asking `Visibility::mark` for what the
virtual eye can see answers **nothing at all**, and the portal draws a black
hole.

Valve's answer is `AddToVisAsExitPortal` (`portalrenderable_flatbasic.cpp:634`),
and it is two separate corrections:

- **Five PVS origins, not one** — the exit portal's four corners and its
  `m_ptForwardOrigin`, each skipped if it is in solid space
  (`GetLeafContainingPoint(...) != -1`). All five are computed in `PortalMoved`
  (`:66-80`) and all five sit **one unit in front of** the portal plane:
  `m_ptForwardOrigin = m_ptOrigin + m_vForward`, and the corners are that point
  ± right·halfWidth ± up·halfHeight.
- **A forced view leaf** — `ForceViewLeaf( m_iViewLeaf )`, the leaf at the
  forward origin, which is what the area-portal flood starts from.

`CLAUDE.md` records that `Map_VisSetup` takes an *array* of origins and ORs their
PVS rows together, *"precisely so that a skybox camera and the world share one
visible set, and this port has one origin"*. This is the other consumer, and it
is the one that arrived first.

`Visibility::mark` therefore grows a caller-supplied view point: the eye (which
the area-portal windows are still clipped against, because the rectangles are in
*this* camera's screen space), the origins whose rows are ORed, and the point
whose leaf the flood starts from. `mark(eye, ..)` stays, as the one-origin case.

### 4.2 The frustum through the opening, cheaply

`CalcFrustumThroughPolygon` (`:195`) builds a frustum with **one plane per edge
of the portal's clipped silhouette** — an unbounded plane count, a polygon clip
against the parent's own complex frustum, and a second reduced 6-plane version
for the parts of the engine that insist on six. `Frustum` here is six planes and
`Rect`/`narrowed` already exist for exactly this shape of job
(`R_SetupVisibleAreaFrustums`).

So this port takes the **screen rectangle** instead:
`ComputeClipSpacePortalCorners` (`:1181`) projects the four corners and returns
false if any of them is behind the near plane; the bounding box of the four,
clamped to `-1..1`, narrows the virtual camera's frustum through
`Frustum::narrowed`. That is strictly weaker than one plane per edge and strictly
stronger than nothing, it reuses code that is already tested, and for a
rectangle seen head-on the two answers are the same.

The same rectangle is the **scissor**, which is free and bounds the fill of steps
2 and 4. Valve computes it (`r_portalscissor`, `:1133`) and ships it turned off.

---

## 5. The near-plane cap, which is what makes walking through one work

When the player gets close enough that the portal's quad crosses the near plane,
the clipped-away part of the quad writes no stencil — so the opening develops a
hard edge across it and the wall shows through, at exactly the moment the player
is walking into it. Valve fixes it with a **cap polygon on the near plane**, and
writes the same routine twice: `Internal_DrawRenderFixMesh` (`:1360`, the old
path) and the near-cap loop of `CreateMeshForPortals` (`:1056-1160`, the fast
one). They agree, and the port takes them as one function.

```
guard      recursion level 0 only, and |eye - origin|² < halfHeight²
quad       the portal's corners, pushed 0.275 units along forward
if the eye is within 0.4 units of that rectangle (point-to-rectangle, not
   point-to-plane — ComputePointToPortalDistance, :912):
    the cap is the whole near plane, clipped by the portal plane
otherwise:
    clip the quad by the *flipped* near plane, offset back by 0.3
      → what the near plane would have cut off
    clip that by the four side frustum planes, each offset back by 0.01
project    every surviving vertex onto the near plane, offset back by 0.01,
           along the ray from the eye
draw       as a fan, depth test off and depth write on
```

Three constants and all three earn their place: `0.3` makes the cap *overlap* the
seam where the quad was clipped rather than meet it, `0.01` gives the
reprojection slack so a vertex on a side plane does not land outside the
viewport, and `0.4` is the distance below which the plane effectively passes
through the eye and the reprojection stops being meaningful.

**The cap wears the same `$Stage 1` material as the quad and needs no second
one**, because every cap vertex is given texture coordinate `(0.5, 0.5)` — the
centre of the portal, where `flDistFromCenter` is 0 and the alpha test passes
for any open amount. The cap is unconditionally inside the hole, which is what
it is for.

---

## 6. Materials and shaders

### 6.1 `$Stage 1` — `portal_stencil_hole.vmt`

`ShaderKind::resolve` has been answering `None` for it with the comment *"the
stencil punch needs a stencil"*. It gets one. Measured from the VPK, the whole
material is:

```
PortalRefract { $Stage 1  $PortalOpenAmount "0.0"  $PortalStatic "0.0"
                "<DX90" { $PortalMaskTexture ... $PortalColorTexture ... }
                $time "0.0"  Proxies { CurrentTime / PortalOpenAmount / PortalStatic } }
```

— both textures are inside a `<DX90` block, so on this port's path the material
has none, and the stage-1 pixel shader samples none. Its whole body
(`portal_refract_ps2x.fxc:184-192`) is `rgb = 0`, `a = flStencilCutout`, where
the cutout is the `step( distFromCentre, openAmount² )` the stage-2 shader
already computes. Shadow state: alpha test `> 0.5`, **depth writes on** (the one
stage that has them), no blending, alpha writes off, polygon offset decal.

It is a second `ShaderKind` rather than a combo on the first, because a
`PipelineKey` is a `ShaderKind` plus state and the fragment body is what differs.
It shares stage 2's group 1 layout, its parameter table, its uniform block, its
vertex layout and its group 3 — so the cost is an enum variant, a `match` arm and
twenty lines of WGSL.

### 6.2 `BufferClearObeyStencil`

Step 2 has to reset the depth buffer *inside the opening only*, and `wgpu` has no
partial clear: an attachment's `LoadOp::Clear` covers the whole attachment.
Valve's answer is a shader with that name, and it is the answer here too — the
vertex shader passes the position through as clip space with `w = 1`
(`bufferclearobeystencil_vs20.fxc:24`), and `DrawClearBufferQuad`
(`cmatrendercontext.cpp:2419`) feeds it a quad at `±1.1` in NDC with `z` set to
the far value. The `1.1` is Valve's, *"to fix small borders around the edges in
full screen with anti-aliasing enabled"*.

Colour writes off, depth writes on, depth compare `Always`. With the scissor of
§4.2 it costs the portal's screen rectangle, not the screen.

### 6.3 `writez` is not needed

`portdocs/PORTAL.md` §1.3 has `writez_dx9.cpp` down as *"port, or stub"*, for
step 4's depth restore. It turns out not to be needed at all: the stencil test at
step 4 admits **exactly** the pixels step 1 incremented, so the region is already
determined and the draw only has to supply the right depth. The `$Stage 1`
material supplies it — it is the same quad, and its alpha test cuts the same
oval — and the only thing that has to change is that **colour writes are off**,
which `EnableColorWrites( false )` is and which this port's `RenderState` did not
have because nothing had wanted it.

So `RenderState` grows `write_color`, `StateOverride` grows `write_color` and
`stencil`, and `writez` stays unported. `models/portals/portal_1_anims.vmt` — the
material the portal *model* wears — still resolves to nothing, which is still
correct: `portdocs/PORTAL.md` §7.1's invariant is that the model draws nothing at
all, and it is still never drawn.

---

## 7. What is deliberately not here

- **`$Stage 0`, the opening warp** (`portal_refract_1.vmt`). `CLAUDE.md` lists it
  beside `$Stage 1` as something the recursive view needs; it is not. It is
  `DrawPortalsUsingStencils`' *step 0*, drawn only for portals that are still
  opening (`m_portalIsOpening`, `:986`), and it warps the pixels **around** the
  hole while the hole grows — an effect on the half-second opening animation, not
  the see-through.

  It is left out for a concrete reason rather than for scope: it samples
  `TEXTURE_FRAME_BUFFER_FULL_TEXTURE_0`, a copy of the scene *as it stands at
  that moment in the frame*. Valve takes that copy inline
  (`UpdateFrontBufferTexturesForMaterial`, `:1013`) because D3D9 has no notion of
  a pass. Here the recursion runs inside the opaque pass, and a texture copy
  cannot happen inside a `wgpu` render pass — so stage 0 costs a split of the
  opaque pass and a full-screen copy on every frame a portal happens to be
  opening. **Reversed by:** wanting the opening animation exactly right, at which
  point the pass splits at the top of `draw_portal_views` and the copy is
  conditional on any portal having `open_for < 0.5`.

- **Refracting geometry inside a portal view.** `World::draw_refracting` is the
  port's second pass for the 71 of 106 maps with a material that samples the
  scene, and it needs that same mid-pass copy. A glass pane seen *through* a
  portal therefore does not draw. On `sp_a1_intro1` that is one model,
  `props_lab/glass_observation_2`. **Reversed by:** the same pass split stage 0
  needs; they are one change.

- **The depth doubler** (`UpdateDepthDoublerTexture`, `:1903`,
  `DrawDepthDoublerMesh`, `:725`). At the deepest recursion level, if the two
  portals face each other within 45°, Valve draws *last frame's* image of the
  same view instead of a black hole — which is what makes an infinite corridor
  look infinite. It needs a persistent colour copy and the view matrix it was
  taken with, and it is a cosmetic improvement to the deepest level only.
  **Reversed by:** someone standing between two facing portals and minding.

- **Pixel-visibility feedback** (`ShouldUpdatePortalView_BasedOnPixelVisibility`,
  occlusion queries around the stencil mask). Valve measures how many pixels a
  portal's mask actually filled and skips the view next frame if it was under
  0.005% of the screen. `wgpu` has occlusion queries; this port has no frame-to-
  frame render state to put the answer in. **Reversed by:** a portal view showing
  up in `world::bench`.

- **`c_portalghostrenderable.cpp`** (980). An entity straddling a portal is drawn
  twice, the second time by a "ghost" renderable transformed through the matrix,
  so that the half sticking out of the far side is visible. The player is the
  only thing that can be in a portal here and the player has no drawn model.
  **Reversed by:** a cube that *moves* — `prop_weighted_cube` has landed but has
  no vphysics, so what this and `prop_floor_cube_button` and the entity teleport
  are all waiting on is `MOVETYPE_VPHYSICS`.

- **The 3D skybox through a portal** (`Draw3dSkyboxworld_Portal`,
  `IsSkyboxVisibleFromExitPortal`). There is no skybox yet. When there is,
  `m_nSkyboxVisibleFromCorners` in `PortalMoved` is the flag it wants, and the
  five PVS origins of §4.1 are already the array `Map_VisSetup` would take.

- **Fog through a portal** (`ShiftFogForExitPortalView`, the fog backup/restore
  around every recursion). No fog volumes are ported.

---

## 8. Invariants that produce a wrong picture rather than an error

Ordered by how likely each is to bite. These belong in `rustdocs/` when it lands.

1. **The virtual eye is in solid space** (§4.1). Culling the sub-scene from it
   draws nothing whatever; the PVS must be asked from the exit portal's corners
   and its forward origin, and the area flood must be forced to the exit
   portal's leaf.
2. **The oblique projection must not be the culling projection** (§3.3). It
   tilts the far plane, and geometry in plain sight vanishes.
3. **The clip-plane guard measures the *virtual* eye** (§3.2), not the real one,
   and it fires every time the player walks through a portal. Getting it wrong
   gives a degenerate projection at the exact moment the effect matters.
4. **Step 4's draw has depth test off and depth write on.** With the test on, the
   depth it is trying to restore is behind the sub-scene it just drew and every
   fragment is rejected — the opening keeps the far room's depth, and the oval,
   the refracting pass and anything else drawn later composite against the wrong
   surface.
5. **Colour writes must be off in step 4**, or the second draw of the hole paints
   the portal's interior black over the picture just rendered into it.
6. **The stencil reference is dynamic state and the rest is pipeline state.**
   `set_stencil_reference` outside the pipeline, compare/ops/masks inside it. A
   reference set without the matching state override silently does nothing.
7. **A pass that writes stencil must say so when it opens.** `stencil_ops: None`
   is a read-only stencil aspect in `wgpu`, and a pipeline with a non-zero
   stencil write mask against it is a validation error, not a wrong picture —
   the one failure in this document that is loud.
8. **The near-plane cap's texture coordinate is `(0.5, 0.5)`** (§5). Carry the
   quad's real coordinates across and the alpha test cuts the cap into an oval of
   its own, which is a hole in the hole.
9. **The recursion must not re-enter the portal it came out of.** The exit portal
   is in the sub-scene's own list, it is facing the virtual camera, and following
   it goes back where it came from — at which point the depth limit is the only
   thing that ends it and every level is wasted. `m_pRenderingViewExitPortal`
   (`:960`) is the check.
10. **`open_for` reaches the hole shader as well as the oval.** The hole's radius
    is `openAmount²`, the same expression stage 2 uses, so a portal that has just
    activated has *no* hole and grows one. Binding a zeroed group-3 block gives a
    hole of radius zero and a portal that never opens.
11. **The scissor has to be restored.** It is pass state, not draw state, and
    everything recorded after a portal would otherwise be clipped to that
    portal's rectangle.

---

## 9. Stages

**Stage 1 — stencil in `materials/`.** `Stencil` in `RenderState` and
`StateOverride`, `write_color` beside `write_alpha`, `stencil_ops` on every pass
that opens with a depth attachment, `Pass::set_stencil`. Not portal work, and
testable on its own: a pipeline that writes a stencil value and a second draw
that reads it.

**Stage 2 — a second camera in a pass.** `Pass::set_camera`, which pushes a
second `FrameUniforms` slot into the arena the pass already owns and rebinds
group 0. This is the whole of what `CLAUDE.md` meant by *"making the view a
parameter is a refactor rather than a rewrite"*. Testable: two cameras, one pass,
two pictures.

**Stage 3 — visibility from somewhere else.** `Visibility::mark` gains the view
point of §4.1. Testable against the depot without drawing anything: for each of
the nine shipped pairs, the set marked from the exit portal's corners is
non-empty and contains the exit portal's own leaf, where the set marked from the
virtual eye is empty.

**Stage 4 — the recursion.** The loop of §2.2, the virtual camera of §3, the
frustum and scissor of §4.2, and `$Stage 1` and `BufferClearObeyStencil` from §6.
The near-plane cap (§5) is the last piece and is separately testable: at every
distance from 0 to a half-height, the cap's projected polygon plus the clipped
quad covers the portal's whole silhouette.

---

## 10. Verification

`sp_a1_intro1` is the test bed again, for the reason `portdocs/PORTAL.md` §11
gives: it places both portals, 7,000 units from the spawn, and `portal 1` /
`portal 2` from the console place a pair anywhere.

- **Unit, no map:** the oblique projection. A plane, a point on its far side,
  and the assertion that the point's clip `z` is negative — plus the assertion
  that a point *on* the plane lands at `z = 0`, which is the definition of a near
  plane, and that the far corner opposite still lands at `z = w`.
- **Unit, no map:** the virtual camera. A linked pair, a point, and the assertion
  that its image under the virtual camera's `view_proj` is where the original is
  under the real one — because that is what "the picture lines up in the opening"
  means, stated as arithmetic.
- **Unit, no map:** the near-plane cap. For a portal at a range of distances and
  angles, the cap's polygon is non-empty exactly when the quad crosses the near
  plane, and every cap vertex lands on the near plane.
- **Unit, fixture map:** visibility through a portal. Two rooms, a linked pair,
  and the assertion that the set marked from the exit portal's corners sees the
  far room's faces and the set marked from the virtual eye sees nothing.
- **Depot, `--ignored`:** for each of the nine shipped pairs, a sub-scene marked
  from the exit portal is non-empty, and its face count is a sane fraction of the
  map's.
- **Rendered, headless:** the one that says it works. Two rooms a thousand units
  apart, differently lit, a linked pair between them; the portal-off frame and
  the portal-on frame must differ inside the oval and be identical outside it.

---

## 11. What landed, and what each stage found

**All four stages of §9 are in.** `sp_a1_intro1` draws the room behind the
orange portal inside the blue one's opening, at two levels of recursion, and
`r_portal_stencil_depth` takes it up to ten. What is *not* here is §7's list,
unchanged: `$Stage 0`, refracting geometry inside a portal view, the depth
doubler, pixel-visibility feedback, `c_portalghostrenderable.cpp`, the skybox
and fog.

### Stage 1 — stencil in `materials/`

`RenderState::stencil: Option<Stencil>` and `RenderState::write_color`,
`StateOverride`'s two matching fields, `Pass::set_stencil`, and stencil
operations on every depth attachment `RenderContext::open` hands out. `Stencil`
is in the *pipeline key* because `wgpu` puts it in the pipeline; only the
reference value is dynamic.

**`Pass::set_state_override` has to preserve the stencil rather than replace
it**, and this was a latent bug rather than a design note: the near-plane cap
sets its own override, `StateOverride` is one struct, and replacing the whole of
it silently turned the stencil test *and* the stencil write off for the one draw
whose entire job is to patch the mark. The two setters are now documented as
independent, and `World::draw_cap` takes the base override it is extending.

**`ShaderKind::from_name` has to answer `BufferClearObeyStencil`.** This is the
one thing here that no unit test could have caught and the running game caught
immediately, with a panic on the first map load:
`___bufferclearobeystencil_depth names a shader this port does not have`. The
material is built in code, the way Valve builds its eight, but
`MaterialCache::synthetic` parses that literal through the same `.vmt` path as
any file — so the shader's name has to resolve like any other, even though no
shipped `.vmt` writes it.

### Stage 2 — a second camera in a pass

`Pass::set_camera` pushes a second `FrameUniforms` slot into the arena the pass
already owns and rebinds group 0. Nothing else was needed: `Engine::render`
built its `Camera` at one call site, exactly as `CLAUDE.md` predicted.

### Stage 3 — visibility from somewhere else

`vis::ViewPoint`, `Visibility::mark_view` and `merged_row`, which ORs several
clusters' PVS rows the way `Map_VisSetup`'s array of origins does.
`World::visible_through` filters the exit portal's five origins through
`cluster_at` before asking, which is `portalrenderable_flatbasic.cpp:641`'s
`GetLeafContainingPoint( ... ) != -1`.

Measured on `sp_a1_intro1` from a pair in the spawn container: **98 faces in 23
leaves** through the portal, against the main view's 99 in 26 — the sub-scene is
the same order of magnitude as the scene, which is the point of doing it at all.

### Stage 4 — the recursion

`src/engine/world/portalview.rs`: the four-step loop, `NdcRect`,
`portal_cameras`, `oblique_near_plane`, `near_plane_cap`, and the two new
materials driven from `world/portals.rs`.

**One divergence, recorded at the site:** `exit_clip_plane`'s degeneracy guard
measures the *virtual* eye's distance from the exit plane, where Valve measures
the **real** camera's origin against the **exit** portal's normal — two
different rooms, and not a meaningful distance. Valve's spelling only ever ran
where `UseFastClipping()` was false; here the shear is the only path there is,
so the guard fires every time the player is within two units of a portal.

### What the rendered test found, twice, about itself

Both of these cost a debugging round and both will bite the next person writing
a rendered portal test, so they are written into the test:

**A portal placed by tracing was behind an opaque surface.**
`World::collision` is the *world model*, so a trace out of `sp_a1_intro1`'s
spawn goes straight through the container the player wakes up in and lands on a
wall 354 units away that the container is drawn in front of. The depth test
then rejected the hole and every assertion read as "nothing drew". Drawing does
not need a wall; the test now places its pair in open air.

**The hole cannot be measured in colour.** `$Stage 1` writes opaque black, so
against an unlit wall — or against the black clear every other shot uses — it is
invisible whether it drew or not. The wall-mounted check measures it *through
the stencil* instead: the hole marks, the brightly-coloured oval is drawn where
the mark is, and no mark means no oval.

### Verification, as it actually stands

`cargo test` is 1,032. The unit tests of §10 are in `portalview.rs`'s `tests`
module — the oblique projection against a hand-computed depth, the far corner
falling out as `1/far`, the degenerate projection, rectangle narrowing,
intersection and scissoring, and polygon clipping.

The rendered one is `portalview::rendered::the_view_through_a_portal_is_another_room_and_stays_inside_the_oval`:

```text
KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_view_through_a_portal -- --ignored --nocapture
```

It asserts, in order: both of the recursive view's materials are real and not
the error checkerboard; the sub-view's visible set is non-empty; the hole covers
pixels on its own; the first recursion level changes **14,130 of the opening's
14,184 pixels and none outside it**; the second changes **2,761 more, also none
outside**; and a wall-mounted portal marks **13,593 pixels** of stencil through
the wall it is coplanar with, which is what says `DepthBias::Decal` is still
winning.

### Frame cost

`engine::world::bench` grew two entries. On `sp_a1_intro1`, from the spawn, with
a linked pair sixty degrees apart in front of the camera:

| | ms/frame CPU |
|---|---|
| everything | 0.27 |
| + portal depth 1 | 0.53 |
| + portal depth 2 | 0.81 |
| everything, `novis` | 1.81 |

**Each level costs one more world draw**, which is what it is, and the last row
is what one level would have cost without the PVS. Two levels of recursion is
three times 0.27 rather than three times 1.81 — which is the whole reason
`portdocs/ENGINE_WORLD_VIS.md` went first.
