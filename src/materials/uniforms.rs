//! The constant ABI: what every shader can read, and where it lives.
//!
//! Replaces `materialsystem/stdshaders/common_hlsl_cpp_consts.h` and the
//! register map at the top of `common_vs_fxc.h` / `common_ps_fxc.h`. Those were
//! **three** files kept in sync by hand — a header `#include`d by both C++ and
//! HLSL for a handful of `#define`s, plus a register declaration in each
//! prelude — and the sync was maintained by comments asking the reader to
//! remember the others. Here there is one set of `#[repr(C)]` structs, mirrored
//! once in `shaders/prelude.wgsl`, and a struct that grows a field is a
//! compile error at every write site.
//!
//! `portdocs/MATERIALSYSTEM.md` §7.4 is the design; this is it, built.
//!
//! # Bind groups
//!
//! Valve's register map is really a *frequency* map — it tells you which
//! constants change per frame, per material and per draw, because that is what
//! the engine had to re-upload and when. That frequency is the bind group:
//!
//! | Group | Contents | Rate | Valve's registers |
//! |---|---|---|---|
//! | 0 | [`FrameUniforms`] | once a frame | VS `c2`, `c8..c11`, `c16`; PS `c29`, `c30`, `c32` |
//! | 1 | material params + textures | once a material | the shader-specific block, VS `c48+` / PS `c0+` |
//! | 2 | [`DrawUniforms`] | once a draw | VS `c4..c7`, `c47`, `c48+` |
//! | 3 | skinning + morph storage buffers | once a draw, skinned only | VS `c58+` (`cModel[53]`), `c1024+` (`cFlexWeights[512]`) |
//!
//! Group 3 does not exist yet: it is bulk data, `wgpu` wants storage buffers
//! for it, and nothing is skinned until `studiorender` is ported. Group 1's
//! layout is the *shader's*, since it is the one thing that genuinely differs
//! between them — see [`shader`](super::shader).
//!
//! # Matrices are column-major and multiply on the left
//!
//! **This is the convention every later shader inherits, and getting it wrong
//! produces a plausible-looking wrong picture rather than an error.**
//!
//! - A `[[f32; 4]; 4]` here is four **columns**. `m[3]` is the translation.
//! - WGSL applies it as `m * vec4(position, 1.0)`.
//!
//! Valve's is the opposite on both counts: `VMatrix` is row-major with
//! translation in column 3, uploaded a row per constant register, and applied
//! as `mul( float4(pos,1), cModelViewProj )` — a row vector on the left. That
//! is a D3D9 convention, `wgpu`/WGSL follows the other one, and translating
//! each shader through a transpose forever would be a permanent tax for
//! nothing. So a `VMatrix`-shaped value is transposed exactly once, on its way
//! into a uniform, and everything downstream of that is column-major.
//!
//! Nothing does that transpose yet, because nothing yet turns a `VMatrix` into
//! a uniform *matrix*: the only matrix a `.vmt` can hold is a texture-coordinate
//! transform, and those are passed as two explicit **rows** and applied with
//! `dot`, exactly as `vertexlit_and_unlit_generic_vs20.fxc:498` does — no
//! matrix type, no convention to get wrong. The first real one arrives with the
//! matrix stack in stage 4, where `glam::Mat4::to_cols_array_2d` is the
//! transpose.

use bytemuck::{Pod, Zeroable};

/// A 4x4 matrix as the GPU reads it: four columns.
pub type ColumnMajor = [[f32; 4]; 4];

/// The identity, in this module's convention.
pub const IDENTITY: ColumnMajor = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Constants that change once a frame — group 0, binding 0.
///
/// Every field is one of Valve's registers, named after it. The registers with
/// no field are the ones that describe machinery this port does not have:
/// `cConstants1` (`cOOGamma`, for a hardware gamma ramp that an sRGB surface
/// replaces), `cViewModel` (view-model rendering), `g_bLightEnabled` /
/// `cLightInfo` (per-draw lights, group 2's business when lighting lands) and
/// the ambient cube.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FrameUniforms {
    /// `cViewProj`, VS `c8..c11`. World space to clip space.
    pub view_proj: ColumnMajor,

    /// `cEyePos_WaterHeightW`, VS `c2`: the eye in world space, and the water
    /// surface height in `w`. Read by range fog, which needs the distance from
    /// the eye rather than the depth.
    pub eye_pos_water_height: [f32; 4],

    /// `cFogParams`, VS `c16`, packed exactly as `CShaderAPIDx8` packs it for
    /// DX level above 90 (`shaderapidx8.cpp:1626`):
    ///
    /// | | |
    /// |---|---|
    /// | `x` | `1 - end/(end-start)`, so the factor starts at 0 at `start` |
    /// | `y` | the water surface's z, for height fog |
    /// | `z` | maximum fog density, 0..1 |
    /// | `w` | `1/(end-start)` |
    ///
    /// The DX-level-90-and-below packing is the *inverse* of this one, because
    /// fixed-function fog read 0 as fully fogged. That branch is gone; this is
    /// the surviving half. See [`FrameUniforms::no_fog`].
    pub fog_params: [f32; 4],

    /// `g_LinearFogColor`, PS `c29`. `w` is `1/(dest alpha depth range)`.
    pub fog_color: [f32; 4],

    /// `cLightScale`, PS `c30`: `(linear, lightmap, envmap, gamma)` light
    /// scales — the tone-mapping multipliers `FinalOutput` picks between.
    /// `UnlitGeneric` asks for none of them, but every lit shader does, and the
    /// point of a shared block is that it does not change per shader.
    pub light_scale: [f32; 4],

    /// `cScreenSize`, PS `c32`: `(width, height, 1/width, 1/height)`.
    pub screen_size: [f32; 4],
}

