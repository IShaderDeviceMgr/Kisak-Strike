//! Static props — the map's placed model instances.
//!
//! `engine/staticpropmgr.cpp`'s reader and instance list, minus the parts that
//! belong to subsystems this port does not have. Design and the measurements
//! that scoped it: `portdocs/STUDIO.md`.
//!
//! A static prop is a model the compiler decided is furniture: it never moves,
//! never animates, has no think function and no entity on the server. Every
//! `prop_static` in the map is deleted from the entity lump at compile time and
//! becomes a row in the `sprp` game lump — an origin, an orientation, and an
//! index into a dictionary of model names. `sp_a1_intro1` places **1,080 props
//! from 136 distinct models**, which is why they are a lump and not entities.
//!
//! # What lives here and what lives in [`studio`](crate::studio)
//!
//! The *model* is an asset: three files, no map, no engine, and the same for
//! every map that uses it. The *instance* comes out of the `.bsp`, is
//! meaningless without one, and dies with it. So `studio/` reads the files and
//! this module places them — the same split `world/` already has against
//! `materials/`, where the `.vtf` is the asset and the lightmap atlas is the
//! map's.
//!
//! # Status
//!
//! Stage 2 of `portdocs/STUDIO.md` §8: the lump is read and every prop has a
//! transform. **Nothing draws them yet** (stage 3) and their lighting is not
//! read yet (stages 4 and 5), so a [`Prop`] is currently a placement waiting
//! for a renderer.

pub mod light;
pub mod lump;
pub mod models;

pub use lump::{PropLumpError, StaticPropLump};
pub use models::PropModels;

use super::bsp::Bsp;
use glam::{Mat3, Mat4, Vec3};

/// `GAMELUMP_STATIC_PROPS` (`gamebspfile.h:29`) — the four-CC `'sprp'` read as
/// a little-endian `i32`.
pub const GAMELUMP_STATIC_PROPS: u32 = 0x7370_7270;

/// `m_Flags`, the byte the compiler and Hammer share.
///
/// Only the two the compiler computes matter to this port so far; the rest are
/// shadow and reflection hints for passes that do not exist yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PropFlags(pub u8);

#[allow(dead_code)]
impl PropFlags {
    /// `STATIC_PROP_FLAG_FADES` — `m_FadeMinDist`/`m_FadeMaxDist` mean
    /// something. Computed by the compiler, not set in Hammer.
    pub const FADES: Self = Self(0x01);
    /// `STATIC_PROP_USE_LIGHTING_ORIGIN` — **this is what makes
    /// `m_LightingOrigin` meaningful**. Without it the field is whatever the
    /// compiler left there and the model's own `illumposition` is the sample
    /// point instead. See [`Prop::lighting_origin`].
    pub const USE_LIGHTING_ORIGIN: Self = Self(0x02);
    pub const NO_FLASHLIGHT: Self = Self(0x04);
    pub const IGNORE_NORMALS: Self = Self(0x08);
    pub const NO_SHADOW: Self = Self(0x10);
    pub const MARKED_FOR_FAST_REFLECTION: Self = Self(0x20);
    /// Set by `vrad`: this prop got no per-vertex bake, so there is no `.vhv`
    /// for it and the ambient cube is the whole of its lighting.
    pub const NO_PER_VERTEX_LIGHTING: Self = Self(0x40);
    pub const NO_SELF_SHADOWING: Self = Self(0x80);

