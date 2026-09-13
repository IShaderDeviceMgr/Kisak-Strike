//! The game server: the entity system.
//!
//! Valve's `server.so` — `game/server/` — reduced to the framework
//! `portdocs/SERVER.md` §1.1 identifies: the entity list, the class table, the
//! keyvalue parse, and the three-pass spawn. It is a sibling of
//! [`crate::client`] and [`crate::engine`] because `server.so` was a sibling of
//! `client.so` and `engine.so`.
//!
//! Stage 1 of five. What exists: entities are created from the map's entity
//! lump, their keys are parsed, they are spawned in hierarchy order and
//! activated, and what the port did not understand is counted rather than
//! dropped. What does not: entity I/O and the event queue (stage 2), thinks
//! (stage 2), movement (stage 3), touch (stage 4).
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
//! The one type it does name from outside is [`bsp::Entity`], the parsed entity
//! lump. That is Valve's shape too: `CServerGameDLL::LevelInit( pMapName,
//! pMapEntities, ... )` is handed the lump by the engine, because the engine is
//! what read the `.bsp`. Re-parsing it here would be the duplication
//! `PORTING.md` warns about.
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
//! ```

pub mod class;
pub mod classes;
pub mod entity;
pub mod keyvalue;
pub mod name;

use std::collections::BTreeMap;

use crate::engine::console::{Command, ExecContext};
use crate::engine::world::bsp;

use class::SpawnResult;
use entity::{Entity, EntityId, EntityList};

/// The server. `CServerGameDLL` plus `gEntList`.
///
/// Level-scoped, and so a field of the engine's `Scene` rather than of the
/// engine: the entity list is emptied and refilled by every map change, and
/// `Scene` is what [`Level`](crate::engine::host::Level) hands to the host.
pub struct Server {
    entities: EntityList,
    /// The map whose entities these are, for reporting. `None` between levels.
    map: Option<String>,
    stats: LevelStats,
}

