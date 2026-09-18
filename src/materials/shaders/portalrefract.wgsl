// PortalRefract, stage 2: the coloured oval a portal wears.
//
// Translated from `materialsystem/stdshaders/portal_refract_vs20.fxc` and
// `portal_refract_ps2x.fxc`, taking the `STAGE == 2` branch of each and the
// `[ps20b]` / non-console path everywhere. The combo bucketing and the
// parameter table are written down on `shader::portal_refract_uniforms`.
//
// Prepended by `shaders/prelude.wgsl`, which declares groups 0 and 2,
// `VertexInput` and the fog and output helpers.
//
// ---------------------------------------------------------------------------
// Stage 2 of three, and the other two are deliberately not here
// ---------------------------------------------------------------------------
// `PortalRefract` is one `.fxc` with a `$Stage` switch over three completely
// different pixel shaders:
//
//     0  portal_refract_1.vmt      the see-through warp    needs a scene copy
//     1  portal_stencil_hole.vmt   the stencil punch       needs a stencil
//     2  portalstaticoverlay_*     THIS — the oval
//
// Stages 0 and 1 exist to make the *recursive view* composite, which is
// explicitly out of `portdocs/PORTAL.md`'s scope, so `ShaderKind::resolve`
// answers `None` for them and their two materials draw as the error
// checkerboard. Nothing in this port draws either of them: they are bound by
// `CPortalRenderable_FlatBasic`, which does not exist here.
//
// Measured over the mounted game: **7 materials name `PortalRefract` and 5 are
// stage 2** — the three `portalstaticoverlay_*` and the two
// `effects/fakeportalring_*`, which reach stage 2 through `$UseOnStaticProp`
// rather than through `$Stage`.
//
// ---------------------------------------------------------------------------
// What the effect is
// ---------------------------------------------------------------------------
// A disc of noise, masked to a ring whose radius is the portal's open amount,
// tinted through a 256x1 gradient strip and multiplied up by `$PortalColorScale`
// so that it reads as emissive. While the portal is still opening the ring is
// a filled disc instead, which is what makes a portal look like it is burning
// its way open rather than fading in.

// ---------------------------------------------------------------------------
// Group 1: the material
// ---------------------------------------------------------------------------
// Mirrors `shader::PortalRefractUniforms`, field for field and pad for pad.

