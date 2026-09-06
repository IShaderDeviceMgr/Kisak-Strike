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
//! [`from_mat4`] and [`from_row_major`] are the two ways in, and the difference
//! between them is exactly this convention: `glam::Mat4` is already
//! column-major and left-multiplying, so it needs no transpose and is what the
//! matrix stack holds; a `VMatrix` read out of Valve's data or Valve's code is
//! row-major and needs one.
//!
//! The `.vmt` texture-coordinate transform is the exception that proves the
//! rule: it is passed as two explicit **rows** and applied with `dot`, exactly
//! as `vertexlit_and_unlit_generic_vs20.fxc:498` does — no matrix type, so no
//! convention to get wrong.

use bytemuck::{Pod, Zeroable};
use glam::Mat4;

/// A 4x4 matrix as the GPU reads it: four columns.
pub type ColumnMajor = [[f32; 4]; 4];

/// A `glam::Mat4` as a uniform.
///
/// No transpose: `glam::Mat4` is column-major and applies as `m * v`, which is
/// this module's convention and WGSL's. This is a re-spelling, not a
/// conversion, and it exists so that call sites name the convention rather than
/// reaching for `to_cols_array_2d` and leaving the reader to check.
pub fn from_mat4(matrix: Mat4) -> ColumnMajor {
    matrix.to_cols_array_2d()
}

/// A Valve `VMatrix`'s `m[4][4]` as a uniform.
///
/// `VMatrix` subscripts as `m[row][col]` and keeps the translation in the last
/// *column* — `m[0][3]`, `m[1][3]`, `m[2][3]` (`public/mathlib/vmatrix.h`) — so
/// as a mathematical matrix it is the ordinary column-vector one, stored the
/// other way round from how WGSL stores it. This transposes the array, which
/// leaves the mathematical matrix alone: afterwards `m[3]` is the translation
/// column and `m * v` means what it says.
///
/// The translation is the field to check when this looks wrong. `[0][3]`,
/// `[1][3]`, `[2][3]` going in must come out at `[3][0]`, `[3][1]`, `[3][2]`;
/// if it comes back unmoved, something skipped the conversion, and the picture
/// will be plausible until the camera translates.
///
/// This is the *only* place a Valve-shaped matrix crosses into the port's
/// convention. Use it when reading a matrix out of Valve data or transcribing
/// one from Valve code; use [`from_mat4`] for anything the port computed
/// itself.
// No caller yet: everything the port computes itself is already a `glam::Mat4`
// (see [`from_mat4`]). The first real one is the `.bsp`'s stored transforms and
// anything transcribed out of `engine/view.cpp`. Kept, and tested, because it
// is the single point at which the two conventions meet and rediscovering which
// direction the transpose goes is the expensive part.
#[allow(dead_code)]
pub fn from_row_major(rows: [[f32; 4]; 4]) -> ColumnMajor {
    // `from_cols_array_2d` reads `rows` as columns, which builds the transpose;
    // transposing that back gives the matrix `rows` describes, now stored the
    // way `to_cols_array_2d` and WGSL want it.
    Mat4::from_cols_array_2d(&rows)
        .transpose()
        .to_cols_array_2d()
}

