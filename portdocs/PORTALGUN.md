# The portal gun — `weapon_portalgun` and `portal_placement.cpp`

> **Written with the port, not before it.** Like `CLIENT_TONEMAP.md`, this is the
> analysis that justifies the shape of the code rather than a plan to follow: the
> gun was not on any staged plan, and most of its source is missing from the tree,
> so the design came out of finding out what could be ported and what had to be
> rebuilt from evidence. The Rust is documented in `rustdocs/SERVER.md`, "The
> portal gun", and `rustdocs/ENGINE.md` (the view model, the reticle, the trace).

`portdocs/PORTAL.md` §8 deleted the gun and all of placement and named the
condition for reversing that: "wanting to play the game rather than test the
mechanism". This is the reversal.

## §0 The decisions, up front

1. **All of `portal_placement.cpp` is ported, case for case, bugs included.**
   It is 1,663 lines of one question — is there room for a 64×112 oval here, and
   if not, a little to one side — and the "a little to one side" is Portal 2's
   feel. A simplified fit would put portals where the shipped game does not,
   which is visible on every shot. The one read of uninitialised memory is the
   only thing not reproduced (§4).
2. **The server half of the gun is reconstructed.** `weapon_portalgun.cpp` is
   not in the tree; the three console commands that give and upgrade the gun
   are defined in it. Their behaviour is pinned from the outside (§2).
3. **A missing class is exactly as much as its readers ask of it.**
   `func_portal_bumper`, `func_noportal_volume`, `trigger_portal_cleanser` and
   `info_placement_helper` have no source here. Placement and the gun read two
   or three things off each, and those are the classes (§3).
4. **Firing is `Server`'s, not the entity's.** A shot traces the whole world and
   moves another entity — the portal — which a `Behaviour` handler cannot do
   from inside the entity list. `WeaponPortalgun` holds `CWeaponPortalgun`'s
   state; `src/server/portalgun.rs` does what it does.
5. **The trace is split by who owns the geometry.** The engine holds the world
   and the brush models (`TouchQuery::shot_trace`); the server's physics
   environment holds the static props and still studio entities
   (`Physics::sweep_studio`). `ShotWorld::trace_line` asks both and keeps the
   nearer, which is what `UTIL_TraceLine` over `enginetrace` and vphysics does
   in one call.

## §1 What is in the tree and what is not

| File | Lines | Here? | Became |
|---|---:|---|---|
| `game/shared/portal/portal_placement.cpp` | 1,663 | yes | `src/server/placement.rs` |
| `game/shared/portal/weapon_portalgun_shared.cpp` | 1,754 | yes | `portalgun.rs`, `classes/weapon.rs` |
| `game/shared/portal/weapon_portalbasecombatweapon.cpp` | — | yes | the fire delays, `ItemPostFrame` |
| `game/server/portal/portal_player.cpp` — `BumpWeapon` | — | yes | `Server::bump_weapon` |
| `game/server/portal/weapon_portalgun.cpp` | — | **no** | the commands, reconstructed (§2) |
| `func_noportal_volume.cpp`, `func_portal_bumper.cpp` | — | **no** | `classes/volume.rs` (§3) |
| `trigger_portal_cleanser.cpp` | — | **no** | `classes/volume.rs`, as a shot blocker only |
| `info_placement_helper.cpp`, `UTIL_FindPlacementHelper` | — | **no** | `PlacementHelper`, the search reconstructed |
| `game/client/portal/c_weapon_portalgun.cpp` | — | **no** | the view model's skin, reconstructed from the model |
| `game/client/portal/hud_quickinfo.cpp` (`CHUDQuickInfo`) | — | **no** | a stand-in reticle |

## §2 How the player gets a gun — the reconstruction

**Three `weapon_portalgun`s are placed in the whole game** (two on `mp_coop_start`,
one on `sp_a3_01`). Everywhere else the gun is given by a console command:

- `sp_a1_intro3` and `sp_a2_intro` fire `Command "give weapon_portalgun"` at a
  `point_servercommand` named `cmd_give_weapon`, and `Kill` the pedestal
  `prop_dynamic` the player took it from. `give` is `GiveNamedItem`, and a new
  weapon touching the player is `BumpWeapon`.
- `transitions/sp_transition_list.nut`'s `OnPostTransition` fires, at `@command`:
  `give_portalgun` on every map from `FIRST_MAP_WITH_GUN` (`sp_a1_intro4`);
  `upgrade_portalgun` as well from `sp_a2_laser_intro`; and `upgrade_potatogun`
  in its place from `sp_a3_speed_ramp`.

