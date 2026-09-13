//! Displacements: the AABB tree over one patch of terrain's triangles.
//!
//! Replaces `public/dispcoll_common.cpp`'s `CDispCollTree` and the parts of
//! `public/builddisp.cpp` that turn a `ddispinfo_t` into geometry. This is
//! `portdocs/ENGINE_TRACE.md` stage 3.
//!
//! **A displacement is a surface, not a volume**, and that is the whole reason
//! this file is not simply more brushes. A brush is a convex intersection of
//! half-spaces and "inside" is well defined; a displacement is a `(2^power+1)²`
//! grid of triangles draped over one four-sided world face, with nothing behind
//! it. So there is no plane set to clip against, no `fraction_left_solid` to
//! accumulate, and the position test ([`DispTree::intersects_box`]) is a
//! separating-axis box-versus-triangle test rather than a half-space walk.
//!
//! Four things here will surprise a reader of the brush code:
//!
//! 1. **Every test is one-sided.** A ray is rejected before it starts if it is
//!    travelling *along* the triangle's normal (`IntersectRayWithTriangle`'s
//!    `oneSided`), and a sweep is rejected the same way by
//!    `sweep_triangle`'s first line. Terrain is solid from the front and
//!    transparent from the back.
//! 2. **The sweep does not use the triangle's plane set** — it builds the
//!    Minkowski sum out of 3 axis planes, 9 edge-cross planes and the face
//!    plane, which is the separating-axis theorem for a box against a triangle
//!    with a direction of travel. The 9 edge planes are precomputed
//!    ([`Triangle::cross`]) because they depend only on the geometry.
//! 3. **A displacement with `NOHULL_COLL` is invisible to a box sweep and
//!    `NORAY_COLL` to a ray**, and Portal 2 uses both: 44 of its 1,181
//!    displacements have all three collision flags off and 7 more have hull
//!    collision off. These are decoration, and treating them as solid puts
//!    invisible walls in the ruins.
//! 4. **A ray only hits a displacement whose contents include
//!    `MASK_OPAQUE`** (`AABBTree_Ray`, `dispcoll_common.cpp:672`). 51 of Portal
//!    2's displacements are `WINDOW | TRANSLUCENT`, so a ray passes straight
//!    through them while a hull sweep does not.

use glam::Vec3;

use super::{Contents, Trace, DIST_EPSILON};
use crate::engine::world::bsp::{Bsp, DispInfo, Face};

/// `DISPSURF_FLAG_*` (`public/trace.h:25`) — what
/// [`Trace::disp_flags`](super::Trace::disp_flags) carries.
///
/// The same bit values as the `DISPTRI_TAG_*` VBSP writes into
/// [`bsp::DispTri::tags`](crate::engine::world::bsp::DispTri::tags), which is
/// why the lump's tags reach a trace result unmapped. Measured on the depot:
/// the shipped tag values are only 0, `WALKABLE` and `WALKABLE | BUILDABLE` —
/// `SURFACE` is never in the file, because the engine ORs it in.
// A fixed external vocabulary, not this module's invention — the same rule
// `Contents` follows. Defining only the two bits something happens to read
// would make the next module to want `BUILDABLE` re-derive it from `trace.h`,
// which is exactly the transcription error these tables exist to prevent.
#[allow(dead_code)]
pub mod disp_surf {
    /// Set by the engine on every displacement triangle, so a non-zero
    /// [`Trace::disp_flags`](super::super::Trace::disp_flags) means "terrain".
    pub const SURFACE: u16 = 1 << 0;
    /// VBSP judged this triangle shallow enough to walk up.
    pub const WALKABLE: u16 = 1 << 1;
    pub const BUILDABLE: u16 = 1 << 2;
    pub const SURFPROP1: u16 = 1 << 3;
    pub const SURFPROP2: u16 = 1 << 4;
    pub const SURFPROP3: u16 = 1 << 5;
    pub const SURFPROP4: u16 = 1 << 6;

    /// `CDispCollTri::m_uiFlags` is a 7-bit bitfield, so anything above
    /// [`SURFPROP4`] in the lump is truncated before a trace ever sees it.
    pub const MASK: u16 = 0x7F;
}

/// `CCoreDispInfo::SURF_*` (`public/builddisp.h:744`) — the *collision* flags a
/// displacement carries, smuggled through `ddispinfo_t::minTess`.
///
/// See [`DispInfo::disp_flags`].
mod surf_coll {
    /// The base face was compiled against a bumped material. Rendering's.
    #[allow(dead_code)]
    pub const BUMPED: u32 = 0x1;
    /// No `vphysics` mesh — `.phy`-shaped objects pass through. Read at stage 5
    /// and not before, but named here because it shares the field.
    #[allow(dead_code)]
    pub const NOPHYSICS_COLL: u32 = 0x2;
    /// Invisible to a swept box.
    pub const NOHULL_COLL: u32 = 0x4;
    /// Invisible to a ray.
    pub const NORAY_COLL: u32 = 0x8;
}

/// `DISP_ALPHA_PROP_DELTA` (`public/builddisp.h:26`) — 255 × 1.5, compared
/// against the *sum* of a triangle's three vertex alphas.
const ALPHA_PROP_DELTA: f32 = 382.5;

/// `DISPCOLL_INVALID_FRAC` (`public/dispcoll_common.h:38`).
const INVALID_FRAC: f32 = -99999.9;

