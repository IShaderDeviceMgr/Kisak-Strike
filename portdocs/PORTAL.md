# `game/*/portal/`: `prop_portal` — teleportation and a frame

The portal itself: a pair of linked holes you can walk through. Scoped deliberately to
**teleportation and a drawn frame**, not the recursive view — you will see the *wall*
through a portal, with a coloured oval on it, and walking into it will put you out of
the other one facing the right way.

**Status: stages 1 and 2 of §10's five have landed.** The blended pass, then the class
and its oval. Sizes and line numbers are from `legacy/`; every count of entities, models
or materials is measured against the 106 shipped maps or the mounted game, not estimated.

> **What stage 2 corrected in this document is recorded where it belongs** — §7.2's
> "two ways to draw it" is settled in §10, and §12's four open questions are answered
> there. The rest of §3–§9 was accurate; the notes marked **LANDED** below say which
> parts of it are now code.

This is the first module in the port that is Portal-specific rather than Source-generic,
and the first that touches `trace/`, `client/`, `server/` and `materials/` in one piece
of work. §10 is the staging that keeps that from being one commit.

---

## 0. What this is, and the one hard part

A portal is four separable problems, and three of them are small:

1. **The class** — an entity with a position, a partner, and a transform between the
   two. Small, and the framework for it already exists.
2. **A hole in the wall's collision.** This is the hard one, and everything else is
   inert without it: with no hole, the player walks into a wall and never reaches the
   portal plane, so the teleport never fires and the frame is a decal.
3. **The teleport**, which lives in the *movement*, not in the entity. The reference
   says so in as many words — `CPortal_Base2D::TeleportTouchingEntity` opens with
   `Warning( "PORTALLING PLAYER SHOULD BE DONE IN GAMEMOVEMENT\n" )`
   (`portal_base2d_shared.cpp:371`).
4. **Drawing an oval**, which is blocked on something this port has never needed: a
   blended pass.

§4 is problem 2 and contains the finding that makes this tractable at all — **the carve
does not need a polyhedron library**, because this port's brush clip consumes planes
and Valve's polyhedra exist only to be turned into `CPhysCollide`s.

---

## 1. Inventory

### 1.1 The module

| File | Lines | Disposition |
|---|---|---|
| `game/server/portal/prop_portal.cpp` | 988 | **Port ~250.** `Spawn`, `ResetModel`, the linkage group, `SetActivatedState`/`Fizzle`/`NewLocation`, `UpdatePortalLinkage`. Delete the particles, the sounds, the gun coupling, the gamestats, CEG. |
| `game/server/portal/portal_base2d.cpp` | 2,088 | **Port ~300.** `NewLocation`, `UpdateCorners`, `TestCollision`, `SetActive`, `IsFloorPortal`. Delete the mic/speaker pair, `PunchPenetratingPlayer`, `WakeNearbyEntities`, `ComputeSubVisibility`. |
| `game/shared/portal/portal_base2d_shared.cpp` | 995 | **Port ~120.** `UpdatePortalTransformationMatrix`, `IsEntityTeleportable`, `UpdateCollisionShape`, `GetExitSpeedRange`. `TeleportTouchingEntity`'s 520 lines are the *entity* path and are not needed while the player is the only teleportable thing (§8). |
| `game/shared/portal/prop_portal_shared.cpp` | 271 | **Port ~60** — `GetMinimumExitSpeed`/`GetMaximumExitSpeed`. The rest is `PlacePortal`'s fizzle taxonomy, which needs a gun. |
| `game/shared/portal/portalsimulation.cpp` | 5,298 | **Port ~250 of it** — §4. The hole box (`:466`), `CreatePolyhedrons` (`:3315-3715`), `CarveWallBrushes_Sub` (`:3716-3811`), `CreateTubePolyhedrons` (`:3812-3917`), `EntityIsInPortalHole` (`:790`), `IsRayInPortalHole` (`:1034-1079`). Everything about physics ownership, cloning and carved *entities* deletes. |
| `game/shared/portal/portal_gamemovement.cpp` | 5,245 | **Port ~750.** `HandlePortalling` (`:2214-2827`, 614 lines), `TracePortalPlayerAABB` (`:1640`), `PortalTracePlayerBBoxForGround` (`:1965`), `ShouldPortalTransitionCrouch`/`ShouldMaintainFlingAssistCrouch` (`:244`, `:252`). The other 4,500 is `CGameMovement` with portal conditions in it, and **`src/client/movement.rs` is already the port of that** — `CLIENT.md` stage 4 took `CPortalGameMovement` as the reference. |
| `game/shared/portal/portal_util_shared.cpp` | 3,472 | **Port ~200.** `UTIL_Portal_PointTransform`/`VectorTransform`/`AngleTransform`/`RayTransform` (`:1476-1525`), `UTIL_Portal_TraceRay( pPortal, … )` (`:638-1020`, of which the holy-wall half matters), `UTIL_IntersectRayWithPortal`, `UTIL_Portal_Triangles`. The `FindClosestPassableSpace` family, the complex multi-segment trace and `CTransformedCollideable` all wait for something that needs them. |
| `game/shared/portal/staticcollisionpolyhedroncache.cpp` | 586 | **Delete** — §4.3. It converts BSP brushes and static props into `CPolyhedron`s; this port traces planes. |
| `mathlib/polyhedron.cpp` | 3,895 | **Delete** — §4.3. This is the single largest saving in the module. |
| `game/shared/portal/portal_placement.cpp` | 1,663 | **Delete** — needs a gun (§8). |
| `game/server/portal/physicsshadowclone.cpp` | 1,220 | **Delete** — §8. |
| `game/server/portal/physicsclonearea.cpp` | 280 | **Delete** — §8. |
| `game/server/portal/pvs_extender.cpp` | 158 | **Delete.** The port has no visibility system to extend; `world/` draws every face every frame. |
| `game/shared/portal/portal_collideable_enumerator.cpp` | 106 | **Delete.** A spatial-partition enumerator; the port has no partition and `ENGINE_TRACE.md` §5 already says where one comes from. |

~26,600 lines across §1.1, of which roughly **1,900** have a counterpart here. The two
largest files are the two smallest ports: `portalsimulation.cpp`'s 5,298 lines are ~250
because §4.3 deletes the polyhedron half, and `portal_gamemovement.cpp`'s 5,245 are ~750
because `src/client/movement.rs` is *already* a port of the rest of that file —
`CLIENT.md` stage 4 took `CPortalGameMovement` as its reference precisely because Portal
2 overrides two dozen of `CGameMovement`'s methods.

