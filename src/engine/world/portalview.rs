//! The recursive view: the picture of the other room, seen through a portal.
//!
//! `game/client/portal/portalrender.cpp`'s `DrawPortalsUsingStencils_Old`
//! (`:1399`) and `CPortalRenderable_FlatBasic::RenderPortalViewToBackBuffer`
//! (`portalrenderable_flatbasic.cpp:347`), which between them are the whole of
//! what a portal *looks* like. `portdocs/PORTAL_RENDER.md` is the design; this
//! is the fifth kind of drawing in `world/` and the first that draws the other
//! four a second time.
//!
//! # The shape of it
//!
//! Drawing through a portal is drawing the scene twice: once from the player's
//! eye, and once from a **virtual eye** — the player carried through the
//! teleport matrix into the exit portal's room. Three mechanisms confine,
//! clip and clean up after the second picture:
//!
//! | Job | Mechanism |
//! |---|---|
//! | Confine it to the opening | the **stencil buffer** |
//! | Cut away the exit portal's own wall | an **oblique near plane** |
//! | Put the depth buffer back | a **second, depth-only draw of the opening** |
//!
//! All of it happens inside the pass that drew the opaque scene. Stencil
//! compare functions, masks and operations are pipeline state, the reference
//! value is dynamic state, and the camera is one more slot in a uniform arena —
//! so a portal view costs no render target, no attachment and no second
//! `begin_render_pass`.
//!
//! # Things here that produce a wrong picture rather than an error
//!
//! **The virtual eye is inside a wall.** A point in front of the entrance
//! images to the same distance *behind* the exit (`portdocs/PORTAL.md` §9's
//! invariant 15), so the virtual camera is inside the exit portal's wall: a
//! solid leaf, cluster -1, an empty PVS row. Visibility is therefore measured
//! from the exit portal's four corners and its forward origin, and the
//! area-portal flood is forced to start at the exit portal's leaf — see
//! [`ViewPoint`](super::vis::ViewPoint). Ask the plain
//! [`Visibility::mark`](super::vis::Visibility::mark) about the virtual eye and
//! the portal draws a black hole.
//!
//! **The oblique projection must not be the culling projection.** The shear
//! tilts the far plane as well as the near one, and a frustum extracted from it
//! culls geometry in plain sight. Every level therefore builds two cameras from
//! one view matrix: [`PortalCameras::cull`] and [`PortalCameras::draw`].
//!
//! **Step 4's draw has the depth test off and depth writes on.** With the test
//! on, the depth it is restoring is behind the sub-scene it has just drawn, and
//! every fragment is rejected — the opening keeps the far room's depth and
//! everything composited afterwards, the oval included, sits against the wrong
//! surface.
//!
//! **Colour writes must be off in step 4**, or the second draw of the hole
//! paints the portal's interior black over the picture just rendered into it.
//!
//! **The recursion must not re-enter the portal it came out of**
//! (`m_pRenderingViewExitPortal`, `portalrender.cpp:960`). The exit portal is
//! in the sub-scene's own list and is facing the virtual camera, so following
//! it goes straight back where it came from and every level below is wasted.

use glam::{Mat4, Vec3, Vec4};

use super::portals::PortalPair;
use super::vis::{Frustum, VisibleSet, FRUSTUM_NEARZ};
use super::World;
use crate::materials::context::{Camera, Pass, StateOverride};
use crate::materials::pipeline::{Stencil, StencilFunc, StencilOp};

/// `MAX_PORTAL_RECURSIVE_VIEWS` (`portalrender.h:20`), minus the one Valve adds
/// because *"0 tends to be the primary view in most arrays of this size"*.
///
/// Ten levels, and the comment beside it is the honest reason for the number:
/// *"5 is extremely choppy under best conditions and is barely visible"*. The
/// real limit here is the same as Valve's second term — an 8-bit stencil, whose
/// values are the recursion level itself (§2.1 of the portdoc).
pub const MAX_RECURSION: u8 = 10;

/// `r_portal_stencil_depth`'s default.
pub const DEFAULT_RECURSION: u8 = 2;

/// How far in front of the portal plane the stencil quad and the near-plane cap
/// are pushed.
///
/// `PORTAL_OFFSET` (`portalrenderable_flatbasic.cpp:1388`). The shipped mesh
/// applies a per-vertex decal offset along the normal instead
/// (`portal_refract_vs20.fxc:72`); this port offsets the whole quad by a
/// constant and leans on [`DepthBias::Decal`] for the rest, which is the same
/// arrangement `world/portals.rs` already makes for the oval.
///
/// [`DepthBias::Decal`]: crate::materials::pipeline::DepthBias
const PORTAL_OFFSET: f32 = 0.275;

/// How far behind the exit portal's plane the oblique near plane sits.
///
/// `vCustomClipPlane.w = vRemotePortalForward.Dot( ptRemotePortalPosition ) - 2.0f`
/// (`portalrenderable_flatbasic.cpp:434`), over the comment *"moving it back a
/// smidge to eliminate visual artifacts for half-in objects"*.
const CLIP_PLANE_SETBACK: f32 = 2.0;

/// How far in front of the eye the clip plane is forced to stay.
///
/// `CAMERA_DIST_EPSILON` (`:441`). A clip plane at or behind the eye makes the
/// sheared projection degenerate, and with [`CLIP_PLANE_SETBACK`] at 2 that
/// happens every time the player walks through a portal.
const CAMERA_DIST_EPSILON: f32 = 1.0;

/// What `Engine::render` hands to [`World::draw_portal_views`].
#[derive(Debug, Clone, Copy)]
pub struct PortalViewSetup {
    /// The scene clock, for the two curves the hole's open amount drives.
    pub curtime: f32,
    /// `r_portal_stencil_depth`, already clamped to [`MAX_RECURSION`].
    pub max_depth: u8,
    /// The target's size in pixels, for the scissor rectangle.
    pub viewport: (u32, u32),
    /// `r_novis`, forwarded to every sub-view's visibility so that the debug
    /// switch means the same thing inside a portal as outside one.
    pub novis: bool,
}

/// A rectangle in normalized device coordinates: `-1..1` on both axes, `y` up.
///
/// The same idea as [`vis::Rect`](super::vis), and deliberately not the same
/// type: that one accumulates a *union* of the windows an area was reached
/// through, and this one accumulates an **intersection** of the openings a view
/// is seen through. Two rectangles, opposite operations.
///
/// It stands in for `CalcFrustumThroughPolygon`'s unbounded plane list
/// (`portalrenderable_flatbasic.cpp:195`), which builds one plane per edge of
/// the portal's clipped silhouette. The rectangle is strictly weaker and
/// strictly stronger than nothing, and for a portal seen head-on the two
/// answers are the same. `portdocs/PORTAL_RENDER.md` §4.2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NdcRect {
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
}

impl NdcRect {
    /// The whole screen.
    pub const FULL: NdcRect = NdcRect {
        left: -1.0,
        right: 1.0,
        bottom: -1.0,
        top: 1.0,
    };

