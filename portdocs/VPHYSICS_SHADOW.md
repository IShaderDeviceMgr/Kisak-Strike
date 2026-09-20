# The shadow controller — porting `physics_shadow.cpp` and the player's half of it

**Status: ported.** This was written *before* the port, which is the rule
`CLAUDE.md` sets; §2, §3 and §8 are the plan as it was written, and §6 and §10
were amended afterwards with what the implementation found.
`portdocs/VPHYSICS.md` §9 filed this as the first of its deliberate omissions
and `rustdocs/VPHYSICS.md` §9 called it "a design question rather than an
implementation"; this document is the answer to that question.
`rustdocs/VPHYSICS.md` §4b and §4c are how to *call* what landed.

Everything below is relative to `legacy/`.

---

## 0. Headline decisions

1. **Half of `physics_shadow.cpp` is already ported, under another name.**
   `CShadowController` exists because IVP had no kinematic bodies: a
   game-driven object had to be a *dynamic* body with a velocity controller
   dragging it towards where the game said it was. Rapier has kinematic
   bodies, and `Physics::follow_movers` has been writing poses into them since
   `src/vphysics/` landed. So `CShadowController` is **replaced**, the way
   `physics_environment.cpp` was, and only `CPlayerController` is *ported*.
2. **The player is a dynamic body that is driven, not a kinematic one that
   drives.** This is Valve's choice and it is not an accident of IVP: a
   kinematic player would push a cube with unbounded force and could never be
   stopped by one. `CPlayerController` is a dynamic body whose *velocity* is
   set each tick by `ComputeController`, with a mass limit, a speed limit and
   a contact-normal clamp on what it may do — and a teleport when it falls
   more than 24 units behind. §3.
3. **What actually stops the player is the trace, not the solver.** In the
   shipped engine a cube blocks you because `CEngineTrace::ClipRayToVPhysics`
   (`enginetrace.cpp:1115`) sweeps the ray against the prop's `CPhysCollide`;
   the physics world never gets a vote. So the visible half of this work is a
   new *query*, not a new body: the props enter `trace/`'s clip chain. §2.
4. **The query goes through Rapier, not through a ported `physics_trace.cpp`.**
   `rustdocs/VPHYSICS.md` §9 already named this seam — "a new query belongs on
   `Environment` over `PhysicsWorld::query_pipeline`" — and
   `PhysicsWorld::cast_shape` is a swept box against exactly the hulls
   `collide.rs` already built. `vphysics/trace.cpp` (2,474 lines) and
   `CPhysicsCollision::TraceBox` (`physics_collide.cpp`, 1,992) are deleted
   outright on the same grounds `physics_environment.cpp` was.
5. **A physics prop must touch triggers, and today nothing but the player
   does.** `PhysFrame`'s second phase is a `GetActiveObjects` loop calling
   `VPhysicsUpdate` (`physics.cpp:1797`), and that ends in
   `PhysicsTouchTriggers( &prevOrigin )` (`baseentity_shared.cpp:1376`) —
   which this port left out because a cube that could not move was not going
   to enter anything. §4.
6. **Two deliberate divergences and five oddities in the shipped source.**
   The divergences: the body is weightless, and the teleport that recovers it
   runs on a path Valve's does not. Of the oddities, three are bugs and two of
   those are corrected rather than reproduced. §3.5 and §6.

---

## 1. Inventory