### 1.2 The client-side renderer, all of which is out of scope

| File | Lines | |
|---|---|---|
| `game/client/portal/portalrender.cpp` | 2,113 | The recursive view. **This is what "not the full visual effect" means.** |
| `game/client/portal/portalrenderable_flatbasic.cpp` | 1,747 | The three-stage draw. §7 takes stage 2 only. |
| `game/client/portal/c_prop_portal.cpp` | 1,372 | |
| `game/client/portal/c_portal_base2d.cpp` | 941 | |
| `game/client/portal/c_portalghostrenderable.cpp` | 980 | The half of an entity that sticks out of the *other* portal. |
| `game/client/portal/portal_dynamicmeshrenderingutils.cpp` | 279 | |

### 1.3 Shaders

| File | Lines | Disposition |
|---|---|---|
| `stdshaders/portal_refract_helper.cpp` | 284 | **Port the `$Stage 2` branch** — §7.2. |
| `stdshaders/portal_refract_ps2x.fxc` | 282 | **Port the stage-2 branch.** |
| `stdshaders/portal_refract_vs20.fxc` | 140 | **Port.** |
| `stdshaders/portal_refract.cpp` | 99 | **Port** — the parameter table. |
| `stdshaders/writez_dx9.cpp` | 103 | **Port, or stub.** The model's own material (§7.1) and a depth-only pass this port has no other use for. |
| `stdshaders/portal.cpp`, `portalstaticoverlay.cpp`, `portal_*_vs20/ps2x.fxc` | 880 | **Delete.** The pre-`PortalRefract` shaders; no shipped `.vmt` names them. |

---

## 2. What the data says

Measured over `portal2/maps/*.bsp` — 106 maps, 60,925 entity blocks — and the mounted
game. Every number below decided something in §3–§8.

| | |
|---|---|
| `prop_portal` entities | **21**, across **10** of 106 maps |
| …on `sp_a1_intro1` | **2** — the default map already is the test bed (§11) |
| …with `Activated 1` | **0**. All 21 start switched off. |
| …with a `LinkageGroupID` key | **0**. All 21 default to group 0. |
| …with a `HalfWidth`/`HalfHeight` key | **0**. All 21 are the default size. |
| Inputs fired at a `prop_portal`, game-wide | **31 `SetActivatedState`**, **4 `NewLocation`**, nothing else |
| `NewLocation` connections | 4, all in `sp_a4_finale1`/`2`, both tractor-beam portals sent to the same point |
| Output connections on a `prop_portal` | **1** — `sp_a1_intro1`'s `OnPlayerTeleportFromMe` |
| Portal angles | 19 of 21 are axis-aligned yaws; the exceptions are one `90 180 0` and the two `NewLocation` targets (`0 30 0`, `-90 0 0`) |

The placement-side classes, all of which are moot without a gun (§8):

| Classname | Entities | Maps |
|---|---|---|
| `func_portal_bumper` | 2,383 | 93 |
| `func_noportal_volume` | 458 | 40 |
| `info_placement_helper` | 392 | 80 |
| `trigger_portal_cleanser` | 371 | 96 |
| `env_portal_laser` | 34 | 27 |
| `func_portal_detector` | 31 | 17 |
| `linked_portal_door` | 6 | 2 |
| `weapon_portalgun` | 3 | 2 |

**The headline is the 21.** A portal in Portal 2 is overwhelmingly a thing the *gun*
makes, and the gun is out of scope, so what this module delivers against shipped
content is twenty-one scripted portals — plus whatever a console command places, which
is how it will actually be exercised.

---

## 3. The class

### 3.1 Linkage is by group, not by colour

`CProp_Portal` keeps a file-scope `s_PortalLinkageGroups[256]`
(`prop_portal.cpp:44`), and `m_iLinkageGroupID` is a `FIELD_CHARACTER` keyed
`LinkageGroupID`, with `255` reserved as `PORTAL_LINKAGE_GROUP_INVALID`
(`prop_portal.h:23`) meaning "not yet linked".

**The pairing is not on `PortalTwo`.** `CProp_Portal::UpdatePortalLinkage`
(`prop_portal.cpp:544`) walks the group and takes the first portal that is

- not itself,
- **active**,
- **not already linked**, and
- **the same half-width and half-height**,

and then *forces* `m_bIsPortal2 = !m_hLinkedPortal->m_bIsPortal2`. So `PortalTwo` is
an output of linking, not an input to it — it decides colour and nothing else, which is
exactly what `portal_base2d.h:38` says: *"For teleportation, this doesn't matter, but
for drawing and moving, it matters."* `FindPortal( group, bPortal2, … )` searches *by*
`m_bIsPortal2`, but that is the gun's lookup, not the linker's.

With all 21 shipped portals in group 0 and starting inactive, the whole of this reduces
to: when a portal activates, link it to the other active unlinked one in its group.
Deactivating unlinks both sides (`:597` onward), which is what `SetActivatedState 0`
does.

### 3.2 The teleport matrix

`CPortal_Base2D_Shared::UpdatePortalTransformationMatrix`
(`portal_base2d_shared.cpp:78`) is the entire teleport, and it is ten lines:

```
matPortal1ToWorldInv = inverse( localToWorld )
matRotation          = identity, with m[0][0] = -1 and m[1][1] = -1   // 180° about up
*pMatrix             = matPortal2ToWorld * matRotation * matPortal1ToWorldInv
```

That 180° is why you come *out* of the exit portal rather than backing into it. It is
the single easiest thing in this module to leave out and the failure is not subtle.

**Convention.** Valve's `VMatrix` is row-major and its vectors multiply on the right;
`rustdocs/MATERIALS.md`'s first rule is that this port is the reverse on both counts. A
transcribed product will be silently inverted. Derive the composition from what it
*means* — "into the entrance's frame, turn around, out of the exit's frame" — rather
than from the operator order.

Everything downstream uses it three ways, and they are different operations:
`UTIL_Portal_PointTransform` (full transform), `UTIL_Portal_VectorTransform` (rotation
only — velocity), and `UTIL_Portal_AngleTransform` (compose with the angle matrix, then
read angles back out). `portal_util_shared.cpp:1476-1525`.

### 3.3 The portal is an OBB, and the port already has one

