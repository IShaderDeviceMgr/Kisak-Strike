// The shared shader prelude: the constant ABI, and the helpers every shader
// calls. Prepended to every shader body by `ShaderKind::wgsl`.
//
// Replaces `materialsystem/stdshaders/common_fxc.h`, `common_vs_fxc.h` and
// `common_ps_fxc.h` — 2,569 lines of the 5,181 that `portdocs/MATERIALSYSTEM.md`
// §7.5 inventories. What is here is what a shader in the current target set
// actually calls; the rest arrives with the shader that needs it:
//
//   skinning + morph (`SkinPosition`, `ApplyMorph`)  -> with studiorender
//   flashlight + shadow filtering                    -> with the flashlight
//   parallax, lightwarp, phong, rim lighting         -> with the Phong shader
//
// Two things that could live here and deliberately do not: the bumped-lightmap
// basis is here (`BUMP_BASIS`) but its *sampling* is `LightmappedGeneric`'s,
// and the ambient cube and local lights are `VertexLitGeneric`'s outright.
// Anything that reads a bind group belongs to the shader that declares it,
// and group 3 is declared per shader -- see `shader::LightingBinding`.
//
// Two conventions are set here and inherited by everything later, and both
// produce a plausible-looking wrong picture rather than an error when broken:
//
//   1. MATRICES ARE COLUMN-MAJOR AND MULTIPLY ON THE LEFT: `m * v`. Valve's
//      HLSL is the opposite on both counts (`mul( float4(pos,1), cViewProj )`
//      against a row-major `VMatrix`); the transpose happens once, on the CPU,
//      in `uniforms::from_row_major`.
//   2. SHADERS WRITE LINEAR COLOUR. The swap chain is an sRGB format, so the
//      hardware encodes on write. Applying a curve here as well double-encodes.

// ---------------------------------------------------------------------------
// Group 0: constants that change once a frame
// ---------------------------------------------------------------------------
// Mirrors `uniforms::FrameUniforms`. Field order and padding are the ABI.

struct FrameUniforms {
    // cViewProj, VS c8..c11
    view_proj: mat4x4<f32>,
    // cEyePos_WaterHeightW, VS c2
    eye_pos_water_height: vec4<f32>,
    // cFogParams, VS c16: (1 - end/range, water z, max density, 1/range)
    fog_params: vec4<f32>,
    // g_LinearFogColor, PS c29
    fog_color: vec4<f32>,
    // cLightScale, PS c30: (linear, lightmap, envmap, gamma)
    light_scale: vec4<f32>,
    // cScreenSize, PS c32: (w, h, 1/w, 1/h)
    screen_size: vec4<f32>,
}

@group(0) @binding(0) var<uniform> frame: FrameUniforms;

// ---------------------------------------------------------------------------
// Group 2: constants that change once a draw
// ---------------------------------------------------------------------------
// Mirrors `uniforms::DrawUniforms`. Group 1 is the material's, and its layout
// belongs to whichever shader is being compiled.

struct DrawUniforms {
    model: mat4x4<f32>,
    // cModulationColor, VS c47: $color * $color2 in rgb, $alpha in w.
    modulation: vec4<f32>,
}

@group(2) @binding(0) var<uniform> draw: DrawUniforms;

// ---------------------------------------------------------------------------
// Vertex input
// ---------------------------------------------------------------------------
// One layout for every stage-3 shader; see `pipeline::Vertex`, which declares
// the matching Rust struct and the attribute locations.

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) texcoord: vec2<f32>,
    @location(2) color: vec4<f32>,
}

// `mesh::WorldVertex` / `VertexLayout::World`: what `BuildMSurfaceVertexArrays`
// writes for a brush surface. `lightmap_offset` is one float rather than
// Valve's float2 because the second component is unconditionally zero
// (`matsys_interface.cpp:1498`).

struct WorldVertexInput {
    @location(0) position: vec3<f32>,
    @location(1) texcoord: vec2<f32>,
    @location(2) lightmap_texcoord: vec2<f32>,
    @location(3) lightmap_offset: f32,
    @location(4) color: vec4<f32>,
}

// ---------------------------------------------------------------------------
// Lightmaps
// ---------------------------------------------------------------------------

