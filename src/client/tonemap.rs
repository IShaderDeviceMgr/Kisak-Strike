//! Auto exposure: how bright to make the frame, given how bright the last one
//! came out.
//!
//! `CTonemapSystem` (`game/client/viewpostprocess.cpp:702`) — the policy half
//! of `DoTonemapping`. Everything here is arithmetic over a histogram and a
//! handful of cvars; **it names no GPU type**, because the measurement is the
//! material system's and lives in
//! [`materials::histogram`](crate::materials::histogram). The split is the same
//! one `console/` makes between `Console::complete` and `ConsoleUi`: the
//! question is one module's, the widget is another's.
//!
//! # Why a map is too dark without this
//!
//! Portal 2 ships HDR-only lighting: `vrad` writes linear radiance into
//! `LUMP_LIGHTING_HDR` with a range well past 1.0, and the shaders multiply it
//! by `cLightScale.x` on the way out (`FinalOutput`, `common_ps_fxc.h:370`).
//! With no exposure controller that scalar is 1.0 and the picture is exactly as
//! bright as the compiler left it, which is not how the game was ever meant to
//! be looked at. This is the controller.
//!
//! # The loop
//!
//! ```text
//!   frame N     scale()  ------> cLightScale.x -> the scene is drawn exposed
//!                                                        |
//!                                                 histogram of that frame
//!                                                        |
//!   frame N+2   measured(counts, dt) <--------------------+
//! ```
//!
//! **The measurement is of an already-exposed frame**, which is why
//! [`ToneMap::measured`] multiplies its answer by the scale currently in force
//! rather than replacing it (`ComputeTargetTonemapScalar`'s "Apply this against
//! last frames scalar"). Reading it as an absolute exposure makes the loop
//! diverge rather than converge, and it is the single easiest thing to get
//! wrong here.
//!
//! # What is not ported
//!
//! - **`mat_tonemap_algorithm 0`**, the original 31-bucket log-spaced
//!   algorithm. `UpdateBucketRanges` selects it by matching the game directory
//!   against `{"dod", "cstrike", "lostcoast"}`, so for Portal 2 it is
//!   unreachable, and it computes a different target from a different histogram
//!   (`0.005 / averageLuminance`) with a different bucket count. The cvar is not
//!   registered either: one that cannot change anything is worse than none.
//! - **`env_tonemap_controller`**. A map's own exposure limits, rate and
//!   percentage targets arrive over the wire from a server entity
//!   (`c_env_tonemap_controller.cpp:97`), and there are no entities. The
//!   constants here are the *no-controller* fallback that same function
//!   installs — note that its `g_flTonemapPercentTarget` is **65**, where the
//!   file-scope initialiser twenty lines away says 60. 65 is what a running
//!   game uses before a controller says otherwise.
//! - **`SetOverrideTonemapScale`**, which VScript and the commentary system
//!   call. Nothing in this port can reach it, and `mat_force_tonemap_scale`
//!   covers the same ground from the console.
//! - **`mat_fullbright 1`**, which forces the scalar to 1. `mat_fullbright` is
//!   an engine-wide unlit mode, not a tone-mapping switch, and registering it
//!   here so it did one tenth of its job would be worse than leaving it out.
//! - **The histogram overlay** (`DisplayHistogram`, `mat_show_histogram`), 200
//!   lines of `Viewport` + `ClearBuffers` used as a bar chart. The `tonemap`
//!   console command prints the same numbers.

use crate::engine::console::{Console, Cvar, CvarFlags};

/// How many luminance buckets the histogram is divided into.
///
/// `NUM_HISTOGRAM_BUCKETS_NEW - 1` (`viewpostprocess.cpp:546`). Valve's
/// seventeenth bucket is not a bucket: it spans `[0, 100000]` and exists only to
/// calibrate the occlusion-query pixel counts, which "some boards (nvidia)
/// return larger when using AA". A compute pass counts pixels exactly, so there
/// is nothing to calibrate and the bucket is deleted.
pub const BUCKETS: usize = 16;

/// `g_flTonemapPercentTarget`: where in the luminance range the bright end of
/// the picture should sit, as a percentage.
///
/// **65, not 60.** `GetTonemapSettingsFromEnvTonemapController`'s no-controller
/// fallback (`c_env_tonemap_controller.cpp:128`) is what a running game
/// installs every frame; the 60 at `viewpostprocess.cpp:66` is only ever the
/// value before the first frame.
const TONEMAP_PERCENT_TARGET: f32 = 65.0;

/// `g_flTonemapPercentBrightPixels`: what fraction of the picture, as a
/// percentage, is allowed to be brighter than the target.
const TONEMAP_PERCENT_BRIGHT_PIXELS: f32 = 2.0;

