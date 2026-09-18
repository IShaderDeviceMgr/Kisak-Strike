// PortalRefract, stage 1: the hole a portal punches in the stencil buffer.
//
// Translated from `materialsystem/stdshaders/portal_refract_vs20.fxc` and
// `portal_refract_ps2x.fxc`, taking the `STAGE == 1` branch of each. The whole
// pixel shader is nine lines (`portal_refract_ps2x.fxc:184-192`); everything
// interesting about this shader is its *state*, which is
// `shader::portal_refract_hole_render_state`.
//
// Prepended by `shaders/prelude.wgsl`, which declares groups 0 and 2,
// `VertexInput` and the fog and output helpers.
//
// ---------------------------------------------------------------------------
// What it is for
// ---------------------------------------------------------------------------
// `portdocs/PORTAL_RENDER.md` §2.2. The recursive view needs a region of the
// screen that means "the other room is visible here", and the stencil buffer
// is where that region lives. This shader is what decides its *shape*: the
// same `step( distFromCentre, openAmount² )` oval the stage-2 overlay draws a
// ring around, turned into an alpha test so that the stencil operation runs
// inside the oval and nowhere else.
//
// It is drawn **twice** per portal per frame with different state, and the two
// draws are what bracket a portal view:
//
//   step 1  stencil Equal(parent) / IncrementClamp, depth test and write on,
//           colour writes on — marks the hole and blacks it out
//   step 4  stencil Equal(child) / DecrementClamp, depth test *off*, write on,
//           colour writes off — unmarks it and puts the wall's depth back
//
// The second draw is why `writez_dx9.cpp` never needed porting: the stencil
// test already names the exact pixels, so the only thing the draw has to
// supply is depth. `portdocs/PORTAL_RENDER.md` §6.3.
//
// ---------------------------------------------------------------------------
// The near-plane cap rides on this shader
// ---------------------------------------------------------------------------
// When the camera is close enough that the portal's quad crosses the near
// plane, a second polygon is drawn with this same material to cover what the
// near plane cut off (`portdocs/PORTAL_RENDER.md` §5). **Every one of its
// vertices carries texture coordinate `(0.5, 0.5)`** — the centre of the
// portal, where `dist_from_center` is 0 and the test below passes for any open
// amount above zero. The cap is unconditionally inside the hole, which is what
// it is for; giving it real coordinates would cut an oval out of the patch and
// leave a hole in the hole.

// ---------------------------------------------------------------------------
// Group 1: the material
// ---------------------------------------------------------------------------
// Byte for byte `shaders/portalrefract.wgsl`'s block, because it is the same
// `shader::PortalRefractUniforms` and the same bind group layout —
// `portal_stencil_hole.vmt` is a `PortalRefract` material like any other. The
// two textures the layout declares are **not** declared here: this stage
// samples neither, and the shipped material only defines them inside a
// `<DX90>` block, which is the pre-shader-model-2 fallback path.

struct PortalRefractUniforms {
    texture_transform: array<vec4<f32>, 2>,
    gradient_dark: vec4<f32>,
    gradient_light: vec4<f32>,
    params: vec4<f32>,
    flags: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(1) @binding(0) var<uniform> material: PortalRefractUniforms;

// ---------------------------------------------------------------------------
// Group 3: this portal's own three numbers
// ---------------------------------------------------------------------------
// `uniforms::PortalOverlay`, shared with stage 2. Only `open_amount` is read
// here, and it is the whole of the hole: a portal that has just activated has
// an open amount of 0, a hole of radius 0 and no view through it at all, which
// is what makes one look like it is burning its way open.

struct PortalOverlay {
    open_amount: f32,
    portal_active: f32,
    time: f32,
    pad0: f32,
}

@group(3) @binding(0) var<uniform> portal: PortalOverlay;

// Declared in both `.fxc` files with a comment saying "Must match!". One
// constant here, and it must also match `shaders/portalrefract.wgsl`'s — the
// hole and the ring drawn around it are measured in the same units.
const PORTAL_OUTER_BORDER: f32 = 0.075;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(vertex: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = clip_position(world_position(vertex.position));

    // `.xy`, so the transform's translation column is dropped — see the same
    // note in `shaders/portalrefract.wgsl`, which this has to agree with or
    // the ring and the hole it rings stop being concentric.
    let base_uv = vec2<f32>(
        dot(vertex.texcoord, material.texture_transform[0].xy),
        dot(vertex.texcoord, material.texture_transform[1].xy),
    );
    out.uv = base_uv * (1.0 + PORTAL_OUTER_BORDER) - (PORTAL_OUTER_BORDER * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let open_amount = smoothstep(0.0, 1.0, saturate(portal.open_amount));
    // **Squared**, exactly as stage 2 squares it. The two shaders must agree
    // to the bit: stage 2 draws a ring at this radius and this one cuts the
    // hole it rings, so a difference here is a ring that does not sit on the
    // edge of the opening.
    let open_squared = open_amount * open_amount;

    let stretch = (in.uv * 2.0) - 1.0;
    let dist_from_center = length(stretch);

    // "Stencil cutout (1.0 in hole)".
    let stencil_cutout = step(dist_from_center, open_squared);

    // `AlphaFunc( SHADER_ALPHAFUNC_GREATER, 0.5f )`
    // (`portal_refract_helper.cpp:134`), which every stage but the second
    // keeps. The cutout is 0 or 1, so this is the alpha test doing the whole
    // of the shaping — with no blending and alpha writes off, the returned
    // value reaches nothing else.
    if stencil_cutout <= 0.5 {
        discard;
    }

    // `result.rgb = 0.0f; result.a = flStencilCutout;`
    //
    // The black is real and it is drawn: step 1 has colour writes on, so the
    // opening is blacked out before the far room is drawn into it. Step 4
    // draws the same thing with colour writes off, which is the only
    // difference between "punch the hole" and "put the wall back".
    return vec4<f32>(0.0, 0.0, 0.0, stencil_cutout);
}
