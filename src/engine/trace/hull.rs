//! Walking the BSP tree with a swept box.
//!
//! `CM_RecursiveHullCheck`, `CM_TraceToLeaf`, `CM_TraceToBrushList`,
//! `CM_TestInLeaf` and `CM_BoxLeafnums` (`engine/cmodel.cpp:2555`, `:2064`,
//! `:1952`, `:2202`, `:552`). See `portdocs/ENGINE_TRACE.md` §4.3.

use glam::Vec3;

use super::brush::{clip_box_to_brush, test_box_in_brush};
use super::model::BrushSides;
use super::{Contents, Work, DIST_EPSILON};

/// Sweeps the box from `p1` to `p2` through the subtree at `num`, which is a
/// node when non-negative and the leaf `-1 - num` when negative.
///
/// `p1f`/`p2f` are those points as fractions of the *whole* sweep, which is
/// what makes `trace.fraction` comparable across the recursion.
pub(super) fn recursive_hull_check<const IS_POINT: bool>(
    work: &mut Work<'_>,
    mut num: i32,
    p1f: f32,
    p2f: f32,
    p1: Vec3,
    p2: Vec3,
) {
    if work.trace.fraction <= p1f {
        return; // already hit something nearer
    }

    let bsp = work.bsp;

    // Descend while the whole box is on one side of the plane. No recursion
    // and no stack for the common case, which is most of the tree.
    let (node, t1, t2, offset) = loop {
        if num < 0 {
            trace_to_leaf::<IS_POINT>(work, (-1 - num) as usize);
            return;
        }
        let node = bsp.nodes[num as usize];
        let plane = &bsp.planes[node.plane as usize];

        let (t1, t2, offset) = match plane.axis {
            Some(axis) => (
                p1[axis] - plane.dist,
                p2[axis] - plane.dist,
                work.extents[axis],
            ),
            None => (
                plane.normal.dot(p1) - plane.dist,
                plane.normal.dot(p2) - plane.dist,
                match IS_POINT {
                    true => 0.0,
                    // The box's extent projected onto the normal.
                    false => plane.normal.abs().dot(work.extents),
                },
            ),
        };

        if t1 > offset && t2 > offset {
            num = node.children[0];
            continue;
        }
        if t1 < -offset && t2 < -offset {
            num = node.children[1];
            continue;
        }
        break (node, t1, t2, offset);
    };

    // The sweep straddles this plane. Split it — and overlap the two halves by
    // `DIST_EPSILON`, so a brush sitting on the plane is reached from both
    // sides rather than missed by both.
    let (side, frac, frac2) = if t1 < t2 {
        let idist = 1.0 / (t1 - t2);
        (
            1usize,
            (t1 - offset - DIST_EPSILON) * idist,
            (t1 + offset + DIST_EPSILON) * idist,
        )
    } else if t1 > t2 {
        let idist = 1.0 / (t1 - t2);
        (
            0usize,
            (t1 + offset + DIST_EPSILON) * idist,
            (t1 - offset - DIST_EPSILON) * idist,
        )
    } else {
        (0usize, 1.0, 0.0)
    };

    // The near half, up to the plane.
    let frac = frac.clamp(0.0, 1.0);
    let midf = p1f + (p2f - p1f) * frac;
    let mid = p1.lerp(p2, frac);
    recursive_hull_check::<IS_POINT>(work, node.children[side], p1f, midf, p1, mid);

    // The far half, from the plane on.
    let frac2 = frac2.clamp(0.0, 1.0);
    let midf = p1f + (p2f - p1f) * frac2;
    let mid = p1.lerp(p2, frac2);
    recursive_hull_check::<IS_POINT>(work, node.children[side ^ 1], midf, p2f, mid, p2);
}

/// Clips against every brush in one leaf.
fn trace_to_leaf<const IS_POINT: bool>(work: &mut Work<'_>, leaf_index: usize) {
    let leaf = work.bsp.leaves[leaf_index];
    if leaf.num_leaf_brushes == 0 {
        return;
    }
    let first = leaf.first_leaf_brush as usize;
    let count = leaf.num_leaf_brushes as usize;

    for i in first..first + count {
        let brush_index = work.bsp.leaf_brushes[i] as usize;

        // A brush spans many leaves; without this it is clipped once per leaf,
        // which is both wasted work and wrong for `fraction_left_solid`.
        if !work.visit(brush_index) {
            continue;
        }

        let brush = work.bsp.brushes[brush_index];
        let relevant = brush.contents.and(work.contents);
        if relevant.is_empty() {
            continue;
        }
        if relevant == Contents::OPAQUE
            && work.contents.intersects(Contents::IGNORE_NODRAW_OPAQUE)
            && brush_is_nodraw(work, brush_index)
        {
            // Hit only because it is opaque, and it is a nodraw blocklight
            // brush: the caller asked not to see those.
            continue;
        }

        clip_box_to_brush::<IS_POINT>(work, brush_index);
        if work.trace.fraction == 0.0 {
            return;
        }
    }
}

