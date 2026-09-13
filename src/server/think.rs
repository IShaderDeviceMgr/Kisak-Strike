//! The server's clock, and the list of entities that want to hear from it.
//!
//! `TICK_INTERVAL`/`TIME_TO_TICKS` (`game/shared/shareddefs.h:16-22`),
//! `CBaseEntity::SetNextThink` (`game/shared/baseentity_shared.cpp:952`),
//! `CBaseEntity::PhysicsRunSpecificThink`
//! (`game/shared/physics_main_shared.cpp:2080`) and `CSimThinkManager`
//! (`game/server/entitylist.cpp:150`).
//!
//! # The server has a fixed tick and the rest of the engine does not
//!
//! `portdocs/SERVER.md` §5 called this "the one architectural decision" and
//! asked for it to be taken before stage 2 rather than after. It is taken here,
//! the way §5 recommends: **the server runs on a fixed tick accumulated inside
//! the rendered frame**, and the client keeps running on the rendered frame at
//! a variable `dt`. That is Valve's own split — its client predicts on the
//! render frame against a server that does not — and it is forced rather than
//! chosen, because `SetNextThink` *quantises to ticks*
//! ([`Time::time_to_ticks`]) and a think schedule built on a variable `dt` is
//! a different schedule at every frame rate.
//!
//! The consequence to know: **`curtime` on the server is not `Scene::curtime`**.
//! The server's is `tick * interval` and moves in steps; the scene's is the
//! accumulated wall clock and moves smoothly. An entity that wants "now" wants
//! [`Time::curtime`].
//!
//! # The rate is one constant and it is not verified
//!
//! [`DEFAULT_TICK_INTERVAL`] is 1/64 because that is what
//! `DEFAULT_TICK_INTERVAL_PC` is in this tree — **and this tree is
//! `cstrike15`, so 1/64 is CS:GO's number, not Portal 2's**. Portal 2's shipped
//! `interval_per_tick` is not in any shipped `.cfg`, is not recoverable from
//! the map files, and the depot ships no engine binary to read it out of (only
//! `vbsp`/`vvis`/`vrad`), so it stays unverified. It is one constant with one
//! definition site, overridable with `-tickrate`, exactly as §5 asked — when a
//! measurement against the shipped game becomes possible, this is the line to
//! change.

use super::entity::EntityId;

/// `TICK_NEVER_THINK` (`shareddefs.h:22`) — "no think scheduled".
///
/// It is `-1` **as both a tick number and a time**: `SetNextThink` compares
/// the incoming `float` against it before converting, so
/// `set_next_think(NEVER_THINK)` is how a class cancels its own think.
pub const TICK_NEVER_THINK: i32 = -1;

/// `DEFAULT_TICK_INTERVAL_PC` (`public/const.h:29`). See the module docs for
/// why this number is suspect and why it is still the default.
pub const DEFAULT_TICK_INTERVAL: f32 = 1.0 / 64.0;

/// `MINIMUM_TICK_INTERVAL` (`public/const.h:39`) — 128 ticks a second.
const MINIMUM_TICK_INTERVAL: f32 = 4.0 / 512.0;
/// `MAXIMUM_TICK_INTERVAL` (`public/const.h:40`) — 20.48 ticks a second.
const MAXIMUM_TICK_INTERVAL: f32 = 25.0 / 512.0;

/// The most ticks one rendered frame may run.
///
/// Not Valve's: `_Host_RunFrame` runs however many the accumulated time asks
/// for, and is protected from a spiral only by `FilterTime` clamping
/// `host_frametime` to `MAX_FRAMETIME` first. This port clamps in the same
/// place ([`crate::engine::host`]'s `FrameClock`, 0.1 s), so at the default
/// interval the accumulator can never ask for more than seven — this is a
/// belt on top of that brace, and exists so that a future change to the clamp
/// cannot turn a long frame into an unbounded loop.
const MAX_TICKS_PER_FRAME: u32 = 16;

/// Where the server's clock is now. `gpGlobals`' time fields, minus the ones
/// that belong to the render frame.
///
/// Copied rather than borrowed: it is three numbers, and a [`Context`]
/// (super::class::Context) that borrowed the clock could not also hand out
/// `&mut` to the queue beside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Time {
    /// `gpGlobals->curtime`, **derived from the tick** — `tick * interval`.
    pub curtime: f32,
    /// `gpGlobals->tickcount`.
    pub tick: i32,
    /// `gpGlobals->interval_per_tick`.
    pub interval: f32,
}

