// Refract: glass, and anything else that warps what is behind it.
//
// Translated from `materialsystem/stdshaders/Refract_vs20.fxc` and
// `refract_ps2x.fxc`, taking the `[ps20b]` / non-console branch everywhere.
// The combo bucketing is written down on `shader::refract_uniforms`.
//
// Prepended by `shaders/prelude.wgsl`, which declares groups 0 and 2,
// `ModelVertexInput`, the tangent-space helpers and the fog and output helpers.
//
// ---------------------------------------------------------------------------
// The one structural difference from every shader before it
// ---------------------------------------------------------------------------
// This shader reads the frame it is being drawn into. Not the attachment —
// that would be illegal and is why `RenderContext::update_refract_texture`
// exists — but a *copy* of it, taken after the opaque scene was drawn and
// bound in group 3. `UpdateRefractTexture` (`game/client/view_scene.h:40`) is
// that copy, and `SetFrameBufferCopyTexture` is what points the shader's
// sampler at it.
//
// A material that supplies its own `$basetexture` reads *that* instead, from
// group 1, and never touches group 3 — Valve's `BindTexture` versus
// `BindStandardTexture` fork at `refract_dx9_helper.cpp:273`, which here is
// `FLAG_BASE_TEXTURE` and `sample_refract` below. Six of Portal 2's glass
// materials take that branch, which is why they need no copy of the frame at
// all.
//
// ---------------------------------------------------------------------------
// What is here and what is not
// ---------------------------------------------------------------------------
// Here: the screen-space warp and its `$refractamount` scale, `$refracttint`
// with `$refracttinttexture`, the four-tap `$bluramount` kernel, the
// `$localrefract` in-texture refraction with its depth and aspect fixup,
// `$fadeoutonsilhouette`, `$envmap` with tint, contrast, saturation and
// fresnel, and fog.
//
// Not here, each pinned on a measurement or a capability rather than deferred
// — see `shader::refract_uniforms`: the secondary normal map, `$masked`,
// `$magnifyenable`, `$vertexcolormodulate`, the viewport fixup and
// viewport-edge mirroring, NVIDIA's stereo texcoord fixup, depth-to-dest-alpha,
// skinning, and the flashlight.

// ---------------------------------------------------------------------------
// Group 1: the material
// ---------------------------------------------------------------------------
// Mirrors `shader::RefractUniforms`, field for field and pad for pad.

struct RefractUniforms {
    // $bumptransform, as two rows dotted against (u, v, 0, 1). The only
    // texture transform this shader has.
    bump_transform: array<vec4<f32>, 2>,
    // g_RefractTint, already gamma-decoded on the CPU.
    refract_tint: vec4<f32>,
    // g_EnvmapTint (also already decoded) in rgb, g_EnvmapContrast in w.
    envmap_tint: vec4<f32>,
    // g_RefractScale, g_EnvmapSaturation, the aspect fixup, g_flRefractDepth.
    refract_params: vec4<f32>,
    // RefractFlags, below.
    flags: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

// `shader::RefractFlags`.
const FLAG_NO_FOG: u32 = 1u;
const FLAG_BASE_TEXTURE: u32 = 2u;
const FLAG_ENVMAP: u32 = 4u;
const FLAG_REFRACT_TINT_TEXTURE: u32 = 8u;
const FLAG_BLUR: u32 = 16u;
const FLAG_FADE_OUT_ON_SILHOUETTE: u32 = 32u;
const FLAG_LOCAL_REFRACT: u32 = 64u;

@group(1) @binding(0) var<uniform> material: RefractUniforms;
// `$basetexture` — the image to warp, when the material supplies one.
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var base_sampler: sampler;
// `$normalmap`. `NormalSampler`, sampler 3 in the original.
@group(1) @binding(3) var bump_texture: texture_2d<f32>;
@group(1) @binding(4) var bump_sampler: sampler;
// `$refracttinttexture`. `RefractTintSampler`, sampler 5.
@group(1) @binding(19) var refract_tint_texture: texture_2d<f32>;
@group(1) @binding(20) var refract_tint_sampler: sampler;
// `$envmap`. `EnvmapSampler`, sampler 4.
@group(1) @binding(11) var envmap_texture: texture_cube<f32>;
@group(1) @binding(12) var envmap_sampler: sampler;

// ---------------------------------------------------------------------------
// Group 3: the scene, as it stood before this pass
// ---------------------------------------------------------------------------
// `RefractSampler`, sampler 2, pointed at `TEXTURE_FRAME_BUFFER_FULL_TEXTURE_0`
// — which `SetFrameBufferCopyTexture` points at the power-of-two frame-buffer
// copy. Render-context state, like a lightmap page: it belongs to neither the
// material nor the draw. `shader::ContextBinding::FrameBufferCopy`.

@group(3) @binding(0) var scene_texture: texture_2d<f32>;
@group(3) @binding(1) var scene_sampler: sampler;

// One sampler in the original, two bindings here, because one of them is a
// material texture and the other is context state. `FLAG_BASE_TEXTURE` is the
// same `if` Valve makes when it decides which to bind.
//
// `tex2Dsrgb` in the reference, which on this platform is a plain `tex2D`:
// `SHADER_SRGB_READ` is 0 off the 360, so the decode is the hardware's and
// both of these textures are sRGB-formatted.
fn sample_refract(uv: vec2<f32>) -> vec4<f32> {
    if (material.flags & FLAG_BASE_TEXTURE) != 0u {
        return textureSample(base_texture, base_sampler, uv);
    }
    return textureSample(scene_texture, scene_sampler, uv);
}

// `g_BlurFraction` (`refract_ps2x.fxc:75`) — **a hard-coded 1/512 of the
// source, not a pixel**, so the blur is wider on a small texture and narrower
// on a large one and does not scale with the window. Valve's constant, kept.
const BLUR_FRACTION: f32 = 1.0 / 512.0;
const HALF_BLUR_FRACTION: f32 = 0.5 * BLUR_FRACTION;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    // $bumptransform applied to the vertex's texture coordinate. Also the
    // coordinate the base texture, the refract tint and every local-refract
    // lookup use.
    @location(0) bump_texcoord: vec2<f32>,
    // The eye vector in *tangent* space, unit length. `vTangentEyeVect`.
    @location(1) tangent_eye_vector: vec3<f32>,
    @location(2) world_normal: vec3<f32>,
    @location(3) world_tangent: vec3<f32>,
    @location(4) world_binormal: vec3<f32>,
    // (x', y', w): the clip position already mapped towards texture space, with
    // the `w` divide left for the fragment shader. See `vs_main`.
    @location(5) refract_xyw: vec3<f32>,
    // The eye-to-vertex direction in world space, unit length.
    @location(6) world_view_vector: vec3<f32>,
    // `worldPos_projPosZ.xyz`, for pixel fog.
    @location(7) world_position: vec3<f32>,
}

