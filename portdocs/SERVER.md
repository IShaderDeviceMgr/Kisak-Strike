# Porting the game server → `src/server/`

`game/server/` — `CBaseEntity`, `CGlobalEntityList`, the datadesc, entity I/O, the
event queue, thinks, `MOVETYPE_PUSH`, and the ~200 entity classes Portal 2 actually
places.

Status: **not started.** This document is written before the port, against the current
architecture. Read [`../PORTING.md`](../PORTING.md) first; this is the module detail.

Siblings worth having open: [`CLIENT.md`](CLIENT.md) (the player that already exists),
[`ENGINE_TRACE.md`](ENGINE_TRACE.md) (stage 4 of which is blocked on this document),
[`ENGINE_WORLD_DISP.md`](ENGINE_WORLD_DISP.md) and
[`CLIENT_TONEMAP.md`](CLIENT_TONEMAP.md) (whose one measured gap this closes).

---

## 0. Headline decisions

1. **This module is the entity *framework*, not the entity *classes*.** The framework
   is about 29,800 lines and is entirely present in `legacy/`. The classes are the long
   tail, and the tail is shorter than it looks: Portal 2's 106 shipped maps place
   **60,925 entities of exactly 200 distinct classnames**, and the **top 25 classnames
   are 79.8% of every entity in the game**.

2. **A large part of Portal 2's own entity code is not in this tree.** `server_portal2.vpc`
   lists 63 `.cpp` files and **59 of them are absent**; `server_portal_base.vpc` lists 65
   and 42 are absent. **45 of the 200 classnames a shipped map places have no
   `LINK_ENTITY_TO_CLASS` anywhere in `game/server/`** — 7,946 instances — and **41 of
   those have no factory anywhere in the tree at all**, 5,583 instances, including
   `func_portal_bumper` (2,383), `trigger_portal_cleanser` (371) and the turrets. For
   those, there is nothing to port. They get reconstructed from the shipped FGD, the
   surviving `game/shared/portal*/` half, and observed behaviour. §1.3.

3. **The shipped game ships its own schema.** `depot_621/bin/{base,portal,halflife2,portal2}.fgd`
   define **494 classes**, and **199 of the 200 classnames the maps place have an entry**
   (the exception, `info_overlay_accessor`, is a `vbsp` compile artifact). Keys, their
   types and defaults, inputs and outputs are all there, machine-readable, for classes
   whose C++ was deleted. §1.4.

4. **Entity I/O is the server, for this game.** 61,391 output connections across 17,091
   entities; the single commonest is `OnTrigger` at 39,807. Nothing else in
   `game/server/` comes close to mattering as much — the entire 122,298-line `ai_*`/`nav_*`
   tree exists to serve **293 `npc_*` instances of 6 classnames**. §1.2.

5. **Do not reproduce the inheritance tree.** `CBaseEntity → CBaseAnimating → CBaseToggle →
   CBaseDoor` is a vtable chain whose only load-bearing job is the `datamap_t`/`baseMap`
   walk that `KeyValue` and `AcceptInput` do at runtime. In Rust that walk becomes a
   `&'static ClassDef` with an explicit parent pointer, and the behaviour becomes a trait
   object with three methods. §7.3.

6. **Networking deletes entirely.** `SendTable`/`DT_`/`CNetworkVar`/`edict_t` exist to get
   server state to a client in another process. There is one process. Every
   `IMPLEMENT_SERVERCLASS_ST` block in the tree is a read-through in this port — the
   `env_tonemap_controller` → `C_EnvTonemapController` → `viewpostprocess.cpp` path
   collapses to a struct field. §6.