`CProp_Portal::ResetModel` (`prop_portal.cpp:265`) ends:

```cpp
SetSolid( SOLID_OBB );
SetSolidFlags( FSOLID_TRIGGER | FSOLID_NOT_SOLID | FSOLID_CUSTOMBOXTEST | FSOLID_CUSTOMRAYTEST );
```

and `CPortal_Base2D::UpdateCollisionShape` (`portal_base2d_shared.cpp:910`) builds a
six-plane box from `GetLocalMins()` = `(0, -halfWidth, -halfHeight)` to `GetLocalMaxs()`
= `(64, halfWidth, halfHeight)` — a box extending **64 units forward** of the portal
plane, in the portal's own frame.

That is the same shape `trigger_portal_button` already uses, so
`server::movement::Solid::Obb` and `server::obb::swept_box_touches_obb` cover it with
no new geometry. `CPortal_Base2D::TestCollision` (`portal_base2d.cpp:363`) is a box
sweep against it and nothing else.

`UpdateCorners` (`portal_base2d.cpp:1750`) is four adds. `m_plane_Origin` is
`(forward, forward · origin)`.

---

## 4. The hole

### 4.1 Why nothing works without it

A portal sits on a wall. The wall is solid. `full_walk_move` sweeps the player's hull,
`TryPlayerMove` stops it at the wall and slides along it, and the player's centre never
reaches the portal plane — so `HandlePortalling` never fires, no matter how correct it
is. **The hole is the prerequisite for every other part of this module**, which is why
it is §4 and not §7.

### 4.2 Valve's carve

`CPortalSimulator` splits the collision near a portal into two sets:

- **World** — what is in *front* of the portal plane. Clipped, but not holed.
- **Wall** — what is *behind* it, with a rectangular hole removed, plus a **Tube**: a
  minimal volume an object must fit inside to be eligible to pass.

plus **RemoteTransformedToLocal**, the linked portal's World set brought through the
matrix so that the far room's floor exists on this side while you straddle the plane
(§5).

The machinery is `CreatePolyhedrons` (`portalsimulation.cpp:3315-3715`) turning brushes
and static props into `CPolyhedron`s via `staticcollisionpolyhedroncache.cpp`, then
`ClipPolyhedron`/`ClipPolyhedrons` against plane sets, then
`physcollision->ConvertConvexToCollide` to make each result a `CPhysCollide` that
vphysics can trace and simulate.

The hole itself is six planes (`portalsimulation.cpp:466`), and it is just a box:

| Plane | Distance |
|---|---|
| `+forward` | `portalPlane.dist - 0.5` |
| `-forward` | `-portalPlane.dist + 500` |
| `±up` | `± halfHeight * 0.98` about the centre |
| `±right` | `± halfWidth * 0.98` about the centre |

Half a unit in front, five hundred behind, and **0.98×** the portal's half-size — not
1.0, and not symmetric about the plane.

### 4.3 This port does not need the polyhedron library

`CarveWallBrushes_Sub` (`portalsimulation.cpp:3716`) is the whole of the hole-cutting,
and it is **four clips of the same four side planes** — up, down, left, right — at four
different sets of distances:

| Piece | `+up` | `-up` | `-right` | `+right` |
|---|---|---|---|---|
| upper wall | `+halfHeight·40` | `-(holeH + 0.1)` | far left | far right |
| lower wall | `-(holeH + 0.1)` | `+halfHeight·40` | far left | far right |
| left wall | `+(holeH + 0.1)` | `+(holeH + 0.1)` | far left | `-(holeW + 0.1)` |
| right wall | `+(holeH + 0.1)` | `+(holeH + 0.1)` | `-(holeW + 0.1)` | far right |

where `holeW = halfWidth + PORTAL_HOLE_HALF_WIDTH_MOD` and `PORTAL_WALL_MIN_THICKNESS`
is the `0.1` (`portalsimulation.cpp:68-73`). That is the classic "wall around a
rectangular hole is four slabs" decomposition, expressed as distances on a fixed set of
normals.

The `CPolyhedron` is used for exactly two things, and **neither applies here**:

1. To decide whether a brush interacts with the hole at all — `ClipPolyhedron` returning
   `NULL` means "no part of this brush is in the hole", so it can be passed through
   uncut.
2. To be converted into a `CPhysCollide`, because Valve traces and simulates against
   vphysics.

This port traces BSP brushes. `clip_box_to_brush` (`src/engine/trace/brush.rs:18`) reads
`bsp.brush_sides[first..first + count]` and `bsp.planes[side.plane]` and **nothing
else**. So:

> **A carved piece is the original brush's planes plus the four side planes at those
> distances.** No vertices are generated, no convex hull is built, and a piece that came
> out empty needs no detection — an infeasible plane set produces `enterfrac > leavefrac`
> in the existing loop and reports a clean miss.

That deletes `mathlib/polyhedron.cpp` (3,895 lines) and
`staticcollisionpolyhedroncache.cpp` (586) outright, and reduces `CreatePolyhedrons`'
401 lines to a plane-list transform. It is the same shape of saving as
`ENGINE_TRACE.md`'s decision not to reach for `parry`: the port's existing
representation is already the right one, and the C++ is carrying a conversion this port
does not have to pay for.

### 4.4 Bevels: the one divergence to record

`clip_box_to_brush` expands each plane by `plane.normal.abs().dot(extents)` and relies
on `vbsp` having emitted **bevel planes** so that a swept box clips exactly at a brush's
edges and corners (`ENGINE_TRACE.md` §4.4). Carved side planes are generated at run time
and have no bevels.

Consequence: a swept box against a hole *corner* is slightly rounded — the box can clip
the corner by up to the difference between the true Minkowski sum and the per-plane
expansion. For an **axis-aligned** portal the side planes are axis-aligned, their own
bevels, and the result is exact. §2 says 19 of the 21 shipped portals are at axis-aligned
yaws, so this is exact for nearly all of the content that exists and becomes visible the
moment a gun can place one on an angled panel.

Record it; do not pre-emptively generate bevels. The fix, when wanted, is the same three
bevel planes per non-axial side that `vbsp` writes.

### 4.5 What still has to be written

