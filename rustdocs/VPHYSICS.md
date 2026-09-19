# `src/vphysics/` — rigid-body physics

Valve's collision format, the surface property database, and a physics
environment on [Rapier]. `portdocs/VPHYSICS.md` is the port plan and the
evidence; this is how to use what landed.

| | |
|---|---|
| Replaces | `legacy/vphysics/` (22,607 lines) and all of `legacy/ivp/` |
| Depends on | `rapier3d` 0.35, `filesystem::keyvalues`, `math` |
| Consumers | `engine/world/physics.rs` (fills it), `server/physics.rs` (owns it) |
| State | **the cube falls.** No shadow controller, no constraints, no collision events — §7 |

[Rapier]: https://rapier.rs

```rust,ignore
use crate::vphysics::{collide::VCollide, env::{Environment, Hulls, Mass, Motion}};

let phy = VCollide::read_phy("models/props/metal_box.phy", &bytes)?;
let params = phy.solid_params();                 // mass 40, surfaceprop "Metal_Box"
let solid = &phy.solids[params.index];

let mut env = Environment::new(surfaces);        // Portal 2 gravity, 64 Hz, Source units
let hulls = Hulls::from_solid(solid);            // one convex piece per ledge
let cube = env.add(
    Motion::Dynamic, &hulls,
    Vec3::new(-496.0, 4112.0, 2949.33),          // where sp_a1_intro1 puts it
    Vec3::new(-0.34, 90.49, -0.91),              // QAngle: pitch, yaw, roll
    &params.surface_prop,
    Some(Mass::from_solid(solid, &params)),
).unwrap();

env.step();                                      // one 1/64 s tick
for (body, origin, angles) in env.active() { /* VPhysicsUpdate */ }
```

---

## 1. The three modules

| Module | Is | Answers |
|---|---|---|
| [`collide`](../src/vphysics/collide.rs) | Valve's format: `.phy` and `LUMP_PHYSCOLLIDE` | "what shape is this model?" |
| [`surfaceprops`](../src/vphysics/surfaceprops.rs) | `scripts/surfaceproperties*.txt` | "how slippery is it?" |
| [`env`](../src/vphysics/env.rs) | Rapier | "where does it end up?" |

`vphysics::Model` ties the three together — hulls, the `solid { }` block, and
the mass properties — and is what a consumer usually holds.

## 2. Units: **Source units, not metres**

This is the first thing to know and the thing most likely to catch a reader who
has read Valve's code.

| | Valve | here |
|---|---|---|
| length | metres (`HL2IVP_FACTOR` = `METERS_PER_INCH` at every boundary) | **Source units** |
| gravity | −15.24 m/s² | **−600 u/s²** |
| mass | kilograms | kilograms |
| timestep | 1/64 s | 1/64 s |

`IntegrationParameters::length_unit` is set to `39.3701`, which is what makes
Rapier's internal tolerances — contact prediction, allowed penetration, sleep
thresholds — mean the same thing they would at metre scale. Nothing in the port
multiplies by `METERS_PER_INCH` except [`collide::ivp_to_source`], which has to
touch every point anyway because IVP also swaps the axes.

`a_body_in_a_vacuum_falls_at_sv_gravity` is the guard: half a second is 75
units, checked against `½gt²` rather than against Rapier.

## 3. Reading a collision model

```rust,ignore
VCollide::read_phy(what: &str, bytes: &[u8]) -> Result<VCollide, CollideError>
VCollide::read_lump(what: &str, bytes: &[u8]) -> Result<Vec<(usize, VCollide)>, CollideError>
```

Both produce the same thing; `.phy` is one model and the lump is one per brush
model, keyed by the `N` of `"*N"`. **Worldspawn is index 0.**

```rust,ignore
pub struct VCollide { pub solids: Vec<Solid>, pub keys: Block, pub checksum: i32 }
pub struct Solid    { pub mass_center: Vec3, pub rotation_inertia: Vec3,
                      pub radius: f32, pub ledges: Vec<Ledge> }
pub struct Ledge    { pub points: Vec<Vec3>, pub triangles: Vec<[u16; 3]>,
                      pub material: u8, pub mixed_materials: bool }
```