    /// The bounding rectangle of four world-space points under `view_proj`, or
    /// `None` if any of them is on or behind the near plane.
    ///
    /// `CPortalRenderable_FlatBasic::ComputeClipSpacePortalCorners`
    /// (`portalrenderable_flatbasic.cpp:1181`), which likewise refuses rather
    /// than clamping: a corner with `w <= 0` projects to the wrong side of the
    /// screen, and a rectangle built from it would exclude the part of the
    /// portal that is actually visible. Refusing means "no narrowing and no
    /// scissor for this level", which is correct and merely slower.
    pub fn of(corners: &[Vec3; 4], view_proj: Mat4) -> Option<NdcRect> {
        let mut rect = NdcRect {
            left: f32::MAX,
            right: f32::MIN,
            bottom: f32::MAX,
            top: f32::MIN,
        };
        for corner in corners {
            let clip = view_proj * corner.extend(1.0);
            if clip.w <= 1e-4 {
                return None;
            }
            let x = clip.x / clip.w;
            let y = clip.y / clip.w;
            rect.left = rect.left.min(x);
            rect.right = rect.right.max(x);
            rect.bottom = rect.bottom.min(y);
            rect.top = rect.top.max(y);
        }
        Some(rect.clamped())
    }

    fn clamped(self) -> NdcRect {
        NdcRect {
            left: self.left.clamp(-1.0, 1.0),
            right: self.right.clamp(-1.0, 1.0),
            bottom: self.bottom.clamp(-1.0, 1.0),
            top: self.top.clamp(-1.0, 1.0),
        }
    }

    /// The part of this rectangle that is also inside `other`.
    ///
    /// Sound across recursion levels *because every level's picture is drawn
    /// into the same pixels*: the virtual camera's projection maps the exit
    /// room onto the screen exactly where the entrance's opening is, so a
    /// rectangle measured at level 1 is in the same coordinates as one measured
    /// at level 0 and the two may simply be intersected.
    pub fn intersect(self, other: NdcRect) -> NdcRect {
        NdcRect {
            left: self.left.max(other.left),
            right: self.right.min(other.right),
            bottom: self.bottom.max(other.bottom),
            top: self.top.min(other.top),
        }
    }

    /// Whether the rectangle has any area left after an [`intersect`](Self::intersect).
    pub fn is_empty(self) -> bool {
        self.right <= self.left || self.top <= self.bottom
    }

    /// `projection` restricted to this rectangle of its own image.
    ///
    /// The half-space `x_ndc >= left` is `clip.x - left·clip.w >= 0`, so
    /// remapping `[left, right]` onto `[-1, 1]` is two row combinations — the
    /// same arithmetic [`Frustum::narrowed`](super::vis::Frustum) does, applied
    /// to the matrix instead of to the extracted planes. Doing it to the matrix
    /// is what lets the narrowing reach
    /// [`Visibility::mark_view`](super::vis::Visibility::mark_view), which
    /// builds its own frustum and takes no rectangle.
    ///
    /// **Only ever applied to the culling projection.** Narrowing the drawing
    /// projection would stretch the portal's contents across the whole screen.
    pub fn narrow(self, projection: Mat4) -> Mat4 {
        let row = |i: usize| projection.row(i);
        let (x, y, w) = (row(0), row(1), row(3));
        let sx = 2.0 / (self.right - self.left);
        let sy = 2.0 / (self.top - self.bottom);
        rows_to_mat4([
            (x - w * (self.left + self.right) * 0.5) * sx,
            (y - w * (self.bottom + self.top) * 0.5) * sy,
            row(2),
            w,
        ])
    }

    /// This rectangle as a `wgpu` scissor, in physical pixels with the origin
    /// at the top left.
    ///
    /// `y` flips, because NDC `y` points up and a scissor's does not. Returns
    /// `None` for a rectangle that rounds away to nothing, which `wgpu` rejects
    /// — a portal one pixel wide simply draws no view rather than failing.
    pub fn scissor(self, viewport: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
        let (width, height) = (viewport.0 as f32, viewport.1 as f32);
        let x0 = ((self.left * 0.5 + 0.5) * width).floor().max(0.0);
        let x1 = ((self.right * 0.5 + 0.5) * width).ceil().min(width);
        // Top of the rectangle is the *larger* `y` in NDC and the *smaller* one
        // in pixels.
        let y0 = ((0.5 - self.top * 0.5) * height).floor().max(0.0);
        let y1 = ((0.5 - self.bottom * 0.5) * height).ceil().min(height);
        let (w, h) = (x1 - x0, y1 - y0);
        (w >= 1.0 && h >= 1.0).then_some((x0 as u32, y0 as u32, w as u32, h as u32))
    }
}

/// The two cameras one recursion level needs.
///
/// One view matrix, two projections. See the module docs for why they cannot be
/// the same matrix.
#[derive(Debug, Clone, Copy)]
pub struct PortalCameras {
    /// What the sub-scene is culled with: the pristine projection, narrowed to
    /// the openings this view is seen through. Its frustum is a real frustum.
    pub cull: Camera,
    /// What the sub-scene is drawn with: the pristine projection sheared so
    /// that its near plane is the exit portal's plane.
    pub draw: Camera,
}

/// The virtual camera for one portal, both ways.
///
/// `RenderPortalViewToBackBuffer` (`:395-412`) computes
///
/// ```text
/// ptPOVOrigin = m_matrixThisToLinked * cameraView.origin
/// matTemp     = matCurrentView * m_pLinkedPortal->m_matrixThisToLinked
/// ```
///
/// and the second matrix is the *linked* portal's, which is this one's inverse
/// — so in this port's column-major convention with vectors on the right it is
/// one expression, `view * matrix.inverse()`. There is exactly one teleport
/// matrix in the port (`server::classes::portal::teleport_matrix`), and it
/// travels here on [`Portal::matrix`](super::portals::Portal); a second
/// spelling can silently lose the 180° about up, which is
/// `portdocs/PORTAL.md` §3.2's whole warning.
///
/// **No cull-mode flip.** A mirror needs one because its view matrix is
/// reflected; a portal transform is a rotation and a translation, determinant
/// `+1`, so winding is preserved.
pub fn portal_cameras(
    camera: &Camera,
    base_projection: Mat4,
    pair: &PortalPair,
    rect: NdcRect,
) -> PortalCameras {
    let eye = pair.matrix.transform_point3(camera.eye);
    let view = camera.view * pair.matrix.inverse();

    PortalCameras {
        cull: Camera {
            view,
            projection: rect.narrow(base_projection),
            eye,
        },
        draw: Camera {
            view,
            projection: oblique_near_plane(base_projection, view, exit_clip_plane(pair, eye)),
            eye,
        },
    }
}

/// The exit portal's plane, as a world-space `(normal, -dist)` 4-vector whose
/// positive side is kept.
///
/// `vCustomClipPlane` (`portalrenderable_flatbasic.cpp:430-450`), with the
/// setback and the degeneracy guard.
///
/// # A divergence, recorded
///
/// Valve's guard measures `DotProduct( cameraView.origin, vRemotePortalForward )`
/// — the **real** camera's origin against the **exit** portal's normal, which
/// are two different rooms and not a meaningful distance. The quantity that
/// matters is the *virtual* eye's distance from the plane, and that is what
/// this computes. Valve's spelling only ever ran on hardware without user clip
/// planes (`UseFastClipping()` is false on PC), where it was close enough often
/// enough; here the shear is the only path there is, so the guard fires every
/// time the player is within about two units of a portal — which is every time
/// they walk through one.
fn exit_clip_plane(pair: &PortalPair, virtual_eye: Vec3) -> Vec4 {
    let normal = pair.exit_forward;
    let mut dist = normal.dot(pair.exit_origin) - CLIP_PLANE_SETBACK;

    let camera_dist = normal.dot(virtual_eye) - dist;
    if camera_dist > -CAMERA_DIST_EPSILON {
        dist += camera_dist + CAMERA_DIST_EPSILON;
    }
    normal.extend(-dist)
}

