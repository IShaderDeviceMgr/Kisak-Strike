//! The coloured oval a portal wears — the fourth kind of geometry in a level.
//!
//! World faces are `.bsp` geometry drawn with the identity, brush models are
//! `.bsp` geometry with a matrix, static props are `.mdl` geometry the
//! *compiler* placed and entity models are `.mdl` geometry the *game* places.
//! This is the first thing in the port that is neither: **four vertices built
//! from four numbers, every frame**, wearing a material with no geometry of its
//! own anywhere in the game's files.
//!
//! `CPortalRenderable_FlatBasic::DrawSimplePortalMesh`
//! (`game/client/portal/portalrenderable_flatbasic.cpp:1212`) with
//! `models/portals/portalstaticoverlay_1.vmt` bound — which is one of the three
//! draws `C_Prop_Portal::DrawPortal` makes, and the only one
//! `portdocs/PORTAL.md` takes. Of the other two, the **stencil punch** is here
//! as well ([`Portals::draw_hole`]), driven by
//! [`portalview`](super::portalview); the see-through warp — `$Stage 0` — is
//! not, and `portdocs/PORTAL_RENDER.md` §7 says why.
//!
//! # What you see, and what you do not
//!
//! **A coloured oval, and the room behind the other portal inside it.** The
//! oval is this module; the picture in the opening is
//! [`World::draw_portal_views`](super::World::draw_portal_views), which reaches
//! back here for [`draw_hole`](Portals::draw_hole),
//! [`draw_hole_cap`](Portals::draw_hole_cap) and
//! [`clear_depth`](Portals::clear_depth).
//!
//! **Both animate open** — the ring from [`Portal::open_for`] and the hole from
//! the same number, which is why [`bind_overlay`](Portals::bind_overlay) is
//! shared: a ring and a hole computed from two blocks would stop being
//! concentric. What is still missing is the *warp*, `PortalRefract`'s
//! `$Stage 0`: a portal's surface does not refract what is behind it.
//! `portdocs/PORTAL_RENDER.md` §7 says why that one is deferred for a reason
//! rather than for scope.
//!
//! # Three things here that produce a plausible wrong picture rather than an
//! error
//!
//! **The portal model draws nothing and must not be drawn.**
//! `models/portals/portal1.mdl` is four vertices wearing `writez`, a
//! depth-only shader whose whole job is to punch a depth hole for the
//! recursive view. `prop_portal` therefore reports no
//! [`ModelState`](crate::server::class::ModelState) at all, so it never
//! reaches [`EntityModels`](super::entities::EntityModels) — and if it did,
//! `writez` is not a shader this port has, so the checkerboard would draw a
//! magenta rectangle across the wall.
//!
//! **The quad's winding comes from `up × right == forward`.** Source's
//! `(forward, right, up)` basis is left-handed — with yaw 0 they are `+X`,
//! `-Y`, `+Z` — so the pair that satisfies `u × v == n` is `(up, right)` and
//! not `(right, up)`. Getting it the other way round back-face-culls the oval
//! and the portal is simply invisible, which is the failure
//! `materials::preview`'s ground quad already documents.
//!
//! **`uv.y` is 0 at the top.** The shader's bottom-to-top brightness shift
//! reads `abs(uv.y)`, so building the quad the other way up inverts the
//! gradient — and an upside-down gradient on a symmetric oval looks
//! deliberate.

use std::sync::Arc;

use glam::{Mat4, Vec3};

use crate::materials::context::Pass;
use crate::materials::material::{Material, MaterialCache};
use crate::materials::mesh::SimpleVertex;
use crate::materials::uniforms::PortalOverlay;
use crate::math::angle_matrix;

/// `models/portals/portalstaticoverlay_1.vmt` and `_2`, the blue one and the
/// orange one.
///
/// **Two materials rather than one with a swapped texture**, which is
/// `portdocs/PORTAL.md` §12's open question answered: they differ only in
/// `$PortalColorTexture` (a 1,669-byte gradient strip each) and
/// `MaterialCache` is keyed by name, so two entries cost two tiny uploads and
/// nothing else. A material *instance* parameter would have been the other
/// answer, and the port does now have one — [`PortalOverlay`] — but it carries
/// the three values that genuinely change per frame, not a texture that never
/// changes at all.
const OVERLAY_MATERIALS: [&str; 2] = [
    "models/portals/portalstaticoverlay_1",
    "models/portals/portalstaticoverlay_2",
];

/// `models/portals/portal_stencil_hole.vmt` — `PortalRefract`'s `$Stage 1`,
/// the shape of the opening.
///
/// One material for both colours, because the hole is not coloured. It is
/// drawn twice per portal per frame by
/// [`draw_portal_views`](super::World::draw_portal_views): once to mark the
/// opening in the stencil buffer and once to take the mark away and put the
/// wall's depth back. `portdocs/PORTAL_RENDER.md` §6.1.
const HOLE_MATERIAL: &str = "models/portals/portal_stencil_hole";