struct PortalRefractUniforms {
    // $TextureTransform, as two rows. **Dotted against `uv` alone, not against
    // the expanded (u, v, 0, 1)** — see `vs_main`.
    texture_transform: array<vec4<f32>, 2>,
    // $PortalColorGradientDark, for the TINTED path. rgb; w unused.
    gradient_dark: vec4<f32>,
    // $PortalColorGradientLight, same.
    gradient_light: vec4<f32>,
    // x = $PortalColorScale (g_flPortalColorScale, PS c4.z). yzw unused.
    params: vec4<f32>,
    // PortalRefractFlags, below.
    flags: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

// `shader::PortalRefractFlags`.
const FLAG_NO_FOG: u32 = 1u;
// The `TINTED` static combo: no `$PortalColorTexture`, so the gradient comes
// from two colour parameters instead of a texture. One material in the game.
const FLAG_TINTED: u32 = 2u;

@group(1) @binding(0) var<uniform> material: PortalRefractUniforms;
// `$PortalMaskTexture` — the noise. `g_tPortalNoiseSampler`, sampler 1. Not
// sRGB (`EnableSRGBRead( SHADER_SAMPLER1, false )`), which matters: it is a
// mask, not a colour.
@group(1) @binding(27) var noise_texture: texture_2d<f32>;
@group(1) @binding(28) var noise_sampler: sampler;
// `$PortalColorTexture` — a 256x1 gradient strip. `g_tPortalColorSampler`,
// sampler 2, sRGB.
@group(1) @binding(29) var color_texture: texture_2d<f32>;
@group(1) @binding(30) var color_sampler: sampler;

// ---------------------------------------------------------------------------
// Group 3: this portal's own three numbers
// ---------------------------------------------------------------------------
// Mirrors `uniforms::PortalOverlay`. In the shipped game these are material
// *vars* rewritten by three proxies before every draw; here they are
// per-instance state bound with a dynamic offset, which is what group 3 is
// already for. `shader::ContextBinding::PortalOverlay`.

struct PortalOverlay {
    open_amount: f32,
    // **`1 - $PortalStatic`**, not `$PortalStatic`. Named for the
    // register rather than for the parameter, and `active` alone is a
    // reserved word in WGSL.
    portal_active: f32,
    time: f32,
    pad0: f32,
}

@group(3) @binding(0) var<uniform> portal: PortalOverlay;

// `kFlPortalOuterBorder` — declared in *both* `.fxc` files with a comment
// saying "Must match VS!" / "Must match PS!", because the vertex shader
// shrinks the quad's texture coordinates by it and the pixel shader measures
// the ring against it. One constant here, which is the whole of that hazard
// removed.
const PORTAL_OUTER_BORDER: f32 = 0.075;
const PORTAL_INNER_BORDER: f32 = PORTAL_OUTER_BORDER * 4.0;

// How far the noise scrolls per second, and how much of it fits across the
// portal. `kFlNoiseUvScroll` and `kFlBorderNoiseScale`
// (`portal_refract_vs20.fxc:130`).
const NOISE_SCROLL: f32 = 0.0275;
const NOISE_SCALE: f32 = 0.3;

// "This is the equivalent of smoothstep built into HLSL but linear"
// (`portal_refract_ps2x.fxc:64`). Valve's own helper, and the shader uses it
// beside `smoothstep` rather than instead of it — the two are not
// interchangeable here.
fn linearstep(low: f32, high: f32, value: f32) -> f32 {
    return saturate((value - low) / (high - low));
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    // The quad's texture coordinate, expanded by the outer border so that the
    // oval sits inside the geometry rather than touching its edge.
    @location(0) uv: vec2<f32>,
    // Two scrolling noise lookups. **`.zw` is fetched as `.wz`** in the
    // fragment shader — Valve's comment: "Will fetch as wz to avoid matching
    // layers". Swapping the components is what stops the two samples of one
    // texture moving together.
    @location(1) noise_uv: vec4<f32>,
    // For pixel fog.
    @location(2) world_position: vec3<f32>,
}

@vertex
fn vs_main(vertex: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    let world = world_position(vertex.position);
    out.clip_position = clip_position(world);
    out.world_position = world;

    // `vBaseUv.x = dot( i.vTexCoord0.xy, cBaseTexCoordTransform[0].xy )`
    // (`portal_refract_vs20.fxc:123`).
    //
    // **`.xy`, so the transform's translation column is dropped** — which is
    // not what `transform_texcoord` in the prelude does, and is why this is
    // written out rather than calling it. `SetVertexShaderTextureTransform`
    // uploads the same 2x4 either way; this shader simply reads two of the
    // four components. No shipped `PortalRefract` material sets
    // `$TextureTransform`, so the difference is invisible on content and would
    // be a silent 2x error on the first material that did.
    let base_uv = vec2<f32>(
        dot(vertex.texcoord, material.texture_transform[0].xy),
        dot(vertex.texcoord, material.texture_transform[1].xy),
    );

    // "Adjust uv's for shrunken portal".
    out.uv = base_uv * (1.0 + PORTAL_OUTER_BORDER) - (PORTAL_OUTER_BORDER * 0.5);

    // `+ 0.001f to avoid divide by zero` — Valve's, and load-bearing: the
    // noise coordinate is divided by this, and a portal at open amount 0 is
    // exactly what the first frame after activation has.
    let open = saturate(portal.open_amount + 0.001);
    let scroll = portal.time * NOISE_SCROLL;
    let noise_uv = ((base_uv - 0.5) / open) + 0.5;
    out.noise_uv = vec4<f32>(
        noise_uv * NOISE_SCALE + vec2<f32>(scroll, 0.0),
        noise_uv * NOISE_SCALE - vec2<f32>(scroll, 0.0),
    );
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // `smoothstep` here and `linearstep` below, which is Valve's choice in
    // each place and not an accident: the open amount is eased and the masks
    // are not.
    let open_amount = smoothstep(0.0, 1.0, saturate(portal.open_amount));
    // **Squared**, and that is what the hole's radius actually is — so the
    // oval spends most of its half-second opening small and finishes fast.
    let open_squared = open_amount * open_amount;

    let stretch = (in.uv * 2.0) - 1.0;
    let dist_from_center = length(stretch);

    // "Stencil cutout (1.0 in hole)". Stage 1 is what would write this to the
    // stencil buffer; here it is only used to tell the ring's inside from its
    // outside.
    let stencil_cutout = step(dist_from_center, open_squared);

    // The ring: a band just outside the hole and a band just inside it.
    let outer_mask = (1.0
        - linearstep(open_squared, open_squared + PORTAL_OUTER_BORDER, dist_from_center))
        * (1.0 - stencil_cutout);
    let inner_mask =
        linearstep(open_squared - PORTAL_INNER_BORDER, open_squared, dist_from_center)
            * stencil_cutout;

    // "This is good enough...smoothstep above is not necessary" — Valve's note
    // on the commented-out `smoothstep` this replaced.
    let settled = saturate(portal.portal_active);
    let fade_in = max(saturate(open_amount * 2.5), 1.0 - settled);
    var effect_mask = (inner_mask + outer_mask) * fade_in;

    // Three taps of one noise texture, each offset by the last — which is what
    // turns a blurred noise image into something that reads as flame. The
    // third overwrites the first on purpose.
    let noise1 = textureSample(noise_texture, noise_sampler, in.noise_uv.xy);
    let noise2 = textureSample(
        noise_texture,
        noise_sampler,
        in.noise_uv.wz - noise1.rg * 0.02,
    );
    let noise3 = textureSample(
        noise_texture,
        noise_sampler,
        in.noise_uv.xy - noise2.rg * 0.02,
    );

    // "More solid flames and calmer" — the average, where the commented-out
    // alternative beside it is the product ("more broken up flames and
    // crazier").
    var noise = (noise3.g + noise2.g) * 0.5;
    let settled_with_noise = smoothstep(0.0, noise, settled);

    // "Larger numbers give more color in the middle when portal is inactive".
    let border_softness = 0.875;
    let border_mask =
        1.0 - smoothstep(effect_mask - border_softness, effect_mask + border_softness, noise);
    noise = border_mask;
    effect_mask *= border_mask;

    // "Magic number at the end will make the flames thicker with larger
    // numbers", and it takes the result above 1 on purpose — the saturate is
    // inside it, not outside.
    let transparency =
        saturate(effect_mask + (stencil_cutout * (1.0 - settled_with_noise))) * 1.5;

    // "This will make the portals shift in color from bottom to top". `uv.y` is
    // **0 at the top of the portal and 1 at the bottom** — the quad's own
    // coordinate, set in `world::portals` — so this darkens the top and leaves
    // the bottom bright. Building the quad the other way up inverts the
    // gradient and nothing else, which is exactly the kind of wrong picture
    // that looks deliberate.
    let bottom_to_top = (pow(abs(in.uv.y), 1.5) * 0.8) + 0.2;

    let gradient_at = pow(noise, 0.5) * bottom_to_top * transparency;
    var flame: vec3<f32>;
    if (material.flags & FLAG_TINTED) != 0u {
        // `TINTED == 1`: one material in the game, `portalstaticoverlay_tinted`,
        // which is what co-op wears so that a player's own portals can be
        // recoloured from script.
        flame = mix(material.gradient_dark.rgb, material.gradient_light.rgb, gradient_at);
    } else {
        // `tex1D( g_tPortalColorSampler, x )`. The gradient is a 256x1 image,
        // so the second coordinate is the middle of its only row; D3D9's
        // `tex1D` is the same lookup.
        flame = textureSample(color_texture, color_sampler, vec2<f32>(gradient_at, 0.5)).rgb;
    }
    // "Brighten colors to make it look more emissive" — 4.0 for both of the
    // portal colours, which is why an oval is brighter than any texture in the
    // level and survives the tone mapper.
    flame *= material.params.x;

    var fog_type = PIXEL_FOG_TYPE_RANGE;
    if (material.flags & FLAG_NO_FOG) != 0u {
        fog_type = PIXEL_FOG_TYPE_NONE;
    }
    let fog_factor = calc_pixel_fog_factor(fog_type, in.world_position);

    // **The exposure is applied to the whole vector, alpha included**, and
    // after `FinalOutput` rather than inside it:
    //
    //     float flTonemapScalar = saturate( LINEAR_LIGHT_SCALE );
    //     return FinalOutput( ..., TONEMAP_SCALE_NONE ) * flTonemapScalar;
    //
    // Valve's comment says why it is clamped — "Limit tonemap scalar to 0.0-1.0
    // so the colors don't oversaturate, but let it drop down to 0 in case we're
    // fading" — and the alpha going with it is the reason a portal *fades out*
    // in a bright room rather than turning grey.
    let tonemap_scalar = saturate(frame.light_scale.x);
    let result = final_output(
        vec4<f32>(flame, transparency),
        fog_factor,
        fog_type,
        TONEMAP_SCALE_NONE,
    ) * tonemap_scalar;

    // `AlphaFunc( SHADER_ALPHAFUNC_GREATER, 1.0f/255.0f )`
    // (`portal_refract_helper.cpp:141`), which replaces the 0.5 the other two
    // stages use. With `SRC_ALPHA, ONE_MINUS_SRC_ALPHA` blending and depth
    // writes off it changes no pixel — it is there to keep the blender off the
    // 90% of the quad that is empty — so this is a cost, not a correction, and
    // it is here because the quad really is mostly empty.
    if result.a <= 1.0 / 255.0 {
        discard;
    }
    return result;
}
