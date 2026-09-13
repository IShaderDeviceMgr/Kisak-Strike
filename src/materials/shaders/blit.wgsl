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
@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(source, source_sampler, input.uv);
}