/// How far a stab travels — `99999.9f` (`cmodel_disp.cpp:294`), which Valve's
/// own comment calls "world extents * 2".
pub(super) const STAB_LENGTH: f32 = 99999.9;

/// `VectorNormalize` (`mathlib/mathlib_base.cpp:77`).
///
/// Not `Vec3::normalize`: Valve divides by `length + FLT_EPSILON`, so a
/// zero-length vector comes back as zero rather than `NaN`. A displacement can
/// have degenerate triangles where the grid folds onto itself, and a `NaN`
/// normal poisons every comparison it reaches instead of failing a test.
fn normalize(v: Vec3) -> Vec3 {
    v / (v.length() + f32::EPSILON)
}

/// One collision triangle — `CDispCollTri`
/// (`public/dispcoll_common.h:58`), unpacked out of its bitfields.
#[derive(Debug, Clone)]
struct Triangle {
    /// Indices into [`DispTree::verts`].
    verts: [u16; 3],
    /// Which of [`verts`](Triangle::verts) is lowest on each axis, 0-2 —
    /// `CDispCollTri::GetMin`. The axis-plane test reads a coordinate through
    /// this rather than taking a min of three, which is the same answer and is
    /// how `AxisPlanesXYZ` is written.
    min: [u8; 3],
    /// Which is highest — `GetMax`.
    max: [u8; 3],
    normal: Vec3,
    dist: f32,
    /// `m_ucPlaneType`: the axis, when the normal is exactly `+1` along one.
    ///
    /// **Positive only.** `CalcPlane` sets it for `normal[axis] == 1.0f` and
    /// never for `-1.0f`, so a downward-facing axial triangle takes the general
    /// path. Ported as written because
    /// [`box_on_plane_side`]'s fast path is only correct for `+1`.
    axis: Option<usize>,
    /// [`disp_surf`] bits.
    flags: u16,
    /// The nine edge-cross planes, `[axis][edge]`, as
    /// `CDispCollTriCache::m_iCrossX/Y/Z` resolved to the planes themselves.
    ///
    /// **The plane's distance is stored in the `axis` component of the
    /// vector**, where the normal is zero by construction — Valve's packing
    /// (`Cache_EdgeCrossAxisX`, "the plane distance gets stored in the normal x
    /// position since it isn't used"), kept so that [`edge_cross_axis`] is a
    /// transcription rather than a translation. `None` is
    /// `DISPCOLL_NORMAL_UNDEF`: an edge parallel to the axis has no plane and
    /// the test is skipped.
    ///
    /// Valve interns these in a global hash and stores 16-bit handles, with the
    /// top bit meaning "the negation of entry n". That is a memory
    /// optimisation for a 2 MB cache budget this port does not have, and
    /// storing the resolved plane gives the same nine vectors.
    cross: [[Option<Vec3>; 3]; 3],
}

/// One node of the quadtree — `CDispCollNode`
/// (`public/dispcoll_common.h:126`), which is four children's boxes in one
/// record rather than a box per node.
#[derive(Debug, Clone, Copy)]
struct DispNode {
    mins: [Vec3; 4],
    maxs: [Vec3; 4],
}

/// One displacement's collision geometry.
///
/// Built once per displacement at map load and immutable after. Traces reach it
/// through the per-leaf lists [`CollisionBsp`](super::CollisionBsp) owns, never
/// directly: a displacement is in every leaf its bounding box touches, which is
/// what makes the visit stamps necessary.
#[derive(Debug)]
pub struct DispTree {
    /// `CONTENTS_*` for the whole patch — `ddispinfo_t::contents`.
    pub(super) contents: Contents,
    /// [`surf_coll`] bits.
    flags: u32,
    /// Index into [`CollisionBsp::surfaces`](super::CollisionBsp), taken from
    /// the base face's texdata.
    surface: u16,
    /// The base face's normal, and the direction [`DispTree::stab_dir`] hands
    /// to the stab. Points *out* of the terrain — see
    /// [`CollisionBsp::test_in_disp_tree`](super::CollisionBsp).
    stab_dir: Vec3,
    /// The patch's bounds, **bloated by one unit on every axis**
    /// (`AABBTree_CalcBounds`, `dispcoll_common.cpp:509`). The bloat is the
    /// reason a trace that grazes the edge of a patch still descends into it.
    pub(super) mins: Vec3,
    pub(super) maxs: Vec3,
    verts: Vec<Vec3>,
    tris: Vec<Triangle>,
    /// The internal nodes. A node index `>= nodes.len()` is the leaf
    /// `index - nodes.len()` — `CDispCollTree::IsLeafNode`.
    nodes: Vec<DispNode>,
    /// Two triangle indices per grid cell — `CDispCollLeaf`.
    leaves: Vec<[u16; 2]>,
}