impl FrameUniforms {
    /// The fog packing that means "no fog", from the same `else` branch of
    /// `CShaderAPIDx8::SetFogParams`: the factor is rigged to come out 0 for
    /// every distance rather than the shader being told to skip the work.
    pub const fn no_fog() -> [f32; 4] {
        [0.0, f32::MIN, 0.0, 0.0]
    }

    /// A frame with no fog, no tone mapping and the given view.
    ///
    /// What the engine would upload before it has a fog controller or an
    /// exposure curve — which is to say, all of stage 3.
    pub fn new(view_proj: ColumnMajor, eye: [f32; 3], size: (u32, u32)) -> FrameUniforms {
        let (width, height) = (size.0.max(1) as f32, size.1.max(1) as f32);
        FrameUniforms {
            view_proj,
            eye_pos_water_height: [eye[0], eye[1], eye[2], 0.0],
            fog_params: FrameUniforms::no_fog(),
            fog_color: [0.0, 0.0, 0.0, 1.0],
            // `TONEMAP_SCALE_NONE` is a shader-side constant, but the scales
            // themselves are still 1: no exposure, no HDR.
            light_scale: [1.0, 1.0, 1.0, 1.0],
            screen_size: [width, height, 1.0 / width, 1.0 / height],
        }
    }
}

/// Constants that change once a draw — group 2, binding 0.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct DrawUniforms {
    /// Object space to world space.
    ///
    /// Valve had no such register: an unskinned draw still went through
    /// `SkinPosition` with `cModel[0]` — the first bone — as its transform
    /// (`common_vs_fxc.h:170`). One matrix in a per-draw block says the same
    /// thing without the skinning path, and when skinning lands it becomes
    /// bone 0 of group 3's storage buffer.
    pub model: ColumnMajor,

    /// `cModulationColor`, VS `c47`: `$color * $alpha`, times whatever the
    /// render context is overriding with.
    ///
    /// Per-draw rather than per-material because it is: `CBaseMeshDX8::DrawMesh`
    /// (`shaderapidx9/meshdx8.cpp:2378`) multiplies the material's colour and
    /// alpha by a per-instance modulation before every draw, and material
    /// proxies rewrite `$color` between draws of the same material.
    pub modulation: [f32; 4],
}

impl DrawUniforms {
    /// One draw of an untransformed, unmodulated thing.
    pub fn identity() -> DrawUniforms {
        DrawUniforms {
            model: IDENTITY,
            modulation: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_blocks_are_the_size_wgsl_expects() {
        // A uniform buffer's size and every member's offset are part of the
        // ABI: WGSL rounds a struct up to a multiple of its largest member's
        // alignment, and a mismatch here binds garbage rather than failing.
        assert_eq!(size_of::<FrameUniforms>(), 64 + 5 * 16);
        assert_eq!(size_of::<DrawUniforms>(), 64 + 16);
        assert_eq!(size_of::<FrameUniforms>() % 16, 0);
        assert_eq!(size_of::<DrawUniforms>() % 16, 0);
    }

    #[test]
    fn the_identity_is_the_identity_in_memory() {
        // Four columns, and the translation column last: the layout `m * v`
        // reads. A transposed identity is still the identity, so this checks
        // the shape rather than the convention — the convention is pinned by
        // `preview.rs`'s `the_model_matrix_places_the_quad`, on a real GPU.
        let flat: Vec<f32> = IDENTITY.iter().flatten().copied().collect();
        assert_eq!(
            flat,
            [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn no_fog_produces_a_zero_factor_at_every_distance() {
        // The shader computes `min( maxDensity, saturate( x + dist * w ) )`.
        let [x, _, max_density, w] = FrameUniforms::no_fog();
        for distance in [0.0f32, 1.0, 1e6] {
            let factor = (x + distance * w).clamp(0.0, 1.0).min(max_density);
            assert_eq!(factor, 0.0, "at {distance}");
        }
    }
}
