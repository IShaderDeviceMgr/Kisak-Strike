//! Visibility: which of a map's faces a frame actually draws.
//!
//! Replaces `engine/mod_vis.cpp` (467), the areaportal half of
//! `engine/cmodel.cpp` (about 180 of its 4,067), and `engine/r_areaportal.cpp`
//! (623) — plus the parts of `gl_rsurf.cpp`'s `R_RecursiveWorldNode` that are
//! *what to draw* rather than *how to draw it*. See
//! `portdocs/ENGINE_WORLD_VIS.md`.
//!
//! Three filters, in the order they run, each strictly cheaper than the one
//! before it and each able to answer on its own:
//!
//! 1. **The areas.** Flow out of the area the eye is in, through the
//!    `func_areaportal` windows that are open and facing the viewer, narrowing
//!    a screen-space rectangle at each one. What comes out is the set of areas
//!    a frame can reach and, for each, a frustum no wider than the window it
//!    was seen through — `R_SetupAreaBits`.
//! 2. **The PVS.** `vvis` wrote, for every cluster, the set of clusters it can
//!    possibly see. One bit test per leaf — `Map_VisMark`.
//! 3. **The frustum.** A node or leaf whose bounding box is entirely outside
//!    one of six planes is skipped, along with everything under it —
//!    `R_CullNode`.
//!
//! Measured over the depot, filter 2 alone leaves **28.9% of `sp_a1_intro1`'s
//! faces** standing on an average frame, and 8.4% of `sp_a2_intro`'s.
//!
//! # What this is not
//!
//! **Not occlusion.** `engine/OcclusionSystem.cpp` (2,999) is a separate
//! runtime system driven by `func_occluder` brushes, and it is not ported:
//! Portal 2 places **no `func_occluder` at all** in its 106 maps, so there is
//! nothing for it to do.
//!
//! **Not the audible set.** `LUMP_VISIBILITY` carries a PAS row beside every
//! PVS row and [`Bsp::pvs`](super::bsp::Bsp::pvs) reads only the first of the
//! pair, because the audible set belongs to sound and there is no sound.

use std::ops::Range;

use glam::{Mat4, Vec3, Vec4};

use super::bsp::{self, Bsp};

/// `CONTENTS_SOLID` (`public/bspflags.h:22`) — the one content flag this
/// module asks about, and the test for whether the camera is inside geometry.
const CONTENTS_SOLID: i32 = 0x1;

/// How far behind an areaportal's plane the viewer may be and still be allowed
/// through it — `flDist < -0.1f` (`r_areaportal.cpp:285`).
///
/// Not zero, because the viewer standing exactly in the window's plane is the
/// normal case for a doorway you are walking through, and a strict test would
/// flicker the far side out for one frame.
const AREAPORTAL_BEHIND: f32 = -0.1;

/// How near the plane the viewer has to be before the window stops being
/// clipped at all — `m_fDistToAreaPortalTolerance` (`r_areaportal.cpp:296`).
///
/// Inside this distance the window fills the view by construction and
/// projecting its corners would divide by something near zero, so the whole
/// screen is used instead.
const AREAPORTAL_TOLERANCE: f32 = 0.1;

/// `MAX_PORTAL_VERTS` (`r_areaportal.cpp:32`) — the clip buffer's size.
///
/// A window that starts with more corners than this is truncated rather than
/// rejected, which is Valve's `MIN( m_nClipPortalVerts, MAX_PORTAL_VERTS )`.
/// The widest in the depot has 8.
const MAX_PORTAL_VERTS: usize = 32;

/// A plane, as `normal · p - dist`.
///
/// Two kinds of plane end up in this type and they point opposite ways. A
/// `.bsp` plane keeps the file's own normal, so a node's child 0 is the
/// positive side and an areaportal's normal faces **out of** the area it looks
/// into. A [`Frustum`]'s six point **inwards**, so that "is this still inside"
/// is one sign test. [`accepts_box`](Plane::accepts_box) is written for the
/// second kind; [`distance`](Plane::distance) serves both.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Plane {
    normal: Vec3,
    dist: f32,
}

impl Plane {
    /// A plane from a clip-space row: `a·x + b·y + c·z + d >= 0`, normalized.
    ///
    /// The Gribb-Hartmann extraction. A degenerate row — which only happens
    /// for a projection with no extent along that axis — becomes a plane that
    /// accepts everything, so a bad matrix draws too much rather than nothing.
    fn from_row(row: Vec4) -> Plane {
        let normal = row.truncate();
        let length = normal.length();
        match length > 1e-12 {
            true => Plane {
                normal: normal / length,
                dist: -row.w / length,
            },
            false => Plane {
                normal: Vec3::Z,
                dist: f32::NEG_INFINITY,
            },
        }
    }

    fn distance(&self, point: Vec3) -> f32 {
        self.normal.dot(point) - self.dist
    }

    /// Whether any part of the box is on the inward side of a frustum plane.
    ///
    /// Tests the box's **support point** along the normal — the corner
    /// furthest in that direction — which is exact for an axis-aligned box
    /// rather than conservative. `CullNodeSIMD` computes the same corner by
    /// selecting per axis; this is the same select written once.
    fn accepts_box(&self, mins: Vec3, maxs: Vec3) -> bool {
        let corner = Vec3::select(self.normal.cmpge(Vec3::ZERO), maxs, mins);
        self.distance(corner) >= 0.0
    }
}

/// The four side planes of a [`Frustum`], in `FRUSTUM_*` order
/// (`mathlib.h:85`): right, left, top, bottom. Near and far follow them.
const SIDES: usize = 4;

/// Six planes bounding what a view can see.
///
/// `Frustum_t`, and built the way nothing in Valve's tree builds one: straight
/// out of the view-projection matrix rather than from a camera basis and a
/// field of view. The two agree — a plane of the frustum *is* a row
/// combination of the matrix — and taking it from the matrix means the frustum
/// cannot disagree with what is actually drawn, which is the failure this
/// avoids. It also makes the orthographic case fall out rather than needing
/// `R_SetupVisibleAreaFrustums`' second branch.
#[derive(Debug, Clone, Copy)]
pub struct Frustum {
    planes: [Plane; 6],
}

impl Frustum {
    /// The six planes of `view_proj`, in `FRUSTUM_*` order.
    ///
    /// The depth convention is `wgpu`'s and DirectX's — clip `z` runs `0..w`,
    /// not `-w..w` — so the near plane is row 2 alone rather than `row3 +
    /// row2`. Getting that wrong puts the near plane at the far plane's
    /// distance and culls nothing.
    pub fn new(view_proj: Mat4) -> Frustum {
        let row = |i: usize| view_proj.row(i);
        Frustum {
            planes: [
                Plane::from_row(row(3) - row(0)), // right
                Plane::from_row(row(3) + row(0)), // left
                Plane::from_row(row(3) - row(1)), // top
                Plane::from_row(row(3) + row(1)), // bottom
                Plane::from_row(row(2)),          // near
                Plane::from_row(row(3) - row(2)), // far
            ],
        }
    }

    /// A frustum that accepts everything — what `r_novis` and a map with no
    /// tree cull against.
    fn everything() -> Frustum {
        Frustum {
            planes: [Plane {
                normal: Vec3::Z,
                dist: f32::NEG_INFINITY,
            }; 6],
        }
    }

    /// Whether any part of the box is inside all six planes.
    ///
    /// False positives are possible and harmless — a box can be outside the
    /// frustum while straddling all six planes' good sides — and Valve's
    /// `CullNodeSIMD` has exactly the same slack.
    pub fn intersects(&self, mins: Vec3, maxs: Vec3) -> bool {
        self.planes.iter().all(|p| p.accepts_box(mins, maxs))
    }

