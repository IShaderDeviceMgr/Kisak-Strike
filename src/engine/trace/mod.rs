//! Ray and swept-box traces against the world.
//!
//! Replaces `engine/cmodel.cpp`'s trace (`CM_BoxTrace` and everything under
//! it) and, later, `engine/enginetrace.cpp`'s dispatch over entities. This is
//! stages 1-2 of `portdocs/ENGINE_TRACE.md`: the world's brushes
//! ([`Tracer::trace`]) and the **brush models** built out of them —
//! doors, platforms, pistons ([`Tracer::trace_model`]). No displacements, no
//! entities, no static props.
//!
//! The two are deliberately separate calls and nothing yet combines them: a
//! caller that wants "what is in the way" asks the world, then asks each brush
//! model, and keeps the nearest. Doing that *for* the caller is
//! `ClipRayToCollideable`'s job and needs a filter and a broadphase, which
//! need entities (stage 4).
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
//! Three things here produce a plausible wrong answer rather than an error,
//! and all three are Valve's rather than this port's:
//!
//! 1. **[`Ray`]'s start is the centre of the box; [`Trace`]'s is not.** A
//!    player hull is 72 units tall, so the two differ by 36 — see [`Ray`].
//! 2. **[`Trace::fraction`] stops `DIST_EPSILON` (1/32 unit) short** of the
//!    surface, deliberately, and movement code depends on the gap.
//! 3. **[`Trace::fraction_left_solid`] is meaningful for rays only.** A hull
//!    sweep gets zero, matching `CEngineTrace::TraceRay`.
//! 4. **A brush model's [`Trace::plane_dist`] stays in the model's frame**,
//!    where its `normal` comes back rotated into the caller's — see
//!    [`Tracer::trace_model`].

mod brush;
#[cfg(test)]
pub(crate) mod fixture;
mod hull;
mod model;
mod ray;
mod result;

pub use model::{BrushModel, CollisionBsp};
pub use ray::{Contents, Ray};
pub use result::{Surface, Trace};

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
    /// The mask this trace is testing against.
    contents: Contents,
    trace: Trace,
    /// One stamp per brush; equal to `stamp` means "already visited".
    stamps: &'a mut [u32],
    stamp: u32,
}

impl Work<'_> {
    /// Whether this brush has not been clipped against yet in this trace.
    ///
    /// `TraceInfo_t::Visit` (`engine/cmodel_private.h:78`). A brush belongs to
    /// every leaf it touches, so without this a wall spanning eight leaves is
    /// clipped eight times — wasted work, and wrong for
    /// [`Trace::fraction_left_solid`], which accumulates.
    fn visit(&mut self, brush: usize) -> bool {
        if self.stamps[brush] == self.stamp {
            return false;
        }
        self.stamps[brush] = self.stamp;
        true
    }
}

/// Traces against one collision model.
///
/// Holds the per-trace scratch, so **make one and keep it**: a fresh `Tracer`
/// allocates a stamp per brush. This is Valve's `BeginTrace`/`EndTrace` pair
/// (`engine/cmodel.cpp:66`, `:111`) expressed as a borrow — including the
/// re-entrancy those two managed by hand with `PushTraceVisits` and a depth
/// counter, which here is simply a second `Tracer`.
#[derive(Debug)]
pub struct Tracer<'a> {
    bsp: &'a CollisionBsp,
    stamps: Vec<u32>,
    stamp: u32,
}

impl<'a> Tracer<'a> {
    pub(super) fn new(bsp: &'a CollisionBsp) -> Tracer<'a> {
        Tracer {
            bsp,
            stamps: vec![0; bsp.brushes.len()],
            stamp: 0,
        }
    }

    /// Sweeps `ray` through the world, stopping at the first thing matching
    /// `mask`.
    ///
    /// `CM_BoxTrace` against head node 0, which is what
    /// `CEngineTrace::TraceRay` passes for the world
    /// (`engine/enginetrace.cpp:2838`). **Brush models are not included** —
    /// a door is not part of the world's subtree, and hitting one is
    /// [`trace_model`](Tracer::trace_model)'s question. Nothing yet combines
    /// the two; that is `ClipRayToCollideable`'s job and it arrives with
    /// entities (`portdocs/ENGINE_TRACE.md` stage 4).
    pub fn trace(&mut self, ray: &Ray, mask: Contents) -> Trace {
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

    /// `CM_BoxTrace` with Valve's `computeEndpt` false: fractions and flags,
    /// with `start`/`end` left for the caller to resolve in whichever frame it
    /// asked the question.
    ///
    /// `head_node` selects the subtree — 0 for the world, a model's own for a
    /// brush model. It is trusted to be a valid node index; `Bsp::parse`'s
    /// `validate` is what makes that true.
    fn box_trace(&mut self, ray: &Ray, head_node: i32, mask: Contents) -> Trace {
        // `if (!pBSPData->numnodes)` — a map with no collision tree traces as
        // a clean miss. Valve returns here leaving `startpos`/`endpos` at
        // zero; the positions below are placeholders either way, because both
        // callers overwrite them with `compute_trace_endpoints`.
        if self.bsp.nodes.is_empty() {
            return Trace::miss(ray.start, ray.start + ray.delta);
        }

        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            // Wrapped: every stamp would compare equal to a stale one.
            self.stamps.fill(0);
            self.stamp = 1;
        }

        let start = ray.start;
        let end = ray.start + ray.delta;
        let mut work = Work {
            bsp: self.bsp,
            start,
            end,
            extents: ray.extents,
            delta: ray.delta,
            inv_delta: ray.inv_delta(),
            contents: mask,
            trace: Trace::miss(start, end),
            stamps: &mut self.stamps,
            stamp: self.stamp,
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
}
