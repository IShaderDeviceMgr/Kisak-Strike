# `CGrabController` — picking the cube up

> **Written before the port**, per `CLAUDE.md`. Subject is the C++ in
> `legacy/`; the Rust that comes out of it is documented in
> `rustdocs/VPHYSICS.md` §9 and `rustdocs/SERVER.md`.

`portdocs/VPHYSICS_SHADOW.md` ended with the honest note that a cube could be
shoved and not carried, and that **no shipped map places a cube on the button it
belongs on** — so 78 floor buttons meant nothing. This is the other half.

## §0 The four decisions, up front

1. **Only the *physics* grab is ported.** `portal_grabcontroller_shared.cpp` is
   3,252 lines and roughly a third of it is the **VM grab** — a clone of the
   held object drawn in the *view model*. In single player
   `CPortal_Player::UpdateVMGrab` (`portal_player.cpp:3835`) sets
   `m_bUseVMGrab = false` for everything except `npc_personality_core`, and
   the cvar that would override it (`player_held_object_use_view_model`)
   defaults to `-1`, meaning "ask the object". **A weighted cube in single
   player takes the physics path**, which is the path this port needs and the
   only one it can draw.

2. **It is the shadow controller's math again, with the angular half turned
   on.** `CGrabController::Simulate` (`:1012`) calls
   `IPhysicsObject::ComputeShadowControl`, which is
   `ComputeShadowControllerIVP` (`physics_shadow.cpp:826`) — the same function
   the player's shadow uses. `src/vphysics/shadow.rs` already has its linear
   half. What is new is rotation, and one trap (§2.1).

3. **The held object is made to weigh one kilogram.**
   `REDUCED_CARRY_MASS` (`:278`) is `1.0f`, and `AttachEntity` writes it over
   a 40 kg cube's mass. This is not a tweak — it is the whole reason a held
   object cannot be used to fling the player, and it has to be restored on
   detach from a saved copy.

4. **Carrying through a portal is deferred, and the blocker is not the grab
   controller.** Nothing but the *player* teleports here:
   `handle_portalling` lives in `client/movement.rs` and takes a `MoveData`.
   Until a physics prop can cross a portal at all, the portal half of
   `UpdateObject` has nothing to stand on. §9.

## §1 What the 3,252 lines are

| Lines | What | Ported? |
|---|---|---|
| `:609` `AttachEntity` | grab: save mass/damping, build the controller | **yes** |
| `:879` `DetachEntity` | release: restore mass, clamp velocity, refuse if stuck | **yes** |
| `:1012` `Simulate` | the per-step shadow control | **yes** |
| `:1506` `UpdateObject` | where the object should be, from the eye | **yes**, minus the portal branch |
| `:376` `ComputeError` | how far behind it is; drives the drop | **yes**, minus the portal branch |
| `:325` `SetTargetPosition` | writes the target, wakes the body | **yes** |
| `:530` `ComputeMaxSpeed` | heavy-object speed limit | **no** — collapses to a constant, §6.3 |
| `:1215` `CPlayerPickupController` | the `+use` state machine | **yes** |
| `:87` `AlignAngles`, `:149` `ComputePlayerMatrix` | carry-angle helpers | **yes** |
| `:2130` `TestIntersectionVsHeldObjectCollide` | refuse a drop inside the player | **yes**, `SOLID_BBOX` arm only |
| `:201` `RotateObject` | `+attack2` turns the held object | **no** — §9 |
| `:2595`–`:2938` `…VM`, `C_PlayerHeldObjectClone` | the view-model clone | **no** — §0.1 |
| `:2328` `FindSafePlacementLocation` | where to drop it (270 lines) | **no** — called only from `DetachEntityVM` |
| `:2237` `CheckPortalOscillation`, portal branches | held across a portal | **no** — §9 |
| `:2938` `PushNearbyTurrets`, `:2964` `ShowDenyPlacement` | turrets, UI | **no** — no turret class, no UI |

`player_pickup.cpp` (182 lines) is the `IPlayerPickupVPhysics` notification
interface — `Pickup_OnPhysGunPickup` and friends. It is three virtual calls and
a default implementation; the port collapses it into two methods on the prop
class, because the only implementor that matters here is the cube.

## §2 The physics grab, as an algorithm

### §2.1 The shadow control, and the overload trap

`ComputeShadowControllerIVP` does two `ComputeController` calls per step:

```
fraction     = secondsToArrival > 0 ? min(dt / secondsToArrival, 1) : 1
scaleDelta   = fraction / dt
linear : ComputeController(speed,     targetPos - pos,  maxSpeed,    maxDampSpeed,    scaleDelta, damp)
angular: ComputeController(rot_speed, axis * angle,     maxAngular,  maxDampAngular,  scaleDelta, damp)
```

where `axis`/`angle` come from `QuaternionAxisAngle(QuaternionDiff(targetRot, currentRot))`.

> **There are two `ComputeController`s and they are not interchangeable.**
> `physics_shadow.cpp:46` takes **scalar** `maxSpeed`/`maxDampSpeed` and clamps
> the acceleration by *vector magnitude*, damping as a separate clamped term.
> `physics_shadow.cpp:94` takes a **per-axis** `IVP_U_Float_Point` maxSpeed and
> folds damping into the acceleration. The shadow control calls the **scalar**
> one; `CPlayerController::ComputeController` calls the per-axis one.
> `src/vphysics/shadow.rs::compute_controller` is the per-axis one, and
> **reusing it here would be wrong** — it would clamp a diagonal carry to
> `maxSpeed` on each axis, i.e. to `√3` times the intended speed, and would
> lose `maxDampSpeed` entirely. The grab needs its own.

The grab's parameters, from the constructor (`:281`) and `ComputeMaxSpeed`:

| | value |
|---|---|
| `dampFactor` | `1.0` |
| `teleportDistance` | **`0`** — so the teleport branch never runs |
| `maxSpeed` | `1000`, `+ m_fPlayerSpeed` each step (`:1021`) |
| `maxDampSpeed` | `maxSpeed * 2` |
| `maxAngular` | `DEFAULT_MAX_ANGULAR` = `360 * 10` = **3600** |
| `maxDampAngular` | `maxAngular` |

`Simulate` then scales the angular limit by `m_contactAmount` **cubed**, where
`m_contactAmount` approaches 0.1 when the object is touching something heavier
than itself and 1.0 when it is not, at `deltaTime * 2` per step. That is the
"stop spinning while jammed" fix, and the cube is the reason it exists.

### §2.2 Where the object is told to go — `UpdateObject`

With the portal branch removed, the non-obvious parts in order:

1. **Pitch is clamped to ±75°** before the carry direction is taken
   (`m_bAllowObjectOverhead` is false for the player pickup, so it is
   `clamp(pitch, -75, 75)`), which is why looking straight down does not put
   the cube in the floor.
2. `radius = playerRadius + BoundingRadius()`, and
   `distance = player_held_object_distance (15) + radius`.
3. **The "column"** (`player_hold_object_in_column`, default `1`) is the
   distinctive Portal 2 carry feel and is easy to miss. Rather than holding the
   object `distance` along the *look* vector, it intersects the look ray with a
   **vertical plane** standing `distance` in front of the player, and clamps
   the result to `[15, player_hold_column_max_size (96)]`. The object therefore
   keeps a fixed *horizontal* stand-off and rides up and down as you look up
   and down, instead of swinging towards your feet.
4. **A `MASK_SOLID_BRUSHONLY` ray** from the eye to that point shortens the
   carry when a wall is in the way; the result is clamped up to `radius`.
5. **A down-trace bumps it off the floor**: a `radius/2` box dropped
   `radius/2 + 1` from the carry point, raising it by `radius * (1 - fraction)`.
6. `flUpOffset = RemapValClamped(|pitch|, 0, 75, 1, 0) * GetObjectOffset()`,
   and `GetObjectOffset` is `player_held_object_offset_up_cube` = **-10** for a
   `prop_weighted_cube` and 0 for everything else. The cube hangs ten units low
   when you look level and comes up to centre as you look down.
7. The object's *orientation* is `m_attachedAnglesPlayerSpace` transformed back
   out of player space, so it keeps the relative orientation it was grabbed at
   as you turn. `SetAngleAlignment(0.866025403784)` — `cos 30°` — snaps that to
   30° steps through `AlignAngles` (`:87`), and `SetIgnorePitch(true)` drops the
   player's pitch out of the transform.
8. The target written is `end - offset`, where `offset` is the object's
   *centre* offset rotated into world space: `end` is where the centre goes and
   an entity's origin is not its centre.

