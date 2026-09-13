# Tone mapping — porting `CTonemapSystem`

**Status: ported.** `src/client/tonemap.rs` is the controller,
`src/materials/{post,histogram}.rs` the measurement, and `rustdocs/CLIENT.md` +
`rustdocs/MATERIALS.md` are how to call them. This document is written *after* the port
rather than before it — the tone mapper was not on `portdocs/CLIENT.md`'s five-stage plan,
because that plan is explicitly the input-and-view layer and this is post-processing — so
read it as the analysis that justifies the shape, not as a plan to follow.

Everything below is relative to `legacy/`.

---

## 0. Headline decisions

1. **The exposure scalar is applied in the shader, not in a post pass.** Source does the
   same, in both HDR modes: `FinalOutput` multiplies by `cLightScale.x`
   (`common_ps_fxc.h:370`) before the sRGB write. Nothing about the tone mapper needs a
   float render target.
2. **The port's frame buffer already *is* `HDR_TYPE_INTEGER`'s** — 8-bit, sRGB, written
   pre-exposed. So `CShaderAPIDx8::SetToneMappingScaleLinear`'s integer arm is the one to
   port and the other two are not.
3. **The scene stops going straight to the back buffer.** The controller has to measure
   the frame it is exposing, and a swap-chain image cannot be sampled. An offscreen scene
   target plus a presenting pass replaces `UpdateScreenEffectTexture` + `_rt_FullFrameFB`.
4. **The histogram is a compute dispatch, not sixteen occlusion queries.** The question
   ("how many pixels have luminance in this range") is domain knowledge; the queries were
   a 2005 encoding of it. `PORTING.md`'s mechanism/format rule applies directly.
5. **Policy and measurement are separate modules and neither names the other's types.**
   `client/tonemap.rs` names no `wgpu` type; `materials/histogram.rs` names no cvar. Same
   split as `Console::complete` versus `ConsoleUi`.
6. **Only `mat_tonemap_algorithm 1` is ported.** Algorithm 0 is selected by matching the
   game directory against `{"dod", "cstrike", "lostcoast"}` and is unreachable for
   Portal 2.

---

## 1. Inventory

| File | Lines | Disposition |
|---|---:|---|
| `game/client/viewpostprocess.cpp` | 4,006 | **~900 in scope.** `CTonemapSystem` (`:702-1530`), `GetExposureRange` (`:988`), `DoTonemapping` (`:2371`) and the head of `DoEnginePostProcessing` (`:2485`). The rest is bloom, blur, colour correction, software AA, FXAA, depth of field, vomit — none ported. |
| `game/client/c_env_tonemap_controller.cpp` | 140 | **Not ported, but read.** Its no-controller fallback (`:97`) is where the constants in `src/client/tonemap.rs` come from. The entity itself needs `server/` and `net/`. |
| `materialsystem/stdshaders/luminance_compare_ps2x.fxc` | 54 | **Replaced.** Its luminance formula survives verbatim in `shaders/histogram.wgsl`; the `step()` range test does not. |
| `materialsystem/stdshaders/screenspace_general.cpp` | ~330 | **Read, not ported.** The shader `dev/lumcompare.vmt` names. What matters is `SHADER_INIT`'s sRGB-read decision (`:98`), which is what makes the histogram linear. |
| `materialsystem/shaderapidx9/shaderapidx8.cpp` | — | `CShaderAPIDx8::SetToneMappingScaleLinear` (`:16227`) only. Ported into `uniforms::tone_mapping_scale`. |
| `materialsystem/cmatrendercontext.cpp` | — | `SetToneMappingScaleLinear`/`GetToneMappingScaleLinear` (`:3291`) only. Ported into `RenderContext::set_exposure`. |
| `game/client/viewrender.cpp` | — | Read for **ordering** only: `UpdateMaterialSystemTonemapScalar()` at `:2989`, before the scene. |
| `stdshaders/Engine_Post_dx9.cpp`, `floattoscreen.cpp` | ~1,400 | **Not ported.** The final full-screen pass survives as `materials/post.rs`'s blit, stripped of everything it was carrying. |

**In scope: roughly 1,000 lines of C++, landing as about 700 lines of Rust plus two small
WGSL files.**

---

## 2. The loop, and the one thing to get right

```
frame N     UpdateMaterialSystemTonemapScalar  -> cLightScale.x
            draw the scene, exposed
            DoTonemapping: measure the drawn frame
frame N+k   the counts come back; the controller corrects
```

**The measurement is of an already-exposed frame.** `ComputeTargetTonemapScalar` ends with