impl DispTree {
    /// Builds the tree for displacement `index` over its base `face`.
    ///
    /// `CollisionBSPData_LoadDispInfo` (`engine/cmodel_bsp.cpp:1046`) plus
    /// `CCoreDispInfo::Create` and `CDispCollTree::Create`, which are three
    /// objects in the original because the middle one is shared with the
    /// renderer and the map compiler.
    ///
    /// `None` when the base face is not a quad — `if ( pFaces->numedges > 4 )
    /// continue;` and the `pointCount != 4` check after it. Measured on the
    /// depot: all 1,181 shipped displacements have exactly four edges, so this
    /// is a guard against a malformed map rather than a case that happens.
    pub(super) fn build(bsp: &Bsp, index: usize, face: &Face, surface: u16) -> Option<DispTree> {
        let info = bsp.disp_info.get(index)?;

        // The corner rotation and the grid itself are `bsp`'s, not this
        // module's: `world/disp/` draws the same grid, and deriving it twice is
        // how the drawn surface and the solid one come to disagree.
        let points = bsp.disp_base_quad(face, info)?;
        let verts = bsp.disp_grid(info, &points);
        let mut tris = build_tris(info, bsp, &verts);
        for tri in &mut tris {
            tri.calc_plane(&verts);
            tri.find_min_max(&verts);
            tri.cache_edge_planes(&verts);
        }

        // `AABBTree_CreateLeafs` (`dispcoll_common.cpp:429`). The leaf for grid
        // cell (x, y) is at its *Morton* index, not at `y * width + x`, which
        // is what makes `child = 4 * node + 1 + direction` land on the right
        // quadrant with no indirection.
        let cells = 1usize << info.power;
        let mut leaves = vec![[0u16; 2]; cells * cells];
        for y in 0..cells {
            for x in 0..cells {
                let tri = ((y * cells + x) * 2) as u16;
                leaves[morton(x, y)] = [tri, tri + 1];
            }
        }

        let node_count = nodes_calc_count(info.power) - leaves.len();
        let mut tree = DispTree {
            contents: Contents(info.contents as u32),
            flags: info.disp_flags(),
            surface,
            // `pSurf->GetNormal` (`builddisp.h:443`), which is the same
            // `(p3-p0) × (p1-p0)` every triangle's normal is built from — so
            // the stab travels along the surface's own normal.
            stab_dir: normalize((points[3] - points[0]).cross(points[1] - points[0])),
            mins: Vec3::splat(f32::MAX),
            maxs: Vec3::splat(-f32::MAX),
            verts,
            tris,
            nodes: vec![
                DispNode {
                    mins: [Vec3::ZERO; 4],
                    maxs: [Vec3::ZERO; 4],
                };
                node_count
            ],
            leaves,
        };

        // `AABBTree_CalcBounds` (`:502`), which fills every node's four child
        // boxes on the way back up and leaves the root's in `mins`/`maxs`.
        if !tree.verts.is_empty() && !tree.nodes.is_empty() {
            let (mins, maxs) = tree.generate_boxes(0);
            tree.mins = mins - Vec3::ONE;
            tree.maxs = maxs + Vec3::ONE;
        }
        Some(tree)
    }

    /// `AABBTree_GenerateBoxes_r` (`dispcoll_common.cpp:466`).
    fn generate_boxes(&mut self, node: usize) -> (Vec3, Vec3) {
        let (mut mins, mut maxs) = (Vec3::splat(f32::MAX), Vec3::splat(-f32::MAX));
        if node >= self.nodes.len() {
            for &tri in &self.leaves[node - self.nodes.len()] {
                for &vert in &self.tris[tri as usize].verts {
                    let v = self.verts[vert as usize];
                    mins = mins.min(v);
                    maxs = maxs.max(v);
                }
            }
            return (mins, maxs);
        }

        let mut child_mins = [Vec3::ZERO; 4];
        let mut child_maxs = [Vec3::ZERO; 4];
        for i in 0..4 {
            let (lo, hi) = self.generate_boxes(child(node, i));
            child_mins[i] = lo;
            child_maxs[i] = hi;
            mins = mins.min(lo).min(hi);
            maxs = maxs.max(lo).max(hi);
        }
        self.nodes[node].mins = child_mins;
        self.nodes[node].maxs = child_maxs;
        (mins, maxs)
    }

    /// Whether this patch collides with a swept box at all.
    fn hull_collides(&self) -> bool {
        self.flags & surf_coll::NOHULL_COLL == 0
    }

    /// Whether it collides with a ray.
    ///
    /// Two conditions, both Valve's (`AABBTree_Ray`,
    /// `dispcoll_common.cpp:672`): the flag, and that the contents are
    /// something a *sight* line stops at. The second is why Portal 2's 51
    /// `WINDOW | TRANSLUCENT` displacements are transparent to a ray and solid
    /// to a hull.
    fn ray_collides(&self) -> bool {
        self.flags & surf_coll::NORAY_COLL == 0 && self.contents.intersects(Contents::MASK_OPAQUE)
    }

    /// The direction the stab travels — the base face's normal.
    pub(super) fn stab_dir(&self) -> Vec3 {
        self.stab_dir
    }

    /// `PointInBounds` (`dispcoll_common.cpp:1479`) — is the query inside this
    /// patch's (bloated) bounds, with the box's own extents added for a hull.
    pub(super) fn point_in_bounds(&self, center: Vec3, extents: Vec3, is_point: bool) -> bool {
        let (mins, maxs) = match is_point {
            true => (self.mins, self.maxs),
            false => (self.mins - extents, self.maxs + extents),
        };
        center.cmpge(mins).all() && center.cmple(maxs).all()
    }

