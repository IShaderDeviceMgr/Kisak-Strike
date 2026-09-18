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
pub mod damage;
pub mod entity;
pub mod io;
pub mod keyvalue;
pub mod movement;
pub mod name;
pub mod obb;
pub mod random;
pub mod sequences;
pub mod think;
pub mod touch;

use std::collections::BTreeMap;

use glam::Vec3;

use crate::client::tonemap::TonemapSettings;
use crate::engine::console::{Command, ExecContext};
use crate::engine::world::bsp;

use class::{base_accept_input, Behaviour, Context, SpawnResult};
use damage::{DamageInfo, Damaged, LifeState};
use entity::{Entity, EntityCore, EntityId, EntityList};
use io::{Event, EventQueue, FieldType, Input, IoStats, Target, Variant};
use movement::ModelBounds;
use name::Procedural;
use random::RandomStream;
use sequences::SequenceTable;
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
    /// `CEntityTouchManager::m_updateList` — the entities that owe the
    /// post-think pass a stale-link sweep. See [`touch`].
    untouch_list: Vec<EntityId>,
    /// The player, once the engine has spawned one.
    ///
    /// `UTIL_PlayerByIndex( 1 )`, which is what `!player` resolves to. `None`
    /// between levels and in every test that does not need one — the entity
    /// list has no player of its own, exactly as Valve's has none until a
    /// client connects.
    player: Option<EntityId>,
    /// Whether the player's hull was the crouched one at the end of the last
    /// tick — the falling edge of it is `CPortal_Player::UnDuck()`. See
    /// [`Server::player_pre_think`].
    player_was_ducked: bool,
    /// Where the player was when the touch pass last ran.
    ///
    /// The *start* of the swept box the next pass tests, which is what stops a
    /// fast player passing through a thin trigger between two ticks —
    /// `PhysicsTouchTriggers( &vecPrevOrigin )`. Updated by the pass and by
    /// nothing else, so it spans however many rendered frames a tick took.
    player_prev_origin: Vec3,
    /// Scratch for the touch query, so that a tick does not allocate.
    overlaps: Vec<usize>,
    /// Scratch for the [`Solid::Obb`](movement::Solid::Obb) half of the same
    /// query — the triggers this module tests itself. See
    /// [`Server::obb_triggers_touching`].
    obb_overlaps: Vec<EntityId>,
    /// Entities [`Context::create_entity`] made and that have not been spawned
    /// yet — `DispatchSpawn`, deferred by one dispatch. See
    /// [`Server::flush_created`].
    pending_spawn: Vec<EntityId>,
    /// Re-entrancy guard for [`Server::flush_created`]: only the outermost
    /// dispatch drains the queue, so a chain of creations is a loop rather
    /// than a stack.
    spawning: bool,
    /// Entities created during [`Server::level_init`]'s spawn pass, which
    /// therefore still owe an `Activate`.
    ///
    /// `ServerActivate` (`gameinterface.cpp:1316`) walks the whole live entity
    /// list rather than the spawn list, so anything a `Spawn` created is
    /// reached by it. Anything created *after* the level has loaded is not,
    /// and gets a `Spawn` and nothing else — which is also Valve's.
    created_while_loading: Vec<EntityId>,
    /// Whether [`Server::level_init`] is between its spawn pass and its
    /// activate pass, which is what makes the field above meaningful.
    level_loading: bool,
    /// Damage [`Context::take_damage`] queued and that has not been applied
    /// yet — `TakeDamage`, deferred by one dispatch. See
    /// [`Server::flush_damage`].
    pending_damage: Vec<(EntityId, DamageInfo)>,
    /// Re-entrancy guard for [`Server::flush_damage`], the twin of
    /// [`spawning`](Server::spawning).
    damaging: bool,
    /// The map [`Context::reload_level`] asked for, until the engine takes it.
    ///
    /// `engine->ServerCommand( "reload\n" )` in one process and with no
    /// saves — see [`Server::take_level_restart`].
    level_restart: Option<String>,
    /// `modelinfo->GetModelPtr`, answered in advance — see
    /// [`sequences`] and [`Server::set_sequences`].
    ///
    /// Empty until the engine has loaded the level's models, which is *after*
    /// `level_init`, and empty for ever in a test with no engine.
    sequences: SequenceTable,
}

/// The engine's half of a touch test — `engine->SolidMoved`
/// (`vengineserver_impl.cpp:2467`, `engine/world.cpp`'s `CTouchLinks`).
///
/// The game knows which entities are triggers and what touching one means; the
/// *engine* owns the collision data and answers "what does this swept box
/// overlap". Keeping that split is what lets this module name no `world/` and
/// no `trace/` type, and it is not a Rust invention: the C++ crosses a DLL
/// boundary at exactly this line.
///
/// Implemented in `engine/mod.rs` over `world/`'s placed brush models. The
/// answers are `"*N"` **model indices**, the same join key stage 3 established
/// for brush-entity placement, so nothing has to carry an entity handle across
/// the boundary in either direction.
pub trait TouchQuery {
    /// Every placed brush model the box `mins`-`maxs`, swept from `start` to
    /// `end`, intersects. Appends; does not clear.
    ///
    /// `mins`/`maxs` are relative to the box's position, so a player hull is
    /// `(-16,-16,0)`-`(16,16,72)`.
    ///
    /// **It reports solid brush models too**, and that is deliberate: which of
    /// them is a trigger is a question about `FSOLID_TRIGGER`, which is the
    /// game's state and would be a frame stale if the engine kept a copy. The
    /// caller filters, and the cost is one extra brush sweep per non-trigger
    /// brush entity per tick.
    fn brush_models_touching(
        &mut self,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        out: &mut Vec<usize>,
    );
}