/// The `BufferClearObeyStencil` material, written in code because Valve writes
/// its eight in code too (`CMatRenderContext::GetBufferClearObeyStencil`).
///
/// This is variant 4 of the eight: depth only, no colour, no alpha —
/// `ClearBuffersObeyStencil( false, true )`, which is what step 2 of a portal
/// view asks for. The three `$clear*` keys are the shader's declared
/// parameters and are written here for the record; nothing reads them, because
/// the write masks they would pick are
/// [`buffer_clear_render_state`](crate::materials::shader) constants.
const CLEAR_MATERIAL_NAME: &str = "___bufferclearobeystencil_depth";
const CLEAR_MATERIAL: &str = r#"
"BufferClearObeyStencil"
{
    "$clearcolor" "0"
    "$clearalpha" "0"
    "$cleardepth" "1"
}
"#;

/// The full-screen quad [`Portals::clear_depth`] draws, in normalized device
/// coordinates with `z` at the far plane.
///
/// `CMatRenderContext::DrawClearBufferQuad` (`cmatrendercontext.cpp:2443`),
/// including the `1.1` — *"1.1 instead of 1.0 to fix small borders around the
/// edges in full screen with anti-aliasing enabled"*. `z` is
/// [`CLEAR_DEPTH`](crate::materials::target), the value a depth clear writes,
/// because resetting the depth inside the opening is exactly what this is.
const CLEAR_QUAD: [[f32; 3]; 4] = [
    [-1.1, -1.1, 1.0],
    [-1.1, 1.1, 1.0],
    [1.1, 1.1, 1.0],
    [1.1, -1.1, 1.0],
];

/// How fast `$PortalOpenAmount` climbs, in units per second.
///
/// `m_fOpenAmount += gpGlobals->frametime * ( 2.0f / flSlowdown )`
/// (`c_prop_portal.cpp:246`), where `flSlowdown` is
/// `GameTimescale()->GetCurrentTimescale()` and is 1 outside of a slow-motion
/// script. So a portal opens over **half a second**.
const OPEN_RATE: f32 = 2.0;

/// How fast `$PortalStatic` decays, in units per second.
///
/// `m_fStaticAmount -= gpGlobals->frametime` (`:228`) from 1, so the
/// interference clears over **one second** — twice as long as the opening
/// takes, which is what leaves a settled ring behind a filled disc.
const STATIC_RATE: f32 = 1.0;

/// What `$time` is wrapped to. `flTime -= floor( flTime / 1000 ) * 1000`
/// (`portal_refract_helper.cpp:180`); see
/// [`PortalOverlay::time`].
const TIME_WRAP: f32 = 1000.0;

/// One portal, as `world/` needs it in order to draw one.
///
/// `server/` names no GPU type and `world/` names no server type, so this is
/// the vocabulary between them —
/// [`PortalState`](crate::server::PortalState) is its other half and
/// `Engine::frame` copies one into the other once a rendered frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Portal {
    /// An opaque, stable key from whoever made the list.
    ///
    /// Nothing about *drawing* a portal reads it — unlike a model instance a
    /// portal owns no uploaded geometry, so there is nothing to match a sync
    /// against and the list is simply rebuilt. What reads it is
    /// [`linked`](Portal::linked), which names a partner by it, and the
    /// recursive view, which uses it to refuse to re-enter the portal it came
    /// out of.
    pub id: u64,
    pub origin: Vec3,
    /// Pitch, yaw, roll.
    pub angles: Vec3,
    pub half_width: f32,
    pub half_height: f32,
    /// Which of [`OVERLAY_MATERIALS`] to wear.
    pub is_portal2: bool,
    /// How long this portal has been open, in seconds — **not** the instant it
    /// opened.
    ///
    /// The subtraction happens on the far side of the seam, in `engine/`,
    /// because the two clocks that would have to be subtracted here belong to
    /// different modules: the instant is the *server's* tick clock and the
    /// elapsed time is measured against the *scene's*.
    pub open_for: f32,
    /// How long ago this portal last filled with interference, in seconds.
    ///
    /// A second clock because the events that reset it are a different set —
    /// `crate::server::classes::PropPortal::static_at` has the table. The one
    /// that is easy to miss: a portal fills with static when its **partner**
    /// moves or lights up, without re-opening.
    pub static_for: f32,
    /// The [`id`](Portal::id) of this portal's partner, if it has one.
    ///
    /// Two readers, and they arrived a stage apart:
    /// [`World::sync_portals`](super::World::sync_portals) carves from this
    /// list and the far side of a portal is half of what the carve has to know
    /// (`crate::engine::trace::PortalLink`), and
    /// [`Portals::pairs`] turns it into the recursive view's candidate list.
    /// The *oval* still reads neither.
    pub linked: Option<u64>,
    /// `m_matrixThisToLinked`, the identity while unlinked.
    ///
    /// The carve's and the recursive view's; the oval is drawn in world space
    /// and never needs it. It travels rather than being rederived because
    /// there is exactly one teleport matrix in the port and a second spelling
    /// of it can silently lose the 180° about up — `portdocs/PORTAL.md` §3.2.
    pub matrix: glam::Mat4,
}