/// `projection` sheared so that its near plane is `plane_world`.
///
/// `ApplyClipPlaneToProjectionMatrix` (`shaderapidx8.cpp:6889`), which is
/// Lengyel's oblique near-plane method and which Valve reaches through
/// `mat_alternatefastclipalgorithm`, default 1. **Not transcribed**: the
/// original is D3D's row-major layout with vectors on the left and indexes the
/// *column* that produces clip `z`, so a literal port would be transposed
/// twice. It is derived instead, which also makes the `1/far` fall out as a
/// check.
///
/// WGSL has no clip distance, so this is the only way to cut a half-space out
/// of a view. It is exact — the hardware clips against the near plane — where a
/// fragment-shader discard would leave the depth buffer written.
///
/// # How it works
///
/// Clip `z` comes from row 2 of the projection, so "the near plane is `C`" is
/// simply "row 2 **is** `C`", in view space. The only freedom left is the
/// scale, and it decides where the far plane lands: with
/// `q` the far-plane corner diagonally opposite the clip plane, scaling `C` so
/// that `row2·q = 1` puts that corner exactly at depth 1 and keeps as much of
/// the depth range as the shear allows.
pub fn oblique_near_plane(projection: Mat4, view: Mat4, plane_world: Vec4) -> Mat4 {
    // A plane transforms by the inverse transpose, which for the *rigid* view
    // matrix is easier written as "rotate the normal, move a point on it".
    let normal_world = plane_world.truncate();
    let dist_world = -plane_world.w;
    let normal = (view * normal_world.extend(0.0)).truncate();
    let point = view.transform_point3(normal_world * dist_world);
    let plane = normal.extend(-normal.dot(point));

    // A projection with no extent along `z` — the orthographic screen camera,
    // or an identity left over from a test — has nothing to shear, and the
    // division below would be by zero.
    let r = projection.z_axis.z;
    let r_near = projection.w_axis.z;
    if r.abs() < 1e-9 || r_near.abs() < 1e-9 {
        return projection;
    }

    let sgn = |x: f32| {
        if x > 0.0 {
            1.0
        } else if x < 0.0 {
            -1.0
        } else {
            0.0
        }
    };
    // The far-plane corner opposite the clip plane, in view space, homogeneous.
    // For a symmetric perspective the last component works out to `1/far`.
    let q = Vec4::new(
        (sgn(plane.x) + projection.z_axis.x) / projection.x_axis.x,
        (sgn(plane.y) + projection.z_axis.y) / projection.y_axis.y,
        -1.0,
        (1.0 + r) / r_near,
    );
    let denominator = plane.dot(q);
    if denominator.abs() < 1e-9 {
        return projection;
    }
    let scaled = plane / denominator;

    let row = |i: usize| projection.row(i);
    rows_to_mat4([row(0), row(1), scaled, row(3)])
}

/// Four rows back into a column-major `Mat4`.
fn rows_to_mat4(rows: [Vec4; 4]) -> Mat4 {
    Mat4::from_cols(
        Vec4::new(rows[0].x, rows[1].x, rows[2].x, rows[3].x),
        Vec4::new(rows[0].y, rows[1].y, rows[2].y, rows[3].y),
        Vec4::new(rows[0].z, rows[1].z, rows[2].z, rows[3].z),
        Vec4::new(rows[0].w, rows[1].w, rows[2].w, rows[3].w),
    )
}

// ---------------------------------------------------------------------------
// The near-plane cap
// ---------------------------------------------------------------------------

/// `CLIP_NEARPLANE_OFFSET` (`portalrenderable_flatbasic.cpp:1389`) — *"push
/// near plane back a bit so that the near plane cap overlaps the clip seam of
/// the portal quad on the actual near plane"*.
const CLIP_NEARPLANE_OFFSET: f32 = 0.3;

/// `PROJECT_NEARPLANE_OFFSET` (`:1390`). The cap is drawn just behind the near
/// plane rather than exactly on it.
const PROJECT_NEARPLANE_OFFSET: f32 = 0.01;

/// `PORTAL_DISTANCE_EPSILON` (`:1391`). Below this the portal plane
/// effectively passes through the eye and the reprojection stops meaning
/// anything, so the cap becomes the whole near plane cut by the portal's plane.
const PORTAL_DISTANCE_EPSILON: f32 = 0.4;

/// How much slack the side planes are given, so that a vertex lying on one does
/// not reproject to outside the viewport (`:1443`).
const SIDE_PLANE_SLACK: f32 = 0.01;

/// The polygon that covers whatever the near plane cut off the portal's quad.
///
/// `Internal_DrawRenderFixMesh` (`portalrenderable_flatbasic.cpp:1360`) and the
/// near-cap half of `CreateMeshForPortals` (`:1056-1160`), which are the same
/// routine written twice and agree. `portdocs/PORTAL_RENDER.md` §5.
///
/// Without it, a portal the player is walking into develops a hard straight
/// edge across the opening — the near plane's — and the wall shows through
/// beyond it, at exactly the moment the effect matters.
///
/// **Level 0 only.** A portal seen through another portal is at least a room
/// away, and the reference bails on the recursion level before anything else.
///
/// Returns `None` when there is nothing to cover.
pub fn near_plane_cap(pair: &PortalPair, camera: &Camera) -> Option<Vec<Vec3>> {
    let eye = camera.eye;
    let to_eye = eye - pair.origin;

    // `if( vPortalCenterToCamera.Dot( m_vForward ) < -1.0f ) return;` — the
    // camera is a unit or more *behind* the portal, so there is no front face
    // to cap.
    if to_eye.dot(pair.forward) < -1.0 {
        return None;
    }
    // `if( vPortalCenterToCamera.LengthSqr() < (m_fHalfHeight * m_fHalfHeight) )`.
    // Further away than the portal is tall and the quad cannot reach the near
    // plane.
    if to_eye.length_squared() >= pair.half_height * pair.half_height {
        return None;
    }

    let frustum = Frustum::new(camera.view_proj());
    let planes = frustum.planes();
    let near = planes[FRUSTUM_NEARZ];

    let offset_origin = pair.origin + pair.forward * PORTAL_OFFSET;
    let mut polygon: Vec<Vec3>;

    if point_to_portal_distance(eye, pair, offset_origin) < PORTAL_DISTANCE_EPSILON {
        // Too close for the reprojection to mean anything: the cap is the
        // whole near plane, cut by the portal's own plane. `±1.01` rather than
        // `±1.0` because *"precision errors ... can leave a 1-pixel border
        // around the screen uncovered by the quad"*.
        let inverse = camera.view_proj().inverse();
        let corner = |x: f32, y: f32| {
            let p = inverse * Vec4::new(x, y, 0.0, 1.0);
            p.truncate() / p.w
        };
        polygon = vec![
            corner(-1.01, -1.01),
            corner(-1.01, 1.01),
            corner(1.01, 1.01),
            corner(1.01, -1.01),
        ];
        // Keep what is *behind* the portal's plane, which is the half of the
        // near plane the opening covers.
        polygon = clip_to_plane(
            &polygon,
            -pair.forward,
            -pair.forward.dot(offset_origin),
        )?;
    } else {
        let right = pair.right * pair.half_width;
        let up = pair.up * pair.half_height;
        polygon = vec![
            offset_origin - right + up,
            offset_origin + right + up,
            offset_origin + right - up,
            offset_origin - right - up,
        ];
        // **The near plane flipped**, so what survives is the part the near
        // plane would have cut away — which is precisely what has to be
        // covered. Set back by `CLIP_NEARPLANE_OFFSET` so the cap overlaps the
        // seam rather than meeting it.
        polygon = clip_to_plane(
            &polygon,
            -near.normal,
            -near.dist - CLIP_NEARPLANE_OFFSET,
        )?;
        for side in &planes[0..4] {
            polygon = clip_to_plane(&polygon, side.normal, side.dist - SIDE_PLANE_SLACK)?;
        }
    }

    if polygon.len() < 3 {
        return None;
    }

    // `ProjectPortalPolyToPlane` (`portal_dynamicmeshrenderingutils.cpp:97`):
    // push every vertex along the ray from the eye until it lands on the near
    // plane. That is what makes the cap cover the right *pixels* — it is the
    // clipped-away part of the quad, seen from the same point, moved to where
    // it can be drawn.
    let plane_dist = near.dist - PROJECT_NEARPLANE_OFFSET;
    for vertex in &mut polygon {
        let direction = *vertex - eye;
        let denominator = direction.dot(near.normal);
        if denominator.abs() < 1e-6 {
            return None;
        }
        let t = (plane_dist - eye.dot(near.normal)) / denominator;
        *vertex = eye + direction * t;
    }
    Some(polygon)
}

