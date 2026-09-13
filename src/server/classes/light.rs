//! The light family. `game/server/lights.cpp`.
//!
//! **7,150 of the shipped game's 60,925 entities**, the second largest family
//! after `logic_relay` — and 6,937 of them delete themselves the instant they
//! spawn, because `vrad` has already baked their whole contribution into the
//! lightmaps this port reads. The 213 that survive are the ones a map can
//! switch on and off by name.

use crate::server::class::{Behaviour, Context, InputDef, InputDefs, SpawnResult};
use crate::server::entity::EntityCore;
use crate::server::io::{FieldType, Input};
use crate::server::keyvalue::{atof, atoi, string_to_color32};

/// `SF_LIGHT_START_OFF` (`game/server/util.h:511`).
///
/// **Written back into `m_spawnflags` at run time**, which is unusual: a
/// spawnflag is normally a mapper's constant, and `CLight::TurnOn`/`TurnOff`
/// clear and set this one as the light's live on/off state. Reproduced,
/// because `Use`'s `ShouldToggle` reads it back.
const SF_LIGHT_START_OFF: u32 = 1;

/// What `CLight` consumes. `defaultstyle` is in its datadesc and appears
/// **zero** times in the shipped maps, so it is left out with the rest of the
/// unreachable keys.
///
/// Everything else a `light` carries — `_light`, `_lightHDR`,
/// `_lightscaleHDR`, `_quadratic_attn` and the six other falloff keys — is
/// `vrad`'s (`utils/vbsp/map.cpp` reads them at compile time) and has no
/// run-time consumer in Valve's engine either. They show up in the unhandled
/// report, correctly: nothing handles them, by design.
pub(super) static LIGHT_KEYS: &[&str] = &["style", "pattern", "pitch"];

/// `CLight` (`game/server/lights.cpp:16`) — `light`, `light_spot`,
/// `light_glspot` and `light_directional`.
pub struct Light {
    /// `m_iStyle` — the lightstyle slot. **Only values `>= 32` are
    /// switchable**; below that the style is one of `vrad`'s animated presets
    /// and every input on this class silently does nothing.
    pub style: i32,
    /// `m_iszPattern` — the lightstyle animation string, a letter per frame
    /// from `a` (off) to `z`. 31 entities in the whole game.
    pub pattern: Option<String>,
}

impl Light {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(Light::new())
    }

    fn new() -> Light {
        Light {
            style: 0,
            pattern: None,
        }
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
    /// survives is the 213 lights a map can switch on and off by name.
    ///
    /// The lightstyle half — `engine->LightStyle( m_iStyle, ... )` — needs
    /// animated lightstyles, which `world/` has not got: the lightmap atlas
    /// holds `vrad`'s bake and nothing re-uploads it. The entity is therefore
    /// fully alive and fully correct about its own state, and the only thing
    /// missing is the one call that would make a wall change colour. That is
    /// `world/`'s to add, not this module's.
    fn spawn(&mut self, entity: &mut EntityCore) -> SpawnResult {
        match entity.name.is_some() {
            true => SpawnResult::Ok,
            false => SpawnResult::Remove,
        }
    }

    /// `CLight::TurnOn`/`TurnOff`/`Toggle` and the three inputs that call
    /// them (`lights.cpp:98`).
    ///
    /// > **Every one of these is gated on `m_iStyle >= 32` in effect, and none
    /// > of them tests it.** `CLight::Use` tests the style; the *inputs* do
    /// > not, so `TurnOff` on a style-0 light still sets `SF_LIGHT_START_OFF`
    /// > and still calls `engine->LightStyle(0, "a")` — which overwrites
    /// > lightstyle slot 0, the one every un-animated surface in the map uses.
    /// > That is Valve's, it is almost certainly a bug, and it costs nothing
    /// > to reproduce because the `LightStyle` call does not exist here.
    fn key_input(&mut self, entity: &mut EntityCore, input: &Input<'_>) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);

        if is("TurnOn") {
            entity.spawn_flags &= !SF_LIGHT_START_OFF;
        } else if is("TurnOff") {
            entity.spawn_flags |= SF_LIGHT_START_OFF;
        } else if is("Toggle") {
            entity.spawn_flags ^= SF_LIGHT_START_OFF;
        } else if is("SetPattern") {
            self.pattern = Some(input.value.to_string());
            // "Light is on if pattern is set".
            entity.spawn_flags &= !SF_LIGHT_START_OFF;
        } else if is("FadeToPattern") {
            // `CLight::FadeThink` walks one lightstyle letter per 0.1 s from
            // the old pattern's first character to the new one's. Without
            // `engine->LightStyle` the walk has no observable effect, so the
            // end state is applied at once and the think is not scheduled.
            // **Zero connections in the shipped maps fire it.**
            self.pattern = Some(input.value.to_string());
            entity.spawn_flags &= !SF_LIGHT_START_OFF;
        } else {
            return false;
        }
        true
    }

    /// Whether this light is currently on.
    ///
    /// **Not a field**: Valve keeps the live on/off state in
    /// `SF_LIGHT_START_OFF`, so the entity's spawnflags are the answer and
    /// this is a reader rather than a mirror. `ent_dump` prints the
    /// spawnflags; the light tests call this.
    #[allow(dead_code)]
    pub fn is_on(&self, entity: &EntityCore) -> bool {
        !entity.has_spawn_flags(SF_LIGHT_START_OFF)
    }
}

impl Behaviour for Light {
    fn key_value(&mut self, entity: &mut EntityCore, key: &str, value: &str) -> bool {
        Light::key_value(self, entity, key, value)
    }

    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        Light::spawn(self, entity)
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        _cx: &mut Context<'_>,
    ) -> bool {
        Light::key_input(self, entity, input)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        // `describe` has no entity, so the spawnflag-derived state is not
        // reachable here; `ent_dump` prints the spawnflags itself.
        let mut out = vec![("style", self.style.to_string())];
        if let Some(pattern) = &self.pattern {
            out.push(("pattern", pattern.clone()));
        }
        out
    }
}

/// The five inputs `CLight` declares (`lights.cpp:31`).
///
/// Measured: Portal 2's maps fire three of them — `TurnOn` (157), `TurnOff`
/// (103) and `SetPattern` (6). `Toggle` and `FadeToPattern` are never fired.
pub(super) static LIGHT_INPUTS: InputDefs = &[
    InputDef::new("SetPattern", FieldType::String),
    InputDef::new("FadeToPattern", FieldType::String),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("TurnOn", FieldType::Void),
    InputDef::new("TurnOff", FieldType::Void),
];

/// What `CEnvLight` adds to `CLight`.
pub(super) static ENV_LIGHT_KEYS: &[&str] = &["_light", "_ambient", "style", "pattern", "pitch"];

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
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(EnvLight {
            light: Light::new(),
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

    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        self.light.spawn(entity)
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        _cx: &mut Context<'_>,
    ) -> bool {
        self.light.key_input(entity, input)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = Behaviour::describe(&self.light);
        let c = self.sun_color;
        out.push(("_light", format!("{} {} {} {}", c[0], c[1], c[2], c[3])));
        out
    }
}

/// Reachable from a test, and from nowhere else — the contained [`Light`] is
/// private so that nothing outside can set it without going through
/// `key_value`.
impl EnvLight {
    #[cfg(test)]
    pub(super) fn light(&self) -> &Light {
        &self.light
    }
}
