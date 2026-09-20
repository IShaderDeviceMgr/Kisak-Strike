//! `sky_camera` — where a map's 3D skybox is, and how much smaller it is.
//!
//! `game/server/SkyCamera.cpp`. **Seven of them across the 106 shipped maps,
//! never two in one map, and `scale 16` on all seven**;
//! `portdocs/ENGINE_WORLD_SKY.md` §2 is the census.
//!
//! In the shipped engine this reaches the renderer the long way round:
//! `ClientData_Update` (`playerlocaldata.cpp:323`) copies the current camera's
//! `sky3dparams_t` into `CPlayerLocalData::m_skybox3d`, thirteen send props
//! carry it to the client, and `CSkyboxView::PreRender3dSkyboxWorld` reads it
//! back off the local player. One process, one accessor —
//! [`Server::sky3d`](crate::server::Server::sky3d) — which is the same
//! collapse `env_tonemap_controller` got and for the same reason: the value is
//! map state that an input can change, so it is read once per rendered frame
//! rather than copied at load.

use glam::Vec3;

use crate::engine::world::sky::Sky3d;
use crate::server::class::{Behaviour, Context, InputDef, InputDefs, SpawnResult};
use crate::server::entity::EntityCore;
use crate::server::io::{FieldType, Input};
use crate::server::keyvalue::{atof, atoi, string_to_color32, string_to_vector};

/// The keys `CSkyCamera` consumes. All eleven, although only two have an
/// effect here — see [`SkyCamera::fog`].
pub(super) static SKY_CAMERA_KEYS: &[&str] = &[
    "scale",
    "use_angles",
    "fogenable",
    "fogblend",
    "fogdir",
    "fogcolor",
    "fogcolor2",
    "fogstart",
    "fogend",
    "fogmaxdensity",
    "HDRColorScale",
];

pub(super) static SKY_CAMERA_INPUTS: InputDefs = &[InputDef::new("ActivateSkybox", FieldType::Void)];

/// `fogparams_t`'s half of `sky3dparams_t`, parsed and recorded and read by
/// nothing.
///
/// **There is no fog anywhere in this port** — `$nofog` is parsed into every
/// shader's flag word and no shader reads it — so the 3D skybox is not a
/// special case here. The keys are consumed rather than dropped because the
/// port's "every key a shipped map declares is consumed" invariant is checked
/// by a depot test, and because `ent_dump` should show what the map asked for.
///
/// `portdocs/ENGINE_WORLD_SKY.md` §7 records what it costs: `sky_fog` is a
/// flat `{70 85 100}` over five maps and is meant to disappear into fog that
/// is not being drawn.
#[derive(Debug, Clone, Copy, Default)]
pub struct SkyFog {
    pub enable: bool,
    pub blend: bool,
    /// `fogdir`, or — when `use_angles` is set — **minus** the entity's own
    /// forward vector, which `CSkyCamera::Activate` computes.
    pub dir_primary: Vec3,
    pub color_primary: [u8; 4],
    pub color_secondary: [u8; 4],
    pub start: f32,
    pub end: f32,
    pub max_density: f32,
    pub hdr_color_scale: f32,
}

/// `CSkyCamera` (`game/server/SkyCamera.h:21`) — a `CLogicalEntity` that marks
/// the point in the 3D skybox corresponding to the world origin.
pub struct SkyCamera {
    /// `m_skyboxData.scale`. **`FIELD_INTEGER`**, not a float — a map cannot
    /// ask for a half-scale skybox.
    scale: i32,
    /// `m_skyboxData.origin`, taken from `GetLocalOrigin()` in `Spawn` rather
    /// than from the `origin` key, which is what makes a parented sky camera
    /// (there are none) use its parent-relative position.
    origin: Vec3,
    /// `m_bUseAngles` — read `fogdir` from the entity's angles instead of from
    /// the key. Set on two of the game's seven cameras.
    use_angles: bool,
    fog: SkyFog,
}

