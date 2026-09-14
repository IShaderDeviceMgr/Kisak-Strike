//! The game server: the entity system.
//!
//! Valve's `server.so` — `game/server/` — reduced to the framework
//! `portdocs/SERVER.md` §1.1 identifies: the entity list, the class table, the
//! keyvalue parse, the three-pass spawn, entity I/O, the event queue and
//! thinks. It is a sibling of [`crate::client`] and [`crate::engine`] because
//! `server.so` was a sibling of `client.so` and `engine.so`.
//!
//! Stage 3 of five. What exists: entities are created from the map's entity
//! lump, spawned in hierarchy order and activated; they fire outputs at each
//! other through one queue; they think on a fixed tick; and the brush ones
//! **move** — doors open, panels slide, fans spin. What does not: touch and
//! triggers (stage 4), the player as an entity (stage 5).
//!
//! # This module names no GPU type
//!
//! Not `wgpu`, not `winit`, not [`crate::materials`], not [`crate::studio`].
//! An entity holds a *model name*; resolving one is somebody else's job
//! (`portdocs/SERVER.md` §3). That is what lets every test here run without a
//! window, the way [`crate::engine::host`], [`crate::engine::trace`] and
//! [`crate::engine::input`] already do — and it is much easier to hold from
//! the start than to recover later.
//!
//! Three types it names from outside, and all three are deliberate.
//! [`bsp::Entity`] is the parsed entity lump and [`bsp::Model`] is the model
//! lump's bounding boxes, which is Valve's shape too:
//! `CServerGameDLL::LevelInit( pMapName, pMapEntities, ... )` is handed the
//! lump by the engine, because the engine is what read the `.bsp`, and
//! `UTIL_SetModel` reads the model's size out of `modelinfo` for the same
//! reason. And [`TonemapSettings`](crate::client::tonemap::TonemapSettings) is
//! what `env_tonemap_controller` produces — see
//! [`Server::tonemap_settings`].
//!
//! # The load
//!
//! ```text
//! Scene::load  -> World::load        reads the .bsp, keeps the entity lump
//!              -> Server::level_init
//!                   for each block:  lookup(classname) -> Entity, parse keys
//!                   worldspawn:      spawned at once, and never parented
//!                   everything else: queued
//!                   ComputeSpawnHierarchyDepth  parents before children
//!                   SortSpawnListByHierarchy    depth, then classname priority
//!                   SetupParentsForSpawnList    resolve parentname
//!                   Spawn pass                  then Activate pass
//!                   CleanupDeleteList           free what Spawn removed
//!                   LevelInitPostEntity         pick the master tone mapper
//! ```
//!
//! # The frame
//!
//! ```text
//! Engine::frame -> Server::frame( frame_time )
//!                   ServerClock::accumulate -> 0..n fixed ticks
//!                   for each tick:
//!                     CleanupDeleteList         anything removed outside the loop
//!                     Physics_RunThinkFunctions think, then push, in entity order
//!                     ServiceEventQueue         everything due, restart-from-head
//!                     CleanupDeleteList         anything a think removed
//! ```
//!
//! That is `CServerGameDLL::GameFrame` (`gameinterface.cpp:1383`) with the
//! CS:GO, Steam, nav-mesh and benchmarking steps removed. **The order is
//! observable and maps depend on it**: an output fired during a think is
//! dispatched later in the *same* tick, but an input handler cannot see a
//! think that has not run yet.

pub mod class;
pub mod classes;
pub mod entity;
pub mod io;
pub mod keyvalue;
pub mod movement;
pub mod name;
pub mod random;
pub mod think;

use std::collections::BTreeMap;

use crate::client::tonemap::TonemapSettings;
use crate::engine::console::{Command, ExecContext};
use crate::engine::world::bsp;

use class::{base_accept_input, Behaviour, Context, SpawnResult};
use entity::{Entity, EntityCore, EntityId, EntityList};
use io::{Event, EventQueue, FieldType, Input, IoStats, Target, Variant};
use movement::ModelBounds;
use name::Procedural;
use random::RandomStream;
use think::{ServerClock, ThinkList};

/// The seed the level's random stream starts from.
///
/// Valve seeds once at host startup from the wall clock
/// (`engine/host.cpp:5626`), so its `logic_case` picks differ between runs.
/// This port seeds per level from a constant, which makes a map's behaviour
/// reproducible — and reproducibility is worth more here than variety: it is
/// what lets the depot test assert exact totals over a hundred and six maps
/// that contain random pickers. `-randomseed` would be the switch if variety
/// is ever wanted.
const LEVEL_RANDOM_SEED: i32 = 0;

/// The server. `CServerGameDLL` plus `gEntList` plus `g_EventQueue`.
///
/// Level-scoped, and so a field of the engine's `Scene` rather than of the
/// engine: the entity list is emptied and refilled by every map change, and
/// `Scene` is what [`Level`](crate::engine::host::Level) hands to the host.
pub struct Server {
    entities: EntityList,
    /// `g_EventQueue`. One per server rather than a file-scope global, which
    /// is `PORTING.md`'s rule and is also what lets a test run two.
    queue: EventQueue,
    /// The entities with a think scheduled. `CSimThinkManager`.
    thinks: ThinkList,
    /// The fixed server tick. `portdocs/SERVER.md` §5.
    clock: ServerClock,
    /// `random->` — see [`LEVEL_RANDOM_SEED`].
    random: RandomStream,
    /// `CEventAction::s_iNextIDStamp`, restarted per level.
    next_output_id: u32,
    /// `CTonemapSystem::m_hMasterController`, resolved at
    /// `LevelInitPostEntity`.
    master_tonemap: Option<EntityId>,
    /// The map whose entities these are, for reporting. `None` between levels.
    map: Option<String>,
    stats: LevelStats,
    io: IoStats,
    /// Scratch for [`ThinkList::due`], so that a tick does not allocate.
    due: Vec<EntityId>,
    /// Every entity that names a `"*N"` brush model, by `N`, sorted.
    ///
    /// The join `world/` and `trace/` need in order to take a brush entity's
    /// placement from the entity rather than from the lump
    /// (`portdocs/SERVER.md` §7.4). **The model index is a usable key because
    /// it is unique**: across all 106 shipped maps there are 11,635
    /// `(map, "*N")` pairs and **not one** is named by two entities, so no
    /// disambiguation is needed and nothing has to carry a lump index around.
    ///
    /// Built once at `level_init` and not maintained afterwards — an entity
    /// that is removed leaves a handle here that stops resolving, which
    /// [`Server::brush_entity`] treats as "no placement", and nothing in the
    /// game creates a brush entity at run time.
    brush_models: Vec<(usize, EntityId)>,
}

