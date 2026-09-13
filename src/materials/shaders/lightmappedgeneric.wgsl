// LightmappedGeneric: a base texture multiplied by a baked lightmap.
//
// Translated from `materialsystem/stdshaders/lightmappedgeneric_vs20.fxc` and
// `lightmappedgeneric_ps2_3_x.h`, taking the `[ps30]` / non-`_X360` branch
// everywhere, per `portdocs/MATERIALSYSTEM.md` §7.7 step 4. The combo
// bucketing is written down in `src/materials/shader.rs`.
//
// Prepended by `shaders/prelude.wgsl`, which declares groups 0 and 2,
// `WorldVertexInput`, `BUMP_BASIS`, and the fog and output helpers.
//
// ---------------------------------------------------------------------------
// What is here and what is not
// ---------------------------------------------------------------------------
// Here: the base texture and its transform, the flat lightmap, the bumped
// (radiosity normal mapped) lightmap, the `$ssbump` variant of that, the
// two-layer `$basetexture2`/`$bumpmap2` blend and its
// `$blendmodulatetexture`, `$vertexcolor`, colour modulation, alpha testing
// and fog.
//
// **This module is also `WorldVertexTransition`**, which is the same shader
// under the name content uses when it sets `$basetexture2` — see
// `ShaderKind::WorldVertexTransition`. It is what Portal 2's terrain wears:
// 937 of its 1,181 displacement faces.
//
// Not here, each deferred with the feature it belongs to and each declared in
// the original's parameter table: `$detail`, `$envmap`/`$envmapmask`,
// `$seamless_scale` (the two features that would make this shader need a
// world-space normal, and therefore a second vertex layout — 553 displacement
// faces in the `sp_a3_*` maps set the latter), `$selfillum`, phong, the
// `FANCY_BLENDING >= 2` layer border and edge terms, drop shadows, the
// flashlight, cascaded shadow maps, and Portal 2's paint layer.

// ---------------------------------------------------------------------------
// Group 1: the material
// ---------------------------------------------------------------------------
// Mirrors `shader::LightmappedUniforms`, field for field and pad for pad.

struct LightmappedUniforms {
    // $basetexturetransform, as two rows dotted against (u, v, 0, 1).
    base_texture_transform: array<vec4<f32>, 2>,
    // $bumptransform, applied to the same base coordinate.
    bump_transform: array<vec4<f32>, 2>,
    // $basetexturetransform2 and $blendmodulatetransform, likewise.
    base_texture2_transform: array<vec4<f32>, 2>,
    blend_modulate_transform: array<vec4<f32>, 2>,
    // $alphatestreference, or the fixed-function default of 0.7.
    alpha_test_reference: f32,
    // LightmappedFlags, below.
    flags: u32,
    pad0: u32,
    pad1: u32,
}

// `shader::LightmappedFlags`.
const FLAG_VERTEX_COLOR: u32 = 1u;     // $vertexcolor
const FLAG_ALPHA_TEST: u32 = 2u;       // $alphatest
const FLAG_NO_FOG: u32 = 4u;           // $nofog
const FLAG_BUMPED_LIGHTMAP: u32 = 8u;  // $bumpmap, without $nodiffusebumplighting
const FLAG_BASE_TEXTURE2: u32 = 16u;   // $basetexture2 -> BASETEXTURE2
const FLAG_BLEND_MODULATE: u32 = 32u;  // $blendmodulatetexture -> FANCY_BLENDING 1
const FLAG_BUMP_MAP2: u32 = 64u;       // $bumpmap2 -> BUMPMAP2
const FLAG_SSBUMP: u32 = 128u;         // $ssbump -> BUMPMAP == 2

@group(1) @binding(0) var<uniform> material: LightmappedUniforms;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var base_sampler: sampler;
@group(1) @binding(3) var bump_texture: texture_2d<f32>;
@group(1) @binding(4) var bump_sampler: sampler;
@group(1) @binding(13) var base2_texture: texture_2d<f32>;
@group(1) @binding(14) var base2_sampler: sampler;
@group(1) @binding(15) var bump2_texture: texture_2d<f32>;
@group(1) @binding(16) var bump2_sampler: sampler;
@group(1) @binding(17) var blend_modulate_texture: texture_2d<f32>;
@group(1) @binding(18) var blend_modulate_sampler: sampler;

