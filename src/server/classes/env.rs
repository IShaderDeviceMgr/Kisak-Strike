//! `env_*`: the entities that tell the renderer how to look at the map.
//!
//! Two so far. `env_tonemap_controller` is the one
//! `portdocs/CLIENT_TONEMAP.md` named as the single measured gap in an
//! otherwise complete tone mapper; `env_fade` is how every transition in the
//! game goes to black.

use crate::client::fade::{ScreenFade, FFADE_IN, FFADE_MODULATE, FFADE_OUT, FFADE_PURGE, FFADE_STAYOUT};
use crate::client::tonemap::TonemapSettings;
use crate::server::class::{Behaviour, Context, InputDef, InputDefs};
use crate::server::entity::EntityCore;
use crate::server::io::{FieldType, Input, Variant};
use crate::server::keyvalue::atof;

/// `SF_TONEMAP_MASTER` (`env_tonemap_controller.cpp:18`).
///
/// **105 of the game's 110 controllers carry it**, and the five that do not
/// are exactly the second controller in the five maps that have two — so
/// "the master is the one with the flag" resolves with no tie-break needed.
pub const SF_TONEMAP_MASTER: u32 = 0x0001;

/// `CEnvTonemapController` (`game/server/env_tonemap_controller.cpp:27`) — a
/// map's own exposure limits.
///
/// **110 of them, across 105 of the 106 shipped maps** (`sp_a5_credits` is the
/// exception), and four of its inputs are the 11th-14th commonest inputs in
/// the entire game. It has **no keyvalues at all** beyond the ones every
/// entity has — measured, not assumed: every setting arrives as an input,
/// which is why the entity does nothing until something fires at it and why
/// `logic_auto` mattered before this did.
///
/// Of its twelve inputs, Portal 2's maps use five:
///
/// ```text
///   1683  SetAutoExposureMax        1618  SetTonemapPercentBrightPixels
///   1683  SetAutoExposureMin         462  SetBloomScale
///   1680  SetTonemapRate
/// ```
///
/// `SetTonemapPercentTarget`, `SetTonemapMinAvgLum`, `SetBloomExponent`,
/// `SetBloomSaturation`, `UseDefaultAutoExposure`, `UseDefaultBloomScale` and
/// `SetBloomScaleRange` appear **zero** times. All twelve are implemented
/// anyway, because the class is twelve assignments and leaving seven out would
/// be a gap with no upside — except `SetBloomScaleRange`, see below.
pub struct TonemapController {
    settings: TonemapSettings,
}

impl TonemapController {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(TonemapController {
            // `CEnvTonemapController::CEnvTonemapController` sets exactly the
            // same values the client's no-controller fallback does, which is
            // why one `Default` serves both.
            settings: TonemapSettings::default(),
        })
    }

    /// What this controller is asking for, for
    /// [`ToneMap::set_settings`](crate::client::tonemap::ToneMap::set_settings).
    pub fn settings(&self) -> TonemapSettings {
        self.settings
    }

    /// `IsMaster` — `HasSpawnFlags( SF_TONEMAP_MASTER )`.
    pub fn is_master(entity: &EntityCore) -> bool {
        entity.has_spawn_flags(SF_TONEMAP_MASTER)
    }
}