| File | Lines | What happens to it |
|---|---:|---|
| `vphysics/physics_shadow.cpp` | 1,455 | `CPlayerController` **ported** (§3); `CShadowController` **replaced** by Rapier's kinematic bodies (§0.1); save/restore deleted |
| `public/vphysics/player_controller.h` | 55 | The interface; **not** transliterated — see §5 |
| `vphysics/trace.cpp` | 2,474 | **Deleted.** `PhysicsWorld::cast_shape` is the whole of what §2 needs. (The file `physics_trace.h` names is `trace.cpp`, not `physics_trace.cpp` — that one does not exist) |
| `vphysics/physics_friction.cpp` | 198 | `CreateFrictionSnapshot`, which is how Valve enumerates an object's contacts. Replaced by `PhysicsWorld::contact_pairs_with` |
| `engine/enginetrace.cpp`'s `ClipRayToVPhysics` | 85 | The *shape* of §2, ported; the `IConvexInfo` machinery is not |
| `game/server/player.cpp`'s `SetupVPhysicsShadow` (`:8370`) | 40 | Ported as the player's body request (§3.1) |
| `game/shared/baseplayer_shared.cpp`'s `UpdateVPhysicsPosition` (`:3381`) | 41 | Ported as the per-tick drive (§3.3) |

---

## 2. Putting the props in the trace world

### 2.1 What the shipped engine does

`CEngineTrace::ClipRayToVPhysics` is reached for any entity whose `GetSolid()`
is `SOLID_VPHYSICS`, which is every physics prop. It asks the *physics object*
for its `CPhysCollide` and calls `physcollision->TraceBox( ray, fMask, NULL,
pSolid, origin, angles, pTrace )`. There is no BSP, no brush list and no
spatial partition below that line: the sweep is a box against the same ledge
hulls the simulation uses.

That is the whole mechanism, and it is why a cube stops the player even while
the cube is asleep and the solver is doing nothing at all.

### 2.2 What this port does instead

`Environment` grows one query:

```rust
pub fn sweep_box(&self, half: Vec3, start: Vec3, end: Vec3) -> Option<Sweep>
```

over `PhysicsWorld::cast_shape` with a `Cuboid`, filtered to **the bodies this
environment created as props**. The filter is not an optimisation, it is a
correctness requirement: the world, its displacements, its static props and its
brush entities are all in the environment *and* in `trace/` already, and a
sweep that returned them would clip the same geometry twice with two different
answers.

> **The filter had to become a predicate over the environment's own record**,
> which the plan did not foresee. `QueryFilter::only_dynamic()` is the obvious
> spelling and it is wrong: `EnableMotion( false )` freezes a prop by making it
> a **fixed** body (`Environment::enable_motion`), indistinguishable by type
> from a wall, so a type filter lets the player walk through every cube a map
> spawns frozen — and a map can spawn one, `sp_a2_pull_the_rug` does. `Body`
> therefore records whether it was created `Motion::Dynamic` and the predicate
> reads that. `a_frozen_prop_still_stops_a_sweep` is the guard.

`Sweep` reports the fraction, the surface normal, the `BodyId` and
`start_solid`.

> **`stop_at_penetration` must be `false`, and getting it wrong traps the
> player.** With `true` — the obvious reading, and parry's default — a sweep
> that *starts* overlapping reports a time of impact of zero whichever way it
> is going, including straight away from the thing it is inside. A player who
> ends up inside a prop for one tick, which happens whenever a prop is shoved
> into them, then has `fraction == 0` in all six directions and is stuck for
> good: there is no `CheckStuck` in this port to nudge them out, and `noclip`
> is the only way. `false` discards a time-zero impact whose relative velocity
> is *separating*, which is exactly what Valve's brush sweep gets for free —
> `CM_ClipBoxToBrush` tests planes offset by `DIST_EPSILON`, so a box leaving
> a brush it is inside is never stopped by it. Approaching still collides, so
> nothing becomes easier to walk through.
> `a_sweep_can_leave_a_prop_it_starts_inside` and
> `a_player_who_walks_into_the_cube_on_sp_a1_intro1_can_walk_away_again` are
> the guards; the second is the bug report, reproduced.

> **Two more things about the answer are not what the API names suggest**, and
> both cost a test to find. `parry`'s `ShapeCastHit::normal1` is documented as the
> outward normal on the *first* shape — the moving box — which would point the
> way the box is going; Source's `trace_t::plane::normal` points the other way.
> Rapier's query pipeline casts the collider against the shape and flips the
> result, so `normal1` is already Source's convention by the time it arrives
> and `normal2` is the wrong one. And **a zero-length sweep answers `None`**:
> with no velocity there is no time of impact and `stop_at_penetration` has no
> separating velocity to judge, so the position test — which Source asks
> constantly, `CM_UnsweptBoxTrace` and every "am I stuck" check — has to go
> through `intersect_shape` instead.