/// `g_flTonemapMinAvgLum`: the floor under the *median* luminance, as a
/// percentage. Stops a scene with a few bright highlights and a lot of darkness
/// from being exposed for the highlights alone.
const TONEMAP_MIN_AVG_LUM: f32 = 3.0;

/// `g_flTonemapRate`: how fast the current scale chases the target, per second.
const TONEMAP_RATE: f32 = 1.0;

/// What `mat_tonemap_algorithm 1` multiplies the rate by, "so it feels the same
/// as the original" (`viewpostprocess.cpp:1481`). Folded in as a constant
/// because there is only one algorithm here.
const TONEMAP_RATE_SCALE: f32 = 2.0;

/// How many recent targets the weighted average is taken over.
const MOVING_AVERAGE: usize = 10;

/// The exponent that puts more buckets at the dark end.
///
/// `UpdateBucketRanges` takes an even split of `[0, 1]` and raises each boundary
/// to this — "Use a distribution with slightly more bins in the low range".
const BUCKET_EXPONENT: f32 = 2.5;

/// The luminance boundaries of the histogram: `BUCKETS + 1` ascending values
/// from 0 to 1, where bucket `i` covers `[bounds[i], bounds[i + 1])`.
///
/// `CTonemapSystem::UpdateBucketRanges`' `mat_tonemap_algorithm 1` branch
/// (`viewpostprocess.cpp:1085`), which is `(i / 16)^2.5`.
///
/// **These are linear-light luminances, not gamma-space ones.** Valve's comment
/// at `CHistogramBucket::IssueQuery` says "gamma-space text range" and is
/// stale: `dev/lumcompare.vmt` leaves `$LINEARREAD_BASETEXTURE` unset, so
/// `screenspace_general` enables an sRGB read on its copy of the frame buffer
/// and the luminance it compares is linear. Reading them as gamma boundaries
/// puts the target at 0.32 linear instead of 0.65 and halves every scene.
pub fn bucket_bounds() -> [f32; BUCKETS + 1] {
    let mut bounds = [0.0f32; BUCKETS + 1];
    for (i, bound) in bounds.iter_mut().enumerate() {
        *bound = (i as f32 / BUCKETS as f32).powf(BUCKET_EXPONENT);
    }
    bounds
}

/// Everything the tone mapper reads out of the cvar system. One handle per
/// cvar, per `ENGINE_CONSOLE.md` §6.1.
struct Cvars {
    mat_dynamic_tonemapping: Cvar,
    mat_autoexposure_min: Cvar,
    mat_autoexposure_max: Cvar,
    mat_autoexposure_max_multiplier: Cvar,
    mat_hdr_uncapexposure: Cvar,
    mat_force_tonemap_scale: Cvar,
    mat_accelerate_adjust_exposure_down: Cvar,
    mat_exposure_center_region_x: Cvar,
    mat_exposure_center_region_y: Cvar,
    mat_force_tonemap_percent_target: Cvar,
    mat_force_tonemap_percent_bright_pixels: Cvar,
    mat_force_tonemap_min_avglum: Cvar,
}

/// The exposure controller.
pub struct ToneMap {
    cvars: Cvars,
    /// `m_flCurrentTonemapScale` — what the shaders are multiplying by now.
    current: f32,
    /// `m_flTargetTonemapScale` — what they are heading towards.
    target: f32,
    /// `m_movingAverageTonemapScale`, oldest first.
    average: [f32; MOVING_AVERAGE],
    /// `m_nNumMovingAverageValid`.
    average_valid: usize,
    /// The last measurement folded in, kept for the `tonemap` command.
    histogram: [u32; BUCKETS],
    /// Whether [`histogram`](ToneMap::histogram) holds a measurement at all.
    /// `CHistogramBucket::ContainsValidData` for all sixteen at once: the
    /// compute pass fills every bucket or none, where Valve's one-query-a-frame
    /// filled them one at a time.
    measured: bool,
}

