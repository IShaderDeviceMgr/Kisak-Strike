# Porting collision and tracing → `src/engine/trace/`

The engine's answer to "what is between here and there?" — ray and swept-box queries
against the world, against brush models, against displacements, and against entities.
It is the module `src/client/` stage 4 is blocked on, and the last thing on the boot
path between a noclip camera and a player who can stand on the floor.

Scope of this doc: `engine/cmodel*.cpp` (the BSP brush trace), `engine/enginetrace.cpp`
(the dispatch over collideables), `engine/spatialpartition.cpp` (the entity broadphase),
`public/dispcoll_common.*` (displacement collision), and the trace-shaped parts of
`public/` (`trace.h`, `gametrace.h`, `cmodel.h`, `bspflags.h`, `engine/IEngineTrace.h`).

Read `PORTING.md` first. `portdocs/CLIENT.md` §8 stage 4 is the consumer this is being
built for; `rustdocs/ENGINE.md` is the API doc for the module it lands beside.

---

## 0. Headline decisions

1. **The BSP brush trace is ported faithfully.** It is not a geometry problem with a
   library answer — it is the contract every line of movement code was written against,
   epsilons included. `portdocs/ENGINE.md` §7.17 already said "port faithfully"; §4 and
   §5 below are the evidence for *why*, which is stronger than the one line there.