/// The identity, in this module's convention.
// Read only by the tests since the matrix stack started producing real
// matrices; kept as the statement of what the layout is.
#[allow(dead_code)]
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
    // `Pass::draw_modulated` builds these from a model matrix and a modulation
    // instead; this is what a caller with neither wants.
    #[allow(dead_code)]
    pub fn identity() -> DrawUniforms {
        DrawUniforms {
            model: IDENTITY,
            modulation: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

/// How many local lights a draw can carry. `MaxNumLights()` on the PC path.
///
/// Valve's registers hold four (`LightInfo cLightInfo[4] : register(c27)`,
/// `common_vs_fxc.h:126`) and `CompilePixelShaderLocalLights` packs a fourth
/// into the `w` components of the first three because SM3 pixel shaders ran out
/// of registers. There is no register pressure here, so the fourth light is an
/// ordinary array element and that packing is deleted.
pub const MAX_LIGHTS: usize = 4;

/// The six directions an ambient cube samples: `+x, -x, +y, -y, +z, -z`.
pub const AMBIENT_CUBE_FACES: usize = 6;

/// One local light, as the shader reads it.
///
/// `LightInfo` (`common_vs_fxc.h:113`), five `float4`s, filled by
/// `CShaderAPIDx8::CompileVertexShaderLocalLights` (`shaderapidx8.cpp:14020`).
/// The layout is transcribed from there rather than from the struct, because
/// two of the fields are *type tags smuggled into `w` components* and the
/// struct does not say so.
///
/// # The light type lives in two `w` components
///
/// Valve's own note (`common_vs_fxc.h:119`): "1x - directional, 01 - spot,
/// 00 - point". There is no type enum in the constant block — the shader
/// selects behaviour with two `lerp`s on [`color`](Light::color)`.w` and
/// [`direction`](Light::direction)`.w`, which is what a shader model with no
/// branches had instead of an `if`. Build one with [`Light::point`],
/// [`Light::spot`] or [`Light::directional`] rather than filling the fields.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Light {
    /// Linear RGB, and in `w` **1 for a directional light**, 0 otherwise.
    /// A directional light skips attenuation entirely.
    pub color: [f32; 4],
    /// The direction the light points, and in `w` **1 for a spot light**,
    /// 0 otherwise. A point light's direction is written as zero, exactly as
    /// `CompileVertexShaderLocalLights` writes it.
    pub direction: [f32; 4],
    /// World-space position, `w` 1. Unused by a directional light.
    pub position: [f32; 4],
    /// `(falloff, thetaDot, phiDot, 1/(thetaDot - phiDot))` for a spot light,
    /// and `(0, 1, 1, 1)` for anything else — which is not "unused" but the
    /// value that makes the spot term fold to 1.
    pub spot: [f32; 4],
    /// `(constant, linear, quadratic, 0)`. The attenuation denominator is
    /// `constant + linear * d + quadratic * d²`, so all-zero is a division by
    /// zero rather than "no attenuation": [`Light::NONE`] uses a constant of 1.
    pub attenuation: [f32; 4],
}

impl Light {
    /// A light contributing nothing, for the array slots past
    /// [`ModelLighting::count`].
    ///
    /// `s_pTwoEmptyLights` (`shaderapidx8.cpp:14146`) is this: a black colour,
    /// a spot-shaped falloff that folds to 1, and a constant attenuation of 1
    /// so that nothing divides by zero. The colour is what makes it dark; the
    /// rest is what makes it *finite*, and the shader still evaluates it.
    pub const NONE: Light = Light {
        color: [0.0, 0.0, 0.0, 0.0],
        direction: [1.0, 0.0, 0.0, 0.0],
        position: [0.0, 0.0, 0.0, 0.0],
        spot: [1.0, 1.0, 1.0, 1.0],
        attenuation: [1.0, 1.0, 1.0, 1.0],
    };

    /// A point light: attenuated by distance, lighting in every direction.
    pub fn point(color: [f32; 3], position: [f32; 3], attenuation: [f32; 3]) -> Light {
        Light {
            color: [color[0], color[1], color[2], 0.0],
            direction: [0.0, 0.0, 0.0, 0.0],
            position: [position[0], position[1], position[2], 1.0],
            spot: [0.0, 1.0, 1.0, 1.0],
            attenuation: [attenuation[0], attenuation[1], attenuation[2], 0.0],
        }
    }

    /// A spot light. `falloff` is `LightDesc_t::m_Falloff`, and the two dots
    /// are the cosines of the inner and outer cone half-angles.
    // No caller in the binary yet: the thing that builds these is
    // `R_StudioSetupLighting`, which arrives with static props. Kept, and
    // tested, because the `w`-component type encoding they exist to hide is
    // the part of this ABI that is silent when it is wrong.
    #[allow(dead_code)]
    pub fn spot(
        color: [f32; 3],
        position: [f32; 3],
        direction: [f32; 3],
        attenuation: [f32; 3],
        falloff: f32,
        theta_dot: f32,
        phi_dot: f32,
    ) -> Light {
        // `LightDesc_t::OneOverThetaDotMinusPhiDot`. A cone whose two angles
        // are equal is a hard edge, not a division by zero.
        let ood = if (theta_dot - phi_dot).abs() > f32::EPSILON {
            1.0 / (theta_dot - phi_dot)
        } else {
            0.0
        };
        Light {
            color: [color[0], color[1], color[2], 0.0],
            direction: [direction[0], direction[1], direction[2], 1.0],
            position: [position[0], position[1], position[2], 1.0],
            spot: [falloff, theta_dot, phi_dot, ood],
            attenuation: [attenuation[0], attenuation[1], attenuation[2], 0.0],
        }
    }

    /// A directional light. No position, no attenuation.
    #[allow(dead_code)]
    pub fn directional(color: [f32; 3], direction: [f32; 3]) -> Light {
        Light {
            color: [color[0], color[1], color[2], 1.0],
            direction: [direction[0], direction[1], direction[2], 0.0],
            position: [0.0, 0.0, 0.0, 1.0],
            spot: [0.0, 1.0, 1.0, 1.0],
            attenuation: [1.0, 0.0, 0.0, 0.0],
        }
    }
}

/// The lighting one model instance is drawn under — group 3, binding 0, for
/// the shaders that read it.
///
/// `MaterialLightingState_t` (`public/materialsystem/imaterialsystem.h`) as it
/// reaches the GPU: `PI_SetVertexShaderAmbientLightCube` and
/// `PI_SetPixelShaderLocalLighting`, the two per-instance commands
/// `vertexlitgeneric_dx9_helper.cpp:634` emits, plus the light array
/// `CommitVertexShaderLighting` uploads.
///
/// # Why this is group 3 and not part of [`DrawUniforms`]
///
/// It is per-draw data, so group 2 would be the obvious home — and it is 432
/// bytes that no world surface and no sprite would ever read. Group 3 is
/// already "whatever this shader's lighting comes from": a lightmap atlas page
/// for `LightmappedGeneric`, this for `VertexLitGeneric`. A pipeline layout is
/// per shader, so a shader that reads neither declares no group 3 at all —
/// which is what keeps the cost off the shaders that do not want it. See
/// [`ShaderKind::lighting_binding`](super::shader::ShaderKind::lighting_binding).
///
/// # Set once per model, not once per draw
///
/// `R_StudioSetupLighting` runs once for a model and every mesh of that model
/// is then drawn under it, which is why
/// [`Pass::set_model_lighting`](super::context::Pass::set_model_lighting) is
/// pass state rather than an argument to a draw — the same shape
/// `bind_lightmap_page` has, for the same reason.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct ModelLighting {
    /// The ambient cube: light arriving from `+x, -x, +y, -y, +z, -z`, in
    /// **that order**, in linear space.
    ///
    /// VS `c21..c26` as three `float3[2]` pairs (`cAmbientCubeX`, `Y`, `Z`),
    /// which is where the axis-major ordering comes from — `AmbientLight`
    /// indexes `cAmbientCubeX[isNegative.x]`, so positive is always the even
    /// slot. Getting the pairing wrong swaps a model's lighting front-to-back
    /// or top-to-bottom and reads as a level lit from the wrong side.
    ///
    /// `w` is padding: a `vec3` in a uniform array still occupies 16 bytes.
    pub ambient_cube: [[f32; 4]; AMBIENT_CUBE_FACES],
    /// Up to four local lights. Slots past [`count`](ModelLighting::count)
    /// must be [`Light::NONE`] rather than zeroed, because the shader
    /// evaluates every slot.
    pub lights: [Light; MAX_LIGHTS],
    /// How many of [`lights`](ModelLighting::lights) are real.
    ///
    /// `MaterialLightingState_t::m_nLocalLightCount`, which reaches the shader
    /// as `g_nLightCount`. It is a count rather than the four
    /// `g_bLightEnabled` booleans because those were a static-control-flow
    /// mechanism and the loop is unrolled here anyway.
    pub count: u32,
    /// Whether the vertex stream's baked static light is meaningful.
    ///
    /// `ShaderStateLighting_t::m_bStaticLight`, which reaches the vertex shader
    /// as `g_flStaticLightEnabled` (`vertexlitgeneric_dx9_helper.cpp:1800`).
    /// A model with no baked lighting has a black colour stream, and the
    /// difference between "black because unlit" and "black because absent" is
    /// exactly this flag.
    pub static_light: u32,
    /// Whether [`ambient_cube`](ModelLighting::ambient_cube) is meaningful.
    ///
    /// `ShaderStateLighting_t::m_bAmbientLight`. Separate from
    /// [`count`](ModelLighting::count) because Valve's two shaders gate the
    /// cube differently and the difference is worth keeping in one place:
    /// the unbumped vertex path adds it under `bDynamicLight`, which is
    /// `m_bAmbientLight || m_nNumLights > 0` (`ishaderdynamic.h:43`), while the
    /// bumped pixel path has its own `AMBIENT_LIGHT` combo
    /// (`vertexlitgeneric_dx9_helper.cpp:1880`). The two agree whenever the
    /// cube is zeroed while disabled, which is what `SetAmbientLightCube` does,
    /// so this flag is the honest form of both.
    pub ambient_light: u32,
    /// One word, not two: WGSL rounds the struct up to 432 bytes and Rust's
    /// `#[repr(C)]` alignment here is 4, so the padding has to be spelled out
    /// exactly rather than left to either language's rules.
    pub _padding: u32,
}