/// The player, as the two halves of the port that own pieces of it agree to
/// describe one.
///
/// # Why it is a copy in both directions
///
/// `client::Player` moves on the **rendered frame** and the server ticks at a
/// fixed 64 Hz (`portdocs/SERVER.md` §5), so neither can hold the other's
/// state. `Engine::frame` therefore copies this in before the ticks and out
/// after them, and the round trip is an identity for every field the server
/// did not touch — which is what makes "always copy back" safe rather than a
/// fight over who owns the origin.
///
/// The fields are what stages 4 and 5 reach: what the touch query sweeps
/// (`origin`, `mins`, `maxs`), what `PassesTriggerFilters` and
/// `CTriggerPush::Touch` branch on (`move_type`, `on_ground`), what a push or
/// a teleport writes (`velocity`, `base_velocity`, `origin`, `angles`), and
/// what damage and death change (`health`, `life_state`, `move_type`,
/// `flags`).
///
/// > **`angles` are the *view* angles**, where `CBasePlayer` keeps
/// > `m_angAbsRotation` (yaw only) and its eye angles separately. Every
/// > consumer here wants the eye — `CTriggerTeleport::Touch` explicitly
/// > substitutes `EyeAngles()` for `GetAbsAngles()` when the toucher is a
/// > player — so the port keeps one field and this note.
///
/// # Not every field goes both ways, and stage 5 is where that started
///
/// Through stage 4 the round trip was an identity for everything the server
/// did not touch, and *every* field went in and came out. Stage 5 gives the
/// server sole ownership of four of them —
/// [`move_type`](PlayerState::move_type), [`health`](PlayerState::health),
/// [`life_state`](PlayerState::life_state) and
/// [`flags`](PlayerState::flags) — because `noclip`, damage and death are all
/// server decisions in the original and all three would be lost if the client
/// wrote them back. [`Server::set_player_state`] ignores what arrives in
/// those four; [`Server::player_state`] fills them in.
///
/// [`buttons`](PlayerState::buttons) is the mirror image and the only field
/// that is **purely** the client's: the server reads it and never writes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerState {
    /// `m_vecAbsOrigin` — the **feet**, not the eye.
    pub origin: Vec3,
    /// The view angles, pitch/yaw/roll. See the type's docs.
    pub angles: Vec3,
    pub velocity: Vec3,
    /// `m_vecBaseVelocity` — what a `trigger_push` is adding.
    pub base_velocity: Vec3,
    /// `FL_ONGROUND`.
    pub on_ground: bool,
    /// `m_MoveType` — **the server's**, since stage 5.
    ///
    /// `noclip` is a `game/server/` command in the original because move type
    /// is server state that gets networked down (`portdocs/CLIENT.md` §9.2),
    /// and `CBasePlayer::Event_Killed` writes `MOVETYPE_FLYGRAVITY` into it,
    /// so the client cannot own it and then be told it died. The client reads
    /// this and runs whichever move it names.
    pub move_type: movement::MoveType,
    /// The collision hull, relative to [`origin`](PlayerState::origin).
    /// Changes when the player ducks, which is why it is here rather than a
    /// constant.
    pub mins: Vec3,
    pub maxs: Vec3,
    /// `m_iHealth` — **the server's**.
    ///
    /// The client reads it for one thing and it is not a HUD:
    /// `CGameMovement::IsDead` is `m_iHealth <= 0` and decides whether the
    /// movement takes any input at all.
    pub health: i32,
    /// `m_lifeState` — **the server's**.
    pub life_state: LifeState,
    /// The `FL_*` bits — **the server's**, since stage 5, for the two that
    /// matter to the movement: `FL_FROZEN` and `FL_ONGROUND`.
    ///
    /// > **`FL_ONGROUND` is the one field that goes both ways**, and it has
    /// > to: the client finds the ground plane and the server takes the player
    /// > off it (a push, a teleport). [`on_ground`](PlayerState::on_ground)
    /// > carries it and this does not — the bit is masked out of `flags` in
    /// > both directions so that the two can never disagree.
    pub flags: u32,
    /// `m_nButtons` — **the client's**, read by the server and never written.
    ///
    /// `IN_*`, as raw bits, because `ButtonBits` is a `client/` type and this
    /// struct is the one place the two halves are allowed to agree on a number
    /// rather than on a type. Read by `PlayerDeathThink`, which waits for
    /// every button to come up and then for any to go down, and by
    /// `logic_playerproxy`, which fires `OnJump` and `OnDuck` on the press
    /// edge.
    pub buttons: u32,
}

/// One entity's studio model, as the renderer needs to see it.
///
/// `world/` names no server type and `server/` names no studio or GPU type, so
/// this is the vocabulary between them — the same arrangement [`PlayerState`]
/// and `world/`'s `Placement` already have.
///
/// # The list is keyed, and it used to be positional
///
/// `engine::world::entities::EntityModels` loads from
/// [`Server::model_entities`] once and syncs against it every frame, matching
/// on [`id`](ModelEntityState::id). It matched by *position* while
/// `prop_floor_button` was the only class here, and the note in this place
/// said the condition for a real key would be "the first class that appears or
/// disappears at run time". `prop_dynamic` is that class twice over: **556
/// shipped connections fire `Kill` at one** and 51 fire `FadeAndKill`, and a
/// positional list would re-point every instance after the one that went.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntityState {
    /// [`EntityId::to_int`] — an opaque, stable key.
    ///
    /// Opaque on purpose: `world/` must not name an `EntityId`, and it has no
    /// use for one beyond telling two instances apart.
    pub id: u64,
    /// The model path, e.g. `models/props/portal_button.mdl`.
    pub model: String,
    pub origin: Vec3,
    /// Pitch, yaw, roll.
    pub angles: Vec3,
    pub skin: i32,
    /// `C_BaseEntity::ShouldDraw` — `false` for `EF_NODRAW`.
    ///
    /// > **Carried rather than filtered**, which is the other half of the
    /// > keying above. 1,000 of the game's `prop_dynamic`s are `StartDisabled`
    /// > and 206 connections turn one on or off, so an entity that is invisible
    /// > now is one that may be visible next tick — and dropping it from the
    /// > list would mean the renderer had never uploaded its model.
    pub visible: bool,
    /// The sequence label — see
    /// [`ModelState::sequence`](class::ModelState::sequence). Owned here
    /// because the seam outlives the borrow.
    pub sequence: String,
    /// `m_flCycle` at [`anim_time`](ModelEntityState::anim_time).
    pub cycle: f32,
    /// The **server's** clock when the pose above was true. See
    /// [`ModelState::anim_time`](class::ModelState::anim_time) for why that is
    /// not the scene's.
    pub anim_time: f32,
    /// `m_flPlaybackRate`, signed. Zero holds the pose.
    pub playback_rate: f32,
    /// [`EntityCore::modulation`] — `rendercolor` and the render mode's alpha,
    /// as the draw wants them. An alpha below 1 is what makes an entity
    /// translucent.
    pub modulation: [f32; 4],
}

/// One active portal, as the renderer needs to see it.
///
/// The third seam of this shape, after [`PlayerState`] and
/// [`ModelEntityState`]: `world/` names no server type and `server/` names no
/// GPU type, so the vocabulary between them is a plain struct that
/// `Engine::frame` copies across once a rendered frame.
///
/// **What it does not carry is the teleport matrix**, because nothing draws
/// with it: `portdocs/PORTAL.md`'s stage 2 is an oval on a wall, not a view
/// through it. The matrix stays on
/// [`PropPortal::matrix`](classes::PropPortal::matrix) until stage 4 moves the
/// player with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortalState {
    /// [`EntityId::to_int`] — an opaque, stable key, for the reason
    /// [`ModelEntityState::id`] is one.
    pub id: u64,
    pub origin: Vec3,
    /// Pitch, yaw, roll. The quad's own basis comes out of this, and the
    /// **right** vector it needs is the negation of the angle matrix's second
    /// column — see [`PropPortal::right`](classes::PropPortal::right).
    pub angles: Vec3,
    /// `m_fNetworkHalfWidth` — 32 for every portal in the shipped game.
    pub half_width: f32,
    /// `m_fNetworkHalfHeight` — 56, not 14; see
    /// [`portal::DEFAULT_HALF_HEIGHT`](classes::portal::DEFAULT_HALF_HEIGHT).
    pub half_height: f32,
    /// Which of the two overlay materials to wear. Blue is `false`.
    pub is_portal2: bool,
    /// The server clock when this portal was switched on or moved.
    ///
    /// The renderer turns it into `$PortalOpenAmount` and `$PortalStatic`,
    /// which `C_Prop_Portal::ClientThink` (`c_prop_portal.cpp:222`) runs up
    /// from 0 and down from 1 at fixed rates. Carried as the instant rather
    /// than as the two curves for the reason
    /// [`ModelEntityState::anim_time`] is: a 64 Hz tick would step an effect
    /// that has to be smooth.
    pub opened_at: f32,
    /// The partner this portal found, if it found one — the same key
    /// [`id`](PortalState::id) is.
    ///
    /// Nothing in the *draw* reads it: an active portal wears its oval linked
    /// or not, which is Valve's, because `ShouldDraw` asks only about
    /// `IsActive()`. The collision does — a portal with no partner cuts its
    /// hole and has nothing on the far side of it — and so does the console,
    /// because it is the one thing about a portal a developer wants told and
    /// `ent_dump` is on the other side of the seam.
    pub linked: Option<u64>,
    /// `m_matrixThisToLinked` — where a point at this portal comes out.
    ///
    /// **The identity while unlinked**, which is what `CPortal_Base2D`'s own
    /// field is. Carried across the seam rather than recomputed on the far
    /// side so that there is exactly one teleport matrix in the port and no
    /// second spelling of the 180° that can be missing from it; see
    /// [`teleport_matrix`](classes::portal::teleport_matrix).
    pub matrix: glam::Mat4,
}