/// What one `level_init` produced.
///
/// Most of this exists to answer "how much of the entity system is there yet",
/// which is the only interesting question about the early stages and stays
/// interesting for several stages after them.
#[derive(Default, Clone)]
pub struct LevelStats {
    /// Blocks in the entity lump.
    pub blocks: usize,
    /// Blocks whose classname is one this port implements.
    pub matched: usize,
    /// Entities alive after the spawn pass.
    pub spawned: usize,
    /// Entities their own `Spawn` deleted — almost all of them unnamed lights.
    pub removed_on_spawn: usize,
    /// Output connections parsed.
    pub outputs: usize,
    /// Entities that named a parent, and how many of those resolved.
    pub parented: usize,
    pub parents_missing: usize,
    /// Classnames with no [`ClassDef`](class::ClassDef), and how many times
    /// each appeared.
    pub unknown: BTreeMap<String, usize>,
    /// Keys nothing consumed, and how many times each appeared.
    ///
    /// **Not the same as "not implemented".** A `light`'s `_quadratic_attn` is
    /// `vrad`'s, read at compile time; the shipped server does not handle it
    /// either. `tests::EXPECTED_UNHANDLED` is the full annotated list.
    ///
    /// Counted at parse time, so an entity that deletes itself during `Spawn`
    /// still reports what it did not understand.
    pub unhandled: BTreeMap<String, usize>,
}

impl LevelStats {
    /// One line, in the shape `World::summary` uses.
    pub fn summary(&self) -> String {
        format!(
            "{} of {} entity blocks matched a class, {} spawned ({} removed themselves), \
             {} outputs, {} unknown classnames, {} unhandled keys",
            self.matched,
            self.blocks,
            self.spawned,
            self.removed_on_spawn,
            self.outputs,
            self.unknown.values().sum::<usize>(),
            self.unhandled.values().sum::<usize>(),
        )
    }
}

/// `SortSpawnListByHierarchy`'s classname priority registry
/// (`mapentities.cpp:177`). **Higher spawns first**, and anything not listed
/// is `-1`.
///
/// **None of these classnames is registered yet**, so the table never changes
/// an order today. It is ported rather than deferred because the moment
/// `prop_physics` lands its spawn order silently matters — a physics prop must
/// spawn after the constraints that hold it — and that is not a bug anyone
/// would find by looking at `prop_physics`.
const SPAWN_PRIORITY: &[(&str, i32)] = &[
    ("func_wall", 10),
    ("scripted_sequence", 9),
    ("phys_hinge", 8),
    ("phys_ballsocket", 8),
    ("phys_slideconstraint", 8),
    ("phys_constraint", 8),
    ("phys_pulleyconstraint", 8),
    ("phys_lengthconstraint", 8),
    ("phys_ragdollconstraint", 8),
    ("info_mass_center", 8),
    ("trigger_vphysics_motion", 8),
    ("prop_physics", 7),
    ("prop_ragdoll", 7),
];

/// `ExtractParentName` (`mapentities.cpp:87`): a `parentname` may name an
/// attachment point after a comma — `"arm,muzzle"` — and everything before the
/// comma is the entity's name.
///
/// The attachment itself is not honoured: it needs `LookupAttachment` on a
/// studio model, which would be this module's first dependency on `studio/`.
/// **Zero of the shipped maps' 4,582 parented entities use the form**, so what
/// is ported is the split, which both callers need to agree on.
fn extract_parent_name(parent_name: &str) -> &str {
    parent_name.split(',').next().unwrap_or(parent_name)
}

/// The spawn priority of a classname, or `-1`.
fn spawn_priority(classname: &str) -> i32 {
    SPAWN_PRIORITY
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(classname))
        .map_or(-1, |(_, priority)| *priority)
}

impl Server {
    pub fn new() -> Server {
        Server::with_tick_interval(think::DEFAULT_TICK_INTERVAL)
    }

    /// A server running at a given tick interval. `-tickrate` is the only
    /// caller that passes anything but the default; see
    /// [`ServerClock::interval_from_tickrate`].
    pub fn with_tick_interval(interval: f32) -> Server {
        Server {
            entities: EntityList::new(),
            queue: EventQueue::new(),
            thinks: ThinkList::new(),
            clock: ServerClock::new(interval),
            random: RandomStream::new(LEVEL_RANDOM_SEED),
            next_output_id: 0,
            master_tonemap: None,
            map: None,
            stats: LevelStats::default(),
            io: IoStats::default(),
            due: Vec::new(),
            brush_models: Vec::new(),
        }
    }