### §2.3 Attach and detach

`AttachEntity`: save mass and rotational damping per object, set mass to
`REDUCED_CARRY_MASS` and rotational damping to `10`, `EnableDrag(false)`, wake
it, set `FVPHYSICS_PLAYER_HELD`, record `m_attachedPositionObjectSpace` and
`m_attachedAnglesPlayerSpace`, set the collision group to
`COLLISION_GROUP_PLAYER_HELD` (`const.h:410`) and start `m_errorTime` at
**`-1.0`**, i.e. one second of grace before error accumulates at all.

`DetachEntity`: **refuses** — returns false, the hold continues — if dropping
would leave the object intersecting the player
(`TestIntersectionVsHeldObjectCollide`, §1). Otherwise it restores the
collision group, mass and damping, clears the flag, and clamps the outgoing
velocity to `MaxSpeed() * 1.5` linear and `720°/s` angular **relative to the
player's own velocity** (`ClampPhysicsVelocity`, `:860`), which is what stops a
drop from a running start turning into a throw.

## §3 The `+use` key

`CPortal_Player::PlayerUse` (`portal_player.cpp:2722`) → `PollForUseEntity`
(`portal_player_shared.cpp:1087`) → `FindUseEntity` (`:1155`).

The shipped game prefers an entity the **client** picked and sent up in the
usercmd (`ucmd->player_held_entity`); `PollForUseEntity` is the server-side
fallback, and since this port has no netcode and runs both halves in one
process, the fallback *is* the path.

`FindUseEntity`, minus the portal tail:

1. A ray from the eye, **1024 units**, against `MASK_SOLID | CONTENTS_DEBRIS`.
2. If that missed anything useable, **ten hull traces** at fixed tangents —
   45°, 30°, 20°, 15°, 10° down, then 10°, 15°, 20°, 30°, 45° **up** — each a
   32-unit box out to 72 units. The comment says the upward half is "useful in
   portal when flying past a use target quickly".
3. The hit counts only within **`PLAYER_USE_RADIUS`**, which
   `baseplayer_shared.h:16` defines as **100** under `PORTAL2` and 80
   otherwise.
4. Failing that, a radius search over entities within 100 units, taking the one
   most nearly in front (`sv_player_use_cone_size`, `0.6`), rejecting anything
   occluded.

Then `CPortal_Player::PickupObject` (`portal_player_shared.cpp:1022`) gates on
`CBasePlayer::CanPickupObject` (`baseplayer_shared.cpp:2829`) with
`PORTAL_PLAYER_MAX_LIFT_MASS` **85** and `PORTAL_PLAYER_MAX_LIFT_SIZE`
**128** (`portal_player_shared.h:19`): move type must be `MOVETYPE_VPHYSICS`,
mass ≤ 85, every OBB dimension ≤ 128, and **"can't pick up what you're standing
on"**.

Per tick while held, `CPlayerPickupController::UsePickupController` (`:1356`)
drops the object if `ComputeError()` exceeds **40** (the value when
`player_held_object_collide_with_player` is 0, which is its default), and
otherwise calls `UpdateObject(player, 12)`.

## §4 What `src/vphysics/env.rs` does not have yet

The grab needs six things the environment has never been asked for:

| Need | Why |
|---|---|
| `angular_velocity` / `set_angular_velocity` | the angular half of the shadow control |
| `rotation` as a quaternion | `QuaternionDiff` needs the current orientation, and `pose()` returns Euler `QAngle` |
| `set_mass` | `REDUCED_CARRY_MASS`, and restoring it |
| `set_angular_damping` | `AttachEntity` sets it to 10 |
| an overlap test against **one** body | `TestIntersectionVsHeldObjectCollide`'s drop refusal |
| a collision filter | `COLLISION_GROUP_PLAYER_HELD` — the held cube must not collide with the player's own shadow |

The last is the one with teeth. The player's shadow is a *dynamic* body
(`portdocs/VPHYSICS_SHADOW.md` §3); a 1 kg cube held 15 units in front of an
85 kg driven body would be in permanent contact with it, and the contact would
fight the controller every step. Valve's answer is a collision *group*; Rapier's
is `InteractionGroups` on the collider, which is the same idea with the table
inverted.

## §5 The interface

Three pieces, in the dependency direction the rest of the port already uses
(`vphysics/` knows bodies, `server/` knows entities, neither names the other's
vocabulary):

