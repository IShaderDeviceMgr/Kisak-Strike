// VertexLitGeneric: a model lit by an ambient cube, up to four local lights,
// and whatever `vrad` baked into its vertices.
//
// Translated from `materialsystem/stdshaders/vertexlit_and_unlit_generic_vs20.fxc`
// and `..._ps2x.fxc`, plus the `_bump_` pair of the same names, taking the
// `[ps30]` / non-`_X360` branch everywhere and `STATICLIGHT3 == 0` throughout
// — see "Two files, one module" below. The combo bucketing is written down in
// `src/materials/shader.rs`.
//
// Prepended by `shaders/prelude.wgsl`, which declares groups 0 and 2,
// `ModelVertexInput`, the colour-space and detail helpers, and the fog and
// output helpers.
//
// ---------------------------------------------------------------------------
// Two files, one module
// ---------------------------------------------------------------------------
// Valve ships this shader twice: `vertexlit_and_unlit_generic_{vs20,ps2x}.fxc`
// for a material with no normal map, and `..._bump_{vs20,ps2x}.fxc` for one
// with. They are not variants of one source — they are two files, and they
// differ in *where the lighting happens*:
//
//   unbumped: `DoLighting` in the VERTEX shader, interpolated. Gouraud.
//   bumped:   `PixelShaderDoLighting` in the FRAGMENT shader, against a normal
//             read from the map. Phong.
//
// Both are here, behind `FLAG_BUMPMAP`, and **the split is preserved rather
// than unified**. Lighting every model per pixel would be strictly prettier
// and strictly wrong: a low-poly prop lit per vertex has visibly flatter
// shading, and content was authored looking at that.
//
// One asymmetry that follows from the split and looks like a bug until you
// check the reference: **a bumped model gets no baked vertex light at all.**
// `vertexlit_and_unlit_generic_bump_ps2x.fxc:452` calls `PixelShaderDoLighting`
// with `staticLightingColor = 0` and `bStaticLight = false`, because the
// per-vertex stream cannot be re-evaluated against a per-pixel normal. Its
// light comes from the ambient cube and the local lights only.
//
// ---------------------------------------------------------------------------
// What is here and what is not
// ---------------------------------------------------------------------------
// Here: the base texture and its transform, the bump map, the ambient cube and
// four local lights with Valve's attenuation and spot terms, `$halflambert`,
// baked per-vertex static light, `$detail` in all of its blend modes,
// `$selfillum` with `$selfillummask` and `$selfillumtint`, `$envmap` with its
// tint, contrast, saturation, fresnel and three mask sources,
// `$blendtintbybasealpha`, colour modulation, alpha testing and fog.
//
// Not here, each deferred with the feature it belongs to: `$phong` and
// everything under it (a separate shader — see `shader::wants_phong`), the
// flashlight, cascaded shadow maps, `$lightwarptexture`, `$rimlight`,
// self-illum fresnel, wrinkle maps, tree sway, `$decaltexture`, `$tintmask`,
// seamless mapping, distance alpha, and skinning and morphing.

// ---------------------------------------------------------------------------
// Group 1: the material
// ---------------------------------------------------------------------------
// Mirrors `shader::VertexLitUniforms`, field for field and pad for pad.

struct VertexLitUniforms {
    // $basetexturetransform, as two rows dotted against (u, v, 0, 1).
    base_texture_transform: array<vec4<f32>, 2>,
    // $bumptransform, its own transform rather than the base one.
    bump_transform: array<vec4<f32>, 2>,
    // cDetailTexCoordTransform: $detailtexturetransform times $detailscale.
    detail_transform: array<vec4<f32>, 2>,
    // $selfillumtint in rgb, $selfillummaskscale in w.
    selfillum_tint: vec4<f32>,
    // $envmaptint in rgb, $envmapcontrast in w.
    envmap_tint: vec4<f32>,
    // $envmapsaturation, $envmapfresnel, fresnel scale, fresnel bias.
    envmap_params: vec4<f32>,
    // fresnel exponent, then $basealphaenvmapmask's scale, bias and exponent.
    fresnel_params: vec4<f32>,
    // $detailtint in rgb, $detailblendfactor in w.
    detail_tint: vec4<f32>,
    // $alphatestreference, or the fixed-function default of 0.7.
    alpha_test_reference: f32,
    // $detailblendmode, one of the prelude's TCOMBINE_* values.
    detail_blend_mode: i32,
    // VertexLitFlags, below.
    flags: u32,
    pad0: u32,
}

