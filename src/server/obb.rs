//! Does a swept box touch an oriented box?
//!
//! `IntersectRayWithBox` and `IntersectRayWithOBB` (`public/collisionutils.cpp`
//! `:1131`, `:1477` and `:1685`), which is what `CEngineTrace::ClipRayToOBB`
//! (`engine/enginetrace.cpp:1284`) runs for a `SOLID_OBB` entity.
//!
//! # Why this is in `server/` and not in `engine/trace/`
//!
//! Every other collision question this port asks is about data the engine
//! owns — the `.bsp`'s brushes, its displacements, a brush model's own
//! subtree — so it goes out through [`TouchQuery`](super::TouchQuery) and
//! comes back as a `"*N"` model index. An **`SOLID_OBB` trigger has no map
//! data at all**: it is a box the game invented, at a placement the game
//! chose, held in the game's own [`ModelBounds`](super::movement::ModelBounds).
//! There is nothing to ask the engine about.
//!
//! That is also where Valve keeps it. `collisionutils.cpp` is in `public/`,
//! compiled into the engine *and* into both game DLLs; it is not
//! engine-private the way `cmodel.cpp` is. So this module names no `engine`
//! type, and `server/` still names no collision type of the engine's.
//!
//! # What a caller gets, and what it does not
//!
//! One `bool`. Valve fills in a whole `trace_t` here, and its one caller on
//! this path — `CTouchLinks::EnumElement` (`engine/world.cpp:181`) — reads
//! exactly one thing out of it:
//!
//! ```text
//! g_pEngineTraceServer->ClipRayToCollideable( m_Ray, MASK_SOLID, pTriggerCollideable, &tr );
//! if ( !(tr.contents & MASK_SOLID) ) return ITERATION_CONTINUE;
//! ```
//!
//! and both of the branches that hit set `contents` to `CONTENTS_SOLID` and
//! return `true`, while the branches that return `false` leave it at
//! `Collision_ClearTrace`'s zero. So "did it touch" is exactly "did the
//! function return `true`", and the fraction, the end position and the plane
//! are all worked out and then thrown away. They are not computed here. The
//! moment something wants to *stop* against an OBB rather than notice it, this
//! grows a `Trace` return and the plane fix-ups at `:1657` and `:1462` come
//! with it.

use glam::{Mat3, Vec3};

use crate::math::angle_matrix;

/// `DIST_EPSILON` (`public/coordsize.h:35`) — the tolerance `ClipRayToOBB`
/// passes, one thirty-second of a unit.
///
/// It is the same number `engine::trace` keeps a swept hull short of a brush
/// by, and it is used the same way: a slab entered within `DIST_EPSILON` of
/// its face is treated as entered *at* the face.
const DIST_EPSILON: f32 = 0.03125;

/// Does the box `mins`-`maxs`, swept from `start` to `end`, touch the box
/// `obb_mins`-`obb_maxs` placed at `origin` under `angles`?
///
/// `mins`/`maxs` are relative to the swept box's position, so a standing
/// player hull is `(-16,-16,0)`-`(16,16,72)` — the same convention
/// [`TouchQuery::brush_models_touching`](super::TouchQuery::brush_models_touching)
/// takes, and for the same reason: it is `Ray_t::Init`'s.
///
/// > **`Ray_t`'s start is the centre of the box and `start` is not.**
/// > `Ray_t::Init` (`public/cmodel.h:88`) offsets the start by
/// > `(mins + maxs) / 2` and keeps half-extents, so for a player the ray
/// > actually swept begins 36 units above the feet. This function does that
/// > conversion itself, which is why it takes the hull rather than a ray —
/// > the identical trap `rustdocs/ENGINE.md` records for `engine::trace::Ray`
/// > is not reachable from here.
// Eight arguments, and they are the question: two boxes and a sweep. The
// alternative is a pair of structs that exist only to be destructured at the
// one call site, and the argument order already matches
// `TouchQuery::brush_models_touching`'s.
#[allow(clippy::too_many_arguments)]
pub fn swept_box_touches_obb(
    start: Vec3,
    end: Vec3,
    mins: Vec3,
    maxs: Vec3,
    origin: Vec3,
    angles: Vec3,
    obb_mins: Vec3,
    obb_maxs: Vec3,
) -> bool {
    // `Ray_t::Init`. `m_IsRay` is deliberately not reproduced: Valve routes a
    // zero-extent ray to a cheaper two-transform function, and the answer is
    // the same one. See [`ray_touches_obb`] for why that is not just a hope.
    let extents = (maxs - mins) * 0.5;
    let ray_start = start + (mins + maxs) * 0.5;
    let delta = end - start;

    // `IntersectRayWithOBB( ray, origin, angles, … )` (`:1685`): an exactly
    // unrotated box takes the axis-aligned path, which is not an optimisation
    // — it is a different function with different arithmetic. **42 of the
    // game's 65 `prop_floor_button`s are at `angles "0 0 0"` and take it**;
    // the other 23 do not, and two of them are not even at right angles.
    if angles == Vec3::ZERO {
        return box_touches_box(
            ray_start,
            delta,
            extents,
            origin + obb_mins,
            origin + obb_maxs,
        );
    }

    let rotation = angle_matrix(angles);
    ray_touches_obb(ray_start, delta, extents, origin, rotation, obb_mins, obb_maxs)
}

