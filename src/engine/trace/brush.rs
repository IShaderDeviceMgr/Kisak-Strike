//! Clipping a swept box against one convex brush.
//!
//! `CM_ClipBoxToBrush`, `CM_TestBoxInBrush` and `IntersectRayWithBoxBrush`
//! (`engine/cmodel.cpp:1511`, `:1698`, `:935`). This is the algorithm the whole
//! module is defined against — `portdocs/ENGINE_TRACE.md` §4.4.

use glam::Vec3;

use super::model::{BrushSides, CBrushSide};
use super::{Contents, Work, DIST_EPSILON, NEVER_UPDATED};

/// Sweeps the box against one brush, keeping the hit if it is nearer than
/// whatever the trace has already found.
///
/// `IS_POINT` is Valve's template parameter, not a convenience: a point trace
/// skips bevel planes and maintains `fraction_left_solid`, a box trace pushes
/// every plane out by the box's extent and does neither.
pub(super) fn clip_box_to_brush<const IS_POINT: bool>(work: &mut Work<'_>, brush_index: usize) {
    let bsp = work.bsp;
    let brush = bsp.brushes[brush_index];

    let (first, count) = match brush.sides {
        BrushSides::Box(index) => {
            intersect_ray_with_box_brush::<IS_POINT>(work, brush.contents, index as usize);
            return;
        }
        BrushSides::Planes { first, count } => (first as usize, count as usize),
    };
    if count == 0 {
        return;
    }

    let p1 = work.start;
    let p2 = work.end;

    let mut enter_frac = NEVER_UPDATED;
    let mut leave_frac = 1.0f32;
    let mut get_out = false;
    let mut start_out = false;
    let mut lead_side: Option<&CBrushSide> = None;

    for side in &bsp.brush_sides[first..first + count] {
        let plane = &bsp.planes[side.plane as usize];

        let dist = if IS_POINT {
            // Bevel planes exist so the box case below is exact. They are
            // redundant for a point and clipping against them would report
            // impacts on planes that are not surfaces.
            if side.bevel {
                continue;
            }
            plane.dist
        } else {
            // The Minkowski expansion, done analytically: pushing the plane
            // out by the box's extent along the normal is the same as sweeping
            // the box, *because* VBSP emitted the bevel planes that make it so.
            plane.dist + plane.normal.abs().dot(work.extents)
        };

        let d1 = p1.dot(plane.normal) - dist;
        let d2 = p2.dot(plane.normal) - dist;

        if d1 > 0.0 {
            start_out = true;
            // In front of this plane at both ends, so outside the brush.
            if d2 > 0.0 {
                return;
            }
        } else {
            // Behind at both ends: this plane cannot be the one hit.
            if d2 <= 0.0 {
                continue;
            }
            get_out = true;
        }

        if d1 > d2 {
            // Entering. The epsilon is subtracted before the divide and
            // clamped at zero, because a short trace can otherwise produce a
            // large negative fraction.
            let f = (d1 - DIST_EPSILON).max(0.0) / (d1 - d2);
            if f > enter_frac {
                enter_frac = f;
                lead_side = Some(side);
            }
        } else {
            let f = (d1 + DIST_EPSILON) / (d1 - d2);
            if f < leave_frac {
                leave_frac = f;
            }
        }
    }

    // Entered this brush *after* leaving the previous one, so the sweep was
    // outside in between and did not start solid after all.
    if IS_POINT && start_out && (work.trace.fraction_left_solid - enter_frac) > 0.0 {
        start_out = false;
    }

    if !start_out {
        work.trace.start_solid = true;
        work.trace.contents = brush.contents;

        if !get_out {
            work.trace.all_solid = true;
            work.trace.fraction = 0.0;
            work.trace.fraction_left_solid = 1.0;
        } else if leave_frac != 1.0 && leave_frac > work.trace.fraction_left_solid {
            // `leave_frac == 1` means it was never updated; the all-solid case
            // is the branch above.
            work.trace.fraction_left_solid = leave_frac;
            // A previous brush may have left us not-solid at a nearer
            // fraction; that hit is behind where this brush ends, so drop it.
            if work.trace.fraction <= leave_frac {
                work.trace.fraction = 1.0;
                work.trace.surface = None;
                work.trace.surface_flags = 0;
            }
        }
        return;
    }

    // Nothing was hit until the sweep had already left.
    if enter_frac < leave_frac && enter_frac > NEVER_UPDATED && enter_frac < work.trace.fraction {
        let side = lead_side.expect("an enter fraction always records its side");
        let plane = &bsp.planes[side.plane as usize];
        let (surface, flags) = bsp.surface_at(side.surface);

        work.trace.fraction = enter_frac.max(0.0);
        work.trace.normal = plane.normal;
        work.trace.plane_dist = plane.dist;
        work.trace.surface = surface;
        work.trace.surface_flags = flags;
        work.trace.contents = brush.contents;
    }
}