    pub fn from_bits_truncate(bits: u8) -> Self {
        Self(bits)
    }

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// One row of the `sprp` lump, as written.
///
/// Deliberately the file's fields and not a renderer's: the decisions that turn
/// these into something drawable — which angle order, which lighting origin,
/// which LOD — are [`Prop`]'s, and keeping them apart is what lets the reader
/// be tested against bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticProp {
    pub origin: Vec3,
    /// `QAngle` — **pitch, yaw, roll, in that order**, in degrees. This is not
    /// a rotation about x, y, z in that order and reading it as one produces a
    /// map where most props look right and anything tilted does not.
    pub angles: Vec3,
    /// Index into [`StaticPropLump::models`].
    pub model: u16,
    pub first_leaf: u16,
    pub leaf_count: u16,
    /// `SOLID_*` — 0 none, 2 bounding box, 6 `SOLID_VPHYSICS`. Collision is
    /// `ENGINE_TRACE.md` stage 5 and this is carried for it.
    pub solid: u8,
    pub flags: PropFlags,
    /// Which skin family of the model to use. 959 of Portal 2's models have
    /// one, so this is almost always 0.
    pub skin: i32,
    pub fade_min: f32,
    pub fade_max: f32,
    /// **Only meaningful with [`PropFlags::USE_LIGHTING_ORIGIN`].**
    pub lighting_origin: Vec3,
    pub forced_fade_scale: f32,
    /// `m_nMinCPULevel` / `m_nMaxCPULevel` — 0 means "no limit" at both ends.
    pub cpu_level: (u8, u8),
    pub gpu_level: (u8, u8),
    /// `m_DiffuseModulation`, RGBA8. Almost always opaque white.
    pub diffuse_modulation: [u8; 4],
}

impl StaticProp {
    /// The prop's model-to-world transform.
    ///
    /// `AngleMatrix` (`mathlib/mathlib_base.cpp:1305`) with the translation
    /// appended, which is what `CStaticProp::SetModelInstance`'s
    /// `m_ModelToWorld` ends up holding.
    ///
    /// **The angle order is Valve's and `glam` gives no shortcut for it.** The
    /// original's own comment is `matrix = (YAW * PITCH) * ROLL`, so the
    /// rotation is `Rz(yaw) · Ry(pitch) · Rx(roll)` — built here from three
    /// explicit axis rotations rather than from `Mat3::from_euler`, because
    /// every `EulerRot` variant encodes an intrinsic/extrinsic convention as
    /// well as an order and picking the wrong one is a silent half-right
    /// answer: props with only a yaw look correct and every tilted one does
    /// not.
    ///
    /// Source units throughout; no scale, because a static prop has none.
    pub fn model_to_world(&self) -> Mat4 {
        Mat4::from_translation(self.origin) * Mat4::from_mat3(self.rotation())
    }

    /// Just the rotation half of [`model_to_world`](Self::model_to_world).
    pub fn rotation(&self) -> Mat3 {
        let (pitch, yaw, roll) = (
            self.angles.x.to_radians(),
            self.angles.y.to_radians(),
            self.angles.z.to_radians(),
        );
        Mat3::from_rotation_z(yaw) * Mat3::from_rotation_y(pitch) * Mat3::from_rotation_x(roll)
    }
}

/// One placed prop, resolved against its model.
///
/// [`StaticProp`] is the file's row; this is what a renderer wants — the model
/// named rather than indexed, the transform built once, and the lighting sample
/// point already decided.
#[derive(Debug, Clone)]
// `lighting_origin`, `flags`, `skin` and `fade` are read by stages 4-6 and
// `leaves` by visibility; they are parsed now because the lump is read now.
#[allow(dead_code)]
pub struct Prop {
    /// The model path as [`StudioModel::load`] wants it, e.g.
    /// `models/props_bts/gantry_rails_a.mdl`.
    ///
    /// [`StudioModel::load`]: crate::studio::StudioModel::load
    pub model: String,
    /// Index into [`Props::models`] — the same model appears once there and
    /// many times here. `sp_a1_intro1`'s 1,080 props share 136 models, so this
    /// indirection is the difference between 136 uploads and 1,080.
    pub model_index: usize,
    pub transform: Mat4,
    /// Where this prop's lighting is sampled.
    ///
    /// `m_LightingOrigin` when [`PropFlags::USE_LIGHTING_ORIGIN`] is set, and
    /// the prop's own origin otherwise — which is a *placeholder* for the
    /// model's `illumposition` transformed into world space, and is refined
    /// when stage 5 has the `.mdl` in hand
    /// (`CStaticProp::GetLightingOrigin`, `staticpropmgr.cpp:579`).
    pub lighting_origin: Vec3,
    pub flags: PropFlags,
    pub skin: i32,
    /// Kept so stage 6 can fade without re-reading the lump.
    pub fade: (f32, f32, f32),
    /// `m_DiffuseModulation` as the draw wants it — `0..1` per channel.
    ///
    /// Converted at load rather than per draw: it is the same four divisions
    /// for the same prop every frame, and there are 1,080 of them.
    pub modulation: [f32; 4],
    /// The prop's slice of [`Props::leaves`]. Unused until visibility lands.
    pub leaves: std::ops::Range<usize>,
}