> **This was the plan; `rustdocs/VPHYSICS.md` §4d and `rustdocs/SERVER.md` are
> what shipped.** Two names moved — `carry_target` became `hold_placement`, and
> `error` needs `&mut self` and the environment because it consumes its own
> accumulated time (§6.1).

```rust
// src/vphysics/grab.rs  — the controller, over a BodyId
pub struct GrabController { /* … */ }
impl GrabController {
    pub fn attach(env: &mut Environment, body: BodyId) -> GrabController;
    pub fn detach(self, env: &mut Environment, player_velocity: Vec3, max_speed: f32);
    pub fn drive(&mut self, env: &mut Environment, target: Vec3, rotation: Quat,
                 player_speed: f32, dt: f32);
    pub fn error(&mut self, env: &Environment) -> f32;
    pub fn body(&self) -> BodyId;
}

// src/server/grab.rs — CPlayerPickupController: which entity, and where to hold it
pub struct Carry { /* entity, carry angles, centre offset, radius, floor bump */ }
pub fn hold_placement(&Hold, floor_bump: &mut f32, &mut dyn TouchQuery) -> (Vec3, Quat);
```

`server/physics.rs` grows `grab`/`release`/`drive_grab` beside the existing
`drive_player`, for the same reason `drive_player` is there: `Physics` owns the
`Environment` and the body-to-entity map, and nothing else may.

## §6 The bugs, and which are reproduced

**§6.1 `ComputeError` is called twice a tick and the second call always
returns 0.** `UsePickupController` calls it (threshold 40), then calls
`UpdateObject`, which calls it again (threshold 12). But `ComputeError` ends
with `m_errorTime = 0`, and begins with `if ( m_errorTime <= 0 ) return 0`. So
**the 12 is dead** — the object is only ever dropped at 40. The port
implements the single reachable threshold and records the number that is
unreachable, rather than reproducing a double call whose second result is
structurally zero.

**§6.2 `flLastDelta` is a function-level `static`.** `UpdateObject:1806`
declares `static float flLastDelta = 0.0f` inside the function, so the
floor-bump distance is **shared by every grab controller in the process** and
persists across pickups. With one player it is merely a value that leaks from
the previous frame into a frame whose own trace started solid; with two it is a
bug. The port keeps it per-controller, which is what the code means.

**§6.3 `ComputeMaxSpeed` is dead for every carryable object in the game.** It
returns early unless mass exceeds `physcannon_maxmass` (**250**), and the
heaviest thing `CanPickupObject` will accept is **85**. It is ported as the
constant it collapses to, with the arithmetic recorded here rather than in code
that can never run.

**§6.4 The double `SetMass` in `AttachEntity` looks like a bug and is not
one.** The loop gives every object `REDUCED_CARRY_MASS / flFactor`, where
`flFactor = max(count / 7.5, 1)`; then `:805` immediately sets the *primary*
object's mass to the undivided `REDUCED_CARRY_MASS`, leaving it heavier than
its siblings. The comment above it says so in as many words — *"Give extra mass
to the phys object we're actually picking up"* — so it is deliberate, and a
future reader tidying the "redundant" second `SetMass` would change behaviour
for jointed objects. For a cube, `count == 1` and `flFactor == 1`, so both
lines write the same 1 kg and the distinction never arises.

**§6.5 `m_bIgnoreRelativePitch` is set and never read in the physics path.**
`SetIgnorePitch(true)` writes it; only the VM path branches on it. Not ported.

## §7 What ships, and what this makes reachable

Counted from the depot's entity lumps (`scratchpad/grab1.py`, 106 maps):

- **390 carryable physics props** — 132 `prop_physics`, **98
  `prop_weighted_cube`**, 22 `prop_monster_box`, and the rest. Only
  `prop_weighted_cube` has a class here, so 98 is the reachable set.
- **78 floor buttons**, and **21 `npc_personality_core`** — the one class that
  would take the VM path, and one this port does not have.
- **128 `npc_portal_turret_floor`**, which `UpdateVMGrab` forces onto the
  physics path. No turret class here either.

**The distance census is the point of the whole exercise.** Of the 268 carryable props that
have a button anywhere on their map, **none is within 128 units of one** and
only 3 are within 256 — and all 3 of those are a `prop_monster_box` or a
`prop_physics`. The nearest **weighted cube** to a button in the whole game is
**258.5 units** away, on `sp_a3_crazy_box`. That is the measurement that made
`portdocs/VPHYSICS_SHADOW.md` §7 say a cube on a button was reachable and not
demonstrable: 258 units is far past what shoving moves one.