    /// This frustum narrowed to a rectangle of its own image, in normalized
    /// device coordinates — `R_SetupVisibleAreaFrustums`.
    ///
    /// Valve remaps the rectangle into a view window sized by `tan(fov/2)` and
    /// builds four planes out of the camera basis. This does it as four row
    /// combinations instead: the half-space `x_ndc >= left` is
    /// `clip.x - left·clip.w >= 0`, which is `row0 - left·row3` applied to the
    /// world-space point. Same planes, no basis, and the orthographic case
    /// needs no second branch.
    fn narrowed(&self, view_proj: Mat4, rect: Rect) -> Frustum {
        let row = |i: usize| view_proj.row(i);
        Frustum {
            planes: [
                Plane::from_row(row(3) * rect.right - row(0)),
                Plane::from_row(row(0) - row(3) * rect.left),
                Plane::from_row(row(3) * rect.top - row(1)),
                Plane::from_row(row(1) - row(3) * rect.bottom),
                // Depth is the view's, not the window's: an areaportal narrows
                // where you can look, never how far.
                self.planes[4],
                self.planes[5],
            ],
        }
    }
}

/// A screen-space rectangle in normalized device coordinates, `-1..1` on both
/// axes with `y` up — `CPortalRect`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
}

impl Rect {
    /// The whole screen.
    const FULL: Rect = Rect {
        left: -1.0,
        right: 1.0,
        bottom: -1.0,
        top: 1.0,
    };

    /// An empty rectangle that any `union` grows to fit.
    const EMPTY: Rect = Rect {
        left: f32::MAX,
        right: f32::MIN,
        bottom: f32::MAX,
        top: f32::MIN,
    };

    fn grow_to(&mut self, x: f32, y: f32) {
        self.left = self.left.min(x);
        self.right = self.right.max(x);
        self.bottom = self.bottom.min(y);
        self.top = self.top.max(y);
    }

    fn union(&mut self, other: Rect) {
        self.left = self.left.min(other.left);
        self.right = self.right.max(other.right);
        self.bottom = self.bottom.min(other.bottom);
        self.top = self.top.max(other.top);
    }

    /// The overlap, or `None` when they do not overlap — `GetRectIntersection`.
    fn intersection(&self, other: Rect) -> Option<Rect> {
        let out = Rect {
            left: self.left.max(other.left),
            right: self.right.min(other.right),
            bottom: self.bottom.max(other.bottom),
            top: self.top.min(other.top),
        };
        (out.left < out.right && out.bottom < out.top).then_some(out)
    }
}

/// One node of the visibility tree.
///
/// A copy of the `.bsp`'s, kept because the [`Bsp`] is dropped when the map
/// finishes loading and this has to outlive it — the same reason
/// [`CollisionBsp`](crate::engine::trace::CollisionBsp) keeps its own.
#[derive(Debug, Clone, Copy)]
struct Node {
    plane: Plane,
    /// Front, then back. Negative means `-1 - child` is a leaf.
    children: [i32; 2],
    mins: Vec3,
    maxs: Vec3,
    /// The area every leaf below shares, or -1 when they differ. `mnode_t::area`.
    area: i16,
}

/// One leaf of it.
#[derive(Debug, Clone)]
struct Leaf {
    /// The PVS cluster, or -1 for a leaf `vvis` gave no row, which sees
    /// nothing.
    cluster: i32,
    /// `contents & CONTENTS_SOLID` — whether a camera here is inside the
    /// world's geometry. Not the same question as `cluster < 0`: a leaf
    /// outside the map's shell is not solid and still has no cluster.
    solid: bool,
    area: u16,
    mins: Vec3,
    maxs: Vec3,
    /// This leaf's slice of [`Visibility::leaf_faces`].
    faces: Range<u32>,
}

/// One window between two areas.
#[derive(Debug, Clone)]
struct AreaPortal {
    key: u16,
    other_area: u16,
    /// Facing **out of** `other_area`, so the viewer must be in front of it.
    plane: Plane,
    /// This window's outline, slicing [`Visibility::portal_verts`].
    verts: Range<u32>,
}

/// A map's visibility data, and the areaportal states that change at runtime.
///
/// Built once by [`build`](Visibility::build) and owned by
/// [`World`](super::World) for the map's lifetime. The one mutable part is
/// which areaportals are open, which `func_areaportal` drives through
/// [`set_area_portal`](Visibility::set_area_portal).
#[derive(Debug, Default)]
pub struct Visibility {
    nodes: Vec<Node>,
    leaves: Vec<Leaf>,
    /// Every leaf's faces, concatenated. Wider than the `u16` the lump holds
    /// because the displacements appended to it (see
    /// [`build`](Visibility::build)) push past 65,535 on no shipped map but
    /// could, and because a face index is compared against
    /// [`face_count`](Visibility::face_count) rather than stored densely.
    leaf_faces: Vec<u32>,
    face_count: usize,
    /// The node each node hangs under, or -1 for the root — what the
    /// leaf-to-root mark walks. `mnode_t::parent`, which Valve fills in the
    /// loader for exactly this.
    parents: Vec<i32>,
    /// The node each *leaf* hangs under. The same field on `mleaf_t`, which
    /// shares `mnode_t`'s header in the shipped engine and so needs no second
    /// array; the two are separate types here.
    leaf_parents: Vec<i32>,
    /// Every cluster's decompressed PVS row, back to back.
    ///
    /// Decompressed **once, at load**, where the shipped engine decompresses
    /// one row per frame and caches the marked leaf list instead
    /// (`VisCache_Build`). The trade is the other way round here because the
    /// rows are small — 135 KB for the largest map in the game, and 39 KB for
    /// `sp_a1_intro1` — and because it takes the run-length decoder out of the
    /// frame entirely.
    pvs: Vec<u8>,
    cluster_bytes: usize,
    clusters: usize,
    /// Each area's slice of [`portals`](Visibility::portals).
    areas: Vec<Range<u32>>,
    portals: Vec<AreaPortal>,
    portal_verts: Vec<Vec3>,
    /// Whether each areaportal **key** is open, indexed by key.
    ///
    /// **All open at load, where the shipped engine starts them all closed**
    /// (`cmodel_bsp.cpp:959`) and waits for every `func_areaportal` to open
    /// itself in `Precache`. Starting closed here would black out any map
    /// whose areaportals have no entity — and 922 areaportal records in the
    /// depot answer to only 409 entities — so the port starts where the game
    /// ends up. `CAreaPortal`'s own constructor is `m_state = AREAPORTAL_OPEN`.
    open: Vec<bool>,
    /// Each area's flood number — `carea_t::floodnum`. Areas with the same one
    /// are reachable from each other through open windows.
    flood: Vec<u16>,
}