/// Every static prop in one map.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct Props {
    /// The distinct models the map places, in dictionary order. What
    /// [`Prop::model_index`] indexes.
    pub models: Vec<String>,
    /// The flat leaf list every [`Prop::leaves`] slices.
    pub leaves: Vec<u16>,
    pub instances: Vec<Prop>,
    /// One lighting state per instance, parallel to
    /// [`instances`](Props::instances).
    ///
    /// Computed once at load by [`light`](Props::light), because a static
    /// prop's lighting is as static as its position — the whole point of
    /// baking it. Empty until that runs, in which case every prop draws under
    /// [`models::FLAT_LIGHTING`].
    pub lighting: Vec<crate::materials::uniforms::ModelLighting>,
}

impl Props {
    /// Reads a map's `sprp` lump, or returns an empty set if it has none.
    ///
    /// A map with no static props is not an error — `sp_a1_intro1` has 1,080
    /// and a test map may have none — so the missing lump and the empty lump
    /// give the same answer.
    pub fn load(map: &str, bsp: &Bsp) -> Result<Props, PropLumpError> {
        let Some(lump) = bsp.game_lump(GAMELUMP_STATIC_PROPS) else {
            return Ok(Props::default());
        };
        Props::from_lump(&StaticPropLump::parse(map, lump)?)
    }

    /// Resolves an already-decoded lump into placements.
    ///
    /// Split out so the transforms can be tested against a hand-built lump with
    /// no `.bsp`.
    pub fn from_lump(lump: &StaticPropLump) -> Result<Props, PropLumpError> {
        let instances = lump
            .props
            .iter()
            .map(|prop| Prop {
                model: lump.models[prop.model as usize].clone(),
                model_index: prop.model as usize,
                transform: prop.model_to_world(),
                lighting_origin: if prop.flags.contains(PropFlags::USE_LIGHTING_ORIGIN) {
                    prop.lighting_origin
                } else {
                    prop.origin
                },
                flags: prop.flags,
                skin: prop.skin,
                fade: (prop.fade_min, prop.fade_max, prop.forced_fade_scale),
                modulation: prop.diffuse_modulation.map(|c| f32::from(c) / 255.0),
                leaves: prop.first_leaf as usize
                    ..prop.first_leaf as usize + prop.leaf_count as usize,
            })
            .collect();

        Ok(Props {
            models: lump.models.clone(),
            leaves: lump.leaves.clone(),
            instances,
            lighting: Vec::new(),
        })
    }