**And the default map settles it.** `sp_a1_intro1` has one cube and one floor
button, **437.4 units apart**. Carrying is the only way a cube covers 437
units, so this is the change that turns 78 floor buttons from scenery into
mechanism — and it can be demonstrated on the map the port already boots into.

## §7b What had to be fixed underneath, and none of it was the grab controller

Three gaps only became reachable once a cube could be carried, and all three
were in code that predates it:

1. **A studio prop was a *point* to every box test in `server/`.**
   `EntityCore::model_bounds` is filled for brush models and for the boxes the
   game makes by hand, and never for a `.mdl` — so a cube's touch box was
   `(0,0,0)`–`(0,0,0)` at its origin. The floor button's trigger is 14 units
   tall and a resting cube's origin is 22 above the pad, so the point sat above
   the box and no press could ever have happened. `VPhysicsInitNormal` now
   takes the bounds from the `.phy`, which is where `SOLID_VPHYSICS` gets them
   in the original (`CBaseEntity::SetSolid` → `CollisionProp()->SetSolid`). It
   also stops every prop being `is_point_sized` to the pusher.
2. **`CBaseTrigger::PassesTriggerFilters` was missing its
   `SF_TRIGGER_ALLOW_PHYSICS` arm** (`triggers.cpp:367`) — the one a cube
   takes, which is a *movetype* test (`MOVETYPE_VPHYSICS`) rather than a class
   test. **841 shipped triggers set that bit** — 325 `trigger_multiple`s, 250
   `trigger_portal_cleanser`s, 115 `trigger_catapult`s, 73 `trigger_once`s, 64
   `trigger_push`es and 14 others — and **481 of them allow no clients at
   all**, so they are physics-only and noticed nothing here at all.
3. **`CPortalButtonTrigger::PassesTriggerFilters` had only its player arm**
   (`prop_floor_button.cpp:518`). Valve's second arm accepts
   `prop_weighted_cube` and `prop_monster_box`, gated on `AcceptsBall()` and
   `OnlyAcceptBall()` — both permissive for `prop_floor_button`, the only one
   of the four sibling classes this port has, so every cube type passes.

## §8 Stages

1. **The environment's six missing pieces** (§4), each with a unit test.
2. **`vphysics/grab.rs`** — the scalar `ComputeController`, the angular delta,
   `attach`/`detach`/`drive`. Unit-testable with a synthetic body: a cube
   commanded to a point arrives at it and stays.
3. **`server/grab.rs`** — `carry_target`'s geometry (§2.2), `CanPickupObject`'s
   gates, `FindUseEntity`'s ray and its ten tangents.
4. **The `+use` wiring** — `IN_USE`'s press edge in `player_pre_think`, the
   per-tick `UsePickupController`, the drop.
5. **The depot test**: on `sp_a1_intro1`, walk to the cube, pick it up, carry it
   437 units to the floor button, and assert the button fires `OnPressed`.

## §9 What it will still not do

- **Carry a cube through a portal.** Not a grab-controller gap: no physics prop
  teleports here at all (`handle_portalling` is `client/movement.rs` and takes
  a `MoveData`). The portal branches of `UpdateObject`, `ComputeError`,
  `AttachEntity` and `CheckPortalOscillation` are ~300 lines that all assume
  the object can already be on the far side. **Fix prop teleport first**; the
  grab's portal half is then a follow-on, and §2.2's target transform is the
  whole of it.
- **Draw the held object in the view model** (§0.1) — 21 personality cores, a
  class this port does not have, and no view model to draw into.
- **Pick up a turret, a monster box or a `prop_physics`** — the classes do not
  exist; `prop_weighted_cube` is the only one with a body.
- **`FindSafePlacementLocation`'s three-pass drop search** — VM-only (§1).
- **`FindUseEntity`'s third pass**, the radius search over nearby entities
  (`portal_player_shared.cpp:1226`). It exists for `FCAP_USE_IN_RADIUS`
  entities — levers and buttons made of clip brushes, which have no surface for
  a ray to land on — and every class here that can be *carried* is a physics
  prop the eleven rays already reach. **Add it when a class wants `+use` without
  a clear line of sight**; the seam is `Server::find_use_entity`, which would
  grow a fallback after the ray loop.