/// Every portal in the level, and the two materials they wear.
///
/// Rebuilt whole by [`sync`](Portals::sync) once a rendered frame. There is no
/// per-instance load step and nothing to keep alive across an absence, which
/// is why this is a plain `Vec` where
/// [`EntityModels`](super::entities::EntityModels) is a keyed table.
pub struct Portals {
    /// Indexed by `is_portal2`. `Arc`, like every other holder of a
    /// `MaterialCache` entry: the cache owns the material and hands out
    /// shares of it.
    materials: [Arc<Material>; 2],
    /// [`HOLE_MATERIAL`], the recursive view's stencil punch.
    hole: Arc<Material>,
    /// [`CLEAR_MATERIAL`], the recursive view's depth reset.
    clear: Arc<Material>,
    live: Vec<Portal>,
}

impl Portals {
    /// Loads the two overlay materials.
    ///
    /// **Unconditional, on every map**, including the 96 of 106 that place no
    /// `prop_portal`. That is Valve's: `c_prop_portal.cpp:75` precaches both
    /// through `PRECACHE( MATERIAL, ... )`, which runs per level and asks no
    /// questions about the entity list. The cost is two `.vmt`s and three
    /// `.vtf`s — a 256x256 DXT1 noise field shared between them and a 256x1
    /// gradient strip each, 45 KB in total.
    ///
    /// A material that will not load is the error material, not an error, so
    /// this cannot fail; on a build with no game content mounted both entries
    /// are the checkerboard and a portal draws as a magenta rectangle.
    pub fn load(materials: &mut MaterialCache, vfs: &crate::filesystem::Vfs) -> Portals {
        Portals {
            materials: OVERLAY_MATERIALS.map(|name| materials.load(vfs, name)),
            hole: materials.load(vfs, HOLE_MATERIAL),
            clear: materials.synthetic(CLEAR_MATERIAL_NAME, CLEAR_MATERIAL),
            live: Vec::new(),
        }
    }

    /// Every linked portal, paired with what is on the other side of it.
    ///
    /// The recursive view's candidate list —
    /// `CPortalRender::m_ActivePortals` reduced to the two conditions that
    /// matter here: a portal draws a view only if it has a partner, and only
    /// the *entrance* side of the relationship is needed, since the partner
    /// contributes nothing but its plane and its five visibility points.
    ///
    /// A `Vec` rather than an iterator because the caller holds `&World`
    /// across a recursive call that borrows it again, and because the longest
    /// it can be is two.
    pub fn pairs(&self) -> Vec<PortalPair> {
        self.live
            .iter()
            .enumerate()
            .filter_map(|(index, portal)| {
                let partner = portal.linked?;
                let exit = self.live.iter().find(|other| other.id == partner)?;
                Some(PortalPair::new(index, portal, exit))
            })
            .collect()
    }

    /// [`HOLE_MATERIAL`], for the one test that has to know it is not the
    /// error checkerboard.
    ///
    /// A fallback material draws a magenta rectangle, which through a stencil
    /// hole is a picture like any other — so this is the seam that lets
    /// `portalview`'s rendered test rule that out before it compares pixels.
    /// `#[cfg(test)]` because that is its only caller and nothing in a running
    /// frame has any business asking which material a portal's hole wears.
    #[cfg(test)]
    pub fn hole_material(&self) -> &Material {
        &self.hole
    }

    /// [`CLEAR_MATERIAL`], for the same reason as
    /// [`hole_material`](Portals::hole_material).
    #[cfg(test)]
    pub fn clear_material(&self) -> &Material {
        &self.clear
    }

    /// Replaces the list with what the game says is active now.
    pub fn sync(&mut self, portals: &[Portal]) {
        self.live.clear();
        self.live.extend_from_slice(portals);
    }

