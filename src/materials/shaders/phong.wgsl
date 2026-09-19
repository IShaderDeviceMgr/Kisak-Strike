// Phong: the model shader with a specular highlight — a base texture lit per
// pixel by an ambient cube and up to four local lights, with a Blinn-Phong
// specular term, an optional rim light, and an environment map.
//
// Translated from `materialsystem/stdshaders/phong_vs20.fxc` and
// `phong_ps20b.fxc`, taking the `[ps20b] [PC]` branch everywhere and
// `STATICLIGHT3 == 0`, `SFM == 0`, `FLASHLIGHT == 0` and
// `CASCADED_SHADOW_MAPPING == 0` throughout. The lighting helpers come from
// `common_vertexlitgeneric_dx9.h`, whose `SpecularAndRimTerms` and
// `PixelShaderDoSpecularLighting` are the two functions this shader exists
// for. The combo bucketing is written down in `portdocs/MATERIALSYSTEM.md`
// §9's stage 6 and summarised on `shader::phong_uniforms`.
//
// Prepended by `shaders/prelude.wgsl` and `shaders/modellighting.wgsl` — the
// second is shared with `VertexLitGeneric`, which declares the same group 3.
//
// ---------------------------------------------------------------------------
// No `.vmt` names this shader
// ---------------------------------------------------------------------------
// There is no `SHADER( Phong )` in `stdshaders/`. `phong_dx9_helper.cpp` is
// reached only from `DrawVertexLitGeneric_DX9`, which hands the material over
// when `WantsPhongShader` says so (`vertexlitgeneric_dx9_helper.cpp:2346`) —
// `$phong 1` plus a `$bumpmap`, a `$lightwarptexture` or
// `$basemapalphaphongmask`. So content writes `VertexLitGeneric` and 317 of
// Portal 2's 1,135 such materials arrive here instead. `ShaderKind::resolve`
// is that redirect and `shader::wants_phong` is the predicate.
//
// ---------------------------------------------------------------------------
// It is always a per-pixel shader, and it gets no baked vertex light
// ---------------------------------------------------------------------------
// `VertexLitGeneric` has two lighting paths and picks between them on
// `$bumpmap`. This has one: the normal map is sampled unconditionally in the
// original, with `TEXTURE_NORMALMAP_FLAT` bound when the material has none
// (`phong_dx9_helper.cpp:678`), so an unbumped Phong material is a bumped one
// reading a flat normal. 110 of the 317 are unbumped and every one of them
// reaches here through `$basemapalphaphongmask`.
//
// Because the shading is per pixel, `PixelShaderDoLighting` is called with
// `staticLightingColor = 0` and `bStaticLight = false` (`phong_ps20b.fxc:643`)
// — **a Phong model reads none of the light `vrad` baked into its vertices**,
// exactly as `VertexLitGeneric`'s bumped path does not. The CS:GO
// `STATICLIGHT3` work exists because of this; `phong_vs20.fxc:194` says so.
// Its whole diffuse term is therefore the ambient cube plus the local lights,
// and **the specular term needs at least one local light to exist at all**.
// `world::props` supplies no local lights yet, so a static prop wearing one of
// these is currently lit by its ambient cube and has no highlight — the shader
// is right and the light source is missing.
//
// ---------------------------------------------------------------------------
// What is here and what is not
// ---------------------------------------------------------------------------
// Here: the base texture and its transform, the normal map and its own
// transform, the phong mask in three places (normal-map alpha, base alpha via
// `$basemapalphaphongmask`, base luminance via `$basemapluminancephongmask`),
// `$phongexponent` and the exponent map's red channel, `$phongtint` including
// the tint-with-albedo path, `$phongboost`, `$phongfresnelranges`,
// `$phongwarptexture`, `$lightwarptexture`, `$rimlight` with its exponent,
// boost and mask, `$envmap` with its tint and `$envmapfresnel`, `$selfillum`
// with `$selfillummask` and `$selfillumtint`, `$detail` in all of its blend
// modes, `$blendtintbybasealpha` and `$notint`, colour modulation, alpha
// testing and fog.
//
// Not here, and each is a combo pinned off in the bucketing: the flashlight
// and its shadow filtering, cascaded shadow maps, wrinkle maps
// (`$compress`/`$stretch` — no Portal 2 material sets one), `$decaltexture`
// and `$tintmasktexture` (likewise none), the screen-space ambient occlusion
// and tessellation paths (Source Filmmaker only), `$selfillumfresnel` (whose
// pre-tonemap body is commented out in the reference and whose live half is a
// CS:GO team-ID glow), three-stream static light, and skinning and morphing.
//
// Read by the reference and deliberately *ignored* by it, which is worth
// knowing because content sets them: `$envmapcontrast` (51 of the 317),
// `$envmapsaturation` (24), `$envmapmask` (1), `$basealphaenvmapmask` (18,
// because base alpha is this shader's envmap mask whether the flag is set or
// not), `$selfillummaskscale`, `$halflambert` (25 — Phong has its own
// half-Lambert switch, see below) and `$multiply` (0).