### 2.3 Who calls it, and which way the dependency points

`trace/` must not name `vphysics`, and `server/` must not name `engine`. Both
constraints are satisfied by the shape `TouchQuery` already established, run in
the other direction:

```text
engine/trace/    defines  trait PropQuery { fn sweep(…) -> Option<PropHit>; }
engine/mod.rs    implements it over the server's Physics
server/physics.rs exposes  Physics::sweep_box  (it already names vphysics)
```

`Tracer::with_props(&dyn PropQuery)` is the third member of a family
`with_entities` and `with_hole` started, and like `with_entities` it is
**additive**: the prop sweep runs after the world sweep and the nearer of the
two wins, which is `CEngineTrace`'s own order.

> **A prop is `CONTENTS_SOLID` here and `studiohdr_t::contents` there.**
> Valve's `CStudioConvexInfo::GetContents` (`enginetrace.cpp:1095`) returns the
> model's own `contents` field, or a per-bone override. This port has never
> parsed either. Every mask that matters — `MASK_PLAYERSOLID`, `MASK_SOLID`,
> `MASK_SHOT_PORTAL` — contains `CONTENTS_SOLID`, so the only observable
> difference would be a physics prop compiled `$contents grate`, and the
> depot census in §7 says the game ships none that this port gives a body to.

---

## 3. `CPlayerController`

### 3.1 The body

`CBasePlayer::SetupVPhysicsShadow` (`player.cpp:8370`) builds it, and every
number in it is content:

| | |
|---|---|
| shape | `PhysCreateBbox( VEC_HULL_MIN, VEC_HULL_MAX )` — and a second one for the duck hull |
| mass | **85 kg** |
| inertia | `1e24` — i.e. it does not rotate |
| `enableCollisions` | **false** at creation; `SetVCollisionState` turns it on |
| `dragCoefficient` | 0 |
| surfaceprop | `"player"` |
| push mass limit | **350 kg** |
| push speed limit | **50 units/s** |

`CPlayerController::AttachObject` then sets `rot_speed_damp_factor` to
`(100,100,100)` and disables drag again. In Rapier the inertia and the damping
are one call — `lock_rotations` — and `EnableDrag(false)` is the default. The
three together are `Motion::Player`.

> **One body, not two.** Valve builds a whole second object for the crouching
> hull and `SetVCollisionState` moves the controller between them. The live
> hull arrives here every tick anyway — it is `PlayerState::mins`/`maxs`, which
> the client already sends because it changes when the player ducks — so the
> body keeps one shape and `set_bounds` rebuilds it on the tick the numbers
> change. That is a cuboid a few times a minute against two bodies for the
> whole level, and it does not have to know how many hulls a player can have.

Gravity is the interesting one. Valve leaves gravity **on** and cancels it
inside the controller when `m_onground`… except that `Update` sets
`m_onground = false;//onground;` (`physics_shadow.cpp:678`), commented out in
the shipped tree. See §6.2.

### 3.2 `ComputeController`, which is the whole algorithm

Two overloads; the player uses the one taking a **per-axis** maximum
(`physics_shadow.cpp:94`):

```c
acceleration = delta * scaleDelta;          // scaleDelta = fraction / dt
acceleration -= currentSpeed * damping;     // damping = m_dampFactor = 1
for each axis i:  clamp |acceleration[i]| to maxSpeed[i]
currentSpeed += acceleration;
```

With `damping == 1` the second line cancels the whole current velocity, so what
remains is "move `delta` in the remaining time, one axis at a time, subject to
a per-axis cap". `delta` is `targetPosition - currentPosition`, and the target
is where the player's *movement code* put the player. The controller never
decides where the player goes; it decides how hard the body may shove to get
there.