impl ToneMap {
    /// Registers the tone mapper's cvars, exposing nothing.
    ///
    /// Names, defaults and flags are Valve's. `FCVAR_DEVELOPMENTONLY` on the
    /// two that carry it is dropped in favour of `FCVAR_CHEAT`, which is what
    /// the rest carry and what this port has a flag for.
    pub fn new(console: &mut Console<'_>) -> ToneMap {
        let cvars = Cvars {
            mat_dynamic_tonemapping: console.cvar(
                "mat_dynamic_tonemapping",
                "1",
                CvarFlags::CHEAT,
                "Adjust the exposure to what is on screen. 0 leaves it where it is.",
            ),
            mat_autoexposure_min: console.cvar(
                "mat_autoexposure_min",
                "0.5",
                CvarFlags::CHEAT,
                "The darkest the tone mapper may expose.",
            ),
            mat_autoexposure_max: console.cvar(
                "mat_autoexposure_max",
                "2",
                CvarFlags::CHEAT,
                "The brightest the tone mapper may expose.",
            ),
            mat_autoexposure_max_multiplier: console.cvar(
                "mat_autoexposure_max_multiplier",
                "1.0",
                CvarFlags::CHEAT,
                "Scales mat_autoexposure_max.",
            ),
            mat_hdr_uncapexposure: console.cvar(
                "mat_hdr_uncapexposure",
                "0",
                CvarFlags::CHEAT,
                "Ignore the exposure limits and allow 0 to 100.",
            ),
            mat_force_tonemap_scale: console.cvar(
                "mat_force_tonemap_scale",
                "0.0",
                CvarFlags::CHEAT,
                "Pin the exposure to this value. 0 lets the tone mapper choose.",
            ),
            mat_accelerate_adjust_exposure_down: console.cvar(
                "mat_accelerate_adjust_exposure_down",
                "40.0",
                CvarFlags::CHEAT,
                "How much faster to darken than to brighten.",
            ),
            mat_exposure_center_region_x: console.cvar(
                "mat_exposure_center_region_x",
                "0.9",
                CvarFlags::CHEAT,
                "Fraction of the screen's width the exposure is measured over.",
            ),
            mat_exposure_center_region_y: console.cvar(
                "mat_exposure_center_region_y",
                "0.85",
                CvarFlags::CHEAT,
                "Fraction of the screen's height the exposure is measured over.",
            ),
            mat_force_tonemap_percent_target: console.cvar(
                "mat_force_tonemap_percent_target",
                "-1",
                CvarFlags::CHEAT,
                "Override. Old default was 60.",
            ),
            mat_force_tonemap_percent_bright_pixels: console.cvar(
                "mat_force_tonemap_percent_bright_pixels",
                "-1",
                CvarFlags::CHEAT,
                "Override. Old value was 2.0",
            ),
            mat_force_tonemap_min_avglum: console.cvar(
                "mat_force_tonemap_min_avglum",
                "-1",
                CvarFlags::CHEAT,
                "Override. Old default was 3.0",
            ),
        };
        ToneMap {
            cvars,
            current: 1.0,
            target: 1.0,
            average: [1.0; MOVING_AVERAGE],
            average_valid: 0,
            histogram: [0; BUCKETS],
            measured: false,
        }
    }

    /// What the material system should multiply lit output by this frame:
    /// `UpdateMaterialSystemTonemapScalar` (`viewpostprocess.cpp:1317`).
    ///
    /// Takes `&mut self` because `mat_force_tonemap_scale` does not merely
    /// report a different number — it *resets* the controller onto that number,
    /// so that clearing the cvar again resumes from where the picture actually
    /// is rather than from wherever the controller had drifted meanwhile.
    pub fn scale(&mut self) -> f32 {
        let forced = self.cvars.mat_force_tonemap_scale.float();
        if forced > 0.0 {
            self.reset(forced);
        }
        self.current
    }

    /// `ResetToneMapping` (`viewpostprocess.cpp:1419`): put the exposure
    /// somewhere and forget the history.
    ///
    /// Called on level load, where the engine's value is 1.0
    /// (`cdll_client_int.cpp:2470`).
    ///
    /// A `scale` of zero or less means "the middle of the exposure range,
    /// clamped to 1..10" — Valve's comment calls it an L4D hack for a game
    /// whose lighting was dark enough that 1.0 was a bad place to restart from.
    /// Its one caller is a spectator-target change (`c_baseplayer.cpp:787`),
    /// which this port has no way to reach; the branch is kept because it is
    /// what the parameter *means*, and deleting it would leave a `f32` with an
    /// undocumented forbidden range.
    pub fn reset(&mut self, scale: f32) {
        let scale = if scale > 0.0 {
            scale
        } else {
            let (min, max) = self.exposure_range();
            ((min + max) * 0.5).clamp(1.0, 10.0)
        };
        self.current = scale;
        self.target = scale;
        self.average_valid = 0;
    }

    /// `mat_dynamic_tonemapping`: whether a measurement should be taken at all.
    ///
    /// With it off the exposure stays exactly where it was, which is Valve's
    /// behaviour and is not the same as forcing it to 1.
    pub fn measuring(&self) -> bool {
        self.cvars.mat_dynamic_tonemapping.bool()
    }

    /// The fraction of the frame's width and height the exposure is measured
    /// over: `mat_exposure_center_region_x` and `_y`.
    pub fn exposure_region(&self) -> (f32, f32) {
        (
            self.cvars.mat_exposure_center_region_x.float(),
            self.cvars.mat_exposure_center_region_y.float(),
        )
    }

