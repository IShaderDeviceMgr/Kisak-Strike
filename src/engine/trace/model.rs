//! The collision model: the `.bsp`'s brushes, arranged for tracing.
//!
//! Replaces `CCollisionBSPData` and `engine/cmodel_bsp.cpp`'s half of the map
//! load. The *file* is read by [`world::bsp`](crate::engine::world::bsp) —
//! Valve's second `.bsp` reader existed because collision lived in code that
//! could not see `modelloader.cpp`'s allocations, and one crate has no such
//! excuse (`portdocs/ENGINE_TRACE.md` §7.4). What is built here is the part
//! that is *derived* rather than read: the surface table, the box brushes, and
//! the contents summary.

use glam::Vec3;

use super::result::SURFACE_INDEX_INVALID;
use super::{Contents, Surface, Tracer};
use crate::engine::world::bsp::Bsp;

/// The name `csurface_t::nullsurface` carries (`engine/cmodel.cpp:53`).
const NULL_SURFACE_NAME: &str = "**empty**";

/// A plane, ready to clip against.
///
/// `dist` is measured along `normal`, so a point is outside the half-space
/// when `normal.dot(p) - dist > 0`.
#[derive(Debug, Clone, Copy)]
pub(super) struct CPlane {
    pub normal: Vec3,
    pub dist: f32,
    /// The axis this plane is perpendicular to, when it is axial.
    ///
    /// `plane->type < 3` in the C++, which the trace tests on every node it
    /// descends: an axial plane needs one subtraction where a general one
    /// needs a dot product (`engine/cmodel.cpp:2578`). An `Option<usize>`
    /// rather than the raw 0-5 enum because those are the only two cases
    /// anything here distinguishes.
    pub axis: Option<usize>,
}

/// A BSP node. `children` is front, then back; negative is `-1 - leaf`.
#[derive(Debug, Clone, Copy)]
pub(super) struct CNode {
    pub plane: u32,
    pub children: [i32; 2],
}

/// A leaf, reduced to what the trace reads.
#[derive(Debug, Clone, Copy)]
pub(super) struct CLeaf {
    /// What the leaf's *volume* is made of.
    ///
    /// **Not the same as the OR of its brush list.** A leaf's brush list holds
    /// everything touching it, so an empty leaf beside a wall references that
    /// wall while its own contents stay 0. Conflating the two makes every
    /// position test in open air report `all_solid`, because
    /// [`unswept_box_trace`](super::hull::unswept_box_trace) reads this to
    /// decide whether the box is in the void outside the map.
    pub contents: Contents,
    /// The PVS cluster, negative for a solid leaf. Read by
    /// [`CollisionBsp::point_contents`], which returns the leaf's own contents
    /// for one rather than testing brushes.
    pub cluster: i16,
    pub first_leaf_brush: u32,
    pub num_leaf_brushes: u32,
}