### 3.3 The tick

`CBasePlayer::UpdateVPhysicsPosition` (`baseplayer_shared.cpp:3381`) calls
`Update( position, velocity, secondsToArrival, onground, ground )` once per
usercmd. `Update` (`:640`) does four things:

1. early-out if neither the target nor the target velocity moved;
2. store the target and `m_secondsToArrival`;
3. **disable the controller entirely when `velocity.LengthSqr() <= 0.1`** —
   "no input velocity, just go where physics takes you". A standing player
   does not push;
4. otherwise `MaxSpeed( velocity )`, which is §6.3.

Then `do_simulation_controller` (`:486`) runs inside the step: teleport if the
error exceeds 24 units, run `ComputeController`, and clamp the result against
every contact normal (§3.4).

> **The target is not the player's origin, and if it were the shove would not
> work at all.** `CBasePlayer::PostThinkVPhysics` (`baseplayer_shared.cpp:3286`)
> computes what `Update` is given:
>
> ```c
> newPosition = GetAbsOrigin();
> if ( !pPhysGround && m_bTouchedPhysObject
>      && g_pMoveData->m_outStepHeight <= 0.f && (GetFlags() & FL_ONGROUND) )
> {
>     newPosition = m_oldOrigin + frametime * g_pMoveData->m_outWishVel;
>     newPosition = (GetAbsOrigin() * 0.5f) + (newPosition * 0.5f);
> }
> ```
>
> Whenever the move touched a prop on the ground, the shadow is aimed **ahead**
> of the player, at the midpoint between where they are and where the wish
> velocity would have taken them. The loop it breaks closes on itself
> otherwise: the player's own trace is stopped by the cube, so a shadow aimed
> at the player's origin catches up to a target that is not moving, so
> `ComputeController` has no error left to correct, so it pushes with nothing.
> Aiming at where the player *tried* to go keeps a persistent error — about 1.4
> units at a walk — for as long as they keep walking.
>
> Measured on `sp_a1_intro1`, two seconds of walking into the cube from three
> approaches: **1.3, 0.1 and 10.9 units** of shove without the bias, **25.3,
> 15.2 and 22.7** with it. This was missed on the first pass because the depot
> test that checked the shove advanced the player's origin *by hand*, straight
> through the cube, which keeps the error alive artificially; only a player
> whose position comes from the trace runs into it.
>
> **Two of the four conditions are not ported and neither exists to be
> missed.** `!pPhysGround` asks whether the player stands on a *moveable
> physics object*, which needs a ground entity where `MoveData::ground` is a
> plane (§9). `m_outStepHeight <= 0` excludes a frame the player stepped up
> in, because the shadow gets `StepUp` instead on those — and `StepUp` is §9's
> as well.
>
> **The order of the two outputs matters**: `newPosition` uses the *real*
> accumulated `m_outWishVel` (`:3296`) and the velocity handed to the
> controller uses the one substituted by `m_outWishVel.Init( maxSpeed, … )`
> (`:3318`), which happens afterwards.

Here the two clocks are already reconciled — this port's tick and its physics
step are both 1/64 s (`portdocs/VPHYSICS.md` §4.1) — so the update and the
step are adjacent statements in `Server::run_tick` rather than a controller
callback, and `secondsToArrival` is always the tick. That collapses `fraction =
dt / secondsToArrival` to 1 and deletes the whole resample.

### 3.4 `CNormalList`, and why the push has to be clamped at all

Without it the player's body would drive at full impulse into whatever it
touches, including the world, and either shove a 5-tonne door or launch itself.
`do_simulation_controller` walks the contacts and collects the normals of every
one where the push would exceed `limitVel`, with two gates:

- a contact against something immovable, or heavier than
  `m_pushableMassLimit`, sets `limitVel = 0` — you may not push it at all;
- a contact whose normal is steeper than `-0.99` (i.e. the floor) is skipped
  entirely, with the comment `// remove this when clamp works better`.

