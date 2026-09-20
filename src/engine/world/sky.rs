//! The sky: six quads around the camera, and the room they are drawn in.
//!
//! `engine/gl_warp.cpp` (the box) and `CSkyboxView` (`game/client/viewrender.cpp:698`,
//! the room). `portdocs/ENGINE_WORLD_SKY.md` is the design.
//!
//! # Two things share the name, and they nest
//!
//! The **2D skybox** is [`Sky`]: six quads of `skybox/<skyname><rt|bk|lf|ft|up|dn>`
//! drawn around the camera at the far plane, so that whichever way you look
//! there is a picture behind everything. The **3D skybox** is [`Sky3d`]: a
//! second room, built at 1/16 scale, sitting somewhere else inside the *same*
//! `.bsp` and sealed off from the playable map, drawn from a second camera
//! before the real map is. The second draws the first inside itself, and then
//! the frame's depth buffer is cleared and the real map is drawn on top.
//!
//! A map's sky *surfaces* are never drawn at all — `R_DrawSurface`
//! (`gl_rsurf.cpp:3760`) turns a `SURFDRAW_SKY` face into `m_bSkyVisible =
//! true` and emits no geometry — so they are **holes** through which the first
//! picture shows. That is why [`bsp::surf::NOT_DRAWN`](super::bsp::surf::NOT_DRAWN)
//! contains the two sky bits, and why a sky face costs nothing but the hole.
//!
//! The division by [`Sky3d::scale`] is what makes the parallax right: walking
//! 16 units in the map moves the sky camera 1 unit, so a skybox building 100
//! units from the sky camera behaves like one 1,600 units away.
//!
//! # Things here that produce a plausible wrong picture rather than an error
//!
//! **Portal 2's sky materials are `UnlitGeneric`, not `Sky`.** All 24 `.vmt`s
//! the game can actually load name it, and the six that name `sky` are a Left
//! 4 Dead import no map's `skyname` selects — so `Sky_HDR_DX9` is not ported
//! and there is no HDR decode here. A sky material that turned up wanting one
//! would draw its compressed texture as if it were colour.
//!
//! **A sky material may have no texture at all.** `sky_fog*` is
//! `UnlitGeneric { $color "{70 85 100}" }` and nothing else — five maps' whole
//! sky is that one colour. `Material::new` binds the white texture for an
//! unset parameter, which is what makes it work; a fallback that bound the
//! checkerboard instead would put a magenta grid across five skies.
//!
//! **The box is drawn with culling off**, deliberately —
//! `portdocs/ENGINE_WORLD_SKY.md` §4.5. `MakeSkyVec`'s `st_to_vec` maps
//! `(s, t, width)` onto a different signed axis triple per face, and carrying
//! that through this port's front-face convention (`rustdocs/ENGINE.md` gotcha
//! #1) is the kind of two-sign-convention winding argument that has already
//! been got wrong once here. The box is a cube centred on the eye: every face
//! is seen from the inside, from one side, and there are six of them.
//!
//! **The box is *just* inside the far plane, not at it.** `SQRT3INV` is
//! 0.57735 where `1/√3` is 0.5773503, so a corner lands at `z_far × 0.999995`
//! and survives the depth test against a cleared buffer. Rounding that
//! constant up clips the corners of the sky.

use std::sync::Arc;

use glam::{Mat4, Vec3};

use crate::filesystem::Vfs;
use crate::materials::context::{Pass, StateOverride};
use crate::materials::material::{Material, MaterialCache};
use crate::materials::mesh::SimpleVertex;

/// `CSkyboxView::DrawInternal`'s near plane (`viewrender.cpp:6800`), over
/// Valve's own warning:
///
/// > if you can get really close to the skybox geometry it's possible that
/// > you'll be able to clip into it with this near plane. If so, move it in a
/// > bit. It's at 2.0 to give us more precision. That means you need to keep
/// > the eye position at least 2 * scale away from the geometry in the skybox
pub const SKY_ZNEAR: f32 = 2.0;

