# Porting rigid-body physics → `src/vphysics/`

The thing that makes a weighted cube fall. `legacy/vphysics/` (22,607 lines) sitting on
the whole of `legacy/ivp/` — Ipion/Havok's IVP engine — plus the game's own glue in
`legacy/game/server/physics*.cpp` and `legacy/game/shared/physics_shared.cpp` (15,397
lines between them).

Scope of this doc: the collision *format* (`.phy`, `LUMP_PHYSCOLLIDE`), the surface
property database, the environment and its tick, and the seam by which an entity becomes
a rigid body. Read `PORTING.md` first. `portdocs/ENGINE_TRACE.md` §5 already decided the
headline — **Rapier for `vphysics/`** — and this doc is the plan that decision implies.

Status: **written before the port, as `PORTING.md` requires. All five stages landed**,
and everything in §9 did not. `rustdocs/VPHYSICS.md` is the API that resulted.

---

## 0. Headline decisions

1. **Rapier, not IVP.** `portdocs/ENGINE_TRACE.md` §5.3 called this "the single largest
   crate-substitution win left in the port" and nothing found since disagrees. IVP is
   20,618 lines of 1999 C++ with its own allocator, its own `IVP_U_Float_Point`, its own
   fixed-point ledge tree and its own coordinate system, and none of it is knowledge —
   it is encoding.

2. **The format is not optional and is not Rapier's.** `.phy` and `LUMP_PHYSCOLLIDE` are
   IVP *compact surfaces*: a ledge tree whose leaves are convex hulls. Every physics
   engine needs those hulls and no physics engine can read them. §2 is the format; it is
   the one part of IVP that has to be ported rather than replaced, exactly as
   `portdocs/ENGINE_TRACE.md` §5.3 predicted.

3. **The simulation stays in Source units.** Valve converts to metres at every boundary
   (`vphysics/convert.h`, `HL2IVP_FACTOR` = `METERS_PER_INCH`) because IVP is tuned for
   metres and had no other option — its own comment says so: `// UNDONE: Remove all
   conversion/scaling`. Rapier *has* the other option:
   `IntegrationParameters::length_unit` is "how many of your units equal one metre", so
   it is set to **39.3701** and nothing converts. §4.1.

4. **Valve's inertia tensor is wrong, and is reproduced anyway.** `IVP_Compact_Surface`
   stores a per-unit-mass rotational inertia that is not a moment of inertia at all —
   §3.3 derives it and shows it is a factor of √2/2 low for a cube. Letting Rapier
   compute the true tensor from the hull would be *more correct and differently wrong*:
   every Portal 2 cube would tumble 1.41× harder than the shipped game's. The stored
   value is used. This is `PORTING.md`'s "keep the knowledge" applied to a number that
   happens to be a bug.

5. **The environment belongs to `src/server/`, and the collision data to the engine.**
   That is Valve's split too — `physenv` is a game-DLL global and the collision models
   come across the DLL boundary from `modelinfo->GetVCollide`. Here it is the same shape
   the port already uses twice, for `sequences` and for `attachments`: the engine hands
   the server a built object at level init and the server names no engine type. §5.

6. **A class does not create its own rigid body during `Spawn`.** It *asks*, and the
   request is drained when the dispatch returns — the same deferral
   `Context::create_entity` and `Context::take_damage` already use, and for the same
   reason: the dispatched entity is lifted out of the entity list for the whole of its
   own handler. §5.3.

---

## 1. What is in `legacy/vphysics/`, and what happens to each file

