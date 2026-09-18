# Visibility — porting `mod_vis`, the areaportals, and `R_RecursiveWorldNode`'s pruning

**Status: ported.** `src/engine/world/vis.rs` is the whole of it, and
`rustdocs/ENGINE.md`, **"Visibility"**, is how to call it. This document is written
*after* the port rather than before it — visibility was a line item on `CLAUDE.md`'s "what
to do next" rather than a staged plan — so read it as the analysis that justifies the
shape, not as a plan to follow.

Everything below is relative to `legacy/`.

---

## 0. Headline decisions

1. **Three filters, not one.** The areas, then the PVS, then the frustum. Each can answer
   alone, each only ever removes, and they run in that order because that is the order of
   increasing per-item cost.
2. **The frustum comes out of the view-projection matrix**, by the Gribb-Hartmann
   extraction, rather than from a camera basis and a field of view. Valve builds one both
   ways in two different files; taking it from the matrix means the frustum cannot
   disagree with what is actually drawn, and it makes `R_SetupVisibleAreaFrustums`'
   orthographic branch fall out instead of needing to be written.
3. **Every PVS row is decompressed once, at load.** Valve decompresses one row per frame
   and caches the *marked leaf list* instead (`VisCache_Build` behind an eight-entry
   `viscache`). The rows are small — 135 KB for the largest map in the game — so this
   trades a little memory for taking the run-length decoder out of the frame entirely, and
   for having no cache to invalidate.