What pins each command:

| Command | Evidence | Reconstructed as |
|---|---|---|
| `give_portalgun` | sent on every map after the player has had the gun; must not give a second | `GiveNamedItem("weapon_portalgun")` unless the player owns one |
| `upgrade_portalgun` | `BumpWeapon`'s comment: *"they fired an upgrade_portalgun command to work around it … we need a way to give the player both portals"* | both chips on the gun the player has |
| `upgrade_potatogun` | replaces `upgrade_portalgun` from the map where PotatOS is attached | both chips, and the potato |

**Every command runs twice.** 60 single-player maps have two entities named
`@command` — a `point_servercommand` *and* a `point_clientcommand` — and `EntFire`
reaches both. So all three must be idempotent, and are: repeating one only re-arms
the upgrade delay. The `sp_a1_intro4` depot test sees exactly
`["give_portalgun", "give_portalgun"]`.

`m_bCanFirePortal1` defaults to **true** (the constructor's *"TODO: specify these in
hammer instead of assuming every gun has blue chip"*), so `give weapon_portalgun`
is a blue gun — except on `sp_a2_intro`, where `BumpWeapon` names the map and
gives both. A second gun bumped into is not a second weapon: its chips are copied
onto the first and it is removed.

## §3 The four classes without source

Each is what its readers ask:

| Class | Read by | Asked |
|---|---|---|
| `func_portal_bumper` | `TraceBumpingEntities` | is it active; clip a line to its brush model |
| `func_noportal_volume` | `TraceBumpingEntities`, `IsPortalIntersectingNoPortalVolume` | is it active; its brushes; its OBB |
| `trigger_portal_cleanser` | `PortalTraceClippedByBlockers` | is it enabled; its bounds |
| `info_placement_helper` | `AttemptSnapToPlacementHelper` | origin, `radius`, `snap_to_helper_angles`, its angles, enabled |

The on/off inputs are what the maps fire: `Activate`/`Deactivate` (and spawnflag 1,
`SF_START_INACTIVE`) for the two volumes, `CBaseTrigger`'s `Enable`/`Disable` and
`StartDisabled` for the cleanser. The cleanser is `InitTrigger`'s: `FSOLID_TRIGGER`
only while enabled.

**The fizzler draws itself.** All 1,174 `effects/fizzler*` brush faces in the game
belong to cleanser models and all 284 of those write `Visible 1`, so a visible
cleanser shows its brushes while enabled and hides them while disabled. Before the
class existed the field drew because nothing owned the model — and so a switched-off
fizzler kept drawing.

`UTIL_FindPlacementHelper` is reconstructed as the nearest enabled helper whose
radius reaches the point the shot hit, which is the test the gun then repeats
itself against the re-traced surface.

## §4 Placement, and what it takes from the engine

`VerifyPortalPlacement` in order — every function below is ported:

```text
TraceFirePortal          MASK_SHOT_PORTAL line from the eye; UTIL_Portal_Trace_Filter's
                         classes (cubes, turrets, the player) are not in this port's trace
PortalTraceClippedByBlockers   an enabled cleanser's box stops the shot → Cleanser
IsPassThroughMaterial    sky, lights/light_orange001 → the shot goes on
AttemptSnapToPlacementHelper   → UsedHelper
VerifyPortalPlacement
  FitPortalOnSurface     TracePortalCorner ×4 (in the wall, in front of it, against
                         portals and bumpers), five cases, ≤6 recursions,
                         FindBumpVectorInCorner, FitPortalAroundOtherPortals
  IsPortalIntersectingNoPortalVolume   15-axis OBB test → InvalidVolume
  IsPortalOverlappingOtherPortals      → OverlapLinked
  IsPortalOnValidSurface               SURF_NOPORTAL, sky, glass, moving brush, model
  the floor snap and the vertical-hop check
VerifyPortalPlacementAndFizzleBlockingPortals
```

What the engine had to grow for it:

- **A richer trace.** `ShotHit` carries `fractionleftsolid` (placement walks a
  wall's edge with it), the plane, the surface index, its `SURF_*` flags, its game
  material, and which brush model stopped it — `Tracer::trace_indexed`.
- **Game materials.** `PortalSurfaceType` refuses `CHAR_TEX_GLASS`, which is the
  `gamematerial` of the material's `$surfaceprop`. **4,302 shipped brush sides are
  glass with no `SURF_NOPORTAL`**, so without it portals go on windows and light
  panels. `CollisionBsp::resolve_game_materials` fills it once per map.
- **`clip_to_model`**: one brush model alone, solid or not — a no-portal volume is
  `FSOLID_NOT_SOLID` and still clips.

Divergences and fidelity notes:

- **`SURF_NOPORTAL` is `0x20`**, from `bspflags.h`. The shipped code reads
  `CEG_SURF_NO_PORTAL_FLAG`, filled by an anti-tamper macro whose value is not in
  the tree; its `0xffff` fallback would refuse every lightmapped wall.
- **`TracePortalCorner`'s binary search passes degrees to `cosf`/`sinf`.** Kept:
  the search still converges, on a different direction than its comment says, and
  a corrected one would bump portals where the game does not.
- **`FindBumpVectorInCorner` reads two points it never wrote** when its lines do
  not meet. Here that is no bump — the answer the function's own `FIXME` gives
  for the other degenerate case.
- **A studio model is a surface a portal cannot go on**, reported as `studio`.
- **A shot does not see portals.** Portal 2 stops a segment at a `prop_portal` and
  treats the hit as its wall; this port's traces reach the same wall directly.

## §5 Firing

`ItemPostFrame`, once a tick after `+use`: nothing while carrying; primary before
secondary, and the primary branch returns either way; a click re-fires after 0.2 s
(`portalgun_fire_delay`), a held button (`m_afButtonLast`) after 0.5 s;
`SetCanFirePortal1`/`2` hold the gun 0.25 s / 0.5 s. `OnFiredPortal1` fires from
the primary attack; **nothing fires `OnFiredPortal2`**, though it is declared.

**The button latch.** The gun is the first thing here a short click must reach, and
the client hands over buttons per frame while the server ticks at 64 Hz. The
server now latches every button seen since the last tick and ORs in the buttons
held now — `kbutton_t`'s impulse bit — which fixed `+use` and `+jump` too.

## §6 The view model and the reticle

`v_portalgun.mdl` in its own pass over a cleared depth buffer (Portal's branch of
`DrawViewModels`), `cl_viewmodelfov` 50, near plane 1. The two reconstructions:

- **skin = the last portal fired** (0, 1, 2). The model's three families replace
  exactly one material, the gun's body, with `v_portalgun_blue` and
  `v_portalgun_orange`, in `m_iLastFiredPortal`'s order.
- **body 1 = the potato.** The second body part, `potatos_vmodel`, is an empty
  model 0 and PotatOS as model 1; the first part has one model, so its `base` is 1.
  This is why body groups landed for the view model — drawing every model put the
  potato on the gun everywhere.

The reticle is `CHUDQuickInfo`'s meaning without its art: a ring, left half blue,
right half orange, dim without the chip, outlined when it can fire, bold while
that portal is up.

## §7 Measured

From each of the 106 maps' `info_player_start`, 72 shots (24 yaws × 3 pitches),
7,632 in all:

```text
  6,959  InvalidSurface      most spawns are inside an elevator or container
    319  PassthroughSurface
    257  Bumped              every success on shipped content is a bump
     54  CantFit
     22  Cleanser
     16  InvalidVolume
      5  UsedHelper
```

24 maps take a portal from their spawn. From `sp_a1_intro4`'s floor button: 52
bumped, 3 can't fit, 17 invalid surfaces.

## §8 Not done, and what reverses each

- ~~**The cleanser's touch**~~ — **landed**; `rustdocs/SERVER.md`, "Fizzlers and
  droppers". Walking through a grill closes the player's portals, a cube touching
  one is dissolved, and `FizzleTouchingPortals` works. Only the dissolve's look is
  still absent.
- **`func_portal_detector`** (31) — now buildable, since portals are placed by rule.
- **The gun's effects and sounds** — prongs, beam, glow, muzzle flash; there is no
  particle or sound system.
- **View-model sway** (`CalcViewModelLag`). Portal's bob is empty, so only the lag.
- **Body groups for map entities** — the selector is in `studio/`; only the view
  model uses it.
- **The pedestal gun and co-op** — `PlacedBy::Pedestal` and
  `OverlapPartnerPortal` exist and are unreachable in single player.
- **Paint** — `IsOnPortalPaint` is always false.
- **`FVisible`'s pickup trace, and a dropped gun's physics** — no shipped
  single-player placement needs either.
