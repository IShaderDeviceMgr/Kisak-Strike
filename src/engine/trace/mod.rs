//! Ray and swept-box traces against the world.
//!
//! Replaces `engine/cmodel.cpp`'s trace (`CM_BoxTrace` and everything under
//! it), `engine/cmodel_disp.cpp`, `public/dispcoll_common.cpp`, and later
//! `engine/enginetrace.cpp`'s dispatch over entities. This is stages 1-3 of
//! `portdocs/ENGINE_TRACE.md`: the world's brushes ([`Tracer::trace`]), the
//! **brush models** built out of them — doors, platforms, pistons
//! ([`Tracer::trace_model`]) — and the **displacements**, the map's terrain.
//! No entities, no static props.
//!
//! [`carve`] is the exception to all of that, and it is `portdocs/PORTAL.md`
//! stage 3 rather than `ENGINE_TRACE.md`: the collision near a portal, with a
//! rectangular hole cut out of the wall behind it, as a collision model of its
//! own. [`Tracer::with_hole`] is how it reaches a trace, and it is the one
//! thing in this module that *changes* what [`Tracer::trace`] answers rather
//! than adding to it.
//!
//! Terrain needs no call of its own: it is part of the world, [`Tracer::trace`]
//! finds it the way it finds a brush, and the only visible difference is that
//! [`Trace::disp_flags`] comes back non-zero. [`disp`] is where it lives, and
//! its module doc is the one to read before believing anything about it — every
//! test there is **one-sided**, so terrain is solid from the front and
//! transparent from behind.
//!
//! The world and the brush models are deliberately separate calls and nothing
//! yet combines them: a caller that wants "what is in the way" asks the world,
//! then asks each brush model, and keeps the nearest. Doing that *for* the
//! caller is `ClipRayToCollideable`'s job and needs a filter and a broadphase,
//! which need entities (stage 4).
//!
//! ```ignore
//! let collision = CollisionBsp::build(&bsp);
//! let mut tracer = collision.tracer();
//!
//! let ray = Ray::hull(feet, feet + motion, PLAYER_MINS, PLAYER_MAXS);
//! let hit = tracer.trace(&ray, Contents::MASK_PLAYERSOLID);
//! if hit.did_hit() {
//!     let stopped_at = hit.end;               // already in the caller's frame
//!     let floor = hit.normal.z > 0.7;         // `CategorizePosition`'s test
//! }
//! ```
//!
//! These produce a plausible wrong answer rather than an error, and all but
//! the last are Valve's rather than this port's:
//!
//! 1. **[`Ray`]'s start is the centre of the box; [`Trace`]'s is not.** A
//!    player hull is 72 units tall, so the two differ by 36 — see [`Ray`].
//! 2. **[`Trace::fraction`] stops `DIST_EPSILON` (1/32 unit) short** of the
//!    surface, deliberately, and movement code depends on the gap — **except
//!    for a ray against a displacement**, which stops exactly on it.
//! 3. **[`Trace::fraction_left_solid`] is meaningful for rays only.** A hull
//!    sweep gets zero, matching `CEngineTrace::TraceRay`.
//! 4. **A brush model's [`Trace::plane_dist`] stays in the model's frame**,
//!    where its `normal` comes back rotated into the caller's — see
//!    [`Tracer::trace_model`].
//! 5. **A *point* inside terrain is reported as not solid**, because the test
//!    that decides "inside" is a box-versus-triangle overlap and a point has no
//!    box — see [`test_in_disp_tree`].
//! 6. **[`Trace::disp_flags`] is cleared when a brush beats a displacement,
//!    and in Valve it is not.** The module's one deliberate divergence; the
//!    two lines are in [`brush`] and are commented as such.

mod brush;
mod carve;
mod disp;
#[cfg(test)]
pub(crate) mod fixture;
mod hull;
mod model;
mod ray;
mod result;

pub use carve::{CarvedWall, LivePortal, PortalHole, PortalHoles, PortalLink, CARVE};
pub use disp::disp_surf;
pub use model::{BrushModel, CollisionBsp};
pub use ray::{Contents, Ray};
pub use result::{Surface, Trace};

use disp::STAB_LENGTH;
use glam::Vec3;

/// `DIST_EPSILON` (`public/coordsize.h:35`) — 1/32 of a unit.
///
/// Not a tolerance to be tuned. A trace stops this far short of what it hits,
/// and the split in [`hull`] overlaps its two halves by the same amount so a
/// brush on a node plane is reached from both sides. Stair stepping, ground
/// probes and the clip-and-retry in `TryPlayerMove` are all written around it.
const DIST_EPSILON: f32 = 0.03125;

/// `NEVER_UPDATED` (`engine/cmodel_private.h:157`) — an enter fraction that no
/// real one can be below.
const NEVER_UPDATED: f32 = -99999.0;

/// `MAX_CHECK_COUNT_DEPTH` (`engine/cmodel_private.h:26`) — how deeply a trace
/// can nest inside itself.
///
/// Two, and the second level exists for exactly one caller: the displacement
/// **stab** (`CM_Stab`, `engine/cmodel_disp.cpp:276`) fires a fresh trace from
/// inside a position test and must not inherit the outer one's visit marks.
const MAX_VISIT_DEPTH: usize = 2;

/// One nesting level's visit stamps — `TraceInfo_t`'s `m_Count[i]`,
/// `m_BrushCounters[i]` and `m_DispCounters[i]`.
#[derive(Debug)]
struct VisitLevel {
    count: u32,
    brushes: Vec<u32>,
    disps: Vec<u32>,
}

/// The visit stamps for a trace and anything nested inside it.
///
/// `PushTraceVisits`/`PopTraceVisits` (`engine/cmodel.cpp:90`, `:105`)
/// expressed as a stack rather than a global depth counter. Valve gives each
/// level its *own* arrays so that a nested trace cannot un-mark what the outer
/// one has already visited — sharing one array and bumping the stamp would
/// look equivalent and is not: a brush the outer trace had marked would be
/// re-visitable once the stab restored the outer stamp.
#[derive(Debug)]
struct Visits {
    levels: [VisitLevel; MAX_VISIT_DEPTH],
    depth: usize,
}

impl Visits {
    fn new(brushes: usize, disps: usize) -> Visits {
        Visits {
            levels: std::array::from_fn(|_| VisitLevel {
                count: 0,
                brushes: vec![0; brushes],
                disps: vec![0; disps],
            }),
            depth: 0,
        }
    }

    /// Begins a new trace at this level — `PushTraceVisits`' counter bump,
    /// including the wrap, where a stamp of 0 would compare equal to a stale
    /// one and the whole array has to be cleared.
    fn begin(&mut self) {
        let level = &mut self.levels[self.depth];
        level.count = level.count.wrapping_add(1);
        if level.count == 0 {
            level.count = 1;
            level.brushes.fill(0);
            level.disps.fill(0);
        }
    }

    /// Enters a nested trace. The caller must [`pop`](Visits::pop).
    fn push(&mut self) {
        self.depth += 1;
        assert!(self.depth < MAX_VISIT_DEPTH, "traces nested too deeply");
        self.begin();
    }

    fn pop(&mut self) {
        self.depth -= 1;
    }

    fn brush(&mut self, index: usize) -> bool {
        let level = &mut self.levels[self.depth];
        std::mem::replace(&mut level.brushes[index], level.count) != level.count
    }

    fn disp(&mut self, index: usize) -> bool {
        let level = &mut self.levels[self.depth];
        std::mem::replace(&mut level.disps[index], level.count) != level.count
    }
}

/// A trace in progress: the query, the scratch, and the answer so far.
///
/// `TraceInfo_t` (`engine/cmodel_private.h:37`) minus the pooling. Valve kept
/// one of these per thread in `g_TraceInfoPool`, handed out by `BeginTrace()`
/// and returned by `EndTrace()`, because the recursion needed somewhere to put
/// state that was not a parameter. Here it is a stack local threaded through
/// as `&mut`, which is the same thing with the lifetime checked.
struct Work<'a> {
    bsp: &'a CollisionBsp,
    /// The centre of the box at the start of the sweep.
    start: Vec3,
    end: Vec3,
    /// Half the box's diagonal; zero for a ray.
    extents: Vec3,
    delta: Vec3,
    inv_delta: Vec3,
    /// `m_ispoint` — the extents are (near enough) zero.
    is_point: bool,
    /// `m_isswept` — the sweep goes somewhere.
    is_swept: bool,
    /// The mask this trace is testing against.
    contents: Contents,
    trace: Trace,
    /// `m_bDispHit` — whether the hit the trace is *currently* holding came
    /// from a displacement.
    ///
    /// Cleared by every brush hit that wins (`engine/cmodel.cpp:1065`,
    /// `:1704`), which is what makes it mean "the best hit so far is terrain"
    /// rather than "terrain was touched at some point". Read by
    /// [`post_trace_to_disp_tree`].
    disp_hit: bool,
    visits: &'a mut Visits,
}

impl Work<'_> {
    /// Whether this brush has not been clipped against yet in this trace.
    ///
    /// `TraceInfo_t::Visit` (`engine/cmodel_private.h:78`). A brush belongs to
    /// every leaf it touches, so without this a wall spanning eight leaves is
    /// clipped eight times — wasted work, and wrong for
    /// [`Trace::fraction_left_solid`], which accumulates.
    fn visit(&mut self, brush: usize) -> bool {
        self.visits.brush(brush)
    }

    /// The same, for a displacement — which likewise belongs to every leaf its
    /// bounding box touches.
    fn visit_disp(&mut self, disp: usize) -> bool {
        self.visits.disp(disp)
    }
}

/// What one swept box met among the **physics props** — the answer
/// [`Tracer::with_props`] merges into a trace.
///
/// Three fields, because three is what `CPhysicsCollision::TraceBox`
/// (`vphysics/physics_collide.cpp`) fills in: the shipped engine's
/// `ClipRayToVPhysics` (`engine/enginetrace.cpp:1115`) hands it a bare
/// `trace_t` and everything else in the result is written by the engine
/// afterwards from the *entity*, not from the collision.
///
/// **No entity, deliberately.** [`Trace`] has no `m_pEnt` either and for the
/// same reason (`portdocs/ENGINE_TRACE.md` §4.2) — a caller that needs to know
/// *which* prop it hit is asking a physics question and should ask the physics
/// module, which is the only place the answer is a stable identity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropHit {
    /// How far along the sweep, in `[0, 1]`. **Before** `DIST_EPSILON` is
    /// taken off — the merge does that, so that an implementor does not have
    /// to know this module's slack convention.
    pub fraction: f32,
    /// The normal of the surface hit, pointing out of it.
    pub normal: Vec3,
    /// The box was already inside this prop when the sweep began.
    pub start_solid: bool,
}

/// The physics props, as something a [`Tracer`] can sweep against.
///
/// # Why this is a trait and not a slice
///
/// `with_entities` takes a slice because a brush model is geometry this module
/// already understands. A physics prop is not: its hulls live in
/// `src/vphysics/`, they are Rapier shapes, and they are indexed by a broad
/// phase that module owns. Copying them here would mean two collision
/// representations of the same cube that are free to disagree.
///
/// So the sweep is asked for, and the dependency points the way every other
/// seam in this port does: **`engine/` implements it over the server's
/// physics**, `server/` never names `engine::trace`, and `trace/` never names
/// `vphysics`. It is `super::super::TouchQuery`'s shape with the two ends
/// swapped.
pub trait PropQuery {
    /// Sweep an axis-aligned box of half-extents `half`, centred on `start`,
    /// to `end`.
    ///
    /// `half`/`start` are the *centred* form — `Ray`'s, not the caller's — so
    /// an implementor never has to know where an entity's origin sits inside
    /// its hull.
    fn sweep(&self, half: Vec3, start: Vec3, end: Vec3) -> Option<PropHit>;
}

/// Traces against one collision model.
///
/// Holds the per-trace scratch, so **make one and keep it**: a fresh `Tracer`
/// allocates a stamp per brush and per displacement. This is Valve's
/// `BeginTrace`/`EndTrace` pair (`engine/cmodel.cpp:66`, `:111`) expressed as a
/// borrow — including the re-entrancy those two managed by hand with
/// `PushTraceVisits` and a depth counter, which here is [`Visits`].
pub struct Tracer<'a> {
    bsp: &'a CollisionBsp,
    visits: Visits,
    /// The brush entities in the clip chain — see
    /// [`with_entities`](Tracer::with_entities). Empty by default, which is
    /// what makes [`trace`](Tracer::trace) a world-only sweep for every caller
    /// that has not asked for more.
    entities: &'a [BrushModel],
    /// The carved wall substituted for the world near a portal, and its own
    /// visit stamps — see [`with_hole`](Tracer::with_hole). `None` by default,
    /// which is every caller that has not asked for a hole.
    ///
    /// The stamps are the hole's rather than the world's because they are
    /// indexed by *its* brush numbering and there are a few hundred of them
    /// against the map's tens of thousands. Allocated once, when the hole is
    /// attached, for the reason the world's are: a `Tracer` is made once and
    /// kept.
    hole: Option<Hole<'a>>,
    /// Half the size of the box on the far side of the hole, when a transition
    /// would change it — see [`with_exit_hull`](Tracer::with_exit_hull).
    ///
    /// Outside the hole entirely, so that setting it before or after
    /// [`with_hole`](Tracer::with_hole) is the same thing.
    exit_extents: Option<Vec3>,
    /// The physics props in the clip chain — see
    /// [`with_props`](Tracer::with_props). `None` by default, which is every
    /// trace in the port that is not the player's movement.
    ///
    /// A trait object, so the `Debug` the rest of a `Tracer` derives has to be
    /// written by hand below.
    props: Option<&'a dyn PropQuery>,
}

impl std::fmt::Debug for Tracer<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracer")
            .field("bsp", &self.bsp)
            .field("visits", &self.visits)
            .field("entities", &self.entities)
            .field("hole", &self.hole)
            .field("exit_extents", &self.exit_extents)
            .field("props", &self.props.map(|_| "..."))
            .finish()
    }
}

/// One attached hole, with a set of visit stamps per model it sweeps.
///
/// Three models rather than one because the shipped engine has three
/// collideables — the carved pieces, the tube and the exit's geometry — and
/// each needs its own stamps, indexed by its own brush numbering.
#[derive(Debug)]
struct Hole<'a> {
    wall: &'a CarvedWall,
    pieces: Visits,
    tube: Visits,
    /// `None` when the portal is unlinked, which is when there is no remote
    /// model to sweep.
    remote: Option<Visits>,
    /// The transition ramp's, which is one brush — `None` for the same reason
    /// [`remote`](Hole::remote) is.
    ramp: Option<Visits>,
}

impl<'a> Hole<'a> {
    fn new(wall: &'a CarvedWall) -> Hole<'a> {
        let stamps = |bsp: &CollisionBsp| Visits::new(bsp.brushes.len(), bsp.disps.len());
        Hole {
            pieces: stamps(wall.collision()),
            tube: stamps(wall.tube()),
            remote: wall.remote().map(stamps),
            ramp: wall.ramp().map(stamps),
            wall,
        }
    }
}

impl<'a> Tracer<'a> {
    pub(super) fn new(bsp: &'a CollisionBsp) -> Tracer<'a> {
        Tracer {
            bsp,
            visits: Visits::new(bsp.brushes.len(), bsp.disps.len()),
            entities: &[],
            hole: None,
            exit_extents: None,
            props: None,
        }
    }

    /// The collision model this tracer sweeps against.
    ///
    /// Exposed so that a caller holding a tracer does not also have to carry
    /// the [`CollisionBsp`] it came from — the light cache asks for a leaf and
    /// then traces, and passing both would let the two disagree.
    pub fn collision(&self) -> &'a CollisionBsp {
        self.bsp
    }