impl Time {
    /// `TIME_TO_TICKS( dt )` (`shareddefs.h:19`) —
    /// `(int)( 0.5f + (float)(dt) / TICK_INTERVAL )`.
    ///
    /// > **This rounds to nearest, and that is the sharpest edge in the whole
    /// > module.** `SetNextThink( curtime + 0.01 )` is one tick at 64 Hz and
    /// > *zero* ticks at 30 Hz — and a think tick that is not greater than
    /// > `tickcount` never runs, so the same line is "next tick" on one
    /// > machine and "never" on another. `logic_auto`'s 0.2-second bootstrap
    /// > and `logic_relay`'s 0.01-second `OnSpawn` both live on this edge.
    pub fn time_to_ticks(&self, time: f32) -> i32 {
        (0.5 + time / self.interval) as i32
    }

    /// `TICKS_TO_TIME( t )`.
    pub fn ticks_to_time(&self, tick: i32) -> f32 {
        self.interval * tick as f32
    }
}

/// The fixed-tick accumulator. Valve's is spread across `_Host_RunFrame`'s
/// `host_remainder`/`numticks` arithmetic and `gpGlobals`.
pub struct ServerClock {
    tick: i32,
    interval: f32,
    /// Real time banked but not yet spent on a tick. `host_remainder`.
    accumulated: f32,
}

impl ServerClock {
    /// A clock at tick 0. `interval` is clamped the way
    /// `CServerGameDLL::GetTickInterval` clamps `-tickrate`.
    pub fn new(interval: f32) -> ServerClock {
        ServerClock {
            tick: 0,
            interval: interval.clamp(MINIMUM_TICK_INTERVAL, MAXIMUM_TICK_INTERVAL),
            accumulated: 0.0,
        }
    }

    /// `CServerGameDLL::GetTickInterval` (`gameinterface.cpp:1015`) — the
    /// interval `-tickrate <hz>` asks for.
    ///
    /// Valve quantises the requested interval to the nearest `N / 512` and
    /// then clamps, so the reachable rates are 20.48 Hz to 128 Hz and are not
    /// a continuum. Reproduced because a server that ran at exactly the
    /// requested rate would disagree with the shipped game about every think
    /// tick.
    ///
    /// The dedicated-console branches (`IsDedicatedServerForXbox`, `…ForPS3`)
    /// are gone with the platforms, per `PORTING.md`.
    pub fn interval_from_tickrate(tickrate: Option<f32>) -> f32 {
        let Some(tickrate) = tickrate else {
            return DEFAULT_TICK_INTERVAL;
        };
        let interval = match tickrate > 0.0 {
            true => (1.0 / tickrate * 512.0 + 0.5).floor() / 512.0,
            // Valve leaves the default in place for a non-positive rate and
            // then clamps it anyway, which is a no-op for the default.
            false => DEFAULT_TICK_INTERVAL,
        };
        interval.clamp(MINIMUM_TICK_INTERVAL, MAXIMUM_TICK_INTERVAL)
    }

    /// Where the clock is now.
    pub fn time(&self) -> Time {
        Time {
            curtime: self.interval * self.tick as f32,
            tick: self.tick,
            interval: self.interval,
        }
    }

    /// Banks `frame_time` seconds and reports how many ticks that bought.
    ///
    /// The caller runs that many ticks, calling [`advance`](ServerClock::advance)
    /// before each — two calls rather than one because a tick's work needs
    /// `&mut` to everything the clock is a field of.
    pub fn accumulate(&mut self, frame_time: f32) -> u32 {
        self.accumulated += frame_time.max(0.0);
        let ticks = (self.accumulated / self.interval) as u32;
        let ticks = ticks.min(MAX_TICKS_PER_FRAME);
        self.accumulated -= ticks as f32 * self.interval;
        // A clamped frame drops the surplus rather than banking it, so that a
        // stall cannot leave the server permanently behind.
        if self.accumulated > self.interval {
            self.accumulated = 0.0;
        }
        ticks
    }

    /// Moves to the next tick. `gpGlobals->tickcount++`.
    pub fn advance(&mut self) {
        self.tick += 1;
    }

    /// Back to tick 0 with nothing banked. `LevelInit`'s half of the clock.
    pub fn reset(&mut self) {
        self.tick = 0;
        self.accumulated = 0.0;
    }
}

/// One entity's place in the think list. `simthinkentry_t`.
struct ThinkEntry {
    id: EntityId,
    next_think_tick: i32,
}