Convenience readers over `keys`, each named after the `PhysCreateWorld_Shared`
call it replaces: `solid_params()`, `static_solids()`, `virtual_terrain()`,
`material_table()`.

Everything is in **Source units in the model's own frame**, already through the
axis swap. A `Ledge`'s points are deduplicated and its triangles re-indexed onto
them, because a ledge's point array is shared with its siblings in the file.

## 4. Building a body

```rust,ignore
Hulls::from_solid(&Solid) -> Hulls                       // one collider per ledge
Hulls::from_mesh(points, indices) -> Hulls               // a displacement
Mass::from_solid(&Solid, &SolidParams) -> Mass
Environment::add(Motion, &Hulls, origin, angles, surface, Option<Mass>) -> Option<BodyId>
```

`Motion` is Valve's three entry points: `Static` is `VPhysicsInitStatic`,
`Kinematic` is `VPhysicsInitShadow` (a door), `Dynamic` is
`VPhysicsInitNormal` (a cube). Only `Dynamic` takes a `Mass`.

`Hulls` is cheap to clone — the shapes are reference-counted — so a model placed
fifty times is hulled once.

## 5. Invariants and gotchas, most likely to bite first

1. **`IVP_Compact_Surface::rotation_inertia` is not a moment of inertia, and
   this port reproduces the error deliberately.** `IVP_Rot_Inertia_Solver`
   writes `sqrt(⟨y²⟩² + ⟨z²⟩²)` where the moment about x is `⟨y²⟩ + ⟨z²⟩`; for
   a cube the two differ by exactly √2/2, so **every Portal 2 cube rotates
   1.41× more easily than a real one**. `Mass::true_inertia` computes the other
   answer and nothing calls it. Reversing the decision is one call site.
   `valves_stored_inertia_is_a_factor_of_root_two_below_the_true_one` checks the
   derivation against the closed form.

2. **A `QAngle` is pitch, yaw, roll**, and `Environment::pose` gives one back.
   It is not guaranteed to be the same triple that went in — `matrix_angles`
   picks its own representative — so compare *rotations*, not components.

3. **`BodyId` is generational.** A handle to a removed body resolves to
   `None`, never to whatever took its slot. That matters because an entity can
   be removed by an input in the same tick the environment is stepped.

4. **`Environment::active()` reports only dynamic bodies**, which is
   `GetActiveObjects`'s purpose: the world is not in it, a sleeping cube is not
   in it, and a kinematic door is not in it (the game already knows where that
   is).

5. **`set_kinematic_pose` is the only way to move a body, and that is
   deliberate.** Rapier derives a kinematic body's velocity for the step from
   the gap between its current pose and its *next* one; a hard `set_position`
   leaves that velocity at zero, which is the difference between a door that
   shoves what it meets and one that teleports through it. A `set_pose` that
   did the hard thing existed for a while and was deleted rather than left as
   a trap — if a caller ever genuinely needs to teleport a body, it should
   arrive with the reason written down.