/// `MAX_TRACE_LENGTH` (`public/worldsize.h:32`) — `1.732050807569 ×
/// COORD_EXTENT`, where `COORD_EXTENT` is `2 × MAX_COORD_INTEGER` = 32,768.
///
/// The diagonal of the biggest legal map, and the 3D skybox view's far plane.
/// Exactly twice the main view's, which is `r_mapextents × √3`.
pub const SKY_ZFAR: f32 = 1.732_050_8 * 2.0 * 16384.0;

/// *"a little less than 1 / sqrt(3)"* (`gl_warp.cpp:25`). See the module docs.
const SQRT3INV: f32 = 0.57735;

/// `skyboxsuffix` (`gl_warp.cpp:107`), in the order the six materials are
/// loaded and stored.
const SUFFIXES: [&str; 6] = ["rt", "bk", "lf", "ft", "up", "dn"];

/// `skytexorder` (`gl_warp.cpp:48`): which *material* face `i` of the box
/// binds.
///
/// Not the identity — faces 1 and 2 are swapped — because the geometry loop's
/// axis order and the filename suffixes disagree about back and left. Getting
/// it wrong swaps two walls of the sky, which on a symmetric sky is invisible
/// and on `sky_l4d_c4m1` would be obvious.
const TEX_ORDER: [usize; 6] = [0, 2, 1, 3, 4, 5];

/// `st_to_vec` (`gl_warp.cpp:34`): `1 = s, 2 = t, 3 = width`, negated by sign.
const ST_TO_VEC: [[i32; 3]; 6] = [
    [3, -1, 2],
    [-3, 1, 2],
    [1, 3, 2],
    [-1, -3, 2],
    [-2, -1, 3], // 0 degrees yaw, look straight up
    [2, -1, -3], // look straight down
];

/// The four corners of one face, in `MakeSkyVec`'s order — `R_DrawSkyBox`'s
/// four calls, which run `(-1,-1), (-1,1), (1,1), (1,-1)`.
const CORNERS: [(f32, f32); 4] = [(-1.0, -1.0), (-1.0, 1.0), (1.0, 1.0), (1.0, -1.0)];

/// Two triangles over [`CORNERS`], which is `MATERIAL_QUADS`' own fan.
const QUAD_INDICES: [u16; 6] = [0, 1, 2, 0, 2, 3];

/// The six materials a map's `skyname` resolves to.
///
/// Built once per map by [`load`](Sky::load) and owned by
/// [`World`](super::World) for its lifetime, because it is derived from that
/// map's `worldspawn` and dies with it. `R_UnloadSkys`' reference counting is
/// `Arc`.
pub struct Sky {
    /// `worldspawn`'s `skyname`, for the log and for
    /// [`summary`](Sky::summary).
    name: Option<String>,
    /// `skyboxMaterials`, in [`SUFFIXES`] order — or `None` when the map named
    /// no sky, or when one of the six did not resolve.
    ///
    /// **All six or none**, which is `R_LoadNamedSkys`' own rule
    /// (`gl_warp.cpp:104`): any error material and the whole set is refused.
    /// Here there is no previous sky to keep, so the answer is no sky — and no
    /// sky draws nothing, where five sixths of a sky would draw a checkerboard
    /// on one wall of the world.
    ///
    /// Valve `break`s at the first failure and this asks for all six, which
    /// costs five more missing-file lookups on the one `skyname` in the game
    /// that has none. It buys a stable report: a sky that is missing *one*
    /// face reads the same in the log as one that is missing all of them,
    /// which is the sort of difference worth not having.
    faces: Option<[Arc<Material>; 6]>,
}

impl Sky {
    /// An empty sky. What a map with no `skyname` gets, and what a test
    /// fixture gets.
    pub fn none() -> Sky {
        Sky {
            name: None,
            faces: None,
        }
    }

