# `src/server/` — API reference

The game server: the map's entity list. Valve's `server.so`, reduced to the framework —
`CBaseEntity`, `CGlobalEntityList`, the datadesc, the keyvalue parse and the three-pass
spawn. Porting doc: [`portdocs/SERVER.md`](../portdocs/SERVER.md).

| | |
|---|---|
| Status | **Stage 1 of 5.** Entities are created, parsed, spawned and activated. |
| Depends on | `engine::world::bsp::Entity` (the parsed lump), `engine::console` (two commands) |
| Names no | `wgpu`, `winit`, `egui`, `materials`, `studio` — every test runs with no GPU |
| Tests | 28 unit tests + one depot test over all 106 shipped maps |

**What does not exist yet**: entity I/O and the event queue, `AcceptInput`, thinks,
movement, touch. Ten classnames are implemented out of the 200 the shipped maps place.
Those are stages 2-5 and `portdocs/SERVER.md` §8 has the plan.

---

## Quick start

```rust
use crate::server::Server;

let mut server = Server::new();
// `world.entities` is the `.bsp`'s entity lump, parsed by `engine::world`.
let stats = server.level_init("sp_a1_intro1", &world.entities);
eprintln!("{}", stats.summary());
// 149 of 598 entity blocks matched a class, 127 spawned (22 removed themselves),
// 531 outputs, 449 unknown classnames, 212 unhandled keys

server.level_shutdown();
```

In the running engine this is wired through `Scene`, so a `map` command does it: see
`Level::load` in `src/engine/mod.rs`. Two console commands read the result —
`report_entities` and `ent_dump <name / index / class>`.

---

## The core types

### `Server` (`mod.rs`)

```rust
pub struct Server { /* private */ }

impl Server {
    pub fn new() -> Server;
    pub fn level_init(&mut self, map: &str, blocks: &[bsp::Entity]) -> LevelStats;
    pub fn level_shutdown(&mut self);
    pub fn report_entities(&self, cx: &mut ExecContext<'_>);
    pub fn ent_dump(&self, cmd: &Command, cx: &mut ExecContext<'_>);
}
```

`level_init` is `CServerGameDLL::LevelInit` + `MapEntity_ParseAllEntities` +
`ServerActivate`'s entity half, in one call: the two boundaries between them in the
original are engine/game-DLL boundaries that do not exist here. It calls
`level_shutdown` first, so calling it twice replaces rather than appends.

It cannot fail. A block with no classname, a classname with no implementation, a key
nobody understands and a `parentname` naming nothing are all *counted*, not errors — see
[`LevelStats`](#levelstats).

### `LevelStats` (`mod.rs`)

```rust
pub struct LevelStats {
    pub blocks: usize,            // entity-lump blocks
    pub matched: usize,           // …whose classname is implemented
    pub spawned: usize,           // alive after the spawn pass
    pub removed_on_spawn: usize,  // deleted themselves in Spawn
    pub outputs: usize,           // output connections recognised
    pub parented: usize,
    pub parents_missing: usize,
    pub unknown: BTreeMap<String, usize>,    // classname -> count
    pub unhandled: BTreeMap<String, usize>,  // key name (lowercased) -> count
}

impl LevelStats { pub fn summary(&self) -> String; }
```

This is the module's progress metric, and it is the reason most of stage 1 exists: it
says exactly how much of the shipped game's entity data the port understands. Across all
106 maps it is 17,069 of 60,925 blocks.

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
    pub outputs: Vec<(String, String)>,     // raw connections, parsed in stage 2
    pub unhandled: Vec<(String, String)>,
    pub removed: bool,
}

impl EntityCore {
    pub fn classname(&self) -> &'static str;
    pub fn debug_name(&self) -> &str;   // targetname, else classname
}
```

**The split is not decoration.** A `Behaviour` method takes `&mut self` *and*
`&mut EntityCore`; one struct could not provide both. It is the same disjoint-field-borrow
move `Engine::frame`'s `EngineCommands` makes.

### `EntityId` and `EntityList` (`entity.rs`)

```rust
pub struct EntityId { /* slot + generation */ }
impl EntityId { pub fn slot(self) -> u32; }

pub struct EntityList { /* private */ }