// The radiosity normal-mapping basis: three unit vectors in tangent space,
// each 54.7 degrees off the surface normal. `bumpBasis` (`common_fxc.h:59`),
// transcribed to the digit — the shipped lightmaps were baked against exactly
// these, so a "tidier" spelling of 1/sqrt(3) would shift every bumped surface.
const OO_SQRT_3: f32 = 0.57735025882720947;
const BUMP_BASIS = array<vec3<f32>, 3>(
    vec3<f32>(0.81649661064147949, 0.0, OO_SQRT_3),
    vec3<f32>(-0.40824833512306213, 0.70710676908493042, OO_SQRT_3),
    vec3<f32>(-0.40824821591377258, -0.7071068286895752, OO_SQRT_3),
);

// ---------------------------------------------------------------------------
// Fog and tone mapping
// ---------------------------------------------------------------------------
// `common_ps_fxc.h`'s `FinalOutput` and the two functions it calls. Valve's
// versions take their mode as a compile-time `const int` from a shader combo;
// here they are ordinary arguments, always passed a literal, and the branch
// folds away at pipeline compilation. That is §7.3's bucket 2 in miniature.

const PIXEL_FOG_TYPE_NONE: i32 = -1;
const PIXEL_FOG_TYPE_RANGE: i32 = 0;

const TONEMAP_SCALE_NONE: i32 = 0;
const TONEMAP_SCALE_LINEAR: i32 = 1;
const TONEMAP_SCALE_GAMMA: i32 = 2;

// `CalcRangeFogFactorNonFixedFunction` (`common_fxc.h:542`). Distance from the
// eye, not depth — which is why the eye position is a per-frame constant and
// not something the vertex shader could work out on its own.
fn calc_range_fog_factor(world_pos: vec3<f32>) -> f32 {
    let distance = distance(frame.eye_pos_water_height.xyz, world_pos);
    let max_density = frame.fog_params.z;
    let end_over_range = frame.fog_params.x;
    let one_over_range = frame.fog_params.w;
    return min(max_density, saturate(end_over_range + distance * one_over_range));
}

// `CalcPixelFogFactor` (`common_ps_fxc.h:273`), minus the height-fog branch,
// which belongs to the water shaders.
fn calc_pixel_fog_factor(fog_type: i32, world_pos: vec3<f32>) -> f32 {
    if fog_type == PIXEL_FOG_TYPE_RANGE {
        return calc_range_fog_factor(world_pos);
    }
    return 0.0;
}

// `BlendPixelFog` (`common_ps_fxc.h:314`).
//
// The factor is *squared* before the lerp. Valve's comment: "squaring the
// factor will get the middle range mixing closer to hardware fog" — it is
// matching the look of D3D9's fixed-function fog, and dropping it would shift
// every foggy scene in the game.
fn blend_pixel_fog(color: vec3<f32>, factor: f32, fog_type: i32) -> vec3<f32> {
    if fog_type == PIXEL_FOG_TYPE_RANGE {
        return mix(color, frame.fog_color.rgb, factor * factor);
    }
    return color;
}

// `FinalOutput` (`common_ps_fxc.h:370`). Every shader's last line.
//
// Not ported from it: `bWriteDepthToDestAlpha`, which packs depth into the
// frame's alpha for the underwater pass and needs a render-target stack to be
// worth anything; and the X360 `LinearToGamma` branch, which is a console.
fn final_output(color: vec4<f32>, fog_factor: f32, fog_type: i32, tonemap: i32) -> vec4<f32> {
    var rgb = color.rgb;
    if tonemap == TONEMAP_SCALE_LINEAR {
        rgb = rgb * frame.light_scale.x;
    } else if tonemap == TONEMAP_SCALE_GAMMA {
        rgb = rgb * frame.light_scale.w;
    }
    rgb = blend_pixel_fog(rgb, fog_factor, fog_type);
    return vec4<f32>(rgb, color.a);
}

// ---------------------------------------------------------------------------
// Transforms
// ---------------------------------------------------------------------------

fn world_position(local: vec3<f32>) -> vec3<f32> {
    return (draw.model * vec4<f32>(local, 1.0)).xyz;
}

fn clip_position(world: vec3<f32>) -> vec4<f32> {
    return frame.view_proj * vec4<f32>(world, 1.0);
}