    /// `CServerGameDLL::LevelInit` (`gameinterface.cpp:1167`) plus
    /// `MapEntity_ParseAllEntities` (`mapentities.cpp:540`) plus
    /// `ServerActivate`'s entity half (`:1305`).
    ///
    /// The three are one call here because the two boundaries between them are
    /// engine/game-DLL boundaries that do not exist: `LevelInit` parses,
    /// `ServerActivate` activates, and the engine calls one and then the other
    /// with nothing in between that this port has.
    /// `models` is the `.bsp`'s model lump, indexed by brush-model number, and
    /// it is what `SetModel` reads: a mover computes how far it travels from
    /// the size of its own brushes, and that number is in the file rather than
    /// in the entity lump. `&[]` is legal and gives every mover a zero-sized
    /// box, which is what `UTIL_SetModel` does for a missing model too — the
    /// unit tests pass it.
    pub fn level_init(
        &mut self,
        map: &str,
        blocks: &[bsp::Entity],
        models: &[bsp::Model],
    ) -> LevelStats {
        self.level_shutdown();
        self.map = Some(map.to_owned());

        let mut stats = LevelStats {
            blocks: blocks.len(),
            ..LevelStats::default()
        };
        // `HierarchicalSpawn_t` — everything queued for the sorted pass.
        let mut spawn_list: Vec<EntityId> = Vec::with_capacity(blocks.len());

        for block in blocks {
            let Some(classname) = block.classname() else {
                // No `classname` key at all. `MapEntity_ParseEntity` treats
                // this as a parse failure and skips the block.
                *stats
                    .unknown
                    .entry(String::from("<no classname>"))
                    .or_default() += 1;
                continue;
            };
            let Some(class) = classes::lookup(classname) else {
                *stats.unknown.entry(classname.to_owned()).or_default() += 1;
                continue;
            };
            stats.matched += 1;

            let mut entity = Entity::new(class);
            self.parse_map_data(&mut entity, block);
            stats.outputs += entity.outputs.iter().map(io::Output::len).sum::<usize>();
            // Counted here rather than after the spawn pass, so that what an
            // entity did not understand is recorded whether or not that entity
            // survived its own `Spawn`. Half the unhandled keys in the game
            // are on lights, and every unnamed light deletes itself.
            for (key, _) in &entity.unhandled {
                *stats.unhandled.entry(key.to_ascii_lowercase()).or_default() += 1;
            }

            // `UTIL_SetModel` (`util.cpp:1426`) — `SetMinMaxSize` from the
            // model lump. Done here rather than in each class's `Spawn`
            // because `SetModel` is `CBaseEntity`'s and every class calls it
            // for the same reason.
            let brush_index = entity
                .core
                .model
                .as_deref()
                .and_then(|name| name.strip_prefix('*'))
                .and_then(|n| n.parse::<usize>().ok());
            if let Some(model) = brush_index.and_then(|i| models.get(i)) {
                entity.core.model_bounds = ModelBounds {
                    mins: glam::Vec3::from(model.mins),
                    maxs: glam::Vec3::from(model.maxs),
                };
            }

            let is_world = class.name == "worldspawn";
            if is_world {
                // `mapentities.cpp:373`: "don't allow a parent on the first
                // entity (worldspawn)". The shipped maps never give it one;
                // this is Valve's belt and braces and costs a line.
                entity.core.parent_name = None;
            }
            let id = self.entities.insert(entity);
            // Model 0 is the world, which `worldspawn` names and which is not
            // a *placement* — `world/` draws it in world space and `trace`
            // already covers it. The same exclusion `find_brush_models` makes.
            if let Some(index) = brush_index.filter(|&i| i != 0) {
                self.brush_models.push((index, id));
            }

            match is_world {
                // Spawned at once and outside the sorted list, because
                // everything else may ask about the world and nothing may ask
                // about anything else yet.
                true => self.dispatch_spawn(id),
                false => spawn_list.push(id),
            }
        }

        let ordered = self.spawn_order(&spawn_list);

        // `SetupParentsForSpawnList` (`:206`). Before the spawn pass, so that
        // a `Spawn` can already see where its parent is.
        //
        // The attachment half of the name is dropped — see
        // [`extract_parent_name`].
        for &id in &ordered {
            let Some(parent_name) = self.entities.get(id).and_then(|e| e.parent_name.clone())
            else {
                continue;
            };
            stats.parented += 1;
            let parent =
                name::find_by_name(&self.entities, extract_parent_name(&parent_name)).next();
            if parent.is_none() {
                stats.parents_missing += 1;
            }
            if let Some(entity) = self.entities.get_mut(id) {
                entity.core.parent = parent;
            }
        }

        // `SpawnAllEntities` (`:253`): spawn every one, then activate every
        // survivor. Two complete passes, which is what makes `Activate` the
        // first place a class may look at another entity — and, for
        // `logic_auto` and `logic_relay`, the first place a think may be
        // scheduled.
        for &id in &ordered {
            self.dispatch_spawn(id);
        }
        for &id in &ordered {
            self.dispatch(id, |core, behaviour, cx| {
                if !core.removed {
                    behaviour.activate(core, cx);
                }
            });
        }

        stats.removed_on_spawn = self.cleanup_delete_list();
        stats.spawned = self.entities.len();

        // Sorted so that [`Server::brush_entity`] can binary-search it. The
        // lump order it loses is not meaningful: the key is unique.
        self.brush_models.sort_unstable_by_key(|&(index, _)| index);

        // `IGameSystem::LevelInitPostEntity`. One system so far, so it is a
        // method rather than a `Vec<Box<dyn GameSystem>>` — see
        // [`Server::update_master_tonemap`].
        self.update_master_tonemap();

        self.stats = stats.clone();
        stats
    }