    /// Folds a new measurement in and advances the exposure: the body of
    /// `DoTonemapping` (`viewpostprocess.cpp:2371`).
    ///
    /// `counts` is one pixel count per bucket, in the order
    /// [`bucket_bounds`] describes. `dt` is `gpGlobals->frametime`.
    ///
    /// # Panics
    ///
    /// If `counts` is not [`BUCKETS`] long — the bucket table and the
    /// measurement are two halves of one decision, and a length mismatch means
    /// they have come apart.
    pub fn measured(&mut self, counts: &[u32], dt: f32) {
        assert_eq!(counts.len(), BUCKETS, "histogram is the wrong width");
        if !self.measuring() {
            return;
        }
        self.histogram.copy_from_slice(counts);
        self.measured = true;

        let (min, max) = self.exposure_range();
        let target = self.target_scale();
        // `MAX( min, MIN( max, target ) )` and then "Don't let this go to 0!",
        // in that order: `mat_hdr_uncapexposure` sets the minimum to 0, and the
        // floor is what stops a black screen from pinning the exposure there.
        let clamped = target.min(max).max(min).max(0.001);
        self.advance(clamped, dt, min, max);
    }

    /// `ComputeTargetTonemapScalar( false )` (`viewpostprocess.cpp:889`).
    fn target_scale(&self) -> f32 {
        let percent_target = force_or(
            &self.cvars.mat_force_tonemap_percent_target,
            TONEMAP_PERCENT_TARGET,
        );
        let percent_bright = force_or(
            &self.cvars.mat_force_tonemap_percent_bright_pixels,
            TONEMAP_PERCENT_BRIGHT_PIXELS,
        );
        let min_avg_lum = force_or(
            &self.cvars.mat_force_tonemap_min_avglum,
            TONEMAP_MIN_AVG_LUM,
        );

        let mut location = self.percentile(percent_bright, Some(percent_target));
        if location < 0.0 {
            // No usable histogram. Pretend the picture is already where it
            // should be, which makes the scale below exactly 1.
            location = percent_target / 100.0;
        }
        let mut scale = (percent_target / 100.0) / location.max(0.0001);

        // The secondary target: pull the *median* up to `min_avg_lum` if the
        // primary would leave the picture darker than that. Only ever
        // brightens.
        let median = self.percentile(50.0, None);
        if median > 0.0 {
            let by_median = (min_avg_lum / 100.0) / median;
            if by_median > scale {
                scale = by_median;
            }
        }

        // **Against the current scale, not instead of it.** The histogram was
        // taken of a frame that was already exposed by `self.current`, so this
        // is a correction factor and not an exposure.
        (scale * self.current).max(0.001)
    }

    /// `FindLocationOfPercentBrightPixels` (`viewpostprocess.cpp:830`): the
    /// luminance below which all but `percent_bright` percent of the measured
    /// pixels lie.
    ///
    /// Returns `-1.0` — Valve's error code — when there is no histogram to
    /// read, which is what makes the first frames of a level leave the exposure
    /// alone.
    ///
    /// `snap` is the "sticky bin" deadband: if the target percentage falls
    /// inside the same bucket the answer does, the answer *is* the target, so
    /// the scale comes out 1 and the exposure holds still instead of hunting
    /// inside one bucket's worth of luminance.
    fn percentile(&self, percent_bright: f32, snap: Option<f32>) -> f32 {
        if !self.measured {
            return -1.0;
        }
        let total: u32 = self.histogram.iter().sum();
        if total == 0 {
            return -1.0;
        }
        let bounds = bucket_bounds();
        let mut tested = 0.0f32;
        // From the bright end down, which is the direction the question is
        // asked in.
        for bucket in (0..BUCKETS).rev() {
            let needed = percent_bright / 100.0 - tested;
            let fraction = self.histogram[bucket] as f32 / total as f32;
            if fraction >= needed {
                if let Some(snap) = snap {
                    let snap = snap / 100.0;
                    if bounds[bucket] <= snap && bounds[bucket + 1] >= snap {
                        return snap;
                    }
                }
                // Linear inside the bucket. Valve writes this as
                // `1 - (rangeTested + range * share)`; the running range sum
                // telescopes to `bounds[bucket + 1]` exactly, because the
                // buckets tile `[0, 1]` and the last boundary is 1.
                //
                // `fraction` can only be zero here if `needed` is too, which
                // takes `mat_force_tonemap_percent_bright_pixels 0`; the border
                // is then the top of this bucket, and Valve's division would
                // make it NaN.
                let share = if fraction > 0.0 {
                    needed / fraction
                } else {
                    0.0
                };
                let border = bounds[bucket + 1] - (bounds[bucket + 1] - bounds[bucket]) * share;
                return border.clamp(bounds[bucket], bounds[bucket + 1]);
            }
            tested += fraction;
        }
        -1.0
    }