impl Visibility {
    /// Reads a map's visibility lumps and builds the tree.
    ///
    /// A map with no nodes, no leaves or no visibility lump gives an empty
    /// [`Visibility`], which reports everything visible — the same answer
    /// `CM_NullVis` gives, and the safe one, because it draws too much rather
    /// than too little.
    pub fn build(bsp: &Bsp) -> Visibility {
        if bsp.nodes.is_empty() || bsp.leaves.is_empty() {
            return Visibility::default();
        }

        let nodes: Vec<Node> = bsp
            .nodes
            .iter()
            .map(|node| Node {
                plane: plane_of(&bsp.planes[node.plane_num.max(0) as usize]),
                children: node.children,
                mins: shorts(node.mins),
                maxs: shorts(node.maxs),
                area: node.area,
            })
            .collect();

        // `Mod_LoadNodes`' second pass (`modelloader.cpp`), which walks the
        // tree once setting every child's parent. Done here rather than stored
        // because the lump has no parent field.
        let mut parents = vec![-1i32; nodes.len()];
        let mut leaf_parents = vec![-1i32; bsp.leaves.len()];
        let mut pending = vec![0i32];
        while let Some(index) = pending.pop() {
            for child in nodes[index as usize].children {
                match child >= 0 {
                    true => {
                        parents[child as usize] = index;
                        pending.push(child);
                    }
                    false => leaf_parents[(-1 - child) as usize] = index,
                }
            }
        }

        // Each leaf's faces: the lump's list, then the displacements whose
        // bounds reach into it.
        //
        // **The second half is not in any lump.** `LUMP_LEAFFACES` names no
        // displacement face at all — measured over the depot, 0 of the game's
        // 1,181 — because the shipped engine gives a leaf a second list built
        // by the loader (`mleaf_t::dispListStart`). Appending them to the one
        // list is this port's version of that, and it works here because a
        // displacement is an ordinary face by the time it reaches a batch.
        let disp_leaves = displacement_leaves(bsp, &nodes);
        let mut leaf_faces: Vec<u32> = Vec::with_capacity(bsp.leaf_faces.len() + disp_leaves.len());
        let leaves: Vec<Leaf> = bsp
            .leaves
            .iter()
            .enumerate()
            .map(|(index, leaf)| {
                let first = leaf_faces.len() as u32;
                let lump = leaf.first_leaf_face as usize;
                leaf_faces.extend(
                    bsp.leaf_faces[lump..lump + leaf.num_leaf_faces as usize]
                        .iter()
                        .map(|&face| u32::from(face)),
                );
                leaf_faces.extend(
                    disp_leaves
                        .iter()
                        .filter(|&&(_, leaf)| leaf == index as u32)
                        .map(|&(face, _)| face),
                );
                Leaf {
                    cluster: i32::from(leaf.cluster),
                    solid: leaf.contents & CONTENTS_SOLID != 0,
                    area: leaf.area(),
                    mins: shorts(leaf.mins),
                    maxs: shorts(leaf.maxs),
                    faces: first..leaf_faces.len() as u32,
                }
            })
            .collect();

        let clusters = bsp.cluster_count();
        let cluster_bytes = bsp.cluster_bytes();
        let mut pvs = Vec::with_capacity(clusters * cluster_bytes);
        let mut row = Vec::new();
        for cluster in 0..clusters {
            bsp.pvs(cluster, &mut row);
            pvs.extend_from_slice(&row);
        }

        let areas: Vec<Range<u32>> = bsp
            .areas
            .iter()
            .map(|area| {
                let first = area.first_area_portal.max(0) as u32;
                first..first + area.num_area_portals.max(0) as u32
            })
            .collect();
        let portals: Vec<AreaPortal> = bsp
            .area_portals
            .iter()
            .map(|portal| AreaPortal {
                key: portal.key,
                other_area: portal.other_area,
                plane: plane_of(&bsp.planes[portal.plane_num.max(0) as usize]),
                verts: u32::from(portal.first_clip_portal_vert)
                    ..u32::from(portal.first_clip_portal_vert)
                        + u32::from(portal.num_clip_portal_verts),
            })
            .collect();
        let keys = portals
            .iter()
            .map(|p| usize::from(p.key))
            .max()
            .unwrap_or(0);

        let mut vis = Visibility {
            nodes,
            leaves,
            leaf_faces,
            face_count: bsp.faces.len(),
            parents,
            leaf_parents,
            pvs,
            cluster_bytes,
            clusters,
            areas,
            portals,
            portal_verts: bsp
                .clip_portal_verts
                .iter()
                .map(|&v| Vec3::from(v))
                .collect(),
            open: vec![true; keys + 1],
            flood: Vec::new(),
        };
        vis.flood();
        vis
    }

