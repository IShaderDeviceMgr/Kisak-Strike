//! Ray and swept-box traces against the world.
//!
//! Replaces `engine/cmodel.cpp`'s trace (`CM_BoxTrace` and everything under
//! it) and, later, `engine/enginetrace.cpp`'s dispatch over entities. This is
//! stage 1 of `portdocs/ENGINE_TRACE.md`: the **world's brushes only** — no
//! brush models, no displacements, no entities, no static props.
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

mod brush;
#[cfg(test)]
pub(crate) mod fixture;
mod hull;
mod model;
mod ray;
mod result;

pub use model::CollisionBsp;
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
    /// (`engine/enginetrace.cpp:2838`). Brush models — doors, platforms — are
    /// stage 2 and are not included.
    pub fn trace(&mut self, ray: &Ray, mask: Contents) -> Trace {
        let origin = ray.origin();

        // `if (!pBSPData->numnodes)` — a map with no collision tree traces as
        // a clean miss. Valve returns here *before* computing the endpoints,
        // leaving `startpos`/`endpos` at zero; this fills them in, because a
        // caller reading `end` from a trace that hit nothing should get the
        // place it was going.
        if self.bsp.nodes.is_empty() {
            return Trace::miss(origin, origin + ray.delta);
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
            hull::unswept_box_trace(&mut work, 0);
        } else if ray.is_ray {
            hull::recursive_hull_check::<true>(&mut work, 0, 0.0, 1.0, start, end);
        } else {
            hull::recursive_hull_check::<false>(&mut work, 0, 0.0, 1.0, start, end);
        }

        let mut trace = work.trace;
        compute_trace_endpoints(ray, &mut trace);

        // `CEngineTrace::TraceRay`'s last act (`engine/enginetrace.cpp:2956`):
        // a box sweep never computed `fractionleftsolid`, so it must not
        // appear to have one. Valve writes a NaN here in debug builds to catch
        // anyone reading it.
        if !ray.is_ray {
            trace.start = ray.origin();
            trace.fraction_left_solid = 0.0;
        }
        trace
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
        let ray = Ray::hull(
            Vec3::ZERO,
            Vec3::new(200.0, 0.0, 0.0),
            HULL_MIN,
            HULL_MAX,
        );
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
        assert!((hit.end.x - (-200.0 - DIST_EPSILON)).abs() < 1e-3, "{hit:?}");
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