/// `ComputePointToPortalDistance` (`portalrenderable_flatbasic.cpp:912`):
/// distance from a point to the portal's bounding **rectangle**, not to its
/// plane.
///
/// The difference is the whole of what the test at the call site is for: a
/// player standing a foot to the side of a portal is nowhere near it even
/// though their distance to its plane is zero.
fn point_to_portal_distance(point: Vec3, pair: &PortalPair, origin: Vec3) -> f32 {
    let local = point - origin;
    let across = local.dot(pair.right).clamp(-pair.half_width, pair.half_width);
    let up = local.dot(pair.up).clamp(-pair.half_height, pair.half_height);
    (local - pair.right * across - pair.up * up).length()
}

/// `ClipPortalPolyToPlane` (`portal_dynamicmeshrenderingutils.cpp:123`): the
/// part of a convex polygon on the `normal · p >= dist` side.
///
/// `None` when nothing survives, which is the reference's `count[0] == 0`
/// early-out and is what lets [`near_plane_cap`]'s chain of clips use `?`.
fn clip_to_plane(polygon: &[Vec3], normal: Vec3, dist: f32) -> Option<Vec<Vec3>> {
    if polygon.len() < 3 {
        return None;
    }
    let distances: Vec<f32> = polygon.iter().map(|p| normal.dot(*p) - dist).collect();
    if distances.iter().all(|d| *d < 0.0) {
        return None;
    }

    let mut out = Vec::with_capacity(polygon.len() + 1);
    for (i, point) in polygon.iter().enumerate() {
        let next = (i + 1) % polygon.len();
        if distances[i] >= 0.0 {
            out.push(*point);
        }
        if (distances[i] >= 0.0) == (distances[next] >= 0.0) {
            continue;
        }
        let t = distances[i] / (distances[i] - distances[next]);
        out.push(*point + (polygon[next] - *point) * t);
    }
    (out.len() >= 3).then_some(out)
}

// ---------------------------------------------------------------------------
// The recursion
// ---------------------------------------------------------------------------

/// The stencil state each of the four steps asks for.
///
/// `reference` is the only part that is dynamic; everything here is baked into
/// a pipeline, which is why four steps cost four pipelines per shader rather
/// than four state changes. `portdocs/PORTAL_RENDER.md` §2.2.
fn stencil(compare: StencilFunc, pass_op: StencilOp) -> Stencil {
    Stencil {
        compare,
        pass_op,
        // `m_FailOp`/`m_ZFailOp` are `KEEP` in every one of the reference's
        // stencil states; nothing in a portal view wants to write a value for
        // a fragment that did not get through.
        fail_op: StencilOp::Keep,
        depth_fail_op: StencilOp::Keep,
        read_mask: 0xFF,
        write_mask: 0xFF,
    }
}

impl World {
    /// Draws every visible portal's view, recursively.
    ///
    /// Call **inside the opaque pass**, after [`World::draw`] and before the
    /// frame-buffer copy — which is `DrawRecursivePortalViews()`' own place in
    /// `CBaseWorldView::DrawExecute` (`viewrender.cpp:8021`), between the world
    /// and the translucent pass.
    ///
    /// `camera` is the view already being drawn and `visible` is its visible
    /// set; both are the caller's and neither is modified. The pass is left
    /// with its stencil disabled, its scissor reset and `camera` bound again,
    /// so the caller need not know this ran.
    ///
    /// Draws nothing, and costs one iteration over a list of at most four, when
    /// the map has no linked portal — which is 96 of the game's 106.
    pub fn draw_portal_views(
        &self,
        pass: &mut Pass<'_>,
        setup: &PortalViewSetup,
        camera: &Camera,
        visible: &VisibleSet,
    ) {
        if setup.max_depth == 0 {
            return;
        }
        self.draw_portal_level(
            pass,
            setup,
            camera,
            camera,
            camera.projection,
            visible,
            NdcRect::FULL,
            0,
            0,
            None,
        );
        // `m_StencilState.m_bEnable = false` at level 0 (`portalrender.cpp:1695`).
        pass.set_stencil(None, 0);
    }