/// Where a brush keeps its sides.
///
/// Valve packs this into `cbrush_t` as `numsides == NUMSIDES_BOXBRUSH`
/// (0xFFFF) with `firstbrushside` reinterpreted as a box index
/// (`engine/cmodel_private.h:181`). The sentinel is a C-ism; the enum says the
/// same thing and cannot be read the wrong way.
#[derive(Debug, Clone, Copy)]
pub(super) enum BrushSides {
    Planes { first: u32, count: u32 },
    /// An index into [`CollisionBsp::box_brushes`].
    Box(u32),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CBrush {
    pub contents: Contents,
    pub sides: BrushSides,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CBrushSide {
    pub plane: u32,
    /// Index into [`CollisionBsp::surfaces`], or
    /// [`SURFACE_INDEX_INVALID`](super::result::SURFACE_INDEX_INVALID).
    pub surface: u16,
    /// A plane VBSP added so a swept *box* clips exactly. Point traces skip
    /// these; box traces must not (`portdocs/ENGINE_TRACE.md` §4.4).
    pub bevel: bool,
}

/// An axis-aligned brush, stored as its bounds rather than six planes.
///
/// `cboxbrush_t` (`engine/cmodel_private.h:191`). Most brushes in a Source map
/// are boxes, so this is the path most traces take.
#[derive(Debug, Clone, Copy)]
pub(super) struct BoxBrush {
    pub mins: Vec3,
    pub maxs: Vec3,
    /// The surface of each face: `[-x, -y, -z, +x, +y, +z]`, which is the
    /// order `IntersectRayWithBoxBrush`'s face index is in
    /// (`g_CubeFaceIndex0`/`1`, `engine/cmodel.cpp:933`).
    pub surfaces: [u16; 6],
}

/// A map's collision geometry.
///
/// Immutable once built. Traces borrow it through a [`Tracer`], which is where
/// the per-trace scratch lives.
#[derive(Debug)]
pub struct CollisionBsp {
    pub(super) planes: Vec<CPlane>,
    pub(super) nodes: Vec<CNode>,
    pub(super) leaves: Vec<CLeaf>,
    pub(super) leaf_brushes: Vec<u16>,
    pub(super) brushes: Vec<CBrush>,
    pub(super) brush_sides: Vec<CBrushSide>,
    pub(super) box_brushes: Vec<BoxBrush>,
    pub(super) surfaces: Vec<Surface>,
    /// The head node of each model — model 0 is the world, 1.. are the brush
    /// entities.
    ///
    /// Stage 2's input. [`Tracer::trace`] does not read it: it traces head
    /// node 0, which is what `CEngineTrace::TraceRay` passes for the world
    /// (`engine/enginetrace.cpp:2838`), and models 1.. need the transform that
    /// stage 2 brings.
    #[allow(dead_code)]
    pub(super) head_nodes: Vec<i32>,
    /// The OR of every leaf's contents.
    ///
    /// `CCollisionBSPData::allcontents` (`engine/cmodel_bsp.cpp:442`): a trace
    /// whose mask shares no bit with this can be answered without descending
    /// at all.
    pub(super) all_contents: Contents,
}

impl CollisionBsp {
    /// Builds the collision model from an already-parsed `.bsp`.
    ///
    /// Infallible: [`Bsp::parse`] has already checked every cross-lump
    /// reference this walks (`world/bsp.rs`'s `validate`), which is what buys
    /// the right to index without bounds checks in the trace's inner loop. A
    /// map with no brushes builds an empty model that traces as a clean miss —
    /// which is not an error, it is a map you cannot collide with, and
    /// `CM_BoxTrace`'s own first act is the same early-out
    /// (`engine/cmodel.cpp:3208`).
    pub fn build(bsp: &Bsp) -> CollisionBsp {
        let surfaces = surface_table(bsp);

        let planes = bsp
            .planes
            .iter()
            .map(|p| CPlane {
                normal: Vec3::from_array(p.normal),
                dist: p.dist,
                axis: p.is_axial().then_some(p.plane_type as usize),
            })
            .collect::<Vec<_>>();

        let nodes = bsp
            .nodes
            .iter()
            .map(|n| CNode {
                plane: n.plane_num as u32,
                children: n.children,
            })
            .collect();

        let mut all_contents = Contents::EMPTY;
        let leaves = bsp
            .leaves
            .iter()
            .map(|l| {
                let contents = Contents(l.contents as u32);
                all_contents = all_contents.or(contents);
                CLeaf {
                    contents,
                    cluster: l.cluster,
                    first_leaf_brush: l.first_leaf_brush as u32,
                    num_leaf_brushes: l.num_leaf_brushes as u32,
                }
            })
            .collect();

        // The side that a brush's texinfo names, as a surface-table index.
        let side_surface = |index: usize| -> u16 {
            let side = &bsp.brush_sides[index];
            // VBSP writes -1 here for some sides; Valve's own comment calls it
            // a BUGBUG (`engine/cmodel_bsp.cpp:806`).
            let Ok(texinfo) = usize::try_from(side.tex_info) else {
                return SURFACE_INDEX_INVALID;
            };
            match bsp.texinfo.get(texinfo) {
                Some(info) => u16::try_from(info.tex_data).unwrap_or(SURFACE_INDEX_INVALID),
                None => SURFACE_INDEX_INVALID,
            }
        };

        let mut brushes = Vec::with_capacity(bsp.brushes.len());
        let mut brush_sides = Vec::new();
        let mut box_brushes = Vec::new();
        for brush in &bsp.brushes {
            let first = brush.first_side as usize;
            let count = brush.num_sides as usize;
            let contents = Contents(brush.contents as u32);

            let sides = match extract_box(bsp, first, count, &side_surface) {
                Some(box_brush) => {
                    box_brushes.push(box_brush);
                    BrushSides::Box(box_brushes.len() as u32 - 1)
                }
                None => {
                    let out_first = brush_sides.len() as u32;
                    for i in first..first + count {
                        let side = &bsp.brush_sides[i];
                        brush_sides.push(CBrushSide {
                            plane: side.plane_num as u32,
                            surface: side_surface(i),
                            bevel: side.bevel != 0,
                        });
                    }
                    BrushSides::Planes {
                        first: out_first,
                        count: count as u32,
                    }
                }
            };
            brushes.push(CBrush { contents, sides });
        }

        CollisionBsp {
            planes,
            nodes,
            leaves,
            leaf_brushes: bsp.leaf_brushes.clone(),
            brushes,
            brush_sides,
            box_brushes,
            surfaces,
            head_nodes: bsp.models.iter().map(|m| m.head_node).collect(),
            all_contents,
        }
    }

    /// A tracer over this model. Reuse one across traces: it owns the
    /// visited-brush stamps, which is the only per-trace allocation.
    pub fn tracer(&self) -> Tracer<'_> {
        Tracer::new(self)
    }

    /// The material name behind a [`Trace::surface`](super::Trace::surface).
    ///
    /// `None` is the null surface, which Valve names `**empty**` — returned
    /// verbatim so a trace that hit nothing prints the same thing the C++
    /// would.
    pub fn surface_name(&self, surface: Option<u16>) -> &str {
        surface
            .and_then(|i| self.surfaces.get(i as usize))
            .map(|s| s.name.as_str())
            .unwrap_or(NULL_SURFACE_NAME)
    }

    /// A surface-table index and its flags, resolving Valve's
    /// `SURFACE_INDEX_INVALID` to the null surface
    /// (`CCollisionBSPData::GetSurfaceAtIndex`, `engine/cmodel.cpp:55`).
    pub(super) fn surface_at(&self, index: u16) -> (Option<u16>, i32) {
        if index == SURFACE_INDEX_INVALID {
            return (None, 0);
        }
        match self.surfaces.get(index as usize) {
            Some(surface) => (Some(index), surface.flags),
            None => (None, 0),
        }
    }

    /// Whether there is anything to trace against at all.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Counts, for `status`-style reporting.
    pub fn summary(&self) -> String {
        format!(
            "{} brushes ({} box), {} sides, {} nodes, {} leaves, {} planes",
            self.brushes.len(),
            self.box_brushes.len(),
            self.brush_sides.len(),
            self.nodes.len(),
            self.leaves.len(),
            self.planes.len(),
        )
    }
}

/// One `csurface_t` per texdata, with the flags of every texinfo that names it
/// ORed together.
///
/// `CollisionBSPData_LoadTextures` + `_LoadTexinfo` (`engine/cmodel_bsp.cpp:304`,
/// `:374`). The OR is Valve's, under a comment reading "HACKHACK: Copy this
/// over for the whole material!!!" — it is why
/// [`Trace::surface_flags`](super::Trace::surface_flags) is per material and
/// not per side.
fn surface_table(bsp: &Bsp) -> Vec<Surface> {
    let mut surfaces: Vec<Surface> = bsp
        .texdata
        .iter()
        .map(|data| Surface {
            name: usize::try_from(data.name_string_table_id)
                .ok()
                .and_then(|i| bsp.texdata_string_table.get(i))
                .cloned()
                .unwrap_or_default(),
            flags: 0,
        })
        .collect();

    for info in &bsp.texinfo {
        // `if (out >= pBSPData->numtextures) out = 0;` — a texinfo naming a
        // texdata that does not exist folds into the first one rather than
        // being dropped.
        let index = match usize::try_from(info.tex_data) {
            Ok(i) if i < surfaces.len() => i,
            _ => 0,
        };
        if let Some(surface) = surfaces.get_mut(index) {
            surface.flags |= info.flags;
        }
    }
    surfaces
}

/// `IsBoxBrush` + `ExtractBoxBrush` (`engine/cmodel_bsp.cpp:667`, `:683`) in
/// one pass: six sides, every plane axial, and every face accounted for.
///
/// **Stricter than Valve on one point.** `IsBoxBrush` tests only that each
/// plane's *type* is axial and then asserts, in `ExtractBoxBrush`, that the
/// normal component is exactly ±1 — an assert that is compiled out of a
/// release build, leaving a box brush with uninitialised bounds. Here a brush
/// that fails any of those checks stays a plane brush, which is slower and
/// correct rather than faster and undefined. Valid maps take the same path
/// either way.
fn extract_box(
    bsp: &Bsp,
    first: usize,
    count: usize,
    side_surface: &impl Fn(usize) -> u16,
) -> Option<BoxBrush> {
    if count != 6 {
        return None;
    }

    let mut mins = [0.0f32; 3];
    let mut maxs = [0.0f32; 3];
    let mut surfaces = [SURFACE_INDEX_INVALID; 6];
    // Which of the six faces has been filled in. Two sides naming the same
    // face is not a box, however axial its planes are.
    let mut seen = [false; 6];

    for i in first..first + count {
        let side = &bsp.brush_sides[i];
        let plane = &bsp.planes[side.plane_num as usize];
        let axis = plane.is_axial().then_some(plane.plane_type as usize)?;
        let surface = side_surface(i);

        // `pBox->maxs[axis] = plane->dist` for the +1 normal, and
        // `pBox->mins[axis] = -plane->dist` for -1: a brush side's plane faces
        // *out* of the brush, so the -x side's normal is (-1,0,0) and its
        // distance is the negated minimum.
        let face = if plane.normal[axis] == 1.0 {
            maxs[axis] = plane.dist;
            axis + 3
        } else if plane.normal[axis] == -1.0 {
            mins[axis] = -plane.dist;
            axis
        } else {
            return None;
        };
        if std::mem::replace(&mut seen[face], true) {
            return None;
        }
        surfaces[face] = surface;
    }

    if !seen.iter().all(|&s| s) {
        return None;
    }
    Some(BoxBrush {
        mins: Vec3::from_array(mins),
        maxs: Vec3::from_array(maxs),
        surfaces,
    })
}