    /// Fills [`lighting`](Props::lighting) from the map's baked ambient cubes.
    ///
    /// Separate from [`from_lump`](Props::from_lump) because it needs the BSP
    /// tree as well as the lump — a prop's lighting is sampled at a *point*,
    /// and finding which leaf that point is in is a tree walk. See
    /// [`light::lighting_for`].
    pub fn light(&mut self, bsp: &Bsp, collision: &crate::engine::trace::CollisionBsp) {
        self.lighting = self
            .instances
            .iter()
            .map(|prop| light::lighting_for(bsp, collision, prop.lighting_origin))
            .collect();
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::world::bsp::GameLump;

    /// Builds an `sprp` payload the way the compiler writes one.
    ///
    /// Deliberately written from `gamebspfile.h`'s field list rather than from
    /// [`lump`]'s reader, so that the two agreeing means something. In
    /// particular it writes the **three bytes of tail padding** explicitly, as
    /// the C++ compiler does implicitly.
    fn sprp(models: &[&str], leaves: &[u16], props: &[StaticProp]) -> GameLump {
        let mut out = Vec::new();
        out.extend_from_slice(&(models.len() as i32).to_le_bytes());
        for name in models {
            let mut field = [0u8; 128];
            field[..name.len()].copy_from_slice(name.as_bytes());
            out.extend_from_slice(&field);
        }
        out.extend_from_slice(&(leaves.len() as i32).to_le_bytes());
        for leaf in leaves {
            out.extend_from_slice(&leaf.to_le_bytes());
        }
        out.extend_from_slice(&(props.len() as i32).to_le_bytes());
        for p in props {
            let start = out.len();
            for v in [p.origin, p.angles] {
                out.extend_from_slice(&v.x.to_le_bytes());
                out.extend_from_slice(&v.y.to_le_bytes());
                out.extend_from_slice(&v.z.to_le_bytes());
            }
            out.extend_from_slice(&p.model.to_le_bytes());
            out.extend_from_slice(&p.first_leaf.to_le_bytes());
            out.extend_from_slice(&p.leaf_count.to_le_bytes());
            out.push(p.solid);
            out.push(p.flags.0);
            out.extend_from_slice(&p.skin.to_le_bytes());
            out.extend_from_slice(&p.fade_min.to_le_bytes());
            out.extend_from_slice(&p.fade_max.to_le_bytes());
            out.extend_from_slice(&p.lighting_origin.x.to_le_bytes());
            out.extend_from_slice(&p.lighting_origin.y.to_le_bytes());
            out.extend_from_slice(&p.lighting_origin.z.to_le_bytes());
            out.extend_from_slice(&p.forced_fade_scale.to_le_bytes());
            out.extend_from_slice(&[
                p.cpu_level.0,
                p.cpu_level.1,
                p.gpu_level.0,
                p.gpu_level.1,
            ]);
            out.extend_from_slice(&p.diffuse_modulation);
            out.push(0); // m_bDisableX360
            assert_eq!(out.len() - start, 69, "the fields sum to 69");
            out.extend_from_slice(&[0; 3]); // the tail padding `sizeof` adds
        }
        GameLump {
            id: GAMELUMP_STATIC_PROPS,
            flags: 0,
            version: 9,
            data: out,
        }
    }

    fn prop_at(origin: Vec3, angles: Vec3) -> StaticProp {
        StaticProp {
            origin,
            angles,
            model: 0,
            first_leaf: 0,
            leaf_count: 0,
            solid: 6,
            flags: PropFlags::default(),
            skin: 0,
            fade_min: 0.0,
            fade_max: 0.0,
            lighting_origin: Vec3::ZERO,
            forced_fade_scale: 1.0,
            cpu_level: (0, 0),
            gpu_level: (0, 0),
            diffuse_modulation: [255; 4],
        }
    }

    #[test]
    fn a_lump_round_trips() {
        let props = [prop_at(Vec3::new(1.0, 2.0, 3.0), Vec3::ZERO)];
        let lump = StaticPropLump::parse("test", &sprp(&["models/a.mdl"], &[7, 8], &props))
            .expect("a well-formed lump");
        assert_eq!(lump.models, ["models/a.mdl"]);
        assert_eq!(lump.leaves, [7, 8]);
        assert_eq!(lump.props.len(), 1);
        assert_eq!(lump.props[0].origin, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(lump.props[0].solid, 6);
    }

    /// The stride trap: `StaticPropLumpV9_t`'s fields sum to 69 and the struct
    /// is 72. A reader that used 69 reads the first prop correctly and every
    /// later one three bytes further off than the last, which is a map whose
    /// props drift — so the second prop is what proves the stride.
    #[test]
    fn the_second_prop_lands_on_the_seventy_two_byte_boundary() {
        let props = [
            prop_at(Vec3::new(1.0, 2.0, 3.0), Vec3::ZERO),
            prop_at(Vec3::new(40.0, 50.0, 60.0), Vec3::new(0.0, 90.0, 0.0)),
        ];
        let raw = sprp(&["models/a.mdl"], &[], &props);
        assert_eq!(raw.data.len(), 4 + 128 + 4 + 4 + 2 * 72);

        let lump = StaticPropLump::parse("test", &raw).expect("a well-formed lump");
        assert_eq!(lump.props[1].origin, Vec3::new(40.0, 50.0, 60.0));
        assert_eq!(lump.props[1].angles, Vec3::new(0.0, 90.0, 0.0));
    }

    /// A lump written at any other stride is refused rather than misread.
    #[test]
    fn a_stride_that_is_not_seventy_two_is_refused() {
        let props = [prop_at(Vec3::ZERO, Vec3::ZERO), prop_at(Vec3::ONE, Vec3::ZERO)];
        let mut raw = sprp(&["models/a.mdl"], &[], &props);
        raw.data.truncate(raw.data.len() - 6); // 69 bytes each
        let e = StaticPropLump::parse("test", &raw).expect_err("a bad stride");
        assert!(format!("{e}").contains("69.00"), "{e}");
    }

    #[test]
    fn a_version_this_reader_does_not_know_is_refused() {
        let mut raw = sprp(&["models/a.mdl"], &[], &[]);
        raw.version = 11;
        assert!(StaticPropLump::parse("test", &raw).is_err());
    }

    #[test]
    fn a_prop_naming_a_model_outside_the_dictionary_is_refused() {
        let mut prop = prop_at(Vec3::ZERO, Vec3::ZERO);
        prop.model = 3;
        let e = StaticPropLump::parse("test", &sprp(&["models/a.mdl"], &[], &[prop]))
            .expect_err("a bad model index");
        assert!(format!("{e}").contains("names model 3"), "{e}");
    }

    /// `AngleMatrix`'s three axes, one at a time.
    ///
    /// A `QAngle` is **pitch, yaw, roll**, and the composition is
    /// `Rz(yaw) · Ry(pitch) · Rx(roll)` — `mathlib_base.cpp:1329`'s own comment
    /// is `matrix = (YAW * PITCH) * ROLL`. Each axis is checked against the
    /// column of `matrix3x4_t` the original writes, which is the only way to
    /// catch a sign: pitch in particular rotates `+X` towards `-Z`, not `+Z`.
    #[test]
    fn valves_angle_order_is_yaw_then_pitch_then_roll() {
        let close = |a: Vec3, b: Vec3| assert!((a - b).length() < 1e-5, "{a} vs {b}");

        // Yaw alone: +X to +Y.
        let yaw = prop_at(Vec3::ZERO, Vec3::new(0.0, 90.0, 0.0)).rotation();
        close(yaw * Vec3::X, Vec3::Y);

        // Pitch alone: +X to -Z. `matrix[2][0] = -sp`.
        let pitch = prop_at(Vec3::ZERO, Vec3::new(90.0, 0.0, 0.0)).rotation();
        close(pitch * Vec3::X, -Vec3::Z);

        // Roll alone: +Y to +Z. `matrix[2][1] = sr*cp`.
        let roll = prop_at(Vec3::ZERO, Vec3::new(0.0, 0.0, 90.0)).rotation();
        close(roll * Vec3::Y, Vec3::Z);

        // All three together, against `AngleMatrix` evaluated by hand at
        // pitch 30, yaw 45, roll 60 — the case that distinguishes this order
        // from every other one.
        let (p, y, r) = (30f32.to_radians(), 45f32.to_radians(), 60f32.to_radians());
        let (sp, cp) = p.sin_cos();
        let (sy, cy) = y.sin_cos();
        let (sr, cr) = r.sin_cos();
        let m = prop_at(Vec3::ZERO, Vec3::new(30.0, 45.0, 60.0)).rotation();
        close(m * Vec3::X, Vec3::new(cp * cy, cp * sy, -sp));
        close(
            m * Vec3::Y,
            Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp),
        );
        close(
            m * Vec3::Z,
            Vec3::new(cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp),
        );
    }

    /// The transform is rotate-then-translate, not the other way around.
    #[test]
    fn the_transform_puts_the_model_at_its_origin() {
        let prop = prop_at(Vec3::new(100.0, 0.0, 0.0), Vec3::new(0.0, 90.0, 0.0));
        let m = prop.model_to_world();
        assert!((m.transform_point3(Vec3::ZERO) - Vec3::new(100.0, 0.0, 0.0)).length() < 1e-4);
        // A point 10 units down the model's +X ends up 10 units along world +Y.
        let moved = m.transform_point3(Vec3::new(10.0, 0.0, 0.0));
        assert!((moved - Vec3::new(100.0, 10.0, 0.0)).length() < 1e-4, "{moved}");
    }

    /// `m_LightingOrigin` is only meaningful with the flag that says so;
    /// without it the field holds whatever the compiler left there.
    #[test]
    fn the_lighting_origin_needs_its_flag() {
        let mut prop = prop_at(Vec3::new(1.0, 2.0, 3.0), Vec3::ZERO);
        prop.lighting_origin = Vec3::new(9.0, 9.0, 9.0);

        let without = Props::from_lump(
            &StaticPropLump::parse("test", &sprp(&["models/a.mdl"], &[], &[prop.clone()])).unwrap(),
        )
        .unwrap();
        assert_eq!(without.instances[0].lighting_origin, Vec3::new(1.0, 2.0, 3.0));

        prop.flags = PropFlags::USE_LIGHTING_ORIGIN;
        let with = Props::from_lump(
            &StaticPropLump::parse("test", &sprp(&["models/a.mdl"], &[], &[prop])).unwrap(),
        )
        .unwrap();
        assert_eq!(with.instances[0].lighting_origin, Vec3::new(9.0, 9.0, 9.0));
    }

    /// Instances share models: 1,080 props from 136 models in `sp_a1_intro1`,
    /// and the indirection is what makes that 136 uploads instead of 1,080.
    #[test]
    fn instances_share_the_dictionary() {
        let props = [prop_at(Vec3::ZERO, Vec3::ZERO), prop_at(Vec3::ONE, Vec3::ZERO)];
        let set = Props::from_lump(
            &StaticPropLump::parse("test", &sprp(&["models/a.mdl"], &[], &props)).unwrap(),
        )
        .unwrap();
        assert_eq!(set.models.len(), 1);
        assert_eq!(set.instances.len(), 2);
        assert!(set.instances.iter().all(|p| p.model_index == 0));
    }

    /// Every shipped map's `sprp` lump, read for real.
    ///
    /// Ignored by default and gated on `KISAK_GAME_DIR` — see
    /// `studio::tests::every_shipped_studio_model_parses` for why. Run with:
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_map -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_map_places_its_props() {
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

        let (mut maps, mut total, mut with_props) = (0, 0usize, 0);
        let (mut vhv_total, mut vhv_matched, mut vhv_stale) = (0usize, 0usize, 0usize);
        for name in &names {
            let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
            let props = Props::load(name, &bsp).unwrap_or_else(|e| panic!("{name}: {e}"));
            maps += 1;
            total += props.instances.len();
            if !props.is_empty() {
                with_props += 1;
            }
            // Every instance names a model in the dictionary and a leaf range
            // inside the leaf list — both already checked by the reader, so
            // this is the invariant the reader is trusted for.
            for prop in &props.instances {
                assert!(prop.model_index < props.models.len());
                assert!(prop.leaves.end <= props.leaves.len());
                assert!(prop.model.ends_with(".mdl"), "{}", prop.model);
            }
            // Stage 4: every prop's `.vhv` must match the model it names, in
            // the hardware vertex order `HardwareMesh` documents. A count that
            // disagrees is the reader being wrong about that order rather than
            // the data being odd — `vrad` writes one file per prop and it is
            // generated from the same `.mdl` this loads.
            let pak = crate::filesystem::mount::pak::PakMount::new(
                name,
                std::sync::Arc::clone(&bsp.pak),
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
            vfs.set_map_pak(Some((
                crate::filesystem::PathId::Game,
                std::sync::Arc::new(pak),
            )));
            let mut cache: std::collections::HashMap<usize, Option<crate::studio::StudioModel>> =
                std::collections::HashMap::new();
            for (i, prop) in props.instances.iter().enumerate() {
                if prop.flags.contains(PropFlags::NO_PER_VERTEX_LIGHTING) {
                    continue;
                }
                let path = crate::studio::vhv::prop_lighting_path(i, bsp.lighting_is_hdr);
                let Ok(bytes) = vfs.read(&path) else { continue };
                let vhv = crate::studio::Vhv::parse(path.clone(), &bytes)
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
                let model = cache.entry(prop.model_index).or_insert_with(|| {
                    crate::studio::StudioModel::load(&vfs, &prop.model).ok()
                });
                let Some(model) = model else { continue };
                vhv_total += 1;
                // Not asserted: `r_ignoreStaticColorChecksum` defaults to 1 and
                // the shipped data needs it to — `mp_coop_paint_longjump_intro`
                // prop 26's `.vhv` carries a checksum that is not its model's,
                // and the real game draws it lit. The *shape* is what has to
                // agree, and that is what is checked below.
                if vhv.checksum != model.checksum {
                    vhv_stale += 1;
                }
                assert!(
                    vhv.colors(&bytes, 0, &model.meshes, model.vertices.len())
                        .is_some(),
                    "{name}: {path} does not describe {} — .vhv lod 0 {:?}, model {:?}",
                    prop.model,
                    vhv.lod_meshes(0).map(|m| m.vertex_count).collect::<Vec<_>>(),
                    model.meshes.iter().map(|m| m.vertices.len()).collect::<Vec<_>>(),
                );
                vhv_matched += 1;
            }
            vfs.set_map_pak(None);

            if name == "sp_a1_intro1" {
                // `portdocs/STUDIO.md` §8 stage 2's acceptance measurement.
                assert_eq!(props.instances.len(), 1080, "sp_a1_intro1 prop count");
                assert_eq!(props.models.len(), 136, "sp_a1_intro1 distinct models");
            }
        }
        println!(
            "{maps} maps, {with_props} with props, {total} props placed; \
             {vhv_matched}/{vhv_total} .vhv files match their model \
             ({vhv_stale} with a stale checksum, used anyway)"
        );

        // The decode question `light::decode` documents, measured rather than
        // argued: a lightmap luxel and an ambient cube sample are both
        // `ColorRGBExp32` written by the same `vrad` run over the same scene,
        // so if one decode puts them in the same range and the other puts them
        // 255 apart, the first is the one this port's single linear space
        // wants.
        let bsp = Bsp::load(&vfs, "sp_a1_intro1").expect("the reference map");
        let mean = |values: &mut dyn Iterator<Item = [f32; 3]>| {
            let (sum, n) = values.fold((0.0f64, 0usize), |(sum, n), c| {
                (sum + f64::from(c[0] + c[1] + c[2]) / 3.0, n + 1)
            });
            sum / n.max(1) as f64
        };
        let lightmap = mean(&mut bsp.lighting.iter().map(|c| c.to_linear()));
        let ambient = mean(
            &mut bsp
                .leaf_ambient
                .iter()
                .flat_map(|s| s.cube.iter().map(|c| c.to_linear())),
        );
        println!(
            "sp_a1_intro1 mean luminance: lightmap {lightmap:.4}, ambient cube {ambient:.4}              (ratio {:.1})",
            lightmap / ambient.max(f64::MIN_POSITIVE)
        );
    }
}