    /// `R_LoadNamedSkys( skyname )` — `materials/skybox/<name><suffix>.vmt`,
    /// six times.
    ///
    /// **`R_LoadSkys`' fallback to `sky_urb01` is not ported**: that material
    /// is not in Portal 2 either, so the fallback can only turn one
    /// checkerboard into another. The one `skyname` in the game with no
    /// material is `sky_day01_01` — named by 60 maps, of which exactly one
    /// (`e1912`, a cut map) has a sky surface to show it through.
    pub fn load(vfs: &Vfs, materials: &mut MaterialCache, name: Option<&str>) -> Sky {
        let Some(name) = name else {
            return Sky::none();
        };
        let error = materials.error_material();
        let mut missing = None;
        // `Material` is not `Debug` — it owns `wgpu` handles — so the array is
        // built in place rather than collected and converted.
        let faces = SUFFIXES.map(|suffix| {
            let material = materials.load(vfs, &format!("skybox/{name}{suffix}"));
            if Arc::ptr_eq(&material, &error) {
                missing.get_or_insert(suffix);
            }
            material
        });
        if let Some(suffix) = missing {
            eprintln!("source-engine: world: sky {name}: no {name}{suffix}, sky not drawn");
            return Sky {
                name: Some(name.to_owned()),
                faces: None,
            };
        }
        Sky {
            name: Some(name.to_owned()),
            faces: Some(faces),
        }
    }

    /// Whether there is anything to draw. False for a map that named no sky
    /// and for one whose sky is missing from the game.
    pub fn is_loaded(&self) -> bool {
        self.faces.is_some()
    }

    /// One line for the startup log.
    pub fn summary(&self) -> String {
        match (&self.name, self.is_loaded()) {
            (Some(name), true) => format!("sky {name}"),
            (Some(name), false) => format!("sky {name} (missing)"),
            (None, _) => "no sky".to_owned(),
        }
    }

    /// Records the six quads. `R_DrawSkyBox( zFar )` (`gl_warp.cpp:257`).
    ///
    /// `eye` is the camera this pass is drawn from — the box is centred on it,
    /// so it never gets any closer however far you walk. Draw it **first** in
    /// its pass, against a cleared depth buffer: it is at the far plane and
    /// everything else in the picture is in front of it.
    ///
    /// `nDrawFlags`, the six-bit per-face mask, is not a parameter: every
    /// caller in the shipped engine passes the default `0x3F`, and Valve's own
    /// per-face rejection by view direction is commented out above it —
    /// *"Disabling this since it doesn't work in L4D and doesn't really buy us
    /// any perf"*. The frustum rejects the off-screen faces anyway.
    pub fn draw(&self, pass: &mut Pass<'_>, eye: Vec3, z_far: f32) {
        let Some(faces) = &self.faces else {
            return;
        };
        // See the module docs. Restored to nothing afterwards, so that a
        // caller that draws the world next is not handed a pass with culling
        // switched off.
        pass.set_state_override(StateOverride {
            cull: Some(false),
            ..StateOverride::default()
        });
        for (axis, &material) in TEX_ORDER.iter().enumerate() {
            let quad = face(axis, eye, z_far);
            let vertices = pass.vertices(&quad);
            let indices = pass.indices(&QUAD_INDICES);
            pass.draw(&faces[material], &vertices, &indices, Mat4::IDENTITY);
        }
        pass.set_state_override(StateOverride::default());
    }
}

/// One face of the box, in world space. Four [`MakeSkyVec`] calls.
///
/// [`MakeSkyVec`]: make_sky_vec
fn face(axis: usize, eye: Vec3, z_far: f32) -> [SimpleVertex; 4] {
    CORNERS.map(|(s, t)| {
        let (position, texcoord) = make_sky_vec(s, t, axis, z_far, eye);
        SimpleVertex::new(position.to_array(), texcoord)
    })
}