    /// `ComputeSpawnHierarchyDepth` then `SortSpawnListByHierarchy`
    /// (`mapentities.cpp:154` and `:172`): the order the spawn pass runs in.
    ///
    /// Shallow before deep, so that a parent always spawns before its child,
    /// and within one depth the [`SPAWN_PRIORITY`] table breaks the tie.
    ///
    /// **Valve sorts with `qsort`, which is not stable**, and its comparator
    /// returns 0 for two entities of equal depth and priority — so the
    /// relative order of nearly every entity in a map is formally unspecified
    /// there, and in practice is whatever the implementation does. A stable
    /// sort keeps entity-lump order within a rank, which is deterministic and
    /// is what a level designer means when they say two things happen in
    /// order.
    fn spawn_order(&self, spawn_list: &[EntityId]) -> Vec<EntityId> {
        let mut ordered: Vec<(i32, i32, usize, EntityId)> = spawn_list
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let depth = self.spawn_hierarchy_depth(id);
                let priority = self
                    .entities
                    .get(id)
                    .map_or(-1, |e| spawn_priority(e.classname()));
                (depth, -priority, i, id)
            })
            .collect();
        ordered.sort_by_key(|&(depth, priority, index, _)| (depth, priority, index));
        ordered.into_iter().map(|(_, _, _, id)| id).collect()
    }

    /// `DispatchSpawn` (`mapentities.cpp:74`) — spawn one entity and mark it
    /// if it asked to go.
    fn dispatch_spawn(&mut self, id: EntityId) {
        self.dispatch(id, |core, behaviour, cx| {
            match behaviour.spawn(core, cx) {
                SpawnResult::Ok => {}
                // `UTIL_Remove( this )`: marked, not freed.
                SpawnResult::Remove => core.remove(),
            }
        });
    }

    /// `ComputeSpawnHierarchyDepth_r` (`mapentities.cpp:133`), iteratively.
    ///
    /// An entity with no parent, or one whose parent is not in the map, is at
    /// depth 1; each resolved step up adds one.
    ///
    /// **Cycle handling diverges, deliberately.** Valve checks only for an
    /// entity parented to *itself* and warns; a two-entity cycle recurses
    /// until the stack runs out. The walk is bounded here by the number of
    /// entities, which cannot be exceeded by an acyclic chain, and a chain
    /// that hits the bound is reported and treated as depth 1. No shipped map
    /// contains a cycle.
    fn spawn_hierarchy_depth(&self, id: EntityId) -> i32 {
        let mut current = id;
        // One step per entity is more than any acyclic chain can need, so
        // reaching the end of this range *is* the cycle detection.
        let limit = self.entities.len() + 1;
        for depth in 1..=limit as i32 {
            let Some(entity) = self.entities.get(current) else {
                return depth;
            };
            let Some(parent_name) = entity.parent_name.as_deref() else {
                return depth;
            };
            let parent_name = extract_parent_name(parent_name);
            let Some(parent) = name::find_by_name(&self.entities, parent_name).next() else {
                return depth;
            };
            if parent == current {
                eprintln!(
                    "source-engine: server: LEVEL DESIGN ERROR: entity {} is parented to itself",
                    entity.debug_name()
                );
                return 1;
            }
            current = parent;
        }
        eprintln!(
            "source-engine: server: LEVEL DESIGN ERROR: parent chain from {} is a cycle",
            self.entities.get(id).map_or("?", |e| e.debug_name())
        );
        1
    }

    /// `CServerGameDLL::LevelShutdown` (`gameinterface.cpp:1586`). Tolerates
    /// being called with nothing loaded.
    pub fn level_shutdown(&mut self) {
        self.entities.clear();
        self.queue.clear();
        self.thinks.clear();
        self.clock.reset();
        self.random = RandomStream::new(LEVEL_RANDOM_SEED);
        self.next_output_id = 0;
        self.master_tonemap = None;
        self.map = None;
        self.stats = LevelStats::default();
        self.io = IoStats::default();
        self.brush_models.clear();
    }

    // -----------------------------------------------------------------------
    // the frame
    // -----------------------------------------------------------------------

    /// Runs however many fixed server ticks `frame_time` seconds bought.
    ///
    /// Returns how many ran — zero is normal and is what happens on most
    /// rendered frames at a high frame rate.
    ///
    /// `frame_time` is the host's already-clamped frame time, so the
    /// accumulator cannot be handed a stall; see
    /// [`ServerClock::accumulate`].
    pub fn frame(&mut self, frame_time: f32) -> u32 {
        if self.map.is_none() {
            return 0;
        }
        let ticks = self.clock.accumulate(frame_time);
        for _ in 0..ticks {
            self.clock.advance();
            self.run_tick();
        }
        ticks
    }

    /// One server tick. `CServerGameDLL::GameFrame` (`gameinterface.cpp:1383`).
    ///
    /// The five steps that survive, in Valve's order. The two things to know
    /// about that order are both consequences of `ServiceEventQueue` running
    /// **once, after every think**: an output a think fires is delivered in the
    /// same tick, and an input handler cannot observe a think that has not run
    /// yet.
    fn run_tick(&mut self) {
        // Anything removed outside the loop — by a console command, say.
        self.cleanup_delete_list();
        self.run_think_functions();
        self.service_events();
        // Anything a think or an input removed.
        self.cleanup_delete_list();
    }

    /// `Physics_RunThinkFunctions` (`physics_main.cpp:2282`).
    ///
    /// The simulation list is **copied** before anything runs, so a think or
    /// an arrival may schedule, cancel or delete anything including itself.
    /// That is what Valve's `stackalloc` + `SimThink_ListCopy` is for.
    ///
    /// Stage 3 turned the body from "run the think" into
    /// `Physics_SimulateEntity`, which is a think *and* a push
    /// ([`movement::simulate`]). The list now holds movers as well as
    /// thinkers, and a mover is copied out every tick whatever its schedule
    /// says — so the "is the think due" question moved down into
    /// `movement::simulate` with it.
    fn run_think_functions(&mut self) {
        let tick = self.clock.time().tick;
        let mut due = std::mem::take(&mut self.due);
        self.thinks.due(tick, &mut due);

        for &id in due.iter() {
            // The entity may have been removed by an earlier think in the same
            // pass; `PhysicsSimulate` is not called on a corpse.
            let alive = self.entities.get(id).is_some_and(|e| !e.removed);
            if !alive {
                continue;
            }
            let thought = self
                .dispatch(id, |core, behaviour, cx| {
                    let before = core.next_think_tick();
                    movement::simulate(core, behaviour, cx);
                    // What `PhysicsRunSpecificThink` did: the schedule is
                    // cleared before the think runs, so a think that happened
                    // is one whose tick is no longer the one it was.
                    before > 0 && before <= cx.time.tick
                })
                .unwrap_or(false);
            if thought {
                self.io.thinks += 1;
            }
        }

        due.clear();
        self.due = due;
    }

    /// `CEventQueue::ServiceEvents` (`cbase.cpp:911`).
    ///
    /// Pops the next due event and dispatches it until nothing is due, which
    /// is Valve's restart-from-the-head loop — see [`EventQueue::pop_due`] for
    /// why the two are the same thing. The consequence is the one that defines
    /// how a Source map behaves: **a chain of eight zero-delay `logic_relay`s
    /// completes in one tick, not eight.**
    fn service_events(&mut self) {
        let now = self.clock.time().curtime;
        // A zero-delay chain is finite in every shipped map, but a map *can*
        // write a loop (a relay that triggers itself with no delay), and Valve
        // hangs on one. This bounds it: 100,000 events is four times the
        // largest map's entire connection count.
        let mut budget = 100_000_u32;

        while let Some(event) = self.queue.pop_due(now) {
            self.io.dispatched += 1;
            self.deliver(event);

            budget -= 1;
            if budget == 0 {
                eprintln!(
                    "source-engine: server: the event queue has not drained in 100000 events; \
                     a map's I/O is looping. Dropping the rest of this tick."
                );
                self.queue.clear();
                break;
            }
        }
    }

    /// One event's target resolution and delivery.
    ///
    /// The order is Valve's: **by name, then by handle, then — only if neither
    /// found anything — by classname**. The classname fallback is not a
    /// curiosity: 2,747 shipped connections fire `SetFogController` at the
    /// literal string `env_fog_controller` and reach every fog controller in
    /// the map without naming one.
    fn deliver(&mut self, event: Event) {
        let mut targets: Vec<EntityId> = Vec::new();
        let mut found = false;

        match &event.target {
            Target::Name(query) if name::is_procedural(query) => {
                // `FindEntityByName` short-circuits a `!name` to exactly one
                // entity and never iterates — "avoid an infinite loop, only
                // find one match per procedural search".
                match name::find_procedural(query, event.caller, event.activator, event.caller) {
                    Procedural::Resolved(Some(id)) => {
                        targets.push(id);
                        found = true;
                    }
                    // A null activator is a legitimate answer in Valve too;
                    // the event simply reaches nothing.
                    Procedural::Resolved(None) => {}
                    Procedural::NeedsPlayer => {
                        *self
                            .io
                            .unhandled
                            .entry(format!("{query} (needs a player)"))
                            .or_default() += 1;
                    }
                    Procedural::Unknown => {
                        *self
                            .io
                            .unhandled
                            .entry(format!("{query} (not a procedural name)"))
                            .or_default() += 1;
                    }
                }
            }
            Target::Name(query) => {
                targets.extend(name::find_by_name(&self.entities, query));
                found = !targets.is_empty();
            }
            Target::Entity(id) => {
                // A dead handle resolves to null and the event is reported as
                // "target entity not found", exactly as `m_pEntTarget` does.
                if self.entities.is_alive(*id) {
                    targets.push(*id);
                    found = true;
                }
            }
        }

        // The classname fallback, guarded on the name form the way Valve
        // guards it on `m_iTarget != NULL_STRING`.
        if !found {
            if let Target::Name(query) = &event.target {
                if !name::is_procedural(query) {
                    targets.extend(
                        self.entities
                            .iter()
                            .filter(|(_, e)| e.classname().eq_ignore_ascii_case(query))
                            .map(|(id, _)| id),
                    );
                    found = !targets.is_empty();
                }
            }
        }

        if !found && targets.is_empty() {
            self.io.no_target += 1;
        }

        for id in targets {
            self.accept_input(
                id,
                &event.input,
                event.value.clone(),
                event.activator,
                event.caller,
                event.output_id,
            );
        }
    }

    /// `CBaseEntity::AcceptInput` (`baseentity.cpp:4457`).
    ///
    /// Finds the declared type for the input name — the class's table first,
    /// then `CBaseEntity`'s, which is what the `baseMap` walk reduces to here
    /// — converts the value to it, and dispatches.
    ///
    /// Returns whether anything took the input. An unmatched input is a
    /// `DevMsg` in the original, not an error; here it is counted, because
    /// "which inputs does the port not implement yet" is the stage's progress
    /// metric.
    fn accept_input(
        &mut self,
        id: EntityId,
        input_name: &str,
        value: Variant,
        activator: Option<EntityId>,
        caller: Option<EntityId>,
        output_id: u32,
    ) -> bool {
        let Some(class) = self.entities.get(id).map(|e| e.class) else {
            return false;
        };

        let (field, on_class) = match class.input_type(input_name) {
            Some(field) => (field, true),
            None => match class::base_input(input_name) {
                Some(field) => (field, false),
                None => {
                    *self
                        .io
                        .unhandled
                        .entry(format!("{}.{input_name}", class.name))
                        .or_default() += 1;
                    return false;
                }
            },
        };

        let mut value = value;
        if value.field_type() != field {
            // "allow empty strings": a `FIELD_VOID` value reaching a
            // `FIELD_STRING` handler is passed through unconverted rather than
            // refused. Without this, every parameterless connection into a
            // string input — `FireUser1`, every proxy relay — would be
            // rejected as a bad link.
            let exempt = value.field_type() == FieldType::Void && field == FieldType::String;
            if !exempt && !value.convert(field) {
                eprintln!(
                    "source-engine: server: bad input/output link: {}.{input_name} \
                     does not take a {:?}",
                    class.name,
                    value.field_type()
                );
                self.io.bad_conversion += 1;
                return false;
            }
        }

        let accepted = self
            .dispatch(id, |core, behaviour, cx| {
                let input = Input {
                    name: input_name,
                    value,
                    activator,
                    caller,
                    output_id,
                };
                match on_class {
                    true => behaviour.accept_input(core, &input, cx),
                    false => base_accept_input(core, behaviour, &input, cx),
                }
            })
            .unwrap_or(false);

        match accepted {
            true => self.io.accepted += 1,
            // Only reachable if a class declares an input its handler refuses,
            // which `classes`' invariant test makes impossible — so this arm
            // is the test's safety net rather than a live path.
            false => {
                *self
                    .io
                    .unhandled
                    .entry(format!("{}.{input_name}", class.name))
                    .or_default() += 1
            }
        }
        accepted
    }

    /// Runs `f` against one entity with a [`Context`], and reconciles the
    /// think list afterwards.
    ///
    /// **This is the borrow seam.** The entity list, the queue and the random
    /// stream are three fields of one struct, so they are destructured before
    /// the entity is borrowed — which is the same disjoint-field move
    /// `Engine::frame`'s `EngineCommands` makes, and the reason `Context` does
    /// not hold the entity list (see its docs).
    ///
    /// Reconciling afterwards rather than inside `set_next_think` is what
    /// keeps [`EntityCore`] free of a back-reference to the server. Every
    /// place a schedule can change is a place that has a `Context`, and every
    /// place that has a `Context` goes through here.
    fn dispatch<R>(
        &mut self,
        id: EntityId,
        f: impl FnOnce(&mut EntityCore, &mut dyn Behaviour, &mut Context<'_>) -> R,
    ) -> Option<R> {
        let Server {
            entities,
            queue,
            random,
            clock,
            ..
        } = self;
        let time = clock.time();

        let (result, next_think, simulates, removed) = {
            let entity = entities.get_mut(id)?;
            let mut cx = Context::new(time, queue, random);
            let result = f(&mut entity.core, &mut *entity.behaviour, &mut cx);
            (
                result,
                entity.core.next_think_tick(),
                // `CheckHasGamePhysicsSimulation`, which `SetMoveDoneTime` and
                // `SetMoveType` both call — reconciled here for the same
                // reason the think schedule is: every place either can change
                // is a place that has a `Context`, and every place that has a
                // `Context` goes through this function.
                entity.core.will_simulate_game_physics(),
                entity.core.removed,
            )
        };

        // `SimThink_EntityChanged` (`entitylist.cpp:302`).
        self.thinks
            .entity_changed(id, next_think, simulates, removed);
        Some(result)
    }

    /// `gEntList.CleanupDeleteList` plus the two lists that name entities.
    fn cleanup_delete_list(&mut self) -> usize {
        let freed = self.entities.cleanup_delete_list();
        if freed > 0 {
            let entities = &self.entities;
            self.thinks.retain_alive(|id| entities.is_alive(id));
            self.queue.retain_targets(|id| entities.is_alive(id));
            // The master tone mapper may have been one of them.
            if self.master_tonemap.is_some_and(|id| !entities.is_alive(id)) {
                self.master_tonemap = None;
            }
        }
        freed
    }

    // -----------------------------------------------------------------------
    // the tone mapper
    // -----------------------------------------------------------------------

    /// `CTonemapSystem::LevelInitPostEntity`
    /// (`env_tonemap_controller.cpp:320`).
    ///
    /// > **The first controller found becomes master, and any later one that
    /// > carries `SF_TONEMAP_MASTER` replaces it** — so with several flagged
    /// > controllers the *last* wins, and with none the *first* does. Portal 2
    /// > never reaches the ambiguity: 105 of its 110 controllers carry the
    /// > flag, and the five that do not are exactly the second controller in
    /// > the five maps that have two.
    ///
    /// This is Valve's one `IGameSystem` on this path. `portdocs/SERVER.md`
    /// §4.9 asks for the registry to be ported as a plain list; with exactly
    /// one system it is a method instead, and the condition that makes the
    /// list worth writing is the second system that needs a level hook.
    fn update_master_tonemap(&mut self) {
        let mut master: Option<EntityId> = None;
        for (id, entity) in self.entities.iter() {
            if entity.classname() != "env_tonemap_controller" {
                continue;
            }
            let is_master = classes::TonemapController::is_master(&entity.core);
            if master.is_none() || is_master {
                master = Some(id);
            }
        }
        self.master_tonemap = master;
    }

    /// What the map's master `env_tonemap_controller` is asking for, or the
    /// no-controller fallback.
    ///
    /// `GetTonemapSettingsFromEnvTonemapController`
    /// (`c_env_tonemap_controller.cpp:97`) collapsed into one call: in Valve's
    /// engine the values travel server entity → `SendTable` → client entity →
    /// `localPlayer->m_hTonemapController` → thirteen file-scope globals. One
    /// process, one struct (`portdocs/SERVER.md` §6).
    ///
    /// Read once per rendered frame by `Engine::render`, because a controller's
    /// values change whenever map I/O says so — `sp_a1_intro1` changes them
    /// 0.21 seconds in.
    pub fn tonemap_settings(&self) -> TonemapSettings {
        self.master_tonemap
            .and_then(|id| self.entities.get(id))
            .and_then(|entity| {
                entity
                    .behaviour
                    .downcast_ref::<classes::TonemapController>()
            })
            .map(classes::TonemapController::settings)
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // the brush entities, for whoever draws and collides with them
    // -----------------------------------------------------------------------

    /// The entity that names brush model `"*index"`, if one is alive.
    ///
    /// **This is the stage-3 seam** `portdocs/SERVER.md` §7.4 asked for: a
    /// brush entity's placement is the *entity's*, not the lump's, the moment
    /// anything can move it. `world/` reads `origin`, `angles`, `effects` and
    /// `solid_flags` off the answer once a frame and writes them into the one
    /// `BrushModel` that both the draw and the trace go through — so what is
    /// drawn and what is collided with still cannot drift apart.
    ///
    /// `None` means the map has no entity for that model, or the port has no
    /// class for its classname (8,225 of the game's 11,635 brush entities are
    /// `trigger_*` and similar), or the entity has been removed. All three are
    /// "leave it where the lump put it".
    pub fn brush_entity(&self, index: usize) -> Option<&EntityCore> {
        let at = self
            .brush_models
            .binary_search_by_key(&index, |&(i, _)| i)
            .ok()?;
        let (_, id) = self.brush_models[at];
        self.entities.get(id).map(|entity| &entity.core)
    }

    /// How many brush entities this map placed that the port has a class for.
    pub fn brush_entity_count(&self) -> usize {
        self.brush_models.len()
    }

    // -----------------------------------------------------------------------
    // reporting
    // -----------------------------------------------------------------------

    /// Where the server's clock is. `gpGlobals`' time fields.
    pub fn time(&self) -> think::Time {
        self.clock.time()
    }

    /// `report_entities` (`entitylist.cpp:1944`) — a count per classname,
    /// sorted by classname, then a total.
    ///
    /// Valve's `CSortedEntityList::ReportEntityList` prints
    /// `Class: <name> (<count>)` and a total line naming how many of the
    /// entries were null and how many had an edict; neither number can be
    /// anything but 0 and "all of them" here, so the total line is shortened
    /// and the coverage this port actually wants is printed instead.
    pub fn report_entities(&self, cx: &mut ExecContext<'_>) {
        let Some(map) = &self.map else {
            cx.print("report_entities: no map is loaded");
            return;
        };

        if self.entities.is_empty() {
            cx.print(&format!("report_entities: {map} spawned no entities"));
            return;
        }

        let mut per_class: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, entity) in self.entities.iter() {
            *per_class.entry(entity.classname()).or_default() += 1;
        }
        for (classname, count) in &per_class {
            cx.print(&format!("Class: {classname} ({count})"));
        }
        cx.print(&format!(
            "Total {} entities of {} classes in {map}",
            self.entities.len(),
            per_class.len()
        ));

        // Everything below is this port's, not Valve's: it is the progress
        // report, and it goes away as the classes land.
        let stats = &self.stats;
        cx.print(&format!(
            "{} of {} entity blocks matched a class; {} removed themselves on spawn",
            stats.matched, stats.blocks, stats.removed_on_spawn
        ));
        if stats.parented > 0 {
            cx.print(&format!(
                "{} entities named a parent, {} of those did not resolve",
                stats.parented, stats.parents_missing
            ));
        }

        let time = self.time();
        cx.print(&format!(
            "tick {} ({:.2}s), {} events dispatched, {} inputs accepted, {} thinks run",
            time.tick, time.curtime, self.io.dispatched, self.io.accepted, self.io.thinks
        ));
        cx.print(&format!(
            "{} connections parsed, {} queued now, {} entities thinking or moving, \
             {} events found no target",
            stats.outputs,
            self.queue.len(),
            self.thinks.len(),
            self.io.no_target
        ));
        cx.print(&format!(
            "{} brush entities have a class; their placements are the server's",
            self.brush_entity_count()
        ));

        print_counts(cx, "unimplemented classnames", &stats.unknown, 12);
        print_counts(cx, "keys nothing consumed", &stats.unhandled, 12);
        print_counts(cx, "inputs nothing handled", &self.io.unhandled, 12);
    }

    /// `ent_dump` (`baseentity.cpp:6103`) — one entity's state, by name, by
    /// classname, or by list index.
    ///
    /// `GetNextCommandEntity` accepts all three and so does this. The state it
    /// prints is [`EntityCore`]'s plus whatever the class says in
    /// [`Behaviour::describe`], which is what replaces `DumpEntity`'s walk
    /// over a datadesc that no longer exists.
    pub fn ent_dump(&self, cmd: &Command, cx: &mut ExecContext<'_>) {
        let Some(query) = cmd.arg(1) else {
            cx.print("ent_dump <entity name / index / class>");
            return;
        };
        if self.map.is_none() {
            cx.print("ent_dump: no map is loaded");
            return;
        }

        for id in self.command_entities(query) {
            let Some(entity) = self.entities.get(id) else {
                continue;
            };
            cx.print(&format!(
                "[{}] {} \"{}\"",
                id.slot(),
                entity.classname(),
                entity.name.as_deref().unwrap_or("")
            ));
            let v = |v: glam::Vec3| format!("{:.1} {:.1} {:.1}", v.x, v.y, v.z);
            cx.print(&format!("  origin: {}", v(entity.origin)));
            cx.print(&format!("  angles: {}", v(entity.angles)));
            if entity.spawn_flags != 0 {
                cx.print(&format!("  spawnflags: {}", entity.spawn_flags));
            }
            if let Some(hammer_id) = entity.hammer_id {
                cx.print(&format!("  hammerid: {hammer_id}"));
            }
            if let Some(model) = &entity.model {
                cx.print(&format!("  model: {model}"));
            }
            if let Some(parent) = &entity.parent_name {
                cx.print(&format!(
                    "  parentname: {parent} ({})",
                    match entity.parent {
                        Some(_) => "resolved",
                        None => "NOT FOUND",
                    }
                ));
            }
            if entity.effects != 0 {
                cx.print(&format!("  effects: {:#x}", entity.effects));
            }
            let next_think = entity.next_think_tick();
            if next_think != think::TICK_NEVER_THINK {
                cx.print(&format!(
                    "  next think: tick {next_think} ({:.2}s, now {:.2}s)",
                    self.clock.time().ticks_to_time(next_think),
                    self.clock.time().curtime
                ));
            }
            for (key, value) in entity.behaviour.describe() {
                cx.print(&format!("  {key}: {value}"));
            }
            for output in &entity.outputs {
                for action in &output.actions {
                    cx.print(&format!(
                        "  {} -> {}.{}({}) delay {} times {}",
                        output.name,
                        action.target,
                        action.input,
                        action.parameter.as_deref().unwrap_or(""),
                        action.delay,
                        action.times_to_fire
                    ));
                }
            }
            for (key, value) in &entity.unhandled {
                cx.print(&format!("  (unhandled) {key}: {value}"));
            }
        }
    }

    /// `ent_fire <target> [input] [value] [delay]`
    /// (`baseentity.cpp:6122`) — post an input from the console.
    ///
    /// The one way to drive entity I/O by hand, and the reason it is worth the
    /// thirty lines: everything in this module is invisible without it.
    ///
    /// Valve's version passes the issuing player as both activator and caller;
    /// there is no player entity until stage 5, so both are null — which means
    /// an `!activator` in whatever it sets off will resolve to nothing.
    ///
    /// **The delay is `atoi`, not `atof`**, in Valve's implementation, so
    /// `ent_fire x Trigger "" 0.5` fires immediately. Reproduced.
    pub fn ent_fire(&mut self, cmd: &Command, cx: &mut ExecContext<'_>) {
        let Some(target) = cmd.arg(1) else {
            cx.print("ent_fire <target> [input] [value] [delay]");
            return;
        };
        if self.map.is_none() {
            cx.print("ent_fire: no map is loaded");
            return;
        }
        let input = cmd.arg(2).unwrap_or("Use");
        let value = match cmd.arg(3) {
            Some(value) if !value.is_empty() => Variant::String(value.to_owned()),
            _ => Variant::Void,
        };
        let delay = cmd.arg(4).map_or(0, keyvalue::atoi) as f32;

        let fire_time = self.clock.time().curtime + delay;
        self.queue.add(Event {
            fire_time,
            target: Target::Name(target.to_owned()),
            input: input.to_owned(),
            value,
            activator: None,
            caller: None,
            output_id: 0,
        });
        cx.print(&format!(
            "queued {target}.{input} for {fire_time:.2}s (now {:.2}s)",
            self.clock.time().curtime
        ));
    }

    /// `dumpeventqueue` (`cbase.cpp:1010`) — everything waiting, in fire
    /// order.
    pub fn dump_event_queue(&self, cx: &mut ExecContext<'_>) {
        let now = self.clock.time().curtime;
        cx.print(&format!(
            "Dumping event queue. Current time is: {now:.2} (tick {})",
            self.clock.time().tick
        ));
        for event in self.queue.iter() {
            let target = match &event.target {
                Target::Name(name) => name.clone(),
                Target::Entity(id) => match self.entities.get(*id) {
                    Some(entity) => format!("[{}] {}", id.slot(), entity.debug_name()),
                    None => format!("[{}] <gone>", id.slot()),
                },
            };
            let who = |id: Option<EntityId>| match id.and_then(|id| self.entities.get(id)) {
                Some(entity) => entity.debug_name().to_owned(),
                None => String::from("None"),
            };
            cx.print(&format!(
                "   ({:.2}) Target: '{target}', Input: '{}', Parameter '{}'. \
                 Activator: '{}', Caller '{}'.",
                event.fire_time,
                event.input,
                event.value.to_string(),
                who(event.activator),
                who(event.caller),
            ));
        }
        cx.print(&format!("Finished dump. {} queued.", self.queue.len()));
    }

    /// `GetNextCommandEntity`'s three forms: a list index, a targetname, or a
    /// classname, tried in that order.
    fn command_entities(&self, query: &str) -> Vec<EntityId> {
        if let Ok(slot) = query.trim().parse::<u32>() {
            let by_index: Vec<EntityId> = self
                .entities
                .iter()
                .filter(|(id, _)| id.slot() == slot)
                .map(|(id, _)| id)
                .collect();
            if !by_index.is_empty() {
                return by_index;
            }
        }
        let by_name: Vec<EntityId> = name::find_by_name(&self.entities, query).collect();
        if !by_name.is_empty() {
            return by_name;
        }
        self.entities
            .iter()
            .filter(|(_, e)| e.classname().eq_ignore_ascii_case(query))
            .map(|(id, _)| id)
            .collect()
    }

    /// `CBaseEntity::ParseMapData` (`baseentity_shared.cpp:334`): every key in
    /// the block, in lump order, through `KeyValue`.
    ///
    /// # The order of the three attempts
    ///
    /// The class first, then the shared ladder, then the output table. That is
    /// Valve's: `ParseMapData` calls `KeyValue` *virtually*, so a class that
    /// overrides it — `CWorld`, `CLight`, `CEnvLight` — tests its own keys and
    /// only then calls `BaseClass::KeyValue`, which is where the if-ladder
    /// lives. The outputs are matched last because Valve matches them in the
    /// datadesc walk that `CBaseEntity::KeyValue` ends with.
    ///
    /// (One nuance not reproduced, because nothing reaches it: a key declared
    /// with `DEFINE_KEYFIELD` rather than handled by an override is matched in
    /// that final walk, so in the original it loses to the ladder rather than
    /// beating it. `StartDisabled` is the only example and no ladder key
    /// shares its name.)
    fn parse_map_data(&mut self, entity: &mut Entity, block: &bsp::Entity) {
        for (key, value) in &block.pairs {
            let Entity { core, behaviour } = &mut *entity;
            if behaviour.key_value(core, key, value) {
                continue;
            }
            if keyvalue::base_key_value(core, key, value) {
                continue;
            }
            // An output key is recognised by the *declared* name, and the
            // connection is filed under that spelling rather than the map's —
            // see [`EntityCore::add_connection`].
            let declared = core.class.declared_output(key).or_else(|| {
                keyvalue::BASE_OUTPUTS
                    .iter()
                    .find(|name| name.eq_ignore_ascii_case(key))
                    .copied()
            });
            match declared {
                Some(name) => {
                    self.next_output_id += 1;
                    core.add_connection(name, value, self.next_output_id);
                }
                None => core.unhandled.push((key.clone(), value.clone())),
            }
        }
    }
}

impl Default for Server {
    fn default() -> Server {
        Server::new()
    }
}

/// A sorted-by-count listing, truncated. Used for all three progress reports.
fn print_counts(
    cx: &mut ExecContext<'_>,
    what: &str,
    counts: &BTreeMap<String, usize>,
    limit: usize,
) {
    if counts.is_empty() {
        return;
    }
    let mut sorted: Vec<(&String, &usize)> = counts.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    cx.print(&format!(
        "{} {what} ({} occurrences):",
        sorted.len(),
        counts.values().sum::<usize>()
    ));
    for (name, count) in sorted.iter().take(limit) {
        cx.print(&format!("  {count:>6}  {name}"));
    }
    if sorted.len() > limit {
        cx.print(&format!("  … and {} more", sorted.len() - limit));
    }
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