    /// Every leaf whose box the swept ray touches, in the order the breadth-
    /// first walk reaches them — `BuildRayLeafList`
    /// (`dispcoll_common.cpp:249`).
    ///
    /// Valve returns an index *into* the pending list and relies on every entry
    /// after it being a leaf, which holds because a child's index is always
    /// greater than its parent's and the walk is FIFO. Collecting the leaves is
    /// the same set in the same order.
    fn ray_leaf_list(&self, start: Vec3, extents: Vec3, inv_delta: Vec3) -> Vec<usize> {
        if self.nodes.is_empty() {
            return Vec::new();
        }
        // Valve's fixed 344 entries with an `Assert` on overflow; a `Vec` is
        // the same thing with the failure mode removed, as in
        // `hull::box_leafnums`.
        let mut pending: Vec<usize> = Vec::with_capacity(64);
        pending.push(0);
        let mut read = 0;
        while read < pending.len() {
            let node = pending[read];
            if node >= self.nodes.len() {
                // The rest are all leaves.
                break;
            }
            read += 1;
            let n = &self.nodes[node];
            for i in 0..4 {
                if ray_hits_box(start, extents, inv_delta, n.mins[i], n.maxs[i]) {
                    pending.push(child(node, i));
                }
            }
        }
        pending
            .drain(read..)
            .map(|node| node - self.nodes.len())
            .collect()
    }

    /// Sweeps a **ray** against this patch, narrowing `trace` if it hits.
    ///
    /// `AABBTree_Ray` (`dispcoll_common.cpp:672`). Returns whether it hit,
    /// which is what tells the caller to attribute the hit to a displacement.
    pub(super) fn trace_ray(
        &self,
        start: Vec3,
        delta: Vec3,
        inv_delta: Vec3,
        trace: &mut Trace,
    ) -> bool {
        if !self.ray_collides() {
            return false;
        }

        // The ray has no extents, so the list is culled by the epsilon alone.
        let extents = Vec3::splat(DIST_EPSILON);
        let mut impact: Option<&Triangle> = None;
        for leaf in self.ray_leaf_list(start, extents, inv_delta) {
            for &index in &self.leaves[leaf] {
                let tri = &self.tris[index as usize];
                let frac = self.intersect_ray_with_triangle(start, delta, tri);
                if frac >= 0.0 && frac < trace.fraction {
                    trace.fraction = frac;
                    impact = Some(tri);
                }
            }
        }

        match impact {
            Some(tri) => {
                trace.normal = tri.normal;
                trace.plane_dist = tri.dist;
                trace.disp_flags = tri.flags;
                true
            }
            None => false,
        }
    }

    /// Sweeps a **box** against this patch.
    ///
    /// `AABBTree_SweepAABB` (`dispcoll_common.cpp:915`).
    pub(super) fn sweep_box(
        &self,
        start: Vec3,
        delta: Vec3,
        extents: Vec3,
        inv_delta: Vec3,
        trace: &mut Trace,
    ) -> bool {
        if !self.hull_collides() {
            return false;
        }
        let before = trace.fraction;
        for leaf in self.ray_leaf_list(start, extents + Vec3::splat(DIST_EPSILON), inv_delta) {
            for &index in &self.leaves[leaf] {
                self.sweep_triangle(start, delta, extents, &self.tris[index as usize], trace);
            }
        }
        trace.fraction < before
    }

    /// Whether an unswept box overlaps any of this patch's triangles.
    ///
    /// `AABBTree_IntersectAABB` (`dispcoll_common.cpp:815`) — the position
    /// test, and the half of it that actually works (see
    /// [`CollisionBsp::test_in_disp_tree`](super::CollisionBsp) for the stab,
    /// which is the other half).
    pub(super) fn intersects_box(&self, abs_mins: Vec3, abs_maxs: Vec3) -> bool {
        if !self.hull_collides() || self.nodes.is_empty() {
            return false;
        }
        let center = (abs_mins + abs_maxs) * 0.5;
        let extents = abs_maxs - center;

        let mut pending: Vec<usize> = Vec::with_capacity(64);
        pending.push(0);
        let mut read = 0;
        while read < pending.len() {
            let node = pending[read];
            if node >= self.nodes.len() {
                break;
            }
            read += 1;
            let n = &self.nodes[node];
            for i in 0..4 {
                if abs_mins.cmple(n.maxs[i]).all() && abs_maxs.cmpge(n.mins[i]).all() {
                    pending.push(child(node, i));
                }
            }
        }

        pending[read..].iter().any(|&node| {
            self.leaves[node - self.nodes.len()].iter().any(|&index| {
                let tri = &self.tris[index as usize];
                // Valve's argument order, `v0, v2, v1` — the same swap the ray
                // test makes, so both agree with `CalcPlane`'s winding.
                box_intersects_triangle(
                    center,
                    extents,
                    self.verts[tri.verts[0] as usize],
                    self.verts[tri.verts[2] as usize],
                    self.verts[tri.verts[1] as usize],
                    tri,
                )
            })
        })
    }

    /// The surface-table index a hit on this patch reports.
    pub(super) fn surface(&self) -> u16 {
        self.surface
    }

