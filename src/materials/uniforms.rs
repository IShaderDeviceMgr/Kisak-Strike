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