impl Behaviour for TonemapController {
    fn accept_input(
        &mut self,
        _entity: &mut EntityCore,
        input: &Input<'_>,
        _cx: &mut Context<'_>,
    ) -> bool {
        let is = |name: &str| input.name.eq_ignore_ascii_case(name);
        let s = &mut self.settings;

        if is("SetAutoExposureMin") {
            s.custom_auto_exposure_min = input.value.float();
            s.use_custom_auto_exposure_min = true;
        } else if is("SetAutoExposureMax") {
            s.custom_auto_exposure_max = input.value.float();
            s.use_custom_auto_exposure_max = true;
        } else if is("UseDefaultAutoExposure") {
            s.use_custom_auto_exposure_min = false;
            s.use_custom_auto_exposure_max = false;
        } else if is("SetBloomScale") {
            s.custom_bloom_scale = input.value.float();
            s.custom_bloom_scale_minimum = s.custom_bloom_scale;
            s.use_custom_bloom_scale = true;
        } else if is("UseDefaultBloomScale") {
            s.use_custom_bloom_scale = false;
        } else if is("SetBloomExponent") {
            s.bloom_exponent = input.value.float();
        } else if is("SetBloomSaturation") {
            s.bloom_saturation = input.value.float();
        } else if is("SetTonemapPercentTarget") {
            s.percent_target = input.value.float();
        } else if is("SetTonemapPercentBrightPixels") {
            s.percent_bright_pixels = input.value.float();
        } else if is("SetTonemapMinAvgLum") {
            s.min_avg_lum = input.value.float();
        } else if is("SetTonemapRate") {
            s.rate = input.value.float();
        } else if is("SetBloomScaleRange") {
            // `InputSetBloomScaleRange` (`env_tonemap_controller.cpp:159`) is
            // **broken in the shipped source and is not reproduced**:
            //
            // ```cpp
            // int nargs = sscanf("%f %f", inputdata.value.String(), bloom_max, bloom_min);
            // ...
            // m_flCustomBloomScale = bloom_max;
            // m_flCustomBloomScale = bloom_min;
            // ```
            //
            // The format string and the buffer are passed the wrong way
            // round, the two floats are passed by value where `sscanf` wants
            // pointers, and then the same field is assigned twice. It cannot
            // have worked; `nargs` is never 2, so it always takes the warning
            // branch and returns. **Zero shipped connections fire it.** What
            // is reproduced is the observable behaviour — the warning branch —
            // rather than the undefined behaviour that precedes it.
            eprintln!(
                "source-engine: server: env_tonemap_controller received SetBloomScaleRange, \
                 which does nothing in the shipped game either"
            );
        } else {
            return false;
        }
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        let s = &self.settings;
        let custom = |on: bool, value: f32| match on {
            true => value.to_string(),
            false => String::from("(cvar)"),
        };
        vec![
            (
                "AutoExposureMin",
                custom(s.use_custom_auto_exposure_min, s.custom_auto_exposure_min),
            ),
            (
                "AutoExposureMax",
                custom(s.use_custom_auto_exposure_max, s.custom_auto_exposure_max),
            ),
            ("TonemapRate", s.rate.to_string()),
            ("TonemapPercentTarget", s.percent_target.to_string()),
            (
                "TonemapPercentBrightPixels",
                s.percent_bright_pixels.to_string(),
            ),
            ("TonemapMinAvgLum", s.min_avg_lum.to_string()),
            (
                "BloomScale",
                custom(s.use_custom_bloom_scale, s.custom_bloom_scale),
            ),
        ]
    }
}

/// The twelve inputs `CEnvTonemapController` declares
/// (`env_tonemap_controller.cpp:91`).
///
/// Eleven `FIELD_FLOAT` and two `FIELD_VOID`. The float declarations are what
/// make `SetAutoExposureMax 1.5` work at all: the parameter arrives as the
/// string `"1.5"` and `AcceptInput` converts it before the handler sees it, so
/// `input.value.float()` is a float and not zero.
pub(super) static TONEMAP_INPUTS: InputDefs = &[
    InputDef::new("SetTonemapRate", FieldType::Float),
    InputDef::new("SetAutoExposureMin", FieldType::Float),
    InputDef::new("SetAutoExposureMax", FieldType::Float),
    InputDef::new("UseDefaultAutoExposure", FieldType::Void),
    InputDef::new("UseDefaultBloomScale", FieldType::Void),
    InputDef::new("SetBloomScale", FieldType::Float),
    InputDef::new("SetBloomScaleRange", FieldType::Float),
    InputDef::new("SetBloomExponent", FieldType::Float),
    InputDef::new("SetBloomSaturation", FieldType::Float),
    InputDef::new("SetTonemapPercentTarget", FieldType::Float),
    InputDef::new("SetTonemapPercentBrightPixels", FieldType::Float),
    InputDef::new("SetTonemapMinAvgLum", FieldType::Float),
];

/// `SF_FADE_IN` (`EnvFade.cpp:67`) — fade from the colour rather than to it.
pub const SF_FADE_IN: u32 = 0x0001;
/// `SF_FADE_MODULATE` — multiply rather than blend.
pub const SF_FADE_MODULATE: u32 = 0x0002;
/// `SF_FADE_ONLYONE` — fade the activator's screen only, if it is a player.
pub const SF_FADE_ONLYONE: u32 = 0x0004;
/// `SF_FADE_STAYOUT` — stay faded until something replaces it.
pub const SF_FADE_STAYOUT: u32 = 0x0008;

pub static FADE_KEYS: &[&str] = &["duration", "holdtime", "ReverseFadeDuration"];

pub static FADE_INPUTS: InputDefs = &[
    InputDef::new("Fade", FieldType::Void),
    InputDef::new("FadeReverse", FieldType::Void),
];