impl SkyCamera {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(SkyCamera {
            scale: 0,
            origin: Vec3::ZERO,
            use_angles: false,
            // `CSkyCamera::CSkyCamera` sets exactly these two and leaves the
            // rest zeroed.
            fog: SkyFog {
                max_density: 1.0,
                hdr_color_scale: 1.0,
                ..SkyFog::default()
            },
        })
    }

    /// What the renderer needs: where the second camera goes and how much
    /// smaller the room is.
    ///
    /// `sky3dparams_t::area` has no counterpart — see [`Sky3d`], which says
    /// why the 255 sentinel is an `Option` here and why this class therefore
    /// needs no `engine->GetArea()`.
    pub fn params(&self) -> Sky3d {
        Sky3d {
            origin: self.origin,
            scale: self.scale as f32,
        }
    }

    /// The fog block, for whoever ports fog. `ent_dump` prints it through
    /// [`describe`](Behaviour::describe) instead, so this has no caller yet
    /// and is the accessor that closes the class rather than a stub.
    #[allow(dead_code)]
    pub fn fog(&self) -> SkyFog {
        self.fog
    }
}

impl Behaviour for SkyCamera {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("scale") {
            self.scale = atoi(value);
        } else if is("use_angles") {
            self.use_angles = atoi(value) != 0;
        } else if is("fogenable") {
            self.fog.enable = atoi(value) != 0;
        } else if is("fogblend") {
            self.fog.blend = atoi(value) != 0;
        } else if is("fogdir") {
            self.fog.dir_primary = string_to_vector(value);
        } else if is("fogcolor") {
            self.fog.color_primary = string_to_color32(value);
        } else if is("fogcolor2") {
            self.fog.color_secondary = string_to_color32(value);
        } else if is("fogstart") {
            self.fog.start = atof(value);
        } else if is("fogend") {
            self.fog.end = atof(value);
        } else if is("fogmaxdensity") {
            self.fog.max_density = atof(value);
        } else if is("HDRColorScale") {
            self.fog.hdr_color_scale = atof(value);
        } else {
            return false;
        }
        true
    }

    /// `CSkyCamera::Spawn` — three lines, of which one survives.
    ///
    /// `m_skyboxData.area = engine->GetArea( origin )` is gone with the
    /// sentinel it feeds ([`Sky3d`]), and `Precache()` has nothing to
    /// precache: the six sky materials are loaded from `worldspawn`'s
    /// `skyname` by `engine::world::World::load`, not from here.
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        self.origin = entity.local_origin;
        SpawnResult::Ok
    }

    /// `CSkyCamera::Activate` — `use_angles` turns the entity's own
    /// orientation into the fog direction.
    ///
    /// **Negated**, which is Valve's `*= -1.0f`: `fogdir` points *from* the
    /// distant fog *towards* the viewer, so a camera pointing at the sun
    /// stores the direction away from it.
    ///
    /// The HL2 `s_pBogusFogMaps` fixup that follows it in the shipped file is
    /// not ported: it is a list of 19 Half-Life 2 map names, guarded by
    /// `#ifdef HL2_DLL`, that averages the two fog colours to preserve the
    /// appearance of maps compiled against a bug this tree has already fixed.
    fn activate(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) {
        if self.use_angles {
            let (forward, _, _) = crate::math::angle_vectors(entity.angles);
            self.fog.dir_primary = -forward;
        }
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        // `InputActivateSkybox` is `g_hActiveSkybox = this` and nothing else.
        // The handle is a property of the *map* rather than of the entity, so
        // it lives on the `Server` and this asks for it — see
        // [`Context::activate_skybox`].
        if !input.name.eq_ignore_ascii_case("ActivateSkybox") {
            return false;
        }
        cx.activate_skybox(entity.id());
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let color = |c: [u8; 4]| format!("{} {} {}", c[0], c[1], c[2]);
        vec![
            ("scale", self.scale.to_string()),
            (
                "origin",
                format!("{:.1} {:.1} {:.1}", self.origin.x, self.origin.y, self.origin.z),
            ),
            ("use_angles", u8::from(self.use_angles).to_string()),
            ("fogenable", u8::from(self.fog.enable).to_string()),
            ("fogblend", u8::from(self.fog.blend).to_string()),
            (
                "fogdir",
                format!(
                    "{:.2} {:.2} {:.2}",
                    self.fog.dir_primary.x, self.fog.dir_primary.y, self.fog.dir_primary.z
                ),
            ),
            ("fogcolor", color(self.fog.color_primary)),
            ("fogcolor2", color(self.fog.color_secondary)),
            ("fogstart", self.fog.start.to_string()),
            ("fogend", self.fog.end.to_string()),
            ("fogmaxdensity", self.fog.max_density.to_string()),
            ("HDRColorScale", self.fog.hdr_color_scale.to_string()),
        ]
    }
}