1. **A brush enumerator over an AABB. LANDED** — `Tracer::brushes_in_box`, which is
   `CEngineTrace::GetBrushesInAABB` (`enginetrace.cpp:599`): `CM_BoxLeafnums`' descent,
   then the ordinary position test on each candidate, deduplicated by the sweep's own
   visit stamp. `rustdocs/ENGINE.md`, "The box query", is the reference.

   **A plain node descent collecting leaf brush lists is *not* enough**, which is what
   this section used to say. A leaf lists every brush that touches it, so the lists offer
   a great deal more than the box contains — measured over 2,862 probe boxes on the 106
   shipped maps, **the position test rejects 86.8% of what they offer** (25,700 down to
   3,387). Carving all of them would cut holes in walls the portal is nowhere near.

   **And the answer is leaf-limited, which the carve has to live with.** Three ways a
   brush is in the box and not in the answer, all of them the shipped engine's too: a
   brush **no leaf names** (25,744 of the game's 141,686 brushes are outside the world
   subtree, orphans and brush models' together); a **brush model's**, which is a
   different call in Valve as well; and a brush that **reaches past the leaves that list
   it** — `mp_coop_catapult_wall_intro` has one spanning `z −112..128` whose only world
   leaf stops at `z 96`. §4.3's claim survives all three, because the shipped game cuts
   against the same set.

   The box itself is the (holy) wall's, not the environment's: `-forward` by
   `2 × MAX( fHalfHeight, fHalfWidth )`, `±4 × fHalfWidth` across and
   `±4 × fHalfHeight` up (`:3524-3541`), taken to a world AABB through the OBB's eight
   corners. `vCollisionCloneExtents` — `MAX( hw, hh ) + portal_environment_radius` (75,
   `:125`) on x and the half-sizes plus 75 on y and z — is the **World** set's box
   (`:3370`), which extends *forward* of the plane only and is §5's, not §4's.
2. **The carved store**, rebuilt on `NewLocation`/activation and thrown away on
   deactivation. Cheap: it is a `Vec<CPlane>` and a `Vec<CBrush>`-shaped side table.
3. **A `Tracer` path that uses it.** `Tracer::with_entities` is the precedent — the
   caller decides what is in the chain and the module does not know why. The portal case
   is *substitutive* rather than additive, which is new: when the player is in a portal
   environment, the carved pieces replace the originals rather than joining them, and
   `TracePortalPlayerAABB` (§5) is how the two answers are reconciled.
4. **The tube** (`CreateTubePolyhedrons`, `:3812-3917`), same plane treatment.

---

## 5. The remote-side trace

`TracePortalPlayerAABB` (`portal_gamemovement.cpp:1640`) is the reconciliation, and its
shape is worth copying exactly:

1. Trace the ray against the ordinary world (`RealTrace`).
2. **Only if** the player is in an active linked portal's environment *and* the real
   trace hit something (`startsolid || (swept && fraction < 1)`), trace again against
   the portal's carved geometry with `bTraceHolyWall = true`.
3. Take the portal trace when it goes further, or when the real trace started solid.
4. Separately, trace `ray_remote` — the ray transformed into the *exit* portal's space —
   against the linked portal's tube, world-only. This is what holds the player up on the
   far room's floor while their box straddles the plane.
5. Take the nearer of (3) and (4), transforming (4)'s normal back.

Step 2's guard is the performance story: a player nowhere near a portal pays one extra
branch. Step 4's "world-only" is deliberate and commented — tracing remote *entities*
reintroduces "the projected floor to wall dilemma where we can ledge walk in the middle
of the portal" — and `sv_portal_new_player_trace_vs_remote_ents` defaults to `0`.

`PortalTracePlayerBBoxForGround` (`:1965`) is the ground probe, and it is the existing
four-quadrant trace with the same local/remote pair threaded through each quadrant.
`src/client/movement.rs` already has the quadrant logic; what it gains is the second ray.

**Not porting:** `pAABBAngleTransformCollideable`, the "transition ramp" that makes a
slightly-angled portal transition present as a standable slope instead of an unclimbable
step. It needs an angled portal to matter and §2 says the content barely has one. Leave
the hook and the `m_bContactedPortalTransitionRamp` flag out until a gun exists.

---

## 6. `HandlePortalling`

`portal_gamemovement.cpp:2214-2827` — 614 lines, and the teleport. It runs at the end of
the move, comparing where the move started to where it ended. In this port that is the
tail of `client::movement::player_move` (`src/client/movement.rs:1566`).

`MoveData` grows two fields: **the move's start position** (`m_vMoveStartPosition`) and
**the portal environment** the player was last touching.

### 6.1 Selecting the portal

A hull ray from the move's start to its end is tested against every active linked
portal's OBB (`TestCollision`), and survivors must pass three filters:

- the **old** centre must have been in front of the plane — unless this portal was
  already the player's environment, "special exception if we were pushed past the plane
  but did not move past it";
- if the new centre is *behind* the plane, it must hover over the portal quad;
- if it is in *front*, the line from the centre to its most-penetrating extent must pass
  through the quad — "avoids case where you can butt up against a portal side on an
  angled panel" — within
  `portal_player_interaction_quadtest_epsilon` (`-0.03125`, `:73`) and a 1-unit quad
  margin.

Nearest centre wins. Then the actual trigger is `planeDist < -FLT_EPSILON` against
`m_plane_Origin` — **the centre crossing the plane**, not the near face.

### 6.2 The frame split

```
fOldPlaneDist        = normal · prevCenter - dist          // positive
fPlaneDist           = normal · center     - dist          // negative
fIntersectionPercentage = fOldPlaneDist / (fOldPlaneDist - fPlaneDist)
fPostPortalFrameTime    = (1 - fIntersectionPercentage) * frametime
```

with `0.5` substituted when the denominator is zero — which happens, and the comment
names the bug: "some kind of physics penetration seems to be the cause (bugbait #61331)".

Gravity for the post-crossing part of the frame is **subtracted before the rotation and
added back after it at 1.008×** — "Apply slightly more gravity on exit so that
floor/floor portals trend towards decaying velocity. 1.008 is a magic number found
through experimentation." Without it an infinite floor-to-floor fall gains energy.

### 6.3 Velocity

Rotate by the transform, then clamp into the exit range from
`CPortal_Base2D::GetExitSpeedRange` (`portal_base2d_shared.cpp:977`), which asks the
**exit** portal:

| Situation | Minimum |
|---|---|
| Player, exit on floor | **300** |
| Non-player, exit on floor, entrance on floor | 225 |
| Non-player, exit on floor, entrance not | 50 |
| Player, exit not on floor but `forward.z > 0.5` | solve a quadratic for the speed that perches the hull on the portal's bottom edge, capped at 300 |

Maximum is **1000** flat (`prop_portal_shared.cpp`, `GetMaximumExitSpeed`). Below the
minimum, velocity is *added along the exit forward*; above the maximum, the whole vector
is scaled. Then a per-component `sv_maxvelocity` clamp, done quietly rather than through
`CheckVelocity`.

Two additions before the rotation: the implicit vertical speed a player carries when
walking a slope (velocity is xy-only on the ground), skipped when
`plane.normal.z > cos(30°)` — i.e. skipped for floor portals.

### 6.4 The forced duck

`ShouldPortalTransitionCrouch` (`:244`) is

```cpp
fabs( m_matrixThisToLinked.m[2][2] ) < COS_PI_OVER_SIX    // 0.8660254…
```

— "how much does zUp still look like zUp after going through this portal". An AABB
cannot rotate, so a wall-to-floor transition has to curl the player into the duck hull
*immediately*: `m_bInDuckJump = true`, `m_nDuckTimeMsecs = GAMEMOVEMENT_DUCK_TIME`,
`FinishDuck()` now, and `vOriginToCenter` recomputed against the duck hull so the
*centre* stays put.

`ShouldMaintainFlingAssistCrouch` (`:252`) keeps that duck when exiting a portal that
faces partly up (`0.1 < forward.z < 0.9`) at more than
`PLAYER_FLING_HELPER_MIN_SPEED` (200, `:103`), and there is a companion nudge that
moves the exit centre toward the portal's axis so a flung player does not stub the hull
corner on the exit lip — "the real world equivalent of stubbing your toe on the exit
hole results in flinging straight up."

### 6.5 Angles, and what this port does not have

Valve transforms four angle sets: the engine's view angles, the prediction's, `pl.v_angle`
and the entity's. **This port has one** — `client::ViewAngles`, which `CLIENT.md` stage 1
made the client's alone on Valve's own `// FIXME, move entirely to client .dll`. So the
whole `#if defined( CLIENT_DLL )` block of angle plumbing collapses to a single compose.

Also absent and not needed: `UnrollPredictedTeleportations`,
`m_PredictedPortalTeleportations`, `ApplyPredictedPortalTeleportation`, and the
`EntityPortalled` user message — all of them exist to reconcile a predicting client with
an authoritative server, and this port is one process.

What *is* needed from the tail: `pPortalPlayer->m_hPortalEnvironment` is reassigned to
the **exit** portal before the final `startsolid` fixup, so that the post-teleport trace
runs against the right carved geometry rather than waiting a frame.

---

## 7. Drawing

### 7.1 The model is a quad wearing a depth-only shader

`models/portals/portal1.mdl` and `portal2.mdl`, measured from the VPK:

| | |
|---|---|
| Size | **2,028 bytes** (`.vvd` 320, `.dx90.vtx` 205) |
| Version | 49, flags `0x1` |
| Bones | **1** |
| Sequences | 1 |
| Materials | 1 — `Portal_1_Anims`, under `models\portals\` |
| Vertices | **4** |
| Geometry | a quad, x ∈ ±32, z ∈ ±54, at **y = −1**, normal **−Y** |
| Hull | `(0, −32, −54)` … `(1, 32, 54)` |

This is the smallest model in the game and the easiest possible case for
`world/entities.rs` — one bone, so the per-bone draw split is trivially exact and
skinning is not on the path.

**But its material is `models/portals/portal_1_anims.vmt`, whose shader is `writez`:**

```
writez
{
	$alphamasktexture "models/portals/portal_mask"
}
```

Depth-only. The model is *invisible by design*: it exists to punch a depth hole so that
the recursive view composites correctly. **Placing `portal1.mdl` and drawing it with the
material it names produces nothing on screen**, and that is correct behaviour, not a
bug to chase.

### 7.2 Stage 2 is the frame

The visible portal is `CPortalRenderable_FlatBasic` drawing three materials, all shader
`PortalRefract`, distinguished by a `$Stage` key:

| `$Stage` | Material | What it is |
|---|---|---|
| 0 | `portal_refract_1.vmt` | the see-through refraction |
| 1 | `portal_stencil_hole.vmt` | the stencil punch |
| **2** | **`portalstaticoverlay_1.vmt`** | **the coloured oval — this is the "frame"** |

Stage 2 in full:

```
PortalRefract
{
	$Stage 2
	$PortalOpenAmount "0.0"
	$PortalStatic "0.0"
	$PortalMaskTexture "models/portals/noise-blur-256x256"
	$PortalColorTexture "models/portals/portal-blue-color"
	$PortalColorScale "4.0"
	$time "0.0"
	Proxies { CurrentTime / PortalOpenAmount / PortalStatic }
}
```

`portalstaticoverlay_2.vmt` is the same with `portal-orange-color`. Both colour textures
are **1,669 bytes** — a tiny gradient strip — and the mask is a 256×256 noise blur.

Two ways to draw it, and **the first is what landed** (§10 stage 2):

- **Port `PortalRefract`'s stage-2 branch** — `portal_refract.cpp` (99) +
  `portal_refract_helper.cpp` (284) + the two `.fxc` (422), minus the stage 0/1
  branches. Gets `$PortalOpenAmount`'s open animation for free.
- ~~**Substitute `UnlitGeneric`** with the mask as `$basetexture` and the colour texture
  tinted in. Loses the open animation and the noise, keeps the oval.~~ Declined: the
  depot census counts materials *by shader*, so calling `portalstaticoverlay_1` an
  `UnlitGeneric` would put an untrue number in it.

The three proxies (`CurrentTime`, `PortalOpenAmount`, `PortalStatic`) are the material
*proxy* system, which is entirely unported. `$time` can come from the scene clock;
`$PortalOpenAmount` is the portal's own age since activation, which the entity knows;
`$PortalStatic` is a co-op effect and is `0`.

### 7.3 The blocker is a blended pass — **landed**

An alpha-masked oval over a wall needs **alpha blending**, and until stage 1 this port had
no *ordered* place to put it.

> **What was actually missing, corrected.** The paragraph this section used to open with
> said the port "has never drawn anything blended". That was wrong: `PipelineCache` has
> honoured `BlendMode` since `MATERIALSYSTEM.md` stage 3 and `render_state` has produced
> it from the `.vmt` since then, so the **1,212 of the game's 2,947 drawable materials**
> that blend have always drawn blended. What they had no way to get was an *order* — they
> were recorded in batch order, interleaved with the opaque geometry, so whatever came
> later was composited on top of them. And the *entity's* render mode reached nothing at
> all, which is the part that really was absent.

So §7 was gated on a `materials/` change that is not portal-specific, and it is done:

1. ~~A blend state in `PipelineCache`~~ — already there; what was needed was the
   **second snapshot**, `Material::state_alpha_modulated`, so that
   `SHADER_USING_ALPHA_MODULATION` turns an opaque material's pipeline into a blending
   one when the instance's modulation alpha is not 1
   (`shaderapidx8.cpp:4944`). That is the whole of how a render mode reaches the GPU.
2. A fourth pass in `Engine::render`'s ordering — opaque → frame-buffer copy →
   refracting → **translucent**. `engine::world::GeometryPass` is the three-way decision;
   refracting wins over translucent, and nothing in Portal 2 is both.
3. A back-to-front sort within it: `World::translucent_list` is
   `CClientLeafSystem::SortEntities`' key, `dot( center - eye, forward )`, ascending, and
   `World::draw_translucent` walks it in reverse the way
   `DrawTranslucentRenderables` counts down from the end of its array. The pass is skipped
   when the list is empty.

Measured: `sp_a1_intro1` has **36 translucent draws** — 4 of its 79 world batches, 0 of 31
brush-model batches and 32 of its 1,080 props — and the sort is over whole instances, so
it costs the per-batch instancing the opaque path gets. `sp_a3_00` is the map the depot
test defaults to, because it is the only one with a brush entity that is translucent
because of its *entity* rather than its materials.

### 7.4 What you will actually see

With stage 2 only and no stencil, no refraction and no recursive view: a **coloured oval
on an unbroken wall**, and walking into it teleports you. That is the deliberate output
of this doc's scope and is worth saying out loud before anyone reports it as a bug.

---

## 8. Deleted, with the numbers

Each of these is a decision, not an omission, and each names what would reverse it.

- **The portal gun and all of placement** — `portal_placement.cpp` (1,663),
  `weapon_portalgun`, `UTIL_TestForOrientationVolumes`, the fizzle taxonomy in
  `PlacePortal`. With it go `func_portal_bumper` (2,383 entities),
  `func_noportal_volume` (458), `info_placement_helper` (392), `func_portal_detector`
  (31) and `env_portal_laser` (34) — every one of which exists to constrain or react to
  gun placement. **Reversed by:** wanting to play the game rather than test the
  mechanism. Until then a console command placing and linking a pair covers everything,
  and `NewLocation`'s "skipping placement rules" path (`prop_portal.cpp:799`) is
  precisely that command already written.
- **Everything teleporting except the player.** This port has no `prop_physics`, no
  weighted cubes, no energy balls and no turrets, so the player is the only teleportable
  entity in it. That deletes `physicsshadowclone.cpp` (1,220),
  `physicsclonearea.cpp` (280), `CPortalSimulator`'s entire ownership/cloning tower
  (`TakeOwnershipOfEntity`, `StartCloningEntityAcrossPortals`, ~1,400 lines),
  `CPortal_CollisionEvent`, and `TeleportTouchingEntity`'s 520 lines — the *entity*
  teleport path, as opposed to the player's in `HandlePortalling`. **Reversed by:**
  `prop_weighted_cube`, which is also what `prop_floor_cube_button` is waiting on.
- **The recursive view** — `portalrender.cpp` (2,113) and the stencil/depth-doubler/ghost
  path (`c_portalghostrenderable.cpp`, 980). This is the module's whole visual identity
  and it is explicitly out of scope; it also wants a second camera and render target per
  recursion level, which is `world/`'s 3D skybox work with a harder ordering problem.
- **PVS extension** (`pvs_extender.cpp`, 158, plus `ComputeSubVisibility` and
  `ComputeFrustumThroughPolygon`). The port has no visibility system; every face is drawn
  every frame. **Reversed by:** `world/`'s PVS landing, at which point a portal must
  extend it or the far room vanishes.
- **Sound** — the `CEnvMicrophone`/`CSpeaker` pair a portal creates to carry sound
  through itself, and `Portal.ambient_loop`. No audio system.
- **`linked_portal_door`** (6 entities, 2 maps) — `prop_linked_portal_door.cpp` (982) is
  a *different* class that happens to share the base. Worth a separate look later; it is
  a permanently-linked pair with no gun involved, which makes it arguably a better first
  target than `prop_portal` if the goal is purely to see teleportation work. **It is
  not chosen here** because `sp_a1_intro1` has two `prop_portal`s and no
  `linked_portal_door`, so `prop_portal` is testable on the map the port already loads.
- **Mobile portals** — `sv_allow_mobile_portals` defaults to `0` and is forced back to
  `0` outside `sp_a2_bts5` unless `sv_cheats`. One map. Skip `SetMobileState`,
  `PhysicsSimulate`'s parent tracking and every `bMobile` branch.

---

## 9. Invariants that produce a wrong picture rather than an error

Ordered by how likely each is to bite. These belong in `rustdocs/` when the module
lands.

1. **The teleport matrix has a 180° rotation about up baked into it** (§3.2). Leave it
   out and you exit facing back the way you came, which looks like the exit portal is
   mirrored.
2. **The entity's forward is +X; the model's quad faces −Y.** The collision OBB runs
   `(0, −hw, −hh)` to `(64, hw, hh)`, so forward is +X, but the `.mdl`'s four vertices
   lie in the XZ plane with normal −Y (§7.1). Drawn under the entity's own matrix the
   quad is edge-on and invisible.
3. **`portal1.mdl` is invisible on purpose** (§7.1) — shader `writez`. The frame is a
   separate draw with a separate material.
4. **Linkage is by group and size, not by `PortalTwo`** (§3.1), and `PortalTwo` is
   *overwritten* on link.
5. **The teleport trigger is the centre crossing the plane**, tested against
   `m_plane_Origin` with `< -FLT_EPSILON`, not the hull's near face and not the
   simulator's plane.
6. **The hole is 0.98× the portal's half-size, 0.5 in front and 500 behind** (§4.2). Not
   1.0, not symmetric. Using 1.0 makes the hole exactly the size of the visible portal
   and the player catches on the rim.
7. **`EntityIsInPortalHole` is a `startsolid` test**, not "is the origin inside" — a
   player whose centre is outside the hole but whose hull overlaps it is in the hole.
8. **The velocity gate is directional and relative.** `ShouldTeleportTouchingEntity`
   returns false when `velocity · forward > 0`, and it subtracts the portal's own
   velocity first.
9. **Post-crossing gravity is re-applied at 1.008×** (§6.2). At 1.0 a floor-to-floor
   drop gains height every cycle.
10. **Minimum exit speed is 300 for a player onto a floor portal** (§6.3). At 0 every
    fling in the game dies on the exit.
11. **`FinishDuck()` runs immediately during the forced duck**, and `vOriginToCenter` is
    recomputed from the duck hull afterwards (§6.4) — the *centre* is what the transform
    preserves, not the origin. Conflating them drops the player 18 units.
12. **The player's portal environment is reassigned to the exit portal before the
    post-teleport `startsolid` fixup** (§6.5), not on the next frame's touch update.

---

## 10. Stages

Ordered so that each stage is separately testable and the first is useful on its own.

**Stage 1 — the blended pass. LANDED.** `materials/`: the alpha-modulated state snapshot
(the blend state itself was already there — see §7.3), and a translucent pass after the
refracting one with a back-to-front sort. Not portal work; it unblocked §7 and the
translucent brush entities at once, and `rustdocs/MATERIALS.md` gained the pass-ordering
table. Tested against `rendermode` on shipped brush entities with no portal in sight —
`engine::world::tests::a_shipped_maps_translucent_list_is_sorted_and_holds_its_blended_geometry`,
which defaults to `sp_a3_00`.

> **The count in §0 and in `CLAUDE.md` was wrong and is corrected here.** It is
> **three** brush entities in the 106 shipped maps that set a translucent render mode,
> not five: one `func_brush` in `mp_coop_teambts` at `rendermode 1 renderamt 200` and two
> in `sp_a3_00` at `rendermode 5 renderamt 10`. The other beneficiary is bigger and was
> not counted at all — **30 `prop_dynamic`s** across the game write one, 24
> `kRenderTransTexture` and 6 `kRenderTransColor`, and they are honoured now too because
> `server/` already parsed all three keys.

**Stage 2 — the class, drawn. LANDED.** `src/server/classes/portal.rs` is `CProp_Portal`
and the placement half of `CPortal_Base2D`: `Activated`, `PortalTwo`, `LinkageGroupID`,
`HalfWidth`/`HalfHeight`, the five inputs, the five outputs, the linkage group, the
teleport matrix and `Solid::Obb`. `src/engine/world/portals.rs` is the oval.
`src/materials/` gained `ShaderKind::PortalRefract` — a real port of
`portal_refract_ps2x.fxc`'s `$Stage 2` branch, not an `UnlitGeneric` substitution — and
with it `ContextBinding::PortalOverlay`, the port's first material *instance* parameter.
The `portal` console command places and links a pair.

**Outcome: two coloured ovals on `sp_a1_intro1` that do nothing**, exactly as predicted.

Six decisions worth carrying forward:

1. **`PortalRefract` was ported rather than substituted**, which §7.2 left open. The
   deciding argument is the census: `every_shipped_material_of_a_ported_shader_builds_a_pipeline`
   counts materials *by shader*, and calling `portalstaticoverlay_1` an `UnlitGeneric`
   would put a number in that table that is not true. It cost ~250 lines of WGSL and one
   new group-3 shape. The census now reads **2,952 of 3,555 materials in 58 pipelines**,
   and `PortalRefract`'s five need **one** between them — the fewest of any shader,
   because its render state is a literal.
2. **Only `$Stage 2` resolves.** `ShaderKind::resolve` answers `None` for the stage-0 and
   stage-1 materials, which are the see-through warp and the stencil punch and belong to
   the recursive view. That is the only place in the census where a shader name the port
   knows falls back to the error material, and it is deliberate.
3. **§12's open questions are all answered.** (2) *Where does the carve live* — settled
   by the seam that already exists: `PortalState` goes `server/` → `engine/` → `world/`
   once a rendered frame, keyed by nothing, and stage 3's carve will take the same route
   into `trace/`. (4) *One material or two* — **two**, because they differ only in a
   1,669-byte gradient strip and `MaterialCache` is keyed by name; the instance parameter
   that §12 predicted exists, and it carries the three numbers that genuinely change per
   frame rather than a texture that never changes. (1) and (3) are stage 3's and untouched.
4. **The linkage recursion collapses to one level.** `UpdatePortalLinkage` recurses in
   three places and two of them do no work; the third — a deactivating portal handing its
   partner on — is the only one that can find a third portal, and it is written out.
   That matters because this module cannot recurse: `Server::dispatch` has lifted the
   entity out of the list, so a partner is reachable as data and never as code.
5. **`Context::find_all_of_class` replaces `s_PortalLinkageGroups[256]`.** A `static`
   cannot hold per-`Server` state, the scan is in spawn order either way because
   `AddToLinkageGroup` runs in `Spawn`, and the whole game has 21 portals with no map
   holding more than four.
6. **The placement snap was deleted and the deletion was measured.**
   `CProp_Portal::ActivatePortal` re-traces and re-places a portal on activation; this
   port activates one where the map put it. `every_shipped_portal_is_on_a_wall` runs
   Valve's own trace against all 21 and classifies: **15 flush, 2 proud of their wall
   (`sp_a1_intro1/portal_red_0` by 1.97 units and `sp_a1_intro4/section_2_portal_a1_rm3a`
   by 3.50), and 4 floating** — and those four are exactly the `NewLocation` targets in
   `sp_a4_finale1`/`2`, which the map parks in mid-air and moves from script. **Not one
   of the 21 is more than 0.00 degrees off the surface behind it**, so the snap cannot
   re-orient a shipped portal and the teleport matrix this port computes is the one the
   shipped game computes.

**Stage 3 — the hole.** §4: the AABB brush enumerator (**landed** — `brushes_in_box`,
see §4.5), the carve, the carved store, and `Tracer`'s substitutive path. **Outcome:** you can walk *into* the wall and stand in the
hole, and fall out the back of it, because nothing catches you yet. Testable headlessly:
a trace into a carved wall must miss where the hole is and hit where it is not.

**Stage 4 — the remote trace and the teleport.** §5 and §6 together; neither is testable
without the other. **Outcome:** the module works.

**Stage 5 — polish, if wanted.** The transition ramp, `$PortalOpenAmount`'s open
animation, `IsFloorPortal`'s special cases, `PunchAllPenetratingPlayers`.

---

## 11. Verification

**The default map is the test bed**, which is unusual for this port and worth exploiting.
`sp_a1_intro1` places both portals:

```
portal_blue_0   PortalTwo 0   angles "0 90 0"    origin -1264 4112 2728   Activated 0
portal_red_0    PortalTwo 1   angles "0 180 0"   origin -1137 4352 2762   Activated 0
```

No `LinkageGroupID` on either, so both are group 0 and are each other's only candidate.
Both start off and are switched on by `SetActivatedState`. `portal_red_0` carries the
only `prop_portal` output connection in the game —
`OnPlayerTeleportFromMe → room_1_portal_deactivate_rl, Trigger`.

Tests worth having, in the order they become possible. **Five of the seven are
written**; the two that are not are stage 3's.

- **Unit, no map:** the teleport matrix round-trips — a point through the matrix and back
  through the inverse is itself, and a portal linked to a *copy of itself at the same
  place* transforms a point to its 180°-rotated self.
  **`the_teleport_matrix_turns_a_point_around_the_exit`.**
- **Unit, fixture map:** a carved wall. Build a box brush, carve a hole in it, sweep a
  hull through the middle (must pass) and through the rim (must stop). `trace::fixture`
  already builds brushes from planes, which is exactly the shape a carved piece has.
- **Unit, fixture map:** the four-slab decomposition is complete — for a grid of points
  over the wall's face, "inside exactly one slab" ⟺ "outside the hole rectangle". This
  is the test that catches a sign error in §4.3's distance table, and it needs no BSP.
- **Depot, `--ignored`:** all 21 shipped `prop_portal`s spawn, and all 10 maps produce
  exactly one linked pair in group 0 once every `SetActivatedState 1` in the map has
  been fired. Cheap — it is `Server::level_init` plus dispatch, the shape
  `every_shipped_map_spawns_its_entities` already has.
  **`every_shipped_portal_spawns_and_its_map_can_link_a_pair`, and "exactly one pair" is
  wrong**: `sp_a1_intro2` places *four* portals in group 0 and firing every
  `SetActivatedState 1` in its lump at once — which the running map never does, because
  they belong to different rooms — legitimately forms **two** pairs. What the test asserts
  instead is the invariant: every linked portal's partner links back to it, the two are
  opposite colours, no portal is claimed twice, and every linked portal has a non-identity
  matrix. Measured: **21 portals across 10 maps, 17 switched on by their own logic, 6 maps
  forming a pair.**
- **Depot, `--ignored`:** for each of the 21, the portal's origin is within some small
  distance of a solid surface along `-forward` — i.e. every scripted portal really is on
  a wall, which is what makes the carve meaningful. `NewLocation`'s four targets are the
  interesting exceptions to check.
  **`every_shipped_portal_is_on_a_wall`, and the four exceptions are exactly the four
  predicted.** It runs Valve's own trace (one unit in front to eight behind) and
  classifies: **15 flush, 2 proud of their wall, 4 floating** — the floating four being
  `sp_a4_finale1`/`2`'s tractor-beam portals, which the map parks in mid-air and moves
  from script, and which is also why neither map fires `SetActivatedState`. The assertion
  with teeth is the **angle**: not one of the 21 is more than 0.00 degrees off the surface
  behind it, so the deleted placement snap cannot re-orient a shipped portal.
- **The one that says it works:** put the player in front of `sp_a1_intro1`'s
  `portal_blue_0`, activate both, walk forward for two seconds of ticks, and assert the
  origin is within the exit portal's forward half-space and the view angles have turned
  by the matrix. Headless, no GPU.
- **Rendered, headless:** the oval draws — the same shape as
  `the_button_draws_and_moves_as_it_presses`, comparing a portal-off frame with a
  portal-on one.
  **`the_portal_overlay_draws_in_two_colours_and_opens`, and it needs no map**: what a
  portal draws does not depend on where it is, so it mounts the game for the two materials
  and places two portals by hand. It compares a *settled* oval with a *half-open* one
  rather than on with off, which catches more: a zeroed group-3 block would draw the same
  thing twice. Measured at 256x256: **19,039 pixels for the blue and 18,718 for the
  orange, mean rgb (0.19 14.34 31.66) and (31.17 17.33 0.00)** — the colour assertion is
  what says the 256x1 gradient strip is sampled on its one row — and ~24,700 pixels differ
  between the two ends of the opening animation.

---

## 12. Open questions

1. **Substitutive tracing is new.** `Tracer::with_entities` adds candidates; the portal
   needs the carved pieces to *replace* the world near the portal. §5 reconciles it by
   running both traces and taking the better, which is Valve's own answer and avoids
   teaching `box_trace` to skip brushes — but it costs a second full descent whenever
   the player is near a portal. Measure it before optimising; the environment box is
   small and `engine::world::bench` is the harness.
2. **Where does the carve live? SETTLED IN STAGE 2, and the seam already exists.**
   `crate::server::PortalState` is "a portal exists here, this size, this angle,
   this long open", `Engine::frame` copies it across once a rendered frame, and
   `engine::world::portals::Portal` is the far side. Stage 3's carve takes the same route
   into `trace/`: the server still names no engine collision type, and the engine still
   names no server type. The one thing stage 3 adds to the seam is nothing at all — the
   placement and the size are already there.
3. **Does `linked_portal_door` come first?** It is 6 entities in 2 maps, but it is a
   permanently-linked pair with no gun, no fizzle and no placement — arguably a cleaner
   first target for the teleport machinery. §8 rejects it on testability (`sp_a1_intro1`
   has `prop_portal`s), but if stage 3 proves painful, the two classes share §4–§6
   entirely and the decision is reversible at no cost.
4. **The second colour. ANSWERED: two `Material`s.** `portalstaticoverlay_2.vmt` differs
   from `_1` only in its colour texture — a 1,669-byte gradient strip — and
   `MaterialCache` is keyed by name, so two entries cost two tiny uploads and nothing
   else. The material *instance* parameter this question predicted does exist, and it is
   `uniforms::PortalOverlay` in group 3: it carries `$PortalOpenAmount`, `$PortalStatic`
   and `$time`, which are the three values that genuinely differ between two portals
   wearing one material. The thin end of the proxy system turned out to cost one arena,
   one bind group layout and one `Pass` setter.