// ---------------------------------------------------------------------------
// Group 3: the lightmap page
// ---------------------------------------------------------------------------
// Not part of the material: one material's surfaces are spread over as many
// atlas pages as the packer needed, so the page is per *batch*. That was
// `BindLightmapPage` in the original and is `Pass::bind_lightmap_page` here.
//
// The page holds linear radiance in `Rgba16Float` and is sampled without an
// sRGB decode, which is what `bHDR ? TEXTURE_BINDFLAGS_NONE :
// TEXTURE_BINDFLAGS_SRGBREAD` (`lightmappedgeneric_dx9_helper.cpp:583`) asks
// for on the HDR path Portal 2's maps ship.

@group(3) @binding(0) var lightmap_texture: texture_2d<f32>;
@group(3) @binding(1) var lightmap_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) texcoord: vec2<f32>,
    @location(1) bump_texcoord: vec2<f32>,
    // The three bumped lightmap coordinates, or just `.xy` when unbumped.
    // `lightmapTexCoord1And2` plus `lightmapTexCoord3_bumpTexCoord.xy` in the
    // original, which packs them into two interpolators and swaps the second
    // pair's components to do it. Three plain `vec2`s here: WGSL has no
    // interpolator budget worth fighting for at this size, and the swap
    // ("reversed component order", `lightmappedgeneric_vs20.fxc:240`) is the
    // kind of packing that produces a mirrored lightmap when misread.
    @location(2) lightmap_texcoord1: vec2<f32>,
    @location(3) lightmap_texcoord2: vec2<f32>,
    @location(4) lightmap_texcoord3: vec2<f32>,
    @location(5) color: vec4<f32>,
    // Pixel fog measures distance from the eye rather than reading depth.
    @location(6) world_position: vec3<f32>,
    // `$basetexture2` and `$blendmodulatetexture`'s coordinates, which are the
    // base coordinate under their own transforms.
    @location(7) texcoord2: vec2<f32>,
    @location(8) blend_modulate_texcoord: vec2<f32>,
    // `i.tangentSpaceTranspose0_vertexBlendX.w` — the two-layer blend factor,
    // in its own interpolator here because this port has no tangent basis to
    // hide it in.
    @location(9) blend_factor: f32,
}