    /// Where each portal is, for the translucent sort's key.
    ///
    /// A portal's box centre and its origin are not the same point — the
    /// collision box runs 64 units *forward* of the plane — and this is the
    /// origin, which is where the quad is. `SortEntities` uses
    /// `CollisionProp()->WorldSpaceCenter()`; for a flat quad the two answers
    /// differ by nothing that can change an ordering.
    pub fn centers(&self) -> impl Iterator<Item = (usize, Vec3)> + '_ {
        self.live
            .iter()
            .enumerate()
            .map(|(index, portal)| (index, portal.origin))
    }

    /// Binds one portal's three per-instance numbers for the draws that
    /// follow.
    ///
    /// `C_Prop_Portal::ClientThink` (`c_prop_portal.cpp:222`), integrated in
    /// closed form. Both curves are clamped at both ends because `open_for` is
    /// an elapsed time and a level that has just restarted can hand over a
    /// negative one.
    ///
    /// Shared by the oval and the hole, and they must not disagree: the hole's
    /// radius and the ring's radius are the same expression of the same open
    /// amount, so a ring drawn from one block and a hole from another would
    /// stop being concentric.
    fn bind_overlay(&self, pass: &mut Pass<'_>, curtime: f32, portal: &Portal, static_amount: f32) {
        let open_amount = (portal.open_for * OPEN_RATE).clamp(0.0, 1.0);
        pass.set_portal_overlay(&PortalOverlay {
            open_amount,
            // **`g_flPortalActive` is `1 - $PortalStatic`**
            // (`portal_refract_helper.cpp:216`), and passing the static amount
            // straight through inverts the whole effect.
            portal_active: 1.0 - static_amount,
            time: curtime - (curtime / TIME_WRAP).floor() * TIME_WRAP,
            _padding: 0.0,
        });
    }

    /// Records one portal's opening, for the stencil.
    ///
    /// `$Stage 1`. Drawn twice per portal per frame with different state — see
    /// [`draw_portal_views`](super::World::draw_portal_views), which is the
    /// only caller, and `portdocs/PORTAL_RENDER.md` §2.2. The geometry is
    /// [`quad`], the same four vertices the oval uses, **at the same place**:
    /// the depth this restores at step 4 is the depth the oval is then tested
    /// against, so a hole pushed even a quarter of a unit off the wall would
    /// cut the ring out of its own opening.
    pub fn draw_hole(&self, pass: &mut Pass<'_>, curtime: f32, index: usize) {
        let Some(portal) = self.live.get(index) else {
            return;
        };
        // `$Stage 1` reads `$PortalOpenAmount` and nothing else — see
        // `shaders/portalhole.wgsl` — so the static amount bound with it is
        // never sampled and the settled value is as good as any.
        self.bind_overlay(pass, curtime, portal, 0.0);
        let vertices = quad(portal);
        let vertices = pass.vertices(&vertices);
        let indices = pass.indices(&QUAD_INDICES);
        pass.draw(&self.hole, &vertices, &indices, Mat4::IDENTITY);
    }

    /// Records the near-plane cap, as a triangle fan over a polygon
    /// [`near_plane_cap`](super::portalview::near_plane_cap) built in world
    /// space.
    ///
    /// **Every vertex carries texture coordinate `(0.5, 0.5)`** — the centre
    /// of the portal, where the stage-1 alpha test passes for any open amount.
    /// The cap is unconditionally inside the opening, which is what it is for;
    /// giving it the quad's real coordinates would cut an oval out of the
    /// patch and leave a hole in the hole.
    pub fn draw_hole_cap(&self, pass: &mut Pass<'_>, curtime: f32, index: usize, cap: &[Vec3]) {
        let Some(portal) = self.live.get(index) else {
            return;
        };
        if cap.len() < 3 {
            return;
        }
        self.bind_overlay(pass, curtime, portal, 0.0);

        let vertices: Vec<SimpleVertex> = cap
            .iter()
            .map(|point| SimpleVertex::new(point.to_array(), [0.5, 0.5]))
            .collect();
        let mut indices = Vec::with_capacity((cap.len() - 2) * 3);
        for triangle in 0..cap.len() - 2 {
            indices.extend_from_slice(&[0, triangle as u16 + 1, triangle as u16 + 2]);
        }
        let vertices = pass.vertices(&vertices);
        let indices = pass.indices(&indices);
        pass.draw(&self.hole, &vertices, &indices, Mat4::IDENTITY);
    }

    /// Resets the depth buffer wherever the stencil test lets it.
    ///
    /// `ClearBuffersObeyStencil( false, true )`. The caller sets the stencil
    /// and, for anything but a debug frame, a scissor — without one this is a
    /// full-screen draw, which is what `DrawClearBufferQuad` always is.
    ///
    /// The quad is in **clip space**: [`CLEAR_QUAD`], straight through the
    /// vertex shader. `portdocs/PORTAL_RENDER.md` §6.2.
    pub fn clear_depth(&self, pass: &mut Pass<'_>) {
        let vertices: Vec<SimpleVertex> = CLEAR_QUAD
            .iter()
            .map(|position| SimpleVertex::new(*position, [0.0, 0.0]))
            .collect();
        let vertices = pass.vertices(&vertices);
        let indices = pass.indices(&QUAD_INDICES);
        pass.draw(&self.clear, &vertices, &indices, Mat4::IDENTITY);
    }

    /// Records one portal's quad.
    ///
    /// `curtime` is the scene clock, and it reaches the shader twice: once as
    /// the noise scroll and once, through
    /// [`Portal::open_for`], as the opening curve.
    ///
    /// `remaining_depth` is how many more recursion levels a portal drawn in
    /// *this* scene could open onto — see
    /// [`static_amount`], which is the only thing that
    /// reads it and is where the shipped game's end-of-the-line kludge lives.
    pub fn draw_one(&self, pass: &mut Pass<'_>, curtime: f32, index: usize, remaining_depth: u8) {
        let Some(portal) = self.live.get(index) else {
            return;
        };
        self.bind_overlay(
            pass,
            curtime,
            portal,
            static_amount(portal, remaining_depth),
        );

        let vertices = quad(portal);
        let vertices = pass.vertices(&vertices);
        let indices = pass.indices(&QUAD_INDICES);
        // The identity, because the four vertices are already in world space —
        // `DrawSimplePortalMesh` pushes the model matrix and loads the identity
        // for exactly this reason. A portal is placed rarely and drawn from a
        // rebuilt quad every frame, so there is no transform to reuse.
        pass.draw(
            &self.materials[usize::from(portal.is_portal2)],
            &vertices,
            &indices,
            Mat4::IDENTITY,
        );
    }
}