// ---------------------------------------------------------------------------
// Group 1: the material
// ---------------------------------------------------------------------------
// Mirrors `shader::PhongUniforms`, field for field and pad for pad. The
// four-component packings are Valve's own constant registers, kept because
// each pairing is a fact about the reference rather than a convenience: the
// rim exponent really does live in the `w` of the specular tint, and the
// detail blend factor really does share a slot with `$phongalbedoboost`.

struct PhongUniforms {
    // $basetexturetransform, as two rows dotted against (u, v, 0, 1).
    base_texture_transform: array<vec4<f32>, 2>,
    // $bumptransform. Its own transform, as cBumpTexCoordTransform.
    bump_transform: array<vec4<f32>, 2>,
    // cDetailTexCoordTransform: $detailtexturetransform times $detailscale.
    detail_transform: array<vec4<f32>, 2>,
    // g_SelfIllumTint_and_DetailBlendFactorOrPhongAlbedoBoost. $selfillumtint
    // in rgb; w is $detailblendfactor when there is a $detail and
    // $phongalbedoboost when there is not — one register, two meanings, and
    // the reference's own name for it says so.
    selfillum_tint: vec4<f32>,
    // g_vPsConst2. $envmaptint in rgb — **in gamma space, undecoded**, which
    // is what `DrawPhong_DX9` writes (`:800`) and is *not* what
    // `VertexLitGeneric` does. w is 1 when the envmap mask is the normal map's
    // alpha rather than base alpha.
    envmap_tint: vec4<f32>,
    // g_FresnelSpecParams: $phongfresnelranges in xyz, $phongboost in w.
    fresnel_spec: vec4<f32>,
    // g_SpecularRimParams: $phongtint in xyz — or x < 0, meaning "tint with
    // the albedo" — and the rim exponent in w.
    specular_rim: vec4<f32>,
    // g_ShaderControls: x = $basemapalphaphongmask, y unused, z = the inverse
    // of $blendtintbybasealpha (-1 for $notint), w = $invertphongmask.
    shader_controls: vec4<f32>,
    // g_ShaderControls2: x = $envmapfresnel, y = $basemapluminancephongmask,
    // z = $phongexponent (0 meaning "read the exponent map"), w = 1 when a
    // $selfillummask texture is bound.
    shader_controls2: vec4<f32>,
    // x = the rim mask control, y = $rimlightboost. Valve splits these across
    // two repurposed flashlight registers on the PC path and packs them into
    // PSREG_RIMPARAMS on console; this is the console packing, because there is
    // no flashlight register to repurpose here.
    rim_params: vec4<f32>,
    // $alphatestreference, or the fixed-function default of 0.7.
    alpha_test_reference: f32,
    // $detailblendmode, one of the prelude's TCOMBINE_* values.
    detail_blend_mode: i32,
    // PhongFlags, below.
    flags: u32,
    pad0: u32,
}

