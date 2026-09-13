//! The entity classes, and the table that turns a classname into one.
//!
//! `EntityFactoryDictionary` (`game/server/entitylist.cpp:60`) and the
//! `LINK_ENTITY_TO_CLASS`/`BEGIN_DATADESC` pairs scattered through
//! `game/server/`.
//!
//! # What is here, and why these
//!
//! Stage 1 of `portdocs/SERVER.md` calls for `worldspawn`, `info_player_start`,
//! `info_target`, `logic_relay` and `func_instance_io_proxy`. The light family
//! is the addition, and it earns its place twice over: **7,150 of the shipped
//! game's 60,925 entities are lights**, the second largest family after
//! `logic_relay`, and `CLight::Spawn` is the one piece of stage-1 behaviour
//! that actually *does* something — it deletes 6,937 of those 7,150, because a
//! light with no `targetname` has already had its whole contribution baked
//! into the lightmaps by `vrad` and has nothing left to do at run time. A port
//! that keeps them is carrying eleven per cent of the game's entity list as
//! garbage, and would never notice.
//!
//! # One file for now
//!
//! `portdocs/SERVER.md` §7.1 lays out a `classes/` directory with a file per
//! family. Eight classes do not need five files; this splits when stage 2's
//! logic family arrives.

use glam::Vec3;

use super::class::{Behaviour, ClassDef, PointEntity, SpawnResult};
use super::entity::EntityCore;
use super::keyvalue::{atof, atoi, string_to_color32, string_to_vector};

/// Every class this port knows. `CEntityFactoryDictionary::m_Factories`.
///
/// Ordered as `portdocs/SERVER.md` §1.2 orders the census — commonest first —
/// because [`lookup`] is a linear scan and because it makes the coverage
/// obvious to a reader.
pub(super) static CLASSES: &[ClassDef] = &[
    ClassDef {
        name: "logic_relay",
        keys: &["StartDisabled"],
        outputs: &["OnTrigger", "OnSpawn"],
        create: Relay::create,
    },
    ClassDef {
        name: "light",
        keys: LIGHT_KEYS,
        outputs: &[],
        create: Light::create,
    },
    ClassDef {
        name: "light_spot",
        keys: LIGHT_KEYS,
        outputs: &[],
        create: Light::create,
    },
    // Both are `CLight` too (`lights.cpp:234`). 4 `light_directional` and no
    // `light_glspot` in the shipped maps; they cost a line each and the
    // alternative is a map that spawns an entity the game would not.
    ClassDef {
        name: "light_directional",
        keys: LIGHT_KEYS,
        outputs: &[],
        create: Light::create,
    },
    ClassDef {
        name: "light_glspot",
        keys: LIGHT_KEYS,
        outputs: &[],
        create: Light::create,
    },
    ClassDef {
        name: "light_environment",
        keys: ENV_LIGHT_KEYS,
        outputs: &[],
        create: EnvLight::create,
    },
    ClassDef {
        name: "func_instance_io_proxy",
        keys: &[],
        outputs: PROXY_RELAYS,
        create: InstanceIoProxy::create,
    },
    ClassDef {
        name: "info_target",
        keys: &[],
        outputs: &[],
        create: PointEntity::create,
    },
    ClassDef {
        name: "info_player_start",
        keys: &[],
        outputs: &[],
        create: PointEntity::create,
    },
    ClassDef {
        name: "worldspawn",
        keys: WORLD_KEYS,
        outputs: &[],
        create: World::create,
    },
];

/// `EntityFactoryDictionary()->Create( className )`'s lookup half.
///
/// Case-insensitive, because `CUtlDict` is. Every classname in every shipped
/// map is already lower case, so this costs nothing and removes a class of
/// bug that would only appear on someone else's map.
pub fn lookup(classname: &str) -> Option<&'static ClassDef> {
    CLASSES
        .iter()
        .find(|class| class.name.eq_ignore_ascii_case(classname))
}

// ---------------------------------------------------------------------------
// worldspawn
// ---------------------------------------------------------------------------

/// The keys `CWorld` consumes *that some shipped Portal 2 map supplies*.
///
/// `CWorld`'s datadesc and `KeyValue` between them take fifteen; ten of them —
/// `chaptertitle`, `startdark`, `gametitle`, `maxoccludeearea`,
/// `minoccluderarea`, `minpropscreenwidth`, `coldworld`, `newunit`,
/// `timeofday` and the `_x360` occlusion pair — appear **zero** times across
/// all 106 maps, so they are measurements rather than omissions.
static WORLD_KEYS: &[&str] = &[
    "skyname",
    "world_mins",
    "world_maxs",
    "maxpropscreenwidth",
    "maxblobcount",
    "detailmaterial",
];