// `shader::VertexLitFlags`.
const FLAG_ALPHA_TEST: u32 = 1u;
const FLAG_NO_FOG: u32 = 2u;
const FLAG_BUMPMAP: u32 = 4u;
const FLAG_ENVMAP: u32 = 8u;
const FLAG_ENVMAP_MASK: u32 = 16u;
const FLAG_BASE_ALPHA_ENVMAP_MASK: u32 = 32u;
const FLAG_NORMAL_ALPHA_ENVMAP_MASK: u32 = 64u;
const FLAG_ENVMAP_FRESNEL: u32 = 128u;
const FLAG_SELFILLUM: u32 = 256u;
const FLAG_SELFILLUM_MASK: u32 = 512u;
const FLAG_DETAIL: u32 = 1024u;
const FLAG_HALF_LAMBERT: u32 = 2048u;
const FLAG_BLEND_TINT_BY_BASE_ALPHA: u32 = 4096u;

@group(1) @binding(0) var<uniform> material: VertexLitUniforms;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var base_sampler: sampler;
@group(1) @binding(3) var bump_texture: texture_2d<f32>;
@group(1) @binding(4) var bump_sampler: sampler;
@group(1) @binding(5) var detail_texture: texture_2d<f32>;
@group(1) @binding(6) var detail_sampler: sampler;
@group(1) @binding(7) var selfillum_mask_texture: texture_2d<f32>;
@group(1) @binding(8) var selfillum_mask_sampler: sampler;
@group(1) @binding(9) var envmap_mask_texture: texture_2d<f32>;
@group(1) @binding(10) var envmap_mask_sampler: sampler;
@group(1) @binding(11) var envmap_texture: texture_cube<f32>;
@group(1) @binding(12) var envmap_sampler: sampler;

// ---------------------------------------------------------------------------
// Group 3: the lighting this instance is drawn under
// ---------------------------------------------------------------------------
// Mirrors `uniforms::ModelLighting`. Not part of the material and not part of
// the draw: it is per model *instance*, which is what
// `R_StudioSetupLighting` computes once and every mesh of that model then
// shares. `Pass::set_model_lighting` is the setter.

struct Light {
    // rgb, and w = 1 for a directional light.
    color: vec4<f32>,
    // xyz, and w = 1 for a spot light.
    direction: vec4<f32>,
    position: vec4<f32>,
    // falloff, thetaDot, phiDot, 1/(thetaDot - phiDot).
    spot: vec4<f32>,
    // constant, linear, quadratic.
    attenuation: vec4<f32>,
}

struct ModelLighting {
    // +x, -x, +y, -y, +z, -z, in linear space.
    ambient_cube: array<vec4<f32>, 6>,
    lights: array<Light, 4>,
    count: u32,
    static_light: u32,
    ambient_light: u32,
    pad0: u32,
}

@group(3) @binding(0) var<uniform> lighting: ModelLighting;

// ---------------------------------------------------------------------------
// The lighting core
// ---------------------------------------------------------------------------
// `common_vertexlitgeneric_dx9.h` and the lighting half of `common_vs_fxc.h`.
// These live here rather than in the prelude because they read group 3, which
// only this shader declares.