/// `IntersectRayWithBox( ray, boxMins, boxMaxs, … )` (`:1265`) and the
/// `BoxTraceInfo_t` overload under it (`:1131`), for a box ray.
///
/// The swept box is shrunk to its centre and the target box is bloated by the
/// half-extents — the standard Minkowski trick, and Valve's.
fn box_touches_box(ray_start: Vec3, delta: Vec3, extents: Vec3, mins: Vec3, maxs: Vec3) -> bool {
    let (mins, maxs) = (mins - extents, maxs + extents);

    let mut t1 = -1.0f32;
    let mut t2 = 1.0f32;
    // "UNDONE: This makes this code a little messy" — Valve's. It starts
    // asserted and is cleared by the first slab the start is outside of.
    let mut start_solid = true;

    for i in 0..6 {
        let (d1, d2) = match i >= 3 {
            true => {
                let d1 = ray_start[i - 3] - maxs[i - 3];
                (d1, d1 + delta[i - 3])
            }
            false => {
                let d1 = -ray_start[i] + mins[i];
                (d1, d1 - delta[i])
            }
        };

        // Completely in front of this face for the whole sweep.
        if d1 > 0.0 && d2 > 0.0 {
            return false;
        }
        // Completely behind it for the whole sweep; this slab says nothing.
        if d1 <= 0.0 && d2 <= 0.0 {
            continue;
        }
        if d1 > 0.0 {
            start_solid = false;
        }

        if d1 > d2 {
            // Entering.
            let f = (d1 - DIST_EPSILON).max(0.0) / (d1 - d2);
            if f > t1 {
                t1 = f;
            }
        } else {
            // Leaving.
            let f = (d1 + DIST_EPSILON) / (d1 - d2);
            if f < t2 {
                t2 = f;
            }
        }
    }

    start_solid || (t1 < t2 && t1 >= 0.0)
}