    /// One recursion level. See [`World::draw_portal_views`].
    ///
    /// `cull` and `draw` are the same camera at level 0 and
    /// [`PortalCameras`]' two at every level below. `base_projection` is the
    /// *pristine* perspective — carried down rather than derived, because the
    /// shear must be applied to it afresh at every level: Valve pops the
    /// parent's clip plane before pushing its own
    /// (`portalrenderable_flatbasic.cpp:421`, over the comment *"if we look
    /// through multiple unique pairs of portals, we have to take care not to
    /// clip too much"*), so only the deepest plane is ever in effect.
    #[allow(clippy::too_many_arguments)]
    fn draw_portal_level(
        &self,
        pass: &mut Pass<'_>,
        setup: &PortalViewSetup,
        cull: &Camera,
        draw: &Camera,
        base_projection: Mat4,
        visible: &VisibleSet,
        rect: NdcRect,
        level: u8,
        parent_reference: u8,
        exit: Option<u64>,
    ) {
        if level >= setup.max_depth {
            return;
        }
        let child_reference = parent_reference + 1;

        for pair in self.portals.pairs() {
            // `pCurrentPortal == m_pRenderingViewExitPortal` — the portal this
            // view is looking *out* of. See the module docs.
            if exit == Some(pair.id) {
                continue;
            }
            // `if( m_vForward.Dot( vCameraPos ) <= m_fPlaneDist ) return false;`
            // (`:1707`) — the back of a portal is a wall.
            if pair.forward.dot(cull.eye) <= pair.forward.dot(pair.origin) {
                continue;
            }

            // The opening's screen rectangle, measured with the matrix the
            // picture is actually drawn with so that the scissor and the pixels
            // agree. `None` — a corner behind the near plane — means no
            // narrowing and no scissor for this level, which is what makes the
            // near-plane cap's job possible.
            let opening = NdcRect::of(&pair.corners, draw.view_proj());
            let sub_rect = match opening {
                Some(opening) => rect.intersect(opening),
                None => rect,
            };
            if sub_rect.is_empty() {
                continue;
            }
            // `ShouldUpdatePortalView_BasedOnView`'s frustum test (`:1717`),
            // as a box against the frustum already in hand. Conservative in
            // the safe direction: a portal straddling the near plane — the one
            // the near-plane cap exists for — passes every plane's box test and
            // is kept.
            let (mins, maxs) = pair.bounds();
            if !visible.frustum().intersects(mins, maxs) {
                continue;
            }

            let cap = (level == 0)
                .then(|| near_plane_cap(&pair, draw))
                .flatten();

            // --- step 1: mark the hole ------------------------------------
            pass.set_stencil(
                if std::env::var("DBG_NOSTENCIL").is_ok() { None } else {
                Some(stencil(StencilFunc::Equal, StencilOp::IncrementClamp)) },
                parent_reference,
            );
            pass.set_state_override(StateOverride::default());
            self.portals.draw_hole(pass, setup.curtime, pair.index);
            if let Some(cap) = &cap {
                self.draw_cap(pass, setup.curtime, &pair, cap, StateOverride::default());
            }
            // Whatever the cap asked for is this level's business and not the
            // sub-scene's: a depth override left on here would draw the far
            // room with no depth test at all.
            pass.set_state_override(StateOverride::default());

            // Everything from here to step 4 touches only the opening.
            let scissor = sub_rect.scissor(setup.viewport);
            if let Some((x, y, width, height)) = scissor {
                pass.set_scissor(x, y, width, height);
            }

            // --- step 2: reset the depth buffer inside it -----------------
            pass.set_stencil(
                Some(stencil(StencilFunc::Equal, StencilOp::Keep)),
                child_reference,
            );
            self.portals.clear_depth(pass);

            // --- step 3: the scene again, from the other side -------------
            let cameras = portal_cameras(draw, base_projection, &pair, sub_rect);
            let sub_visible = self.visible_through(&pair, &cameras.cull, setup.novis);
            pass.set_camera(&cameras.draw);
            self.draw(pass, setup.curtime, &sub_visible);
            self.draw_portal_level(
                pass,
                setup,
                &cameras.cull,
                &cameras.draw,
                base_projection,
                &sub_visible,
                sub_rect,
                level + 1,
                child_reference,
                Some(pair.partner_id),
            );
            // The recursion restored the stencil to *its* parent, which is this
            // level's child reference, and left the camera on its own.
            pass.set_camera(&cameras.draw);
            pass.set_stencil(
                Some(stencil(StencilFunc::Equal, StencilOp::Keep)),
                child_reference,
            );
            let translucent =
                self.translucent_list(cameras.cull.eye, cameras.cull.forward(), &sub_visible);
            if !translucent.is_empty() {
                self.draw_translucent(pass, setup.curtime, &translucent, &sub_visible);
            }

            // --- step 4: put the wall's depth and the stencil back --------
            pass.set_camera(draw);
            let restore = StateOverride {
                // Depth test **off** — what is in the buffer now is the far
                // room, which is in front of the value being restored, so an
                // ordinary test would reject every fragment.
                depth_test: Some(false),
                depth_write: Some(true),
                // Colour writes off, or the hole's black is painted over the
                // picture just drawn into it.
                write_color: Some(false),
                ..StateOverride::default()
            };
            pass.set_stencil(
                Some(stencil(StencilFunc::Equal, StencilOp::DecrementClamp)),
                child_reference,
            );
            pass.set_state_override(restore);
            self.portals.draw_hole(pass, setup.curtime, pair.index);
            if let Some(cap) = &cap {
                self.draw_cap(pass, setup.curtime, &pair, cap, restore);
            }
            pass.set_state_override(StateOverride::default());
            if scissor.is_some() {
                pass.set_scissor(0, 0, setup.viewport.0.max(1), setup.viewport.1.max(1));
            }
        }

        // `m_StencilState.m_nReferenceValue = iParentLevelStencilReferenceValue`
        // (`portalrender.cpp:1709`): hand the caller back the state it had, so
        // that a level which drew two portals leaves after the second exactly
        // as it arrived.
        pass.set_stencil(
            Some(stencil(StencilFunc::Equal, StencilOp::Keep)),
            parent_reference,
        );
    }

    /// The near-plane cap, as a triangle fan, under `base` plus its own two
    /// corrections.
    ///
    /// `base` is whichever of the two draws this belongs to — step 1's nothing
    /// or step 4's colour-write mask — so that the cap is always in the same
    /// state as the quad it is patching. The two corrections are:
    ///
    /// **`OverrideDepthEnable( true, true, false )`** (`:1456`): the cap sits
    /// on the near plane with the whole world behind it, so an ordinary depth
    /// test would reject nothing and an ordinary depth *write* is what is
    /// wanted.
    ///
    /// **Culling off**, which is this port's and not the reference's: the
    /// polygon's winding after reprojection depends on which corners the near
    /// plane cut away, and a back-facing cap is an invisible one. The
    /// reference builds its cap as a fan over a quad clipped in a fixed order
    /// and never has to ask.
    fn draw_cap(
        &self,
        pass: &mut Pass<'_>,
        curtime: f32,
        pair: &PortalPair,
        cap: &[Vec3],
        base: StateOverride,
    ) {
        pass.set_state_override(StateOverride {
            cull: Some(false),
            depth_test: Some(false),
            depth_write: Some(true),
            ..base
        });
        self.portals.draw_hole_cap(pass, curtime, pair.index, cap);
    }