// `PixelShaderAmbientLight` (`common_vertexlitgeneric_dx9.h:38`).
//
// Valve has two spellings of this — a vertex one that indexes the cube array
// dynamically and a pixel one that does not — and they compute the same thing.
// The pixel form is used for both here because WGSL cannot dynamically index a
// value array, and because "the same thing" is not an approximation: the six
// products are the same six products.
//
// The cube is stored `+x, -x, +y, -y, +z, -z`, so `is_negative` picks the odd
// slot. Swapping a pair lights a model from the wrong side, which reads as a
// level built wrong rather than as a shader bug.
fn ambient_light(world_normal: vec3<f32>) -> vec3<f32> {
    if lighting.ambient_light == 0u {
        return vec3<f32>(0.0);
    }
    let n_squared = world_normal * world_normal;
    let is_negative = vec3<f32>(world_normal < vec3<f32>(0.0)) * n_squared;
    let is_positive = n_squared - is_negative;

    return is_positive.x * lighting.ambient_cube[0].rgb
        + is_negative.x * lighting.ambient_cube[1].rgb
        + is_positive.y * lighting.ambient_cube[2].rgb
        + is_negative.y * lighting.ambient_cube[3].rgb
        + is_positive.z * lighting.ambient_cube[4].rgb
        + is_negative.z * lighting.ambient_cube[5].rgb;
}

// `VertexAttenInternal` (`common_vs_fxc.h:733`).
//
// Three terms folded together with two `mix`es rather than branches, which is
// Valve's shape and worth keeping: the light *type* is not a uniform here
// either, it is the `w` of two of the light's own vectors, so a branch would
// be per light rather than per draw.
//
//   distance: 1 / (a0 + a1*d + a2*d²)   -- `dst()` builds (1, d, d²)
//   spot:     saturate( pow( max( 1e-4, (cos - phiDot) * ooDot ), falloff ) )
//   select:   mix( dist, dist * spot, dir.w ) then mix( that, 1, color.w )
//
// The second `mix` is what makes a directional light unattenuated.
fn light_attenuation(light: Light, world_pos: vec3<f32>) -> f32 {
    var light_dir = light.position.xyz - world_pos;
    let dist_squared = dot(light_dir, light_dir);
    let one_over_dist = inverseSqrt(max(dist_squared, 1e-12));
    light_dir = light_dir * one_over_dist;

    // `dst( distSquared, ooDist ).xyz` is (1, d, d²).
    let dist = vec3<f32>(1.0, dist_squared * one_over_dist, dist_squared);
    let distance_atten = 1.0 / max(dot(light.attenuation.xyz, dist), 1e-6);

    let cos_theta = dot(light.direction.xyz, -light_dir);
    var spot_atten = (cos_theta - light.spot.z) * light.spot.w;
    spot_atten = pow(max(1e-4, spot_atten), light.spot.x);
    spot_atten = saturate(spot_atten);

    let atten = mix(distance_atten, distance_atten * spot_atten, light.direction.w);
    return mix(atten, 1.0, light.color.w);
}

// `CosineTermInternal` (`common_vs_fxc.h:781`), minus one CS:GO line.
//
// **`SoftenCosineTerm` is deliberately not applied.** The reference reads
//
//     NDotL = max( 0.0f, NDotL );
//     NDotL = SoftenCosineTerm( NDotL ); // For CS:GO
//
// and `SoftenCosineTerm` is `(d + d²) / 2` (`common_fxc.h:112`) — a CS:GO
// change to the diffuse falloff of every lit surface in the game, tagged as
// such in its own comment. Portal 2 predates it and this port targets Portal 2,
// so the plain saturated dot is what stays. The same line appears in
// `DiffuseTerm` (`common_vertexlitgeneric_dx9.h:99`) and is dropped there too.
fn cosine_term(light: Light, world_normal: vec3<f32>, world_pos: vec3<f32>, half_lambert: bool) -> f32 {
    // `normalize`, guarded. The reference writes a plain
    // `normalize( cLightInfo[i].pos.xyz - worldPos )`, which is a NaN when a
    // light sits exactly on the vertex being lit — and `mix` propagates it
    // even on the branch that discards the result, so one degenerate vertex
    // turns a whole surface into garbage rather than a black spot.
    //
    // Valve never hits it by construction, twice over: a real light has a real
    // position, and `CompilePixelShaderLocalLights` (`shaderapidx8.cpp:8434`)
    // even converts a *directional* light into a point light 10,000 units away
    // so that this expression stays well-defined. The `max` is cheaper than
    // relying on that and is the same answer everywhere else.
    let to_light = light.position.xyz - world_pos;
    let point_dir = to_light * inverseSqrt(max(dot(to_light, to_light), 1e-12));
    // A directional light's direction is in the struct; a point or spot
    // light's is derived. `color.w` selects, and the negation is Valve's:
    // `cLightInfo.dir` points the way the light shines.
    let light_dir = mix(point_dir, -light.direction.xyz, light.color.w);

    let n_dot_l = dot(world_normal, light_dir);
    if half_lambert {
        let scaled = n_dot_l * 0.5 + 0.5;
        return scaled * scaled;
    }
    return max(0.0, n_dot_l);
}