    /// `IntersectRayWithTriangle` (`public/collisionutils.cpp:66`), specialised
    /// to the one call this module makes.
    ///
    /// Barycentric coordinates by Cramer's rule, **one-sided**: a ray moving
    /// along the triangle's normal is rejected outright, so terrain is solid
    /// from the front and transparent from behind. Valve's call passes the
    /// vertices as `v0, v2, v1`, which makes the normal it derives here the
    /// same one [`Triangle::calc_plane`] stored.
    fn intersect_ray_with_triangle(&self, start: Vec3, delta: Vec3, tri: &Triangle) -> f32 {
        let v1 = self.verts[tri.verts[0] as usize];
        let v2 = self.verts[tri.verts[2] as usize];
        let v3 = self.verts[tri.verts[1] as usize];

        let edge1 = v2 - v1;
        let edge2 = v3 - v1;
        if edge1.cross(edge2).dot(delta) >= 0.0 {
            return -1.0;
        }

        let dir_cross_edge2 = delta.cross(edge2);
        let denom = dir_cross_edge2.dot(edge1);
        if denom.abs() < 1e-6 {
            return -1.0;
        }
        let denom = 1.0 / denom;

        let org = start - v1;
        let u = dir_cross_edge2.dot(org) * denom;
        if !(0.0..=1.0).contains(&u) {
            return -1.0;
        }

        let org_cross_edge1 = org.cross(edge1);
        let v = org_cross_edge1.dot(delta) * denom;
        if v < 0.0 || v + u > 1.0 {
            return -1.0;
        }

        // `ComputeBoxOffset` (`collisionutils.cpp:43`) is `1e-3` for a ray, and
        // this is only ever reached from the ray path — `sweep_triangle` is
        // what a box goes through.
        const BOX_T: f32 = 1e-3;
        let t = org_cross_edge1.dot(edge2) * denom;
        if !(-BOX_T..=1.0 + BOX_T).contains(&t) {
            return -1.0;
        }
        t.clamp(0.0, 1.0)
    }

    /// `SweepAABBTriIntersect` (`dispcoll_common.cpp:1357`) — the separating
    /// axis test, with a direction of travel.
    ///
    /// Thirteen planes: the three axis slabs of the triangle's own bounds, the
    /// nine edge-cross planes, and the triangle's face plane. Each is expanded
    /// by the box's extent along its normal, which turns the swept-box problem
    /// into a ray against an extruded triangle — the same trick
    /// `clip_box_to_brush` plays with a brush's sides, done here against a
    /// plane set that has to be derived because a triangle has no side list.
    fn sweep_triangle(
        &self,
        start: Vec3,
        delta: Vec3,
        extents: Vec3,
        tri: &Triangle,
        trace: &mut Trace,
    ) {
        let mut helper = Helper {
            start_frac: INVALID_FRAC,
            end_frac: 1.0,
            normal: Vec3::ZERO,
            dist: 0.0,
        };

        // "Make sure objects are traveling toward one another." The same
        // one-sidedness the ray test has, with the epsilon's worth of slack
        // that makes `post_trace_to_disp_tree`'s "we came out the back" test
        // reachable at all.
        if tri.normal.dot(delta) > DIST_EPSILON {
            return;
        }

        if !self.axis_planes(start, delta, extents, tri, &mut helper) {
            return;
        }
        for axis in 0..3 {
            for edge in 0..3 {
                if !edge_cross_axis(
                    start,
                    delta,
                    extents,
                    axis,
                    tri.cross[axis][edge],
                    &mut helper,
                ) {
                    return;
                }
            }
        }
        if !face_plane(start, delta, extents, tri, &mut helper) {
            return;
        }

        // The `0.001` is Valve's and it matters: a box that enters and leaves
        // in the same instant — a graze along an edge — has `start_frac`
        // fractionally *past* `end_frac` and is still a hit.
        if helper.start_frac >= helper.end_frac
            && (helper.start_frac - helper.end_frac).abs() >= 0.001
        {
            return;
        }
        if helper.start_frac == INVALID_FRAC || helper.start_frac >= trace.fraction {
            return;
        }

        trace.fraction = helper.start_frac.max(0.0);
        trace.normal = helper.normal;
        trace.plane_dist = helper.dist;
        trace.disp_flags = tri.flags;
    }

    /// `AxisPlanesXYZ` (`dispcoll_common.cpp:1020`) — the triangle's own
    /// axis-aligned bounds as six planes.
    ///
    /// Note the descending axis order, which is Valve's: it decides which of
    /// two equally-near planes wins a tie, and therefore which normal a corner
    /// impact reports.
    fn axis_planes(
        &self,
        start: Vec3,
        delta: Vec3,
        extents: Vec3,
        tri: &Triangle,
        helper: &mut Helper,
    ) -> bool {
        for axis in (0..3).rev() {
            let (ray_start, ray_extent, ray_delta) = (start[axis], extents[axis], delta[axis]);

            let mut normal = Vec3::ZERO;

            // Min: the plane facing `-axis`, pushed out by the extent.
            let dist = self.verts[tri.verts[tri.min[axis] as usize] as usize][axis];
            let start_d = (dist - ray_extent) - ray_start;
            let end_d = start_d - ray_delta;
            normal[axis] = -1.0;
            if !resolve_ray_plane_intersect(start_d, end_d, normal, dist, helper) {
                return false;
            }

            // Max: the plane facing `+axis`.
            let dist = self.verts[tri.verts[tri.max[axis] as usize] as usize][axis];
            let start_d = ray_start - (dist + ray_extent);
            let end_d = start_d + ray_delta;
            normal[axis] = 1.0;
            if !resolve_ray_plane_intersect(start_d, end_d, normal, dist, helper) {
                return false;
            }
        }
        true
    }
}

/// `CDispCollHelper` (`public/dispcoll_common.h:103`) — the running interval
/// one triangle's thirteen planes narrow.
struct Helper {
    start_frac: f32,
    end_frac: f32,
    normal: Vec3,
    dist: f32,
}

