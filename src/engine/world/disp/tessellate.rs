//! The render index list for one displacement.
//!
//! `TesselateDisplacement` (`public/disp_tesselate.h:195`) and the two tables it
//! reads out of `public/disp_powerinfo.cpp`. This is
//! `portdocs/ENGINE_WORLD_DISP.md` §4.
//!
//! **This is not the two-triangles-per-cell list `trace::disp` builds**, even
//! though for most displacements it produces exactly that — see
//! [`tessellate`]'s second paragraph. It is a walk of the displacement's
//! quadtree that fans around each node and skips any vertex `vbsp` disallowed,
//! which is the whole of Source's fix for a high-power patch cracking against a
//! lower-power neighbour.
//!
//! Everything in `engine/disp.cpp` about *choosing* which vertices are active
//! is deleted rather than ported. `disp_mapload.cpp:734` is the entire policy
//! and it is that there is none:
//!
//! ```text
//! // If we're not using LOD, then maximally tesselate all the displacements and
//! // make sure they never change.
//! pDisp->m_ActiveVerts = pDisp->m_AllowedVerts;
//! ```
//!
//! `InitializeActiveVerts`' corner-and-midpoint seeding is computed and then
//! thrown away by that line, so the active set is exactly the file's
//! `m_AllowedVerts` and the walk runs once, at load.

/// `g_ChildNodeIndexMul` (`disp_powerinfo.cpp:47`), indexed by the
/// `CHILDNODE_*` enum (`public/bspfile.h:228`): upper-right, upper-left,
/// lower-left, lower-right.
const CHILD_OFFSET: [[i32; 2]; 4] = [[1, 1], [-1, 1], [-1, -1], [1, -1]];

const CHILD_UPPER_RIGHT: usize = 0;
const CHILD_UPPER_LEFT: usize = 1;
const CHILD_LOWER_LEFT: usize = 2;
const CHILD_LOWER_RIGHT: usize = 3;

/// `g_TesselateVerts` (`disp_powerinfo.cpp:241`) — the nine offsets a node
/// visits, **clockwise**, starting and ending at its lower-right corner.
///
/// The four corner entries name the child node that owns that quadrant; the
/// four edge entries name none. A corner whose child tessellated itself breaks
/// the run, which is what stops a node fanning over a quadrant that is already
/// covered.
const WINDING: [([i32; 2], Option<usize>); 9] = [
    ([1, -1], Some(CHILD_LOWER_RIGHT)),
    ([0, -1], None),
    ([-1, -1], Some(CHILD_LOWER_LEFT)),
    ([-1, 0], None),
    ([-1, 1], Some(CHILD_UPPER_LEFT)),
    ([0, 1], None),
    ([1, 1], Some(CHILD_UPPER_RIGHT)),
    ([1, 0], None),
    ([1, -1], Some(CHILD_LOWER_RIGHT)),
];

/// Builds the triangle list for a displacement of this power.
///
/// `allowed` is [`DispInfo::allowed_verts`](crate::engine::world::bsp::DispInfo::allowed_verts),
/// one bit per grid vertex in `y * side + x` order — which is the same linear
/// index as `world/disp/`'s `i * spacing + j`, since Valve's `x` is this port's
/// `j`.
///
/// **When every bit is set this produces the same triangles as
/// `trace::disp`'s collision list**, and that is not a coincidence worth
/// leaving unwritten. The grid is `2^power + 1` wide, so the parity of
/// `n = y * width + x` is the parity of `x + y`; a deepest-level node sits at
/// odd `(x, y)` and covers the 2×2 cell block whose corner is even, and across
/// that block `GenerateCollisionSurface`'s parity rule sends every cell's
/// diagonal through the block's centre — which is the node. The fan emits those
/// eight triangles, in the same winding, up to a cyclic rotation of each.
/// `tessellation_matches_the_collision_surface` pins it at powers 2, 3 and 4.
///
/// **Winding is Valve's, as written.** The reversal every piece of
/// Valve-authored geometry gets on the way into this port happens one level up,
/// in [`Displacement::build`](super::Displacement::build), so that the list
/// this returns can be compared against `trace::disp`'s — which is also Valve's
/// order — winding and all.
pub(super) fn tessellate(power: i32, allowed: &[u32; 10]) -> Vec<u16> {
    let mut t = Tessellator {
        power,
        side: (1 << power) + 1,
        allowed,
        indices: Vec::new(),
    };
    // `m_RootNode` is `(sideLength/2, sideLength/2)` (`disp_powerinfo.cpp:464`),
    // which for `side = 2^p + 1` is `2^(p-1)` — the node whose `vertInc` reaches
    // both edges of the grid.
    let root = t.side / 2;
    t.node_r([root, root], 0);
    t.indices
}