/// `IntersectRayWithOBB( ray, matOBBToWorld, … )` (`:1477`) — the box-ray
/// half, which is a separating-axis sweep written as a slab clip.
///
/// Valve's own description is the clearest one there is: *"we're going to do
/// the GJK thing explicitly. We'll shrink the ray down to a point, and bloat
/// the OBB by the ray's extents. This will generate facet planes which are
/// perpendicular to all of the separating axes typically seen in a standard
/// separating axis implementation."*
///
/// Fifteen planes, in OBB space, in the order the original builds them:
/// **0-2** the OBB's own faces, **3-5** the three world axes, and **6-14** the
/// nine cross products of an OBB axis with a world axis. Each is bloated by
/// the swept box's extent along it — the OBB-face planes and the cross planes
/// measure that in *world* space, because the extents are a world-space box.
///
/// > **A zero-extent ray would give the same answer here**, which is why
/// > [`swept_box_touches_obb`] does not reproduce `Ray_t::m_IsRay`'s branch to
/// > the cheaper function. With no bloat the twelve extra planes are the
/// > OBB's own *supporting* planes along those directions, so each of the
/// > fifteen slabs contains the OBB exactly and their intersection is the OBB
/// > itself — the same interval planes 0-2 alone would have given.
#[allow(clippy::too_many_arguments)]
fn ray_touches_obb(
    ray_start: Vec3,
    delta: Vec3,
    extents: Vec3,
    origin: Vec3,
    rotation: Mat3,
    obb_mins: Vec3,
    obb_maxs: Vec3,
) -> bool {
    // > **The trivial reject is centred on the wrong point, and it is Valve's.**
    // > `:1483` adds the *local* box centre straight onto the translation
    // > column without rotating it, so for a box whose centre is not its
    // > origin the sphere sits somewhere the box is not. It is conservative in
    // > practice — the radius carries the box's whole half-diagonal plus the
    // > ray's — and it is reproduced rather than fixed because a sphere that
    // > rejects differently is a touch that fires differently.
    let centre = (obb_mins + obb_maxs) * 0.5 + origin;
    let radius = ((obb_maxs - obb_mins) * 0.5).length() + extents.length();
    if !ray_intersects_sphere(ray_start, delta, centre, radius, DIST_EPSILON) {
        return false;
    }

    // Into the OBB's frame. `VectorITransform` and `VectorIRotate` — the
    // transpose is the inverse, there being no scale.
    let inverse = rotation.transpose();
    let local_start = inverse * (ray_start - origin);
    let local_delta = inverse * delta;
    let local_end = local_start + local_delta;

    let mut normal = [Vec3::ZERO; 15];
    // `[i][0]` is the near plane and `[i][1]` the far one, in the original's
    // sense: the near one is compared negated.
    let mut dist = [[0.0f32; 2]; 15];

    /// `s_ExtIndices` (`:1470`) — which two of the ray's extents pair with
    /// each world axis in the cross-product planes.
    const EXT: [[usize; 2]; 3] = [[2, 1], [0, 2], [0, 1]];
    /// `s_MatIndices` (`:1475`) — and which two rows of the matrix.
    const MAT: [[usize; 2]; 3] = [[1, 2], [2, 0], [1, 0]];

    for i in 0..3 {
        // 0-2: the OBB's own faces. The bloat is the swept box's extent along
        // the *world* direction of this local axis, which is column `i`.
        normal[i] = Vec3::ZERO;
        normal[i][i] = 1.0;
        let axis = rotation.col(i);
        let bloat = (axis.x * extents.x).abs()
            + (axis.y * extents.y).abs()
            + (axis.z * extents.z).abs();
        dist[i][0] = obb_mins[i] - bloat;
        dist[i][1] = obb_maxs[i] + bloat;

        // 3-5: the three world axes, written in OBB space — which is row `i`
        // of the matrix. Their bloat is one extent each.
        let row = rotation.row(i);
        normal[i + 3] = row;
        dist[i + 3] = support_map(row, obb_mins, obb_maxs);
        dist[i + 3][0] -= extents[i];
        dist[i + 3][1] += extents[i];

        // 6-14: an OBB axis crossed with world axis `i`. `row` is that world
        // axis in OBB space, so the local X, Y and Z axes cross with it to
        // give three planes each carrying two of the extents.
        let ext0 = extents[EXT[i][0]];
        let ext1 = extents[EXT[i][1]];
        let mat0 = rotation.row(MAT[i][0]);
        let mat1 = rotation.row(MAT[i][1]);

        for (slot, plane, axes, bloat) in [
            (
                6,
                Vec3::new(0.0, -row.z, row.y),
                [1, 2],
                mat0.x.abs() * ext0 + mat1.x.abs() * ext1,
            ),
            (
                9,
                Vec3::new(row.z, 0.0, -row.x),
                [0, 2],
                mat0.y.abs() * ext0 + mat1.y.abs() * ext1,
            ),
            (
                12,
                Vec3::new(-row.y, row.x, 0.0),
                [0, 1],
                mat0.z.abs() * ext0 + mat1.z.abs() * ext1,
            ),
        ] {
            let n = slot + i;
            normal[n] = plane;
            dist[n] = support_map_2d(plane, axes, obb_mins, obb_maxs);
            dist[n][0] -= bloat;
            dist[n][1] += bloat;
        }
    }

    let mut enter = -1.0f32;
    let mut leave = 1.0f32;
    let mut start_solid = true;

    for i in 0..15 {
        let start_dot = normal[i].dot(local_start);
        let end_dot = normal[i].dot(local_end);
        // "Negative here is because the plane normal + dist are defined in
        // negative terms for the far plane (plane dist index 0)" — Valve's.
        let pairs = [
            (-(start_dot - dist[i][0]), -(end_dot - dist[i][0])),
            (start_dot - dist[i][1], end_dot - dist[i][1]),
        ];

        for (d1, d2) in pairs {
            if d1 > 0.0 && d2 > 0.0 {
                return false;
            }
            if d1 <= 0.0 && d2 <= 0.0 {
                continue;
            }
            if d1 > 0.0 {
                start_solid = false;
            }
            let denominator = 1.0 / (d1 - d2);
            if d1 > d2 {
                let f = (d1 - DIST_EPSILON).max(0.0) * denominator;
                if f > enter {
                    enter = f;
                }
            } else {
                let f = (d1 + DIST_EPSILON) * denominator;
                if f < leave {
                    leave = f;
                }
            }
        }
    }

    (enter < leave && enter >= 0.0) || start_solid
}

