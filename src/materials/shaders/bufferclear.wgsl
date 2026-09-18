// BufferClearObeyStencil: a clear that a stencil test can mask.
//
// `materialsystem/stdshaders/BufferClearObeyStencil_dx9.cpp` and
// `bufferclearobeystencil_vs20.fxc`, which exist for one reason and this port
// has the same reason: **an API's clear covers the whole attachment.** D3D9's
// `Clear` takes no stencil predicate and `wgpu`'s `LoadOp::Clear` is a property
// of the pass, so "reset the depth buffer inside the portal's opening and
// nowhere else" has to be a draw.
//
// Prepended by `shaders/prelude.wgsl`. Nothing in the prelude is used: this is
// the one shader in the port that reads neither the frame nor the draw block,
// because a full-screen quad in clip space needs no matrices.
//
// ---------------------------------------------------------------------------
// The vertices are already in clip space
// ---------------------------------------------------------------------------
// `o.vProjPos.xyz = v.vPos.xyz; o.vProjPos.w = 1.0f;`
// (`bufferclearobeystencil_vs20.fxc:24`) — the caller supplies normalized
// device coordinates and `z` is the depth value being written.
// `CMatRenderContext::DrawClearBufferQuad` (`cmatrendercontext.cpp:2419`) feeds
// it a quad at ±1.1 rather than ±1.0, *"to fix small borders around the edges
// in full screen with anti-aliasing enabled"*, and `z` at the far value.
//
// So the shape of the clear is whatever the caller draws, the depth written is
// whatever `z` the caller put in the vertices, and the *mask* is the stencil
// test and the scissor rectangle. See `materials::context::Pass::set_stencil`
// and `portdocs/PORTAL_RENDER.md` §6.2.

// ---------------------------------------------------------------------------
// Group 1: the colour, which this port never writes
// ---------------------------------------------------------------------------
// `params[CLEARCOLOR]` decides whether colour writes are enabled at all, and
// the colour itself arrives on the vertex in the original. The only caller
// here clears depth alone (`EnableColorWrites( false )`, which is
// `RenderState::write_color`), so this block exists to satisfy the group-1
// layout every pipeline declares and is never read from a written pixel.

struct ClearUniforms {
    color: vec4<f32>,
}

@group(1) @binding(0) var<uniform> material: ClearUniforms;

@vertex
fn vs_main(vertex: VertexInput) -> @builtin(position) vec4<f32> {
    return vec4<f32>(vertex.position, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return material.color;
}