    /// Puts brush entities in the clip chain — `ENGINE_TRACE.md` stage 4, and
    /// the thing that makes a door a wall.
    ///
    /// **Which entities is the game's decision, not this module's.** Whether a
    /// brush entity is solid is `FSOLID_NOT_SOLID`, whether it is a trigger is
    /// `FSOLID_TRIGGER`, and both live in `src/server/`; `world/`'s
    /// [`clip_models`](crate::engine::world::World::clip_models) is where the
    /// answer arrives. Handing over an empty slice — the default — is a
    /// world-only trace, which is what every caller before this stage wanted
    /// and still gets.
    ///
    /// The list is walked linearly. That is Valve's spatial partition replaced
    /// by nothing, deliberately: a Portal 2 map has a few hundred brush
    /// entities (78 on `sp_a1_intro1`), each rejected by a bounding-box test
    /// at the top of its own BSP descent, and `ENGINE_TRACE.md` §5 already
    /// records that `parry`'s `Qbvh` is where a broadphase comes from when one
    /// is needed.
    pub fn with_entities(mut self, entities: &'a [BrushModel]) -> Tracer<'a> {
        self.entities = entities;
        self
    }

    /// Puts the **physics props** in the clip chain — the thing that makes a
    /// cube a wall.
    ///
    /// `CEngineTrace::ClipRayToVPhysics` (`engine/enginetrace.cpp:1115`) is
    /// what this stands in for, and the shipped engine reaches it for exactly
    /// the entities whose `GetSolid()` is `SOLID_VPHYSICS`. Which props those
    /// are is the game's decision and not this module's, the same way
    /// [`with_entities`](Tracer::with_entities)' is — see [`PropQuery`].
    ///
    /// **Additive, like `with_entities` and unlike
    /// [`with_hole`](Tracer::with_hole).** The prop sweep runs after the
    /// world and the brush entities and the nearest of the three wins, which
    /// is `CEngineTrace::ClipRayToEntity`'s own order.
    ///
    /// > **It runs only for a mask containing [`Contents::SOLID`].** A
    /// > physics prop reports `CONTENTS_SOLID` and nothing else here, where
    /// > Valve reads `studiohdr_t::contents` (`enginetrace.cpp:1099`) — see
    /// > `portdocs/VPHYSICS_SHADOW.md` §2.3. The consequence is that a light
    /// > trace, a `MASK_OPAQUE` trace and a paint trace are all unaffected by
    /// > props, which is right for the first two and is a divergence for
    /// > nothing that currently exists.
    pub fn with_props(mut self, props: &'a dyn PropQuery) -> Tracer<'a> {
        self.props = Some(props);
        self
    }

    /// Substitutes a portal's carved geometry for the world near it —
    /// `portdocs/PORTAL.md` §4.5's third item, and what makes the hole a hole.
    ///
    /// **Which portal is the caller's decision**, the way
    /// [`with_entities`](Tracer::with_entities) leaves the clip chain to the
    /// caller: `PortalHoles::touching` is the answer for a player, and this
    /// module does not ask why.
    ///
    /// # Substitutive, not additive
    ///
    /// The clip chain *adds* candidates and takes the nearest. A hole does the
    /// opposite — it takes away the wall the portal is on — and no amount of
    /// adding can produce a subtraction, so [`trace`](Tracer::trace) runs the
    /// sweep twice and reconciles: once against the real world and, only if
    /// that hit something, once against the carved pieces, keeping **whichever
    /// went further**. That is `TracePortalPlayerAABB`
    /// (`portal_gamemovement.cpp:1640`) steps 1 to 3, exactly; its fourth step,
    /// the ray transformed into the exit portal's space, is stage 4's and is
    /// not here.
    ///
    /// # The cost, and the hazard
    ///
    /// A trace that hits nothing pays one comparison, which is Valve's guard
    /// and is why a player nowhere near a portal is unaffected. One that hits
    /// pays a second descent over a few hundred brushes, and — when the portal
    /// is linked and that descent came out better than the real world — a
    /// third over the geometry at the far end.
    ///
    /// **"Whichever went further" is only safe inside the carved region.** The
    /// carve holds the world within `vCollisionCloneExtents` of the portal
    /// (`halfWidth + 75` across, `halfHeight + 75` through and up — 107 by 131
    /// units for a shipped portal) and nothing outside it, so a sweep that leaves
    /// that region finds nothing in the carved set, comes back at fraction 1,
    /// and wins — i.e. passes through whatever the real world had there. A
    /// movement step cannot: the guard needs the real trace to have hit
    /// something, the selection needs the player inside the portal's trigger
    /// box, and a tick's motion is a fraction of the distance to the edge.
    /// A long ray can, and must not be traced with a hole attached. This is
    /// the shipped engine's shape and is recorded rather than fixed —
    /// `rustdocs/ENGINE.md` gotcha 23.
    pub fn with_hole(mut self, wall: &'a CarvedWall) -> Tracer<'a> {
        self.set_hole(Some(wall));
        self
    }

    /// The same, on a tracer already in hand.
    ///
    /// [`with_hole`](Tracer::with_hole) is how a caller that is *building* a
    /// tracer attaches one; this is for the one caller that has to change it
    /// mid-move — `HandlePortalling`, which teleports the player and then has
    /// to check whether they came out inside something, against the geometry
    /// at the **exit**.
    ///
    /// Allocates a set of visit stamps, so it is not something to call per
    /// trace.
    pub fn set_hole(&mut self, wall: Option<&'a CarvedWall>) {
        self.hole = wall.map(Hole::new);
    }

    /// The box the player would have on the far side of a portal, when the
    /// transition would change it.
    ///
    /// `vTeleportExtents` (`portal_gamemovement.cpp:1930`). A wall-to-floor
    /// transition rotates the player's up axis, so `HandlePortalling` folds
    /// them into the duck hull as they cross — and the remote trace has to ask
    /// about the box they will *have*, not the one they have now, or it holds
    /// them up on a floor their real hull would never have cleared.
    ///
    /// Left unset the remote box is the local one, which is what Valve uses
    /// for every transition that does not force a crouch. **`trace/` cannot
    /// name the duck hull** — it is `client/`'s — so the decision and the
    /// numbers both come from the caller.
    pub fn with_exit_hull(mut self, mins: Vec3, maxs: Vec3) -> Tracer<'a> {
        self.exit_extents = Some((maxs - mins) * 0.5);
        self
    }

    /// Sweeps `ray` through the world and everything in the clip chain,
    /// stopping at the nearest thing matching `mask`.
    ///
    /// `CEngineTrace::TraceRay` (`engine/enginetrace.cpp:2786`). With no
    /// entities — the default, and every caller before stage 4 — it is
    /// `CM_BoxTrace` against head node 0 and nothing else; with entities it is
    /// that, followed by [`trace_model`](Tracer::trace_model) against each in
    /// turn. See [`with_entities`](Tracer::with_entities) for who decides
    /// which.
    ///
    /// **With a hole attached this is no longer only that**: it is the world's
    /// answer *or* the portal's carved one, whichever went further. See
    /// [`with_hole`](Tracer::with_hole), which is the only way to get one.
    pub fn trace(&mut self, ray: &Ray, mask: Contents) -> Trace {
        let real = match self.entities.is_empty() && self.props.is_none() {
            true => self.trace_world(ray, mask),
            false => self.trace_chain(ray, mask),
        };
        match self.hole.is_some() {
            false => real,
            true => self.substitute(ray, mask, real),
        }
    }

    /// [`trace`](Tracer::trace)'s portal half — `TracePortalPlayerAABB`
    /// (`portal_gamemovement.cpp:1640`) steps 1 to 3, with step 1 already done
    /// and handed over as `real`.
    ///
    /// Two conditions, both Valve's and both worth reading twice:
    ///
    /// - **The carved sweep only runs when the real one hit something.** A
    ///   trace that reached its end in open air has nothing a hole could
    ///   improve, and letting the carved set answer it is exactly the failure
    ///   [`with_hole`](Tracer::with_hole) warns about.
    /// - **The carved answer wins when it went at least as far**, or whenever
    ///   the real one began inside a solid — which is the case that matters,
    ///   because a player standing in the hole *is* inside the wall as far as
    ///   the real world is concerned, and without this they would be shoved
    ///   out of it. A carved sweep that itself began solid never wins.
    ///
    /// # The far side
    ///
    /// Steps 4 and 5 are the rest of `portdocs/PORTAL.md` §5 and they are what
    /// makes a portal a portal rather than a hole. The same sweep is asked
    /// again in the **exit** portal's space — see
    /// [`CarvedWall::remote_ray`] for what "the same sweep" means for a box
    /// that cannot rotate — against the exit's own World geometry and the
    /// entrance's tube. Anything it hits is something the player *would* be
    /// hitting if they teleported this instant, so it is taken when it is
    /// nearer, transformed back into the caller's frame, and the whole
    /// reconciliation against the real world is then run **again**: a remote
    /// hit that made the portal answer worse than the real one gives the real
    /// one back. Valve's comment lists the three ways that happens and ends
    /// *"if we switch over to portal traces, we have to commit 100%"*.
    ///
    /// The remote sweep is deliberately **world-only** in the shipped engine —
    /// `sv_portal_new_player_trace_vs_remote_ents` defaults to 0, to avoid
    /// *"the projected floor to wall dilemma where we can ledge walk in the
    /// middle of the portal"* — and it is world-only here because the carve
    /// holds brushes and nothing else.
    fn substitute(&mut self, ray: &Ray, mask: Contents, real: Trace) -> Trace {
        if !(real.start_solid || (ray.is_swept && real.fraction < 1.0)) {
            return real;
        }
        let exit_extents = self.exit_extents;
        let Some(hole) = self.hole.as_mut() else {
            return real;
        };

        // Step 2: the portal's own geometry — the carved pieces and the tube
        // that lines the hole, which is a separate collideable in the shipped
        // engine and is one here.
        let mut portal = sweep(hole.wall.collision(), &mut hole.pieces, ray, 0, mask);
        keep_nearer(
            &mut portal,
            sweep(hole.wall.tube(), &mut hole.tube, ray, 0, mask),
            ray,
        );
        compute_trace_endpoints(ray, &mut portal);
        fix_up_hull_start(ray, &mut portal);

        // Step 3, which is also the gate on doing any more work: if the carved
        // answer is not already better than the real one, the far side cannot
        // make it so — everything it can do is bring the fraction *down*.
        if !better(&real, &portal, ray) {
            return real;
        }

        // The transition ramp, which is asked before the far side and read
        // after it. `PortalTrace.fraction` in Valve's comparison is the
        // carved answer *before* any remote merge, which is what `portal`
        // still holds here.
        let mut contacted_ramp = false;
        if let (Some(ramp), Some(visits)) = (hole.wall.ramp(), hole.ramp.as_mut()) {
            if let Some((far_ray, _)) = hole.wall.remote_ray(ray, exit_extents) {
                let mut angle = sweep(ramp, visits, &far_ray, 0, mask);
                compute_trace_endpoints(&far_ray, &mut angle);
                fix_up_hull_start(&far_ray, &mut angle);
                // *"note, only has to go as far as local trace to work"* — so
                // `<=` rather than `<`, and the `< 1.0` is what stops a sweep
                // that reached its end from claiming a contact.
                contacted_ramp = angle.start_solid
                    || (far_ray.is_swept
                        && angle.fraction <= portal.fraction
                        && angle.fraction < 1.0);
            }
        }

        // Steps 4 and 5.
        if let (Some(remote), Some(visits), Some(link)) =
            (hole.wall.remote(), hole.remote.as_mut(), hole.wall.link())
        {
            if let Some((far_ray, shift)) = hole.wall.remote_ray(ray, exit_extents) {
                let mut far = sweep(remote, visits, &far_ray, 0, mask);
                compute_trace_endpoints(&far_ray, &mut far);
                fix_up_hull_start(&far_ray, &mut far);

                if far.start_solid || (ray.is_swept && far.fraction < portal.fraction) {
                    let back = link.to_entrance;
                    let point_on_plane = back.transform_point3(far.normal * far.plane_dist - shift);
                    let normal = back.transform_vector3(far.normal);
                    portal = Trace {
                        // The sweep began where the *caller* said it did; only
                        // the impact came from the far side.
                        start: real.start,
                        end: back.transform_point3(far.end - shift) + ray.offset,
                        normal,
                        plane_dist: normal.dot(point_on_plane),
                        ..far
                    };
                }
            }
        }

        // `pm.m_bContactedPortalTransitionRamp = bContactedTransitionRamp` on
        // the branch that keeps the portal's answer, and `= false` on both
        // branches that give the real one back — *"if we switch over to portal
        // traces, we have to commit 100%"*, and the ramp is part of that
        // commitment.
        match better(&real, &portal, ray) {
            true => Trace {
                portal_ramp: contacted_ramp,
                ..portal
            },
            false => real,
        }
    }

    /// The world's subtree alone — `CM_BoxTrace( ray, 0, mask, … )`.
    ///
    /// **Brush models are not included**: a door is not part of the world's
    /// subtree, and hitting one is [`trace_model`](Tracer::trace_model)'s
    /// question. [`trace`](Tracer::trace) is what combines the two.
    pub fn trace_world(&mut self, ray: &Ray, mask: Contents) -> Trace {
        let mut trace = self.box_trace(ray, 0, mask);
        compute_trace_endpoints(ray, &mut trace);
        fix_up_hull_start(ray, &mut trace);
        trace
    }

    /// Sweeps `ray` against one brush model — a door, a platform, a piston.
    ///
    /// `CM_TransformedBoxTrace` (`engine/cmodel.cpp:3253`), which is the whole
    /// of `ClipRayToBSP` (`engine/enginetrace.cpp:1203`): the ray is moved into
    /// the model's frame, the ordinary sweep runs against the model's own
    /// subtree, and the normal is turned back out. **The answer is in the
    /// caller's frame** — `start`, `end` and `normal` are all world space, and
    /// `fraction` means the same thing it means for [`trace`](Tracer::trace),
    /// because rotating a delta does not change its length.
    ///
    /// The one field that is *not* rotated back is
    /// [`Trace::plane_dist`](Trace::plane_dist), which stays in the model's
    /// local frame. That is Valve's — `CM_TransformedBoxTrace` fixes up
    /// `plane.normal` and says nothing about `plane.dist` — and it is ported
    /// as written rather than corrected, because every consumer reads the
    /// normal and none reads the distance.
    pub fn trace_model(&mut self, ray: &Ray, model: &BrushModel, mask: Contents) -> Trace {
        let local = model.local_ray(ray);
        let mut trace = self.box_trace(&local, model.head_node, mask);

        // "If we hit, gotta fix up the normal..." — and only then, because a
        // trace that hit nothing has a zero normal that would rotate to
        // another zero and cost the work anyway.
        if trace.fraction != 1.0 {
            if let Some(rotation) = model.rotation {
                trace.normal = rotation * trace.normal;
            }
        }

        // Against the **world** ray, not the local one: Valve passes
        // `computeEndpt = false` into `CM_BoxTrace` for exactly this reason,
        // so that the positions are produced once, from the ray the caller
        // actually handed over.
        compute_trace_endpoints(ray, &mut trace);
        fix_up_hull_start(ray, &mut trace);
        trace
    }

    /// Every world brush the axis-aligned box `mins`..`maxs` overlaps and
    /// `mask` admits — `CEngineTrace::GetBrushesInAABB`
    /// (`engine/enginetrace.cpp:599`).
    ///
    /// The one query this module answers that is not a sweep, and it exists
    /// for `portdocs/PORTAL.md` stage 3: cutting a hole in a wall means
    /// re-emitting the planes of every brush near the portal with four more
    /// added, so the carve has to be handed the brushes before it can say
    /// anything about them. The indices are into this module's own brush
    /// table.
    ///
    /// # Overlap, not leaf membership
    ///
    /// A brush belongs to every leaf it touches and a leaf lists every brush
    /// that touches *it*, so concatenating the leaf lists answers a larger
    /// question than the one asked — a brush can share a leaf with the box and
    /// be the length of the room away from it. Each candidate is therefore put
    /// through the position test, [`brush::test_box_in_brush`], and kept only
    /// when the box is really inside it. That is Valve's own shape: it sets up
    /// a `TraceInfo_t` whose start and end are both the box's centre and keeps
    /// the brushes that come back `allsolid`.
    ///
    /// # The world only, and leaf-limited
    ///
    /// Head node 0, no brush models, and nothing from
    /// [`with_entities`](Tracer::with_entities) — the same scope as
    /// [`trace_world`](Tracer::trace_world). Valve's form for anything else is
    /// a separate call, `GetBrushesInCollideable`, which is what the portal
    /// simulator carves static props and moving geometry with; both are stage
    /// 3's deferred half.
    ///
    /// **"Every world brush the box overlaps" is not quite what comes back**,
    /// and the difference is the shipped engine's as much as this port's: a
    /// brush that no leaf names is unreachable, and a brush can reach past the
    /// leaves that do name it. `rustdocs/ENGINE.md`'s gotcha 22 has the
    /// measured shape of both, and
    /// `every_shipped_map_enumerates_the_brushes_in_a_box` is written around
    /// them. **The carve cuts against what the leaves offer, not against the
    /// geometry.**
    ///
    /// Order is leaf order, then leaf-list order within a leaf, and **each
    /// brush appears once** however many of the box's leaves hold it.
    // Written ahead of its caller, which is `PORTAL.md` stage 3's carve. The
    // seeding also keeps `leaves_in_box` alive, since that has no other one.
    #[allow(dead_code)]
    pub fn brushes_in_box(&mut self, mins: Vec3, maxs: Vec3, mask: Contents) -> Vec<usize> {
        let leaves = self.bsp.leaves_in_box(mins, maxs, 0);
        if leaves.is_empty() {
            return Vec::new();
        }

        self.visits.begin();

        // The degenerate sweep the position test reads: a box at rest at the
        // query's centre. `is_point` stays false even for a zero-size query,
        // because the flag selects an algorithm rather than describing the
        // extents — Valve sets it false here unconditionally.
        let center = (mins + maxs) * 0.5;
        let mut work = Work {
            bsp: self.bsp,
            start: center,
            end: center,
            extents: maxs - center,
            delta: Vec3::ZERO,
            inv_delta: Vec3::ZERO,
            is_point: false,
            is_swept: false,
            contents: mask,
            trace: Trace::miss(center, center),
            disp_hit: false,
            visits: &mut self.visits,
        };

        let mut found = Vec::new();
        for leaf_index in leaves {
            let leaf = work.bsp.leaves[leaf_index];
            let first = leaf.first_leaf_brush as usize;
            let count = leaf.num_leaf_brushes as usize;
            for i in first..first + count {
                let index = work.bsp.leaf_brushes[i] as usize;
                // Marked before the contents test, as Valve marks it: a brush
                // the mask rejects is rejected in every leaf, and re-deciding
                // that per leaf would be work with no possible answer of its
                // own.
                if !work.visit(index) {
                    continue;
                }
                if !work.bsp.brushes[index].contents.intersects(mask) {
                    continue;
                }

                brush::test_box_in_brush(&mut work, index);
                if work.trace.all_solid {
                    found.push(index);
                    // "Clear the flag for re-use." The rest of the trace is
                    // left dirty deliberately: `test_box_in_brush` reads
                    // nothing out of it, so `all_solid` is the only field that
                    // could carry an answer from one brush to the next.
                    work.trace.all_solid = false;
                }
            }
        }
        found
    }

    /// [`trace`](Tracer::trace)'s entity half.
    ///
    /// # Three details, and each of them is load-bearing
    ///
    /// - **The world is traced first and the ray is then shortened to the
    ///   hit**, so a door behind a wall costs a rejected descent rather than a
    ///   full sweep. The shortening recomputes the end and *subtracts* to get
    ///   the delta rather than scaling it — Valve's comment says why: it makes
    ///   the shortened ray quantise exactly the way `endpos` does, and scaling
    ///   instead "would miss intersections we would get by feeding these
    ///   results back in to the tracer".
    /// - **The fractions come back rescaled onto the original ray.** Inside
    ///   the loop they are fractions of the shortened one; the last two lines
    ///   put them back, which is why `fraction` means the same thing here as
    ///   it does for a world-only trace.
    /// - **A trace that starts inside the world never looks at an entity.**
    ///   `if ( pTrace->startsolid ) return;` — "inside world, no need to check
    ///   being inside anything else".
    ///
    /// The static props and `vphysics` halves of `ClipRayToCollideable` are
    /// stage 5's; for a brush model the whole of it is `ClipRayToBSP`, which
    /// is [`trace_model`](Tracer::trace_model).
    fn trace_chain(&mut self, ray: &Ray, mask: Contents) -> Trace {
        let mut trace = self.trace_world(ray, mask);
        if trace.start_solid {
            return trace;
        }

        let world_fraction = trace.fraction;
        let mut world_fraction_left_solid = world_fraction;
        let mut entity_ray = *ray;

        if trace.fraction == 0.0 {
            entity_ray.delta = Vec3::ZERO;
            entity_ray.is_swept = false;
            world_fraction_left_solid = trace.fraction_left_solid;
            trace.fraction_left_solid = 1.0;
            trace.fraction = 1.0;
        } else {
            let end = entity_ray.start + trace.fraction * entity_ray.delta;
            entity_ray.delta = end - entity_ray.start;
            entity_ray.is_swept = entity_ray.delta.length_squared() != 0.0;
            trace.fraction_left_solid /= trace.fraction;
            trace.fraction = 1.0;
        }

        for i in 0..self.entities.len() {
            let model = self.entities[i];
            let clip = self.trace_model(&entity_ray, &model, mask);
            clip_trace_to_trace(&clip, &mut trace);
            if trace.all_solid {
                break;
            }
        }

        // The props, swept against the same rescaled ray the brush entities
        // were — which is what keeps the `*= world_fraction` below correct for
        // all three sources at once.
        if let Some(props) = self.props.filter(|_| !trace.all_solid) {
            if mask.intersects(Contents::SOLID) {
                let start = entity_ray.start;
                let hit = props.sweep(entity_ray.extents, start, start + entity_ray.delta);
                if let Some(hit) = hit {
                    let clip = prop_trace(&entity_ray, &hit);
                    clip_trace_to_trace(&clip, &mut trace);
                }
            }
        }

        trace.fraction *= world_fraction;
        trace.fraction_left_solid *= world_fraction_left_solid;

        // "Make sure no fractionleftsolid can be used with box sweeps."
        if !ray.is_ray {
            trace.start = ray.origin();
            trace.fraction_left_solid = 0.0;
        }
        trace
    }

    /// `CM_BoxTrace` with Valve's `computeEndpt` false: fractions and flags,
    /// with `start`/`end` left for the caller to resolve in whichever frame it
    /// asked the question.
    ///
    /// `head_node` selects the subtree — 0 for the world, a model's own for a
    /// brush model. It is trusted to be a valid node index; `Bsp::parse`'s
    /// `validate` is what makes that true.
    fn box_trace(&mut self, ray: &Ray, head_node: i32, mask: Contents) -> Trace {
        sweep(self.bsp, &mut self.visits, ray, head_node, mask)
    }
}

/// [`Tracer::box_trace`] against a collision model that is not the tracer's
/// own, with its own visit stamps.
///
/// Split out for exactly one caller — [`Tracer::substitute`], which sweeps the
/// carved pieces and cannot borrow the tracer's world and stamps to do it. A
/// `Tracer` over the pieces would allocate a stamp table per trace; this
/// allocates none, because [`with_hole`](Tracer::with_hole) allocated it once.
fn sweep(
    bsp: &CollisionBsp,
    visits: &mut Visits,
    ray: &Ray,
    head_node: i32,
    mask: Contents,
) -> Trace {
    // `if (!pBSPData->numnodes)` — a map with no collision tree traces as
    // a clean miss. Valve returns here leaving `startpos`/`endpos` at
    // zero; the positions below are placeholders either way, because both
    // callers overwrite them with `compute_trace_endpoints`.
    if bsp.nodes.is_empty() {
        return Trace::miss(ray.start, ray.start + ray.delta);
    }

    visits.begin();

    let start = ray.start;
    let end = ray.start + ray.delta;
    let mut work = Work {
        bsp,
        start,
        end,
        extents: ray.extents,
        delta: ray.delta,
        inv_delta: ray.inv_delta(),
        is_point: ray.is_ray,
        is_swept: ray.is_swept,
        contents: mask,
        trace: Trace::miss(start, end),
        disp_hit: false,
        visits,
    };

    if !ray.is_swept {
        // A zero-length sweep is a position test and has no direction to
        // split the tree on.
        hull::unswept_box_trace(&mut work, head_node);
    } else if ray.is_ray {
        hull::recursive_hull_check::<true>(&mut work, head_node, 0.0, 1.0, start, end);
    } else {
        hull::recursive_hull_check::<false>(&mut work, head_node, 0.0, 1.0, start, end);
    }
    work.trace
}

/// Sweeps every displacement in one leaf's list — `CM_TraceToDispList`
/// (`engine/cmodel.cpp:1761`).
///
/// The per-displacement rejection is a box test against the patch's bounds,
/// grown by the sweeping box's extents; the tree walk inside
/// [`DispTree`](disp::DispTree) then culls to the leaves the ray touches. Both
/// are needed — a displacement's bounds are as big as its whole patch.
fn trace_to_disp_list<const IS_POINT: bool>(work: &mut Work<'_>, first: usize, count: usize) {
    let bsp = work.bsp;
    for i in first..first + count {
        let index = bsp.leaf_disps[i] as usize;
        let Some(disp) = &bsp.disps[index] else {
            continue;
        };

        // Only collide with what the caller asked for.
        if !disp.contents.intersects(work.contents) {
            continue;
        }
        // `if( CHECK_COUNTERS && pTraceInfo->m_isswept )` — the stamp is
        // skipped for an unswept trace, because `CM_TestInDispTree` has its
        // own loop and wants to see every patch.
        if work.is_swept && !work.visit_disp(index) {
            continue;
        }

        // The bounds test. A ray is tested against the patch's own box, a hull
        // against the box grown by its extents.
        let (mins, maxs) = match IS_POINT {
            true => (disp.mins, disp.maxs),
            false => (disp.mins - work.extents, disp.maxs + work.extents),
        };
        if !box_intersects_ray(mins, maxs, work.start, work.delta, work.inv_delta) {
            continue;
        }

        // `CM_TraceToDispTree` (`engine/cmodel_disp.cpp:364`).
        let hit = match IS_POINT {
            true => disp.trace_ray(work.start, work.delta, work.inv_delta, &mut work.trace),
            false => disp.sweep_box(
                work.start,
                work.delta,
                work.extents,
                work.inv_delta,
                &mut work.trace,
            ),
        };
        if hit {
            work.disp_hit = true;
            work.trace.contents = disp.contents;
            set_disp_surface(work, index);
        }

        if work.trace.fraction == 0.0 {
            break;
        }
    }

    post_trace_to_disp_tree(work);
}

/// `SetDispTraceSurfaceProps` (`engine/cmodel_disp.cpp:37`).
///
/// **One deliberate divergence.** Valve names the surface `"**displacement**"`,
/// a constant string with no information in it; this reports the base face's
/// own material instead, so `trace` prints `CONCRETE/CONCRETE_MODULAR_FLOOR001`
/// rather than a placeholder. The *flags* are unchanged, and they are the
/// load-bearing half: Valve's `pDisp->GetTexinfoFlags()` is the surface table's
/// entry for that same texdata, which is exactly what this index resolves to.
fn set_disp_surface(work: &mut Work<'_>, index: usize) {
    let surface = work.bsp.disps[index]
        .as_ref()
        .expect("a hit came from this displacement")
        .surface();
    let (surface, flags) = work.bsp.surface_at(surface);
    work.trace.surface = surface;
    work.trace.surface_flags = flags;
}

/// `CM_PostTraceToDispTree` (`engine/cmodel_disp.cpp:344`) — decides, after the
/// fact, that a displacement hit means the sweep began inside the terrain.
///
/// The test is "did we hit the surface from behind", and it can only *just*
/// fire: both triangle tests are one-sided, and the sweep's is one-sided with
/// `DIST_EPSILON` of slack (`disp::sweep_triangle`'s first line), so
/// `normal · delta` is at most 1/32 when a hit is recorded at all. Ported as
/// written; it is not this port's asymmetry to fix.
fn post_trace_to_disp_tree(work: &mut Work<'_>) {
    if !work.disp_hit {
        return;
    }
    if work.trace.normal.dot(work.delta) > 0.0 {
        work.trace.start_solid = true;
        work.trace.all_solid = true;
    }
}

/// The position test against one leaf's displacements — `CM_TestInDispTree`
/// (`engine/cmodel_disp.cpp:364`).
///
/// Two halves, and only the first one does real work:
///
/// 1. **A box** is tested against each patch's triangles with a
///    separating-axis test ([`DispTree::intersects_box`](disp::DispTree)). An
///    overlap is `all_solid` and returns immediately.
/// 2. **Otherwise the stab** — a second, full trace fired from the query point
///    along the patch's own surface normal, whose result decides whether the
///    point was behind the surface.
///
/// The stab is the ugliest code in the subsystem, and reading it against the
/// triangle tests shows it can **essentially only clear a solid verdict, never
/// set one**: both triangle tests reject a query travelling *along* the
/// normal, and the stab travels along the normal by construction, so nothing
/// is hit and [`post_stab`] takes its clearing branch. That is Valve's, it is
/// ported as written, and it is safe because `test_in_leaf` returns before
/// reaching here if a *brush* already reported solid.
fn test_in_disp_tree(work: &mut Work<'_>, first: usize, count: usize) {
    let bsp = work.bsp;

    // `bIsBox`: Valve tests `m_mins`/`m_maxs` for any non-zero component,
    // which is the extents being non-zero. Note this is **not** `!is_point`:
    // a hull small enough to have taken the point path still has extents.
    if work.extents != Vec3::ZERO {
        let abs_mins = work.start - work.extents;
        let abs_maxs = work.start + work.extents;

        for i in first..first + count {
            let index = bsp.leaf_disps[i] as usize;
            let Some(disp) = &bsp.disps[index] else {
                continue;
            };
            if !disp.contents.intersects(work.contents) {
                continue;
            }
            if !work.visit_disp(index) {
                continue;
            }
            if !(abs_mins.cmple(disp.maxs).all() && abs_maxs.cmpge(disp.mins).all()) {
                continue;
            }
            if disp.intersects_box(abs_mins, abs_maxs) {
                work.trace.start_solid = true;
                work.trace.all_solid = true;
                work.trace.fraction = 0.0;
                work.trace.fraction_left_solid = 0.0;
                work.trace.contents = disp.contents;
                return;
            }
        }
    }

    let dir = pre_stab(work, first, count);
    stab(work, dir);
    post_stab(work);
}

/// Which way to stab — `CM_PreStab` (`engine/cmodel_disp.cpp:422`).
///
/// The direction belongs to whichever patch in the leaf the query is inside the
/// bounds of; failing that, to the first one in the list, "and set contents to
/// solid". Valve's `contents` out-parameter is dropped here because `CM_Stab`
/// takes it and never reads it.
fn pre_stab(work: &mut Work<'_>, first: usize, count: usize) -> Vec3 {
    let bsp = work.bsp;
    let mut dir = Vec3::ZERO;
    let mut found_any = false;

    for i in first..first + count {
        let Some(disp) = &bsp.disps[work.bsp.leaf_disps[i] as usize] else {
            continue;
        };
        if !found_any {
            dir = disp.stab_dir();
            found_any = true;
        }
        if !disp.contents.intersects(work.contents) {
            continue;
        }
        if disp.point_in_bounds(work.start, work.extents, work.is_point) {
            return disp.stab_dir();
        }
    }
    dir
}

/// `CM_Stab` (`engine/cmodel_disp.cpp:476`) — a whole second trace, fired from
/// inside a position test.
///
/// It re-aims the query along `dir` for `STAB_LENGTH` units and re-runs the
/// hull check from **head node 0**, the world. That is right rather than
/// lucky: displacements are world faces, so only the world subtree's leaves
/// ever carry them, and this is unreachable from
/// [`trace_model`](Tracer::trace_model) for the same reason.
fn stab(work: &mut Work<'_>, dir: Vec3) {
    work.trace.fraction = 1.0;
    work.trace.fraction_left_solid = 0.0;
    work.trace.surface = None;
    work.trace.surface_flags = 0;
    work.trace.start_solid = false;
    work.trace.all_solid = false;
    work.disp_hit = false;

    let saved = (work.end, work.delta, work.inv_delta);
    work.end = work.start + dir * STAB_LENGTH;
    work.delta = work.end - work.start;
    work.inv_delta = ray::inv_delta(work.delta);

    // A nested trace needs its own visit marks, or every brush and patch the
    // outer position test has already looked at is invisible to it.
    work.visits.push();
    let (p1, p2) = (work.start, work.end);
    match work.is_point {
        true => hull::recursive_hull_check::<true>(work, 0, 0.0, 1.0, p1, p2),
        false => hull::recursive_hull_check::<false>(work, 0, 0.0, 1.0, p1, p2),
    }
    work.visits.pop();

    // Valve restores `m_end` alone and leaves the stab's delta behind. That is
    // unobservable — nothing after this reads the delta of an unswept trace —
    // but it is restored here rather than left as a trap for stage 4.
    (work.end, work.delta, work.inv_delta) = saved;
}

/// `CM_PostStab` (`engine/cmodel_disp.cpp:518`).
fn post_stab(work: &mut Work<'_>) {
    if work.disp_hit && work.trace.start_solid {
        work.trace.all_solid = true;
        work.trace.fraction = 0.0;
        work.trace.fraction_left_solid = 0.0;
    } else {
        work.trace.start_solid = false;
        work.trace.all_solid = false;
        work.trace.contents = Contents::EMPTY;
        work.trace.fraction = 1.0;
        work.trace.fraction_left_solid = 0.0;
    }
}

/// `IsBoxIntersectingRay` with a tolerance (`public/collisionutils.cpp:766`),
/// scalar path.
///
/// The `DISPCOLL_DIST_EPSILON` tolerance is Valve's at every displacement call
/// site, and it is the same 1/32 [`DIST_EPSILON`] is — the two constants are
/// spelled separately in the C++ and have never differed.
fn box_intersects_ray(mins: Vec3, maxs: Vec3, start: Vec3, delta: Vec3, inv_delta: Vec3) -> bool {
    let mut t_min = -f32::MAX;
    let mut t_max = f32::MAX;
    for i in 0..3 {
        if delta[i].abs() < 1e-8 {
            // Parallel to this slab: the start has to already be inside it.
            if start[i] < mins[i] - DIST_EPSILON || start[i] > maxs[i] + DIST_EPSILON {
                return false;
            }
            continue;
        }
        let t1 = (mins[i] - DIST_EPSILON - start[i]) * inv_delta[i];
        let t2 = (maxs[i] + DIST_EPSILON - start[i]) * inv_delta[i];
        let (t1, t2) = (t1.min(t2), t1.max(t2));
        t_min = t_min.max(t1);
        t_max = t_max.min(t2);
        if t_min > t_max || t_max < 0.0 || t_min > 1.0 {
            return false;
        }
    }
    true
}

/// `CEngineTrace::TraceRay`'s last act (`engine/enginetrace.cpp:2956`): a box
/// sweep never computed `fractionleftsolid`, so it must not appear to have
/// one. Valve writes a NaN here in debug builds to catch anyone reading it.
///
/// Runs after [`compute_trace_endpoints`], never before — that function reads
/// `fraction_left_solid` and can turn a value of 1 into an all-solid verdict,
/// which zeroing it first would throw away.
fn fix_up_hull_start(ray: &Ray, trace: &mut Trace) {
    if !ray.is_ray {
        trace.start = ray.origin();
        trace.fraction_left_solid = 0.0;
    }
}

/// Whether the portal's answer is the one to keep — `TracePortalPlayerAABB`'s
/// test, which appears twice in the original and has to be the same both
/// times.
///
/// Read it as: *the real world says the player is stuck, or the portal got at
/// least as far without being stuck itself.* The `>=` rather than `>` is
/// Valve's and is load-bearing — the two traces agree exactly whenever the
/// carve did not remove anything in the way, and a `>` there would throw the
/// portal's answer away in precisely the case where the two are equivalent.
fn better(real: &Trace, portal: &Trace, ray: &Ray) -> bool {
    real.start_solid || (!portal.start_solid && ray.is_swept && portal.fraction >= real.fraction)
}

/// Merges one more collideable's answer into a trace — the rule
/// `UTIL_Portal_TraceRay` (`portal_util_shared.cpp:638`) applies after every
/// one of its sweeps.
///
/// Note what it is *not*: `ClipTraceToTrace`'s merge, which ORs the solid
/// flags. This one replaces wholesale, so a nearer hit against something the
/// ray did not start inside clears a `start_solid` an earlier model set. That
/// is the shipped engine's, and the models it combines do not overlap.
fn keep_nearer(into: &mut Trace, other: Trace, ray: &Ray) {
    if (other.start_solid && !into.start_solid) || (ray.is_swept && other.fraction < into.fraction)
    {
        *into = other;
    }
}

/// `CM_ComputeTraceEndpoints` (`engine/cmodel.cpp:2252`) — turns fractions
/// into positions, in the caller's frame rather than the centred one.
fn compute_trace_endpoints(ray: &Ray, trace: &mut Trace) {
    let start = ray.origin();

    trace.end = if trace.fraction == 1.0 {
        start + ray.delta
    } else {
        start + ray.delta * trace.fraction
    };

    if trace.fraction_left_solid == 0.0 {
        trace.start = start;
        return;
    }
    if trace.fraction_left_solid == 1.0 {
        // Never left solid.
        trace.start_solid = true;
        trace.all_solid = true;
        trace.fraction = 0.0;
        trace.end = start;
    }
    trace.start = start + ray.delta * trace.fraction_left_solid;
}

/// A [`PropHit`] as a [`Trace`], with this module's slack convention applied.
///
/// The two things the physics sweep cannot know about:
///
/// - **`DIST_EPSILON`.** The brush sweep bakes 1/32 of a unit into every plane
///   it tests (`hull.rs:77`), so it stops just short of the surface and a
///   player never fuses to a wall. A shape cast has no equivalent, so the
///   backoff is taken off the fraction here — converted from a distance to a
///   fraction by the sweep's own length, which is why a zero-length sweep
///   skips it rather than dividing by zero.
/// - **The trace fields that describe a *brush*.** `contents`, `surface`,
///   `surface_flags` and `disp_flags` all come from `.bsp` data a prop has
///   none of. `contents` is [`Contents::SOLID`] because that is what every
///   consumer needs to see to treat the hit as solid at all; the other three
///   stay at their miss values, so a caller asking "what material did I hit"
///   gets the null surface rather than a wrong one.
fn prop_trace(ray: &Ray, hit: &PropHit) -> Trace {
    let length = ray.delta.length();
    let backoff = match length > 0.0 {
        true => DIST_EPSILON / length,
        false => 0.0,
    };
    let fraction = match hit.start_solid {
        true => 0.0,
        false => (hit.fraction - backoff).max(0.0),
    };
    let start = ray.origin();
    Trace {
        start,
        end: start + ray.delta * fraction,
        normal: hit.normal,
        plane_dist: hit.normal.dot(start + ray.delta * fraction),
        fraction,
        // **A ray that starts inside a prop reports no exit fraction**, where
        // the brush sweep would. `fraction_left_solid` is "how far along the
        // sweep it stopped being inside", and a shape cast reports the entry
        // and not the exit; getting it would take a second cast from the far
        // end. Valve's own comment on computing it for a *box* is that it
        // needs "*a lot* more computation", and `trace_chain` zeroes it for
        // every box sweep two dozen lines below regardless — so the divergence
        // is a **ray** that begins inside a prop, and the conservative answer
        // (zero) only ever leaves the merge preferring whatever the world
        // said about where the sweep really started.
        fraction_left_solid: 0.0,
        contents: Contents::SOLID,
        surface: None,
        disp_flags: 0,
        surface_flags: 0,
        // A shape cast reports penetration; it does not report *never having
        // left* it, which is what `all_solid` means. Reporting `all_solid`
        // from a penetrating start would stop the whole chain
        // (`trace_chain`'s `break`) on a cube the player is merely clipping
        // the corner of.
        all_solid: false,
        start_solid: hit.start_solid,
        portal_ramp: false,
        hit_prop: true,
    }
}

/// `CEngineTrace::ClipTraceToTrace` (`engine/enginetrace.cpp:1524`) — keep
/// whichever of the two hits is nearer, and merge the start-solid state.
///
/// > **The merge is not "take the smaller fraction".** A trace that started
/// > inside something has a fraction of 1 and matters anyway, and when *both*
/// > started solid the surviving `start`/`fraction_left_solid` is the pair
/// > from whichever left solid **later** — because the point the sweep is
/// > really starting from is the last one that was still inside anything. Get
/// > that backwards and a player standing in a doorway is teleported to the
/// > near edge of the door instead of the far one.
fn clip_trace_to_trace(clip: &Trace, final_trace: &mut Trace) -> bool {
    if clip.all_solid || clip.start_solid || clip.fraction < final_trace.fraction {
        if final_trace.start_solid {
            let fraction_left_solid = final_trace.fraction_left_solid;
            let start = final_trace.start;

            *final_trace = *clip;
            final_trace.start_solid = true;

            if fraction_left_solid > clip.fraction_left_solid {
                final_trace.fraction_left_solid = fraction_left_solid;
                final_trace.start = start;
            }
        } else {
            *final_trace = *clip;
        }
        return true;
    }

    if clip.start_solid {
        final_trace.start_solid = true;
        if clip.fraction_left_solid > final_trace.fraction_left_solid {
            final_trace.fraction_left_solid = clip.fraction_left_solid;
            final_trace.start = clip.start;
        }
    }
    false
}

impl CollisionBsp {
    /// The contents at a point — `CM_PointContents`
    /// (`engine/cmodel.cpp:719`).
    ///
    /// The OR of every brush in the point's leaf that actually contains it,
    /// which is not the same as the leaf's own `contents`: a leaf's is the OR
    /// of every brush *touching* it.
    ///
    /// Takes `&self` rather than `&mut Tracer` because it visits each brush at
    /// most once by construction — there is one leaf, so there is no
    /// deduplication to do.
    pub fn point_contents(&self, point: Vec3) -> Contents {
        if self.nodes.is_empty() || !self.all_contents.intersects(Contents::MASK_ALL) {
            return Contents::EMPTY;
        }

        let leaf_index = self.leaf(point);
        let leaf = self.leaves[leaf_index];
        // `if (leaf.cluster < 0) return leaf.contents;` — a solid leaf is not
        // in the PVS and has no brushes worth testing, so its own contents are
        // the answer. Testing `num_leaf_brushes` instead looks equivalent and
        // is not: an *empty* leaf with no brushes near it would then report the
        // leaf's contents rather than nothing.
        if leaf.cluster < 0 {
            return leaf.contents;
        }

        let first = leaf.first_leaf_brush as usize;
        let count = leaf.num_leaf_brushes as usize;
        let mut contents = Contents::EMPTY;

        for i in first..first + count {
            let brush = self.brushes[self.leaf_brushes[i] as usize];
            if brush.contents.is_empty() {
                continue;
            }
            let inside = match brush.sides {
                model::BrushSides::Box(index) => {
                    let b = self.box_brushes[index as usize];
                    (0..3).all(|a| point[a] >= b.mins[a] && point[a] <= b.maxs[a])
                }
                model::BrushSides::Planes { first, count } => self.brush_sides
                    [first as usize..(first + count) as usize]
                    .iter()
                    // Bevels are unnecessary for testing points.
                    .filter(|side| !side.bevel)
                    .all(|side| {
                        let plane = &self.planes[side.plane as usize];
                        plane.normal.dot(point) - plane.dist <= 0.0
                    }),
            };
            if inside {
                contents = contents.or(brush.contents);
            }
        }
        contents
    }

    /// The leaf containing a point — `CM_PointLeafnum_r`
    /// (`engine/cmodel.cpp:444`). Leaf 0 when there is no tree.
    pub fn leaf(&self, point: Vec3) -> usize {
        if self.nodes.is_empty() {
            return 0;
        }
        let mut num = 0i32;
        while num >= 0 {
            let node = self.nodes[num as usize];
            let plane = &self.planes[node.plane as usize];
            let d = match plane.axis {
                Some(axis) => point[axis] - plane.dist,
                None => plane.normal.dot(point) - plane.dist,
            };
            num = if d < 0.0 {
                node.children[1]
            } else {
                node.children[0]
            };
        }
        (-1 - num) as usize
    }

    /// Every leaf the axis-aligned box `mins`..`maxs` reaches, under
    /// `head_node` — `CM_BoxLeafnums` (`engine/cmodel.cpp:552`).
    ///
    /// The box form of [`leaf`](CollisionBsp::leaf), and the same descent with
    /// one difference: a plane the box straddles is descended on **both**
    /// sides rather than one. The straddle test is the box's radius along the
    /// plane normal, `|normal| · extents`, which is the support function of an
    /// axis-aligned box and so exact rather than conservative.
    ///
    /// No leaf can appear twice — the tree partitions space, so a second visit
    /// would mean two nodes claiming the same volume. That is what lets
    /// [`brushes_in_box`](Tracer::brushes_in_box) dedupe brushes and nothing
    /// else.
    ///
    /// The pending list is read front-to-back rather than as a stack, which
    /// is not a detail: it is what makes the leaves come out in Valve's order,
    /// and so what makes [`brushes_in_box`](Tracer::brushes_in_box)'s
    /// documented ordering the shipped engine's. Valve's is a ring buffer of
    /// 1,024 with an assert on overflow, and it also carries a `topnode` out
    /// for the PVS; a `Vec` read with a cursor has neither limit, and no
    /// caller wants the top node yet.
    fn leaves_in_box(&self, mins: Vec3, maxs: Vec3, head_node: i32) -> Vec<usize> {
        // A map with no collision tree has no leaves worth naming. `leaf`
        // answers 0 instead, because a point is always *somewhere*; a box that
        // touches nothing is properly empty.
        if self.nodes.is_empty() {
            return Vec::new();
        }

        let center = (mins + maxs) * 0.5;
        let extents = maxs - center;

        let mut leaves = Vec::new();
        let mut pending = vec![head_node];
        let mut next = 0;
        while next < pending.len() {
            let mut num = pending[next];
            next += 1;
            while num >= 0 {
                let node = self.nodes[num as usize];
                let plane = &self.planes[node.plane as usize];
                let (distance, radius) = match plane.axis {
                    Some(axis) => (center[axis] - plane.dist, extents[axis]),
                    None => (
                        plane.normal.dot(center) - plane.dist,
                        plane.normal.abs().dot(extents),
                    ),
                };
                num = if distance >= radius {
                    node.children[0]
                } else if distance < -radius {
                    node.children[1]
                } else {
                    pending.push(node.children[0]);
                    node.children[1]
                };
            }
            leaves.push((-1 - num) as usize);
        }
        leaves
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::Fixture;
    use super::*;
    use crate::engine::world::bsp::BrushSide;

    /// The player hull (`portal_mp_gamerules.cpp:173`), which is what stage 4
    /// will sweep and so what these test with.
    const HULL_MIN: Vec3 = Vec3::new(-16.0, -16.0, 0.0);
    const HULL_MAX: Vec3 = Vec3::new(16.0, 16.0, 72.0);

    /// A wall filling `x` 100..200, tall and wide enough for a hull to meet it.
    fn wall(axial: bool) -> CollisionBsp {
        let mut fixture = Fixture::default();
        fixture.add_box(
            Vec3::new(100.0, -500.0, -500.0),
            Vec3::new(200.0, 500.0, 500.0),
            Contents::SOLID,
            axial,
        );
        fixture.single_leaf()
    }

    /// A [`PropQuery`] that reports one axis-aligned box, so that the clip
    /// chain can be tested without a physics world.
    struct OneProp {
        mins: Vec3,
        maxs: Vec3,
    }

    impl PropQuery for OneProp {
        fn sweep(&self, half: Vec3, start: Vec3, end: Vec3) -> Option<PropHit> {
            // A slab test of the swept box against the prop's box, expanded by
            // the sweeper's half-extents. Enough for a test and nothing more —
            // the real one is `Environment::sweep_box`.
            let mins = self.mins - half;
            let maxs = self.maxs + half;
            let delta = end - start;
            let (mut enter, mut exit) = (0.0f32, 1.0f32);
            for axis in 0..3 {
                if delta[axis].abs() < 1e-6 {
                    if start[axis] < mins[axis] || start[axis] > maxs[axis] {
                        return None;
                    }
                    continue;
                }
                let inv = 1.0 / delta[axis];
                let (mut t0, mut t1) = (
                    (mins[axis] - start[axis]) * inv,
                    (maxs[axis] - start[axis]) * inv,
                );
                if t0 > t1 {
                    std::mem::swap(&mut t0, &mut t1);
                }
                enter = enter.max(t0);
                exit = exit.min(t1);
            }
            if enter > exit {
                return None;
            }
            let start_solid = enter <= 0.0;
            // The face the sweep entered through, as an outward normal.
            let mut normal = Vec3::ZERO;
            if !start_solid {
                for axis in 0..3 {
                    if delta[axis].abs() < 1e-6 {
                        continue;
                    }
                    let inv = 1.0 / delta[axis];
                    let t0 = (mins[axis] - start[axis]) * inv;
                    let t1 = (maxs[axis] - start[axis]) * inv;
                    if (t0.min(t1) - enter).abs() < 1e-6 {
                        normal[axis] = match delta[axis] > 0.0 {
                            true => -1.0,
                            false => 1.0,
                        };
                    }
                }
            }
            Some(PropHit {
                fraction: enter.max(0.0),
                normal,
                start_solid,
            })
        }
    }

    /// **A prop stops a sweep that the world would have let through**, which
    /// is the whole of stage 2 of `portdocs/VPHYSICS_SHADOW.md`: the cube is a
    /// wall.
    #[test]
    fn a_prop_in_the_clip_chain_stops_a_sweep_the_world_would_not() {
        let world = wall(true);
        let prop = OneProp {
            mins: Vec3::new(40.0, -16.0, -16.0),
            maxs: Vec3::new(72.0, 16.0, 16.0),
        };
        let ray = Ray::line(Vec3::ZERO, Vec3::new(200.0, 0.0, 0.0));

        let without = world.tracer().trace(&ray, Contents::MASK_PLAYERSOLID);
        assert!((without.end.x - (100.0 - DIST_EPSILON)).abs() < 1e-3, "{without:?}");
        assert!(!without.hit_prop);

        let with = world
            .tracer()
            .with_props(&prop)
            .trace(&ray, Contents::MASK_PLAYERSOLID);
        // The prop's near face is at x = 40, and the prop sweep takes the same
        // `DIST_EPSILON` off that the brush sweep takes off a plane.
        assert!((with.end.x - (40.0 - DIST_EPSILON)).abs() < 1e-3, "{with:?}");
        assert_eq!(with.normal, Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(with.contents, Contents::SOLID);
        assert!(with.hit_prop, "the flag `TouchedPhysics()` is derived from");
    }

    /// …and it does **not** stop one the world already stopped earlier. The
    /// merge is `ClipTraceToTrace`, so the nearer answer wins whichever source
    /// it came from.
    #[test]
    fn a_prop_behind_a_wall_changes_nothing() {
        let world = wall(true);
        let prop = OneProp {
            mins: Vec3::new(300.0, -16.0, -16.0),
            maxs: Vec3::new(332.0, 16.0, 16.0),
        };
        let ray = Ray::line(Vec3::ZERO, Vec3::new(400.0, 0.0, 0.0));
        let hit = world
            .tracer()
            .with_props(&prop)
            .trace(&ray, Contents::MASK_PLAYERSOLID);
        assert!((hit.end.x - (100.0 - DIST_EPSILON)).abs() < 1e-3, "{hit:?}");
        assert!(!hit.hit_prop, "the wall won, so this is not a prop contact");
    }

    /// **A mask without `CONTENTS_SOLID` never asks the props at all** — a
    /// light trace, a paint trace. The prop would otherwise report itself as
    /// solid to a query that was not about solidity.
    #[test]
    fn a_mask_without_solid_does_not_see_props() {
        let world = wall(true);
        let prop = OneProp {
            mins: Vec3::new(40.0, -16.0, -16.0),
            maxs: Vec3::new(72.0, 16.0, 16.0),
        };
        let ray = Ray::line(Vec3::ZERO, Vec3::new(200.0, 0.0, 0.0));
        let hit = world
            .tracer()
            .with_props(&prop)
            .trace(&ray, Contents::WATER);
        assert_eq!(hit.fraction, 1.0, "nothing in this fixture is water");
        assert!(!hit.hit_prop);
    }

    /// A sweep that begins inside a prop reports `startsolid` with a fraction
    /// of zero — and **not** `allsolid`, which would stop the rest of the clip
    /// chain from being asked at all.
    #[test]
    fn a_sweep_starting_inside_a_prop_is_start_solid_but_not_all_solid() {
        let world = wall(true);
        let prop = OneProp {
            mins: Vec3::new(-16.0, -16.0, -16.0),
            maxs: Vec3::new(16.0, 16.0, 16.0),
        };
        let ray = Ray::line(Vec3::ZERO, Vec3::new(200.0, 0.0, 0.0));
        let hit = world
            .tracer()
            .with_props(&prop)
            .trace(&ray, Contents::MASK_PLAYERSOLID);
        assert!(hit.start_solid);
        assert!(!hit.all_solid);
        assert_eq!(hit.fraction, 0.0);
        assert!(hit.hit_prop);
    }

    #[test]
    fn a_ray_stops_dist_epsilon_short_of_the_surface() {
        let world = wall(true);
        let ray = Ray::line(Vec3::ZERO, Vec3::new(200.0, 0.0, 0.0));
        let hit = world.tracer().trace(&ray, Contents::MASK_SOLID);

        assert!(hit.did_hit());
        // The wall's near face is at x = 100; the trace stops 1/32 before it.
        assert!((hit.end.x - (100.0 - DIST_EPSILON)).abs() < 1e-3, "{hit:?}");
        assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(hit.contents, Contents::SOLID);
        assert!(!hit.start_solid && !hit.all_solid);
    }

    /// The box path and the plane path are the same geometry reached two ways,
    /// and they disagree at a corner if either is ported wrong.
    #[test]
    fn a_box_brush_answers_the_same_as_its_six_planes() {
        let boxed = wall(true);
        let planed = wall(false);
        assert_eq!(boxed.box_brushes.len(), 1, "the axial fixture is a box");
        assert_eq!(planed.box_brushes.len(), 0, "the other one is not");

        for (from, to) in [
            (Vec3::ZERO, Vec3::new(200.0, 0.0, 0.0)),
            // A diagonal, so the winning face is not decided by one axis.
            (Vec3::new(0.0, -300.0, 0.0), Vec3::new(300.0, 100.0, 40.0)),
            (Vec3::new(150.0, 0.0, 600.0), Vec3::new(150.0, 0.0, -600.0)),
        ] {
            let ray = Ray::line(from, to);
            let a = boxed.tracer().trace(&ray, Contents::MASK_SOLID);
            let b = planed.tracer().trace(&ray, Contents::MASK_SOLID);
            assert!(
                (a.fraction - b.fraction).abs() < 1e-4 && a.normal == b.normal,
                "box {a:?}\nplanes {b:?}"
            );
        }
    }

    /// The Minkowski expansion: a 32-wide hull stops a half-width earlier than
    /// the ray down its centre.
    #[test]
    fn a_hull_sweep_stops_a_half_width_early() {
        let world = wall(true);
        let ray = Ray::hull(Vec3::ZERO, Vec3::new(200.0, 0.0, 0.0), HULL_MIN, HULL_MAX);
        let hit = world.tracer().trace(&ray, Contents::MASK_SOLID);

        assert!(hit.did_hit());
        assert!(
            (hit.end.x - (100.0 - 16.0 - DIST_EPSILON)).abs() < 1e-3,
            "{hit:?}"
        );
        // The result is in the caller's frame: the feet, not the box centre.
        assert_eq!(hit.end.z, 0.0, "{hit:?}");
    }

    #[test]
    fn a_hull_reports_no_fraction_left_solid() {
        let world = wall(true);
        // Starting inside the wall.
        let ray = Ray::hull(
            Vec3::new(150.0, 0.0, 0.0),
            Vec3::new(400.0, 0.0, 0.0),
            HULL_MIN,
            HULL_MAX,
        );
        let hit = world.tracer().trace(&ray, Contents::MASK_SOLID);

        assert!(hit.start_solid, "{hit:?}");
        assert_eq!(hit.fraction_left_solid, 0.0);
        // ...and `start` is therefore the ray's own start.
        assert_eq!(hit.start, Vec3::new(150.0, 0.0, 0.0));
    }

    #[test]
    fn a_ray_starting_inside_reports_where_it_left() {
        let world = wall(true);
        let ray = Ray::line(Vec3::new(150.0, 0.0, 0.0), Vec3::new(350.0, 0.0, 0.0));
        let hit = world.tracer().trace(&ray, Contents::MASK_SOLID);

        assert!(hit.start_solid && !hit.all_solid, "{hit:?}");
        assert!(hit.fraction_left_solid > 0.0);
        // The far face is at x = 200, a quarter of the way along a 200-long
        // ray from x = 150.
        assert!((hit.start.x - 200.0).abs() < 0.1, "{hit:?}");
        assert!(hit.start.x > 150.0, "start moved to where it left solid");
    }

    #[test]
    fn a_ray_that_never_leaves_a_brush_is_all_solid() {
        let world = wall(true);
        let ray = Ray::line(Vec3::new(120.0, 0.0, 0.0), Vec3::new(180.0, 0.0, 0.0));
        let hit = world.tracer().trace(&ray, Contents::MASK_SOLID);

        assert!(hit.all_solid && hit.start_solid, "{hit:?}");
        assert_eq!(hit.fraction, 0.0);
        assert!(hit.did_hit());
    }

    #[test]
    fn a_mask_that_excludes_the_brush_hits_nothing() {
        let mut fixture = Fixture::default();
        fixture.add_box(
            Vec3::new(100.0, -500.0, -500.0),
            Vec3::new(200.0, 500.0, 500.0),
            Contents::WATER,
            true,
        );
        let world = fixture.single_leaf();

        let ray = Ray::line(Vec3::ZERO, Vec3::new(400.0, 0.0, 0.0));
        let solid = world.tracer().trace(&ray, Contents::MASK_SOLID);
        assert!(!solid.did_hit(), "{solid:?}");
        assert_eq!(solid.fraction, 1.0);
        assert_eq!(solid.end, Vec3::new(400.0, 0.0, 0.0));

        let water = world.tracer().trace(&ray, Contents::MASK_WATER);
        assert!(water.did_hit(), "{water:?}");
    }

    /// Playerclip is the case Portal 2 actually depends on: invisible to a
    /// bullet, solid to a player.
    #[test]
    fn playerclip_stops_a_player_and_not_a_shot() {
        let mut fixture = Fixture::default();
        fixture.add_box(
            Vec3::new(100.0, -500.0, -500.0),
            Vec3::new(200.0, 500.0, 500.0),
            Contents::PLAYERCLIP,
            true,
        );
        let world = fixture.single_leaf();
        let ray = Ray::line(Vec3::ZERO, Vec3::new(400.0, 0.0, 0.0));

        assert!(world
            .tracer()
            .trace(&ray, Contents::MASK_PLAYERSOLID)
            .did_hit());
        assert!(!world.tracer().trace(&ray, Contents::MASK_SOLID).did_hit());
    }

    #[test]
    fn a_zero_length_sweep_is_a_position_test() {
        let world = wall(true);

        let inside = Ray::hull(
            Vec3::new(150.0, 0.0, 0.0),
            Vec3::new(150.0, 0.0, 0.0),
            HULL_MIN,
            HULL_MAX,
        );
        let hit = world.tracer().trace(&inside, Contents::MASK_SOLID);
        assert!(hit.all_solid && hit.start_solid, "{hit:?}");
        assert_eq!(hit.fraction, 0.0);

        let outside = Ray::hull(Vec3::ZERO, Vec3::ZERO, HULL_MIN, HULL_MAX);
        let miss = world.tracer().trace(&outside, Contents::MASK_SOLID);
        assert!(!miss.all_solid, "{miss:?}");
    }

    /// The descent has to reach the *near* brush first, and the far one's leaf
    /// must not overwrite it.
    #[test]
    fn the_tree_descent_keeps_the_nearer_hit() {
        let mut fixture = Fixture::default();
        let back = fixture.add_box(
            Vec3::new(-200.0, -500.0, -500.0),
            Vec3::new(-100.0, 500.0, 500.0),
            Contents::SOLID,
            true,
        );
        let front = fixture.add_box(
            Vec3::new(100.0, -500.0, -500.0),
            Vec3::new(200.0, 500.0, 500.0),
            Contents::SOLID,
            true,
        );
        let world = fixture.split(&[front], &[back]);
        assert_eq!(world.leaves.len(), 2);

        // Left to right: the x = -100 face is the first thing in the way.
        let ray = Ray::line(Vec3::new(-400.0, 0.0, 0.0), Vec3::new(400.0, 0.0, 0.0));
        let hit = world.tracer().trace(&ray, Contents::MASK_SOLID);
        assert!(
            (hit.end.x - (-200.0 - DIST_EPSILON)).abs() < 1e-3,
            "{hit:?}"
        );
        assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));

        // ...and right to left it is the other brush's far face.
        let ray = Ray::line(Vec3::new(400.0, 0.0, 0.0), Vec3::new(-400.0, 0.0, 0.0));
        let hit = world.tracer().trace(&ray, Contents::MASK_SOLID);
        assert!((hit.end.x - (200.0 + DIST_EPSILON)).abs() < 1e-3, "{hit:?}");
        assert_eq!(hit.normal, Vec3::new(1.0, 0.0, 0.0));
    }

    /// A floor, traced downwards — `CategorizePosition`'s question, and the
    /// one stage 4 asks most.
    #[test]
    fn a_downward_hull_sweep_finds_the_floor_normal() {
        let mut fixture = Fixture::default();
        fixture.add_box(
            Vec3::new(-500.0, -500.0, -100.0),
            Vec3::new(500.0, 500.0, 0.0),
            Contents::SOLID,
            true,
        );
        let world = fixture.single_leaf();

        let ray = Ray::hull(
            Vec3::new(0.0, 0.0, 200.0),
            Vec3::new(0.0, 0.0, -50.0),
            HULL_MIN,
            HULL_MAX,
        );
        let hit = world.tracer().trace(&ray, Contents::MASK_PLAYERSOLID);

        assert!(hit.did_hit(), "{hit:?}");
        assert_eq!(hit.normal, Vec3::new(0.0, 0.0, 1.0));
        // `CategorizePosition`'s floor test.
        assert!(hit.normal.z > 0.7);
        // The feet come to rest on the surface, a hair above it.
        assert!((hit.end.z - DIST_EPSILON).abs() < 1e-3, "{hit:?}");
    }

    #[test]
    fn point_contents_and_leaf_lookup() {
        let mut fixture = Fixture::default();
        fixture.add_box(
            Vec3::new(100.0, -500.0, -500.0),
            Vec3::new(200.0, 500.0, 500.0),
            Contents::WATER,
            true,
        );
        // The leaf is inside the water, which is what a real `.bsp` would
        // say and what `all_contents` is built from.
        let world = fixture.single_leaf_with(Contents::WATER);

        assert_eq!(
            world.point_contents(Vec3::new(150.0, 0.0, 0.0)),
            Contents::WATER
        );
        assert_eq!(
            world.point_contents(Vec3::new(0.0, 0.0, 0.0)),
            Contents::EMPTY
        );
        assert_eq!(world.leaf(Vec3::new(150.0, 0.0, 0.0)), 0);
    }

    /// An empty collision model is a map you cannot collide with, not an
    /// error — and the trace still reports where the ray was going.
    #[test]
    fn a_map_with_no_brushes_traces_as_a_clean_miss() {
        let world = Fixture::default().finish();
        assert!(world.is_empty());

        let ray = Ray::line(Vec3::ZERO, Vec3::new(100.0, 0.0, 0.0));
        let hit = world.tracer().trace(&ray, Contents::MASK_ALL);
        assert!(!hit.did_hit());
        assert_eq!(hit.fraction, 1.0);
        assert_eq!(hit.end, Vec3::new(100.0, 0.0, 0.0));
    }

    /// Bevel planes exist so a *box* sweep clips exactly, and a point trace
    /// must not see them at all.
    ///
    /// The plane here is deliberately tighter than the brush it belongs to,
    /// which no compiler would emit — a real bevel is redundant for the volume
    /// and only binds once the volume is expanded, so its effect on a point
    /// trace is invisible by construction. Making it tight is what makes the
    /// skip observable rather than merely believed.
    #[test]
    fn bevel_planes_bind_a_hull_and_are_invisible_to_a_ray() {
        let mut fixture = Fixture::default();
        // Not axial, so it stays a plane brush: a box brush has no sides to
        // mark as bevels.
        fixture.add_box(
            Vec3::new(100.0, -500.0, -500.0),
            Vec3::new(200.0, 500.0, 500.0),
            Contents::SOLID,
            false,
        );
        let plane_num = fixture.plane([-1.0, 0.0, 0.0], -150.0, false);
        fixture.brush_sides.push(BrushSide {
            plane_num,
            tex_info: -1,
            disp_info: -1,
            bevel: 1,
            thin: 0,
        });
        fixture.brushes[0].num_sides = 7;
        let world = fixture.single_leaf();

        let from = Vec3::ZERO;
        let to = Vec3::new(400.0, 0.0, 0.0);

        let ray = world
            .tracer()
            .trace(&Ray::line(from, to), Contents::MASK_SOLID);
        assert!(
            (ray.end.x - (100.0 - DIST_EPSILON)).abs() < 1e-3,
            "the ray ignored the bevel and stopped at the real face: {ray:?}"
        );

        let hull = world.tracer().trace(
            &Ray::hull(from, to, HULL_MIN, HULL_MAX),
            Contents::MASK_SOLID,
        );
        assert!(
            (hull.end.x - (150.0 - 16.0 - DIST_EPSILON)).abs() < 1e-3,
            "the hull was stopped by the bevel: {hull:?}"
        );
    }

    // ---- Stage 2: brush models -------------------------------------------

    /// A world wall at `x` 300..400 and a brush model's wall at `x` 100..200,
    /// in separate subtrees. The model's is *nearer*, so a `trace_model` that
    /// descended from head node 0 would report the world's and be caught.
    fn world_and_model() -> CollisionBsp {
        let mut fixture = Fixture::default();
        let world_wall = fixture.add_box(
            Vec3::new(300.0, -500.0, -500.0),
            Vec3::new(400.0, 500.0, 500.0),
            Contents::SOLID,
            true,
        );
        let model_wall = fixture.add_box(
            Vec3::new(100.0, -500.0, -500.0),
            Vec3::new(200.0, 500.0, 500.0),
            Contents::SOLID,
            true,
        );
        fixture.world_and_model(&[world_wall], &[model_wall])
    }

    /// The whole point of a head node: model 1's brushes are not the world's,
    /// and neither trace can see the other's.
    #[test]
    fn a_brush_model_is_traced_against_its_own_subtree() {
        let world = world_and_model();
        let ray = Ray::line(Vec3::ZERO, Vec3::new(500.0, 0.0, 0.0));
        let model = world
            .brush_model(1, Vec3::ZERO, Vec3::ZERO)
            .expect("model 1");

        // The world subtree holds only the far wall.
        let hit = world.tracer().trace(&ray, Contents::MASK_SOLID);
        assert!((hit.end.x - (300.0 - DIST_EPSILON)).abs() < 1e-3, "{hit:?}");

        // The model subtree holds only the near one. Placed at the origin, so
        // any difference here is the head node and nothing else.
        let hit = world
            .tracer()
            .trace_model(&ray, &model, Contents::MASK_SOLID);
        assert!((hit.end.x - (100.0 - DIST_EPSILON)).abs() < 1e-3, "{hit:?}");
        assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));
    }