/// The same sweep against an axis-aligned brush: a slab test rather than a
/// plane loop.
///
/// `IntersectRayWithBoxBrush` (`engine/cmodel.cpp:935`), which is SIMD there
/// and scalar here — `PORTING.md` on hand-written SIMD. Two passes over the
/// axes are Valve's: the first decides whether there is an intersection at
/// all, the second repeats the arithmetic with the box grown by
/// `DIST_EPSILON` and tracks which face won.
fn intersect_ray_with_box_brush<const IS_POINT: bool>(
    work: &mut Work<'_>,
    contents: Contents,
    box_index: usize,
) {
    let bsp = work.bsp;
    let brush = bsp.box_brushes[box_index];

    // Relocate so the sweep starts at the origin, and grow the box by the
    // sweeping box's extents — the Minkowski sum, which for two AABBs is this.
    let mut offset_mins = [0.0f32; 3];
    let mut offset_maxs = [0.0f32; 3];
    let mut start_out_mins = [false; 3];
    let mut start_out_maxs = [false; 3];
    let mut cross_plane = [false; 3];

    for i in 0..3 {
        offset_mins[i] = brush.mins[i] - work.start[i] - work.extents[i];
        offset_maxs[i] = brush.maxs[i] - work.start[i] + work.extents[i];

        start_out_mins[i] = 0.0 < offset_mins[i];
        start_out_maxs[i] = 0.0 > offset_maxs[i];
        let end_out_mins = work.delta[i] < offset_mins[i];
        let end_out_maxs = work.delta[i] > offset_maxs[i];

        // Both ends outside the same slab: no intersection, on any axis.
        if (start_out_mins[i] && end_out_mins) || (start_out_maxs[i] && end_out_maxs) {
            return;
        }
        cross_plane[i] = (start_out_mins[i] != end_out_mins) || (start_out_maxs[i] != end_out_maxs);
    }

    // Only the axes a plane is actually crossed on constrain the interval;
    // the others are masked to an interval that can never win.
    let interval = |mins: &[f32; 3], maxs: &[f32; 3], i: usize| -> (f32, f32) {
        if !cross_plane[i] {
            return (-f32::MAX, f32::MAX);
        }
        let t0 = mins[i] * work.inv_delta[i];
        let t1 = maxs[i] * work.inv_delta[i];
        (t0.min(t1), t0.max(t1))
    };

    let mut first_out = f32::MAX;
    let mut last_in = -f32::MAX;
    for i in 0..3 {
        let (mint, maxt) = interval(&offset_mins, &offset_maxs, i);
        first_out = first_out.min(maxt);
        last_in = last_in.max(mint);
    }
    if last_in.max(0.0) > first_out.min(1.0) {
        return;
    }

    let mut start_solid = !(0..3).any(|i| start_out_mins[i] || start_out_maxs[i]);

    // Second pass, with the epsilon, tracking the face. Growing the box is how
    // this path stops short of the surface; the plane path subtracts the
    // epsilon from the fraction instead, and the two agree.
    let mut first_out = f32::MAX;
    let mut last_in = -f32::MAX;
    let mut face = 0usize;
    let mins: [f32; 3] = std::array::from_fn(|j| offset_mins[j] - DIST_EPSILON);
    let maxs: [f32; 3] = std::array::from_fn(|j| offset_maxs[j] + DIST_EPSILON);
    for i in 0..3 {
        let t0 = mins[i] * work.inv_delta[i];
        let t1 = maxs[i] * work.inv_delta[i];
        // `g_CubeFaceIndex0`/`1` (`engine/cmodel.cpp:933`): the minimum face
        // when the interval runs forwards, the maximum face when it is
        // reversed.
        let face_id = if t0 <= t1 { i } else { i + 3 };
        let (mint, maxt) = interval(&mins, &maxs, i);
        first_out = first_out.min(maxt);
        // `>=` and not `>`: Valve's max-of-three keeps the *later* axis on a
        // tie, and which face a corner impact reports follows from it.
        if mint >= last_in {
            last_in = mint;
            face = face_id;
        }
    }
    let first_out = first_out.min(1.0);
    let last_in = last_in.max(0.0);
    if last_in > first_out {
        return;
    }

    // Copied from the plane case "to avoid hitting an assert and overwriting a
    // previous start solid with a new shorter fraction".
    if !start_solid && IS_POINT && work.trace.fraction_left_solid > last_in {
        start_solid = true;
    }

    if start_solid {
        work.trace.start_solid = true;
        work.trace.contents = contents;
        if first_out >= 1.0 {
            work.trace.all_solid = true;
            work.trace.fraction = 0.0;
            // NOTE: Valve does *not* set `fractionleftsolid` here, where the
            // plane path sets it to 1. Ported as written — the asymmetry
            // decides what `CM_ComputeTraceEndpoints` reports as `start` for a
            // sweep that begins inside an axial brush, and inventing symmetry
            // would be a silent divergence rather than a fix.
        } else if first_out > work.trace.fraction_left_solid {
            work.trace.fraction_left_solid = first_out;
            if work.trace.fraction <= first_out {
                work.trace.fraction = 1.0;
                work.trace.surface = None;
                work.trace.surface_flags = 0;
            }
        }
    } else if last_in < work.trace.fraction {
        let (surface, flags) = bsp.surface_at(brush.surfaces[face]);
        work.trace.fraction = last_in;
        work.trace.surface = surface;
        work.trace.surface_flags = flags;

        let mut normal = Vec3::ZERO;
        if face >= 3 {
            let axis = face - 3;
            normal[axis] = 1.0;
            work.trace.plane_dist = brush.maxs[axis];
        } else {
            normal[face] = -1.0;
            work.trace.plane_dist = -brush.mins[face];
        }
        work.trace.normal = normal;
        work.trace.contents = contents;
    }
}

