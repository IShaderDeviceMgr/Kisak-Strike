# `src/server/` — API reference

The game server: the map's entity list, and the I/O that runs it. Valve's
`server.so`, reduced to the framework — `CBaseEntity`, `CGlobalEntityList`, the
datadesc, the keyvalue parse, the three-pass spawn, `CEventQueue`, `AcceptInput`
and the think schedule. Porting doc:
[`portdocs/SERVER.md`](../portdocs/SERVER.md).

| | |
|---|---|
| Status | **Stage 2 of 5.** Entities spawn, fire outputs at each other, and think on a fixed tick. |
| Depends on | `engine::world::bsp::Entity` (the parsed lump), `engine::console` (four commands), `client::tonemap::TonemapSettings` (what `env_tonemap_controller` produces) |
| Names no | `wgpu`, `winit`, `egui`, `materials`, `studio` — every test runs with no GPU |
| Tests | 78 unit tests + one depot test over all 106 shipped maps |

**What does not exist yet**: movement and `MOVETYPE_PUSH` (stage 3), touch and
triggers (stage 4), the player as an entity (stage 5). Sixteen classnames are
implemented out of the 200 the shipped maps place.

---

## Quick start

```rust
use crate::server::Server;

let mut server = Server::new();
// `world.entities` is the `.bsp`'s entity lump, parsed by `engine::world`.
let stats = server.level_init("sp_a1_intro1", &world.entities);
eprintln!("{}", stats.summary());
// 185 of 598 entity blocks matched a class, 163 spawned (22 removed themselves),
// 618 outputs, 413 unknown classnames, 212 unhandled keys

// Once per rendered frame. Runs zero or more fixed server ticks.
server.frame(frame_time_seconds);

// What the map's master env_tonemap_controller is asking for.
let settings = server.tonemap_settings();

server.level_shutdown();
```

In the running engine this is wired through `Scene`, so a `map` command does the
load and `Engine::frame` does the tick: see `Level::load` and `Engine::frame` in
`src/engine/mod.rs`. Four console commands read or drive the result —
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
    pub fn level_init(&mut self, map: &str, blocks: &[bsp::Entity]) -> LevelStats;
    pub fn level_shutdown(&mut self);
    pub fn frame(&mut self, frame_time: f32) -> u32;
    pub fn time(&self) -> think::Time;
    pub fn tonemap_settings(&self) -> TonemapSettings;
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

`frame` takes the host's already-clamped frame time and returns how many server
ticks it bought. **Zero is the normal answer** at a high frame rate.

### `LevelStats` (`mod.rs`)

```rust
pub struct LevelStats {
    pub blocks: usize,            // entity-lump blocks
    pub matched: usize,           // …whose classname is implemented
    pub spawned: usize,           // alive after the spawn pass
    pub removed_on_spawn: usize,  // deleted themselves in Spawn
    pub outputs: usize,           // connections parsed
    pub parented: usize,
    pub parents_missing: usize,
    pub unknown: BTreeMap<String, usize>,    // classname -> count
    pub unhandled: BTreeMap<String, usize>,  // key name (lowercased) -> count
}

impl LevelStats { pub fn summary(&self) -> String; }
```

The parse-side progress metric. Across all 106 maps it is 19,229 of 60,925
blocks matched and 12,292 spawned. The *run*-side metric is
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
    /* private: id, next_think_tick */
}

impl EntityCore {
    pub fn classname(&self) -> &'static str;
    pub fn id(&self) -> EntityId;                     // GetRefEHandle
    pub fn debug_name(&self) -> &str;                 // targetname, else classname
    pub fn has_spawn_flags(&self, flags: u32) -> bool;
    pub fn remove(&mut self);                         // UTIL_Remove( this )

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
}
```

`EntityId` is `CBaseHandle`: a generational index, so a handle to a removed
entity resolves to `None` rather than to whatever took its slot. `insert` writes
the handle back into the entity, because everything an entity does to the world
names itself as the caller.

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
    entity: &mut EntityCore, input: &Input<'_>, cx: &mut Context<'_>,
) -> bool;

pub enum SpawnResult { Ok, Remove }
pub const NEVER_THINK: f32;            // TICK_NEVER_THINK as a time, -1

pub trait Behaviour: Any {
    fn key_value(&mut self, entity: &mut EntityCore, key: &str, value: &str) -> bool;
    fn spawn(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) -> SpawnResult;
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>);
    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>);
    fn accept_input(
        &mut self, entity: &mut EntityCore, input: &Input<'_>, cx: &mut Context<'_>,
    ) -> bool;
    fn describe(&self) -> Vec<(&'static str, String)>;   // for `ent_dump`
}

impl dyn Behaviour {
    pub fn downcast_ref<T: Behaviour>(&self) -> Option<&T>;
}

pub struct Context<'a> {
    pub time: Time,     // curtime, tick, interval — see think.rs
    /* private: the event queue and the random stream */
}

impl Context<'_> {
    pub fn curtime(&self) -> f32;
    pub fn random(&mut self) -> &mut RandomStream;
}

pub struct PointEntity;   // CPointEntity — no state, no behaviour
```

All six trait methods have defaults, so a class with no state is
`impl Behaviour for Thing {}`.