// `shader::PhongFlags`.
const FLAG_ALPHA_TEST: u32 = 1u;
const FLAG_NO_FOG: u32 = 2u;
const FLAG_BUMPMAP: u32 = 4u;
const FLAG_ENVMAP: u32 = 8u;
const FLAG_SELFILLUM: u32 = 16u;
const FLAG_SELFILLUM_MASK: u32 = 32u;
const FLAG_DETAIL: u32 = 64u;
const FLAG_RIMLIGHT: u32 = 128u;
const FLAG_LIGHTWARP: u32 = 256u;
const FLAG_PHONGWARP: u32 = 512u;
const FLAG_HALF_LAMBERT: u32 = 1024u;

@group(1) @binding(0) var<uniform> material: PhongUniforms;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var base_sampler: sampler;
@group(1) @binding(3) var bump_texture: texture_2d<f32>;
@group(1) @binding(4) var bump_sampler: sampler;
@group(1) @binding(5) var detail_texture: texture_2d<f32>;
@group(1) @binding(6) var detail_sampler: sampler;
@group(1) @binding(7) var selfillum_mask_texture: texture_2d<f32>;
@group(1) @binding(8) var selfillum_mask_sampler: sampler;
@group(1) @binding(11) var envmap_texture: texture_cube<f32>;
@group(1) @binding(12) var envmap_sampler: sampler;
// `SpecExponentSampler`, sampler 7: red is the exponent, green scales the
// albedo tint, alpha masks the rim term. An undefined `$phongexponenttexture`
// binds the standard **white** texture, which is what the reference binds
// (`phong_dx9_helper.cpp:668`) and is why none of the three reads needs a flag
// — white gives exponent 1, full albedo tint and an unmasked rim.
@group(1) @binding(21) var phong_exponent_texture: texture_2d<f32>;
@group(1) @binding(22) var phong_exponent_sampler: sampler;
// `DiffuseWarpSampler`, sampler 2: `$lightwarptexture`, a 1D ramp indexed by
// the scalar diffuse term. Sampled as a 2D texture at `v = 0.5`, because a
// `.vtf` is 2D and Valve's `tex1D` read the same single row.
@group(1) @binding(23) var lightwarp_texture: texture_2d<f32>;
@group(1) @binding(24) var lightwarp_sampler: sampler;
// `SpecularWarpSampler`, sampler 1: `$phongwarptexture`, indexed by
// (specular, fresnel) — an iridescence table. One Portal 2 material has one.
@group(1) @binding(25) var phongwarp_texture: texture_2d<f32>;
@group(1) @binding(26) var phongwarp_sampler: sampler;

// ---------------------------------------------------------------------------
// This shader's half of the lighting core
// ---------------------------------------------------------------------------
// `shaders/modellighting.wgsl` has group 3, the ambient cube and the
// attenuation. What is here is `common_vertexlitgeneric_dx9.h`'s pixel-shader
// side: the diffuse term, the specular and rim terms, and the accumulation
// over the lights.

// `DiffuseTerm` (`common_vertexlitgeneric_dx9.h:80`), minus one CS:GO line.
//
// Three differences from `VertexLitGeneric`'s `cosine_term`, and none of them
// is cosmetic:
//
//   1. The half-Lambert result is **saturated before squaring**.
//   2. The square is **skipped when a light warp is in play**, so
//      `$lightwarptexture` changes the falloff curve and not just its colour.
//   3. It returns a *colour*, because the warp lookup replaces the scalar
//      outright.
//
// **`SoftenCosineTerm` is deliberately not applied**, for the same reason it is
// dropped in `VertexLitGeneric`: `fResult = SoftenCosineTerm( fResult ); // For
// CS:GO` is `(d + d²)/2`, a CS:GO change to the falloff of every lit surface,
// and Portal 2 predates it.
fn diffuse_term(
    world_normal: vec3<f32>,
    light_dir: vec3<f32>,
    half_lambert: bool,
    do_lighting_warp: bool,
) -> vec3<f32> {
    let n_dot_l = dot(world_normal, light_dir);
    var result: f32;
    if half_lambert {
        result = saturate(n_dot_l * 0.5 + 0.5);
        if !do_lighting_warp {
            result = result * result;
        }
    } else {
        result = saturate(n_dot_l);
    }
    if do_lighting_warp {
        return textureSample(lightwarp_texture, lightwarp_sampler, vec2<f32>(result, 0.5)).rgb;
    }
    return vec3<f32>(result, result, result);
}