/// `MakeSkyVec` (`gl_warp.cpp:197`), transcribed.
///
/// The `s`/`t` clamp to ±1 is kept although both callers pass exactly ±1, and
/// the bilerp-seam inset that used to follow it is not — Valve commented it
/// out with the note that *"our skyboxes aren't 512x512 and we don't modify
/// the textures to deal with the border seam fixup correctly"*, and Portal 2's
/// are 64×64.
///
/// **`t` is flipped at the end and `s` is not.** That is Valve's `t = 1.0 - t`
/// and it is the difference between a sky and a sky upside down.
fn make_sky_vec(s: f32, t: f32, axis: usize, z_far: f32, eye: Vec3) -> (Vec3, [f32; 2]) {
    let width = z_far * SQRT3INV;
    let s = s.clamp(-1.0, 1.0);
    let t = t.clamp(-1.0, 1.0);

    let b = [s * width, t * width, width];
    let mut v = [0.0f32; 3];
    for (j, slot) in v.iter_mut().enumerate() {
        let k = ST_TO_VEC[axis][j];
        *slot = match k < 0 {
            true => -b[(-k - 1) as usize],
            false => b[(k - 1) as usize],
        };
        *slot += eye[j];
    }

    let s = (s + 1.0) * 0.5;
    let t = (t + 1.0) * 0.5;
    (Vec3::from(v), [s, 1.0 - t])
}

/// A map's 3D skybox: where its second camera sits, and how much smaller the
/// room it looks at is.
///
/// `sky3dparams_t` (`public/playernet_vars.h`) reduced to the two fields that
/// have an effect here. The fog block is parsed by
/// [`SkyCamera`](crate::server::classes::SkyCamera) and goes no further —
/// there is no fog in this port at all.
///
/// **`sky3dparams_t::area` is not here, and its absence is the point.**
/// `PreRender3dSkyboxWorld` refuses when `area == 255`, which is not a fact
/// about areas: the struct lives in `CPlayerLocalData`, `ClientData_Update`
/// writes 255 into it when there is no sky camera, and 255 is chosen because
/// the field is an 8-bit send prop. Here the whole thing is an `Option` and
/// the sentinel disappears — along with the `engine->GetArea()` call the
/// server would otherwise need. `portdocs/ENGINE_WORLD_SKY.md` §4.3.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sky3d {
    /// `sky_camera`'s own origin: the point in the skybox room that
    /// corresponds to the world origin.
    pub origin: Vec3,
    /// `scale`. **16 on all seven of the game's sky cameras**; the
    /// `scale <= 0` case means 1 and is unreachable on shipped content.
    pub scale: f32,
}