| File | Lines | Disposition |
|---|---|---|
| `physics_collide.cpp` | 1,992 | **§2** — the container and ledge walk survive; the IVP builders do not |
| `vcollide_parse.cpp` | 1,040 | **Deleted.** It is a KeyValues reader written from scratch because vphysics could not link `tier1`. One binary has no such excuse — `filesystem::keyvalues` parses the same text. §3.4 |
| `physics_environment.cpp` | 2,335 | **§4** — replaced by `rapier3d::pipeline::PhysicsWorld`; the *settings* survive |
| `physics_object.cpp` | 2,060 | **§3.3, §4.2** — the mass/inertia/damping setup survives; the IVP core does not |
| `physics_material.cpp` | 671 | **§3.5** — the surface property database, ported |
| `convert.cpp` / `convert.h` | 644 | **§4.1** — the *axis swap* survives, the metre scale does not |
| `trace.cpp` | 2,474 | **Deleted.** Hand-written GJK; `parry::query` is the replacement (`ENGINE_TRACE.md` §5.3) |
| `physics_shadow.cpp` | 1,455 | **Not ported.** §9 — the shadow controller is how the player pushes a cube |
| `physics_constraint.cpp` | 1,865 | **Not ported.** §9 |
| `physics_vehicle.cpp`, `physics_airboat.cpp`, `physics_controller_raycast_vehicle.cpp` | 3,586 | **Deleted outright.** Portal 2 has no vehicles |
| `physics_fluid.cpp` | 231 | **Not ported.** §9 — no Portal 2 map this port loads has a `fluid` block that matters |
| `physics_spring.cpp`, `physics_motioncontroller.cpp`, `physics_friction.cpp` | 817 | **Not ported.** §9 |
| `physics_virtualmesh.cpp` | 643 | **Replaced.** §2.5 — displacement collision already exists in `trace/disp.rs` |
| `ledgewriter.cpp`, `linear_solver.cpp`, `vphysics_saverestore.cpp` | 855 | **Deleted.** Tools, a solver Rapier has, and saved games this port has not got |
| `legacy/ivp/**` | — | **Deleted outright** once `src/vphysics/` lands, except as the reference for §2 |

The game-side glue is not in this table because it is `src/server/`'s, not this module's:
`physics_shared.cpp`'s `PhysModelCreate`/`PhysCreateWorld_Shared` become §5, and
`physics.cpp`'s `PhysFrame` becomes §5.4.

---

## 2. The format: `.phy` and `LUMP_PHYSCOLLIDE`

This is the part that must be written by hand. All of it was prototyped in Python against
the shipped depot before a line of Rust was written, and §2.6 is what that proved.

### 2.1 The container

A `.phy` file (`legacy/public/phyfile.h`) is:

```text
i32 size;          // sizeof(phyheader_t) == 16, and doubles as the version
i32 id;            // 0 — studiomdl writes a literal zero (collisionmodel.cpp:2781)
i32 solidCount;
i32 checkSum;      // of the source .mdl
<solidCount solids>
<keydata: NUL-terminated vphysics KeyValues text>
```

Each solid is `i32 size` followed by `size` bytes. `size` is *not* counted in itself.

A BSP's `LUMP_PHYSCOLLIDE` (lump 29) is the same solids under a different header, one
entry per brush model, terminated by `modelIndex == -1`:

```text
i32 modelIndex;    // 0 is worldspawn; the engine's model index is this plus one
i32 dataSize;      // bytes of solids
i32 keydataSize;
i32 solidCount;
```

**`id` is zero, not `'VPHY'`.** The first draft of the reader asserted on a magic number
that studiomdl never writes; the magic lives one level down, on each solid.

### 2.2 The solid

`CPhysCollide::UnserializeFromBuffer` (`physics_collide.cpp:318`) sniffs two layouts:

- a `compactsurfaceheader_t` — `'VPHY'`, `i16 version` (0x0100), `i16 modelType`,
  `i32 surfaceSize`, `Vector dragAxisAreas`, `i32 axisMapSize` — **28 bytes**, followed by
  the compact surface;
- or a bare `IVP_Compact_Surface`, recognised by `dummy[2]` being `'IVPS'`, `'SPVI'` or
  zero.

`modelType` 1 is `COLLIDE_MOPP`, Havok's compressed mesh. **`ENABLE_IVP_MOPP` is `0` in
the shipped tree** (`physics_collide.cpp:169`), so the shipped game itself cannot read
one; this port refuses them the same way.

### 2.3 The compact surface and its ledge tree

`IVP_Compact_Surface` (`legacy/ivp/ivp_surface_manager/ivp_compact_surface.hxx`) is 48
bytes:

```text
f32 mass_center[3];
f32 rotation_inertia[3];
f32 upper_limit_radius;
u32 packed;                 // max_factor_surface_deviation:8, byte_size:24
i32 offset_ledgetree_root;  // relative to the surface
i32 dummy[3];               // dummy[2] is the 'IVPS' tag
```

The ledge tree root is an `IVP_Compact_Ledgetree_Node`, **28 bytes**:

```text
i32 offset_right_node;      // 0 means terminal
i32 offset_compact_ledge;
f32 center[3];
f32 radius;
u8  box_sizes[3];
u8  free_0;
```

`left_son()` is `this + 1` — which is `+28`, and getting that wrong is the second trap:
a model whose root node is terminal (most of them) parses correctly at any node size, so
the error only shows up on the world.

Each terminal node points at an `IVP_Compact_Ledge`, 16 bytes:

```text
i32 c_point_offset;         // relative to the ledge
i32 ledgetree_node_offset | client_data;
u32 packed;                 // has_children:2, is_compact:2, dummy:4, size_div_16:24
i16 n_triangles;
i16 for_future_use;
```

followed immediately by `n_triangles` × `IVP_Compact_Triangle`, also 16 bytes: a packed
word (`tri_index:12, pierce_index:12, material_index:7, is_virtual:1`) and three
`IVP_Compact_Edge`s of four bytes each (`start_point_index:16, opposite_index:15,
is_virtual:1`). Points are `IVP_Compact_Poly_Point` = `IVP_U_Float_Hesse` = four floats,
of which the fourth is not a coordinate.

**Do not use `get_n_points()`.** The header offers
`size_div_16 - n_triangles - 1` and wraps it in `#if defined(LINUX) || …`, which is the
warning. It is a guess about layout, not a field, and it produced a world hull sixteen
units wide. The reader gathers the point *indices the triangles actually name* and reads
those — which is what a convex hull wants anyway.

### 2.4 The coordinate system

Ledge points, the mass centre and the rotational inertia are all in **IVP space**:
metres, with the axes swapped. `ConvertPositionToIVP` (`convert.h:36`) is
`(x, −z, y) × 0.0254`, so the inverse is

```text
hl = (ivp.x, ivp.z, −ivp.y) / 0.0254
```

The determinant of that axis map is +1, so handedness is preserved and triangle winding
carries across unchanged — despite `CreateDebugMesh` reversing it, which is a *rendering*
convention and not a geometric one.

### 2.5 Displacements are not in the lump

Every one of the 106 shipped maps has `virtualterrain {}` in its worldspawn keydata and
an **empty** `LUMP_PHYSDISP`. Valve builds displacement collision at load
(`PhysCreateVirtualTerrain` → `modelinfo->GetCollideForVirtualTerrain`) out of
`physics_virtualmesh.cpp`'s streaming mesh.

This port does not need any of that: `Bsp::disp_grid` already exists, shared by
`trace/disp.rs` and `world/disp/`, precisely so that the drawn surface and the solid one
cannot diverge. A displacement becomes a Rapier `TriMesh` over the same grid. 643 lines
deleted for about fifteen.

### 2.6 What the prototype proved

Written in Python against the depot before the Rust, and each of these is now a test:

- **1,056 of Portal 2's 2,041 models ship a `.phy` (51.7%), and all 1,056 parse.**
- Solid counts are 1 for 1,005 of them; the rest are jointed models, up to 23.
- **4,643 model ledges, of which 0 mix more than one material.**
- **All 106 maps carry lump 29**, 11,720 brush models between them, the worst
  `mp_coop_start` at 408.
- **115,225 world ledges, of which 73,856 (64%) mix more than one material** — which is
  what §3.5 has to give up on, and the reason it is not a loss.
- The decisive check: parsing `sp_a1_intro1`'s worldspawn solid and taking the bounds of
  every point gives `x[−9275, 6080] y[416, 11264] z[−1920, 5248]`, which is
  `dmodel[0]`'s mins and maxs **to the float**. The format and the coordinate conversion
  are both right, and neither is guessed.

---

## 3. What the numbers mean

### 3.1 Mass

From the `solid { }` block's `"mass"` key, in kilograms. Both cubes are 40 kg.
`VPHYSICS_MIN_MASS`/`VPHYSICS_MAX_MASS` clamp it (`physics_object.cpp:1480`).

### 3.2 The mass centre

`IVP_Compact_Surface::mass_center`, converted by §2.4. Rapier takes it as
`MassProperties::local_com`.

### 3.3 The inertia, which is where the bodies are buried

`IVP_Real_Object::init_object_core` (`ivp_object.cxx:845`) does:

```text
rot_inertia = surface->rotation_inertia * template->rot_inertia * mass
```