impl EntityList {
    pub fn new() -> EntityList;
    pub fn insert(&mut self, entity: Entity) -> EntityId;
    pub fn get(&self, id: EntityId) -> Option<&Entity>;
    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut Entity>;
    pub fn mark_for_deletion(&mut self, id: EntityId);
    pub fn cleanup_delete_list(&mut self) -> usize;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = (EntityId, &Entity)>;
    pub fn clear(&mut self);
}
```

`EntityId` is `CBaseHandle`: a generational index, so a handle to a removed entity
resolves to `None` rather than to whatever took its slot. Valve packs slot and serial
into a `u32` because the index goes over the wire; nothing here does, so `MAX_EDICTS` and
`NUM_ENT_ENTRIES` go away. The largest shipped map places 1,446 entities.

### `ClassDef` and `Behaviour` (`class.rs`)

```rust
pub struct ClassDef {
    pub name: &'static str,
    pub keys: &'static [&'static str],       // declaration; see gotcha 8
    pub outputs: &'static [&'static str],
    pub create: fn() -> Box<dyn Behaviour>,
}

impl ClassDef {
    pub fn declares_output(&self, name: &str) -> bool;   // case-insensitive
    pub fn declares_key(&self, name: &str) -> bool;
}

pub enum SpawnResult { Ok, Remove }

