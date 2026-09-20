# The sky — porting `CSkyboxView`, `R_DrawSkyBox` and `sky_camera`

**Status: ported.** Written *before* the port, against `legacy/`, and §6's open questions
answered after it with the numbers they asked for. `src/engine/world/sky.rs` is the whole
of it, plus `src/server/classes/sky.rs` and one new `Load` variant;
`rustdocs/ENGINE.md`, **"The sky"**, is how to call it.

Everything below is relative to `legacy/`.

---

## 0. Headline decisions

1. **Two things share one name, and both are here.** The **2D skybox** is six quads of
   `skybox/<skyname><rt|bk|lf|ft|up|dn>` drawn around the camera at the far plane. The
   **3D skybox** is the map's *own geometry*, somewhere else in the same `.bsp`, drawn
   from a second camera at 1/16 scale. The second draws the first inside itself.
2. **Portal 2's sky materials are `UnlitGeneric`, so no new shader is needed.** This is a
   measurement, not an assumption — see §2. Valve's `Sky_HDR_DX9`
   (`stdshaders/sky_hdr_dx9.cpp`, 300 lines, three HDR compression paths) is **not
   ported**: the only materials in the game that name `sky` are the six
   `skybox/sky_l4d_c4m1_hdr*`, an import from Left 4 Dead that **no shipped map's
   `skyname` selects**.
3. **The 3D skybox is a second pass, not a second target.** The recursive portal view
   (`portdocs/PORTAL_RENDER.md`) stayed inside one pass because it needed the stencil;
   this one needs a *depth clear between the two pictures*, which is a pass boundary in
   `wgpu`. `Load` grows a third variant, `ClearDepth` — which is exactly what
   `CSkyboxView::Setup` leaves in `*pClearFlags`.
4. **Separation is by the PVS, not by the area bits.** `CSkyboxView::DrawInternal` slams
   the area bits to the sky camera's area alone; on a modern client that write is **dead
   code** (§4.2). What actually keeps the main map out of the sky picture is that the sky
   camera's cluster sees only the skybox room's leaves. `ViewPoint` already carries
   everything needed and does not change.
5. **`sky_camera` is a server class**, read once a frame through a `Server` accessor —
   the arrangement `env_tonemap_controller` already has, and for the same reason: its
   value is map state that an input can change.
6. **No fog.** `Enable3dSkyboxFog` is out of scope because there is no fog anywhere in
   this port yet, not because the 3D skybox is a special case. §7 is the honest cost.

---

## 1. Inventory

| File | Lines | Disposition |
|---|---|---|
| `engine/gl_warp.cpp` | 342 | **Ported** — `MakeSkyVec`, `st_to_vec`, `skytexorder`, `R_DrawSkyBox`, `R_LoadNamedSkys`. Minus `R_LoadSkys`' `sv_skyname` round trip (§4.1). |
| `game/client/viewrender.cpp`, `CSkyboxView` | ~270 of 8,700 | **Ported** — `Setup`, `DrawInternal`, `ComputeSkyboxVisibility`, `PreRender3dSkyboxWorld`. |
| `game/client/viewrender.cpp`, `CSkyboxView::Enable3dSkyboxFog` + `GetSkyboxFog*` | ~90 | **Deferred** — there is no fog. §7. |
| `game/server/SkyCamera.cpp/.h` | 220 | **Ported** as `server::classes::SkyCamera`, minus the HL2 `s_pBogusFogMaps` fixup (a list of 19 Half-Life 2 map names) and `skybox_swap` (§4.4). |
| `game/server/playerlocaldata.cpp`, `ClientData_Update` | ~15 | **Collapsed.** One process, one accessor — `portdocs/SERVER.md` §6, the same collapse `env_tonemap_controller` got. |
| `engine/cdll_engine_int.cpp`, `IsSkyboxVisibleFromPoint` | 12 | **Ported** as a leaf-flag read. |
| `game/server/skyboxswapper.cpp` | 120 | **Deleted.** `env_skyboxswapper` — **0 instances across the 106 maps.** |
| `materialsystem/stdshaders/sky_hdr_dx9.cpp` + 5 `.fxc` | ~700 | **Deleted.** §2 and headline 2. |
| `game/client/viewrender.cpp`, `CPortalSkyboxView` | 30 | **Deferred** with `IsSkyboxVisibleFromExitPortal` — `portdocs/PORTAL_RENDER.md` §9 already lists it. |