/// Whether every side of a brush is `SURF_NODRAW` — the
/// `CONTENTS_IGNORE_NODRAW_OPAQUE` test (`engine/cmodel.cpp:1872`).
fn brush_is_nodraw(work: &Work<'_>, brush_index: usize) -> bool {
    use crate::engine::world::bsp::surf;

    let bsp = work.bsp;
    let nodraw = |surface: u16| bsp.surface_at(surface).1 & surf::NODRAW != 0;

    match bsp.brushes[brush_index].sides {
        BrushSides::Box(index) => bsp.box_brushes[index as usize]
            .surfaces
            .iter()
            .any(|&s| nodraw(s)),
        BrushSides::Planes { first, count } => bsp.brush_sides
            [first as usize..(first + count) as usize]
            .iter()
            .any(|side| nodraw(side.surface)),
    }
}

/// The zero-length case: a position test rather than a sweep.
///
/// `CM_UnsweptBoxTrace` (`engine/cmodel.cpp:2696`). It cannot go through the
/// hull check, which needs a direction of travel to split on, so it gathers the
/// leaves the box overlaps and tests each.
pub(super) fn unswept_box_trace(work: &mut Work<'_>, head_node: i32) {
    // The `+1` on the extents is Valve's, and it is what makes a box resting
    // exactly on a leaf boundary find the brush on the other side.
    let leaves = box_leafnums(work, work.start, work.extents + Vec3::ONE, head_node);

    let mut found_non_solid = false;
    for leaf_index in leaves {
        if !work.bsp.leaves[leaf_index].contents.intersects(Contents::SOLID) {
            found_non_solid = true;
        }
        test_in_leaf(work, leaf_index);
        if work.trace.all_solid {
            break;
        }
    }

    // Every leaf the box touches is solid, so it is buried in the void
    // outside the map rather than merely inside a brush.
    if !found_non_solid {
        work.trace.all_solid = true;
        work.trace.start_solid = true;
        work.trace.fraction = 0.0;
        work.trace.fraction_left_solid = 1.0;
    }
}

/// `CM_TestInLeaf` (`engine/cmodel.cpp:2202`).
fn test_in_leaf(work: &mut Work<'_>, leaf_index: usize) {
    let leaf = work.bsp.leaves[leaf_index];
    let first = leaf.first_leaf_brush as usize;
    let count = leaf.num_leaf_brushes as usize;

    for i in first..first + count {
        let brush_index = work.bsp.leaf_brushes[i] as usize;
        if !work.visit(brush_index) {
            continue;
        }
        if !work.bsp.brushes[brush_index]
            .contents
            .intersects(work.contents)
        {
            continue;
        }
        test_box_in_brush(work, brush_index);
        if work.trace.fraction == 0.0 {
            return;
        }
    }
}

/// Every leaf a box overlaps — `CM_BoxLeafnums` (`engine/cmodel.cpp:552`).
///
/// Valve's ring buffer of pending nodes becomes a plain queue: it was a fixed
/// 1,024 entries with an `Assert` on overflow, which is a `Vec` with the
/// failure mode removed.
fn box_leafnums(work: &Work<'_>, center: Vec3, extents: Vec3, head_node: i32) -> Vec<usize> {
    let bsp = work.bsp;
    let mut leaves = Vec::new();
    let mut pending = vec![head_node];
    let mut read = 0;

    while read < pending.len() {
        let mut num = pending[read];
        read += 1;

        loop {
            if num < 0 {
                leaves.push((-1 - num) as usize);
                break;
            }
            let node = bsp.nodes[num as usize];
            let plane = &bsp.planes[node.plane as usize];
            let d0 = plane.normal.dot(center) - plane.dist;
            let d1 = plane.normal.abs().dot(extents);

            if d0 >= d1 {
                num = node.children[0];
            } else if d0 < -d1 {
                num = node.children[1];
            } else {
                pending.push(node.children[0]);
                num = node.children[1];
            }
        }
    }
    leaves
}