// `SpecularAndRimTerms` (`common_vertexlitgeneric_dx9.h:145`).
//
// Blinn-Phong: the half-angle between the eye and the light, dotted against
// the normal and raised to the exponent — **not** the reflect-and-dot form the
// shader's name suggests.
//
// Two masks that are easy to lose. The specular term is multiplied by
// `pow( saturate( N·L ), 0.5 )` — a *softened* `N·L`, so a highlight fades out
// as the light goes behind the surface rather than being clipped — and the rim
// term by a plain `saturate( N·L )`. The `STATIC3` copies of these functions
// further down the reference file drop both multiplies, which is what makes
// them a separate copy; that path is `STATICLIGHT3` and is pinned off here.
struct SpecularAndRim {
    specular: vec3<f32>,
    rim: vec3<f32>,
}

fn specular_and_rim_terms(
    world_normal: vec3<f32>,
    light_dir: vec3<f32>,
    specular_exponent: f32,
    eye_dir: vec3<f32>,
    do_specular_warp: bool,
    fresnel: f32,
    color: vec3<f32>,
    do_rim_lighting: bool,
    rim_exponent: f32,
) -> SpecularAndRim {
    var out: SpecularAndRim;
    let half_angle = normalize(eye_dir + light_dir);
    let n_dot_h = saturate(dot(world_normal, half_angle));
    var specular = vec3<f32>(pow(n_dot_h, specular_exponent));

    if do_specular_warp {
        // Sampled at { (N·H)^k, fresnel } — a 2D table, which is what makes
        // iridescence possible at all.
        specular *= textureSample(
            phongwarp_texture,
            phongwarp_sampler,
            vec2<f32>(specular.x, fresnel),
        ).rgb;
    }

    let n_dot_l = saturate(dot(world_normal, light_dir));
    specular *= pow(n_dot_l, 0.5);
    out.specular = specular * color;

    out.rim = vec3<f32>(0.0);
    if do_rim_lighting {
        out.rim = vec3<f32>(pow(n_dot_h, rim_exponent)) * n_dot_l * color;
    }
    return out;
}

// `PixelShaderDoLighting` with `bStaticLight = false, bAmbientLight = true`
// (`phong_ps20b.fxc:643`), and `PixelShaderDoSpecularLighting`
// (`common_vertexlitgeneric_dx9.h:288`), fused into one loop.
//
// Valve writes them as two functions each unrolled four times, because a
// shader model with no loops had to be. One loop is the same arithmetic — and
// `lighting.count` is `NUM_LIGHTS`, which was a *dynamic* combo, so this is
// §7.3's bucket 2 applied to an axis that had five values.
struct PhongLighting {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
    rim: vec3<f32>,
}

fn phong_lighting(
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
    eye_dir: vec3<f32>,
    specular_exponent: f32,
    fresnel: f32,
    rim_exponent: f32,
    half_lambert: bool,
    do_lighting_warp: bool,
    do_specular_warp: bool,
    do_rim_lighting: bool,
) -> PhongLighting {
    var out: PhongLighting;
    out.diffuse = ambient_light(world_normal);
    out.specular = vec3<f32>(0.0);
    out.rim = vec3<f32>(0.0);

    for (var i = 0u; i < lighting.count; i = i + 1u) {
        let light = lighting.lights[i];
        let light_dir = light_direction(light, world_pos);
        // The attenuation is `VertexAttenInternal`, which the reference
        // evaluates once per *vertex* and interpolates
        // (`phong_vs20.fxc:279`). It is evaluated per pixel here, which is the
        // same function and the same choice `VertexLitGeneric`'s bumped path
        // already makes — the only difference is that a 1/d² term stops being
        // linearly interpolated across a large triangle.
        let atten = light_attenuation(light, world_pos);
        let color = light.color.rgb * atten;

        out.diffuse += color * diffuse_term(
            world_normal,
            light_dir,
            half_lambert,
            do_lighting_warp,
        );

        let terms = specular_and_rim_terms(
            world_normal,
            light_dir,
            specular_exponent,
            eye_dir,
            do_specular_warp,
            fresnel,
            color,
            do_rim_lighting,
            rim_exponent,
        );
        out.specular += terms.specular;
        out.rim += terms.rim;
    }
    return out;
}