/// `ResolveRayPlaneIntersect` (`dispcoll_common.cpp:966`).
///
/// Returns `false` only for "wholly in front of this plane", which is a
/// separating axis and ends the triangle's test. The epsilon is subtracted on
/// the way *in* and added on the way *out*, which is the same
/// stop-short-of-the-surface rule `clip_box_to_brush` follows.
fn resolve_ray_plane_intersect(
    start: f32,
    end: f32,
    normal: Vec3,
    dist: f32,
    helper: &mut Helper,
) -> bool {
    if start > 0.0 && end > 0.0 {
        return false;
    }
    if start < 0.0 && end < 0.0 {
        return true;
    }

    let denom = start - end;
    let zero = denom == 0.0;
    if start >= 0.0 && end <= 0.0 {
        let t = if zero {
            0.0
        } else {
            (start - DIST_EPSILON) / denom
        };
        if t > helper.start_frac {
            helper.start_frac = t;
            helper.normal = normal;
            helper.dist = dist;
        }
    } else {
        let t = if zero {
            0.0
        } else {
            (start + DIST_EPSILON) / denom
        };
        if t < helper.end_frac {
            helper.end_frac = t;
        }
    }
    true
}

/// `CDispCollTree::FacePlane` (`dispcoll_common.cpp:1003`).
fn face_plane(
    start: Vec3,
    delta: Vec3,
    extents: Vec3,
    tri: &Triangle,
    helper: &mut Helper,
) -> bool {
    // `CalcClosestExtents` (`dispcoll_common.h:448`): the corner of the box
    // furthest *behind* the plane.
    let extent = Vec3::from(std::array::from_fn(|i| {
        if tri.normal[i] < 0.0 {
            extents[i]
        } else {
            -extents[i]
        }
    }));
    let expand = tri.dist - tri.normal.dot(extent);
    let start_d = tri.normal.dot(start) - expand;
    let end_d = tri.normal.dot(start + delta) - expand;
    resolve_ray_plane_intersect(start_d, end_d, tri.normal, tri.dist, helper)
}

/// `CDispCollTree::EdgeCrossAxis<AXIS>` (`dispcoll_common.cpp:1285`).
///
/// One of the nine planes formed by crossing a triangle edge with an axis. The
/// test is two-dimensional: the plane contains `axis`, so the component along
/// it is zero and the *distance* is stored there instead — see
/// [`Triangle::cross`].
fn edge_cross_axis(
    start: Vec3,
    delta: Vec3,
    extents: Vec3,
    axis: usize,
    plane: Option<Vec3>,
    helper: &mut Helper,
) -> bool {
    let Some(mut normal) = plane else {
        // `DISPCOLL_NORMAL_UNDEF`: the edge is parallel to this axis, so the
        // cross product is degenerate and there is no separating axis here.
        return true;
    };

    let a = (axis + 1) % 3;
    let b = (axis + 2) % 3;

    let dist = normal[axis];
    normal[axis] = 0.0;

    let extent = |i: usize| {
        if normal[i] < 0.0 {
            extents[i]
        } else {
            -extents[i]
        }
    };
    let expand = dist - (normal[a] * extent(a) + normal[b] * extent(b));
    let start_d = normal[a] * start[a] + normal[b] * start[b] - expand;
    let end_d = normal[a] * (start[a] + delta[a]) + normal[b] * (start[b] + delta[b]) - expand;

    resolve_ray_plane_intersect(start_d, end_d, normal, dist, helper)
}

impl Triangle {
    /// `CDispCollTri::CalcPlane` (`dispcoll_common.cpp:83`).
    ///
    /// The normal is `(v2 - v0) × (v1 - v0)` — note the order, which is the
    /// reverse of the obvious one and is why every caller passes the vertices
    /// as `v0, v2, v1`. It points **out of the terrain**: measured on the
    /// depot, where a displacement's base face still has solid on one side, the
    /// solid is on the `-normal` side 60 times to 8.
    fn calc_plane(&mut self, verts: &[Vec3]) {
        let v = |i: usize| verts[self.verts[i] as usize];
        let edge0 = v(1) - v(0);
        let edge1 = v(2) - v(0);
        self.normal = normalize(edge1.cross(edge0));
        self.dist = self.normal.dot(v(0));

        // `m_ucPlaneType` is set only for an exactly `+1` component. Valve also
        // records `m_ucSignBits` here, for the eight-way corner switch in
        // `BoxOnPlaneSide`; [`box_on_plane_side`] picks the corner
        // arithmetically instead, so there is nothing to store.
        self.axis = (0..3).find(|&i| self.normal[i] == 1.0);
    }

    /// `CDispCollTri::FindMinMax` (`dispcoll_common.cpp:133`).
    fn find_min_max(&mut self, verts: &[Vec3]) {
        let v = |i: usize| verts[self.verts[i] as usize];
        for axis in 0..3 {
            let c = [v(0)[axis], v(1)[axis], v(2)[axis]];
            // `FindMin`/`FindMax` keep the *first* extreme on a tie, which
            // `min_by`/`max_by` would not for `max`.
            let mut lo = 0;
            let mut hi = 0;
            for i in 1..3 {
                if c[i] < c[lo] {
                    lo = i;
                }
                if c[i] > c[hi] {
                    hi = i;
                }
            }
            self.min[axis] = lo as u8;
            self.max[axis] = hi as u8;
        }
    }

