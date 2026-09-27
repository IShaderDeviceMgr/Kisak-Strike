// One texture onto another, full screen, nothing else.
//
// What is left of `stdshaders/floattoscreen.cpp` and the final copy at the end
// of `Engine_Post_dx9.cpp` once the bloom, the colour correction, the software
// AA and the vignette are taken out: the scene is rendered somewhere else and
// this is what puts it on the back buffer. It exists because a texture cannot
// be sampled while it is the thing being drawn into, and the tone mapper has to
// read the frame it is exposing.
//
// Deliberately *not* part of the material system: no `.vmt`, no `ShaderKind`,
// no constant ABI. `materials/ui.rs` has the same shape and the same reason.

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

// `post::FadeUniforms`: the screen fade, `engine_post`'s `FADE_TYPE`.
struct FadeUniforms {
    // The fade colour, and in `a` how far towards it.
    color: vec4<f32>,
    // 0: none. 1: blend to the colour. 2: modulate by it.
    kind: u32,
    // Whether the destination encodes on write, so the lerp has to be put in
    // gamma space by hand.
    srgb: u32,
    pad0: u32,
    pad1: u32,
}
@group(0) @binding(2) var<uniform> fade: FadeUniforms;

// The sRGB transfer function both ways, exactly: `engine_post` lerped the
// *encoded* bytes, and a wrong curve here would change where half way is.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let low = c * 12.92;
    let high = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// One oversized triangle rather than two triangles, so there is no seam along
// the diagonal and no vertex buffer at all.
//
//   index 0 -> uv (0, 0) -> clip (-1,  1)   top left
//   index 1 -> uv (2, 0) -> clip ( 3,  1)   off to the right
//   index 2 -> uv (0, 2) -> clip (-1, -3)   off the bottom
//
// `uv` runs top-left to bottom-right, which is both `wgpu`'s texture origin and
// the direction clip space's `y` runs *backwards* in — hence the sign flip on
// `y` and not on `x`.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return VertexOutput(
        vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0),
        uv,
    );
}

// The source and the destination are the same size and the same format, so this
// is a copy: an sRGB source decodes on the fetch, an sRGB destination encodes on
// the write, and the two are inverses. The round trip is not bit-exact — it
// goes through a float and back — so a channel can land one of 255 steps away
// from where it started. That is the price of having the frame readable at all,
// and it is the same price `UpdateScreenEffectTexture` and the `Engine_Post`
// pass charged in the shipped game.
//
// The fade, when there is one, is the last thing `engine_post` did before
// writing (`engine_post_ps2x.fxc:437`):
//
//     FADE_TYPE 1: outColor.rgb = lerp( outColor.rgb, g_vViewFadeColor.rgb, g_vViewFadeColor.aaa );
//     FADE_TYPE 2: outColor.rgb = lerp( outColor.rgb, g_vViewFadeColor.rgb * outColor.rgb, g_vViewFadeColor.aaa );
@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let scene = textureSample(source, source_sampler, input.uv);
    if fade.kind == 0u {
        return scene;
    }
    var rgb = scene.rgb;
    if fade.srgb != 0u {
        rgb = linear_to_srgb(saturate(rgb));
    }
    let amount = vec3<f32>(fade.color.a);
    if fade.kind == 1u {
        rgb = mix(rgb, fade.color.rgb, amount);
    } else {
        rgb = mix(rgb, fade.color.rgb * rgb, amount);
    }
    if fade.srgb != 0u {
        rgb = srgb_to_linear(rgb);
    }
    return vec4<f32>(rgb, scene.a);
}