---

## 2. What the data says

Measured over the depot with a Python lump reader and a Python VPK reader, before
anything was written.

| | |
|---|---:|
| Maps | 106 |
| Maps placing a `sky_camera` | **7** |
| Maps placing **two** sky cameras | **0** |
| `sky_camera` `scale`, every one of the seven | **16** |
| `SURF_SKY` faces | **1,870**, across **36** maps |
| `SURF_SKY2D` faces | **0** |
| Leaves | 220,537 |
| Leaves with `LEAF_FLAGS_SKY` | **194,641** (88%) |
| Leaves with `LEAF_FLAGS_SKY2D` | **0** |
| Leaves with `LEAF_FLAGS_RADIAL` | **0** |
| Distinct `skyname` values | 5 |
| `skyname` values with **no material in the game** | **1** (`sky_day01_01`, named by 60 maps) |
| Sky materials naming the `sky` shader | **6**, selected by **0** maps |

Five of those decided things.

### 2.1 There is no 2D-only sky in Portal 2, and `CSkyboxView` is the only thing that draws the box

`SURF_SKY2D` appears on no face and `LEAF_FLAGS_SKY2D` on no leaf, so
`IsSkyboxVisibleFromPoint` returns `SKYBOX_3DSKYBOX_VISIBLE` or `SKYBOX_NOT_VISIBLE` and
never the middle value.

`SkyboxVisibility_t` is **still a three-valued enum here** (`vis::SkyVisibility`) rather
than the `bool` that would fit this content, and that is deliberate: the difference between
the two live values is exactly the difference between the two code paths below — only
`Sky3d` gets the second camera, but **both** get the box — so collapsing it would leave the
`.any()` test looking arbitrary.

That does **not** delete the main view's own sky draw. `ViewDrawScene`'s

```c
bool drawSkybox = r_skybox.GetBool();
if ( bDrew3dSkybox || ( nSkyboxVisible == SKYBOX_NOT_VISIBLE ) )
    drawSkybox = false;
```

is reached on every map that has sky faces and **no** `sky_camera` — 29 of the 36 — where
the sky box is drawn by the main view, first, with the main view's `zFar`. So both halves
of §5 are live content.

### 2.2 88% of leaves claim to see the 3D sky, and that is a compiler artefact