    /// Whether this map has no visibility data, in which case everything is
    /// drawn.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn cluster_count(&self) -> usize {
        self.clusters
    }

    /// How many areas the map has, area 0 included — the `vis` command's
    /// denominator and nothing else's.
    #[allow(dead_code)]
    pub fn area_count(&self) -> usize {
        self.areas.len()
    }

    pub fn area_portal_count(&self) -> usize {
        self.portals.len()
    }

    /// The leaf a point is in — `CM_PointLeafnum_r` (`cmodel.cpp:444`).
    ///
    /// Leaf 0 when the map has no tree, because a point is always *somewhere*;
    /// leaf 0 of a real map is the solid leaf `vbsp` writes first.
    pub fn leaf_at(&self, point: Vec3) -> usize {
        if self.nodes.is_empty() {
            return 0;
        }
        let mut index = 0i32;
        while index >= 0 {
            let node = &self.nodes[index as usize];
            // The inward sense is this port's; `CM_PointLeafnum_r` descends
            // child 0 when the point is in *front* of an outward-facing plane,
            // which is the same side.
            index = node.children[usize::from(node.plane.distance(point) < 0.0)];
        }
        (-1 - index) as usize
    }

    /// The area a point is in — `CM_LeafArea( CM_PointLeafnum( p ) )`.
    pub fn area_at(&self, point: Vec3) -> u16 {
        match self.leaves.get(self.leaf_at(point)) {
            Some(leaf) => leaf.area,
            None => 0,
        }
    }

    /// The PVS cluster a point is in, or -1 in solid space —
    /// `CM_LeafCluster( CM_PointLeafnum( p ) )`.
    pub fn cluster_at(&self, point: Vec3) -> i32 {
        match self.leaves.get(self.leaf_at(point)) {
            Some(leaf) => leaf.cluster,
            None => -1,
        }
    }

    /// Opens or closes one areaportal, by the `portalnumber` a
    /// `func_areaportal` carries — `CM_SetAreaPortalState`
    /// (`cmodel.cpp:3501`).
    ///
    /// Re-floods the areas, which is what the original does on every call and
    /// which costs a walk of at most 33 areas.
    ///
    /// The engine drives the areaportals through
    /// [`set_area_portals`](Visibility::set_area_portals) instead, one report
    /// for all of them, so this has no caller outside the tests. It is kept
    /// because it is the operation — the plural is the batching of it.
    #[allow(dead_code)]
    pub fn set_area_portal(&mut self, key: u16, open: bool) {
        let Some(slot) = self.open.get_mut(usize::from(key)) else {
            return;
        };
        if *slot == open {
            return;
        }
        *slot = open;
        self.flood();
    }

    /// Sets every areaportal at once — `CM_SetAreaPortalStates`
    /// (`cmodel.cpp:3517`), which exists so that the flood runs once instead
    /// of once per portal.
    ///
    /// Keys not named are **left alone**, which is the divergence
    /// [`open`](Visibility::open) explains: an areaportal record with no
    /// entity has nobody to report its state and must stay open.
    pub fn set_area_portals(&mut self, states: &[(u16, bool)]) {
        let mut changed = false;
        for &(key, open) in states {
            if let Some(slot) = self.open.get_mut(usize::from(key)) {
                changed |= *slot != open;
                *slot = open;
            }
        }
        if changed {
            self.flood();
        }
    }

    pub fn area_portal_is_open(&self, key: u16) -> bool {
        self.open.get(usize::from(key)).copied().unwrap_or(true)
    }

    /// Whether two areas are reachable from each other through open windows —
    /// `CM_AreasConnected` (`cmodel.cpp:3535`).
    ///
    /// **Nothing in the render path asks this**, and that is a divergence
    /// worth knowing. `R_FlowThroughArea` tests the server's `m_chAreaBits`
    /// before stepping into an area, because in the shipped engine the flood
    /// runs on the server and reaches the client as a bit vector. Here there
    /// is one process and the flow already walks only open windows, so the
    /// test could never fire — flow is a subset of flood by construction. It
    /// is kept as a query because it is the question `CM_LeavesConnected` and
    /// the sound system ask, neither of which is the renderer.
    #[allow(dead_code)]
    pub fn areas_connected(&self, a: u16, b: u16) -> bool {
        match (
            self.flood.get(usize::from(a)),
            self.flood.get(usize::from(b)),
        ) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        }
    }

    /// `FloodAreaConnections` (`cmodel.cpp:3480`): give every area reachable
    /// from another through open windows the same number.
    ///
    /// Area 0 is skipped, as it is there — `vbsp` never assigns it, so it is
    /// solid space and belongs to no flood.
    fn flood(&mut self) {
        self.flood = vec![0; self.areas.len()];
        let mut number = 0u16;
        for start in 1..self.areas.len() {
            if self.flood[start] != 0 {
                continue;
            }
            number += 1;
            let mut pending = vec![start];
            self.flood[start] = number;
            while let Some(area) = pending.pop() {
                for portal in self.areas[area].clone() {
                    let portal = &self.portals[portal as usize];
                    let other = usize::from(portal.other_area);
                    if !self.open[usize::from(portal.key)] || other >= self.flood.len() {
                        continue;
                    }
                    if self.flood[other] == 0 {
                        self.flood[other] = number;
                        pending.push(other);
                    }
                }
            }
        }
    }

    /// One frame's answer: which faces, leaves and areas this view reaches.
    ///
    /// `Map_VisSetup` + `R_SetupAreaBits` + `R_RecursiveWorldNode`'s pruning,
    /// in one call. `eye` is the camera's world-space origin and `view_proj`
    /// is what everything this frame is drawn with, so the frustum cannot
    /// disagree with the picture.
    ///
    /// `novis` is `r_novis`, and also what a caller passes when the view is
    /// somewhere the PVS cannot answer for — noclipping outside the world.
    /// See [`VisibleSet::everything`].
    pub fn mark(&self, eye: Vec3, view_proj: Mat4, novis: bool) -> VisibleSet {
        if self.is_empty() || novis {
            return VisibleSet::everything();
        }

        let frustum = Frustum::new(view_proj);
        let view_leaf = self.leaf_at(eye);
        let leaf = &self.leaves[view_leaf];

        // `g_bViewerInSolidSpace` (`r_areaportal.cpp:529`): a camera inside the
        // world's geometry has no area to flow out of, so every area is
        // offered and the base frustum does all the culling. The PVS is
        // untouched by this — a solid leaf's cluster is -1 and so sees
        // nothing, which is why `Map_VisMark` needs `g_bNoClipEnabled` as a
        // *separate* term to draw anything at all out there. Here that term is
        // the caller's `novis`.
        let in_solid = leaf.solid;

        let mut set = VisibleSet {
            all: false,
            faces: Bits::new(self.face_count),
            leaves: Bits::new(self.leaves.len()),
            areas: Bits::new(self.areas.len().max(1)),
            area_frustum: vec![None; self.areas.len()],
            frustum,
            in_solid,
            stats: VisStats {
                cluster: leaf.cluster,
                ..VisStats::default()
            },
        };

        // Phase one, the areas: which of them this view can reach, and through
        // how wide a window. `R_SetupAreaBits`.
        let mut rects: Vec<Option<Rect>> = vec![None; self.areas.len()];
        if in_solid {
            for area in 0..self.areas.len() {
                set.areas.set(area);
            }
        } else {
            self.flow(
                usize::from(leaf.area),
                eye,
                view_proj,
                &mut set,
                &mut rects,
                Rect::FULL,
                &mut Bits::new(self.areas.len().max(1)),
            );
            // `R_SetupVisibleAreaFrustums` (`r_areaportal.cpp:349`), which the
            // shipped engine runs after the flow and not during it, because an
            // area reached through two windows has to end up with the union of
            // both rectangles rather than the first one.
            for (area, rect) in rects.iter().enumerate() {
                if let Some(rect) = rect {
                    set.area_frustum[area] = Some(frustum.narrowed(view_proj, *rect));
                }
            }
        }
        set.stats.areas = set.areas.count();

        // Phase two, the PVS: mark every leaf the view cluster can see, then
        // walk each one's ancestors so that a subtree with nothing visible
        // under it can be skipped whole. `VisCache_Build` (`mod_vis.cpp:180`).
        //
        // **Kept apart from `set.leaves`**, which is the answer *after* the
        // frustum has had its say: a prop asking `any_leaf` must not be told
        // its leaf is visible when the walk never reached it.
        let row = self.row(leaf.cluster);
        let mut pvs_leaves = Bits::new(self.leaves.len());
        let mut node_visible = Bits::new(self.nodes.len());
        for (index, leaf) in self.leaves.iter().enumerate() {
            if leaf.cluster < 0 || !bit(row, leaf.cluster as usize) {
                continue;
            }
            pvs_leaves.set(index);
            let mut node = self.leaf_parents[index];
            while node >= 0 && !node_visible.get(node as usize) {
                node_visible.set(node as usize);
                node = self.parents[node as usize];
            }
        }
        set.stats.clusters = (0..self.clusters).filter(|&c| bit(row, c)).count();

        // Phase three, the frustum, and the faces that survive all three.
        self.walk(&pvs_leaves, &node_visible, &mut set);
        set.stats.leaves = set.leaves.count();
        set.stats.faces = set.faces.count();
        set
    }

    /// One cluster's PVS row. An out-of-range cluster — solid space — sees
    /// nothing, which is `CM_Vis`' `cluster == -1` branch.
    fn row(&self, cluster: i32) -> &[u8] {
        let start = match usize::try_from(cluster) {
            Ok(cluster) if cluster < self.clusters => cluster * self.cluster_bytes,
            _ => return &[],
        };
        &self.pvs[start..start + self.cluster_bytes]
    }

    /// `R_FlowThroughArea` (`r_areaportal.cpp:230`): step out of `area`
    /// through every open window facing the viewer, narrowing `clip` at each.
    ///
    /// `rects` accumulates the union of every rectangle an area was reached
    /// through, which is what the frustums are built from afterwards.
    #[allow(clippy::too_many_arguments)]
    fn flow(
        &self,
        area: usize,
        eye: Vec3,
        view_proj: Mat4,
        set: &mut VisibleSet,
        rects: &mut [Option<Rect>],
        clip: Rect,
        stack: &mut Bits,
    ) {
        if area >= self.areas.len() {
            return;
        }
        match &mut rects[area] {
            Some(rect) => rect.union(clip),
            slot @ None => *slot = Some(clip),
        }
        set.areas.set(area);
        stack.set(area);

        for index in self.areas[area].clone() {
            let portal = &self.portals[index as usize];
            let other = usize::from(portal.other_area);
            // Never back through a window already on the stack: an areaportal
            // graph has cycles, and the rectangle only ever shrinks along a
            // path.
            if other >= self.areas.len() || stack.get(other) {
                continue;
            }
            if !self.open[usize::from(portal.key)] {
                continue;
            }
            // The viewer has to be on the side the window faces to see
            // through it.
            let distance = portal.plane.distance(eye);
            if distance < AREAPORTAL_BEHIND {
                continue;
            }
            let rect = match distance > AREAPORTAL_TOLERANCE {
                true => match self.window_rect(portal, view_proj, &set.frustum) {
                    Some(rect) => rect,
                    None => continue,
                },
                // Standing in the window: it fills the view.
                false => Rect::FULL,
            };
            let Some(narrowed) = rect.intersection(clip) else {
                continue;
            };
            self.flow(other, eye, view_proj, set, rects, narrowed, stack);
        }

        stack.clear(area);
    }

    /// The screen-space extent of one window — `GetPortalScreenExtents`
    /// (`r_areaportal.cpp:105`).
    ///
    /// **Clipped against five planes where Valve clips against four.** Its
    /// loop is `iPlane < 4`, the sides only, which leaves a corner behind the
    /// eye to be projected with a negative `w` and fold the rectangle inside
    /// out. Adding the near plane cannot lose anything — geometry nearer than
    /// the near plane is not drawn — and it is what makes every surviving
    /// corner projectable.
    fn window_rect(&self, portal: &AreaPortal, view_proj: Mat4, frustum: &Frustum) -> Option<Rect> {
        let verts = &self.portal_verts[portal.verts.start as usize..portal.verts.end as usize];
        let mut current: Vec<Vec3> = verts.iter().take(MAX_PORTAL_VERTS).copied().collect();
        let mut next: Vec<Vec3> = Vec::with_capacity(MAX_PORTAL_VERTS);

        for plane in &frustum.planes[..SIDES + 1] {
            next.clear();
            if current.is_empty() {
                return None;
            }
            let mut previous = *current.last().expect("not empty");
            let mut previous_d = plane.distance(previous);
            for &point in &current {
                let d = plane.distance(point);
                if (d > 0.0) != (previous_d > 0.0) && next.len() < MAX_PORTAL_VERTS {
                    let t = previous_d / (previous_d - d);
                    next.push(previous.lerp(point, t));
                }
                if d > 0.0 && next.len() < MAX_PORTAL_VERTS {
                    next.push(point);
                }
                previous = point;
                previous_d = d;
            }
            std::mem::swap(&mut current, &mut next);
            if current.is_empty() {
                return None;
            }
        }

        let mut rect = Rect::EMPTY;
        for point in current {
            let clip = view_proj * point.extend(1.0);
            if clip.w <= 1e-6 {
                continue;
            }
            rect.grow_to(clip.x / clip.w, clip.y / clip.w);
        }
        (rect.left <= rect.right).then_some(rect)
    }

    /// `R_RecursiveWorldNode` (`gl_rsurf.cpp:4147`), minus the node-surface
    /// bookkeeping: descend, prune on the PVS and the frustum, and mark what
    /// the leaves that survive point at.
    ///
    /// Iterative rather than recursive. Valve's recursion descends the near
    /// child first so that a surface lying on a node's plane can be drawn in
    /// front-to-back order; nothing here depends on draw order, because a
    /// batch is gathered and then drawn once.
    fn walk(&self, pvs_leaves: &Bits, node_visible: &Bits, set: &mut VisibleSet) {
        let mut pending = vec![0i32];
        while let Some(index) = pending.pop() {
            if index < 0 {
                let leaf_index = (-1 - index) as usize;
                if !pvs_leaves.get(leaf_index) {
                    continue;
                }
                let leaf = &self.leaves[leaf_index];
                if self.cull(set, i32::from(leaf.area), leaf.mins, leaf.maxs) {
                    continue;
                }
                set.leaves.set(leaf_index);
                for face in leaf.faces.clone() {
                    set.faces.set(self.leaf_faces[face as usize] as usize);
                }
                continue;
            }

            let node = &self.nodes[index as usize];
            if !node_visible.get(index as usize) {
                continue;
            }
            if self.cull(set, i32::from(node.area), node.mins, node.maxs) {
                continue;
            }
            set.stats.nodes += 1;
            pending.extend(node.children);
        }
    }

    /// `R_CullNode` (`r_areaportal.cpp:464`): cull against the area's own
    /// frustum when the box belongs to exactly one area, and against the
    /// view's otherwise.
    fn cull(&self, set: &VisibleSet, area: i32, mins: Vec3, maxs: Vec3) -> bool {
        if !set.in_solid && area > 0 {
            let area = area as usize;
            if !set.areas.get(area) {
                return true;
            }
            if let Some(frustum) = &set.area_frustum[area] {
                return !frustum.intersects(mins, maxs);
            }
        }
        !set.frustum.intersects(mins, maxs)
    }

    /// Whether a world-space box reaches any leaf this frame can see.
    ///
    /// A descent rather than a scan: a box is tested against each node's plane
    /// with its own support radius, both sides taken when it straddles, and
    /// the walk stops at the first leaf that is in `set`. Short-circuiting is
    /// the point — most boxes are answered by the first leaf they reach.
    ///
    /// The frustum is already accounted for, because `set`'s leaves are the
    /// ones that survived it. A box in a visible leaf but outside the frustum
    /// is therefore reported visible; so is Valve's, which culls a renderable
    /// against the frustum separately in `CClientLeafSystem` and not here.
    pub fn box_visible(&self, set: &VisibleSet, mins: Vec3, maxs: Vec3) -> bool {
        if self.is_empty() || set.is_everything() {
            return true;
        }
        if !set.frustum.intersects(mins, maxs) {
            return false;
        }
        let center = (mins + maxs) * 0.5;
        let extents = (maxs - mins) * 0.5;
        let mut pending = vec![0i32];
        while let Some(index) = pending.pop() {
            if index < 0 {
                if set.leaves.get((-1 - index) as usize) {
                    return true;
                }
                continue;
            }
            let node = &self.nodes[index as usize];
            let distance = node.plane.distance(center);
            let radius = node.plane.normal.abs().dot(extents);
            if distance >= -radius {
                pending.push(node.children[0]);
            }
            if distance < radius {
                pending.push(node.children[1]);
            }
        }
        false
    }

    /// A one-line summary for the startup log.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "no visibility".to_owned();
        }
        format!(
            "{} clusters over {} leaves, {} areas, {} areaportals",
            self.clusters,
            self.leaves.len(),
            self.areas.len().saturating_sub(1),
            self.portals.len(),
        )
    }
}