`ClampVector` then projects: one normal clips the component along it to
`limitVel`, two normals project onto their crease, three or more stop the
motion dead.

**Up to eight normals** (`MAX_LIST_NORMALS`), deduplicated at `dot > 0.99`.

### 3.5 The teleport

`m_maxDeltaPosition` is 24 units. When the body is further than that from the
target, `TryTeleportObject` asks the game's `IPhysicsPlayerControllerEvent::
ShouldMoveTo` — the hook that lets the game refuse to put the shadow somewhere
solid — and beams the body there with collisions disabled for the instant.

> **The teleport runs on both paths here and on one there.** Valve tests it
> after the `m_enable` check, so a disabled shadow is never recovered — and it
> does not need to be, because their body keeps its gravity and the floor holds
> it still. §6.2 takes the gravity away, so a disabled shadow that had been
> shoved would drift for as long as the player stood still, with nothing to
> stop it and nothing to bring it back. A disabled shadow therefore has its
> velocity zeroed and is teleported like any other.
> `a_standing_players_shadow_is_stopped_rather_than_left_coasting` is the
> guard.

This port has no `IPhysicsPlayerControllerEvent` implementation to consult,
and neither does Portal 2. **The only implementation of that interface in the
whole tree is `CPhysicsPlayerCallback` in `cstrike15/cs_player.cpp:285`**,
installed by `CCSPlayer` and by nothing else; `CBasePlayer` and
`CPortal_Player` never call `SetEventHandler`. So `m_handler` is null for the
Portal 2 player, `TryTeleportObject` skips straight past the `ShouldMoveTo`
gate, and the teleport is unconditional — in the shipped game as well as
here.

---

## 4. A physics prop has to touch triggers

`PhysFrame`'s second phase is

```c
for each active object:  pEntity->VPhysicsUpdate( object );   // physics.cpp:1797
```

and `CBaseEntity::VPhysicsUpdate` (`baseentity_shared.cpp:1311`) for a
`MOVETYPE_VPHYSICS` entity is `SetAbsOrigin`/`SetAbsAngles` **then
`PhysicsTouchTriggers( &prevOrigin )`** (`:1376`).
This port does the first two in `Physics::step`'s writeback and not the third,
because until the cube could move there was nothing for it to enter.

`Server::player_touch_triggers` is already the whole of
`PhysicsTouchTriggers` for a solid non-trigger, specialised to one entity. The
work is to generalise it: the player's `origin`, `model_bounds` and
`player_prev_origin` become parameters, and the previous origin for a prop is
the one the writeback just replaced.

> **A prop is `SOLID_VPHYSICS`, and `GetRequiredTriggerFlags` does not care.**
> The branch `player_touch_triggers` takes is `isSolidCheckTriggers`, which is
> `IsSolid() && !IsSolidFlagSet( FSOLID_TRIGGER )` — true of a cube exactly as
> it is of the player. So the same pass serves both and the only per-entity
> input is the swept box.

---

## 5. The interface this port does *not* build

`IPhysicsPlayerController` is 18 virtuals. Under "the Rust interface is the
contract", almost none of them survives:

- `SetObject` exists because the player swaps between a standing and a
  crouching hull. Here that is one body whose collider is replaced, so it is
  `Environment::set_hull`.
- `GetShadowPosition`, `GetShadowVelocity`, `GetLastImpulse`, `GetObject`,
  `WasFrozen`, `GetContactState`, `IsInContact` are seven accessors on state
  the caller already owns or does not use. `IsInContact` is the only one with
  a live consumer — `CBasePlayer::PhysicsSimulate` uses it to decide whether
  the player is standing on something simulated — and it becomes a `bool`
  returned by the update.
- `SetPushMassLimit`/`SetPushSpeedLimit`/`GetPushMassLimit`/
  `GetPushSpeedLimit` are two constants set once at spawn and never changed by
  anything in the Portal 2 tree. They are fields.
- `Jump()` is **`#if 0`'d out in the shipped source** (`physics_shadow.cpp:407`).
  It is not ported.