    /// `Cache_Create` (`dispcoll_common.cpp:1073`) — the nine edge-cross
    /// planes, computed once because they depend only on the geometry.
    fn cache_edge_planes(&mut self, verts: &[Vec3]) {
        let v = |i: usize| verts[self.verts[i] as usize];
        // Each edge is paired with the vertex *off* it, which is what decides
        // the plane's facing: the triangle must end up behind it.
        let edges = [
            (v(1) - v(0), v(0), v(2)),
            (v(2) - v(1), v(1), v(0)),
            (v(0) - v(2), v(2), v(1)),
        ];
        for (edge, (vector, on_edge, off_edge)) in edges.into_iter().enumerate() {
            for axis in 0..3 {
                self.cross[axis][edge] = edge_cross_plane(axis, vector, on_edge, off_edge);
            }
        }
    }
}

/// `Cache_EdgeCrossAxisX`/`Y`/`Z` (`dispcoll_common.cpp:1138`, `:1188`,
/// `:1236`), which are one function written three times.
///
/// The plane's normal is `axis × edge`, so its component along `axis` is zero —
/// and that slot is then reused to carry the plane's distance. `None` when the
/// cross product is degenerate on either of the other two axes, which is
/// `DISPCOLL_NORMAL_UNDEF`.
fn edge_cross_plane(axis: usize, edge: Vec3, on_edge: Vec3, off_edge: Vec3) -> Option<Vec3> {
    let a = (axis + 1) % 3;
    let b = (axis + 2) % 3;

    // x: (0, e.z, -e.y); y: (-e.z, 0, e.x); z: (e.y, -e.x, 0) — which is
    // `[a] = edge[b]`, `[b] = -edge[a]` for every axis.
    let mut normal = Vec3::ZERO;
    normal[a] = edge[b];
    normal[b] = -edge[a];
    let mut normal = normalize(normal);

    if normal[a] == 0.0 || normal[b] == 0.0 {
        return None;
    }

    let dist = normal[a] * on_edge[a] + normal[b] * on_edge[b];
    let off_dist = normal[a] * off_edge[a] + normal[b] * off_edge[b];

    // Flip the plane if the third vertex is in front of it, so that the whole
    // triangle is behind — unless the third vertex is *on* it, in which case
    // the facing is already right and flipping would lose the edge.
    if (off_dist - dist).abs() >= DIST_EPSILON && off_dist > dist {
        normal[a] = -normal[a];
        normal[b] = -normal[b];
        normal[axis] = -dist;
    } else {
        normal[axis] = dist;
    }
    Some(normal)
}

/// Two triangles per grid cell — `GenerateCollisionSurface`
/// (`builddisp.cpp:977`) and `CreateTris` (`:3073`), which between them turn
/// the render index list into the collision triangle list.
///
/// **The diagonal alternates**, on the parity of the cell's *vertex* index
/// rather than of the cell itself, which is how a displacement avoids the
/// directional bias a uniform diagonal would give it.
fn build_tris(info: &DispInfo, bsp: &Bsp, verts: &[Vec3]) -> Vec<Triangle> {
    let width = (1usize << info.power) + 1;
    let first_tri = info.disp_tri_start as usize;
    let first_vert = info.disp_vert_start as usize;

    let mut tris = Vec::with_capacity(DispInfo::tri_count(info.power));
    for v in 0..width - 1 {
        for u in 0..width - 1 {
            let n = v * width + u;
            let (a, b) = match n % 2 == 1 {
                // Top left to bottom right.
                true => ([n, n + width, n + 1], [n + 1, n + width, n + width + 1]),
                // Bottom left to top right.
                false => ([n, n + width, n + width + 1], [n, n + width + 1, n + 1]),
            };
            for indices in [a, b] {
                let tags = bsp.disp_tris[first_tri + tris.len()].tags;
                let flags = (tags | surf_prop_flag(bsp, first_vert, &indices) | disp_surf::SURFACE)
                    & disp_surf::MASK;
                tris.push(Triangle {
                    verts: indices.map(|i| i as u16),
                    min: [0; 3],
                    max: [0; 3],
                    normal: Vec3::ZERO,
                    dist: 0.0,
                    axis: None,
                    flags,
                    cross: [[None; 3]; 3],
                });
            }
        }
    }
    debug_assert_eq!(tris.len(), DispInfo::tri_count(info.power));
    debug_assert!(tris
        .iter()
        .all(|t| t.verts.iter().all(|&v| (v as usize) < verts.len())));
    tris
}

/// Which `$surfaceprop` slot a triangle reports, from its vertices' blend alpha
/// — `AABBTree_CopyDispData` (`dispcoll_common.cpp:400`).
///
/// Two slots, not four: the four-way form needs `LUMP_DISP_MULTIBLEND`, and no
/// Portal 2 displacement sets `DISP_INFO_FLAG_HAS_MULTIBLEND`.
///
/// The bits reach [`Trace::disp_flags`](super::Trace::disp_flags) and go no
/// further, because resolving a slot to a surface property needs the physics
/// property database that arrives with `vphysics/`.
fn surf_prop_flag(bsp: &Bsp, first_vert: usize, indices: &[usize; 3]) -> u16 {
    let total: f32 = indices
        .iter()
        .map(|&i| bsp.disp_verts[first_vert + i].alpha)
        .sum();
    if total > ALPHA_PROP_DELTA {
        disp_surf::SURFPROP2
    } else {
        disp_surf::SURFPROP1
    }
}