/// `CWorld` (`game/server/world.cpp`) — the map itself, as an entity.
///
/// Exactly one per map, spawned before everything else and forbidden a parent
/// (`mapentities.cpp:373`). Everything it holds is read by something else in
/// the engine today; the duplication is deliberate and temporary, see
/// [`World::sky_name`].
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
    fn create() -> Box<dyn Behaviour> {
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

// ---------------------------------------------------------------------------
// the light family
// ---------------------------------------------------------------------------

/// What `CLight` consumes. `defaultstyle` is in its datadesc and appears
/// **zero** times in the shipped maps, so it is left out with the rest of the
/// unreachable keys.
///
/// Everything else a `light` carries — `_light`, `_lightHDR`,
/// `_lightscaleHDR`, `_quadratic_attn` and the six other falloff keys — is
/// `vrad`'s (`utils/vbsp/map.cpp` reads them at compile time) and has no
/// run-time consumer in Valve's engine either. They show up in the unhandled
/// report, correctly: nothing handles them, by design.
static LIGHT_KEYS: &[&str] = &["style", "pattern", "pitch"];

/// `CLight` (`game/server/lights.cpp:16`) — `light`, `light_spot`,
/// `light_glspot` and `light_directional`.
pub struct Light {
    /// `style` — the lightstyle slot. Only values `>= 32` are switchable;
    /// below that the style is one of `vrad`'s animated presets.
    pub style: i32,
    /// `pattern` — the lightstyle animation string, 31 entities in the whole
    /// game.
    pub pattern: Option<String>,
}

impl Light {
    fn create() -> Box<dyn Behaviour> {
        Box::new(Light {
            style: 0,
            pattern: None,
        })
    }

    /// `CLight::KeyValue` (`lights.cpp:44`), factored out so that
    /// [`EnvLight`] can delegate to it the way `CEnvLight` calls
    /// `BaseClass::KeyValue`.
    fn key_value(&mut self, entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("pitch") {
            // `CLight::KeyValue` writes the pitch into `angles.x` and leaves
            // yaw and roll alone. It is **order dependent against `angles`**,
            // exactly as it is in the original: a `light_spot` carries both,
            // and whichever comes last in the lump wins. All 2,470 of them
            // spell `angles` before `pitch`, so the pitch survives.
            entity.angles.x = atof(value);
        } else if is("style") {
            self.style = atoi(value);
        } else if is("pattern") {
            self.pattern = Some(value.to_owned());
        } else {
            return false;
        }
        true
    }

    /// `CLight::Spawn` (`lights.cpp:62`).
    ///
    /// **An unnamed light deletes itself.** `vrad` has already baked it; what
    /// survives is the 213 lights a map can switch on and off by name. The
    /// lightstyle half — `engine->LightStyle( m_iStyle, ... )` — needs
    /// animated lightstyles, which `world/` has not got, so the entity stays
    /// alive and does nothing with its style yet.
    fn spawn(&mut self, entity: &mut EntityCore) -> SpawnResult {
        match entity.name.is_some() {
            true => SpawnResult::Ok,
            false => SpawnResult::Remove,
        }
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![("style", self.style.to_string())];
        if let Some(pattern) = &self.pattern {
            out.push(("pattern", pattern.clone()));
        }
        out
    }
}

impl Behaviour for Light {
    fn key_value(&mut self, entity: &mut EntityCore, key: &str, value: &str) -> bool {
        Light::key_value(self, entity, key, value)
    }

    fn spawn(&mut self, entity: &mut EntityCore) -> SpawnResult {
        Light::spawn(self, entity)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        Light::describe(self)
    }
}

/// What `CEnvLight` adds to `CLight`.
static ENV_LIGHT_KEYS: &[&str] = &["_light", "_ambient", "style", "pattern", "pitch"];

/// `CEnvLight` (`lights.cpp:236`) — `light_environment`, the sun.
///
/// **This is where composition replaces `BaseClass`**: `CEnvLight : public
/// CLight` becomes an `EnvLight` that *holds* a [`Light`] and ends its
/// `key_value` by calling the contained one's. Same order, same result, and
/// `portdocs/SERVER.md` §7.3's `parent` pointer turns out not to be needed.
///
/// What is not ported is everything `CEnvLight` actually does with the values:
/// `CCascadeLight::SetLightColor`/`SetEnvLightShadowAngles` is CS:GO's
/// cascaded shadow map, which Portal 2 does not have — its sun is baked into
/// the lightmaps and the skybox. The keys are consumed and recorded.
pub struct EnvLight {
    light: Light,
    /// `_light` — the sun colour, RGB plus an intensity in the fourth
    /// component rather than an alpha.
    pub sun_color: [u8; 4],
}

impl EnvLight {
    fn create() -> Box<dyn Behaviour> {
        Box::new(EnvLight {
            light: Light {
                style: 0,
                pattern: None,
            },
            sun_color: [255, 255, 255, 255],
        })
    }
}

impl Behaviour for EnvLight {
    fn key_value(&mut self, entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("_light") {
            self.sun_color = string_to_color32(value);
            return true;
        }
        // `CEnvLight::KeyValue`'s `_ambient` branch is empty and returns true,
        // which is a consumed key with no effect. Reproduced rather than
        // dropped, so that it is not reported as unhandled — nothing is
        // missing here.
        if key.eq_ignore_ascii_case("_ambient") {
            return true;
        }
        self.light.key_value(entity, key, value)
    }

    fn spawn(&mut self, entity: &mut EntityCore) -> SpawnResult {
        self.light.spawn(entity)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = self.light.describe();
        let c = self.sun_color;
        out.push(("_light", format!("{} {} {} {}", c[0], c[1], c[2], c[3])));
        out
    }
}

// ---------------------------------------------------------------------------
// logic_relay
// ---------------------------------------------------------------------------

/// `CLogicRelay` (`game/server/logicrelay.cpp`) — the commonest entity in the
/// game, 8,082 of them.
///
/// Stage 1 parses it and spawns it; the behaviour is stage 2's, because all of
/// it is entity I/O. What that behaviour will be, recorded here so the
/// spawnflag numbers do not have to be looked up twice:
/// `SF_REMOVE_ON_FIRE` is 1 (308 relays) and `SF_ALLOW_FAST_RETRIGGER` is 2
/// (784); the 6,989 with neither latch themselves shut after firing and post
/// `EnableRefire` to themselves at `GetMaxDelay() + 0.001`, without which a
/// relay re-triggered during its own delay double-fires.
pub struct Relay {
    /// `StartDisabled` — 249 of the game's relays set it.
    pub disabled: bool,
}

impl Relay {
    fn create() -> Box<dyn Behaviour> {
        Box::new(Relay { disabled: false })
    }
}

impl Behaviour for Relay {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("StartDisabled") {
            self.disabled = atoi(value) != 0;
            return true;
        }
        false
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![("StartDisabled", self.disabled.to_string())]
    }
}