**`Context` is small, and that is the finding.** `portdocs/SERVER.md` §7.2
expected it to carry the entity list so a handler could "fire an output, find by
name, remove an entity, trace", and §10.3 flagged the borrow shape as the
module's biggest risk. It is not a risk, because **the C++ is not re-entrant
either**: `FireOutput` appends to the queue rather than calling the target, and
the queue is drained by one top-level loop. So a handler needs the queue, the
clock and the RNG — and nothing else.

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
```

Sixteen classnames, **17,479 of the shipped game's 60,925 entities**:

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
accordingly. That is the whole of what `AcceptInput`'s `baseMap` chain walk
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
Engine::frame -> Server::frame( frame_time )
                   ServerClock::accumulate -> 0..n fixed ticks
                   for each tick:
                     CleanupDeleteList         anything removed outside the loop
                     Physics_RunThinkFunctions the due thinks, in entity order
                     ServiceEventQueue         everything due, restart-from-head
                     CleanupDeleteList         anything a think removed
```

`CServerGameDLL::GameFrame` (`gameinterface.cpp:1383`) with the CS:GO, Steam,
nav-mesh and benchmarking steps removed. **The order is observable and maps
depend on it**: an output fired during a think is dispatched later in the *same*
tick, but an input handler cannot see a think that has not run yet.

### Inheritance is composition

There is no `parent` pointer on `ClassDef` and no datadesc chain walk. `CEnvLight
: public CLight` is an `EnvLight` that *holds* a `Light` and ends its `key_value`
with `self.light.key_value(..)`. See gotcha 13.

---

## Invariants and gotchas

Ordered by how likely each is to bite.

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
   22 shipped connections rely on it. `Use` is accepted and does nothing for
   every class here, which is the behaviour and not a stub: `CBaseEntity::Use`
   dispatches a null `m_pfnUse` for all of them.

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
    41,696 of the shipped game's 60,925 entity blocks name a class this port has
    not got, and 61,674 keys go unconsumed. Both are expected and counted. Do
    not add an error path for them.

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

---

## What is deliberately absent

| | Why |
|---|---|
| `MOVETYPE_*`, `SOLID_*`, velocity, the pusher | Stage 3. |
| Touch, `FSOLID_TRIGGER`, `StartDisabled` honoured for brush entities | Stage 4. |
| The player as an entity, `!player`, `noclip`'s home | Stage 5. 171 of the depot's 186 unhandled inputs are the three player procedurals. |
| Named think *contexts* (`m_aThinkFunctions`) | No class implemented so far has two independent timers. Note that stage 3's `SetMoveDoneTime` is **not** one — it is a separate alarm with its own field, and conflating the two is what breaks doors that think while moving. |
| `IGameSystem` as a registry | One system exists (`CTonemapSystem`), so it is a method. The condition is the second system that needs a level hook. |
| `FIELD_EHANDLE` and `FIELD_POSITION_VECTOR` | No class declares either. `FIELD_EHANDLE`'s two conversions both need the entity list, which `Variant::convert` has not got. |
| `AddOutput`, `SetParent` and the parenting inputs | Stage 3's. 14 shipped connections reach an implemented class with one. |
| `logic_branch_listener` | It is the first thing in the game that needs a handler to read *another* entity during dispatch — the condition that changes `Context`'s shape. |
| `SendTable`/`DT_`/`edict_t` | One process. Deleted, not deferred. |
| Save/restore, `FTYPEDESC_SAVE` | Deferred; `serde` over entity state when it comes back, not `ISave`. |
| `ent_pause`/`ent_step` (`Debug_ShouldStep`) | 20 lines and genuinely useful; reconsider when entities do more. |
| VScript | `portdocs/SERVER.md` §9. One `RunScriptCode` reaches an implemented class. |

---

## Extending it

**To add an entity class**, in `src/server/classes/`:

1. Write the state as a struct and `impl Behaviour for` it in the right family
   file (`logic.rs`, `light.rs`, `env.rs`, `world.rs`), with a `create`
   returning `Box<dyn Behaviour>`.
2. Add a `ClassDef` to `CLASSES` in `classes/mod.rs`, listing in `keys` exactly
   the names `key_value` consumes, in `inputs` exactly the names
   `accept_input` handles **with the field type Valve declared**, and in
   `outputs` exactly the names it fires.
3. `describe` returns whatever `ent_dump` should print.
4. Run `cargo test`: the two invariant tests check both declarations against the
   code, and the class table is checked for duplicates and for shadowing a base
   input.
5. Run the depot test. `EXPECTED_UNHANDLED` and the unhandled-input table will
   change — that is the point, and the change should be read before it is
   pasted in.

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
| `tests::every_shipped_map_spawns_its_entities` | **everything, against all 106 maps** |

The depot test is `--ignored` and gated on `KISAK_GAME_DIR`:

```
KISAK_GAME_DIR=/path/to/portal2 cargo test --release every_shipped_map_spawns -- --ignored --nocapture
```

It loads all 106 maps, runs **two seconds of server time on each**, and asserts
exact totals: 60,925 blocks, 19,229 matched, 12,292 spawned, 6,937 lights
deleted, 213 kept, 46,489 connections, 185 unimplemented classnames, the full
29-name unhandled-key table, 5,763 events dispatched, 2,070 inputs accepted,
1,197 thinks, 3,700 events that found no target, zero bad conversions, the
7-name unhandled-input table, a peak of 43 entities thinking at once, 105 maps
with a master tone mapper — and that `sp_a1_intro1` ends up asking for an
exposure ceiling of **1.5**.