pub trait Behaviour: Any {
    fn key_value(&mut self, entity: &mut EntityCore, key: &str, value: &str) -> bool;
    fn spawn(&mut self, entity: &mut EntityCore) -> SpawnResult;
    fn activate(&mut self, entity: &mut EntityCore);
    fn describe(&self) -> Vec<(&'static str, String)>;   // for `ent_dump`
}

impl dyn Behaviour {
    pub fn downcast_ref<T: Behaviour>(&self) -> Option<&T>;
}

pub struct PointEntity;   // CPointEntity — no state, no behaviour
```

All four trait methods have defaults, so a class with no state is
`impl Behaviour for Thing {}`.

### `classes` (`classes.rs`)

```rust
pub fn lookup(classname: &str) -> Option<&'static ClassDef>;   // case-insensitive
pub(super) static CLASSES: &[ClassDef];

pub struct World { pub sky_name: Option<String>, pub world_mins: Vec3,
                   pub world_maxs: Vec3, pub max_prop_screen_width: f32,
                   pub max_blob_count: i32, pub detail_material: Option<String> }
pub struct Light { pub style: i32, pub pattern: Option<String> }
pub struct EnvLight { /* holds a Light */ pub sun_color: [u8; 4] }
pub struct Relay { pub disabled: bool }
pub struct InstanceIoProxy;
```

Ten classnames, covering 17,069 of the shipped game's 60,925 entities:

| classname | C++ | instances |
|---|---|---:|
| `logic_relay` | `CLogicRelay` | 8,082 |
| `light`, `light_spot`, `light_directional`, `light_glspot` | `CLight` | 7,125 |
| `light_environment` | `CEnvLight` | 25 |
| `func_instance_io_proxy` | `CFuncInstanceIoProxy` | 1,184 |
| `info_target` | `CInfoTarget` | 431 |
| `info_player_start` | `CPointEntity` | 116 |
| `worldspawn` | `CWorld` | 106 |

### `keyvalue` (`keyvalue.rs`)

```rust
pub fn atof(s: &str) -> f32;
pub fn atoi(s: &str) -> i32;
pub fn string_to_float_array(s: &str, out: &mut [f32]);
pub fn string_to_vector(s: &str) -> Vec3;
pub fn string_to_int_array(s: &str, out: &mut [i32]) -> usize;
pub fn string_to_color32(s: &str) -> [u8; 4];
pub fn base_key_value(entity: &mut EntityCore, key: &str, value: &str) -> bool;

pub mod effects { /* EF_NOSHADOW, EF_NORECEIVESHADOW, … */ }
pub const EFL_NO_DAMAGE_FORCES: u32;
pub const BASE_OUTPUTS: &[&str];   // OnUser1..4
```

### `name` (`name.rs`)

```rust
pub fn names_match(query: &str, name: &str) -> bool;
pub fn is_procedural(name: &str) -> bool;                     // starts with '!'
pub fn find_by_name<'a>(list: &'a EntityList, query: &'a str)
    -> impl Iterator<Item = EntityId> + 'a;
```

---

## Cross-cutting semantics

### The order a key is offered to three things

`parse_map_data` walks the block in **lump order** and for each key tries, in this order:

1. the class's `Behaviour::key_value`,
2. `keyvalue::base_key_value` — `CBaseEntity::KeyValue`'s if-ladder,
3. the output table — `ClassDef::outputs` plus `BASE_OUTPUTS`.

Anything left over lands in `EntityCore::unhandled`. The order is Valve's:
`ParseMapData` calls `KeyValue` *virtually*, so a class that overrides it tests its own
keys and only then calls `BaseClass::KeyValue`, where the ladder lives.

### The spawn pipeline

`worldspawn` is spawned immediately, outside the sorted list, and is forcibly unparented
(`mapentities.cpp:373`). Everything else is queued, then:

1. **depth** — follow `parentname` to the root, counting (`ComputeSpawnHierarchyDepth_r`).
2. **sort** — depth ascending, then `SPAWN_PRIORITY` descending, then lump order.
3. **parents** — resolve `parentname` to an `EntityId`.
4. **spawn** — every entity, in that order; `SpawnResult::Remove` marks it.
5. **activate** — every survivor, after every spawn.
6. **cleanup** — free what step 4 marked.

### Inheritance is composition

There is no `parent` pointer on `ClassDef` and no datadesc chain walk. `CEnvLight : public
CLight` is an `EnvLight` that *holds* a `Light` and ends its `key_value` with
`self.light.key_value(..)`. See gotcha 9.

---

## Invariants and gotchas

Ordered by how likely each is to bite.

1. **`Server::level_init` never fails, and "unhandled" is not "broken".** 43,856 of the
   shipped game's 60,925 entity blocks name a class this port has not got, and 61,672
   keys go unconsumed. Both are expected and counted. Do not add an error path for them.

2. **`unhandled` is not the same as "not implemented".** 17 of the 28 key names in the
   whole game that nothing consumes are the *map compiler's* — `_light`, `_lightHDR`,
   `_quadratic_attn` and the falloff family are read by `vbsp`/`vrad` at compile time and
   have **no run-time consumer in Valve's engine either**. Four more are mapper mistakes
   shipped in the game (`//OnTrigger`, `_OnTrigger`, `OnUnPressed`, `AddonPoints`).
   `tests::EXPECTED_UNHANDLED` annotates all 28.

3. **`atoi`/`atof` read a prefix and never fail; `str::parse` would.** `"1.5abc"` is 1.5
   and `""` is 0. Map data is dirty enough that this matters: there is a `logic_relay` in
   the shipped game with a key called `//OnTrigger`. Use `keyvalue::atof`, never
   `parse::<f32>()`, on anything that came out of a lump.

4. **A short vector is zero-filled, not an error.** `string_to_vector("5")` is
   `(5, 0, 0)`, which is `UTIL_StringToFloatArray`.

5. **There are two splitters and they differ.** `string_to_float_array` separates on any
   byte `<= ' '`; `string_to_int_array` (and so `string_to_color32`) separates on the
   space character alone. Vectors use the first, colours the second. No shipped Portal 2
   value reaches the difference — measured — but it is Valve's and is reproduced.

6. **`rendercolor` sets alpha and `renderamt` overwrites it**, so the two share
   `render_color` and lump order is observable. `string_to_color32` leaves alpha 255 for
   three components and takes it from the fourth when there are exactly four — and the
   "exactly four" test is Valve's `j + 1` arithmetic, so *five* components fall back to
   opaque and a trailing space supplies a fourth component of zero.

7. **`names_match`'s `*` does not have to be trailing.** Valve's comment says "only thing
   supported is trailing `*`" and the code says otherwise: it walks until the strings
   diverge and asks whether the query is sitting on a `*`. So `"*door"` matches
   *everything* and `"do*r"` matches `"dover"`. All 234 wildcard targets in the shipped
   maps are a plain trailing `*`, so nothing in Portal 2 reaches it.

8. **`ClassDef::keys` is a declaration; `key_value` is the implementation.** Nothing at
   run time reads `keys` — its reader is the invariant test in `classes`, which asserts
   that every declared key is consumed and no undeclared key is. Add to both or to
   neither.

9. **A class that "inherits" must *contain*.** There is no chain walk to fall through, so
   `EnvLight::key_value` ends with `self.light.key_value(entity, key, value)` — that call
   *is* `BaseClass::KeyValue`. Forget it and the derived class silently loses every base
   key.

10. **`CLight::Spawn` deletes any light with no `targetname`** — 6,937 of the shipped
    game's 7,150. It is the only behaviour stage 1 has, and it is load-bearing: without
    it the entity list carries eleven per cent of the game as garbage and every other
    number still looks right.

11. **Removal is deferred, always.** `mark_for_deletion` sets a flag; `cleanup_delete_list`
    frees. An entity being iterated can therefore always be removed — which is why Valve
    works this way and, separately, what the Rust borrow rules want.

12. **`EntityCore::model` is a string.** `"*12"` is a brush model index and
    `"models/props/box.mdl"` is a studio model, and resolving either is `world/`'s or
    `studio/`'s job. Reaching for a model here would be this module's first GPU
    dependency.

13. **`unhandled` is counted at parse time, not after the spawn pass.** An entity that
    deletes itself during `Spawn` still reports what it did not understand — which is
    most of the count, since most of it is on lights.

14. **Procedural names resolve to nothing.** `find_by_name("!player")` yields no entities
    rather than erroring. Four of the five names Portal 2 uses need an activator or a
    caller (stage 2) and `!player` needs a player entity (stage 5).

15. **The `angle` key is not implemented**, and not by oversight: it appears zero times in
    the 106 shipped maps, and Valve's implementation is **infinitely recursive** — it
    rewrites the value and re-enters `KeyValue( szKeyName, szBuf )` with the key name
    still `"angle"` (`baseentity_shared.cpp:475`).

---

## What is deliberately absent

| | Why |
|---|---|
| Entity I/O, `CEventQueue`, `AcceptInput` | Stage 2. Outputs are recognised and stored raw. |
| Thinks, `SetNextThink`, the `SimThink` list | Stage 2, and it needs the fixed server tick (`portdocs/SERVER.md` §5). |
| `MOVETYPE_*`, `SOLID_*`, velocity, the pusher | Stage 3. |
| Touch, `FSOLID_TRIGGER`, `StartDisabled` honoured | Stage 4. |
| The player as an entity, `!player`, `noclip`'s home | Stage 5. |
| `SendTable`/`DT_`/`edict_t` | One process. Deleted, not deferred. |
| Save/restore, `FTYPEDESC_SAVE` | Deferred; `serde` over entity state when it comes back, not `ISave`. |
| `fieldtype_t`, `ClassDef::inputs` | They exist only to serve `AcceptInput`. Stage 2. |
| `parentname` with an attachment (`"arm,muzzle"`) | Needs `LookupAttachment`; zero of the 4,582 parented entities use it. |
| `angle`, `rendercolor32`, `mins`, `maxs` | Zero occurrences across the shipped maps. |
| `defaultstyle`, and ten `CWorld` keys | Same — measured, not assumed. |
| `CCascadeLight` (what `light_environment` feeds) | CS:GO's cascaded shadow map. Portal 2's sun is baked. |

---

## Extending it

**To add an entity class**, in `classes.rs`:

1. Write the state as a struct and `impl Behaviour for` it. Give it a `create` returning
   `Box<dyn Behaviour>`.
2. Add a `ClassDef` to `CLASSES`, listing in `keys` exactly the names `key_value`
   consumes and in `outputs` exactly the names it fires.
3. `describe` returns whatever `ent_dump` should print.
4. Run `cargo test`: the invariant test checks the declaration against the code, and the
   class table is checked for duplicates.
5. Run the depot test. `EXPECTED_UNHANDLED` will change — that is the point, and the
   change should be read before it is pasted in.

**Read the FGD first.** `depot_621/bin/{base,portal,halflife2,portal2}.fgd` declare 494
classes and cover 199 of the 200 the shipped maps place, including every class whose C++
was cut from this tree. They are not a superset of the datadesc (see
`portdocs/SERVER.md` §1.4) but they are the fastest way to see what a class's keys are
called.

---

## Which tests guard what

| Test | Guards |
|---|---|
| `keyvalue::atof_and_atoi_read_a_prefix_and_never_fail` | gotcha 3 |
| `keyvalue::a_short_vector_is_zero_filled` | gotcha 4 |
| `keyvalue::a_three_component_colour_is_opaque_and_a_four_component_one_is_not` | gotcha 6 |
| `keyvalue::renderamt_overwrites_the_alpha_rendercolor_set` | gotcha 6 |
| `keyvalue::the_render_flag_family_sets_effect_bits_and_one_of_them_clears` | the tri-state `shadowdepthnocache` |
| `name::matching_is_case_insensitive_and_only_a_trailing_star_wildcards` | gotcha 7 |
| `name::a_name_may_match_several_entities_and_the_order_is_list_order` | one name, many entities, lump order |
| `entity::a_handle_to_a_removed_entity_stops_resolving` | gotchas 11 and the generation counter |
| `entity::a_reused_slot_does_not_answer_the_old_handle` | the generation counter |
| `classes::every_declared_key_is_consumed_and_every_consumed_key_is_declared` | gotcha 8 |
| `classes::an_unnamed_light_removes_itself_and_a_named_one_does_not` | gotcha 10 |
| `classes::env_light_inherits_the_light_keys_by_holding_one` | gotcha 9 |
| `tests::a_child_spawns_after_its_parent_whatever_the_lump_order` | the depth sort |
| `tests::a_parent_cycle_terminates` | the one deliberate divergence from Valve |
| `tests::spawn_priority_is_valves_table` | the priority table, and that nothing in it is live yet |
| `tests::every_shipped_map_spawns_its_entities` | **everything, against all 106 maps** |

The depot test is `--ignored` and gated on `KISAK_GAME_DIR`:

```
KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_maps -- --ignored --nocapture
```

It asserts exact totals — 60,925 blocks, 17,069 matched, 10,132 spawned, 6,937 lights
deleted, 213 lights kept, 42,065 outputs, 191 unimplemented classnames, and the full
28-name unhandled table. They are exact because the files are.