/// The entities that have a think scheduled. `CSimThinkManager`.
///
/// # Why this is not "iterate the entity list"
///
/// `portdocs/SERVER.md` §4.5: the shipped maps place 60,925 entities and 38,257
/// of them are inert bookkeeping, so `Physics_RunThinkFunctions` does not touch
/// the entity list at all — it copies a list of entities that have registered
/// interest. "Build it from the start", because the difference between walking
/// this list and walking the entity list is the difference between a server
/// frame and a server stall.
///
/// # Why it is a flat `Vec` and not an index or a heap
///
/// Valve's is a `CUtlVector` plus a 16,384-entry `unsigned short` index array
/// for `FastRemove`. The index array exists because its list can hold every
/// entity in the level (anything with *physics* is in it too, not just
/// thinkers). Stage 2 has no movetypes, so only actual thinkers are in it, and
/// the depot test measures how many that is: **at most 43 entities at once
/// across all 106 shipped maps**, which is short enough that a linear scan
/// beats a hash lookup
/// and far short of anything that would justify a binary heap with lazy
/// deletion. `ENGINE_WORLD_DISP.md`'s rule — measure before optimising —
/// applies; when stage 3 puts every mover in here the measurement should be
/// retaken.
#[derive(Default)]
pub struct ThinkList {
    entries: Vec<ThinkEntry>,
}

impl ThinkList {
    pub fn new() -> ThinkList {
        ThinkList::default()
    }

    /// `SimThink_EntityChanged` (`entitylist.cpp:302`): reconcile one entity's
    /// membership against its current schedule.
    ///
    /// Called after every dispatch that could have changed a schedule — spawn,
    /// activate, think, input — which is every place a [`Context`](super::class::Context)
    /// exists. An entity marked for deletion is removed and **not re-added**,
    /// which is Valve's first line and is what stops a think firing on a
    /// corpse.
    pub fn entity_changed(&mut self, id: EntityId, next_think_tick: i32, removed: bool) {
        let existing = self.entries.iter().position(|entry| entry.id == id);
        let wants_in = !removed && next_think_tick != TICK_NEVER_THINK;

        match (existing, wants_in) {
            (Some(at), true) => self.entries[at].next_think_tick = next_think_tick,
            // `FastRemove`: order in this list is not meaningful, because
            // `ListCopy` filters it and the filtered order is entity order.
            (Some(at), false) => {
                self.entries.swap_remove(at);
            }
            (None, true) => self.entries.push(ThinkEntry {
                id,
                next_think_tick,
            }),
            (None, false) => {}
        }
    }

    /// `SimThink_ListCopy` (`entitylist.cpp:214`): everything due at `tick`.
    ///
    /// Valve copies into a stack buffer so that the list may change while the
    /// thinks run; a `Vec` handed back to the caller is the same thing, and
    /// the copy is what makes it safe for a think to schedule, cancel or
    /// delete anything — including itself.
    ///
    /// **Returned in entity-list order**, not in list order: a `logic_auto`
    /// and a `logic_relay` due on the same tick must run in the order the map
    /// placed them, and `swap_remove` above has already destroyed the
    /// insertion order.
    pub fn due(&self, tick: i32, out: &mut Vec<EntityId>) {
        out.clear();
        out.extend(
            self.entries
                .iter()
                .filter(|entry| entry.next_think_tick > 0 && entry.next_think_tick <= tick)
                .map(|entry| entry.id),
        );
        out.sort_unstable_by_key(|id| id.slot());
    }

    /// Drops entries for entities that no longer exist. Called after
    /// `CleanupDeleteList`.
    pub fn retain_alive(&mut self, alive: impl Fn(EntityId) -> bool) {
        self.entries.retain(|entry| alive(entry.id));
    }

    /// How many entities have a think scheduled.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::classes;
    use crate::server::entity::{Entity, EntityList};

    fn time(interval: f32, tick: i32) -> Time {
        Time {
            curtime: interval * tick as f32,
            tick,
            interval,
        }
    }

    /// The rounding rule, and the 30 Hz cliff it produces.
    #[test]
    fn time_to_ticks_rounds_to_nearest() {
        let t = time(1.0 / 64.0, 0);
        assert_eq!(t.time_to_ticks(0.0), 0);
        assert_eq!(t.time_to_ticks(1.0 / 64.0), 1);
        // 0.01 s is 0.64 of a tick at 64 Hz, which rounds up to one…
        assert_eq!(t.time_to_ticks(0.01), 1);
        assert_eq!(t.time_to_ticks(0.2), 13);

        // …and 0.3 of a tick at 30 Hz, which rounds down to none. Same line of
        // map logic, two different behaviours.
        let t = time(1.0 / 30.0, 0);
        assert_eq!(t.time_to_ticks(0.01), 0);
    }

