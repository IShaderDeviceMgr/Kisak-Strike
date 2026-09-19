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
    /// `DISPSURF_FLAG_*` for a displacement hit, and **0 for everything
    /// else** — see [`disp_surf`](super::disp_surf).
    ///
    /// The engine ORs `DISPSURF_FLAG_SURFACE` into every displacement
    /// triangle, so a non-zero value here is exactly `CGameTrace::IsDispSurface`
    /// (`public/trace.h:43`): *this was terrain*. `disp_surf::WALKABLE` is the
    /// bit gameplay reads most — it is VBSP's judgement, made at compile time
    /// against the triangle's slope, and is not the same question as
    /// `normal.z > 0.7`.
    pub disp_flags: u16,
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
    /// The box touched the wall immediately below a portal's opening, as the
    /// **exit** portal sees it — `m_bContactedPortalTransitionRamp`
    /// (`portal_gamemovement.h:167`).
    ///
    /// # This is the one field here that is not about the surface hit
    ///
    /// Valve puts it on a subclass, `CTrace_PlayerAABB_vs_Portals`, which only
    /// the player's movement ever declares. It is here instead of on a second
    /// type because it has to survive every place a `Trace` is copied around —
    /// the four-quadrant ground retry, the step-up/step-down comparison — and
    /// a parallel `bool` threaded beside those is a field that can be dropped
    /// silently.
    ///
    /// **Only [`Tracer::with_hole`](super::Tracer::with_hole) ever sets it**,
    /// and only on an answer the portal's carved geometry won. Every other
    /// trace in the port leaves it `false`, which is what
    /// [`hit_portal_ramp`](Trace::hit_portal_ramp) then answers.
    pub portal_ramp: bool,
    /// What stopped the sweep was a **physics prop** rather than the world or
    /// a brush entity — `CBasePlayer::TouchedPhysics()`.
    ///
    /// # The second field here that is not about the surface hit
    ///
    /// Valve does not get this from the trace at all: `CBasePlayer::Touch`
    /// (`player.cpp:4920`) sets `m_bTouchedPhysObject` when the *entity* touch
    /// pass reports a `MOVETYPE_VPHYSICS`, `SOLID_VPHYSICS`, non-trigger,
    /// moveable other. That is a different mechanism for the same fact, and it
    /// is a different mechanism because Valve's trace cannot answer — the
    /// engine's `trace_t` names the entity, but `CGameMovement` throws it away
    /// before `PostThinkVPhysics` runs.
    ///
    /// It is here because the answer decides how hard the player's physics
    /// shadow may push (`PostThinkVPhysics`'s `m_outWishVel.Init( maxSpeed,
    /// maxSpeed, maxSpeed )` substitution — `baseplayer_shared.cpp:3316`), and
    /// because the prop sweep that stage 2 of `portdocs/VPHYSICS_SHADOW.md`
    /// adds already knows it for free.
    ///
    /// **Only [`Tracer::with_props`](super::Tracer::with_props) ever sets it**,
    /// and only on an answer the prop sweep won.
    pub hit_prop: bool,
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
            disp_flags: 0,
            surface_flags: 0,
            all_solid: false,
            start_solid: false,
            portal_ramp: false,
            hit_prop: false,
        }
    }

    /// Whether anything at all was hit — `CGameTrace::DidHit`
    /// (`public/gametrace.h:87`). Note that starting inside a solid counts,
    /// even though the fraction is 1.
    pub fn did_hit(&self) -> bool {
        self.fraction < 1.0 || self.all_solid || self.start_solid
    }

    /// `CTrace_PlayerAABB_vs_Portals::HitPortalRamp`
    /// (`portal_gamemovement.cpp:214`) — should whatever this hit be treated
    /// as standable however steep it is?
    ///
    /// Three conditions, and Valve's fourth — `sv_portal_new_player_trace`,
    /// which is `1` — is the switch this whole path lives under and is not a
    /// `ConVar` here:
    ///
    /// - the sweep touched the ramp ([`portal_ramp`](Trace::portal_ramp));
    /// - it hit *something* — the ramp is a label on another surface, not a
    ///   surface;
    /// - what it hit faces **up at all**. Not `>= CRITICAL_SLOPE`, which is
    ///   the test this exists to bypass: `> 0.0`, so a wall is still a wall
    ///   and anything that leans even slightly back is ground.
    ///
    /// `up` is the movement's stick normal, which without paint is world up
    /// at every one of this port's four call sites.
    pub fn hit_portal_ramp(&self, up: Vec3) -> bool {
        self.portal_ramp && self.did_hit() && self.normal.dot(up) > 0.0
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
