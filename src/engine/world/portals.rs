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
//! `portdocs/PORTAL.md` takes. The other two are the see-through warp and the
//! stencil punch, and both belong to the recursive view.
//!
//! # What you see, and what you do not
//!
//! **A coloured oval on an unbroken wall.** No hole, no view through, no
//! reflection of the room on the other side. That is the deliberate output of
//! `portdocs/PORTAL.md`'s scope and is worth saying out loud before anyone
//! reports it as a bug.
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
    /// An opaque, stable key from whoever made the list. Not read here yet:
    /// unlike a model instance a portal owns no uploaded geometry, so there is
    /// nothing to match a sync against — the list is simply rebuilt. It is
    /// carried because the seam on the other side is keyed and a one-way
    /// asymmetry is a thing to explain twice.
    #[allow(dead_code)]
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
    /// The [`id`](Portal::id) of this portal's partner, if it has one.
    ///
    /// **Nothing in the draw reads it.** It is here because this list is also
    /// what [`World::sync_portals`](super::World::sync_portals) carves from,
    /// and the far side of a portal is half of what the carve has to know —
    /// see `crate::engine::trace::PortalLink`.
    #[allow(dead_code)]
    pub linked: Option<u64>,
    /// `m_matrixThisToLinked`, the identity while unlinked. Likewise the
    /// carve's rather than the draw's.
    #[allow(dead_code)]
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
            live: Vec::new(),
        }
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

    /// Records one portal's quad.
    ///
    /// `curtime` is the scene clock, and it reaches the shader twice: once as
    /// the noise scroll and once, through
    /// [`Portal::open_for`], as the two curves.
    pub fn draw_one(&self, pass: &mut Pass<'_>, curtime: f32, index: usize) {
        let Some(portal) = self.live.get(index) else {
            return;
        };

        // `C_Prop_Portal::ClientThink` (`c_prop_portal.cpp:222`), integrated
        // in closed form. Both are clamped at both ends because `open_for` is
        // an elapsed time and a level that has just restarted can hand over a
        // negative one.
        let open_amount = (portal.open_for * OPEN_RATE).clamp(0.0, 1.0);
        let static_amount = (1.0 - portal.open_for * STATIC_RATE).clamp(0.0, 1.0);
        pass.set_portal_overlay(&PortalOverlay {
            open_amount,
            // **`g_flPortalActive` is `1 - $PortalStatic`**
            // (`portal_refract_helper.cpp:216`), and passing the static amount
            // straight through inverts the whole effect.
            portal_active: 1.0 - static_amount,
            time: curtime - (curtime / TIME_WRAP).floor() * TIME_WRAP,
            _padding: 0.0,
        });

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
    let rotation = angle_matrix(portal.angles);
    let right = -(rotation * Vec3::Y) * portal.half_width;
    let up = (rotation * Vec3::Z) * portal.half_height;
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

    fn portal(angles: Vec3) -> Portal {
        Portal {
            id: 1,
            origin: Vec3::new(10.0, 20.0, 30.0),
            angles,
            half_width: 32.0,
            half_height: 56.0,
            is_portal2: false,
            open_for: 1.0,
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
                    portals.draw_one(&mut pass, curtime, index);
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
                linked: None,
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