with `rot_inertia_is_factor` always true from Source
(`physics_object.cpp:1504`) and `template->rot_inertia` the scalar `"inertia"` key
splatted across all three axes. Then `auto_check_rot_inertia` — `rotInertiaLimit`, 0.05 —
raises any axis below 5% of the vector's length.

So `rotation_inertia` is a per-unit-mass principal moment, in m². Except it is not.
`IVP_Rot_Inertia_Solver` (`ivp_rot_inertia_solver.cxx:150`) integrates the second moment
about each axis — `⟨x²⟩`, `⟨y²⟩`, `⟨z²⟩` — and then writes

```c
a *= a; b *= b; c *= c;
sa = sqrt(b + c);   // and cyclically
```

which is `sqrt(⟨y²⟩² + ⟨z²⟩²)`, not `⟨y²⟩ + ⟨z²⟩`. For a cube of side *a* the two differ
by exactly √2/2: the true `I/m` is `a²/6` and IVP stores `(a²/12)·√2`.

Checked against the shipped file: `models/props/metal_box.phy` is a 35.64-unit cube
(0.9053 m), so `a²/12 = 0.06830` and `0.06830·√2 = 0.09659`. The file stores
**0.09632**. Valve's cubes rotate about **1.41× too easily**, have done since 2004, and
every hand-tuned throw, drop and funnel in Portal 2 is tuned against it.

**Reproduced deliberately.** A note at the site and in `rustdocs/VPHYSICS.md` says what
the true tensor would be and which constant reverses it.

The stored value is in m²; with §0.3's units it is multiplied by 39.3701² = 1550.0031 to
become kg·in².

### 3.4 The keydata

`vcollide_parse.cpp` is deleted and `filesystem::keyvalues` reads the text instead.
The blocks that matter:

- `solid { index, name, mass, surfaceprop, damping, rotdamping, drag, inertia, volume }`
- `staticsolid { index, contents }` — the world's, one per solid
- `materialtable { … }` — §3.5
- `virtualterrain {}` — §2.5
- `editparams { }` — the model editor's; ignored, as Valve ignores it at run time
- `fluid { }`, `ragdollconstraint { }` — §9

`PhysModelParseSolidByIndex` takes the **first** `solid` block when no index is asked for
and asserts that its index is 0, which is what this port does.

### 3.4b Which world solids are solid

`PhysCreateWorld_Shared` creates a static object for **every** `staticsolid`
block and then calls `SetContents( solid.contents )`. That reads like
bookkeeping and is not: `CCollisionEvent::ShouldCollide` (`physics.cpp:487`)
ends with

```c
if ( !(pObj0->GetContents() & pEntity1->PhysicsSolidMaskForEntity()) ||
     !(pObj1->GetContents() & pEntity0->PhysicsSolidMaskForEntity()) )
    return 0;
```

and `CBaseEntity::PhysicsSolidMaskForEntity` returns `MASK_SOLID`
(`physics_main_shared.cpp:1129`). So the rule is: **a world solid is in a
physics prop's way iff its contents intersect `MASK_SOLID`**, and because
nothing in this port overrides that mask, the filter can be applied once at
build rather than per contact.

Measured over the 106 shipped maps' 384 `staticsolid` blocks:

| contents | count | in a cube's way? |
|---|---|---|
| `SOLID\|WINDOW\|GRATE\|…` (`0x2003003`) | 106 | **yes** |
| `GRATE` (`0x8`) | 83 | **yes** |
| `PLAYERCLIP` (`0x10000`) | 103 | no |
| `MONSTERCLIP` (`0x20000`) | 92 | no |
| `WATER\|TRANSLUCENT` (`0x10000020`) | 76 | no |

The grate row is the one worth stating out loud, because it is the opposite of
what "bullets pass through it" suggests — `bspflags.h:29`'s own comment is
"Bullets/sight pass through, but solids don't". Getting it backwards would drop
83 solids' worth of floor out of the game.

The last row is also exactly the 76 solids a `fluid { }` block names (44 maps
have one), and they would be excluded twice over: `CreateFluidController` turns
that object into an **IVP phantom** (`physics_fluid.cpp:192`), a volume that
reports what is inside it and collides with nothing. In the shipped game a
prop falls into water and is pushed back out by buoyancy; here it falls in and
keeps going, which is the closer of the two answers available without buoyancy.