// A `$basetexturetransform`-style 2x4 texture coordinate transform.
//
// `vertexlit_and_unlit_generic_vs20.fxc:498` does exactly this:
//
//     o.baseTexCoord.x = dot( v.vTexCoord0, cBaseTexCoordTransform[0] );
//
// with `vTexCoord0` a float4 that D3D9 expanded from the vertex's two
// components to (u, v, 0, 1) — which is where the translation in `.w` comes
// from. The expansion is explicit here because WGSL does not do it.
//
// (`unlitgeneric_vs20.fxc:121` writes the same transform as
// `mul( vTexCoord0, (float2x4) cBaseTextureTransform )`, which drops the
// translation entirely. That file is the DX8-era standalone shader, still
// referenced by three debug shaders; this is the form the shader `UnlitGeneric`
// actually runs.)
fn transform_texcoord(uv: vec2<f32>, row0: vec4<f32>, row1: vec4<f32>) -> vec2<f32> {
    let expanded = vec4<f32>(uv, 0.0, 1.0);
    return vec2<f32>(dot(expanded, row0), dot(expanded, row1));
}

// `mesh::ModelVertex` / `VertexLayout::Model`: what a `.vvd`'s vertex and
// tangent arrays hold, minus the bone weights that skinning will add.
//
// `color` is COLOR1, `vStaticLight` — the light `vrad` baked per vertex, in
// gamma space and pre-multiplied by 1/2. It is *not* a vertex colour: the
// shadow phase picks `VERTEX_COLOR` or `VERTEX_COLOR_STREAM_1` and never both,
// and for this shader the choice is always the second
// (`vertexlitgeneric_dx9_helper.cpp:594`).

struct ModelVertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) texcoord: vec2<f32>,
    // xyz = tangent S, w = the sign of the binormal.
    @location(3) tangent: vec4<f32>,
    @location(4) color: vec4<f32>,
}

// ---------------------------------------------------------------------------
// Colour space and shading maths
// ---------------------------------------------------------------------------
// `common_fxc.h`'s helpers, ported as the shaders that call them arrive.

// `cOverbright` (`common_vs_fxc.h:63`). Source stores baked lighting divided by
// two so that a light brighter than white survives an 8-bit channel; every
// reader multiplies it back before using it.
const OVERBRIGHT: f32 = 2.0;

// `GammaToLinear` (`common_fxc.h:266`). Note this is a plain 2.2 power, *not*
// the sRGB piecewise curve `SrgbGammaToLinear` next to it: Valve uses the
// simple one for vertex-stream colours and the piecewise one for texture reads,
// and the two disagree by up to 5% in the darks. Which one a value wants is
// decided by which one it was authored against, so this is not a place to
// "improve" on the reference.
fn gamma_to_linear(gamma: vec3<f32>) -> vec3<f32> {
    return pow(max(gamma, vec3<f32>(0.0)), vec3<f32>(2.2));
}

// `Luminance` (NTSC weights, as every `.fxc` in the tree spells it inline).
fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2125, 0.7154, 0.0721));
}

// `Vec3TangentToWorld` (`common_fxc.h:520`). Deliberately not normalized, as
// the name in the original says: callers that need a unit vector say so.
fn vec3_tangent_to_world(
    tangent_vector: vec3<f32>,
    world_normal: vec3<f32>,
    world_tangent: vec3<f32>,
    world_binormal: vec3<f32>,
) -> vec3<f32> {
    return tangent_vector.x * world_tangent
        + tangent_vector.y * world_binormal
        + tangent_vector.z * world_normal;
}

// `CalcReflectionVectorUnnormalized` (`common_fxc.h:127`).
//
// `2 * (N·V) * N - (N·N) * V`, which is the ordinary reflection formula scaled
// through by `N·N`. The scale is deliberate and free: the result only ever
// indexes a cubemap, and a cubemap lookup does not care about magnitude — so
// this saves the normalize that the textbook form would need.
fn calc_reflection_vector_unnormalized(normal: vec3<f32>, eye_vector: vec3<f32>) -> vec3<f32> {
    return 2.0 * dot(normal, eye_vector) * normal - dot(normal, normal) * eye_vector;
}

// `Fresnel` (`common_vertexlitgeneric_dx9.h:180`): `(1 - N·V)²`.
fn fresnel(normal: vec3<f32>, eye_dir: vec3<f32>) -> f32 {
    let f = 1.0 - saturate(dot(normal, eye_dir));
    return f * f;
}