- `SetEventHandler` has no implementation to point at — §3.5.

What is left is: make one, tell it where the player is, ask whether it is
touching anything simulated, and step up. Four calls.

---

## 6. The bugs, and what happens to each

### 6.1 `MaxSpeed` has one factor of speed too many — **not reproduced**

`CPlayerController::MaxSpeed` (`:699`):

```c
IVP_U_Float_Point available = ivpVel;
float length = ivpVel.real_length_plus_normize();   // |vel|, and ivpVel is now a unit vector
float dot = ivpVel.dot_product( &pCore->speed );    // units of speed
if ( dot > 0 ) {
    ivpVel.mult( dot * length );                    // units of speed SQUARED
    available.subtract( &ivpVel );
}
IVP_Float_PointAbs( m_maxSpeed, available );
```

`real_length_plus_normize` returns the former length (`ivu_linear.cxx:109`
— `f * qlength` where `f = 1/sqrt(qlength)`). The evident intent is
`available = vel - vel̂ (vel̂ · speed)`, "the velocity still to be made up";
the extra `* length` makes the subtracted term a speed² and, at a walking 175
u/s, overshoots by a factor of 4.4 in IVP units. The result is then passed
through `fabsf` per axis, so it never goes negative and the failure is silent:
the cap comes out several times looser than intended whenever the player is
already moving the way they are asking to.

**This port writes the dimensionally consistent form** and records it here.
Unlike `IVP_Compact_Surface::rotation_inertia` (`portdocs/VPHYSICS.md` §3.3),
no shipped content is tuned against this one: it only ever *loosens* a cap that
the contact clamp in §3.4 re-applies, and the cap is on the player's own
shadow rather than on anything a map author placed.

### 6.2 `m_onground` is commented out — **reproduced**

`Update` (`:678`):

```c
	// m_onground makes this object anti-grav
	// UNDONE: Re-evaluate this
	m_onground = false;//onground;
```

so the `if ( m_onground ) pCore->speed.subtract( &gravSpeed )` in
`do_simulation_controller` is dead code in the shipped game. The port does not
write the branch at all, and instead gives the body `gravity_scale = 0`, which
is what the surviving path amounts to: the player's height is decided entirely
by the movement code, and the body is dragged to it every tick.

### 6.3 `CShadowController::MaxSpeed` does not convert units — **not ported**

`m_shadow.maxSpeed = maxSpeed` (`:1272`) assigns a value in **inches per
second** to a field every reader treats as **metres per second**
(`ConvertShadowControllerToIVP` converts the same field, `:928`), so the cap is
39.37× too permissive. It is inside the `#else` of a `#if 0` whose enabled half
was never finished. `CShadowController` is replaced wholesale (§0.1) so nothing
here inherits it; it is recorded because it is the kind of thing a future
session would otherwise "fix" in a file it was reading for another reason.

### 6.4 `CGameMovement::Friction` subtracts a speed where it means a
proportion — **not reproduced**

This one is not in `physics_shadow.cpp` at all; it is upstream, in what feeds
the controller, and it was found by porting `m_outWishVel` (§3.3).
`gamemovement.cpp:1918`:

```c
newspeed = speed - drop;
if (newspeed < 0) newspeed = 0;
if ( newspeed != speed ) {
    newspeed /= speed;                                  // now a proportion
    VectorScale( mv->m_vecVelocity, newspeed, mv->m_vecVelocity );
}
mv->m_outWishVel -= (1.f-newspeed) * mv->m_vecVelocity; // …only if the branch ran
```

The division is **inside** the branch. The only thing that fills `drop` is
ground friction, so on every airborne tick `drop` is zero, `newspeed == speed`,
the branch is skipped and the subtraction runs with `newspeed` still an
absolute speed: at a walking 175 u/s it is `m_outWishVel += 174 * velocity`.