    #[test]
    fn the_tickrate_switch_quantises_to_512ths_and_clamps() {
        let interval = ServerClock::interval_from_tickrate;
        assert_eq!(interval(None), DEFAULT_TICK_INTERVAL);
        // 64 Hz is exactly 8/512.
        assert_eq!(interval(Some(64.0)), 8.0 / 512.0);
        // 100 Hz asks for 0.01, which quantises to 5/512 ≈ 0.009766.
        assert_eq!(interval(Some(100.0)), 5.0 / 512.0);
        // Out of range at both ends.
        assert_eq!(interval(Some(1000.0)), MINIMUM_TICK_INTERVAL);
        assert_eq!(interval(Some(1.0)), MAXIMUM_TICK_INTERVAL);
        assert_eq!(interval(Some(-5.0)), DEFAULT_TICK_INTERVAL);
    }

    #[test]
    fn the_clock_banks_time_and_spends_it_a_tick_at_a_time() {
        let mut clock = ServerClock::new(1.0 / 64.0);
        assert_eq!(clock.time().tick, 0);

        // Less than a tick: nothing runs, the time is kept.
        assert_eq!(clock.accumulate(0.01), 0);
        assert_eq!(clock.accumulate(0.01), 1, "0.02 s is one 64 Hz tick");
        clock.advance();
        assert_eq!(clock.time().tick, 1);
        assert!((clock.time().curtime - 1.0 / 64.0).abs() < 1e-6);

        // A long frame buys several.
        assert_eq!(clock.accumulate(0.05), 3);
    }

    /// The catch-up cap. The host clamps the frame time long before this, so
    /// reaching it means something upstream changed.
    #[test]
    fn a_huge_frame_cannot_ask_for_unbounded_ticks() {
        let mut clock = ServerClock::new(1.0 / 64.0);
        assert_eq!(clock.accumulate(100.0), MAX_TICKS_PER_FRAME);
        // …and the surplus is dropped rather than banked.
        assert_eq!(clock.accumulate(0.0), 0);
    }

    fn entity(list: &mut EntityList) -> EntityId {
        let class = classes::lookup("info_target").expect("registered");
        list.insert(Entity::new(class))
    }

    #[test]
    fn the_think_list_holds_only_entities_with_a_schedule() {
        let mut list = EntityList::new();
        let (a, b) = (entity(&mut list), entity(&mut list));

        let mut thinks = ThinkList::new();
        thinks.entity_changed(a, TICK_NEVER_THINK, false);
        assert_eq!(thinks.len(), 0, "no schedule, not in the list");

        thinks.entity_changed(a, 5, false);
        thinks.entity_changed(b, 9, false);
        assert_eq!(thinks.len(), 2);

        let mut due = Vec::new();
        thinks.due(4, &mut due);
        assert!(due.is_empty());
        thinks.due(5, &mut due);
        assert_eq!(due, vec![a], "due at exactly its tick");
        thinks.due(100, &mut due);
        assert_eq!(due, vec![a, b]);

        // Cancelling takes it back out.
        thinks.entity_changed(a, TICK_NEVER_THINK, false);
        assert_eq!(thinks.len(), 1);
        thinks.due(100, &mut due);
        assert_eq!(due, vec![b]);
    }

    /// `EntityChanged`'s first line: a marked entity is dropped and never
    /// re-added, whatever its schedule says.
    #[test]
    fn a_removed_entity_leaves_the_think_list() {
        let mut list = EntityList::new();
        let a = entity(&mut list);
        let mut thinks = ThinkList::new();

        thinks.entity_changed(a, 3, false);
        assert_eq!(thinks.len(), 1);
        thinks.entity_changed(a, 3, true);
        assert_eq!(thinks.len(), 0);
        thinks.entity_changed(a, 3, true);
        assert_eq!(thinks.len(), 0, "and it does not come back");
    }

    /// A think at tick 0 or a negative one never comes due — which is what
    /// makes `SetNextThink(0)` mean "not scheduled" in Valve's code.
    #[test]
    fn a_think_tick_of_zero_never_runs() {
        let mut list = EntityList::new();
        let a = entity(&mut list);
        let mut thinks = ThinkList::new();
        thinks.entity_changed(a, 0, false);

        let mut due = Vec::new();
        thinks.due(0, &mut due);
        assert!(due.is_empty());
        thinks.due(1000, &mut due);
        assert!(due.is_empty());
    }
}