/// A [`TouchQuery`] that never reports anything.
///
/// What a server with no map loaded — or a unit test with no collision —
/// touches. `Server::frame` takes `&mut dyn TouchQuery` rather than an
/// `Option` because the query is asked at most once per tick and a
/// do-nothing implementation reads better at both call sites than a `None`
/// does.
pub struct NoTouchQuery;

impl TouchQuery for NoTouchQuery {
    fn brush_models_touching(&mut self, _: Vec3, _: Vec3, _: Vec3, _: Vec3, _: &mut Vec<usize>) {}
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
    /// Entities that were **not** in the entity lump — made by another
    /// entity's `Spawn` through [`Context::create_entity`](class::Context::create_entity).
    ///
    /// The `trigger_portal_button` every `prop_floor_button` puts over itself,
    /// and so far nothing else. It is broken out because it is the one term
    /// that makes `spawned + removed_on_spawn` differ from `matched`, and a
    /// silent difference there would look like an entity going missing.
    pub created: usize,
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
            "{} of {} entity blocks matched a class, {} spawned ({} removed themselves, \
             {} created by another entity), {} outputs, {} unknown classnames, \
             {} unhandled keys",
            self.matched,
            self.blocks,
            self.spawned,
            self.removed_on_spawn,
            self.created,
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
            untouch_list: Vec::new(),
            player: None,
            player_was_ducked: false,
            player_prev_origin: Vec3::ZERO,
            overlaps: Vec::new(),
            obb_overlaps: Vec::new(),
            pending_spawn: Vec::new(),
            spawning: false,
            created_while_loading: Vec::new(),
            level_loading: false,
            pending_damage: Vec::new(),
            damaging: false,
            level_restart: None,
            sequences: SequenceTable::new(),
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
        self.level_loading = true;
        for &id in &ordered {
            self.dispatch_spawn(id);
        }
        self.level_loading = false;