@vertex
fn vs_main(vertex: ModelVertexInput) -> VertexOutput {
    var out: VertexOutput;

    let world = world_position(vertex.position);
    // The upper 3x3 of the model matrix — a rigid placement, so no inverse
    // transpose. Same reasoning as `vertexlitgeneric.wgsl`.
    let normal_matrix = mat3x3<f32>(
        draw.model[0].xyz,
        draw.model[1].xyz,
        draw.model[2].xyz,
    );
    let world_normal = normalize(normal_matrix * vertex.normal);
    let world_tangent = normalize(normal_matrix * vertex.tangent.xyz);
    // `vWorldBinormal = cross( normal, tangent ) * tangent.w`. The `w` is the
    // handedness the `.vvd` stored; drop it and every mirrored UV island
    // refracts the wrong way. Valve's `SkinPositionNormalAndTangentSpace`
    // builds the same thing from `vObjTangent.w`.
    let world_binormal = cross(world_normal, world_tangent) * vertex.tangent.w;

    let clip = clip_position(world);
    out.clip_position = clip;
    out.world_position = world;
    out.world_normal = world_normal;
    out.world_tangent = world_tangent;
    out.world_binormal = world_binormal;

    // Clip space to texture space, exactly as `Refract_vs20.fxc:113` writes it:
    //
    //     vRefractPos.x =  vProjPos.x;
    //     vRefractPos.y = -vProjPos.y;         // invert Y
    //     vRefractPos   = (vRefractPos + vProjPos.w) * 0.5;
    //
    // and the fragment shader divides by `w`. **It carries across unchanged**,
    // which is worth stating because the world draw's winding does not (see
    // `rustdocs/ENGINE.md` gotcha 1): the y flip here is between clip space,
    // which is y-up, and the texture, whose origin is top-left. WebGPU agrees
    // with D3D9 on both of those. What the two disagree about is which way a
    // *front* face winds, and that is settled by the pipeline, not here.
    //
    // Left as three interpolated components with the divide downstream rather
    // than collapsed into `@builtin(position) / screen_size`, which would be
    // equivalent: this way the file reads against the one it came from.
    let refract_pos = (vec2<f32>(clip.x, -clip.y) + clip.w) * 0.5;
    out.refract_xyw = vec3<f32>(refract_pos, clip.w);

    // `vWorldEyeVect = normalize( cEyePos - vWorldPos )`, then the output is
    // its negation — so `world_view_vector` points *at* the surface.
    let eye_vector = normalize(frame.eye_pos_water_height.xyz - world);
    out.world_view_vector = -eye_vector;
    out.tangent_eye_vector = vec3_world_to_tangent(
        eye_vector,
        world_normal,
        world_tangent,
        world_binormal,
    );

    out.bump_texcoord = transform_texcoord(
        vertex.texcoord,
        material.bump_transform[0],
        material.bump_transform[1],
    );
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let flags = material.flags;
    let refract_scale = material.refract_params.x;
    let envmap_saturation = material.refract_params.y;
    let refract_depth = material.refract_params.w;

    var fog_type = PIXEL_FOG_TYPE_RANGE;
    if (flags & FLAG_NO_FOG) != 0u {
        fog_type = PIXEL_FOG_TYPE_NONE;
    }
    let fog_factor = calc_pixel_fog_factor(fog_type, in.world_position);

    // How much of the *warped* image survives, against the unwarped one.
    // 1 unless `$fadeoutonsilhouette`, which cubes the facing ratio so that
    // glass seen edge-on shows the scene straight through.
    var blend = 1.0;
    if (flags & FLAG_FADE_OUT_ON_SILHOUETTE) != 0u {
        let facing = saturate(dot(-in.world_view_vector, in.world_normal));
        blend = facing * facing * facing;
    }

    // The normal map is the whole shader: `xy` is the screen-space offset and
    // `a` scales it. A material whose `$normalmap` has no alpha channel reads
    // `a` as 1, which is what the six glass materials in `sp_a1_intro1` do —
    // theirs is DXT1.
    let normal_texel = textureSample(bump_texture, bump_sampler, in.bump_texcoord);
    let tangent_normal = vec4<f32>(normal_texel.xyz * 2.0 - 1.0, normal_texel.a);

    // `$refracttinttexture` is multiplied in at **twice** the tint, which is
    // Valve's `2.0 * g_RefractTint * tex2D( ... ).rgb` and is the mod2x
    // convention: a mid-grey texel is the identity.
    var tint = material.refract_tint.rgb;
    if (flags & FLAG_REFRACT_TINT_TEXTURE) != 0u {
        tint = 2.0
            * tint
            * textureSample(
                refract_tint_texture,
                refract_tint_sampler,
                in.bump_texcoord,
            ).rgb;
    }

    // Where this pixel is in the copy of the scene, and where the normal map
    // says to read instead.
    let unwarped_uv = in.refract_xyw.xy / in.refract_xyw.z;
    let warped_uv = tangent_normal.xy * (tangent_normal.a * refract_scale) + unwarped_uv;

    var result = vec3<f32>(0.0);
    if (flags & FLAG_BLUR) == 0u {
        let warped = sample_refract(warped_uv).rgb;
        let unwarped = sample_refract(unwarped_uv).rgb;
        result = mix(unwarped, warped * tint, blend);
    } else {
        // "use polyphase magic to convert 9 lookups into 4": a 3x3 box filter
        // built out of four bilinear fetches, weighted 4/9, 2/9, 2/9 and 1/9
        // by how many of the nine taps each one straddles. The offsets are
        // Valve's to the half-texel.
        let upper_2x2 = warped_uv - vec2<f32>(HALF_BLUR_FRACTION, HALF_BLUR_FRACTION);
        let right_1x2 = warped_uv + vec2<f32>(BLUR_FRACTION, -HALF_BLUR_FRACTION);
        let lower_2x1 = warped_uv + vec2<f32>(-HALF_BLUR_FRACTION, BLUR_FRACTION);
        let singleton = warped_uv + vec2<f32>(BLUR_FRACTION, BLUR_FRACTION);
        var blurred = sample_refract(upper_2x2).rgb * 0.4444444;
        blurred += sample_refract(right_1x2).rgb * 0.2222222;
        blurred += sample_refract(lower_2x1).rgb * 0.2222222;
        blurred += sample_refract(singleton).rgb * 0.1111111;

        let unwarped = sample_refract(unwarped_uv).rgb;
        result = mix(unwarped, blurred * tint, blend);
    }

    // **Overwrites the result above rather than combining with it**, which is
    // what `#if ( LOCALREFRACT )` does in the original: it is a second, whole
    // answer to "what colour is this pixel", not a term. All six
    // `$localrefract` materials in the game also set `$bluramount 1`, so they
    // pay for the four-tap blur and discard it. See `shader::refract_uniforms`.
    if (flags & FLAG_LOCAL_REFRACT) != 0u {
        // The interpolated tangent-space eye vector is not accurate enough for
        // this, so it is rebuilt per pixel — Valve's comment says exactly that.
        let vertex_to_eye_ws = frame.eye_pos_water_height.xyz - in.world_position;
        let vertex_to_eye_ts = vec3_world_to_tangent_normalized(
            vertex_to_eye_ws,
            in.world_normal,
            in.world_tangent,
            in.world_binormal,
        );
        // `refract( -V, N, 0.66 )` is commented out above this in the original,
        // and what ships is "just use the vert to eye vector as the refract
        // vector". So there is no index of refraction anywhere in this shader.
        let refract_ts = vertex_to_eye_ts;
        // `R · GeometricNormal`, "so just use tangent z" — and *negated*, so
        // for a front-facing surface this is negative and the offsets below
        // point back towards the eye.
        let r_dot_n = -refract_ts.z;

        var refracted_uv = refract_ts.xy / r_dot_n;
        refracted_uv += tangent_normal.xy;
        refracted_uv += (1.0 - tangent_normal.z) * refract_ts.xy / r_dot_n;
        // `g_vRefractTextureAspectFixup.xy` is `(float(height/width), 1)` with
        // that division done in *integers* on the CPU — see
        // `shader::refract_uniforms`, which is where the whole number comes
        // from and why it is 4 for the shipped glass.
        refracted_uv *= vec2<f32>(material.refract_params.z, 1.0) * refract_depth;
        refracted_uv += in.bump_texcoord;

        let refracted = sample_refract(saturate(refracted_uv));
        let nearby = sample_refract(saturate(in.bump_texcoord + tangent_normal.xy * 0.1));
        // 2.5% of a *greyscale* reading of the nearby sample's alpha, which
        // looks like a typo for `.rgb` and is not: it is how the shipped glass
        // picks up a faint bloom off its own alpha channel.
        let mixed = mix(refracted.rgb, vec3<f32>(nearby.a), 0.025);

        let fresnel_term = pow(tangent_normal.z, 3.0);
        result = mixed * fresnel_term * tint;
        let unwarped = sample_refract(in.bump_texcoord).rgb;
        result = mix(unwarped, result, blend);
    }

    if (flags & FLAG_ENVMAP) != 0u {
        let world_normal = vec3_tangent_to_world(
            tangent_normal.xyz,
            in.world_normal,
            in.world_tangent,
            in.world_binormal,
        );
        // **Valve passes the *tangent-space* eye vector to a function whose
        // other argument is a world-space normal** (`refract_ps2x.fxc:338`),
        // where `vertexlitgeneric` passes the world-space one. It is a bug in
        // the shipped shader and it changes what the glass reflects, so it is
        // reproduced rather than corrected: the reflection every pane of glass
        // in Portal 2 shows is the one this line produces. To get the
        // physically intended vector, pass `-in.world_view_vector`.
        let reflect_vector =
            calc_reflection_vector_unnormalized(world_normal, in.tangent_eye_vector);
        // `ENV_MAP_SCALE` is `cLightScale.z` — `uniforms::ENVMAP_SCALE`.
        var specular = frame.light_scale.z
            * textureSample(envmap_texture, envmap_sampler, reflect_vector).rgb;
        // The spec mask is the normal map's alpha, the same channel that
        // scaled the warp.
        specular *= tangent_normal.a;
        specular *= material.envmap_tint.rgb;

        let squared = specular * specular;
        specular = mix(specular, squared, material.envmap_tint.w);
        let grey = vec3<f32>(dot(specular, vec3<f32>(0.299, 0.587, 0.114)));
        specular = mix(grey, specular, envmap_saturation);

        // `g_flReflectance` is a literal 0.6 inside the shader, and the
        // exponent on the fresnel term is a literal `1.0` — so `pow` is the
        // identity and this is a straight lerp from 0.6 at grazing incidence
        // to 1.0 head-on. Written out rather than folded, because the
        // exponent is the thing a later version would change.
        let n_dot_v = saturate(dot(tangent_normal.xyz, in.tangent_eye_vector));
        let reflectance = 0.6;
        let fresnel_term = reflectance + (1.0 - reflectance) * (1.0 - n_dot_v);

        result += specular * fresnel_term;
    }

    // The output alpha is the normal map's, untouched — which for a material
    // whose normal map has no alpha channel is 1.
    let alpha = tangent_normal.a;

    // `TONEMAP_SCALE_NONE`, and it is not an omission: what this shader mostly
    // returns is a *resample of an already-exposed frame*, so multiplying by
    // the exposure again would square it. The environment map is the one term
    // that is not, and Valve leaves it unscaled too.
    return final_output(vec4<f32>(result, alpha), fog_factor, fog_type, TONEMAP_SCALE_NONE);
}
