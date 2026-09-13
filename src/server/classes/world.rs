//! `worldspawn`. `game/server/world.cpp`.
//!
//! Exactly one per map, spawned before everything else and forbidden a parent
//! (`mapentities.cpp:373`).

use glam::Vec3;

use crate::server::class::Behaviour;
use crate::server::entity::EntityCore;
use crate::server::keyvalue::{atof, atoi, string_to_vector};

/// The keys `CWorld` consumes *that some shipped Portal 2 map supplies*.
///
/// `CWorld`'s datadesc and `KeyValue` between them take fifteen; ten of them —
/// `chaptertitle`, `startdark`, `gametitle`, `maxoccludeearea`,
/// `minoccluderarea`, `minpropscreenwidth`, `coldworld`, `newunit`,
/// `timeofday` and the `_x360` occlusion pair — appear **zero** times across
/// all 106 maps, so they are measurements rather than omissions.
pub(super) static WORLD_KEYS: &[&str] = &[
    "skyname",
    "world_mins",
    "world_maxs",
    "maxpropscreenwidth",
    "maxblobcount",
    "detailmaterial",
];

/// `CWorld` (`game/server/world.cpp`) — the map itself, as an entity.
///
/// Everything it holds is read by something else in the engine today; the
/// duplication is deliberate and temporary, see [`World::sky_name`].
pub struct World {
    /// `skyname`. **Also read straight from the lump by
    /// [`crate::engine::world::World`]**, which is the older of the two paths
    /// and the one the renderer uses. Valve has the same split — `CWorld`
    /// pushes it into the `sv_skyname` cvar and the client reads *that* — and
    /// it resolves when the 3D skybox lands and has one owner.
    pub sky_name: Option<String>,
    /// `world_mins`/`world_maxs`, which `vbsp` writes. Not the same numbers as
    /// [`crate::engine::world::World::bounds`]: those are model 0's bounding
    /// box and these are the map's declared extents.
    pub world_mins: Vec3,
    pub world_maxs: Vec3,
    /// `maxpropscreenwidth`. `-1` in every shipped map, meaning "use the
    /// default", which is what makes prop fade distances the renderer's
    /// business and not this module's.
    pub max_prop_screen_width: f32,
    /// `maxblobcount` — Portal 2's paint blob pool size, 250 in every shipped
    /// map. `CWorld::KeyValue` allocates the pool here; there is no paint
    /// system, so the number is recorded and nothing is allocated.
    pub max_blob_count: i32,
    /// `detailmaterial` — the sprite sheet detail props are cut from.
    pub detail_material: Option<String>,
}

impl World {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(World {
            sky_name: None,
            world_mins: Vec3::ZERO,
            world_maxs: Vec3::ZERO,
            max_prop_screen_width: -1.0,
            max_blob_count: 0,
            detail_material: None,
        })
    }
}

impl Behaviour for World {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("skyname") {
            self.sky_name = Some(value.to_owned());
        } else if is("world_mins") {
            self.world_mins = string_to_vector(value);
        } else if is("world_maxs") {
            self.world_maxs = string_to_vector(value);
        } else if is("maxpropscreenwidth") {
            self.max_prop_screen_width = atof(value);
        } else if is("maxblobcount") {
            self.max_blob_count = atoi(value);
        } else if is("detailmaterial") {
            self.detail_material = Some(value.to_owned());
        } else {
            return false;
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let v = |v: Vec3| format!("{:.1} {:.1} {:.1}", v.x, v.y, v.z);
        let mut out = vec![
            ("world_mins", v(self.world_mins)),
            ("world_maxs", v(self.world_maxs)),
            ("maxpropscreenwidth", self.max_prop_screen_width.to_string()),
            ("maxblobcount", self.max_blob_count.to_string()),
        ];
        if let Some(sky) = &self.sky_name {
            out.push(("skyname", sky.clone()));
        }
        if let Some(material) = &self.detail_material {
            out.push(("detailmaterial", material.clone()));
        }
        out
    }
}