/// `C_Prop_Portal::ComputeStaticAmountForRendering`
/// (`c_prop_portal.cpp:1148`) — what `$PortalStatic`'s proxy actually
/// writes, which is `m_fStaticAmount` only some of the time.
///
/// `remaining_depth` is `CPortalRender::GetRemainingPortalViewDepth()`:
/// how many *more* recursion levels a portal drawn in this scene could
/// still open onto. It is `r_portal_stencil_depth` minus the level being
/// drawn, and the whole reason the field exists is its own comment —
/// *"let's portals know that they should do 'end of the line' kludges to
/// cover up that portals don't go infinitely recursive"*.
///
/// Two overrides, and both are the difference between a portal that looks
/// like a hole and one that looks like a mistake:
///
/// - **an unlinked portal is full static.** It has nothing to show, and
///   without this it settles into a clear oval with the wall visible
///   through it.
/// - **so is the deepest one drawn.** At `r_portal_stencil_depth 0` that
///   is *every* portal, which is what makes the flat-oval debug setting
///   look like the shipped game's rather than like a decal.
///
/// The third branch — `m_fSecondaryStaticAmount` at
/// `remaining_depth == 1`, *"fading in from no views to another view
/// (player just walked through it)"* — is **dead in this tree** and is not
/// ported. The field is declared, decayed by `ClientThink` and set to
/// `0.0f` in two places (`c_prop_portal.cpp:933`, `:1019`); nothing
/// anywhere assigns it a non-zero value, so the branch's own guard
/// (`m_fSecondaryStaticAmount > flStaticAmount`) can only pass when the
/// static amount is already negative. The depth doubler's branch above it
/// goes with `WillUseDepthDoublerThisDraw`, which
/// `portdocs/PORTAL_RENDER.md` §7 deletes.
fn static_amount(portal: &Portal, remaining_depth: u8) -> f32 {
    if portal.linked.is_none() || remaining_depth == 0 {
        return 1.0;
    }
    (1.0 - portal.static_for * STATIC_RATE).clamp(0.0, 1.0)
}

/// One linked portal and everything the recursive view needs to know about
/// the other side of it.
///
/// Gathered once per level from [`Portals::pairs`] rather than looked up
/// repeatedly, because the *exit* half of it is what
/// `CPortalRenderable_FlatBasic::PortalMoved`
/// (`portalrenderable_flatbasic.cpp:63`) precomputes and caches: five points
/// that are all a unit in front of the exit plane, and the plane itself.
#[derive(Debug, Clone, Copy)]
pub struct PortalPair {
    /// Index into [`Portals`]' live list, for the draws.
    pub index: usize,
    /// This portal's key.
    pub id: u64,
    /// The partner's key — what the recursion must not follow back.
    pub partner_id: u64,
    /// `m_matrixThisToLinked`, entrance to exit.
    pub matrix: Mat4,
    pub origin: Vec3,
    /// The portal's basis, as unit vectors. `right` is the **negation** of the
    /// angle matrix's second column, which is Valve's `m_vRight = -m_vRight`
    /// (`portal_base2d.cpp:1213`) — column 1 is *left*.
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub half_width: f32,
    pub half_height: f32,
    /// The four world corners of this portal's own quad, for the screen
    /// rectangle and the frustum test. **On the portal's plane**, not the unit
    /// in front the PVS points sit at: this rectangle has to bound the pixels
    /// the opening actually covers.
    pub corners: [Vec3; 4],
    /// The exit portal's plane, for the oblique near plane.
    pub exit_origin: Vec3,
    pub exit_forward: Vec3,
    /// `m_ptForwardOrigin` of the exit portal — one unit in front of it, and
    /// the point whose leaf the area flood is forced to start from.
    pub exit_forward_origin: Vec3,
    /// The five points the exit portal contributes to the PVS: its
    /// `m_ptForwardOrigin` and its four `m_ptCorners`, all a unit in front of
    /// its plane. See [`ViewPoint`](super::vis::ViewPoint) for why the virtual
    /// camera's own position is useless here.
    pub exit_vis_origins: [Vec3; 5],
}

impl PortalPair {
    fn new(index: usize, entrance: &Portal, exit: &Portal) -> PortalPair {
        let (forward, right, up) = basis(entrance);
        let (exit_forward, exit_right, exit_up) = basis(exit);
        let exit_forward_origin = exit.origin + exit_forward;
        let exit_across = exit_right * exit.half_width;
        let exit_upward = exit_up * exit.half_height;

        PortalPair {
            index,
            id: entrance.id,
            partner_id: exit.id,
            matrix: entrance.matrix,
            origin: entrance.origin,
            forward,
            right,
            up,
            half_width: entrance.half_width,
            half_height: entrance.half_height,
            corners: {
                let across = right * entrance.half_width;
                let upward = up * entrance.half_height;
                [
                    entrance.origin - across + upward,
                    entrance.origin + across + upward,
                    entrance.origin + across - upward,
                    entrance.origin - across - upward,
                ]
            },
            exit_origin: exit.origin,
            exit_forward,
            exit_forward_origin,
            exit_vis_origins: [
                exit_forward_origin,
                exit_forward_origin + exit_across + exit_upward,
                exit_forward_origin - exit_across + exit_upward,
                exit_forward_origin - exit_across - exit_upward,
                exit_forward_origin + exit_across - exit_upward,
            ],
        }
    }