4. **Static vertices, dynamic indices.** A world batch keeps its vertex buffer and gathers
   the visible faces' indices into the per-frame arena. That is `GetDynamicMesh( false,
   g_WorldStaticMeshes[sortID] )` (`gl_rsurf.cpp:1168`) and the pattern
   `rustdocs/MATERIALS.md` was built for; `DynamicBuffers` existed and had no caller until
   this landed.
5. **Displacement faces get a leaf list that no lump holds.** See §3 — this is the one
   thing about the port that is not a simplification of the original but an addition.
6. **Areaportals start open.** See §4.

---

## 1. Inventory

| File | Lines | Disposition |
|---|---|---|
| `engine/mod_vis.cpp` | 467 | **Ported**, minus the cache and the multi-origin merge. |
| `engine/r_areaportal.cpp` | 623 | **Ported**, minus the debug draw. |
| `engine/cmodel.cpp`, areaportal half | ~180 | **Ported** — `FloodAreaConnections`, `CM_SetAreaPortalState(s)`, `CM_AreasConnected`, `CM_DecompressVis`. |
| `engine/gl_rsurf.cpp`, `R_RecursiveWorldNode` + `R_DrawLeaf` | ~200 of 6,465 | **Ported** as the walk; the rest of the file is *how to draw*, which is `src/materials/`'s. |
| `engine/debug_leafvis.cpp` | 701 | **Deleted.** A debug renderer that draws the leaf you are standing in. The `vis` console command prints the same facts. |
| `engine/OcclusionSystem.cpp` | 2,999 | **Deleted.** Runtime occlusion from `func_occluder` brushes, and **Portal 2 places none at all** in its 106 maps. |
| `game/server/func_areaportal.cpp` | 216 | **Ported** as `server::classes::AreaPortal`. |
| `game/server/func_areaportalwindow.cpp` | 218 | **Ported to the same class**, minus the distance fade — see §4.3. |

Deleted outright: **3,700 lines**, plus whatever `spatialpartition.cpp` would have cost had
the per-leaf renderable index been ported (§5).

---

## 2. What the data says

Measured over the depot with a Python lump reader, before anything was written:

| | |
|---|---:|
| Maps | 106 |
| Maps with **no** visibility lump | **0** |
| PVS clusters | 44,590 (most: `sp_a3_portal_intro`, 1,037) |
| Leaves | 220,537 |
| `LUMP_VISIBILITY` bytes | 5,239,083 (most: 235,771) |
| Areas | 1,081 (most: 33) |
| Areaportal records | 922 (most: 51) |
| `func_areaportal` + `func_areaportalwindow` entities | 409 |
| Clip portal verts | 3,840 |
| Displacement faces named by `LUMP_LEAFFACES` | **0 of 1,181** |

Two of those numbers decided things.

**922 areaportal records against 409 entities.** `vbsp` writes two `dareaportal_t` per
areaportal brush — one in each area's list, each naming the other area, both carrying the
same `m_PortalKey` — so 922 records are ~461 windows. That is still more than 409, which
means some windows have no entity at all. See §4.

**0 of 1,181 displacement faces are in any leaf's face list.** That is §3, and it is the
finding that would have produced a map with no terrain.

The PVS's power, computed the same way before committing to it: decompressing every row of
`sp_a1_intro1` and unioning the faces its clusters reach gives **28.9% of faces standing on
an average cluster**, and 8.4% on `sp_a2_intro`. Worth having.

---

## 3. The leaf-face list is incomplete, and the loader is what completes it

`LUMP_LEAFFACES` is the list every `dleaf_t` slices with `firstleafface`/`numleaffaces`,
and it names every world face **except a displacement's**. Not "sometimes": zero of the
game's 1,181.

The shipped engine gives a leaf a **second** list for them. `mleaf_t` carries
`dispListStart`/`dispCount` into a separate array that the *loader* fills from each
displacement's bounds — it is not read from the file, because no lump holds it. A
displacement's geometry is pushed off its base quad along per-vertex directions and can
reach well outside the quad's own leaf, so the association cannot be derived from the face
either.

This port rebuilds the same association and **appends it to the one list**, because by the
time a displacement reaches a batch it is an ordinary face: `Visibility::build` calls
`disp::Displacement::build` for each displaced face, takes the built grid's bounds, and
runs a box descent to find the leaves it reaches. 1,181 faces game-wide, 11 on
`sp_a1_intro1`, so the cost is nothing.

The depot test asserts every one of the 1,181 lands in at least one leaf. Without this, all
terrain in the game would be invisible and nothing else would look wrong.

---

## 4. The areaportals

### 4.1 They start open, where the engine's array starts closed

`CollisionBSPData_LoadAreaPortals` sets every `portalopen[i] = false`
(`cmodel_bsp.cpp:959`). Every `CAreaPortal` then opens itself: its constructor is `m_state =
AREAPORTAL_OPEN` and `Precache` calls `UpdateState`, which is
`engine->SetAreaPortalState( m_portalNumber, m_state )`.

So the shipped engine's resting state is "closed until an entity says otherwise", and it
works because every window that matters has an entity. **This port starts them open
instead**, because 922 records answer to 409 entities and a record with no entity would
otherwise never open — the areas behind it would be black for the whole level. Starting
open is also the direction that draws too much rather than too little.

`StartOpen` is then honoured on top, because it is real: **39 of the game's 206
`func_areaportal`s start closed**, and the other 167 say `StartOpen 1` explicitly.

### 4.2 The flood is ported and the renderer does not use it

`FloodAreaConnections` gives every area reachable from another through open windows the
same number, and `CM_AreasConnected` compares those numbers. `R_FlowThroughArea` tests the
*server's* `m_chAreaBits` — the flood, written by `CM_WriteAreaBits` and sent to the client
— before stepping into an area.

That test cannot fire here. There is one process, so there is no bit vector in flight, and
the flow already walks only open windows: flow is a subset of flood by construction. The
flood is ported anyway, as `Visibility::areas_connected`, because it is the question
`CM_LeavesConnected` and the sound system ask and neither is the renderer. The divergence is
recorded at the function.

### 4.3 The window half does not fade

`CFuncAreaPortalWindow::UpdateVisibility` closes its own portal when the viewer is further
away than `FadeStartDist`, so that a fogged translucent pane can stand in for the geometry
behind it. That needs a per-view update inside the render loop *and* the pane, and this port
has neither. A `func_areaportalwindow` is therefore a `func_areaportal` that starts open and
answers the same three inputs.

The cost is measured and visible in the census: `SetFadeStartDistance` (43) and
`SetFadeEndDistance` (43) are reported as unhandled inputs rather than accepted and ignored,
which is the honest state.

### 4.4 The window rectangle is clipped against five planes, not four

`GetPortalScreenExtents` clips the window's polygon to the frustum and projects what
survives, and its loop is `for( iPlane=0; iPlane < 4; iPlane++ )` — the four sides only. Four
planes through the eye do not bound a half-space: they bound a double cone, so a corner
*behind* the eye can survive and then project with a negative `w`, folding the rectangle
inside out.

This port clips against the near plane too. It cannot lose anything — geometry nearer than
the near plane is not drawn — and it is what makes every surviving corner projectable.

---

## 5. What is deliberately not ported

- **The viscache.** Eight entries, keyed on a sorted list of view clusters, because
  `Map_VisSetup` is called for water reflections, the 3D skybox and monitor cameras as well
  as the world. This port has one view. The marked leaf set is rebuilt every frame instead,
  and it costs **0.004 ms** on `sp_a1_intro1`.
- **The multi-origin merge.** `Map_VisSetup` takes an array of origins and ORs their rows
  together, for the same reason. One origin here.
- **`r_portalsopenall`, `r_portalscloseall`, `r_ClipAreaPortals`, `r_ClipAreaFrustums`,
  `r_DrawPortals`, `r_snapportal`, `r_ShowViewerArea`, `map_noareas`.** Eight cheats whose
  only purpose is to switch off a piece of this and look at the result. `r_novis` and
  `r_lockpvs` are ported because they are the two that answer "is visibility the reason I
  cannot see that".
- **The PAS.** `LUMP_VISIBILITY` carries an audible-set row beside every PVS row.
  `Bsp::pvs` reads the first of each pair and the second is never touched, because the PAS
  belongs to sound.
- **The per-leaf renderable index.** `CClientLeafSystem` keeps every renderable in a list
  per leaf, inserts and removes them as they move, and walks the leaves to build both the
  opaque and the translucent lists. This port culls a renderable by testing its box against
  the tree instead — exact, because a box is visible when some leaf it reaches is, and
  `O(depth)` rather than `O(1)`. What is lost is the *ordering*: Valve's translucent pass
  walks leaves back to front and this one sorts whole instances, which is a pre-existing
  divergence that visibility does not fix and does not worsen.
- **`SortVisViewClusters`' bug**, which sorts `viewcluster` and leaves `oldviewcluster` and
  `origin` where they were. Harmless for one cluster, and there is one cluster.

---

## 6. What it measured

| | |
|---|---:|
| `sp_a1_intro1`, whole frame, PVS off | **1.76 ms** |
| `sp_a1_intro1`, whole frame, PVS on | **0.28 ms** |
| Computing the visible set | **0.004 ms** |
| World faces standing, over 103 shipped spawns | **6.9%** |
| Leaves that drew themselves from inside (invariant) | 379 of 379 |
| Spawns from which the flow left the eye's own area | 13 of 103 |

The 6.3x is from `sp_a1_intro1`'s own spawn, which is inside the sealed starting container —
a favourable viewpoint, not a typical one. The 6.9% across all 103 spawns is the honest
figure, and it is also measured facing one fixed direction.

---

## 7. What visibility unblocks

**The recursive view** (`portdocs/PORTAL.md` §7). A portal's second camera draws the world
again from somewhere else, and without a PVS that is a second whole-map frame: two levels of
recursion would have been three times 1.76 ms. With one it is three times 0.28 ms, and
`VisibleSet::frustum` is already the shape the second camera needs.

**The 3D skybox**, for the same reason and more cheaply — `sky_camera`'s view is a third
pass over a second set of geometry.

Neither is blocked on anything here beyond what already exists.