It reaches the controller only when the player is airborne *and* touching a
physics prop, because `PostThinkVPhysics` throws `m_outWishVel` away otherwise
(§3.3) — and there its effect is to leave the push cap effectively unbounded.
Corrected here for §6.1's reason: it only ever loosens a clamp on the player's
own shadow, and nothing a mapper places can be tuned against it.

### 6.5 `CPlayerController::do_simulation_controller`'s gravity press reads
`k[1]` — **an IVP axis, correctly**

`m_lastImpulse.k[1]` and `gravSpeed.k[1]` look like a y-axis bug in Source
terms. They are not: this is IVP space, where **gravity is along −y**
(`portdocs/VPHYSICS.md` §2.4), and the comment two lines up says `// UNDONE:
Assumes gravity points down`. In Source units that is `z`. The port writes `z`.

---

## 7. What the shipped content says

Measured over the depot, `portal2/maps/*.bsp`, 106 maps.

| | |
|---|---:|
| entities of a class this port gives a **dynamic** body | **98** (`prop_weighted_cube`) |
| …across maps | 59 |
| `prop_physics` / `prop_physics_override` / `prop_monster_box` | 132 / 138 / 22 — **none is a ported class** |
| `prop_floor_button` / `prop_under_floor_button` | 65 / 13 |
| cubes spawned within 128 units of a button | **0** |
| nearest button, closest cube in the game | 128–256 units |
| mass, `models/props/metal_box.phy` | **40 kg** |
| mass, `reflection_cube` / `mp_ball` / `personality_sphere` | 40 / 75 / 45 kg |

Three things follow, and the third is the one that changes what this document
promises.

1. **The push mass limit never bites.** 350 kg against a heaviest prop of 75.
   It is implemented because it is cheap and because it is what stops the
   player shoving the *world*, which is the same code path — a static body
   fails `IsMoveable()` and sets `limitVel = 0` before mass is ever consulted.
2. **The push speed limit bites constantly.** 50 units/s against a walk of
   175: the clamp is active for every step the player takes into a cube, and
   it is what makes the cube slide rather than fly.
3. **No cube in the game ships on a button.** Every one comes out of a dropper
   or sits on a shelf, and it is the *player* who carries it to the button —
   which is `CGrabController` (`portal_grabcontroller_shared.cpp`, 3,252
   lines), the portal gun's alternate fire, and neither is ported. So "a cube can hold a
   floor button down" becomes *reachable* with this work and is not
   *demonstrable* from shipped content alone. What is demonstrable is §4's
   general case: a cube that moves now enters and leaves triggers.

What is demonstrable immediately, on the default map: the cube on
`sp_a1_intro1` falls out of its dropper onto the chamber floor, and from this
change on **the player cannot walk through it, can stand on it, and shoves it
along the floor by walking into it**.

---

## 8. Stages

1. **`Environment::sweep_box`** and `Physics::sweep_box` — the query, with unit
   tests against a hand-built environment.
2. **`Tracer::with_props`** and `PropQuery` — the props in the clip chain, wired
   at the one site in `engine/mod.rs` that builds the movement tracer.
3. **The player's body** — `Motion::Player`, the hull, the swap between stand
   and duck.
4. **The drive** — `ComputeController`, the contact clamp, the teleport, run
   from `Server::run_tick` immediately before `step_physics`.
5. **`Physics::touch_triggers`** — §4.

Stages 1–2 are the visible half and are independent of 3–4; stage 5 is
independent of both.

**All five landed**, and the order held. What the plan did not have, and the
implementation needed, is in §2.2, §2.3, §3.1 and §6.4: the sweep filter is a
predicate rather than `only_dynamic`; `stop_at_penetration` has to be `false`
or a player who touches a prop is trapped against it; the normal that comes
back is `normal1` rather than `normal2`; a zero-length sweep needs a different
query; the two hulls became one body; and `m_outWishVel` had to be ported into
`client/movement.rs` before stage 4 had anything to drive with, which brought
a fifth Valve bug with it.

