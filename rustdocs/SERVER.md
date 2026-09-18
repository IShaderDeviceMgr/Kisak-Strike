# `src/server/` — API reference

The game server: the map's entity list, and the I/O that runs it. Valve's
`server.so`, reduced to the framework — `CBaseEntity`, `CGlobalEntityList`, the
datadesc, the keyvalue parse, the three-pass spawn, `CEventQueue`, `AcceptInput`
and the think schedule. Porting doc:
[`portdocs/SERVER.md`](../portdocs/SERVER.md).

| | |
|---|---|
| Status | **Stages 1-5 of 5, plus `prop_floor_button`, `prop_dynamic`, `prop_testchamber_door`, `logic_branch_listener` and `prop_portal`.** Entities spawn, fire outputs at each other, think on a fixed tick, the brush ones move, the map notices the player, a pad you stand on presses, **the models the map places draw and animate**, **the chamber doors open and shut** — the player can be hurt and die, and **a portal links to its partner and draws an oval**. |
| Depends on | `engine::world::bsp::{Entity, Model}` (the parsed lumps), `engine::console` (eight commands), `client::tonemap::TonemapSettings` (what `env_tonemap_controller` produces) |
| Names no | `wgpu`, `winit`, `egui`, `materials`, `studio`, `engine::trace`, `client::Player` — every test runs with no GPU |
| Tests | 191 unit tests + ten depot tests over all 106 shipped maps |

**What stage 5 added**: `damage.rs` (the `DMG_*` table, `CTakeDamageInfo`,
`m_takedamage`, `m_lifeState` and the health arithmetic), health and death on
`CBasePlayer`, `noclip`'s move from `client/` to here, `logic_playerproxy` and
`player_loadsaved`, and the `god`/`kill`/`hurtme` commands. **`trigger_hurt`
kills**: 138 of the game's 215 kill a player standing in them, and the other 77
are switched off, admit no clients or have nowhere to stand.