/// Which faces, leaves and areas one view reaches. Owned, so that it can be
/// held across a draw that borrows the world it came from.
#[derive(Debug)]
pub struct VisibleSet {
    /// Everything is visible: `r_novis`, a map with no tree, or a caller that
    /// does not cull.
    all: bool,
    faces: Bits,
    leaves: Bits,
    areas: Bits,
    area_frustum: Vec<Option<Frustum>>,
    frustum: Frustum,
    in_solid: bool,
    pub stats: VisStats,
}

/// What one frame's visibility cost and bought. Reported by the `vis` console
/// command, and the numbers to watch after a change here.
#[derive(Debug, Clone, Copy, Default)]
pub struct VisStats {
    /// The cluster the eye is in, or -1 in solid space.
    pub cluster: i32,
    /// How many clusters that one can see.
    pub clusters: usize,
    pub leaves: usize,
    pub faces: usize,
    pub areas: usize,
    /// Interior nodes the walk descended into — the cost side.
    pub nodes: usize,
}

impl VisibleSet {
    /// A set that contains everything.
    ///
    /// What a caller that does not cull passes — the benchmark, the shader
    /// preview, a test — and what [`Visibility::mark`] answers for `r_novis`
    /// or a map with no tree.
    pub fn everything() -> VisibleSet {
        VisibleSet {
            all: true,
            faces: Bits::new(0),
            leaves: Bits::new(0),
            areas: Bits::new(0),
            area_frustum: Vec::new(),
            frustum: Frustum::everything(),
            in_solid: false,
            stats: VisStats::default(),
        }
    }

    /// Whether everything is visible, in which case a caller can skip the
    /// per-face gather and draw its static index buffer whole.
    pub fn is_everything(&self) -> bool {
        self.all
    }

    /// Whether one `.bsp` face index is drawn this frame.
    pub fn face(&self, index: usize) -> bool {
        self.all || self.faces.get(index)
    }

    /// Whether one leaf is drawn this frame — the per-leaf form of
    /// [`any_leaf`](VisibleSet::any_leaf), and what the depot test's
    /// self-visibility invariant asks.
    #[allow(dead_code)]
    pub fn leaf(&self, index: usize) -> bool {
        self.all || self.leaves.get(index)
    }

    /// Whether any leaf in the list is visible — `Map_AreAnyLeavesVisible`
    /// (`mod_vis.cpp:221`), which is how a static prop is culled.
    ///
    /// An **empty** list means visible. A prop whose leaf range is empty is
    /// one `vbsp` placed outside the tree, and the shipped engine's loop
    /// returns false for it — but the shipped engine also never asks, because
    /// `CStaticPropMgr` inserts props into leaves itself. Answering "visible"
    /// keeps a prop that would otherwise vanish for a reason nobody could see.
    pub fn any_leaf(&self, leaves: &[u16]) -> bool {
        self.all
            || leaves.is_empty()
            || leaves
                .iter()
                .any(|&leaf| self.leaves.get(usize::from(leaf)))
    }

    /// The view frustum this set was built from, for a caller culling
    /// something the tree does not know about.
    ///
    /// Nothing asks yet — every current caller is culled by a leaf list or a
    /// box against the tree, both of which have already had the frustum
    /// applied. The recursive view (`portdocs/PORTAL.md` §7) is what wants a
    /// bare frustum, because a portal's second camera has its own.
    #[allow(dead_code)]
    pub fn frustum(&self) -> &Frustum {
        &self.frustum
    }
}

/// A flat bitset. `Vec<bool>` would be eight times the memory for the same
/// answer, and the face set is asked `face_count` questions a frame.
#[derive(Debug, Clone)]
struct Bits {
    words: Vec<u64>,
    len: usize,
}

impl Bits {
    fn new(len: usize) -> Bits {
        Bits {
            words: vec![0; len.div_ceil(64)],
            len,
        }
    }

    fn get(&self, index: usize) -> bool {
        index < self.len && self.words[index / 64] & (1 << (index % 64)) != 0
    }

    fn set(&mut self, index: usize) {
        if index < self.len {
            self.words[index / 64] |= 1 << (index % 64);
        }
    }

