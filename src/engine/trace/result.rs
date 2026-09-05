//! What a trace answers with.

use glam::Vec3;

use super::Contents;

/// `SURFACE_INDEX_INVALID` (`engine/cmodel_private.h`) — a brush side with no
/// texinfo. VBSP writes -1 for these and Valve's own comment calls it a
/// BUGBUG.
pub(super) const SURFACE_INDEX_INVALID: u16 = 0xFFFF;

/// The result of a trace — `CBaseTrace` (`public/trace.h`) and the parts of
/// `CGameTrace` (`public/gametrace.h`) that mean anything without entities.
///
/// Deliberately absent: `m_pEnt`, `hitbox`, `hitgroup`, `physicsbone` and
/// `worldSurfaceIndex`. The first four are entity and studio-model state that
/// arrives with stages 4 and 5; the last is for decals and paint, which are
/// `render/`'s. See `portdocs/ENGINE_TRACE.md` §4.2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trace {
    /// Where the sweep began, **in the caller's frame** — the centring offset
    /// has been added back. When the trace began inside a solid this is not
    /// the point passed in: it is where the sweep *left* solid, which is what
    /// [`fraction_left_solid`](Trace::fraction_left_solid) measures.
    pub start: Vec3,
    /// Where it ended, in the caller's frame.
    pub end: Vec3,
    /// The normal of the surface hit, pointing out of it. Meaningless when
    /// nothing was hit or when [`all_solid`](Trace::all_solid).
    pub normal: Vec3,
    /// The plane's distance along its own normal.
    pub plane_dist: f32,
    /// How far along the sweep the impact was, 0..=1. 1 means nothing was hit.
    ///
    /// Pulled back by `DIST_EPSILON` (1/32 unit), so a trace **stops just
    /// short of the surface**. Every consumer depends on that gap — it is why
    /// a player does not fuse to a wall and why stair stepping terminates.
    pub fraction: f32,
    /// When the sweep began inside a solid, how far along it stopped being
    /// inside.
    ///
    /// **Rays only.** Computing it for a box sweep needs, in Valve's words,
    /// "*a lot* more computation", so [`trace`](super::Tracer::trace) forces it
    /// to zero for a hull — matching `CEngineTrace::TraceRay`
    /// (`engine/enginetrace.cpp:2958`).
    pub fraction_left_solid: f32,
    /// The contents of the brush hit — "contents on the other side of the
    /// surface hit". Water, ladders, player clips and paint are all read from
    /// here.
    pub contents: Contents,
    /// Index into the collision model's surface table, or `None` for the null
    /// surface. Resolve with
    /// [`CollisionBsp::surface_name`](super::CollisionBsp::surface_name).
    pub surface: Option<u16>,
    /// `SURF_*` for the surface hit.
    ///
    /// **Per *material*, not per side.** Valve ORs every texinfo's flags into
    /// the one surface entry its texdata shares, under a comment reading
    /// "HACKHACK: Copy this over for the whole material!!!"
    /// (`engine/cmodel_bsp.cpp:381`). Ported as written, because a divergence
    /// here would show up as a surface behaving like an unrelated one.
    pub surface_flags: i32,
    /// The sweep began inside a solid and never left it. `normal` is not valid.
    pub all_solid: bool,
    /// The sweep began inside a solid.
    pub start_solid: bool,
}

impl Trace {
    /// A trace that hit nothing, starting and ending where the ray does.
    ///
    /// `CM_ClearTrace` (`engine/cmodel.cpp:2676`).
    pub(super) fn miss(start: Vec3, end: Vec3) -> Trace {
        Trace {
            start,
            end,
            normal: Vec3::ZERO,
            plane_dist: 0.0,
            fraction: 1.0,
            fraction_left_solid: 0.0,
            contents: Contents::EMPTY,
            surface: None,
            surface_flags: 0,
            all_solid: false,
            start_solid: false,
        }
    }

    /// Whether anything at all was hit — `CGameTrace::DidHit`
    /// (`public/gametrace.h:87`). Note that starting inside a solid counts,
    /// even though the fraction is 1.
    pub fn did_hit(&self) -> bool {
        self.fraction < 1.0 || self.all_solid || self.start_solid
    }
}

/// One entry of the collision model's surface table — `csurface_t`
/// (`public/cmodel.h:47`), one per texdata rather than one per brush side.
///
/// `surfaceProps` is deliberately absent: it is an index into the physics
/// surface-property database, which is filled in from the material's
/// `$surfaceprop` at load (`engine/cmodel_bsp.cpp:355`) and there is no
/// material system call and no `physprops` here yet. It arrives with
/// `vphysics/`; carrying a field that is always zero would read as "this
/// surface has the default properties", which is a different claim.
#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    /// The material name, without `materials/` or `.vmt` — the same string
    /// [`Bsp::face_material`](crate::engine::world::bsp::Bsp::face_material)
    /// hands out.
    pub name: String,
    /// The OR of `SURF_*` over every texinfo naming this texdata. See
    /// [`Trace::surface_flags`].
    pub flags: i32,
}