- **`RotateObject`** (`:201`), turning a held object with `+attack2`. It is
  gated on `sv_enableholdrotation`, which **defaults to `0`**
  (`portal_player_shared.cpp:162`), so it is off in the shipped game; it also
  needs the raw mouse deltas (`ucmd->mousedx`/`mousedy`), which this port's
  `CUserCmd` does not carry. **Add it when the usercmd grows mouse deltas**,
  which nothing else wants yet.
- **The `+use` deny sound, the prong animation, the HUD hide** — no sound, no
  weapon, no HUD.

## §10 What it measured

### §10.0 What it does on the default map

`the_player_carries_the_cube_to_the_floor_button_on_sp_a1_intro1`, which drives
`client/`'s movement code over the real map with the solver running underneath
it, prints:

```
cube rests 345.3 units from the button
picked the cube up from 63.8 units away
held at 74.6 units from the eye
closest the cube came to the pad: 10.1, carried for 109 ticks
standing still, the cube hangs 13.3 from the pad
dropped 13.4 from the pad
player walked 160 units off the pad
the floor button is held down by the cube, with nobody standing on it
```

Three of those numbers are worth keeping:

- **345.3 units**, the distance the cube has to travel, against the **18.8**
  the shadow controller could shove it. That gap is the whole reason this
  module exists.
- **74.6 units from the eye**, which is `player_held_object_distance` (15) plus
  the player's flattened half-hull (22.6) plus the cube's bounding radius
  (38.1) — §2.2's arithmetic, arriving where it should.
- **109 ticks**, 1.7 seconds of carrying, with the hold never breaking:
  `ComputeError` stayed under 40 for the whole walk.

**And the release drops rather than throws.** The first attempt let go
mid-stride and the cube landed **48.4 units** past the pad, because
`ClampPhysicsVelocity` (`:860`) clamps the outgoing velocity *relative to the
player* — so a cube let go at a run keeps the run. Standing still first puts it
down **13.4 units** from the pad instead. That is Valve's behaviour and not a
defect; the test now stops before it lets go, which is what a player does.

### §10.1 The test that passed for the wrong reason

**The first end-to-end depot test passed, and it was a false positive** — worth
recording because it is the same failure mode `portdocs/VPHYSICS_SHADOW.md`
ended on, wearing different clothes.

It walked the player at the floor button until the *player* was within 24 units
of it, dropped the cube, and asserted the pad was down. It was: the pad was
down because **the player was standing on it**. The cube had landed
**102 units away**, because a held object hangs about seventy-five units in
front of the eye, so steering the *player* onto a pad puts the cube well past
it.

Two changes make the assertion mean what it says: steer by the **cube's**
distance to the pad rather than the player's, and then **walk the player off**
before reading the button. `a_falling_cube_presses_a_floor_button` — the
synthetic half, which has no player in the map at all — cannot fail this way
and is the reason the filter itself is guarded independently.

The general form, and the second time it has bitten this pair of modules: **a
test whose success condition can be met by something other than the thing under
test is not a test of that thing.** The first time it was the harness supplying
the player's position; this time it was the player supplying the press.

### §10.2 A number that was wrong in the repo, and is now measured

The comment this port already carried said *"141 of the game's
`trigger_multiple`s are physics-only in exactly this way"*. It is not 141.
Counted over the 106 shipped maps' entity lumps
(`scratchpad/trigflags.py`):

| | count |
|---|---|
| triggers setting `SF_TRIGGER_ALLOW_PHYSICS` | **841** |
| …of those, allowing no clients at all | **481** |
| `trigger_multiple`s setting it | 325 (307 physics-only) |
| `trigger_portal_cleanser`s | 250 (53) |
| `trigger_catapult`s | 115 (31) |
| `trigger_once`s | 73 (51) |
| `trigger_push`es | 64 (34) |

Every one of the 841 was inert here before the arm existed, because
`MOVETYPE_VPHYSICS` was a thing nothing could be. It is worth stating the
measure precisely, because "physics-only" can mean two different counts: 341
`trigger_multiple`s set no client bit *at all* (many of those simply omit
`spawnflags`, which allows nothing and makes the trigger inert in the shipped
game too), where 307 set the physics bit *and* no client bit.