`vbsp` writes `leaf_p->flags = LEAF_FLAGS_SKY` on **every** leaf it emits
(`utils/vbsp/writebsp.cpp:146`, over the comment *"By default, assume the leaf can see the
skybox. VRAD will do the actual computation"*), and `vrad` clears it only inside
`BuildVisForLightEnvironment` (`utils/vrad/lightmap.cpp:1459`), which is called **only
from `ParseLightEnvironment` and `ParseLightDirectional`**. **80 of the 106 maps place
neither**, so on those every leaf keeps `vbsp`'s default and
`engine->IsSkyboxVisibleFromPoint` answers "yes" from inside a sealed room.

This is harmless in the shipped game and it is harmless here, because the *other* two
gates hold: a map with no `sky_camera` has no 3D skybox to draw (§4.3), and the sky box
quads are gated on a sky surface actually being in the drawn set (§5.3). It is recorded
because "the leaf flag is wrong on four fifths of the game" is exactly the kind of thing a
future session would otherwise spend an afternoon disbelieving.

Two of the seven `sky_camera` maps — `sp_a4_finale2` and `sp_a4_finale3` — have no
`light_environment`, so their 3D skybox is drawn from every leaf in the map. That is the
shipped behaviour and is reproduced rather than tidied.

### 2.3 `Map_VisForceFullSky` is always false

`vis.bForceFullSky` is set from `LEAF_FLAGS_RADIAL` (`mod_vis.cpp:391`), and **no leaf in
the game carries it**. So `Map_VisForceFullSky()` is a constant `false` and is not ported,
and with it goes the radial-vis branch of `ViewDrawScene` that clears the frame to the fog
colour.

### 2.4 The sky materials are `UnlitGeneric`

All four sky sets the game can actually load are complete (6 of 6 faces) and every one of
the 24 `.vmt`s names `UnlitGeneric`:

| `skyname` | maps | `.vmt` | note |
|---|---:|---|---|
| `sky_day01_01` | 60 | **absent from the game** | only `e1912` has a sky face to show it through |
| `sky_white` | 27 | `UnlitGeneric`, `$basetexture` + `$hdrbasetexture` + `$nofog` | 64×64 DXT1 |
| `sky_black` | 13 | `UnlitGeneric`, no `$nofog` | |
| `sky_fog` | 5 | `UnlitGeneric`, **`$color "{70 85 100}"` and no texture at all** | a flat colour |
| `sky_black_nofog` | 1 | `UnlitGeneric` | `$basetexture` points at `sky_black`'s |

Three consequences. **No new shader**, which is most of the work gone. **`$basetexture`
values carry a literal `.vtf` extension** (`"skybox/sky_whitert.vtf"`) — already handled,
because `texture::normalize_name` is `Q_StripExtension`. And **a sky material with no
`$basetexture` must draw its `$color` on the white texture**, which `Material::new`
already does for an unset texture parameter; `sky_fog` is the shipped case that proves it.

`sky_day01_01` resolving to nothing is not an error to handle: `MaterialCache` answers a
missing `.vmt` with the error material, and the only map that could show one is `e1912`, a
cut map. `R_LoadSkys`' fallback to `sky_urb01` is **not ported** — that material is not in
this game either, so the fallback can only turn one checkerboard into another.

### 2.5 The scale is always 16

Seven cameras, `scale 16` on all seven. The `scale <= 0` branch of `CSkyboxView` (which
means "scale 1") is implemented because it is two characters, and it is unreachable on
shipped content.

---

## 3. What a 3D skybox actually is

A second room, built at 1/16 scale, sitting somewhere else inside the same `.bsp` and
sealed off from the playable map. `sky_camera` marks the point in it that corresponds to
the world origin. Every frame the engine draws that room from a camera at

```
sky_eye = view.origin / scale + sky_camera.origin
```

with the player's own angles, then clears depth and draws the real map on top of it. The
real map's sky *surfaces* are never drawn at all — `R_DrawSurface` (`gl_rsurf.cpp:3760`)
turns a `SURFDRAW_SKY` face into `m_bSkyVisible = true` and emits no geometry — so those
faces are holes through which the first picture shows.

The division by `scale` is what makes the parallax right: walking 16 units in the map
moves the sky camera 1 unit, so a skybox building 100 units from the sky camera behaves
like one 1,600 units away.

---

## 4. Decisions with a reason behind them

### 4.1 `sv_skyname` is not ported

`R_LoadSkys` reads a `ConVarRef sv_skyname`, which `CWorld::Spawn` writes from
`worldspawn`'s `skyname` key. That round trip exists because the engine and the game DLL
were separate modules with no other channel. Here the `.bsp`'s `worldspawn` block is
already parsed twice — once by `engine::world::World::sky_name` and once by
`server::classes::World::sky_name` — and the renderer uses the first. The cvar would be a
third copy of one string.

**This closes `server::classes::World::sky_name`'s standing comment**, which said the
duplication "resolves when the 3D skybox lands and has one owner". The owner is
`engine::world::World`; the server's copy stays only because `ent_dump` prints it.

### 4.2 The area-bit slam is dead code, and the PVS is what separates the two pictures

`CSkyboxView::DrawInternal` opens with

```c
unsigned char **areabits = render->GetAreaBits();
savebits = *areabits;
memset( tmpbits, 0, sizeof(tmpbits) );
tmpbits[m_pSky3dParams->area>>3] |= 1 << (m_pSky3dParams->area&7);
*areabits = tmpbits;
```

`render->GetAreaBits()` returns `&CClientState::m_pAreaBits` (`engine/view.cpp:590`), the
**backwards-compatibility pointer**. The bits the area flood actually consults are
`m_chAreaBits` (`r_areaportal.cpp:289`), which is filled from `m_pAreaBits` by
`UpdateAreaBits_BackwardsCompatible()` — called once per frame at the top of
`SCR_UpdateScreen` (`gl_screen.cpp:299`), *before* `V_RenderView()`, and in any case a
no-op because the modern path (`C_BasePlayer::OnDataChanged` →
`render->SetAreaState`) sets `m_pAreaBits` to null. So the slam writes a pointer nothing
reads for the rest of the frame.

What keeps the playable map out of the sky picture is `render->ViewSetupVis( false, 1,
&m_pSky3dParams->origin )`: the skybox room is sealed, so the sky camera's cluster sees
only the skybox room's leaves. The area test in `R_CullNode` only ever *removes*, so a
wider area set cannot add anything the PVS has already refused.

The port therefore builds the sky view's visible set with the `ViewPoint` that already
exists:

```rust
ViewPoint { eye: sky_eye, origins: &[sky.origin], leaf: None }
```

— `eye` is the scaled camera, because that is what `R_SetupAreaBits` floods from
(`vVisOrigin = g_EngineRenderer->ViewOrigin()` with the sky view pushed); `origins` is the
sky camera's fixed entity origin, because that is what `ViewSetupVis` is handed. They are
deliberately different points and this is the one place in the engine where they are.

**This also makes a sky camera inside solid geometry safe.** If the scaled camera lands
inside the skybox's own terrain, `mark_view` sets `in_solid` and offers every area — and
the PVS row, taken from the sky camera's entity origin, still refuses everything outside
the skybox.

### 4.3 `area == 255` is `Option::None`

`PreRender3dSkyboxWorld` refuses when `local->m_skybox3d.area == 255`. That is not a fact
about areas: `sky3dparams_t` lives in `CPlayerLocalData`, `ClientData_Update` writes 255
into it when `GetCurrentSkyCamera()` is null, and 255 is chosen because the field is an
8-bit send prop. In Rust the whole thing is `Option<Sky3dParams>` and the sentinel
disappears — which is also why the port does not need `engine->GetArea()` on the server
side at all, and so does not need a new engine query from `server/`.

### 4.4 `ActivateSkybox` is ported; `skybox_swap` is not

`CSkyCamera` keeps a class list and `g_hActiveSkybox`; `GetCurrentSkyCamera()` returns the
handle if set and the list head otherwise, and `CEntityClassList::Insert` pushes to the
head — so **the list head is the last sky camera constructed**, i.e. the last one in map
file order. No map has two, so the tie-break is unobservable; it is reproduced because it
costs one line.

`InputActivateSkybox` is one assignment and is ported. `skybox_swap`, the `#ifdef PORTAL2`
console command that rotates the list, is not: it is a cheat that needs two cameras and
there are none. Its cycle-building code is also wrong in the shipped tree — it links
`m_pClassList->m_pNext->m_pNext = m_pClassList` unconditionally, which makes a two-element
list into a loop and leaves a three-element list broken.

### 4.5 Culling is turned off for the sky box quads

`MakeSkyVec`'s `st_to_vec` table maps `(s, t, width)` onto a different axis triple per
face, three of them with sign flips, and the result is fed to `MATERIAL_QUADS`. Deriving
the resulting winding, per face, and then carrying it through this port's
front-face convention (`rustdocs/ENGINE.md` gotcha #1, where Valve's `D3DCULL_CCW` does
*not* mean `wgpu`'s `Ccw`) is precisely the "winding argument that runs through two sign
conventions" that `portdocs/ENGINE_WORLD_DISP.md` got wrong once already.

There is nothing to gain by getting it right: the box is a cube centred on the eye, every
one of its six faces is seen from the inside and from one side only, and the whole thing
is six quads. `StateOverride { cull: Some(false) }` removes the question. Recorded as a
deliberate divergence.

### 4.6 One cvar, not three

`r_3dsky` (client, default 1) gates the second camera; `r_skybox` (client, cheat, default
1) and `r_drawskybox` (engine, cheat, default 1) both gate the same six quads. The port
takes `r_3dsky` and `r_skybox` and drops `r_drawskybox`, which can only ever be the same
switch twice. `r_skybox_draw_last` defaults to 0 off the PS3 and is not ported either.

---

## 5. The shape of the frame

### 5.1 With a 3D skybox — 7 maps

```
pass 0   camera = sky_eye, znear 2, zfar MAX_TRACE_LENGTH      Load::Clear
           the six sky quads, if a sky surface is in this view's set
           World::draw with the sky visible set
           translucent, if any                                 §6
pass 1   camera = the player's                                 Load::ClearDepth
           everything the frame already does
```

`Load::ClearDepth` is `CSkyboxView::Setup`'s

```c
*pClearFlags &= ~( VIEW_CLEAR_COLOR | VIEW_CLEAR_DEPTH | VIEW_CLEAR_STENCIL | VIEW_CLEAR_FULL_TARGET );
*pClearFlags |= VIEW_CLEAR_DEPTH;
```

spelled once. It clears stencil with depth, which is `ClearBuffers`' own pairing and which
the recursive portal view depends on: it starts every frame at stencil reference 0.

### 5.2 Without one, but with sky in view — 29 maps

No extra pass. The six quads are drawn at the top of the existing scene pass, with the
main camera's `zFar`, before any world geometry. That is `Shader_WorldEnd`'s
`!r_skybox_draw_last` branch (`gl_rsurf.cpp:3352`).

### 5.3 `m_bSkyVisible`

Both cases are gated on a `SURF_SKY` face being in the view's visible set, which is
Valve's `pRenderList->m_bSkyVisible`. The port already drops sky faces at load, so the
world keeps their `.bsp` face indices in a `Vec<u32>` and asks `VisibleSet::face` — one
scan over at most 266 indices (`sp_a3_01`'s, the largest in the game), short-circuited at
the first hit.

### 5.4 The clip planes

`zNear = 2.0` and `zFar = MAX_TRACE_LENGTH` (`1.732050807569 × 2 × 16384` = 56,755.84) are
`DrawInternal`'s, and the near plane carries Valve's own warning: *"if you can get really
close to the skybox geometry it's possible that you'll be able to clip into it with this
near plane"*. The main view's `zFar` is `r_mapextents × √3` = 28,377.92, so the sky view's
is exactly twice it.

The sky box's half-width is `zFar × SQRT3INV` with `SQRT3INV = 0.57735`, *"a little less
than 1/sqrt(3)"* — so a corner of the cube lands at `zFar × 0.999995`, just inside the far
plane. The box is drawn with an ordinary depth test against a cleared buffer; it does not
need `MATERIAL_VAR_IGNOREZ`, which is why Portal 2's `UnlitGeneric` sky materials work
without setting it where Valve's `Sky` shader forces it on.

---

## 6. The open questions, and what the measurement said

`the_3d_skybox_of_every_map_that_has_one` (depot-gated, in `sky.rs`) loads all seven maps,
spawns each one's entities and measures from the map's own `info_player_start`:

| map | sky faces | `SURF_SKY` in it | props | brush | translucent faces | refracting | box drawn | leaf says sky |
|---|---:|---:|---:|---:|---:|---:|---|---|
| `e1912` | 161 | 0 | 0 | 0 | 60 | 0 | no — `sky_day01_01` is missing | yes |
| `sp_a1_intro1` | 98 | 21 | 3 | 0 | 4 | 0 | yes | **no** |
| `sp_a3_01` | 93 | 87 | 0 | 1 | 6 | 0 | yes | no |
| `sp_a4_finale1` | 62 | 23 | 12 | 0 | 10 | 0 | yes | no |
| `sp_a4_finale2` | 51 | 19 | 48 | 0 | 10 | 0 | yes | yes |
| `sp_a4_finale3` | 50 | 32 | 2 | 0 | 2 | 0 | yes | yes |
| `sp_a4_finale4` | 51 | 0 | 0 | 0 | 0 | 0 | no — no sky brush in the room | no |

**1. Is there anything translucent or refracting inside a skybox room?** Translucent, yes —
**six of the seven**, between 2 and 60 world faces. Refracting, **none at all**, on any of
the seven. So `draw_sky_view` draws the box, the room and the room's translucents *in one
pass*, and never needs `update_refract_texture`'s pass-copy-pass dance. Leaving the
translucent draw out would have been a visible hole rather than a saving.

**2. Do the portal ovals leak into the sky view?** They would have. `draw_sky_view` builds
the translucent list and drops `Translucent::Portal` from it before drawing. **Four shipped
maps place both a `prop_portal` and a `sky_camera`** — `sp_a1_intro1`, `sp_a4_finale1`,
`sp_a4_finale2` and `sp_a4_finale4` — so this is live content, not a hypothetical: without
the guard, an oval sixteen times too big would hang in the skybox as soon as map logic
switched one on, and it is not a bug anyone would connect back to here.

**3. Does the sky box draw at all on `sp_a1_intro1`?** Yes — 21 of its sky surfaces are in
the sky camera's own PVS. But **the 3D skybox does not draw from that map's spawn at all**,
because the spawn leaf does not claim to see the sky: the player wakes up inside a sealed
container. `+map sp_a1_intro1` shows no sky until you walk out of the room. Two of the
seven (`e1912`, `sp_a4_finale4`) draw the *room* and not the box — the first because
`sky_day01_01` does not exist, the second because its skybox has no sky brush of its own.

**4. What does the sky pass cost?** `engine::world::bench` grew a `3d skybox` row, which
records the whole sky view — box, room and translucents — as one pass. **0.41 ms on
`sp_a1_intro1` against `everything`'s 0.44 in the same run, and 0.34 against 0.27 on
`sp_a4_finale2`**: about one more world draw, which is what a second camera is, and the
same shape of answer the recursive portal view gave. Both spawns have a small main view
(99 and 16 faces) and a comparable sky view (59 and 41), so those are two views of similar
size rather than a cheap one beside an expensive one. See `rustdocs/ENGINE.md`, "Frame
cost, measured", for the standing caveat about back-to-back runs.

**And one the list did not think to ask.** The test asserts that **no face is in both the
sky view's set and the player's**, on all seven maps. That is the whole of what §4.2 claims
— that the PVS, not the area bits, is what keeps the two pictures apart — and it is the one
assumption whose failure would be spectacular and silent: the level drawn a second time at
1/16 scale behind itself.

---

## 7. What is deliberately not ported

- **Fog.** `Enable3dSkyboxFog`, `GetSkyboxFogEnable/Color/Start/End/MaxDensity` and
  `fogparams_t`. There is no fog in this port at all — `$nofog` is parsed into every
  shader's flag word and nothing reads it — so the 3D skybox is not a special case, and
  the six `fog*` keys on `sky_camera` are parsed and recorded so that the "every declared
  key is consumed" invariant holds. **The visible cost is real and is largest on the two
  maps whose sky is a fog colour**: `sky_fog` is a flat `{70 85 100}` and is meant to
  disappear into the fog that is not there.
- **`Sky_HDR_DX9`.** §2.4.
- **The stereo skybox scale matrix** (`materials->IsStereoActiveThisFrame()`), the
  depth-of-field `dev/clearalpha` full-screen patch, `CGlowOverlay::UpdateSkyOverlays` and
  `PixelVisibility_EndCurrentView` — none of the four subsystems exists here.
- **`CPortalSkyboxView` / `IsSkyboxVisibleFromExitPortal`.** The 3D skybox seen *through* a
  portal. Already on `portdocs/PORTAL_RENDER.md` §9's list; `m_nSkyboxVisibleFromCorners`
  in `PortalMoved` is the flag it wants.
- **`env_skyboxswapper`.** Zero instances.
- **`R_UnloadSkys`' reference counting.** `Arc` is what that was.