impl Sky3d {
    /// Where the sky view's camera goes, for a player looking from
    /// `view_origin`.
    ///
    /// `VectorScale( origin, 1/scale, origin ); VectorAdd( origin, vSkyOrigin,
    /// origin )` (`viewrender.cpp:6806`). The angles are the player's own and
    /// are not transformed at all, which is what makes the sky turn with the
    /// view.
    pub fn eye(&self, view_origin: Vec3) -> Vec3 {
        let inv = match self.scale > 0.0 {
            true => 1.0 / self.scale,
            false => 1.0,
        };
        view_origin * inv + self.origin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one number in `MakeSkyVec` that has to be *just* under the far
    /// plane: `SQRT3INV` is deliberately a little less than `1/√3`, so a
    /// corner of the cube is a little nearer than `z_far`.
    ///
    /// A `SQRT3INV` rounded up to `0.577350` puts the corner at
    /// `z_far × 1.0000001` and clips the corners of the sky away.
    #[test]
    fn a_corner_of_the_box_lands_just_inside_the_far_plane() {
        let z_far = 1000.0;
        for axis in 0..6 {
            for (s, t) in CORNERS {
                let (position, _) = make_sky_vec(s, t, axis, z_far, Vec3::ZERO);
                let distance = position.length();
                assert!(
                    distance < z_far,
                    "axis {axis} corner {s},{t} is at {distance}, outside {z_far}"
                );
                assert!(
                    distance > z_far * 0.9999,
                    "axis {axis} corner {s},{t} is at {distance}, well inside {z_far}"
                );
            }
        }
    }

    /// Each face's *centre* is one axis-aligned direction times the width, and
    /// the six together are the six directions — which is the cheapest
    /// statement of "this is a cube around the eye" that does not restate
    /// `st_to_vec`.
    #[test]
    fn the_six_faces_are_the_six_directions() {
        let z_far = 100.0;
        let width = z_far * SQRT3INV;
        let mut centres: Vec<[i32; 3]> = Vec::new();
        for axis in 0..6 {
            let sum: Vec3 = CORNERS
                .iter()
                .map(|&(s, t)| make_sky_vec(s, t, axis, z_far, Vec3::ZERO).0)
                .sum();
            let centre = sum / 4.0 / width;
            centres.push([
                centre.x.round() as i32,
                centre.y.round() as i32,
                centre.z.round() as i32,
            ]);
        }
        centres.sort_unstable();
        assert_eq!(
            centres,
            vec![
                [-1, 0, 0],
                [0, -1, 0],
                [0, 0, -1],
                [0, 0, 1],
                [0, 1, 0],
                [1, 0, 0],
            ]
        );
    }

    /// The box is centred on the camera and nowhere else: the whole point of
    /// the sky is that walking never gets you any closer to it.
    #[test]
    fn the_box_moves_with_the_eye() {
        let eye = Vec3::new(1234.0, -567.0, 89.0);
        for axis in 0..6 {
            for (s, t) in CORNERS {
                let at_origin = make_sky_vec(s, t, axis, 500.0, Vec3::ZERO).0;
                let moved = make_sky_vec(s, t, axis, 500.0, eye).0;
                assert!((moved - at_origin - eye).length() < 1e-3);
            }
        }
    }

    /// `t = 1.0 - t`, and `s` is left alone. Both halves matter: a sky built
    /// the other way up looks deliberate rather than broken.
    #[test]
    fn the_texture_coordinates_flip_t_and_not_s() {
        let (_, low) = make_sky_vec(-1.0, -1.0, 0, 100.0, Vec3::ZERO);
        let (_, high) = make_sky_vec(1.0, 1.0, 0, 100.0, Vec3::ZERO);
        assert_eq!(low, [0.0, 1.0]);
        assert_eq!(high, [1.0, 0.0]);
    }

    /// `skytexorder` is not the identity, and the two it swaps are the two
    /// that would otherwise be silently wrong on a symmetric sky.
    #[test]
    fn the_texture_order_swaps_back_and_left() {
        let bound: Vec<&str> = TEX_ORDER.iter().map(|&i| SUFFIXES[i]).collect();
        assert_eq!(bound, vec!["rt", "lf", "bk", "ft", "up", "dn"]);
    }

    /// The scale is a *division*: 16 units of walking is one unit of sky.
    #[test]
    fn the_sky_camera_divides_the_view_origin_by_the_scale() {
        let sky = Sky3d {
            origin: Vec3::new(4688.0, 7852.0, -816.0),
            scale: 16.0,
        };
        // `sp_a1_intro1`'s own sky camera and its own spawn.
        let eye = sky.eye(Vec3::new(-8674.0, 1773.0, 101.0));
        assert!((eye - Vec3::new(4145.875, 7962.8125, -809.6875)).length() < 1e-3, "{eye}");
    }

    /// `(m_pSky3dParams->scale > 0) ? (1.0f / scale) : 1.0f`. No shipped map
    /// reaches it; it is two characters and leaving it out would make a
    /// zero-scale camera divide by zero.
    #[test]
    fn a_zero_scale_sky_camera_is_scale_one() {
        let sky = Sky3d {
            origin: Vec3::ZERO,
            scale: 0.0,
        };
        assert_eq!(sky.eye(Vec3::new(10.0, 20.0, 30.0)), Vec3::new(10.0, 20.0, 30.0));
    }

    /// Every map in the game that has a 3D skybox, loaded, spawned and
    /// measured from its own `info_player_start`.
    ///
    /// This is `portdocs/ENGINE_WORLD_SKY.md` §6 answered: what a skybox room
    /// actually contains, whether the sky box draws inside it, and therefore
    /// which of `CSkyboxView::DrawInternal`'s passes the port needs.
    ///
    /// What it **asserts** is the pair of invariants the whole design rests
    /// on, and each would be silently catastrophic if it failed:
    ///
    /// 1. the sky camera's PVS reaches *some* geometry (otherwise the sky is
    ///    an empty clear colour and nobody would know why), and
    /// 2. the sky view and the player's view see **no face in common** — the
    ///    skybox room is a sealed part of the same `.bsp`, and a sky camera
    ///    whose cluster reached the playable map would draw the level a second
    ///    time at 1/16 scale behind itself. This is the whole of what keeps
    ///    the two pictures apart (`portdocs/ENGINE_WORLD_SKY.md` §4.2), so it
    ///    is the one worth asserting.
    ///
    /// It deliberately does **not** assert that the sky view is *smaller*:
    /// both sets are measured through a 90° frustum pointing down `+X`, which
    /// is a fact about this test rather than about the map, and on `e1912` the
    /// skybox is the larger of the two.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_3d_skybox -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn the_3d_skybox_of_every_map_that_has_one() {
        use crate::engine::world::{GeometryPass, World};
        use crate::materials::MaterialCache;

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

        let mut names: Vec<String> = vfs
            .list("maps")
            .expect("maps/")
            .into_iter()
            .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
            .map(|e| e.name.trim_end_matches(".bsp").to_owned())
            .collect();
        names.sort();
        assert!(names.len() > 50, "only {} maps found", names.len());

        println!(
            "{:<16} {:>5} {:>8} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>5} {:>5} {:>6}",
            "map", "scale", "skyleaf", "faces", "main", "skyfc", "props", "brush", "trans", "refr",
            "box", "shared"
        );
        let mut found = 0;
        let mut refracting_total = 0;
        for name in &names {
            // **The entity lump before the map.** Loading a `World` means
            // uploading every prop model the map places, which is seconds a
            // map; 99 of the 106 have no `sky_camera` and there is nothing to
            // measure on them. `Bsp::load` alone answers that.
            let bsp = crate::engine::world::bsp::Bsp::load(&vfs, name).expect("a shipped map");
            if !bsp
                .entities()
                .iter()
                .any(|e| e.classname() == Some("sky_camera"))
            {
                continue;
            }
            drop(bsp);

            let mut materials = MaterialCache::new(&device, &queue);
            let mut world = match World::load(&vfs, &mut materials, &device, name) {
                Ok(world) => world,
                Err(e) => panic!("{name}: {e}"),
            };
            let mut server = crate::server::Server::new();
            server.level_init(name, &world.entities, &world.models);
            let sky3d = server
                .sky3d()
                .unwrap_or_else(|| panic!("{name} places a sky_camera but the server has none"));
            found += 1;

            // The map's own spawn, plus the player's eye height — the same
            // viewpoint `every_shipped_map_culls_most_of_itself` uses, and the
            // only one that is a fact about the map rather than about the
            // test.
            let eye = world
                .spawn
                .map(|s| s.origin + Vec3::Z * 64.0)
                .unwrap_or_else(|| world.center());
            let projection = glam::camera::rh::proj::directx::perspective(
                90f32.to_radians(),
                16.0 / 9.0,
                7.0,
                28_400.0,
            );
            let look = |from: Vec3| glam::camera::rh::view::look_to_mat4(from, Vec3::X, Vec3::Z);
            let main = world.visible(eye, projection * look(eye), false);

            let sky_eye = sky3d.eye(eye);
            let sky_camera = crate::materials::context::Camera {
                view: look(sky_eye),
                projection: glam::camera::rh::proj::directx::perspective(
                    90f32.to_radians(),
                    16.0 / 9.0,
                    SKY_ZNEAR,
                    SKY_ZFAR,
                ),
                eye: sky_eye,
            };
            let set = world.sky_visible_set(&sky_camera, &sky3d, false);

            // World faces in a *translucent* batch, which is the half of §6.1
            // the world can answer for.
            let translucent_faces: usize = world
                .batches
                .iter()
                .filter(|b| GeometryPass::of(&b.material) == GeometryPass::Translucent)
                .flat_map(|b| b.faces.iter())
                .filter(|span| set.face(span.face as usize))
                .count();
            // Props in the sky's leaves, and how many of those wear a material
            // that would need the frame-buffer copy — the other half.
            let mut props = 0;
            let mut refracting = 0;
            for prop in &world.props.instances {
                if !set.any_leaf(&world.props.leaves[prop.leaves.clone()]) {
                    continue;
                }
                props += 1;
                // `PropModels::load`'s own `refracts` predicate, per
                // *placement* rather than per model — a model can carry a
                // refracting material in a skin family no map asks for.
                let refracts = world.prop_models.get(prop.model_index).is_some_and(|model| {
                    model
                        .batches
                        .iter()
                        .any(|b| b.material(prop.skin).needs_frame_buffer_copy)
                });
                if refracts {
                    refracting += 1;
                }
            }
            refracting_total += refracting;
            let brush = (0..world.brush_models.len())
                .filter(|&i| {
                    let (mins, maxs) = world.brush_model_bounds(i);
                    world.box_visible(&set, mins, maxs)
                })
                .count();

            let shared = (0..world.stats.faces_total)
                .filter(|&f| set.face(f) && main.face(f))
                .count();
            println!(
                "{:<16} {:>5} {:>8} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>5} {:>5} {:>6}",
                name,
                sky3d.scale,
                format!("{:?}", world.sky_visible_from(eye)),
                set.stats.faces,
                main.stats.faces,
                world.sky_faces.iter().filter(|&&f| set.face(f as usize)).count(),
                props,
                brush,
                translucent_faces,
                refracting,
                world.sky_visible(&set),
                shared,
            );

            assert!(
                set.stats.faces > 0 || props > 0,
                "{name}: the sky camera sees nothing at all"
            );
            assert_eq!(
                shared, 0,
                "{name}: {shared} faces are in both the sky view and the \
                 player's — the skybox room is not sealed off from the map",
            );
            // Keep the GPU objects alive no longer than the map.
            world.entity_models = Default::default();
        }
        println!("{found} maps with a 3D skybox; {refracting_total} refracting props inside one");
        assert_eq!(found, 7, "the depot ships seven maps with a sky_camera");
    }

