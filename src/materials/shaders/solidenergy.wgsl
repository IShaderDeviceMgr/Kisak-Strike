// SolidEnergy: fizzler fields, laser planes, light bridges and tractor beams —
// an unlit, usually additive surface whose colour is a texture, detail layers
// and, for a fizzler, a scrolling flow field.
//
// Translated from `materialsystem/stdshaders/solidenergy_vs20.fxc` and
// `solidenergy_ps20b.fxc`, reached through `solidenergy_dx9_helper.cpp`.
// Every static and dynamic combo of those two files is a bit in
// `material.flags` here; `shader::solid_energy_uniforms` says which bits are
// pinned and why.
//
// Only the *brush* form is compiled: `MODELFORMAT` is pinned to 0, because
// the model form's content — the tractor beam and the light bridge — is a
// mesh the client builds at run time, and neither class is ported. So the
// frame comes in on the vertex (`WorldTangentVertexInput`) the way
// `TangentSpaceComputeBasis` wrote it, rather than decompressed from a
// model's normal and user data.
//
// Prepended by `shaders/prelude.wgsl`, which declares groups 0 and 2 and the
// output helpers.

// ---------------------------------------------------------------------------
// Group 1: the material
// ---------------------------------------------------------------------------
// Mirrors `shader::SolidEnergyUniforms`, field for field and pad for pad.

