# `src/server/` — API reference

The game server: the map's entity list, and the I/O that runs it. Valve's
`server.so`, reduced to the framework — `CBaseEntity`, `CGlobalEntityList`, the
datadesc, the keyvalue parse, the three-pass spawn, `CEventQueue`, `AcceptInput`
and the think schedule. Porting doc:
[`portdocs/SERVER.md`](../portdocs/SERVER.md).

| | |
|---|---|
| Status | **Stage 4 of 5, plus `prop_floor_button`.** Entities spawn, fire outputs at each other, think on a fixed tick, the brush ones move, **the map notices the player** — and **a pad you stand on presses**. |
| Depends on | `engine::world::bsp::{Entity, Model}` (the parsed lumps), `engine::console` (four commands), `client::tonemap::TonemapSettings` (what `env_tonemap_controller` produces) |
| Names no | `wgpu`, `winit`, `egui`, `materials`, `studio`, `engine::trace` — every test runs with no GPU |
| Tests | 143 unit tests + three depot tests over all 106 shipped maps |

**What does not exist yet**: the player as a *whole* entity — stage 5 —
movement, health, death and `noclip`'s home are still `client/`'s
([What is deliberately absent](#what-is-deliberately-absent)). Thirty-five of
the 200 classnames the shipped maps place are implemented, plus one that no map
places, and
**nothing pushes what is in its way**: a door moves through the player rather
than shoving it (`portdocs/SERVER.md` stage 3 says why).

---

## Quick start

```rust
use crate::server::Server;

let mut server = Server::new();
// Both lumps come from the `.bsp`, parsed by `engine::world`: the entities,
// and the model bounding boxes a mover measures itself against.
let stats = server.level_init("sp_a1_intro1", &world.entities, &world.models);
eprintln!("{}", stats.summary());
// 250 of 598 entity blocks matched a class, 228 spawned (22 removed themselves),
// 714 outputs, 348 unknown classnames, 259 unhandled keys

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

    // stage 4 — the player
    pub fn spawn_player(&mut self, state: PlayerState) -> EntityId;
    pub fn player(&self) -> Option<EntityId>;
    pub fn set_player_state(&mut self, state: PlayerState);
    pub fn player_state(&self) -> Option<PlayerState>;
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
    pub origin: Vec3,          // the FEET
    pub angles: Vec3,          // the VIEW angles, pitch/yaw/roll
    pub velocity: Vec3,
    pub base_velocity: Vec3,   // what a trigger_push is adding
    pub on_ground: bool,       // FL_ONGROUND
    pub noclip: bool,          // MOVETYPE_NOCLIP rather than MOVETYPE_WALK
    pub mins: Vec3,            // the collision hull, relative to origin
    pub maxs: Vec3,
}
```

The player, as the two halves of the port that own pieces of it agree to
describe one. `client::Player` moves on the **rendered frame** and the server
ticks at a fixed 64 Hz, so neither can hold the other's state; `Engine::frame`
copies this in before the ticks and out after them.

**The round trip is an identity for every field the server did not touch**,
which is what makes an unconditional copy-back safe rather than a fight over
who owns the origin — and it is why a teleport is an ordinary field write on
the player entity rather than a message.

### `ModelEntityState` (`mod.rs`)

```rust
pub struct ModelEntityState {
    pub model: String,            // models/props/portal_button.mdl
    pub origin: Vec3,
    pub angles: Vec3,
    pub skin: i32,
    pub sequence: &'static str,   // the label; "" is the bind pose
    pub anim_time: f32,           // the SERVER's clock — see below
}
```

Every entity that draws a studio model, and what its model is doing — the
counterpart of [`brush_entity`](#the-brush-entity-seam), which answers "where is
brush model `N`". `engine::world::entities::EntityModels` loads from it once and
syncs against it every frame.

Excluded, and each for its own reason: a class that returns no `ModelState`
(35 of the 36, because a model is the exception); an entity whose `model` is a
`"*N"` brush model, which goes out through the other seam; and `EF_NODRAW`,
which is what `StartDisabled` sets.

**The list is positional and the order is the contract** — the `n`th entry has
to stay the `n`th, which holds because the order is slot order and nothing
creates or destroys a model entity after the spawn pass.

> **`anim_time` is the *server's* clock and the renderer measures against the
> scene's** (gotcha 1). The two track each other and differ by at most one tick,
> because the server's is the scene's quantised down; the renderer clamps a
> negative elapsed time to zero, so the worst case is one frame of an animation
> not having started yet.

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

The parse-side progress metric. Across all 106 maps it is 26,026 of 60,925
blocks matched, 65 created and 19,154 spawned.

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
    pub take_damage: bool,             // m_takedamage != DAMAGE_NO
    pub touch_links: Vec<TouchLink>,   // the TOUCHLINK list
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
    fn model_state(&self) -> Option<ModelState>;         // CBaseAnimating's, networked
}

/// `m_nSequence` / `m_flAnimTime` / `m_nSkin`, as much of `CBaseAnimating` as
/// anything reads. The sequence is a **label**, not an index, because looking
/// one up needs the `.mdl` and this module names no studio type.
pub struct ModelState { pub sequence: &'static str, pub anim_time: f32, pub skin: i32 }

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
    pub fn find_target(
        &self, query: &str, searching: Option<EntityId>,
        activator: Option<EntityId>, caller: Option<EntityId>,
    ) -> Option<EntityId>;                               // procedurals included
    pub fn filters(&self) -> Filters<'_>;
}

/// The read-only view a `filter_*` class evaluates against.
pub struct Filters<'a> { /* private */ }
impl Filters<'_> {
    pub fn passes(&self, filter: EntityId, caller: &EntityCore, other: &EntityCore) -> bool;
    pub fn find(&self, name: &str) -> Option<EntityId>;  // …and it must BE a filter
}

pub struct PointEntity;   // CPointEntity — no state, no behaviour
```

All fourteen trait methods have defaults, so a class with no state is
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
seconds of every shipped map is 5,763 events dispatched, 2,070 inputs accepted,
1,197 thinks, 3,700 events that reached nothing and **zero** bad conversions.

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
pub struct Branch { pub value: bool }
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
// classes/prop.rs — Portal 2's own, and the first entity that makes another
pub struct FloorButton { pub pressed: bool, pub skin: i32 }
pub struct ButtonTrigger { /* private; the SOLID_OBB box over a pad */ }
```

Thirty-six classnames, **26,026 of the shipped game's 60,925 entities** —
thirty-five of which the maps place, plus `trigger_portal_button`, which no map
places and every `prop_floor_button` makes:

| classname | C++ | instances |
|---|---|---:|
| `logic_relay` | `CLogicRelay` | 8,082 |
| `light`, `light_spot`, `light_directional`, `light_glspot` | `CLight` | 7,125 |
| `func_instance_io_proxy` | `CFuncInstanceIoProxy` | 1,184 |
| `logic_auto` | `CLogicAuto` | 1,112 |
| `logic_branch` | `CLogicBranch` | 601 |
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
| `prop_floor_button` | `CPropFloorButton` | 65, in 47 maps |
| `trigger_portal_button` | `CPortalButtonTrigger` | **0 placed** — one per button, 65 |
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
entities that make other entities** — if a door is in the wrong place or at the
wrong time start at 26, if a trigger does not fire start at 35, and if
something an entity built is not there start at 47.

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
| Entities created *by* a spawn | Bounded only by the stack | Bounded at 4,096 per dispatch, then dropped with a warning | Same shape as the zero-delay event chain's bound. The most any map creates is four. |
| `Enable`/`Disable`/`Toggle` on a trigger | Calls `PhysicsTouchTriggers()` at once, so enabling a trigger you are standing in fires `OnStartTouch` in the same tick | Fires it on the **next** tick | The touch pass is player-driven and runs at one fixed point in the tick. At most 15.6 ms late; the condition for closing it is a touch query the server can ask mid-tick. |
| `!player_blue` / `!player_orange` | `GetGlobalTeam( … )->GetPlayer( 0 )` | Reported as "no such player" | Single player has no teams, so Valve answers null here too — this is a report line rather than a divergence, and it is 74 of the depot's unhandled procedurals. |

---

## What is deliberately absent

| | Why |
|---|---|
| **Pushing what is in the way** — `CPhysicsPushedEntities`, `Blocked`/`StartBlocked`/`EndBlocked`, `m_bDoorGroup`, `forceclosed`, `dmg`/`BlockDamage` | ~1,000 lines of speculative push and rollback, and it wants `ENGINE_TRACE.md` stage 4 underneath it. A door that moves through the player is a better state than a door that does not move. The keys are parsed so they are not counted as unknown; nothing reads them. |
| `SOLID_*` — `SOLID_BSP` versus `SOLID_VPHYSICS`, and `solidbsp` | Nothing chooses between *those two*: for a brush entity they are the same brushes. `SOLID_OBB` is different and is now real — it decides that a shape is answered by `obb` rather than by the engine (gotcha 48). |
| **Damage** — health, `CTakeDamageInfo`, `TakeDamage`, death, and the physics force a hurt imparts | There is no health on anything. `trigger_hurt` therefore runs the whole of Valve's *timing* — the half-second think, the radiation quarter-second one, the doubling model's arithmetic, the parting half-dose — and takes nothing away, so `OnHurt` and `OnHurtPlayer` fire exactly when the shipped game fires them. The condition is `CBasePlayer`'s state, which is stage 5's. |
| Pushing a *physics object* — `SF_TRIGGER_ALLOW_PHYSICS`, `SF_TRIGGER_PUSH_USE_MASS`, `ApplyForceCenter` | `MOVETYPE_VPHYSICS` needs `rapier` (`ENGINE_TRACE.md` stage 5). 147 `trigger_multiple`s in the game are physics-only and correctly refuse the player. |
| NPCs and vehicles in `PassesTriggerFilters` — the `FL_NPC` sub-tests, `IsInAVehicle` | Portal 2 has 293 NPCs of 6 classnames and no vehicles at all. The `FL_NPC` term of the disjunction is kept so the line reads like the C++; the two `IN_VEHICLES` refusals are kept because they *refuse* rather than allow, and zero shipped triggers set either flag. |
| `filter_activator_team`, `filter_enemy`, `filter_size`, `filter_activator_mass_greater`, `filter_activator_context` | Zero placed by any shipped map. |
| `trigger_look` (41), `trigger_playerteam` (645), `trigger_catapult` (185), `trigger_portal_cleanser` (371), `trigger_transition` (62), `trigger_autosave` (57), `trigger_ping_detector` (20) | Either reconstruction jobs with no C++ in this tree (`portdocs/SERVER.md` §1.3) or wanting a subsystem that does not exist — saves, co-op teams, the paint system. |
| `CTriggerHurt`'s geiger counter, and `CTriggerTeleport`'s `CheckDestIfClearForPlayer` | A client-side HUD element, and `g_pGameRules->IsSpawnPointValid`. Zero shipped maps set the second. |
| Sound — `noise1`/`noise2`/`startclosesound`/`closesound`/`StartSound`/`StopSound`/`sounds`/`message`, the lock sentences, `MovingSoundThink` | There is no sound system. The names are parsed and printed by `ent_dump`; `MovingSoundThink` is a *named think context*, which is also not ported and is the only thing in the game that wanted one. |
| `CBaseDoor::Activate`'s movement group and `UpdateAreaPortals` | `m_bDoorGroup` is read only by `Blocked`, above; area portals are the engine's visibility system, which is not written. |
| `CBaseDoor::DoorActivate`, `DoorTouch`, `ChainUse`, `ButtonTouch`, `ButtonResponseToTouch`, `OnTakeDamage` | The touch and damage entry points. Stage 4's and later; nothing reaches them through I/O. |
| Which way a rotating door swings away from you (`DoorGoUp`'s 40-line cross product) | It needs the activator's position, and the activator is a player in every case that reaches it. Without one Valve's `sign` stays `1.0`, which is the branch every door in Portal 2 takes because every door in Portal 2 is opened by I/O. |
| `func_door`'s `SetToggleState` input | Declared `FIELD_FLOAT` and read with `value.Int()` (`doors.cpp:495`), which `variant_t` answers with **zero** for a float — so in the shipped game it always means `TS_AT_TOP`. Zero shipped connections fire it. |
| `CBaseToggle`'s `master` / `UTIL_IsMasterTriggered` | The `multisource` interlock. **No shipped Portal 2 map sets a `master` key on any of these classes.** |
| `SF_DOOR_START_OPEN_OBSOLETE` | **No shipped map sets it.** The 40 doors that spawn open use `spawnpos 1`. |
| `func_rot_button` (2), `momentary_rot_button` (1), `func_tracktrain` (233), `func_tanktrain` (20) | The remaining movers. `CBaseButton`'s `m_fRotating` branch is `CRotButton`'s and is therefore dead here; the trains need `path_track`. |
| `AnimateThink`, and the rest of `CDynamicProp` — bone followers, `VPhysicsInitStatic`, prop data, LOS blocking, fade distances | **The model and its animation are not absent any more** — a floor button draws and its plate presses. What is still not scheduled is the 10 Hz `AnimateThink`, and that is now a saving: its body is `StudioFrameAdvance`, which the renderer does for itself from `m_flAnimTime` and does *smoothly*, where a 10 Hz think would step it. So 65 entities do not wake ten times a second and no button sits in the simulation list for ever. `m_nSkin` is parsed and printed and not drawn; skin families are `portdocs/STUDIO.md` stage 6's. |
| `prop_floor_cube_button` (13), `prop_floor_ball_button` (10), `prop_under_floor_button` (13), `prop_button` (64) | The first two accept **only** cubes and balls, and `prop_weighted_cube` is not ported — so in this port they would be furniture that nothing can ever press. The other two are ordinary follow-on work: `prop_under_floor_button` is `prop_floor_button` with a bigger box and different sequence names, and `prop_button` is a separate class in `prop_button.cpp` with a timer. |
| `CPortalButtonTrigger`'s cube half — `SetActivated`, `GetCubeType`, `OnlyAcceptBall`/`AcceptsBall`, `prop_monster_box`'s `BecomeBox`/`BecomeMonster`, `sv_slippery_cube_button` | All of it needs `prop_weighted_cube`, which needs `MOVETYPE_VPHYSICS` (`ENGINE_TRACE.md` stage 5). `ShouldPlayerTouch` is asked of the owner rather than answered in the trigger, so the shape is there for it. |
| A floor button's co-op outputs — `OnPressedOrange`, `OnPressedBlue` | `GameRules()->IsMultiplayer()` and `GetTeamNumber()`. Declared so the connection parses as an output; one shipped map writes each. |
| **The player as a whole entity** — `CBasePlayer`'s 9,940 lines: health, death, the weapon, the view, the suit, and `noclip`'s home (`portdocs/CLIENT.md` §9.2) | Stage 5. What stage 4 added is the *minimum* touch needs — a box with `FL_CLIENT` set whose position arrives as `PlayerState` — because a touch is a fact about two entities and building it against something outside the list would have been building a different system. `client::Player` still owns the movement and still runs on the rendered frame. |
| `CBasePlayer::SetFogController` and the rest of the player's own inputs | 97 connections in the game fire `SetFogController` at `!player`, and there is no fog. It is the largest single entry in the depot's unhandled-input table now that `!player` resolves. |
| Named think *contexts* (`m_aThinkFunctions`) | Still no class here needs two independent timers, and stage 3 is the evidence rather than the counter-example: a mover uses the think schedule **and** the arrival alarm, which are two different mechanisms with two different fields, not two contexts. The one class that genuinely wanted a context is `CBaseDoor`'s `"MovingSound"`, and there is no sound system. |
| `IGameSystem` as a registry | One system exists (`CTonemapSystem`), so it is a method. The condition is the second system that needs a level hook. |
| `FIELD_EHANDLE` and `FIELD_POSITION_VECTOR` | No class declares either. `FIELD_EHANDLE`'s two conversions both need the entity list, which `Variant::convert` has not got. |
| `AddOutput`, `SetParent`, `ClearParent` and `SetParentAttachment*` | **The condition is a real local/abs transform pair on `EntityCore`**, which this port does not have — a child's origin is the world-space one the map gave and nothing rebases it — plus `LookupAttachment` on a studio model for the attachment forms, which would be this module's first dependency on `studio/`. It is the largest single absence left: **1,078 of the depot's 1,081 unhandled inputs** are this family, 883 of them `func_brush.SetParentAttachmentMaintainOffset`. See gotcha 34. |
| `logic_branch_listener` | It is the first thing in the game that needs a handler to read *another* entity during dispatch — the condition that changes `Context`'s shape. |
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
| `tests::every_shipped_map_spawns_its_entities` | **everything, against all 106 maps** |
| `tests::every_shipped_maps_triggers_notice_the_player` | **every brush trigger in the game, touched** |
| `tests::every_shipped_floor_button_presses_when_stood_on` | **every floor button in the game, stood on** |

Both depot tests are `--ignored` and gated on `KISAK_GAME_DIR`:

```
KISAK_GAME_DIR=/path/to/portal2 cargo test --release every_shipped_map -- --ignored --nocapture
KISAK_GAME_DIR=/path/to/portal2 cargo test --release triggers_notice -- --ignored --nocapture
KISAK_GAME_DIR=/path/to/portal2 cargo test --release floor_button_presses -- --ignored --nocapture
```

The first loads all 106 maps, spawns a player in each, runs **two seconds of
server time**, and asserts exact totals: 60,925 blocks, 26,026 matched, 65
created, 19,154 spawned, 6,937 lights deleted, 213 kept, 53,382 connections,
166 unimplemented classnames, the full 42-name unhandled-key table, 5,766
events dispatched, 2,480 inputs accepted, 1,450 thinks, 2,548 events that found
no target, zero bad conversions, the 11-name unhandled-input table, a peak of 48
entities in the simulation list at once, 105 maps with a master tone mapper —
and that `sp_a1_intro1` ends up asking for an exposure ceiling of **1.5**.

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
all.

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