### 3.5 Surface properties

`scripts/surfaceproperties_manifest.txt` names three files;
`CPhysicsSurfaceProps::ParseSurfaceData` (`physics_material.cpp:400`) reads them in
order. Two rules are easy to miss and both are load-bearing:

- a block whose name is **already defined** overwrites that definition rather than adding
  a second, and starts from the existing one;
- a block whose name is new starts from **`"default"`**, before its own `base` key is
  applied. So `default_silent`, which declares nothing but a `gamematerial`, is
  friction 0.8 and elasticity 0.25 — not zero.

Only two of the numbers reach the simulation: **friction** and **elasticity**, and IVP
**multiplies** both across the contacting pair (`ivp_material.cxx:13`), which is
`CoefficientCombineRule::Multiply`. `density`, `dampening` and `thickness` are for
buoyancy, impact sounds and `func_breakable`, none of which is here.

What this port gives up, and why it costs nothing measurable: IVP resolves the surface
property **per triangle** through `material_index` and the map's `materialtable`, and a
Rapier collider has one material. §2.6 measured 73,856 of 115,225 world ledges mixing
materials, so there is no per-ledge answer to pick. The world therefore gets `"default"`,
which is exactly what `PhysCreateWorld_Shared` passes to `CreatePolyObjectStatic` anyway.
The divergence is bounded by the table itself — every material Portal 2's maps name
(`default`, `concrete`, `wood`, `default_silent`, `metal`, `glass`, `plaster`, `tile`) has
**friction 0.8 except `glass`, which has 0.5**. Elasticity varies from 0.01 to 0.3 and is
the half that is actually lost.

---

## 4. The environment

### 4.1 Units

| | Valve | here |
|---|---|---|
| length | metres (`× 0.0254` at every boundary) | **Source units**, `length_unit = 39.3701` |
| mass | kilograms | kilograms |
| gravity | `ConvertPositionToIVP(0,0,−600)` = −15.24 m/s² | `−600` u/s² |
| timestep | `1/64` s, fixed | `1/64` s, fixed |

Gravity is `sv_gravity`, which is **600 for Portal 2** — already in the port as
`client::movement::SV_GRAVITY`. `physics.cpp:265` pins the physics timestep at 1/64
regardless of the game's tick rate, with a comment explaining that smaller steps made
guns bounce; this port's tick is already 64 Hz, so the two coincide and the accumulator
in `ServerClock` is the only one needed.

### 4.2 The body

| `objectparams_t` / `solid_t` | Rapier |
|---|---|
| `mass`, mass centre, inertia | `MassProperties` on the collider (§3.1-3.3) |
| `damping` | `RigidBodyBuilder::linear_damping` |
| `rotdamping` | `RigidBodyBuilder::angular_damping` |
| `surfaceprop` | `ColliderBuilder::friction` / `restitution` + `Multiply` |
| `enableCollisions` | `ColliderBuilder::enabled` |
| `drag` | not ported — `physics_environment.cpp`'s air drag model |
| `volume` | informational; Rapier computes its own |

The hulls of one solid become **one collider per ledge**, all parented to the same body.
A ledge is a convex brush-shaped piece and Rapier's broad phase would rather have several
small AABBs than one that covers the model.

### 4.3 Static, kinematic, dynamic

Valve has three entry points and they map cleanly:

- `VPhysicsInitStatic` → `RigidBodyBuilder::fixed`. The world, static props, and any
  brush entity that does not move.
- `VPhysicsInitShadow` → `RigidBodyBuilder::kinematic_position_based`. A door is a
  *shadow object*: `SetShadow(1e4, 1e4, false, false)` then `UpdateShadow(origin,
  angles)` every tick, which is precisely a kinematic body whose pose the game writes.
  §9 records what the rest of the shadow controller is for.
- `VPhysicsInitNormal` → `RigidBodyBuilder::dynamic`. Physics props. The cube.

### 4.4 What goes in at level init

1. **The world** — lump 29's model 0, every solid, fixed. `staticsolid`'s `contents`
   decides whether it collides at all.
2. **Displacements** — §2.5, one `TriMesh` each, fixed.
3. **Static props** whose `solid` is `SOLID_VPHYSICS` (6), from their model's `.phy`
   solid 0, placed by the prop's origin and angles, fixed.