    fn clear(&mut self, index: usize) {
        if index < self.len {
            self.words[index / 64] &= !(1 << (index % 64));
        }
    }

    fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }
}

/// One bit out of a decompressed PVS row. An empty row sees nothing, which is
/// `CM_Vis`' answer for a cluster of -1.
fn bit(row: &[u8], index: usize) -> bool {
    row.get(index >> 3)
        .is_some_and(|byte| byte & (1 << (index & 7)) != 0)
}

/// A `.bsp` plane, in this module's own type. The normal is the file's and is
/// not turned round — see [`Plane`].
fn plane_of(plane: &bsp::Plane) -> Plane {
    Plane {
        normal: Vec3::from(plane.normal),
        dist: plane.dist,
    }
}

fn shorts(v: [i16; 3]) -> Vec3 {
    Vec3::new(f32::from(v[0]), f32::from(v[1]), f32::from(v[2]))
}

/// Every `(face, leaf)` pair a displacement reaches — the port's
/// `mleaf_t::dispListStart`.
///
/// A displacement's geometry is pushed off its base quad along per-vertex
/// directions, so it can reach well outside the quad's own leaf; its bounds
/// are taken from the built grid rather than guessed at. Cheap because there
/// are so few: **1,181 in the whole game, 11 on `sp_a1_intro1`**, against
/// 318,694 faces.
fn displacement_leaves(bsp: &Bsp, nodes: &[Node]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for (index, face) in bsp.faces.iter().enumerate() {
        if face.disp_info < 0 {
            continue;
        }
        let Some(patch) = super::disp::Displacement::build(bsp, face) else {
            continue;
        };
        let (mins, maxs) = patch.vertices.iter().fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |bounds, vertex| (bounds.0.min(vertex.position), bounds.1.max(vertex.position)),
        );
        for leaf in leaves_in_box(nodes, mins, maxs) {
            out.push((index as u32, leaf));
        }
    }
    out.sort_unstable();
    out
}