/// What one `level_init` produced.
///
/// Most of this exists to answer "how much of the entity system is there yet",
/// which is the only interesting question about stage 1 and stays interesting
/// for several stages after it.
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
    /// Output connections recognised. Parsed in stage 2.
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
        Server {
            entities: EntityList::new(),
            map: None,
            stats: LevelStats::default(),
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
    pub fn level_init(&mut self, map: &str, blocks: &[bsp::Entity]) -> LevelStats {
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
            parse_map_data(&mut entity, block);
            stats.outputs += entity.outputs.len();
            // Counted here rather than after the spawn pass, so that what an
            // entity did not understand is recorded whether or not that entity
            // survived its own `Spawn`. Half the unhandled keys in the game
            // are on lights, and every unnamed light deletes itself.
            for (key, _) in &entity.unhandled {
                *stats.unhandled.entry(key.to_ascii_lowercase()).or_default() += 1;
            }

            let is_world = class.name == "worldspawn";
            if is_world {
                // `mapentities.cpp:373`: "don't allow a parent on the first
                // entity (worldspawn)". The shipped maps never give it one;
                // this is Valve's belt and braces and costs a line.
                entity.core.parent_name = None;
            }
            let id = self.entities.insert(entity);

            match is_world {
                // Spawned at once and outside the sorted list, because
                // everything else may ask about the world and nothing may ask
                // about anything else yet.
                true => {
                    if self.dispatch_spawn(id) == SpawnResult::Remove {
                        self.entities.mark_for_deletion(id);
                    }
                }
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
        // first place a class may look at another entity.
        for &id in &ordered {
            if self.dispatch_spawn(id) == SpawnResult::Remove {
                self.entities.mark_for_deletion(id);
            }
        }
        for &id in &ordered {
            let Some(entity) = self.entities.get_mut(id) else {
                continue;
            };
            if entity.removed {
                continue;
            }
            let Entity { core, behaviour } = entity;
            behaviour.activate(core);
        }

        stats.removed_on_spawn = self.entities.cleanup_delete_list();
        stats.spawned = self.entities.len();

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

    /// `DispatchSpawn` (`mapentities.cpp:74`). Split out because the borrow of
    /// the entity has to end before the caller can mark it for deletion.
    fn dispatch_spawn(&mut self, id: EntityId) -> SpawnResult {
        match self.entities.get_mut(id) {
            Some(Entity { core, behaviour }) => behaviour.spawn(core),
            None => SpawnResult::Ok,
        }
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
        self.map = None;
        self.stats = LevelStats::default();
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

        // Everything below is this port's, not Valve's: it is the stage-1
        // progress report, and it goes away as the classes land.
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
        cx.print(&format!(
            "{} output connections recognised (unparsed until stage 2)",
            stats.outputs
        ));

        print_counts(cx, "unimplemented classnames", &stats.unknown, 12);
        print_counts(cx, "keys nothing consumed", &stats.unhandled, 12);
    }

    /// `ent_dump` (`baseentity.cpp:6103`) — one entity's state, by name, by
    /// classname, or by list index.
    ///
    /// `GetNextCommandEntity` accepts all three and so does this. The state it
    /// prints is [`EntityCore`](entity::EntityCore)'s plus whatever the class
    /// says in [`Behaviour::describe`](class::Behaviour::describe), which is
    /// what replaces `DumpEntity`'s walk over a datadesc that no longer
    /// exists.
    pub fn ent_dump(&self, cmd: &Command, cx: &mut ExecContext<'_>) {
        let Some(query) = cmd.arg(1) else {
            cx.print("ent_dump <entity name / index / class>");
            return;
        };
        if self.map.is_none() {
            cx.print("ent_dump: no map is loaded");
            return;
        }

        let by_index: Vec<EntityId> = match query.trim().parse::<u32>() {
            Ok(slot) => self
                .entities
                .iter()
                .filter(|(id, _)| id.slot() == slot)
                .map(|(id, _)| id)
                .collect(),
            Err(_) => Vec::new(),
        };
        let by_name: Vec<EntityId> = name::find_by_name(&self.entities, query).collect();
        let by_class: Vec<EntityId> = self
            .entities
            .iter()
            .filter(|(_, e)| e.classname().eq_ignore_ascii_case(query))
            .map(|(id, _)| id)
            .collect();

        // Valve's order: index, then name, then classname.
        let found = match (by_index.is_empty(), by_name.is_empty()) {
            (false, _) => by_index,
            (true, false) => by_name,
            (true, true) => by_class,
        };
        if found.is_empty() {
            cx.print("ent_dump: no such entity");
            return;
        }
        for id in found {
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
            for (key, value) in entity.behaviour.describe() {
                cx.print(&format!("  {key}: {value}"));
            }
            for (key, value) in &entity.outputs {
                // The lump's ESC delimiter would be invisible in the console,
                // so it is shown as the `,` a mapper typed in Hammer.
                cx.print(&format!("  output {key}: {}", value.replace('\u{1b}', ",")));
            }
            for (key, value) in &entity.unhandled {
                cx.print(&format!("  (unhandled) {key}: {value}"));
            }
        }
    }
}

/// A sorted-by-count listing, truncated. Used for both progress reports.
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

/// `CBaseEntity::ParseMapData` (`baseentity_shared.cpp:334`): every key in the
/// block, in lump order, through `KeyValue`.
///
/// # The order of the three attempts
///
/// The class first, then the shared ladder, then the output table. That is
/// Valve's: `ParseMapData` calls `KeyValue` *virtually*, so a class that
/// overrides it — `CWorld`, `CLight`, `CEnvLight` — tests its own keys and
/// only then calls `BaseClass::KeyValue`, which is where the if-ladder lives.
/// The outputs are matched last because Valve matches them in the datadesc
/// walk that `CBaseEntity::KeyValue` ends with.
///
/// (One nuance not reproduced, because nothing reaches it: a key declared with
/// `DEFINE_KEYFIELD` rather than handled by an override is matched in that
/// final walk, so in the original it loses to the ladder rather than beating
/// it. `StartDisabled` is the only stage-1 example and no ladder key shares
/// its name.)
fn parse_map_data(entity: &mut Entity, block: &bsp::Entity) {
    for (key, value) in &block.pairs {
        let Entity { core, behaviour } = &mut *entity;
        if behaviour.key_value(core, key, value) {
            continue;
        }
        if keyvalue::base_key_value(core, key, value) {
            continue;
        }
        let is_output = core.class.declares_output(key)
            || keyvalue::BASE_OUTPUTS
                .iter()
                .any(|o| o.eq_ignore_ascii_case(key));
        match is_output {
            true => core.outputs.push((key.clone(), value.clone())),
            false => core.unhandled.push((key.clone(), value.clone())),
        }
    }
}

#[cfg(test)]
mod tests;