**What `prop_dynamic` added** (after stage 5, not part of it): `CDynamicProp`
across its four classnames — **8,462 entities, the commonest thing in a Portal 2
map after `logic_relay`** — `sequences.rs` (the seam that tells the game what a
`.mdl`'s animation is), `DisableDraw`/`EnableDraw` and the `solid` key on
`CBaseEntity`, and a `ModelEntityState` that is **keyed and carries visibility**
rather than positional and filtered.

**What `prop_testchamber_door` added** (after `prop_dynamic`): `CPropTestChamberDoor`
— **138 entities across 71 of the 106 maps, two of them on `sp_a1_intro1`** —
`SequenceInfo::fade_out_time` and the two `CBaseAnimating` questions it answers
(`GetSequenceCycleRate`, `GetLastVisibleCycle`), and the first class here whose
whole behaviour is a *playback rate*. Despite the classname it is a
`CBaseAnimating` and not a prop: no `DefaultAnim`, no `SetAnimation`, no
propdata, five inputs and four outputs.

**What `logic_branch_listener` added** (after `prop_testchamber_door`, and
because of it): `CLogicBranchList` — **158 entities across 46 of the 106 maps**
— the listener list on `CLogicBranch`, and `Context::find_all_by_name`. It is
the AND gate a test chamber shuts its door with, and it is the first class here
where one entity **registers with another** rather than sending it an input:
`Activate` reaches into each branch it names through `Context::behaviour_mut`,
and the branch posts `_OnLogicBranchChanged` back at it when its value moves.
`portdocs/SERVER.md` §10.3 named this class as the condition that would change
the borrow shape, and it did not: stage 4's answer — `Server::dispatch` lifts
the dispatched entity out of the list, so `Context` can carry the rest of it —
was already enough.

**What `prop_portal` added** (after `logic_branch_listener`, and it is
`portdocs/PORTAL.md` **stage 2 of five**): `CProp_Portal` and the placement half
of `CPortal_Base2D` — **21 entities across 10 of the 106 maps, two of them on
`sp_a1_intro1`** — the linkage group, the teleport matrix, `PortalState` (the
third seam of its kind, after `PlayerState` and `ModelEntityState`),
`Context::find_all_of_class`, and the `portal` console command's server half
(`Server::place_portal`, `Server::fizzle_portals`). It links, it computes the
matrix, and `engine::world::portals` draws a coloured oval where it is. **It
does not carve the wall (stage 3) and it does not teleport anybody (stage 4)**,
so what you see is an oval on an unbroken wall that you walk into and stop.

**What does not exist yet**: the weapon (Portal 2's is `weapon_portalgun` and
it needs the portal system), the armour, drowning, and
**nothing pushes what is in its way** — a door moves through the player rather
than shoving it (`portdocs/SERVER.md` stage 3 says why). **41 of the 200
classnames the shipped maps place are implemented**, out of 46 registered — the
other five (`player`, `trigger_portal_button`, `light_glspot`, `dynamic_prop`,
`prop_dynamic_glow`) are placed by no map
([What is deliberately absent](#what-is-deliberately-absent)).

---

## Quick start

```rust
use crate::server::Server;

let mut server = Server::new();
// Both lumps come from the `.bsp`, parsed by `engine::world`: the entities,
// and the model bounding boxes a mover measures itself against.
let stats = server.level_init("sp_a1_intro1", &world.entities, &world.models);
eprintln!("{}", stats.summary());
// 342 of 598 entity blocks matched a class, 321 spawned (22 removed themselves,
// 1 created by another entity), 740 outputs, 256 unknown classnames, 324 unhandled keys

// `ClientPutInServer` — the player joins the list, so that `!player` resolves
// and a trigger has something to notice.
server.spawn_player(player_state);

// Once per rendered frame, either side of the ticks. `query` is the engine's
// collision half; `NoTouchQuery` is a server with nothing to walk into.
server.set_player_state(player_state);
server.frame(frame_time_seconds, &mut query);
if let Some(state) = server.player_state() { /* …back onto client::Player */ }

// Where every brush entity is now, by its "*N" model index — what `world/`
// draws and what `trace/` collides with.
if let Some(entity) = server.brush_entity(12) {
    let (origin, angles) = (entity.origin, entity.angles);
}

// What the map's master env_tonemap_controller is asking for.
let settings = server.tonemap_settings();

server.level_shutdown();
```

In the running engine this is wired through `Scene`, so a `map` command does the
load and `Engine::frame` does the rest, in this order:

```text
Engine::frame
  server.set_player_state( player_state( client ) )   client -> the entity list
  server.frame( dt, &mut WorldTouchQuery { world } )  0..n ticks; triggers notice
  apply_player_state( client, server.player_state() ) …and back again
  sync_brush_models( world, server )                  where every door is now
  update_client( … )                                  the player moves, and is
                                                      clipped against the doors
```

— see `Level::load`, `Engine::frame`, `player_state`, `apply_player_state`,
`WorldTouchQuery` and `sync_brush_models` in `src/engine/mod.rs`. Four console
commands read or drive the result —
`report_entities`, `ent_dump <name / index / class>`,
`ent_fire <target> [input] [value] [delay]` and `dumpeventqueue`.

---

## The core types

### `Server` (`mod.rs`)

```rust
pub struct Server { /* private */ }

impl Server {
    pub fn new() -> Server;
    pub fn with_tick_interval(interval: f32) -> Server;
    pub fn level_init(
        &mut self, map: &str, blocks: &[bsp::Entity], models: &[bsp::Model],
    ) -> LevelStats;
    pub fn level_shutdown(&mut self);
    pub fn frame(&mut self, frame_time: f32, query: &mut dyn TouchQuery) -> u32;
    pub fn time(&self) -> think::Time;
    pub fn brush_entity(&self, model_index: usize) -> Option<&EntityCore>;
    pub fn brush_entity_count(&self) -> usize;
    pub fn tonemap_settings(&self) -> TonemapSettings;
    pub fn model_entities(&self) -> Vec<ModelEntityState>;
    // What `studio/` says about those models. Filled in once, AFTER level_init.
    pub fn set_sequences(&mut self, sequences: SequenceTable);

    // stage 4 — the player
    pub fn spawn_player(&mut self, state: PlayerState) -> EntityId;
    pub fn player(&self) -> Option<EntityId>;
    pub fn set_player_state(&mut self, state: PlayerState);
    pub fn player_state(&self) -> Option<PlayerState>;

    // stage 5 — the player's commands, and the level restart
    pub fn toggle_noclip(&mut self) -> Option<bool>;   // the `noclip` command
    pub fn toggle_god(&mut self) -> Option<bool>;      // the `god` command
    pub fn kill_player(&mut self) -> bool;             // the `kill` command
    pub fn hurt_player(&mut self, amount: f32, damage_type: i32) -> bool;
    pub fn take_level_restart(&mut self) -> Option<String>;

    pub fn report_entities(&self, cx: &mut ExecContext<'_>);
    pub fn ent_dump(&self, cmd: &Command, cx: &mut ExecContext<'_>);
    pub fn ent_fire(&mut self, cmd: &Command, cx: &mut ExecContext<'_>);
    pub fn dump_event_queue(&self, cx: &mut ExecContext<'_>);
}
```

`level_init` is `CServerGameDLL::LevelInit` + `MapEntity_ParseAllEntities` +
`ServerActivate`'s entity half + `IGameSystem::LevelInitPostEntity`, in one
call: the boundaries between them in the original are engine/game-DLL
boundaries that do not exist here. It calls `level_shutdown` first, so calling
it twice replaces rather than appends.

It cannot fail. A block with no classname, a classname with no implementation, a
key nobody understands and a `parentname` naming nothing are all *counted*, not
errors — see [`LevelStats`](#levelstats).

`models` is the `.bsp`'s model lump. It is there for one reason: a mover
computes how far it travels from the size of its own brushes, and that size is
in the file rather than in the entity lump — `SetModel` → `UTIL_SetModel` →
`SetMinMaxSize` (`game/server/util.cpp:1426`). Passing `&[]` is legal and gives
every mover a zero-sized box, which is what `UTIL_SetModel` does for a missing
model too; the unit tests pass it.

`frame` takes the host's already-clamped frame time and returns how many server
ticks it bought. **Zero is the normal answer** at a high frame rate. Its
`query` is the engine's collision half — see
[`TouchQuery`](#touchquery-mod-rs); pass [`NoTouchQuery`](#touchquery-mod-rs)
when nothing can be walked into.

`brush_entity` is the seam `world/` and `trace/` read, keyed by the `"*N"`
model index — see [The brush-entity seam](#the-brush-entity-seam).

`spawn_player` is `ClientPutInServer`, **not** part of `level_init`: Valve's
entity list has no player until a client connects either, and keeping it that
way is what lets every test here run without one. `set_player_state` and
`player_state` are the two halves of the copy `Engine::frame` makes either
side of the ticks — see [`PlayerState`](#playerstate-mod-rs).

The four commands are `game/server/client.cpp`'s, all `FCVAR_CHEAT` there.
**`noclip` is one of them again**: it lived in `src/client/` from `client/`
stage 1 because the move type had nowhere else to be, and `portdocs/CLIENT.md`
§9.2 recorded the condition for moving it as "stage 5, where the move type
becomes the server's state". `hurtme` is this port's own — Valve's is
`#ifdef _DEBUG` — and exists because `trigger_hurt` is the only damage source
in the shipped maps, so without it the only way to test the arithmetic is to
walk into goo.

`take_level_restart` is `engine->ServerCommand( "reload\n" )`, which is what
single-player `respawn()` and `CRevertSaved::LoadThink` both end in. With no
save/restore the nearest honest thing is to start the map again, and
`Engine::frame` reads this once a frame and turns it into
`Host::request_new_game` — so nothing in this module names the host state
machine.

### `TouchQuery` (`mod.rs`)

```rust
pub trait TouchQuery {
    fn brush_models_touching(
        &mut self, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, out: &mut Vec<usize>,
    );
}

pub struct NoTouchQuery;   // reports nothing
```

`engine->SolidMoved` (`vengineserver_impl.cpp:2467`, `engine/world.cpp`'s
`CTouchLinks`) — **the engine's half of a touch test**, and the reason this
module names no collision type. The game knows which entities are triggers and
what touching one means; the engine owns the collision data and answers "what
does this swept box overlap". That split is not a Rust invention: the C++
crosses a DLL boundary at exactly this line.

Implemented by `engine/mod.rs`'s `WorldTouchQuery` over `world/`'s placed brush
models. Two properties of the answer are load-bearing:

- **It is a swept box against the model's real brushes**, not a bounding-box
  overlap — Valve's enumerator ends in
  `ClipRayToCollideable( ray, MASK_SOLID, pTrigger, &tr )`. A test chamber's
  triggers are L- and U-shaped often enough that the two disagree.
- **It is not filtered to triggers.** Which of them is one is
  `FSOLID_TRIGGER`, which is *this* module's live state; an engine-side copy
  would be a frame stale every time something was enabled. The server filters,
  and pays one brush sweep per non-trigger brush entity per tick.

### `PlayerState` (`mod.rs`)

```rust
pub struct PlayerState {
    // The client's, written by the server where it says so.
    pub origin: Vec3,          // the FEET
    pub angles: Vec3,          // the VIEW angles, pitch/yaw/roll
    pub velocity: Vec3,
    pub base_velocity: Vec3,   // what a trigger_push is adding
    pub on_ground: bool,       // FL_ONGROUND — and the only two-way flag
    pub mins: Vec3,            // the collision hull, relative to origin
    pub maxs: Vec3,
    // The SERVER's, since stage 5. `set_player_state` ignores these.
    pub move_type: movement::MoveType,
    pub health: i32,
    pub life_state: LifeState,
    pub flags: u32,            // FL_FROZEN; FL_ONGROUND is masked out
    // The CLIENT's, and never written by the server.
    pub buttons: u32,          // IN_*, as a raw mask
}
```

The player, as the two halves of the port that own pieces of it agree to
describe one. `client::Player` moves on the **rendered frame** and the server
ticks at a fixed 64 Hz, so neither can hold the other's state; `Engine::frame`
copies this in before the ticks and out after them.

**Through stage 4 the round trip was an identity for every field the server did
not touch**, and every field went both ways. **Stage 5 broke that on purpose**
for four of them — `move_type`, `health`, `life_state` and `flags` — because
`noclip`, damage and death are server decisions in the original and all three
would be undone by the client's copy arriving on the next rendered frame.
`Server::set_player_state` ignores what arrives in those four and
`Server::player_state` fills them in; `Engine::frame`'s `apply_player_state`
writes them onto `client::Player`.

`buttons` is the mirror image: the server reads it (`PlayerDeathThink` waits
for buttons, `logic_playerproxy` fires on the press edge) and never writes it.
**A press and release inside one server tick is lost**, which is Valve's too —
a shipped server sees one usercmd per tick and computes the same edge from it.

### `ModelEntityState` (`mod.rs`)

```rust
pub struct ModelEntityState {
    pub id: u64,                  // EntityId::to_int — opaque, stable, the sync key
    pub model: String,            // models/props/portal_button.mdl
    pub origin: Vec3,
    pub angles: Vec3,
    pub skin: i32,
    pub visible: bool,            // ShouldDraw — EF_NODRAW or rendermode 10
    pub sequence: String,         // the label; "" is the bind pose
    pub cycle: f32,               // m_flCycle at anim_time
    pub anim_time: f32,           // m_flAnimTime — the SERVER's clock, see below
    pub playback_rate: f32,       // m_flPlaybackRate, signed; 0 holds the pose
    pub modulation: [f32; 4],     // rendercolor + ComputeRenderAlpha
}
```

Every entity that draws a studio model, and what its model is doing — the
counterpart of [`brush_entity`](#the-brush-entity-seam), which answers "where is
brush model `N`". `engine::world::entities::EntityModels` loads from it once and
syncs against it every frame.

**`modulation` is `GetColorModulation()` and `ComputeRenderAlpha()` as one vector** —
`m_DiffuseModulation`, which `SetupPerInstanceColorModulation`
(`modelrendersystem.cpp:1723`) hands every model draw. An alpha below 1 puts the whole
instance in the renderer's translucent pass, and the alpha is the whole of the render-mode
question: `kRenderNormal` substitutes 255 and every other mode reads `renderamt`, so a
translucent mode at `renderamt 255` is opaque and a normal entity's `renderamt` is
ignored. **30 of the game's 8,462 `prop_dynamic`s** set a translucent mode, 24
`kRenderTransTexture` and 6 `kRenderTransColor`; none sets a glow mode, which is the only
one that would need more than these four numbers. `EntityCore::modulation` is the
computation, and `world/`'s `PlacedBrushModel::modulation` is the same arithmetic over the
same three keys read from the `.bsp` lump instead — a brush entity's placement is resolved
before the game has spawned anything.

**The five animation fields are exactly `DT_BaseAnimating`'s**, which is not a
coincidence: what Valve's client needs in order to pose a model is what this
renderer needs, and one process does not change the list. The pose is
`cycle + (now - anim_time) * playback_rate / duration`, wrapped for a looping
sequence and clamped otherwise; **`DynamicProp::cycle_now` and
`EntityModels::cycle` both compute it** — the server from its
[`sequences`](#sequences-sequencesrs) table and the renderer from the `.mdl` —
and the two must not drift.

Excluded, and each for its own reason: a class that returns no `ModelState`
(39 of the 43, because a model is the exception); and an entity whose `model` is
a `"*N"` brush model, which goes out through the other seam.

`visible` is `C_BaseEntity::ShouldDraw`, and it refuses **two** things:
`EF_NODRAW` (what `StartDisabled` sets) and `rendermode 10`
(`movement::RENDER_NONE`). `world/`'s brush seam has refused both since stage 3,
where the second hides 94 entities; **no shipped `prop_dynamic` writes it**, so
it is there to keep the two seams agreeing rather than for content. The
*translucent* modes 1 and 2 are **not** honoured — 30 props write one and they
want a blended pass, the same gap `world/` records for its five brush
entities.

**The list is keyed on `id`, and it used to be positional.** The note here said
the condition for a real key would be "the first class that appears or
disappears at run time"; `prop_dynamic` is that class twice over — 556 shipped
connections fire `Kill` at one and 51 fire `FadeAndKill`. `EF_NODRAW` is
likewise **carried rather than filtered**, because a prop that is invisible now
may be visible next tick (1,000 are `StartDisabled` and 206 connections toggle
one) and a filtered-out entity is one whose model was never uploaded.

> **`anim_time` is the *server's* clock and the renderer measures against the
> scene's** (gotcha 1). The two track each other and differ by at most one tick,
> because the server's is the scene's quantised down; the renderer clamps a
> negative elapsed time to zero, so the worst case is one frame of an animation
> not having started yet.

### `PortalState` (`mod.rs`)

```rust
pub struct PortalState {
    pub id: u64,              // EntityId::to_int — opaque and stable
    pub origin: Vec3,
    pub angles: Vec3,         // pitch, yaw, roll
    pub half_width: f32,      // 32 for every portal in the game
    pub half_height: f32,     // 56 — NOT 14; see gotcha 79
    pub is_portal2: bool,     // which of the two overlay materials
    pub opened_at: f32,       // the SERVER's clock, like ModelEntityState::anim_time
    pub linked: bool,         // IsActivedAndLinked()
}
```

Every **active** portal, as the renderer needs one — `Server::portals()`. The
third seam of this shape, and the simplest: a portal owns no uploaded geometry,
so the list is *replaced* every frame rather than matched on `id`, and an
inactive portal is filtered out rather than carried with a `visible` flag.
`C_Portal_Base2D::ShouldDraw` refuses an inactive portal and
`CPortalRender::AddPortal`/`RemovePortal` are gated on the same thing.

**What it does not carry is the teleport matrix**, because nothing draws with
it. Stage 2 is an oval on a wall, not a view through one; the matrix stays on
`PropPortal::matrix` until stage 4 moves the player with it.

`opened_at` is what the renderer turns into `$PortalOpenAmount` (0 to 1 over
half a second) and `$PortalStatic` (1 to 0 over one second) —
`C_Prop_Portal::ClientThink` integrates both and this port derives them, the
same split `anim_time` already has and for the same reason.

### `LevelStats` (`mod.rs`)

```rust
pub struct LevelStats {
    pub blocks: usize,            // entity-lump blocks
    pub matched: usize,           // …whose classname is implemented
    pub spawned: usize,           // alive after the spawn pass
    pub removed_on_spawn: usize,  // deleted themselves in Spawn
    pub created: usize,           // made by another entity's Spawn, not by the lump
    pub outputs: usize,           // connections parsed
    pub parented: usize,
    pub parents_missing: usize,
    pub unknown: BTreeMap<String, usize>,    // classname -> count
    pub unhandled: BTreeMap<String, usize>,  // key name (lowercased) -> count
}

impl LevelStats { pub fn summary(&self) -> String; }
```

The parse-side progress metric. Across all 106 maps it is 34,506 of 60,925
blocks matched, 65 created and 27,634 spawned.

`created` is the term that makes `spawned + removed_on_spawn` differ from
`matched`: entities that were never in the entity lump, made by another
entity's `Spawn` through [`Context::create_entity`](#classdef-behaviour-and-context-classrs).
Today that is one `trigger_portal_button` per `prop_floor_button` and nothing
else. The *run*-side metric is
[`IoStats`](#iostats), which `report_entities` prints alongside it.

### `Entity` and `EntityCore` (`entity.rs`)

```rust
pub struct Entity {
    pub core: EntityCore,
    pub behaviour: Box<dyn Behaviour>,
}   // Deref/DerefMut to EntityCore

pub struct EntityCore {
    pub class: &'static ClassDef,
    pub name: Option<String>,          // targetname
    pub target: Option<String>,        // the `target` key — m_target
    pub parent_name: Option<String>,
    pub parent: Option<EntityId>,
    pub origin: Vec3,
    pub angles: Vec3,                  // pitch, yaw, roll
    pub spawn_flags: u32,
    pub model: Option<String>,         // "*12" or "models/…/x.mdl" — a NAME
    pub hammer_id: Option<u32>,
    pub render_color: [u8; 4],
    pub render_mode: u8,
    pub render_fx: u8,
    pub effects: u32,                  // EF_*
    pub entity_flags: u32,             // EFL_*
    pub outputs: Vec<Output>,          // one per output NAME
    pub unhandled: Vec<(String, String)>,
    pub removed: bool,

    // stage 3 — the movement block
    pub model_bounds: ModelBounds,     // the collision box: the "*N" model's,
                                       // or the player's hull
    pub move_type: MoveType,           // NONE, PUSH, WALK or NOCLIP
    pub velocity: Vec3,                // units a second
    pub angular_velocity: Vec3,        // degrees a second
    pub speed: f32,                    // m_flSpeed, the `speed` key
    pub local_time: f32,               // this pusher's own clock

    // stage 4 — solidity, touching, and being pushed
    pub solid: Solid,                  // NONE / BSP / BBOX / VPHYSICS
    pub solid_flags: u32,              // FSOLID_*
    pub flags: u32,                    // FL_*
    pub base_velocity: Vec3,           // what a trigger_push is adding
    pub touch_links: Vec<TouchLink>,   // the TOUCHLINK list

    // stage 5 — damage
    pub take_damage: DamageMode,       // No / EventsOnly / Yes
    pub health: i32,                   // the `health` key — 682 carry it, all 0
    pub max_health: i32,               // the `max_health` key — none carry it
    pub life_state: LifeState,         // Alive / Dying / Dead
    pub damage_accumulator: f32,       // the fraction of a point carried over
    pub damage_filter_name: Option<String>,  // the `damagefilter` key
    pub damage_filter: Option<EntityId>,     // …resolved by Activate
    /* private: id, next_think_tick, move_done_time, touch_stamp, check_untouch */
}

impl EntityCore {
    pub fn classname(&self) -> &'static str;
    pub fn id(&self) -> EntityId;                     // GetRefEHandle
    pub fn debug_name(&self) -> &str;                 // targetname, else classname
    pub fn has_spawn_flags(&self, flags: u32) -> bool;
    pub fn remove(&mut self);                         // UTIL_Remove( this )

    pub fn is_solid(&self) -> bool;                   // IsSolid(), BOTH halves
    pub fn is_solid_flag_set(&self, flags: u32) -> bool;
    pub fn add_solid_flags(&mut self, flags: u32);
    pub fn remove_solid_flags(&mut self, flags: u32);
    pub fn has_flags(&self, flags: u32) -> bool;      // the FL_* set
    pub fn is_alive(&self) -> bool;                   // the LIFE STATE, not health

    pub fn output(&self, name: &str) -> Option<&Output>;
    pub fn output_count(&self, name: &str) -> usize;  // NumberOfElements
    pub fn max_output_delay(&self, name: &str) -> f32;// GetMaxDelay
    pub fn fire_output(
        &mut self,
        name: &str,
        value: Variant,
        activator: Option<EntityId>,
        caller: Option<EntityId>,
        delay: f32,
        cx: &mut Context<'_>,
    );
    pub fn post_to_self(&mut self, input: &str, delay: f32, cx: &mut Context<'_>);
    pub fn cancel_pending(&self, cx: &mut Context<'_>) -> usize;

    pub fn set_next_think(&mut self, time: f32, cx: &Context<'_>);
    pub fn next_think(&self, cx: &Context<'_>) -> f32;

    pub fn set_move_done_time(&mut self, delay: f32);   // arm, in local time
    pub fn move_done_time(&self) -> f32;                // …and how much is left
    pub fn will_simulate_game_physics(&self) -> bool;
}
```

**The split is not decoration.** A `Behaviour` method takes `&mut self` *and*
`&mut EntityCore`; one struct could not provide both. It is the same
disjoint-field-borrow move `Engine::frame`'s `EngineCommands` makes.

### `EntityId` and `EntityList` (`entity.rs`)

```rust
pub struct EntityId { /* slot + generation */ }
impl EntityId {
    pub const INVALID: EntityId;   // Valve's INVALID_EHANDLE
    pub fn slot(self) -> u32;
}

pub struct EntityList { /* private */ }

impl EntityList {
    pub fn new() -> EntityList;
    pub fn insert(&mut self, entity: Entity) -> EntityId;
    pub fn get(&self, id: EntityId) -> Option<&Entity>;
    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut Entity>;
    pub fn is_alive(&self, id: EntityId) -> bool;
    pub fn mark_for_deletion(&mut self, id: EntityId);
    pub fn cleanup_delete_list(&mut self) -> usize;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = (EntityId, &Entity)>;
    pub fn clear(&mut self);
    /* pub(super): detach / attach — the dispatch borrow seam, below */
}
```

`EntityId` is `CBaseHandle`: a generational index, so a handle to a removed
entity resolves to `None` rather than to whatever took its slot. `insert` writes
the handle back into the entity, because everything an entity does to the world
names itself as the caller.

**`detach`/`attach` are the borrow seam stage 4 needed.** `Server::dispatch`
lifts the entity it is about to run *out* of the list and puts it back
afterwards, so a handler can hold `&mut EntityCore` and a
[`Context`](#classdef-behaviour-and-context-classrs) over the rest of the list
at the same time. `len` is decremented across the gap so it never disagrees
with `iter`, and the entity is put back on every path including a panic
(nothing `?`s between the two).

### `ClassDef`, `Behaviour` and `Context` (`class.rs`)

```rust
pub struct InputDef { pub name: &'static str, pub field: FieldType }
impl InputDef { pub const fn new(name: &'static str, field: FieldType) -> InputDef; }
pub type InputDefs = &'static [InputDef];

pub struct ClassDef {
    pub name: &'static str,
    pub keys: &'static [&'static str],     // declaration; see gotcha 12
    pub inputs: InputDefs,                 // declaration AND the declared type
    pub outputs: &'static [&'static str],
    pub create: fn() -> Box<dyn Behaviour>,
}

impl ClassDef {
    pub fn declared_output(&self, name: &str) -> Option<&'static str>;
    pub fn declares_key(&self, name: &str) -> bool;
    pub fn input_type(&self, name: &str) -> Option<FieldType>;
}

pub const BASE_INPUTS: &[InputDef];    // CBaseEntity's: Kill, Use, FireUser1..4
pub fn base_input(name: &str) -> Option<FieldType>;
pub fn base_accept_input(
    entity: &mut EntityCore, behaviour: &mut dyn Behaviour,
    input: &Input<'_>, cx: &mut Context<'_>,
) -> bool;

/// `USE_TYPE` (`shareddefs.h:581`) — and see gotcha 8 for what it really is.
pub enum UseType { Off, On, Set, Toggle, Other }
impl UseType { pub fn from_output_id(id: u32) -> UseType; }

pub enum SpawnResult { Ok, Remove }
pub const NEVER_THINK: f32;            // TICK_NEVER_THINK as a time, -1

pub trait Behaviour: Any {
    fn key_value(&mut self, entity: &mut EntityCore, key: &str, value: &str) -> bool;
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult;
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>);
    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>);
    fn move_done(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>);
    fn use_entity(
        &mut self, entity: &mut EntityCore, use_type: UseType,
        input: &Input<'_>, cx: &mut Context<'_>,
    );
    fn accept_input(
        &mut self, entity: &mut EntityCore, input: &Input<'_>, cx: &mut Context<'_>,
    ) -> bool;
    fn describe(&self) -> Vec<(&'static str, String)>;   // for `ent_dump`

    // stage 4
    fn start_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>);
    fn touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>);
    fn end_touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>);
    fn is_filter(&self) -> bool;                         // dynamic_cast<CBaseFilter*>
    fn passes_filter(
        &self, entity: &EntityCore, other: &EntityCore, filters: &Filters<'_>,
    ) -> bool;
    fn is_player(&self) -> bool;                         // CBaseEntity::IsPlayer
    fn model_state(&self) -> Option<ModelState<'_>>;     // CBaseAnimating's, networked

    // stage 5 — damage
    fn on_take_damage(
        &mut self, entity: &mut EntityCore, info: &DamageInfo, cx: &mut Context<'_>,
    ) -> Damaged;
    fn event_killed(
        &mut self, entity: &mut EntityCore, info: &DamageInfo, cx: &mut Context<'_>,
    );
    fn passes_damage_filter(
        &self, entity: &EntityCore, info: &DamageInfo, cx: &Context<'_>,
    ) -> bool;
}

/// Exactly `DT_BaseAnimating`'s five fields. The sequence is a **label**, not
/// an index, because looking one up needs the `.mdl` and this module names no
/// studio type — and it is borrowed, because a `prop_dynamic`'s comes out of
/// the map and there are 1,141 distinct ones in the game.
///
/// **An empty label, or one the model does not have, means `m_nSequence`'s
/// zero — not "no animation".** See gotcha 65; the engine closes it.
pub struct ModelState<'a> {
    pub sequence: &'a str, pub cycle: f32, pub anim_time: f32,
    pub playback_rate: f32, pub skin: i32,
}

impl dyn Behaviour {
    pub fn downcast_ref<T: Behaviour>(&self) -> Option<&T>;
    pub fn downcast_mut<T: Behaviour>(&mut self) -> Option<&mut T>;
}

pub struct Context<'a> {
    pub time: Time,     // curtime, tick, interval — see think.rs
    /* private: the queue, the random stream, the entity list, the player */
}

impl Context<'_> {
    pub fn curtime(&self) -> f32;
    pub fn random(&mut self) -> &mut RandomStream;

    // stage 4 — every entity BUT the one being dispatched
    pub fn entity(&self, id: EntityId) -> Option<&Entity>;
    pub fn entity_mut(&mut self, id: EntityId) -> Option<&mut EntityCore>;
    pub fn behaviour_mut<T: Behaviour>(&mut self, id: EntityId) -> Option<&mut T>;
    pub fn create_entity(&mut self, classname: &str) -> Option<EntityId>;
    pub fn find_by_name(&self, query: &str) -> Option<EntityId>;
    pub fn find_all_by_name(&self, query: &str) -> Vec<EntityId>;   // names only
    pub fn find_target(
        &self, query: &str, searching: Option<EntityId>,
        activator: Option<EntityId>, caller: Option<EntityId>,
    ) -> Option<EntityId>;                               // procedurals included
    pub fn filters(&self) -> Filters<'_>;

    // stage 5
    pub fn player(&self) -> Option<EntityId>;            // UTIL_GetLocalPlayer
    pub fn take_damage(&mut self, target: EntityId, info: DamageInfo) -> bool;
    pub fn reload_level(&mut self);                      // ServerCommand("reload")

    // prop_dynamic — LookupSequence/SequenceDuration/SequenceLoops in one call
    pub fn sequence(&self, model: &str, label: &str) -> Lookup;
}

/// The read-only view a `filter_*` class evaluates against.
pub struct Filters<'a> { /* private */ }
impl Filters<'_> {
    pub fn passes(&self, filter: EntityId, caller: &EntityCore, other: &EntityCore) -> bool;
    pub fn find(&self, name: &str) -> Option<EntityId>;  // …and it must BE a filter
}

pub struct PointEntity;   // CPointEntity — no state, no behaviour
```

All seventeen trait methods have defaults, so a class with no state is
`impl Behaviour for Thing {}`. `move_done` and `use_entity` are `m_pfnMoveDone`
and `m_pfnUse`, the two function pointers `CBaseEntity` dispatches through: a
class that has one keeps its own enum in place of the pointer and matches on
it, because a mover re-points `SetMoveDone` at each step of its cycle. The same
is true of `start_touch`/`touch`/`end_touch` (`m_pfnTouch` and the two
virtuals) — `CTriggerMultiple` really does `SetTouch( NULL )` to stop itself
firing twice.

**`Context` grew the entity list at stage 4, in exactly the shape stage 2
predicted.** `portdocs/SERVER.md` §7.2 expected it to carry the list so a
handler could "fire an output, find by name, remove an entity, trace", and
§10.3 flagged the borrow shape as the module's biggest risk. Stages 1-3 needed
none of it, because **the C++ is not re-entrant either**: `FireOutput` appends
to the queue rather than calling the target, and the queue is drained by one
top-level loop.

Stage 4 is the condition stage 2 named — "a handler that must *read* another
entity during dispatch" — and it arrived three times over: a trigger asks its
`filter_*` entity whether the toucher passes, a `trigger_push` writes the
toucher's base velocity, and a `point_teleport` moves whatever `!player`
resolves to. The answer stage 2 wrote down was "the entity list minus the one
entity being dispatched, **not** a `RefCell`", and that is literally what
shipped: `Server::dispatch` [detaches](#entityid-and-entitylist-entityrs) the
entity it is about to run, so the rest of the list is free to be borrowed.
There is still no cell and no `unsafe`.

> **The one rule that follows: `cx.entity(self.id())` is `None` inside your own
> handler.** You already hold `&mut EntityCore`; asking the list for yourself
> would be asking for it twice, which is the bug this prevents rather than a
> limitation it imposes.

**`create_entity` is `CreateEntityByName`, and its `Spawn` is deferred by one
dispatch.** In the C++ a creator calls `DispatchSpawn( pEnt )` itself, part-way
through its own `Spawn` — plain re-entrancy, which this module does not have,
because `Server::dispatch` has lifted the *creator* out of the entity list.
So a created entity is **queued**, exactly the way `EntityCore::remove` queues
a deletion, and the server spawns it the moment the current handler returns
(`Server::flush_created`, which is a loop with a re-entrancy guard rather than
a recursion). Everything a creator does between `CreateEntityByName` and
`DispatchSpawn` — the origin, the angles, the size, the owner — happens before
the `Spawn` either way, which is the order that matters.

`behaviour_mut` is the other half of building one: `entity_mut` reaches the
shared `EntityCore` and this reaches the class's own state, which is
`pTrigger->m_pOwnerButton = pOwner`. It deliberately does **not** hand out
`&mut dyn Behaviour` — calling into another class from inside a class is the
re-entrancy `create_entity` exists to avoid.

**Activation follows `ServerActivate`, which walks the entity list rather than
the spawn list** (`gameinterface.cpp:1316`): an entity created during
`level_init`'s spawn pass *is* activated, and one created after the level has
loaded gets a `Spawn` and nothing else. Both are Valve's.

### `Variant` and `FieldType` (`io.rs`)

```rust
pub enum Variant { Void, Bool(bool), Int(i32), Float(f32), String(String),
                   Vector(Vec3), Color32([u8; 4]) }
pub enum FieldType { Void, Bool, Int, Float, String, Vector, Color32, Input }

impl Variant {
    pub fn field_type(&self) -> FieldType;
    pub fn convert(&mut self, to: FieldType) -> bool;   // variant_t::Convert
    pub fn float(&self) -> f32;     // 0.0 unless it IS a float — gotcha 5
    pub fn int(&self) -> i32;
    pub fn bool(&self) -> bool;
    pub fn to_string(&self) -> String;                  // printf %g for floats
}
```

### `EventAction`, `Output`, `Event` and `EventQueue` (`io.rs`)

```rust
pub const EVENT_FIRE_ALWAYS: i32 = -1;

pub struct EventAction {
    pub target: String, pub input: String, pub parameter: Option<String>,
    pub delay: f32, pub times_to_fire: i32, pub id: u32,
}
impl EventAction { pub fn parse(value: &str, id: u32) -> EventAction; }

pub struct Output { pub name: String, pub actions: Vec<EventAction> }
impl Output {
    pub fn new(name: &str) -> Output;
    pub fn add(&mut self, action: EventAction);   // PREPENDS — gotcha 3
    pub fn max_delay(&self) -> f32;
    pub fn len(&self) -> usize;
}

pub enum Target { Name(String), Entity(EntityId) }

pub struct Event {
    pub fire_time: f32, pub target: Target, pub input: String,
    pub value: Variant, pub activator: Option<EntityId>,
    pub caller: Option<EntityId>, pub output_id: u32,
}

pub struct EventQueue { /* private */ }
impl EventQueue {
    pub fn new() -> EventQueue;
    pub fn add(&mut self, event: Event);              // stable, sorted by time
    pub fn pop_due(&mut self, now: f32) -> Option<Event>;
    pub fn cancel_from(&mut self, caller: EntityId) -> usize;
    pub fn cancel_on(&mut self, target: EntityId, input: &str) -> usize;
    pub fn has_pending(&self, target: EntityId, input: Option<&str>) -> bool;
    pub fn clear(&mut self);
    pub fn len(&self) -> usize;
    pub fn iter(&self) -> impl Iterator<Item = &Event>;
    pub fn retain_targets(&mut self, alive: impl Fn(EntityId) -> bool);
}

pub struct Input<'a> {
    pub name: &'a str, pub value: Variant,
    pub activator: Option<EntityId>, pub caller: Option<EntityId>,
    pub output_id: u32,
}
```

### `IoStats` (`io.rs`)

```rust
pub struct IoStats {
    pub dispatched: usize,     // events taken off the queue
    pub accepted: usize,       // inputs a class took
    pub no_target: usize,      // events whose target resolved to nothing
    pub unhandled: BTreeMap<String, usize>,   // "classname.Input" -> count
    pub bad_conversion: usize,
    pub thinks: usize,
}
```

The run-side progress metric, the way `LevelStats` is the parse-side one. Two
seconds of every shipped map is 5,766 events dispatched, 2,480 inputs accepted,
1,450 thinks, 2,548 events that reached nothing and **zero** bad conversions.

### `Time`, `ServerClock` and `ThinkList` (`think.rs`)

```rust
pub const TICK_NEVER_THINK: i32 = -1;
pub const DEFAULT_TICK_INTERVAL: f32 = 1.0 / 64.0;

pub struct Time { pub curtime: f32, pub tick: i32, pub interval: f32 }
impl Time {
    pub fn time_to_ticks(&self, time: f32) -> i32;   // rounds to NEAREST
    pub fn ticks_to_time(&self, tick: i32) -> f32;
}

pub struct ServerClock { /* private */ }
impl ServerClock {
    pub fn new(interval: f32) -> ServerClock;
    pub fn interval_from_tickrate(tickrate: Option<f32>) -> f32;   // -tickrate
    pub fn time(&self) -> Time;
    pub fn accumulate(&mut self, frame_time: f32) -> u32;
    pub fn advance(&mut self);
    pub fn reset(&mut self);
}

pub struct ThinkList { /* private */ }
impl ThinkList {
    pub fn new() -> ThinkList;
    pub fn entity_changed(&mut self, id: EntityId, next_think_tick: i32, removed: bool);
    pub fn due(&self, tick: i32, out: &mut Vec<EntityId>);
    pub fn retain_alive(&mut self, alive: impl Fn(EntityId) -> bool);
    pub fn len(&self) -> usize;
    pub fn clear(&mut self);
}
```

### `sequences` (`sequences.rs`)

```rust
pub struct SequenceInfo { pub duration: f32, pub loops: bool, pub fade_out_time: f32 }
impl SequenceInfo {
    pub fn cycle_rate(&self) -> f32;                       // GetSequenceCycleRate
    pub fn last_visible_cycle(&self, playback_rate: f32) -> f32;  // GetLastVisibleCycle
}
pub enum Lookup { Unknown, Missing, Found(SequenceInfo) }

pub struct SequenceTable { /* private */ }
impl SequenceTable {
    pub fn new() -> SequenceTable;
    pub fn insert_model(&mut self, model: &str,
                        sequences: impl IntoIterator<Item = (String, SequenceInfo)>);
    pub fn lookup(&self, model: &str, label: &str) -> Lookup;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}

// Filled in once by the engine, after level_init:
impl Server { pub fn set_sequences(&mut self, sequences: SequenceTable); }
// Read by a class during a dispatch:
impl Context<'_> { pub fn sequence(&self, model: &str, label: &str) -> Lookup; }
```

**`LookupSequence` + `SequenceDuration` + `SequenceLoops`, answered in
advance.** `server/` names no `studio` type, so what a `.mdl` says about its
animation is *copied in* rather than called for — the same shape `world/`
already has in the other direction, where a `Placement` is the answer to "where
is this brush model". Both keys fold case and slashes.

Two classes read it — `DynamicProp` (in `AnimThink` and `SetPlaybackRate`) and
`TestChamberDoor` (in `StudioFrameAdvance` and every rate change) — and
`Engine::load_level` fills it, from the models `World::load_entity_models` has
just uploaded.

`fade_out_time` is `mstudioseqdesc_t::fadeouttime`, and it is here for exactly
one reader: `GetLastVisibleCycle`, which is what `IsSequenceFinished()` is made
of. **A non-looping sequence counts as finished `fade_out_time` seconds before
it ends** — so a chamber door's 0.9167-second `open` is "finished" at cycle
0.782, 0.717 seconds in. It is 0.2 for 10,664 of the shipped game's 10,666
sequences and 0.5 for the other two; **none is zero**, so the term never folds
away. `cycle_rate` is `1/duration` with Valve's `1/0.1` guard for a zero-length
sequence.

> **Three answers, not two, and every `Spawn` in the game sees the third.** A
> level loads `World::load` → `Server::level_init` →
> `World::load_entity_models`, and it cannot load in any other order: the models
> an entity places are named by the entities. So the table is **empty** while
> every `Spawn` runs, and `Lookup::Unknown` is what "nobody has loaded this
> model" has to be told apart from `Lookup::Missing`, "it is loaded and has no
> such sequence". The rule a caller follows is: `Unknown` succeeds where
> `LookupSequence` would have, and has no duration — so an animation whose
> model was never loaded never finishes, which is what an entity with nothing
> on screen should do.

### `RandomStream` (`random.rs`)

```rust
pub struct RandomStream { /* private */ }
impl RandomStream {
    pub fn new(seed: i32) -> RandomStream;
    pub fn set_seed(&mut self, seed: i32);
    pub fn next(&mut self) -> i32;
    pub fn float(&mut self, low: f32, high: f32) -> f32;   // [low, high)
    pub fn int(&mut self, low: i32, high: i32) -> i32;     // [low, high] INCLUSIVE
}
```

`CUniformRandomStream` — *Numerical Recipes*' `ran1`. Ported rather than swapped
for a crate because two pieces of its behaviour are load-bearing and would be
silently lost: `int` **rejects rather than taking a modulus**, and the seed
convention is inverted and lossy (gotcha 17).

### `damage` (`damage.rs`)

```rust
// The DMG_* table (shareddefs.h:455) — all thirty, because map data writes
// the numbers and `damage_type_string` names every one.
pub const DMG_GENERIC: i32 = 0;
pub const DMG_CRUSH: i32 = 1 << 0;      // 71 trigger_hurts — the commonest
pub const DMG_FALL: i32 = 1 << 5;       // 34 — and nothing here ever GENERATES it
pub const DMG_RADIATION: i32 = 1 << 18; // 27 — the goo, and the one bit a class
                                        //      branches on
pub const DMG_DIRECT: i32 = 1 << 28;    // the bit FilterDamageType masks off
/* …and the other twenty-five */

pub enum DamageMode { No, EventsOnly, Yes }      // m_takedamage
impl DamageMode { pub fn takes_damage(self) -> bool; }

pub enum LifeState { Alive, Dying, Dead }        // m_lifeState — three of Valve's four

pub struct DamageInfo {                           // CTakeDamageInfo, 4 of 16 fields
    pub inflictor: Option<EntityId>,
    pub attacker: Option<EntityId>,
    pub damage: f32,
    pub damage_type: i32,
}
impl DamageInfo {
    pub fn new(
        inflictor: Option<EntityId>, attacker: Option<EntityId>,
        damage: f32, damage_type: i32,
    ) -> DamageInfo;
    pub fn scale(&mut self, factor: f32);
}

pub enum Damaged { Refused, Survived, Killed }

pub fn take_damage(
    mode: DamageMode, health: &mut i32, accumulator: &mut f32, info: &DamageInfo,
) -> Damaged;
pub fn take_health(
    mode: DamageMode, health: &mut i32, max_health: i32, amount: f32,
) -> i32;
pub fn damage_type_string(bits: i32) -> String;
```

`CTakeDamageInfo` and the `TakeDamage` → `OnTakeDamage` → `Event_Killed`
ladder. **Valve's ladder is five overrides deep for a player** —
`CPortal_Player::OnTakeDamage` → `CBasePlayer::OnTakeDamage` →
`CBaseCombatCharacter::OnTakeDamage` → `CPortal_Player::OnTakeDamage_Alive` →
`CBaseCombatCharacter::OnTakeDamage_Alive` → `CBaseEntity::OnTakeDamage` — and
the `_Alive`/`_Dying`/`_Dead` split exists only so that
`CBaseCombatCharacter` can dispatch on `m_lifeState` in one place. There is no
inheritance here, so [`Behaviour::on_take_damage`] is **one** method and
`take_damage` is the shared arithmetic it calls.

[`Behaviour::on_take_damage`]: #classdef-behaviour-and-context-classrs

**Damage is deferred by one dispatch.** `Context::take_damage` queues, exactly
the way `Context::create_entity` queues a spawn and `EntityCore::remove` queues
a deletion, and `Server::dispatch` applies it the moment the current handler
returns — because applying damage means running the *victim's* virtuals and the
caller has been lifted out of the entity list. It costs no tick, and the two
gates a caller branches on (`m_takedamage` and `PassesDamageFilter`) are
checked synchronously before the queue, so `CTriggerHurt::HurtEntity` still
returns the right answer to `HurtAllTouchers`.

**There is exactly one damage source in the port, and it is the one the maps
use.** 215 `trigger_hurt`s across 66 of the 106 maps, dealing between 10 and
1,000,000 points a second against 100 health — 138 of them kill a player who
stands in one. Everything else in Portal 2 that can hurt you is a class this
port has not got (turrets, crushers, `prop_physics`), and the game's *other*
way of dying takes no health at all: `player_loadsaved` freezes the player,
fades the screen and reloads.

### `movement` (`movement.rs`)

```rust
/// MOVETYPE_NONE / PUSH / WALK / NOCLIP. The last two are the player's and the
/// server does not run them — `client/` does, on the rendered frame.
pub enum MoveType { None, Push, Walk, Noclip }
/// SOLID_NONE / BSP / BBOX / OBB / VPHYSICS — *how* an entity is solid.
pub enum Solid { None, Bsp, Bbox, Obb, VPhysics }

pub const EF_NODRAW: u32 = 0x020;
pub const FSOLID_NOT_SOLID: u32 = 0x0004;
pub const FSOLID_TRIGGER: u32 = 0x0008;
pub const FSOLID_VOLUME_CONTENTS: u32 = 0x0020;
pub const FL_ONGROUND: u32 = 1 << 0;
pub const FL_CLIENT: u32 = 1 << 8;
pub const FL_BASEVELOCITY: u32 = 1 << 24;
pub const SF_DOOR_ROTATE_ROLL: u32 = 64;
pub const SF_DOOR_ROTATE_PITCH: u32 = 128;

/// The `"*N"` brush model's box, put on the entity by `level_init`.
pub struct ModelBounds { pub mins: Vec3, pub maxs: Vec3 }

pub enum ToggleState { AtTop, AtBottom, GoingUp, GoingDown }

/// `CBaseToggle` — held, not inherited, by Door / Button / MoveLinear.
pub struct Toggle {
    pub state: ToggleState,
    pub wait: f32, pub lip: f32, pub move_distance: f32,
    pub position1: Vec3, pub position2: Vec3,
    pub angle1: Vec3, pub angle2: Vec3,
    pub move_ang: Vec3,
    /* private: final_dest, final_angle, movement */
}

impl Toggle {
    pub fn key_value(&mut self, key: &str, value: &str) -> bool;   // lip/wait/distance
    #[must_use] pub fn linear_move(&mut self, &mut EntityCore, dest: Vec3, speed: f32) -> bool;
    #[must_use] pub fn angular_move(&mut self, &mut EntityCore, dest: Vec3, speed: f32) -> bool;
    pub fn move_done(&mut self, entity: &mut EntityCore);          // snap, stop, disarm
    pub fn axis_dir(&mut self, spawn_flags: u32);
    pub fn final_dest(&self) -> Vec3;
    pub fn set_final_dest(&mut self, dest: Vec3);
}

/// `Physics_SimulateEntity` — think, then push. One entity, one tick.
pub fn simulate(&mut EntityCore, &mut dyn Behaviour, &mut Context<'_>);

pub fn anglemod(a: f32) -> f32;                  // the 16-bit fixed-point fold
pub fn dot_product_abs(a: Vec3, b: Vec3) -> f32; // NOT |a·b|
pub fn move_dir(angles: Vec3) -> Vec3;           // the `movedir` key
```

**A mover is four lines and an alarm.** `linear_move` sets a velocity and an
arrival time; `simulate` integrates the velocity once a tick and, when the
arrival time comes round, calls [`Behaviour::move_done`], which snaps the entity
onto its destination and runs the class's own callback. `angular_move` is the
same four lines on angles.

The two `_move` functions are `#[must_use]` and the `false` they return is not
an error: it means *the destination is where we already are*, and the caller
must run `Behaviour::move_done` **itself, before firing any output** — see
gotcha 7.

**Nothing is pushed out of the way.** `CPhysicsPushedEntities`
(`physics_main.cpp:130-1130`) is ~1,000 lines of speculative push, blocker
enumeration and rollback, and it is deliberately absent: a door moves *through*
the player. `EntityCore::local_time` is where a future rollback would put its
answer, and it is real and correct today — it just never goes backwards.

### `obb` (`obb.rs`)

```rust
pub fn swept_box_touches_obb(
    start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3,     // the toucher's hull
    origin: Vec3, angles: Vec3,                          // where the box is
    obb_mins: Vec3, obb_maxs: Vec3,                      // …and how big
) -> bool;
```

`IntersectRayWithBox` and `IntersectRayWithOBB` (`public/collisionutils.cpp`
`:1131`, `:1477` and `:1685`) — what `CEngineTrace::ClipRayToOBB` runs for a
`SOLID_OBB` entity.

**Why it is here and not in `engine::trace`.** Every other collision question
this port asks is about data the *engine* owns, so it goes out through
[`TouchQuery`](#touchquery-mod-rs) and comes back as a `"*N"` index. A
`SOLID_OBB` trigger has no map data in it at all: it is a box the game invented,
at a placement the game chose, in the game's own `ModelBounds`. There is nothing
to ask the engine about — and `collisionutils.cpp` lives in `public/` and is
compiled into both game DLLs as well as the engine, so this is where Valve keeps
it too.

Two paths, and which one runs is decided by an **exact** comparison against zero
angles: an unrotated box takes an axis-aligned slab clip, and a turned one takes
a fifteen-plane separating-axis sweep (the OBB's three faces, the three world
axes, and the nine cross products, each bloated by the swept box's extent along
it). 42 of the game's 65 `prop_floor_button`s are at `angles "0 0 0"` and take
the first; 23 are not, including the one on `sp_a1_intro1`.

It returns one `bool` because its one caller reads one thing —
`if ( !(tr.contents & MASK_SOLID) ) continue;` — and every branch that hits sets
that and every branch that misses does not. The fraction, the end position and
the plane are computed and thrown away in the original; they are not computed
here. Something that wants to *stop* against an OBB rather than notice one is
what makes this grow a `Trace`.

### `touch` (`touch.rs`)

```rust
/// One entry of an entity's `TOUCHLINK` list.
pub struct TouchLink { pub other: EntityId, pub stamp: i32, pub start_touch: bool }

/// Everything `entity` is currently touching, as handles.
pub fn touching(entity: &EntityCore) -> impl Iterator<Item = EntityId> + '_;

/// `CBaseEntity::Teleport( &origin, &angles, &velocity )` — any of the three.
pub struct Teleport {
    pub origin: Option<Vec3>, pub angles: Option<Vec3>, pub velocity: Option<Vec3>,
}
impl Teleport { pub fn apply(&self, entity: &mut EntityCore); }

/* pub(super) on Server: set_check_untouch, mark_entities_as_touching,
   remove_touched_list, check_for_entity_untouch */
```

`touchlink_t` and the four `CBaseEntity::Physics*Touch*` functions. **Every
function here takes `&mut Server`**, because a touch is a fact about *two*
entities: both carry a link and exactly one of the two carries the flag that
will fire an `EndTouch`. What a class gets is the three callbacks and its own
`EntityCore::touch_links` to read.

**The stamp is the mechanism, not a geometric test.** Nothing ever asks "have
these two stopped overlapping":

1. Before a toucher re-tests what it is in, `SetCheckUntouch` bumps its
   `touch_stamp` and puts it on the sweep list.
2. Every link the test confirms is written with the *new* stamp.
3. After the thinks, `check_for_entity_untouch` walks the sweep list and every
   link still carrying an old stamp is a touch that has ended.

So an `EndTouch` costs nothing to detect — and a trigger that is switched off
simply stops being reported by the query, and everything inside it leaves on
the next tick.

### The brush-entity seam

Stage 3's other half. `world/` draws a brush entity and `trace/` collides with
it, and since stage 3 the placement they share comes from the *entity*:

```text
Engine::frame
  server.frame( dt )                    ticks; a door integrates its velocity
  sync_brush_models( world, server )    engine/mod.rs, the joining layer
    world.sync_brush_models(|index| …)  world/, asks by "*N" index
      server.brush_entity(index)        server/, answers with an &EntityCore
        BrushModel::set_placement(…)    trace/, the one transform both read
  update_client( … )                    the player is traced against it
…
Engine::render                          …and the renderer draws the same thing
```

Three things about it are worth knowing.

- **The model index is the key, and it is unique.** Across all 106 shipped maps
  there are 11,635 `(map, "*N")` pairs and **not one** is claimed by two
  entities, so nothing has to carry a lump index around. `brush_entity` binary
  searches a list built once at `level_init`.
- **`None` means "leave it where the lump put it"**, and that is the answer for
  8,225 of the game's 11,635 brush entities, whose classnames this port has not
  got.
- **Neither module names the other.** `world/` defines its own `Placement`
  value and `engine/mod.rs` converts, the same arrangement `console/` and
  `input/` already have.

### `name` (`name.rs`)

```rust
pub fn names_match(query: &str, name: &str) -> bool;
pub fn is_procedural(name: &str) -> bool;                     // starts with '!'
pub fn find_by_name<'a>(list: &'a EntityList, query: &'a str)
    -> impl Iterator<Item = EntityId> + 'a;

pub enum Procedural { Resolved(Option<EntityId>), NeedsPlayer, Unknown }
pub fn find_procedural(
    name: &str,
    searching: Option<EntityId>,
    activator: Option<EntityId>,
    caller: Option<EntityId>,
) -> Procedural;
```

### `classes` (`classes/`)

```rust
pub fn lookup(classname: &str) -> Option<&'static ClassDef>;   // case-insensitive
pub(super) static CLASSES: &[ClassDef];

// classes/world.rs
pub struct World { pub sky_name, world_mins, world_maxs, max_prop_screen_width,
                   max_blob_count, detail_material }
// classes/light.rs
pub struct Light { pub style: i32, pub pattern: Option<String> }
pub struct EnvLight { /* holds a Light */ pub sun_color: [u8; 4] }
// classes/logic.rs
pub struct Relay { pub disabled: bool, pub wait_for_refire: bool }
pub struct Auto { pub global_state: Option<String> }
pub struct Branch { pub value: bool /* + its logic_branch_listeners */ }
pub struct BranchList { /* private; logic_branch_listener */ }
impl BranchList {
    pub fn branches(&self) -> &[EntityId];  // resolved in Activate
    pub fn state(&self) -> &'static str;    // "not-init" | "all-true" | …
}
pub struct Case { /* private */ }
pub struct Timer { pub disabled, refire_time, use_random_time,
                   lower_random_bound, upper_random_bound }
pub struct MathCounter { pub value, min, max, disabled }
pub struct InstanceIoProxy;
// classes/env.rs
pub struct TonemapController { /* private */ }
impl TonemapController {
    pub fn settings(&self) -> TonemapSettings;
    pub fn is_master(entity: &EntityCore) -> bool;
}
// classes/brush.rs — stage 3, and the first classes that move
pub struct Door { /* private; func_door and func_door_rotating */ }
pub struct MoveLinear { /* private */ }
pub struct Button { /* private */ }
pub struct Rotating { /* private */ }
pub struct Brush { /* private; func_brush */ }
// classes/trigger.rs — stage 4, and the first classes that notice the player
pub struct BaseTrigger { /* private; held by all five, and by ButtonTrigger */ }
/// What one `BaseTrigger::start_touch` did — `passed` is the filters,
/// `all` is the `OnStartTouchAll` virtual. `end_touch` returns the `all` half.
pub struct Touched { pub passed: bool, pub all: bool }
pub struct TriggerMultiple { /* private; trigger_multiple and trigger_once */ }
pub struct TriggerHurt { /* private */ }
pub struct TriggerPush { /* private */ }
pub struct TriggerTeleport { /* private */ }
// classes/filter.rs
pub struct BaseFilter { pub negated: bool }   // held by all six
pub struct FilterName / FilterClass / FilterModel { /* private */ }
pub struct FilterMulti / FilterPlayerHeld / FilterDamageType { /* private */ }
// classes/point.rs
pub struct PointTeleport { /* private */ }
// classes/player.rs
pub struct Player;    // stateless; see the file for why
// classes/prop.rs — the models the game places, and the one you stand on
pub struct DynamicProp { /* private; all four prop_dynamic classnames */ }
pub struct FloorButton { pub pressed: bool, pub skin: i32 }
pub struct ButtonTrigger { /* private; the SOLID_OBB box over a pad */ }
/// The chamber door. A `CBaseAnimating`, not a prop — see the note below.
pub struct TestChamberDoor { /* private */ }
impl TestChamberDoor {
    pub fn is_open(&self) -> bool;       // m_bIsOpen: where it is *going*
    pub fn is_animating(&self) -> bool;  // m_bIsAnimating: a `fully` is owed
    pub fn is_locked(&self) -> bool;     // m_bIsLocked
}
// classes/player.rs — stage 5's two
pub struct LogicPlayerProxy;
pub struct RevertSaved { /* private; player_loadsaved */ }
// classes/portal.rs — `portdocs/PORTAL.md` stage 2
pub struct PropPortal {
    pub activated, old_activated, is_portal2: bool,
    pub linkage_group: u8,                  // 255 is LINKAGE_GROUP_INVALID
    pub half_width, half_height: f32,       // 32 and 56
    pub linked: Option<EntityId>,
    pub matrix: Mat4,                       // m_matrixThisToLinked; identity while unlinked
    pub opened_at: f32,
}
impl PropPortal {
    pub fn forward(entity: &EntityCore) -> Vec3;   // +X of the angle matrix
    pub fn right(entity: &EntityCore) -> Vec3;     // NEGATED column 1 — gotcha 81
    pub fn up(entity: &EntityCore) -> Vec3;
    pub fn plane(entity: &EntityCore) -> (Vec3, f32);        // m_plane_Origin
    pub fn corners(&self, entity: &EntityCore) -> [Vec3; 4]; // UpdateCorners
    pub fn is_floor_portal(entity: &EntityCore, threshold: f32) -> bool;
    pub fn is_active_and_linked(&self) -> bool;
    pub fn new_location(&mut self, entity, origin, angles, cx);
}
/// `UTIL_Portal_ComputeMatrix_ForReal` — see gotcha 80 before using it.
/// Both arguments are `(origin, angles)`.
pub fn teleport_matrix(entrance: (Vec3, Vec3), exit: (Vec3, Vec3)) -> Mat4;
```

Forty-six classnames, **34,823 of the shipped game's 60,925 entity blocks**.
**Forty-one of them are among the 200 classnames the maps place**; the other
five are `player` (the engine makes it when a client connects),
`trigger_portal_button` (a `prop_floor_button` makes it in its own `Spawn`), and
`light_glspot`, `dynamic_prop` and `prop_dynamic_glow`, which are registered
because Valve registers them:

| classname | C++ | instances |
|---|---|---:|
| `logic_relay` | `CLogicRelay` | 8,082 |
| `light`, `light_spot`, `light_directional`, `light_glspot` | `CLight` | 7,125 |
| `func_instance_io_proxy` | `CFuncInstanceIoProxy` | 1,184 |
| `logic_auto` | `CLogicAuto` | 1,112 |
| `logic_branch` | `CLogicBranch` | 601 |
| `logic_branch_listener` | `CLogicBranchList` | 158 |
| `info_target` | `CInfoTarget` | 431 |
| `logic_timer` | `CTimerEntity` | 151 |
| `info_player_start` | `CPointEntity` | 116 |
| `env_tonemap_controller` | `CEnvTonemapController` | 110 |
| `worldspawn` | `CWorld` | 106 |
| `math_counter` | `CMathCounter` | 102 |
| `logic_case` | `CLogicCase` | 84 |
| `light_environment` | `CEnvLight` | 25 |
| `func_brush` | `CFuncBrush` | 2,502 |
| `func_door_rotating` | `CRotDoor` | 346 |
| `func_door` | `CBaseDoor` | 275 |
| `func_movelinear` | `CFuncMoveLinear` | 196 |
| `func_button` | `CBaseButton` | 64 |
| `func_rotating` | `CFuncRotating` | 27 |
| `trigger_once` | `CTriggerOnce` | 1,476 |
| `trigger_multiple` | `CTriggerMultiple` | 899 |
| `trigger_hurt` | `CTriggerHurt` | 215 |
| `filter_activator_class` | `CFilterClass` | 212 |
| `trigger_push` | `CTriggerPush` | 192 |
| `point_teleport` | `CPointTeleport` | 128 |
| `trigger_teleport` | `CTriggerTeleport` | 110 |
| `filter_activator_name` | `CFilterName` | 74 |
| `filter_multi` | `CFilterMultiple` | 9 |
| `filter_player_held` | `CFilterPlayerHeld` | 4 |
| `filter_damage_type` | `FilterDamageType` | 2 |
| `filter_activator_model` | `CFilterModel` | 1 |
| `prop_dynamic` | `CDynamicProp` | 8,072, in 105 maps |
| `prop_dynamic_override` | `CDynamicProp` | 390 |
| `dynamic_prop`, `prop_dynamic_glow` | `CDynamicProp` | **0 placed** |
| `logic_playerproxy` | `CLogicPlayerProxy` | 9 |
| `player_loadsaved` | `CRevertSaved` | 9 |
| `prop_floor_button` | `CPropFloorButton` | 65, in 47 maps |
| `prop_testchamber_door` | `CPropTestChamberDoor` | 138, in 71 maps |
| `trigger_portal_button` | `CPortalButtonTrigger` | **0 placed** — one per button, 65 |
| `prop_portal` | `CProp_Portal` | 21, in 10 maps |
| `player` | `CPortal_Player` | **0 placed** — `spawn_player` makes it |

---

## Cross-cutting semantics

### The order a key is offered to three things

`parse_map_data` walks the block in **lump order** and for each key tries, in
this order:

1. the class's `Behaviour::key_value`,
2. `keyvalue::base_key_value` — `CBaseEntity::KeyValue`'s if-ladder,
3. the output table — `ClassDef::outputs` plus `keyvalue::BASE_OUTPUTS`.

Anything left over lands in `EntityCore::unhandled`. The order is Valve's:
`ParseMapData` calls `KeyValue` *virtually*, so a class that overrides it tests
its own keys and only then calls `BaseClass::KeyValue`, where the ladder lives.

### The order an input is offered to two things

`Server::accept_input` looks the name up in `ClassDef::inputs` first and
`BASE_INPUTS` second, converts the value to whichever type it found, and
dispatches to `Behaviour::accept_input` or `class::base_accept_input`
accordingly. `base_accept_input` is handed the behaviour as well as the entity,
because one of `CBaseEntity`'s six inputs — `Use` — dispatches back into the
class through `Behaviour::use_entity`. That is the whole of what `AcceptInput`'s `baseMap` chain walk
reduces to, because the only thing every class shares is `CBaseEntity`.

### The spawn pipeline

`worldspawn` is spawned immediately, outside the sorted list, and is forcibly
unparented (`mapentities.cpp:373`). Everything else is queued, then:

1. **depth** — follow `parentname` to the root, counting.
2. **sort** — depth ascending, then `SPAWN_PRIORITY` descending, then lump order.
3. **parents** — resolve `parentname` to an `EntityId`.
4. **spawn** — every entity, in that order; `SpawnResult::Remove` marks it.
5. **activate** — every survivor, after every spawn.
6. **cleanup** — free what step 4 marked.
7. **`LevelInitPostEntity`** — pick the master `env_tonemap_controller`.

### The tick

```text
Engine::frame -> Server::frame( frame_time, query )
                   ServerClock::accumulate -> 0..n fixed ticks
                   for each tick:
                     CleanupDeleteList         anything removed outside the loop
                     CheckMovingGround         a push that stopped becomes momentum
                     PhysicsTouchTriggers      the player's sweep -> StartTouch/Touch
                                               (brush models AND SOLID_OBB boxes)
                     Physics_RunThinkFunctions think, then push, in entity order
                     FrameUpdatePostEntityThink the stale-link sweep -> EndTouch
                     ServiceEventQueue         everything due, restart-from-head
                     CleanupDeleteList         anything a think removed
```

`CServerGameDLL::GameFrame` (`gameinterface.cpp:1383`) with the CS:GO, Steam,
nav-mesh and benchmarking steps removed. **The order is observable and maps
depend on all three of these:**

- an output fired during a think is dispatched later in the *same* tick, but an
  input handler cannot see a think that has not run yet;
- **the player's touch test runs before the thinks**, because in the original it
  is part of `CBasePlayer::PhysicsSimulate` and the player is entity index 1 —
  so a `trigger_multiple`'s `OnTrigger` is queued *before* the same tick's
  thinks rather than after them;
- **`EndTouch` is detected between the thinks and the queue**, so an
  `OnEndTouch` is delivered in the tick it happened rather than the next one.

The middle step is `Physics_SimulateEntity` for every entity in the simulation
list, which is [`movement::simulate`]: run the think if it is due, then, for a
`MOVETYPE_PUSH` entity, advance its local clock by `min(time left, one tick)`,
integrate its two velocities, and fire the arrival alarm if it has come round.
**The last step of a move is exactly as long as the travel that is left**, so a
door arrives on the tick it was scheduled for rather than the tick after.

### The simulation list holds movers as well as thinkers

`ThinkList` is `CSimThinkManager`, and stage 3 is when the *Sim* half of that
name started meaning something. An entity is in it when it will think **or**
when `EntityCore::will_simulate_game_physics()` — a `MOVETYPE_PUSH` entity with
an arrival alarm in the future. A mover is stored with a tick of **zero**, so it
is copied out every tick whatever its schedule says and decides for itself
whether its think is due; a think-only entity is stored with its real tick and
filtered. That split is Valve's (`entitylist.cpp:249`) and it is what lets one
list answer two different questions.

Measured over the depot: the peak is **48 entities at once across all 106
maps**. Stage 2 measured 43 and stage 3 did not move it — a mover is only in
the list while it is actually travelling, and a Portal 2 map starts with its
doors shut. Stage 4's five come from triggers: a `trigger_multiple` holds a
think for the whole of its `wait`, and a `trigger_once` for the tenth of a
second before it deletes itself.

### Inheritance is composition

There is no `parent` pointer on `ClassDef` and no datadesc chain walk. `CEnvLight
: public CLight` is an `EnvLight` that *holds* a `Light` and ends its `key_value`
with `self.light.key_value(..)`. See gotcha 13.

---

## Invariants and gotchas

Ordered by how likely each is to bite. **1-25 are stages 1 and 2; 26-34 are
stage 3's and are about movement; 35-46 are stage 4's and are about touch, the
player, and solidity; 47-51 came with `prop_floor_button` and are about
entities that make other entities; 52-59 are stage 5's and are about damage,
death and the move type** — if a door is in the wrong place or at the wrong
time start at 26, if a trigger does not fire start at 35, if something an
entity built is not there start at 47, and if something will not die start at
52.

1. **The server's `curtime` is not `Scene::curtime`.** The server's is
   `tick * interval` and moves in steps of 1/64 s; the scene's is the
   accumulated wall clock and moves smoothly. An entity that wants "now" wants
   `Context::curtime`, which comes off `Server::time()`. Reading the scene's
   would put a think schedule on a variable `dt`, which is the thing the fixed
   tick exists to prevent.

2. **`SetNextThink` quantises to a tick, rounded to nearest**, and a think tick
   that is not *greater than zero and at most `tickcount`* never runs. So
   `set_next_think(curtime + 0.01)` is next tick at 64 Hz and **never** at
   30 Hz. `logic_auto`'s 0.2-second bootstrap and `logic_relay`'s 0.01-second
   `OnSpawn` both live on this edge, and every map in the game starts through
   the first of them.

3. **An output's connections fire in *reverse* lump order.**
   `CBaseEntityOutput::AddEventAction` prepends
   (`pEventAction->m_pNext = m_ActionList`), and `ParseKeyvalue` feeds it the
   output keys in lump order. `Output::add` inserts at the front to match. Two
   connections on one output that reach the same target are delivered
   last-written-first.

4. **The event queue restarts from the head after every event.** A chain of
   eight zero-delay `logic_relay`s completes in **one tick**, not eight. Get
   this wrong and every map in the game runs its logic in slow motion.
   `EventQueue::pop_due` is the same thing as Valve's restart loop, because the
   queue is sorted and `add` is stable for equal fire times.

5. **`Variant`'s accessors return zero unless the value already is that type.**
   `variant_t::Float()` is `(fieldType == FIELD_FLOAT) ? flVal : 0`, and this
   port reproduces it. A handler reads `input.value.float()` safely **only
   because `Server::accept_input` has already converted against the type
   `ClassDef::inputs` declared**. Declaring the wrong type there is a silently
   wrong value, not a compile error — which is why
   `every_declared_input_is_handled_and_every_handled_input_is_declared` exists.

6. **The think schedule is cleared *before* the think runs**
   (`PhysicsRunSpecificThink`), so a think that does not reschedule itself never
   runs again. Every recurring behaviour in the game re-arms on the way out.

7. **A per-action parameter override silently discards the caller's extra
   delay.** `FireOutput`'s no-override branch posts at `ev->m_flDelay + fDelay`
   and its override branch at `ev->m_flDelay` alone (`cbase.cpp:280` against
   `:289`). Valve's, almost certainly a bug, reproduced deliberately.

8. **An input with an empty name is `Use`, not nothing** (`cbase.cpp:150`), and
   22 shipped connections rely on it. It dispatches `Behaviour::use_entity`,
   which is `m_pfnUse` — null for every class here but `func_button` and
   `func_rotating`, so for the rest accepting it and doing nothing is the
   behaviour and not a stub.

   **And the use *type* is the connection's serial number, cast.**
   `CBaseEntity::InputUse` passes `(USE_TYPE)inputdata.nOutputID`
   (`baseentity.cpp:4627`), where `nOutputID` is the `CEventAction`'s ID stamp
   — an ever-increasing counter over every connection in the map — so only the
   first four stamps in a level can name a real `USE_TYPE`. It is almost
   certainly a copy-paste of the value argument and it is reproduced, because
   it decides something: `CFuncMoveLinear::Use` returns immediately unless the
   type is `USE_SET`, so an I/O `Use` on a `func_movelinear` does nothing in
   the shipped game. `func_button` and `func_rotating` ignore the type, which
   is why they work anyway.

9. **A `times-to-fire` of 1 deletes the connection after it fires**, so an
   entity's action list is mutable state rather than a parsed constant. 2,925
   shipped connections depend on it — and a `times-to-fire` of **0** means
   `EVENT_FIRE_ALWAYS`, not "never".

10. **Removal is deferred, always.** `EntityCore::remove` sets a flag;
    `cleanup_delete_list` frees, twice a tick. An entity being iterated can
    therefore always be removed — and an event delivered later in the *same*
    tick still reaches a marked entity, which is Valve's behaviour and is
    observable.

11. **Target resolution is name, then handle, then — only if neither found
    anything — classname.** The classname fallback is not a curiosity: 2,747
    shipped connections fire `SetFogController` at the literal string
    `env_fog_controller` and reach every controller in the map without naming
    one.

12. **`ClassDef::keys` and `ClassDef::inputs` are declarations; `key_value` and
    `accept_input` are the implementations.** Nothing at run time reads `keys` —
    its reader is the invariant test. `inputs` *is* read at run time, for the
    field type. Add to both or to neither.

13. **A class that "inherits" must *contain*.** There is no chain walk to fall
    through, so `EnvLight::key_value` ends with
    `self.light.key_value(entity, key, value)` — that call *is*
    `BaseClass::KeyValue`. Forget it and the derived class silently loses every
    base key.

14. **`atoi`/`atof` read a prefix and never fail; `str::parse` would.** `"1.5abc"`
    is 1.5 and `""` is 0. Map data is dirty enough that this matters: there is a
    `logic_relay` in the shipped game with a key called `//OnTrigger`. Use
    `keyvalue::atof`, never `parse::<f32>()`, on anything that came out of a
    lump.

15. **`names_match`'s `*` does not have to be trailing.** Valve's comment says
    "only thing supported is trailing `*`" and the code says otherwise: it walks
    until the strings diverge and asks whether the query is sitting on a `*`. So
    `"*door"` matches *everything*. All 234 wildcard targets in the shipped maps
    are a plain trailing `*`, so nothing in Portal 2 reaches it.

16. **`CLight::Spawn` deletes any light with no `targetname`** — 6,937 of the
    shipped game's 7,150. Without it the entity list carries eleven per cent of
    the game as garbage and every other number still looks right.

17. **`RandomStream` seeds 0, 1 and -1 are one stream.** `SetSeed` stores
    `-seed`, and `ran1`'s zero guard (`if ( -(m_idum) < 1 ) m_idum = 1;`) then
    folds all three onto the same state. Valve seeds from the wall clock and
    never notices; a test that seeds 0 and 1 and expects two streams gets one.

18. **`RandomStream::int` is inclusive at both ends**, unlike every Rust range.

19. **`EntityCore::next_think` does not return what `set_next_think` was
    given.** It round-trips through the tick, so at 64 Hz
    `set_next_think(curtime + 0.2)` reads back 0.203125 later.
    `logic_timer`'s `AddToTimer` reads it and adds to it, so the drift is
    Valve's and is observable.

20. **`logic_case`'s `PickRandom` chooses among the cases whose *output* has
    connections, not the ones whose *value* is set.** A `logic_case` with
    sixteen `CaseNN` keys and one `OnCase03` connection always picks case 3.
    That is what lets Portal 2 use it as a random picker with no case values at
    all, which is most of how it uses it.

21. **`math_counter`'s `startvalue` is read with `atoi`, not `atof`**
    (`logicentities.cpp:1812`), so a `startvalue` of `2.5` starts at 2. It is
    the only key on that class not read the way its type suggests.

22. **`math_counter` does no clamping at all when `min` and `max` are both
    zero.** Valve's sentinel, and it is why 43 of the game's 102 set `min`
    explicitly.

23. **`func_instance_io_proxy` forwards the *caller* as well as the
    activator**, where every other class here passes itself. A chain crossing a
    proxy therefore looks to `!self` and to `CancelEvents` as though the proxy
    were not there — which is what makes it a proxy rather than a relay.

24. **`EntityCore::model` is a string.** `"*12"` is a brush model index and
    `"models/props/box.mdl"` is a studio model; resolving either is `world/`'s
    or `studio/`'s job. Reaching for a model here would be this module's first
    GPU dependency.

25. **`Server::level_init` never fails, and "unhandled" is not "broken".**
    38,286 of the shipped game's 60,925 entity blocks name a class this port has
    not got, and 67,485 keys go unconsumed. Both are expected and counted. Do
    not add an error path for them.

26. **The arrival alarm is not the think schedule, and a mover uses both.**
    `set_move_done_time` is `m_flMoveDoneTime`, a second timer with its own
    field: a `func_button` travels on the alarm and waits on the think, and a
    `func_door` uses the alarm for *both*. They differ in two ways that matter
    — the alarm is **not quantised to a tick** (the pusher lands on it by
    shortening the last step instead), and it runs on `local_time` rather than
    on `curtime`. Conflating them is the mistake `portdocs/SERVER.md` §4.7
    warns about.

27. **`set_move_done_time(0)` arms an alarm that can never fire.**
    `PerformPush` tests `m_flMoveDoneTime <= m_flLocalTime && m_flMoveDoneTime
    > 0` against the *absolute* alarm, and zero fails the second half — and
    `will_simulate_game_physics()` then takes the entity out of the simulation
    list, so nothing ever looks at it again. Valve's, reproduced: **four
    `func_door_rotating`s in the shipped game carry `wait 0` and stand open for
    ever.** A `func_button` cannot reach it, because `CBaseButton::Spawn`
    substitutes a `wait` of 1 for a 0 and 14 of the game's 64 rely on that.

28. **`move_done_time()` is not what `set_move_done_time` was given.** The
    setter takes a *delay*, the getter returns the *remaining* time, and the
    field holds neither — it is the absolute local time of the alarm.
    `min(remaining, one tick)` is the pusher's whole step calculation, which is
    why the field itself is private and `raw_move_done_time` is `pub(super)`.

29. **A `Behaviour` that holds a `Toggle` must call `Toggle::move_done`
    first.** That call *is* `CBaseToggle::MoveDone`: it snaps the origin or the
    angles onto the exact destination, zeroes the velocity and disarms. Skip it
    and the mover stops a fraction of a tick's travel past where it should be
    with its velocity still set — a door that never quite fills its doorway and
    then drifts. Same rule as gotcha 13, one method down.

30. **`linear_move`/`angular_move` return `false` for "we are already there",
    and the caller must run `Behaviour::move_done` itself — *before* firing any
    output.** In the C++ that call happens **inside** `LinearMove`
    (`subs.cpp:219`), so a zero-length open queues `OnFullyOpen`'s connections
    before `OnOpen`'s, which is the reverse of the order the two lines appear
    in `DoorGoUp`. Both are delivered in the same tick, so getting it backwards
    is a silent reordering rather than an error.
    `tests::a_zero_length_open_arrives_before_it_announces_itself` pins it.

31. **`speed` lives on `EntityCore`, not on the mover.** It is
    `CBaseEntity::m_flSpeed`, and `func_rotating` uses it as its *current*
    rotation rate rather than as a setting — so it is written from code as
    often as from the map. Each class substitutes its own default for a zero in
    `Spawn`: 100 for a door and a `func_movelinear`, 40 for a button, and
    `func_rotating` slams it to 0 and drives it from `maxspeed`.

32. **A mover's travel is the model's own size, and the model is not in the
    entity lump.** `EntityCore::model_bounds` is filled in by `level_init` from
    the `.bsp`'s model lump, and `CBaseDoor::Spawn` then takes
    `dot_product_abs(movedir, size - (2,2,2)) - lip`. The two units are Valve's
    "the engine expands bboxes by 1 in all directions"; drop them and a door
    that exactly fills its doorway no longer clears it. And `dot_product_abs`
    is **not** `|a·b|` — it is the sum of the absolute products, which is "how
    far the box extends along `movedir`, whichever way `movedir` points".

33. **A `movedir` is angles, and `AngleVectors` of a right angle is not
    exact.** `move_dir(Vec3::new(-90.0, 0.0, 0.0))` is `(4.4e-8, 0, 1)`, so a
    door that travels 64 units "straight up" also travels 2.8 millionths of a
    unit sideways. Valve's arithmetic has the same residue. Compare positions
    with a tolerance; asserting an exact zero asserts something the shipped
    game does not do either.

34. **Parented movers move in world space.** This port resolves `parentname` to
    a handle and keeps no local/abs transform pair, so `EntityCore::origin` is
    always the world-space origin the map gave and a move is applied there.
    Valve integrates `GetLocalVelocity()` into `GetLocalOrigin()` — the
    *parent's* frame. Measured: **174 of the game's 1,164 movers name a
    parent**, and for those the motion is right in shape and wrong in frame
    whenever the parent is itself turned or moved. It is the same missing pair
    that keeps the `SetParent` family unimplemented, and 1,078 of the depot's
    1,081 unhandled inputs are that family.

35. **A trigger is `SOLID_BSP` *and* `FSOLID_NOT_SOLID` *and*
    `FSOLID_TRIGGER`, and all three are load-bearing.** The type is what lets
    the touch query sweep against its real brushes; `FSOLID_NOT_SOLID` is what
    lets you walk into it; `FSOLID_TRIGGER` is what makes the query look at it
    at all. [`EntityCore::is_solid`] reads the first two together, and reading
    only the bit would have put every trigger in the game into the player's
    clip chain as an invisible wall.

    [`EntityCore::is_solid`]: #entity-and-entitycore-entityrs

36. **Every class that is meant to be solid must set `EntityCore::solid`, and
    forgetting it is invisible.** `Solid` arrived at stage 4 and defaults to
    `SOLID_NONE`; the five stage-3 brush classes did not set it, so
    `is_solid()` was false for every door in the game,
    `World::clip_models` came back empty, and **the clip chain silently
    collided with nothing** while every unit test passed — they all build a
    `PlacedBrushModel` by hand. Caught by loading the game and reading
    `trace`'s "N in the clip chain"; guarded now by
    `tests::every_brush_class_is_solid_unless_it_says_otherwise`.

37. **`world/` only clips against brush models the *game* answers for.**
    `PlacedBrushModel::owned` is false until `sync_brush_models` hears from the
    server, and `World::clip_models` needs `owned && solid`. That is not
    caution for its own sake: this port has classes for 6,302 of the game's
    11,635 brush entities, and among the rest are 2,383 `func_portal_bumper`s
    and 371 `trigger_portal_cleanser`s, **none of which is solid to a player**.
    Defaulting the other way fills every chamber with invisible walls, quietly.

38. **Only one side of a touch owes an `EndTouch`, and it is the trigger's.**
    `PhysicsMarkEntityAsTouched`'s `bShouldTouch` ends in
    `&& !other->IsSolidFlagSet( FSOLID_TRIGGER )`, so the trigger's link to the
    player carries `FTOUCHLINK_START_TOUCH` and the player's link back does
    not. `EndTouch` is therefore called on the trigger with the player and
    never the reverse.

39. **An entity that deletes itself fires no `EndTouch` of its own.**
    `PhysicsRemoveTouchedList` calls `PhysicsNotifyOtherOfUntouch` — which
    fires the *other* side's — and then `FreeTouchLink`, **not**
    `PhysicsRemoveToucher`. So a `trigger_once` never fires `OnEndTouch`, and
    the game places 1,476 of them.

40. **`CTriggerHurt::HurtAllTouchers` walks the *touch-link* list, not the
    trigger's own `m_hTouchingEntities`.** The two differ by exactly the
    entities that failed the filters, which is why `HurtEntity` re-tests them
    one at a time. A trigger keeps both lists and they answer different
    questions: the link list is everything overlapping it, and
    `m_hTouchingEntities` is everything that *passed*, which is what decides
    `OnStartTouchAll`, `OnEndTouchAll` and `TouchTest`.

41. **`InputToggle` on a trigger does not toggle `m_bDisabled`.** It flips
    `FSOLID_TRIGGER` and leaves the flag alone (`triggers.cpp:566`), so a
    toggled trigger and a disabled one give different answers to `TouchTest`.
    Valve's; zero shipped connections fire `Toggle` at a trigger.

42. **`CBaseTrigger::EndTouch` does not consult the filters and does not test
    `m_bDisabled`.** Valve has two `//FIXME: Without this, triggers fire their
    EndTouch outputs when they are disabled!` comments around the two places
    the test was commented out, and the behaviour they describe is the shipped
    behaviour. Reproduced, comments and all.

43. **A Portal 2 single-player `trigger_push` is twice as strong as the map
    says.** `CTriggerPush::Activate` doubles the speed whenever
    `maxClients == 1` and `sv_alternateticks` is off, under a comment reading
    `DIRTY HACK TO FOLLOW` — the game was tuned with alternate ticks on and
    ships with them off on PC. Both conditions are constants here, so the
    doubling is unconditional; dropping it makes every airlock half strength.
    The class's own `m_flSpeed` default is **100**, not the FGD's 40.

44. **A base velocity is not a velocity.** `trigger_push` writes
    `EntityCore::base_velocity` and sets `FL_BASEVELOCITY` *every tick it is
    pushing*; the movement adds it for the duration of a move and takes it back
    out, so a player carried along still reports a velocity of zero. The tick
    after the push stops, `CheckMovingGround` sees the flag clear and converts
    the whole thing into real velocity with a `1 + frametime/2` boost. Skip
    either half and a push either does nothing or never lets go.

45. **A teleport discards the swept-from point.** The touch pass sweeps from
    where the player was at the *last tick*, and a `trigger_teleport` moves it
    a thousand units mid-pass — so the next tick's sweep is reset to the new
    origin. Valve gets the same by having `CBaseEntity::Teleport` call
    `PhysicsTouchTriggers()` with **no** previous origin. Without it a teleport
    fires every trigger between the two ends.

46. **The player entity's `angles` are its *view* angles**, where
    `CBasePlayer` keeps `m_angAbsRotation` (yaw only) and its eye angles
    separately. Every consumer here wants the eye —
    `CTriggerTeleport::Touch` explicitly substitutes `EyeAngles()` for
    `GetAbsAngles()` when the toucher is a player — so the port keeps one
    field. Its `origin` is still the **feet**.

47. **An entity created by another entity is spawned *after* its creator's
    handler returns, not during it.** `Context::create_entity` inserts and
    queues; `Server::dispatch` drains the queue on the way out. So a creator
    may set the new entity's fields and keep its handle, and may **not** read
    anything its `Spawn` would have written. Nothing in the game does —
    `CreateTriggers` stores the handle and stops — and the compiler cannot
    tell you, because the handle resolves either way.

48. **A class must set `EntityCore::solid` to `Solid::Obb` to be a box
    trigger, and `Solid` now decides which *code* answers for a shape.**
    Before this class the solidity type was a record of what Valve would have
    used and nothing read it (see the absence table). Now `Bsp`/`VPhysics`
    means "ask the engine through `TouchQuery`, by `"*N"` index" and `Obb`
    means "answer it here, with `obb::swept_box_touches_obb`". A trigger that
    is neither is invisible to the touch pass, in silence — which is
    gotcha 36's failure mode one level up.

49. **A box trigger's size lives in `model_bounds`, and nothing fills it in
    for you.** `Server::level_init` puts a `"*N"` model's box there out of the
    `.bsp`; a created entity names no model, so its creator writes the field
    (`UTIL_SetSize`). Leave it and the box is zero-sized and the trigger is
    never touched.

50. **The `OnStartTouchAll` / `OnEndTouchAll` virtuals reach a containing
    class through a return value, not a callback.** `BaseTrigger::start_touch`
    returns [`Touched`] and `end_touch` returns the `all` half. And a class
    that overrides `PassesTriggerFilters` must call
    **`start_touch_passing`** rather than `start_touch`, or the base asks its
    own question first and the override never runs.

    [`Touched`]: #classes-classes

51. **A `prop_floor_button` is pressed by an *input*, not by a call.** The
    trigger posts `PressIn` at its owner where Valve calls
    `m_pOwnerButton->TriggerStartTouch( pOther )` directly, because a handler
    cannot dispatch into another class. It costs one extra event and **no
    tick** — the queue restarts from the head, so the whole chain lands inside
    the tick the touch happened in (gotcha 4), which
    `tests::a_press_completes_in_the_tick_it_started_in` pins. What is
    observable is the ordering against other zero-delay events and one extra
    row in `IoStats::dispatched`.

52. **`Context::take_damage` is queued, and a self-aimed one is silently
    dropped.** It resolves the target through the entity list, and the entity
    currently being dispatched is *not in it* (gotcha 36) — so
    `cx.take_damage(entity.id(), …)` inside your own handler hurts nobody and
    returns `false`. Hurting yourself does not need the queue at all: you hold
    `&mut EntityCore` and `&mut self` already, so call `self.on_take_damage`
    directly, which is what `CBasePlayer::InputSetHealth` compiles to in the
    C++ anyway.

53. **`EntityCore::is_alive` is the *life state* and `IsDead()` is the
    *health*, and they disagree.** `CBaseEntity::IsAlive` is
    `m_lifeState == LIFE_ALIVE`; `CGameMovement::IsDead` is `m_iHealth <= 0`
    (`gamemovement.cpp:1091`). The window between them is the single dispatch
    in which `on_take_damage` has subtracted the last point and `event_killed`
    has not run yet. That is why `PlayerState` carries the **health** across
    to `client/` and not the life state: the movement asks the health
    question, and answering it with the life state leaves a corpse that can
    still walk for one frame.

54. **`trigger_hurt` fires its outputs whether or not the damage lands**, and
    it keeps firing them at a corpse. Three things that look like they would
    stop it do not: the dead player going `FSOLID_NOT_SOLID` only stops the
    *player* testing triggers, and a stationary `MOVETYPE_NONE` trigger never
    re-tests its own, so the touch link survives; `m_takedamage` stays
    `DAMAGE_YES`, because `CBaseCombatCharacter::Event_Killed` does **not**
    chain to `CBaseEntity::Event_Killed`, which is the one that would clear it;
    and `TakeDamage` returns `void`, so `HurtEntity` cannot see a refusal. The
    consequence is bounded and is Valve's: for the three seconds between dying
    and the reload, a map's `OnHurtPlayer` chain runs six more times. It is
    also why `god` mode does not wedge a scripted chamber.

55. **`m_flDamage` is per second and a dose is per *think*.** A `trigger_hurt`
    deals `m_flDamage * dt`, where `dt` is 0.5 for the half-second `HurtThink`
    and the real elapsed time for `RadiationThink`. Dealing the key's value
    per dose doubles the lethality of every `trigger_hurt` in the game.

56. **The fractional damage accumulator is not decoration.** `take_damage`
    keeps the fraction of a point in `EntityCore::damage_accumulator` and pays
    it out when it reaches one, so five hits of 2.5 take 12 points and not 10.
    Drop it and every repeating damage source is weaker than the map asked
    for; a hit smaller than a whole point becomes free rather than
    accumulating, which is `Damaged::Refused` on the first two of three.

57. **Four `PlayerState` fields do not round-trip, on purpose.**
    `move_type`, `health`, `life_state` and `flags` are the server's since
    stage 5 and `set_player_state` ignores what arrives in them. If you add a
    field, decide which way it goes and say so on the field — a server-owned
    field that the client writes back is undone a fraction of a frame after it
    is set, which looks like `noclip` not working rather than like a bug in
    this file.

58. **`FL_ONGROUND` travels in `PlayerState::on_ground` and is masked out of
    `PlayerState::flags`.** It is the one flag that goes both ways — the
    client finds the ground plane and the server takes the player off it — so
    carrying it in both fields would let the two disagree.

59. **`sk_dmg_take_scale1` is 1 because the number does not exist.** Every hit
    a Portal 2 player takes is multiplied by it (`portal_player.cpp:3607`), the
    cvar is declared `extern` here and defined in an `hl2_gamerules.cpp` this
    tree does not contain, and the shipped depot sets it in no `.cfg` and no
    VPK. One definition site (`classes::player::SK_DMG_TAKE_SCALE`), one line
    to change if it is ever recovered. It barely matters: the weakest
    `trigger_hurt` in the game deals 10 a second against 100 health and 202 of
    the 215 deal 100 or more, so any scale between about 0.1 and 10 kills the
    player in the same place.

60. **A `prop_dynamic`'s playback rate starts at *zero*, not one.**
    `CBaseProp::Spawn` sets `m_flPlaybackRate = 0` and only `ResetSequenceInfo`
    — reached through `PropSetAnim`/`PropSetSequence` — puts it back to 1. That
    is what makes the 6,046 props in the game with no `DefaultAnim` stand
    perfectly still instead of looping their first sequence, and it is why the
    rate is a field of `ModelState` rather than an assumed 1. A `FloorButton`'s
    is 1 for the opposite reason: its `Spawn` ends with a `ResetSequence`.

61. **The classname is behaviour on a `prop_dynamic`, and two lines apart.**
    `CDynamicProp::Spawn` promotes `SOLID_NONE` to `SOLID_OBB` only
    `if ( FClassnameIs( this, "prop_dynamic" ) )`, and *then* renames
    `prop_dynamic_override` to `prop_dynamic` — so 2,622 props take the
    promotion and 211 `_override`s with the identical `solid 0` do not.
    `CBaseProp::KeyValue` asks the same question about `health`. Both are
    `DynamicProp::is_plain_dynamic` / `allows_health`, asked of
    `EntityCore::class` rather than stored.

62. **`Lookup::Unknown` is not `Lookup::Missing`, and every `Spawn` in the game
    gets `Unknown`.** See [`sequences`](#sequences-sequencesrs). A class that
    treats the two alike either believes a map that names a sequence nothing
    has (harmless: the renderer draws the bind pose) or refuses 2,416 shipped
    `DefaultAnim` keys at spawn (not harmless).

63. **`SetNextThink( curtime )` at tick zero means *never*.** `SetNextThink(0)`
    is "not scheduled" — `physics_run_think`'s `think_tick <= 0` guard, which
    is Valve's too — so `SUB_StartFadeOut( 0 )` fired before the first tick
    arms nothing. It cannot happen from map data, because an input needs an
    event and an event needs a tick; it happens in tests that fire an input
    into a freshly loaded level.

64. **`AnimThink` cancels itself when there is nothing left to decide**, where
    Valve re-arms it for ever. The port derives the cycle from `ModelState`'s
    five numbers instead of accumulating it, so a think over a looping
    sequence, a zero-length one, or one whose model never loaded has no work —
    see the divergence table. The one line of Valve's `else` branch that had to
    survive is `m_bAnimationDone = false`: without it a looping sequence
    followed by a finite one fires no `OnAnimationDone`.

65. **An empty sequence label is sequence 0, and the engine is what knows it.**
    `ModelState::sequence` is a label where Valve networks `m_nSequence` as an
    `int`, and the two differ in exactly one place: an `int` starts at zero and
    a label starts empty. `CDynamicProp::Spawn` calls `PropSetAnim` only for a
    prop carrying a `DefaultAnim` (`props.cpp:2036`), and `PropSetAnim` answers
    a name the model does not have with an explicit `SetSequence( 0 )`
    (`props.cpp:2422`) — so **every prop in the game is posed by some
    sequence**, 6,046 of the 8,462 by sequence 0 alone.

    This side is right to leave the label empty: the server cannot look one up.
    What must not happen is the *engine* reading "no label" as "no animation",
    because that is the bind pose, and a bind pose is not a pose anybody ever
    looked at — `props_motel/hotel_container_furniture01`-`03` are a quarter
    turn away from their own `idle`, which stands `sp_a1_intro1`'s furniture
    inside the bed. `EntityModels` resolves an unknown label to 0; see
    `rustdocs/ENGINE.md`'s `world::entities`.

66. **A test chamber door only ever plays `open`, and shuts by playing it
    backwards.** `CPropTestChamberDoor::Spawn` looks up four sequences and
    caches them in four fields; `m_nSequenceClose`, `m_nSequenceOpenIdle` and
    `m_nSequenceCloseIdle` are then read by nothing in the class or anywhere
    else in the tree. All `Open` and `Close` do is `SetPlaybackRate( ±1 )`.
    The model says the asymmetry is deliberate rather than an oversight:
    `open` is 23 frames and `close` is **36**, so shutting a door with `close`
    would take 1.46 seconds where the shipped game takes 0.92.

67. **`m_bSequenceFinished` is sticky, so only a door's *first* opening
    reports its own end.** Nothing clears the flag except `ResetSequenceInfo`,
    and `CPropTestChamberDoor` calls `ResetSequence` exactly once, in `Spawn`.
    So the first `OnFullyOpen` waits for `GetLastVisibleCycle` — 0.797 seconds
    on the tick grid, against a 0.9167-second travel — and **every later
    `OnFullyOpen` and every `OnFullyClosed` fires on the first think after the
    input**, 0.094 seconds in, while the door is still visibly moving.

    It is reproduced deliberately. 150 of the game's 247 door output
    connections are `OnFullyClosed` and they were authored against it — 29 of
    them disable a `func_clip_vphysics` and 25 enable a fizzler — so a door
    that waited for its animation would delay all of them by three quarters of
    a second. `every_shipped_testchamber_door_opens_and_shuts` measures both
    numbers over all 138.

68. **A rate change on a derived cycle has to re-base, and for this class it
    is not optional.** Valve accumulates `m_flCycle`, so `SetPlaybackRate`
    simply changes how fast it grows from where it is; this port derives the
    cycle from `(cycle, anim_time, playback_rate)`, so `Open` and `Close` must
    first pin `cycle` to where the door actually is and restart `anim_time`.
    Skip it and a door told to `Close` computes its position from the moment it
    spawned. It is the same divergence `DynamicProp`'s `SetPlaybackRate` input
    records (gotcha 63); there it keeps 427 connections from snapping, here it
    is the whole of how a door shuts.

69. **`AnimateThink` re-arms unconditionally, and this class deliberately does
    *not* take `AnimThink`'s cancel-when-idle divergence** (gotcha 64). That
    divergence is worth it for 8,462 props; there are 138 doors, two per map.
    What re-arming buys is the exact 0.1-second grid, and the grid is what
    decides when the 181 `OnFullyOpen`/`OnFullyClosed` connections fire. The
    cost is visible in one depot number: **`io.thinks` went from 4,420 to
    7,318**, and all 2,898 of those are doors.

70. **`IsOpen()` is where the door is *going*, not where it is.** `m_bIsOpen`
    is set the instant `Open` is accepted, three quarters of a second before
    the door has finished opening — so a second `Open` during the travel is
    refused, and it is what decides which of the two "fully" outputs the think
    fires. `LockOpen` opens *and then* locks, in that order, so the open
    itself gets through; that is what its 29 shipped connections want.

71. **A `logic_branch_listener` reports nothing at level start, and every
    chamber door in the game depends on it.** `Spawn` is empty, `Activate`
    only registers, and `m_eLastState` starts `NOT_INIT` — so the first output
    comes from the first branch *change*, not from the first evaluation. Both
    branches of a door's listener read "shut me" at spawn; a listener that
    tested itself on the way up would slam every door in the map closed on
    tick one.

72. **`SetValue` fires no output of its own and still reaches a listener.**
    The notification in `CLogicBranch::UpdateValue` is guarded by the value
    having *changed*; the `OnTrue`/`OnFalse` firing is guarded by the input
    being a `*Test` form. They are independent, and getting it wrong in either
    direction is silent: fold the notification under `eFire` and no door in
    the game ever shuts (1,175 of the 1,601 connections into a branch are
    `SetValue`), or drop the change guard and `Test` — 308 connections — makes
    every listener re-report.

73. **A listener with no branches reports `OnMixed`.** With an empty list
    neither `bOneTrue` nor `bOneFalse` is set, so `DoTest` falls through both
    arms into the `else`. Unreachable in shipped content — all 350 `Branch*`
    keys in the game resolve, to exactly one entity each — and the arm a
    reimplementation gets backwards.

74. **Linkage is by group and size, never by `PortalTwo` — and `PortalTwo` is
    *overwritten* by linking.** `UpdatePortalLinkage` takes the first portal in
    the group that is active, unlinked and exactly the same size, and the base
    class then assigns `m_bIsPortal2 = !m_hLinkedPortal->m_bIsPortal2`. The key
    decides colour and nothing else — `portal_base2d.h:38` says so in as many
    words. Reading it as the pairing key looks right on all 21 shipped portals,
    because every one of them is already the opposite of its partner.

75. **The portal that activates *second* keeps its colour.** The forcing line
    runs on the *partner* first, through the recursion at `prop_portal.cpp:584`,
    and on the activating portal second — by which time the partner is already
    the opposite, so the second assignment is a no-op. A map that switched on
    two blues would turn the **first** one orange.

76. **A `prop_portal` draws no model, and drawing one would be a magenta
    rectangle across the wall.** `portal1.mdl` is four vertices wearing
    `writez`, a depth-only shader that exists to punch a hole for the recursive
    view. `PropPortal::model_state` therefore answers `None`, which keeps it out
    of `ModelEntityState` entirely — and `writez` is not a shader this port has,
    so a material lookup would fall back to the error checkerboard. What you
    see where a portal is, is `engine::world::portals`' quad.

77. **`NewLocation` switches a portal on.** `SetActive( true )` sits in the
    middle of `CPortal_Base2D::NewLocation`, which is what lets the `portal`
    console command place and activate in one call — and is why all four
    shipped `NewLocation` connections are aimed at portals that are already on.

78. **A portal's trigger box is one-sided.** `GetLocalMins()`..`GetLocalMaxs()`
    is `(0, -hw, -hh)`..`(**64**, hw, hh)` in the portal's own frame: the room
    in front of it and nothing inside the wall. A symmetric box would make the
    portal notice things behind the surface it is stuck to.

79. **The default half-height is 56 and the reference tree says 14.**
    `prop_portal_shared.cpp:167` initializes it to `0.25 *
    DEFAULT_PORTAL_HALF_HEIGHT` under a comment that says exactly what it is —
    *"default to sane-looking but incorrect portal height for CEG - Updated in
    constructor"* — and the constructor overwrites it from an anti-tamper macro
    whose value is not in this tree. The `#define` is, and the shipped game's
    portal is 64 x 112 units.

80. **The teleport matrix has a 180° turn about *up* baked into it**, and a
    point in *front* of the entrance therefore maps to *behind* the exit. Both
    halves read as bugs and neither is. Without the half turn you come out of
    the exit facing back the way you came, which looks like a mirrored portal;
    and the front-to-back relationship is what makes the same matrix serve as a
    camera transform for the view through a portal, and is consistent for the
    teleport because the player crosses the entrance *plane* — so the point
    being transformed is a hair behind it and lands a hair in front of the
    exit.

81. **A portal's `right` is the negation of its angle matrix's second column.**
    `UpdatePortalTeleportMatrix` reads the three columns out and immediately
    writes `m_vRight = -m_vRight`, because Valve's `matrix3x4_t` column 1 is
    *left*. `PropPortal::right` does the negation once; anything that repeats it
    mirrors the quad, the corners and the matrix together, which is a picture
    that looks plausible until you walk through.

---

## Deliberate divergences from Valve

Each of these is a place the port does *not* do what the C++ does, on purpose.

| | Valve | Here | Why |
|---|---|---|---|
| The global state table | `env_global` registers globals; `logic_auto`'s `globalstate` consults them | Every global reads `GLOBAL_OFF` | `env_global` is not ported, and `GetState` returns `GLOBAL_OFF` for an unregistered name — so this **is** Valve's behaviour against an empty table. Two entities in the game are affected. |
| `SetBloomScaleRange` | Passes `sscanf`'s format and buffer the wrong way round, passes floats by value, assigns one field twice | Warns and returns | The C++ cannot ever have worked; `nargs` is never 2. Zero shipped connections fire it. |
| The no-controller tone-map fallback | Resets every custom flag **except** `g_bUseCustomAutoExposureMin` | Resets all of them | Valve's omission makes a custom minimum sticky for the rest of a level. `portdocs/SERVER.md` §7.4 asks not to reproduce it. |
| The RNG seed | Once per process, from the wall clock | Once per level, from a constant | Reproducibility is worth more than variety here: it is what lets the depot test assert exact totals over maps containing random pickers. |
| `CancelEvents`' caller test | Compares the caller pointer, then re-compares its own name and classname against themselves | Compares the handle | The extra test can only ever be true. Dead code, not reproduced. |
| A parent cycle | Recurses until the stack runs out (only self-parenting is checked) | Bounded by the entity count, reported, treated as depth 1 | No shipped map contains a cycle. |
| `qsort` in the spawn sort | Unstable; equal-rank order is unspecified | Stable, so lump order survives within a rank | Deterministic, and it is what a level designer means by "in order". |
| The zero-delay event chain | Unbounded; a self-triggering relay hangs the server | Bounded at 100,000 events a tick, then the queue is dropped with a warning | Four times the largest map's entire connection count. |
| A `filter_multi` chain | Unbounded; a filter naming itself recurses until the stack runs out | Bounded at 8 deep, reported, treated as a pass | No shipped map has a chain deeper than one. |
| Pressing a floor button | `m_pOwnerButton->TriggerStartTouch( pOther )`, a direct call | The trigger posts `PressIn` at the button | A handler cannot dispatch into another class — the dispatched entity is lifted out of the list. Same tick, one more event; gotcha 51. |
| `CPropFloorButton::CreateTriggers`' `SetParent` | Parents the trigger to the button, so a button on a platform carries it | Places the trigger at the button's absolute origin and angles | The same missing local/abs pair as gotcha 34. **Not one of the game's 65 `prop_floor_button`s has a `parentname`**, and none is a mover, so there is nothing for the transform to do. |
| `CPropFloorButton::UpdateOnRemove` | `UTIL_Remove( m_hButtonTrigger )` | The trigger outlives a killed button | There is no removal hook on `Behaviour`, and **no connection in any shipped map fires `Kill` at a floor button**. An orphan does nothing: its owner handle stops resolving, so its filter refuses everything. |
| A creator's `DispatchSpawn` | Called by the creator, part-way through its own `Spawn` | Queued, run the moment the creator's handler returns | Gotcha 47. |
| `pOther->TakeDamage( info )` | A direct call, part-way through the hurter's think | Queued, applied the moment the hurter's handler returns | Same reason and same shape as the row above: applying damage runs the *victim's* virtuals, and the hurter has been lifted out of the list. Gotcha 52. |
| `respawn()` in single player | `engine->ServerCommand( "reload\n" )` — restores the last **save** | `Context::reload_level`, which restarts the map | There is no save/restore (`portdocs/SERVER.md` §6 defers it as `serde` over the entity state). For a Portal 2 chamber the two are usually the same place, because the game autosaves on entry. |
| `UTIL_ScreenFade` on death and on `player_loadsaved` | Fades the screen to black over three seconds | Nothing is drawn; the *timer* it is drawn over is kept exactly | A screen fade is a user message to a HUD that does not exist. The respawn still happens at `m_flDeathTime + 3` and the reload still at `loadtime`. |
| `CBasePlayer::PreThink`'s proxy outputs | Called from `CPlayerMove::RunCommand`, once per usercmd | A step of `Server::run_tick`, before the touch pass | The think schedule cannot express "every tick, first". Same place in the order, same once-per-tick cadence. |
| `CDynamicProp::AnimThink`'s cadence | Re-arms itself at 10 Hz for as long as the entity has a sequence, so that `StudioFrameAdvance` can accumulate `m_flCycle` | Cancels itself once the sequence cannot end — looping, zero-length, or a model that never loaded | The cycle is *derived* here, not accumulated (`ModelEntityState`), so the second half of Valve's think does not exist and the first half has nothing to decide. `SetAnimation`, `SetPlaybackRate` and `Spawn` all re-arm it, so a prop given something new to do wakes for it. Gotcha 64. |
| `CDynamicProp::InputSetPlaybackRate` | Changes `m_flPlaybackRate` and leaves `m_flCycle` where it is, because it is accumulated | Also re-bases `m_flCycle` and `m_flAnimTime` so the derived pose is unchanged at that instant | Same cause, opposite sign: without it the 427 shipped `SetPlaybackRate -1` connections would each snap their prop to a different frame before running it backwards. The pose is identical; the fields it is stored in are not. |
| `CDynamicProp::PropSetSequence`'s `GotoSequence` | Walks the model's `$node`/`$transition` graph to reach the goal sequence, possibly through an intermediate | Goes straight to the goal, forwards, from cycle 0 | That *is* `GotoSequence`'s first branch — "bail if we're going to or from a node 0". Measured: across the **2,597 sequences of the 606 models the game's props name, not one has a non-zero entry or exit node and not one has `nodeflags`**, so no other branch is reachable and `m_iTransitionDirection` is `+1` everywhere. |
| `PropSetAnim`'s failure branch | `SetSequence( 0 )` — the model's first sequence, at whatever cycle | The bind pose | This module has labels, not indices, and sequence 0 has no label it can name. Reached by 183 `DefaultAnim` keys in the game, every one of them naming a sequence that is in no model at all. |
| `AddFlag( FL_UNPAINTABLE )` | Sets `1 << 32` on a 32-bit `m_fFlags` | Nothing | Valve's own comment three lines above it: `// FIXME[HPE]: this won't actually work - we're out of bits. :(`. There is no paint system here to read it either. |
| `logic_playerproxy`'s inputs | Twenty declared across three `#ifdef` families | **None** | Every input the class has in Portal 2 is a portal-gun or grab-controller input, and `RequestPlayerHealth`/`SetPlayerHealth` are `#if defined HL2_EPISODIC && !defined( PORTAL2 )`. Accepting none is the shape rather than a gap — and it is why the `PlayerHealth` output cannot fire in Portal 2 at all. |
| `CPropTestChamberDoor`'s `Open`/`Close` | `SetPlaybackRate( ±1 )` and nothing else, because `m_flCycle` is accumulated | Also re-bases `m_flCycle` and `m_flAnimTime` onto where the door is now | Same cause as the `SetPlaybackRate` row above, and here it is not cosmetic: without it a door told to `Close` would compute its position from the moment it spawned. Gotcha 68. |
| `CPropTestChamberDoor::AnimateThink`'s cadence | Re-arms at 10 Hz for the rest of the level | **The same** — this class does *not* take `AnimThink`'s cancel-when-idle divergence | Not a divergence, listed because the neighbouring row is: the grid is what decides when the 181 `OnFullyOpen`/`OnFullyClosed` connections fire, and there are 138 doors rather than 8,462 props. Gotcha 69. |
| `CLogicBranchList::Activate`'s `FindEntityGeneric` | Falls back to `FindEntityByClassname` when the name matches nothing, so `Branch01 "logic_branch"` would monitor every branch in the map | `Context::find_all_by_name`, which searches names only | All 350 `Branch*` keys in the game resolve by name, to exactly one entity each — no empty slot, no wildcard, nothing named that is not a `logic_branch` — so the fallback is unreachable. |
| Entities created *by* a spawn | Bounded only by the stack | Bounded at 4,096 per dispatch, then dropped with a warning | Same shape as the zero-delay event chain's bound. The most any map creates is four. |
| `Enable`/`Disable`/`Toggle` on a trigger | Calls `PhysicsTouchTriggers()` at once, so enabling a trigger you are standing in fires `OnStartTouch` in the same tick | Fires it on the **next** tick | The touch pass is player-driven and runs at one fixed point in the tick. At most 15.6 ms late; the condition for closing it is a touch query the server can ask mid-tick. |
| `!player_blue` / `!player_orange` | `GetGlobalTeam( … )->GetPlayer( 0 )` | Reported as "no such player" | Single player has no teams, so Valve answers null here too — this is a report line rather than a divergence, and it is 74 of the depot's unhandled procedurals. |

---

## What is deliberately absent

| | Why |
|---|---|
| **Pushing what is in the way** — `CPhysicsPushedEntities`, `Blocked`/`StartBlocked`/`EndBlocked`, `m_bDoorGroup`, `forceclosed`, `dmg`/`BlockDamage` | ~1,000 lines of speculative push and rollback, and it wants `ENGINE_TRACE.md` stage 4 underneath it. A door that moves through the player is a better state than a door that does not move. The keys are parsed so they are not counted as unknown; nothing reads them. |
| `SOLID_*` — `SOLID_BSP` versus `SOLID_VPHYSICS`, and `solidbsp` | Nothing chooses between *those two*: for a brush entity they are the same brushes. `SOLID_OBB` is different and is now real — it decides that a shape is answered by `obb` rather than by the engine (gotcha 48). |
| The **physics force** a hurt imparts — `GuessDamageForce`, `VPhysicsTakeDamage`, `CBaseEntity::OnTakeDamage`'s impulse | The damage itself landed at stage 5; the force needs `rapier`. `DamageInfo` carries neither the force nor the position, because a field nothing reads is a field nothing checks — and the impulse branch is unreachable anyway: it demands `!info.GetAttacker()->IsSolidFlagSet( FSOLID_TRIGGER )` and the only attacker in the port is a `trigger_hurt`. |
| Everything else that can hurt you — turrets (`npc_portal_turret_floor`), crushers (`CPhysicsPushedEntities`), `prop_physics` | Each is a class or a subsystem that is not ported. `trigger_hurt` is the whole damage surface the shipped maps reach. |
| The **armour** — `m_ArmorValue`, `ARMOR_RATIO`, `ARMOR_BONUS`, `old_armor` | Portal has no armour and no item that gives any, so the block in `CBasePlayer::OnTakeDamage` is thirty lines of arithmetic on a value that is always zero. |
| Drowning, the HEV suit's `SetSuitUpdate` voice lines, `m_DmgTake`/`m_bitsHUDDamage`, the geiger counter | A HUD, a sound system and a suit, none of which exist. |
| **Fall damage** | Not deferred — **deleted**, and it is a measurement: `CPortalGameRules::FlPlayerFallDamage` is `{ return 0.0f; } //no fall damage in portal` (`portal_gamerules.h:61`), and the multiplayer rules agree in words. Nothing in Portal 2 can be killed by landing, whatever the height, which is why 34 of the game's `trigger_hurt`s carry `DMG_FALL`: the pit does the killing, not the fall. |
| `LIFE_RESPAWNABLE` and the wait-for-a-button respawn (`PORTAL_RESPAWN_DELAY`) | Multiplayer's. `sp_fade_and_force_respawn` defaults to 1, so single-player Portal 2 fades for three seconds and reloads without waiting, and the branch underneath is unreachable. |
| `player_speedmod` (4 placed) — `SetLaggedMovementValue`, `DisableButtons` | Two more `PlayerState` fields and a multiplier on the movement's `dt`, for four entities on three maps (`e1912`, `sp_a3_00`, `sp_a4_finale4`). Ordinary follow-on work rather than a subsystem. |
| Pushing a *physics object* — `SF_TRIGGER_ALLOW_PHYSICS`, `SF_TRIGGER_PUSH_USE_MASS`, `ApplyForceCenter` | `MOVETYPE_VPHYSICS` needs `rapier` (`ENGINE_TRACE.md` stage 5). 147 `trigger_multiple`s in the game are physics-only and correctly refuse the player. |
| NPCs and vehicles in `PassesTriggerFilters` — the `FL_NPC` sub-tests, `IsInAVehicle` | Portal 2 has 293 NPCs of 6 classnames and no vehicles at all. The `FL_NPC` term of the disjunction is kept so the line reads like the C++; the two `IN_VEHICLES` refusals are kept because they *refuse* rather than allow, and zero shipped triggers set either flag. |
| `filter_activator_team`, `filter_enemy`, `filter_size`, `filter_activator_mass_greater`, `filter_activator_context` | Zero placed by any shipped map. |
| `trigger_look` (41), `trigger_playerteam` (645), `trigger_catapult` (185), `trigger_portal_cleanser` (371), `trigger_transition` (62), `trigger_autosave` (57), `trigger_ping_detector` (20) | Either reconstruction jobs with no C++ in this tree (`portdocs/SERVER.md` §1.3) or wanting a subsystem that does not exist — saves, co-op teams, the paint system. |
| `CTriggerHurt`'s geiger counter, and `CTriggerTeleport`'s `CheckDestIfClearForPlayer` | A client-side HUD element, and `g_pGameRules->IsSpawnPointValid`. Zero shipped maps set the second. |
| Sound — `noise1`/`noise2`/`startclosesound`/`closesound`/`StartSound`/`StopSound`/`sounds`/`message`, the lock sentences, `MovingSoundThink` | There is no sound system. The names are parsed and printed by `ent_dump`; `MovingSoundThink` is a *named think context*, which is also not ported and is the only thing in the game that wanted one. |
| `CBaseDoor::Activate`'s movement group and `UpdateAreaPortals` | `m_bDoorGroup` is read only by `Blocked`, above; area portals are the engine's visibility system, which is not written. |
| `CBaseDoor::DoorActivate`, `DoorTouch`, `ChainUse`, `ButtonTouch`, `ButtonResponseToTouch`, `OnTakeDamage` | The touch and damage entry points. Nothing reaches them through I/O — and the damage one is now a *measurement*: `CBaseDoor::Spawn` and `CBaseButton::Spawn` only set `m_takedamage = DAMAGE_YES` when `health > 0`, and **all 682 `health` keys in the shipped game are `0`**, so no door or button in Portal 2 is shootable. |
| Which way a rotating door swings away from you (`DoorGoUp`'s 40-line cross product) | It needs the activator's position, and the activator is a player in every case that reaches it. Without one Valve's `sign` stays `1.0`, which is the branch every door in Portal 2 takes because every door in Portal 2 is opened by I/O. |
| `func_door`'s `SetToggleState` input | Declared `FIELD_FLOAT` and read with `value.Int()` (`doors.cpp:495`), which `variant_t` answers with **zero** for a float — so in the shipped game it always means `TS_AT_TOP`. Zero shipped connections fire it. |
| `CBaseToggle`'s `master` / `UTIL_IsMasterTriggered` | The `multisource` interlock. **No shipped Portal 2 map sets a `master` key on any of these classes.** |
| `SF_DOOR_START_OPEN_OBSOLETE` | **No shipped map sets it.** The 40 doors that spawn open use `spawnpos 1`. |
| `func_rot_button` (2), `momentary_rot_button` (1), `func_tracktrain` (233), `func_tanktrain` (20) | The remaining movers. `CBaseButton`'s `m_fRotating` branch is `CRotButton`'s and is therefore dead here; the trains need `path_track`. |
| `CPropTestChamberDoor`'s area portal window — `AreaPortalWindow`, `UseAreaPortalFade`, `AreaPortalFadeStart`/`End`, `AreaPortalOpen`/`Close`, `CFuncAreaPortalWindow` | All the two calls do is write `m_flFadeStartDist` and `m_flFadeDist` on a `func_areaportalwindow`, which belongs to the engine's visibility system (areas and areaportals, `cmodel.cpp`) and is not ported. **84 doors name a window and 94 write the fade triple.** The four keys are consumed and printed by `ent_dump`; the two call sites are marked in `Spawn`, `OnOpen` and `OnFullyClosed`, so wiring them up later is one line each. |
| `CPropTestChamberDoor`'s bone followers — `CreateVPhysics`, `CreateBoneFollowers`, `TestCollision`, `UpdateOnRemove` | The same `vphysics` gap `prop_dynamic`'s row above records, and here it is the door's *whole* collision: the model's `bone_followers` block becomes one physics entity per moving bone and the door itself goes `FSOLID_NOT_SOLID`. So a chamber door is **drawn and walked through**. In the shipped map the doorway also carries a `func_clip_vphysics`, which is not ported either. |
| `CPropTestChamberDoor`'s sounds — `prop_portal_door.open`, `prop_portal_door.close` | There is no sound system. They are the only two things `Precache` asks for beyond the model. |
| `CPropTestChamberDoor`'s `SetFadeDistance( -1, 0 )` / `SetGlobalFadeScale( 0 )` — "never let crucial game components fade out" | `fademindist`/`fademaxdist`/`fadescale` are already read and dropped by `base_key_value`, because `world/` has no per-instance distance fade. There is nothing for the override to override. |
| `CPropFloorButton::AnimateThink` | Still not scheduled, and it is a saving: its body is `StudioFrameAdvance`, which the renderer does for itself from `m_flAnimTime` and does *smoothly*, where a 10 Hz think would step it. So 65 entities do not wake ten times a second and no button sits in the simulation list for ever. (`CDynamicProp::AnimThink` **is** scheduled, because it does a second job: it decides when a sequence has ended. See the divergence table for the half of it that is not here.) |
| `CDynamicProp`'s `ParsePropData` — `scripts/propdata.txt`, the gib lists, `PROPINTER_*`, `prop_physics` | The breakable-prop system. The one thing it decides for `prop_dynamic` is a *deletion*: a plain `prop_dynamic` whose model carries a `prop_data` block is removed at load with a `DevWarning`, and an `_override` — which is what that classname is *for* — is not. Measured: 15 of the 606 models the game's props name have such a block and 106 entities wear one, but **94 of the 106 are `prop_dynamic_override`**, so the whole cost is **12 entities across three maps** (`sp_a2_bts3`, `mp_coop_tbeam_end`, `sp_a1_intro7` — laser gibs and a lab chair) that the shipped game deletes and this port draws. |
| `CDynamicProp`'s bone followers — `CreateBoneFollowers`, `m_BoneFollowerManager`, `TestCollision`, `NotifyPositionChanged`, `DisableBoneFollowers` | One physics entity per named bone, so that an animated prop *collides* as it moves. That is `vphysics`, replaced here by `rapier` and not reached. 260 props set the key that would turn it off. |
| `CDynamicProp`'s `VPhysicsInitStatic` | A prop's collision is its `.phy` (`ENGINE_TRACE.md` stage 5), and `World::clip_models` only ever sees `"*N"` brush models. So **5,629 props that write `solid 6` are drawn and walked through**. |
| `CDynamicProp`'s glow block — `m_bShouldGlow`, `m_clrGlow`, `m_nGlowStyle`, `SetGlowEnabled`/`SetGlowDisabled`/`SetGlowColor`/`GlowColor{Red,Green,Blue}Value`, `ShouldTransmit` | CS:GO's wall-hack glow; it reaches the client as a `CCSUsrMsg_GlowPropTurnOff` user message. **No shipped Portal 2 map writes `glowenabled`, `glowcolor`, `glowdist` or `glowstyle`, and no connection fires one of the six inputs**, so the class declares none of them. |
| `m_bRandomAnimator` and `SelectWeightedSequence( ACT_IDLE )` | Parsed and dead: **all 5,117 props that write `RandomAnimation` write `0`**, and `MinAnimTime`/`MaxAnimTime` are Hammer's defaults of 5 and 10 on every one of the 8,462. It is the one branch of `AnimThink` that needs the activity table. |
| `HandleAnimEvent`, `DispatchAnimEvents`, `SuppressAnimSounds`, `m_bUseHitboxesForRenderBox`, `AnimateEveryFrame`, `CalculateBlockLOS`, `BecomeRagdollOnClient` | Each parsed where it is a key, each with nothing here to drive it: anim events want sounds, the render box wants hitboxes, `AnimateEveryFrame` asks the *server* to advance the cycle more often and the server does not advance it at all, LOS wants an AI, and there are no ragdolls. `BecomeRagdoll` is declared and no shipped connection fires it. |
| `m_nSkin` and `m_nBody` | Parsed, carried across the seam and printed by `ent_dump`; not drawn. Skin families and bodygroups are `portdocs/STUDIO.md` stage 6's. **1,437 shipped connections fire `Skin` at a prop**, so this is the most-fired input in the game that lands on a field nothing reads. |
| Flex deltas (`studio/`'s) | Also not this module's, and also measured here. The 16 models `StudioModel::load` refuses are `models/props_destruction/toxin*`; **15 of them are placed as `prop_dynamic`s, by 41 entities**, and those 41 draw nothing. `portdocs/STUDIO.md` records flex deltas as "absent from the data" because no *static prop* has any — still true, and `prop_dynamic` is the first thing in the port that places a model that is not a static prop. |
| ~~`$includemodel`~~ | **Landed** in `src/studio/include.rs`, and it was measured here first: 9 of the 606 models the game's props name keep their sequences in a companion `*_animation.mdl`, and those 9 are worn by **926 entities**. Of the 2,738 props playing a sequence two seconds into their map, the labels that resolve went from 1,666 to **2,556** and the ones that do not from 897 to **182** — and that remainder is Valve's own map errors, 183 `DefaultAnim` keys naming a sequence in no model at all. `animating` rose with it, because an animation that can now *end* fires `OnAnimationDone` into the game's 5,311 `SetAnimation` connections. |
| `prop_dynamic_ornament` (`COrnamentProp`) | A prop that `FollowEntity`s another, which needs the same local/abs transform pair the `SetParent` family does. **Zero placed by any shipped map.** |
| `prop_floor_cube_button` (13), `prop_floor_ball_button` (10), `prop_under_floor_button` (13), `prop_button` (64) | The first two accept **only** cubes and balls, and `prop_weighted_cube` is not ported — so in this port they would be furniture that nothing can ever press. The other two are ordinary follow-on work: `prop_under_floor_button` is `prop_floor_button` with a bigger box and different sequence names, and `prop_button` is a separate class in `prop_button.cpp` with a timer. |
| `CPortalButtonTrigger`'s cube half — `SetActivated`, `GetCubeType`, `OnlyAcceptBall`/`AcceptsBall`, `prop_monster_box`'s `BecomeBox`/`BecomeMonster`, `sv_slippery_cube_button` | All of it needs `prop_weighted_cube`, which needs `MOVETYPE_VPHYSICS` (`ENGINE_TRACE.md` stage 5). `ShouldPlayerTouch` is asked of the owner rather than answered in the trigger, so the shape is there for it. |
| A floor button's co-op outputs — `OnPressedOrange`, `OnPressedBlue` | `GameRules()->IsMultiplayer()` and `GetTeamNumber()`. Declared so the connection parses as an output; one shipped map writes each. |
| **The player's weapon** — `weapon_portalgun` (3 placed), `trigger_weapon_strip` (2), `player_weaponstrip` (2), `CBaseCombatWeapon` | Portal 2's only weapon is the portal gun and it needs the portal system (`portdocs/SERVER.md` §1.3). |
| **The movement, still** — `CGameMovement` on the server, `CPlayerMove::RunCommand` | Stage 5 moved the *authority* (the move type, the health, the life state) and deliberately left the *integration* in `client/` on the rendered frame. §5 of the porting doc is the argument: `CPrediction` re-runs the same movement code on the client, so a one-process port with no `net/` already has the client half and would gain nothing but a 64 Hz camera by moving it. Revisit when `net/` exists. |
| `CBasePlayer::SetFogController` and `SetHUDVisibility` | 97 connections in the game fire `SetFogController` at `!player`, and there is no fog or HUD. `SetHealth`, the player's third input, **is** implemented. |
| Named think *contexts* (`m_aThinkFunctions`) | Still no class here needs two independent timers, and stage 3 is the evidence rather than the counter-example: a mover uses the think schedule **and** the arrival alarm, which are two different mechanisms with two different fields, not two contexts. The one class that genuinely wanted a context is `CBaseDoor`'s `"MovingSound"`, and there is no sound system. |
| `IGameSystem` as a registry | One system exists (`CTonemapSystem`), so it is a method. The condition is the second system that needs a level hook. |
| `FIELD_EHANDLE` and `FIELD_POSITION_VECTOR` | No class declares either. `FIELD_EHANDLE`'s two conversions both need the entity list, which `Variant::convert` has not got. |
| `AddOutput`, `SetParent`, `ClearParent` and `SetParentAttachment*` | **The condition is a real local/abs transform pair on `EntityCore`**, which this port does not have — a child's origin is the world-space one the map gave and nothing rebases it — plus `LookupAttachment` on a studio model for the attachment forms, which would be this module's first dependency on `studio/`. It is the largest single absence left: **1,078 of the depot's 1,081 unhandled inputs** are this family, 883 of them `func_brush.SetParentAttachmentMaintainOffset`. See gotcha 34. |
| `CLogicBranch::UpdateOnRemove`'s notification | Valve posts `_OnLogicBranchRemoved` at the *branch* instead of at the listener (`logicentities.cpp:2622`), so no listener in the shipped game has ever received one; a stale branch is counted as false for the rest of the level. This port reaches the same state by a different route — there is no `UpdateOnRemove` hook on `Behaviour`, and a dead id reads as false in `DoTest`. **No shipped map fires `Kill` at a `logic_branch`.** |
| `SendTable`/`DT_`/`edict_t` | One process. Deleted, not deferred. |
| Save/restore, `FTYPEDESC_SAVE` | Deferred; `serde` over entity state when it comes back, not `ISave`. |
| `ent_pause`/`ent_step` (`Debug_ShouldStep`) | 20 lines and genuinely useful; reconsider when entities do more. |
| VScript | `portdocs/SERVER.md` §9. One `RunScriptCode` reaches an implemented class. |

---

## Extending it

**To add an entity class**, in `src/server/classes/`:

1. Write the state as a struct and `impl Behaviour for` it in the right family
   file (`logic.rs`, `light.rs`, `env.rs`, `world.rs`, `brush.rs`,
   `trigger.rs`, `filter.rs`, `point.rs`, `player.rs`, `prop.rs`), with a
   `create` returning `Box<dyn Behaviour>`.
2. Add a `ClassDef` to `CLASSES` in `classes/mod.rs`, listing in `keys` exactly
   the names `key_value` consumes, in `inputs` exactly the names
   `accept_input` handles **with the field type Valve declared**, and in
   `outputs` exactly the names it fires.
3. `describe` returns whatever `ent_dump` should print.
4. If it moves, hold a `Toggle` and set `EntityCore::move_type` to
   `MoveType::Push` in `Spawn`. Everything else follows: `linear_move` and
   `angular_move` arm it, the pusher runs it, and `Behaviour::move_done` is
   where it lands — beginning with `self.toggle.move_done(entity)` (gotcha 29).
   A key the `Toggle` consumes (`lip`, `wait`, `distance`) still has to appear
   in *your* `ClassDef::keys`, because a contained class has no chain to be
   found through.
5. If it is a trigger, hold a `BaseTrigger` and call `spawn`, `init_trigger`,
   `activate`, `start_touch`, `end_touch` and `accept_input` from your own —
   the same containment rule, one class up. `InitTrigger` is what sets the
   three solidity values gotcha 35 is about, and forgetting it makes an
   ordinary solid brush that never notices anything.
6. If it *makes* another entity, do it in `Spawn` with
   `Context::create_entity`, finish building it with `Context::entity_mut` and
   `Context::behaviour_mut`, and remember that its `Spawn` runs after yours
   returns (gotchas 47-49). A box trigger also needs `EntityCore::solid =
   Solid::Obb` and a `model_bounds` you write yourself — nothing fills either
   in for an entity that came from no lump. A class that overrides
   `PassesTriggerFilters` calls `start_touch_passing`, not `start_touch`
   (gotcha 50).
7. Run `cargo test`: the two invariant tests check both declarations against the
   code, and the class table is checked for duplicates and for shadowing a base
   input.
8. Run the depot test. `EXPECTED_UNHANDLED` and the unhandled-input table will
   change — that is the point, and the change should be read before it is
   pasted in. Stage 3 added eight key names to that table and **six of them are
   not gaps**: `_minlight` and `vrad_brush_cast_shadows` are `vrad`'s,
   `inputfilter` is declared by `base.fgd` and consumed by nothing in the
   entire tree, and `filtername`/`message`/`onfullyopen` are mapper mistakes on
   classes that have no such key in any version of the server.
   `prop_floor_button` changed it by two: `skin` **stayed** at 1 because the
   class consumes its 18, and `vscripts` went from 38 to 39 because the one on
   a floor button only became visible once the classname was implemented.

**Read the FGD first.** `depot_621/bin/{base,portal,halflife2,portal2}.fgd`
declare 494 classes and cover 199 of the 200 the shipped maps place, including
every class whose C++ was cut from this tree. They are not a superset of the
datadesc (see `portdocs/SERVER.md` §1.4) but they are the fastest way to see
what a class's keys and inputs are called.

**Measure before implementing.** Every scoping number in this document came
from a script over `LUMP_ENTITIES`, and several of them invert the conclusion
you would reach by reading `game/server/` — `env_tonemap_controller` has *no
keyvalues at all*, and two thirds of `logic_case`'s use in Portal 2 ignores its
case values.

---

## Which tests guard what

| Test | Guards |
|---|---|
| `io::an_outputs_connections_fire_in_reverse_lump_order` | gotcha 3 |
| `io::an_event_posted_at_the_current_time_runs_in_the_same_pass` | gotcha 4 |
| `io::an_empty_input_is_use_and_a_zero_fire_count_is_always` | gotchas 8 and 9 |
| `io::conversions_are_valves_square_table` | gotcha 5's conversion half |
| `io::a_value_prints_the_way_printf_g_prints_it` | `logic_case`'s string compare |
| `io::the_queue_is_sorted_and_stable_for_equal_times` | the stable insert |
| `io::pending_events_match_by_handle_and_by_prefix` | `CancelEventOn`'s two quirks |
| `think::time_to_ticks_rounds_to_nearest` | gotcha 2, including the 30 Hz cliff |
| `think::a_think_tick_of_zero_never_runs` | gotcha 2's other half |
| `think::a_removed_entity_leaves_the_think_list` | `EntityChanged`'s first line |
| `think::the_tickrate_switch_quantises_to_512ths_and_clamps` | `-tickrate` |
| `random::the_same_seed_gives_the_same_stream` | gotcha 17 |
| `random::random_int_is_inclusive_at_both_ends` | gotcha 18 |
| `entity::an_entity_knows_its_own_handle` | the handle write-back |
| `entity::a_handle_to_a_removed_entity_stops_resolving` | gotcha 10 |
| `name::matching_is_case_insensitive_and_only_a_trailing_star_wildcards` | gotcha 15 |
| `name::procedural_names_resolve_from_the_io_context` | `!self` is the caller |
| `classes::every_declared_key_is_consumed_and_every_consumed_key_is_declared` | gotcha 12, keys |
| `classes::every_declared_input_is_handled_and_every_handled_input_is_declared` | gotcha 12, inputs |
| `classes::no_class_shadows_a_base_input` | `Kill`/`Use` staying `CBaseEntity`'s |
| `classes::an_unnamed_light_removes_itself_and_a_named_one_does_not` | gotcha 16 |
| `classes::env_light_inherits_the_light_keys_by_holding_one` | gotcha 13 |
| `tests::a_zero_delay_chain_completes_in_one_tick` | gotcha 4, end to end |
| `tests::logic_auto_fires_on_map_spawn_after_two_tenths_of_a_second` | the bootstrap every map uses |
| `tests::a_relay_latches_until_its_slowest_output_has_gone_out` | the refire latch |
| `tests::a_connection_with_a_fire_limit_deletes_itself` | gotcha 9 |
| `tests::an_unmatched_name_falls_back_to_the_classname` | gotcha 11 |
| `tests::a_parameter_override_drops_the_callers_extra_delay` | gotcha 7 |
| `tests::a_think_that_does_not_rearm_runs_once` | gotcha 6 |
| `tests::the_base_kill_input_removes_an_entity_at_the_end_of_the_tick` | gotcha 10 |
| `tests::a_map_sets_its_own_exposure_limits` | the whole stage, in miniature |
| `tests::the_master_tone_mapper_is_the_last_flagged_one` | `LevelInitPostEntity` |
| `tests::the_activator_is_forwarded_across_a_relay_chain` | `!activator`, `!self` |
| `tests::the_schedule_does_not_depend_on_the_frame_rate` | the fixed tick |
| `tests::pick_random_chooses_among_connected_outputs` | gotcha 20 |
| `client::tonemap::a_maps_exposure_limits_win_over_the_cvars` | the client half of the join |
| `movement::a_linear_move_arrives_on_the_tick_it_was_scheduled_for` | the pusher, end to end |
| `movement::a_move_in_progress_is_where_it_should_be` | the integration is not deferred to the arrival |
| `movement::the_alarm_runs_down_with_no_velocity_at_all` | the alarm as a wait timer (gotcha 26) |
| `movement::an_alarm_armed_for_no_delay_at_all_never_fires` | gotcha 27 |
| `movement::a_very_short_angular_move_is_stretched_to_a_hundredth_of_a_second` | `MinTravelTime`, and why it is load-bearing |
| `movement::a_move_to_where_we_already_are_starts_nothing` | gotcha 30's first half |
| `movement::dot_product_abs_is_not_the_absolute_dot_product` | gotcha 32 |
| `movement::movedir_is_angles_read_as_a_forward_vector` | gotcha 33 |
| `movement::anglemod_folds_through_sixteen_bits` | the quantised fold `func_rotating` needs |
| `classes::brush::a_rotated_box_is_measured_by_its_projection` | `RotateAABB`, and the transpose that hides at 0° and 90° |
| `damage::fractional_damage_accumulates_rather_than_truncating` | gotcha 56 |
| `damage::damage_under_one_point_is_refused_until_the_accumulator_fills` | gotcha 56's other half |
| `damage::events_only_runs_the_handlers_and_leaves_health_alone` | `DAMAGE_EVENTS_ONLY`, which no map reaches |
| `damage::health_is_restored_up_to_the_maximum_and_no_further` | `TakeHealth`'s asymmetric `DAMAGE_YES` gate |
| `damage::the_weakest_shipped_trigger_hurt_takes_twenty_doses_to_kill` | the slowest death in the game |
| `tests::a_lethal_trigger_kills_the_player_and_asks_for_the_level_back` | **the whole of stage 5, end to end** |
| `tests::god_mode_refuses_the_damage_and_the_outputs_still_fire` | gotcha 54 |
| `tests::a_damage_filter_on_the_victim_refuses_the_damage` | `PassesDamageFilter`, and `FilterDamageType`'s `==` |
| `tests::noclip_is_the_servers_and_survives_the_round_trip` | gotcha 57 |
| `tests::jumping_and_ducking_reach_the_player_proxy` | `logic_playerproxy`, and the press *edge* |
| `tests::player_loadsaved_freezes_the_player_and_restarts_the_level` | Portal 2's other way of dying |
| `tests::the_kill_command_kills_once_and_then_refuses` | `CommitSuicide`'s cooldown |
| `tests::the_health_key_is_read_and_the_shipped_value_makes_nothing_damageable` | the 682 `health` keys |
| `client::movement::a_dead_player_falls_under_gravity_and_stops_on_the_floor` | `FullTossMove` (`rustdocs/CLIENT.md`) |
| `client::movement::the_dead_view_drops_to_the_floor_and_duck_does_not_lift_it_back` | `VEC_DEAD_VIEWHEIGHT`, and the write order against `Duck()` |
| `think::a_moving_entity_is_due_every_tick` | the simulation list's two questions |
| `tests::a_door_opens_and_fires_its_arrival` | the stage, in miniature |
| `tests::a_door_with_a_wait_closes_itself_on_the_same_alarm` | gotcha 26, end to end |
| `tests::a_door_with_a_wait_of_zero_stays_open_for_ever` | gotcha 27, end to end |
| `tests::a_zero_length_open_arrives_before_it_announces_itself` | gotcha 30 |
| `tests::the_travel_is_the_model_minus_the_lip_minus_two` | gotcha 32 |
| `tests::a_rotating_door_turns_about_the_axis_its_spawnflags_name` | `AxisDir` and `SF_DOOR_ROTATE_*` |
| `tests::a_door_can_spawn_open` | `spawnpos`, the 40 doors that start open |
| `tests::a_locked_door_refuses_to_open_and_still_closes` | `InputClose` not testing `m_bLocked` |
| `tests::a_movelinear_measures_its_ends_from_where_it_was_drawn` | `startposition`, and `SetPosition` |
| `tests::a_button_presses_in_and_returns_by_itself` | the one class that waits on a think |
| `tests::a_button_that_does_not_move_still_completes_its_cycle` | `SF_BUTTON_DONTMOVE`, 53 of 64 |
| `tests::a_use_with_no_input_name_presses_a_button` | gotcha 8, now that `Use` does something |
| `tests::a_rotator_starts_stops_and_reports_its_speed` | `SetTargetSpeed`/`UpdateSpeed`/`GetSpeed` |
| `tests::a_rotator_that_starts_on_needs_no_input` | `SUB_CallUseToggle`, a think that calls `Use` |
| `tests::a_func_brush_switches_itself_off_and_on` | `StartDisabled`, `EF_NODRAW`, `Solidity` |
| `tests::a_brush_entity_is_found_by_its_model_index` | the seam `world/` and `trace/` read |
| `tests::every_brush_class_is_solid_unless_it_says_otherwise` | gotcha 36 — the bug the running game found |
| `engine::world::a_placement_can_be_moved_by_whoever_owns_it` | the other end of that seam |
| `engine::world::syncing_leaves_a_placement_nobody_owns_where_it_was` | …and what it does not touch |
| `tests::walking_into_a_trigger_fires_its_outputs` | **the stage, in one test** |
| `tests::a_trigger_that_does_not_allow_clients_ignores_the_player` | `SF_TRIGGER_ALLOW_CLIENTS` |
| `tests::a_trigger_once_fires_once_and_then_deletes_itself` | `m_flWait = -1` and `SUB_Remove` |
| `tests::a_trigger_multiple_re_arms_after_its_wait` | the think schedule as a lock-out |
| `tests::leaving_a_trigger_fires_on_end_touch` | the stamp sweep, end to end |
| `tests::only_the_trigger_side_of_a_touch_carries_the_start_flag` | gotcha 37 |
| `tests::a_trigger_that_deletes_itself_fires_no_end_touch` | gotcha 38 |
| `tests::a_disabled_trigger_notices_nothing_until_it_is_enabled` | `StartDisabled` and `Enable` |
| `tests::a_class_filter_keeps_the_player_out` | the filter 250 `trigger_multiple`s wear |
| `tests::a_negated_filter_is_the_other_way_round` | `Negated`, and Hammer writing a label |
| `tests::a_dynamic_prop_spawns_still_and_a_plain_one_is_promoted_to_an_obb` | gotchas 60 and 61 |
| `tests::the_solid_key_reaches_a_prop_and_no_other_class_writes_one` | the `solid` key, and `SF_DYNAMICPROP_DISABLE_COLLISION` |
| `tests::a_prop_with_no_model_removes_itself` | `CBaseProp::Spawn`'s first four lines |
| `tests::set_animation_plays_a_sequence_and_fires_both_of_its_outputs` | **the class, end to end** — 5,311 and 181 shipped connections |
| `tests::a_finished_animation_reverts_to_the_default_unless_the_prop_holds_it` | `DefaultAnim` (2,416) against `HoldAnimation` (857) |
| `tests::a_looping_sequence_never_finishes_and_stops_thinking` | gotcha 64, and the flag reset that had to survive it |
| `tests::set_playback_rate_rebases_the_pose_so_it_does_not_jump` | the `SetPlaybackRate` divergence; 427 shipped `-1`s |
| `tests::start_disabled_hides_a_prop_and_enable_brings_it_back` | `EF_NODRAW` carried across the seam rather than filtered |
| `tests::fade_and_kill_removes_the_prop_after_a_second` | `SUB_FadeOut`, and gotcha 63 in its comment |
| `tests::the_break_input_fires_on_break_and_removes_the_prop` | the only way `OnBreak` can fire in Portal 2 |
| `tests::only_an_override_prop_may_be_given_health_by_the_map` | `CBaseProp::KeyValue`'s one line |
| `tests::a_sequence_the_model_does_not_have_is_refused_once_the_models_are_loaded` | `PropSetAnim`'s failure branch, and `Lookup::Missing` |
| `sequences::a_model_nobody_loaded_is_not_a_model_without_the_sequence` | gotcha 62 |
| `engine::world::entities::tests::a_prop_with_no_default_anim_is_posed_by_sequence_zero` | gotcha 65 — depot-gated |
| `engine::world::entities::the_button_draws_and_moves_as_it_presses` | the pose reaching the right geometry (`rustdocs/ENGINE.md`) |
| `tests::every_shipped_prop_dynamic_plays_the_animation_its_map_asks_for` | **the depot test**: 8,462 props across 106 maps, 606 models, what flex deltas and the missing skinning each cost — and what `$includemodel` was worth, since its numbers are pinned on both sides of that change |
| `tests::filter_multi_combines_its_children` | the handler that reads other entities |
| `tests::point_teleport_sends_the_player_where_it_was_told` | `!player`, 121 of 128 |
| `tests::a_landmark_teleport_carries_the_offset_across` | the elevator, and gotcha 44 |
| `tests::a_teleport_with_no_landmark_lands_on_its_target` | the other branch |
| `tests::a_push_is_a_base_velocity_and_then_momentum` | gotchas 42 and 43 |
| `tests::a_noclipping_player_is_not_pushed` | `CTriggerPush::Touch`'s switch |
| `tests::a_hurt_trigger_fires_on_hurt_player_twice_a_second` | the half-second cadence |
| `tests::a_radiation_trigger_charges_on_the_way_out` | the one shape that reaches `EndTouch`'s dose |
| `tests::the_player_is_an_entity_and_resolves_procedurally` | `!player`, and `Kill` at it |
| `classes::trigger::matrix_angles_inverts_angle_matrix` | the landmark transform |
| `engine::trace::the_clip_chain_keeps_the_nearest_of_the_world_and_the_entities` | `TraceRay`'s rescaling |
| `engine::trace::a_tracer_with_no_entities_is_a_world_trace` | the chain is opt-in |
| `engine::trace::a_trace_that_starts_in_the_world_ignores_the_chain` | the early return |
| `engine::world::only_a_brush_model_the_game_answers_for_is_in_the_clip_chain` | gotcha 36 |
| `engine::world::the_touch_query_reports_the_models_a_swept_box_meets` | `engine->SolidMoved` |
| `obb::a_player_standing_on_the_pad_is_touching_it` | the swept box against a box, at all |
| `obb::turning_the_pad_turns_what_it_notices` | the fifteen-plane path, in both directions |
| `obb::the_fifteen_planes_are_the_separating_axis_test` | the whole sweep, against an independent SAT reference |
| `obb::a_right_angle_yaw_agrees_with_the_axis_aligned_path` | the two paths on a case where they must coincide |
| `obb::a_sweep_notices_what_a_position_test_would_miss` | it is a *sweep* |
| `obb::the_trivial_reject_never_rejects_a_real_touch` | Valve's mis-centred bounding sphere |
| `tests::a_floor_button_creates_its_own_trigger` | `Context::create_entity`, end to end — and gotchas 48 and 49 |
| `tests::standing_on_a_floor_button_presses_it_and_stepping_off_releases_it` | **the class, in one test** |
| `tests::a_press_completes_in_the_tick_it_started_in` | gotcha 51 — the extra queue hop costs no tick |
| `tests::the_thing_standing_on_the_pad_is_the_activator` | `!activator` survives that hop |
| `tests::the_press_inputs_work_with_nobody_on_the_pad` | `PressIn`/`PressOut`, 8 shipped connections |
| `tests::a_turned_pad_notices_a_turned_area` | the OBB path through the entity, not the maths |
| `tests::the_skin_key_is_read_and_then_overwritten_by_spawn` | `SetSkin` running after `KeyValue` |
| `tests::a_button_with_no_model_gets_the_default_one` | `GetButtonModelName`'s unreached branch |
| `tests::a_testchamber_door_spawns_shut_and_still` | `ResetSequence` then `SetPlaybackRate( 0 )` — why a chamber does not open itself |
| `tests::a_testchamber_door_opens_forwards_and_shuts_backwards` | gotchas 66 and 68 — **the class, end to end** |
| `tests::only_a_doors_first_opening_reports_its_own_end` | gotcha 67, the sticky `m_bSequenceFinished` |
| `tests::a_testchamber_door_refuses_an_input_it_is_already_obeying` | gotcha 70 |
| `tests::a_locked_testchamber_door_refuses_everything_and_lockopen_gets_in_first` | `Lock`/`Unlock`/`LockOpen`'s ordering |
| `tests::a_testchamber_door_with_no_model_loaded_opens_but_never_arrives` | gotcha 62 applied to a second class |
| `tests::the_area_portal_keys_are_consumed_including_the_two_broken_ones` | the four keys, and the two Hammer instance-fixup leftovers |
| `studio::the_testchamber_door_model_animates` | the facts the class is written against: `open` 23 frames against `close`'s 36, non-looping, `fadeouttime` 0.2, every vertex on one bone |
| `engine::world::entities::the_chamber_door_draws_and_opens` | **the model drawn**: the doorway covered when shut and clear when open, the rings turning inside the door, the leaves travelling 53 units each |
| `tests::every_shipped_map_spawns_its_entities` | **everything, against all 106 maps** |
| `tests::every_shipped_maps_triggers_notice_the_player` | **every brush trigger in the game, touched** |
| `tests::every_shipped_floor_button_presses_when_stood_on` | **every floor button in the game, stood on** |
| `tests::every_shipped_testchamber_door_opens_and_shuts` | **every chamber door in the game, opened and shut** |
| `tests::a_branch_listener_fires_only_when_the_verdict_changes` | gotchas 71 and 72 — **the class, end to end** |
| `tests::test_forces_a_branch_listener_to_report_and_an_empty_one_is_mixed` | gotcha 73, and `InputTest`'s `NOT_INIT` reset |
| `tests::the_intro_maps_door_opens_through_the_chain_its_map_built` | the default map's own `trigger → proxy → relay → door` chain, **both ways** — the `Close` half runs through a `logic_branch_listener` |
| `tests::every_shipped_branch_listener_registers_and_shuts_the_doors_it_is_for` | **every branch listener in the game, registered and driven** |
| `tests::a_portal_spawns_as_a_one_sided_box_trigger` | gotchas 76, 78 and 79 — the model that must not be drawn, the box that reaches forward only, and the half-height that is 56 |
| `tests::two_active_portals_in_a_group_find_each_other` | the linkage, both ways, and that the colours end up opposite |
| `tests::linking_two_portals_of_one_colour_flips_the_first` | gotchas 74 and 75 — `PortalTwo` overwritten, and *which* of the two keeps its colour |
| `tests::deactivating_a_portal_hands_its_partner_on` | the one real recursion in `UpdatePortalLinkage`, which needs three portals to be visible at all |
| `tests::the_teleport_matrix_turns_a_point_around_the_exit` | gotcha 80 — the round trip, the crossing, the velocity, and a portal linked to a copy of itself |
| `tests::new_location_moves_a_portal_and_activates_it` | gotcha 77, the activation hidden in the middle of `NewLocation` |
| `tests::resizing_one_portal_of_a_pair_unlinks_it` | that the partner search compares both half-extents exactly |
| `tests::placing_a_pair_by_hand_creates_and_links_two_portals` | the `portal` command's server half: `FindPortal` preferring an active match, and `fizzle_portals` |
| `tests::every_shipped_portal_spawns_and_its_map_can_link_a_pair` | **all 21 portals in the game, switched on by their own maps' logic** |
| `tests::every_shipped_portal_is_on_a_wall` | **the deleted placement snap, measured**: 15 flush, 2 proud, 4 floating — and not one more than 0.00 degrees off its surface |

Every depot test is `--ignored` and gated on `KISAK_GAME_DIR`:

```
KISAK_GAME_DIR=/path/to/portal2 cargo test --release every_shipped_map -- --ignored --nocapture
KISAK_GAME_DIR=/path/to/portal2 cargo test --release triggers_notice -- --ignored --nocapture
KISAK_GAME_DIR=/path/to/portal2 cargo test --release floor_button_presses -- --ignored --nocapture
KISAK_GAME_DIR=/path/to/portal2 cargo test --release testchamber_door -- --ignored --nocapture
KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_intro_maps_door -- --ignored --nocapture
KISAK_GAME_DIR=/path/to/portal2 cargo test --release branch_listener -- --ignored --nocapture
KISAK_GAME_DIR=/path/to/portal2 cargo test --release every_shipped_portal -- --ignored --nocapture
```

The first loads all 106 maps, spawns a player in each, runs **two seconds of
server time**, and asserts exact totals: 60,925 blocks, 34,823 matched, 65
created, 27,951 spawned, 6,937 lights deleted, 213 kept, 54,535 connections,
159 unimplemented classnames, the full 48-name unhandled-key table, 5,787
events dispatched, 3,932 inputs accepted, 7,318 thinks, 1,122 events that found
no target, zero bad conversions, the 18-name unhandled-input table, a peak of
215 entities in the simulation list at once, 2,341 live triggers, 105 maps with
a master tone mapper — and that `sp_a1_intro1` ends up asking for an exposure
ceiling of **1.5**.

> **`prop_portal` moved six of those and every one of them is the same 21
> entities.** `matched` and `spawned` rose by 21 and the unimplemented set lost
> its one portal classname, taking 26,123 occurrences to 26,102. `outputs` rose
> by **one**, which is the whole of the class's output surface in the shipped
> game — `sp_a1_intro1`'s `portal_red_0.OnPlayerTeleportFromMe`, connected by
> no other map. `accepted` rose by 2 and `no_target` fell by the same 2, which
> is a `SetActivatedState` that used to be aimed at a classname nothing
> answered for; only two of the game's 31 are fired inside two seconds, because
> a portal is switched on when the player reaches the room. And the live-trigger
> total went from 2,320 to **2,341**: a portal is `FSOLID_TRIGGER` from
> `Spawn`, whether or not it is `Activated`. The unhandled-key table did *not*
> move, because an unmatched block's keys were never in it.

> **Four of those numbers moved a long way with `prop_dynamic` and each says
> something.** `accepted` and `thinks` rose because a prop that is given an
> animation wakes at 10 Hz until it has finished one; `no_target` more than
> halved because most events used to reach nothing for the single reason that
> most *targets* were props; and the peak think count went from 48 to 214,
> which is the first time that number has said anything about the shape of the
> list rather than about the `logic_auto` bootstrap.

> **`thinks` then went from 4,420 to 7,318 with `prop_testchamber_door`, and
> all 2,898 of those are doors.** `AnimateThink` re-arms unconditionally, which
> is Valve's, so all 138 wake ten times a second for the whole level and never
> leave the simulation list. That is a deliberate non-divergence and gotcha 69
> says why.

`prop_testchamber_door` added a depot test of its own, and its numbers are the
argument for the class: **138 doors across 71 of the 106 maps, 130 of them
opened by something in their own map.** Of the other eight, five carry no
`targetname` at all and three are named and never fired at — Valve's dead map
data, shut in the shipped game too. Seven are opened by their own map's
bootstrap within a second of the level starting, so "a chamber door waits for
the player" is nearly but not quite a rule. Every one of the 131 driven doors
reports the same two times, because the whole schedule is quantised:
**0.797 s for the first `OnFullyOpen` against a 0.9167-second travel, and
0.094 s — one think — for every "fully" output after it** (gotcha 67).

Stage 3 added its own three numbers to that list, and they are the ones that
say the stage works: **3,410 brush entities have a class, 67 of them are
somewhere other than where the entity lump put them after two seconds, and 34
are still travelling when the clock stops.** 67 is small because a Portal 2 map
starts with its doors shut and what moves in the first two seconds is the
handful of panels and lifts a chamber opens with. What matters is that before
stage 3 the number was zero, and that the 34 mean the simulation list is being
entered and left rather than filled once.

Stage 4 added two more numbers to the first test and a second test entirely.
**6,302 brush entities now have a class — double stage 3's — and 2,255 of them
are live triggers two ticks into the map**: the 2,892 the maps place minus the
637 that are `StartDisabled`, including 107 of the game's 110
`trigger_teleport`s. `prop_floor_button` then took the live-trigger total to
**2,320**, because the other 65 are `SOLID_OBB` and have no brush model at
all, and `prop_portal` takes it to **2,341** for the same reason — a portal is
a `SOLID_OBB` trigger from `Spawn`, active or not.

`every_shipped_maps_triggers_notice_the_player` is the one that could not be
faked. Per map it builds the real collision, and then, **for every one of
those 2,255 triggers**, reloads the level, finds a point inside the trigger's
*actual brushes* that a 32×32×72 hull fits in, puts a player there and runs two
ticks through the same `ClipRayToCollideable` sweep the running game uses.
**2,246 notice; 1,888 of them dispatch something; 3 have no point a standing
player fits in and 6 are switched off or deleted by the map's own bootstrap
before the second tick.** Three things fail loudly here and are invisible to
every synthetic test: a wrong `"*N"` join, a trigger whose `FSOLID_TRIGGER`
never got set, and a swept-box test that answers for the bounding box rather
than the brushes.

> Nine of that 1,888 are not the brush trigger's doing: **21 of the probe
> points also stand on a `prop_floor_button`**, and twelve of those belong to
> triggers that were already firing something. The test counts the 21 as well,
> so the number is explained rather than absorbed.

**`every_shipped_floor_button_presses_when_stood_on` is the `SOLID_OBB` half of
the same claim**, split out because it is answered by different code and needs
no collision data at all. Per map it finds every `prop_floor_button`, reloads
the level for each, puts a player's hull centre on the pad's box centre — which
is inside it whichever way the pad faces, and some of them are on walls — and
then walks away. **65 buttons in 47 maps, 23 of them turned; 65 press and 65
release.** There is no room for a partial answer here: a wrong `angle_matrix`
convention, a trigger that never got created, a missing `Solid::Obb` and a
fifteen-plane sweep that disagrees with the axis-aligned one all fail it.

**Where to actually see the movers.** No *single-player* map moves a brush
entity in the first twenty seconds of server time — a Portal 2 chamber starts with everything
shut and waits for the player, which is why `sp_a1_intro1` looks identical to
before. The **co-op** maps do, because their airlock doors and exit fans are
started by the `logic_auto` bootstrap: `mp_coop_fan` spins `brush_fan` and opens
`security_3_door_left`/`_right`, `mp_coop_lobby_2` slides eleven
`func_movelinear` screen panels, and every `mp_coop_paint_*` map opens an
airlock. Those are the maps to load when changing this code.

**`every_shipped_trigger_hurt_kills_the_player_standing_in_it` is stage 5's
depot test**, and the sibling of the trigger one above. Per map it builds the
real collision, and then for each of the game's 215 `trigger_hurt`s it reloads
the level, finds a point inside the trigger's *actual brushes* that a 32×32×72
hull fits in, puts a player there and runs sixteen seconds of server time
without moving it. **138 kill the player**; the fastest kills on the first tick
and the slowest takes 9.78 seconds, which is the one `damage 10` trigger in the
game working exactly as its arithmetic says. All 138 deaths reach
`RespawnPlayer` and ask the engine for the level back, three seconds later.

The other 77 are each accounted for and none of them is a failure: **72 are
never touched** (73 of the 215 carry `StartDisabled 1`, and a disabled trigger
is `FSOLID_NOT_SOLID` without `FSOLID_TRIGGER`, so nothing can touch it until
the map sends an `Enable`; one of the 73 is switched on by its own map's
bootstrap inside the sixteen seconds and then refuses for the next reason),
**4 are touched and refuse** (`PassesTriggerFilters` — five in the game carry no
`SF_TRIGGER_ALLOW_CLIENTS`, and one names an `npc_bullseye` filter), and **1 has
no point a standing player fits in**.

**Where to actually see a death.** **`sp_a1_intro1` places no `trigger_hurt`
at all**, so the default map cannot kill you. The nearest one that can is
**`sp_a1_intro5`** — which is already the map to load for the floor button —
and 23 of the 106 single-player maps have at least one that is switched on. Or
type `kill`, which takes the same path from `Event_Killed` onwards.

**What `sp_a1_intro1` does have is the `logic_playerproxy`**, and it is the
only map in the game whose proxy is connected to anything: jumping fires three
relays and ducking fires two. That is the stage-5 behaviour to watch on the
default map.

**`every_shipped_branch_listener_registers_and_shuts_the_doors_it_is_for` is
`logic_branch_listener`'s, and it exists because the 106-map census cannot see
the class at all.** In the first two seconds of a level **not one
`logic_branch` in the game changes value** — a chamber door shuts after the
player has walked through it, which is minutes in — so `every_shipped_map`'s
event, input and think totals are *identical* with the class registered and
with it disabled. The measurement had to drive the maps instead: per map it
opens every chamber door, sets every `logic_branch` true, and reads back what
each listener reported.

**158 listeners across 46 maps; 350 `Branch*` keys written and 350 resolved;
157 of the 157 that survive their map's bootstrap report a verdict.** That last
number is the check with teeth — a listener still on `NOT_INIT` either failed
to register in `Activate` or was never told a branch had moved, and either one
leaves a chamber door open for ever. Of the 157, only **56 end up all-true**,
and that is the map logic working rather than a fault: a door's
`OnAllTrue → logic_relay` chain ends by setting the very branch that asked for
it back to `0`, so the listener is talked straight back down to mixed inside
the same tick. **99 of the game's 138 chamber doors are on one of these 46
maps; all 99 take an `Open`, and 79 are shut again by a listener going
all-true.**

> That "same tick" is worth keeping in hand, because it makes the obvious
> assertion wrong: after driving `sp_a1_intro1`'s close chain the branch reads
> `false` and the listener reads `mixed`, which is indistinguishable from a
> chain that never arrived. Check the **door**.