/// Whether the box, unswept, is inside this brush — the position test.
///
/// `CM_TestBoxInBrush` (`engine/cmodel.cpp:1698`). Note the asymmetry with the
/// sweep: this pulls the plane *in* by the corner facing it rather than
/// pushing it out by the extent, because there is no direction of travel to
/// expand along.
pub(super) fn test_box_in_brush(work: &mut Work<'_>, brush_index: usize) {
    let bsp = work.bsp;
    let brush = bsp.brushes[brush_index];

    match brush.sides {
        BrushSides::Box(index) => {
            // `IsTraceBoxIntersectingBoxBrush` (`engine/cmodel.cpp:1677`).
            let other = bsp.box_brushes[index as usize];
            let mins = work.start - work.extents;
            let maxs = work.start + work.extents;
            for i in 0..3 {
                if other.mins[i].max(mins[i]) > other.maxs[i].min(maxs[i]) {
                    return;
                }
            }
        }
        BrushSides::Planes { first, count } => {
            if count == 0 {
                return;
            }
            for side in &bsp.brush_sides[first as usize..(first + count) as usize] {
                let plane = &bsp.planes[side.plane as usize];
                // The corner of the box facing this plane.
                let corner = Vec3::from(std::array::from_fn(|j| {
                    if plane.normal[j] < 0.0 {
                        work.extents[j]
                    } else {
                        -work.extents[j]
                    }
                }));
                let dist = plane.dist - corner.dot(plane.normal);
                if work.start.dot(plane.normal) - dist > 0.0 {
                    return;
                }
            }
        }
    }

    work.trace.start_solid = true;
    work.trace.all_solid = true;
    work.trace.fraction = 0.0;
    work.trace.fraction_left_solid = 1.0;
    work.trace.contents = brush.contents;
}