**Two of those were found by playing rather than by testing**, and they were
the two that mattered most: everything above passed, the depot tests passed,
and walking at the cube on `sp_a1_intro1` both pinned the player against it
*and* barely moved it. Both came from the same blind spot — the depot test
that checked the shove advanced the player's origin **by hand**, straight
through the cube, and a player driven that way never gets stuck on anything
and always keeps the controller's error alive.

The test that now guards both,
`a_player_who_walks_into_the_cube_on_sp_a1_intro1_can_walk_away_again`, is the
first here to drive the *movement code* on a real map with the solver running
underneath, so the player's position comes from the trace exactly as it does in
the running game. It walks in from every one of the eight compass points the
chamber leaves open, for two seconds, then walks away for two, and asserts
three things: the cube moved, the player got away, and the player is not
standing inside it at the end. **The lesson is cheap to state and was expensive
to learn: a harness that supplies the answer under test — here, the player's
position — cannot fail the way the game does.**

---

## 10. What it measured

| | before | after |
|---|---|---|
| `cargo test` | 1,135 | **1,160** |
| tests guarding this | — | 24, plus 2 depot |
| `trace/` stages | 4 of 5 | **5 of 5** |

- **The player shoves the real cube on the real map.** Driven by the movement
  code, from the three of eight approaches the chamber leaves open, two seconds
  of walking moves it **25.3, 15.2 and 22.7 units** — against 1.3, 0.1 and 10.9
  before the forward-biased target landed (§3.3). The number is **not
  calibrated against the shipped game**, which this port cannot run; it is a
  regression guard on a mechanism that is known to fail silently.
- **The measurement is the furthest it got, not where it ended**, and that is
  the map rather than the port: the cube comes to rest on the slope it fell
  onto, so a shove eastward is a shove *uphill* and the cube rolls back down
  behind the player as they walk past. Watching where it settles afterwards
  would be a test of the slope.
- **Frame cost.** The prop sweep runs once per player trace and only for a mask
  containing `CONTENTS_SOLID`, so it is not in the draw path at all — but
  `engine::world::bench` is the standing instrument and CLAUDE.md's rule is to
  reach for it either side of anything that could touch a frame. The numbers
  are in `rustdocs/ENGINE.md`, "Frame cost, measured".

---

## 9. What this still does not do

- **`CGrabController`** — picking a cube up (`portal_grabcontroller_shared.cpp`,
  3,252 lines). It is the other half of every cube puzzle in the game and it
  needs the portal gun's use key, a held-object constraint and `+use` tracing.
  §7.3 is why it matters.
- **`CCubeRotationController`** (`prop_weightedcube.h:35`, `prop_weightedcube.cpp:117-230`), the
  `IMotionEvent` that turns a tumbling cube upright as it lands. It is a
  *motion* controller, not a shadow one, and it is the reason a dropped cube in
  the shipped game settles square instead of on a corner — which this port's
  cube visibly does not do. Listed here because it is the nearest neighbour of
  this work, not because it is part of it.
- **Collision events** — a cube still makes no sound and takes no damage from
  landing. `portdocs/VPHYSICS.md` §9.
- **`StepUp`.** `CBasePlayer::PostThinkVPhysics` beams the shadow up by
  `m_outStepHeight` after the movement code steps the player up a stair. With
  the teleport at 24 units and a step height of 18, the shadow recovers on its
  own within one tick; `StepUp` is what makes it recover without the body
  briefly being inside the step. It is a one-line body move and it is listed
  here rather than in §8 because there is no shipped measurement that
  distinguishes the two.
- **The crouch hull's transition.** `SetVCollisionState` swaps the body's
  collider between the stand and duck hulls; what it does *not* do is check
  that the new hull fits, because `CGameMovement::CanUnduck` has already done
  that on the trace side. Both hulls are built; the swap is stage 3.
- **Ragdolls, vehicles, constraints, fluids** — still every one of them a class
  this port has not got.