    /// What a portal view can see, measured from the exit portal rather than
    /// from the virtual camera.
    ///
    /// The module docs say why it cannot be the camera. `pair` carries the five
    /// points `PortalMoved` computes (`portalrenderable_flatbasic.cpp:66-80`),
    /// all of them a unit in front of the exit portal's plane and so in real
    /// space rather than inside the wall.
    fn visible_through(&self, pair: &PortalPair, cull: &Camera, novis: bool) -> VisibleSet {
        let origins: Vec<Vec3> = pair
            .exit_vis_origins
            .iter()
            .copied()
            // `if( enginetrace->GetLeafContainingPoint( ... ) != -1 )`
            // (`portalrenderable_flatbasic.cpp:641`): a corner that landed
            // inside geometry contributes nothing and must not be the only
            // thing asked.
            .filter(|point| self.vis.cluster_at(*point) >= 0)
            .collect();
        self.vis.mark_view(
            &super::vis::ViewPoint {
                eye: cull.eye,
                origins: &origins,
                // `ForceViewLeaf( m_iViewLeaf )`.
                leaf: Some(pair.exit_forward_origin),
            },
            cull.view_proj(),
            novis,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::camera::rh::proj::directx;

    fn perspective() -> Mat4 {
        directx::perspective(90f32.to_radians(), 1.0, 1.0, 4096.0)
    }

    /// The identity that makes the far corner fall out as `1/far`, which is the
    /// cheapest possible check that §3.2's derivation is the same arithmetic
    /// the reference's `q.w = ( 1.0f + _33 ) / _43` is.
    #[test]
    fn the_far_corner_of_a_perspective_projection_is_one_over_far() {
        let projection = perspective();
        let q_w = (1.0 + projection.z_axis.z) / projection.w_axis.z;
        assert!(
            (q_w - 1.0 / 4096.0).abs() < 1e-7,
            "q.w is {q_w}, not 1/4096"
        );
    }

    /// The oblique shear does what its name says: the *plane* becomes the near
    /// plane.
    ///
    /// **A near plane's inward normal points away from the camera**, which is
    /// the one thing to get right here and is easy to get backwards. For an
    /// ordinary projection, `row2 · p >= 0` works out to `z <= -near` — the
    /// visible half is the far side — so a clip plane written as `(n, -d)` must
    /// have the camera at a *negative* distance from it. An exit portal's plane
    /// does: the virtual camera is behind it, inside the wall, looking out
    /// along `+forward`. See [`exit_clip_plane`].
    ///
    /// Three assertions, each catching a different way of getting it wrong. A
    /// point on the plane must land at depth exactly 0 — that is the definition
    /// of a near plane. A point between the camera and the plane must land at
    /// negative depth, which is what the hardware clips on. And a point beyond
    /// it must survive, or the shear has deleted the room it was meant to
    /// reveal.
    #[test]
    fn the_oblique_projection_makes_the_clip_plane_the_near_plane() {
        // Looking down -z from the origin, which is what an identity view
        // matrix means for a right-handed camera.
        let depth = depth_through(Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 0.0, -100.0));

        assert!(
            depth(Vec3::new(0.0, 0.0, -100.0)).abs() < 1e-4,
            "a point on the plane is at depth {}",
            depth(Vec3::new(0.0, 0.0, -100.0))
        );
        assert!(
            depth(Vec3::new(0.0, 0.0, -50.0)) < 0.0,
            "a point on the camera's side of the plane was not clipped"
        );
        let beyond = depth(Vec3::new(0.0, 0.0, -200.0));
        assert!(
            (0.0..=1.0).contains(&beyond),
            "a point beyond the plane is at depth {beyond}",
        );
    }

    /// The same three assertions against a plane that is neither axis-aligned
    /// nor centred, so that the `sgn` terms of the derivation are actually
    /// exercised — they are what pick *which* far corner the shear normalizes
    /// against, and a symmetric plane cannot tell a wrong choice from a right
    /// one.
    #[test]
    fn the_oblique_projection_handles_a_tilted_plane() {
        let on_plane = Vec3::new(0.0, 0.0, -150.0);
        let depth = depth_through(Vec3::new(0.3, -0.2, -1.0).normalize(), on_plane);

        assert!(
            depth(on_plane).abs() < 1e-3,
            "a point on the tilted plane is at depth {}",
            depth(on_plane)
        );
        assert!(
            depth(on_plane * 0.5) < 0.0,
            "a point on the camera's side of the tilted plane was not clipped"
        );
        let beyond = depth(on_plane * 1.5);
        assert!(
            (0.0..=1.0).contains(&beyond),
            "a point beyond the tilted plane is at depth {beyond}",
        );
    }

    /// A depth function for a camera at the origin looking down `-z`, sheared
    /// so that the plane through `on_plane` with inward normal `normal` is its
    /// near plane.
    fn depth_through(normal: Vec3, on_plane: Vec3) -> impl Fn(Vec3) -> f32 {
        let plane = normal.extend(-normal.dot(on_plane));
        // The camera must be *behind* the plane, or it is not a near plane and
        // the shear has nothing meaningful to do.
        assert!(plane.w < 0.0, "the test's own plane is inverted");
        let oblique = oblique_near_plane(perspective(), Mat4::IDENTITY, plane);
        move |point: Vec3| {
            let clip = oblique * point.extend(1.0);
            clip.z / clip.w
        }
    }

    /// A projection with nothing to shear is returned unchanged rather than
    /// dividing by zero — the orthographic screen camera is one, and so is an
    /// identity left over from a test.
    #[test]
    fn a_degenerate_projection_is_left_alone() {
        let plane = Vec4::new(0.0, 0.0, 1.0, 100.0);
        assert_eq!(
            oblique_near_plane(Mat4::IDENTITY, Mat4::IDENTITY, plane),
            Mat4::IDENTITY
        );
    }

    /// Narrowing a projection to a sub-rectangle of its own image is the same
    /// as remapping that rectangle onto the whole screen: the rectangle's
    /// corners must come out at the corners.
    #[test]
    fn narrowing_a_projection_remaps_the_rectangle_onto_the_screen() {
        let projection = perspective();
        let rect = NdcRect {
            left: -0.5,
            right: 0.25,
            bottom: -0.75,
            top: 0.5,
        };
        let narrowed = rect.narrow(projection);

        // A point that projects to the rectangle's left edge must project to
        // -1 under the narrowed matrix. Pick it by projecting backwards: at
        // view depth -10, ndc x of `left` is at view x = left * 10 * tan(45).
        let at = |x: f32, y: f32| {
            let point = Vec3::new(x * 10.0, y * 10.0, -10.0);
            let clip = narrowed * point.extend(1.0);
            (clip.x / clip.w, clip.y / clip.w)
        };
        let (x, y) = at(rect.left, rect.bottom);
        assert!((x + 1.0).abs() < 1e-4 && (y + 1.0).abs() < 1e-4, "{x} {y}");
        let (x, y) = at(rect.right, rect.top);
        assert!((x - 1.0).abs() < 1e-4 && (y - 1.0).abs() < 1e-4, "{x} {y}");
    }

    /// The scissor's `y` flips and the rectangle is inclusive of the pixels it
    /// covers — an opening that rounds away to nothing gives `None` rather than
    /// a zero-sized rectangle, which `wgpu` rejects.
    #[test]
    fn the_scissor_flips_y_and_refuses_an_empty_rectangle() {
        let full = NdcRect::FULL.scissor((800, 600)).expect("the whole screen");
        assert_eq!(full, (0, 0, 800, 600));

        // The top-left quarter in NDC is x in -1..0, y in 0..1.
        let quarter = NdcRect {
            left: -1.0,
            right: 0.0,
            bottom: 0.0,
            top: 1.0,
        };
        assert_eq!(quarter.scissor((800, 600)), Some((0, 0, 400, 300)));

        let sliver = NdcRect {
            left: 0.0,
            right: 0.0,
            bottom: 0.0,
            top: 0.0,
        };
        assert_eq!(sliver.scissor((800, 600)), None);
    }

    /// Intersection, which is what carries a narrowing down the recursion, and
    /// the emptiness it can produce.
    #[test]
    fn rectangles_intersect_down_the_recursion() {
        let a = NdcRect {
            left: -0.5,
            right: 0.5,
            bottom: -0.5,
            top: 0.5,
        };
        let b = NdcRect {
            left: 0.0,
            right: 1.0,
            bottom: -1.0,
            top: 0.0,
        };
        let c = a.intersect(b);
        assert_eq!(c.left, 0.0);
        assert_eq!(c.right, 0.5);
        assert_eq!(c.bottom, -0.5);
        assert_eq!(c.top, 0.0);
        assert!(!c.is_empty());
        assert!(c.intersect(NdcRect {
            left: 0.9,
            right: 1.0,
            bottom: -1.0,
            top: 1.0
        })
        .is_empty());
    }

    /// The convex clip, against the case the near-plane cap actually uses: a
    /// square cut in half.
    #[test]
    fn clipping_a_square_in_half_leaves_four_corners() {
        let square = [
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(-1.0, 0.0, 1.0),
        ];
        let half = clip_to_plane(&square, Vec3::X, 0.0).expect("half a square");
        assert_eq!(half.len(), 4);
        assert!(half.iter().all(|p| p.x >= -1e-6), "{half:?}");

        // Entirely on the wrong side: nothing survives, which is the
        // early-out the cap's `?` chain depends on.
        assert!(clip_to_plane(&square, Vec3::X, 2.0).is_none());
        // Entirely on the right side: unchanged.
        let all = clip_to_plane(&square, Vec3::X, -2.0).expect("all of it");
        assert_eq!(all.len(), 4);
    }
}

#[cfg(test)]
mod rendered {
    use super::*;
    use crate::engine::trace::{Contents, Ray};
    use crate::materials::context::{Load, RenderContext};
    use crate::materials::target::RenderTarget;
    use crate::materials::MaterialCache;
    use crate::server::classes::portal::teleport_matrix;
    use crate::engine::world::portals::Portal;

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