// `Fresnel( vNormal, vEyeDir, vRanges )`
// (`common_vertexlitgeneric_dx9.h:210`): the traditional `(1 - N·V)²` remapped
// through a piecewise-linear curve with a low, a mid and a high value, so that
// content can decide how much specular a face-on surface keeps. The default
// `[0 0.5 1]` is the identity; the commonest shipped value is `[0.6 1 2]` and
// 52 materials write `[5 1 2]`, which is a 5x boost head-on.
fn fresnel_ranges(normal: vec3<f32>, eye_dir: vec3<f32>, ranges: vec3<f32>) -> f32 {
    let f = fresnel(normal, eye_dir);
    if f > 0.5 {
        return mix(ranges.y, ranges.z, 2.0 * f - 1.0);
    }
    return mix(ranges.x, ranges.y, 2.0 * f);
}

// `Fresnel4` (`common_vertexlitgeneric_dx9.h:196`): `(1 - N·V)⁴`. The rim
// term's own fresnel, which deliberately does *not* go through the ranges.
fn fresnel4(normal: vec3<f32>, eye_dir: vec3<f32>) -> f32 {
    let f = fresnel(normal, eye_dir);
    return f * f;
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) texcoord: vec2<f32>,
    @location(1) bump_texcoord: vec2<f32>,
    @location(2) detail_texcoord: vec2<f32>,
    @location(3) world_position: vec3<f32>,
    @location(4) world_normal: vec3<f32>,
    // xyz = world tangent S, w = the binormal sign.
    @location(5) world_tangent: vec4<f32>,
}