```cpp
float flLastScale = m_flCurrentTonemapScale;
flTargetScalar *= flLastScale;
```

under the comment "Apply this against last frames scalar". Read the histogram as an
absolute exposure instead of as a correction factor and the loop oscillates instead of
converging — it is the single easiest thing to get wrong in the whole port, and it is one
line.

---

## 3. What the histogram actually measures

Three findings, each of which changes the answer by a large factor:

**It is linear light, not gamma.** `CHistogramBucket::IssueQuery`'s comment says "Find min
and max gamma-space text range" and is stale. `dev/lumcompare.vmt` (extracted from the
shipped `pak01_dir.vpk`) is:

```
screenspace_general
{
    $PIXSHADER luminance_compare_ps20
    $BASETEXTURE _rt_FullFrameFB
    $ALPHATESTED 1
    $DISABLE_COLOR_WRITES 1
}
```

It does not set `$LINEARREAD_BASETEXTURE`, and `screenspace_general.cpp:109` therefore
loads `$basetexture` with `TEXTUREFLAGS_SRGB` and `:182` enables an sRGB read. The
luminance compared against the bucket boundaries is linear. Reading the boundaries as
gamma values puts the 65% target at 0.32 linear and halves every scene. This port gets the
same decode for free — `textureLoad` on an sRGB format linearizes — and
`materials::histogram`'s `an_srgb_source_is_measured_in_linear_light` is the test that
pins it, because it is an assumption rather than something visible in the source.

**Luminance is NTSC-weighted**, `dot(rgb, (0.2125, 0.7154, 0.0721))`. Two alternatives sit
commented out beside it in the `.fxc`; neither is what shipped.

**The buckets are `(i/16)^2.5`.** Sixteen of them, tiling `[0, 1]`, with more resolution at
the dark end. Valve's seventeenth is not a bucket: it spans `[0, 100000]` and exists only
to calibrate occlusion-query pixel counts, "some boards (nvidia) have their occlusion
query return values larger when using AA". A compute pass counts exactly, so it is deleted.

---

## 4. The algorithm, as ported

`ComputeTargetTonemapScalar( false )`, all of which is in `src/client/tonemap.rs`:

1. `FindLocationOfPercentBrightPixels( 2.0, 65.0 )` — walk the buckets from the bright end
   down until 2% of the pixels are accounted for, interpolating linearly inside the bucket
   the border lands in. That is the 98th-percentile luminance.
   - **The "sticky bin"**: if the target (0.65) is inside the same bucket the answer is,
     return the target exactly, so the correction comes out 1 and the exposure holds
     still. A deadband, and the reason the exposure does not hunt.
2. `target = 0.65 / location`.
3. `FindLocationOfPercentBrightPixels( 50.0 )` is the median. If `0.03 / median` asks for
   *more* exposure than step 2, take it instead. A floor under dark scenes; it only ever
   brightens.
4. Multiply by the current scale (§2).
5. Clamp into `GetExposureRange`, then to a floor of 0.001.
6. `SetTonemapScale`: push into a ten-entry moving average, take a weighted mean, and
   exponentially chase it.

Three things in step 6 that look like bugs and are not:

- **The moving-average weights are `|i - 5| / 5`** over ten samples — 1.0 on the oldest,
  0.0 on the *middle* one, 0.8 on the newest. Nobody would write that on purpose, and it
  is what every Source game's exposure has been smoothed with. A flat or recency-weighted
  mean adapts visibly differently. Kept, and `the_moving_average_weights_are_valve_s_v_shape`
  is the test that stops someone "fixing" it. Note the buffer is scrolled *before* it is
  weighted, so the sample that lands on the zero-weight slot is the one that was one place
  newer.
- **The step is capped at `(1 / 16) * 0.25` per frame**, not per second. Valve's reason is
  the rolling sixteen-frame histogram rebuild this port does not have, but it is also what
  bounds the adaptation speed at all, so it is kept. The consequence is real:
  **adaptation is frame-rate dependent above about 128 fps**, and below that the cap is
  what decides the rate rather than `mat_accelerate_adjust_exposure_down`.
- **`mat_accelerate_adjust_exposure_down` (40) is therefore inert at ordinary frame
  rates.** `rate * dt` exceeds the cap whenever `dt > 1/128 s`, so at 60 fps darkening and
  brightening move by exactly the same amount. Measured, and tested both ways in
  `the_per_frame_cap_hides_the_accelerated_darkening_below_128_fps`.

---

## 5. Deletions, with reasons