// `DoLightInternal` (`common_vs_fxc.h:837`), summed over the enabled lights.
fn local_lighting(world_pos: vec3<f32>, world_normal: vec3<f32>, half_lambert: bool) -> vec3<f32> {
    var color = vec3<f32>(0.0);
    for (var i = 0u; i < lighting.count; i = i + 1u) {
        let light = lighting.lights[i];
        color += light.color.rgb
            * cosine_term(light, world_normal, world_pos, half_lambert)
            * light_attenuation(light, world_pos);
    }
    return color;
}

// `DoLighting` (`common_vs_fxc.h:843`): the unbumped, per-vertex path.
//
// `static_lighting` is the vertex stream, which arrives in **gamma space and
// pre-multiplied by 1/2** — hence `gamma_to_linear( c * OVERBRIGHT )`, and
// hence a baked value of 0.5 meaning "white". Skipping either half of that is
// a plausible-looking wrong brightness rather than an error.
//
// The ambient cube is added under `bDynamicLight` in the original, which is
// `m_bAmbientLight || m_nNumLights > 0`; here `ambient_light` gates it
// directly, which is the same answer whenever the cube is zeroed while
// disabled — see `uniforms::ModelLighting::ambient_light`.
fn do_lighting(
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
    static_lighting: vec3<f32>,
    half_lambert: bool,
) -> vec3<f32> {
    var color = vec3<f32>(0.0);
    if lighting.static_light != 0u {
        color += gamma_to_linear(static_lighting * OVERBRIGHT);
    }
    color += local_lighting(world_pos, world_normal, half_lambert);
    color += ambient_light(world_normal);
    return color;
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
    // The unbumped path's per-vertex lighting result, in `rgb`; the modulation
    // alpha in `w`. `o.color` in the original.
    @location(6) color: vec4<f32>,
}

