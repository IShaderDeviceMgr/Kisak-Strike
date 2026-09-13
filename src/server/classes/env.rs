//! `env_*`: the entities that tell the renderer how to look at the map.
//!
//! One so far, and it is the one `portdocs/CLIENT_TONEMAP.md` named as the
//! single measured gap in an otherwise complete tone mapper.

use crate::client::tonemap::TonemapSettings;
use crate::server::class::{Behaviour, Context, InputDef, InputDefs};
use crate::server::entity::EntityCore;
use crate::server::io::{FieldType, Input};

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