- **`mat_tonemap_algorithm 0`** — the 31-bucket log-spaced original, selected by
  `UpdateBucketRanges`' game-directory match against `{"dod", "cstrike", "lostcoast"}`.
  Unreachable for Portal 2, a different bucket count and a different target formula
  (`0.005 / averageLuminance`). The cvar is not registered either: one that cannot change
  anything is worse than none.
- **`SetOverrideTonemapScale`** — VScript and the commentary system call it; neither
  exists. `mat_force_tonemap_scale` covers the same ground from a console.
- **`DisplayHistogram`** — 200 lines of `Viewport` + `ClearBuffers` used as a bar chart.
  Replaced by a `tonemap` console command, which is this port's own, the way `trace` is.
  `mat_show_histogram` is not registered.
- **`mat_fullbright 1`**'s tone-mapping branch — `mat_fullbright` is an engine-wide unlit
  mode and registering it here to do a tenth of its job would be worse than omitting it.
- **Split-screen** (`GetCurrentTonemappingSystem`'s per-slot array), per
  `portdocs/CLIENT.md` §5.
- **`mat_tonemapping_occlusion_use_stencil`** and `dev/no_pixel_write` — a workaround for
  drivers whose occlusion queries counted wrong. There are no occlusion queries.
- **The `HDR_TYPE_NONE` and `HDR_TYPE_FLOAT` arms** of
  `CShaderAPIDx8::SetToneMappingScaleLinear`. `HDR_TYPE_NONE` is `mat_hdr_level 0`, which
  would also change the lightmap format; the tone mapper turns itself off by choosing an
  exposure of 1, which reaches the same place without a second switch.

---

## 6. `env_tonemap_controller`, and the one measured divergence

**105 of Portal 2's 106 maps place an `env_tonemap_controller`**, and they drive it hard.
Scanning every map's entity lump for `SetAutoExposure*`/`SetTonemapRate` outputs:

| Input | Values, by how many outputs fire them |
|---|---|
| `SetAutoExposureMax` | 3 ×693, 5 ×480, 1.2 ×248, 2.5 ×82, 1.5 ×49, 2 ×43, 3.2 ×42, 2.7 ×41, and singletons at 2.8, 3.7, 6, 8, 40 |
| `SetAutoExposureMin` | 0.5 ×981, 1 ×658, 2 ×42, 0.8 ×2, 3 ×1 |
| `SetTonemapRate` | 0.25 ×1185, 0.15 ×410, 4 ×42, 2 ×41, and singletons at 1, 100 |

So the cvar defaults this port falls back on — `mat_autoexposure_min 0.5`,
`mat_autoexposure_max 2`, rate 1 — are **not** what the shipped game runs anywhere. The
commonest map setting is a ceiling of 3 or 5 and a rate four times slower.

`sp_a1_intro1` is concrete: `@rl_lighting_fixup` fires `@rl_prestasis_exposure_reload`
`OnSpawn`, which sets max 1.5, min 1, rate 0.25 — and a `trigger_once` later in the level
swaps that for max 5. So at the spawn point the shipped game caps the exposure at **1.5**
and this port runs up towards **2.0**, which is the measured state of things and not a
bug in the controller. `mat_autoexposure_max 1.5` in the console reproduces the shipped
behaviour there.

`src/engine/exposure.rs` takes `KISAK_AUTOEXPOSURE_MAX` for exactly this: on
`sp_a2_bts2`, a dark maintenance area, the default ceiling of 2 binds within a second and
the map's own 5 takes the exposure to 4.4.

**When entities land, this is one function**: `GetTonemapSettingsFromEnvTonemapController`
copies eight floats out of the controller the local player points at, and
`ToneMap::exposure_range` grows the `g_bUseCustomAutoExposure*` branch it is currently
missing. Note Valve's own bug while doing it: the no-controller path resets
`g_bUseCustomAutoExposureMax` and **not** `...Min`.

---

## 7. What is still owed

Everything else in `DoEnginePostProcessing`, in the order it would matter:

- **Bloom** — `Generate8BitBloomTexture`, the downsample/blur chain and `BloomAdd`. Needs
  the same scene target this landed, plus three quarter-size render targets. It is the
  most visible thing still missing, and Portal 2 leans on it.
- **Colour correction** — `CColorCorrectionMgr`, per-area lookup textures. Portal 2 uses
  it heavily and the entity is another `server/` dependency.
- **Local contrast, vignette, film grain** — `Engine_Post`'s remaining parameters.
- **A float scene target**, if `HDR_TYPE_FLOAT` is ever wanted. It would change the
  measurement's meaning (nothing clips at 1.0 any more, so the 98th percentile moves) and
  therefore the tuning constants in §4. `portdocs/MATERIALSYSTEM.md` §10 is where that
  decision lives.