    /// The box, drawn and read back: does it actually cover the view, in the
    /// sky's own colour, from the inside?
    ///
    /// This is the test the winding decision (§4.5 of the portdoc) rests on.
    /// With culling left on and the winding the wrong way round, every quad is
    /// back-facing and the readback is the clear colour — which is exactly
    /// what a missing sky looks like in the running game, and exactly what
    /// nobody would notice in a screenshot of an indoor map.
    ///
    /// **Six distinct colours, one per face**, so that a face bound to the
    /// wrong material fails here rather than agreeing by accident: Portal 2's
    /// own skies are near-uniform and would pass whatever `skytexorder` did.
    ///
    /// Needs a GPU but **no game files** — the six materials are literals.
    #[test]
    fn the_box_surrounds_the_camera_in_six_colours() {
        use crate::materials::context::{Camera, Load, RenderContext};
        use crate::materials::target::RenderTarget;
        use crate::materials::MaterialCache;

        let Some((device, queue)) = device() else {
            return;
        };
        const SIZE: u32 = 64;
        // `Rgba8Unorm`, *not* sRGB: a readback byte is then exactly
        // `round(shader_output * 255)` with no curve in the way.
        const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

        let mut materials = MaterialCache::new(&device, &queue);
        // One quarter-step per face, so every pair differs by 64 and a swap
        // cannot be absorbed by the tolerance. No `$basetexture`, which is
        // `sky_fog`'s shape and binds the white texture.
        let colours: [f32; 6] = [0.2, 0.4, 0.6, 0.8, 1.0, 0.0];
        let faces: [Arc<Material>; 6] = std::array::from_fn(|i| {
            materials.synthetic(
                &format!("sky_test_{i}"),
                &format!("UnlitGeneric {{ \"$color\" \"[{c} {c} {c}]\" }}", c = colours[i]),
            )
        });
        let sky = Sky {
            name: Some("sky_test".to_owned()),
            faces: Some(faces),
        };

        let mut context = RenderContext::new(&device, &queue, materials.pipelines());
        let target = RenderTarget::new(&device, "sky", SIZE, SIZE, FORMAT, true);
        let eye = Vec3::new(1234.0, -567.0, 89.0);
        let z_far = 4096.0;

        // The six directions, paired with the suffix that must be looking
        // back. **Derived from `st_to_vec` and `skytexorder`, not from the
        // names**, which do not mean what they look like: face `axis`'s centre
        // is `st_to_vec[axis]` at `s = t = 0`, and the material it binds is
        // `SUFFIXES[TEX_ORDER[axis]]`.
        //
        // | axis | `st_to_vec` | centre | material |
        // |---|---|---|---|
        // | 0 | `{3,-1,2}`  | `+x` | `rt` |
        // | 1 | `{-3,1,2}`  | `-x` | `lf` |
        // | 2 | `{1,3,2}`   | `+y` | `bk` |
        // | 3 | `{-1,-3,2}` | `-y` | `ft` |
        // | 4 | `{-2,-1,3}` | `+z` | `up` |
        // | 5 | `{2,-1,-3}` | `-z` | `dn` |
        //
        // So **`rt` is `+x` and `ft` is `-y`**: the names are Hammer's, from a
        // viewer facing north (`+y`), and not the player's, whose yaw 0 faces
        // `+x`. Guessing them the obvious way puts the sky's four walls a
        // quarter-turn out — invisible on Portal 2's near-uniform skies and
        // glaring on anything with a horizon.
        let looks: [(Vec3, &str); 6] = [
            (Vec3::X, "rt"),
            (-Vec3::X, "lf"),
            (Vec3::Y, "bk"),
            (-Vec3::Y, "ft"),
            (Vec3::Z, "up"),
            (-Vec3::Z, "dn"),
        ];

        for (look, suffix) in looks {
            let up = match look.z.abs() > 0.5 {
                true => Vec3::X,
                false => Vec3::Z,
            };
            let camera = Camera::perspective(
                eye,
                glam::camera::rh::view::look_to_mat4(eye, look, up),
                // Well inside the 90° a face subtends, so the centre of the
                // picture is unambiguously one face.
                60.0,
                1.0,
                SKY_ZNEAR,
                z_far,
            );
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
                    // Magenta, so that "the sky did not draw" is not the same
                    // byte as "the sky is black".
                    Load::Clear(wgpu::Color {
                        r: 1.0,
                        g: 0.0,
                        b: 1.0,
                        a: 1.0,
                    }),
                );
                sky.draw(&mut pass, eye, z_far);
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
                r.expect("map the readback");
            });
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .expect("idle");
            let pixels = readback
                .slice(..)
                .get_mapped_range()
                .expect("the readback is mapped")
                .to_vec();

            let middle = ((SIZE / 2 * SIZE) + SIZE / 2) as usize * 4;
            let got = &pixels[middle..middle + 4];
            let index = SUFFIXES.iter().position(|&s| s == suffix).expect("a suffix");
            let want = (colours[index] * 255.0).round() as i32;
            println!("  looking {look:?}: expected {suffix} ({want}), got {got:?}");
            assert!(
                (i32::from(got[0]) - want).abs() <= 3
                    && (i32::from(got[1]) - want).abs() <= 3
                    && (i32::from(got[2]) - want).abs() <= 3,
                "looking {look:?} the middle of the picture should be {suffix}, \
                 which is {want}; it is {got:?}"
            );
        }
    }

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
}