    /// **The view through a portal, drawn — a different room inside the oval
    /// and nothing changed outside it.**
    ///
    /// The one test that can say this module works, because every step of it
    /// is invisible on its own and most of the ways it can be wrong still
    /// draw *something*. The stencil could never be written, in which case
    /// step 3 draws the far room over the whole screen; it could be written
    /// and never tested, same picture; the depth reset could be missing, in
    /// which case the far room is rejected by the wall's own depth and the
    /// oval stays solid; the virtual camera could be built with the teleport
    /// matrix the wrong way round, in which case the picture inside the hole
    /// is the room you are already standing in; and the scissor could be off
    /// by a sign, in which case the picture lands in the wrong corner.
    ///
    /// So the assertions are, in order: the hole is **not empty** (something
    /// was drawn), it is **confined** (every changed pixel is inside the
    /// opening's own screen rectangle, which is what the stencil and the
    /// scissor are for together), and it is **not the near room** (the picture
    /// differs from what the same camera draws with the recursion switched
    /// off, which is the wall).
    ///
    /// `sp_a1_intro1` has no `prop_portal` of its own, so the pair is placed
    /// the way the `portal` console command places one: two traces out of the
    /// player's eye, at right angles to each other, against
    /// `MASK_SHOT_PORTAL`.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_view_through_a_portal -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install and a GPU; set KISAK_GAME_DIR"]
    fn the_view_through_a_portal_is_another_room_and_stays_inside_the_oval() {
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
        let map = std::env::var("KISAK_MAP").unwrap_or_else(|_| "sp_a1_intro1".to_owned());
        let mut world = World::load(&vfs, &mut materials, &device, &map).expect("the map loads");

        // **Both of the recursive view's own materials must be real.** A
        // fallback to the error checkerboard would still produce a picture,
        // and every assertion below would still pass on it.
        assert_eq!(
            world.portals.hole_material().shader,
            crate::materials::shader::ShaderKind::PortalRefractHole,
            "the stencil hole material fell back to {}",
            world.portals.hole_material().name
        );
        assert_eq!(
            world.portals.clear_material().shader,
            crate::materials::shader::ShaderKind::BufferClearObeyStencil,
            "the depth-reset material fell back to {}",
            world.portals.clear_material().name
        );

        let spawn = world.spawn.expect("the map has an info_player_start");
        // `VEC_VIEW.z` — where a standing player's eye is.
        let eye = spawn.origin + Vec3::new(0.0, 0.0, 64.0);

        // **Both portals stand in open air, at one point, ninety degrees
        // apart** — not on a wall, and that is worth explaining because the
        // obvious thing to do here is to trace the way the `portal` console
        // command does.
        //
        // Tracing was the first version of this test and it failed, for a
        // reason that is the test's and not the engine's:
        // [`World::collision`](super::World::collision) is the *world model*,
        // so a trace out of `sp_a1_intro1`'s spawn goes straight through the
        // container the player wakes up in and lands on a wall 354 units away
        // that the container is drawn in front of. The portal was then behind
        // an opaque surface, the depth test rejected the hole, and every
        // assertion below read as "nothing drew".
        //
        // Drawing does not need a wall — the hole is four vertices in world
        // space like any other quad — so the placement that has no such
        // failure mode is the one to use. The two are co-located so that both
        // are in the same known-open cluster; the ninety degrees between them
        // is what makes the far picture a different picture.
        let forward = crate::math::angle_matrix(Vec3::new(0.0, spawn.yaw, 0.0)) * Vec3::X;
        let origin = eye + forward * 96.0;
        // Blue faces the eye, so the camera looks into its front.
        let blue_angles = Vec3::new(0.0, spawn.yaw + 180.0, 0.0);
        // **Sixty degrees and not ninety**, and the sign of that difference is
        // the whole of what makes the *second* recursion level draw. The
        // virtual eye lands at `origin - 96 * exit_forward`, so it is on blue's
        // front side exactly when the turn is under a right angle; at ninety
        // it is in blue's own plane, the facing test at the top of
        // [`World::draw_portal_level`] rejects it, and level 2 is empty —
        // correctly, and uninformatively.
        let orange_angles = Vec3::new(0.0, spawn.yaw - 60.0, 0.0);
        let (blue_origin, orange_origin) = (origin, origin);
        println!("  eye    {eye:?}");
        println!("  blue   {blue_origin:?} angles {blue_angles:?}");
        println!("  orange {orange_origin:?} angles {orange_angles:?}");

        let pair = |id: u64, origin: Vec3, angles: Vec3, is_portal2: bool, to: Mat4| Portal {
            id,
            origin,
            angles,
            half_width: 32.0,
            half_height: 54.0,
            is_portal2,
            // Wide open: the hole's cutout is a function of this, and a portal
            // still opening punches a smaller hole than its own oval.
            open_for: 10.0,
            linked: Some(1 - id),
            matrix: to,
        };
        let blue_to_orange = teleport_matrix((blue_origin, blue_angles), (orange_origin, orange_angles));
        let orange_to_blue = teleport_matrix((orange_origin, orange_angles), (blue_origin, blue_angles));
        world.sync_portals(&[
            pair(0, blue_origin, blue_angles, false, blue_to_orange),
            pair(1, orange_origin, orange_angles, true, orange_to_blue),
        ]);
        assert_eq!(world.portals.pairs().len(), 2, "the two portals are linked");

        // Straight at the blue portal from where the player stands.
        let camera = Camera::perspective(
            eye,
            glam::camera::rh::view::look_at_mat4(eye, blue_origin, Vec3::Z),
            75.0,
            1.0,
            1.0,
            4096.0,
        );
        let visible = world.visible(eye, camera.view_proj(), false);

        // **The sub-view can see something.** Checked before any pixel,
        // because an empty visible set is the one failure that would draw a
        // correct-looking black hole: the virtual eye is inside solid
        // geometry, so this is the assertion that `ViewPoint`'s five exit
        // origins and its forced view leaf are doing their job.
        let pairs = world.portals.pairs();
        let blue = pairs.iter().find(|p| p.id == 0).expect("the blue pair");
        let cameras = portal_cameras(&camera, camera.projection, blue, NdcRect::FULL);
        let through = world.visible_through(blue, &cameras.cull, false);
        println!(
            "  through the blue portal: cluster {}, {} face(s) in {} leaf/leaves",
            through.stats.cluster, through.stats.faces, through.stats.leaves
        );
        assert!(
            through.stats.faces > 0,
            "the far side of the portal sees no world face at all — \
             the virtual eye's PVS row is empty"
        );

        let mut context = RenderContext::new(&device, &queue, materials.pipelines());
        let target = RenderTarget::new(
            &device,
            "portalview",
            SIZE,
            SIZE,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            true,
        );

        let shot = |context: &mut RenderContext,
                        materials: &mut MaterialCache,
                        clear: wgpu::Color,
                        camera: &Camera,
                        record: &dyn Fn(&mut Pass<'_>)|
         -> Vec<u8> {
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
                    camera,
                    Load::Clear(clear),
                );
                record(&mut pass);
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

        // **The stencil hole's own geometry is on screen**, checked before
        // anything that depends on it. Drawn alone, against a black clear,
        // with no stencil and no world in front of it: if this is empty then
        // the opening is empty for a reason that has nothing to do with the
        // recursion — the quad is back-face culled, or the `$Stage 1` alpha
        // test discarded every fragment.
        //
        // Against **white**, because what `$Stage 1` writes is opaque black:
        // against the black clear every other shot uses it would be invisible
        // whether it drew or not.
        let hole_only = shot(&mut context, &mut materials, wgpu::Color::WHITE, &camera, &|pass| {
            world.portals.draw_hole(pass, 10.0, blue.index);
        });
        let drawn = hole_only
            .chunks_exact(4)
            .filter(|p| p[0..3] == [0, 0, 0])
            .count();
        println!("  the hole alone covers {drawn} pixel(s)");
        assert!(
            drawn > 200,
            "the stencil hole covers {drawn} pixel(s) on its own —              it is culled, discarded, or off screen"
        );

        let frame = |max_depth: u8| {
            let (world, camera, visible) = (&world, &camera, &visible);
            move |pass: &mut Pass<'_>| {
                world.draw(pass, 10.0, visible);
                world.draw_portal_views(
                    pass,
                    &PortalViewSetup {
                        curtime: 10.0,
                        max_depth,
                        viewport: (SIZE, SIZE),
                        novis: false,
                    },
                    camera,
                    visible,
                );
            }
        };
        let off = shot(&mut context, &mut materials, wgpu::Color::BLACK, &camera, &frame(0));
        let one = shot(&mut context, &mut materials, wgpu::Color::BLACK, &camera, &frame(1));
        let two = shot(&mut context, &mut materials, wgpu::Color::BLACK, &camera, &frame(2));

        // Where the opening is on screen, in pixels. The same call the
        // recursion makes, so a disagreement here would be a disagreement with
        // the scissor it actually set.
        let opening = NdcRect::of(&blue.corners, camera.view_proj())
            .expect("the portal is wholly in front of the near plane");
        let (sx, sy, sw, sh) = opening
            .scissor((SIZE, SIZE))
            .expect("the opening covers at least one pixel");
        println!("  opening: {sw}x{sh} at ({sx}, {sy})");

        let changed = |a: &[u8], b: &[u8]| -> (usize, usize) {
            let (mut inside, mut outside) = (0, 0);
            for (i, (p, q)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
                if p == q {
                    continue;
                }
                let (x, y) = ((i as u32) % SIZE, (i as u32) / SIZE);
                match x >= sx && x < sx + sw && y >= sy && y < sy + sh {
                    true => inside += 1,
                    false => outside += 1,
                }
            }
            (inside, outside)
        };

        let (inside, outside) = changed(&off, &one);
        println!("  depth 0 -> 1: {inside} pixel(s) changed inside the opening, {outside} outside");
        assert!(
            inside > 200,
            "the view through the portal changed only {inside} pixel(s); \
             the hole is empty"
        );
        assert_eq!(
            outside, 0,
            "{outside} pixel(s) outside the opening changed — \
             the stencil or the scissor is not confining the sub-view"
        );

        // **The second level draws too**, which is the only thing that
        // separates a recursion from a single sub-view. It is guaranteed by
        // the sixty degrees chosen above and by nothing else — see the
        // placement comment.
        let (inside2, outside2) = changed(&one, &two);
        println!("  depth 1 -> 2: {inside2} pixel(s) changed inside the opening, {outside2} outside");
        assert!(
            inside2 > 200,
            "the second recursion level changed only {inside2} pixel(s); \
             `r_portal_stencil_depth` is doing nothing past the first"
        );
        assert_eq!(outside2, 0, "the second recursion level escaped the opening");

        // **And a portal mounted on a wall still marks the stencil**, which
        // the placement above deliberately does not test and which is the case
        // every portal in the game is. The hole is coplanar with the surface
        // behind it and beats it only on
        // [`DepthBias::Decal`](crate::materials::pipeline::DepthBias); if that
        // bias were ever dropped or given the wrong sign, everything above
        // would still pass and no portal in a real level would draw.
        //
        // **Measured through the stencil and not in colour**, because `$Stage
        // 1` writes opaque black and a wall in an unlit corner is black
        // already: the hole marks the stencil, the *oval* is then drawn where
        // the mark is, and the oval is the one thing here that is brightly
        // coloured. No mark, no oval, no difference from the bare frame.
        let ray = Ray::line(eye, eye + forward * 4096.0);
        let hit = world
            .collision
            .tracer()
            .trace(&ray, Contents::MASK_SHOT_PORTAL);
        assert!(hit.did_hit(), "nothing in front of the spawn to trace");
        let wall_angles = crate::math::vector_angles(hit.normal, Vec3::Z);
        world.sync_portals(&[pair(0, hit.end, wall_angles, false, Mat4::IDENTITY)]);
        // Five feet off the wall, so that the wall is the nearest surface: the
        // trace is against the *world model*, and `sp_a1_intro1`'s spawn is
        // inside a container that is drawn in front of everything it hits.
        let close = hit.end + hit.normal * 64.0;
        let wall_camera = Camera::perspective(
            close,
            glam::camera::rh::view::look_at_mat4(close, hit.end, Vec3::Z),
            75.0,
            1.0,
            1.0,
            4096.0,
        );
        let wall_visible = world.visible(close, wall_camera.view_proj(), false);
        let bare = shot(
            &mut context,
            &mut materials,
            wgpu::Color::BLACK,
            &wall_camera,
            &|pass| world.draw(pass, 10.0, &wall_visible),
        );
        let marked = shot(
            &mut context,
            &mut materials,
            wgpu::Color::BLACK,
            &wall_camera,
            &|pass| {
                world.draw(pass, 10.0, &wall_visible);
                pass.set_stencil(
                    Some(stencil(StencilFunc::Equal, StencilOp::IncrementClamp)),
                    0,
                );
                world.portals.draw_hole(pass, 10.0, 0);
                pass.set_stencil(Some(stencil(StencilFunc::Equal, StencilOp::Keep)), 1);
                world.portals.draw_one(pass, 10.0, 0);
                pass.set_stencil(None, 0);
            },
        );
        let survived = marked
            .chunks_exact(4)
            .zip(bare.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        println!("  on a wall: {survived} pixel(s) inside the stencil mark");
        assert!(
            survived > 2000,
            "the hole marked {survived} pixel(s) of stencil on the wall it sits on — \
             the decal depth bias is not winning the depth test"
        );
    }
}