4. **Brush entities** — lump 29's models 1…n, placed from the *entity*, and **kinematic
   whether they move or not**: every brush class this port implements reaches
   `VPhysicsInitShadow` directly, and `CFuncBrush::CreateVPhysics` says why —
   *"Don't init this static. It's pretty common for these to be constrained and
   dynamically parented."*
5. **Entities that place a studio model** — `CDynamicProp::CreateVPhysics`
   (`props.cpp:2119`), which ends in `VPhysicsInitStatic()`. **This is the one that was
   almost missed**, and it is 8,072 `prop_dynamic`s across 105 maps: the panels, hatches
   and machinery a chamber is built out of. Without it a cube falls through the
   furniture and lands on the level shell.

   The rule for *these* is `VPhysicsInitStatic`'s own and **does not read the
   movetype**: parented → shadow, otherwise static. That distinction matters because
   `CBaseProp::Spawn` (`props.cpp:253`) sets `MOVETYPE_PUSH` on every prop in the game,
   exactly as `CFuncBrush::Spawn` does on every brush entity — so a rule written off the
   movetype would make all 8,072 of them kinematic bodies rewritten every tick. An
   *animating* prop is still static: `prop_dynamic` animates its bones and its origin
   does not move, and Valve's answer for collision that follows an animation is bone
   followers (§9).

---

## 5. The seam

### 5.1 Who owns what

```text
engine/world/   reads lump 29, the disp grids and the props' .phy → vphysics::Collision
engine/        builds vphysics::Environment from that, hands it to the server
server/        owns the Environment, steps it, writes poses back onto entities
server/classes  ask for a body through Context; never name a vphysics type but the request
```

This is the third time the port has used this shape — `Server::set_sequences` and
`Server::set_attachments` are the first two — and the reason is the same each time: the
server must not name `studio`, `world` or `filesystem`.

### 5.2 `Collision`: what the engine hands over

One trait, implemented in `engine/mod.rs`, answering the two questions `PhysModelCreate`
asks `modelinfo` for:

- the vcollide of a **studio model**, by name — `GetVCollide(modelIndex)` for a
  `mod_studio`;
- the vcollide of a **brush model**, by `*N` — the same call for a `mod_brush`.

The join key for the second is the `"*N"` model index, which `Server::brush_models`
already established (`portdocs/SERVER.md` §7.4) and which is unique across all 106 maps.

### 5.3 Asking for a body

`CPhysicsProp::CreateVPhysics` → `VPhysicsInitNormal( SOLID_VPHYSICS, 0, asleep,
&tmpSolid )` happens inside `Spawn`. It cannot happen inside `Spawn` here, because
`Server::dispatch` has lifted the spawning entity out of the entity list and the
environment needs to record which entity a body belongs to.

So `Context` grows a queue, exactly like `created`, `damage` and `punches`:

```rust
cx.vphysics_init_normal(entity, VPhysicsInit { asleep, mass_scale, ... });
cx.vphysics_destroy(entity);
cx.vphysics_enable_motion(entity, false);
```

drained by `Server::flush_physics` the moment the handler returns. Everything a caller
does between the request and the flush — set the origin, the angles, the model — happens
before the body is built either way, which is the order that matters.

### 5.4 The tick

`CPhysicsHook::FrameUpdatePostEntityThink` (`physics.cpp:405`) calls `PhysFrame(
TICK_INTERVAL )`, and `PhysFrame` (`:1731`) does three things:

1. `physenv->Simulate( deltaTime )`;
2. for every object in `GetActiveObjects`, `pEntity->VPhysicsUpdate( object )`, which for
   `MOVETYPE_VPHYSICS` is `SetAbsOrigin`/`SetAbsAngles` and then
   `PhysicsTouchTriggers( &prevOrigin )`;
3. the shadow entities' `VPhysicsShadowUpdate`, which is §9.

In `Server::run_tick` that lands **after `run_think_functions` and before
`check_for_entity_untouch`**, so a cube that moves this tick is in its new place when the
touch pass runs and an `OnEndTouch` still arrives in the tick it happened.

`VPhysicsUpdate`'s two guards are ported because they are cheap and they are what stops a
solver blow-up becoming a NaN in the entity list: `IsEntityPositionReasonable` and
`IsEntityQAngleReasonable`.