    /// An unrotated model is `CM_TransformedBoxTrace`'s cheap branch: a
    /// subtraction, and the hit moves with the placement.
    #[test]
    fn an_unrotated_brush_model_moves_with_its_origin() {
        let world = world_and_model();
        let ray = Ray::line(Vec3::ZERO, Vec3::new(500.0, 0.0, 0.0));

        for shift in [0.0, 50.0, -60.0] {
            let model = world
                .brush_model(1, Vec3::new(shift, 0.0, 0.0), Vec3::ZERO)
                .expect("model 1");
            let hit = world
                .tracer()
                .trace_model(&ray, &model, Contents::MASK_SOLID);
            assert!(
                (hit.end.x - (100.0 + shift - DIST_EPSILON)).abs() < 1e-3,
                "shifted by {shift}: {hit:?}"
            );
            assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));
        }
    }

    /// Model 0 *is* the world, so naming it has to give the world trace back
    /// unchanged — the same field for field, not merely the same distance.
    #[test]
    fn model_zero_is_the_world() {
        let world = world_and_model();
        let ray = Ray::line(Vec3::ZERO, Vec3::new(500.0, 0.0, 0.0));
        let model = world
            .brush_model(0, Vec3::ZERO, Vec3::ZERO)
            .expect("model 0");

        assert_eq!(
            world
                .tracer()
                .trace_model(&ray, &model, Contents::MASK_SOLID),
            world.tracer().trace(&ray, Contents::MASK_SOLID),
        );
    }

    #[test]
    fn a_model_the_map_does_not_have_is_none() {
        let world = world_and_model();
        assert!(world.brush_model(2, Vec3::ZERO, Vec3::ZERO).is_none());
        assert!(world.brush_model(1, Vec3::ZERO, Vec3::ZERO).is_some());
    }

    /// A yaw of 90° turns the model's `+X` onto the world's `+Y`, so the wall
    /// moves and **the normal has to come back out in world space**. Without
    /// the rotate-back it would still point along `-X` and every consumer
    /// would slide the wrong way along the wall.
    #[test]
    fn a_rotated_brush_model_reports_a_world_space_normal() {
        let world = world_and_model();
        let model = world
            .brush_model(1, Vec3::ZERO, Vec3::new(0.0, 90.0, 0.0))
            .expect("model 1");

        // Down +Y now, because the wall has turned to face it.
        let ray = Ray::line(Vec3::ZERO, Vec3::new(0.0, 400.0, 0.0));
        let hit = world
            .tracer()
            .trace_model(&ray, &model, Contents::MASK_SOLID);

        assert!(hit.did_hit(), "{hit:?}");
        assert!((hit.end.y - (100.0 - DIST_EPSILON)).abs() < 1e-3, "{hit:?}");
        assert!(
            (hit.normal - Vec3::new(0.0, -1.0, 0.0)).length() < 1e-5,
            "{hit:?}"
        );

        // ...and the ray that used to hit it now misses entirely.
        let along_x = Ray::line(Vec3::ZERO, Vec3::new(400.0, 0.0, 0.0));
        let miss = world
            .tracer()
            .trace_model(&along_x, &model, Contents::MASK_SOLID);
        assert!(!miss.did_hit(), "{miss:?}");
    }

    /// Turn the model and the question by the same rotation and the answer
    /// must turn with them — the invariant that pins `VectorITransform`'s
    /// order, both its transposes and the sign of the translation.
    ///
    /// A **ray**, not a hull: a swept box is deliberately *not* rotated into
    /// the model's frame (see [`BrushModel::local_ray`]), so the box case does
    /// not have this symmetry and asserting it would be asserting a bug.
    #[test]
    fn rotating_the_model_and_the_query_together_rotates_the_answer() {
        let world = world_and_model();
        let angles = Vec3::new(30.0, 45.0, 60.0);
        let rotation = crate::math::angle_matrix(angles);
        let origin = Vec3::new(10.0, 20.0, 30.0);

        let from = Vec3::new(-50.0, 5.0, -7.0);
        let to = from + Vec3::new(400.0, 0.0, 0.0);

        let plain = world.tracer().trace_model(
            &Ray::line(from, to),
            &world.brush_model(1, origin, Vec3::ZERO).expect("model 1"),
            Contents::MASK_SOLID,
        );
        let turned = world.tracer().trace_model(
            &Ray::line(rotation * from, rotation * to),
            // The placement rotates about the world origin too, so the model
            // ends up where the rotated geometry would be.
            &world
                .brush_model(1, rotation * origin, angles)
                .expect("model 1"),
            Contents::MASK_SOLID,
        );

        assert!(plain.did_hit() && turned.did_hit(), "{plain:?} {turned:?}");
        assert!(
            (plain.fraction - turned.fraction).abs() < 1e-6,
            "{plain:?} {turned:?}"
        );
        assert!(
            (turned.normal - rotation * plain.normal).length() < 1e-5,
            "{plain:?} {turned:?}"
        );
        assert!(
            (turned.end - rotation * plain.end).length() < 1e-3,
            "{plain:?} {turned:?}"
        );
    }

    /// The centring dance in the rotated branch: Valve transforms the
    /// *caller's* start and re-applies the box centre afterwards.
    ///
    /// A player hull's centre is 36 units above its feet, so dropping that
    /// re-application lands the player 36 units out — which is the whole
    /// reason `CM_TransformedBoxTrace` does not simply transform `m_Start`.
    #[test]
    fn a_rotated_model_keeps_the_hulls_centring() {
        let mut fixture = Fixture::default();
        let floor = fixture.add_box(
            Vec3::new(-500.0, -500.0, -100.0),
            Vec3::new(500.0, 500.0, 0.0),
            Contents::SOLID,
            true,
        );
        // No world brushes at all, so only the model can stop the fall.
        let world = fixture.world_and_model(&[], &[floor]);

        // Raised 64 units, and yawed — which leaves a floor a floor, so the
        // expected answer is known exactly and the rotated path still runs.
        let model = world
            .brush_model(1, Vec3::new(0.0, 0.0, 64.0), Vec3::new(0.0, 90.0, 0.0))
            .expect("model 1");

        let ray = Ray::hull(
            Vec3::new(0.0, 0.0, 200.0),
            Vec3::new(0.0, 0.0, 0.0),
            HULL_MIN,
            HULL_MAX,
        );
        let hit = world
            .tracer()
            .trace_model(&ray, &model, Contents::MASK_PLAYERSOLID);

        assert!(hit.did_hit(), "{hit:?}");
        assert!((hit.normal - Vec3::Z).length() < 1e-5, "{hit:?}");
        // The feet come to rest on the raised floor, a hair above it. Read
        // the centre instead of the feet and this is 36 units out.
        assert!((hit.end.z - (64.0 + DIST_EPSILON)).abs() < 1e-3, "{hit:?}");
        // The caller's frame, not the centred one.
        assert_eq!(hit.start, Vec3::new(0.0, 0.0, 200.0));
    }

    /// The swept box is **not** turned into the model's frame — Valve copies
    /// `m_Extents` across untouched — so a hull meets a rotated model with a
    /// box that is axis aligned in the *model's* space.
    ///
    /// Pinned with a deliberately oblong box, because a cube (and the player
    /// hull, which is square in `x`/`y`) hides the difference under any yaw.
    #[test]
    fn a_hull_is_not_rotated_into_the_models_frame() {
        let world = world_and_model();
        let model = world
            .brush_model(1, Vec3::ZERO, Vec3::new(0.0, 45.0, 0.0))
            .expect("model 1");

        // 80 long in x, 16 in y: the two axes give different answers, so
        // which frame the box lives in is observable.
        let (mins, maxs) = (Vec3::new(-40.0, -8.0, -4.0), Vec3::new(40.0, 8.0, 4.0));
        // Straight down the model's local +X, which a 45° yaw points here.
        let direction = crate::math::angle_matrix(Vec3::new(0.0, 45.0, 0.0)) * Vec3::X;
        let ray = Ray::hull(Vec3::ZERO, direction * 400.0, mins, maxs);
        let hit = world
            .tracer()
            .trace_model(&ray, &model, Contents::MASK_SOLID);

        assert!(hit.did_hit(), "{hit:?}");
        // The wall's near face is 100 along the model's +X and the box's
        // half-extent *on that axis* is 40 — so 60, less the epsilon. Rotate
        // the box into world space instead and the half-extent along the
        // sweep becomes 24√2 ≈ 33.9, which is a different answer.
        assert!(
            (hit.end.length() - (60.0 - DIST_EPSILON)).abs() < 1e-2,
            "stopped {} from the start: {hit:?}",
            hit.end.length()
        );
    }

    /// `plane_dist` is left in the model's own frame, where `normal` is
    /// rotated out of it. Valve's asymmetry, ported as written — this test
    /// exists so that nobody "fixes" it without meaning to.
    #[test]
    fn a_brush_models_plane_dist_stays_in_its_own_frame() {
        let world = world_and_model();
        let model = world
            .brush_model(1, Vec3::new(50.0, 0.0, 0.0), Vec3::ZERO)
            .expect("model 1");
        let ray = Ray::line(Vec3::ZERO, Vec3::new(400.0, 0.0, 0.0));
        let hit = world
            .tracer()
            .trace_model(&ray, &model, Contents::MASK_SOLID);

        assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));
        // The face is at world x = 150 and at model x = 100; the plane this
        // reports is the model's.
        assert!((hit.plane_dist - -100.0).abs() < 1e-3, "{hit:?}");
    }

    /// A zero-length sweep against a brush model is a position test, and it
    /// has to reach `CM_UnsweptBoxTrace` with the *model's* head node — the
    /// one path that gathers leaves rather than descending with a direction.
    #[test]
    fn a_position_test_against_a_brush_model_finds_it() {
        let world = world_and_model();
        let model = world
            .brush_model(1, Vec3::ZERO, Vec3::ZERO)
            .expect("model 1");

        let inside = Vec3::new(150.0, 0.0, 0.0);
        let hit = world.tracer().trace_model(
            &Ray::hull(inside, inside, HULL_MIN, HULL_MAX),
            &model,
            Contents::MASK_SOLID,
        );
        assert!(hit.all_solid && hit.start_solid, "{hit:?}");

        // ...and the world's own wall, at x 300..400, is not this model's.
        let outside = Vec3::new(350.0, 0.0, 0.0);
        let miss = world.tracer().trace_model(
            &Ray::hull(outside, outside, HULL_MIN, HULL_MAX),
            &model,
            Contents::MASK_SOLID,
        );
        assert!(!miss.all_solid, "{miss:?}");
    }

    /// `model_to_world` and `local_ray` are inverses, which is the property
    /// that stops a brush model being *drawn* somewhere other than where it is
    /// *collided with*.
    ///
    /// Both read the same two fields, so this cannot drift by accident — but it
    /// could drift by edit, and a door you can see a foot to the left of the
    /// one you walk into is a bug nobody would look for here.
    #[test]
    fn the_render_transform_inverts_the_trace_transform() {
        let world = world_and_model();
        for (origin, angles) in [
            (Vec3::ZERO, Vec3::ZERO),
            (Vec3::new(10.0, -20.0, 30.0), Vec3::ZERO),
            (Vec3::new(10.0, -20.0, 30.0), Vec3::new(0.0, 90.0, 0.0)),
            (Vec3::new(-400.0, 55.0, 7.0), Vec3::new(30.0, 45.0, 60.0)),
        ] {
            let model = world.brush_model(1, origin, angles).expect("model 1");
            let to_world = model.model_to_world();

            // A ray's start is not centred, so its local start is exactly the
            // world point carried through the inverse transform.
            let point = Vec3::new(120.0, 8.0, -3.0);
            let local = model.local_ray(&Ray::line(point, point + Vec3::X));
            let expected = to_world.inverse().transform_point3(point);
            assert!(
                (local.start - expected).length() < 1e-3,
                "{origin} {angles}: local {} vs {expected}",
                local.start,
            );

            // ...and a direction rotates back the same way the normal does.
            assert!(
                (to_world.transform_vector3(local.delta) - Vec3::X).length() < 1e-4,
                "{origin} {angles}: delta {}",
                local.delta,
            );
        }
    }

    // ---- Stage 4: the clip chain -----------------------------------------

    /// `trace` with entities is the world *and* the brush models, nearest
    /// wins — and the fraction it reports is against the original ray, not
    /// against the shortened one the entities were swept with.
    #[test]
    fn the_clip_chain_keeps_the_nearest_of_the_world_and_the_entities() {
        let world = world_and_model();
        let ray = Ray::line(Vec3::ZERO, Vec3::new(500.0, 0.0, 0.0));
        let near = world
            .brush_model(1, Vec3::ZERO, Vec3::ZERO)
            .expect("model 1");
        // The same model, pushed past the world's wall.
        let far = world
            .brush_model(1, Vec3::new(300.0, 0.0, 0.0), Vec3::ZERO)
            .expect("model 1");

        // Nothing in the chain: the world's wall at 300.
        let hit = world.tracer().trace(&ray, Contents::MASK_SOLID);
        assert!((hit.end.x - (300.0 - DIST_EPSILON)).abs() < 1e-3, "{hit:?}");

        // A model in front of it wins…
        let chain = [near];
        let hit = world
            .tracer()
            .with_entities(&chain)
            .trace(&ray, Contents::MASK_SOLID);
        assert!((hit.end.x - (100.0 - DIST_EPSILON)).abs() < 1e-3, "{hit:?}");
        // …and the fraction is against the whole 500-unit ray, which is the
        // rescaling at the end of `TraceRay`.
        assert!((hit.fraction - (100.0 - DIST_EPSILON) / 500.0).abs() < 1e-4);

        // A model *behind* it does not, and — the point of shortening the ray
        // — is not even reached.
        let chain = [far];
        let hit = world
            .tracer()
            .with_entities(&chain)
            .trace(&ray, Contents::MASK_SOLID);
        assert!((hit.end.x - (300.0 - DIST_EPSILON)).abs() < 1e-3, "{hit:?}");

        // Both: the nearest of the three.
        let chain = [far, near];
        let hit = world
            .tracer()
            .with_entities(&chain)
            .trace(&ray, Contents::MASK_SOLID);
        assert!((hit.end.x - (100.0 - DIST_EPSILON)).abs() < 1e-3, "{hit:?}");
    }

    /// The clip chain is opt-in: `collision.tracer()` is world-only, which is
    /// what every caller before stage 4 asked for and still gets.
    #[test]
    fn a_tracer_with_no_entities_is_a_world_trace() {
        let world = world_and_model();
        let ray = Ray::line(Vec3::ZERO, Vec3::new(500.0, 0.0, 0.0));
        let plain = world.tracer().trace(&ray, Contents::MASK_SOLID);
        let world_only = world.tracer().trace_world(&ray, Contents::MASK_SOLID);
        assert_eq!(plain, world_only);
    }

    /// A trace that begins inside the world never looks at an entity —
    /// "inside world, no need to check being inside anything else".
    #[test]
    fn a_trace_that_starts_in_the_world_ignores_the_chain() {
        let world = world_and_model();
        let near = world
            .brush_model(1, Vec3::ZERO, Vec3::ZERO)
            .expect("model 1");
        // Start inside the world's wall.
        let ray = Ray::line(Vec3::new(350.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0));
        let chain = [near];
        let hit = world
            .tracer()
            .with_entities(&chain)
            .trace(&ray, Contents::MASK_SOLID);
        assert!(hit.start_solid);
        assert_eq!(
            hit,
            world.tracer().trace_world(&ray, Contents::MASK_SOLID),
            "the world's answer, returned before the chain was walked"
        );
    }

    /// Every shipped map's brush entities, resolved and swept against for
    /// real — `portdocs/ENGINE_TRACE.md` stage 2's verification.
    ///
    /// Ignored by default and gated on `KISAK_GAME_DIR`, the same as
    /// `studio::tests::every_shipped_studio_model_parses`. Run with:
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_map_brush_models -- --ignored --nocapture
    /// ```
    ///
    /// The assertion that earns the runtime is the **bounds check**: a brush
    /// model's geometry lives in its own frame, and a hit is required to land
    /// inside that frame's box transformed out to world space. A wrong head
    /// node reports some *other* model's geometry, a dropped origin reports it
    /// at the wrong place, and an inverted rotation reports it turned the
    /// wrong way — all three land outside the box, and none of them is visible
    /// to a test that only checks that a trace returned.
    ///
    /// It reaches up into `world/` for the entity-lump resolution, which is
    /// where that code has to live (it needs the `.bsp`, which `trace/` never
    /// sees). Testing the real resolver beats testing a copy of it.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_map_traces_its_brush_models() {
        use crate::engine::world::bsp::Bsp;

        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = crate::filesystem::Vfs::mount_game(&dir, &base, &Default::default())
            .expect("mount the game");

        let mut names: Vec<String> = vfs
            .list("maps")
            .expect("maps/")
            .into_iter()
            .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
            .map(|e| e.name.trim_end_matches(".bsp").to_owned())
            .collect();
        names.sort();
        assert!(names.len() > 50, "only {} maps found", names.len());

        let (mut maps, mut total, mut rotated, mut translated) = (0, 0usize, 0usize, 0usize);
        let (mut traced, mut hits) = (0usize, 0usize);
        let mut classnames: std::collections::BTreeMap<String, usize> = Default::default();

        for name in &names {
            let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
            let collision = CollisionBsp::build(&bsp);
            let entities = bsp.entities();
            let placed = crate::engine::world::find_brush_models(&entities, &collision);
            maps += 1;
            total += placed.len();

            let mut tracer = collision.tracer();
            for model in &placed {
                *classnames.entry(model.classname.clone()).or_default() += 1;
                assert!(
                    model.index < bsp.models.len(),
                    "{name}: model *{} of {}",
                    model.index,
                    bsp.models.len()
                );
                if model.model.rotation.is_some() {
                    rotated += 1;
                }
                if model.model.origin != Vec3::ZERO {
                    translated += 1;
                }

                // The model's own box, carried out to world space: every
                // corner transformed, then re-bounded.
                let lump = &bsp.models[model.index];
                let (lo, hi) = (Vec3::from(lump.mins), Vec3::from(lump.maxs));
                if !lo.is_finite() || !hi.is_finite() || lo.cmpgt(hi).any() {
                    continue; // a degenerate model has no box to check against
                }
                let (mut mins, mut maxs) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                for i in 0..8 {
                    let corner = Vec3::new(
                        if i & 1 == 0 { lo.x } else { hi.x },
                        if i & 2 == 0 { lo.y } else { hi.y },
                        if i & 4 == 0 { lo.z } else { hi.z },
                    );
                    let world = match model.model.rotation {
                        Some(r) => r * corner + model.model.origin,
                        None => corner + model.model.origin,
                    };
                    mins = mins.min(world);
                    maxs = maxs.max(world);
                }

                // Straight down through the middle of that box, from well
                // above it to well below.
                let centre = (mins + maxs) * 0.5;
                let drop = (maxs.z - mins.z).max(1.0) + 64.0;
                let ray = Ray::line(
                    Vec3::new(centre.x, centre.y, maxs.z + drop),
                    Vec3::new(centre.x, centre.y, mins.z - drop),
                );
                let hit = tracer.trace_model(&ray, &model.model, Contents::MASK_SOLID);
                traced += 1;
                assert!(
                    hit.fraction.is_finite() && (0.0..=1.0).contains(&hit.fraction),
                    "{name}: *{} fraction {}",
                    model.index,
                    hit.fraction
                );
                if !hit.did_hit() {
                    continue;
                }
                hits += 1;
                // One unit of slack, which is `DIST_EPSILON` (1/32) with room
                // to spare, for the epsilon and for `f32` on a 16k map.
                let slack = Vec3::ONE;
                assert!(
                    hit.end.cmpge(mins - slack).all() && hit.end.cmple(maxs + slack).all(),
                    "{name}: *{} \"{}\" hit {} outside its own box {mins}..{maxs}",
                    model.index,
                    model.classname,
                    hit.end,
                );
            }
        }

        let mut common: Vec<_> = classnames.iter().collect();
        common.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        println!(
            "{maps} maps, {total} brush models ({rotated} rotated, {translated} \
             off the origin), {} distinct classnames; {traced} traced, {hits} hit.\n  \
             most common: {:?}",
            classnames.len(),
            &common[..common.len().min(8)],
        );
        assert!(total > 1000, "only {total} brush models across {maps} maps");
    }

    /// A tracer is reusable, and the visit stamps must not leak between
    /// traces — a brush skipped as "already seen" would make the second trace
    /// miss it entirely.
    #[test]
    fn a_tracer_gives_the_same_answer_twice() {
        let world = wall(true);
        let mut tracer = world.tracer();
        let ray = Ray::line(Vec3::ZERO, Vec3::new(200.0, 0.0, 0.0));

        let first = tracer.trace(&ray, Contents::MASK_SOLID);
        for _ in 0..4 {
            assert_eq!(tracer.trace(&ray, Contents::MASK_SOLID), first);
        }
    }

    // ---- Stage 3: displacements ------------------------------------------

    /// The base quad of every displacement fixture below: 256 units square in
    /// the `z = 0` plane, wound so that `(p3 - p0) × (p1 - p0)` — which is what
    /// every triangle's normal comes out along — points **`+Z`**.
    ///
    /// The winding is the thing to get right and the easiest to get wrong: the
    /// obvious counter-clockwise order gives `-Z`, and terrain whose normals
    /// point down is terrain you fall through in one direction and cannot
    /// leave in the other.
    ///
    /// Grid coordinates: `i` runs `p0 → p1`, which is `+Y` here, and `j` runs
    /// across to the `p3 → p2` edge, which is `+X`. So `height(i, j)` raises
    /// the point at world `(256 j / n, 256 i / n)`.
    const QUAD: [Vec3; 4] = [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(0.0, 256.0, 0.0),
        Vec3::new(256.0, 256.0, 0.0),
        Vec3::new(256.0, 0.0, 0.0),
    ];

    /// A flat displacement over [`QUAD`], and nothing else in the map.
    fn terrain(contents: Contents, flags: u32) -> CollisionBsp {
        terrain_shaped(contents, flags, |_, _| 0.0)
    }

    fn terrain_shaped(
        contents: Contents,
        flags: u32,
        height: impl Fn(usize, usize) -> f32,
    ) -> CollisionBsp {
        let mut fixture = Fixture::default();
        fixture.add_displacement(QUAD, QUAD[0], 2, contents, flags, height);
        fixture.single_leaf()
    }

    /// Straight down the middle of a flat patch.
    fn down(from: Vec3) -> Ray {
        Ray::line(from, from - Vec3::Z * 200.0)
    }

    #[test]
    fn a_displacement_is_built_and_reachable_from_its_leaf() {
        let world = terrain(Contents::SOLID, 0);
        assert_eq!(world.disp_count(), 1);
        assert!(!world.leaf_disps.is_empty(), "the leaf list names it");
    }

    /// The headline: terrain stops a ray, reports the surface normal, and says
    /// it was terrain.
    #[test]
    fn a_ray_stops_on_a_displacement() {
        let world = terrain(Contents::SOLID, 0);
        let hit = world
            .tracer()
            .trace(&down(Vec3::new(128.0, 128.0, 100.0)), Contents::MASK_SOLID);

        assert!(hit.did_hit(), "{hit:?}");
        assert!((hit.normal - Vec3::Z).length() < 1e-5, "{hit:?}");
        // **A ray stops *on* a displacement, not `DIST_EPSILON` short of it.**
        // `IntersectRayWithTriangle` has no epsilon pullback where
        // `clip_box_to_brush` does — Valve's asymmetry, and the reason a ray's
        // impact point on terrain and on a wall are not directly comparable.
        assert!(hit.end.z.abs() < 1e-3, "{hit:?}");
        assert_eq!(hit.contents, Contents::SOLID);
        assert!(
            hit.disp_flags & disp_surf::SURFACE != 0,
            "a terrain hit says so: {hit:?}"
        );
        assert_eq!(world.surface_name(hit.surface), "nature/test_displacement");
    }

    /// A hull *does* stop short, because the sweep path resolves its planes
    /// through `ResolveRayPlaneIntersect`, which has the epsilon.
    #[test]
    fn a_hull_stops_a_hair_above_a_displacement() {
        let world = terrain(Contents::SOLID, 0);
        let from = Vec3::new(128.0, 128.0, 100.0);
        let hit = world.tracer().trace(
            &Ray::hull(from, from - Vec3::Z * 200.0, HULL_MIN, HULL_MAX),
            Contents::MASK_PLAYERSOLID,
        );

        assert!(hit.did_hit(), "{hit:?}");
        assert!((hit.normal - Vec3::Z).length() < 1e-5, "{hit:?}");
        assert!(hit.normal.z > 0.7, "standable: {hit:?}");
        // The feet, not the box centre.
        assert!((hit.end.z - DIST_EPSILON).abs() < 1e-3, "{hit:?}");
        assert!(hit.disp_flags & disp_surf::SURFACE != 0, "{hit:?}");
    }

    /// **Every displacement test is one-sided.** Terrain is solid from the
    /// front and transparent from behind, which is what makes the stab the
    /// strange thing it is — see [`test_in_disp_tree`].
    #[test]
    fn a_displacement_is_transparent_from_behind() {
        let world = terrain(Contents::SOLID, 0);
        let from = Vec3::new(128.0, 128.0, -100.0);

        let ray = world.tracer().trace(
            &Ray::line(from, from + Vec3::Z * 200.0),
            Contents::MASK_SOLID,
        );
        assert!(!ray.did_hit(), "{ray:?}");

        let hull = world.tracer().trace(
            &Ray::hull(from, from + Vec3::Z * 200.0, HULL_MIN, HULL_MAX),
            Contents::MASK_PLAYERSOLID,
        );
        assert!(!hull.did_hit(), "{hull:?}");
    }

    /// The displacement vectors are what make it terrain rather than a quad.
    #[test]
    fn a_displaced_vertex_raises_the_surface_under_it() {
        // A ramp along the grid's `i` axis, which is world `+Y`.
        let world = terrain_shaped(Contents::SOLID, 0, |i, _| i as f32 * 16.0);
        let mut tracer = world.tracer();

        let mut at = |y: f32| {
            let hit = tracer.trace(&down(Vec3::new(128.0, y, 200.0)), Contents::MASK_SOLID);
            assert!(hit.did_hit(), "at y={y}: {hit:?}");
            hit.end.z
        };

        // Four cells over 256 units, rising 16 per grid step: 0 at y=0 and 64
        // at y=256, linear in between.
        assert!((at(0.5) - 0.0).abs() < 1.0, "{}", at(0.5));
        assert!((at(128.0) - 32.0).abs() < 1.0, "{}", at(128.0));
        assert!((at(255.0) - 64.0).abs() < 1.0, "{}", at(255.0));
    }

    /// `startPosition` rotates the grid, and the grid is what the vertex
    /// offsets are indexed by — so the same offsets over the same quad make a
    /// *different* shape depending on which corner the file names.
    ///
    /// This is the one piece of the build that is pure bookkeeping and has no
    /// geometric tell: get it wrong and every patch in the map is the right
    /// shape turned by a multiple of 90°.
    #[test]
    fn the_start_position_rotates_the_grid() {
        let ramp = |start: Vec3| {
            let mut fixture = Fixture::default();
            fixture.add_displacement(QUAD, start, 2, Contents::SOLID, 0, |i, _| i as f32 * 16.0);
            fixture.single_leaf()
        };

        let height = |world: &CollisionBsp, x: f32, y: f32| {
            let hit = world
                .tracer()
                .trace(&down(Vec3::new(x, y, 200.0)), Contents::MASK_SOLID);
            assert!(hit.did_hit(), "({x}, {y}): {hit:?}");
            hit.end.z
        };

        // Corner 0 is the quad's own first point, so the ramp rises along +Y.
        let a = ramp(QUAD[0]);
        assert!(height(&a, 32.0, 224.0) > height(&a, 224.0, 32.0));

        // Corner 1 becomes the new origin, so `i` now runs along +X.
        let b = ramp(QUAD[1]);
        assert!(height(&b, 224.0, 32.0) > height(&b, 32.0, 224.0));
    }

    /// `SURF_NOHULL_COLL` and `SURF_NORAY_COLL` are live in Portal 2 — 44 of
    /// its 1,181 displacements carry both — and a patch that ignores them is
    /// an invisible wall in the ruins.
    #[test]
    fn the_collision_flags_hide_a_displacement_from_one_kind_of_query() {
        const NOHULL: u32 = 0x4;
        const NORAY: u32 = 0x8;

        let from = Vec3::new(128.0, 128.0, 100.0);
        let ray = down(from);
        let hull = Ray::hull(from, from - Vec3::Z * 200.0, HULL_MIN, HULL_MAX);

        let world = terrain(Contents::SOLID, NOHULL);
        assert!(world.tracer().trace(&ray, Contents::MASK_SOLID).did_hit());
        assert!(!world
            .tracer()
            .trace(&hull, Contents::MASK_PLAYERSOLID)
            .did_hit());

        let world = terrain(Contents::SOLID, NORAY);
        assert!(!world.tracer().trace(&ray, Contents::MASK_SOLID).did_hit());
        assert!(world
            .tracer()
            .trace(&hull, Contents::MASK_PLAYERSOLID)
            .did_hit());
    }

    /// A ray needs the displacement's contents to be *opaque*, where a hull
    /// only needs them to be in the mask. Portal 2's 51 `WINDOW | TRANSLUCENT`
    /// patches are exactly this case.
    #[test]
    fn a_ray_passes_through_a_displacement_that_blocks_a_hull() {
        // `CONTENTS_WINDOW | CONTENTS_TRANSLUCENT`, which is what the depot
        // holds for 51 of its displacements.
        let world = terrain(Contents(0x1000_0002), 0);
        let from = Vec3::new(128.0, 128.0, 100.0);

        assert!(!world
            .tracer()
            .trace(&down(from), Contents::MASK_SOLID)
            .did_hit());
        assert!(world
            .tracer()
            .trace(
                &Ray::hull(from, from - Vec3::Z * 200.0, HULL_MIN, HULL_MAX),
                Contents::MASK_SOLID,
            )
            .did_hit());
    }

    #[test]
    fn a_mask_that_excludes_the_displacement_hits_nothing() {
        let world = terrain(Contents::WATER, 0);
        let from = Vec3::new(128.0, 128.0, 100.0);
        assert!(!world
            .tracer()
            .trace(
                &Ray::hull(from, from - Vec3::Z * 200.0, HULL_MIN, HULL_MAX),
                Contents::MASK_PLAYERSOLID,
            )
            .did_hit());
        assert!(world
            .tracer()
            .trace(
                &Ray::hull(from, from - Vec3::Z * 200.0, HULL_MIN, HULL_MAX),
                Contents::MASK_WATER,
            )
            .did_hit());
    }

    /// The position test: a box straddling the surface is inside it.
    ///
    /// This is the half of `CM_TestInDispTree` that works — the separating-axis
    /// box-versus-triangle test. The other half, the stab, is tested below.
    #[test]
    fn a_box_straddling_a_displacement_is_all_solid() {
        let world = terrain(Contents::SOLID, 0);
        // Feet 36 below the surface, so the 72-tall hull is cut in half by it.
        let feet = Vec3::new(128.0, 128.0, -36.0);
        let hit = world.tracer().trace(
            &Ray::hull(feet, feet, HULL_MIN, HULL_MAX),
            Contents::MASK_PLAYERSOLID,
        );

        assert!(hit.all_solid && hit.start_solid, "{hit:?}");
        assert_eq!(hit.fraction, 0.0);
        assert_eq!(hit.contents, Contents::SOLID);
    }

    /// ...and a box in open air over terrain is not, which is the case the
    /// stab decides and the one it gets right.
    #[test]
    fn a_box_above_a_displacement_is_not_solid() {
        let world = terrain(Contents::SOLID, 0);
        let feet = Vec3::new(128.0, 128.0, 64.0);
        let hit = world.tracer().trace(
            &Ray::hull(feet, feet, HULL_MIN, HULL_MAX),
            Contents::MASK_PLAYERSOLID,
        );

        assert!(!hit.all_solid && !hit.start_solid, "{hit:?}");
        assert_eq!(hit.fraction, 1.0);
    }

    /// A point buried in terrain reports **not solid**, and that is Valve's
    /// answer rather than this port's.
    ///
    /// `CM_TestInDispTree` has no box test to run for a point, so it falls
    /// through to the stab — which fires along the surface's own normal, into
    /// the back of every triangle it could reach, where every triangle test in
    /// [`disp`] culls it. `CM_PostStab` then takes its clearing branch. Pinned
    /// here so that nobody "fixes" the stab into reporting solid without
    /// meaning to.
    #[test]
    fn a_point_inside_terrain_is_reported_as_not_solid() {
        let world = terrain(Contents::SOLID, 0);
        let inside = Vec3::new(128.0, 128.0, -16.0);
        let hit = world
            .tracer()
            .trace(&Ray::line(inside, inside), Contents::MASK_SOLID);

        assert!(!hit.all_solid && !hit.start_solid, "{hit:?}");
        assert_eq!(hit.fraction, 1.0);
    }

    /// A brush and a displacement in one leaf, and the nearer one wins —
    /// including [`Trace::disp_flags`], which must not claim terrain when a
    /// brush was hit.
    #[test]
    fn a_brush_and_a_displacement_compete_on_distance() {
        let build = |brush_z: f32| {
            let mut fixture = Fixture::default();
            fixture.add_displacement(QUAD, QUAD[0], 2, Contents::SOLID, 0, |_, _| 0.0);
            fixture.add_box(
                Vec3::new(0.0, 0.0, brush_z),
                Vec3::new(256.0, 256.0, brush_z + 8.0),
                Contents::SOLID,
                true,
            );
            fixture.single_leaf()
        };

        // A ledge above the terrain: the brush is hit and nothing says terrain.
        let world = build(64.0);
        let hit = world
            .tracer()
            .trace(&down(Vec3::new(128.0, 128.0, 200.0)), Contents::MASK_SOLID);
        assert!((hit.end.z - (72.0 + DIST_EPSILON)).abs() < 1e-2, "{hit:?}");
        assert_eq!(hit.disp_flags, 0, "a brush hit is not terrain: {hit:?}");

        // The brush buried below: the terrain is what stops the ray.
        let world = build(-64.0);
        // Past the terrain rather than exactly onto it: a ray whose *end* is
        // the surface is the degenerate case, not the interesting one.
        let from = Vec3::new(128.0, 128.0, 200.0);
        let hit = world.tracer().trace(
            &Ray::line(from, from - Vec3::Z * 400.0),
            Contents::MASK_SOLID,
        );
        assert!(hit.end.z.abs() < 1e-2, "{hit:?}");
        assert!(hit.disp_flags & disp_surf::SURFACE != 0, "{hit:?}");
    }

    /// **The one place this module fixes Valve rather than reproducing it.**
    ///
    /// `dispFlags` is written when a displacement wins and cleared nowhere, so
    /// in the original a brush that supersedes a displacement *keeps the
    /// displacement's flags* and `IsDispSurface()` calls a wall terrain. This
    /// port clears them where `m_bDispHit` is cleared — see `brush.rs`.
    ///
    /// The shape that reaches it needs care, because within one leaf the
    /// brushes are clipped first and cannot come second. What makes it
    /// reachable is that **a displacement is traced against the whole ray from
    /// whichever leaf reaches it first** — its bounding box spans many — so a
    /// near leaf can record a hit far down the ray, and a brush in a later leaf
    /// can then beat it.
    #[test]
    fn a_brush_hit_after_a_displacement_does_not_report_terrain() {
        let mut fixture = Fixture::default();
        // Terrain across the whole map, so its bounding box is in both leaves
        // and the *near* one traces it.
        fixture.add_displacement(
            [
                Vec3::new(-256.0, 0.0, 0.0),
                Vec3::new(-256.0, 256.0, 0.0),
                Vec3::new(256.0, 256.0, 0.0),
                Vec3::new(256.0, 0.0, 0.0),
            ],
            Vec3::new(-256.0, 0.0, 0.0),
            2,
            Contents::SOLID,
            0,
            |_, _| 0.0,
        );
        // A ledge above the terrain, entirely in the far (`x < 0`) leaf.
        let brush = fixture.add_box(
            Vec3::new(-100.0, 0.0, 10.0),
            Vec3::new(-20.0, 256.0, 30.0),
            Contents::SOLID,
            true,
        );
        let world = fixture.split(&[], &[brush]);

        // Down and to the left: it crosses `x = 0` at fraction 0.5, enters the
        // ledge at about 0.55, and would reach the terrain at about 0.71. So
        // the near leaf records the terrain first and the far leaf's brush
        // then wins.
        let from = Vec3::new(200.0, 128.0, 100.0);
        let hit = world.tracer().trace(
            &Ray::line(from, Vec3::new(-200.0, 128.0, -40.0)),
            Contents::MASK_SOLID,
        );

        assert!(hit.did_hit(), "{hit:?}");
        assert!(
            (hit.normal - Vec3::X).length() < 1e-5,
            "the ledge's +X face, not the terrain: {hit:?}"
        );
        assert_eq!(
            hit.disp_flags, 0,
            "the brush won, so nothing may claim terrain: {hit:?}"
        );
    }

    /// The displacement visit stamps are a second array beside the brushes',
    /// and they must reset between traces the same way.
    #[test]
    fn a_tracer_gives_the_same_displacement_answer_twice() {
        let world = terrain(Contents::SOLID, 0);
        let mut tracer = world.tracer();
        let ray = down(Vec3::new(128.0, 128.0, 100.0));

        let first = tracer.trace(&ray, Contents::MASK_SOLID);
        assert!(first.did_hit());
        for _ in 0..4 {
            assert_eq!(tracer.trace(&ray, Contents::MASK_SOLID), first);
        }
    }

    /// Every shipped map's terrain, built and swept for real —
    /// `portdocs/ENGINE_TRACE.md` stage 3's verification.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_map_displacements -- --ignored --nocapture
    /// ```
    ///
    /// The assertion that earns the runtime is the **bounds check**: a hit has
    /// to land inside the patch it came from. A wrong start corner rotates the
    /// grid, a wrong vertex stride reads another patch's offsets, and a wrong
    /// triangle winding drops the hit entirely — the first two land outside the
    /// box and the third shows up in the hit count.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_map_traces_its_displacements() {
        use crate::engine::world::bsp::Bsp;

        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = crate::filesystem::Vfs::mount_game(&dir, &base, &Default::default())
            .expect("mount the game");

        let mut names: Vec<String> = vfs
            .list("maps")
            .expect("maps/")
            .into_iter()
            .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
            .map(|e| e.name.trim_end_matches(".bsp").to_owned())
            .collect();
        names.sort();
        assert!(names.len() > 50, "only {} maps found", names.len());

        let (mut maps, mut with_terrain, mut total, mut built) = (0, 0, 0usize, 0usize);
        let (mut ray_hits, mut hull_hits, mut walkable) = (0usize, 0usize, 0usize);
        let mut powers = std::collections::BTreeMap::<i32, usize>::new();
        let mut listed = 0usize;
        let (mut eligible, mut head_on) = (0usize, 0usize);
        let (mut head_on_other, mut head_on_blocked) = (0usize, 0usize);

        for name in &names {
            let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
            let collision = CollisionBsp::build(&bsp);
            maps += 1;
            total += bsp.disp_info.len();
            built += collision.disp_count();
            listed += collision.leaf_disps.len();
            if bsp.disp_info.is_empty() {
                continue;
            }
            with_terrain += 1;
            for info in &bsp.disp_info {
                *powers.entry(info.power).or_default() += 1;
            }
            // Every displacement is named by at least one leaf, or nothing
            // could ever reach it.
            assert!(
                !collision.leaf_disps.is_empty(),
                "{name}: {} displacements and no leaf lists them",
                bsp.disp_info.len()
            );

            // The base quad of each displacement, read straight out of the
            // `.bsp` rather than asked of the module — so the head-on test
            // below is checking the build against the file rather than against
            // itself.
            let mut quads: Vec<Option<[Vec3; 4]>> = vec![None; bsp.disp_info.len()];
            for face in &bsp.faces {
                if let Ok(index) = usize::try_from(face.disp_info) {
                    let corners: Vec<Vec3> = bsp.face_vertices(face).collect();
                    if let (Some(slot @ None), Ok(corners)) =
                        (quads.get_mut(index), <[Vec3; 4]>::try_from(corners))
                    {
                        *slot = Some(corners);
                    }
                }
            }

            let mut tracer = collision.tracer();
            for (index, quad) in quads.iter().enumerate() {
                // Every shipped displacement builds, which the assertion at the
                // end pins; a slot that did not is skipped rather than assumed
                // away, because the indices are not compacted.
                let Some((mins, maxs)) = collision.disp_bounds(index) else {
                    continue;
                };

                // **Head-on.** Every triangle test in `disp` is one-sided
                // against `(p3 - p0) × (p1 - p0)`, so a ray fired back along
                // that normal, from just outside the envelope the vertex
                // offsets can reach, has to hit — and if the winding
                // convention were inverted, *nothing* would.
                let info = &bsp.disp_info[index];
                let opaque = Contents(info.contents as u32).intersects(Contents::MASK_OPAQUE);
                let rays_collide = info.disp_flags() & 0x8 == 0;
                if let (Some(quad), true, true) = (*quad, opaque, rays_collide) {
                    eligible += 1;
                    let normal = (quad[3] - quad[0])
                        .cross(quad[1] - quad[0])
                        .normalize_or_zero();
                    let first = info.disp_vert_start as usize;
                    let reach = bsp.disp_verts[first
                        ..first + crate::engine::world::bsp::DispInfo::vert_count(info.power)]
                        .iter()
                        .fold(1.0f32, |m, v| m.max(v.dist.abs()))
                        + 2.0;
                    let mid = (quad[0] + quad[1] + quad[2] + quad[3]) * 0.25;
                    let hit = tracer.trace(
                        &Ray::line(mid + normal * reach, mid - normal * reach),
                        Contents::MASK_SOLID,
                    );
                    if hit.disp_flags & disp_surf::SURFACE == 0 {
                        head_on_blocked += 1;
                    }
                    if hit.disp_flags & disp_surf::SURFACE != 0 {
                        let slack = Vec3::ONE;
                        let inside = |lo: Vec3, hi: Vec3| {
                            hit.end.cmpge(lo - slack).all() && hit.end.cmple(hi + slack).all()
                        };
                        if inside(mins, maxs) {
                            head_on += 1;
                        } else {
                            head_on_other += 1;
                            // The ray starts as far out as the vertex offsets
                            // could possibly carry the surface, which on a
                            // deeply-displaced patch is far enough to cross a
                            // *different* one first. That is the trace working,
                            // not failing — but the hit still has to be
                            // somebody's terrain.
                            assert!(
                                (0..collision.disp_count())
                                    .filter_map(|i| collision.disp_bounds(i))
                                    .any(|(lo, hi)| inside(lo, hi)),
                                "{name}: displacement {index} head-on hit {} inside no \
                                 displacement's box at all",
                                hit.end,
                            );
                        }
                    }
                }

                let centre = (mins + maxs) * 0.5;
                // From just above this patch's own box to just below it, and
                // **not** from far away: a map's patches overlap in plan —
                // terrain over a cave, a rubble pile against a wall — so a long
                // ray reports whichever one it reaches first and says nothing
                // about this one. Starting inside the box's own vertical span
                // is what makes the assertion below about *this* displacement.
                let from = Vec3::new(centre.x, centre.y, maxs.z + 1.0);
                let to = Vec3::new(centre.x, centre.y, mins.z - 1.0);

                for (is_hull, hit) in [
                    (
                        false,
                        tracer.trace(&Ray::line(from, to), Contents::MASK_SOLID),
                    ),
                    (
                        true,
                        tracer.trace(
                            &Ray::hull(from, to, HULL_MIN, HULL_MAX),
                            Contents::MASK_PLAYERSOLID,
                        ),
                    ),
                ] {
                    assert!(
                        hit.fraction.is_finite() && (0.0..=1.0).contains(&hit.fraction),
                        "{name}: displacement {index} fraction {}",
                        hit.fraction
                    );
                    if hit.disp_flags == 0 {
                        continue; // a brush or nothing at all was in the way
                    }
                    match is_hull {
                        true => hull_hits += 1,
                        false => ray_hits += 1,
                    }
                    if hit.disp_flags & disp_surf::WALKABLE != 0 {
                        walkable += 1;
                    }
                    // The hull's extents push the impact out by up to a hull
                    // half-width beyond the patch, so the slack is the hull.
                    let slack = match is_hull {
                        true => Vec3::new(17.0, 17.0, 73.0),
                        false => Vec3::ONE,
                    };
                    assert!(
                        hit.end.cmpge(mins - slack).all() && hit.end.cmple(maxs + slack).all(),
                        "{name}: displacement {index} hit {} outside its own box {mins}..{maxs}",
                        hit.end,
                    );
                }
            }
        }

        println!(
            "{maps} maps, {with_terrain} with terrain: {total} displacements, {built} built, \
             {listed} leaf references; powers {powers:?};\n  \
             head-on along the base normal: {head_on} of {eligible} ray-eligible hit their own \
             patch, {head_on_other} another patch, {head_on_blocked} something else;\n  \
             straight down the middle of the box: {ray_hits} ray hits, {hull_hits} hull hits, \
             {walkable} of them walkable."
        );
        assert!(
            total > 1000,
            "only {total} displacements across {maps} maps"
        );
        assert_eq!(built, total, "every shipped displacement builds");
        // The winding check. A patch can still be missed head-on — a vertex
        // offset can carry the surface sideways out of the ray's path — but if
        // the normals came out inverted this would be zero.
        assert!(
            head_on * 4 > eligible * 3,
            "only {head_on} of {eligible} displacements were hit along their own normal"
        );
    }
    /// A real split at `x = 0` with four brushes hung off it: two clear of the
    /// plane on the front side, one straddling it so that **both** leaves list
    /// it, and one that is `PLAYERCLIP` rather than `SOLID`.
    ///
    /// The shape is chosen so that the front leaf lists brushes the query box
    /// does not touch — which is the whole difference between enumerating
    /// brushes and concatenating leaf lists.
    fn split_with_four_brushes() -> (CollisionBsp, [usize; 4]) {
        let mut fixture = Fixture::default();
        let near = fixture.add_box(
            Vec3::new(10.0, -10.0, -10.0),
            Vec3::new(20.0, 10.0, 10.0),
            Contents::SOLID,
            true,
        );
        let far = fixture.add_box(
            Vec3::new(100.0, -10.0, -10.0),
            Vec3::new(110.0, 10.0, 10.0),
            Contents::SOLID,
            true,
        );
        // Not axial, so this one takes the plane path through the position
        // test while the other three take the box-brush path.
        let across = fixture.add_box(
            Vec3::new(-5.0, -10.0, -10.0),
            Vec3::new(5.0, 10.0, 10.0),
            Contents::SOLID,
            false,
        );
        let clip = fixture.add_box(
            Vec3::new(10.0, 20.0, -10.0),
            Vec3::new(20.0, 30.0, 10.0),
            Contents::PLAYERCLIP,
            true,
        );
        let world = fixture.split(&[near, far, across, clip], &[across]);
        (
            world,
            [near as usize, far as usize, across as usize, clip as usize],
        )
    }

    #[test]
    fn a_box_finds_the_brushes_it_overlaps_and_not_the_ones_it_shares_a_leaf_with() {
        let (world, [near, far, across, clip]) = split_with_four_brushes();
        // Straddles the node plane, so both leaves are descended and the front
        // leaf hands over all four brushes as candidates. Only one is really
        // in the box.
        let found = world.tracer().brushes_in_box(
            Vec3::new(-8.0, -12.0, -12.0),
            Vec3::new(8.0, 12.0, 12.0),
            Contents::MASK_ALL,
        );
        assert_eq!(found, vec![across], "near {near}, far {far}, clip {clip}");
    }

    #[test]
    fn a_brush_in_two_leaves_is_reported_once() {
        let (world, [_, _, across, _]) = split_with_four_brushes();
        // The same query. `across` is listed by the front leaf *and* the back
        // one, so without the visit stamp it would come back twice — and the
        // carve would cut the same wall twice and leave two copies of it.
        let found = world.tracer().brushes_in_box(
            Vec3::new(-8.0, -12.0, -12.0),
            Vec3::new(8.0, 12.0, 12.0),
            Contents::MASK_ALL,
        );
        assert_eq!(found.iter().filter(|&&b| b == across).count(), 1);
    }

    #[test]
    fn brushes_come_back_in_the_order_their_leaves_list_them() {
        let (world, [near, _, across, _]) = split_with_four_brushes();
        // Wholly in front of the node plane, so one leaf, whose list is
        // `near, far, across, clip`.
        let found = world.tracer().brushes_in_box(
            Vec3::new(4.0, -2.0, -2.0),
            Vec3::new(12.0, 2.0, 2.0),
            Contents::MASK_ALL,
        );
        assert_eq!(found, vec![near, across]);
    }

    #[test]
    fn the_contents_mask_decides_what_comes_back() {
        let (world, [_, _, _, clip]) = split_with_four_brushes();
        let query = |mask| {
            world.tracer().brushes_in_box(
                Vec3::new(12.0, 22.0, -2.0),
                Vec3::new(18.0, 28.0, 2.0),
                mask,
            )
        };
        assert_eq!(query(Contents::MASK_PLAYERSOLID), vec![clip]);
        // The same box, and the brush is still there — a mask that does not
        // name `PLAYERCLIP` simply cannot see it.
        assert_eq!(query(Contents::MASK_SOLID_BRUSHONLY), Vec::<usize>::new());
    }

    #[test]
    fn a_box_in_open_air_finds_nothing() {
        let (world, _) = split_with_four_brushes();
        let found = world.tracer().brushes_in_box(
            Vec3::new(500.0, 500.0, 500.0),
            Vec3::new(510.0, 510.0, 510.0),
            Contents::MASK_ALL,
        );
        assert_eq!(found, Vec::<usize>::new());
    }

    #[test]
    fn a_collision_model_with_no_tree_has_no_brushes_in_any_box() {
        let world = Fixture::default().finish();
        assert!(world.is_empty());
        let found = world.tracer().brushes_in_box(
            Vec3::splat(-1000.0),
            Vec3::splat(1000.0),
            Contents::MASK_ALL,
        );
        assert_eq!(found, Vec::<usize>::new());
    }

    /// Every leaf under `head_node`, found by walking the whole subtree and
    /// culling nothing.
    ///
    /// The scope half of the checks below: the one thing they and
    /// [`CollisionBsp::leaves_in_box`] have to agree about that is *not* under
    /// test — which leaves belong to the tree at all — derived the only way
    /// that cannot encode a box test by accident.
    fn every_leaf_under(bsp: &CollisionBsp, head_node: i32) -> Vec<usize> {
        let mut leaves = Vec::new();
        let mut pending = vec![head_node];
        while let Some(num) = pending.pop() {
            match num < 0 {
                true => leaves.push((-1 - num) as usize),
                false => pending.extend(bsp.nodes[num as usize].children),
            }
        }
        leaves
    }

    /// Whether one brush passes the position test against this box.
    ///
    /// [`Tracer::brushes_in_box`]'s own predicate, pulled out so the checks
    /// below can apply it to a brush of their choosing rather than to the
    /// answer they are checking.
    fn box_overlaps_brush(
        bsp: &CollisionBsp,
        visits: &mut Visits,
        index: usize,
        mins: Vec3,
        maxs: Vec3,
        mask: Contents,
    ) -> bool {
        let center = (mins + maxs) * 0.5;
        let mut work = Work {
            bsp,
            start: center,
            end: center,
            extents: maxs - center,
            delta: Vec3::ZERO,
            inv_delta: Vec3::ZERO,
            is_point: false,
            is_swept: false,
            contents: mask,
            trace: Trace::miss(center, center),
            disp_hit: false,
            visits,
        };
        super::brush::test_box_in_brush(&mut work, index);
        work.trace.all_solid
    }

    /// [`Tracer::brushes_in_box`] on every shipped map, checked four ways.
    ///
    /// # Why this is not one comparison against brute force
    ///
    /// The obvious test — enumerate the box, enumerate every brush in the map
    /// by hand, assert the two sets are equal — was written first and is
    /// **wrong**, three times over, and each failure says something about the
    /// map format worth keeping:
    ///
    /// 1. **A map's brush lump holds brushes no leaf names.** One of them sits
    ///    squarely inside a probe box in `mp_coop_catapult_1`. Nothing in the
    ///    engine can reach them; a leaf-limited query is not missing them.
    /// 2. **A brush model's leaves share the array with the world's.** Eleven
    ///    of `mp_coop_catapult_2`'s stack up at one probe, and a door is not
    ///    what this call answers about — see [`Tracer::brushes_in_box`].
    /// 3. **A brush can reach outside the leaves that list it.** In
    ///    `mp_coop_catapult_wall_intro` a brush spans `z −112..128` and the one
    ///    world leaf naming it stops at `z 96`, so a box in `z 112..544`
    ///    genuinely overlaps a brush that no reachable leaf offers. **Valve's
    ///    `GetBrushesInAABB` misses it too** — it is leaf-limited by
    ///    construction — so the carve inherits it, and `PORTAL.md` §4 should
    ///    expect a hole cut against the leaves rather than against the
    ///    geometry.
    ///
    /// What is left after those three is not "the set of brushes in the box",
    /// and pretending otherwise is how the first version of this test claimed
    /// a bug that was not there. So the properties checked are the ones that
    /// are actually true:
    ///
    /// - **Nothing comes back twice**, which is the visit stamp.
    /// - **Everything that comes back really is in the box**, checked for box
    ///   brushes by plain interval arithmetic that shares no code with the
    ///   module, and for all of them against the contents mask.
    /// - **The descent reaches every leaf the box is in**, checked by dropping
    ///   125 points into the box and asking [`CollisionBsp::leaf`] — a
    ///   different function, already under test — which leaf each is in.
    /// - **Nothing those leaves offer is dropped.** This is the one with
    ///   teeth: a leaf the descent reaches and a brush that passes the
    ///   position test must be in the answer.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release brushes_in_a_box -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_map_enumerates_the_brushes_in_a_box() {
        use crate::engine::world::bsp::Bsp;
        use std::collections::HashSet;

        /// `PORTAL_HALF_WIDTH` and `PORTAL_HALF_HEIGHT`
        /// (`game/shared/portal/prop_portal_shared.h`).
        const HALF_WIDTH: f32 = 32.0;
        const HALF_HEIGHT: f32 = 54.0;
        /// What the portal simulator carves with — its four brush sets' masks
        /// unioned, which is the commented-out line at
        /// `portalsimulation.cpp:1167`. The four exist to give vphysics four
        /// collideables with four collision filters; a BSP brush carries its
        /// own contents, so this port has no reason to split them.
        const CARVE: Contents = Contents(
            Contents::MASK_SOLID_BRUSHONLY.0 | Contents::PLAYERCLIP.0 | Contents::MONSTERCLIP.0,
        );

        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = crate::filesystem::Vfs::mount_game(&dir, &base, &Default::default())
            .expect("mount the game");

        let mut names: Vec<String> = vfs
            .list("maps")
            .expect("maps/")
            .into_iter()
            .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
            .map(|e| e.name.trim_end_matches(".bsp").to_owned())
            .collect();
        names.sort();
        assert!(names.len() > 50, "only {} maps found", names.len());

        // The (holy) wall query box's half-extents: `2 * max(halfHeight,
        // halfWidth)` back from the portal and `4 * halfWidth` by `4 *
        // halfHeight` across it (`portalsimulation.cpp:3530`). Axis-aligned
        // here, which is the *worst* case for the descent rather than the
        // best — an oriented box's world AABB is larger and reaches more
        // leaves.
        let extents = Vec3::new(
            HALF_HEIGHT.max(HALF_WIDTH),
            HALF_WIDTH * 4.0,
            HALF_HEIGHT * 4.0,
        );

        let (mut maps, mut probes, mut hits) = (0usize, 0usize, 0usize);
        let (mut returned, mut candidates, mut worst) = (0usize, 0usize, 0usize);
        let (mut brushes, mut elsewhere) = (0usize, 0usize);
        let (mut leaves_reached, mut points) = (0usize, 0usize);
        for name in &names {
            let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
            let collision = CollisionBsp::build(&bsp);
            let Some(model) = bsp.models.first() else {
                continue;
            };
            maps += 1;
            brushes += collision.brushes.len();

            let world_leaves = every_leaf_under(&collision, 0);
            // Brushes the world subtree cannot reach: a brush model's, and the
            // ones no leaf names at all. Neither is this call's to find.
            let mut listed = vec![false; collision.brushes.len()];
            for &leaf in &world_leaves {
                let leaf = collision.leaves[leaf];
                let first = leaf.first_leaf_brush as usize;
                for n in first..first + leaf.num_leaf_brushes as usize {
                    listed[collision.leaf_brushes[n] as usize] = true;
                }
            }
            elsewhere += listed.iter().filter(|l| !**l).count();

            let mut visits = Visits::new(collision.brushes.len(), collision.disps.len());
            visits.begin();

            let mins = Vec3::from(model.mins);
            let maxs = Vec3::from(model.maxs);
            for i in 0..3 {
                for j in 0..3 {
                    for k in 0..3 {
                        // The 27 interior points of a 4x4x4 division, so no
                        // probe sits on the world's own bounding planes.
                        let t = Vec3::new(i as f32 + 1.0, j as f32 + 1.0, k as f32 + 1.0) / 4.0;
                        let center = mins + (maxs - mins) * t;
                        let (lo, hi) = (center - extents, center + extents);

                        let found = collision.tracer().brushes_in_box(lo, hi, CARVE);
                        probes += 1;
                        returned += found.len();
                        worst = worst.max(found.len());
                        if !found.is_empty() {
                            hits += 1;
                        }

                        let unique: HashSet<usize> = found.iter().copied().collect();
                        assert_eq!(
                            unique.len(),
                            found.len(),
                            "{name}: the box at {center} reported a brush twice"
                        );

                        for &index in &found {
                            let brush = collision.brushes[index];
                            assert!(
                                brush.contents.intersects(CARVE),
                                "{name}: brush {index} has contents {} and the mask is {CARVE}",
                                brush.contents
                            );
                            // The independent half: a box brush's overlap is
                            // interval arithmetic and needs none of this
                            // module to decide.
                            if let model::BrushSides::Box(b) = brush.sides {
                                let b = collision.box_brushes[b as usize];
                                assert!(
                                    (0..3).all(|a| b.mins[a].max(lo[a]) <= b.maxs[a].min(hi[a])),
                                    "{name}: brush {index} is {} .. {} and the box is {lo} .. {hi}",
                                    b.mins,
                                    b.maxs
                                );
                            }
                        }

                        // Every leaf the descent claims, and every leaf 125
                        // points inside the box say it should have claimed.
                        let reached: HashSet<usize> =
                            collision.leaves_in_box(lo, hi, 0).into_iter().collect();
                        leaves_reached += reached.len();
                        for a in 0..5 {
                            for b in 0..5 {
                                for c in 0..5 {
                                    let t = Vec3::new(a as f32, b as f32, c as f32) / 4.0;
                                    let point = lo + (hi - lo) * t;
                                    let leaf = collision.leaf(point);
                                    points += 1;
                                    assert!(
                                        reached.contains(&leaf),
                                        "{name}: {point} is in leaf {leaf} and the box \
                                         {lo} .. {hi} did not reach it"
                                    );
                                }
                            }
                        }

                        // Nothing the reached leaves offer may be dropped.
                        let mut offered = HashSet::new();
                        for &leaf in &reached {
                            let leaf = collision.leaves[leaf];
                            let first = leaf.first_leaf_brush as usize;
                            for n in first..first + leaf.num_leaf_brushes as usize {
                                let index = collision.leaf_brushes[n] as usize;
                                offered.insert(index);
                                if !collision.brushes[index].contents.intersects(CARVE) {
                                    continue;
                                }
                                if box_overlaps_brush(&collision, &mut visits, index, lo, hi, CARVE)
                                {
                                    assert!(
                                        unique.contains(&index),
                                        "{name}: brush {index} is in a leaf the box \
                                         {lo} .. {hi} reached, passes the position test, \
                                         and was not reported"
                                    );
                                }
                            }
                        }
                        candidates += offered.len();
                    }
                }
            }
        }

        println!(
            "{maps} maps, {brushes} brushes, {elsewhere} of them outside the world subtree;\n  \
             {probes} probes, {hits} of them on something: {returned} brushes returned in all, \
             at most {worst} from one box;\n  \
             {leaves_reached} leaves reached and {points} points asked which leaf they are \
             in;\n  \
             those leaves offer {candidates} brushes — the overlap test rejects {:.1}% of \
             what the leaf lists alone would have carved.",
            match candidates {
                0 => 0.0,
                _ => 100.0 * (candidates - returned) as f32 / candidates as f32,
            }
        );
        assert_eq!(maps, 106, "the 106 shipped maps");
        // A grid over a map's bounding box lands mostly in solid rock or
        // outside the level, so most probes finding nothing is the expected
        // shape. Measured: 760 of 2,862. This is here to catch a descent that
        // has stopped finding anything at all, not to pin the ratio.
        assert!(
            hits * 5 > probes,
            "only {hits} of {probes} probes found a brush, so the probes are in the void"
        );
    }
}