impl ModelLighting {
    /// A neutral flat-lit state: a white ambient cube, no local lights, no
    /// baked static light.
    ///
    /// Not a *correct* lighting state for anything — it is what a caller who
    /// has not set one gets, so that a model drawn before the lighting path
    /// exists is visible rather than black. `R_StudioSetupLighting`'s job is
    /// to replace it.
    pub fn fullbright() -> ModelLighting {
        ModelLighting {
            ambient_cube: [[1.0, 1.0, 1.0, 0.0]; AMBIENT_CUBE_FACES],
            lights: [Light::NONE; MAX_LIGHTS],
            count: 0,
            static_light: 0,
            ambient_light: 1,
            _padding: 0,
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

        // `Light` is Valve's five constant registers, and `ModelLighting` is
        // six ambient vectors, four of those, and one word of switches — which
        // has to be padded by hand, because Rust's alignment for an array of
        // `[f32; 4]` is 4 and WGSL's is 16, so the two disagree about the tail
        // unless it is spelled out.
        assert_eq!(size_of::<Light>(), 5 * 16);
        assert_eq!(
            size_of::<ModelLighting>(),
            AMBIENT_CUBE_FACES * 16 + MAX_LIGHTS * 5 * 16 + 16
        );
        assert_eq!(size_of::<ModelLighting>() % 16, 0);
    }

    #[test]
    fn a_lights_type_lives_in_two_w_components() {
        // "1x - directional, 01 - spot, 00 - point" (`common_vs_fxc.h:119`).
        // There is no type enum in the constant block — the shader selects
        // behaviour with two `lerp`s on these two `w`s, which is what a shader
        // model with no branches had instead of an `if`. Swap them and every
        // point light in the game stops attenuating.
        let point = Light::point([1.0; 3], [1.0, 2.0, 3.0], [0.0, 0.0, 1.0]);
        assert_eq!(point.color[3], 0.0, "not directional");
        assert_eq!(point.direction[3], 0.0, "not a spot");
        // `CompileVertexShaderLocalLights` writes a point light's direction as
        // zero rather than leaving it: `pDest[1].Init( 0, 0, 0, w )`.
        assert_eq!(point.direction[..3], [0.0, 0.0, 0.0]);

        let spot = Light::spot(
            [1.0; 3],
            [0.0; 3],
            [0.0, 0.0, -1.0],
            [0.0, 0.0, 1.0],
            5.0,
            0.9,
            0.4,
        );
        assert_eq!(spot.color[3], 0.0);
        assert_eq!(spot.direction[3], 1.0, "a spot");
        // exponent, thetaDot, phiDot, 1/(thetaDot - phiDot).
        assert_eq!(spot.spot[0], 5.0);
        assert_eq!(spot.spot[1], 0.9);
        assert_eq!(spot.spot[2], 0.4);
        assert!((spot.spot[3] - 1.0 / 0.5).abs() < 1e-6);

        let directional = Light::directional([1.0; 3], [0.0, 0.0, -1.0]);
        assert_eq!(directional.color[3], 1.0, "directional");
        assert_eq!(directional.direction[3], 0.0, "not a spot");
    }

    #[test]
    fn a_cone_with_no_width_does_not_divide_by_zero() {
        // `OneOverThetaDotMinusPhiDot` with equal angles. A hard-edged cone is
        // a thing content can ask for; an infinity in a uniform is not.
        let spot = Light::spot(
            [1.0; 3],
            [0.0; 3],
            [0.0, 0.0, -1.0],
            [1.0, 0.0, 0.0],
            1.0,
            0.5,
            0.5,
        );
        assert!(spot.spot[3].is_finite());
    }

    #[test]
    fn an_unused_light_slot_is_dark_but_finite() {
        // `s_pTwoEmptyLights` (`shaderapidx8.cpp:14146`). The shader evaluates
        // every slot, so a zeroed one is a division by zero in the attenuation
        // denominator — the colour is what makes this contribute nothing, and
        // the constant attenuation of 1 is what keeps it finite.
        assert_eq!(Light::NONE.color[..3], [0.0, 0.0, 0.0]);
        // The denominator the shader divides by is
        // `constant + linear * d + quadratic * d²`, so a zeroed slot is an
        // infinity rather than a dark light.
        const _: () = assert!(Light::NONE.attenuation[0] > 0.0);
        // And `fullbright` fills every slot with it rather than zeroing them.
        let lighting = ModelLighting::fullbright();
        assert_eq!(lighting.count, 0);
        assert!(lighting.lights.iter().all(|light| *light == Light::NONE));
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
    fn a_valve_matrix_moves_its_translation_into_the_last_column() {
        // The one field whose position differs between the two conventions,
        // and the one that produces a plausible picture when it is wrong: a
        // rotation-only matrix survives a missed transpose looking merely
        // mirrored, a translation does not survive it at all.
        let mut rows = [[0.0f32; 4]; 4];
        for (i, row) in rows.iter_mut().enumerate() {
            row[i] = 1.0;
        }
        // VMatrix puts translation in the last column of each row.
        rows[0][3] = 10.0;
        rows[1][3] = 20.0;
        rows[2][3] = 30.0;

        let columns = from_row_major(rows);
        assert_eq!(columns[3], [10.0, 20.0, 30.0, 1.0]);
        // ... and nowhere else.
        assert_eq!(columns[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(columns[1], [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(columns[2], [0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn the_conversion_is_a_transpose_and_nothing_else() {
        // Every entry distinct, so a swap of any pair shows up.
        let mut rows = [[0.0f32; 4]; 4];
        for (row, values) in rows.iter_mut().enumerate() {
            for (col, value) in values.iter_mut().enumerate() {
                *value = (row * 4 + col) as f32;
            }
        }
        let columns = from_row_major(rows);
        for row in 0..4 {
            for col in 0..4 {
                assert_eq!(columns[col][row], rows[row][col], "[{row}][{col}]");
            }
        }
    }

    #[test]
    fn a_glam_matrix_is_passed_through_unchanged() {
        // The other half of the pair: glam is already in this convention, so
        // `from_mat4` must *not* transpose. Asserting that against a matrix
        // with a translation is what separates it from `from_row_major`.
        let matrix = Mat4::from_translation(glam::Vec3::new(10.0, 20.0, 30.0));
        assert_eq!(from_mat4(matrix)[3], [10.0, 20.0, 30.0, 1.0]);
        assert_eq!(from_mat4(Mat4::IDENTITY), IDENTITY);
    }

    #[test]
    fn a_point_transforms_the_same_way_under_both_conventions() {
        // The end-to-end statement the other three are pieces of: apply
        // Valve's matrix to a point Valve's way (row vector on the left, over
        // the row-major array), apply the converted one WGSL's way (`m * v`),
        // and get the same point.
        let rows = [
            [0.0, -1.0, 0.0, 5.0],
            [1.0, 0.0, 0.0, 6.0],
            [0.0, 0.0, 1.0, 7.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let point = [2.0f32, 3.0, 4.0, 1.0];

        // Valve: result[i] = sum over j of m[i][j] * p[j] -- a column-vector
        // multiply, which is what VMatrix's translation-in-column-3 layout is.
        let mut valve = [0.0f32; 4];
        for (i, out) in valve.iter_mut().enumerate() {
            *out = (0..4).map(|j| rows[i][j] * point[j]).sum();
        }

        // Ours: the same thing WGSL does with `m * v` over four columns.
        let columns = from_row_major(rows);
        let mut ours = [0.0f32; 4];
        for (i, out) in ours.iter_mut().enumerate() {
            *out = (0..4).map(|c| columns[c][i] * point[c]).sum();
        }

        assert_eq!(valve, ours);
        assert_eq!(valve, [2.0, 8.0, 11.0, 1.0]);
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