// ---------------------------------------------------------------------------
// func_instance_io_proxy
// ---------------------------------------------------------------------------

/// `OnProxyRelay1` … `OnProxyRelay30`.
///
/// Valve's datadesc declares **31** `DEFINE_OUTPUT` lines for 30 names —
/// `OnProxyRelay16` appears twice (`func_instance_io_proxy.cpp:140` and
/// `:141`). Harmless there, because `AcceptInput`'s walk takes the first
/// match; thirty distinct names here.
///
/// The FGD declares the *unnumbered* `OnProxyRelay`, which is what a mapper
/// sees; Hammer's instance compiler emits the numbered ones. 135 entities in
/// the shipped maps still carry the unnumbered key, which no version of the
/// server has ever handled — they show up as unhandled, correctly.
static PROXY_RELAYS: &[&str] = &[
    "OnProxyRelay1",
    "OnProxyRelay2",
    "OnProxyRelay3",
    "OnProxyRelay4",
    "OnProxyRelay5",
    "OnProxyRelay6",
    "OnProxyRelay7",
    "OnProxyRelay8",
    "OnProxyRelay9",
    "OnProxyRelay10",
    "OnProxyRelay11",
    "OnProxyRelay12",
    "OnProxyRelay13",
    "OnProxyRelay14",
    "OnProxyRelay15",
    "OnProxyRelay16",
    "OnProxyRelay17",
    "OnProxyRelay18",
    "OnProxyRelay19",
    "OnProxyRelay20",
    "OnProxyRelay21",
    "OnProxyRelay22",
    "OnProxyRelay23",
    "OnProxyRelay24",
    "OnProxyRelay25",
    "OnProxyRelay26",
    "OnProxyRelay27",
    "OnProxyRelay28",
    "OnProxyRelay29",
    "OnProxyRelay30",
];

/// `CFuncInstanceIoProxy` (`game/server/func_instance_io_proxy.cpp`) — thirty
/// pass-through relays and nothing else.
///
/// 1,184 of them in the shipped maps, the thirteenth commonest entity in the
/// game, and it has **no state at all**: input *n* fires output *n*. The whole
/// class is its output table, which is why it is 312 lines of C++ and one
/// struct here.
pub struct InstanceIoProxy;

impl InstanceIoProxy {
    fn create() -> Box<dyn Behaviour> {
        Box::new(InstanceIoProxy)
    }
}

impl Behaviour for InstanceIoProxy {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::entity::Entity;
    use crate::server::keyvalue;

