// Draws one texture over the whole frame. Stage 2's verification path only --
// see src/materials/blit.rs.
//
// The surface is sRGB (Renderer::new picks an sRGB format when the platform
// offers one), so the hardware encodes on write and this shader must not apply
// a curve of its own. Sampling an *_Srgb texture decodes to linear, writing
// linear re-encodes, and the round trip is the identity.

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One oversized triangle rather than two triangles of a quad: it covers the
// viewport with three vertices and no vertex buffer, and avoids the diagonal
// seam where a quad's two triangles meet.
//
//   index 0 -> clip (-1,-1)   index 1 -> clip (3,-1)   index 2 -> clip (-1,3)
//
// Everything past the viewport edge is clipped away.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));

    var out: VertexOut;
    out.position = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    // Clip space puts -1 at the bottom; texture space puts v = 0 at the top.
    out.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(source, source_sampler, in.uv);
}