/// `Nodes_CalcCount` (`public/dispcoll_common.h:368`) — every node of a
/// four-way tree of the given depth, leaves included.
fn nodes_calc_count(power: i32) -> usize {
    (1usize << ((power + 1) * 2)) / 3
}

/// `Nodes_GetChild` (`public/dispcoll_common.h:355`).
fn child(node: usize, direction: usize) -> usize {
    (node << 2) + direction + 1
}

/// `Nodes_GetIndexFromComponents` (`public/dispcoll_common.h:414`) — the two
/// coordinates interleaved bit by bit, which is a Morton code.
fn morton(x: usize, y: usize) -> usize {
    let mut index = 0;
    for (start, mut v) in [(0, x), (1, y)] {
        let mut shift = start;
        while v != 0 {
            index |= (v & 1) << shift;
            shift += 2;
            v >>= 1;
        }
    }
    index
}

/// One quarter of `IntersectRayWithFourBoxes` (`dispcoll_common.cpp:154`),
/// scalar.
///
/// The box is grown by the swept box's extents so that the sweep becomes a
/// point, which is the same Minkowski expansion the brush path does — and the
/// reciprocal is passed in because the caller computed it once per trace.
fn ray_hits_box(start: Vec3, extents: Vec3, inv_delta: Vec3, mins: Vec3, maxs: Vec3) -> bool {
    let hit_mins = (mins - start - extents) * inv_delta;
    let hit_maxs = (maxs - start + extents) * inv_delta;
    let entry = hit_mins.min(hit_maxs);
    let exit = hit_mins.max(hit_maxs);
    entry.max_element().max(0.0) <= exit.min_element().min(1.0)
}

/// `BoxOnPlaneSide` (`mathlib/mathlib_base.cpp:937`) and the `BOX_ON_PLANE_SIDE`
/// macro that shortcuts it: bit 0 means the box reaches in front of the plane,
/// bit 1 that it reaches behind.
///
/// Shared with [`CollisionBsp`](super::CollisionBsp)'s per-leaf displacement
/// lists, which push a patch's bounds down the *world* tree with the same
/// question.
pub(super) fn box_on_plane_side(
    mins: Vec3,
    maxs: Vec3,
    normal: Vec3,
    dist: f32,
    axis: Option<usize>,
) -> u8 {
    // The axial fast path. Only correct for a `+1` normal, which is the only
    // kind either caller records an axis for.
    if let Some(axis) = axis {
        if dist <= mins[axis] {
            return 1;
        }
        if dist >= maxs[axis] {
            return 2;
        }
        return 3;
    }

    // The general case, which Valve writes as an eight-way switch on the sign
    // bits picking a corner per branch. Choosing the corner arithmetically is
    // the same two numbers: the furthest and nearest corner along the normal.
    let center = (mins + maxs) * 0.5;
    let extents = maxs - center;
    let along = normal.dot(center);
    let reach = normal.abs().dot(extents);

    let mut sides = 0;
    if along + reach >= dist {
        sides |= 1;
    }
    if along - reach < dist {
        sides |= 2;
    }
    sides
}

/// `IsBoxIntersectingTriangle` (`public/collisionutils.cpp:2823`) — the
/// separating-axis test without a direction of travel.
///
/// Thirteen axes again, and the same thirteen: three axis planes, nine edge
/// crosses, and the face plane. The edge-cross half is written here as one loop
/// where Valve has nine hand-specialised `AxisTestEdgeCross*` functions
/// differing only in which two vertices they compare.
fn box_intersects_triangle(
    center: Vec3,
    extents: Vec3,
    v1: Vec3,
    v2: Vec3,
    v3: Vec3,
    tri: &Triangle,
) -> bool {
    let p = [v1 - center, v2 - center, v3 - center];

    // The three axis planes.
    for axis in 0..3 {
        let (mut lo, mut hi) = (p[0][axis], p[0][axis]);
        for q in &p[1..] {
            lo = lo.min(q[axis]);
            hi = hi.max(q[axis]);
        }
        if lo > extents[axis] || hi < -extents[axis] {
            return false;
        }
    }

    // The nine edge crosses. For edge `e` between `p[e]` and `p[(e+1)%3]`, the
    // plane normal is `axis × edge`, and the two points compared are the two
    // *not* both on the edge — which is what Valve's choice of `(p1,p3)`,
    // `(p1,p2)` and `(p2,p3)` per case spells out one function at a time.
    for e in 0..3 {
        let edge = p[(e + 1) % 3] - p[e];
        for axis in 0..3 {
            let a = (axis + 1) % 3;
            let b = (axis + 2) % 3;
            // The same `(edge[b], -edge[a])` normal as `edge_cross_plane`, left
            // unnormalized here exactly as Valve leaves it — the comparison is
            // against a box reach built from the same unnormalized components,
            // so the scale cancels.
            let project = |q: Vec3| edge[b] * q[a] - edge[a] * q[b];
            let d0 = project(p[e]);
            let d1 = project(p[(e + 2) % 3]);
            let reach = edge[b].abs() * extents[a] + edge[a].abs() * extents[b];
            if d0.min(d1) > reach || d0.max(d1) < -reach {
                return false;
            }
        }
    }

    // The face plane.
    box_on_plane_side(
        center - extents,
        center + extents,
        tri.normal,
        tri.dist,
        tri.axis,
    ) == 3
}