6. **A world solid is in a cube's way iff its contents intersect
   `MASK_SOLID`.** `PhysCreateWorld_Shared` creates a body for *every*
   `staticsolid` and then calls `SetContents`, which makes the contents look
   like bookkeeping — they are not. `CCollisionEvent::ShouldCollide`
   (`physics.cpp:487`) ends with
   `if ( !(pObj0->GetContents() & pEntity1->PhysicsSolidMaskForEntity()) ) return 0;`,
   and `CBaseEntity::PhysicsSolidMaskForEntity` is `MASK_SOLID`. So a **grate
   stops a cube** (`CONTENTS_GRATE` is in `MASK_SOLID` — "bullets and sight
   pass through, but solids don't") and a **playerclip does not**. Measured
   over the shipped maps' 384 `staticsolid` blocks: 106 level shells and 83
   grates are in the way; 103 playerclips, 92 monsterclips and 76 water
   volumes are not. `world::physics::add_world` applies the mask once at build
   rather than per contact, which is exact because nothing in this port
   overrides that mask — `CBasePlayer` and `CAI_BaseNPC` do, and neither is in
   the environment.

7. **The world gets one surface property, `"default"`.** IVP resolves the
   material **per triangle** through `IVP_Compact_Triangle::material_index` and
   the map's `materialtable`; a Rapier collider has one material and
   **73,856 of the game's 115,225 world ledges mix more than one**, so there is
   no per-ledge answer to pick either. The cost is bounded: every material a
   shipped `materialtable` names has friction 0.8 except `glass` (0.5).
   Elasticity, which ranges 0.01–0.3, is the half that is genuinely lost.

8. **Friction and elasticity multiply and are then clamped — but the clamp is
   on the factors here and on the product in IVP.** The two agree wherever both
   factors are in range, which is every pair Portal 2 can form except those
   involving one of **five** superelastic surfaces (`energyball` and
   `metal_bouncy` at 1000, three more between 1.2 and 3).

9. **`get_n_points()` is not used and must not be.** The IVP header offers
   `size_div_16 - n_triangles - 1` behind an `#if defined(LINUX)`, which is a
   guess about layout rather than a field; reading a *world* ledge that way
   produced a map sixteen units wide. The reader gathers the indices the
   triangles name.

10. **A ledgetree node is 28 bytes.** `left_son()` is `this + 1`. A model whose
   root node is terminal — most models — parses correctly whatever size is
   guessed, so the error only shows up on the world.

11. **`phyheader_t::id` is zero**, not `'VPHY'`. `studiomdl` writes a literal
    zero; the magic is one level down and even there it is optional.

## 6. How a cube gets one, end to end

```text
World::load            physics::build(map, bsp, props, vfs, surfaces)
                         ├─ LUMP_PHYSCOLLIDE model 0        → static bodies
                         ├─ every displacement's grid       → static TriMesh
                         ├─ static props that are SOLID_VPHYSICS
                         └─ every .phy the props name       → models table
Scene::load            physics.add_models(entity model names, vfs)
                       Server::set_physics(env, models, brush models)
                         ├─ every solid brush entity        → kinematic
                         ├─ (the queued requests, below)    → dynamic
                         └─ every solid studio entity       → static, or
                                                              kinematic if parented
WeightedCube::spawn    cx.vphysics_init_normal(id, asleep)  → queued
Server::flush_physics  Physics::init_normal                 → a dynamic body
Server::run_tick       Physics::follow_movers → step → writeback
```

**The order of those three is load-bearing**, and the middle one is why: a
cube's dynamic body has to exist before the studio pass runs, or the studio
pass would hand the cube a *static* body which `init_normal` would then throw
away. Same answer, twice the work, and one more place for the two to disagree.

The deferral in the middle is not decoration: `Server::dispatch` lifts the
spawning entity out of the entity list for the whole of its own handler, so a
body — which has to be recorded *against* an entity in that list — cannot be
built there. It is the same queue `Context::create_entity` and
`Context::take_damage` already use. A request made before the environment
exists is **kept**, which is what makes a `Spawn` during `level_init` work at
all.

## 6b. What `sp_a1_intro1` ends up with

`sp_a1_intro1_drops_its_cube_through_the_whole_server_path` builds the same
environment the running game builds and prints it:

| | |
|---|---|
| static bodies | **667** — 2 world solids, 11 displacements, 654 static props |
| brush entity bodies | **26**, all kinematic (§5, `CFuncBrush`) |
| studio entity bodies | **49**, of which 2 are kinematic because they ride a parent |
| studio models refused as jointed | **4** |
| dynamic bodies | **1** — the cube |

and then drops the cube: **254.82 units in 136 ticks**, to `z` 2694.51, where it
sleeps. It does not rest level, and that is the map rather than the solver —
the floor there is 6.85° off vertical and the cube's own up ends up 1.30° from
the floor's normal. `the_shipped_cube_hull_lands_flat_on_a_flat_floor` is the
control: the same hull, the same drop height, onto a slab, rests at 0.000°.

Two world solids and not four, because `MASK_SOLID` refuses the map's
playerclip and monsterclip (gotcha 6).

## 7. Deliberately not implemented

`portdocs/VPHYSICS.md` §9 has the reasons; this is the list, ordered by what
would be noticed first.

- **The shadow controller and the player controller** (`physics_shadow.cpp`,
  1,455 lines). This is why **the player walks through a cube** rather than
  pushing it, and why a cube cannot hold a floor button down. A *door* is a
  kinematic body here, which is the half of the shadow controller a moving
  brush actually uses.
- **Collision events** — impact sounds, impact damage, `CCollisionEvent`.
- **Constraints, springs, motion controllers, fluids, ragdolls, vehicles.**
- **Bone followers**, which is collision that follows an animation. A model whose
  `.phy` carries more than one solid (51 of the game's 1,056) therefore gets no
  body at all rather than its first bone frozen in the bind pose.
- **`trace/` integration.** The cube is in the physics world and not in the
  trace world. Same seam as the first item.
- **Per-triangle world materials** — gotcha 7.
- **Save/restore**, and `phys_timescale`/`phys_speeds`.

## 8. Which test guards what

| Behaviour | Test |
|---|---|
| A world solid's contents decide whether it stops a prop | `world::physics::add_world`, asserted by `the_cube_on_sp_a1_intro1_falls_and_comes_to_rest` *(depot)* |
| The `.phy` byte layout, all of it | `a_box_round_trips_through_the_compact_surface_layout` |
| Both container layouts agree | `the_bare_ivps_layout_reads_the_same_as_the_vphy_one` |
| The IVP→Source axis map, including its handedness | `ivp_space_is_metres_with_y_and_z_exchanged_and_one_of_them_negated` |
| Points come from the triangles, not from a size guess | `a_ledges_points_come_from_the_indices_its_triangles_name` |
| No malformed file panics | `a_truncated_file_is_an_error_rather_than_a_panic`, `a_ledge_tree_that_points_at_itself_is_refused_rather_than_looped_on` |
| MOPP is refused the way the shipped game refuses it | `a_mopp_is_refused_the_way_the_shipped_game_refuses_it` |
| Lump 29 is the same solids | `a_lump_29_entry_is_the_same_solids_under_a_different_header` |
| `base` and the two inheritance rules | `base_copies_the_whole_of_what_it_names`, `a_block_that_declares_nothing_physical_inherits_default` |
| A later file amends rather than shadows | `a_later_file_amends_an_earlier_definition_rather_than_shadowing_it` |
| The units | `a_body_in_a_vacuum_falls_at_sv_gravity`, `a_heavy_body_and_a_light_one_fall_together` |
| A cube lands and sleeps | `a_cube_dropped_on_a_floor_comes_to_rest_on_top_of_it` |
| Valve's inertia bug, reproduced | `valves_stored_inertia_is_a_factor_of_root_two_below_the_true_one` |
| The 5% inertia floor | `a_degenerate_inertia_axis_is_raised_to_five_percent_of_the_vector` |
| Angles survive the round trip | `a_source_qangle_round_trips_through_the_pose` |
| A stale handle stays dead | `a_removed_body_leaves_a_handle_that_resolves_to_nothing` |
| `EnableMotion` | `a_frozen_body_stays_where_it_is_and_a_thawed_one_falls` |
| Only dynamic bodies are active | `only_dynamic_bodies_are_reported_as_active` |
| **Every shipped `.phy`** | `every_shipped_collision_model_parses` *(depot)* |
| **Every shipped map's lump 29** | `every_shipped_map_carries_a_world_collision_model` *(depot)* |
| The surface database, and the five superelastic surfaces | `the_surface_property_database_resolves_the_cubes_chain` *(depot)* |
| **The real cube on the real map** | `the_cube_on_sp_a1_intro1_falls_and_comes_to_rest` *(depot)* |

## 9. Extending it

- **A new kind of body** goes through `Environment::add` with a new `Motion`
  variant, not through a second `add_*`.
- **A new query** (a ray cast for the trace module, say) belongs on
  `Environment` over `PhysicsWorld::query_pipeline`; do not hand a
  `RigidBodyHandle` out — `BodyId` is generational and the handle is not.
- **The shadow controller** is the one piece that would change shapes rather
  than add to them: it needs `client/`'s movement, `trace/` and the environment
  to agree about where the player is, which is a design question rather than an
  implementation.