@vertex
fn vs_main(vertex: WorldVertexInput) -> VertexOutput {
    let world = world_position(vertex.position);

    var out: VertexOutput;
    out.clip_position = clip_position(world);
    out.world_position = world;
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
    out.texcoord2 = transform_texcoord(
        vertex.texcoord,
        material.base_texture2_transform[0],
        material.base_texture2_transform[1],
    );
    out.blend_modulate_texcoord = transform_texcoord(
        vertex.texcoord,
        material.blend_modulate_transform[0],
        material.blend_modulate_transform[1],
    );

    // `lightmappedgeneric_vs20.fxc:232`. The offset is the width of one
    // lightmap block as a fraction of the page, so the three directional maps
    // are one, two and three blocks to the right of the flat one — which is
    // the order `LightmapAtlas::write` lays them out in.
    //
    // Note that the *bumped* path starts at the block after the flat map and
    // never samples the flat one. That is deliberate in the original and it is
    // why the atlas still writes block 0 for a bumped surface: it is what an
    // unbumped shader reading the same surface would want.
    let offset = vec2<f32>(vertex.lightmap_offset, 0.0);
    if (material.flags & FLAG_BUMPED_LIGHTMAP) != 0u {
        out.lightmap_texcoord1 = vertex.lightmap_texcoord + offset;
        out.lightmap_texcoord2 = out.lightmap_texcoord1 + offset;
        out.lightmap_texcoord3 = out.lightmap_texcoord2 + offset;
    } else {
        out.lightmap_texcoord1 = vertex.lightmap_texcoord;
        out.lightmap_texcoord2 = vertex.lightmap_texcoord;
        out.lightmap_texcoord3 = vertex.lightmap_texcoord;
    }

    // `o.vertexColor = float4( 1, 1, 1, cModulationColor.a )`, or the vertex's
    // own colour with the modulation alpha folded in
    // (`lightmappedgeneric_vs20.fxc:258`). The *rgb* modulation does not
    // travel this way: it reaches the pixel shader as
    // `g_TintValuesTimesLightmapScale`, applied to the lighting rather than to
    // the albedo. See `fs_main`.
    if (material.flags & FLAG_VERTEX_COLOR) != 0u {
        out.color = vec4<f32>(vertex.color.rgb, vertex.color.a * draw.modulation.a);
    } else {
        out.color = vec4<f32>(1.0, 1.0, 1.0, draw.modulation.a);
    }

    // `if ( g_bVertexAlphaTexBlendFactor ) o.tangentSpaceTranspose0_vertexBlendX.w
    // = v.vColor.a` (`lightmappedgeneric_vs20.fxc:288`), which the helper turns
    // on for `hasBaseTexture2 || hasBump2` (`:699`). The blend factor is the
    // raw vertex alpha and is **not** multiplied by the modulation the way the
    // alpha above is, so it travels in its own channel.
    //
    // A world *face* has no colour stream and gets white — alpha 1, the second
    // layer everywhere — which is what Valve's mesh builder leaves there too.
    // A displacement's comes from `CDispVert::alpha`; see `world::disp`.
    out.blend_factor = vertex.color.a;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var albedo = textureSample(base_texture, base_sampler, in.texcoord);

    // ---------------------------------------------------------------------
    // The two-layer blend — `WorldVertexTransition`, and `LightmappedGeneric`
    // with a `$basetexture2`. `lightmappedgeneric_ps2_3_x.h:400-470`.
    // ---------------------------------------------------------------------
    var blend_factor = in.blend_factor;
    if (material.flags & FLAG_BASE_TEXTURE2) != 0u {
        // `FANCY_BLENDING == 1`: the modulation texture's green is the centre
        // of the crossfade and its red the half-width, so `smoothstep` between
        // them turns a linear fade into one that follows the texture — dirt
        // settling into the low parts of the rock rather than a wash over it.
        if (material.flags & FLAG_BLEND_MODULATE) != 0u {
            let texel = textureSample(
                blend_modulate_texture,
                blend_modulate_sampler,
                in.blend_modulate_texcoord,
            );
            let min_b = max(0.0, texel.g - texel.r);
            let max_b = min(1.0, texel.g + texel.r);
            // `smoothstep` with `min_b == max_b` is a divide by zero in the
            // general formula; WGSL returns 0 or 1 at the boundary rather than
            // a NaN, but a fully-saturated modulation texel is common enough
            // (`r == 0`) that it is worth not relying on that.
            blend_factor = select(
                step(min_b, blend_factor),
                smoothstep(min_b, max_b, blend_factor),
                max_b > min_b,
            );
        }
        let albedo2 = textureSample(base2_texture, base2_sampler, in.texcoord2);
        // `baseColor.rgb = lerp( baseColor.rgb, baseColor2.rgb, blendfactor )`
        // and `blendedAlpha = lerp( baseColor.a, baseColor2.a, blendfactor )`.
        albedo = vec4<f32>(
            mix(albedo.rgb, albedo2.rgb, blend_factor),
            mix(albedo.a, albedo2.a, blend_factor),
        );
    }

    var alpha = albedo.a * in.color.a;

    // `g_TintValuesTimesLightmapScale` (PS c12), which
    // `PI_SetModulationPixelShaderDynamicState_LinearScale_ScaleInW( 12,
    // flLScale )` (`lightmappedgeneric_dx9_helper.cpp:842`) fills with the
    // modulation colour times `GetLightMapScaleFactor()`. That factor is 1.0
    // for float HDR lightmaps (`hardwareconfig.cpp:832`), which is what this
    // port's pages are, and `cLightScale.y` carries it so the shader does not
    // have to know which kind it got.
    let tint = draw.modulation.rgb * frame.light_scale.y;

    var diffuse_lighting: vec3<f32>;
    if (material.flags & FLAG_BUMPED_LIGHTMAP) != 0u {
        // The normal is used in *tangent space*, straight out of the map — the
        // basis it is dotted against is the constant one the lightmaps were
        // baked in, so no world-space frame is involved. That is the whole
        // reason this shader needs no tangents and no second vertex layout.
        //
        // **A self-shadowed bump map is not decoded**: `#if BUMPMAP == 1 //
        // not ssbump` guards the `2 * t - 1`
        // (`lightmappedgeneric_ps2_3_x.h:322`). An ssbump texel is three
        // already-positive coefficients, one per basis vector, not a signed
        // direction — running it through the signed decode darkens every
        // surface that wears one and tilts the rest.
        let ssbump = (material.flags & FLAG_SSBUMP) != 0u;
        let texel = textureSample(bump_texture, bump_sampler, in.bump_texcoord).xyz;
        var normal = select(2.0 * texel - 1.0, texel, ssbump);

        // `vNormal.xyz = lerp( vNormal.xyz, vNormal2.xyz, blendfactor )`
        // (`:525`) — the second layer's bump map, under the *same* blend
        // factor the albedo used, and sampled at the first layer's coordinate
        // because `$bumptransform2` is not read here.
        if (material.flags & FLAG_BUMP_MAP2) != 0u {
            let texel2 = textureSample(bump2_texture, bump2_sampler, in.bump_texcoord).xyz;
            let normal2 = select(2.0 * texel2 - 1.0, texel2, ssbump);
            normal = mix(normal, normal2, blend_factor);
        }

        let light1 = textureSample(lightmap_texture, lightmap_sampler, in.lightmap_texcoord1).rgb;
        let light2 = textureSample(lightmap_texture, lightmap_sampler, in.lightmap_texcoord2).rgb;
        let light3 = textureSample(lightmap_texture, lightmap_sampler, in.lightmap_texcoord3).rgb;

        if ssbump {
            // `#if ( BUMPMAP == 2 )` (`:649`): a plain weighted sum, with the
            // coefficients used directly rather than dotted against the basis.
            //
            // The `1/√3` is Valve's, and the comment above it is worth keeping
            // in full because the constant is otherwise unexplainable: vrad's
            // three lightmap weights are barycentric and sum to 1, so a flat
            // normal gives each 0.333; an ssbump texture is authored assuming
            // the *other* decode, so a flat normal there gives each 0.578 and
            // they sum to 1.733. 1/1.733 = 0.57735 puts the two back on the
            // same scale.
            diffuse_lighting =
                (normal.x * light1 + normal.y * light2 + normal.z * light3)
                * 0.57735025882720947
                * tint;
        } else {
            // Radiosity normal mapping (`:664`).
            var dp = vec3<f32>(
                saturate(dot(normal, BUMP_BASIS[0])),
                saturate(dot(normal, BUMP_BASIS[1])),
                saturate(dot(normal, BUMP_BASIS[2])),
            );
            dp = dp * dp;
            let weighted = dp.x * light1 + dp.y * light2 + dp.z * light3;

            // The divide by the weight sum is what keeps a flat normal as
            // bright as an unbumped surface; `dp` is squared above and does not
            // sum to 1. A normal pointing away from all three basis vectors
            // saturates every component to zero, and the original divides
            // anyway — guarded here, because a NaN in a lightmap is a black
            // hole in the wall rather than a crash, and it is the sort of thing
            // that only shows up on one map.
            let sum = dp.x + dp.y + dp.z;
            diffuse_lighting = select(vec3<f32>(0.0), weighted * tint / sum, sum > 0.0);
        }
    } else {
        let light = textureSample(lightmap_texture, lightmap_sampler, in.lightmap_texcoord1).rgb;
        diffuse_lighting = light * tint;
    }

    // `diffuseComponent = albedo.rgb * diffuseLighting`
    // (`lightmappedgeneric_ps2_3_x.h:755`), with `albedo.rgb *=
    // i.vertexColor.rgb` (`:618`) before it.
    let result = albedo.rgb * in.color.rgb * diffuse_lighting;

    // D3D9 tested alpha in fixed-function state, after the shader, so the
    // comparison is against the modulated alpha rather than the texture's.
    if (material.flags & FLAG_ALPHA_TEST) != 0u && alpha < material.alpha_test_reference {
        discard;
    }

    var fog_type = PIXEL_FOG_TYPE_RANGE;
    if (material.flags & FLAG_NO_FOG) != 0u {
        fog_type = PIXEL_FOG_TYPE_NONE;
    }
    let fog_factor = calc_pixel_fog_factor(fog_type, in.world_position);

    // `TONEMAP_SCALE_LINEAR` (`lightmappedgeneric_ps2_3_x.h:982`): a lit
    // surface is in HDR range and `cLightScale.x` is the exposure the tone
    // mapper chose. There is no tone mapper yet, so it is 1 — see
    // `uniforms::FrameUniforms::light_scale`.
    return final_output(vec4<f32>(result, alpha), fog_factor, fog_type, TONEMAP_SCALE_LINEAR);
}