2. **Rapier is the right call — one layer down from here.** The recommendation to use
   [Rapier](https://github.com/dimforge/rapier) is a good one and this doc adopts it, but
   for `vphysics/` (rigid-body simulation: cubes, ragdolls, funnels, turrets — 20,618
   lines of Valve code plus the whole `legacy/ivp` Havok/IVP submodule) and for the
   `.phy`/vcollide *sweep* (`vphysics/trace.cpp`, 2,474 lines of hand-written GJK). Its
   collision half, **`parry`, is also the recommended broadphase** when entities land.
   It is **not** the right tool for the world brush trace, for six reasons set out in
   §5.4 — the short form being that `parry` returns geometry and `trace_t` returns
   gameplay. §5 is a full evaluation, including what would reverse the decision.

3. **`spatialpartition.cpp` (3,219 lines) is not ported.** It is an entity broadphase
   over a world with no entities in it yet. When entities arrive, `parry`'s `Qbvh` is
   the replacement — enumeration order is not observable, because every consumer keeps
   the *minimum* fraction (`ClipTraceToTrace`, `enginetrace.cpp:1524`). §6.

4. **`portdocs/ENGINE.md` §7.14 and §7.17 needed correcting, and this doc is the
   correction** — applied to `ENGINE.md` when stage 1 landed. §7.17 sizes `trace/` at ~6,400 lines counting only `spatialpartition.cpp`
   + `enginetrace.cpp` + `gametrace_engine.cpp`, while §7.14 counts `cmodel.cpp` (4,067),
   `cmodel_bsp.cpp` (1,297) and `cmodel_disp.cpp` (603) under `world/`. **The BSP
   collision core is this module's, not `world/`'s** — `world/` reads lumps and draws
   faces, and none of it wants a brush. Corrected sizes in §2.

5. **The collision lumps are read by `world/bsp.rs`, not by a second reader.** Valve has
   two BSP readers (`modelloader.cpp` for rendering, `cmodel_bsp.cpp` for collision)
   because they lived in code that could not see each other's allocations. One crate has
   no such excuse: `bsp.rs` already reads fourteen lumps with bounds-checked `bytemuck`
   casts, and collision adds six more to the same reader. `trace/` owns the *derived*
   structures — box brushes, per-leaf displacement lists — not the file format. §7.4.

6. **`BeginTrace`/`EndTrace` become a borrow.** `TraceInfo_t` (`cmodel_private.h:37`) is
   a pool of scratch buffers handed out by `BeginTrace()` (`cmodel.cpp:66`) and returned
   by `EndTrace()`, with a `PushTraceVisits`/`PopTraceVisits` pair for the one re-entrant
   case. In Rust that is a `Tracer<'a>` borrowing the collision model and owning its own
   visited-brush stamps; re-entrancy is a second `Tracer`. §7.2.

7. **Portal-aware tracing is `game/shared/portal/`'s, not this module's** — but two
   `IEngineTrace` methods exist solely to serve it, and they constrain the data model
   here. `GetBrushesInAABB` and `GetBrushInfo` hand a caller a brush's **planes**, and
   the only callers in the tree are Portal's polyhedron cache and portal simulation.
   §4.10.

---

## 1. Scope: three layers, and only the first one is needed to walk

The subsystem is three layers that Valve interleaves and this port should not:

| Layer | Valve | Answers | Needed by |
|---|---|---|---|
| **World** | `cmodel*.cpp` | ray/box vs. the BSP's brushes and displacements | `client/` stage 4 |
| **Dispatch** | `enginetrace.cpp` | which collideables a ray touches, and how each is clipped | entities, props |
| **Broadphase** | `spatialpartition.cpp` | which entities are near a ray at all | many entities |

`client/` stage 4 — `FullWalkMove`, gravity, `CategorizePosition`, ducking, stair
stepping — needs **only the first layer**. `sp_a1_intro1` has no simulated entities in
the port today, and the world's brushes are what a player stands on. That is the whole
argument for stage 1's scope in §8: the module can deliver its blocking consumer before
either of the other two layers exists.

### What this module is not

- **Not physics simulation.** No rigid bodies, no solver, no constraints, no mass. That
  is `vphysics/` and it is where Rapier goes (§5.3). A trace asks a question about static
  geometry and returns; nothing integrates.
- **Not portal traces.** `UTIL_Portal_TraceRay` (`portal_util_shared.cpp:638`) and
  `CPortalGameMovement::TracePlayerBBox` (`portal_gamemovement.cpp:1955`) are *game*
  code that calls this module several times per query and stitches the results across a
  portal pair. They land in `src/client/` and `src/server/` when portals do.
- **Not visibility.** `CM_ClusterPVS`, `CM_Vis`, `CM_DecompressVis` and the area/areaportal
  flood (`cmodel.cpp:3318-3700`) live in `cmodel.cpp` because they share the leaf array,
  not because they are collision. They belong to `world/`'s visibility work (§7.14) and
  this doc only claims the leaf lookup they will call.
- **Not `collisionutils.cpp` wholesale.** 3,409 lines of standalone geometry predicates
  (`IsBoxIntersectingBox`, `ComputeSeparatingPlane`, ray/OBB, ray/sphere, quadratic
  solvers). It is a library, not a subsystem: port the handful of functions each caller
  needs, at the call site, and let `glam` do the rest.

---

## 2. Inventory

Line counts measured in this tree, not inherited from `ENGINE.md`.

### The world trace — 5,967 (`engine/`, currently mis-filed under §7.14)

| File | Lines | What it is |
|---|---:|---|
| `cmodel.cpp` | 4,067 | The trace itself: `CM_BoxTrace`, `CM_RecursiveHullCheck`, `CM_ClipBoxToBrush`, `CM_TestBoxInBrush`, leaf/point queries, plus PVS/areas (not ours) and the occlusion-query system (deleted) |
| `cmodel_bsp.cpp` | 1,297 | Loading the collision lumps into `CCollisionBSPData`, and the box-brush extraction pass |
| `cmodel_disp.cpp` | 603 | Displacement collision: per-leaf disp lists, the trace-to-disp-tree entry, and the "stab" |
| `cmodel_private.h` | 658 | `cbrush_t`, `cboxbrush_t`, `cleaf_t`, `cnode_t`, `cbrushside_t`, `TraceInfo_t` |
| `cmodel_engine.h` | 124 | The `CM_*` declarations the rest of the engine calls |

Of `cmodel.cpp`'s 4,067, roughly **1,500 lines are the trace**, ~700 are the occlusion
system (`CM_IsFullyOccluded`, `CM_BrushOcclusionPass`, `OccludeWithBoxBrush` and their
SIMD shuffles — a CS:GO-era shadow optimization, deleted), ~600 are PVS/areas
(`world/`'s), and the rest is loading, debug commands and platform tests.

### The dispatch — 3,201

| File | Lines | What it is |
|---|---:|---|
| `enginetrace.cpp` | 3,177 | `CEngineTrace::TraceRay` and the per-collideable clip chain; also the occlusion job system (deleted) |
| `gametrace_engine.cpp` | 24 | Three `CGameTrace` accessors |

### The broadphase — 3,431

| File | Lines | What it is |
|---|---:|---|
| `spatialpartition.cpp` | 3,219 | A leaf-based entity list with hide/unhide, deferred insertion, query callbacks and debug rendering |
| `public/ispatialpartition.h` | 212 | Its interface — 30 virtuals, 14 of them debug rendering |

### Displacements — 2,026 (shared with `world/disp/`, §7.15)

| File | Lines | What it is |
|---|---:|---|
| `public/dispcoll_common.cpp` | 1,565 | `CDispCollTree`: an AABB tree over a displacement's triangles, with `AABBTree_Ray` and `AABBTree_SweepAABB` |
| `public/dispcoll_common.h` | 461 | Its declaration and the per-triangle cache |

### The public types — 778

| File | Lines | What it is |
|---|---:|---|
| `public/engine/IEngineTrace.h` | 304 | The interface the game DLLs call: 26 methods, filters, `CBrushQuery` |
| `public/bspflags.h` | 156 | `CONTENTS_*` (26 bits used), `SURF_*` (16), `MASK_*` (22 combinations) |
| `public/cmodel.h` | 134 | `Ray_t`, `csurface_t`, `cmodel_t` |
| `public/gametrace.h` | 111 | `CGameTrace` / `trace_t` |
| `public/trace.h` | 73 | `CBaseTrace`, `DISPSURF_FLAG_*` |

### What consumes it

- **`game/shared/gamemovement.cpp`: 27 `TracePlayerBBox` call sites.** That is stage 4's
  surface area, and every one of them is a place a divergence in trace semantics shows
  up as a movement bug.
- **182 `enginetrace->` call sites across `game/`**, of which the Portal-only
  brush-plane queries (§4.10) are the ones that constrain this module's data model.
- **`engine/` internally**: sound (occlusion and DSP presets), `render/` (decal
  placement, lightcache), `server/` (entity movement, triggers), and `world/` (leaf
  lookup). None of those exist yet.

**Total in scope: ~15,400 lines, of which roughly 6,000 survive contact with this plan**
— see §6 for what goes.

---

## 3. Dependency graph

**Depends on** (all of which exist today):

- `world/bsp.rs` — the lumps. Adds `LUMP_PLANES`(1), `LUMP_NODES`(5), `LUMP_LEAFS`(10),
  `LUMP_LEAFBRUSHES`(17), `LUMP_BRUSHES`(18), `LUMP_BRUSHSIDES`(19) at stage 1;
  `LUMP_DISPINFO`(26), `LUMP_DISP_VERTS`(33), `LUMP_DISP_TRIS`(48) at stage 3;
  `LUMP_PHYSCOLLIDE`(29) at stage 5. `Model::head_node` is **already parsed**
  (`src/engine/world/bsp.rs:214`), which is most of stage 2's input.
- `glam` — `Vec3` and nothing more exotic. The trace is dot products and lerps.
- `materials/` — **no**. This module never touches the GPU and its tests never open a
  window, which is the same property that makes `host/` and `input/` testable.

**Depended on by:**

- `src/client/` stage 4 — `full_walk_move` and friends. The seam is §7.3.
- `world/` later, for the leaf lookup its PVS work needs.
- `server/`, `render/`, `audio/` eventually. None of them exist.

**Nothing about this module is blocked.** That is what makes it the correct next port:
`materialsystem` stage 6 is also unblocked but is off the boot path, while this is the
only remaining item *on* it.

---

## 4. The architecture you need in your head

### 4.1 `Ray_t` is a centered box, and the offset is the trap

`Ray_t` (`public/cmodel.h`) does not store start and end. It stores:

```c
VectorAligned m_Start;        // the CENTRE of the box, not the caller's start
VectorAligned m_Delta;        // direction * length
VectorAligned m_StartOffset;  // add to m_Start to get the caller's start back
VectorAligned m_Extents;      // half-diagonal of the box
bool m_IsRay;                 // extents are ~zero
bool m_IsSwept;               // delta is non-zero
```

`Init(start, end, mins, maxs)` sets `m_Extents = (maxs-mins)/2`, moves `m_Start` to
`start + (mins+maxs)/2`, and stores `m_StartOffset = -(mins+maxs)/2`. Everything inside
the trace then works with a box centered on the origin, which is what lets
`CM_ClipBoxToBrush` push planes out by a single `DotProductAbs`.

**This is the first thing that will bite.** A Portal 2 player's hull is
`(-16,-16,0)-(16,16,72)` (`portal_mp_gamerules.cpp:173-174`), so `m_StartOffset` is
`(0,0,-36)` and `m_Start` sits 36 units above the feet. `CM_ComputeTraceEndpoints`
(`cmodel.cpp:2252`) adds the offset back before writing `startpos`/`endpos`, so the
*trace result* is in the caller's frame — the feet — while everything in between is in
the centered frame. Skip the re-offset and the player teleports 36 units up on the first
hit; do it twice and they sink into the floor.

`m_IsRay` and `m_IsSwept` are not conveniences. `IS_POINT` is a **template parameter** on
`CM_ClipBoxToBrush` and `CM_RecursiveHullCheckImpl`, and it changes the algorithm: point
traces skip bevel planes and compute `fractionleftsolid`, box traces offset every plane
and do not. In Rust that is a `const IS_POINT: bool` generic parameter or two functions;
it should not become a runtime `if` inside the inner loop.

### 4.2 What a trace actually returns

`CBaseTrace` (`public/trace.h`) plus `CGameTrace` (`public/gametrace.h`). The fields
that matter, and why:

| Field | Why it is load-bearing |
|---|---|
| `fraction` | 0..1 along `m_Delta`. Pulled back by `DIST_EPSILON` — see §4.4 |
| `plane` | The surface normal *and distance* of the brush side hit. `ClipVelocity` is a function of this normal; so is `CategorizePosition`'s 0.7 floor test |
| `contents` | The 32-bit `CONTENTS_*` of the brush hit — water, ladder, playerclip, grate. Gameplay branches on it constantly |
| `surface` | `{name, surfaceProps, flags}` — footstep sounds, friction, whether a portal may be placed (`SURF_NOPORTAL`) |
| `startsolid` | The box began inside a brush |
| `allsolid` | It began inside and never got out |
| `fractionleftsolid` | *How far along the ray it stopped being inside.* §4.5 |
| `dispFlags` | `DISPSURF_FLAG_WALKABLE` and friends, when a displacement was hit |
| `worldSurfaceIndex` | Which `msurface2_t` — decals and paint need it |

**None of that is geometry.** A shape-cast library returns time-of-impact, witness points
and a normal; the other seven fields are Valve data attached to the brush and its sides,
and they are the reason §5.4 says what it says.

Deleted from the Rust type: `m_pEnt`, `hitbox`, `hitgroup`, `physicsbone` (studio models
and entities, stage 4/5 at the earliest), and the `edict_t` backdoor.

### 4.3 `CM_RecursiveHullCheck`: descend, split, and overlap the split

`cmodel.cpp:2555`. The classic Quake hull check with Valve's shape:

1. **Walk down while the whole box is on one side.** The `while (num >= 0)` loop computes
   the signed distance of both endpoints to the node plane, plus `offset` — the box's
   extent projected onto the plane normal (`fabsf(extents·n)` summed per axis, or just
   `extents[type]` for the axial planes, which is what `plane->type < 3` is testing). If
   both are beyond `+offset` it takes child 0 and loops; both below `-offset`, child 1.
   No recursion, no stack.
2. **At a leaf, clip against its brushes** (`CM_TraceToLeaf`, `:2064`).
3. **Otherwise split, and overlap the halves.** `frac` and `frac2` are computed so the
   near half extends `DIST_EPSILON` *past* the plane and the far half starts
   `DIST_EPSILON` *before* it. Both children are visited, near first, and the near call
   may set `trace.fraction` low enough that the far call returns immediately on its
   `fraction <= p1f` guard.

The overlap is not sloppiness — it is what stops a brush that straddles the split plane
from being missed by both children. Reproduce the epsilon exactly.

**Brushes are in many leaves, so brush visits must be deduplicated.** `TraceInfo_t::Visit`
(`cmodel_private.h:78`) stamps a per-brush generation counter and skips repeats. Without
it, a brush spanning eight leaves is clipped eight times — not wrong, but it makes
`fractionleftsolid` accumulate incorrectly (§4.5) and it is the difference between a
trace being cheap and being quadratic. In Rust: a `Vec<u32>` of stamps sized to the brush
count, plus a counter, living in the `Tracer`.

### 4.4 `CM_ClipBoxToBrush`: the algorithm the whole port is defined against

`cmodel.cpp:1511`. For each side of a convex brush:

```
dist = plane.dist + |normal · extents|     // box: push the plane out
dist = plane.dist                          // ray: don't, and skip bevel sides
d1 = start·normal - dist
d2 = end·normal   - dist
```

`d1 > 0 && d2 > 0` — wholly in front of this plane, so wholly outside the brush: return.
`d1 <= 0 && d2 <= 0` — wholly behind, this plane cannot be the one hit: continue. Otherwise
the segment crosses, and it is an *enter* if `d1 > d2` and a *leave* otherwise. Keep the
largest enter fraction and the smallest leave fraction; if `enterfrac < leavefrac` at the
end and the enter beats the running best, that is the hit, and `leadside` names the plane
and the surface.

Three details that are behavior, not noise:

- **`DIST_EPSILON` = 0.03125** (`public/coordsize.h:35`) — 1/32 of a unit. The enter
  fraction is `(d1 - DIST_EPSILON) / (d1 - d2)`, clamped at zero, so a trace **stops
  1/32 unit short of the surface**. Every consumer assumes that gap: it is why the player
  does not fuse to walls, why `TryPlayerMove`'s re-trace after `ClipVelocity` finds room
  to move, and why stair stepping terminates.
- **Bevel planes are skipped for rays** (`side->bBevel`). `vbsp` adds redundant planes to
  non-axial brushes so that the "push the plane out by `|n·extents|`" trick produces the
  exact Minkowski sum for an AABB. They are correct for boxes and wrong for points, hence
  the skip. **A brush's plane set is therefore not its hull** — it is its hull *plus* the
  planes that make box sweeps exact.
- **Thin sides** (`side->bThin`) are a CS:GO addition. Check what Portal 2's maps
  actually set before porting the branch; the flag is loaded either way.

### 4.5 `startsolid`, `allsolid`, `fractionleftsolid`

The part of `trace_t` with no counterpart in any physics library, and the part movement
code uses to get unstuck.

- **`startsolid`** — the box was inside this brush at fraction 0.
- **`allsolid`** — inside, and never left. `fraction` is forced to 0 and
  `fractionleftsolid` to 1.
- **`fractionleftsolid`** — started inside, and *this* is the fraction at which it got
  out. Accumulated as a maximum across every brush the trace starts inside
  (`cmodel.cpp:1628-1655`), then rescaled across the world/entity split
  (`enginetrace.cpp:2879`), then turned into `trace.startpos` by
  `CM_ComputeTraceEndpoints` — so `startpos` is **not** the ray's start when the ray
  begins in solid. It is where the ray left solid.

Two rules that produce a plausible wrong answer rather than an error:

1. **`fractionleftsolid` is only computed for rays.** `CM_ClipBoxToBrush`'s own comment
   says computing it for box sweeps needs "*a lot* more computation", and
   `CEngineTrace::TraceRay` zeroes it for non-rays on the way out (`enginetrace.cpp:2958`,
   with `VEC_T_NAN` in debug builds to catch readers). A box trace's `startpos` is just
   the start.
2. **`startsolid` is per-trace, not per-brush.** `CM_ClipBoxToBrush` clears a *previous*
   brush's `startout` when this brush's enter fraction is behind the running
   `fractionleftsolid` — "we entered this brush after leaving the previous one, so we are
   still outside". Port the comparison as written.

### 4.6 Box brushes: a load-time optimization that behaves like a format

`cmodel_bsp.cpp:667`. At load, any brush with exactly six sides whose planes are all
axial (`plane->type <= PLANE_Z`) is converted to a `cboxbrush_t` — mins, maxs, and six
surface indices, 48 bytes — and the brush records `numsides = 0xFFFF` with
`firstbrushside` repurposed as a box index (`cmodel_private.h:181`). `CM_ClipBoxToBrush`
branches on `IsBox()` and calls `IntersectRayWithBoxBrush` (`cmodel.cpp:935`) instead.

Worth porting at stage 1 rather than later, for a reason that is not performance: **most
brushes in a Source map are boxes**, so the box path is the one most traces take, and a
slab test and a plane loop disagree about which face is hit at a corner. Getting both in
from the start means the disagreement is visible in tests rather than discovered as a
movement bug.

### 4.7 Contents masks, and where the filtering happens

`bspflags.h`. 26 `CONTENTS_*` bits and 22 named `MASK_*` combinations. The ones this port
will actually meet:

- `MASK_PLAYERSOLID` = `SOLID|MOVEABLE|PLAYERCLIP|WINDOW|MONSTER|GRATE` — what
  `PlayerSolidMask()` returns, and therefore what all 27 of stage 4's traces use.
- `MASK_SOLID`, `MASK_SHOT`, `MASK_OPAQUE`, `MASK_WATER` — the rest of gameplay.
- `MASK_SHOT_PORTAL` and `CONTENTS_BRUSH_PAINT` (`0x40000`) — Portal 2's, and evidence
  that this bit vocabulary is not CS:GO-specific baggage.

**The mask is applied at three levels**, and all three matter:

1. `CM_TraceToLeaf` skips a leaf whose `contents` shares no bit with the mask.
2. `CM_TraceToBrushList` skips a brush the same way.
3. `IsNoDrawBrush` (`cmodel.cpp:1872`) handles the `CONTENTS_IGNORE_NODRAW_OPAQUE`
   special case — an opaque brush with `SURF_NODRAW` on every side does not block
   `MASK_VISIBLE`.

That is a 32-bit AND inside the traversal, rejecting whole subtrees. It is not a
collision-group system and it does not map cleanly onto one (§5.4).

### 4.8 Displacements: an AABB tree, and the stab

`CDispCollTree` (`public/dispcoll_common.h:156`) is a per-displacement AABB tree over
`(2^power)² × 2` triangles — up to 512 for power 4. Leaves hold triangles; queries are
`AABBTree_Ray` (`:167`) and `AABBTree_SweepAABB` (`:174`). A leaf carries a list of the
displacements overlapping it (`CM_DispTreeLeafnum`, `cmodel_disp.cpp:194`), and
`CM_TraceToDispList` (`cmodel.cpp:1761`) walks it exactly as the brush list is walked.

The part that will surprise: **the "stab"** (`CM_PreStab`/`CM_Stab`/`CM_PostStab`,
`cmodel_disp.cpp:421-543`). A displacement is a surface, not a volume, so a box that
starts *inside* the terrain has no brush to report `startsolid` against. Valve fires a
second trace along `m_DispStabDir` to find which side of the surface the box is on and
synthesizes the contents from that. It is the ugliest code in the subsystem and it is
load-bearing for anything that spawns inside terrain.

Deferred to stage 3, and it is the one stage where `parry`'s `TriMesh` is a genuine
candidate rather than a bad fit — §5.5.

### 4.9 Entities, brush models and props: the clip chain

`CEngineTrace::TraceRay` (`enginetrace.cpp:2786`) is short and worth reading in full. Its
shape:

1. **Trace the world** with `CM_BoxTrace(ray, headnode=0, mask, …)`. Return early if the
   trace starts solid, or if the filter is world-only.
2. **Shorten the ray to the world hit** — and note the comment at `:2870`: the end is
   recomputed as `start + fraction*delta` and the delta re-derived by subtraction, rather
   than scaling the delta, so that the shortened ray quantizes exactly the way `endpos`
   does. Scaling the delta instead produces traces that miss things they should hit.
3. **Enumerate entities along the shortened ray** through the spatial partition.
4. **Clip to each** via `ClipRayToCollideable` (`:1350`), keeping the nearest with
   `ClipTraceToTrace` (`:1524`).
5. **Rescale the fractions** back onto the original unshortened ray.

`ClipRayToCollideable` is a five-way dispatch, in priority order: a custom ray test
(hitboxes), then **vphysics** (`ClipRayToVPhysics`, `:1115` — `SOLID_VPHYSICS`, which is
every static prop and every physics object), then **the BSP** for brush models
(`ClipRayToBSP`, `:1203`, which is just `CM_TransformedBoxTrace`), then an OBB, then an
AABB. Studio models get their contents and surface properties overwritten from the
`studiohdr_t` afterwards.

**`CM_TransformedBoxTrace`** (`cmodel.cpp:3253`) is how a rotating door works: transform
the ray into the model's local frame, run the ordinary `CM_BoxTrace` against the model's
headnode, and rotate the resulting normal back out. For an unrotated brush model it is a
subtraction. This is stage 2 and it is cheap, because `Model::head_node` is already
parsed.

### 4.10 The two methods that exist only for Portal

`IEngineTrace::GetBrushesInAABB` (`enginetrace.cpp:599`) and `GetBrushInfo`
(`:987`) return, respectively, the brush indices overlapping a box and a given brush's
**plane list** (`BrushSideInfo_t { cplane_t plane; unsigned short bevel; unsigned short thin; }`).
Every caller in the tree is Portal code:

- `staticcollisionpolyhedroncache.cpp:121,146,203` — converts brushes to polyhedra so a
  portal can be cut out of a wall.
- `portalsimulation.cpp:3415,3619` — collects the world and wall brushes inside a
  portal's AABB to rebuild local collision.
- `portal_player_shared.cpp:4211,4329` — the player's own portal-placement checks.
- `GetMeshesFromDisplacementsInAABB` (`enginetrace.cpp:900`, used at
  `portalsimulation.cpp:2930`) is the same idea for terrain.

**This is a hard constraint on the data model, and the target game is the one that
imposes it.** Whatever `trace/` stores brushes as, it must be able to hand back a brush's
planes, its contents, and which of its sides are bevels. That is free if brushes are
stored as Valve stores them and expensive-to-impossible if they have been baked into some
other representation at load.

---

## 5. Rapier and parry: the evaluation

The suggestion to use Rapier deserves a real answer rather than a line, because it is
right about a large part of this port and the part it is wrong about is not obvious from
outside the C++.

### 5.1 What the two crates are

- **`parry3d`** (0.30.2, August 2026) — collision detection and geometric queries: ray
  casting, **shape casting** (swept tests), point projection, contact manifolds, and a
  `Qbvh` acceleration structure. Shapes include `Cuboid`, `Ball`, `Capsule`,
  `ConvexPolyhedron`, `TriMesh`, `HeightField` and `Compound`.
- **`rapier3d`** (0.35.3, August 2026) — rigid-body dynamics on top of `parry`: islands,
  the constraint solver, joints, CCD, sensors.

### 5.2 The dependency cost, measured rather than assumed

The obvious objection — "it drags in `nalgebra` alongside `glam`" — **is out of date, and
checking it changed this doc's conclusion about `parry`**:

- `parry3d` 0.30.2 depends on **`glamx` 0.3**, which depends on **`glam` 0.33**. This
  project pins `glam = "0.33.6"`. `parry` would **share this port's existing math crate**
  and bring no second linear-algebra stack. (`glamx` is dimforge's own "glam extensions":
  it re-exports `glam` and adds `Pose2`/`Pose3`.)
- `rapier3d` 0.35.3 depends on `glam` 0.33 and `glamx` 0.3 **and still on `nalgebra`
  0.35**, plus `simba`, `wide`, `num-traits` and `approx`.

So the two are priced differently: **`parry` is cheap for this project and `rapier` is
not**. That distinction is what §5.3's ordering is built on.

### 5.3 Where they are the right answer — three places

1. **`vphysics/` → `rapier3d`.** 20,618 lines of Valve code sitting on the entire
   `legacy/ivp` submodule (Havok/IVP: `ivp_physics`, `ivp_controller`,
   `ivp_compact_builder`, `havana`). Portal 2 is a physics game — weighted cubes, the
   excursion funnel, turret knockdown, ragdolls, paint blobs. Porting IVP is not a
   sensible use of anybody's time and `PORTING.md`'s "prefer modern Rust crates" rule
   points directly at Rapier. **This is the single largest crate-substitution win left in
   the port**, and it is a bigger win than anything `trace/` could offer.
2. **`vphysics/trace.cpp` → `parry::query::cast_shape`.** 2,474 lines implementing GJK
   with a hand-written Voronoi-region simplex solver (`simplex_t::SolveGJKSet`, `:1927`,
   plus four `SolveVoronoiRegion*` functions and three `ClipRayTo*` routines) sweeping a
   support-mapped box against an IVP ledge tree (`CTraceSolverSweptObject::SweepLedgeTree_r`,
   `:1198`). That *is* `parry`'s job description. The work that remains either way is
   decoding `.phy`/`LUMP_PHYSCOLLIDE` into convex hulls — a fixed Valve format
   (`PORTING.md`, "Format is fixed"), and unavoidable whichever engine consumes it.
3. **`spatialpartition.cpp` → `parry`'s `Qbvh`.** 3,219 lines of leaf-list bookkeeping
   replaced by a maintained BVH. Safe because enumeration order is not observable:
   `ClipTraceToTrace` keeps the minimum fraction regardless of the order results arrive
   in.

Note that the reason Rapier is popular with Bevy is exactly (1) — Bevy needs rigid-body
dynamics and has no BSP brush model and no twenty-year-old movement contract to match.
That popularity is a strong signal for `vphysics/` and a weak one for `cmodel.cpp`.

### 5.4 Why not the world brush trace

Ordered strongest first.

1. **The return type is gameplay data, not geometry.** §4.2: seven of `trace_t`'s
   fields — `contents`, `surface.name`, `surface.surfaceProps`, `surface.flags`,
   `dispFlags`, `worldSurfaceIndex`, and the solid trio — are Valve data attached to
   brushes and brush *sides*. `parry` returns a time of impact, witness points, a normal
   and a feature id. Recovering the rest means keeping Valve's brush and side arrays and
   indexing them by feature id — at which point the data model is unchanged and only the
   arithmetic has been swapped.
2. **Portal needs the brushes back as planes.** §4.10. `GetBrushInfo` hands out a brush's
   plane list *including which sides are bevels*, and it is Portal-only code that asks.
   A `ConvexPolyhedron` or `TriMesh` inside `parry` has discarded exactly that.
3. **The epsilons are the behavior.** `DIST_EPSILON` (1/32 unit) appears in both the
   node split (`cmodel.cpp:2624-2637`) and the brush clip (`:1592`), and a Source trace
   deliberately reports a fraction that stops short of the surface. Stair stepping,
   `CategorizePosition`'s ground probe, `TryPlayerMove`'s clip-and-retry, and the
   wall-strafing players have muscle memory for are all tuned against that number. An
   exact convex sweep is *more correct* and *differently wrong*: it sticks on brush seams
   where the analytic offset does not. There are 27 `TracePlayerBBox` sites for it to
   show up in.
4. **`fractionleftsolid` has no counterpart.** §4.5. It is accumulated across brushes,
   rescaled across the world/entity split, and read by the unstuck paths. Nothing in a
   physics library produces it, because no physics library has Quake's "you may begin
   inside the world and must be told when you left it" contract.
5. **The mask is a 32-bit predicate evaluated mid-traversal.** §4.7. Source rejects whole
   *leaves* by contents before looking at a brush. `parry`'s `QueryFilter` predicate can
   express the test, but it runs per-shape after the broadphase has already produced
   candidates, which inverts where the cheap rejection happens.
6. **The acceleration structure already exists and is needed anyway.** The BSP tree *is*
   the broadphase, it ships in the file, and `world/` needs its nodes and leaves for PVS
   regardless of what traces against them. A `Qbvh` over the same brushes is a second
   index built at load to answer questions the first one already answers.

And the cost side is small: stage 1 is roughly 1,200-1,500 lines of Rust with no GPU, no
window and no I/O in its tests. This is not a case of hand-rolling something large to
avoid a dependency.

### 5.5 What would change this answer

Recorded so it can be revisited on evidence rather than re-argued from taste:

- **If the port explicitly decides Portal 2 movement need not match the shipped game**,
  reasons 3 and 4 evaporate and a `parry` `Cuboid`-vs-`Compound` shape cast becomes
  attractive. That would be a deliberate decision in `PORTING.md`, not a drift.
- **If displacement collision (stage 3) proves disproportionate**, `parry` is a genuine
  candidate *there specifically*: a displacement really is a triangle soup,
  `CDispCollTree` really is an AABB tree over triangles, and `TriMesh` carries per-triangle
  feature ids that the `DISPSURF_*` flags and surface properties can hang off. The stab
  (§4.8) is the part that would still have to be written by hand. **Left open, not
  decided** — see §9.
- **If profiling ever shows the ported traversal is the frame's cost.** Unlikely: this
  code shipped on 2011 consoles, and the port has no entities yet.
- **If `.phy` decoding lands for props (stage 5) and `parry` is already in the tree**,
  the marginal cost of *also* trying it against brushes drops to nearly nothing, and the
  comparison becomes an experiment rather than a rewrite. That is the cheapest moment to
  test reason 3 empirically, and stage 5 is the right time to do it.

---

## 6. What is deleted, and why

Roughly **9,400 of the ~15,400 lines in §2 do not become Rust.**

| Deleted | Lines | Why |
|---|---:|---|
| The occlusion-test system — `CM_IsFullyOccluded`, `CM_BrushOcclusionPass`, `OccludeWithBoxBrush`, `IntersectOcclusionInterval`, `COcclusionQueryJob`, `occlusion_test_*` commands | ~1,700 | A CS:GO-era shadow-culling optimization split across `cmodel.cpp` and `enginetrace.cpp`, complete with an associative brush cache and hand-written SIMD shuffles. It answers "is this shadow caster hidden", not "what did I hit". Rebuild from measurements if shadows ever need it |
| `spatialpartition.cpp` | 3,219 | §5.3 — `parry`'s `Qbvh` when entities exist. Half of `ISpatialPartition` is debug rendering |
| The SIMD/`fltx4` variants of every inner loop | ~600 | `glam` is SIMD-accelerated already, and `wgpu`/`rustc` autovectorize. Port the scalar path, which is also the readable one |
| X360/PS3 paths — `ps3_testf16`, `IsBoxIntersectingRayNoLowest`, `_GAMECONSOLE` branches | ~300 | `PORTING.md`: consoles are permanently out of scope |
| Hitboxes and studio-model traces — `ClipRayToHitboxes` (`enginetrace.cpp:508`), `CStudioConvexInfo` | ~400 | Needs `.mdl`, which is unported. Returns when models do |
| `ITraceListData` / `CTraceListData` / `SetupLeafAndEntityList*` / `CM_BoxTraceAgainstLeafList` | ~450 | A caching layer for "many traces against the same leaf set" — an optimization for AI and bullet spread, with no caller until there is one. `IEngineTrace`'s `AllocTraceListData`/`FreeTraceListData` go with it |
| `GetSetDebugTraceCounter`, `debugrayenable`, `BENCHMARK_RAY_TEST`, `dump_occlusion_map` | ~250 | Debug plumbing whose comment explains it exists to work around a DLL boundary that this port does not have |
| PVS / areas / areaportals in `cmodel.cpp` — `CM_DecompressVis`, `CM_ClusterPVS`, `FloodAreaConnections`, `CM_WriteAreaBits` | ~600 | Not deleted, **reassigned**: this is `world/`'s visibility work (§7.14). It sits in `cmodel.cpp` because it shares the leaf array |
| The interface tower — `IEngineTrace`, `ITraceFilter`, `IHandleEntity`, `ICollideable`, `IEntityEnumerator`, `CBrushQuery`'s release-function dance, the client/server `CEngineTrace` split | ~500 | `PORTING.md`: no `CreateInterface`, no version strings. `CBrushQuery` carries a function pointer purely because "release function is almost always in a different dll than calling code" — one binary deletes the problem, and `Vec<u32>` deletes the class |
| `Hunk_AllocName`, `CRangeValidatedArray`, `CUtlVector` | ~350 | `std`. `CRangeValidatedArray` exists because "we keep running into overflow errors here" — a bounds-checked `Vec` is the fix its comment is asking for |

Two client/server `CEngineTrace` subclasses collapse into one type: they differ only in
how a handle becomes a collideable, which is an entity-system concern that does not exist
yet and will be one function when it does.

---

## 7. The Rust design

### 7.1 Module layout

```
src/engine/trace/
  mod.rs      Tracer and the public queries; the module doc
  ray.rs      Ray (the centered box), Contents, the MASK_* combinations
  result.rs   Trace, Surface, Plane
  model.rs    CollisionBsp: planes, nodes, leaves, brushes, box brushes, submodels
  brush.rs    clip_box_to_brush, test_box_in_brush, the box-brush slab test
  hull.rs     recursive_hull_check, trace_to_leaf, the unswept path
  disp.rs     stage 3 — DispTree and the stab
  entity.rs   stage 4 — the collideable dispatch and ClipTraceToTrace
```

Sibling of `world/`, not inside it. They are separate concerns over one file: `world/`
answers "what do I draw", `trace/` answers "what do I hit", and the only thing they share
is the reader in `world/bsp.rs` (§7.4).

### 7.2 Types

```rust
/// A swept box. `Ray::line` is the degenerate zero-extent case.
pub struct Ray {
    start: Vec3,      // the CENTRE of the box (§4.1)
    delta: Vec3,
    offset: Vec3,     // start + offset == the caller's start
    extents: Vec3,
    is_ray: bool,
    is_swept: bool,
}

impl Ray {
    pub fn line(start: Vec3, end: Vec3) -> Ray;
    pub fn hull(start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> Ray;
}

/// `CONTENTS_*` and the `MASK_*` combinations (`public/bspflags.h`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Contents(u32);

impl Contents {
    pub const SOLID: Contents;
    pub const PLAYERCLIP: Contents;
    pub const BRUSH_PAINT: Contents;   // Portal 2's
    // ...
    pub const MASK_PLAYERSOLID: Contents;
    pub const MASK_SOLID: Contents;

    pub fn intersects(self, other: Contents) -> bool;
}

pub struct Surface { pub name: Option<Box<str>>, pub props: i16, pub flags: u16 }

pub struct Trace {
    pub start: Vec3,          // the CALLER's frame, after the offset is added back
    pub end: Vec3,
    pub plane: Plane,
    pub fraction: f32,
    pub fraction_left_solid: f32,   // rays only — §4.5
    pub contents: Contents,
    pub surface: Surface,
    pub disp_flags: u16,
    pub world_surface: Option<u16>,
    pub all_solid: bool,
    pub start_solid: bool,
}

impl Trace { pub fn did_hit(&self) -> bool; }
```

A `Contents` newtype with associated constants rather than the `bitflags` crate — this is
the shape `world/bsp.rs`'s `pub mod surf` already uses, and it keeps the dependency count
where `Cargo.toml` argues it should be.

The collision model and the query object:

```rust
/// Immutable, built once per map from the lumps.
pub struct CollisionBsp { /* planes, nodes, leaves, brushes, sides, box brushes */ }

impl CollisionBsp {
    pub fn build(bsp: &Bsp) -> Result<CollisionBsp, TraceError>;
    pub fn tracer(&self) -> Tracer<'_>;
    /// `GetBrushInfo` (§4.10) — Portal needs these back.
    pub fn brush_planes(&self, brush: BrushIndex) -> &[BrushSide];
    pub fn brushes_in_aabb(&self, mins: Vec3, maxs: Vec3, mask: Contents) -> Vec<BrushIndex>;
}

/// `BeginTrace`/`EndTrace` (`cmodel.cpp:66`, `:111`) collapsed into a borrow.
/// Owns the per-trace visited-brush stamps; reuse it across traces.
pub struct Tracer<'a> { bsp: &'a CollisionBsp, visited: Visited }

impl Tracer<'_> {
    pub fn trace(&mut self, ray: &Ray, mask: Contents) -> Trace;
    pub fn trace_model(&mut self, ray: &Ray, model: &Model, origin: Vec3, angles: Vec3,
                       mask: Contents) -> Trace;                 // stage 2
    pub fn point_contents(&self, point: Vec3) -> Contents;
    pub fn leaf(&self, point: Vec3) -> LeafIndex;
}
```

`&mut self` on `trace` is the honest signature: the visited stamps are mutated, and
hiding that behind interior mutability would buy nothing but a `RefCell`. Valve's
re-entrancy machinery (`MAX_CHECK_COUNT_DEPTH`, `PushTraceVisits`/`PopTraceVisits`) is a
second `Tracer` on the rare path that needs one — the borrow checker enforces for free
what the `m_nCheckDepth` counter was enforcing by convention.

`IS_POINT` becomes a const generic on the two hot functions:

```rust
fn clip_box_to_brush<const IS_POINT: bool>(&mut self, brush: &Brush) { … }
fn recursive_hull_check<const IS_POINT: bool>(&mut self, node: i32, p1f: f32, p2f: f32,
                                              p1: Vec3, p2: Vec3) { … }
```

monomorphizing the two paths exactly as the C++ template does, and keeping the bevel-skip
and the plane-offset out of each other's inner loop.

### 7.3 The seams

**With `client/` (stage 4).** `Client::run_move` (`src/client/mod.rs:756`) gains a
tracer:

```rust
pub fn run_move(&mut self, cmd: &UserCmd, dt: f32, tracer: Option<&mut Tracer<'_>>)
```

`Option` because `MOVETYPE_NOCLIP` needs no collision and a map may not be loaded — the
same shape `Scene::world: Option<World>` already has.

**The borrow that will bite, and its fix.** `Engine::update_client`
(`src/engine/mod.rs:561`) will want a `Tracer` borrowed from `self.scene.world` while
holding `&mut self.scene.client`. That is fine — they are disjoint fields — **provided
the call site names the fields directly**:

```rust
let mut tracer = self.scene.world.as_ref().map(|w| w.collision().tracer());
self.scene.client.run_move(&command, seconds, tracer.as_mut());
```

Adding a convenience method on `Scene` that returns a `Tracer` would borrow all of
`Scene` and make the next line fail to compile. `portdocs/CLIENT.md` §6.4 records the
same class of problem from stage 1.

**With `world/`.** `World` gains `collision(&self) -> &CollisionBsp`, built in
`World::load` beside the lightmap packing. `world/` does not otherwise know this module
exists.

**With the console.** One command, `trace`, registered by `trace/` (§8 stage 1): fire a
ray from the player's eye along the view vector and print the fraction, normal, contents
and surface name. It is the acceptance test for stage 1, it needs nothing that is not
already built, and it makes the module inspectable before `client/` stage 4 exists.

### 7.4 Where the data lives, and the one duplication not to reproduce

Valve reads the `.bsp` twice — `modelloader.cpp` for rendering and `cmodel_bsp.cpp` for
collision, each into its own hunk allocation, because the two lived in code that could
not share. **Do not reproduce this.** `world/bsp.rs` reads the file; `trace/model.rs`
builds query structures from what it read.

The split, concretely:

- **`bsp.rs` gains** `Plane`, `Node`, `Leaf`, `Brush`, `BrushSide` and the leaf-brush
  index array, as `#[repr(C)] Pod` structs in the style already there, plus their lump
  reads. Six new lumps at stage 1 (§3), all plain struct arrays — `bytemuck` handles
  them, and the file's own note about `binrw`/`deku` not being needed for `.bsp` holds.
- **`trace/model.rs` owns** the derived data: the box-brush extraction (§4.6), the
  per-leaf displacement lists (stage 3), and the surface-name interning that
  `csurface_t` gets from the texdata string table `bsp.rs` already parses.

One consequence worth stating: `LUMP_LEAFS` has **two versions** in the wild
(`bspfile.h:370`, `LUMP_LEAFS_VERSION = 1`), and version 0 carries per-leaf ambient
lighting inline while version 1 moved it to its own lump.
`CollisionBSPData_LoadLeafs_Version_0`/`_1` (`cmodel_bsp.cpp:391`, `:456`) are the two
readers. Portal 2 ships version 1; refuse version 0 with a named error rather than
misreading it, the way `bsp.rs` already refuses `LVLFLAGS_LIGHTMAP_ALPHA`.

---

## 8. Staged plan

Five stages. **Stage 1 is the one that unblocks `client/` stage 4**, and it depends on
nothing that is not already built.

### Stage 1 — the world brush trace — **DONE** (2,093 lines, 15 tests)

The collision lumps in `bsp.rs`; `CollisionBsp::build` with box-brush extraction; `Ray`,
`Trace`, `Contents`, `Surface`; `recursive_hull_check`, `trace_to_leaf`,
`clip_box_to_brush`, `test_box_in_brush`, the box-brush slab test, and
`CM_UnsweptBoxTrace`'s position-test path; `point_contents`; `leaf`. Plus the `trace`
console command (§7.3).

No entities, no displacements, no props, no brush models. The world's brushes are what a
player stands on and they are enough.

**Tests, with no GPU and no map file**: build a `CollisionBsp` by hand — a six-brush room,
a step, a wedge — and assert on it. The cases that matter, each of which is a bug this
port can otherwise ship:

- A ray down the middle: `fraction`, and that the impact is `DIST_EPSILON` short.
- A box sweep into a wall: the same fraction as the equivalent ray offset by the extent.
- A box starting inside a brush: `start_solid`, and `all_solid` when it never leaves.
- A ray starting inside: `fraction_left_solid`, and `Trace::start` **not** equal to the
  ray's start.
- A trace that begins and ends outside but passes through: fraction and normal.
- A hull sweep against a non-axial (bevelled) brush versus the same sweep as a ray —
  they must differ, and by the bevel offset.
- A box brush and the equivalent six-sided brush: same answer.
- A mask that excludes the only brush in the way: `fraction == 1`.
- A trace whose `delta` is zero: the unswept path, not a division by zero.

**Done when** `trace` printed from the console in `sp_a1_intro1` reports plausible
distances and real material names for the floor, and the nine cases above pass.

**Done.** `src/engine/trace/` (2,093 lines) plus six lumps and their validation in
`src/engine/world/bsp.rs`. `sp_a1_intro1` loads 1,681 brushes (1,010 of them box
brushes), 7,058 sides, 1,958 nodes, 2,038 leaves and 11,324 planes; `trace` at the spawn
reports the floor as `MOTEL/HOTEL_CARPET001` 8.97 units below the feet with a `(0, 0, 1)`
normal, and a `TOOLS/TOOLSPLAYERCLIP` brush 127 units ahead with contents `0x8010000`
(`PLAYERCLIP | DETAIL`). All fifteen cases pass, plus two lump-stride tests. **API:
`rustdocs/ENGINE.md`, `src/engine/trace/`** — read that before calling in.

#### Corrections to this plan, found while implementing

- **§7.2's `Tracer::point_contents`/`leaf` are on `CollisionBsp`.** Neither needs the
  per-trace scratch — a point is in exactly one leaf, so there is nothing to
  deduplicate — and `&self` is the honest signature. `Tracer` is only for `trace`.
- **`CollisionBsp::build` is infallible, so there is no `TraceError`.** Every cross-lump
  reference the trace walks is checked by `Bsp::parse`'s `validate`, which is what buys
  the right to index without bounds tests in the inner loop. Putting a second set of
  checks here would have been checking the same thing twice.
- **`brush_planes`/`brushes_in_aabb` are not built.** They are Portal's (§4.10) and have
  no caller until portals exist; building them now would be guessing at a shape. The
  *data model* keeps the ability, which is the part that mattered.
- **`CLeaf` had to carry `cluster`.** `CM_PointContents` branches on `cluster < 0`, not
  on the brush count, and the two are not equivalent — see `rustdocs/ENGINE.md`'s gotcha
  8.
- **The stage-1 test list was nine cases and shipped as fifteen.** The additions worth
  naming: bevel planes binding a hull and not a ray (§4.4, which nothing in the nine
  covered), and a `Tracer` reused across traces, which is the only way the visit stamps
  can be observed at all.

### Stage 2 — brush models (small)

`CM_TransformedBoxTrace` (`cmodel.cpp:3253`): transform the ray into the model's frame,
trace against `Model::head_node`, rotate the normal back. `Model` is already parsed
(`world/bsp.rs:209`), so this is the transform plus a normal rotation.

Unblocks doors, platforms, and the moving parts of a test chamber — none of which move
yet, but all of which are solid.

### Stage 3 — displacements (medium, and the ugly one)

`LUMP_DISPINFO`/`LUMP_DISP_VERTS`/`LUMP_DISP_TRIS`; a `DispTree` (AABB tree over
triangles); the per-leaf displacement lists; `CM_TraceToDispList`; the stab (§4.8).
Pairs with `world/disp/`'s rendering work (§7.15) — one lump read, two consumers, and
the same argument as §7.4 for doing them together.

**This is the stage where `parry` should be reconsidered on its merits** (§5.5), because
a displacement is a triangle soup and this is the one part of the module where the Valve
structure has no semantics a library would have to reproduce — beyond the per-triangle
`DISPSURF_*` flags, which map onto feature ids.

### Stage 4 — entities and the dispatch (blocked on entities)

`ClipRayToCollideable`'s dispatch, `ClipTraceToTrace`, the filter (a Rust trait or a
closure, not `ITraceFilter`), the world/entity fraction rescaling (§4.9), and a
broadphase. Blocked on there being entities, which means `server/`.

### Stage 5 — vcollide and static props (blocked on `.phy`, and where `parry` lands)

`LUMP_PHYSCOLLIDE` and the models' `.phy` files decoded into convex hulls, then
`parry::query::cast_shape` in place of `vphysics/trace.cpp`'s GJK. Static props are the
first consumer; `vphysics/` proper — and `rapier` — follows from the same data.

**This is the moment to test §5.4's reason 3 empirically**: with `parry` in the tree, the
same brush geometry can be swept both ways and the fractions compared. Cheap to do then,
and it turns a design argument into a measurement.

---

## 9. Open questions and risks

1. **Is `bThin` live in Portal 2?** `cbrushside_t::bThin` and `cboxbrush_t::thinMask`
   (`cmodel_private.h:169`, `:197`) are a CS:GO-era addition. Load the flag at stage 1
   regardless — it is in the format — but check a Portal 2 map before porting the branch
   that reads it, and record the answer here.
2. **`parry` for displacements — decide at stage 3, not now.** §5.5. The tell will be how
   much of `CDispCollTree`'s 1,565 lines is tree-walking (replaceable) versus
   displacement semantics (not).
3. **The port has no `sp_a1_intro1` to measure against on this machine.** The stats in
   `CLAUDE.md` came from a session with the depots mounted; nothing in §2 or §8 depends on
   them, but **stage 1's "done when" does**, and whoever picks this up needs the game
   files to close it.
4. **Surface names cost memory if interned naively.** `csurface_t::name` is a `const char*`
   into the texdata string table, and there is one `csurface_t` per texdata, not per
   brush side. Keep `Surface` pointing at an index into a table `CollisionBsp` owns; a
   `Box<str>` per trace result allocates on every hit. (The signature in §7.2 shows
   `Option<Box<str>>` for legibility — the implementation should be an index, and
   `rustdocs/` should say so.)
5. **Does `trace/` register cvars?** The C++ has `debugrayenable`, the occlusion commands
   and the trace counter, all deleted (§6). The `trace` command in §7.3 is this port's
   own, not Valve's, and should be marked as such where it is registered — the same
   honesty `noclip`'s wart note gets in `CLAUDE.md`.
6. **Areas and areaportals are unowned.** They live in `cmodel.cpp`, they are visibility,
   and §1 hands them to `world/`. If `world/`'s visibility work does not pick them up,
   `CM_AreasConnected` has no home and `server/` will want it. Flag it there rather than
   letting it drift back here.

---

## 10. Notes for whoever picks this up

- **Read `CM_ClipBoxToBrush` (`cmodel.cpp:1511`) and `CM_RecursiveHullCheckImpl`
  (`:2555`) in full before writing anything.** Together they are under 200 lines and
  they are 90% of what this module is. Everything else in §4 is context for those two
  functions.
- **The epsilon is not a detail.** If a number in this port disagrees with the C++ by
  about 0.03, it is `DIST_EPSILON` and it is deliberate.
- **`Ray::start` is the centre of the box and `Trace::start` is not.** §4.1. If a player
  ends up 36 units above the floor, this is why.
- **Write the hand-built test fixtures first.** A `CollisionBsp` for a room is about
  thirty lines of planes and brushes, and it makes every case in §8 stage 1 assertable
  without a map, a GPU, or the depots. It is also the only way to test `start_solid`,
  since deliberately spawning inside a wall in a real map is fiddly.
- **`legacy/` is latin-1 and the `Grep` tool may not be available.** When it is not,
  shell `grep` needs `-a` on anything under `legacy/` or a symbol that is present reads
  as absent. `CLAUDE.md` has the full warning; this module's files are among the ones it
  bites on.
- **Update `portdocs/ENGINE.md` §7.14 and §7.17 when stage 1 lands** (§0.4), and write
  `rustdocs/ENGINE_TRACE.md` — or a `trace/` section in `rustdocs/ENGINE.md`, matching
  how `input/` and `console/` are documented — as the code lands, not after.