/// `CEnvFade` (`game/server/EnvFade.cpp`) — fades every player's screen to,
/// or from, its render colour.
///
/// **327 of them, on 105 of the 106 maps**, and every transition in the game
/// goes through one: `@transition_from_map` fires `exit_fade` — 0.3 s to
/// black, `SF_FADE_STAYOUT` — in the same breath as the script that fires
/// `@changelevel`, so the level is left in the dark and the next one's
/// `LevelInit` lifts it. 326 of the game's connections to one fire `Fade`
/// and 5 fire `FadeReverse`; 324 are black and two fade to white.
///
/// The colour is `m_clrRender` — `rendercolor` and `renderamt`, which every
/// entity parses — so there is nothing to read here but the two times and
/// the reverse duration.
pub struct EnvFade {
    duration: f32,
    hold_time: f32,
    reverse_duration: f32,
    /// `m_flFadeStartTime`, which `FadeReverse` reads to start where `Fade`
    /// has got to. Zero until the first `Fade`.
    ///
    /// Its mirror, `m_flReverseFadeStartTime`, is not kept: `FadeReverse`
    /// writes it and the only reader is `InputFade`'s commented-out version of
    /// the same anti-pop, so in the shipped game it is never read.
    fade_start: f32,
}

impl EnvFade {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(EnvFade {
            duration: 0.0,
            hold_time: 0.0,
            reverse_duration: 0.0,
            fade_start: 0.0,
        })
    }

    /// The flags both inputs build: the direction `SF_FADE_IN` asks for when
    /// `forward` — `Fade` — and the other one for `FadeReverse`, then
    /// modulate and stay-out as the spawnflags say.
    fn flags(entity: &EntityCore, forward: bool) -> u16 {
        let fading_in = entity.has_spawn_flags(SF_FADE_IN) == forward;
        let mut flags = if fading_in { FFADE_IN } else { FFADE_OUT };
        if entity.has_spawn_flags(SF_FADE_MODULATE) {
            flags |= FFADE_MODULATE;
        }
        if entity.has_spawn_flags(SF_FADE_STAYOUT) {
            flags |= FFADE_STAYOUT;
        }
        flags
    }

    /// `UTIL_ScreenFade` to the activator for `SF_FADE_ONLYONE`, else
    /// `UTIL_ScreenFadeAll` with `FFADE_PURGE` added.
    fn send(
        entity: &EntityCore,
        input: &Input<'_>,
        color: [u8; 4],
        duration: f32,
        hold: f32,
        flags: u16,
        cx: &mut Context<'_>,
    ) {
        if entity.has_spawn_flags(SF_FADE_ONLYONE) {
            // `pActivator->IsNetClient()`: only a player's own screen, and
            // only if a player is what fired it.
            if input.activator.is_some() && input.activator == cx.player() {
                cx.screen_fade(ScreenFade::new(color, duration, hold, flags));
            }
        } else {
            cx.screen_fade(ScreenFade::new(color, duration, hold, flags | FFADE_PURGE));
        }
    }
}

impl Behaviour for EnvFade {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("duration") {
            self.duration = atof(value);
        } else if is("holdtime") {
            self.hold_time = atof(value);
        } else if is("ReverseFadeDuration") {
            self.reverse_duration = atof(value);
        } else {
            return false;
        }
        true
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let me = entity.id();
        if input.name.eq_ignore_ascii_case("Fade") {
            let flags = EnvFade::flags(entity, true);
            let color = entity.render_color;
            EnvFade::send(entity, input, color, self.duration, self.hold_time, flags, cx);
            self.fade_start = cx.curtime();
            entity.fire_output("OnBeginFade", Variant::Void, input.activator, Some(me), 0.0, cx);
            return true;
        }
        if input.name.eq_ignore_ascii_case("FadeReverse") {
            // The other direction, at the reverse duration.
            let flags = EnvFade::flags(entity, false);
            let mut color = entity.render_color;
            // "Change the fade alpha to match the alpha of the current fade to
            // prevent a pop" — a reverse part way through a forward fade
            // starts from where that fade had got to. The `u8` truncation is
            // `color32::a`'s.
            if self.fade_start != 0.0 {
                let elapsed = cx.curtime() - self.fade_start;
                if elapsed < self.duration {
                    color[3] = (f32::from(color[3]) * elapsed / self.duration) as u8;
                }
            }
            EnvFade::send(entity, input, color, self.reverse_duration, self.hold_time, flags, cx);
            entity.fire_output("OnBeginFade", Variant::Void, input.activator, Some(me), 0.0, cx);
            return true;
        }
        false
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("duration", self.duration.to_string()),
            ("holdtime", self.hold_time.to_string()),
            ("ReverseFadeDuration", self.reverse_duration.to_string()),
        ]
    }
}