    /// `SetTonemapScale` (`viewpostprocess.cpp:1428`): smooth the target and
    /// step the current scale towards it.
    fn advance(&mut self, target: f32, dt: f32, min: f32, max: f32) {
        if !target.is_finite() {
            return;
        }

        if self.average_valid < MOVING_AVERAGE {
            self.average[self.average_valid] = target;
            self.average_valid += 1;
        } else {
            self.average.rotate_left(1);
            self.average[MOVING_AVERAGE - 1] = target;
        }

        if self.average_valid == MOVING_AVERAGE {
            // Valve's weights are `|i - 5| / 5` over ten samples, so they run
            // 1.0, 0.8, ... 0.0 ... 0.8 — **the middle sample counts for
            // nothing and the oldest counts for most**. That is not what anyone
            // would write on purpose, and it is not a typo to fix: it is the
            // filter every Source game's exposure has been smoothed with, and
            // a flat or recency-weighted average adapts visibly differently.
            let middle = (MOVING_AVERAGE / 2) as i32;
            let mut weighted = 0.0;
            let mut total = 0.0;
            for (i, sample) in self.average.iter().enumerate() {
                let weight = (i as i32 - middle).abs() as f32 / (MOVING_AVERAGE / 2) as f32;
                total += weight;
                weighted += weight * sample;
            }
            self.set_target((weighted / total).clamp(min, max));
        } else {
            self.set_target(target);
        }

        let mut rate = TONEMAP_RATE * TONEMAP_RATE_SCALE;
        if rate == 0.0 {
            // Zero is documented as "instantaneous", and only an
            // `env_tonemap_controller` can set it. Unreachable today, kept
            // because the branch is what makes a rate of zero mean that rather
            // than mean "never move".
            self.current = self.target;
            return;
        }

        if self.target < self.current {
            // Over-exposed: darken faster than we brighten, ramping from the
            // base rate up to `mat_accelerate_adjust_exposure_down` times it
            // over the first 1.5 of over-exposure. `FLerp` is unclamped, which
            // is why the `MIN` is there.
            let accelerate = self.cvars.mat_accelerate_adjust_exposure_down.float();
            rate = (accelerate * rate).min(flerp(
                rate,
                accelerate * rate,
                0.0,
                1.5,
                self.current - self.target,
            ));
        }

        // **The step is capped per frame, not per second**, at a quarter of a
        // bucket's worth. Valve's reason is the rolling sixteen-frame histogram
        // rebuild — "help reduce the tone map scalar riding the wave" — which
        // this port's single-dispatch histogram does not have. It is kept
        // anyway, because it is also what bounds how fast the exposure can move
        // at all, and dropping it would make every adaptation visibly snappier
        // than the shipped game's. The consequence is real and worth knowing:
        // adaptation is frame-rate dependent above about 130 fps.
        let step = (rate * dt.max(0.0)).min(0.25 / BUCKETS as f32);
        let alpha = step.clamp(0.0, 1.0);
        self.current = self.target * alpha + self.current * (1.0 - alpha);
        if !self.current.is_finite() {
            self.current = self.target;
        }

        // `mat_force_tonemap_scale` steps on the result, so that turning it on
        // takes effect on the same frame rather than being smoothed towards.
        let forced = self.cvars.mat_force_tonemap_scale.float();
        if forced > 0.0 {
            self.reset(forced);
        }
    }

    /// `SetTargetTonemappingScale` — the assert is the whole function.
    fn set_target(&mut self, target: f32) {
        if target.is_finite() {
            self.target = target;
        }
    }

    /// `GetExposureRange` (`viewpostprocess.cpp:988`), minus the
    /// `env_tonemap_controller` overrides, which need entities.
    pub fn exposure_range(&self) -> (f32, f32) {
        let mut min = self.cvars.mat_autoexposure_min.float();
        let mut max = self.cvars.mat_autoexposure_max.float()
            * self.cvars.mat_autoexposure_max_multiplier.float();
        if self.cvars.mat_hdr_uncapexposure.bool() {
            min = 0.0;
            max = 100.0;
        }
        if min > max {
            max = min;
        }
        (min, max)
    }

    /// What the shaders are multiplying by right now, without the
    /// `mat_force_tonemap_scale` side effect [`scale`](ToneMap::scale) has.
    pub fn current(&self) -> f32 {
        self.current
    }

    /// What [`current`](ToneMap::current) is heading towards.
    pub fn target(&self) -> f32 {
        self.target
    }

    /// The last measurement, one pixel count per bucket. All zeroes until one
    /// arrives.
    pub fn histogram(&self) -> &[u32; BUCKETS] {
        &self.histogram
    }

    /// The median measured luminance, or `None` if nothing has been measured.
    /// `FindLocationOfPercentBrightPixels( 50 )`, which is what the debug
    /// readout calls "AvgLum".
    pub fn median_luminance(&self) -> Option<f32> {
        let median = self.percentile(50.0, None);
        (median >= 0.0).then_some(median)
    }