    /// The axis-aligned bounds of this portal's quad, for the frustum test.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.corners.iter().fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(mins, maxs), corner| (mins.min(*corner), maxs.max(*corner)),
        )
    }
}

/// A portal's `(forward, right, up)`, as unit vectors.
///
/// The same expression [`quad`] uses, factored out because the recursive view
/// wants the directions without the sizes. See [`PortalPair::right`] for the
/// negation.
fn basis(portal: &Portal) -> (Vec3, Vec3, Vec3) {
    let rotation = angle_matrix(portal.angles);
    (
        rotation * Vec3::X,
        -(rotation * Vec3::Y),
        rotation * Vec3::Z,
    )
}

/// Two triangles over four corners, wound counter-clockwise seen from `+n` —
/// the same order `materials::preview`'s `QUAD_INDICES` uses, and the same
/// convention.
const QUAD_INDICES: [u16; 6] = [0, 1, 2, 0, 2, 3];

/// The four world-space corners of a portal's quad, with Valve's texture
/// coordinates.
///
/// `DrawSimplePortalMesh`'s four `meshBuilder` blocks, reordered into the
/// `(-u,-v), (+u,-v), (+u,+v), (-u,+v)` sequence [`QUAD_INDICES`] wants. The
/// coordinates are carried across vertex by vertex rather than derived, so the
/// mapping is checkable against the reference by reading:
///
/// | corner | Valve's uv |
/// |---|---|
/// | `+right -up` | `(0, 1)` |
/// | `+right +up` | `(0, 0)` |
/// | `-right -up` | `(1, 1)` |
/// | `-right +up` | `(1, 0)` |
///
/// So `u` runs from the portal's **right** edge to its left and `v` from its
/// top to its bottom — both backwards from the obvious reading, and both
/// load-bearing: `v` decides which end of the oval the shader's
/// bottom-to-top brightness shift darkens.
///
/// The vertex colour is white and unread. `DrawSimplePortalMesh` writes
/// `{1, 1, 1, flAlpha}` with `flAlpha` 0.25, and neither `.fxc` declares a
/// colour input at all — `portal_refract_vs20.fxc`'s `VS_INPUT` has position,
/// normal, two texture coordinates and a tangent, and no `COLOR`. So the
/// quarter alpha the call site passes reaches nothing, which is worth knowing
/// before anyone reproduces it and wonders why the oval did not dim.
fn quad(portal: &Portal) -> [SimpleVertex; 4] {
    // The portal's basis. **`right` is the negation of the angle matrix's
    // second column**, which is Valve's `m_vRight = -m_vRight`
    // (`portal_base2d.cpp:1213`) — column 1 is *left*.
    let (_, right, up) = basis(portal);
    let right = right * portal.half_width;
    let up = up * portal.half_height;
    let origin = portal.origin;

    // `u = up`, `v = right`, because `up × right == forward` in Source's
    // left-handed basis — see the module docs. The corners then run
    // `(-u,-v), (+u,-v), (+u,+v), (-u,+v)`, which is counter-clockwise seen
    // from in front of the portal.
    let corner =
        |position: Vec3, texcoord: [f32; 2]| SimpleVertex::new(position.to_array(), texcoord);
    [
        corner(origin - up - right, [1.0, 1.0]),
        corner(origin + up - right, [1.0, 0.0]),
        corner(origin + up + right, [0.0, 0.0]),
        corner(origin - up + right, [0.0, 1.0]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ComputeStaticAmountForRendering`'s two live branches, and the curve
    /// they override.
    #[test]
    fn a_portal_with_nothing_behind_it_is_full_static() {
        let settled = Portal {
            static_for: 10.0,
            linked: Some(2),
            ..portal(Vec3::ZERO)
        };
        assert_eq!(static_amount(&settled, 2), 0.0, "settled and linked");
        // Half a second in, half the interference is left.
        assert!(
            (static_amount(
                &Portal {
                    static_for: 0.5,
                    ..settled
                },
                2
            ) - 0.5)
                .abs()
                < 1e-6
        );

        // *"end of the line, no more views"* — whatever the clock says.
        assert_eq!(static_amount(&settled, 0), 1.0);
        // …and an unlinked portal, at any depth.
        assert_eq!(
            static_amount(
                &Portal {
                    linked: None,
                    ..settled
                },
                2
            ),
            1.0
        );
    }

    fn portal(angles: Vec3) -> Portal {
        Portal {
            id: 1,
            origin: Vec3::new(10.0, 20.0, 30.0),
            angles,
            half_width: 32.0,
            half_height: 56.0,
            is_portal2: false,
            open_for: 1.0,
            static_for: 1.0,
            linked: None,
            matrix: glam::Mat4::IDENTITY,
        }
    }

    /// The winding test, and it is the one that decides whether a portal is
    /// visible at all: `FrontFace::Ccw` with back-face culling means the
    /// corners must run counter-clockwise **seen from in front of the portal**,
    /// which is the `+forward` side.
    ///
    /// Checked as a cross product rather than by rendering, because the
    /// failure is silent — the oval simply is not there — and because the sign
    /// of `up × right` is the whole of what could be wrong.
    #[test]
    fn the_quad_faces_out_of_the_wall() {
        for angles in [
            Vec3::ZERO,
            Vec3::new(0.0, 90.0, 0.0),
            Vec3::new(0.0, 180.0, 0.0),
            // `sp_a1_intro4`'s `section_2_portal_a2_rm3a`, the one shipped
            // portal with a non-zero pitch *and* yaw.
            Vec3::new(90.0, 180.0, 0.0),
            // `sp_a1_intro7`'s, which is at no right angle at all.
            Vec3::new(-75.4673, 21.1748, 6.802),
        ] {
            let p = portal(angles);
            let v = quad(&p);
            let position = |i: usize| Vec3::from_array(v[i].position);
            let normal = (position(1) - position(0))
                .cross(position(2) - position(0))
                .normalize();
            let forward = angle_matrix(angles) * Vec3::X;
            assert!(
                normal.dot(forward) > 0.999,
                "{angles}: quad faces {normal}, portal faces {forward}",
            );
        }
    }

    /// The texture coordinates against `DrawSimplePortalMesh`'s own table:
    /// `u` runs from the right edge to the left, `v` from the top to the
    /// bottom.
    ///
    /// The `v` half is what the shader's bottom-to-top brightness shift reads,
    /// so an inverted quad is a gradient the wrong way up rather than a
    /// missing portal.
    #[test]
    fn the_texture_coordinates_are_valves_way_round() {
        let p = portal(Vec3::ZERO);
        let v = quad(&p);
        let rotation = angle_matrix(p.angles);
        let right = -(rotation * Vec3::Y);
        let up = rotation * Vec3::Z;

        for vertex in v {
            let offset = Vec3::from_array(vertex.position) - p.origin;
            let on_right = offset.dot(right) > 0.0;
            let on_top = offset.dot(up) > 0.0;
            assert_eq!(
                vertex.texcoord,
                [
                    if on_right { 0.0 } else { 1.0 },
                    if on_top { 0.0 } else { 1.0 },
                ],
                "corner at right={on_right} top={on_top}",
            );
        }
    }

    /// The quad is the portal's own size, centred on its origin: two corners
    /// half a width apart across and half a height apart up.
    #[test]
    fn the_quad_is_the_portals_size() {
        let p = portal(Vec3::new(0.0, 90.0, 0.0));
        let v = quad(&p);
        let position = |i: usize| Vec3::from_array(v[i].position);
        assert!(((position(2) - position(1)).length() - 2.0 * p.half_width).abs() < 1e-3);
        assert!(((position(1) - position(0)).length() - 2.0 * p.half_height).abs() < 1e-3);
        let center = (position(0) + position(2)) * 0.5;
        assert!((center - p.origin).length() < 1e-3);
    }
}

#[cfg(test)]
mod rendered {
    use super::*;
    use crate::materials::context::{Camera, Load, RenderContext};
    use crate::materials::target::RenderTarget;
    use crate::materials::MaterialCache;

    const SIZE: u32 = 256;

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            eprintln!("skipping: adapter has no BC texture support");
            return None;
        }
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
            ..Default::default()
        }))
        .ok()
    }

    /// **The oval, drawn — blue on one portal and orange on the other, and
    /// different while it is still opening.**
    ///
    /// The one test that can say the whole draw path works, because every step
    /// of it is invisible on its own: the two materials could have fallen back
    /// to the checkerboard, the quad could be back-face culled and absent, the
    /// group-3 block could be arriving as zeroes, or the 256x1 gradient strip
    /// could be sampled off its single row. All of those produce *something*;
    /// only one of them produces two differently-coloured ovals that **change
    /// while the portal opens**.
    ///
    /// It needs the game mounted, for the two materials, and **not a map**:
    /// what a portal draws does not depend on where it is, so the two portals
    /// here are placed by hand, facing the camera, against a black clear.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_portal_overlay_draws -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install and a GPU; set KISAK_GAME_DIR"]
    fn the_portal_overlay_draws_in_two_colours_and_opens() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let Some((device, queue)) = device() else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = crate::filesystem::Vfs::mount_game(&dir, &base, &Default::default())
            .expect("mount the game");

        let mut materials = MaterialCache::new(&device, &queue);
        let mut portals = Portals::load(&mut materials, &vfs);

        // **Both overlay materials must be real**, and this is the first thing
        // that could be silently wrong: the checkerboard draws a rectangle, and
        // every assertion below would still pass on "something drew".
        for (index, material) in portals.materials.iter().enumerate() {
            println!(
                "  overlay {index}: {} ({:?})",
                material.name, material.shader
            );
            assert_eq!(
                material.shader,
                crate::materials::shader::ShaderKind::PortalRefract,
                "overlay {index} is {} — it fell back to the error material",
                material.name
            );
        }

        let mut context = RenderContext::new(&device, &queue, materials.pipelines());
        let target = RenderTarget::new(
            &device,
            "portal",
            SIZE,
            SIZE,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            true,
        );

        // Straight at the portal from four feet in front of it, which is where
        // a player about to walk through one would be. `+X` forward, so the eye
        // is on the `+X` side.
        let eye = Vec3::new(48.0, 0.0, 56.0);
        let camera = Camera::perspective(
            eye,
            glam::Mat4::look_at_rh(eye, Vec3::new(0.0, 0.0, 56.0), Vec3::Z),
            75.0,
            1.0,
            1.0,
            4096.0,
        );

        let mut shot = |portals: &Portals, curtime: f32| -> Vec<u8> {
            context.begin_frame();
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size: (SIZE * SIZE * 4) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = context.offscreen_pass(
                    &mut encoder,
                    materials.pipelines(),
                    &target,
                    &camera,
                    Load::Clear(wgpu::Color::BLACK),
                );
                for index in 0..portals.live.len() {
                    // One level still to go, so `static_amount` uses the
                    // portal's own curve rather than the end-of-the-line 1.
                    portals.draw_one(&mut pass, curtime, index, 1);
                }
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: target.color_texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(SIZE * 4),
                        rows_per_image: Some(SIZE),
                    },
                },
                wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
            readback.slice(..).map_async(wgpu::MapMode::Read, |r| {
                r.expect("readback mapped");
            });
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("the queue drained");
            let pixels = readback.slice(..).get_mapped_range().unwrap().to_vec();
            readback.unmap();
            pixels
        };

        // `Bgra8UnormSrgb`, so the channels arrive blue first — which is worth
        // stating, because reading them as RGB swaps the two colours and the
        // colour assertions below would then be exactly backwards.
        let mean = |pixels: &[u8]| -> (f64, f64, f64, usize) {
            let mut sum = [0u64; 3];
            let mut drawn = 0;
            for p in pixels.chunks_exact(4) {
                if p[0..3] != [0, 0, 0] {
                    drawn += 1;
                }
                for (i, s) in sum.iter_mut().enumerate() {
                    *s += u64::from(p[i]);
                }
            }
            let n = (pixels.len() / 4) as f64;
            (
                sum[2] as f64 / n,
                sum[1] as f64 / n,
                sum[0] as f64 / n,
                drawn,
            )
        };

        for is_portal2 in [false, true] {
            let colour = if is_portal2 { "orange" } else { "blue" };
            let portal = Portal {
                id: u64::from(is_portal2),
                origin: Vec3::new(0.0, 0.0, 56.0),
                // Facing `+X`, at the camera.
                angles: Vec3::ZERO,
                half_width: 32.0,
                half_height: 56.0,
                is_portal2,
                open_for: 10.0,
                static_for: 10.0,
                // **Linked, to a partner that is not in the list.** Nothing
                // about the oval reads where the partner is, but
                // [`static_amount`] reads *whether there is one*: an
                // unlinked portal is full static whatever its clock says, and
                // a fixture that left this `None` would compare two noise
                // fields and call it an opening animation.
                linked: Some(!u64::from(is_portal2)),
                matrix: glam::Mat4::IDENTITY,
            };

            // Settled: the ring, a second after opening.
            portals.sync(&[portal]);
            let settled = shot(&portals, 10.0);
            let (r, g, b, drawn) = mean(&settled);
            println!(
                "  {colour} settled: {drawn} of {} pixels drawn, mean rgb ({r:.2} {g:.2} {b:.2})",
                SIZE * SIZE
            );
            assert!(
                drawn > 500,
                "the {colour} oval drew {drawn} pixels; it is not on screen"
            );
            // **The colour comes from the gradient strip**, which is the whole
            // difference between the two materials — and it is what says the
            // 256x1 texture is sampled on its one row rather than clamped off
            // the end of it.
            match is_portal2 {
                false => assert!(r < b, "the blue portal is not blue: ({r:.2} {g:.2} {b:.2})"),
                true => assert!(
                    b < r,
                    "the orange portal is not orange: ({r:.2} {g:.2} {b:.2})"
                ),
            }

            // Half-open: a filled disc rather than a ring, because the static
            // has not cleared. This is what says group 3 reaches the shader at
            // all — with a zeroed block both shots would be identical.
            portals.sync(&[Portal {
                open_for: 0.25,
                static_for: 0.25,
                ..portal
            }]);
            let opening = shot(&portals, 10.0);
            let differences = settled
                .chunks_exact(4)
                .zip(opening.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count();
            let (_, _, _, opening_drawn) = mean(&opening);
            println!(
                "  {colour} half-open: {opening_drawn} pixels drawn, \
                 {differences} differ from settled"
            );
            assert!(
                differences > 500,
                "the {colour} oval drew identically half-open and settled: \
                 {differences} pixels differ — is the group-3 block reaching the shader?"
            );
        }
    }
}