### 5.5 What the cube gets

`prop_weighted_cube`'s `spawn` already sets `Solid::VPhysics` and `MoveType::None`.
`CPhysicsProp::Spawn` calls `CreateVPhysics()`, which is the only thing standing between
the class as it is and a cube that falls, so:

- `spawn` asks for a normal body, mass and surface property from the model's own `.phy`;
- `MoveType::VPhysics` arrives as a movetype so that the writeback has something to test;
- `EnableMotion`, which `rustdocs/SERVER.md` deliberately left unhandled because there
  was nothing for it to do, becomes real;
- `Dissolve`/`SilentDissolve` destroy the body along with the entity.

---

## 6. Stages

1. **`collide.rs`** — the container, the ledge tree, the hulls. Tested against all 1,056
   shipped `.phy` files and all 106 maps' lump 29.
2. **`surfaceprops.rs`** — the manifest, the three files, `base`, friction and elasticity.
3. **`env.rs`** — Rapier: units, gravity, the timestep, fixed/kinematic/dynamic bodies,
   the active list.
4. **The seam** — `Collision`, `Context`'s queue, `Server::flush_physics`,
   `Server::step_physics`.
5. **The cube** — `CPhysicsProp::CreateVPhysics`, and the depot test that drops it.

---

## 7. How this is verified

- **The format**, against the depot: every `.phy` parses, every map's lump 29 parses, and
  the world hull's bounds equal `dmodel[0]`'s.
- **The units**, against a closed form: a body released in a vacuum falls `½gt²`, and
  `metal_box.phy`'s hull has the volume its own `"volume"` key claims.
- **The inertia**, against the derivation in §3.3 rather than against Rapier.
- **The cube**, against the shipped map: `sp_a1_intro1` places one at
  `(−496, 4112, 2949.33)` with a tilt of a third of a degree, and a working simulation
  drops it, settles it, and leaves it level.

---

## 8. What would change these decisions

- **If the player has to push a cube**, §9's shadow controller stops being optional and
  `physics_shadow.cpp` comes back.
- **If `length_unit` proves not to be enough** — if contacts jitter at 39.37 units per
  metre where they would not at 1 — then §0.3 reverses and the port adopts Valve's
  boundary conversion after all. The switch is one constant and the `.phy` reader stops
  dividing.
- **If a map's load time becomes a problem**, the world's ~1,500 convex hulls are the
  first thing to look at, and the answer is Rapier's own serialization rather than a
  faster parser.

---

## 9. Deliberately not ported

Each of these is a real feature of `legacy/vphysics/` and each is left out for a reason
rather than for lack of time.

- **The shadow controller** (`physics_shadow.cpp`, 1,455) and **the player controller**
  (`vphysics/player_controller.h`). This is how a player pushes a cube and how a cube
  stops a player, and it needs `client/`'s movement and `trace/` to agree with the
  environment about where everything is. A door is a *kinematic* body here, which is the
  half of the shadow controller a moving brush actually uses.
- **Collision events** — impact sounds, `CCollisionEvent`, impact damage
  (`physics_impact_damage.cpp`, 750). There is no sound, and no cube in Portal 2 is
  damaged by falling.
- **Constraints** (1,865), **springs** (286), **motion controllers** (333), **fluids**
  (231), **ragdolls** (`physics_prop_ragdoll.cpp`, 1,716). Every one of them is a class
  this port has not got.
- **Bone followers** (`physics_bone_follower.cpp`, 487). Valve's answer for collision
  that follows an *animation*: a separate solid entity per collision joint, with the prop
  itself going `FSOLID_NOT_SOLID`. Without them a jointed model has no usable static
  answer — solid 0 of one is a ragdoll's pelvis in its bind pose — so
  `Physics::add_studio_entities` refuses jointed models outright. **51 of the game's
  1,056 collision models are jointed**, up to 23 solids.
- **`phys_timescale`, `phys_speeds`, `phys_debug_check_contacts`** and the rest of
  `physics.cpp`'s console surface.
- **Save/restore** (`vphysics_saverestore.cpp`, 222) — there are no saved games.
- **`trace/` integration.** The cube is in the physics world and not in the trace world,
  so the player walks through it. That is not an oversight, it is the same seam the
  shadow controller sits in.