    /// Where the bright end of the picture currently sits, and where it is
    /// being aimed: `(actual, wanted)` as fractions of the luminance range.
    /// `None` until something has been measured.
    pub fn bright_end(&self) -> Option<(f32, f32)> {
        let wanted = force_or(
            &self.cvars.mat_force_tonemap_percent_target,
            TONEMAP_PERCENT_TARGET,
        ) / 100.0;
        let percent_bright = force_or(
            &self.cvars.mat_force_tonemap_percent_bright_pixels,
            TONEMAP_PERCENT_BRIGHT_PIXELS,
        );
        // No `snap`: the sticky bin would report the picture as exactly on
        // target whenever it is within a bucket of it, which is the one thing a
        // readout must not do.
        let actual = self.percentile(percent_bright, None);
        (actual >= 0.0).then_some((actual, wanted))
    }
}

/// The `mat_force_tonemap_*` pattern: a negative cvar means "no override".
fn force_or(cvar: &Cvar, default: f32) -> f32 {
    let forced = cvar.float();
    if forced >= 0.0 {
        forced
    } else {
        default
    }
}

/// `FLerp` (`public/mathlib/mathlib.h:1068`). **Unclamped** — the caller is
/// expected to bound it, and `SetTonemapScale` does.
fn flerp(f1: f32, f2: f32, i1: f32, i2: f32, x: f32) -> f32 {
    f1 + (f2 - f1) * (x - i1) / (i2 - i1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::console::Console;

    /// A console with nothing attached, for registering cvars against.
    fn console() -> Console<'static> {
        Console::new(
            Box::new(crate::engine::console::NoConfigFiles),
            Default::default(),
        )
    }

    fn tonemap() -> ToneMap {
        ToneMap::new(&mut console())
    }

    /// Every pixel in one bucket.
    fn only(bucket: usize, pixels: u32) -> [u32; BUCKETS] {
        let mut counts = [0; BUCKETS];
        counts[bucket] = pixels;
        counts
    }

    #[test]
    fn bucket_bounds_tile_zero_to_one_and_ascend() {
        let bounds = bucket_bounds();
        assert_eq!(bounds[0], 0.0);
        assert_eq!(bounds[BUCKETS], 1.0);
        for pair in bounds.windows(2) {
            assert!(pair[0] < pair[1], "{pair:?}");
        }
    }

    #[test]
    fn bucket_bounds_are_valve_s_power_distribution() {
        // Spot values of `pow( i / 16, 2.5 )`, which is the one line of
        // `UpdateBucketRanges` that survives.
        let bounds = bucket_bounds();
        assert!((bounds[8] - 0.176_776_7).abs() < 1e-6, "{}", bounds[8]);
        assert!((bounds[12] - 0.487_139_3).abs() < 1e-6, "{}", bounds[12]);
        // More resolution at the dark end than at the bright: the first four
        // buckets together cover less range than the last one alone.
        assert!(bounds[4] < bounds[16] - bounds[15]);
    }

    #[test]
    fn exposure_starts_at_one_and_stays_there_with_no_measurement() {
        let mut map = tonemap();
        assert_eq!(map.scale(), 1.0);
        assert_eq!(map.median_luminance(), None);
        assert_eq!(map.bright_end(), None);
    }

    #[test]
    fn a_dark_frame_brightens_and_a_bright_frame_darkens() {
        // Everything in bucket 1 — a nearly black picture. The target is above
        // it, so the exposure should climb.
        let mut dark = tonemap();
        for _ in 0..600 {
            dark.measured(&only(1, 10_000), 1.0 / 60.0);
        }
        assert!(dark.current() > 1.5, "{}", dark.current());

        // Everything in the top bucket — a blown-out picture.
        let mut bright = tonemap();
        for _ in 0..600 {
            bright.measured(&only(BUCKETS - 1, 10_000), 1.0 / 60.0);
        }
        assert!(bright.current() < 0.75, "{}", bright.current());
    }

    #[test]
    fn the_exposure_range_bounds_where_it_can_settle() {
        let mut map = tonemap();
        let (min, max) = map.exposure_range();
        assert_eq!((min, max), (0.5, 2.0));
        for _ in 0..2000 {
            map.measured(&only(0, 10_000), 1.0 / 60.0);
        }
        assert!(map.current() <= max + 1e-4, "{}", map.current());
        for _ in 0..2000 {
            map.measured(&only(BUCKETS - 1, 10_000), 1.0 / 60.0);
        }
        assert!(map.current() >= min - 1e-4, "{}", map.current());
    }

    #[test]
    fn mat_hdr_uncapexposure_replaces_both_ends() {
        let mut console = console();
        let map = ToneMap::new(&mut console);
        console
            .cvars()
            .find("mat_hdr_uncapexposure")
            .expect("registered")
            .set_int(1);
        assert_eq!(map.exposure_range(), (0.0, 100.0));
    }

    #[test]
    fn a_minimum_above_the_maximum_widens_the_maximum() {
        let mut console = console();
        let map = ToneMap::new(&mut console);
        console
            .cvars()
            .find("mat_autoexposure_min")
            .expect("registered")
            .set_float(5.0);
        assert_eq!(map.exposure_range(), (5.0, 5.0));
    }

    #[test]
    fn mat_force_tonemap_scale_pins_the_exposure() {
        let mut console = console();
        let mut map = ToneMap::new(&mut console);
        console
            .cvars()
            .find("mat_force_tonemap_scale")
            .expect("registered")
            .set_float(4.0);
        // Above `mat_autoexposure_max`, deliberately: forcing is not clamped.
        assert_eq!(map.scale(), 4.0);
        map.measured(&only(0, 10_000), 1.0 / 60.0);
        assert_eq!(map.scale(), 4.0);
    }

    #[test]
    fn mat_dynamic_tonemapping_zero_freezes_the_exposure_where_it_is() {
        let mut console = console();
        let mut map = ToneMap::new(&mut console);
        for _ in 0..600 {
            map.measured(&only(1, 10_000), 1.0 / 60.0);
        }
        let held = map.current();
        assert!(held > 1.0);
        console
            .cvars()
            .find("mat_dynamic_tonemapping")
            .expect("registered")
            .set_int(0);
        for _ in 0..600 {
            map.measured(&only(BUCKETS - 1, 10_000), 1.0 / 60.0);
        }
        // Not reset to 1: left exactly where it was.
        assert_eq!(map.current(), held);
    }

    #[test]
    fn an_empty_histogram_leaves_the_exposure_alone() {
        let mut map = tonemap();
        for _ in 0..600 {
            map.measured(&[0; BUCKETS], 1.0 / 60.0);
        }
        assert_eq!(map.current(), 1.0);
    }

    #[test]
    fn reset_forgets_the_history() {
        let mut map = tonemap();
        for _ in 0..600 {
            map.measured(&only(1, 10_000), 1.0 / 60.0);
        }
        assert!(map.current() > 1.0);
        map.reset(1.0);
        assert_eq!((map.current(), map.target()), (1.0, 1.0));
        assert_eq!(map.average_valid, 0);
    }

    #[test]
    fn reset_with_a_non_positive_scale_takes_the_middle_of_the_range() {
        let mut map = tonemap();
        // (0.5 + 2.0) / 2 = 1.25, then clamped into 1..10, which does nothing.
        map.reset(-1.0);
        assert_eq!(map.current(), 1.25);
    }

    #[test]
    fn the_percentile_is_linear_inside_the_bucket_it_lands_in() {
        let mut map = tonemap();
        // Half the pixels in the top bucket: the 25%-brightest border sits
        // halfway down it.
        let mut counts = [0u32; BUCKETS];
        counts[BUCKETS - 1] = 50;
        counts[0] = 50;
        map.histogram = counts;
        map.measured = true;

        let bounds = bucket_bounds();
        let top = bounds[BUCKETS] - bounds[BUCKETS - 1];
        let found = map.percentile(25.0, None);
        assert!(
            (found - (bounds[BUCKETS] - top * 0.5)).abs() < 1e-6,
            "{found}"
        );
    }

    #[test]
    fn the_sticky_bin_reports_the_target_exactly() {
        let mut map = tonemap();
        // 65% of the range is inside the last bucket (which starts at 0.824),
        // no — it is inside bucket 13. Put every pixel there and the answer
        // should be the target itself rather than a location inside the bucket.
        let bounds = bucket_bounds();
        let bucket = (0..BUCKETS)
            .find(|&i| bounds[i] <= 0.65 && bounds[i + 1] >= 0.65)
            .expect("0.65 is inside some bucket");
        map.histogram = only(bucket, 1000);
        map.measured = true;
        assert_eq!(map.percentile(2.0, Some(65.0)), 0.65);
        // And so the correction factor is exactly 1: the exposure holds still.
        assert_eq!(map.target_scale(), map.current());
    }

    #[test]
    fn the_target_is_a_correction_to_the_current_scale_and_not_a_replacement() {
        let mut map = tonemap();
        map.histogram = only(0, 1000);
        map.measured = true;
        let from_one = map.target_scale();
        map.current = 2.0;
        assert!((map.target_scale() - from_one * 2.0).abs() < 1e-4);
    }

    #[test]
    fn the_median_floor_only_ever_brightens() {
        let mut map = tonemap();
        // A scene that is on target at the bright end but very dark in the
        // middle: 2% of pixels at the target, the rest at the bottom. The
        // median rule should take over and ask for more exposure than the
        // primary rule's 1.0.
        let bounds = bucket_bounds();
        let target_bucket = (0..BUCKETS)
            .find(|&i| bounds[i] <= 0.65 && bounds[i + 1] >= 0.65)
            .expect("in range");
        let mut counts = [0u32; BUCKETS];
        counts[target_bucket] = 20;
        counts[0] = 980;
        map.histogram = counts;
        map.measured = true;
        assert!(map.target_scale() > 1.0, "{}", map.target_scale());
    }

    /// One step each way from the same distance, at a frame time short enough
    /// that the per-frame cap is not what decides the answer.
    fn one_step(from: f32, to: f32, dt: f32) -> f32 {
        let mut map = tonemap();
        map.reset(from);
        map.average_valid = MOVING_AVERAGE;
        map.average = [to; MOVING_AVERAGE];
        map.advance(to, dt, 0.5, 2.0);
        (map.current() - from).abs()
    }

    #[test]
    fn darkening_is_faster_than_brightening() {
        // `mat_accelerate_adjust_exposure_down` is 40, so an over-exposure is
        // corrected far quicker than the same under-exposure.
        let dt = 0.001;
        let down = one_step(2.0, 0.5, dt);
        let up = one_step(0.5, 2.0, dt);
        assert!(down > up * 5.0, "{down} vs {up}");
    }

    #[test]
    fn the_per_frame_cap_hides_the_accelerated_darkening_below_128_fps() {
        // A finding worth a test rather than a comment: the step cap is
        // `0.25 / 16` **per frame**, and the base rate is 2 per second, so any
        // frame longer than 1/128 s is capped either way and
        // `mat_accelerate_adjust_exposure_down` changes nothing at all. At 60
        // fps — where the game is actually played — darkening and brightening
        // move by exactly the same amount.
        let dt = 1.0 / 60.0;
        assert_eq!(one_step(2.0, 0.5, dt), one_step(0.5, 2.0, dt));
        // Above it, the acceleration reappears.
        let dt = 1.0 / 1000.0;
        assert!(one_step(2.0, 0.5, dt) > one_step(0.5, 2.0, dt));
    }

    #[test]
    fn one_step_is_capped_at_a_quarter_of_a_bucket() {
        // Whatever the frame time, `alpha` never exceeds 0.25 / 16.
        let mut map = tonemap();
        map.average_valid = MOVING_AVERAGE;
        map.average = [2.0; MOVING_AVERAGE];
        map.advance(2.0, 10.0, 0.5, 2.0);
        let alpha = 0.25 / BUCKETS as f32;
        assert!((map.current() - (2.0 * alpha + 1.0 * (1.0 - alpha))).abs() < 1e-6);
    }

    #[test]
    fn the_moving_average_weights_are_valve_s_v_shape() {
        // The middle sample counts for nothing. Two runs that differ only in
        // the sixth sample must land on the same target.
        let mut a = tonemap();
        let mut b = tonemap();
        a.average_valid = MOVING_AVERAGE;
        b.average_valid = MOVING_AVERAGE;
        a.average = [1.0; MOVING_AVERAGE];
        b.average = [1.0; MOVING_AVERAGE];
        // Index 6, not 5: `advance` scrolls the buffer *before* it weights it,
        // so the sample that lands on the zero-weight slot is the one that was
        // one place newer.
        b.average[MOVING_AVERAGE / 2 + 1] = 1000.0;
        a.advance(1.0, 0.0, 0.001, 1000.0);
        b.advance(1.0, 0.0, 0.001, 1000.0);
        assert_eq!(a.target(), b.target());
        // Any other slot does reach the target, so the test above is not
        // passing for want of an effect.
        let mut c = tonemap();
        c.average_valid = MOVING_AVERAGE;
        c.average = [1.0; MOVING_AVERAGE];
        c.average[MOVING_AVERAGE / 2 + 2] = 1000.0;
        c.advance(1.0, 0.0, 0.001, 1000.0);
        assert!(c.target() > a.target());
    }

    #[test]
    fn flerp_is_unclamped() {
        assert_eq!(flerp(0.0, 10.0, 0.0, 1.0, 0.5), 5.0);
        assert_eq!(flerp(0.0, 10.0, 0.0, 1.0, 3.0), 30.0);
    }

    #[test]
    fn a_negative_force_cvar_means_no_override() {
        let mut console = console();
        let map = ToneMap::new(&mut console);
        assert_eq!(
            force_or(
                &map.cvars.mat_force_tonemap_percent_target,
                TONEMAP_PERCENT_TARGET
            ),
            TONEMAP_PERCENT_TARGET
        );
        console
            .cvars()
            .find("mat_force_tonemap_percent_target")
            .expect("registered")
            .set_float(0.0);
        // Zero is an override, not an absence: the test is `>= 0`.
        assert_eq!(
            force_or(
                &map.cvars.mat_force_tonemap_percent_target,
                TONEMAP_PERCENT_TARGET
            ),
            0.0
        );
    }
}