@vertex
fn vs_main(vertex: ModelVertexInput) -> VertexOutput {
    var out: VertexOutput;

    // The pose this vertex rides, with the entity's placement already folded
    // in — `draw.model` alone when the draw is not skinned. See
    // `skin_model_matrix` in the prelude.
    let model = skin_model_matrix(vertex.bone_weights, vertex.bone_indices);
    let world = (model * vec4<f32>(vertex.position, 1.0)).xyz;
    // The upper 3x3 of that matrix — a placement times a blend of bone
    // transforms, all rigid, so no inverse transpose.
    // `SkinPositionNormalAndTangentSpace` (`common_vs_fxc.h:252`).
    let normal_matrix = mat3x3<f32>(
        model[0].xyz,
        model[1].xyz,
        model[2].xyz,
    );

    out.clip_position = clip_position(world);
    out.world_position = world;
    out.world_normal = normalize(normal_matrix * vertex.normal);
    // "Propagate binormal sign in world tangent.w" (`phong_vs20.fxc:291`).
    out.world_tangent = vec4<f32>(
        normalize(normal_matrix * vertex.tangent.xyz),
        vertex.tangent.w,
    );

    out.texcoord = transform_texcoord(
        vertex.texcoord,
        material.base_texture_transform[0],
        material.base_texture_transform[1],
    );
    out.bump_texcoord = transform_texcoord(
        vertex.texcoord,
        material.bump_transform[0],
        material.bump_transform[1],
    );
    out.detail_texcoord = transform_texcoord(
        vertex.texcoord,
        material.detail_transform[0],
        material.detail_transform[1],
    );
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let flags = material.flags;

    var base_color = textureSample(base_texture, base_sampler, in.texcoord);

    // Sampled once and used twice: ten of the thirteen blend modes composite
    // the detail texture into the albedo before lighting, and two more add it
    // after.
    var detail_color = vec4<f32>(0.0);
    if (flags & FLAG_DETAIL) != 0u {
        detail_color = textureSample(detail_texture, detail_sampler, in.detail_texcoord);
        // **No `$detailtint`.** `VertexLitGeneric` multiplies the detail
        // sample by `g_DetailTint` and this shader does not
        // (`phong_ps20b.fxc:495`) — Valve's asymmetry, and free to reproduce:
        // no Portal 2 Phong material sets the parameter.
        base_color = texture_combine(
            base_color,
            detail_color,
            material.detail_blend_mode,
            material.selfillum_tint.w,
        );
    }

    // `float3 lumCoefficients = { 0.3, 0.59, 0.11 }` — the NTSC weights spelled
    // inline in this file, which are *not* the prelude's `luminance`
    // (Rec. 709). Kept as written, because the value feeds a phong mask that
    // content was authored against.
    let base_lum = dot(base_color.rgb, vec3<f32>(0.3, 0.59, 0.11));

    // The normal map. Valve samples it unconditionally with a flat normal map
    // bound when the material has none; that is `(0, 0, 1)` with alpha 1 after
    // the decode, which is what the `else` here writes — the same answer
    // without a second fallback texture in the cache.
    var tangent_normal = vec3<f32>(0.0, 0.0, 1.0);
    var normal_alpha = 1.0;
    if (flags & FLAG_BUMPMAP) != 0u {
        let normal_texel = textureSample(bump_texture, bump_sampler, in.bump_texcoord);
        tangent_normal = normal_texel.xyz * 2.0 - 1.0;
        normal_alpha = normal_texel.a;
    }

    // The phong mask, chosen by two `lerp`s rather than two branches — which is
    // Valve's shape and is worth keeping, because they are not exclusive: a
    // material can set both and the second wins.
    var spec_mask = mix(normal_alpha, base_color.a, material.shader_controls.x);
    spec_mask = mix(spec_mask, base_lum, material.shader_controls2.y);

    let geometric_normal = normalize(in.world_normal);
    let tangent = normalize(in.world_tangent.xyz);
    // `vWorldBinormal = cross( vWorldNormal, i.vWorldTangent.xyz ) * i.vWorldTangent.w`
    // (`phong_ps20b.fxc:473`). The `w` is the handedness the `.vvd` stored;
    // drop it and every mirrored UV island lights inside out.
    let binormal = cross(geometric_normal, tangent) * in.world_tangent.w;
    let world_normal = normalize(vec3_tangent_to_world(
        tangent_normal,
        geometric_normal,
        tangent,
        binormal,
    ));

    let eye_dir = normalize(frame.eye_pos_water_height.xyz - in.world_position);
    let fresnel_term = fresnel_ranges(world_normal, eye_dir, material.fresnel_spec.xyz);

    // --- the exponent, the tint and the rim mask ----------------------------
    let spec_exp_map = textureSample(
        phong_exponent_texture,
        phong_exponent_sampler,
        in.texcoord,
    );
    // "If the exponent passed in as a constant is zero, use the value from the
    // map as the exponent" — remapped from the map's red channel onto 1..150.
    // 71 of the 317 materials have an exponent texture and no
    // `$phongexponent`, so this is the live path and not a fallback.
    var spec_exp = material.shader_controls2.z;
    if spec_exp == 0.0 {
        spec_exp = 1.0 - spec_exp_map.r + 150.0 * spec_exp_map.r;
    }
    let rim_mask = mix(1.0, spec_exp_map.a, material.rim_params.x);

    let specular_boost = material.fresnel_spec.w;
    var specular_tint = specular_boost * material.specular_rim.xyz;

    // --- diffuse, specular and rim ------------------------------------------
    let lit = phong_lighting(
        in.world_position,
        world_normal,
        eye_dir,
        spec_exp,
        fresnel_term,
        material.specular_rim.w,
        (flags & FLAG_HALF_LAMBERT) != 0u,
        (flags & FLAG_LIGHTWARP) != 0u,
        (flags & FLAG_PHONGWARP) != 0u,
        (flags & FLAG_RIMLIGHT) != 0u,
    );
    var diffuse_lighting = lit.diffuse;
    var specular_lighting = lit.specular;

    // --- the environment map ------------------------------------------------
    var envmap_color = vec3<f32>(0.0);
    if (flags & FLAG_ENVMAP) != 0u {
        let reflect_vector = calc_reflection_vector_unnormalized(world_normal, eye_dir);
        // `ENV_MAP_SCALE` is `cLightScale.z`, the tone mapper's envmap scale.
        //
        // **`g_vEnvmapTint` is gamma space here.** `DrawPhong_DX9` reads
        // `$envmaptint` with a plain `GetVecValue` and no decode
        // (`phong_dx9_helper.cpp:800`), where `VertexLitGeneric`'s helper runs
        // it through `GammaToLinearFullRange`. Decoding it here would darken
        // every reflection in the game by up to a factor of 36 — see
        // `shader::gamma_to_linear_full_range_param`.
        envmap_color = frame.light_scale.z
            * textureSample(envmap_texture, envmap_sampler, reflect_vector).rgb
            * material.envmap_tint.rgb;
        // `$envmapfresnel` as a lerp weight rather than a switch, so 0 is the
        // identity.
        envmap_color = mix(
            envmap_color,
            fresnel_term * envmap_color,
            material.shader_controls2.x,
        );

        // **The mask is base alpha unless the normal map's alpha is asked
        // for.** There is no `$basealphaenvmapmask` test on this path — unlike
        // `VertexLitGeneric`, where base-alpha masking has to be requested —
        // so 18 materials that set that flag get what they would have had
        // anyway and the rest get base-alpha masking they did not ask for.
        // Valve's.
        let envmap_mask = mix(base_color.a, spec_mask, material.envmap_tint.w);
        envmap_color *= mix(envmap_mask, 1.0 - envmap_mask, material.shader_controls.w);
    }

    // --- the tint-with-albedo path ------------------------------------------
    // "If constant tint is negative, tint with albedo, based upon scalar tint
    // map" (`phong_ps20b.fxc:704`). The CPU writes `x = -1` only for a
    // `$phongtint "[0 0 0]"` material that also has a `$phongexponenttexture`
    // for `$phongalbedotint` to read — **no Portal 2 material does both**, so
    // this is ported and unreachable, and the depot census says so. The two
    // spellings are Valve's: with a `$detail` the albedo boost's register is
    // the detail blend factor instead, so it cannot be used.
    if material.specular_rim.x < 0.0 {
        if (flags & FLAG_DETAIL) != 0u {
            specular_tint = specular_boost * mix(vec3<f32>(1.0), base_color.rgb, spec_exp_map.g);
        } else {
            let albedo_boost = material.selfillum_tint.w;
            specular_tint = mix(
                vec3<f32>(specular_boost),
                albedo_boost * base_color.rgb,
                spec_exp_map.g,
            );
            if (flags & FLAG_ENVMAP) != 0u {
                envmap_color = spec_exp_map.r
                    * mix(
                        envmap_color,
                        envmap_color * base_color.rgb * albedo_boost,
                        spec_exp_map.g,
                    );
            }
        }
    }

    let albedo = base_color.rgb;

    specular_lighting *= spec_mask * specular_tint;
    // The fresnel is applied to the specular here *only* when no warp texture
    // consumed it as a lookup coordinate — one fresnel, two possible uses.
    if (flags & FLAG_PHONGWARP) == 0u {
        specular_lighting *= fresnel_term;
    }

    // --- colour modulation --------------------------------------------------
    // `saturate( baseColor.a + g_fInverseBlendTintByBaseAlpha )`: the control
    // is 1 by default (so the tint applies in full), 0 under
    // `$blendtintbybasealpha` (so base alpha decides), and **-1** under
    // `$notint` (so nothing does). No Portal 2 Phong material sets `$notint`.
    diffuse_lighting *= mix(
        vec3<f32>(1.0),
        draw.modulation.rgb,
        saturate(base_color.a + material.shader_controls.z),
    );

    var diffuse_component = albedo * diffuse_lighting;

    // --- self-illumination --------------------------------------------------
    if (flags & FLAG_SELFILLUM) != 0u {
        // `vSelfIllumMask = lerp( baseColor.aaa, mask, g_SelfIllumMaskControl )`
        // — the control is 1 exactly when a `$selfillummask` is bound, so this
        // is a selection and not a scale. `$selfillummaskscale`, which
        // `VertexLitGeneric` multiplies in here, is not read by this shader.
        var mask = vec3<f32>(base_color.a);
        if (flags & FLAG_SELFILLUM_MASK) != 0u {
            mask = textureSample(
                selfillum_mask_texture,
                selfillum_mask_sampler,
                in.texcoord,
            ).rgb;
        }
        diffuse_component = mix(
            diffuse_component,
            material.selfillum_tint.rgb * albedo,
            mask,
        );
        diffuse_component = max(vec3<f32>(0.0), diffuse_component);
    }

    if (flags & FLAG_DETAIL) != 0u {
        diffuse_component = texture_combine_post_lighting(
            diffuse_component,
            detail_color,
            material.detail_blend_mode,
            material.selfillum_tint.w,
        );
    }

    // --- rim lighting -------------------------------------------------------
    if (flags & FLAG_RIMLIGHT) != 0u {
        let rim_fresnel = fresnel4(world_normal, eye_dir);
        let rim = lit.rim * rim_mask * rim_fresnel;
        // "Fold rim lighting into specular term by using the max so that we
        // don't really add light twice."
        specular_lighting = max(specular_lighting, rim);
        // Plus a view-ray lookup from the ambient cube, so that a rim survives
        // with no local light at all — and gated on the normal facing **world
        // up**, `saturate( dot( N, float3( 0, 0, 1 ) ) )`, which is Valve's own
        // way of keeping a rim off the undersides of things.
        specular_lighting += rim_fresnel
            * rim_mask
            * material.rim_params.y
            * ambient_light(eye_dir)
            * saturate(dot(world_normal, vec3<f32>(0.0, 0.0, 1.0)));
    }

    let result = specular_lighting + envmap_color + diffuse_component;

    // --- alpha --------------------------------------------------------------
    // `fBaseAlphaIsForTranslucency`, which is the product of three tests
    // because base alpha has three other possible jobs: the self-illum mask,
    // the phong mask, and the tint blend. Each contributes a factor of 1 when
    // it is not using base alpha and 0 when it is, so the product is 1 only if
    // the channel is free.
    //
    // **Under `$notint` the third factor is -1, not 0**, which makes
    // `mix( 1, a, -1 )` equal `2 - a` and pushes alpha above 1 for a
    // non-opaque texel. That is the reference's arithmetic, reproduced; it is
    // unreachable on shipped content, where nothing sets `$notint`.
    var base_alpha_is_for_translucency = 1.0;
    if (flags & FLAG_SELFILLUM) != 0u {
        base_alpha_is_for_translucency *= material.shader_controls2.w;
    }
    base_alpha_is_for_translucency *= 1.0 - material.shader_controls.x;
    base_alpha_is_for_translucency *= material.shader_controls.z;
    let alpha = draw.modulation.a * mix(1.0, base_color.a, base_alpha_is_for_translucency);

    // D3D9 tested alpha in fixed-function state, after the shader, so the
    // comparison is against the modulated alpha rather than the texture's.
    if (flags & FLAG_ALPHA_TEST) != 0u && alpha < material.alpha_test_reference {
        discard;
    }

    var fog_type = PIXEL_FOG_TYPE_RANGE;
    if (flags & FLAG_NO_FOG) != 0u {
        fog_type = PIXEL_FOG_TYPE_NONE;
    }
    let fog_factor = calc_pixel_fog_factor(fog_type, in.world_position);

    return final_output(vec4<f32>(result, alpha), fog_factor, fog_type, TONEMAP_SCALE_LINEAR);
}