/// `ComputeSupportMap` (`:1723`) — how far the box reaches along `direction`,
/// least first.
fn support_map(direction: Vec3, mins: Vec3, maxs: Vec3) -> [f32; 2] {
    let mut out = [0.0f32; 2];
    for axis in 0..3 {
        let high = usize::from(direction[axis] > 0.0);
        out[high] += maxs[axis] * direction[axis];
        out[1 - high] += mins[axis] * direction[axis];
    }
    out
}

/// `ComputeSupportMap`'s two-axis overload (`:1740`). The cross-product planes
/// have a zero component, and the original skips it rather than multiplying by
/// it.
fn support_map_2d(direction: Vec3, axes: [usize; 2], mins: Vec3, maxs: Vec3) -> [f32; 2] {
    let mut out = [0.0f32; 2];
    for axis in axes {
        let high = usize::from(direction[axis] > 0.0);
        out[high] += maxs[axis] * direction[axis];
        out[1 - high] += mins[axis] * direction[axis];
    }
    out
}

/// `IsRayIntersectingSphere` (`:375`) — the closest point on the *segment*,
/// against the radius. `t` is clamped to `[0, 1]`, so this is a capsule test
/// rather than an infinite-line one.
fn ray_intersects_sphere(start: Vec3, delta: Vec3, centre: Vec3, radius: f32, tolerance: f32) -> bool {
    let radius = radius + tolerance;
    let to_centre = centre - start;
    let numerator = to_centre.dot(delta);

    let t = if numerator <= 0.0 {
        0.0
    } else {
        let denominator = delta.dot(delta);
        match numerator > denominator {
            true => 1.0,
            false => numerator / denominator,
        }
    };

    (start + t * delta - centre).length_squared() <= radius * radius
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A standing player hull, the one thing that touches an OBB trigger in
    /// this port.
    const MINS: Vec3 = Vec3::new(-16.0, -16.0, 0.0);
    const MAXS: Vec3 = Vec3::new(16.0, 16.0, 72.0);

    /// `CPropFloorButton::CreateTriggers` (`prop_floor_button.cpp:461`).
    const BUTTON_MINS: Vec3 = Vec3::new(-20.0, -20.0, 0.0);
    const BUTTON_MAXS: Vec3 = Vec3::new(20.0, 20.0, 14.0);

    fn standing_on(feet: Vec3, button: Vec3, angles: Vec3) -> bool {
        swept_box_touches_obb(
            feet,
            feet,
            MINS,
            MAXS,
            button,
            angles,
            BUTTON_MINS,
            BUTTON_MAXS,
        )
    }

    /// The whole point of the module: a player standing on the pad is inside
    /// it, and one standing a body's width away is not.
    #[test]
    fn a_player_standing_on_the_pad_is_touching_it() {
        let button = Vec3::new(100.0, 200.0, 64.0);
        assert!(standing_on(button, button, Vec3::ZERO));

        // The pad is 40 across and the hull 32, so the two stop overlapping
        // at 36 units of separation.
        assert!(standing_on(button + Vec3::new(35.0, 0.0, 0.0), button, Vec3::ZERO));
        assert!(!standing_on(
            button + Vec3::new(37.0, 0.0, 0.0),
            button,
            Vec3::ZERO
        ));
    }

    /// Turning a square pad 45 degrees moves its *vertex* out onto the axis
    /// and pulls its *face* in onto the diagonal — so the same probe can be
    /// inside one orientation and outside the other, in both directions. This
    /// is the pair the axis-aligned early-out cannot answer.
    #[test]
    fn turning_the_pad_turns_what_it_notices() {
        let button = Vec3::ZERO;
        let turned = Vec3::new(0.0, 45.0, 0.0);

        // Straight out along +X the unrotated pad reaches 20 and the turned
        // one reaches its corner at 28.3, so the hull loses the first at 36
        // units out and keeps the second to 44.3.
        let along_x = Vec3::new(40.0, 0.0, 0.0);
        assert!(!standing_on(along_x, button, Vec3::ZERO));
        assert!(standing_on(along_x, button, turned));

        // Out along the diagonal it is the other way round: the turned pad
        // presents a face there and reaches only 20.
        let along_diagonal = Vec3::new(33.0, 33.0, 0.0);
        assert!(standing_on(along_diagonal, button, Vec3::ZERO));
        assert!(!standing_on(along_diagonal, button, turned));
    }

    /// Yaw alone is enough to take the fifteen-plane path, and it had better
    /// agree with the axis-aligned one on the case where the two coincide: a
    /// square turned by a right angle is the same square.
    #[test]
    fn a_right_angle_yaw_agrees_with_the_axis_aligned_path() {
        let button = Vec3::new(-624.0, 4432.0, 2680.0);
        for x in -60..=60 {
            for y in -60..=60 {
                let feet = button + Vec3::new(x as f32 * 1.5, y as f32 * 1.5, 0.0);
                let flat = standing_on(feet, button, Vec3::ZERO);
                let turned = standing_on(feet, button, Vec3::new(0.0, 90.0, 0.0));
                assert_eq!(flat, turned, "at {feet} the two paths disagree");
            }
        }
    }

    /// The sweep is what stops a fast player crossing a thin trigger between
    /// two ticks — at 64 Hz and 175 units a second a walking player moves 2.7
    /// units a tick, but a `trigger_push` can throw one much faster than that.
    #[test]
    fn a_sweep_notices_what_a_position_test_would_miss() {
        let button = Vec3::new(0.0, 0.0, 0.0);
        let before = Vec3::new(-400.0, 0.0, 0.0);
        let after = Vec3::new(400.0, 0.0, 0.0);

        assert!(!standing_on(before, button, Vec3::ZERO));
        assert!(!standing_on(after, button, Vec3::ZERO));
        assert!(swept_box_touches_obb(
            before,
            after,
            MINS,
            MAXS,
            button,
            Vec3::ZERO,
            BUTTON_MINS,
            BUTTON_MAXS,
        ));
        // …and the same sweep past one side of it does not.
        assert!(!swept_box_touches_obb(
            before + Vec3::new(0.0, 60.0, 0.0),
            after + Vec3::new(0.0, 60.0, 0.0),
            MINS,
            MAXS,
            button,
            Vec3::ZERO,
            BUTTON_MINS,
            BUTTON_MAXS,
        ));
    }

    /// The pad is 14 units tall and sits at the player's feet, so a player
    /// floating above it touches nothing — which is how a button releases
    /// when you jump off it rather than when you walk away.
    #[test]
    fn height_matters_and_the_pad_is_fourteen_units_tall() {
        let button = Vec3::new(0.0, 0.0, 0.0);
        assert!(standing_on(button + Vec3::new(0.0, 0.0, 13.0), button, Vec3::ZERO));
        assert!(!standing_on(button + Vec3::new(0.0, 0.0, 15.0), button, Vec3::ZERO));
        // Below it, the 72-unit hull still reaches up into the pad.
        assert!(standing_on(button - Vec3::new(0.0, 0.0, 70.0), button, Vec3::ZERO));
        assert!(!standing_on(button - Vec3::new(0.0, 0.0, 73.0), button, Vec3::ZERO));
    }

    /// The separating-axis test the fifteen planes *are*, written out
    /// independently: an axis-aligned box against an oriented one, the
    /// textbook fifteen axes, no bloat and no slabs.
    ///
    /// Valve's version is the same test folded into a swept slab clip, and
    /// for a stationary query the two have to agree exactly. That is what
    /// makes the fifteen-plane path checkable at all — reading it back out of
    /// the original proves only that it was transcribed.
    fn separated(probe: Vec3, hull: Vec3, centre: Vec3, rotation: Mat3, extents: Vec3) -> bool {
        let delta = centre - probe;
        let axes = [Vec3::X, Vec3::Y, Vec3::Z];
        let mut candidates: Vec<Vec3> = Vec::with_capacity(15);
        candidates.extend(axes);
        candidates.extend((0..3).map(|i| rotation.col(i)));
        for a in axes {
            for i in 0..3 {
                candidates.push(a.cross(rotation.col(i)));
            }
        }
        candidates.into_iter().any(|axis| {
            if axis.length_squared() < 1e-12 {
                return false;
            }
            let reach_hull = hull.x * axis.x.abs() + hull.y * axis.y.abs() + hull.z * axis.z.abs();
            let reach_box: f32 = (0..3)
                .map(|i| extents[i] * axis.dot(rotation.col(i)).abs())
                .sum();
            delta.dot(axis).abs() > reach_hull + reach_box + 1e-3
        })
    }

    /// A pad turned about all three axes, over a grid, against that reference.
    /// The angles are the worst case the shipped game actually places: one
    /// `prop_floor_button` in `sp_a4_finale1` sits at `44.9997 0 90.0004`.
    #[test]
    fn the_fifteen_planes_are_the_separating_axis_test() {
        let button = Vec3::new(12.0, -34.0, 56.0);
        let angles = Vec3::new(44.9997, 0.0, 90.0004);
        let rotation = angle_matrix(angles);
        // The OBB in centre-and-half-extent form, which is what SAT wants.
        let box_centre = button + rotation * ((BUTTON_MINS + BUTTON_MAXS) * 0.5);
        let box_extents = (BUTTON_MAXS - BUTTON_MINS) * 0.5;
        let hull_extents = (MAXS - MINS) * 0.5;

        let mut inside = 0;
        for x in -8..=8 {
            for y in -8..=8 {
                for z in -5..=5 {
                    let feet = button + Vec3::new(x as f32 * 7.0, y as f32 * 7.0, z as f32 * 13.0);
                    let got = standing_on(feet, button, angles);
                    let hull_centre = feet + (MINS + MAXS) * 0.5;
                    let want = !separated(hull_centre, hull_extents, box_centre, rotation, box_extents);
                    assert_eq!(got, want, "at {feet}: the sweep and the SAT disagree");
                    inside += usize::from(got);
                }
            }
        }
        assert!(inside > 50, "only {inside} of 3,179 probes were inside the pad");
    }

    /// The sphere reject must never be the thing that says no. A box the
    /// sweep genuinely reaches has to survive it whatever the box's centre
    /// offset does to Valve's mis-centred sphere.
    #[test]
    fn the_trivial_reject_never_rejects_a_real_touch() {
        let angles = Vec3::new(30.0, 40.0, 50.0);
        let rotation = angle_matrix(angles);
        let centre = (BUTTON_MINS + BUTTON_MAXS) * 0.5;
        let radius = ((BUTTON_MAXS - BUTTON_MINS) * 0.5).length() + ((MAXS - MINS) * 0.5).length();

        for x in -4..=4 {
            for y in -4..=4 {
                for z in -4..=4 {
                    let feet = Vec3::new(x as f32 * 11.0, y as f32 * 11.0, z as f32 * 11.0);
                    if !standing_on(feet, Vec3::ZERO, angles) {
                        continue;
                    }
                    let ray_start = feet + (MINS + MAXS) * 0.5;
                    assert!(
                        ray_intersects_sphere(ray_start, Vec3::ZERO, centre, radius, DIST_EPSILON),
                        "the sphere at {centre} rejected a touch at {feet}",
                    );
                    // …and the true centre is inside it too, which is the
                    // margin that makes Valve's mis-centring harmless.
                    let true_centre = rotation * centre;
                    assert!((true_centre - centre).length() < radius);
                }
            }
        }
    }
}