    #[test]
    fn lookup_is_case_insensitive_and_knows_nothing_it_was_not_given() {
        assert!(lookup("logic_relay").is_some());
        assert!(lookup("LOGIC_RELAY").is_some());
        assert!(lookup("prop_dynamic").is_none());
        assert_eq!(lookup("light_spot").map(|c| c.name), Some("light_spot"));
    }

    /// No class may declare a name twice, and no two classes may share one.
    #[test]
    fn the_class_table_is_well_formed() {
        let mut names: Vec<&str> = CLASSES.iter().map(|c| c.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate classname in the table");

        for class in CLASSES {
            let mut keys: Vec<String> = class.keys.iter().map(|k| k.to_ascii_lowercase()).collect();
            keys.sort();
            let before = keys.len();
            keys.dedup();
            assert_eq!(keys.len(), before, "{}: duplicate key", class.name);

            let mut outputs: Vec<String> = class
                .outputs
                .iter()
                .map(|o| o.to_ascii_lowercase())
                .collect();
            outputs.sort();
            let before = outputs.len();
            outputs.dedup();
            assert_eq!(outputs.len(), before, "{}: duplicate output", class.name);
        }
    }

    /// The declaration and the implementation must agree: every name in
    /// [`ClassDef::keys`] has to be a name `key_value` actually takes, and no
    /// class may consume a key it did not declare. Without this the unhandled
    /// report drifts from the truth in whichever direction the typo went.
    #[test]
    fn every_declared_key_is_consumed_and_every_consumed_key_is_declared() {
        for class in CLASSES {
            for key in class.keys {
                let mut entity = Entity::new(class);
                let Entity { core, behaviour } = &mut entity;
                assert!(
                    behaviour.key_value(core, key, "1"),
                    "{}: declares {key} but key_value refuses it",
                    class.name
                );
            }
            // A key nothing declares must be refused, including one another
            // class in the table owns.
            for other in CLASSES {
                for key in other.keys {
                    if class.declares_key(key) {
                        continue;
                    }
                    let mut entity = Entity::new(class);
                    let Entity { core, behaviour } = &mut entity;
                    assert!(
                        !behaviour.key_value(core, key, "1"),
                        "{}: consumes {key} without declaring it",
                        class.name
                    );
                }
            }
        }
    }

    /// `CLight::Spawn`'s one rule, which removes 6,937 of the shipped game's
    /// 7,150 lights.
    #[test]
    fn an_unnamed_light_removes_itself_and_a_named_one_does_not() {
        let class = lookup("light").expect("registered");

        let mut entity = Entity::new(class);
        let Entity { core, behaviour } = &mut entity;
        assert_eq!(behaviour.spawn(core), SpawnResult::Remove);

        let mut entity = Entity::new(class);
        keyvalue::base_key_value(&mut entity.core, "targetname", "flicker");
        let Entity { core, behaviour } = &mut entity;
        assert_eq!(behaviour.spawn(core), SpawnResult::Ok);
    }

    /// `pitch` writes `angles.x` and leaves the rest, and it is the contained
    /// [`Light`] that does it for a `light_environment` — the composition that
    /// replaced `BaseClass::KeyValue`.
    #[test]
    fn env_light_inherits_the_light_keys_by_holding_one() {
        let mut entity = Entity::new(lookup("light_environment").expect("registered"));
        keyvalue::base_key_value(&mut entity.core, "angles", "0 90 0");
        let Entity { core, behaviour } = &mut entity;

        assert!(behaviour.key_value(core, "pitch", "-45"));
        assert_eq!(core.angles, Vec3::new(-45.0, 90.0, 0.0));
        assert!(behaviour.key_value(core, "style", "33"));
        assert!(behaviour.key_value(core, "_light", "255 200 150 400"));
        assert!(behaviour.key_value(core, "_ambient", "1 2 3 4"), "consumed");
        assert!(!behaviour.key_value(core, "_lightHDR", "1 1 1 1"), "vrad's");

        let env = behaviour
            .downcast_ref::<EnvLight>()
            .expect("light_environment is an EnvLight");
        assert_eq!(env.sun_color, [255, 200, 150, 400u32 as u8]);
        assert_eq!(env.light.style, 33);
    }

    #[test]
    fn the_proxy_declares_thirty_distinct_relays() {
        let class = lookup("func_instance_io_proxy").expect("registered");
        assert_eq!(class.outputs.len(), 30);
        assert!(class.declares_output("onproxyrelay16"));
        assert!(class.declares_output("OnProxyRelay30"));
        // The unnumbered one the FGD shows a mapper is not an output the
        // server has ever had.
        assert!(!class.declares_output("OnProxyRelay"));
    }
}