        // `ServerActivate` walks `gEntList`, not the spawn list, so an entity
        // a `Spawn` created — a `prop_floor_button`'s trigger — is activated
        // too. Appended rather than merged: the ordering within `ordered` is
        // the hierarchy sort and there is nothing to sort a runtime creation
        // into.
        let created = std::mem::take(&mut self.created_while_loading);
        stats.created = created.len();
        for &id in ordered.iter().chain(created.iter()) {
            self.dispatch(id, |core, behaviour, cx| {
                if !core.removed {
                    // `CBaseEntity::Activate`'s own body, which is two lines
                    // and one of them is this (`baseentity.cpp:1782`). It runs
                    // *before* the class's, exactly as `BaseClass::Activate()`
                    // at the top of an override does — and it has to be here
                    // rather than at spawn, because the filter it names may
                    // not have existed yet.
                    if let Some(name) = core.damage_filter_name.clone() {
                        core.damage_filter = cx.find_by_name(&name);
                    }
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
        self.untouch_list.clear();
        self.player = None;
        self.player_was_ducked = false;
        self.player_prev_origin = Vec3::ZERO;
        self.pending_damage.clear();
        // **Not `level_restart`**: the whole point of it is to survive
        // `level_shutdown`, because the shutdown is what it asked for.
        self.overlaps.clear();
        self.obb_overlaps.clear();
        self.pending_spawn.clear();
        self.created_while_loading.clear();
        self.level_loading = false;
        self.sequences = SequenceTable::new();
    }

    /// What `studio/` says about the models this level's entities place.
    ///
    /// Called once by the engine, **after** [`level_init`](Server::level_init)
    /// — the models are named by the entities, so there is nothing to load
    /// until they exist. See [`sequences`] for the consequence, which is that
    /// every `Spawn` in the game runs against an empty table and every class
    /// has to know it.
    pub fn set_sequences(&mut self, sequences: SequenceTable) {
        self.sequences = sequences;
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
    /// `query` is the engine's collision half — see [`TouchQuery`]. Pass
    /// [`NoTouchQuery`] when there is no map to sweep against, which is what
    /// every test that is not about touching does.
    pub fn frame(&mut self, frame_time: f32, query: &mut dyn TouchQuery) -> u32 {
        if self.map.is_none() {
            return 0;
        }
        let ticks = self.clock.accumulate(frame_time);
        for _ in 0..ticks {
            self.clock.advance();
            self.run_tick(query);
        }
        ticks
    }

    /// One server tick. `CServerGameDLL::GameFrame` (`gameinterface.cpp:1383`).
    ///
    /// The steps that survive, in Valve's order. Three things about that order
    /// are observable and maps depend on all three:
    ///
    /// - **`ServiceEventQueue` runs once, after every think**, so an output a
    ///   think fires is delivered in the same tick and an input handler cannot
    ///   see a think that has not run yet.
    /// - **The player's touch test runs before the thinks**, because in the
    ///   original it is part of `CBasePlayer::PhysicsSimulate` and the player
    ///   is entity index 1 — so a `trigger_multiple`'s `OnTrigger` is queued
    ///   before the same tick's thinks rather than after them.
    /// - **`EndTouch` is detected between the thinks and the queue**
    ///   (`FrameUpdatePostEntityThinkAllSystems`), so an `OnEndTouch` is
    ///   delivered in the tick it happened rather than the next one.
    fn run_tick(&mut self, query: &mut dyn TouchQuery) {
        // Anything removed outside the loop — by a console command, say.
        self.cleanup_delete_list();
        // `CPlayerMove::CheckMovingGround`, which in the original is the first
        // thing the player's own simulation does.
        self.check_moving_ground();
        // `CBasePlayer::PreThink` — which is **not** a think: it is called
        // once per usercmd from `CPlayerMove::RunCommand`, before the movement
        // and before `PhysicsSimulate`. The think schedule cannot express
        // "every tick, first", so it is a step of the tick like the two
        // either side of it.
        self.player_pre_think();
        self.player_touch_triggers(query);
        self.run_think_functions();
        self.check_for_entity_untouch();
        self.service_events();
        // Anything a think or an input removed.
        self.cleanup_delete_list();
    }

    /// `CPortal_Player::PreThink` (`portal_player.cpp:1855`), reduced to the
    /// three lines that reach a map.
    ///
    /// All three are `FirePlayerProxyOutput` calls, and the proxy is found the
    /// way `CBasePlayer::GetPlayerProxy` finds it — `FindEntityByClassname`,
    /// first match. 9 entities in the game and no map has two. Valve caches
    /// the handle and this does not; see the body.
    ///
    /// > **The duck events are not symmetric in the original and are not here
    /// > either.** `OnJump` and `OnDuck` fire off `m_afButtonPressed`, which is
    /// > the *button*; `OnUnDuck` fires from `CPortal_Player::UnDuck()`
    /// > (`portal_player_shared.cpp:4710`), which the movement calls when the
    /// > hull has actually grown back. So a duck that is refused — standing
    /// > under something too low to un-crouch — fires `OnDuck` and no
    /// > `OnUnDuck` until it succeeds.
    ///
    /// What is left out of `PreThink`, and each needs a subsystem: the air
    /// control decay and the tractor-beam gravity (paint), `Jump()` itself
    /// (the client's), `ZoomIn`/`ZoomOut`, and `playtest_random_death` — a
    /// cvar that kills the player every 30 to 120 seconds, which is exactly
    /// the kind of thing not to port by accident.
    fn player_pre_think(&mut self) {
        let Some(player) = self.player else {
            return;
        };
        let Some(entity) = self.entities.get(player) else {
            return;
        };
        let Some(class) = entity.behaviour.downcast_ref::<classes::Player>() else {
            return;
        };
        let pressed = class.pressed_buttons();
        let ducked = entity.core.model_bounds.maxs.z <= classes::DUCK_HULL_HEIGHT;
        let unducked = self.player_was_ducked && !ducked;
        self.player_was_ducked = ducked;

        // `GetPlayerProxy()` — `FindEntityByClassname( NULL,
        // "logic_playerproxy" )`, resolved every tick rather than cached,
        // because a cache would have to be invalidated and the scan is over a
        // list a map has at most one match in.
        let Some(proxy) = self
            .entities
            .iter()
            .find(|(_, e)| e.core.class.name == "logic_playerproxy")
            .map(|(id, _)| id)
        else {
            return;
        };

        for (fires, output) in [
            (pressed & classes::IN_JUMP != 0, "OnJump"),
            (pressed & classes::IN_DUCK != 0, "OnDuck"),
            (unducked, "OnUnDuck"),
        ] {
            if !fires {
                continue;
            }
            // `FirePlayerProxyOutput( name, variant_t(), this, this )` — the
            // *player* is both activator and caller, not the proxy, which is
            // what makes `!activator` in the chain resolve to the player.
            self.dispatch(proxy, |core, _behaviour, cx| {
                core.fire_output(output, Variant::Void, Some(player), Some(player), 0.0, cx);
            });
        }
    }

    /// `CBasePlayer::PhysicsSimulate`'s `PhysicsTouchTriggers( &vecPrevOrigin )`
    /// (`baseentity_shared.cpp:2800`), for the one entity in this port that
    /// moves under its own power.
    ///
    /// The player is `IsSolid()` and is not a trigger, so it takes the
    /// `isSolidCheckTriggers` branch: sweep its hull from where it was to
    /// where it is, and mark everything with `FSOLID_TRIGGER` that the sweep
    /// meets.
    ///
    /// > **The sweep starts at the last *tick*'s origin, not the last frame's.**
    /// > `player_prev_origin` is written only here, so at 200 fps and 64 Hz it
    /// > spans the three rendered frames since the previous tick — which is
    /// > exactly what stops a sprinting player crossing a one-unit-thick
    /// > trigger between two ticks without ever being inside it.
    fn player_touch_triggers(&mut self, query: &mut dyn TouchQuery) {
        let Some(player) = self.player else {
            return;
        };
        let Some(entity) = self.entities.get(player) else {
            // The handle stopped resolving — a `Kill` at `!player`, which two
            // shipped connections send.
            self.player = None;
            return;
        };
        if !entity.core.is_solid() {
            return;
        }
        let (origin, mins, maxs) = (
            entity.core.origin,
            entity.core.model_bounds.mins,
            entity.core.model_bounds.maxs,
        );
        let start = std::mem::replace(&mut self.player_prev_origin, origin);

        // `SetCheckUntouch( true )` — before the marks, so that this tick's
        // stamp is what they are written with and last tick's are stale.
        self.set_check_untouch(player);

        let mut overlaps = std::mem::take(&mut self.overlaps);
        overlaps.clear();
        query.brush_models_touching(start, origin, mins, maxs, &mut overlaps);
        // The other half of the same enumeration, and the reason it is a
        // separate call rather than a second kind of answer from the engine:
        // a `SOLID_OBB` trigger is a box the *game* made out of numbers it
        // owns, so there is nothing to ask the engine about. See
        // [`obb`](self::obb).
        //
        // **Both halves are collected before either is handled**, which is
        // `CTouchLinks`'s own shape: `EnumElement` fills `m_TouchedEntities`
        // and `HandleTouchedEntities` runs afterwards. It matters because a
        // handler can move the player — a `trigger_teleport` does — and every
        // candidate this tick is meant to have been tested against the same
        // swept box.
        let obb_found = self.obb_triggers_touching(start, origin, mins, maxs);
        // Taken rather than borrowed: the loop dispatches into behaviours,
        // which reach `&mut Server` through `Context`.
        let found = std::mem::take(&mut overlaps);
        for index in found {
            // `GetRequiredTriggerFlags()` for a solid non-trigger is
            // `FSOLID_TRIGGER`, and `CTouchLinks::EnumElement` requires every
            // bit of it. The engine reports solid brush models too — see
            // [`TouchQuery::brush_models_touching`] — and this is the line
            // that drops them.
            let Some(trigger) = self.brush_entity_id(index) else {
                continue;
            };
            let is_trigger = self
                .entities
                .get(trigger)
                .is_some_and(|e| e.core.is_solid_flag_set(movement::FSOLID_TRIGGER));
            if !is_trigger {
                continue;
            }
            // `serverGameEnts->MarkEntitiesAsTouching( m_TouchedEntities[i], m_pEnt )`
            // — **the trigger first**, which is what decides that the
            // trigger's link is the one carrying `FTOUCHLINK_START_TOUCH`.
            self.mark_entities_as_touching(trigger, player);
        }
        self.overlaps = overlaps;

        let mut obb_found = obb_found;
        for &trigger in obb_found.iter() {
            self.mark_entities_as_touching(trigger, player);
        }
        obb_found.clear();
        self.obb_overlaps = obb_found;

        // > **A teleport discards the swept-from point.** Something in that
        // > loop may have moved the player — a `trigger_teleport` does it from
        // > inside its own `Touch` — and the next tick's sweep must start
        // > where the player *is*, not where it was before the teleport.
        // > Valve gets this by `CBaseEntity::Teleport` calling
        // > `PhysicsTouchTriggers()` with **no** previous origin, and without
        // > it a teleport that lands you 1,000 units away sweeps a box the
        // > length of the level and fires every trigger between the two.
        if let Some(entity) = self.entities.get(player) {
            if entity.core.origin != origin {
                self.player_prev_origin = entity.core.origin;
            }
        }
    }

    /// The [`Solid::Obb`](movement::Solid::Obb) half of `CTouchLinks`'s
    /// enumeration — every box trigger the swept hull meets.
    ///
    /// Returns the scratch buffer, emptied by the caller and handed back; the
    /// list is short enough that it is usually still capacity zero.
    ///
    /// > **This is a linear scan of the entity list and Valve's is a spatial
    /// > partition query.** `SpatialPartition()->EnumerateElementsAlongRay`
    /// > exists because Valve's list is every entity in a map — 598 blocks on
    /// > `sp_a1_intro1`, and the partition is shared with tracing and
    /// > rendering. Here the scan reads two fields per entity and the
    /// > geometry runs only for the handful that pass both: across all 106
    /// > shipped maps the *whole game* has 65 `SOLID_OBB` triggers — one per
    /// > `prop_floor_button` — and no map has more than four. `spatialpartition.cpp` is not ported and
    /// > `ENGINE_TRACE.md` §5 says why; the condition for revisiting this is a
    /// > class that makes box triggers in bulk.
    fn obb_triggers_touching(
        &mut self,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
    ) -> Vec<EntityId> {
        let mut found = std::mem::take(&mut self.obb_overlaps);
        found.clear();
        for (id, entity) in self.entities.iter() {
            let core = &entity.core;
            // `GetRequiredTriggerFlags()` for a solid non-trigger is
            // `FSOLID_TRIGGER`, and `EnumElement` requires every bit of it —
            // the same line that drops the solid brush models above.
            if core.solid != movement::Solid::Obb
                || !core.is_solid_flag_set(movement::FSOLID_TRIGGER)
            {
                continue;
            }
            if obb::swept_box_touches_obb(
                start,
                end,
                mins,
                maxs,
                core.origin,
                core.angles,
                core.model_bounds.mins,
                core.model_bounds.maxs,
            ) {
                found.push(id);
            }
        }
        found
    }

    /// `CPlayerMove::CheckMovingGround` (`player_command.cpp:93`) — turn a
    /// push that has stopped into momentum.
    ///
    /// > **The pair of a base velocity and its flag is what makes a
    /// > `trigger_push` let go.** While the trigger is pushing it sets both
    /// > every tick; the tick after the player leaves, the flag is clear and
    /// > the accumulated base velocity is added to the real velocity — with a
    /// > `1 + frametime/2` boost, which is Valve's and is why walking out of a
    /// > blower throws you rather than dropping you.
    ///
    /// The `FL_CONVEYOR` branch above it needs a ground *entity*, which this
    /// port does not track; no Portal 2 entity sets the flag
    /// (`CFuncMoveLinear::Spawn` has the one call commented out, with a name
    /// and a reason).
    fn check_moving_ground(&mut self) {
        let Some(player) = self.player else {
            return;
        };
        let interval = self.clock.time().interval;
        let Some(entity) = self.entities.get_mut(player) else {
            return;
        };
        let core = &mut entity.core;
        if core.flags & movement::FL_BASEVELOCITY == 0 {
            core.velocity += (1.0 + interval * 0.5) * core.base_velocity;
            core.base_velocity = Vec3::ZERO;
        }
        core.flags &= !movement::FL_BASEVELOCITY;
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
                match name::find_procedural(
                    query,
                    event.caller,
                    event.activator,
                    event.caller,
                    self.player,
                ) {
                    Procedural::Resolved(Some(id)) => {
                        targets.push(id);
                        found = true;
                    }
                    // A null activator is a legitimate answer in Valve too;
                    // the event simply reaches nothing.
                    Procedural::Resolved(None) => {}
                    Procedural::Unavailable => {
                        *self
                            .io
                            .unhandled
                            .entry(format!("{query} (no such player)"))
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
    ///
    /// # The entity is lifted out of the list while it runs
    ///
    /// Stage 4 gave [`Context`] the entity list, because a trigger has to ask
    /// its filter about the toucher and then push or teleport it. The entity
    /// being dispatched is [`detach`](EntityList::detach)ed for the duration
    /// and put back afterwards, which is what makes the two borrows disjoint —
    /// see the type's docs for the one rule that follows.
    ///
    /// **It is put back on every path**, including the one where `f` panics:
    /// there is no `?` between the detach and the attach.
    fn dispatch<R>(
        &mut self,
        id: EntityId,
        f: impl FnOnce(&mut EntityCore, &mut dyn Behaviour, &mut Context<'_>) -> R,
    ) -> Option<R> {
        let time = self.clock.time();
        let player = self.player;
        let Server {
            entities,
            queue,
            random,
            sequences,
            ..
        } = self;
        let mut entity = entities.detach(id)?;
        let mut cx = Context::new(time, queue, random, entities, player, sequences);
        let result = f(&mut entity.core, &mut *entity.behaviour, &mut cx);
        let changed = cx.take_changed();
        let created = cx.take_created();
        let damage = cx.take_damage_queue();
        let reload = cx.take_reload_level();
        let next_think = entity.core.next_think_tick();
        // `CheckHasGamePhysicsSimulation`, which `SetMoveDoneTime` and
        // `SetMoveType` both call — reconciled here for the same reason the
        // think schedule is: every place either can change is a place that has
        // a `Context`, and every place that has a `Context` goes through this
        // function.
        let simulates = entity.core.will_simulate_game_physics();
        let removed = entity.core.removed;
        entities.attach(id, entity);

        // `SimThink_EntityChanged` (`entitylist.cpp:302`), for the dispatched
        // entity and for anything it reached through `Context::entity_mut`.
        self.thinks
            .entity_changed(id, next_think, simulates, removed);
        for other in changed {
            let Some(other_entity) = self.entities.get(other) else {
                continue;
            };
            let (next_think, simulates, removed) = (
                other_entity.core.next_think_tick(),
                other_entity.core.will_simulate_game_physics(),
                other_entity.core.removed,
            );
            self.thinks
                .entity_changed(other, next_think, simulates, removed);
        }

        // `DispatchSpawn( pEnt )`, which in the C++ the creator calls itself
        // part-way through its own handler. It happens here instead, on the
        // way out — see [`Context::create_entity`] for why, and
        // [`Server::flush_created`] for what stops it recursing.
        self.pending_spawn.extend(created);
        self.flush_created();

        // `pOther->TakeDamage( info )`, which in the C++ the hurter calls
        // itself part-way through its own think. It happens here instead and
        // for exactly the reason the spawn above does — see
        // [`Context::take_damage`] — and it is *after* the spawn flush,
        // because an entity created by this handler is one the damage could
        // legitimately be aimed at.
        self.pending_damage.extend(damage);
        self.flush_damage();

        if reload {
            self.level_restart = self.map.clone();
        }

        Some(result)
    }

    /// Applies whatever [`Context::take_damage`] queued, and whatever the
    /// resulting `OnTakeDamage`s queued in turn.
    ///
    /// Same shape and same guard as [`flush_created`](Server::flush_created):
    /// the re-entry from a nested `dispatch` returns immediately, so a chain
    /// of damage unwinds as a loop at the outermost frame rather than as a
    /// stack.
    fn flush_damage(&mut self) {
        if self.damaging {
            return;
        }
        self.damaging = true;
        // A `trigger_hurt` deals at most one dose per victim per think and
        // nothing in the port deals damage from inside `OnTakeDamage`, so
        // anything past this is a class that hurts whatever hurts it.
        const LIMIT: usize = 4096;
        let mut applied = 0;
        while !self.pending_damage.is_empty() {
            for (target, info) in std::mem::take(&mut self.pending_damage) {
                self.apply_damage(target, &info);
                applied += 1;
            }
            if applied > LIMIT {
                eprintln!(
                    "source-engine: server: LEVEL DESIGN ERROR: more than {LIMIT} points of \
                     damage dealt in one dispatch; dropping the rest"
                );
                self.pending_damage.clear();
                break;
            }
        }
        self.damaging = false;
    }

    /// One queued hit: `OnTakeDamage` on the victim.
    fn apply_damage(&mut self, target: EntityId, info: &DamageInfo) {
        self.dispatch(target, |core, behaviour, cx| {
            // `CBaseEntity::IsMarkedForDeletion` — an entity removed between
            // the queue and the flush is not there to be hurt. Valve gets this
            // for free from the handle going null a frame later; here the
            // entity is still in the list until `CleanupDeleteList`.
            if core.removed {
                return;
            }
            behaviour.on_take_damage(core, info, cx);
        });
    }

    /// Spawns whatever [`Context::create_entity`] made, and whatever *those*
    /// `Spawn`s made in turn.
    ///
    /// Called on the way out of every [`dispatch`](Server::dispatch), and
    /// re-entered by every one of the dispatches it starts — so the guard is
    /// what turns a chain of creations into a loop at the outermost frame
    /// rather than a stack. The bound is the same kind of thing as the event
    /// queue's: Valve has none, and a class that creates itself would recurse
    /// until the stack ran out.
    fn flush_created(&mut self) {
        if self.spawning {
            return;
        }
        self.spawning = true;
        // A generous multiple of the largest thing any shipped map creates:
        // one `trigger_portal_button` per button, and the most any map has is
        // `mp_coop_fling_crushers`' four.
        const LIMIT: usize = 4096;
        let mut spawned = 0;
        while !self.pending_spawn.is_empty() {
            for id in std::mem::take(&mut self.pending_spawn) {
                if self.level_loading {
                    self.created_while_loading.push(id);
                }
                self.dispatch_spawn(id);
                spawned += 1;
            }
            if spawned > LIMIT {
                eprintln!(
                    "source-engine: server: LEVEL DESIGN ERROR: more than {LIMIT} entities \
                     created in one dispatch; dropping the rest"
                );
                self.pending_spawn.clear();
                break;
            }
        }
        self.spawning = false;
    }

    /// `gEntList.CleanupDeleteList` plus the two lists that name entities.
    fn cleanup_delete_list(&mut self) -> usize {
        // `CBaseEntity::UpdateOnRemove`'s `PhysicsRemoveTouchedList( this )`,
        // which has to run **before** the entity is freed so that whatever it
        // was touching gets its `EndTouch` against a handle that still
        // resolves. Only the entities that are actually going, and only when
        // one of them was touching something.
        let going: Vec<EntityId> = self
            .entities
            .iter()
            .filter(|(_, e)| e.core.removed && !e.core.touch_links.is_empty())
            .map(|(id, _)| id)
            .collect();
        for id in going {
            self.remove_touched_list(id);
        }

        let freed = self.entities.cleanup_delete_list();
        if freed > 0 {
            let entities = &self.entities;
            self.thinks.retain_alive(|id| entities.is_alive(id));
            self.queue.retain_targets(|id| entities.is_alive(id));
            self.untouch_list.retain(|&id| entities.is_alive(id));
            // The master tone mapper may have been one of them.
            if self.master_tonemap.is_some_and(|id| !entities.is_alive(id)) {
                self.master_tonemap = None;
            }
            if self.player.is_some_and(|id| !entities.is_alive(id)) {
                self.player = None;
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
    pub fn model_entities(&self) -> Vec<ModelEntityState> {
        self.entities
            .iter()
            .filter(|(_, e)| !e.core.removed)
            .filter_map(|(id, entity)| {
                let state = entity.behaviour.model_state()?;
                let model = entity.core.model.as_deref()?;
                // A `"*N"` brush model is the *other* seam's, and a name that
                // is not a `.mdl` is nothing the studio loader can read.
                if model.starts_with('*') || !model.to_ascii_lowercase().ends_with(".mdl") {
                    return None;
                }
                Some(ModelEntityState {
                    id: id.to_int(),
                    model: model.to_owned(),
                    origin: entity.core.origin,
                    angles: entity.core.angles,
                    skin: state.skin,
                    // `C_BaseEntity::ShouldDraw` refuses **two** things:
                    // `EF_NODRAW`, which is what `StartDisabled` sets, and
                    // `kRenderNone`. `world/`'s brush seam has refused both
                    // since stage 3 — see [`movement::RENDER_NONE`] for why
                    // this seam has to agree even though no shipped prop
                    // writes it. The *translucent* modes are the
                    // `modulation` below, and are honoured since the blended
                    // pass landed: 30 props in the game write one.
                    visible: entity.core.effects & movement::EF_NODRAW == 0
                        && entity.core.render_mode != movement::RENDER_NONE,
                    sequence: state.sequence.to_owned(),
                    cycle: state.cycle,
                    anim_time: state.anim_time,
                    playback_rate: state.playback_rate,
                    modulation: entity.core.modulation(),
                })
            })
            .collect()
    }

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
        let id = self.brush_entity_id(index)?;
        self.entities.get(id).map(|entity| &entity.core)
    }

    /// The same lookup, as a handle. What the touch pass needs, since it has
    /// to dispatch to the entity rather than read it.
    ///
    /// The handle may be dead: [`brush_models`](Server::brush_models) is built
    /// once at `level_init` and a `trigger_once` deletes itself.
    fn brush_entity_id(&self, index: usize) -> Option<EntityId> {
        let at = self
            .brush_models
            .binary_search_by_key(&index, |&(i, _)| i)
            .ok()?;
        Some(self.brush_models[at].1)
    }

    // -----------------------------------------------------------------------
    // the player
    // -----------------------------------------------------------------------

    /// `ClientPutInServer` — put a player in the world.
    ///
    /// Called by the engine when the client spawns, **not** by `level_init`:
    /// Valve's entity list has no player until a client connects either, and
    /// keeping it that way is what lets every test in this module run without
    /// one. Calling it twice replaces the first.
    pub fn spawn_player(&mut self, state: PlayerState) -> EntityId {
        if let Some(old) = self.player.take() {
            self.remove_touched_list(old);
            self.entities.mark_for_deletion(old);
            self.cleanup_delete_list();
        }
        let class = classes::lookup("player").expect("player is registered");
        let id = self.entities.insert(Entity::new(class));
        self.player = Some(id);
        self.dispatch(id, |core, behaviour, cx| {
            behaviour.spawn(core, cx);
        });
        self.set_player_state(state);
        self.player_prev_origin = state.origin;
        id
    }

    /// The player, if the engine has spawned one. `UTIL_PlayerByIndex( 1 )`.
    pub fn player(&self) -> Option<EntityId> {
        self.player
    }

    /// Copies `client/`'s idea of the player **into** the entity list.
    ///
    /// One half of the seam described on [`PlayerState`]; call it once per
    /// rendered frame, before [`frame`](Server::frame).
    ///
    /// **Four fields arrive and are ignored** — `move_type`, `health`,
    /// `life_state` and `flags` — because since stage 5 the server owns them.
    /// They are in the struct so that the client can *read* them; writing them
    /// here would undo every `noclip`, every hit and every death on the next
    /// rendered frame.
    pub fn set_player_state(&mut self, state: PlayerState) {
        let Some(player) = self.player else {
            return;
        };
        let Some(entity) = self.entities.get_mut(player) else {
            return;
        };
        let core = &mut entity.core;
        core.origin = state.origin;
        core.angles = state.angles;
        core.velocity = state.velocity;
        core.base_velocity = state.base_velocity;
        core.model_bounds = ModelBounds {
            mins: state.mins,
            maxs: state.maxs,
        };
        match state.on_ground {
            true => core.flags |= movement::FL_ONGROUND,
            false => core.flags &= !movement::FL_ONGROUND,
        }
        // `CBasePlayer::UpdateButtonState` (`player.cpp:4030`), which the
        // original runs once per usercmd — that is, once per tick — from
        // `CPlayerMove::SetupMove`. It runs once per *rendered frame* here,
        // which is the one place the two clocks show: a tick sees whatever the
        // last frame before it sampled.
        //
        // > **A press and release inside one tick is lost**, and that is
        // > Valve's too rather than this port's: a shipped server sees one
        // > usercmd per tick and computes the same edge from it. What differs
        // > is only *which* sample within the tick, and the answer here is
        // > "the most recent one", where Valve's client would have merged the
        // > frames into the command.
        let player_class = entity.behaviour.downcast_mut::<classes::Player>();
        if let Some(player_class) = player_class {
            player_class.update_button_state(state.buttons);
        }
    }

    /// Copies the entity list's idea of the player back **out**.
    ///
    /// The other half. `None` when no player has been spawned.
    pub fn player_state(&self) -> Option<PlayerState> {
        let entity = self.entities.get(self.player?)?;
        let core = &entity.core;
        Some(PlayerState {
            origin: core.origin,
            angles: core.angles,
            velocity: core.velocity,
            base_velocity: core.base_velocity,
            on_ground: core.has_flags(movement::FL_ONGROUND),
            move_type: core.move_type,
            mins: core.model_bounds.mins,
            maxs: core.model_bounds.maxs,
            health: core.health,
            life_state: core.life_state,
            // `FL_ONGROUND` is masked out on the way past — see the field's
            // docs for why it travels in `on_ground` and nowhere else.
            flags: core.flags & !movement::FL_ONGROUND,
            // Read-only from the server's side: whatever came in last.
            buttons: entity
                .behaviour
                .downcast_ref::<classes::Player>()
                .map_or(0, classes::Player::buttons),
        })
    }

    /// `CON_COMMAND_F( noclip, "Toggle. Player becomes non-solid and flies.",
    /// FCVAR_CHEAT )` — and it is a `game/server/` command again.
    ///
    /// > **This is `portdocs/CLIENT.md` §9.2's wart, closed.** `noclip` lived
    /// > in `src/client/` from stage 1 because move type had nowhere else to
    /// > be; the condition recorded for moving it was "stage 5, where the move
    /// > type becomes the server's state rather than a field on
    /// > `client::Player`". That is this.
    ///
    /// Returns whether the player is now noclipping, or `None` if there is no
    /// player to ask.
    pub fn toggle_noclip(&mut self) -> Option<bool> {
        let entity = self.entities.get_mut(self.player?)?;
        // `CC_Noclip_f` flips between `MOVETYPE_NOCLIP` and `MOVETYPE_WALK`
        // and knows about no third state, so a dead player who noclips comes
        // back as a *walking* corpse. Reproduced: the alternative is to invent
        // a rule Valve does not have, and `kill` followed by `noclip` is a
        // sequence a developer types.
        entity.core.move_type = match entity.core.move_type {
            movement::MoveType::Noclip => movement::MoveType::Walk,
            _ => movement::MoveType::Noclip,
        };
        Some(entity.core.move_type == movement::MoveType::Noclip)
    }

    /// `CC_God_f` (`client.cpp:1334`) — `ToggleFlag( FL_GODMODE )`.
    ///
    /// Returns whether god mode is now on, or `None` with no player.
    pub fn toggle_god(&mut self) -> Option<bool> {
        let entity = self.entities.get_mut(self.player?)?;
        entity.core.flags ^= movement::FL_GODMODE;
        Some(entity.core.has_flags(movement::FL_GODMODE))
    }

    /// `ClientKill` (`client.cpp:56`) → `CBasePlayer::CommitSuicide`.
    ///
    /// Returns whether the player died. `false` for one already dead, or
    /// inside the five-second suicide cooldown.
    pub fn kill_player(&mut self) -> bool {
        let Some(player) = self.player else {
            return false;
        };
        self.dispatch(player, |core, behaviour, cx| {
            match behaviour.downcast_mut::<classes::Player>() {
                Some(class) => class.commit_suicide(core, cx, false),
                None => false,
            }
        })
        .unwrap_or(false)
    }

    /// Hurt the player by hand. **This port's, not Valve's** — the nearest
    /// thing in the original is `hurtme`, which is `#ifdef _DEBUG` only.
    ///
    /// It is here because damage has exactly one source in the shipped maps
    /// (`trigger_hurt`), and a source you have to walk into is not a way to
    /// test the arithmetic.
    pub fn hurt_player(&mut self, amount: f32, damage_type: i32) -> bool {
        let Some(player) = self.player else {
            return false;
        };
        let info = DamageInfo::new(Some(player), Some(player), amount, damage_type);
        self.dispatch(player, |core, behaviour, cx| {
            behaviour.on_take_damage(core, &info, cx)
        })
        .is_some_and(|result| result != Damaged::Refused)
    }

    /// The map the game has asked the engine to start again, taken once.
    ///
    /// `engine->ServerCommand( "reload\n" )`, which in a game with saves
    /// restores the last one and here restarts the level — see
    /// [`Context::reload_level`](class::Context::reload_level). Two things ask
    /// for it: a dead player, three seconds after dying, and
    /// `player_loadsaved`'s `LoadThink`.
    ///
    /// **Read once per rendered frame by `Engine::frame`**, which turns it
    /// into `Host::request_new_game` — so nothing in this module names the
    /// host state machine, the same way nothing in it names `wgpu`.
    pub fn take_level_restart(&mut self) -> Option<String> {
        self.level_restart.take()
    }

    /// How many brush entities this map placed that the port has a class for.
    pub fn brush_entity_count(&self) -> usize {
        self.brush_models.len()
    }

    // -----------------------------------------------------------------------
    // portals
    // -----------------------------------------------------------------------

    /// Every portal the renderer should draw an oval for, as `world/` wants to
    /// see it.
    ///
    /// **Active ones only**, which is `C_Portal_Base2D::ShouldDraw`
    /// (`c_portal_base2d.cpp:544`): *"if ( !IsActive() ... ) return false"*, and
    /// `CPortalRender::AddPortal`/`RemovePortal` are gated on the same thing.
    ///
    /// Filtering here rather than carrying a `visible` flag the way
    /// [`ModelEntityState`] does, and the difference is real: a model entity
    /// that vanishes from this list has *uploaded geometry* the renderer must
    /// not throw away, where a portal's whole geometry is the four numbers
    /// below rebuilt every frame. So there is nothing to keep alive across an
    /// absence.
    pub fn portals(&self) -> Vec<PortalState> {
        self.entities
            .iter()
            .filter(|(_, e)| !e.core.removed)
            .filter_map(|(id, entity)| {
                let portal = entity.behaviour.downcast_ref::<classes::PropPortal>()?;
                portal.activated.then(|| PortalState {
                    id: id.to_int(),
                    origin: entity.core.origin,
                    angles: entity.core.angles,
                    half_width: portal.half_width,
                    half_height: portal.half_height,
                    is_portal2: portal.is_portal2,
                    opened_at: portal.opened_at,
                    linked: portal
                        .is_active_and_linked()
                        .then(|| portal.linked.map(|id| id.to_int()))
                        .flatten(),
                    matrix: portal.matrix,
                })
            })
            .collect()
    }

    /// Every `func_areaportal`'s key and whether it is open —
    /// `CAreaPortal::UpdateState`'s `engine->SetAreaPortalState` calls,
    /// gathered instead of pushed.
    ///
    /// Pushed in the shipped game because the call crosses the game/engine DLL
    /// boundary and the engine has no way to ask; gathered here because there
    /// is no boundary and a pull cannot go stale. Two entities naming the same
    /// `portalnumber` is legal and the last one wins, which is what a sequence
    /// of pushes would also do.
    ///
    /// **Removed entities are skipped, not reported closed.** An areaportal
    /// with no entity is open (see
    /// [`AreaPortal`](classes::AreaPortal)), so a key that drops out of this
    /// list keeps whatever state it last had rather than slamming shut.
    pub fn area_portals(&self) -> Vec<(u16, bool)> {
        self.entities
            .iter()
            .filter(|(_, e)| !e.core.removed)
            .filter_map(|(_, entity)| {
                Some(
                    entity
                        .behaviour
                        .downcast_ref::<classes::AreaPortal>()?
                        .state(),
                )
            })
            .collect()
    }

    /// `CProp_Portal::FindPortal( group, bPortal2, bCreateIfNothingFound )`
    /// (`prop_portal.cpp:892`) — the portal of that colour in that group,
    /// making one if the group has none.
    ///
    /// **An active portal of the right colour wins over an inactive one**, and
    /// the loop keeps looking after it finds an inactive match rather than
    /// returning it — which is what lets the gun re-place the portal you can
    /// see rather than a spare.
    ///
    /// The one caller is the [`place_portal`](Server::place_portal) console
    /// command; in the shipped game it is the portal gun.
    fn find_portal(&mut self, group: u8, is_portal2: bool, create: bool) -> Option<EntityId> {
        let mut inactive = None;
        for (id, entity) in self.entities.iter() {
            let Some(portal) = entity.behaviour.downcast_ref::<classes::PropPortal>() else {
                continue;
            };
            if portal.linkage_group != group || portal.is_portal2 != is_portal2 {
                continue;
            }
            match portal.activated {
                true => return Some(id),
                false => inactive = Some(id),
            }
        }
        if inactive.is_some() || !create {
            return inactive;
        }

        // `CreateEntityByName` + `DispatchSpawn`, with the two fields set in
        // between — the same order [`class::Context::create_entity`] documents,
        // and reachable *directly* here because nothing is being dispatched:
        // the console runs between ticks.
        let class = classes::lookup("prop_portal")?;
        let mut entity = Entity::new(class);
        if let Some(portal) = entity.behaviour.downcast_mut::<classes::PropPortal>() {
            portal.linkage_group = group;
            portal.is_portal2 = is_portal2;
        }
        let id = self.entities.insert(entity);
        self.dispatch(id, |core, behaviour, cx| {
            behaviour.spawn(core, cx);
        });
        Some(id)
    }

    /// Put a portal somewhere. **This port's console command, and the gun's
    /// path through the shipped game minus its rules.**
    ///
    /// `CWeaponPortalgun::FirePortal` ends in `FindPortal( group, bPortal2,
    /// true )` followed by `PlacePortal`; this is that pair with
    /// `CProp_Portal::NewLocation` in place of `PlacePortal`, which is the
    /// branch the *map* uses — `InputNewLocation`'s own comment calls it
    /// "skipping placement rules" (`prop_portal.cpp:799`). What is skipped is
    /// `VerifyPortalPlacementAndFizzleBlockingPortals`: whether the surface is
    /// portalable, whether the oval fits on it, whether a bumper or a
    /// no-portal volume forbids it, and whether it overlaps the other portal.
    /// All of that is `portal_placement.cpp`, which needs the gun
    /// (`portdocs/PORTAL.md` §8).
    ///
    /// `NewLocation` activates the portal, so placing both colours links them.
    /// Returns whether a portal was placed, which is `false` only with no map
    /// loaded.
    pub fn place_portal(&mut self, is_portal2: bool, origin: Vec3, angles: Vec3) -> bool {
        let Some(id) = self.find_portal(0, is_portal2, true) else {
            return false;
        };
        self.dispatch(id, |core, behaviour, cx| {
            if let Some(portal) = behaviour.downcast_mut::<classes::PropPortal>() {
                portal.new_location(core, origin, angles, cx);
            }
        });
        true
    }

    /// Switch every portal in the map off — `Fizzle` at each, which is what
    /// walking through a fizzler does to both ends of a pair.
    ///
    /// Returns how many were on. Also this port's console command.
    pub fn fizzle_portals(&mut self) -> usize {
        let portals: Vec<EntityId> = self
            .entities
            .iter()
            .filter(|(_, e)| e.behaviour.downcast_ref::<classes::PropPortal>().is_some())
            .map(|(id, _)| id)
            .collect();
        let mut fizzled = 0;
        for id in portals {
            let was_active = self
                .entities
                .get(id)
                .and_then(|e| e.behaviour.downcast_ref::<classes::PropPortal>())
                .is_some_and(|portal| portal.activated);
            if !was_active {
                continue;
            }
            fizzled += 1;
            self.dispatch(id, |core, behaviour, cx| {
                let input = Input {
                    name: "Fizzle",
                    value: Variant::Void,
                    activator: None,
                    caller: None,
                    output_id: 0,
                };
                behaviour.accept_input(core, &input, cx);
            });
        }
        fizzled
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
        let triggers = self
            .entities
            .iter()
            .filter(|(_, e)| e.core.is_solid_flag_set(movement::FSOLID_TRIGGER))
            .count();
        cx.print(&format!(
            "{} brush entities have a class; their placements are the server's, \
             and {triggers} of them are live triggers",
            self.brush_entity_count()
        ));
        match self.player().and_then(|id| self.entities.get(id)) {
            Some(player) => cx.print(&format!(
                "the player is entity #{} at ({:.0} {:.0} {:.0}), touching {}",
                player.id().slot(),
                player.origin.x,
                player.origin.y,
                player.origin.z,
                player.touch_links.len()
            )),
            None => cx.print("there is no player"),
        }

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
            // The damage block. Printed only when something can take damage,
            // because 60,000 of the game's 60,925 entities are
            // `DAMAGE_NO`/0/0 and a line saying so on every one of them is
            // noise.
            if entity.take_damage.takes_damage() || entity.health != 0 {
                cx.print(&format!(
                    "  health: {}/{} ({:?}, {:?})",
                    entity.health, entity.max_health, entity.take_damage, entity.life_state
                ));
            }
            if let Some(filter) = &entity.damage_filter_name {
                cx.print(&format!(
                    "  damagefilter: {filter} ({})",
                    match entity.damage_filter {
                        Some(_) => "resolved",
                        None => "NOT FOUND",
                    }
                ));
            }
            if entity.flags != 0 {
                cx.print(&format!("  flags: {:#x}", entity.flags));
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
    /// Valve's version passes the issuing player as both activator and caller.
    /// This one passes the player as **activator** since stage 5 — so
    /// `ent_fire <trigger> StartTouch` and anything resolving `!activator`
    /// reach the player — and leaves the caller null, because the console is
    /// not an entity and `!caller` has nothing to be.
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
            activator: self.player,
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
