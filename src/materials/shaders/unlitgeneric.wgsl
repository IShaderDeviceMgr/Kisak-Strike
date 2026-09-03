// UnlitGeneric: a base texture, a texture transform, and colour modulation.
//
// Translated from `materialsystem/stdshaders/vertexlit_and_unlit_generic_vs20.fxc`
// and `..._ps2x.fxc` with lighting off — which is what `unlitgeneric_dx9.cpp`
// asks for when it calls the shared helper with `bVertexLitGeneric = false` —
// taking the `[ps2b] [ps30]` branch everywhere, per §7.7 step 4.
//
// The combo bucketing this shader's variants were sorted into is written down
// in `src/materials/shader.rs`; the short version is that nothing textual
// survives, so this is one module rather than a family of them.
//
// Prepended by `shaders/prelude.wgsl`, which declares groups 0 and 2,
// `VertexInput`, and the fog and output helpers.

// ---------------------------------------------------------------------------
// Group 1: the material
// ---------------------------------------------------------------------------
// Mirrors `shader::UnlitUniforms`, field for field and pad for pad.

struct UnlitUniforms {
    // $basetexturetransform, as two rows dotted against (u, v, 0, 1).
    base_texture_transform: array<vec4<f32>, 2>,
    // $alphatestreference, or the fixed-function default of 0.7.
    alpha_test_reference: f32,
    // UnlitFlags, below.
    flags: u32,
    pad0: u32,
    pad1: u32,
}

// `shader::UnlitFlags`. Bucket 2 of the combo split: what used to be a static
// shader variant, or fixed-function state D3D9 had and WebGPU does not.
const FLAG_VERTEX_COLOR: u32 = 1u;   // $vertexcolor
const FLAG_ALPHA_TEST: u32 = 2u;     // $alphatest
const FLAG_NO_FOG: u32 = 4u;         // $nofog

@group(1) @binding(0) var<uniform> material: UnlitUniforms;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var base_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) texcoord: vec2<f32>,
    @location(1) color: vec4<f32>,
    // Needed by pixel fog, which measures distance from the eye rather than
    // reading depth. `worldPos_projPosZ` in the original.
    @location(2) world_position: vec3<f32>,
}

@vertex
fn vs_main(vertex: VertexInput) -> VertexOutput {
    let world = world_position(vertex.position);

    var out: VertexOutput;
    out.clip_position = clip_position(world);
    out.world_position = world;
    out.texcoord = transform_texcoord(
        vertex.texcoord,
        material.base_texture_transform[0],
        material.base_texture_transform[1],
    );

    // `o.vColor = cModulationColor;` then `#if VERTEXCOLOR o.vColor *= v.vColor;`
    out.color = draw.modulation;
    if (material.flags & FLAG_VERTEX_COLOR) != 0u {
        out.color = out.color * vertex.color;
    }
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // The whole of the original's pixel shader body:
    //   float4 result = i.vColor0 * tex2D( TextureSampler, i.vTexCoord0 );
    let result = in.color * textureSample(base_texture, base_sampler, in.texcoord);

    // D3D9 tested alpha in fixed-function state, after the shader
    // (`AlphaFunc( SHADER_ALPHAFUNC_GEQUAL, ref )`), so the comparison is
    // against the *modulated* alpha and not against the texture's.
    if (material.flags & FLAG_ALPHA_TEST) != 0u && result.a < material.alpha_test_reference {
        discard;
    }

    var fog_type = PIXEL_FOG_TYPE_RANGE;
    if (material.flags & FLAG_NO_FOG) != 0u {
        fog_type = PIXEL_FOG_TYPE_NONE;
    }
    let fog_factor = calc_pixel_fog_factor(fog_type, in.world_position);

    // `TONEMAP_SCALE_NONE`: an unlit surface is already in output range, and
    // there is no exposure to apply until HDR is decided (§10).
    return final_output(result, fog_factor, fog_type, TONEMAP_SCALE_NONE);
}