struct SolidEnergyUniforms {
    // $basetexturetransform — VS SHADER_SPECIFIC_CONST_0/1.
    base_texture_transform: array<vec4<f32>, 2>,
    // $detail1texturetransform scaled by $detail1scale — CONST_2/3.
    detail1_transform: array<vec4<f32>, 2>,
    // $detail2texturetransform scaled by $detail2scale — CONST_6/7.
    detail2_transform: array<vec4<f32>, 2>,
    // PS c0, c1, c2: $tangenttopacityranges, $tangentsopacityranges,
    // $fresnelopacityranges.
    tangent_t_opacity: vec4<f32>,
    tangent_s_opacity: vec4<f32>,
    fresnel_opacity: vec4<f32>,
    // PS c6: (flow world uv scale, 0, 0, $outputintensity).
    flow_params1: vec4<f32>,
    // PS c7: (flow interval in seconds, uv scroll distance, 0, lerp exponent).
    flow_params2: vec4<f32>,
    // PS c8: $flow_color in rgb.
    flow_color: vec4<f32>,
    // PS c9: $flow_vortex_color in rgb, $flow_vortex_size in w.
    vortex_params: vec4<f32>,
    // VS CONST_9: $flow_vortex_pos1, $flow_noise_scale.
    vortex_pos1_noise_scale: vec4<f32>,
    // VS CONST_10: $flow_vortex_pos2, $flow_normaluvscale.
    vortex_pos2_normal_uv_scale: vec4<f32>,
    // PS c3.z and c3.w: $powerup, $flow_color_intensity. zw unused.
    power: vec4<f32>,
    flags: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

// `shader::SolidEnergyFlags`.
const FLAG_DETAIL1: u32 = 1u;
const FLAG_DETAIL2: u32 = 2u;
const FLAG_DETAIL1_BLEND_MODE: u32 = 4u;
const FLAG_DETAIL2_BLEND_MODE: u32 = 8u;
const FLAG_TANGENT_T_OPACITY: u32 = 16u;
const FLAG_TANGENT_S_OPACITY: u32 = 32u;
const FLAG_FRESNEL_OPACITY: u32 = 64u;
const FLAG_VERTEX_COLOR: u32 = 128u;
const FLAG_FLOWMAP: u32 = 256u;
const FLAG_FLOW_CHEAP: u32 = 512u;
const FLAG_ADDITIVE: u32 = 1024u;
const FLAG_ACTIVE: u32 = 2048u;
const FLAG_POWERUP: u32 = 4096u;
const FLAG_VORTEX1: u32 = 8192u;
const FLAG_VORTEX2: u32 = 16384u;

@group(1) @binding(0) var<uniform> material: SolidEnergyUniforms;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var base_sampler: sampler;
@group(1) @binding(5) var detail1_texture: texture_2d<f32>;
@group(1) @binding(6) var detail1_sampler: sampler;
@group(1) @binding(31) var detail2_texture: texture_2d<f32>;
@group(1) @binding(32) var detail2_sampler: sampler;
@group(1) @binding(33) var flow_map: texture_2d<f32>;
@group(1) @binding(34) var flow_map_sampler: sampler;
@group(1) @binding(35) var flow_noise: texture_2d<f32>;
@group(1) @binding(36) var flow_noise_sampler: sampler;
@group(1) @binding(37) var flow_bounds: texture_2d<f32>;
@group(1) @binding(38) var flow_bounds_sampler: sampler;

fn has(flag: u32) -> bool {
    return (material.flags & flag) != 0u;
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    // `vUV0_UV1`: the base coordinate, then detail 1's — or, for a flow map,
    // the flow coordinate scaled by `$flow_normaluvscale`.
    @location(0) uv0_uv1: vec4<f32>,
    // `vFlowUV_UV2`: the flow coordinate, then detail 2's — or, for a flow
    // map, the flow coordinate scaled by `$flow_noise_scale`.
    @location(1) flow_uv_uv2: vec4<f32>,
    // `vIteratedProjPos`, for `ComputeCameraFade`.
    @location(2) proj_pos: vec4<f32>,
    @location(3) vortex1: vec3<f32>,
    @location(4) vortex2: vec3<f32>,
    // `vWorldNormal_worldEyeX`, `vWorldTangent_worldEyeY`,
    // `vTangentAlignedView_worldEyeZ` — three vectors with the eye direction
    // spread across their `w`s.
    @location(5) normal_eye_x: vec4<f32>,
    @location(6) tangent_eye_y: vec4<f32>,
    @location(7) aligned_view_eye_z: vec4<f32>,
    @location(8) color: vec4<f32>,
}

@vertex
fn vs_main(vertex: WorldTangentVertexInput) -> VertexOutput {
    var out: VertexOutput;

    let world = world_position(vertex.position);
    let proj = clip_position(world);
    out.clip_position = proj;
    out.proj_pos = proj;
    out.color = vertex.color;

    // `mul3x3( v, cModel[0] )` for all three: the tangents and the normal go
    // through the draw's rotation, which for a brush entity is its angles.
    let model3 = mat3x3<f32>(draw.model[0].xyz, draw.model[1].xyz, draw.model[2].xyz);
    let world_tangent_s = model3 * vertex.tangent_s;
    let world_tangent_t = model3 * vertex.tangent_t;
    let world_normal = model3 * vertex.normal;

    let opacity = has(FLAG_TANGENT_T_OPACITY) || has(FLAG_TANGENT_S_OPACITY)
        || has(FLAG_FRESNEL_OPACITY);
    var eye_dir = vec3<f32>(0.0);
    out.normal_eye_x = vec4<f32>(0.0);
    out.tangent_eye_y = vec4<f32>(0.0);
    out.aligned_view_eye_z = vec4<f32>(0.0);
    if opacity {
        eye_dir = normalize(frame.eye_pos_water_height.xyz - world);
        out.normal_eye_x = vec4<f32>(world_normal, eye_dir.x);
        out.tangent_eye_y.w = eye_dir.y;
        out.aligned_view_eye_z.w = eye_dir.z;
    }
    // T wins when both are set — `solidenergy_dx9_helper.cpp:117` clears S —
    // so these three are exclusive by construction.
    if has(FLAG_TANGENT_T_OPACITY) {
        out.tangent_eye_y = vec4<f32>(world_tangent_t, out.tangent_eye_y.w);
        let right = cross(world_tangent_s, eye_dir);
        let aligned = normalize(cross(world_tangent_s, right));
        out.aligned_view_eye_z = vec4<f32>(aligned, out.aligned_view_eye_z.w);
    } else if has(FLAG_TANGENT_S_OPACITY) {
        let right = cross(world_tangent_t, eye_dir);
        let aligned = normalize(cross(world_tangent_t, right));
        out.aligned_view_eye_z = vec4<f32>(aligned, out.aligned_view_eye_z.w);
        out.tangent_eye_y = vec4<f32>(world_tangent_s, out.tangent_eye_y.w);
    }

    let base_uv = transform_texcoord(
        vertex.texcoord,
        material.base_texture_transform[0],
        material.base_texture_transform[1],
    );
    var detail1_uv = vec2<f32>(0.0);
    if has(FLAG_DETAIL1) {
        detail1_uv = transform_texcoord(
            vertex.texcoord,
            material.detail1_transform[0],
            material.detail1_transform[1],
        );
    }
    var detail2_uv = vec2<f32>(0.0);
    if has(FLAG_DETAIL2) {
        detail2_uv = transform_texcoord(
            vertex.texcoord,
            material.detail2_transform[0],
            material.detail2_transform[1],
        );
    }
    out.uv0_uv1 = vec4<f32>(base_uv, detail1_uv);
    out.flow_uv_uv2 = vec4<f32>(0.0, 0.0, detail2_uv);

    out.vortex1 = vec3<f32>(0.0);
    out.vortex2 = vec3<f32>(0.0);
    if has(FLAG_FLOWMAP) {
        // **The flow is in world units along the texture's axes**, not in the
        // texture's own coordinates: `dot( worldPos, vWorldTangentS )`. That is
        // what makes a fizzler's field continuous across every brush face of
        // it and the same density on a 64-unit gap as on a 256-unit one.
        let flow_uv = vec2<f32>(dot(world, world_tangent_s), dot(world, world_tangent_t));
        out.flow_uv_uv2 = vec4<f32>(flow_uv, flow_uv * material.vortex_pos1_noise_scale.w);
        out.uv0_uv1 = vec4<f32>(base_uv, flow_uv * material.vortex_pos2_normal_uv_scale.w);

        if has(FLAG_VORTEX1) {
            let v = material.vortex_pos1_noise_scale.xyz - world;
            out.vortex1 = -vec3<f32>(
                dot(v, world_tangent_s),
                dot(v, world_tangent_t),
                dot(v, world_normal),
            );
        }
        if has(FLAG_VORTEX2) {
            let v = material.vortex_pos2_normal_uv_scale.xyz - world;
            out.vortex2 = -vec3<f32>(
                dot(v, world_tangent_s),
                dot(v, world_tangent_t),
                dot(v, world_normal),
            );
        }
    }
    return out;
}

// `ComputeCameraFade` (`common_ps_fxc.h:920`), the PC branch: fades an
// additive surface out over the forty units in front of the near plane, so
// walking through a fizzler does not fill the screen with it.
fn camera_fade(proj_pos: vec4<f32>) -> f32 {
    return smoothstep(0.0, 1.0, saturate(proj_pos.z * 0.025));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var out = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    var base = vec4<f32>(0.0, 0.0, 0.0, 1.0);

    var eye_dir = vec3<f32>(in.normal_eye_x.w, in.tangent_eye_y.w, in.aligned_view_eye_z.w);
    var normal = in.normal_eye_x.xyz;
    var tangent = in.tangent_eye_y.xyz;

    var backface_ratio = 0.0;

    // The samples whose coordinates are known up front are taken up front,
    // and the flags choose what to do with them. The flow path's two base
    // samples depend on the flow texel and are taken inside its branch, which
    // `textureSample` allows because `material.flags` is uniform.
    let bounds = textureSample(flow_bounds, flow_bounds_sampler, in.uv0_uv1.xy);
    let flow_texel = textureSample(
        flow_map,
        flow_map_sampler,
        in.flow_uv_uv2.xy * material.flow_params1.x,
    );
    let noise = textureSample(flow_noise, flow_noise_sampler, in.flow_uv_uv2.zw).g;
    let plain_base = textureSample(base_texture, base_sampler, in.uv0_uv1.xy);
    let detail1 = textureSample(detail1_texture, detail1_sampler, in.uv0_uv1.zw);
    let detail2_texel = textureSample(detail2_texture, detail2_sampler, in.flow_uv_uv2.zw);

    let cheap = has(FLAG_FLOW_CHEAP);

    if has(FLAG_ACTIVE) {
        if has(FLAG_FLOWMAP) {
            var flow = vec2<f32>(0.0);
            if !cheap {
                flow = flow_texel.rg * 2.0 - 1.0;
                flow = flow * bounds.r; // slow flow
            }
            var vortex_intensity = 0.0;
            let vortex_size = material.vortex_params.w;
            if has(FLAG_VORTEX1) {
                let intensity = saturate(vortex_size / length(in.vortex1) - 0.5);
                if !cheap {
                    flow = mix(flow, normalize(in.vortex1.xy), intensity * 0.5);
                }
                vortex_intensity = vortex_intensity + intensity;
            }
            if has(FLAG_VORTEX2) {
                let intensity = saturate(vortex_size / length(in.vortex2) - 0.5);
                if !cheap {
                    flow = mix(flow, normalize(in.vortex2.xy), intensity * 0.5);
                }
                vortex_intensity = vortex_intensity + intensity;
            }

            // Every interval has a unique offset so the same texels do not
            // repeat continuously.
            let interval = material.flow_params2.x;
            let in_intervals = frame.time.x / (interval * 2.0) + noise;
            let scroll1 = fract(in_intervals) - 0.5;
            let scroll2 = fract(in_intervals + 0.5) - 0.5; // half an interval off

            var offset1 = 0.0;
            var offset2 = 0.5;
            if !cheap {
                offset1 = floor(in_intervals) * 0.311;
                offset2 = floor(in_intervals + 0.5) * 0.311 + 0.5;
            }

            var weight1 = abs(2.0 * fract(in_intervals + 0.5) - 1.0);
            var weight2 = abs(2.0 * fract(in_intervals) - 1.0);
            if !cheap {
                weight1 = pow(weight1, material.flow_params2.w);
                weight2 = pow(weight2, material.flow_params2.w);
            } else {
                weight1 = weight1 * weight1;
                weight2 = weight2 * weight2;
            }

            var scroll_distance = material.flow_params2.y;
            var uv0 = in.uv0_uv1.zw + offset1;
            var uv1 = in.uv0_uv1.zw + offset2;
            if !cheap {
                scroll_distance = scroll_distance * (1.0 + vortex_intensity);
                uv0 = uv0 + scroll1 * (scroll_distance * flow);
                uv1 = uv1 + scroll2 * (scroll_distance * flow);
            }

            base = textureSample(base_texture, base_sampler, uv0) * weight1;
            base = base + textureSample(base_texture, base_sampler, uv1) * weight2;

            let power_up = material.power.x;
            if has(FLAG_POWERUP) {
                let reveal = (noise + (1.0 - bounds.g)) * 0.5;
                let stage2 = saturate(power_up * 3.0);
                let range1 = smoothstep(0.02, 0.0, abs(reveal - power_up));
                let range2 = smoothstep(0.02, 0.0, reveal - power_up);
                var ag = vec2<f32>(base.a, base.g);
                ag = ag + range1 * power_up * (1.0 - power_up);
                ag = ag * (stage2 * range2);
                ag = ag + bounds.g * stage2;
                base = vec4<f32>(base.r, ag.y, base.b, ag.x);
            } else {
                base = vec4<f32>(base.r, base.g + bounds.g, base.b, base.a + bounds.g);
            }

            let field = base.a * material.flow_color.rgb;
            var rgb = field;
            if has(FLAG_VORTEX1) || has(FLAG_VORTEX2) {
                let vortex = base.g * material.vortex_params.rgb;
                rgb = mix(field, vortex, vortex_intensity);
            }
            rgb = rgb * (bounds.b * material.power.y);
            base = vec4<f32>(rgb, base.a);
        } else {
            base = plain_base;
        }

        let tangent_opacity = has(FLAG_TANGENT_T_OPACITY) || has(FLAG_TANGENT_S_OPACITY);
        if tangent_opacity || has(FLAG_FRESNEL_OPACITY) {
            eye_dir = normalize(eye_dir);
            normal = normalize(normal);
            backface_ratio = dot(eye_dir, normal) * 0.5 + 0.5;
            backface_ratio = backface_ratio * backface_ratio;
        }
        if tangent_opacity {
            tangent = normalize(tangent);
        }

        var alpha = 1.0;
        var backface_alpha = 1.0;
        if has(FLAG_TANGENT_T_OPACITY) {
            let r = material.tangent_t_opacity;
            let facing = abs(dot(tangent, normalize(in.aligned_view_eye_z.xyz)));
            alpha = alpha * mix(r.x, r.y, pow(facing, r.z));
            backface_alpha = mix(1.0, r.w, backface_ratio);
        }
        if has(FLAG_TANGENT_S_OPACITY) {
            let r = material.tangent_s_opacity;
            let facing = abs(dot(tangent, normalize(in.aligned_view_eye_z.xyz)));
            // **The exponent is T's**, `g_vTangentTOpacityRanges.z`, in the
            // shipped shader. Valve's slip, reproduced: no shipped material
            // sets `$tangentsopacityranges`, so no picture depends on it.
            alpha = alpha * mix(r.x, r.y, pow(facing, material.tangent_t_opacity.z));
            backface_alpha = min(backface_alpha, mix(r.w, 1.0, backface_ratio));
        }
        if has(FLAG_FRESNEL_OPACITY) {
            let r = material.fresnel_opacity;
            let facing = abs(dot(normal, eye_dir));
            alpha = alpha * mix(r.x, r.y, pow(facing, r.z));
            backface_alpha = min(backface_alpha, mix(r.w, 1.0, dot(eye_dir, normal) * 0.5 + 0.5));
        }
        alpha = alpha * backface_alpha;

        // `DEPTHBLEND` is `[CONSOLE]` only; the PC compiles it out.

        let detail1_mod = has(FLAG_DETAIL1) && has(FLAG_DETAIL1_BLEND_MODE);
        if !has(FLAG_FLOWMAP) && !tangent_opacity && !has(FLAG_FRESNEL_OPACITY) && !detail1_mod {
            alpha = alpha * base.a;
        }

        let fade = camera_fade(in.proj_pos);

        var detail2 = vec4<f32>(0.0);
        if has(FLAG_DETAIL2) {
            detail2 = detail2_texel;
        }
        var rgb = base.rgb;
        if has(FLAG_DETAIL1) {
            if !has(FLAG_DETAIL1_BLEND_MODE) {
                rgb = rgb * (2.0 * detail1.rgb);
            } else {
                rgb = mix(rgb * detail1.rgb, rgb, base.a);
            }
        }
        if has(FLAG_DETAIL2) {
            if !has(FLAG_DETAIL2_BLEND_MODE) {
                var d2 = detail2.rgb;
                if has(FLAG_DETAIL1) {
                    d2 = d2 * detail1.rgb;
                }
                rgb = rgb + d2;
            } else {
                rgb = rgb * detail2.rgb;
            }
        }

        out = vec4<f32>(rgb, alpha);
        // `POWERUP && !FLOWMAP` is skipped by the `.fxc`'s own `SKIP` rules,
        // so there is no power-up scale here.

        if has(FLAG_ADDITIVE) {
            out = vec4<f32>(out.rgb * ((1.0 + alpha) * fade), 1.0);
        }
        if has(FLAG_VERTEX_COLOR) {
            // "fun with saturation"
            let tint = pow(in.color.rgb, vec3<f32>((2.0 - alpha) * 2.0));
            out = vec4<f32>(out.rgb * tint, out.a);
        }
    }

    // "Limit tonemap scalar to 0.0-1.0 so the colors don't oversaturate, but
    // let it drop down to 0 in case we're fading." The exposure is applied,
    // but only ever to darken.
    let tonemap = saturate(frame.light_scale.x);
    // **All four components**, alpha included, as `FinalOutput(...) * scalar`
    // multiplies a `float4`. With `$additive` that makes the blend factor
    // `$outputintensity` — 2.3 on a fizzler — which an 8-bit target clamps to
    // 1 before blending, as D3D9's did.
    return final_output(out, 0.0, PIXEL_FOG_TYPE_NONE, TONEMAP_SCALE_NONE)
        * tonemap * material.flow_params1.w;
}