/// Every leaf an axis-aligned box reaches — `CM_BoxLeafnums`
/// (`cmodel.cpp:552`), over the visibility tree rather than the collision one.
fn leaves_in_box(nodes: &[Node], mins: Vec3, maxs: Vec3) -> Vec<u32> {
    let mut out = Vec::new();
    if nodes.is_empty() {
        return out;
    }
    let center = (mins + maxs) * 0.5;
    let extents = (maxs - mins) * 0.5;
    let mut pending = vec![0i32];
    while let Some(index) = pending.pop() {
        if index < 0 {
            out.push((-1 - index) as u32);
            continue;
        }
        let plane = &nodes[index as usize].plane;
        let distance = plane.distance(center);
        // The box's radius along the normal: the support function of an
        // axis-aligned box, so the straddle test is exact.
        let radius = plane.normal.abs().dot(extents);
        let children = nodes[index as usize].children;
        if distance >= -radius {
            pending.push(children[0]);
        }
        if distance < radius {
            pending.push(children[1]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two rooms either side of one splitting plane, with a window between
    /// them.
    ///
    /// `x > 0` is leaf 0, cluster 0, area 1, and holds face 0; `x < 0` is leaf
    /// 1, cluster 1, area 2, and holds face 1. **The PVS is deliberately
    /// asymmetric** — cluster 0 sees only itself, cluster 1 sees both — which
    /// a real `vvis` would never write and which is exactly what proves the
    /// right row is being read rather than a symmetric guess.
    fn two_rooms() -> Visibility {
        let x = |dist: f32| Plane {
            normal: Vec3::X,
            dist,
        };
        let room = |sign: f32| {
            (
                Vec3::new(sign.min(0.0) * 100.0, -100.0, -100.0),
                Vec3::new(sign.max(0.0) * 100.0, 100.0, 100.0),
            )
        };
        let (near_mins, near_maxs) = room(1.0);
        let (far_mins, far_maxs) = room(-1.0);

        // The window: a 64-unit square in the splitting plane.
        let verts = vec![
            Vec3::new(0.0, -32.0, -32.0),
            Vec3::new(0.0, 32.0, -32.0),
            Vec3::new(0.0, 32.0, 32.0),
            Vec3::new(0.0, -32.0, 32.0),
        ];

        let mut vis = Visibility {
            nodes: vec![Node {
                plane: x(0.0),
                children: [-1, -2],
                mins: Vec3::splat(-100.0),
                maxs: Vec3::splat(100.0),
                area: -1,
            }],
            leaves: vec![
                Leaf {
                    cluster: 0,
                    solid: false,
                    area: 1,
                    mins: near_mins,
                    maxs: near_maxs,
                    faces: 0..1,
                },
                Leaf {
                    cluster: 1,
                    solid: false,
                    area: 2,
                    mins: far_mins,
                    maxs: far_maxs,
                    faces: 1..2,
                },
            ],
            leaf_faces: vec![0, 1],
            face_count: 2,
            parents: vec![-1],
            leaf_parents: vec![0, 0],
            pvs: vec![0b01, 0b11],
            cluster_bytes: 1,
            clusters: 2,
            areas: vec![0..0, 0..1, 1..2],
            portals: vec![
                // In area 1's list, looking into area 2. Its plane faces out
                // of area 2, which is `-X`, so a viewer at `+X` is in front.
                AreaPortal {
                    key: 1,
                    other_area: 2,
                    plane: Plane {
                        normal: Vec3::X,
                        dist: 0.0,
                    },
                    verts: 0..4,
                },
                // The same window in area 2's list, turned round.
                AreaPortal {
                    key: 1,
                    other_area: 1,
                    plane: Plane {
                        normal: -Vec3::X,
                        dist: 0.0,
                    },
                    verts: 0..4,
                },
            ],
            portal_verts: verts,
            open: vec![true, true],
            flood: Vec::new(),
        };
        vis.flood();
        vis
    }

    /// A camera at `eye` looking towards `at`, as the engine builds one.
    fn view(eye: Vec3, at: Vec3) -> Mat4 {
        let projection =
            glam::camera::rh::proj::directx::perspective(90f32.to_radians(), 1.0, 1.0, 10_000.0);
        projection * glam::camera::rh::view::look_at_mat4(eye, at, Vec3::Z)
    }

    #[test]
    fn a_point_lands_in_the_leaf_that_contains_it() {
        let vis = two_rooms();
        assert_eq!(vis.leaf_at(Vec3::new(50.0, 0.0, 0.0)), 0);
        assert_eq!(vis.leaf_at(Vec3::new(-50.0, 0.0, 0.0)), 1);
        assert_eq!(vis.cluster_at(Vec3::new(50.0, 0.0, 0.0)), 0);
        assert_eq!(vis.area_at(Vec3::new(-50.0, 0.0, 0.0)), 2);
        // Exactly on the plane is the front side, which is
        // `CM_PointLeafnum_r`'s `d >= 0` and not a tie to be broken.
        assert_eq!(vis.leaf_at(Vec3::ZERO), 0);
    }

    /// The whole point of a PVS: standing in the near room, the far room's
    /// face is not drawn even though it is in front of the camera and inside
    /// the frustum.
    #[test]
    fn the_near_room_does_not_see_the_far_one() {
        let vis = two_rooms();
        let eye = Vec3::new(50.0, 0.0, 0.0);
        let set = vis.mark(eye, view(eye, Vec3::new(-100.0, 0.0, 0.0)), false);

        assert!(set.face(0), "the room the eye is in");
        assert!(!set.face(1), "the far room, which cluster 0's row excludes");
        assert_eq!(set.stats.clusters, 1);
        assert_eq!(set.stats.leaves, 1);
        assert_eq!(set.stats.faces, 1);
    }

    /// And from the other side, where the row *does* include both, both draw —
    /// so the first test is measuring the PVS and not the frustum.
    #[test]
    fn the_far_room_sees_them_both() {
        let vis = two_rooms();
        let eye = Vec3::new(-50.0, 0.0, 0.0);
        let set = vis.mark(eye, view(eye, Vec3::new(100.0, 0.0, 0.0)), false);

        assert!(set.face(0) && set.face(1));
        assert_eq!(set.stats.clusters, 2);
    }

    /// `r_novis`: everything, without consulting anything.
    #[test]
    fn turning_the_pvs_off_draws_the_far_room_too() {
        let vis = two_rooms();
        let eye = Vec3::new(50.0, 0.0, 0.0);
        let set = vis.mark(eye, view(eye, Vec3::new(-100.0, 0.0, 0.0)), true);

        assert!(set.is_everything());
        assert!(set.face(0) && set.face(1));
        // Out of range of a set that has no bitset at all, and still visible.
        assert!(set.face(9_999));
    }

    /// Looking the other way culls the room behind the camera — the frustum,
    /// on its own, with the PVS saying yes to both.
    #[test]
    fn a_room_behind_the_camera_is_not_drawn() {
        let vis = two_rooms();
        let eye = Vec3::new(-50.0, 0.0, 0.0);
        let set = vis.mark(eye, view(eye, Vec3::new(-100.0, 0.0, 0.0)), false);

        assert_eq!(set.stats.clusters, 2, "the PVS offered both");
        assert!(set.face(1), "the room the eye is in");
        assert!(!set.face(0), "the one behind it");
    }

    /// Closing the window between the areas closes the far room off, even
    /// though the PVS still says it is visible — which is the whole reason
    /// areaportals exist.
    #[test]
    fn closing_an_areaportal_shuts_the_far_room_out() {
        let mut vis = two_rooms();
        let eye = Vec3::new(-50.0, 0.0, 0.0);
        let looking = view(eye, Vec3::new(100.0, 0.0, 0.0));

        assert!(vis.mark(eye, looking, false).face(0), "open to start with");
        assert!(vis.areas_connected(1, 2));

        vis.set_area_portal(1, false);
        let set = vis.mark(eye, looking, false);
        assert!(!set.face(0), "the area is unreachable now");
        assert!(set.face(1), "the room the eye is in is not");
        assert!(!vis.areas_connected(1, 2), "and the flood agrees");
        assert_eq!(set.stats.areas, 1);
    }

    /// `set_area_portals` is the same thing in bulk, and leaves keys it is not
    /// told about alone.
    #[test]
    fn setting_the_portals_in_bulk_leaves_unnamed_keys_open() {
        let mut vis = two_rooms();
        vis.set_area_portals(&[(1, false)]);
        assert!(!vis.area_portal_is_open(1));
        vis.set_area_portals(&[]);
        assert!(
            !vis.area_portal_is_open(1),
            "an empty report changes nothing"
        );
        vis.set_area_portals(&[(1, true)]);
        assert!(vis.area_portal_is_open(1));
    }

    /// A box is visible when some leaf it reaches is — the test a brush entity
    /// and an entity model are culled with.
    #[test]
    fn a_box_is_visible_when_a_leaf_it_reaches_is() {
        let vis = two_rooms();
        let eye = Vec3::new(50.0, 0.0, 0.0);
        let set = vis.mark(eye, view(eye, Vec3::new(-100.0, 0.0, 0.0)), false);

        let near = vis.box_visible(&set, Vec3::new(10.0, -8.0, -8.0), Vec3::new(26.0, 8.0, 8.0));
        let far = vis.box_visible(
            &set,
            Vec3::new(-26.0, -8.0, -8.0),
            Vec3::new(-10.0, 8.0, 8.0),
        );
        assert!(near, "in the visible room");
        assert!(!far, "in the one the PVS excluded");

        // A box straddling the plane reaches both, and one is enough.
        assert!(vis.box_visible(&set, Vec3::new(-8.0, -8.0, -8.0), Vec3::new(8.0, 8.0, 8.0)));
    }

    /// `Map_AreAnyLeavesVisible`, including the empty-list case a prop placed
    /// outside the tree hits.
    #[test]
    fn any_leaf_answers_for_a_props_leaf_list() {
        let vis = two_rooms();
        let eye = Vec3::new(50.0, 0.0, 0.0);
        let set = vis.mark(eye, view(eye, Vec3::new(-100.0, 0.0, 0.0)), false);

        assert!(set.any_leaf(&[0]));
        assert!(!set.any_leaf(&[1]));
        assert!(set.any_leaf(&[1, 0]), "one is enough");
        assert!(set.any_leaf(&[]), "a prop with no leaves is not hidden");
    }

    /// The frustum comes out of the matrix, so it has to agree with what the
    /// matrix draws: in front is in, behind and off to the side are out.
    #[test]
    fn the_frustum_keeps_what_the_matrix_would_draw() {
        let eye = Vec3::ZERO;
        let frustum = Frustum::new(view(eye, Vec3::X * 100.0));
        let unit = |center: Vec3| (center - Vec3::splat(8.0), center + Vec3::splat(8.0));

        let (mins, maxs) = unit(Vec3::new(100.0, 0.0, 0.0));
        assert!(frustum.intersects(mins, maxs), "straight ahead");
        let (mins, maxs) = unit(Vec3::new(-100.0, 0.0, 0.0));
        assert!(!frustum.intersects(mins, maxs), "straight behind");
        // 90 degrees horizontal FOV at aspect 1, so a point 100 out and 300
        // to the side is well outside.
        let (mins, maxs) = unit(Vec3::new(100.0, 300.0, 0.0));
        assert!(!frustum.intersects(mins, maxs), "off to the side");
        // The near plane: this port's depth range is 0..w, so row 2 alone is
        // the near plane. Getting that wrong keeps a box behind the eye.
        let (mins, maxs) = unit(Vec3::new(-2000.0, 0.0, 0.0));
        assert!(!frustum.intersects(mins, maxs), "far behind");
    }

    /// Narrowing to a rectangle is the same planes the matrix already has,
    /// scaled: the right half of the screen rejects what is on the left.
    #[test]
    fn narrowing_to_half_the_screen_rejects_the_other_half() {
        let eye = Vec3::ZERO;
        let view_proj = view(eye, Vec3::X * 100.0);
        let frustum = Frustum::new(view_proj);
        let right_half = frustum.narrowed(
            view_proj,
            Rect {
                left: 0.0,
                right: 1.0,
                bottom: -1.0,
                top: 1.0,
            },
        );

        // The view looks down +X with +Z up, so screen-right is -Y.
        let unit = |center: Vec3| (center - Vec3::splat(4.0), center + Vec3::splat(4.0));
        let (mins, maxs) = unit(Vec3::new(100.0, -50.0, 0.0));
        assert!(right_half.intersects(mins, maxs), "on the kept half");
        let (mins, maxs) = unit(Vec3::new(100.0, 50.0, 0.0));
        assert!(!right_half.intersects(mins, maxs), "on the discarded half");
        assert!(
            frustum.intersects(mins, maxs),
            "and the unnarrowed frustum still keeps it"
        );
    }

    #[test]
    fn rectangles_intersect_or_do_not() {
        let a = Rect {
            left: -1.0,
            right: 0.0,
            bottom: -1.0,
            top: 1.0,
        };
        let b = Rect {
            left: -0.5,
            right: 1.0,
            bottom: -1.0,
            top: 1.0,
        };
        let hit = a.intersection(b).expect("they overlap");
        assert_eq!((hit.left, hit.right), (-0.5, 0.0));
        assert!(a
            .intersection(Rect {
                left: 0.5,
                right: 1.0,
                bottom: -1.0,
                top: 1.0,
            })
            .is_none());
        // Touching is not overlapping — `GetRectIntersection`'s `>=`.
        assert!(a
            .intersection(Rect {
                left: 0.0,
                right: 1.0,
                bottom: -1.0,
                top: 1.0,
            })
            .is_none());
    }

    /// The flood is what `CM_AreasConnected` answers, and area 0 is not in it.
    #[test]
    fn the_flood_joins_areas_through_open_windows_only() {
        let mut vis = two_rooms();
        assert!(vis.areas_connected(1, 2));
        vis.set_area_portal(1, false);
        assert!(!vis.areas_connected(1, 2));
        assert!(vis.areas_connected(1, 1), "an area is connected to itself");
        // An area nobody named: out of range, and the answer is the permissive
        // one rather than a panic.
        assert!(vis.areas_connected(1, 99));
    }

    #[test]
    fn a_map_with_no_tree_draws_everything() {
        let vis = Visibility::default();
        assert!(vis.is_empty());
        let set = vis.mark(Vec3::ZERO, Mat4::IDENTITY, false);
        assert!(set.is_everything());
        assert!(vis.box_visible(&set, Vec3::splat(-1.0), Vec3::splat(1.0)));
        assert_eq!(vis.summary(), "no visibility");
    }

    /// Every shipped map's visibility data, built and asked a real question.
    ///
    /// The acceptance test for the module: a `.bsp` off the disk, a tree built
    /// from it, and a view from somewhere a player can actually stand. What it
    /// asserts is that the PVS *does something* — if it culled nothing this
    /// would pass silently — and what it prints is how much.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release every_shipped_map -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_map_culls_most_of_itself() {
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

        let (mut maps, mut clusters, mut areas, mut portals) = (0, 0usize, 0usize, 0usize);
        let (mut leaves, mut disp_faces, mut no_vis) = (0usize, 0usize, 0usize);
        let (mut seen, mut drawn, mut total) = (0usize, 0usize, 0usize);
        // How often the flow gets out of the area the eye is in. A flow that
        // never stepped through a window would look exactly like a working one
        // from most viewpoints, because most viewpoints are in a sealed room.
        let (mut multi_area, mut best_areas) = (0usize, 0usize);
        let mut worst: (f32, String) = (0.0, String::new());
        let mut best: (f32, String) = (1.0, String::new());

        for name in &names {
            let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
            let vis = Visibility::build(&bsp);
            assert!(!vis.is_empty(), "{name}: no tree");
            maps += 1;
            clusters += vis.clusters;
            leaves += vis.leaves.len();
            areas += vis.areas.len().saturating_sub(1);
            portals += vis.portals.len();
            if vis.clusters == 0 {
                no_vis += 1;
            }

            // Every displacement face reached at least one leaf, which is the
            // half of the leaf-face list that no lump holds.
            let disp: Vec<usize> = (0..bsp.faces.len())
                .filter(|&i| bsp.faces[i].disp_info >= 0)
                .collect();
            disp_faces += disp.len();
            for face in disp {
                assert!(
                    vis.leaf_faces.contains(&(face as u32)),
                    "{name}: displacement face {face} is in no leaf"
                );
            }

            // A view from the map's own spawn, which is somewhere a player
            // stands rather than a corner of the bounding box.
            let spawn = bsp
                .entities()
                .iter()
                .find(|e| e.classname() == Some("info_player_start"))
                .and_then(|e| e.vector("origin"))
                .map(Vec3::from);
            let Some(eye) = spawn else { continue };
            let eye = eye + Vec3::Z * 64.0;
            if vis.cluster_at(eye) < 0 {
                // A spawn inside solid, which a few maps have because the
                // entity is a marker for a scripted intro rather than a
                // standing position.
                continue;
            }

            let projection = glam::camera::rh::proj::directx::perspective(
                90f32.to_radians(),
                16.0 / 9.0,
                7.0,
                28_000.0,
            );
            let view = glam::camera::rh::view::look_at_mat4(eye, eye + Vec3::X, Vec3::Z);
            let set = vis.mark(eye, projection * view, false);

            let faces = bsp.model_faces(bsp.world_model()).len();
            seen += set.stats.faces;
            total += faces;
            drawn += 1;
            if set.stats.areas > 1 {
                multi_area += 1;
            }
            best_areas = best_areas.max(set.stats.areas);
            let fraction = set.stats.faces as f32 / faces.max(1) as f32;
            if fraction > worst.0 {
                worst = (fraction, name.clone());
            }
            if fraction < best.0 {
                best = (fraction, name.clone());
            }
            assert!(
                set.stats.faces > 0,
                "{name}: the view from its own spawn draws nothing"
            );
        }

        // **Standing in a leaf, that leaf draws.** The invariant that catches
        // an over-aggressive cull, which is the failure mode with no symptom
        // other than a hole in the world: every filter here can only ever
        // remove, so if any one of them removed the leaf the camera is
        // *inside*, nothing downstream would notice.
        let mut sampled = 0;
        for name in names.iter().take(12) {
            let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
            let vis = Visibility::build(&bsp);
            for (index, leaf) in vis.leaves.iter().enumerate() {
                if leaf.cluster < 0 || leaf.solid {
                    continue;
                }
                // Every seventh, which is a few hundred a map rather than
                // thousands, and not a round number so that it does not land
                // on a pattern in the leaf order.
                if index % 7 != 0 {
                    continue;
                }
                let eye = (leaf.mins + leaf.maxs) * 0.5;
                if vis.leaf_at(eye) != index {
                    // A leaf whose box centre is in a neighbour, which a
                    // non-convex-looking leaf box allows. Nothing to assert.
                    continue;
                }
                // **A near plane of 0.1, not the game's 7.** What this is
                // asserting is that the PVS and the areas keep the leaf, and
                // a leaf can be thinner than the near plane: `sp` and `mp`
                // maps are full of two-unit sky slivers, and
                // `mp_coop_catapult_1`'s leaf 504 is 2 x 128 x 64. Standing in
                // the middle of one, its far face is a unit ahead and the near
                // plane is seven, so the frustum culls it — correctly, and the
                // shipped `CullNodeSIMD` would too.
                let projection = glam::camera::rh::proj::directx::perspective(
                    90f32.to_radians(),
                    16.0 / 9.0,
                    0.1,
                    28_000.0,
                );
                let view = glam::camera::rh::view::look_at_mat4(eye, eye + Vec3::X, Vec3::Z);
                let set = vis.mark(eye, projection * view, false);
                assert!(
                    set.leaf(index),
                    "{name}: standing in leaf {index} (cluster {}, area {}), \
                     it culled itself",
                    leaf.cluster,
                    leaf.area,
                );
                sampled += 1;
            }
        }
        // Counted across the twelve rather than per map, because
        // `mp_coop_credits` is a single room and has one leaf to sample.
        // Measured at 379; the bound is a guard against the sampling silently
        // stopping, not the number itself.
        assert!(sampled > 300, "only {sampled} leaves sampled");

        println!(
            "{maps} maps: {clusters} clusters over {leaves} leaves, {areas} areas, \
             {portals} areaportals, {disp_faces} displacement faces placed, {no_vis} without vis"
        );
        println!("{sampled} leaves each drew themselves from inside");
        println!(
            "from {drawn} spawns: {seen} of {total} world faces ({:.1}%); \
             worst {} at {:.1}%, best {} at {:.1}%",
            100.0 * seen as f32 / total.max(1) as f32,
            worst.1,
            100.0 * worst.0,
            best.1,
            100.0 * best.0,
        );

        println!(
            "the areaportal flow left the eye's own area on {multi_area} of {drawn} spawns, \
             reaching {best_areas} areas at most"
        );

        // The number that says this is worth having. Measured at 27% over the
        // shipped maps from their own spawns; the bound is loose because it is
        // a regression guard, not the measurement.
        assert!(
            (seen as f32) < 0.6 * total as f32,
            "the PVS left {seen} of {total} faces standing, which is not culling"
        );
        assert_eq!(no_vis, 0, "every shipped map is compiled with vvis");
        // **The flow gets out of the room.** Measured at 13 of 103 spawns,
        // reaching five areas at most — low because a Portal 2 spawn is
        // usually a sealed chamber, and the reason this is asserted at all:
        // a flow that never stepped through a window would be invisible from
        // every other viewpoint in this test.
        assert!(
            multi_area > 5,
            "the areaportal flow never left the eye's area on any of {drawn} spawns"
        );
    }

    /// A bitset is a bitset.
    #[test]
    fn the_bitset_ignores_what_is_past_its_end() {
        let mut bits = Bits::new(70);
        bits.set(0);
        bits.set(69);
        bits.set(70);
        assert!(bits.get(0) && bits.get(69));
        assert!(!bits.get(70), "out of range reads false rather than panics");
        assert_eq!(bits.count(), 2);
        bits.clear(69);
        assert_eq!(bits.count(), 1);
    }
}