7. **The tick is the one architectural decision.** Source's server is fixed-tick
   (`DEFAULT_TICK_INTERVAL_PC` is 1/64 in this tree, which is CS:GO's number) and
   `SetNextThink` *quantises to ticks*. This port's frame loop is variable-dt. Resolve
   this before stage 1, not after. §5.

8. **Suggested first four stages produce something visible each time**: the entity list
   and spawn (stage 1), entity I/O and thinks (stage 2 — this is where
   `env_tonemap_controller` lands and where a map's own exposure limits finally apply),
   brush-entity movement (stage 3 — doors open), triggers and touch (stage 4). §8.

---

## 1. Scope

### 1.1 The tree is 447,000 lines and most of it is not this

`game/server/` is 847 `.cpp`/`.h` files and **446,861 lines** — larger than any module
ported so far by an order of magnitude. It decomposes:

| Group | Lines | Disposition |
|---|---:|---|
| `ai_*`, `nav_*` (161 files) | 122,298 | **Deferred**, and it serves 6 classnames — §1.5 |
| `cstrike15/` | 69,666 | **Deleted** — out of scope (`PORTING.md`, "Game scope") |
| `NextBot/` | 18,493 | **Deleted** — CS:GO bots |
| `portal/` + `portal2/` | 17,496 | Port, and it is mostly *missing* — §1.3 |
| The framework (§2) | ~29,800 | **Port faithfully.** This is the module. |
| Everything else | ~189,000 | The entity classes. Port the ones the maps place. |

`game/shared/` is a further 522 files and 266,330 lines, and the server's half of it
(`baseentity_shared.cpp`, `physics_main_shared.cpp`, `saverestore.cpp`, `igamesystem.h`,
`ehandle.h`, `variant_t`) is part of the framework, not a separate module — the
`client`/`server` split it exists to serve does not exist here.

### 1.2 What the maps actually place

Measured over all 106 shipped `.bsp` files, by reading `LUMP_ENTITIES` directly
(all 106 are BSP version 21, all entity lumps uncompressed):

```
entities                60,925        distinct classnames        200
distinct keys              892        entities per map     7 … 1,446 (mean 575)
brush entities ("*N")   11,635        point entities with .mdl  11,033
no model at all         38,257        with a targetname   40,664 (66.7%)
with a parentname        4,582        with ≥1 output      17,091 (28.1%)
```

**63% of all entities have no model of any kind.** The server is mostly invisible
bookkeeping, which is exactly why it can be built and tested before any of it draws.

Cumulative coverage by classname rank — the reason the long tail is affordable:

```
top  5 classnames   42.5%      top  30   83.0%
top 10              58.7%      top  50   90.9%
top 15              68.9%      top 100   98.2%
top 25              79.8%      top 200  100.0%
```

The head, with the number of maps each appears in:

```
  8082 105  logic_relay          1476 105  trigger_once
  8072 105  prop_dynamic         1464  64  path_track
  4651 106  light                1330  26  env_sprite_clientside
  2556  67  vgui_movie_display   1184 105  func_instance_io_proxy
  2502 105  func_brush           1112 106  logic_auto
  2470 105  light_spot           1106  76  info_overlay_accessor
  2383  93  func_portal_bumper   1102 105  env_soundscape
  1910 106  ambient_generic       899 103  trigger_multiple
  1685 105  env_fog_controller    645  42  trigger_playerteam
```

`sp_a1_intro1` — the map the port already loads — has **598 entities of 87 classnames**,
which is close to the game's median and a fair stage-by-stage target.

Two of the head entries are almost free. `func_instance_io_proxy` (1,184 instances, 13th
commonest) is 30 pass-through relays and nothing else: `InputProxyRelay<N>` fires
`m_OnProxyRelay<N>`, thirty times, in 312 lines of boilerplate. `logic_relay` (the
commonest entity in the game) is ~60 lines of Rust — see §4.3.

### 1.3 41 classnames have no source in this tree

`game/server/` contains **507 `LINK_ENTITY_TO_CLASS` names**. Cross-referenced against
the 200 the maps place:

- **155 have a factory** — 52,979 of the 60,925 instances.
- **45 do not** — 7,946 instances. Four of those 45 have a factory in `game/client/` or
  `game/shared/` instead, which is where they belong: `env_sprite_clientside` (1,330),
  `env_sprite` (601), `info_target` (431) and `game_gib_manager` (1) — 2,363 instances
  between them.
- **41 have no factory anywhere in the tree** — 5,583 instances. These are the ones with
  nothing to port.
- 352 registered names are never placed by any shipped map.

The 41 are not an accident of the search. `server_portal_base.vpc` and
`server_portal2.vpc` still *list* the files; the files were removed when this tree was
cut from `cstrike15`:

```
server_portal2.vpc       63 .cpp listed,  59 missing   (4 survive)
server_portal_base.vpc   65 .cpp listed,  42 missing  (23 survive)
```

What survives on the server side is `portal/{prop_portal, portal_player, portal_base2d,
physicsshadowclone, PhysicsCloneArea, prop_mirror, pvs_extender,
portal_physics_collisionevent}` and `portal2/{prop_button, prop_floor_button,
prop_linked_portal_door, prop_weightedcube}`. What is gone includes
`func_portal_bumper` (2,383 placed), `trigger_portal_cleanser` (371),
`npc_portal_turret_floor` (128), `npc_security_camera` (84), `trigger_catapult` (185),
`info_placement_helper` (392), `weapon_portalgun`, and the entire paint system
(`paint_sphere`, `paint_sprayer`, `cpaintblob`, `paint_stream`).

`game/shared/portal/` is in much better shape — `portal_gamemovement.cpp`,
`portal_placement.cpp`, `paint_blobs_shared.cpp`, `paint_power_info.cpp`,
`portal_base2d_shared.cpp` and 40-odd more files are all present. So for the paint and
portal systems the *shared* half exists and the *server* half must be rebuilt around it.

**This changes what "porting" means for those classes.** For everything in §1.2's head
it is the usual exercise: read the C++, understand it, design the Rust. For these 41 it
is reconstruction from three sources — the FGD (§1.4), the surviving shared code, and
the maps' own I/O graphs, which say exactly which inputs each class must accept and
which outputs it must fire. Record that difference at each site; a reconstructed entity
is a different kind of claim from a ported one.

### 1.4 The shipped FGDs are the schema, and they cover 199 of 200

`depot_621/bin/` ships `base.fgd` (6,996 lines), `halflife2.fgd` (2,082),
`portal.fgd` (503) and `portal2.fgd` (910), the last `@include`-ing the other three.
Together they define **494 classes**: 323 `@PointClass`, 80 `@SolidClass`, 73
`@BaseClass`, 10 `@FilterClass`, 3 `@NPCClass`, 3 `@MoveClass`, 2 `@KeyFrameClass`.

**199 of the 200 classnames placed by a shipped map have an entry.** The one that does
not is `info_overlay_accessor` (1,106 instances), which `vbsp` generates during compile
and which no `.vmf` ever contains.

An FGD entry gives, per class: every keyvalue with its type (`integer`, `float`,
`string`, `choices`, `flags`, `target_destination`, `studio`, …) and default, every
spawnflag bit with its meaning, every input with its parameter type, and every output.
That is precisely the `DEFINE_KEYFIELD`/`DEFINE_INPUTFUNC`/`DEFINE_OUTPUT` content of a
`BEGIN_DATADESC` block, in a form a script can read.

Two consequences:

- For the 41 classes of §1.3 the FGD is the **only** surviving description of the
  interface, and it is a complete one. It says nothing about behaviour.
- For the other 155 it is a cross-check. A `ClassDef` table (§7.3) that disagrees with
  the FGD about a key's name, type or default is wrong, and that check is mechanical.
  This is a candidate for a depot-gated test in the same family as the existing ones.

Do **not** parse FGDs at runtime. They are a design-time artifact; the port's tables are
source.

### 1.5 AI: 122,298 lines for 293 entities

The `ai_*` and `nav_*` files are 27% of `game/server/`. What places them:

```
   160  generic_actor          32  scripted_sequence
   128  npc_portal_turret_floor 21  npc_personality_core
   113  info_node               18  ai_script_conditions
    84  npc_security_camera     15  npc_wheatley_boss
    44  npc_bullseye             5  ai_relationship
                                 3  ai_addon_builder
                                 1  npc_enemyfinder
```

624 instances in the whole game, 293 of them `npc_*`, across 6 NPC classnames — and
**four of those six have no source in this tree** (§1.3). The schedule/task/condition
machinery of `CAI_BaseNPC` (14,382 lines on its own) is HL2's, sized for a game with
combat. Portal 2 has turrets that pivot and shoot, cameras that track, and two scripted
boss entities.

**Defer all of it, and when it comes back, reconstruct rather than port.** Nothing on
the critical path needs a behaviour tree. `generic_actor` and `scripted_sequence` are
choreography (`sceneentity.cpp`, 6,159 lines), which is a separate question and also
deferred.

---

## 2. Inventory: the framework

These are the files to read, and roughly the order to read them in. Line counts are
this tree's.

| File | Lines | What it is |
|---|---:|---|
| `game/server/baseentity.h` | 2,972 | `CBaseEntity` — the whole entity interface |
| `game/server/baseentity.cpp` | 8,998 | …and its implementation; `AcceptInput` at :4457 |
| `game/shared/baseentity_shared.cpp` | 2,848 | `ParseMapData`, `KeyValue`, `SetNextThink` |
| `game/server/entitylist.cpp` | 2,010 | `CGlobalEntityList`; the name/classname searches |
| `game/server/entitylist.h` | 396 | …and the list's own interface |
| `game/server/cbase.cpp` | 1,857 | `CEventAction`, `CBaseEntityOutput`, `CEventQueue`, `variant_t` |
| `game/server/entityoutput.h` | 201 | `COutputEvent` and the `CEntityOutputTemplate` family |
| `game/server/eventqueue.h` | 83 | `CEventQueue`'s interface |
| `game/server/variant_t.h` | 121 | The I/O value type |
| `public/datamap.h` | 521 | `fieldtype_t`, `FTYPEDESC_*`, the `DEFINE_*` macros |
| `game/shared/saverestore.cpp` | 3,759 | `ParseKeyvalue` at :3377 — and nothing else needed |
| `game/server/mapentities.cpp` | 688 | Entity-lump parse, spawn ordering, `Activate` |
| `game/server/physics_main.cpp` | 2,340 | `Physics_RunThinkFunctions`, the pusher, movetypes |
| `game/shared/physics_main_shared.cpp` | 2,225 | `PhysicsRunThink`, touch/ground links |
| `game/server/subs.cpp` | 365 | `LinearMove`/`AngularMove` — how a door moves |
| `game/server/util.cpp` + `.h` | 4,155 | `UTIL_Remove`, `UTIL_SetOrigin`, the search helpers |
| `game/shared/igamesystem.h` | 260 | The level/frame lifecycle hooks |
| `game/shared/ehandle.h` | 196 | `CHandle<T>` over `CBaseHandle` |
| `public/const.h` | 469 | `MOVETYPE_*`, `SOLID_*`, `FSOLID_*`, handle bit layout |
| `game/server/gameinterface.cpp` | 4,037 | `CServerGameDLL::GameFrame` at :1383 — the frame order |

Entity classes worth reading early, because they are the commonest or the most
instructive:

| File | Lines | Why |
|---|---:|---|
| `logicrelay.cpp` | 171 | The commonest entity in the game, and a complete worked example |
| `logicauto.cpp` | 134 | How a map bootstraps itself |
| `func_instance_io_proxy.cpp` | 312 | 13th commonest; 30 relays; zero behaviour |
| `env_tonemap_controller.cpp` | 364 | Closes `CLIENT_TONEMAP.md`'s one measured gap |
| `logicentities.cpp` | 3,251 | `logic_timer`, `logic_case`, `logic_branch`, `math_counter`, `env_global` |
| `doors.cpp` | 1,395 | `func_door`, on top of `CBaseToggle` |
| `bmodels.cpp` | 1,502 | `func_brush`, `func_rotating`, `func_illusionary` |
| `triggers.cpp` | 6,650 | Every `trigger_*`; 4,286 instances placed |
| `buttons.cpp` | 1,598 | `func_button`, `momentary_rot_button` |
| `trains.cpp` | 3,418 | `func_tracktrain` + `path_track` (233 + 1,464 placed) |
| `world.cpp` | 980 | `worldspawn` — 106 placed, one per map, and it is an entity |

Not in the inventory and deliberately so: `player.cpp` (9,940), `props.cpp` (7,484),
`physics.cpp` (3,037, the `vphysics` bridge). §6.

---

## 3. Dependency graph

**What `server/` needs that already exists:**

- `engine::trace` — stages 1-3. Entity movement, ground checks and triggers are all
  traces. Stage 4 of `ENGINE_TRACE.md` (entities in the clip chain) is the reciprocal
  dependency and is blocked *on this module*, so the two are built together: `server/`
  owns the entity list that `trace/` stage 4 enumerates.
- `engine::world` — `World::brush_models` already resolves `"model" "*N"` from the
  entity lump, and `BrushModel::model_to_world` is already the transform both the
  renderer and the trace use. §7.4 is about handing that ownership over.
- `engine::console` — cvars and `ConCommand`s. Every entity class registers some.
- `engine::host` — `HostState`'s level machine is where `LevelInit`/`ServerActivate`/
  `LevelShutdown` hang.
- `client::Player` — the player is an entity in Valve's model. §7.4.
- `filesystem::Vfs` — the `.bsp` pak lump is already mounted at map load, which is
  where `point_template` and VScript content live.

**What `server/` does not need and must not grow a dependency on:**

- `materials/`, `studio/` — an entity holds a *model name*. Resolving it is the
  renderer's job. The module should be unit-testable with no GPU, the way `host/`,
  `trace/` and `input/` already are. This is the single most important structural
  constraint in the document.
- `net/` — there is no wire.

**What is blocked on `server/`:**

- `ENGINE_TRACE.md` stage 4.
- `CLIENT_TONEMAP.md`'s `env_tonemap_controller` — 105 of 106 maps.
- `CLAUDE.md`'s `noclip` wart (`noclip` is a *server* command living in `src/client/`).
- Moving doors, platforms and test-chamber machinery, which `world/` already draws and
  `trace/` already collides with and which nothing moves.

---

## 4. The architecture you need in your head

### 4.1 An entity is a keyvalue bag until `Spawn`

`MapEntity_ParseAllEntities` (`mapentities.cpp:560`) walks the entity lump block by
block. For each block it reads `classname` first, calls `CreateEntityByName` to get an
instance from the factory dictionary, then `ParseMapData` feeds **every** key to
`KeyValue`, then the entity is queued. Nothing is spawned during parsing.

`CBaseEntity::KeyValue` (`baseentity_shared.cpp:361`) is a long `if` ladder of special
cases — `rendercolor`, `renderamt`, `disableshadows`, `angles`, `origin`, `targetname`,
about thirty of them — and then a fallback that walks the datadesc chain calling
`ParseKeyvalue` (`saverestore.cpp:3377`), which matches `FTYPEDESC_KEY` fields by
`externalName`, case-insensitively, and writes the field by offset with a
`switch (fieldType)` of `atoi`/`atof`/`UTIL_StringToVector`/`AllocPooledString`.

Two details in the parser that the port must match and one it can drop:

- **An entity's keys are an ordered list with duplicates, not a map.** Across the 106
  maps there are **38,465 duplicate key occurrences, and only 14 of them are not
  `On*` outputs** — an output key legitimately repeats once per connection. The port's
  existing `bsp::Entity` already stores `Vec<(String, String)>`, which is right.
- `KeyValue` strips everything from a `#` onward from the key name ("temp hack, until
  worldcraft is fixed"). **No key in any shipped Portal 2 map contains a `#`.** The hack
  is dead data for this game; note it and skip it.
- Unknown keys are silently ignored. With 892 distinct keys in the shipped maps against
  a much smaller set the port will implement, a *counter* of ignored keys per classname
  is worth having — it is the cheapest possible progress metric for this module.

Spawning is then a three-pass affair (`mapentities.cpp:130-300`):

1. **Depth**, `ComputeSpawnHierarchyDepth_r` — follow `parentname` to the root, counting.
   4,582 entities have a `parentname`.
2. **Sort**, `SortSpawnListByHierarchy` — by depth ascending, with a classname priority
   registry breaking ties: `func_wall` 10, `scripted_sequence` 9, the `phys_*`
   constraints and `trigger_vphysics_motion` 8, `prop_physics` and `prop_ragdoll` 7,
   everything else −1. A parent always spawns before its child.
3. **Spawn then Activate**, `SpawnAllEntities` — two complete passes over the list.
   `Spawn()` sets up one entity; `Activate()` may look at others, because by then they
   all exist. A `Spawn` that returns < 0 removes the entity and the list is re-walked
   for anything its removal took with it.

`worldspawn` is special-cased out of the list entirely and spawned first, with its
`parentname` forcibly cleared. `CNodeEnt` and `CLight` are spawned immediately during
the walk rather than queued, because both remove themselves in `Spawn` and Valve was
running out of edicts — **4,651 `light` and 2,470 `light_spot` entities** exist in the
shipped maps purely to be deleted at load (their contribution is already baked into the
lightmaps this port reads). `point_template` (302 placed) is collected and spawned
before everything else.

This port has no edict limit, so the light shortcut is an optimisation rather than a
necessity — but the *outcome* must be the same: a `light` is not a live entity.

### 4.2 The datadesc is three mechanisms wearing one hat

`datamap_t` is a flat array of `typedescription_t` plus a `baseMap` pointer to the
parent class's. Each entry has a field type, a byte offset, an `externalName` and a flag
word. Four of the 32 `FTYPEDESC_*` bits matter:

| Flag | Set by | Consumed by |
|---|---|---|
| `FTYPEDESC_KEY` 0x0004 | `DEFINE_KEYFIELD` | `ParseKeyvalue` at load |
| `FTYPEDESC_INPUT` 0x0008 | `DEFINE_INPUTFUNC`, `DEFINE_INPUT` | `AcceptInput` at run time |
| `FTYPEDESC_OUTPUT` 0x0010 | `DEFINE_OUTPUT` (which also sets `KEY`) | `ParseKeyvalue`, building the action list |
| `FTYPEDESC_SAVE` 0x0002 | most of them | save/restore — **deleted**, §6 |

So one table serves map parsing, entity I/O and save/restore at once. The port needs the
first three and not the fourth, which removes the only reason the table has to be
*data about field offsets* rather than *code*. §7.3.

`DEFINE_INPUT` is the interesting hybrid: `FTYPEDESC_INPUT | FTYPEDESC_KEY`, no handler
function. Such a field can be set from the map *and* from an input at run time, and
`AcceptInput` writes it directly (`baseentity.cpp:4569`) with no code involved.

### 4.3 Entity I/O

**The connection.** An output key's value is five fields:
`target ␛ input ␛ parameter ␛ delay ␛ times-to-fire`. The delimiter is `0x1B` (ESC)
if the string contains one and a comma otherwise
(`cbase.cpp:128`, `public/entitydefs.h:17` — ESC so that a parameter may contain commas).
Measured across all 106 maps: **61,375 values use ESC and 16 use a comma.** Both paths
are live; the comma path is 16 connections in the whole game.

```
connections                61,391      on 17,091 entities (28.1%)
distinct output names         163      distinct input names       325
with a delay > 0           15,274      times-to-fire = 1       2,924
```

A `times-to-fire` of `1` makes the action **delete itself from the list after firing**
(`CBaseEntityOutput::FireOutput`, `cbase.cpp:265`); `-1`, the default, is
`EVENT_FIRE_ALWAYS`. 2,924 connections depend on this, so the action list is mutable
state, not a parsed constant.

**Firing.** `FireOutput` walks the action list and posts each action to the global
`CEventQueue` with its delay. It does **not** call the target. There is one trap in this
function worth pinning with a test:

> **A per-action parameter override silently discards the caller's extra delay.** The
> no-override branch posts at `ev->m_flDelay + fDelay`; the override branch posts at
> `ev->m_flDelay` alone (`cbase.cpp:280` vs `:289`). This is Valve's, it is almost
> certainly a bug, and reproducing it is cheaper than discovering later that some door
> in Chapter 4 is out of sync.

**Dispatch.** `CEventQueue::ServiceEvents` (`cbase.cpp:911`) drains everything whose
fire time has arrived, and its loop has the semantic that defines how a Source map
behaves:

```cpp
// remove the event from the list (remembering that the queue may have been added to)
RemoveEvent( pe );
delete pe;
...
// restart the list (to catch any new items have probably been added to the queue)
pe = m_Events.m_pNext;
```

> **The queue restarts from the head after every event.** An input handler that fires an
> output with zero delay has that output dispatched *within the same `ServiceEvents`
> call*. A chain of eight zero-delay `logic_relay`s completes in one tick, not eight.
> Get this wrong and every map in the game runs its logic in slow motion.

Per event, the target resolution is, in order: by name (all matches, each gets the
input); then by direct handle if one was stored; and **if neither found anything, by
classname** — so `OnTrigger → env_fog_controller → SetFogController` reaches every fog
controller in the map without naming one. 2,747 connections in the shipped maps use
`SetFogController` this way.

**Receiving.** `CBaseEntity::AcceptInput` (`baseentity.cpp:4457`) walks the datadesc
chain for an `FTYPEDESC_INPUT` field whose `externalName` matches case-insensitively,
coerces the `variant_t` to the field's declared type (`variant_t::Convert`,
`cbase.cpp:1289` — a small square table; `FIELD_VOID` and `FIELD_INPUT` accept
anything), and then either calls the handler or writes the field. An unmatched input is
a `DevMsg`, not an error.

**The inputs that actually get used.** 325 distinct names, and the **top 45 are 91.5%**
of all connections:

```
  8513 Trigger              1972 SetPlaybackRate      1147 CancelPending
  5389 SetAnimation         1964 PlaySound            1121 SetParentAttachmentMaintainOffset
  4013 Enable               1896 Kill                  923 OnProxyRelay1
  2868 Disable              1686 SetAutoExposureMax    734 TurnOn
  2747 SetFogController     1684 SetAutoExposureMin    671 Close
  2515 SetDefaultAnimation  1680 SetTonemapRate        604 OnProxyRelay2
  2262 RunScriptCode        1618 SetTonemapPercentBrightPixels
                            1446 Skin                  563 TurnOff
                            1218 SetValue              528 SetTextureIndex
                            1157 …                     510 Start
```

Four of the top twelve are the tone mapper. §7.4.

**`logic_relay`, in full**, because it is the commonest entity in the game and porting
it exercises nearly the whole subsystem (`logicrelay.cpp`, 171 lines):

- Keys: `StartDisabled` (249 of 8,082 set it).
- Spawnflags: `1` remove-on-fire (308), `2` allow-fast-retrigger (784). 6,989 have
  neither.
- Inputs: `Trigger`, `Enable`, `Disable`, `Toggle`, `CancelPending`, `EnableRefire`.
- Outputs: `OnTrigger`, `OnSpawn`.
- `Trigger` fires `OnTrigger` **forwarding the activator it received** — which is what
  makes `!activator` work across a chain of relays.
- Unless flag 2 is set, it then latches itself closed and posts `EnableRefire` to itself
  at `m_OnTrigger.GetMaxDelay() + 0.001`. Without that latch, a relay re-triggered
  during its own delay double-fires. 86% of the game's relays rely on it.
- `OnSpawn` is implemented by scheduling a think at `curtime + 0.01` in `Activate`, but
  **only if something is connected to it** — otherwise the entity never thinks at all.

### 4.4 Finding an entity by name

`FindEntityByName` (`entitylist.cpp:752`) is a linear walk of the entity list, and the
comparison is `EntityNamesMatchCStrings` (`baseentity.cpp:644`): case-insensitive, and
the **only** wildcard is a trailing `*`. 234 connections in the shipped maps use one.

A name beginning with `!` is *procedural* and resolves to exactly one entity
(`FindEntityProcedural`, `entitylist.cpp:651`), never iterated. Portal 2's maps use five
of the eight:

```
  1640  !player           520  !self
   535  !player_blue      402  !activator
   534  !player_orange
```

`!caller`, `!picker` and `!pvsplayer` appear zero times in the shipped maps. `!player`
is `UTIL_PlayerByIndex(1)`; `!player_blue`/`!player_orange` are the co-op teams and are
behind `#ifdef PORTAL2` in this tree, which is a rare case of the cstrike15 branch
carrying Portal 2 code rather than losing it.

40,664 entities (66.7%) carry a `targetname`, so the linear walk is over tens of
thousands of entries several times per tick. Valve kept a `m_iName` cached in the
`CEntInfo` slot to avoid touching the entity. The Rust version should hold a
`HashMap<Symbol, SmallVec<EntityId>>` and keep it current on rename/create/destroy —
but **note that the map must preserve insertion order per name**, because a chain of
identically-named relays fires in list order and level designers rely on it.

### 4.5 Thinks

Each entity has a base think function plus a list of named *think contexts*, each with
its own next-think time. `SetNextThink` (`baseentity_shared.cpp:952`):

```cpp
int thinkTick = ( thinkTime == TICK_NEVER_THINK ) ? TICK_NEVER_THINK : TIME_TO_TICKS( thinkTime );
```

> **Think times are stored as tick numbers, rounded to nearest** (`TIME_TO_TICKS` is
> `(int)(0.5f + dt / TICK_INTERVAL)`). `SetNextThink(curtime + 0.01)` is one tick at
> 64 Hz and *zero* ticks at 30 Hz. This is the sharpest edge of §5.

`PhysicsRunSpecificThink` (`physics_main_shared.cpp:2080`) fires a think whose tick has
arrived, and does one thing that surprises people:

```cpp
SetNextThink( nContextIndex, TICK_NEVER_THINK );
PhysicsDispatchThink( thinkFunc );
```

> **The schedule is cleared before the think runs.** A think that does not reschedule
> itself never runs again. Every recurring behaviour in the game re-arms itself on the
> way out.

Scheduling idiom, measured over `game/server/*.cpp`: 456 `SetNextThink` calls, of which
**388 are `curtime + <seconds>`** and only 9 mention `TICK_INTERVAL`. 71 sites use
`TICK_NEVER_THINK`. So the *authoring* is in seconds even though the *storage* is in
ticks — which is what makes §5's variable-dt option survivable.

An entity with no think function carries `EFL_NO_THINK_FUNCTION` and is skipped
immediately. More importantly, `Physics_RunThinkFunctions` does not iterate the entity
list at all — it copies a **`SimThink` list** of entities that have registered interest.
With 60,925 entities and 38,257 of them inert bookkeeping, that list is the difference
between a server frame and a server stall. Build it from the start.

### 4.6 The frame

`CServerGameDLL::GameFrame` (`gameinterface.cpp:1383`), with the CS:GO-specific and
Steam-specific steps removed:

```
gEntList.CleanupDeleteList()                  // anything removed outside the loop
IGameSystem::FrameUpdatePreEntityThinkAllSystems()
GameStartFrame()
Physics_RunThinkFunctions( simulating )       // thinks + movetype simulation
IGameSystem::FrameUpdatePostEntityThinkAllSystems()
ServiceEventQueue()                           // the I/O queue, ONCE, after the thinks
gEntList.CleanupDeleteList()                  // anything removed by a think
```

> **`ServiceEvents` runs once per tick and after every think.** An output fired during a
> think is dispatched later in the *same* tick, not the next one — but an input handler
> cannot see a think that has not run yet. The ordering is observable and maps depend on
> it.

`Physics_RunThinkFunctions` (`physics_main.cpp:2282`) resets `gpGlobals->curtime` to the
frame's start time **before each entity**, so an entity cannot observe time advancing
because of another entity's think.

### 4.7 Movetypes, and how a door moves

`MOVETYPE_*` (`public/const.h:172`) — 12 values, of which this module needs:

| Movetype | Used by |
|---|---|
| `MOVETYPE_NONE` | The overwhelming majority. Never moves. |
| `MOVETYPE_PUSH` | Every mover: doors, platforms, trains. Pushes and crushes; does not clip to the world. |
| `MOVETYPE_VPHYSICS` | `prop_physics` (132 + 138 `prop_physics_override` placed). Deferred with `vphysics`. |
| `MOVETYPE_WALK` | The player. **Already ported** — `CLIENT.md` stage 4. |
| `MOVETYPE_NOCLIP` | The player again, and `MOVETYPE_FLY`/`FLYGRAVITY`/`STEP` for NPCs. |

`SOLID_*` and the `FSOLID_*` flags (`const.h:216`, `:228`) are the other half. The
`FSOLID_TRIGGER` bit is the thing `ENGINE_TRACE.md` stage 2 already noted is missing: a
trigger's brushes are `CONTENTS_SOLID` in the file, and what makes them non-solid is a
flag a game DLL sets. This module is that game DLL.

**A mover is astonishingly simple.** `CBaseToggle::LinearMove` (`subs.cpp:214`) in full:

```cpp
Vector vecDestDelta = vecDest - GetLocalOrigin();
float flTravelTime = vecDestDelta.Length() / flSpeed;
SetMoveDoneTime( flTravelTime );
SetLocalVelocity( vecDestDelta / flTravelTime );
```

Set a velocity, set an arrival alarm. `MOVETYPE_PUSH` integrates the velocity each tick
and pushes whatever is in the way; at the alarm, `LinearMoveDone` snaps the origin to the
exact destination, zeroes the velocity and calls the class's `MoveDone` callback.
`AngularMove` is the same four lines on angles.

> **`SetMoveDoneTime` is a second timer, independent of the think schedule.** A mover
> uses both at once. Conflating them is the obvious mistake and it breaks doors that
> think while moving.

Of the 11,635 brush entities in the shipped maps, **1,164 move**, across 9 classnames —
and the commonest mover is not the one you would guess:

```
  346  func_door_rotating     64  func_button       2  func_rot_button
  275  func_door              27  func_rotating     1  momentary_rot_button
  233  func_tracktrain        20  func_tanktrain
  196  func_movelinear
```

`func_door_rotating` outnumbers `func_door`. Do `AngularMove` first.

### 4.8 Handles, and deletion that is never immediate

`CBaseHandle` packs a 14-bit slot index (`NUM_ENT_ENTRIES` = 16,384) and a 16-bit serial
number into a `u32`. The serial is bumped when a slot is reused, so a stale handle
resolves to `NULL` rather than to whatever moved in. `MAX_EDICTS` is 4,096 and the
largest shipped map (`mp_coop_start`) places 1,446 entities, so the port has generous
headroom either way.

This is a generational index and Rust does it natively — a `Vec<Option<Entity>>` plus a
parallel `Vec<u32>` of generations, or any of the slotmap crates if one earns its place
under `PORTING.md`'s dependency rules (it probably does not; this is thirty lines).

`UTIL_Remove` **marks** an entity and appends it to a delete list; the actual free
happens in `CleanupDeleteList`, twice a frame (§4.6). During
`Physics_RunThinkFunctions`, `UTIL_RemoveImmediate` is explicitly disabled. An entity
being iterated can therefore always be safely removed, which is the entire reason the
mechanism exists — and it maps onto Rust's borrow rules almost too well: a deferred
removal queue is what you would write anyway.

### 4.9 Game systems

`IGameSystem` (`game/shared/igamesystem.h`) is a registry of singletons with lifecycle
hooks: `Init`/`PostInit`/`Shutdown`, `LevelInitPreEntity`/`LevelInitPostEntity`,
`LevelShutdownPreEntity`/`LevelShutdownPostEntity`, and for the per-frame variant
`FrameUpdatePreEntityThink`/`FrameUpdatePostEntityThink`.

Small, and genuinely useful — `CTonemapSystem` (which tracks the master
`env_tonemap_controller`) is one. Port it as a plain enum-dispatched list or a
`Vec<Box<dyn GameSystem>>`, not as a self-registering static constructor; the C++ uses
`CAutoGameSystem`'s constructor-registration trick, which Rust has no equivalent of and
does not need.

---

## 5. The tick: decide this before stage 1

Source's server runs on a **fixed tick**. `gpGlobals->tickcount` increments by one per
server frame, `interval_per_tick` is constant for a level, `TIME_TO_TICKS` quantises
every think, and `Physics_RunThinkFunctions` hands every entity the same `curtime`.

This port has no tick. `Engine::frame` (`src/engine/mod.rs:485`) advances
`scene.curtime` by the real elapsed frame time and `update_client` moves the player with
a variable `dt`. `host::FrameClock` caps the rate; it does not quantise it.

**The evidence, for and against.**

For keeping the variable frame: 388 of 456 `SetNextThink` calls are
`curtime + <seconds>` and only 9 name `TICK_INTERVAL` (§4.5), so entity *authoring* is
in seconds. `CLIENT.md` stage 4's movement is already variable-`dt` and works.
A variable frame is one fewer moving part.

Against: `TIME_TO_TICKS` rounds to the nearest tick, so `SetNextThink(curtime + 0.01)`
is one tick at 64 Hz and zero at 30 Hz — the difference between "next tick" and "this
tick again", which is an infinite loop in the wrong shape of think. `LinearMove` divides
by travel time and snaps at the alarm, so movers are robust; `logic_timer` with
`RefireTime 0.01` is not. And the tree's own number is suspect: `DEFAULT_TICK_INTERVAL_PC`
is **1/64**, which is CS:GO's rate, in a tree whose game is Portal 2 — exactly the
CS:GO-shaped default `CLAUDE.md` warns about (`DEFAULT_HL2_GAMEDIR` is `"csgo"` in the
same family). `CServerGameDLL::GetTickInterval` (`gameinterface.cpp:1015`) takes it
straight from the macro unless `-tickrate` is given, quantised to `N/512` and clamped to
`[4/512, 25/512]` — i.e. 20.48 to 128 Hz.

**Recommendation: give the server a fixed tick, accumulated inside `Engine::frame`.**
A `ServerClock` that accumulates real frame time and runs zero or more fixed server
ticks per rendered frame, with `tickcount` and `curtime` derived from the tick number.
It is perhaps eighty lines, it is what every subsequent behaviour in this module assumes,
and it is the difference between "thinks are approximately right" and "thinks are right".
The player already runs on the rendered frame and should keep doing so until `net/`
exists — Source's own client does the same thing and calls it prediction.

**What is not yet known**: Portal 2's shipped `interval_per_tick`. It is not in any
shipped `.cfg` and not recoverable from the map files. Treat the rate as a constant with
one definition site, measure it against the shipped game when that becomes possible, and
do not scatter `1.0 / 64.0` through the module.

---

## 6. What is deleted, and why

| Deleted | Lines | Why |
|---|---:|---|
| `SendTable`/`DT_`/`CNetworkVar`/`edict_t`/`IServerNetworkable` | large, diffuse | One process. Every `IMPLEMENT_SERVERCLASS_ST` becomes a direct field read. |
| Save/restore (`saverestore.cpp` minus `ParseKeyvalue`) | ~3,600 | `FTYPEDESC_SAVE` and the block-based save format. Portal 2 needs saves eventually; nothing on the boot path does, and reintroducing it as `serde` over the entity state is a better shape than porting `ISave`/`IRestore`. Deferred, not refused. |
| `cstrike15/`, `NextBot/` | 88,159 | Out of scope. |
| `ai_*`, `nav_*` | 122,298 | §1.5. Deferred. |
| `sceneentity.cpp` and choreography | 6,159 | Deferred with the NPCs it drives. |
| `physics.cpp` (the `vphysics` bridge) | 3,037 | `rapier` replaces `vphysics` — `ENGINE_TRACE.md` §5. Deferred to that. |
| `CommentarySystem.cpp` | 1,908 | Portal 2 has developer commentary; it is not on any path. |
| `EntityFactoryDictionary` / `LINK_ENTITY_TO_CLASS` | — | A string-keyed factory registry built by static constructors. Replaced by a `&'static [ClassDef]` table and a `phf`-shaped lookup; §7.3. |
| `CStringRegistry`, `AllocPooledString`, `string_t` | — | String interning. `std` plus a small symbol table. |
| `variant_t`'s save tables (`m_SaveBool` … `m_SaveMatrix3x4Worldspace`) | ~90 | Only exist for save/restore. |
| `Debug_ShouldStep`/`Debug_IsPaused` in `ServiceEvents` | — | `ent_pause`/`ent_step`, a designer debugger. Reconsider once entities work; it is 20 lines and genuinely useful. |
| Foundry (`HandleFoundryEntitySpawnRecords`, `m_bFoundryMode`) | — | Hammer live-edit. Dead. |

**Not deleted, but explicitly out of scope for this document**: `player.cpp` (9,940) and
`props.cpp` (7,484). The player already exists in `src/client/` and joining it to the
entity system is §7.4, not a port of `CBasePlayer`. `props.cpp` covers `prop_dynamic`
(8,072 placed — the second commonest entity in the game), `prop_physics` and
`prop_ragdoll`; `prop_dynamic` needs `studio/` stage 6 and an animation system that does
not exist, and `prop_physics` needs `rapier`. Both get their own portdoc when scheduled.

---

## 7. The Rust design

### 7.1 Module layout

```
src/server/
  mod.rs        Server: the entity list, the tick, LevelInit/Activate/Shutdown
  entity.rs     Entity (the shared state every class has), EntityId, EntityRef
  class.rs      ClassDef, the class table, the factory
  keyvalue.rs   ParseMapData + KeyValue: the entity lump into spawned entities
  io.rs         Output, EventAction, EventQueue, Variant  (§4.3)
  think.rs      the think schedule and the SimThink list   (§4.5)
  name.rs       name lookup, wildcards, the procedural names  (§4.4)
  move_.rs      MoveType, LinearMove/AngularMove, the pusher (§4.7)
  classes/      one file per family
    logic.rs      logic_relay, logic_auto, logic_branch, logic_case, logic_timer,
                  math_counter, func_instance_io_proxy
    brush.rs      func_brush, func_door, func_door_rotating, func_movelinear,
                  func_rotating, func_button
    trigger.rs    trigger_once, trigger_multiple, trigger_teleport, …
    env.rs        env_tonemap_controller, env_fog_controller, env_global, …
    point.rs      info_target, info_player_start, point_teleport, …
```

`mod.rs` names no `wgpu` type, no `winit` type and no `egui` type, and — the constraint
that matters most — **no `materials/` or `studio/` type**. An entity holds a model
*name*. The module is unit-testable with no window and no GPU, like `host/`, `trace/`
and `input/` before it.

### 7.2 The core types

```rust
/// A generational index into `Server::entities`. Valve's `CBaseHandle`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId { slot: u32, generation: u32 }

/// The state every entity has, whatever its class.
pub struct Entity {
    pub class: &'static ClassDef,
    pub name: Option<Symbol>,          // targetname
    pub parent: Option<EntityId>,
    pub origin: Vec3,
    pub angles: Vec3,                  // pitch, yaw, roll — studio/'s convention
    pub velocity: Vec3,
    pub avelocity: Vec3,
    pub move_type: MoveType,
    pub solid: Solid,
    pub solid_flags: SolidFlags,
    pub spawn_flags: u32,
    pub model: Option<ModelRef>,       // "*12" or "models/props/box.mdl" — a NAME
    pub outputs: Vec<Output>,          // name -> actions; duplicates expected (§4.1)
    pub next_think: ThinkSchedule,
    pub move_done_time: Option<f32>,
    pub flags: EntityFlags,            // marked-for-deletion, disabled, …
    pub behaviour: Box<dyn Behaviour>, // the class's own state
}

/// What a class does. Three methods, against the C++'s hundred virtuals.
pub trait Behaviour: Any {
    fn spawn(&mut self, _e: &mut EntityMut<'_>) {}
    fn activate(&mut self, _e: &mut EntityMut<'_>) {}
    fn think(&mut self, _e: &mut EntityMut<'_>, _context: ThinkContext) {}
    fn accept_input(&mut self, _e: &mut EntityMut<'_>, _input: &Input) -> bool { false }
    fn move_done(&mut self, _e: &mut EntityMut<'_>) {}
}
```

`EntityMut<'_>` is the borrow seam: it hands a behaviour its own `Entity` plus a
`&mut ServerContext` for everything it needs to reach outward (fire an output, find by
name, remove an entity, trace). This is the same disjoint-field-borrow move
`console.run(&mut EngineCommands { … })` already makes in `Engine::frame`, and the same
reason `host::Level` is a trait. Do not try to give a behaviour `&mut Server`.

### 7.3 A class is a table plus a trait, not a base class

`ClassDef` replaces `datamap_t` + `LINK_ENTITY_TO_CLASS`:

```rust
pub struct ClassDef {
    pub name: &'static str,                       // "logic_relay"
    pub parent: Option<&'static ClassDef>,        // the baseMap chain
    pub keys: &'static [KeyDef],                  // FTYPEDESC_KEY
    pub inputs: &'static [&'static str],          // FTYPEDESC_INPUT, by name
    pub outputs: &'static [&'static str],         // FTYPEDESC_OUTPUT, by name
    pub create: fn() -> Box<dyn Behaviour>,
}
```

Three things change relative to the C++ and each is deliberate:

- **No field offsets.** `ParseKeyvalue` writes `*(float *)((char *)pObject + offset)`.
  The Rust version dispatches a key to the behaviour, which matches on the name and
  assigns a real field. Slightly more typing per class; no `unsafe`, no `sizeof`
  agreement, and the compiler checks the type. The offsets only existed to let save/
  restore reuse the table, and save/restore is deleted (§6).
- **The chain walk stays.** `keys`/`inputs`/`outputs` resolve by walking `parent` exactly
  as `for (datamap_t *dmap = GetDataDescMap(); dmap; dmap = dmap->baseMap)` does, so
  `StartDisabled` is declared once on a shared `ClassDef` and inherited. This is the one
  piece of the inheritance tree worth keeping, and keeping it as *data* rather than as
  vtables is the whole trick.
- **The table is checkable against the FGD.** §1.4. A depot-gated test that loads the
  four shipped FGDs and asserts every `KeyDef`/input/output name and type agrees is
  worth writing on day one — it catches the entire category of "ported the wrong key
  name", which synthetic tests never will (compare `STUDIO.md` §11's two wrong `.vtx`
  offsets that every synthetic test passed).

### 7.4 The seams

**`world/` — placement moves to the entity.** `find_brush_models`
(`src/engine/world/mod.rs:1152`) currently resolves the entity lump itself and keeps
`classname`, `index` and `render_mode`; the transform lives in `BrushModel`'s private
`origin`/`rotation`, set once at load. When `server/` exists, the entity owns the
placement and `world/` asks for it. Two things must stay true through that change:

- What is drawn and what is collided with must still come from **one** transform — the
  invariant `BrushModel::model_to_world` exists to protect. The entity's origin and
  angles become that single source.
- `BrushModel`'s placement has to become *mutable*. It is baked at load today, which is
  exactly why nothing moves. This is the smallest change that makes stage 3 possible.

`StartDisabled` also comes home here. `world/` records it and does not honour it,
correctly noting it is `server/`'s — 12,372 entities carry the key and 2,808 set it.

**`trace/` — stage 4 is the reciprocal.** `ENGINE_TRACE.md` stage 4 needs an entity list
to enumerate; this module is it. Until then `trace` and `trace_model` are separate
questions and combining them is the caller's job — that stays true for stages 1-3 here.

**`client/` — the player is an entity.** `Player` already has `origin`, `velocity`,
`MoveType` and `old_buttons`. The cheapest correct joining is for the player to *be* an
entity whose behaviour holds `client::Player`, so that `!player` resolves, triggers can
touch it, and `noclip` can move to `src/server/` where `CLAUDE.md`'s wart says it
belongs. Do not network anything; `update_client` keeps running on the rendered frame.

**`client/tonemap.rs` — the gap this closes.** `CLIENT_TONEMAP.md` records that
`env_tonemap_controller` is the one measured absence in an otherwise complete tone
mapper, and the numbers say it is not marginal:

- **110 controllers across 105 of the 106 maps** (`sp_a5_credits` is the exception).
- **105 carry spawnflag 1 (`SF_TONEMAP_MASTER`) and 5 do not** — and the 5 are exactly
  the second controller in the 5 maps that have two, so "the master is the one with the
  flag" resolves cleanly with no tie-break needed.
- `SetAutoExposureMax` (1,686), `SetAutoExposureMin` (1,684), `SetTonemapRate` (1,680)
  and `SetTonemapPercentBrightPixels` (1,618) are the 10th–13th commonest inputs **in
  the entire game**, ahead of `Open`, `TurnOn` and `Close`.
- Portal 2's maps use **5 of the entity's 12 inputs**; the other 7 appear zero times,
  including `SetBloomScaleRange`, whose implementation is broken anyway
  (`env_tonemap_controller.cpp:159` passes `sscanf`'s format and buffer in the wrong
  order and then assigns `m_flCustomBloomScale` twice, never the minimum).

The delivery path in Valve's engine is server entity → `SendTable` → client entity →
`localPlayer->m_hTonemapController` → `GetTonemapSettingsFromEnvTonemapController()`
writes thirteen file-scope globals in `viewpostprocess.cpp` → `GetExposureRange` reads
them. **In one process that is a struct the tone mapper reads.** One further Valve bug
to not reproduce: the no-controller fallback resets `g_bUseCustomAutoExposureMax` and
`g_bUseCustomBloomScale` but **not** `g_bUseCustomAutoExposureMin`
(`c_env_tonemap_controller.cpp:123`), so a custom minimum is sticky for the rest of the
level.

---

## 8. Staged plan

Each stage is meant to end somewhere observable, and stage 2 is where the module starts
paying for itself.

### Stage 1 — the entity list, spawn, and the class table

The list (`Vec<Option<Entity>>` + generations), `ClassDef` and the chain walk, the
factory, `ParseMapData`/`KeyValue` over `bsp::Entity`, the three-pass spawn ordering
(§4.1), `UTIL_Remove`'s deferred deletion, and `LevelInit`/`Activate`/`LevelShutdown`
hung off `host::HostState`. Classes: `worldspawn`, `info_player_start`, `info_target`,
`logic_relay` and `func_instance_io_proxy` as stubs with keys parsed and no behaviour.

Ends with: `ent_dump`-style console output listing every entity a map spawned, with its
class, name, origin and unparsed keys. That last column is the progress metric for the
rest of the module.

Depends on: nothing unbuilt. Tests without a GPU. A depot-gated test spawns all 106 maps
and asserts every entity gets a class or is counted — the same shape as
`world/`'s existing all-maps test.

### Stage 2 — entity I/O, the event queue, and thinks

`Output`/`EventAction` parsing with both delimiters, the `times-to-fire` self-deletion,
`EventQueue` with the restart-from-head semantic (§4.3), `AcceptInput` and `Variant`
coercion, name lookup with wildcards and the five procedural names (§4.4), the think
schedule and the `SimThink` list (§4.5), the fixed server tick (§5), and the frame order
(§4.6).

Classes: `logic_relay` for real, `logic_auto`, `logic_branch`, `logic_case`,
`logic_timer`, `math_counter`, `func_instance_io_proxy` — and **`env_tonemap_controller`
wired to `client::tonemap`** (§7.4).

Ends with: **a map's own exposure limits finally apply.** `sp_a1_intro1` asks for a
ceiling of 1.5 against the cvar default of 2, and it asks for it through a chain of
`logic_auto` → `logic_relay` → `env_tonemap_controller` that this stage makes run. That
is a visible change in the shipped picture, produced by a module with no renderer
dependency, which is a good sign the seams are right.

One implementation note that will otherwise cost a day: `logic_auto` fires `OnMapSpawn`
from a think scheduled at `curtime + 0.2` in `Activate`, not from `Spawn`
(`logicauto.cpp:82`). 998 of the game's 1,112 `logic_auto`s then remove themselves
(spawnflag 1). Every map in the game bootstraps through this one 0.2-second delay.

### Stage 3 — brush entities move

`MoveType`, `MOVETYPE_PUSH`, `LinearMove`/`AngularMove`/`MoveDone` and the
`SetMoveDoneTime` alarm (§4.7); `BrushModel`'s placement becomes mutable and comes from
the entity (§7.4). Classes: `func_door_rotating`, `func_door`, `func_movelinear`,
`func_brush`, `func_button`, `func_rotating` — 1,008 of the game's 1,164 movers.

Deliberately **not** in this stage: pushing the player. `CPhysicsPushedEntities`
(`physics_main.cpp:130-1130`, ~1,000 lines of speculative push, blocker enumeration and
rollback) is most of the complexity and none of the payoff. A door that moves through
the player is a better state than a door that does not move, and the push logic wants
`trace/` stage 4 underneath it anyway.

Ends with: test-chamber doors open.

### Stage 4 — triggers and touch

The touch link list, `FSOLID_TRIGGER`, `StartDisabled` honoured, and `trace/` stage 4
(entities in the clip chain) built alongside. Classes: `trigger_once` (1,476),
`trigger_multiple` (899), `trigger_teleport` (110), `trigger_push` (192),
`trigger_hurt` (215), `point_teleport` (128), and the `filter_*` family (302) that
several of them consult.

Ends with: walking through a trigger fires its outputs. Which is to say, the map starts
responding to the player — the first time anything in this port has.

### Stage 5 — the player as an entity

Join `client::Player` to the entity list (§7.4), move `noclip` to `src/server/`,
resolve `!player`. Depends on stages 1-4.

### Beyond

`prop_dynamic` (8,072 placed) needs `studio/` stage 6 and animation. `prop_physics` and
`func_physbox` need `rapier`. The 41 reconstructed Portal 2 classes (§1.3) need the
paint and portal systems. Each gets its own portdoc.

---

## 9. VScript

Portal 2 is scripted, and the maps say how much:

- **681 entities across 105 of the 106 maps carry a `vscripts` key** —
  `logic_script` (384), `generic_actor` (111), `point_template` (45),
  `func_tracktrain` (42), `trigger_teleport` (37), `prop_testchamber_door` (33), …
- **`RunScriptCode` is the 8th commonest input in the game**, 2,262 connections.
- 295 entities carry a `thinkfunction` key, which names a Squirrel function to run as
  the entity's think.
- The depot ships at least 92 loose `.nut` files under `portal2/scripts/vscripts/`,
  plus whatever is in the VPKs.
- `AcceptInput` itself has a VScript hook: if the entity has a script scope, a function
  named `Input<Name>` runs first and its boolean return decides whether the built-in
  handler runs at all (`baseentity.cpp:4541`).

`legacy/vscript/` contains `vscript.cpp` and `languages/{squirrel,lua,gm,python}`, so
the VM is in the tree — and it is a 2005-era vendored Squirrel, which is exactly the
kind of thing `PORTING.md` says to replace with a crate rather than port.

**This is an open question, not a decision.** The options, with what would pick each:

1. **Defer.** `sp_a1_intro1` has 6 scripted entities out of 598. Most maps open, draw
   and are walkable with every script a no-op. This is the right answer through stage 5.
2. **A Squirrel crate.** Whether a maintained, pure-Rust Squirrel implementation exists
   at the needed fidelity is unverified and should be checked rather than assumed.
3. **`libsquirrel` via FFI.** `PORTING.md` allows exactly one FFI exception
   (`libsteam_api.so`, a closed-source blob). Squirrel is not that; it is open source
   and would be a second exception, which needs a stronger argument than convenience.
4. **Reimplement the surface.** The scripts call a bounded API (`EntFire`,
   `Entities.FindByName`, `self.GetOrigin`, …). Measuring what the shipped `.nut` files
   actually use would size this, and that measurement has not been done.

Whoever picks this up: **do option 4's measurement first**, even if the answer is
option 1. It is a script over the depot's `.nut` files and it turns an unbounded
question into a number.

---

## 10. Open questions and risks

1. **The tick rate.** §5. Portal 2's shipped `interval_per_tick` is not recoverable from
   this tree or from the map files; the tree's 1/64 is CS:GO's. One definition site.
2. **Whether the player is an entity or adjacent to one.** §7.4 recommends "is", for
   `!player` and touch. The risk is dragging `client/`'s variable-`dt` movement into the
   server's fixed tick. Stage 5, deliberately late, so the seam is known by then.
3. **Borrow shape of `EntityMut`.** An input handler that fires an output that reaches
   the same entity is normal and legal in C++. Verify early that the chosen shape
   survives it; a re-entrant `RefCell` panic discovered at stage 4 is expensive.
4. **How much of `CPhysicsPushedEntities` is really needed.** Stage 3 defers all of it.
   Portal 2 has crushing doors and moving platforms the player rides; the condition that
   forces the port is the first puzzle that cannot be solved without standing on
   something that moves.
5. **The 41 reconstructed classes.** §1.3. The risk is silent divergence: reconstructed
   behaviour that looks right and is not. Mark them, and lean on the FGD check (§7.3)
   for at least the interface.
6. **VScript.** §9.
7. **Save/restore.** Deleted for now (§6), and Portal 2 autosaves constantly —
   `logic_autosave` (85), `trigger_autosave` (57), `player_loadsaved` (9). The condition
   that forces it is wanting to keep progress across a session, and the right shape then
   is `serde` over entity state, not a port of `ISave`/`IRestore`.

---

## 11. Notes for whoever picks this up

- **Measure the maps before reading the code.** Every scoping number in this document
  came from a hundred lines of Python over `LUMP_ENTITIES`, and several of them
  (200 classnames, not 500; 41 with no source; the top-25-is-80% curve) invert the
  conclusion you would reach by reading `game/server/`'s directory listing. The
  census script lives in the scratchpad, not the repo; rewriting it as an `--ignored`
  depot test in the existing family is the better home.
- **`legacy/` is ISO-8859, so shell `grep` needs `-a`** (`CLAUDE.md`, "Searching") — and
  it lies without it. The `Grep` tool was unavailable in the session that wrote this
  document, which is the documented fallback.
- **The VPC scripts are the inventory of record for what *should* be here**, which is how
  §1.3's missing files were found: `server_portal2.vpc` lists 63 `.cpp` and 59 of them do
  not exist on disk. When a Portal 2 entity seems to be missing, check the VPC before
  concluding you searched wrong.
- **Do not let `materials/` or `studio/` into this module.** It is the constraint that
  keeps the whole thing testable without a GPU, and it is much easier to hold from the
  start than to recover.
- Write `rustdocs/SERVER.md` as the port lands, per `CLAUDE.md`. The gotchas this
  document already knows will belong in it: the event queue's restart-from-head, the
  parameter-override delay drop, `SetNextThink`'s tick quantisation, the
  schedule-cleared-before-dispatch rule, `logic_auto`'s 0.2-second bootstrap, and the
  fact that a `light` entity is not a live entity.