// ---------------------------------------------------------------------------
// Detail texturing
// ---------------------------------------------------------------------------
// `TextureCombine` / `TextureCombinePostLighting` (`common_ps_fxc.h:770,818`).
// Valve's `combine_mode` is a compile-time `int` from a static combo and a
// chain of thirteen `if`s that the compiler folds to one; here it is a uniform
// and the chain is a `switch`, which is §7.3's bucket 2 for an axis with more
// than two values.
//
// `shader::detail_blend`'s constants, which are what a `.vmt` writes in
// `$detailblendmode`.

const TCOMBINE_MOD2X: i32 = 0;
const TCOMBINE_RGB_ADDITIVE: i32 = 1;
const TCOMBINE_DETAIL_OVER_BASE: i32 = 2;
const TCOMBINE_FADE: i32 = 3;
const TCOMBINE_BASE_OVER_DETAIL: i32 = 4;
const TCOMBINE_RGB_ADDITIVE_SELFILLUM: i32 = 5;
const TCOMBINE_RGB_ADDITIVE_SELFILLUM_THRESHOLD_FADE: i32 = 6;
const TCOMBINE_MOD2X_SELECT_TWO_PATTERNS: i32 = 7;
const TCOMBINE_MULTIPLY: i32 = 8;
const TCOMBINE_MASK_BASE_BY_DETAIL_ALPHA: i32 = 9;
const TCOMBINE_SSBUMP_BUMP: i32 = 10;
const TCOMBINE_SSBUMP_NOBUMP: i32 = 11;
const TCOMBINE_NONE: i32 = 12;

fn texture_combine(
    base: vec4<f32>,
    detail: vec4<f32>,
    mode: i32,
    blend: f32,
) -> vec4<f32> {
    var result = base;
    switch mode {
        case 7: {
            // Base alpha selects between the detail's red and alpha channels,
            // which is how one texture holds two mod2x patterns.
            let dc = vec3<f32>(mix(detail.r, detail.a, base.a));
            result = vec4<f32>(base.rgb * mix(vec3<f32>(1.0), 2.0 * dc, blend), base.a);
        }
        case 0: {
            result = vec4<f32>(base.rgb * mix(vec3<f32>(1.0), 2.0 * detail.rgb, blend), base.a);
        }
        case 1: {
            result = vec4<f32>(base.rgb + blend * detail.rgb, base.a);
        }
        case 2: {
            result = vec4<f32>(mix(base.rgb, detail.rgb, blend * detail.a), base.a);
        }
        case 3: {
            result = mix(base, detail, blend);
        }
        case 4: {
            result = vec4<f32>(mix(base.rgb, detail.rgb, blend * (1.0 - base.a)), detail.a);
        }
        case 8: {
            result = mix(base, base * detail, blend);
        }
        case 9: {
            result = vec4<f32>(base.rgb, mix(base.a, base.a * detail.a, blend));
        }
        case 11: {
            // `dot( detail.rgb, 2/3 )` in the original, which is a dot against
            // a scalar broadcast to a float3 — the sum of the channels times
            // two thirds.
            result = vec4<f32>(base.rgb * dot(detail.rgb, vec3<f32>(2.0 / 3.0)), base.a);
        }
        // 5 and 6 are applied after lighting, 10 needs a self-shadowing bump
        // map, and 12 is "no detail texture at all".
        default: {}
    }
    return result;
}

fn texture_combine_post_lighting(
    lit_base: vec3<f32>,
    detail: vec4<f32>,
    mode: i32,
    blend: f32,
) -> vec3<f32> {
    if mode == TCOMBINE_RGB_ADDITIVE_SELFILLUM {
        return lit_base + blend * detail.rgb;
    }
    if mode == TCOMBINE_RGB_ADDITIVE_SELFILLUM_THRESHOLD_FADE {
        // "fade in an unusual way - instead of fading out color, remap an
        // increasing band of it from 0..1".
        if blend > 0.5 {
            return lit_base
                + min(vec3<f32>(1.0), (1.0 / blend) * max(vec3<f32>(0.0), detail.rgb - (1.0 - blend)));
        }
        return lit_base + 2.0 * blend * 2.0 * max(vec3<f32>(0.0), detail.rgb - 0.5);
    }
    return lit_base;
}