@vertex
fn vs_main(vertex: ModelVertexInput) -> VertexOutput {
    var out: VertexOutput;

    let world = world_position(vertex.position);
    // The upper 3x3 of the model matrix, which is what `SkinPositionAndNormal`
    // applies (`common_vs_fxc.h:200`) — a bone transform, so rigid, so no
    // inverse transpose. A model matrix with non-uniform scale would bend
    // these; nothing produces one, and the day something does this is where it
    // shows up.
    let normal_matrix = mat3x3<f32>(
        draw.model[0].xyz,
        draw.model[1].xyz,
        draw.model[2].xyz,
    );
    let world_normal = normalize(normal_matrix * vertex.normal);
    let world_tangent = normalize(normal_matrix * vertex.tangent.xyz);

    out.clip_position = clip_position(world);
    out.world_position = world;
    out.world_normal = world_normal;
    out.world_tangent = vec4<f32>(world_tangent, vertex.tangent.w);

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

    // The unbumped path lights here, once per vertex, and the fragment shader
    // reads the interpolated result. The bumped path leaves this at zero and
    // lights per pixel instead — see the header.
    var vertex_lighting = vec3<f32>(0.0);
    if (material.flags & FLAG_BUMPMAP) == 0u {
        vertex_lighting = do_lighting(
            world,
            world_normal,
            vertex.color.rgb,
            (material.flags & FLAG_HALF_LAMBERT) != 0u,
        );
    }
    out.color = vec4<f32>(vertex_lighting, draw.modulation.a);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let flags = material.flags;
    let half_lambert = (flags & FLAG_HALF_LAMBERT) != 0u;

    var base_color = textureSample(base_texture, base_sampler, in.texcoord);

    // The detail texture composites into the albedo *before* lighting for ten
    // of the thirteen modes, and after it for two more — which is why it is
    // sampled once here and used twice.
    var detail_color = vec4<f32>(0.0);
    if (flags & FLAG_DETAIL) != 0u {
        detail_color = textureSample(detail_texture, detail_sampler, in.detail_texcoord);
        detail_color = vec4<f32>(detail_color.rgb * material.detail_tint.rgb, detail_color.a);
        base_color = texture_combine(
            base_color,
            detail_color,
            material.detail_blend_mode,
            material.detail_tint.w,
        );
    }

    // The normal, and with it the envmap mask that shares the map's alpha.
    var world_normal = normalize(in.world_normal);
    var specular_factor = 1.0;
    if (flags & FLAG_BUMPMAP) != 0u {
        let normal_texel = textureSample(bump_texture, bump_sampler, in.bump_texcoord);
        let tangent_normal = normal_texel.xyz * 2.0 - 1.0;
        if (flags & FLAG_NORMAL_ALPHA_ENVMAP_MASK) != 0u {
            specular_factor = normal_texel.a;
        }
        // `vWorldBinormal = cross( normal, tangent ) * tangent.w`
        // (`vertexlit_and_unlit_generic_bump_ps2x.fxc:317`). The `w` is the
        // handedness the `.vvd` stored; drop it and every mirrored UV island
        // lights inside out.
        let tangent = normalize(in.world_tangent.xyz);
        let binormal = cross(world_normal, tangent) * in.world_tangent.w;
        world_normal = normalize(vec3_tangent_to_world(
            tangent_normal,
            world_normal,
            tangent,
            binormal,
        ));
    }

    if (flags & FLAG_ENVMAP_MASK) != 0u {
        specular_factor = textureSample(envmap_mask_texture, envmap_mask_sampler, in.texcoord).r;
    }
    if (flags & FLAG_BASE_ALPHA_ENVMAP_MASK) != 0u {
        // `scale * pow( a, exp ) + bias`, from `$basealphaenvmapmaskminmaxexp`.
        // Its default of `[1 0 1]` gives scale -1, bias 1 — so the *default*
        // meaning of this parameter is `1 - baseColor.a`, which is inverted
        // relative to what the name suggests and is what Valve's comment calls
        // "the legacy behavior".
        specular_factor *= saturate(
            material.fresnel_params.y * pow(base_color.a, material.fresnel_params.w)
                + material.fresnel_params.z,
        );
    }

    let eye_dir = frame.eye_pos_water_height.xyz - in.world_position;
    if (flags & FLAG_ENVMAP_FRESNEL) != 0u {
        var f = 1.0 - saturate(dot(world_normal, normalize(eye_dir)));
        f = material.envmap_params.z * pow(f, material.fresnel_params.x) + material.envmap_params.w;
        specular_factor *= f;
    }

    // --- diffuse lighting ---------------------------------------------------
    var diffuse_lighting: vec3<f32>;
    if (flags & FLAG_BUMPMAP) != 0u {
        // `PixelShaderDoLighting` with `bStaticLight = false`: a bumped model
        // is lit entirely by the ambient cube and the local lights, because
        // the baked per-vertex stream cannot be re-evaluated against a
        // per-pixel normal. See the header.
        diffuse_lighting = ambient_light(world_normal)
            + local_lighting(in.world_position, world_normal, half_lambert);
    } else {
        diffuse_lighting = in.color.rgb;
    }

    var albedo = base_color.rgb;
    var alpha = 1.0;
    // `alpha *= lerp( 1, baseColor.a, g_EyePos_BaseTextureTranslucency.w )`
    // (`vertexlit_and_unlit_generic_ps2x.fxc:556`). The two flags tested here
    // claim base alpha for something else, so it must not also become opacity.
    //
    // **The `lerp`'s weight is not ported**, and this is the one place in this
    // shader that knowingly differs. That `w` is
    // `TextureIsTranslucent( BASETEXTURE, true )` (`:1535`) — 1 for a
    // `$translucent` or `$alphatest` material and **0 for an opaque one**, so
    // Valve writes an opaque material's alpha as 1 while this writes whatever
    // its base texture happens to carry. It agrees exactly wherever the value
    // is read: a blended material and an alpha-tested one both have `w` of 1,
    // so blending and the `discard` below are unaffected. What differs is the
    // *frame buffer's* alpha channel for opaque draws, and the thing that reads
    // that is the underwater fog pass, which is not ported. Porting it means
    // threading the resolved base texture into `vertex_lit_uniforms`, which is
    // the same change `unlitgeneric.wgsl` needs for the same reason.
    if (flags & (FLAG_BASE_ALPHA_ENVMAP_MASK | FLAG_SELFILLUM)) == 0u {
        alpha *= base_color.a;
    }

    // `$color` reaches the *lighting*, not the albedo — which is why a tinted
    // model tints its lit colour and not its texture. `$blendtintbybasealpha`
    // makes base alpha decide how much of it lands; without it the saturate of
    // `a + 1` is 1 everywhere and the tint applies in full.
    var tint_amount = 1.0;
    if (flags & FLAG_BLEND_TINT_BY_BASE_ALPHA) != 0u {
        tint_amount = saturate(base_color.a);
    }
    diffuse_lighting *= mix(vec3<f32>(1.0), draw.modulation.rgb, tint_amount);

    alpha *= in.color.a;

    var diffuse_component = albedo * diffuse_lighting;

    if (flags & FLAG_DETAIL) != 0u {
        diffuse_component = texture_combine_post_lighting(
            diffuse_component,
            detail_color,
            material.detail_blend_mode,
            material.detail_tint.w,
        );
    }

    // --- self-illumination --------------------------------------------------
    if (flags & FLAG_SELFILLUM) != 0u {
        // `vSelfIllumMask = lerp( baseColor.aaa, mask, g_SelfIllumMaskControl )`
        // (`vertexlit_and_unlit_generic_ps2x.fxc:672`): the mask texture
        // replaces base alpha when one is bound, and `$selfillummaskscale`
        // scales the result either way.
        var mask = vec3<f32>(base_color.a);
        if (flags & FLAG_SELFILLUM_MASK) != 0u {
            mask = textureSample(
                selfillum_mask_texture,
                selfillum_mask_sampler,
                in.texcoord,
            ).rgb;
        }
        let emitted = material.selfillum_tint.rgb * albedo;
        diffuse_component = mix(
            diffuse_component,
            emitted,
            saturate(mask * material.selfillum_tint.w),
        );
    }

    // --- environment map ----------------------------------------------------
    var specular_lighting = vec3<f32>(0.0);
    if (flags & FLAG_ENVMAP) != 0u {
        let reflect_vector = calc_reflection_vector_unnormalized(world_normal, eye_dir);
        // `ENV_MAP_SCALE` is `cLightScale.z`, the tone mapper's envmap scale.
        specular_lighting = frame.light_scale.z
            * textureSample(envmap_texture, envmap_sampler, reflect_vector).rgb;
        specular_lighting *= specular_factor;
        specular_lighting *= material.envmap_tint.rgb;

        // `$envmapcontrast` squares the reflection toward the darks;
        // `$envmapsaturation` pulls it toward its own luminance. Both are
        // `lerp`s so that 0 and 1 are the identity at the ends content expects.
        let squared = specular_lighting * specular_lighting;
        specular_lighting = mix(specular_lighting, squared, material.envmap_tint.w);
        let grey = vec3<f32>(dot(specular_lighting, vec3<f32>(0.299, 0.587, 0.114)));
        specular_lighting = mix(grey, specular_lighting, material.envmap_params.x);
    }

    let result = diffuse_component + specular_lighting;

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