struct Tessellator<'a> {
    power: i32,
    side: i32,
    allowed: &'a [u32; 10],
    indices: Vec<u16>,
}

impl Tessellator<'_> {
    /// `TesselateDisplacement_R` (`disp_tesselate.h:93`).
    ///
    /// Children first, then the node itself — the order matters, because a
    /// node's own fan has to know which of its quadrants already covered
    /// themselves.
    ///
    /// The `DispNodeInfo_t` bookkeeping the original threads through here
    /// (`m_FirstTesselationIndex`, `m_Count`, `CHILDREN_HAVE_TRIANGLES`, and
    /// the `m_NodeIndexIncrements` that walk the node-bit index alongside the
    /// recursion) exists so a decal fragment can name a subtree, and is not
    /// ported: there are no decals.
    fn node_r(&mut self, node: [i32; 2], level: i32) {
        let mut active_children = [false; 4];

        // `if( iLevel >= m_pPowerInfo->m_Power - 1 )` — the deepest nodes have
        // no children, and at power 2 that is the root.
        if level < self.power - 1 {
            let node_inc = self.vert_inc(level) >> 1;
            for (child, offset) in CHILD_OFFSET.iter().enumerate() {
                let vert = [
                    node[0] + offset[0] * node_inc,
                    node[1] + offset[1] * node_inc,
                ];
                // A child is active when its own centre vertex is — which is
                // how a disallowed vertex prunes a whole quadrant rather than
                // just itself.
                active_children[child] = self.is_active(vert);
                if active_children[child] {
                    self.node_r(vert, level + 1);
                }
            }
        }

        self.node(node, level, &active_children);
    }

    /// `TesselateDisplacementNode` (`disp_tesselate.h:47`).
    fn node(&mut self, node: [i32; 2], level: i32, active_children: &[bool; 4]) {
        let vert_inc = self.vert_inc(level);
        let centre = self.index(node);

        // The run of consecutive vertices being fanned. `count` is Valve's
        // `iCurTriVert` and never exceeds 1 on entry to an iteration, because
        // reaching 2 closes a triangle immediately.
        let mut run = [0u16; 2];
        let mut count = 0usize;

        for (offset, child) in WINDING {
            let vert = [
                node[0] + offset[0] * vert_inc,
                node[1] + offset[1] * vert_inc,
            ];

            // `bNode`: this corner is a child that covered itself, so the run
            // has to break here. Valve also closes a pending triangle first;
            // that branch cannot fire, because a run of two is closed the
            // moment it forms, and it is left out rather than transliterated
            // as unreachable code.
            if child.is_some_and(|c| active_children[c]) {
                count = 0;
                continue;
            }

            // The crack fix, and the only reason this is not a nested loop over
            // cells: a vertex `vbsp` disallowed because the neighbouring patch
            // is coarser is stepped over, and the two triangles that would have
            // met at it become one that spans it.
            if !self.is_active(vert) {
                continue;
            }

            run[count] = self.index(vert);
            count += 1;
            if count == 2 {
                self.indices.extend_from_slice(&[run[0], run[1], centre]);
                // The triangle's second vertex starts the next one, so the fan
                // stays connected.
                run[0] = run[1];
                count = 1;
            }
        }
    }

    /// `int vertInc = 1 << (iPower - 1)` for `iPower = m_Power - iLevel`
    /// (`disp_tesselate.h:52`) — how far a node's winding reaches.
    fn vert_inc(&self, level: i32) -> i32 {
        1 << (self.power - level - 1)
    }

    /// `InternalVertIndex` (`disp_tesselate.h:18`) — `y * sideLength + x`.
    fn index(&self, vert: [i32; 2]) -> u16 {
        debug_assert!((0..self.side).contains(&vert[0]) && (0..self.side).contains(&vert[1]));
        (vert[1] * self.side + vert[0]) as u16
    }

    /// Whether this grid vertex is in the active set.
    ///
    /// `m_pActiveVerts[iVertBit>>5] & (1 << (iVertBit & 31))`, over the bit
    /// vector the file shipped. A vertex outside the grid cannot be reached by
    /// the walk — the root's `vert_inc` is exactly half the grid and every
    /// deeper node is further in — so an out-of-range index is a bug in the
    /// arithmetic above rather than data, and it reads as inactive rather than
    /// panicking in a release build.
    fn is_active(&self, vert: [i32; 2]) -> bool {
        if !(0..self.side).contains(&vert[0]) || !(0..self.side).contains(&vert[1]) {
            debug_assert!(false, "tessellation left the grid at {vert:?}");
            return false;
        }
        let bit = (vert[1] * self.side + vert[0]) as usize;
        match self.allowed.get(bit / 32) {
            Some(word) => word & (1 << (bit % 32)) != 0,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::world::bsp::DispInfo;

    const ALL: [u32; 10] = [u32::MAX; 10];

    /// The collision surface, `GenerateCollisionSurface` (`builddisp.cpp:977`)
    /// — the same rule `trace::disp::build_tris` ports, restated here over bare
    /// indices so the two lists can be compared without building a `Bsp`.
    fn collision_surface(power: i32) -> Vec<[u16; 3]> {
        let width = (1usize << power) + 1;
        let mut tris = Vec::new();
        for v in 0..width - 1 {
            for u in 0..width - 1 {
                let n = v * width + u;
                let (a, b) = match n % 2 == 1 {
                    true => ([n, n + width, n + 1], [n + 1, n + width, n + width + 1]),
                    false => ([n, n + width, n + width + 1], [n, n + width + 1, n + 1]),
                };
                tris.push(a.map(|i| i as u16));
                tris.push(b.map(|i| i as u16));
            }
        }
        tris
    }

    /// A triangle in its canonical rotation, so that `(a,b,c)`, `(b,c,a)` and
    /// `(c,a,b)` compare equal — and `(a,c,b)` still does not.
    fn canonical(tri: [u16; 3]) -> [u16; 3] {
        let lowest = (0..3).min_by_key(|&i| tri[i]).unwrap();
        [tri[lowest], tri[(lowest + 1) % 3], tri[(lowest + 2) % 3]]
    }

    /// `y * side + x`, the one index arithmetic the walk and its tests share.
    fn vert_index(power: i32, x: usize, y: usize) -> usize {
        y * ((1usize << power) + 1) + x
    }

    fn triangles(indices: &[u16]) -> Vec<[u16; 3]> {
        indices
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect()
    }

    /// `portdocs/ENGINE_WORLD_DISP.md` §4.3, and the reason this module can be
    /// trusted at all: with every vertex allowed, the quadtree fan and the
    /// per-cell diagonal rule are the same triangulation.
    ///
    /// This is what catches an error in the node indexing, the child offsets,
    /// the winding table or the `vert_inc` arithmetic — each of which
    /// otherwise produces a plausible mesh with a subtly wrong surface.
    #[test]
    fn tessellation_matches_the_collision_surface() {
        for power in 2..=4 {
            let mut rendered: Vec<[u16; 3]> = triangles(&tessellate(power, &ALL))
                .into_iter()
                .map(canonical)
                .collect();
            let mut collision: Vec<[u16; 3]> = collision_surface(power)
                .into_iter()
                .map(canonical)
                .collect();
            rendered.sort_unstable();
            collision.sort_unstable();

            assert_eq!(
                rendered.len(),
                DispInfo::tri_count(power),
                "power {power}: wrong triangle count"
            );
            assert_eq!(rendered, collision, "power {power}");
        }
    }

    /// Every grid vertex is used, and every triangle is a real one.
    #[test]
    fn a_full_tessellation_covers_the_whole_grid() {
        for power in 2..=4 {
            let indices = tessellate(power, &ALL);
            let used: std::collections::BTreeSet<u16> = indices.iter().copied().collect();
            assert_eq!(used.len(), DispInfo::vert_count(power), "power {power}");
            for tri in triangles(&indices) {
                assert!(
                    tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2],
                    "power {power}: degenerate {tri:?}"
                );
            }
        }
    }

    /// A disallowed vertex is never named, and the patch loses triangles rather
    /// than gaining a hole: the vertex's neighbours are still covered, because
    /// the fan spans it.
    #[test]
    fn a_disallowed_vertex_is_skipped() {
        let power = 3;
        // The middle of the bottom edge at the second-deepest level — the shape
        // a coarser neighbour below this patch produces.
        let victim = vert_index(power, 3, 0);

        let mut allowed = ALL;
        allowed[victim / 32] &= !(1 << (victim % 32));

        let indices = tessellate(power, &allowed);
        assert!(
            !indices.contains(&(victim as u16)),
            "the disallowed vertex is still drawn"
        );
        assert!(
            indices.len() < tessellate(power, &ALL).len(),
            "skipping a vertex should merge triangles, not add them"
        );
        for tri in triangles(&indices) {
            assert!(tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2]);
        }
    }

    /// Clearing a *node* vertex prunes its quadrant, which is the propagating
    /// half of `SetupAllowedVerts` — and the half that would silently drop half
    /// the patch if `node_r` recursed unconditionally.
    #[test]
    fn a_disallowed_node_prunes_its_quadrant() {
        let power = 3;
        // The upper-right child of the root, at (6, 6) of a 9x9 grid.
        let node = vert_index(power, 6, 6);

        let mut allowed = ALL;
        allowed[node / 32] &= !(1 << (node % 32));

        let indices = tessellate(power, &allowed);
        assert!(!indices.contains(&(node as u16)));
        // The quadrant's interior vertices are unreachable once its node is
        // gone: nothing below it is visited.
        let interior = vert_index(power, 7, 7) as u16;
        assert!(!indices.contains(&interior));
        // ...and the rest of the patch still draws, coarsely over the pruned
        // quadrant: fewer triangles than a full patch, and not zero.
        let full = tessellate(power, &ALL).len();
        assert!(
            (1..full).contains(&indices.len()),
            "{} of {full}",
            indices.len()
        );
    }

    /// **The root node emits nothing** when all four of its children are
    /// active, and at power 2 — 904 of Portal 2's 1,181 displacements — that is
    /// the whole shape: one level of recursion, four leaf nodes, eight
    /// triangles each.
    ///
    /// Worth pinning because "the root fans the patch" is the obvious wrong
    /// mental model, and it produces a 5×5 grid drawn as 8 triangles instead of
    /// 32 — a patch that is the right outline and flat in the middle.
    #[test]
    fn the_root_defers_to_its_children() {
        let indices = tessellate(2, &ALL);
        assert_eq!(indices.len() / 3, DispInfo::tri_count(2));

        // The fan centre is the third index, by construction — `EndTriangle`
        // writes `m_TempIndices[2] = nodeIndex`. Every one is a leaf node, at
        // the odd coordinates of a 5x5 grid, and none is the root at (2,2).
        //
        // Note the root is still a perfectly ordinary *vertex* of its
        // children's fans; what it never is, is a centre.
        let leaves: Vec<u16> = [[1, 1], [3, 1], [1, 3], [3, 3]]
            .iter()
            .map(|v: &[u16; 2]| v[1] * 5 + v[0])
            .collect();
        for tri in triangles(&indices) {
            assert!(leaves.contains(&tri[2]), "{tri:?} fans no leaf node");
        }
    }
}
